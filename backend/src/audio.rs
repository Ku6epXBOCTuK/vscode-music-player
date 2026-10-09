use std::error::Error;
use std::fs::File;
use std::path::{Path, PathBuf};

use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const TARGET_RATE: u32 = 48000;
const RESAMPLE_CHUNK: usize = 4096;

pub struct DecodeStats {
    pub sample_rate: u32,
    pub channels: usize,
    pub total_frames: u64,
    pub duration_secs: f64,
}

struct RawPcm {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: usize,
}

fn decode_raw(path: &Path) -> Result<RawPcm, Box<dyn Error + Send + Sync>> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;

    let track = format.default_track().ok_or("no default audio track")?;
    let track_id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut sample_rate = 0;
    let mut channels = 0;
    let mut samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(_)) => break,
            Err(e) => return Err(Box::new(e)),
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
            Err(e) => return Err(Box::new(e)),
        };

        let spec = *decoded.spec();
        sample_rate = spec.rate;
        channels = spec.channels.count();

        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
    }

    Ok(RawPcm {
        samples,
        sample_rate,
        channels,
    })
}

pub fn decode_file(path: &Path) -> Result<DecodeStats, Box<dyn Error + Send + Sync>> {
    let raw = decode_raw(path)?;
    let total_frames = (raw.samples.len() / raw.channels.max(1)) as u64;
    let duration_secs = if raw.sample_rate > 0 {
        total_frames as f64 / raw.sample_rate as f64
    } else {
        0.0
    };

    Ok(DecodeStats {
        sample_rate: raw.sample_rate,
        channels: raw.channels,
        total_frames,
        duration_secs,
    })
}

fn to_stereo(samples: &[f32], channels: usize) -> [Vec<f32>; 2] {
    let frames = samples.len() / channels.max(1);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);

    for frame in 0..frames {
        let l = samples[frame * channels];
        let r = if channels > 1 {
            samples[frame * channels + 1]
        } else {
            l
        };
        left.push(l);
        right.push(r);
    }

    [left, right]
}

fn interleave(left: &[f32], right: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(left.len() * 2);
    for i in 0..left.len() {
        out.push(left[i]);
        out.push(right[i]);
    }
    out
}

fn resample_to_target(left: Vec<f32>, right: Vec<f32>, in_rate: u32) -> Result<Vec<f32>, Box<dyn Error + Send + Sync>> {
    if in_rate == TARGET_RATE {
        return Ok(interleave(&left, &right));
    }

    let mut resampler =
        FftFixedIn::<f32>::new(in_rate as usize, TARGET_RATE as usize, RESAMPLE_CHUNK, 2, 2)?;

    let frames = left.len();
    let mut out_left: Vec<f32> = Vec::new();
    let mut out_right: Vec<f32> = Vec::new();

    let mut pos = 0;
    while pos < frames {
        let end = (pos + RESAMPLE_CHUNK).min(frames);
        let input = vec![left[pos..end].to_vec(), right[pos..end].to_vec()];

        let result = if end - pos < RESAMPLE_CHUNK {
            resampler.process_partial(Some(&input), None)?
        } else {
            resampler.process(&input, None)?
        };

        out_left.extend_from_slice(&result[0]);
        out_right.extend_from_slice(&result[1]);
        pos = end;
    }

    Ok(interleave(&out_left, &out_right))
}

pub fn decode_to_pcm(path: &Path, gain: f32) -> Result<Vec<f32>, Box<dyn Error + Send + Sync>> {
    let raw = decode_raw(path)?;
    if raw.channels == 0 || raw.sample_rate == 0 {
        return Err("decoded file has no audio".into());
    }

    let [left, right] = to_stereo(&raw.samples, raw.channels);
    let mut pcm = resample_to_target(left, right, raw.sample_rate)?;

    if gain != 1.0 {
        for s in pcm.iter_mut() {
            *s *= gain;
        }
    }

    Ok(pcm)
}

const AUDIO_EXTENSIONS: [&str; 6] = ["mp3", "flac", "wav", "aac", "m4a", "ogg"];

// Recursively collects audio files under `dir`, sorted by path.
pub fn scan_folder(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error + Send + Sync>> {
    fn visit(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit(&path, out)?;
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
            {
                out.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    let started = std::time::Instant::now();
    visit(dir, &mut files)?;
    files.sort();
    println!(
        "scanned {}: {} audio files in {:.1}s",
        dir.display(),
        files.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(files)
}
