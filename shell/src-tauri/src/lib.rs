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
pub mod config;
pub mod journal;
pub mod qvac;
pub mod state;
pub mod training;
#[cfg(feature = "llm")]
pub mod training_local;
#[cfg(feature = "llm")]
pub mod training_scheduler;
pub mod wire;

#[cfg(feature = "llm")]
pub mod inference;

#[cfg(all(target_os = "macos", feature = "overlay"))]
pub mod overlay;

// Re-exports for tests and external callers.
pub use state::{CheckerState, RewriteState};
pub use wire::{check_text_with, Capabilities, WireLint, WireSuggestion};

/// Global-hotkey handler: grab the user's current selection via simulated
/// ⌘C, run the LLM rewrite on it, paste the result back via ⌘V. Runs on
/// a background thread spawned from the plugin handler.
#[cfg(all(target_os = "macos", feature = "overlay", feature = "llm"))]
fn run_rewrite_selection(app: tauri::AppHandle) {
    use tauri::Manager;
    let selection = match overlay::clipboard::read_selection_via_copy() {
        Some(s) => s,
        None => {
            eprintln!("[quill][hotkey] no selection to rewrite");
            return;
        }
    };
    eprintln!(
        "[quill][hotkey] selection={} chars; calling rewrite…",
        selection.chars().count()
    );

    let state: tauri::State<'_, RewriteState> = app.state::<RewriteState>();
    let result = {
        let lock = match state.engine.lock() {
            Ok(g) => g,
            Err(_) => {
                eprintln!("[quill][hotkey] engine mutex poisoned");
                return;
            }
        };
        match &*lock {
            Some(engine) => match engine.rewrite(&selection, None) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[quill][hotkey] rewrite failed: {e:#}");
                    return;
                }
            },
            None => {
                eprintln!("[quill][hotkey] no model loaded");
                return;
            }
        }
    };
    let posted = overlay::clipboard::paste_via_clipboard(&result);
    eprintln!(
        "[quill][hotkey] rewrite paste posted={posted} result={} chars",
        result.chars().count()
    );
}

