#![windows_subsystem = "windows"]

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, Response};
use axum::{routing::get, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

mod audio;
mod player;
mod pump;
mod radio;
mod streamer;
mod tray;

use player::Playlist;

#[cfg(target_os = "windows")]
fn attach_parent_console() {
    // the binary is built with windows_subsystem (no console window), but
    // when started from a terminal we still want log output there
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn attach_parent_console() {}

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

struct AppState {
    audio: Arc<streamer::SharedAudio>,
    player: player::Shared,
    playlists: player::SharedPlaylists,
    port: u16,
    cmd_tx: tokio::sync::mpsc::Sender<player::Command>,
    shutdown: CancellationToken,
}

fn persist_config(port: u16, player: &player::Shared, playlists: &player::SharedPlaylists) {
    let (volume, index) = {
        let s = player.read().expect("player state poisoned");
        (s.volume, s.playlist_index)
    };
    let config = Config {
        current_playlist_index: index,
        volume,
        port,
        playlists: playlists.read().expect("playlists poisoned").clone(),
    };
    let path = config_path();
    match serde_json::to_string_pretty(&config) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("failed to save config to {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("failed to serialize config: {e}"),
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn stream_handler(State(state): State<Arc<AppState>>) -> Response {
    let (mut rx, snapshot) = {
        let recent = state.audio.recent.read().expect("recent buffer poisoned");
        let rx = state.audio.tx.subscribe();
        let snapshot: Vec<bytes::Bytes> = recent.iter().cloned().collect();
        (rx, snapshot)
    };
    let header_bytes = state.audio.header.clone();

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

async fn shutdown_handler(State(state): State<Arc<AppState>>) -> StatusCode {
    state.shutdown.cancel();
    println!("shutdown requested via /api/shutdown");
    StatusCode::OK
}

#[derive(Deserialize)]
struct ControlRequest {
    action: String,
}

async fn control_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ControlRequest>,
) -> StatusCode {
    match req.action.as_str() {
        "play" | "pause" | "stop" => {
            let status = match req.action.as_str() {
                "play" => player::Status::Playing,
                "pause" => player::Status::Paused,
                _ => player::Status::Stopped,
            };
            let mut s = state.player.write().expect("player state poisoned");
            s.status = status;
            println!("control: {status:?}");
            StatusCode::OK
        }
        "next_playlist" | "prev_playlist" => {
            let cmd = if req.action == "next_playlist" {
                player::Command::NextPlaylist
            } else {
                player::Command::PrevPlaylist
            };
            match state.cmd_tx.try_send(cmd) {
                Ok(()) => StatusCode::OK,
                Err(e) => {
                    eprintln!("failed to queue playlist switch: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

#[derive(Deserialize)]
struct VolumeRequest {
    volume: f32,
}

async fn volume_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VolumeRequest>,
) -> StatusCode {
    if !(0.0..=1.0).contains(&req.volume) {
        return StatusCode::BAD_REQUEST;
    }
    let mut s = state.player.write().expect("player state poisoned");
    s.volume = req.volume;
    println!("volume: {:.2}", req.volume);
    StatusCode::OK
}

async fn now_playing_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let s = state.player.read().expect("player state poisoned");
    Json(serde_json::json!({
        "status": s.status.as_str(),
        "track": s.current_track,
        "volume": s.volume,
        "playlist_index": s.playlist_index,
    }))
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn list_playlists_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let playlists = state.playlists.read().expect("playlists poisoned");
    let current = state.player.read().expect("player state poisoned").playlist_index;
    Json(serde_json::json!({
        "current": current,
        "playlists": *playlists,
    }))
}

#[derive(Deserialize)]
struct AddPlaylistRequest {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    path: String,
}

async fn add_playlist_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddPlaylistRequest>,
) -> StatusCode {
    let name = req.name.trim();
    let path = req.path.trim();
    if name.is_empty() || path.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    match req.kind.as_str() {
        "local_folder" => match audio::scan_folder(std::path::Path::new(path)) {
            Ok(files) if !files.is_empty() => {}
            _ => return StatusCode::BAD_REQUEST,
        },
        "network_radio" => {
            if !(path.starts_with("http://") || path.starts_with("https://")) {
                return StatusCode::BAD_REQUEST;
            }
        }
        _ => return StatusCode::BAD_REQUEST,
    }

    {
        let mut playlists = state.playlists.write().expect("playlists poisoned");
        playlists.push(Playlist {
            name: name.to_string(),
            kind: req.kind,
            path: path.to_string(),
        });
    }
    println!("added playlist: {name} ({path})");
    persist_config(state.port, &state.player, &state.playlists);
    StatusCode::OK
}

async fn shutdown_signal(token: CancellationToken, quiet: bool) {
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(e) = result {
                eprintln!("failed to listen for Ctrl+C: {e}");
            }
            if !quiet {
                println!("shutdown requested via Ctrl+C");
            }
        }
        _ = token.cancelled() => {}
    }
}

#[tokio::main]
async fn main() {
    attach_parent_console();

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

    let queue = match config.playlists.get(config.current_playlist_index) {
        Some(pl) if pl.kind == "network_radio" => Vec::new(),
        Some(pl) if pl.kind == "local_folder" => {
            match audio::scan_folder(std::path::Path::new(&pl.path)) {
                Ok(files) if !files.is_empty() => files,
                Ok(_) => {
                    eprintln!("playlist folder is empty: {}", pl.path);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("failed to scan playlist folder {}: {e}", pl.path);
                    std::process::exit(1);
                }
            }
        }
        Some(pl) => {
            eprintln!("unknown playlist type: {}", pl.kind);
            std::process::exit(1);
        }
        None => {
            println!("no playlists configured, falling back to testdata/");
            match audio::scan_folder(std::path::Path::new("testdata")) {
                Ok(files) if !files.is_empty() => files,
                Ok(_) => {
                    eprintln!("no audio files in testdata/");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("failed to scan testdata/: {e}");
                    std::process::exit(1);
                }
            }
        }
    };
    println!("playlist: {} tracks", queue.len());

    let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel::<Vec<f32>>(128);
    let (epoch_tx, epoch_rx) = tokio::sync::watch::channel(0u64);
    let playlists_shared: player::SharedPlaylists =
        Arc::new(RwLock::new(config.playlists.clone()));
    let (player_state, cmd_tx) = player::spawn(
        playlists_shared.clone(),
        config.current_playlist_index,
        queue,
        config.volume,
        pcm_tx,
        epoch_tx,
    );

    let shared = match streamer::start_encoder(pcm_rx, player_state.clone(), epoch_rx) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("failed to start audio broadcast: {e}");
            std::process::exit(1);
        }
    };

    let shutdown = CancellationToken::new();
    let state = Arc::new(AppState {
        audio: shared,
        player: player_state.clone(),
        playlists: playlists_shared.clone(),
        port: config.port,
        cmd_tx,
        shutdown: shutdown.clone(),
    });

    tray::run(config.port, player_state.clone(), shutdown.clone());

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/health", get(health))
        .route("/api/control", post(control_handler))
        .route("/api/volume", post(volume_handler))
        .route("/api/now_playing", get(now_playing_handler))
        .route(
            "/api/playlists",
            get(list_playlists_handler).post(add_playlist_handler),
        )
        .route("/api/shutdown", post(shutdown_handler))
        .route("/stream", get(stream_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind port {}: {e}", config.port);
            eprintln!("the backend may already be running, or another app is using this port");
            std::process::exit(1);
        }
    };

    println!("music-player backend listening on http://{addr}");

    let server_shutdown = shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(server_shutdown, true))
            .await
    });

    shutdown_signal(shutdown, false).await;

    // stream clients (OBS, VLC) hold infinite connections; do not wait for them
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), server).await;

    let path = config_path();
    let mut config = config;
    {
        let s = player_state.read().expect("player state poisoned");
        config.volume = s.volume;
        config.current_playlist_index = s.playlist_index;
    }
    config.playlists = playlists_shared
        .read()
        .expect("playlists poisoned")
        .clone();
    match serde_json::to_string_pretty(&config) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                eprintln!("failed to save config to {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("failed to serialize config: {e}"),
    }
    println!("shutdown complete");
}
