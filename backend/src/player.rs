use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::config::Playlist;
use crate::{audio, pump, radio};

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

// NOTE: the supervisor resolves a command into an intent, so the player
// reacts wherever it is parked instead of only between tracks
#[derive(Clone, Copy, Debug)]
enum Intent {
    NextPlaylist,
    PrevPlaylist,
    SkipTrack,
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

// NOTE: handles for the player task, bundled so its entry point stays small
struct Ctx {
    state: Shared,
    playlists: SharedPlaylists,
    shuffle: SharedShuffle,
    cancel: Arc<Mutex<CancellationToken>>,
    pcm_tx: mpsc::Sender<Vec<f32>>,
    epoch_tx: watch::Sender<u64>,
}

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

fn resolve(
    intent: Intent,
    count: usize,
    pl_idx: usize,
    switch: &mut Option<usize>,
    skip: &mut bool,
) {
    if count == 0 {
        log_error!("ignoring a command: no playlists configured");
        return;
    }
    match intent {
        Intent::NextPlaylist => *switch = Some((switch.unwrap_or(pl_idx) + 1) % count),
        Intent::PrevPlaylist => *switch = Some((switch.unwrap_or(pl_idx) + count - 1) % count),
        Intent::SkipTrack => *skip = true,
    }
}

fn cancel_current(slot: &Mutex<CancellationToken>) {
    slot.lock().expect("cancel slot poisoned").cancel();
}

fn on_radio(playlists: &SharedPlaylists, state: &Shared) -> bool {
    let index = state.read().expect("player state poisoned").playlist_index;
    playlists
        .read()
        .expect("playlists poisoned")
        .get(index)
        .map(|p| p.kind == "network_radio")
        .unwrap_or(false)
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
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(32);
    let (intent_tx, intent_rx) = mpsc::unbounded_channel::<Intent>();
    let ctx = Ctx {
        state: state.clone(),
        playlists: playlists.clone(),
        shuffle: shuffle_flag,
        cancel: Arc::new(Mutex::new(CancellationToken::new())),
        pcm_tx,
        epoch_tx,
    };

    tokio::spawn(supervise(
        cmd_rx,
        intent_tx,
        state.clone(),
        playlists.clone(),
        ctx.shuffle.clone(),
        ctx.cancel.clone(),
    ));

    log_info!(
        "player task started: start index {}, queue {} tracks, volume {:.2}",
        start_index,
        initial_queue.len(),
        volume
    );
    tokio::spawn(play(intent_rx, ctx, initial_queue, start_index));

    (state, cmd_tx)
}

// NOTE: owns the command channel: every command becomes an intent plus a
// cancellation of the live source, so a press never waits for the player
// task to reach a checkpoint
async fn supervise(
    mut cmd_rx: mpsc::Receiver<Command>,
    intents: mpsc::UnboundedSender<Intent>,
    state: Shared,
    playlists: SharedPlaylists,
    shuffle_flag: SharedShuffle,
    cancel_slot: Arc<Mutex<CancellationToken>>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::ToggleShuffle => {
                let now = !shuffle_flag.load(Ordering::Relaxed);
                shuffle_flag.store(now, Ordering::Relaxed);
                log_info!(
                    "shuffle turned {}: the current source keeps playing, the new order applies from the next track",
                    if now { "on" } else { "off" }
                );
            }
            Command::NextTrack => {
                if on_radio(&playlists, &state) {
                    log_debug!("command NextTrack: next track has no effect on a live stream");
                    continue;
                }
                log_info!("command NextTrack: skipping the current track");
                let _ = intents.send(Intent::SkipTrack);
                cancel_current(&cancel_slot);
            }
            Command::NextPlaylist | Command::PrevPlaylist => {
                let intent = if matches!(cmd, Command::NextPlaylist) {
                    Intent::NextPlaylist
                } else {
                    Intent::PrevPlaylist
                };
                log_info!("command {cmd:?}: playlist switch requested");
                let _ = intents.send(intent);
                cancel_current(&cancel_slot);
            }
        }
    }

    // NOTE: the player exits when the intent channel closes, so break out of
    // the live source first
    cancel_current(&cancel_slot);
    log_warn!("player command channel closed, stopping the current source");
}

