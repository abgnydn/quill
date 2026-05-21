//! Local, private edit journal.
//!
//! Every time the user accepts or dismisses a Quill suggestion (whether a
//! Harper-derived chip or an AI rewrite), we append one JSON line to a
//! file under `~/Library/Application Support/Quill/journal.jsonl`. The
//! file never leaves the device unless the user explicitly exports it.
//!
//! Format is one self-contained JSON object per line so:
//!   - we can append with no parsing of prior content (fast, thread-safe
//!     via a Mutex<File>)
//!   - re-reading is trivial (jq-able, streamable)
//!   - the CoEdIT-style `{src, tgt}` export drops in directly to the
//!     existing train pipeline
//!
//! Future v0.6+ pulls preference signals from `(action, kind)` to train
//! a DPO head; v0.5 just collects.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One line in `journal.jsonl`.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JournalEvent {
    /// RFC-3339 UTC timestamp.
    pub ts: String,
    /// `apply`, `dismiss`, `rewrite_apply`, `rewrite_dismiss`.
    pub action: String,
    /// Full text of the focused field at the moment of the event.
    pub source_text: String,
    /// Span of the lint, when applicable (None for whole-text rewrites).
    pub lint_start: Option<u32>,
    pub lint_end: Option<u32>,
    pub lint_kind: Option<String>,
    pub lint_message: Option<String>,
    /// The text Quill offered — suggestion replacement, or the LLM rewrite.
    pub suggested: String,
    /// The text actually written back. For an `apply` this is `suggested`;
    /// for a `rewrite_apply` same. For `dismiss` events: empty.
    pub applied: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct JournalStats {
    pub count: u64,
    pub applied: u64,
    pub dismissed: u64,
    pub rewrite_applied: u64,
    pub oldest_ts: Option<String>,
    pub newest_ts: Option<String>,
}

pub struct Journal {
    path: PathBuf,
    file: Mutex<File>,
}

impl Journal {
    pub fn open_default() -> std::io::Result<Self> {
        let path = default_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Append one event. Non-fatal on errors (the journal is best-effort —
    /// never break the rewrite flow because of a disk hiccup).
    pub fn append(&self, evt: &JournalEvent) {
        let line = match serde_json::to_string(evt) {
            Ok(s) => s,
            Err(_) => return,
        };
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{}", line);
            let _ = f.flush();
        }
    }

    /// Scan the file from disk to produce stats. O(n) in events.
    /// Cheap enough up to millions of entries; we'll add an index if needed.
    pub fn stats(&self) -> JournalStats {
        let mut s = JournalStats::default();
        let f = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return s,
        };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            let Ok(evt): Result<JournalEvent, _> = serde_json::from_str(&line) else { continue };
            s.count += 1;
            match evt.action.as_str() {
                "apply" => s.applied += 1,
                "dismiss" => s.dismissed += 1,
                "rewrite_apply" => s.rewrite_applied += 1,
                _ => {}
            }
            if s.oldest_ts.is_none() {
                s.oldest_ts = Some(evt.ts.clone());
            }
            s.newest_ts = Some(evt.ts);
        }
        s
    }

    /// Export events as a CoEdIT-style `{src, tgt}` JSONL — drops directly
    /// into train/scripts/train_personal.py. Only includes successful applies
    /// (no dismisses). Returns the number of pairs written.
    pub fn export_training_pairs(&self, out_path: &PathBuf) -> std::io::Result<usize> {
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let in_f = File::open(&self.path)?;
        let mut out_f = File::create(out_path)?;
        let mut n = 0usize;
        for line in BufReader::new(in_f).lines().map_while(Result::ok) {
            let Ok(evt): Result<JournalEvent, _> = serde_json::from_str(&line) else { continue };
            if evt.action != "apply" && evt.action != "rewrite_apply" {
                continue;
            }
            if evt.applied.is_empty() {
                continue;
            }
            let pair = serde_json::json!({
                "src": format!("Fix the grammar and improve clarity: {}", evt.source_text),
                "tgt": evt.applied,
                "kind": evt.lint_kind.unwrap_or_else(|| "rewrite".into()),
            });
            writeln!(out_f, "{}", pair)?;
            n += 1;
        }
        out_f.flush()?;
        Ok(n)
    }

    /// Wipe the journal. Returns the byte count removed.
    pub fn clear(&self) -> std::io::Result<u64> {
        let size = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if let Ok(mut f) = self.file.lock() {
            f.set_len(0)?;
            // Reopen at offset 0 so subsequent appends start fresh.
            *f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        }
        Ok(size)
    }
}

