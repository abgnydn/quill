//! Tauri-managed application state.

use std::sync::Mutex;

use harper_core::Dialect;
use harper_core::linting::LintGroup;
use harper_core::spell::FstDictionary;

#[cfg(feature = "llm")]
use crate::inference;

pub struct CheckerState {
    pub linter: Mutex<LintGroup>,
}

impl CheckerState {
    pub fn new() -> Self {
        let dict = FstDictionary::curated();
        Self {
            linter: Mutex::new(LintGroup::new_curated(dict, Dialect::American)),
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
    pub engine: Mutex<Option<inference::RewriteEngine>>,
}

#[cfg(feature = "llm")]
impl RewriteState {
    pub fn from_path(path: Option<std::path::PathBuf>) -> Self {
        Self::from_paths(path, personal_adapter_path())
    }

    /// Load the base model and optionally a personal LoRA adapter on top.
    /// `adapter_path` is only used when it exists on disk.
    pub fn from_paths(
        path: Option<std::path::PathBuf>,
        adapter_path: Option<std::path::PathBuf>,
    ) -> Self {
        let adapter = adapter_path.filter(|p| p.exists());
        let engine = match path {
            Some(p) if p.exists() => match inference::RewriteEngine::load_with_adapter(&p, adapter.as_ref()) {
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
                eprintln!(
                    "[quill] no model path resolved (QUILL_MODEL unset, no bundled resource); rewrite disabled"
                );
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

    pub fn has_personal_adapter(&self) -> bool {
        self.engine
            .lock()
            .map(|g| g.as_ref().map(|e| e.has_adapter()).unwrap_or(false))
            .unwrap_or(false)
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

    pub fn has_personal_adapter(&self) -> bool {
        false
    }
}

/// Resolve the model path: prefer QUILL_MODEL env var (dev override), fall
/// back to the bundled `resources/lfm2.5-350m-q4_k_m.gguf` shipped inside the .app.
pub fn resolve_model_path(app: &tauri::App) -> Option<std::path::PathBuf> {
    if let Ok(env) = std::env::var("QUILL_MODEL") {
        return Some(env.into());
    }
    use tauri::Manager;
    app.path()
        .resolve(
            "resources/lfm2.5-350m-q4_k_m.gguf",
            tauri::path::BaseDirectory::Resource,
        )
        .ok()
}

/// Where Quill looks for an optional personal LoRA adapter on startup.
/// `~/Library/Application Support/Quill/personal-adapter.gguf` — same
/// directory as the journal so all per-user state is co-located.
pub fn personal_adapter_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = std::path::PathBuf::from(home);
    p.push("Library/Application Support/Quill");
    p.push("personal-adapter.gguf");
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_adapter_path_uses_home() {
        // Save & restore the real HOME so this test doesn't leak.
        let saved = std::env::var("HOME").ok();
        // SAFETY: tests are single-threaded by default for env mutation here.
        unsafe { std::env::set_var("HOME", "/tmp/quill-test-home"); }
        let got = personal_adapter_path();
        let expected = std::path::PathBuf::from(
            "/tmp/quill-test-home/Library/Application Support/Quill/personal-adapter.gguf",
        );
        assert_eq!(got, Some(expected));
        // Restore.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[cfg(feature = "llm")]
    #[test]
    fn rewrite_state_with_no_model_path_is_not_loaded() {
        let s = RewriteState::from_paths(None, None);
        assert!(!s.is_loaded(), "no model path → should not be loaded");
        assert!(!s.has_personal_adapter());
    }

    #[cfg(feature = "llm")]
    #[test]
    fn rewrite_state_with_missing_model_path_is_not_loaded() {
        let s = RewriteState::from_paths(
            Some(std::path::PathBuf::from("/tmp/quill-no-such-model-xyz.gguf")),
            None,
        );
        assert!(!s.is_loaded(), "nonexistent model path → not loaded");
        assert!(!s.has_personal_adapter());
    }

    #[cfg(feature = "llm")]
    #[test]
    fn rewrite_state_with_missing_adapter_doesnt_break_init() {
        // No model AND no adapter — verifies the adapter-missing branch
        // doesn't panic / error out the state constructor.
        let s = RewriteState::from_paths(
            None,
            Some(std::path::PathBuf::from(
                "/tmp/quill-no-such-adapter-xyz.gguf",
            )),
        );
        assert!(!s.is_loaded());
        assert!(!s.has_personal_adapter(), "missing adapter file → no personal adapter");
    }

    /// End-to-end test gated behind QUILL_TEST_MODEL env var. When the user
    /// has artifacts on disk, `cargo test -- --ignored` exercises the full
    /// model+adapter load + rewrite path. CI without artifacts skips this.
    #[cfg(feature = "llm")]
    #[test]
    #[ignore]
    fn full_model_load_and_rewrite_if_artifacts_present() {
        let Ok(model) = std::env::var("QUILL_TEST_MODEL") else {
            eprintln!("QUILL_TEST_MODEL not set; skipping");
            return;
        };
        let adapter = std::env::var("QUILL_TEST_ADAPTER").ok().map(std::path::PathBuf::from);
        let s = RewriteState::from_paths(Some(std::path::PathBuf::from(&model)), adapter.clone());
        assert!(s.is_loaded(), "model at {model} should load");
        if adapter.is_some() {
            assert!(s.has_personal_adapter(), "adapter at {adapter:?} should load");
        }
    }
}
