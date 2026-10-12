// Minimal logging with a file sink.
//
// Every message goes to the console (stdout for info/debug, stderr for
// warn/error) and is appended to `music-player.log` next to the executable,
// so failures inside a detached process (no console attached) can be
// inspected afterwards.
//
// Level filter: the MUSIC_PLAYER_LOG env var (debug|info|warn|error) overrides
// the default level. Debug builds default to `debug`, release builds to
// `info`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

pub const LOG_FILE_NAME: &str = "music-player.log";

struct Sink {
    file: Option<File>,
    path: PathBuf,
    level: Level,
    bytes_written: u64,
}

// Rotate instead of growing forever: one backup file (music-player.log.1).
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

pub fn log_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(LOG_FILE_NAME)
}

fn sink() -> &'static Mutex<Sink> {
    SINK.get_or_init(|| {
        let default_level = if cfg!(debug_assertions) {
            Level::Debug
        } else {
            Level::Info
        };
        let level = std::env::var("MUSIC_PLAYER_LOG")
            .map(|v| parse_level(&v).unwrap_or(default_level))
            .unwrap_or(default_level);

        let path = log_path();
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| eprintln!("cannot open log file {}: {e}", path.display()))
            .ok();
        let bytes_written = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        Mutex::new(Sink {
            file,
            path,
            level,
            bytes_written,
        })
    })
}

fn parse_level(value: &str) -> Option<Level> {
    match value.trim().to_ascii_lowercase().as_str() {
        "debug" | "trace" => Some(Level::Debug),
        "info" => Some(Level::Info),
        "warn" | "warning" => Some(Level::Warn),
        "error" => Some(Level::Error),
        _ => None,
    }
}

// Call once at startup; reports the active log file and level.
pub fn init() {
    let level = sink().lock().expect("log sink poisoned").level;
    crate::log_info!("log file: {}", log_path().display());
    crate::log_debug!("log level: {:?}", level);
}

pub fn log(level: Level, args: std::fmt::Arguments<'_>) {
    {
        let sink = sink().lock().expect("log sink poisoned");
        if level < sink.level {
            return;
        }
    }

    let line = format_line(level, &args);

    match level {
        Level::Debug | Level::Info => println!("{line}"),
        Level::Warn | Level::Error => eprintln!("{line}"),
    }

    write_to_file(level, &line);
}

fn write_to_file(level: Level, line: &str) {
    let mut sink = sink().lock().expect("log sink poisoned");
    let payload_len = line.len() as u64 + 1;
    if sink.bytes_written + payload_len > MAX_LOG_BYTES {
        rotate(&mut sink);
    }
    let Some(file) = sink.file.as_mut() else {
        return;
    };
    if file.write_all(line.as_bytes()).is_ok()
        && file.write_all(b"\n").is_ok()
        && file.flush().is_ok()
    {
        sink.bytes_written += payload_len;
    } else if level >= Level::Warn {
        eprintln!("cannot write to log file {}", sink.path.display());
    }
}

// Renames the current log to `music-player.log.1` and starts a fresh file.
// Must be called with the sink lock NOT held by this caller's logging path, so
// it stays silent about the rotation itself to avoid re-entering the lock.
fn rotate(sink: &mut Sink) {
    let backup = sink.path.with_extension("log.1");
    let reopened = std::fs::rename(&sink.path, &backup).ok().and_then(|_| {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink.path)
            .map_err(|e| eprintln!("cannot reopen log file: {e}"))
            .ok()
    });
    if let Some(fresh) = reopened {
        sink.file = Some(fresh);
        sink.bytes_written = 0;
    }
}
// "2026-10-12T14:03:11.482Z INFO  message"
fn format_line(level: Level, args: &std::fmt::Arguments<'_>) -> String {
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{} {:5} {args}",
        format_unix_timestamp(since_epoch.as_secs(), since_epoch.subsec_millis()),
        level.as_str()
    )
}

// Converts unix seconds to UTC "YYYY-MM-DDTHH:MM:SS.mmmZ" without extra deps.
fn format_unix_timestamp(secs: u64, millis: u32) -> String {
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (hour, min, sec) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

// Howard Hinnant's days-to-civil-date algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Debug, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Info, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Warn, format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::log($crate::log::Level::Error, format_args!($($arg)*)) };
}
