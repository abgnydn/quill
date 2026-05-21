//! Persisted Quill settings — lives at
//! `~/Library/Application Support/Quill/config.json`.
//!
//! Kept tiny and serde-driven. Defaults are sane on first launch so a fresh
//! install never sees a missing-file error. Writes are atomic (tempfile +
//! rename) so a crashing Quill can't leave a half-written config.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    /// Background continual training enabled?
    pub auto_retrain_enabled: bool,
    /// Train after N more applied events since the last successful train.
    pub auto_retrain_threshold: u64,
    /// Event count at the time of the last successful training.
    pub last_train_event_count: u64,
    /// RFC-3339 timestamp of the last successful training (UTC).
    pub last_train_at: Option<String>,
    /// True after a successful auto-train; cleared once the user has
    /// relaunched (we use the absence of any prior session as the cue).
    pub pending_relaunch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_retrain_enabled: false,
            auto_retrain_threshold: 25,
            last_train_event_count: 0,
            last_train_at: None,
            pending_relaunch: false,
        }
    }
}

pub struct ConfigStore {
    path: PathBuf,
    inner: Mutex<Config>,
}

impl ConfigStore {
    pub fn open_default() -> std::io::Result<Self> {
        let path = default_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let inner = match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => Config::default(),
        };
        Ok(Self {
            path,
            inner: Mutex::new(inner),
        })
    }

    pub fn snapshot(&self) -> Config {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn update<F: FnOnce(&mut Config)>(&self, f: F) -> std::io::Result<Config> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("config mutex poisoned"))?;
        f(&mut g);
        let snapshot = g.clone();
        write_atomic(&self.path, &snapshot)?;
        Ok(snapshot)
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

fn write_atomic(dst: &PathBuf, cfg: &Config) -> std::io::Result<()> {
    let tmp = dst.with_extension("json.tmp");
    let s = serde_json::to_string_pretty(cfg)?;
    fs::write(&tmp, s)?;
    fs::rename(&tmp, dst)?;
    Ok(())
}

fn default_path() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/Quill");
    p.push("config.json");
    Ok(p)
}

/// Helper: short ISO-8601 UTC timestamp for `last_train_at`.
pub fn now_rfc3339() -> String {
    crate::journal::now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_safe() {
        let c = Config::default();
        assert!(!c.auto_retrain_enabled);
        assert_eq!(c.auto_retrain_threshold, 25);
        assert_eq!(c.last_train_event_count, 0);
    }

    #[test]
    fn round_trip_through_disk() {
        let tmp = std::env::temp_dir().join(format!("quill-cfg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.json");
        let store = ConfigStore {
            path: path.clone(),
            inner: Mutex::new(Config::default()),
        };
        store
            .update(|c| {
                c.auto_retrain_enabled = true;
                c.auto_retrain_threshold = 7;
                c.last_train_event_count = 42;
                c.last_train_at = Some("2026-05-21T00:00:00Z".into());
            })
            .unwrap();

        // Reload from disk.
        let raw = fs::read_to_string(&path).unwrap();
        let loaded: Config = serde_json::from_str(&raw).unwrap();
        assert!(loaded.auto_retrain_enabled);
        assert_eq!(loaded.auto_retrain_threshold, 7);
        assert_eq!(loaded.last_train_event_count, 42);
        assert_eq!(loaded.last_train_at.as_deref(), Some("2026-05-21T00:00:00Z"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn unknown_fields_dont_break_load() {
        // Forward compatibility: a future field should not nuke the user's
        // settings on roll-back to this version.
        let tmp = std::env::temp_dir().join(format!("quill-cfg-fwd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("config.json");
        fs::write(
            &path,
            r#"{"auto_retrain_enabled":true,"some_future_field":"banana","auto_retrain_threshold":13}"#,
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let loaded: Config = serde_json::from_str(&raw).unwrap();
        assert!(loaded.auto_retrain_enabled);
        assert_eq!(loaded.auto_retrain_threshold, 13);
        fs::remove_dir_all(&tmp).ok();
    }
}
