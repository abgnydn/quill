//! All `#[tauri::command]` functions. Kept thin — every command delegates
//! to a typed helper in `state.rs` / `wire.rs` / `overlay::*`.

use tauri::State;

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

/// Apply a correction to the currently-focused text field via AXUI.
/// Selects [start, end) then replaces with `replacement`.
#[tauri::command]
pub fn apply_suggestion(start: u32, end: u32, replacement: String) -> Result<(), String> {
    #[cfg(all(target_os = "macos", feature = "overlay"))]
    {
        crate::overlay::apply::apply(start, end, &replacement).map_err(|e| e.to_string())
    }
    #[cfg(not(all(target_os = "macos", feature = "overlay")))]
    {
        let _ = (start, end, replacement);
        Err("apply_suggestion requires the 'overlay' feature on macOS".into())
    }
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
