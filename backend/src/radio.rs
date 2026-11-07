use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::pump;

// NOTE: live sources arrive in real time, so the pipeline buffer stays near
// empty without an initial prebuffer; hold back this many blocks (~3 s)
// at session start to absorb network jitter
const PREBUFFER_BLOCKS: usize = 36;

// NOTE: a station that accepted the connection but never answered used to
// park the player until the OS gave up, so the whole request is capped here
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FAILURE_WAIT: Duration = Duration::from_secs(5);
const RECONNECT_WAIT: Duration = Duration::from_secs(2);
// NOTE: consecutive failed connections before the player moves to the next playlist
const FAILURE_LIMIT: u32 = 3;

pub enum Outcome {
    // NOTE: the player was asked to do something else
    Interrupted,
    GaveUp,
    // NOTE: the pcm consumer is gone
    Closed,
}

// NOTE: synchronous Read adapter over a tokio mpsc byte channel, so symphonia
// can demux a network stream inside a blocking thread.
struct ChannelReader {
    rx: Mutex<mpsc::Receiver<Bytes>>,
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

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("music-player/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|e| {
                log_error!("radio: cannot build the HTTP client: {e}");
                reqwest::Client::new()
            })
    })
}

async fn sleep_or_cancel(cancel: &CancellationToken, duration: Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

// NOTE: streams one radio station until the token is cancelled or the station
// keeps failing; reconnects on its own between those two events
pub async fn run(
    url: &str,
    pcm_tx: &mpsc::Sender<Vec<f32>>,
    cancel: &CancellationToken,
) -> Outcome {
    let mut failures: u32 = 0;
    loop {
        if pcm_tx.is_closed() {
            log_warn!("radio: pcm channel closed, stopping");
            return Outcome::Closed;
        }
        if cancel.is_cancelled() {
            return Outcome::Interrupted;
        }

        let attempt = failures + 1;
        let result = tokio::select! {
            _ = cancel.cancelled() => return Outcome::Interrupted,
            res = tokio::time::timeout(CONNECT_TIMEOUT, client().get(url).send()) => res,
        };
        let response = match result {
            Ok(Ok(r)) if r.status().is_success() => Ok(r),
            Ok(Ok(r)) => Err(format!("station returned HTTP {}", r.status())),
            Ok(Err(e)) => Err(format!("connection failed: {e}")),
            Err(_) => Err(format!("no response within {}s", CONNECT_TIMEOUT.as_secs())),
        };
        let response = match response {
            Ok(r) => r,
            Err(reason) => {
                failures += 1;
                log_error!("radio: {reason} (attempt {attempt}/{FAILURE_LIMIT})");
                if failures >= FAILURE_LIMIT {
                    log_warn!("radio: {failures} failed connections to {url}, giving up");
                    return Outcome::GaveUp;
                }
                if !sleep_or_cancel(cancel, FAILURE_WAIT).await {
                    return Outcome::Interrupted;
                }
                continue;
            }
        };

        log_info!("radio: connected to {url} (attempt {attempt})");
        failures = 0;
        let (byte_tx, byte_rx) = mpsc::channel::<Bytes>(128);
        let reader = ChannelReader {
            rx: Mutex::new(byte_rx),
            current: None,
            pos: 0,
        };
        let pump_tx = pcm_tx.clone();
        let pump_cancel = cancel.clone();
        let decode_handle = tokio::task::spawn_blocking(move || {
            pump::pump(Box::new(reader), &pump_tx, &pump_cancel, PREBUFFER_BLOCKS)
        });

        let mut stream = response.bytes_stream();
        let mut total_bytes: u64 = 0;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    log_info!("radio: stopping the stream after {total_bytes} bytes");
                    break;
                }
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            total_bytes += bytes.len() as u64;
                            if byte_tx.send(bytes).await.is_err() {
                                log_warn!("radio: decode side closed the byte channel");
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            log_error!("radio: stream error after {total_bytes} bytes: {e}");
                            break;
                        }
                        None => {
                            log_warn!("radio: station closed the stream after {total_bytes} bytes");
                            break;
                        }
                    }
                }
            }
        }

        drop(byte_tx);
        match decode_handle.await {
            Ok(pump::Outcome::Exhausted) => log_info!("radio: session ended, reconnecting"),
            Ok(pump::Outcome::Interrupted) => log_info!("radio: session interrupted"),
            Ok(pump::Outcome::Failed(e)) => {
                log_error!("radio: session failed: {e}, reconnecting")
            }
            Err(e) => log_error!("radio: decode task panicked: {e}"),
        }

        if cancel.is_cancelled() {
            return Outcome::Interrupted;
        }
        if pcm_tx.is_closed() {
            log_warn!("radio: pcm channel closed, stopping");
            return Outcome::Closed;
        }
        if !sleep_or_cancel(cancel, RECONNECT_WAIT).await {
            return Outcome::Interrupted;
        }
    }
}
