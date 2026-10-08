use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::{audio, pump};

#[derive(Serialize, Deserialize, Clone)]
pub struct Playlist {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
}

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

pub enum Command {
    NextPlaylist,
    PrevPlaylist,
}

pub struct PlayerState {
    pub status: Status,
    pub volume: f32,
    pub current_track: Option<String>,
    pub playlist_index: usize,
}

pub type Shared = Arc<RwLock<PlayerState>>;
pub type SharedPlaylists = Arc<RwLock<Vec<Playlist>>>;

fn build_queue(playlist: &Playlist) -> Result<Vec<PathBuf>, String> {
    match playlist.kind.as_str() {
        "local_folder" => {
            let files = audio::scan_folder(std::path::Path::new(&playlist.path))
                .map_err(|e| e.to_string())?;
            if files.is_empty() {
                Err(format!("folder has no audio files: {}", playlist.path))
            } else {
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
        let mut queue = initial_queue;
        let mut pl_idx = start_index;
        let mut pos = 0usize;
        let mut switch: Option<usize> = None;

        loop {
            // snapshot: additions via POST /api/playlists apply on the next iteration
            let playlists = task_playlists.read().expect("playlists poisoned").clone();

            if let Some(new_idx) = switch.take() {
                match playlists.get(new_idx).map(build_queue) {
                    Some(Ok(q)) => {
                        queue = q;
                        pl_idx = new_idx;
                        pos = 0;
                        {
                            let mut s = task_state.write().expect("player state poisoned");
                            s.playlist_index = pl_idx;
                        }
                        epoch_tx.send_modify(|e| *e += 1);
                        println!("switched playlist: {}", playlists[pl_idx].name);
                    }
                    Some(Err(e)) => {
                        eprintln!("cannot switch to {}: {e}", playlists[new_idx].name)
                    }
                    None => eprintln!("playlist index {new_idx} no longer exists"),
                }
            }

            if let Some(pl) = playlists.get(pl_idx)
                && pl.kind == "network_radio"
            {
                {
                    let mut s = task_state.write().expect("player state poisoned");
                    s.current_track = Some(pl.name.clone());
                }
                println!("now playing: radio {}", pl.name);
                match crate::radio::run(&pl.path, &pcm_tx, &mut cmd_rx, playlists.len(), pl_idx)
                    .await
                {
                    Some(new_idx) => {
                        switch = Some(new_idx);
                        continue;
                    }
                    None => return,
                }
            }

            if queue.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }

            let path = queue[pos % queue.len()].clone();
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            {
                let mut s = task_state.write().expect("player state poisoned");
                s.current_track = Some(name.clone());
            }
            println!("now playing: {name}");

            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("failed to open {name}: {e}");
                    pos += 1;
                    continue;
                }
            };

            let cancel = Arc::new(AtomicBool::new(false));
            let pump_cancel = cancel.clone();
            let pump_tx = pcm_tx.clone();
            let mut handle = tokio::task::spawn_blocking(move || {
                pump::pump(Box::new(file), &pump_tx, &pump_cancel, 0)
            });

            tokio::select! {
                cmd = cmd_rx.recv() => {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = handle.await;
                    if playlists.is_empty() {
                        eprintln!("ignoring playlist switch: no playlists configured");
                    } else if let Some(c) = cmd {
                        let next = match c {
                            Command::NextPlaylist => (pl_idx + 1) % playlists.len(),
                            Command::PrevPlaylist => {
                                (pl_idx + playlists.len() - 1) % playlists.len()
                            }
                        };
                        switch = Some(next);
                    }
                }
                result = &mut handle => {
                    match result {
                        Ok(pump::Outcome::Exhausted) => {}
                        Ok(pump::Outcome::Interrupted) => {}
                        Ok(pump::Outcome::Failed(e)) => {
                            eprintln!("playback of {name} failed: {e}")
                        }
                        Err(e) => eprintln!("pump task panicked for {name}: {e}"),
                    }
                }
            }

            if switch.is_none() {
                pos += 1;
            }
        }
    });

    (state, cmd_tx)
}