fn default_path() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
    })?;
    let mut p = PathBuf::from(home);
    p.push("Library/Application Support/Quill");
    p.push("journal.jsonl");
    Ok(p)
}

/// Build an event with `ts` set to current UTC and the action string.
pub fn build_event(
    action: &str,
    source_text: &str,
    lint: Option<(u32, u32, &str, &str)>,
    suggested: &str,
    applied: &str,
) -> JournalEvent {
    let (lint_start, lint_end, lint_kind, lint_message) = match lint {
        Some((s, e, k, m)) => (Some(s), Some(e), Some(k.to_string()), Some(m.to_string())),
        None => (None, None, None, None),
    };
    JournalEvent {
        ts: now_rfc3339(),
        action: action.to_string(),
        source_text: source_text.to_string(),
        lint_start,
        lint_end,
        lint_kind,
        lint_message,
        suggested: suggested.to_string(),
        applied: applied.to_string(),
    }
}

fn now_rfc3339() -> String {
    // Avoid pulling chrono just for this; format a minimal UTC ISO timestamp
    // from SystemTime. Good enough for journal use; not for billing.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hh, mm, ss) = epoch_to_ymdhms(secs as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hh, mm, ss
    )
}

/// Tiny self-contained UTC epoch → (y, m, d, h, m, s). Avoids adding chrono
/// just for one timestamp. Accurate from 1970 well past 2100.
fn epoch_to_ymdhms(epoch: i64) -> (i32, u32, u32, u32, u32, u32) {
    let secs_per_day: i64 = 86_400;
    let days = epoch / secs_per_day;
    let secs = epoch % secs_per_day;
    let hh = (secs / 3600) as u32;
    let mm = ((secs % 3600) / 60) as u32;
    let ss = (secs % 60) as u32;

    // 1970-01-01 = day 0.
    let mut year = 1970i32;
    let mut d = days;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if d >= dy {
            d -= dy;
            year += 1;
        } else {
            break;
        }
    }
    let months_lengths = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 0u32;
    for &len in &months_lengths {
        if d >= len as i64 {
            d -= len as i64;
            month += 1;
        } else {
            break;
        }
    }
    (year, month + 1, d as u32 + 1, hh, mm, ss)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_ymdhms_known_dates() {
        assert_eq!(epoch_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
        // 2023-11-14T22:13:20Z — `date -u -r 1700000000`
        assert_eq!(epoch_to_ymdhms(1_700_000_000), (2023, 11, 14, 22, 13, 20));
        // 2024-02-29T00:00:00Z (leap day)
        let leap = 1_709_164_800;
        let (y, m, d, _, _, _) = epoch_to_ymdhms(leap);
        assert_eq!((y, m, d), (2024, 2, 29));
    }

    #[test]
    fn round_trip_event() {
        let evt = build_event(
            "apply",
            "I has a apple.",
            Some((2, 5, "Agreement", "verb should be 'have'")),
            "have",
            "have",
        );
        let s = serde_json::to_string(&evt).unwrap();
        let back: JournalEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.action, "apply");
        assert_eq!(back.lint_kind.as_deref(), Some("Agreement"));
        assert_eq!(back.suggested, "have");
    }
}
