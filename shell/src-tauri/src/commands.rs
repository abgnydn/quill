//! All `#[tauri::command]` functions. Kept thin — every command delegates
//! to a typed helper in `state.rs` / `wire.rs` / `overlay::*`.

use std::sync::Arc;

use serde::Deserialize;
use tauri::State;

use crate::journal::{self, Journal, JournalStats};
use crate::state::{CheckerState, RewriteState};
use crate::wire::{Capabilities, WireLint};

#[tauri::command]
pub fn check(
    text: &str,
    state: State<'_, CheckerState>,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Vec<WireLint> {
    let mut linter = state.linter.lock().expect("checker mutex poisoned");
    let snap = config.snapshot();
    crate::wire::check_text_filtered(&mut linter, text, &snap.ignored_words)
}

#[tauri::command]
pub fn capabilities(
    state: State<'_, RewriteState>,
    app: tauri::AppHandle,
) -> Capabilities {
    let qvac_available = crate::qvac::is_available(&app);
    let qvac_version = if qvac_available {
        crate::qvac::version(&app)
    } else {
        None
    };
    Capabilities {
        llm_built: cfg!(feature = "llm"),
        model_loaded: state.is_loaded(),
        personal_adapter_loaded: state.has_personal_adapter(),
        qvac_available,
        qvac_version,
    }
}

/// Diagnostic ping — overlay JS calls back into Rust on each pipeline stage
/// so we can verify the Rust→JS→Rust round-trip by tailing the stderr log.
#[tauri::command]
pub fn overlay_ping(stage: &str, count: u32, detail: Option<String>) {
    eprintln!(
        "[quill][overlay-js] {stage} count={count}{}",
        detail.map(|d| format!(" {d}")).unwrap_or_default()
    );
}

/// JS pushes the list of interactive rects (underlines + popover + fallback)
/// here so the mouse arbiter knows when to disable click-through.
#[cfg(all(target_os = "macos", feature = "overlay"))]
#[tauri::command]
pub fn overlay_set_hot_regions(
    rects: Vec<crate::overlay::mouse_arbiter::HotRect>,
    state: tauri::State<'_, std::sync::Arc<crate::overlay::mouse_arbiter::HotRegions>>,
) {
    if let Ok(mut g) = state.rects.lock() {
        *g = rects;
    }
}

#[cfg(not(all(target_os = "macos", feature = "overlay")))]
#[tauri::command]
pub fn overlay_set_hot_regions(_: serde_json::Value) {}

/// Per-call context passed from JS so the journal can record useful pairs
/// without a second round-trip. All fields optional — when missing we still
/// perform the AXUI write but skip journaling that event.
#[derive(Deserialize, Default)]
pub struct ApplyContext {
    /// "apply" (suggestion chip) or "rewrite_apply" (whole-text AI rewrite).
    pub kind: Option<String>,
    /// Full text of the focused field before the apply.
    pub source_text: Option<String>,
    /// What we'd predict the text becomes after applying (frontend-computed
    /// for speed; we don't need to re-read AXUI to know).
    pub applied_text: Option<String>,
    pub lint_kind: Option<String>,
    pub lint_message: Option<String>,
}

/// Apply a correction to the currently-focused text field via AXUI.
/// Selects [start, end) then replaces with `replacement`. Journals the
/// (source, applied) pair when `context` is supplied.
#[tauri::command]
pub fn apply_suggestion(
    start: u32,
    end: u32,
    replacement: String,
    context: Option<ApplyContext>,
    journal: State<'_, Arc<Journal>>,
) -> Result<(), String> {
    #[cfg(all(target_os = "macos", feature = "overlay"))]
    {
        crate::overlay::apply::apply(start, end, &replacement).map_err(|e| e.to_string())?;
        if let Some(ctx) = context {
            let lint = match (ctx.lint_kind.as_deref(), ctx.lint_message.as_deref()) {
                (Some(k), Some(m)) => Some((start, end, k, m)),
                _ => None,
            };
            let evt = journal::build_event(
                ctx.kind.as_deref().unwrap_or("apply"),
                ctx.source_text.as_deref().unwrap_or(""),
                lint,
                &replacement,
                ctx.applied_text.as_deref().unwrap_or(&replacement),
            );
            journal.append(&evt);
        }
        Ok(())
    }
    #[cfg(not(all(target_os = "macos", feature = "overlay")))]
    {
        let _ = (start, end, replacement, context, journal);
        Err("apply_suggestion requires the 'overlay' feature on macOS".into())
    }
}

