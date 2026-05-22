//! Locator + thin wrapper for the bundled QVAC Fabric binaries.
//!
//! `scripts/install-dev.sh` builds `qvac-fabric-llm.cpp` (Tether's BitNet
//! + on-device LoRA fork of `llama.cpp`) and stages
//!   llama-cli, llama-finetune-lora, *.dylib, *.metallib
//! into `shell/src-tauri/resources/qvac/`. Tauri's bundler ships those
//! into `Quill.app/Contents/Resources/_up_/resources/qvac/` (the
//! resource-directory layout Tauri uses for `resources` glob entries).
//!
//! This module finds them at runtime via Tauri's resource resolver +
//! exposes a small Rust API the commands layer wraps.

use std::path::PathBuf;
use std::process::Command;

use tauri::path::BaseDirectory;
use tauri::Manager;

/// Locate the bundled QVAC binary by name (e.g. `"llama-cli"`).
/// Returns `None` if Quill was built without QVAC staged.
pub fn binary_path(app: &tauri::AppHandle, name: &str) -> Option<PathBuf> {
    app.path()
        .resolve(
            format!("resources/qvac/{name}"),
            BaseDirectory::Resource,
        )
        .ok()
        .filter(|p| p.exists())
}

/// Returns the QVAC build version string (`llama-cli --version` first line)
/// when the binary is bundled + runnable, else None.
pub fn version(app: &tauri::AppHandle) -> Option<String> {
    let bin = binary_path(app, "llama-cli")?;
    let out = Command::new(&bin)
        .arg("--version")
        .output()
        .ok()?;
    // llama.cpp `--version` writes to stderr; check both streams.
    let first_line = |bytes: &[u8]| -> Option<String> {
        let s = String::from_utf8_lossy(bytes).into_owned();
        s.lines().next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    first_line(&out.stderr).or_else(|| first_line(&out.stdout))
}

/// Cheap presence check — does the bundle actually have what we'd need
/// for both inference and training?
pub fn is_available(app: &tauri::AppHandle) -> bool {
    binary_path(app, "llama-cli").is_some()
        && binary_path(app, "llama-finetune-lora").is_some()
}

/// Snapshot of resolved bundled-binary paths, captured at app startup so
/// the background scheduler (which doesn't have an AppHandle) can decide
/// which training backend to use without re-resolving every poll.
#[derive(Default, Clone)]
pub struct BackendConfig {
    pub finetune_bin: Option<PathBuf>,
    pub base_model: Option<PathBuf>,
}

impl BackendConfig {
    pub fn resolve(app: &tauri::App) -> Self {
        let handle = app.handle().clone();
        Self {
            finetune_bin: binary_path(&handle, "llama-finetune-lora"),
            base_model: crate::state::resolve_model_path(app),
        }
    }

    /// True when both the QVAC trainer AND the base model are resolvable —
    /// the two preconditions for `TrainingState::start_local`.
    pub fn local_ready(&self) -> bool {
        self.finetune_bin.as_ref().is_some_and(|p| p.exists())
            && self.base_model.as_ref().is_some_and(|p| p.exists())
    }
}

#[cfg(test)]
mod tests {
    // No unit tests at this layer — the bundled-binary check requires a
    // running Tauri app context. Integration via commands::qvac_version is
    // exercised through ./scripts/test.sh --with-app (manual for now).
}
