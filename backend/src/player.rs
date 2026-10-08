use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use tokio::sync::mpsc;

use crate::{audio, streamer};

// Paused and Stopped are driven by /api/control, wired up in the next step
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

pub struct PlayerState {
    pub status: Status,
    pub volume: f32,
    pub current_track: Option<String>,
}

pub type Shared = Arc<RwLock<PlayerState>>;

pub fn spawn(queue: Vec<PathBuf>, volume: f32, pcm_tx: mpsc::Sender<Vec<f32>>) -> Shared {
    let state = Arc::new(RwLock::new(PlayerState {
        status: Status::Playing,
        volume,
        current_track: None,
    }));

    let task_state = state.clone();
    tokio::spawn(async move {
        let mut idx = 0usize;
        loop {
            let path = queue[idx % queue.len()].clone();
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
            idx += 1;
        }
    });

    state
}
