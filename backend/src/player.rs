use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{audio, streamer};

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
        "network_radio" => Err("network radio is not implemented yet".to_string()),
        other => Err(format!("unknown playlist type: {other}")),
    }
}

pub fn spawn(
    playlists: Vec<Playlist>,
    start_index: usize,
    initial_queue: Vec<PathBuf>,
    volume: f32,
    pcm_tx: mpsc::Sender<Vec<f32>>,
) -> (Shared, mpsc::Sender<Command>) {
    let state = Arc::new(RwLock::new(PlayerState {
        status: Status::Playing,
        volume,
        current_track: None,
        playlist_index: start_index,
    }));
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Command>(32);

    let task_state = state.clone();
    tokio::spawn(async move {
        let mut queue = initial_queue;
        let mut pl_idx = start_index;
        let mut pos = 0usize;
        let mut switch: Option<usize> = None;

        loop {
            if let Some(new_idx) = switch.take() {
                match build_queue(&playlists[new_idx]) {
                    Ok(q) => {
                        queue = q;
                        pl_idx = new_idx;
                        pos = 0;
                        {
                            let mut s = task_state.write().expect("player state poisoned");
                            s.playlist_index = pl_idx;
                        }
                        println!("switched playlist: {}", playlists[pl_idx].name);
                    }
                    Err(e) => eprintln!("cannot switch to {}: {e}", playlists[new_idx].name),
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

            let decoded =
                tokio::task::spawn_blocking(move || audio::decode_to_pcm(&path, 1.0)).await;
            match decoded {
                Ok(Ok(pcm)) => {
                    for block in pcm.chunks(streamer::BLOCK_SIZE * 2) {
                        while let Ok(cmd) = cmd_rx.try_recv() {
                            if playlists.is_empty() {
                                eprintln!("ignoring playlist switch: no playlists configured");
                                continue;
                            }
                            let next = match cmd {
                                Command::NextPlaylist => (pl_idx + 1) % playlists.len(),
                                Command::PrevPlaylist => {
                                    (pl_idx + playlists.len() - 1) % playlists.len()
                                }
                            };
                            switch = Some(next);
                        }
                        if switch.is_some() {
                            break;
                        }

                        let mut padded = block.to_vec();
                        padded.resize(streamer::BLOCK_SIZE * 2, 0.0);
                        if pcm_tx.send(padded).await.is_err() {
                            return;
                        }
                    }
                }
                Ok(Err(e)) => eprintln!("failed to decode {name}: {e}"),
                Err(e) => eprintln!("decode task failed for {name}: {e}"),
            }

            if switch.is_none() {
                pos += 1;
            }
        }
    });

    (state, cmd_tx)
}
