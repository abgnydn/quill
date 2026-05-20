//! Quill — local-first grammar/writing assistant.
//!
//! - `wire.rs`        — types crossing the Tauri IPC boundary
//! - `state.rs`       — `CheckerState`, `RewriteState` managed by Tauri
//! - `commands.rs`    — `#[tauri::command]` thunks (one-line delegations)
//! - `inference.rs`   — llama-cpp-2 wrapper (feature = "llm")
//! - `overlay/`       — macOS focus tracker, click-through window,
//!                      mouse arbiter, AXUI write-back (feature = "overlay")

use tauri::Manager;

pub mod commands;
pub mod state;
pub mod wire;

#[cfg(feature = "llm")]
pub mod inference;

#[cfg(all(target_os = "macos", feature = "overlay"))]
pub mod overlay;

// Re-exports for tests and external callers.
pub use state::{CheckerState, RewriteState};
pub use wire::{check_text_with, Capabilities, WireLint, WireSuggestion};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(CheckerState::new());
            let model_path = state::resolve_model_path(app);
            app.manage(RewriteState::from_path(model_path));

            #[cfg(all(target_os = "macos", feature = "overlay"))]
            {
                if let Err(e) = overlay::window::create(&app.handle()) {
                    eprintln!("[quill] failed to create overlay window: {e}");
                }
                let hot = std::sync::Arc::new(overlay::mouse_arbiter::HotRegions::default());
                app.manage(hot.clone());
                overlay::mouse_arbiter::spawn(app.handle().clone(), hot);
                overlay::focus_tracker::spawn(app.handle().clone());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::check,
            commands::capabilities,
            commands::rewrite,
            commands::overlay_ping,
            commands::apply_suggestion,
            commands::overlay_set_hot_regions,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use harper_core::Dialect;
    use harper_core::linting::LintGroup;
    use harper_core::spell::FstDictionary;

    fn fresh_linter() -> LintGroup {
        LintGroup::new_curated(FstDictionary::curated(), Dialect::American)
    }

    #[test]
    fn flags_obvious_grammar_error() {
        let mut linter = fresh_linter();
        let lints = check_text_with(&mut linter, "This is an test.");
        assert!(!lints.is_empty(), "Harper should flag 'an test'");
    }

    #[test]
    fn clean_text_returns_no_lints() {
        let mut linter = fresh_linter();
        let lints = check_text_with(&mut linter, "This is a perfectly normal sentence.");
        assert!(lints.is_empty(), "clean text should produce no lints");
    }

    /// Integration test for the focus-update event pipeline. Builds a mock
    /// Tauri app, registers a listener, emits an event, asserts the listener
    /// fires. Catches capabilities / permissions regressions without launching
    /// any GUI.
    #[test]
    fn focus_update_event_round_trip() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tauri::{Emitter, Listener, test::mock_app};

        let app = mock_app();
        let handle = app.handle();
        let received = Arc::new(AtomicU32::new(0));

        let r = received.clone();
        handle.listen("focus-update", move |_evt| {
            r.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));

        handle
            .emit(
                "focus-update",
                serde_json::json!({"bounds": {"x":1.0,"y":2.0,"w":3.0,"h":4.0}}),
            )
            .expect("emit should succeed");

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            received.load(Ordering::SeqCst) >= 1,
            "listener should have received at least one focus-update; got {}",
            received.load(Ordering::SeqCst)
        );
    }
}
