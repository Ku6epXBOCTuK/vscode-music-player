use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::player::Command;
use crate::pump;

// live sources arrive in real time, so the pipeline buffer stays near
// empty without an initial prebuffer; hold back this many blocks (~3 s)
// at session start to absorb network jitter
const PREBUFFER_BLOCKS: usize = 36;

// Synchronous Read adapter over a tokio mpsc byte channel, so symphonia
// can demux a network stream inside a blocking thread.
struct ChannelReader {
    rx: std::sync::Mutex<mpsc::Receiver<Bytes>>,
    current: Option<Bytes>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(cur) = &self.current
                && self.pos < cur.len()
            {
                let n = (cur.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&cur[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            let next = {
                let mut rx = self.rx.lock().expect("byte channel poisoned");
                rx.blocking_recv()
            };
            match next {
                Some(chunk) => {
                    self.current = Some(chunk);
                    self.pos = 0;
                }
                None => return Ok(0),
            }
        }
    }
}

impl std::io::Seek for ChannelReader {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "network streams are not seekable",
        ))
    }
}

impl symphonia::core::io::MediaSource for ChannelReader {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

// Streams one radio station until a playlist switch is requested.
// Returns the new playlist index, or None when the pcm channel is closed.
pub async fn run(
    url: &str,
    pcm_tx: &mpsc::Sender<Vec<f32>>,
    cmd_rx: &mut mpsc::Receiver<Command>,
    playlist_count: usize,
    pl_idx: usize,
) -> Option<usize> {
    loop {
        let attempt = {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        };
        let response = match reqwest::get(url).await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                eprintln!("radio: station returned HTTP {} (attempt {attempt})", r.status());
                if let Some(next) = wait_or_switch(cmd_rx, playlist_count, pl_idx, 5).await {
                    return Some(next);
                }
                continue;
            }
            Err(e) => {
                eprintln!("radio: connection failed (attempt {attempt}): {e}");
                if let Some(next) = wait_or_switch(cmd_rx, playlist_count, pl_idx, 5).await {
                    return Some(next);
                }
                continue;
            }
        };

        println!("radio: connected to {url}");
        let (byte_tx, byte_rx) = mpsc::channel::<Bytes>(128);
        let reader = ChannelReader {
            rx: std::sync::Mutex::new(byte_rx),
            current: None,
            pos: 0,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let pump_cancel = cancel.clone();
        let pump_tx = pcm_tx.clone();
        let decode_handle = tokio::task::spawn_blocking(move || {
            pump::pump(Box::new(reader), &pump_tx, &pump_cancel, PREBUFFER_BLOCKS)
        });

        let mut stream = response.bytes_stream();
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    cancel.store(true, Ordering::Relaxed);
                    drop(byte_tx);
                    return cmd.map(|c| match c {
                        Command::NextPlaylist => (pl_idx + 1) % playlist_count,
                        Command::PrevPlaylist => (pl_idx + playlist_count - 1) % playlist_count,
                    });
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            if byte_tx.send(bytes).await.is_err() {
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            eprintln!("radio: stream error: {e}");
                            break;
                        }
                        None => {
                            eprintln!("radio: station closed the stream");
                            break;
                        }
                    }
                }
            }
        }

        drop(byte_tx);
        match decode_handle.await {
            Ok(pump::Outcome::Exhausted) => eprintln!("radio: session ended, reconnecting"),
            Ok(pump::Outcome::Interrupted) => eprintln!("radio: session interrupted"),
            Ok(pump::Outcome::Failed(e)) => eprintln!("radio: session failed: {e}, reconnecting"),
            Err(e) => eprintln!("radio: decode task panicked: {e}"),
        }

        if let Some(next) = wait_or_switch(cmd_rx, playlist_count, pl_idx, 2).await {
            return Some(next);
        }
    }
}

// Waits `secs` for a playlist switch command; returns None on timeout.
async fn wait_or_switch(
    cmd_rx: &mut mpsc::Receiver<Command>,
    playlist_count: usize,
    pl_idx: usize,
    secs: u64,
) -> Option<usize> {
    let wait = tokio::time::sleep(std::time::Duration::from_secs(secs));
    tokio::select! {
        cmd = cmd_rx.recv() => cmd.map(|c| match c {
            Command::NextPlaylist => (pl_idx + 1) % playlist_count,
            Command::PrevPlaylist => (pl_idx + playlist_count - 1) % playlist_count,
        }),
        _ = wait => None,
    }
}