#[cfg(not(all(target_os = "macos", feature = "overlay", feature = "llm")))]
fn run_rewrite_selection(_app: tauri::AppHandle) {
    eprintln!("[quill][hotkey] requires both 'overlay' and 'llm' features");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

    let rewrite_shortcut = Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::KeyR);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler({
                    let trigger = rewrite_shortcut;
                    move |app, shortcut, event| {
                        if event.state() != ShortcutState::Pressed {
                            return;
                        }
                        if shortcut != &trigger {
                            return;
                        }
                        eprintln!("[quill][hotkey] ⌘⇧R triggered");
                        let app_handle = app.clone();
                        std::thread::spawn(move || {
                            run_rewrite_selection(app_handle);
                        });
                    }
                })
                .build(),
        )
        .setup(move |app| {
            app.global_shortcut()
                .register(rewrite_shortcut)
                .unwrap_or_else(|e| eprintln!("[quill] could not register ⌘⇧R: {e}"));

            // Config: load (or create) ~/Library/Application Support/Quill/config.json
            // (loaded before the tray so the Pause/Resume menu item can reflect
            // the persisted `paused` state at startup.)
            let config = match config::ConfigStore::open_default() {
                Ok(c) => {
                    eprintln!("[quill] config at {}", c.path().display());
                    std::sync::Arc::new(c)
                }
                Err(e) => {
                    eprintln!("[quill] config open failed: {e} (using defaults in memory)");
                    std::sync::Arc::new(
                        config::ConfigStore::open_default()
                            .unwrap_or_else(|_| {
                                // Last-resort: in-memory only. open_default writes
                                // through, so two failures imply HOME is unwritable.
                                unreachable!("config fallback unreachable in practice")
                            }),
                    )
                }
            };
            app.manage(config.clone());

            // ---- Menubar tray ------------------------------------------
            // Quill runs as an LSUIElement — no dock icon, no main app
            // menu. The tray icon is the only persistent surface.
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;

                // Build the menu fresh each time we need to refresh the
                // Pause/Resume label. Cheap and avoids holding MenuItem
                // handles across the closure boundary.
                fn build_tray_menu<R: tauri::Runtime>(
                    app: &tauri::AppHandle<R>,
                    paused: bool,
                ) -> tauri::Result<tauri::menu::Menu<R>> {
                    let pause_label = if paused { "Resume Nib" } else { "Pause Nib" };
                    let pause = MenuItem::with_id(
                        app, "pause-toggle", pause_label, true, None::<&str>,
                    )?;
                    let settings = MenuItem::with_id(
                        app, "open-settings", "Settings…", true, None::<&str>,
                    )?;
                    let train = MenuItem::with_id(
                        app, "train", "Train personal adapter…", true, None::<&str>,
                    )?;
                    let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
                    let quit = MenuItem::with_id(app, "quit", "Quit Nib", true, Some("Cmd+Q"))?;
                    Menu::with_items(
                        app, &[&pause, &settings, &train, &sep1, &quit],
                    )
                }

                let initial_paused = config.snapshot().paused;
                let menu = build_tray_menu(app.handle(), initial_paused)?;

                let config_for_tray = config.clone();
                let _tray = TrayIconBuilder::with_id("main")
                    .tooltip("Nib — local-first grammar")
                    .menu(&menu)
                    .icon(app.default_window_icon().expect("icon").clone())
                    .icon_as_template(true)
                    .on_menu_event(move |app, event| match event.id.as_ref() {
                        "pause-toggle" => {
                            // Flip the persisted paused flag, then rebuild
                            // the menu so the label flips Pause ↔ Resume.
                            let new_paused = match config_for_tray
                                .update(|c| c.paused = !c.paused)
                            {
                                Ok(c) => c.paused,
                                Err(e) => {
                                    eprintln!("[quill][tray] pause toggle failed: {e}");
                                    return;
                                }
                            };
                            if let Some(tray) = app.tray_by_id("main") {
                                match build_tray_menu(app, new_paused) {
                                    Ok(m) => {
                                        let _ = tray.set_menu(Some(m));
                                    }
                                    Err(e) => eprintln!(
                                        "[quill][tray] menu rebuild failed: {e}"
                                    ),
                                }
                            }
                        }
                        "open-settings" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.unminimize();
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "train" => {
                            // Forward to the existing train command (no UI yet,
                            // results land in /tmp/quill.log).
                            let app_handle = app.clone();
                            std::thread::spawn(move || {
                                use tauri::Manager;
                                let journal: tauri::State<'_, std::sync::Arc<crate::journal::Journal>> =
                                    app_handle.state();
                                let training: tauri::State<'_, crate::training::SharedTraining> =
                                    app_handle.state();
                                match journal.export_training_pairs(
                                    &std::env::temp_dir().join("quill-tray-train.jsonl"),
                                ) {
                                    Ok(n) if n >= 10 => {
                                        let _ = training.start(
                                            std::env::temp_dir().join("quill-tray-train.jsonl"),
                                        );
                                    }
                                    Ok(n) => eprintln!("[quill][tray] only {n} pairs; need ≥10"),
                                    Err(e) => eprintln!("[quill][tray] export failed: {e}"),
                                }
                            });
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(app)?;
            }

            // Hide main window's close button into "minimise to tray" — by
            // default Tauri closes the window AND quits the app since this
            // is the last window. Override.
            if let Some(w) = app.get_webview_window("main") {
                let wc = w.clone();
                w.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = wc.hide();
                    }
                });
            }

            app.manage(CheckerState::new());
            let model_path = state::resolve_model_path(app);
            app.manage(RewriteState::from_path(model_path));

            match journal::Journal::open_default() {
                Ok(j) => {
                    eprintln!("[quill] journal at {}", j.path().display());
                    app.manage(std::sync::Arc::new(j));
                }
                Err(e) => eprintln!("[quill] journal open failed: {e}"),
            }

            let training = std::sync::Arc::new(training::TrainingState::default());
            app.manage(training.clone());

            // Snapshot the bundled-binary + base-model paths once at startup
            // so the background scheduler doesn't have to re-resolve them.
            let backend_config = std::sync::Arc::new(qvac::BackendConfig::resolve(app));
            eprintln!(
                "[quill] backend config: local_ready={} finetune_bin={:?} base_model={:?}",
                backend_config.local_ready(),
                backend_config.finetune_bin,
                backend_config.base_model,
            );
            app.manage(backend_config.clone());

            // Background scheduler — only when LLM feature is on (no
            // training infrastructure otherwise).
            #[cfg(feature = "llm")]
            {
                let journal_arc: std::sync::Arc<journal::Journal> = match app.try_state::<std::sync::Arc<journal::Journal>>() {
                    Some(s) => s.inner().clone(),
                    None => {
                        // journal failed to open earlier; skip scheduler.
                        eprintln!("[quill] scheduler: no journal state, skipping auto-retrain");
                        std::sync::Arc::new(journal::Journal::open_default().unwrap_or_else(|_| unreachable!()))
                    }
                };
                training_scheduler::spawn(
                    journal_arc,
                    training.clone(),
                    config.clone(),
                    backend_config.clone(),
                );
            }

            #[cfg(all(target_os = "macos", feature = "overlay"))]
            {
                if let Err(e) = overlay::window::create(&app.handle()) {
                    eprintln!("[quill] failed to create overlay window: {e}");
                }
                let hot = std::sync::Arc::new(overlay::mouse_arbiter::HotRegions::default());
                app.manage(hot.clone());
                overlay::mouse_arbiter::spawn(app.handle().clone(), hot);
                overlay::focus_tracker::spawn(app.handle().clone(), config.clone());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::check,
            commands::capabilities,
            commands::rewrite,
            commands::rewrite_variants,
            commands::overlay_ping,
            commands::apply_suggestion,
            commands::overlay_set_hot_regions,
            commands::journal_log,
            commands::journal_stats,
            commands::journal_export,
            commands::journal_clear,
            commands::train_personal_start,
            commands::train_personal_status,
            commands::train_personal_install,
            commands::train_personal_reset,
            commands::config_get,
            commands::config_set_auto_retrain,
            commands::config_clear_pending_relaunch,
            commands::dictionary_list,
            commands::dictionary_add,
            commands::dictionary_remove,
            commands::pause_set,
            commands::pause_toggle,
            commands::app_override_set,
            commands::app_override_remove,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use harper_core::linting::LintGroup;

    fn fresh_linter() -> LintGroup {
        // Mirror production wiring (curated + EXTRA_RULES) so tests catch
        // regressions in either the defaults or the extras.
        state::build_linter()
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
