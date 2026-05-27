//! Persisted Quill settings — lives at
//! `~/Library/Application Support/Quill/config.json`.
//!
//! Kept tiny and serde-driven. Defaults are sane on first launch so a fresh
//! install never sees a missing-file error. Writes are atomic (tempfile +
//! rename) so a crashing Quill can't leave a half-written config.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Per-app override for the engagement policy. Lets the user force-enable
/// Quill in an app that the hardcoded policy would skip (e.g. VS Code's
/// markdown panes) or force-disable it in an app that the policy would
/// engage (e.g. a specific browser the user wants quiet).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppOverride {
    /// Engage even when [`engagement_policy::is_engageable`] returns false.
    ForceAllow,
    /// Skip even when the policy would normally engage.
    ForceDeny,
}

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
    /// Words Quill should never lint (matched case-insensitive against the
    /// substring under a lint span). User's personal dictionary — names,
    /// jargon, slang, codenames. Lives in the config so it survives a
    /// reinstall.
    pub ignored_words: Vec<String>,
    /// When true the focus tracker skips ALL apps. The lint pipeline still
    /// runs (for the main-window panel) but the overlay stays silent. Used
    /// for screen-shares, demos, calls.
    pub paused: bool,
    /// Auto-pause-until timestamp (RFC-3339 UTC). When `paused` is false
    /// but this is set and in the future, [`is_paused_now`] returns true.
    /// Used by tray menu items like "Pause for 1 hour".
    pub pause_until: Option<String>,
    /// Per-app override map keyed by bundle ID. Overrides [`engagement_policy`].
    pub app_overrides: HashMap<String, AppOverride>,
    /// Selected LLM model id from `models::REGISTRY`. Default is the
    /// bundled lightweight option; users opt into larger downloads via
    /// the Settings panel.
    pub selected_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_retrain_enabled: false,
            auto_retrain_threshold: 25,
            last_train_event_count: 0,
            last_train_at: None,
            pending_relaunch: false,
            ignored_words: Vec::new(),
            paused: false,
            pause_until: None,
            app_overrides: HashMap::new(),
            selected_model: "lfm2.5-350m".to_string(),
        }
    }
}

impl Config {
    /// Case-insensitive lookup against [`ignored_words`].
    pub fn is_ignored(&self, word: &str) -> bool {
        let lw = word.to_lowercase();
        self.ignored_words.iter().any(|w| w.to_lowercase() == lw)
    }

    /// Effective pause: either the manual `paused` flag is on, OR the
    /// `pause_until` timestamp is in the future. The focus tracker
    /// consults this on every poll so the auto-pause expires without
    /// any explicit "resume" action.
    pub fn is_paused_now(&self) -> bool {
        if self.paused {
            return true;
        }
        if let Some(until) = &self.pause_until {
            // Best-effort RFC-3339 parse; if it doesn't parse, treat as
            // not-paused so the user isn't permanently locked out.
            if let Ok(t) = parse_rfc3339_secs(until) {
                if t > now_secs() {
                    return true;
                }
            }
        }
        false
    }

    /// Resolve the per-app override for a bundle ID, if any.
    pub fn app_override(&self, bundle_id: &str) -> Option<AppOverride> {
        self.app_overrides.get(bundle_id).copied()
    }
}

/// UNIX timestamp in seconds, monotonic-ish via SystemTime.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse an RFC-3339 timestamp like "2026-05-27T13:45:00Z" into a UNIX
/// seconds value. Returns Err on any format issue — caller falls back
/// to treating the pause as expired.
fn parse_rfc3339_secs(s: &str) -> Result<u64, ()> {
    // Y-M-D T H:M:S Z — accept either Z or +HH:MM, simple parse.
    let s = s.trim();
    if s.len() < 19 { return Err(()); }
    let (date, rest) = s.split_at(10);
    if !rest.starts_with('T') { return Err(()); }
    let time = &rest[1..9];
    let mut date_parts = date.split('-');
    let y: i64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let m: u64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let d: u64 = date_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let mut time_parts = time.split(':');
    let hh: u64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let mm: u64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    let ss: u64 = time_parts.next().ok_or(())?.parse().map_err(|_| ())?;
    // Days since UNIX epoch via the proleptic Gregorian formula.
    // Source: Howard Hinnant's date algorithms (public domain).
    let y_adj = y - (m <= 2) as i64;
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    if days < 0 { return Err(()); }
    Ok((days as u64) * 86400 + hh * 3600 + mm * 60 + ss)
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
        assert!(c.ignored_words.is_empty());
        assert!(!c.paused);
        assert!(c.app_overrides.is_empty());
    }

    #[test]
    fn is_ignored_is_case_insensitive() {
        let mut c = Config::default();
        c.ignored_words.push("BitNet".into());
        c.ignored_words.push("abgunaydin".into());
        assert!(c.is_ignored("bitnet"));
        assert!(c.is_ignored("BITNET"));
        assert!(c.is_ignored("Abgunaydin"));
        assert!(!c.is_ignored("bitnett"));
        assert!(!c.is_ignored("ab"));
    }

    #[test]
    fn app_override_lookup() {
        let mut c = Config::default();
        c.app_overrides.insert("com.example.Foo".into(), AppOverride::ForceAllow);
        c.app_overrides.insert("com.example.Bar".into(), AppOverride::ForceDeny);
        assert_eq!(c.app_override("com.example.Foo"), Some(AppOverride::ForceAllow));
        assert_eq!(c.app_override("com.example.Bar"), Some(AppOverride::ForceDeny));
        assert_eq!(c.app_override("com.example.Baz"), None);
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