/// Record an event without going through AXUI write-back. Used by the
/// main Quill window — it mutates its own textarea via plain JS and just
/// wants to be counted in the personalization journal.
#[tauri::command]
pub fn journal_log(
    kind: String,
    source_text: String,
    applied_text: String,
    suggested: Option<String>,
    lint_kind: Option<String>,
    lint_message: Option<String>,
    lint_start: Option<u32>,
    lint_end: Option<u32>,
    journal: State<'_, Arc<Journal>>,
) {
    let lint = match (
        lint_start,
        lint_end,
        lint_kind.as_deref(),
        lint_message.as_deref(),
    ) {
        (Some(s), Some(e), Some(k), Some(m)) => Some((s, e, k, m)),
        _ => None,
    };
    let evt = journal::build_event(
        &kind,
        &source_text,
        lint,
        suggested.as_deref().unwrap_or(&applied_text),
        &applied_text,
    );
    journal.append(&evt);
}

#[tauri::command]
pub fn journal_stats(journal: State<'_, Arc<Journal>>) -> JournalStats {
    journal.stats()
}

#[tauri::command]
pub fn journal_export(
    out_path: String,
    journal: State<'_, Arc<Journal>>,
) -> Result<usize, String> {
    let path = std::path::PathBuf::from(out_path);
    journal.export_training_pairs(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn journal_clear(journal: State<'_, Arc<Journal>>) -> Result<u64, String> {
    journal.clear().map_err(|e| e.to_string())
}

use crate::training::{SharedTraining, TrainingStatus};

#[tauri::command]
pub fn train_personal_start(
    journal: State<'_, Arc<Journal>>,
    training: State<'_, SharedTraining>,
    backend: State<'_, Arc<crate::qvac::BackendConfig>>,
) -> Result<TrainingStatus, String> {
    // 1. Export the current journal to a fresh temp file.
    let tmp = std::env::temp_dir().join(format!(
        "quill-training-{}.jsonl",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    let n = journal.export_training_pairs(&tmp).map_err(|e| e.to_string())?;
    if n < 10 {
        return Err(format!(
            "only {n} applied edits in the journal — need ≥10 before training is useful"
        ));
    }
    eprintln!("[quill][train] exported {n} pairs to {}", tmp.display());

    // Local backend (QVAC + bundled base model) is preferred — free, ~5
    // min on Metal vs ~15 min + $0.20 on Modal. Fall back to Modal only
    // when the local toolchain isn't fully bundled (e.g. dev build
    // without install-dev.sh having staged QVAC).
    if backend.local_ready() {
        let finetune = backend.finetune_bin.clone().unwrap();
        let base = backend.base_model.clone().unwrap();
        let out = crate::state::personal_adapter_path()
            .ok_or_else(|| "HOME not resolvable".to_string())?;
        training
            .start_local(finetune, base, tmp, out)
            .map_err(|e| e.to_string())?;
    } else {
        eprintln!("[quill][train] local backend not ready, falling back to Modal");
        training.start(tmp).map_err(|e| e.to_string())?;
    }
    Ok(training.status())
}

#[tauri::command]
pub fn train_personal_status(training: State<'_, SharedTraining>) -> TrainingStatus {
    training.status()
}

/// Copy a successfully-produced adapter into Quill's Application Support
/// dir. Quill auto-loads it on next launch.
#[tauri::command]
pub fn train_personal_install(training: State<'_, SharedTraining>) -> Result<String, String> {
    let dest = crate::state::personal_adapter_path()
        .ok_or_else(|| "HOME not resolvable".to_string())?;
    let bytes = training.install(&dest)?;
    eprintln!("[quill][train] installed {bytes}B → {}", dest.display());
    Ok(dest.display().to_string())
}

#[tauri::command]
pub fn train_personal_reset(training: State<'_, SharedTraining>) {
    training.reset();
}

#[tauri::command]
pub fn config_get(
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> crate::config::Config {
    config.snapshot()
}

#[tauri::command]
pub fn config_set_auto_retrain(
    enabled: bool,
    threshold: Option<u64>,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<crate::config::Config, String> {
    config
        .update(|c| {
            c.auto_retrain_enabled = enabled;
            if let Some(t) = threshold {
                c.auto_retrain_threshold = t.max(5);
            }
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn config_clear_pending_relaunch(
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<crate::config::Config, String> {
    config
        .update(|c| c.pending_relaunch = false)
        .map_err(|e| e.to_string())
}

// ─────────── Personal dictionary ───────────

#[tauri::command]
pub fn dictionary_list(
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Vec<String> {
    let mut words = config.snapshot().ignored_words;
    words.sort();
    words
}

#[tauri::command]
pub fn dictionary_add(
    word: String,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<Vec<String>, String> {
    let trimmed = word.trim();
    if trimmed.is_empty() {
        return Err("word is empty".into());
    }
    let w = trimmed.to_string();
    config
        .update(|c| {
            let lw = w.to_lowercase();
            if !c.ignored_words.iter().any(|x| x.to_lowercase() == lw) {
                c.ignored_words.push(w.clone());
            }
        })
        .map_err(|e| e.to_string())
        .map(|c| {
            let mut v = c.ignored_words;
            v.sort();
            v
        })
}

#[tauri::command]
pub fn dictionary_remove(
    word: String,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<Vec<String>, String> {
    let lw = word.to_lowercase();
    config
        .update(|c| c.ignored_words.retain(|w| w.to_lowercase() != lw))
        .map_err(|e| e.to_string())
        .map(|c| {
            let mut v = c.ignored_words;
            v.sort();
            v
        })
}

// ─────────── Pause toggle ───────────

#[tauri::command]
pub fn pause_set(
    paused: bool,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<bool, String> {
    config
        .update(|c| c.paused = paused)
        .map_err(|e| e.to_string())
        .map(|c| c.paused)
}

#[tauri::command]
pub fn pause_toggle(
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<bool, String> {
    config
        .update(|c| {
            c.paused = !c.paused;
            // Clear any temporary auto-pause when user manually toggles.
            if !c.paused {
                c.pause_until = None;
            }
        })
        .map_err(|e| e.to_string())
        .map(|c| c.paused)
}

/// Pause Nib for `minutes` minutes (auto-resume when the deadline
/// passes). Sets `pause_until` to now + minutes in RFC-3339 UTC.
/// `minutes = 0` clears any pending pause.
#[tauri::command]
pub fn pause_for_minutes(
    minutes: u64,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<String, String> {
    let until = if minutes == 0 {
        None
    } else {
        // Compute future UNIX seconds, format as RFC-3339 UTC.
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + minutes * 60;
        Some(format_unix_rfc3339(secs))
    };
    config
        .update(|c| {
            c.paused = false; // pause_until takes over
            c.pause_until = until.clone();
        })
        .map_err(|e| e.to_string())?;
    Ok(until.unwrap_or_else(|| "cleared".into()))
}

/// Inverse Hinnant date algorithm — UNIX seconds → "YYYY-MM-DDTHH:MM:SSZ".
fn format_unix_rfc3339(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y_adj = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y_adj + (m <= 2) as i64;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hh, mm, ss)
}

// ─────────── Per-app overrides ───────────

#[tauri::command]
pub fn app_override_set(
    bundle_id: String,
    kind: String,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<crate::config::Config, String> {
    let override_kind = match kind.as_str() {
        "force_allow" => crate::config::AppOverride::ForceAllow,
        "force_deny" => crate::config::AppOverride::ForceDeny,
        other => return Err(format!("unknown override kind: {other}")),
    };
    config
        .update(|c| {
            c.app_overrides.insert(bundle_id, override_kind);
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_override_remove(
    bundle_id: String,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<crate::config::Config, String> {
    config
        .update(|c| {
            c.app_overrides.remove(&bundle_id);
        })
        .map_err(|e| e.to_string())
}

// ─────────── Model picker ───────────

#[tauri::command]
pub fn model_list(
    app: tauri::AppHandle,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Vec<crate::models::ModelInfoExt> {
    let selected = config.snapshot().selected_model;
    crate::models::REGISTRY
        .iter()
        .map(|m| crate::models::ModelInfoExt {
            info: m.clone(),
            installed: crate::models::is_installed(&app, m.id),
            selected: m.id == selected,
        })
        .collect()
}

#[tauri::command]
pub fn model_get_selected(
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> String {
    config.snapshot().selected_model
}

#[tauri::command]
pub fn model_set_selected(
    id: String,
    config: State<'_, Arc<crate::config::ConfigStore>>,
) -> Result<String, String> {
    // Verify the id exists in the registry — otherwise a typo would
    // brick the next startup. lookup() returns default for unknown ids
    // so we explicitly check existence here.
    let known = crate::models::REGISTRY.iter().any(|m| m.id == id);
    if !known {
        return Err(format!("unknown model id: {id}"));
    }
    config
        .update(|c| c.selected_model = id.clone())
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub fn model_download(
    id: String,
    tracker: State<'_, Arc<crate::models::DownloadTracker>>,
) -> Result<crate::models::DownloadStatus, String> {
    // Refuse to start a second download while one is running.
    let current = tracker.snapshot();
    if current.state == crate::models::DownloadState::Running {
        return Err(format!("download already running: {}", current.model_id));
    }
    crate::models::spawn_download(id.clone(), tracker.inner().clone(), None);
    Ok(tracker.snapshot())
}

#[tauri::command]
pub fn model_download_status(
    tracker: State<'_, Arc<crate::models::DownloadTracker>>,
) -> crate::models::DownloadStatus {
    tracker.snapshot()
}

#[tauri::command]
pub fn rewrite(
    text: &str,
    instruction: Option<String>,
    session: Option<String>,
    state: State<'_, RewriteState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    #[cfg(feature = "llm")]
    {
        use tauri::Emitter;
        let session = session.unwrap_or_else(|| "default".into());
        let lock = state
            .engine
            .lock()
            .map_err(|e| format!("engine mutex poisoned: {e}"))?;
        match &*lock {
            Some(engine) => {
                let app_clone = app.clone();
                let session_for_cb = session.clone();
                let result = engine
                    .rewrite_streaming(text, instruction.as_deref(), move |delta| {
                        // Best-effort emit; if the overlay isn't subscribed we
                        // just keep generating tokens.
                        let _ = app_clone.emit_to(
                            "overlay",
                            "rewrite-token",
                            serde_json::json!({
                                "session": session_for_cb,
                                "delta": delta,
                                "done": false,
                            }),
                        );
                        // Also broadcast to the main window for its rewrite panel.
                        let _ = app_clone.emit(
                            "rewrite-token",
                            serde_json::json!({
                                "session": session_for_cb,
                                "delta": delta,
                                "done": false,
                            }),
                        );
                    })
                    .map_err(|e| format!("{e:#}"))?;
                let _ = app.emit_to(
                    "overlay",
                    "rewrite-token",
                    serde_json::json!({"session": session, "delta": "", "done": true}),
                );
                let _ = app.emit(
                    "rewrite-token",
                    serde_json::json!({"session": session, "delta": "", "done": true}),
                );
                Ok(result)
            }
            None => Err(
                "no model loaded — set QUILL_MODEL=<path-to.gguf> before launching".into(),
            ),
        }
    }
    #[cfg(not(feature = "llm"))]
    {
        let _ = (text, instruction, session, state, app);
        Err("rewrite not available — build with --features llm".into())
    }
}

/// Generate up to `n` (default 3, capped at 5) independent rewrite
/// alternatives. Variant 0 is deterministic greedy (identical to the single
/// `rewrite` path); the rest use temp=0.7/top_p=0.9 with distinct RNG seeds.
/// Wall-clock is ~N× single-rewrite latency since each variant builds its
/// own context — the frontend should show a spinner.
#[tauri::command]
pub fn rewrite_variants(
    text: &str,
    instruction: Option<String>,
    n: Option<u32>,
    state: State<'_, RewriteState>,
) -> Result<Vec<String>, String> {
    #[cfg(feature = "llm")]
    {
        let n = n.unwrap_or(3).clamp(1, 5) as usize;
        let lock = state
            .engine
            .lock()
            .map_err(|e| format!("engine mutex poisoned: {e}"))?;
        match &*lock {
            Some(engine) => engine
                .rewrite_variants(text, instruction.as_deref(), n)
                .map_err(|e| format!("{e:#}")),
            None => Err(
                "no model loaded — set QUILL_MODEL=<path-to.gguf> before launching".into(),
            ),
        }
    }
    #[cfg(not(feature = "llm"))]
    {
        let _ = (text, instruction, n, state);
        Err("rewrite_variants not available — build with --features llm".into())
    }
}
