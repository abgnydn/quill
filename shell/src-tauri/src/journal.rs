//! Local, private edit journal.
//!
//! Every time the user accepts or dismisses a Nib suggestion (whether a
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

pub fn now_rfc3339() -> String {
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

    /// Confirms `journal_log` semantics from the Rust side:
    /// the same code path the Tauri command takes — build_event +
    /// append — produces a stats count of N after N calls. This catches
    /// the "main-window applies don't increment counter" regression we
    /// hit before journal_log existed.
    #[test]
    fn n_logs_yield_n_in_stats() {
        let tmp = std::env::temp_dir().join(format!("quill-jlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true).append(true).open(tmp.join("j.jsonl")).unwrap();
        let j = Journal { path: tmp.join("j.jsonl"), file: std::sync::Mutex::new(file) };

        for i in 0..7 {
            j.append(&build_event(
                if i % 2 == 0 { "apply" } else { "rewrite_apply" },
                "source text",
                None,
                "suggested",
                "applied",
            ));
        }

        let s = j.stats();
        assert_eq!(s.count, 7, "expected 7 logged events");
        assert_eq!(s.applied, 4);          // i = 0,2,4,6
        assert_eq!(s.rewrite_applied, 3);  // i = 1,3,5
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Even with no events, stats and export are well-defined (no panic,
    /// zero counts, zero exported pairs).
    #[test]
    fn empty_journal_is_safe() {
        let tmp = std::env::temp_dir().join(format!("quill-jemp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("j.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).unwrap();
        let j = Journal { path: path.clone(), file: std::sync::Mutex::new(file) };

        let s = j.stats();
        assert_eq!(s.count, 0);
        assert!(s.oldest_ts.is_none());
        assert!(s.newest_ts.is_none());

        let out = tmp.join("export.jsonl");
        let n = j.export_training_pairs(&out).unwrap();
        assert_eq!(n, 0, "empty journal exports zero pairs");

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Exporting only emits events that ACTUALLY succeeded (apply +
    /// rewrite_apply). Pure dismiss events shouldn't show up as training
    /// pairs — those are negative signals for v0.6 DPO, not pairs.
    #[test]
    fn export_skips_non_applied_events() {
        let tmp = std::env::temp_dir().join(format!("quill-jexp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("j.jsonl");
        let file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).unwrap();
        let j = Journal { path: path.clone(), file: std::sync::Mutex::new(file) };

        j.append(&build_event("apply", "src1", None, "sugg", "applied1"));
        j.append(&build_event("dismiss", "src2", None, "sugg", ""));
        j.append(&build_event("rewrite_apply", "src3", None, "sugg", "applied3"));
        j.append(&build_event("rewrite_dismiss", "src4", None, "sugg", ""));

        let out = tmp.join("export.jsonl");
        let n = j.export_training_pairs(&out).unwrap();
        assert_eq!(n, 2, "only the 2 applied events should export");

        // Spot-check the schema once more.
        use std::io::BufRead;
        let lines: Vec<String> = std::io::BufReader::new(std::fs::File::open(&out).unwrap())
            .lines().map_while(Result::ok).collect();
        assert_eq!(lines.len(), 2);
        for l in &lines {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            assert!(v.get("src").is_some() && v.get("tgt").is_some());
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Full storage-layer end-to-end:
    ///   open temp journal → append two events → stats reflects them →
    ///   export training pairs → read back, assert {src, tgt} schema.
    /// If the UI counter isn't ticking up, it's NOT the journal — this passes.
    #[test]
    fn journal_append_stats_export_roundtrip() {
        use std::io::{BufRead, BufReader};

        // Use a tempdir so we don't touch ~/Library.
        let tmp = std::env::temp_dir().join(format!("quill-journal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let journal_path = tmp.join("journal.jsonl");

        // Hand-build a Journal at our temp path (bypassing default_path()).
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal_path)
            .unwrap();
        let j = Journal {
            path: journal_path.clone(),
            file: std::sync::Mutex::new(file),
        };

        // Append an Agreement apply and an AI rewrite_apply.
        j.append(&build_event(
            "apply",
            "I has a apple.",
            Some((2, 5, "Agreement", "verb form")),
            "have",
            "I have a apple.",
        ));
        j.append(&build_event(
            "rewrite_apply",
            "this is a sentence with errors",
            None,
            "This is a sentence with errors.",
            "This is a sentence with errors.",
        ));

        // Stats reflect both events.
        let s = j.stats();
        assert_eq!(s.count, 2, "expected 2 events in journal");
        assert_eq!(s.applied, 1, "expected 1 apply");
        assert_eq!(s.rewrite_applied, 1, "expected 1 rewrite_apply");
        assert!(s.oldest_ts.is_some());
        assert!(s.newest_ts.is_some());

        // Export → both events yield training pairs.
        let export_path = tmp.join("export.jsonl");
        let n = j.export_training_pairs(&export_path).unwrap();
        assert_eq!(n, 2, "expected 2 exported pairs (both have non-empty applied)");

        // Read back and verify the shape main.js / train_personal.py expects.
        let exported: Vec<serde_json::Value> = std::io::BufReader::new(
            std::fs::File::open(&export_path).unwrap(),
        )
        .lines()
        .map_while(Result::ok)
        .map(|l| serde_json::from_str(&l).unwrap())
        .collect();
        assert_eq!(exported.len(), 2);
        for row in &exported {
            assert!(row.get("src").is_some(), "exported row missing `src`");
            assert!(row.get("tgt").is_some(), "exported row missing `tgt`");
        }
        // Specifically the AI rewrite path: src is the raw source, tgt is the rewrite.
        assert_eq!(
            exported[1].get("tgt").and_then(|v| v.as_str()).unwrap(),
            "This is a sentence with errors."
        );

        // Clear wipes the file.
        let bytes = j.clear().unwrap();
        assert!(bytes > 0, "clear should report some bytes removed");
        let s2 = j.stats();
        assert_eq!(s2.count, 0, "stats should be empty after clear");

        std::fs::remove_dir_all(&tmp).ok();
    }
}
