use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const CONFIG_VERSION: u32 = 1;
pub const DEFAULT_PORT: u32 = 45880;
pub const DEFAULT_VOLUME: f32 = 0.8;
const MIN_PORT: u32 = 1024;
const MAX_PORT: u32 = 65535;
const PLAYLIST_TYPE_LOCAL: &str = "local_folder";
const PLAYLIST_TYPE_RADIO: &str = "network_radio";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Playlist {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub current_playlist_index: usize,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default = "default_port")]
    pub port: u32,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn default_volume() -> f32 {
    DEFAULT_VOLUME
}

fn default_port() -> u32 {
    DEFAULT_PORT
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            current_playlist_index: 0,
            volume: DEFAULT_VOLUME,
            port: DEFAULT_PORT,
            shuffle: false,
            playlists: Vec::new(),
        }
    }
}

impl Config {
    pub fn same_content(&self, other: &Config) -> bool {
        self.current_playlist_index == other.current_playlist_index
            && self.volume == other.volume
            && self.port == other.port
            && self.shuffle == other.shuffle
            && self.playlists == other.playlists
    }
}

#[derive(Debug)]
pub struct ConfigError {
    pub path: PathBuf,
    pub reason: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.reason)
    }
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
}

pub fn config_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .expect("cannot locate executable")
        .parent()
        .expect("executable has no parent directory")
        .to_path_buf();
    exe_dir.join("config.json")
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.bak", file_name_of(path)))
}

pub fn backup_path_of_config() -> PathBuf {
    backup_path(&config_path())
}

fn second_backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.bak.2", file_name_of(path)))
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".to_string())
}

/// NOTE: Migrations run in order, each bumps the version by one.
type Migration = fn(&mut Value) -> Result<(), String>;

const MIGRATIONS: &[(u32, Migration)] = &[];

pub fn load() -> Result<LoadedConfig, ConfigError> {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log_info!("no config at {} ({e}), creating defaults", path.display());
            let config = Config::default();
            if let Err(e) = save(&config) {
                return Err(ConfigError {
                    path,
                    reason: format!("cannot write the default config: {e}"),
                });
            }
            return Ok(LoadedConfig {
                config,
                warnings: Vec::new(),
            });
        }
        Err(e) => {
            return Err(ConfigError {
                path,
                reason: format!("cannot read the config file: {e}"),
            });
        }
    };

    let mut value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            return Err(ConfigError {
                path,
                reason: format!("the config file is not valid JSON: {e}"),
            });
        }
    };

    let mut warnings = Vec::new();
    if let Err(e) = migrate(&mut value) {
        return Err(ConfigError { path, reason: e });
    }

    let config = match from_value(&value, &mut warnings) {
        Ok(config) => config,
        Err(reason) => {
            return Err(ConfigError { path, reason });
        }
    };

    Ok(LoadedConfig { config, warnings })
}

fn migrate(value: &mut Value) -> Result<(), String> {
    if !value.is_object() {
        return Err("the config file must contain a JSON object".to_string());
    }
    let version = match value.get("version") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as u32,
        Some(Value::Null) | None => CONFIG_VERSION,
        Some(_) => return Err("'version' must be a number".to_string()),
    };
    if version > CONFIG_VERSION {
        return Err(format!(
            "the config file was written by a newer version (config version {version}, supported {CONFIG_VERSION})"
        ));
    }
    let mut current = version;
    for (from, migration) in MIGRATIONS {
        if *from == current {
            migration(value)
                .map_err(|e| format!("config migration {from} -> {} failed: {e}", from + 1))?;
            current += 1;
        }
    }
    if current != version {
        value["version"] = Value::from(current);
    }
    Ok(())
}

