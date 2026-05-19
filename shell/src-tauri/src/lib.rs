use std::sync::Mutex;

use harper_core::linting::{LintGroup, Linter, Suggestion};
use harper_core::parsers::PlainEnglish;
use harper_core::spell::FstDictionary;
use harper_core::{Dialect, Document};
use serde::Serialize;
use tauri::{Manager, State};

#[cfg(feature = "llm")]
pub mod inference;

#[cfg(all(target_os = "macos", feature = "overlay"))]
pub mod overlay;

#[derive(Serialize)]
pub struct WireSuggestion {
    pub kind: &'static str,
    pub text: String,
}

#[derive(Serialize)]
pub struct WireLint {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub kind: String,
    pub priority: u8,
    pub suggestions: Vec<WireSuggestion>,
}

#[derive(Serialize)]
pub struct Capabilities {
    pub llm_built: bool,
    pub model_loaded: bool,
}

pub struct CheckerState {
    pub linter: Mutex<LintGroup>,
}

impl CheckerState {
    pub fn new() -> Self {
        let dict = FstDictionary::curated();
        let linter = LintGroup::new_curated(dict, Dialect::American);
        Self {
            linter: Mutex::new(linter),
        }
    }
}

impl Default for CheckerState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "llm")]
pub struct RewriteState {
    engine: Mutex<Option<inference::RewriteEngine>>,
}

#[cfg(feature = "llm")]
impl RewriteState {
    pub fn from_path(path: Option<std::path::PathBuf>) -> Self {
        let engine = match path {
            Some(p) if p.exists() => match inference::RewriteEngine::load(&p) {
                Ok(e) => {
                    eprintln!("[quill] loaded model from {}", p.display());
                    Some(e)
                }
                Err(err) => {
                    eprintln!("[quill] failed to load {}: {err:#}", p.display());
                    None
                }
            },
            Some(p) => {
                eprintln!("[quill] model path does not exist: {}", p.display());
                None
            }
            None => {
                eprintln!("[quill] no model path resolved (QUILL_MODEL unset, no bundled resource); rewrite disabled");
                None
            }
        };
        Self {
            engine: Mutex::new(engine),
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.engine.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

#[cfg(not(feature = "llm"))]
pub struct RewriteState;

#[cfg(not(feature = "llm"))]
impl RewriteState {
    pub fn from_path(_: Option<std::path::PathBuf>) -> Self {
        Self
    }

    pub fn is_loaded(&self) -> bool {
        false
    }
}

/// Resolve the model path: prefer QUILL_MODEL env var (dev override), fall back
/// to the bundled `resources/quill-q4_k_m.gguf` shipped inside the .app.
fn resolve_model_path(app: &tauri::App) -> Option<std::path::PathBuf> {
    if let Ok(env) = std::env::var("QUILL_MODEL") {
        return Some(env.into());
    }
    app.path()
        .resolve(
            "resources/quill-q4_k_m.gguf",
            tauri::path::BaseDirectory::Resource,
        )
        .ok()
}

fn wire_lints_from<I: IntoIterator<Item = harper_core::linting::Lint>>(lints: I) -> Vec<WireLint> {
    lints
        .into_iter()
        .map(|l| WireLint {
            start: l.span.start,
            end: l.span.end,
            message: l.message,
            kind: format!("{:?}", l.lint_kind),
            priority: l.priority,
            suggestions: l
                .suggestions
                .into_iter()
                .map(|s| match s {
                    Suggestion::ReplaceWith(chars) => WireSuggestion {
                        kind: "replace",
                        text: chars.iter().collect(),
                    },
                    Suggestion::InsertAfter(chars) => WireSuggestion {
                        kind: "insert_after",
                        text: chars.iter().collect(),
                    },
                    Suggestion::Remove => WireSuggestion {
                        kind: "remove",
                        text: String::new(),
                    },
                })
                .collect(),
        })
        .collect()
}

pub fn check_text_with(linter: &mut LintGroup, text: &str) -> Vec<WireLint> {
    let document = Document::new_curated(text, &PlainEnglish);
    wire_lints_from(linter.lint(&document))
}

#[tauri::command]
fn check(text: &str, state: State<'_, CheckerState>) -> Vec<WireLint> {
    let mut linter = state
        .linter
        .lock()
        .expect("checker mutex poisoned");
    check_text_with(&mut linter, text)
}

#[tauri::command]
fn capabilities(state: State<'_, RewriteState>) -> Capabilities {
    Capabilities {
        llm_built: cfg!(feature = "llm"),
        model_loaded: state.is_loaded(),
    }
}

/// Diagnostic ping invoked by the overlay JS each time it receives a
/// `focus-update`. Lets us verify the full Rust→JS→Rust round-trip from the
/// CLI by tailing the Quill stderr log — no manual UI inspection needed.
#[tauri::command]
fn overlay_ping(stage: &str, count: u32, detail: Option<String>) -> () {
    eprintln!(
        "[quill][overlay-js] {stage} count={count}{}",
        detail.map(|d| format!(" {d}")).unwrap_or_default()
    );
}

#[tauri::command]
fn rewrite(
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
            None => Err(
                "no model loaded — set QUILL_MODEL=<path-to.gguf> before launching".into(),
            ),
        }
    }
    #[cfg(not(feature = "llm"))]
    {
        let _ = (text, instruction, state);
        Err("rewrite not available — build with --features llm".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            app.manage(CheckerState::new());
            let model_path = resolve_model_path(app);
            app.manage(RewriteState::from_path(model_path));

            #[cfg(all(target_os = "macos", feature = "overlay"))]
            {
                if let Err(e) = overlay::window::create(&app.handle()) {
                    eprintln!("[quill] failed to create overlay window: {e}");
                }
                overlay::focus_tracker::spawn(app.handle().clone());
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![check, capabilities, rewrite, overlay_ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Integration test for the focus-update event pipeline.
    /// Builds a mock Tauri app, registers a listener for `focus-update`,
    /// emits the event from the AppHandle, and asserts the listener fires.
    /// Catches breakage like the capabilities/permissions ERR we just hit
    /// without anyone needing to click in a text field.
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

        // Spin the event loop a beat so the listener is registered.
        std::thread::sleep(std::time::Duration::from_millis(50));

        handle
            .emit("focus-update", serde_json::json!({"bounds": {"x":1.0,"y":2.0,"w":3.0,"h":4.0}}))
            .expect("emit should succeed");

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            received.load(Ordering::SeqCst) >= 1,
            "listener should have received at least one focus-update; got {}",
            received.load(Ordering::SeqCst)
        );
    }
}
