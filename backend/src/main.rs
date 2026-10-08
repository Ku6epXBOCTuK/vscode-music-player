use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::{routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

mod audio;
mod streamer;

#[derive(Serialize, Deserialize, Clone)]
struct Playlist {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct Config {
    current_playlist_index: usize,
    volume: f32,
    port: u16,
    playlists: Vec<Playlist>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            current_playlist_index: 0,
            volume: 0.8,
            port: 3000,
            playlists: Vec::new(),
        }
    }
}

fn config_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .expect("cannot locate executable")
        .parent()
        .expect("executable has no parent directory")
        .to_path_buf();
    exe_dir.join("config.json")
}

fn load_or_create_config() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("config.json is corrupted ({e}), falling back to defaults");
            Config::default()
        }),
        Err(_) => {
            let config = Config::default();
            let text = serde_json::to_string_pretty(&config).expect("cannot serialize config");
            std::fs::write(&path, text).expect("cannot write config.json");
            println!("created default config: {}", path.display());
            config
        }
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn stream_handler(State(state): State<Arc<streamer::SharedAudio>>) -> Response {
    let (mut rx, snapshot) = {
        let recent = state.recent.read().expect("recent buffer poisoned");
        let rx = state.tx.subscribe();
        let snapshot: Vec<bytes::Bytes> = recent.iter().cloned().collect();
        (rx, snapshot)
    };
    let header_bytes = state.header.clone();

    let body = async_stream::stream! {
        yield Ok::<bytes::Bytes, std::io::Error>(header_bytes);
        for chunk in snapshot {
            yield Ok(chunk);
        }
        loop {
            match rx.recv().await {
                Ok(chunk) => yield Ok(chunk),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("stream client lagged by {n} chunks");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/flac")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .body(Body::from_stream(body))
        .expect("failed to build stream response")
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("decode") {
        let path = PathBuf::from(
            args.get(2).map(String::as_str).unwrap_or("testdata/sample.mp3"),
        );
        match audio::decode_file(&path) {
            Ok(stats) => {
                println!("decoded: {}", path.display());
                println!("  sample rate: {} Hz", stats.sample_rate);
                println!("  channels:    {}", stats.channels);
                println!("  frames:      {}", stats.total_frames);
                println!("  duration:    {:.2} s", stats.duration_secs);
            }
            Err(e) => {
                eprintln!("decode failed for {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        match audio::decode_to_pcm(&path, 0.8) {
            Ok(pcm) => {
                println!(
                    "resampled:   {} frames @ {} Hz stereo (gain 0.8)",
                    pcm.len() / 2,
                    audio::TARGET_RATE
                );
            }
            Err(e) => {
                eprintln!("resample failed for {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    let config = load_or_create_config();
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));

    let pcm = match audio::decode_to_pcm(std::path::Path::new("testdata/sample.mp3"), config.volume)
    {
        Ok(pcm) => pcm,
        Err(e) => {
            eprintln!("failed to decode testdata/sample.mp3: {e}");
            std::process::exit(1);
        }
    };
    let shared = match streamer::start_broadcast(pcm) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("failed to start audio broadcast: {e}");
            std::process::exit(1);
        }
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/stream", get(stream_handler))
        .layer(CorsLayer::permissive())
        .with_state(shared);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind port {}: {e}", config.port);
            eprintln!("the backend may already be running, or another app is using this port");
            std::process::exit(1);
        }
    };

    println!("music-player backend listening on http://{addr}");
    axum::serve(listener, app).await.expect("server error");
}
