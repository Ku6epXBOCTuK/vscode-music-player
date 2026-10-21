use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::{mpsc, watch};

use crate::config::Playlist;
use crate::{audio, pump};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Playing => "playing",
            Status::Paused => "paused",
            Status::Stopped => "stopped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    NextPlaylist,
    PrevPlaylist,
    NextTrack,
    ToggleShuffle,
}

pub struct PlayerState {
    pub status: Status,
    pub volume: f32,
    pub current_track: Option<String>,
    pub playlist_index: usize,
}

pub type Shared = Arc<RwLock<PlayerState>>;
pub type SharedPlaylists = Arc<RwLock<Vec<Playlist>>>;
pub type SharedShuffle = Arc<AtomicBool>;

fn build_queue(playlist: &Playlist) -> Result<Vec<PathBuf>, String> {
    log_debug!(
        "building queue for playlist '{}' ({}): {}",
        playlist.name,
        playlist.kind,
        playlist.path
    );
    match playlist.kind.as_str() {
        "local_folder" => {
            let files = audio::scan_folder(std::path::Path::new(&playlist.path))
                .map_err(|e| format!("cannot scan '{}': {e}", playlist.path))?;
            if files.is_empty() {
                Err(format!("folder has no audio files: {}", playlist.path))
            } else {
                log_info!("queue for '{}': {} tracks", playlist.name, files.len());
                Ok(files)
            }
        }
        "network_radio" => Ok(Vec::new()),
        other => Err(format!("unknown playlist type: {other}")),
    }
}

