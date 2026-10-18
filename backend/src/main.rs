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

#[macro_use]
mod log;
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
    #[serde(default)]
    shuffle: bool,
    playlists: Vec<Playlist>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            current_playlist_index: 0,
            volume: 0.8,
            port: 45880,
            shuffle: false,
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
        Ok(text) => match serde_json::from_str(&text) {
            Ok(config) => {
                log_info!("loaded config: {}", path.display());
                config
            }
            Err(e) => {
                log_error!("config.json is corrupted ({e}), falling back to defaults");
                Config::default()
            }
        },
        Err(e) => {
            log_info!("no config at {} ({e}), creating defaults", path.display());
            let config = Config::default();
            let text = serde_json::to_string_pretty(&config).expect("cannot serialize config");
            if let Err(e) = std::fs::write(&path, text) {
                log_error!("cannot write config.json: {e}");
            }
            config
        }
    }
}

struct AppState {
    audio: Arc<streamer::SharedAudio>,
    player: player::Shared,
    playlists: player::SharedPlaylists,
    shuffle: player::SharedShuffle,
    port: u16,
    cmd_tx: tokio::sync::mpsc::Sender<player::Command>,
    shutdown: CancellationToken,
}

fn persist_config(
    port: u16,
    player: &player::Shared,
    playlists: &player::SharedPlaylists,
    shuffle: &player::SharedShuffle,
) {
    let (volume, index) = {
        let s = player.read().expect("player state poisoned");
        (s.volume, s.playlist_index)
    };
    let config = Config {
        current_playlist_index: index,
        volume,
        port,
        shuffle: shuffle.load(std::sync::atomic::Ordering::Relaxed),
        playlists: playlists.read().expect("playlists poisoned").clone(),
    };
    let path = config_path();
    match serde_json::to_string_pretty(&config) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                log_error!("failed to save config to {}: {e}", path.display());
            } else {
                log_debug!("config saved to {}", path.display());
            }
        }
        Err(e) => log_error!("failed to serialize config: {e}"),
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn stream_handler(State(state): State<Arc<AppState>>) -> Response {
    log_info!(
        "stream client connected (total subscribers: {})",
        state.audio.tx.receiver_count()
    );
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
                    log_warn!("stream client lagged by {n} chunks");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        log_info!("stream client disconnected");
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
    log_info!("shutdown requested via /api/shutdown");
    state.shutdown.cancel();
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
            log_info!("control: {}", status.as_str());
            StatusCode::OK
        }
        "next_playlist" | "prev_playlist" | "next_track" | "toggle_shuffle" => {
            let cmd = match req.action.as_str() {
                "next_playlist" => player::Command::NextPlaylist,
                "prev_playlist" => player::Command::PrevPlaylist,
                "next_track" => player::Command::NextTrack,
                _ => player::Command::ToggleShuffle,
            };
            match state.cmd_tx.try_send(cmd) {
                Ok(()) => {
                    log_info!("control: {}", req.action);
                    StatusCode::OK
                }
                Err(e) => {
                    log_error!("failed to queue {}: {e}", req.action);
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            }
        }
        other => {
            log_warn!("control: unknown action '{other}'");
            StatusCode::BAD_REQUEST
        }
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
        log_warn!("volume rejected: {} is out of range", req.volume);
        return StatusCode::BAD_REQUEST;
    }
    let mut s = state.player.write().expect("player state poisoned");
    s.volume = req.volume;
    log_info!("volume: {:.2}", req.volume);
    StatusCode::OK
}

async fn now_playing_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let (status, track, volume, index) = {
        let s = state.player.read().expect("player state poisoned");
        (s.status, s.current_track.clone(), s.volume, s.playlist_index)
    };
    let (playlist, playlist_type) = {
        let playlists = state.playlists.read().expect("playlists poisoned");
        match playlists.get(index) {
            Some(pl) => (Some(pl.name.clone()), Some(pl.kind.clone())),
            None => (None, None),
        }
    };
    Json(serde_json::json!({
        "status": status.as_str(),
        "track": track,
        "volume": volume,
        "playlist_index": index,
        "playlist": playlist,
        "playlist_type": playlist_type,
        "shuffle": state.shuffle.load(std::sync::atomic::Ordering::Relaxed),
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
        log_warn!("add playlist rejected: empty name or path");
        return StatusCode::BAD_REQUEST;
    }

    match req.kind.as_str() {
        "local_folder" => match audio::scan_folder(std::path::Path::new(path)) {
            Ok(files) if !files.is_empty() => {
                log_info!(
                    "add playlist: validated local folder '{}' ({} files)",
                    path,
                    files.len()
                );
            }
            Ok(_) => {
                log_warn!("add playlist rejected: no audio files in '{path}'");
                return StatusCode::BAD_REQUEST;
            }
            Err(e) => {
                log_error!("add playlist rejected: cannot scan '{path}': {e}");
                return StatusCode::BAD_REQUEST;
            }
        },
        "network_radio" => {
            if !(path.starts_with("http://") || path.starts_with("https://")) {
                log_warn!("add playlist rejected: '{path}' is not an http(s) URL");
                return StatusCode::BAD_REQUEST;
            }
        }
        other => {
            log_warn!("add playlist rejected: unknown type '{other}'");
            return StatusCode::BAD_REQUEST;
        }
    }

    {
        let mut playlists = state.playlists.write().expect("playlists poisoned");
        playlists.push(Playlist {
            name: name.to_string(),
            kind: req.kind,
            path: path.to_string(),
        });
        log_info!(
            "added playlist '{}' at index {} (total playlists: {})",
            name,
            playlists.len() - 1,
            playlists.len()
        );
    }
    persist_config(state.port, &state.player, &state.playlists, &state.shuffle);
    StatusCode::OK
}

async fn shutdown_signal(token: CancellationToken, quiet: bool) {
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            if let Err(e) = result {
                log_error!("failed to listen for Ctrl+C: {e}");
            }
            if !quiet {
                log_info!("shutdown requested via Ctrl+C");
            }
        }
        _ = token.cancelled() => {}
    }
}

