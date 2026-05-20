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

/// Resolve the model path: prefer QUILL_MODEL env var (dev override), fall
/// back to the bundled `resources/quill-q4_k_m.gguf` shipped inside the .app.
pub fn resolve_model_path(app: &tauri::App) -> Option<std::path::PathBuf> {
    if let Ok(env) = std::env::var("QUILL_MODEL") {
        return Some(env.into());
    }
    use tauri::Manager;
    app.path()
        .resolve(
            "resources/quill-q4_k_m.gguf",
            tauri::path::BaseDirectory::Resource,
        )
        .ok()
}