fn from_value(value: &Value, warnings: &mut Vec<String>) -> Result<Config, String> {
    let mut config = Config::default();

    config.volume = match value.get("volume") {
        None | Some(Value::Null) => DEFAULT_VOLUME,
        Some(Value::Number(n)) => {
            let raw = n.as_f64().unwrap_or(f64::NAN);
            if raw.is_nan() {
                warnings.push("volume is not a number, using the default".to_string());
                DEFAULT_VOLUME
            } else if !(0.0..=1.0).contains(&raw) {
                let clamped = raw.clamp(0.0, 1.0);
                warnings.push(format!("volume {raw} is out of range, using {clamped}"));
                clamped as f32
            } else {
                raw as f32
            }
        }
        Some(_) => {
            warnings.push("volume is not a number, using the default".to_string());
            DEFAULT_VOLUME
        }
    };

    config.port = match value.get("port") {
        None | Some(Value::Null) => DEFAULT_PORT,
        Some(Value::Number(n)) => {
            let raw = n.as_u64().unwrap_or(0);
            if raw < MIN_PORT as u64 || raw > MAX_PORT as u64 {
                let clamped = raw.clamp(MIN_PORT as u64, MAX_PORT as u64) as u32;
                warnings.push(format!("port {raw} is out of range, using {clamped}"));
                clamped
            } else {
                raw as u32
            }
        }
        Some(_) => {
            warnings.push("port is not a number, using the default".to_string());
            DEFAULT_PORT
        }
    };

    config.shuffle = match value.get("shuffle") {
        Some(Value::Bool(b)) => *b,
        None | Some(Value::Null) => false,
        Some(_) => {
            warnings.push("shuffle is not a boolean, using the default".to_string());
            false
        }
    };

    config.playlists = match value.get("playlists") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut playlists = Vec::new();
            for (i, item) in items.iter().enumerate() {
                match parse_playlist(item) {
                    Ok(playlist) => playlists.push(playlist),
                    Err(reason) => {
                        warnings.push(format!("playlist[{i}] is invalid ({reason}), skipped"))
                    }
                }
            }
            playlists
        }
        Some(_) => {
            warnings.push("playlists is not an array, using an empty list".to_string());
            Vec::new()
        }
    };

    config.current_playlist_index = match value.get("current_playlist_index") {
        None | Some(Value::Null) => 0,
        Some(Value::Number(n)) => {
            let raw = n.as_u64().unwrap_or(0);
            if config.playlists.is_empty() {
                if raw != 0 {
                    warnings.push(format!(
                        "current_playlist_index {raw} has no playlists, using 0"
                    ));
                }
                0
            } else {
                let max = config.playlists.len() - 1;
                if raw > max as u64 {
                    warnings.push(format!(
                        "current_playlist_index {raw} is out of range, using {max}"
                    ));
                    max
                } else {
                    raw as usize
                }
            }
        }
        Some(_) => {
            warnings.push("current_playlist_index is not a number, using 0".to_string());
            0
        }
    };

    Ok(config)
}

fn parse_playlist(item: &Value) -> Result<Playlist, String> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .ok_or("'name' is missing or not a string")?
        .trim()
        .to_string();
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .ok_or("'type' is missing or not a string")?
        .trim()
        .to_string();
    let path = item
        .get("path")
        .and_then(Value::as_str)
        .ok_or("'path' is missing or not a string")?
        .trim()
        .to_string();
    let playlist = Playlist { name, kind, path };
    if playlist.kind != PLAYLIST_TYPE_LOCAL && playlist.kind != PLAYLIST_TYPE_RADIO {
        return Err(format!("unknown playlist type '{}'", playlist.kind));
    }
    if playlist.name.is_empty() {
        return Err("'name' is empty".to_string());
    }
    if playlist.path.is_empty() {
        return Err("'path' is empty".to_string());
    }
    Ok(playlist)
}