#[tokio::main]
async fn main() {
    attach_parent_console();
    log::init();
    log_info!(
        "music-player backend v{} starting (pid {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    if let Ok(exe) = std::env::current_exe() {
        log_debug!("executable: {}", exe.display());
    }

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
                log_error!("decode failed for {}: {e}", path.display());
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
                log_error!("resample failed for {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    let config = load_or_create_config();
    log_info!(
        "config: port {}, volume {:.2}, shuffle {}, {} playlist(s), start index {}",
        config.port,
        config.volume,
        config.shuffle,
        config.playlists.len(),
        config.current_playlist_index
    );
    for (i, pl) in config.playlists.iter().enumerate() {
        log_debug!(
            "  playlist[{}] '{}' ({}) -> {}",
            i,
            pl.name,
            pl.kind,
            pl.path
        );
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));

    let queue = match config.playlists.get(config.current_playlist_index) {
        Some(pl) if pl.kind == "network_radio" => {
            log_info!(
                "start playlist[{}] '{}' is a radio station: {}",
                config.current_playlist_index,
                pl.name,
                pl.path
            );
            Vec::new()
        }
        Some(pl) if pl.kind == "local_folder" => {
            log_info!(
                "start playlist[{}] '{}' local folder: {}",
                config.current_playlist_index,
                pl.name,
                pl.path
            );
            match audio::scan_folder(std::path::Path::new(&pl.path)) {
                Ok(files) if !files.is_empty() => files,
                Ok(_) => {
                    log_error!("playlist folder is empty: {}", pl.path);
                    Vec::new()
                }
                Err(e) => {
                    log_error!("failed to scan playlist folder {}: {e}", pl.path);
                    Vec::new()
                }
            }
        }
        Some(pl) => {
            log_error!(
                "unknown playlist type '{}' for playlist[{}] '{}'",
                pl.kind,
                config.current_playlist_index,
                pl.name
            );
            Vec::new()
        }
        None => {
            log_warn!("no playlists configured, falling back to testdata/");
            match audio::scan_folder(std::path::Path::new("testdata")) {
                Ok(files) if !files.is_empty() => files,
                Ok(_) => {
                    log_error!("no audio files in testdata/");
                    Vec::new()
                }
                Err(e) => {
                    log_error!("failed to scan testdata/: {e}");
                    Vec::new()
                }
            }
        }
    };
    if queue.is_empty() {
        log_warn!("queue is empty: waiting for playlists to be added via the settings page");
    }
    log_info!("initial queue: {} tracks", queue.len());

    let (pcm_tx, pcm_rx) = tokio::sync::mpsc::channel::<Vec<f32>>(128);
    let (epoch_tx, epoch_rx) = tokio::sync::watch::channel(0u64);
    let playlists_shared: player::SharedPlaylists =
        Arc::new(RwLock::new(config.playlists.clone()));
    let shuffle_flag: player::SharedShuffle =
        Arc::new(std::sync::atomic::AtomicBool::new(config.shuffle));
    let (player_state, cmd_tx) = player::spawn(
        playlists_shared.clone(),
        config.current_playlist_index,
        queue,
        config.volume,
        shuffle_flag.clone(),
        pcm_tx,
        epoch_tx,
    );

    let shared = match streamer::start_encoder(pcm_rx, player_state.clone(), epoch_rx) {
        Ok(s) => {
            log_info!("FLAC encoder started (48000 Hz stereo)");
            Arc::new(s)
        }
        Err(e) => {
            log_error!("failed to start audio broadcast: {e}");
            std::process::exit(1);
        }
    };

    let shutdown = CancellationToken::new();
    let state = Arc::new(AppState {
        audio: shared,
        player: player_state.clone(),
        playlists: playlists_shared.clone(),
        shuffle: shuffle_flag.clone(),
        port: config.port,
        cmd_tx,
        shutdown: shutdown.clone(),
    });

    tray::run(config.port, player_state.clone(), shutdown.clone());
    log_debug!("tray icon initialized");

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
            log_error!("failed to bind port {}: {e}", config.port);
            log_error!("the backend may already be running, or another app is using this port");
            std::process::exit(1);
        }
    };

    log_info!("music-player backend listening on http://{addr}");
    log_debug!(
        "routes: / /api/health /api/control /api/volume /api/now_playing /api/playlists /api/shutdown /stream"
    );

    let server_shutdown = shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal(server_shutdown, true))
            .await
    });

    shutdown_signal(shutdown, false).await;
    log_info!("shutting down");

    // stream clients (OBS, VLC) hold infinite connections; do not wait for them
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), server).await;

    let path = config_path();
    let mut config = config;
    {
        let s = player_state.read().expect("player state poisoned");
        config.volume = s.volume;
        config.current_playlist_index = s.playlist_index;
    }
    config.shuffle = shuffle_flag.load(std::sync::atomic::Ordering::Relaxed);
    config.playlists = playlists_shared
        .read()
        .expect("playlists poisoned")
        .clone();
    match serde_json::to_string_pretty(&config) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                log_error!("failed to save config to {}: {e}", path.display());
            } else {
                log_debug!("final config saved to {}", path.display());
            }
        }
        Err(e) => log_error!("failed to serialize config: {e}"),
    }
    log_info!("shutdown complete");
}