pub fn spawn(
    playlists: SharedPlaylists,
    start_index: usize,
    initial_queue: Vec<PathBuf>,
    volume: f32,
    shuffle_flag: SharedShuffle,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    epoch_tx: watch::Sender<u64>,
) -> (Shared, mpsc::Sender<Command>) {
    let state = Arc::new(RwLock::new(PlayerState {
        status: Status::Playing,
        volume,
        current_track: None,
        playlist_index: start_index,
    }));
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);

    let task_state = state.clone();
    let task_playlists = playlists.clone();
    tokio::spawn(async move {
        log_info!(
            "player task started: start index {}, queue {} tracks, volume {:.2}",
            start_index,
            initial_queue.len(),
            volume
        );
        let mut queue = initial_queue;
        let mut order: Vec<usize> = (0..queue.len()).collect();
        let mut pl_idx = start_index;
        let mut pos = 0usize;
        let mut switch: Option<usize> = None;
        let mut shuffle = shuffle_flag.load(Ordering::Relaxed);
        if shuffle {
            fastrand::shuffle(&mut order);
        }
        log_info!(
            "player queue ready: {} tracks, shuffle {}",
            order.len(),
            shuffle
        );

        loop {
            // NOTE: snapshot: additions via POST /api/playlists apply on the next iteration
            let playlists = task_playlists.read().expect("playlists poisoned").clone();

            if let Some(new_idx) = switch.take() {
                log_info!("switching to playlist index {new_idx}");
                match playlists.get(new_idx).map(build_queue) {
                    Some(Ok(q)) => {
                        queue = q;
                        pl_idx = new_idx;
                        pos = 0;
                        order = (0..queue.len()).collect();
                        if shuffle {
                            fastrand::shuffle(&mut order);
                        }
                        {
                            let mut s = task_state.write().expect("player state poisoned");
                            s.playlist_index = pl_idx;
                        }
                        epoch_tx.send_modify(|e| *e += 1);
                        log_info!(
                            "switched playlist: '{}' (index {}, {} tracks)",
                            playlists[pl_idx].name,
                            pl_idx,
                            queue.len()
                        );
                    }
                    Some(Err(e)) => {
                        let current = playlists
                            .get(pl_idx)
                            .map(|p| p.name.as_str())
                            .unwrap_or("unknown");
                        log_error!(
                            "cannot switch to playlist index {new_idx} ('{}'): {e}; keeping playlist '{current}'",
                            playlists[new_idx].name
                        );
                    }
                    None => log_error!(
                        "playlist index {new_idx} no longer exists ({} configured), staying on index {pl_idx}",
                        playlists.len()
                    ),
                }
            }

            let flag = shuffle_flag.load(Ordering::Relaxed);
            if flag != shuffle {
                shuffle = flag;
                let current = order.get(pos % order.len().max(1)).copied().unwrap_or(0);
                order = (0..queue.len()).collect();
                if shuffle {
                    fastrand::shuffle(&mut order);
                }
                pos = order.iter().position(|&i| i == current).unwrap_or(0);
                log_info!(
                    "shuffle {} from outside the player, keeping the current track and applying the new order",
                    if shuffle { "enabled" } else { "disabled" }
                );
            }

            if let Some(pl) = playlists.get(pl_idx)
                && pl.kind == "network_radio"
            {
                {
                    let mut s = task_state.write().expect("player state poisoned");
                    s.current_track = Some(pl.name.clone());
                }
                log_info!("now playing: radio '{}' -> {}", pl.name, pl.path);
                match crate::radio::run(
                    &pl.path,
                    &pcm_tx,
                    &mut cmd_rx,
                    &shuffle_flag,
                    playlists.len(),
                    pl_idx,
                )
                .await
                {
                    Some(new_idx) => {
                        log_info!("radio requested switch to playlist index {new_idx}");
                        switch = Some(new_idx);
                        continue;
                    }
                    None => {
                        log_warn!("radio pipeline closed, player task exiting");
                        return;
                    }
                }
            }

            if queue.is_empty() {
                log_debug!(
                    "queue empty on playlist index {pl_idx}, waiting for playlists to be added"
                );
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }

            let queue_pos = order.get(pos % order.len()).copied().unwrap_or(0);
            let path = queue[queue_pos].clone();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            {
                let mut s = task_state.write().expect("player state poisoned");
                s.current_track = Some(name.clone());
            }
            log_info!(
                "now playing: {name} (track {}/{})",
                pos % order.len() + 1,
                order.len()
            );
            log_debug!("  file: {}", path.display());

            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    log_error!(
                        "failed to open '{}': {e}, skipping to next track",
                        path.display()
                    );
                    pos += 1;
                    continue;
                }
            };
            log_debug!(
                "  opened {} ({} bytes)",
                path.display(),
                file.metadata().map(|m| m.len()).unwrap_or(0)
            );

            let cancel = Arc::new(AtomicBool::new(false));
            let pump_cancel = cancel.clone();
            let pump_tx = pcm_tx.clone();
            let mut handle = tokio::task::spawn_blocking(move || {
                pump::pump(Box::new(file), &pump_tx, &pump_cancel, 0)
            });

            loop {
                tokio::select! {
                    cmd = cmd_rx.recv() => {
                        let Some(c) = cmd else {
                            cancel.store(true, Ordering::Relaxed);
                            let _ = handle.await;
                            log_warn!("playlist command channel closed, player task exiting");
                            return;
                        };
                        if matches!(c, Command::ToggleShuffle) {
                            shuffle = !shuffle;
                            let current = order.get(pos % order.len()).copied().unwrap_or(0);
                            order = (0..queue.len()).collect();
                            if shuffle {
                                fastrand::shuffle(&mut order);
                            }
                            pos = order.iter().position(|&i| i == current).unwrap_or(0);
                            shuffle_flag.store(shuffle, Ordering::Relaxed);
                            log_info!(
                                "shuffle turned {}: '{name}' keeps playing, the new order applies from the next track",
                                if shuffle { "on" } else { "off" }
                            );
                            continue;
                        }
                        cancel.store(true, Ordering::Relaxed);
                        let _ = handle.await;
                        if matches!(c, Command::NextPlaylist | Command::PrevPlaylist) {
                            if playlists.is_empty() {
                                log_error!("ignoring playlist switch: no playlists configured");
                            } else {
                                let next = if matches!(c, Command::NextPlaylist) {
                                    (pl_idx + 1) % playlists.len()
                                } else {
                                    (pl_idx + playlists.len() - 1) % playlists.len()
                                };
                                log_info!(
                                    "command {c:?}: playlist '{}' (index {pl_idx}) -> index {next}",
                                    playlists[pl_idx].name
                                );
                                switch = Some(next);
                            }
                        } else {
                            log_info!("command {c:?}: stopping '{name}' and moving on");
                        }
                        break;
                    }
                    result = &mut handle => {
                        let playlist_name = playlists
                            .get(pl_idx)
                            .map(|p| p.name.as_str())
                            .unwrap_or("unknown");
                        match result {
                            Ok(pump::Outcome::Exhausted) => {
                                log_info!("track finished: {name} (playlist '{playlist_name}')")
                            }
                            Ok(pump::Outcome::Interrupted) => {
                                log_info!("track interrupted: {name} (playlist '{playlist_name}')")
                            }
                            Ok(pump::Outcome::Failed(e)) => log_error!(
                                "playback of '{name}' failed: {e} | file: {} | playlist '{playlist_name}' (index {pl_idx})",
                                path.display()
                            ),
                            Err(e) => log_error!(
                                "pump task panicked for '{name}' (file: {}): {e}",
                                path.display()
                            ),
                        }
                        break;
                    }
                }
            }

            if switch.is_none() {
                pos += 1;
                if shuffle && order.len() > 1 && pos.is_multiple_of(order.len()) {
                    fastrand::shuffle(&mut order);
                    log_debug!("shuffle: new random round of {} tracks", order.len());
                }
                log_debug!(
                    "queue position advanced to {} of {}",
                    pos % order.len().max(1) + 1,
                    order.len()
                );
            }
        }
    });

    (state, cmd_tx)
}
