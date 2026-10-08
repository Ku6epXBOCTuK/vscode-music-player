use axum::{routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::cors::CorsLayer;

mod audio;

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
        return;
    }

    let config = load_or_create_config();
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));

    let app = Router::new()
        .route("/api/health", get(health))
        .layer(CorsLayer::permissive());

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
