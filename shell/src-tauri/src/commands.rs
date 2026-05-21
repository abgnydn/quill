//! All `#[tauri::command]` functions. Kept thin — every command delegates
//! to a typed helper in `state.rs` / `wire.rs` / `overlay::*`.

use std::sync::Arc;

use serde::Deserialize;
use tauri::State;

use crate::journal::{self, Journal, JournalStats};
use crate::state::{CheckerState, RewriteState};
use crate::wire::{check_text_with, Capabilities, WireLint};

#[tauri::command]
pub fn check(text: &str, state: State<'_, CheckerState>) -> Vec<WireLint> {
    let mut linter = state.linter.lock().expect("checker mutex poisoned");
    check_text_with(&mut linter, text)
}

#[tauri::command]
pub fn capabilities(state: State<'_, RewriteState>) -> Capabilities {
    Capabilities {
        llm_built: cfg!(feature = "llm"),
        model_loaded: state.is_loaded(),
        personal_adapter_loaded: state.has_personal_adapter(),
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

    training.start(tmp).map_err(|e| e.to_string())?;
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
pub fn rewrite(
    text: &str,
    instruction: Option<String>,
    state: State<'_, RewriteState>,
) -> Result<String, String> {
    #[cfg(feature = "llm")]
    {
        let lock = state
            .engine
            .lock()
            .map_err(|e| format!("engine mutex poisoned: {e}"))?;
        match &*lock {
            Some(engine) => engine
                .rewrite(text, instruction.as_deref())
                .map_err(|e| format!("{e:#}")),
            None => {
                Err("no model loaded — set QUILL_MODEL=<path-to.gguf> before launching".into())
            }
        }
    }
    #[cfg(not(feature = "llm"))]
    {
        let _ = (text, instruction, state);
        Err("rewrite not available — build with --features llm".into())
    }
}
