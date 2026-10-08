use std::collections::VecDeque;
use std::error::Error;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use bytes::Bytes;
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, Stream};
use flacenc::config::Encoder as EncoderConfig;
use flacenc::encode_fixed_size_frame;
use flacenc::error::Verify;
use flacenc::source::{Fill, FrameBuf};
use tokio::sync::broadcast;

use crate::audio;

const BLOCK_SIZE: usize = 4096;
const BROADCAST_CAPACITY: usize = 512;
const RECENT_CAPACITY: usize = 240;

pub struct SharedAudio {
    pub tx: broadcast::Sender<Bytes>,
    pub header: Bytes,
    pub recent: Arc<RwLock<VecDeque<Bytes>>>,
}

pub fn start_broadcast(pcm: Vec<f32>) -> Result<SharedAudio, Box<dyn Error>> {
    let samples: Vec<i32> = pcm
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i32)
        .collect();
    let total_frames = samples.len() / 2;
    if total_frames < BLOCK_SIZE {
        return Err("audio is shorter than one FLAC block".into());
    }

    let encoder_config = EncoderConfig::default()
        .into_verified()
        .map_err(|(_, e)| format!("invalid FLAC encoder config: {e}"))?;

    let mut stream = Stream::new(audio::TARGET_RATE as usize, 2, 16)
        .map_err(|e| format!("failed to create FLAC stream: {e}"))?;
    stream
        .stream_info_mut()
        .set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
        .map_err(|e| format!("invalid FLAC block size: {e}"))?;
    stream
        .stream_info_mut()
        .set_frame_sizes(0, 0)
        .map_err(|e| format!("invalid FLAC frame size: {e}"))?;
    let stream_info = stream.stream_info().clone();

    let mut header_sink = ByteSink::new();
    stream
        .write(&mut header_sink)
        .map_err(|e| format!("FLAC header write failed: {e}"))?;
    let header = Bytes::copy_from_slice(header_sink.as_slice());

    let (tx, _) = broadcast::channel::<Bytes>(BROADCAST_CAPACITY);
    let recent: Arc<RwLock<VecDeque<Bytes>>> = Arc::new(RwLock::new(VecDeque::new()));

    let producer_tx = tx.clone();
    let producer_recent = recent.clone();
    tokio::spawn(async move {
        let block_duration =
            Duration::from_secs_f64(BLOCK_SIZE as f64 / audio::TARGET_RATE as f64);
        let mut next_deadline = tokio::time::Instant::now();
        let mut frame_number: usize = 0;
        let mut framebuf = match FrameBuf::with_size(2, BLOCK_SIZE) {
            Ok(fb) => fb,
            Err(e) => {
                eprintln!("failed to allocate FLAC frame buffer: {e}");
                return;
            }
        };

        loop {
            let start = (frame_number * BLOCK_SIZE) % total_frames;
            let mut block = Vec::with_capacity(BLOCK_SIZE * 2);
            for f in 0..BLOCK_SIZE {
                let idx = ((start + f) % total_frames) * 2;
                block.push(samples[idx]);
                block.push(samples[idx + 1]);
            }

            if let Err(e) = framebuf.fill_interleaved(&block) {
                eprintln!("failed to fill FLAC frame buffer: {e}");
                return;
            }

            let frame = match encode_fixed_size_frame(
                &encoder_config,
                &framebuf,
                frame_number,
                &stream_info,
            ) {
                Ok(frame) => frame,
                Err(e) => {
                    eprintln!("FLAC frame encode failed: {e}");
                    frame_number += 1;
                    continue;
                }
            };
            frame_number = (frame_number + 1) % (1usize << 31);

            let mut sink = ByteSink::new();
            if let Err(e) = frame.write(&mut sink) {
                eprintln!("FLAC frame write failed: {e}");
                continue;
            }
            let bytes = Bytes::copy_from_slice(sink.as_slice());

            let _ = producer_tx.send(bytes.clone());
            {
                let mut queue = producer_recent.write().expect("recent buffer poisoned");
                queue.push_back(bytes);
                while queue.len() > RECENT_CAPACITY {
                    queue.pop_front();
                }
            }

            next_deadline += block_duration;
            tokio::time::sleep_until(next_deadline).await;
        }
    });

    Ok(SharedAudio { tx, header, recent })
}