async fn play(
    mut intents: mpsc::UnboundedReceiver<Intent>,
    ctx: Ctx,
    mut queue: Vec<PathBuf>,
    mut pl_idx: usize,
) {
    let mut order: Vec<usize> = (0..queue.len()).collect();
    let mut pos = 0usize;
    let mut switch: Option<usize> = None;
    let mut shuffle = ctx.shuffle.load(Ordering::Relaxed);
    if shuffle {
        fastrand::shuffle(&mut order);
    }
    log_info!(
        "player queue ready: {} tracks, shuffle {}",
        order.len(),
        shuffle
    );

    loop {
        // NOTE: a fresh token per iteration, installed before the intents are
        // drained, closes the window where a command could land between them
        let token = CancellationToken::new();
        *ctx.cancel.lock().expect("cancel slot poisoned") = token.clone();

        // NOTE: snapshot: additions via POST /api/playlists apply on the next iteration
        let playlists = ctx.playlists.read().expect("playlists poisoned").clone();
        let count = playlists.len();

        let mut skip = false;
        let mut quit = false;
        loop {
            match intents.try_recv() {
                Ok(intent) => resolve(intent, count, pl_idx, &mut switch, &mut skip),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    quit = true;
                    break;
                }
            }
        }
        if quit {
            log_warn!("player command channel closed, player task exiting");
            return;
        }
        if skip {
            pos += 1;
            log_debug!(
                "queue position advanced to {} of {} on a skip request",
                pos % order.len().max(1) + 1,
                order.len()
            );
        }

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
                        let mut s = ctx.state.write().expect("player state poisoned");
                        s.playlist_index = pl_idx;
                    }
                    ctx.epoch_tx.send_modify(|e| *e += 1);
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

        let flag = ctx.shuffle.load(Ordering::Relaxed);
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
                let mut s = ctx.state.write().expect("player state poisoned");
                s.current_track = Some(pl.name.clone());
            }
            log_info!("now playing: radio '{}' -> {}", pl.name, pl.path);
            match radio::run(&pl.path, &ctx.pcm_tx, &token).await {
                radio::Outcome::Interrupted => {
                    log_info!("radio stopped on request (playlist index {pl_idx})");
                    continue;
                }
                radio::Outcome::GaveUp => {
                    if count > 1 {
                        let next = (pl_idx + 1) % count;
                        log_warn!(
                            "radio gave up on '{}', moving to playlist index {next}",
                            pl.name
                        );
                        switch = Some(next);
                    } else {
                        log_warn!("radio gave up on '{}', the only playlist left", pl.name);
                    }
                    continue;
                }
                radio::Outcome::Closed => {
                    log_warn!("radio pipeline closed, player task exiting");
                    return;
                }
            }
        }

        if queue.is_empty() {
            log_debug!("queue empty on playlist index {pl_idx}, waiting for playlists to be added");
            tokio::select! {
                _ = token.cancelled() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
            }
            continue;
        }

        let queue_pos = order.get(pos % order.len()).copied().unwrap_or(0);
        let path = queue[queue_pos].clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        {
            let mut s = ctx.state.write().expect("player state poisoned");
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

        let pump_tx = ctx.pcm_tx.clone();
        let pump_cancel = token.clone();
        let mut handle = tokio::task::spawn_blocking(move || {
            pump::pump(Box::new(file), &pump_tx, &pump_cancel, 0)
        });

        let playlist_name = playlists
            .get(pl_idx)
            .map(|p| p.name.as_str())
            .unwrap_or("unknown");
        let mut advance = false;
        tokio::select! {
            _ = token.cancelled() => {
                let _ = handle.await;
                log_info!("track interrupted: {name} (playlist '{playlist_name}')");
            }
            result = &mut handle => {
                match result {
                    Ok(pump::Outcome::Exhausted) => {
                        log_info!("track finished: {name} (playlist '{playlist_name}')");
                        advance = true;
                    }
                    Ok(pump::Outcome::Interrupted) => {
                        log_info!("track interrupted: {name} (playlist '{playlist_name}')");
                    }
                    Ok(pump::Outcome::Failed(e)) => {
                        log_error!(
                            "playback of '{name}' failed: {e} | file: {} | playlist '{playlist_name}' (index {pl_idx})",
                            path.display()
                        );
                        advance = true;
                    }
                    Err(e) => {
                        log_error!(
                            "pump task panicked for '{name}' (file: {}): {e}",
                            path.display()
                        );
                        advance = true;
                    }
                }
            }
        }

        if advance && switch.is_none() {
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
}