pub fn save(config: &Config) -> Result<(), String> {
    let path = config_path();
    let text =
        serde_json::to_string_pretty(config).map_err(|e| format!("cannot serialize: {e}"))?;
    if let Err(e) = rotate_backup(&path) {
        log_warn!("config backup failed: {e}");
    }
    write_atomic(&path, &text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn rotate_backup(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let bak = backup_path(path);
    let bak2 = second_backup_path(path);
    if bak.exists() {
        copy_atomic(&bak, &bak2)?;
    }
    copy_atomic(path, &bak)
}

fn copy_atomic(src: &Path, dst: &Path) -> Result<(), String> {
    let text = std::fs::read(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    write_atomic(dst, &String::from_utf8_lossy(&text))
}

fn write_atomic(target: &Path, contents: &str) -> Result<(), String> {
    let tmp = target.with_file_name(format!("{}.tmp", file_name_of(target)));
    {
        let mut file = std::fs::File::create(&tmp)
            .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
        use std::io::Write;
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("cannot flush {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", target.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<LoadedConfig, ConfigError> {
        let mut value: Value = serde_json::from_str(text).map_err(|e| ConfigError {
            path: PathBuf::from("config.json"),
            reason: format!("the config file is not valid JSON: {e}"),
        })?;
        let mut warnings = Vec::new();
        migrate(&mut value).map_err(|reason| ConfigError {
            path: PathBuf::from("config.json"),
            reason,
        })?;
        let config = from_value(&value, &mut warnings).map_err(|reason| ConfigError {
            path: PathBuf::from("config.json"),
            reason,
        })?;
        Ok(LoadedConfig { config, warnings })
    }

    #[test]
    fn legacy_config_without_version_is_accepted() {
        let loaded = parse(
            r#"{"current_playlist_index":1,"volume":0.5,"port":3000,"playlists":[{"name":"a","type":"local_folder","path":"testdata"},{"name":"b","type":"network_radio","path":"http://example.com/stream"}]}"#,
        )
        .expect("legacy config must load");
        assert_eq!(loaded.config.port, 3000);
        assert_eq!(loaded.config.volume, 0.5);
        assert_eq!(loaded.config.current_playlist_index, 1);
        assert_eq!(loaded.config.playlists.len(), 2);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let loaded = parse("{}").expect("empty config must load");
        assert_eq!(loaded.config.version, CONFIG_VERSION);
        assert_eq!(loaded.config.port, DEFAULT_PORT);
        assert_eq!(loaded.config.volume, DEFAULT_VOLUME);
        assert!(!loaded.config.shuffle);
        assert!(loaded.config.playlists.is_empty());
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let loaded = parse(
            r#"{"port":99999,"volume":5.0,"current_playlist_index":7,"playlists":[{"name":"a","type":"local_folder","path":"testdata"}]}"#,
        )
        .expect("config must load");
        assert_eq!(loaded.config.port, MAX_PORT);
        assert_eq!(loaded.config.volume, 1.0);
        assert_eq!(loaded.config.current_playlist_index, 0);
        assert_eq!(loaded.warnings.len(), 3);
    }

    #[test]
    fn broken_playlist_is_skipped() {
        let loaded = parse(
            r#"{"playlists":[{"name":"a","type":"local_folder","path":"testdata"},{"name":"b","type":"weird","path":"x"},{"name":"","type":"local_folder","path":"y"}]}"#,
        )
        .expect("config must load");
        assert_eq!(loaded.config.playlists.len(), 1);
        assert_eq!(loaded.config.playlists[0].name, "a");
        assert_eq!(loaded.warnings.len(), 2);
    }

    #[test]
    fn invalid_json_is_rejected() {
        assert!(parse("{not json}").is_err());
    }

    #[test]
    fn newer_version_is_rejected() {
        let err = parse(r#"{"version":99}"#).expect_err("newer version must be rejected");
        assert!(err.reason.contains("newer version"));
    }

    #[test]
    fn wrong_types_are_rejected_or_defaulted() {
        let loaded = parse(r#"{"port":"abc","volume":0.5}"#).expect("config must load");
        assert_eq!(loaded.config.port, DEFAULT_PORT);
        assert_eq!(loaded.config.volume, 0.5);
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn atomic_write_creates_backup() {
        let dir =
            std::env::temp_dir().join(format!("music-player-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let target = dir.join("config.json");

        write_atomic(&target, "{\"a\":1}").expect("write");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"a\":1}");

        rotate_backup(&target).expect("backup");
        assert!(backup_path(&target).exists());

        write_atomic(&target, "{\"a\":2}").expect("write");
        rotate_backup(&target).expect("backup");
        assert!(second_backup_path(&target).exists());
        assert_eq!(
            std::fs::read_to_string(backup_path(&target)).unwrap(),
            "{\"a\":2}"
        );
        assert_eq!(
            std::fs::read_to_string(second_backup_path(&target)).unwrap(),
            "{\"a\":1}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
