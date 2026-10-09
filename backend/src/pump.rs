use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::sync::mpsc;

use crate::audio;
use crate::streamer::BLOCK_SIZE;

const RESAMPLE_CHUNK: usize = 4096;

pub enum Outcome {
    // source ended (file finished / stream closed)
    Exhausted,
    // cancelled via the flag (playlist switch)
    Interrupted,
    Failed(String),
}

// Decodes any MediaSource and feeds interleaved 48 kHz stereo PCM blocks
// into the pipeline channel as they are produced. Runs in a blocking
// thread; `cancel` is polled between blocks and before every send.
pub fn pump(
    source: Box<dyn MediaSource>,
    pcm_tx: &mpsc::Sender<Vec<f32>>,
    cancel: &AtomicBool,
    prebuffer_blocks: usize,
) -> Outcome {
    let mss = MediaSourceStream::new(source, Default::default());
    let probed = match symphonia::default::get_probe().format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => return Outcome::Failed(format!("probe failed: {e}")),
    };
    let mut format = probed.format;

    let track = match format.default_track() {
        Some(t) => t,
        None => return Outcome::Failed("no default audio track".to_string()),
    };
    let track_id = track.id;
    let mut decoder = match symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
    {
        Ok(d) => d,
        Err(e) => return Outcome::Failed(format!("codec init failed: {e}")),
    };

    let mut in_rate: u32 = 0;
    let mut resampler: Option<FftFixedIn<f32>> = None;
    let mut left_q: VecDeque<f32> = VecDeque::new();
    let mut right_q: VecDeque<f32> = VecDeque::new();
    let mut out: VecDeque<f32> = VecDeque::new();
    let mut prebuffer: Vec<Vec<f32>> = Vec::with_capacity(prebuffer_blocks);
    let mut prebuffering = prebuffer_blocks > 0;
    let started = std::time::Instant::now();
    let mut blocks_sent: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Outcome::Interrupted;
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(_)) => {
                println!(
                    "source exhausted after {blocks_sent} blocks ({:.1}s of audio pumped)",
                    started.elapsed().as_secs_f64()
                );
                return Outcome::Exhausted;
            }
            Err(e) => return Outcome::Failed(format!("demux failed: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                eprintln!("skipping corrupted packet: {e}");
                continue;
            }
            Err(e) => return Outcome::Failed(format!("decode failed: {e}")),
        };

        let spec = *decoded.spec();
        if spec.rate != in_rate {
            in_rate = spec.rate;
            left_q.clear();
            right_q.clear();
            resampler = if in_rate != audio::TARGET_RATE {
                match FftFixedIn::<f32>::new(
                    in_rate as usize,
                    audio::TARGET_RATE as usize,
                    RESAMPLE_CHUNK,
                    2,
                    2,
                ) {
                    Ok(r) => Some(r),
                    Err(e) => return Outcome::Failed(format!("resampler init failed: {e}")),
                }
            } else {
                None
            };
        }

        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        let inter = buf.samples();
        for f in 0..inter.len() / channels {
            let l = inter[f * channels];
            let r = if channels > 1 { inter[f * channels + 1] } else { l };
            left_q.push_back(l);
            right_q.push_back(r);
        }

        match &mut resampler {
            Some(rs) => {
                while left_q.len() >= RESAMPLE_CHUNK {
                    let l: Vec<f32> = left_q.drain(..RESAMPLE_CHUNK).collect();
                    let r: Vec<f32> = right_q.drain(..RESAMPLE_CHUNK).collect();
                    let result = match rs.process(&[l, r], None) {
                        Ok(r) => r,
                        Err(e) => return Outcome::Failed(format!("resample failed: {e}")),
                    };
                    for (l, r) in result[0].iter().zip(result[1].iter()) {
                        out.push_back(*l);
                        out.push_back(*r);
                    }
                }
            }
            None => {
                while let (Some(l), Some(r)) = (left_q.pop_front(), right_q.pop_front()) {
                    out.push_back(l);
                    out.push_back(r);
                }
            }
        }

        while out.len() >= BLOCK_SIZE * 2 {
            let block: Vec<f32> = out.drain(..BLOCK_SIZE * 2).collect();
            if prebuffering {
                prebuffer.push(block);
                if prebuffer.len() >= prebuffer_blocks {
                    for buffered in prebuffer.drain(..) {
                        if !send_block(pcm_tx, buffered, cancel) {
                            return finish(cancel);
                        }
                    }
                    prebuffering = false;
                }
                continue;
            }
            if !send_block(pcm_tx, block, cancel) {
                return finish(cancel);
            }
            blocks_sent += 1;
        }
    }
}

fn finish(cancel: &AtomicBool) -> Outcome {
    if cancel.load(Ordering::Relaxed) {
        Outcome::Interrupted
    } else {
        Outcome::Failed("pcm channel closed".to_string())
    }
}

// try_send with a wait loop so a full channel (paused playback) does not
// block cancellation forever
fn send_block(pcm_tx: &mpsc::Sender<Vec<f32>>, block: Vec<f32>, cancel: &AtomicBool) -> bool {
    let mut block = block;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match pcm_tx.try_send(block) {
            Ok(()) => return true,
            Err(mpsc::error::TrySendError::Full(b)) => {
                block = b;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
}
