//! Registry of LLM models Nib can run + download orchestration.
//!
//! The default `lfm2.5-350m` ships bundled inside the .app. Additional
//! models are downloaded on demand into
//!   `~/Library/Application Support/Nib/models/<id>.gguf`
//! and selected via `config.selected_model`. The focus tracker / inference
//! engine resolves the path through [`resolve_path`].

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;

/// Static metadata for one supported model.
#[derive(Serialize, Clone, Debug)]
pub struct ModelInfo {
    /// Stable identifier (e.g. `"lfm2.5-350m"`). Used as the on-disk
    /// filename stem and the config value.
    pub id: &'static str,
    /// User-facing display name.
    pub display_name: &'static str,
    /// Params, e.g. `"350M"`, `"1.2B"`.
    pub params: &'static str,
    /// Approximate Q4_K_M file size in MB.
    pub size_mb: u64,
    /// One-line description for the settings UI.
    pub blurb: &'static str,
    /// True when the .gguf ships inside the .app bundle. False = needs
    /// download to `~/Library/Application Support/Nib/models/`.
    pub bundled: bool,
    /// Download URL (HuggingFace direct GGUF). None for bundled.
    pub url: Option<&'static str>,
    /// On-disk filename (also used as bundle resource name when bundled).
    pub filename: &'static str,
}

/// All models Nib can run. Order = display order in the settings panel.
pub const REGISTRY: &[ModelInfo] = &[
    ModelInfo {
        id: "lfm2.5-350m",
        display_name: "LFM2.5 350M",
        params: "350M",
        size_mb: 219,
        blurb: "Default. Fast and light. Best for grammar fixes; \
                rewrites may pad or invent content.",
        bundled: true,
        url: None,
        filename: "lfm2.5-350m-q4_k_m.gguf",
    },
    ModelInfo {
        id: "nib-qwen-v2",
        display_name: "Nib-Qwen v2 (1.5B, faithful)",
        params: "1.5B",
        size_mb: 940,
        blurb: "Premium. Qwen 2.5-1.5B fine-tuned by Nib on a faithful-\
                rewrite dataset — preserves facts, numbers, and \
                technical tokens. 70% pass on our internal eval (vs 34% \
                for the 350M default). 940 MB download (one-time).",
        bundled: false,
        url: Some("https://github.com/abgnydn/quill/releases/download/v2.0.0/nib-qwen-v2-q4_k_m.gguf"),
        filename: "nib-qwen-v2-q4_k_m.gguf",
    },
];

/// Look up by ID. Falls back to the default (first registry entry) when
/// the id is unknown — keeps a stale config value from breaking startup.
pub fn lookup(id: &str) -> &'static ModelInfo {
    REGISTRY.iter().find(|m| m.id == id).unwrap_or(&REGISTRY[0])
}

/// Resolve the on-disk path for a given model. Checks BOTH the bundle
/// resources dir AND the downloaded-models dir — that way the Full
/// installer (which ships Nib-Qwen v2 inside the .app) and the regular
/// installer (which expects users to download it) both work via the
/// same code path. Bundle wins when present.
///
/// Generic over `Manager` so both `&tauri::App` (setup-time) and
/// `&tauri::AppHandle` (command-time) work without duplication.
pub fn resolve_path<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    id: &str,
) -> Option<PathBuf> {
    let info = lookup(id);
    if let Ok(p) = app.path().resolve(
        format!("resources/{}", info.filename),
        tauri::path::BaseDirectory::Resource,
    ) {
        if p.exists() {
            return Some(p);
        }
    }
    let p = downloaded_models_dir().ok()?.join(info.filename);
    if p.exists() { Some(p) } else { None }
}

/// Runtime check: is the model on disk anywhere we can load from?
pub fn is_installed<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    id: &str,
) -> bool {
    resolve_path(app, id).is_some()
}

/// Extended ModelInfo with runtime "installed" + "loaded" flags. Used
/// by the model_list Tauri command so the UI can render "bundled",
/// "downloaded", "needs download" pills correctly per actual disk state.
#[derive(Serialize, Clone, Debug)]
pub struct ModelInfoExt {
    #[serde(flatten)]
    pub info: ModelInfo,
    /// File is present on disk (bundled in .app OR downloaded).
    pub installed: bool,
    /// True if this is the model currently selected in config.
    pub selected: bool,
}

/// `~/Library/Application Support/Nib/models/`, created if missing.
pub fn downloaded_models_dir() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    let mut p = PathBuf::from(home);
    // Note: dir stays as "Quill" for back-compat with existing user data;
    // a future migration will move it to "Nib".
    p.push("Library/Application Support/Quill/models");
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

/// Progress callback signature: (bytes_downloaded, total_bytes_or_zero).
pub type ProgressFn = dyn Fn(u64, u64) + Send + 'static;

/// Tracks the currently-running download so the UI can poll its state
/// and we don't start two downloads of the same model simultaneously.
#[derive(Serialize, Clone, Debug, Default)]
pub struct DownloadStatus {
    pub model_id: String,
    pub bytes_done: u64,
    pub total_bytes: u64,
    pub state: DownloadState,
    pub error: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    #[default]
    Idle,
    Running,
    Done,
    Failed,
}

pub struct DownloadTracker {
    inner: Mutex<DownloadStatus>,
}

impl DownloadTracker {
    pub const fn new() -> Self {
        Self { inner: Mutex::new(DownloadStatus {
            model_id: String::new(),
            bytes_done: 0,
            total_bytes: 0,
            state: DownloadState::Idle,
            error: None,
        }) }
    }

    pub fn snapshot(&self) -> DownloadStatus {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set(&self, s: DownloadStatus) {
        if let Ok(mut g) = self.inner.lock() {
            *g = s;
        }
    }

    pub fn update<F: FnOnce(&mut DownloadStatus)>(&self, f: F) {
        if let Ok(mut g) = self.inner.lock() {
            f(&mut g);
        }
    }
}

/// Spawn a background thread that downloads `id` via `curl`. Polls the
/// destination file size for progress (curl streams writes), updates the
/// shared [`DownloadTracker`]. Returns immediately.
///
/// curl is universally available on macOS 13+ — no extra deps. It also
/// handles HuggingFace's 302→CDN redirect chain cleanly via `-L`.
pub fn spawn_download(
    id: String,
    tracker: std::sync::Arc<DownloadTracker>,
    on_complete: Option<Box<dyn Fn() + Send + 'static>>,
) {
    let info = lookup(&id).clone();
    let url = match info.url {
        Some(u) => u.to_string(),
        None => {
            tracker.set(DownloadStatus {
                model_id: id,
                bytes_done: 0,
                total_bytes: 0,
                state: DownloadState::Failed,
                error: Some("model is bundled, no download needed".into()),
            });
            return;
        }
    };
    let dest_dir = match downloaded_models_dir() {
        Ok(d) => d,
        Err(e) => {
            tracker.set(DownloadStatus {
                model_id: id,
                bytes_done: 0,
                total_bytes: 0,
                state: DownloadState::Failed,
                error: Some(format!("dest dir: {e}")),
            });
            return;
        }
    };
    let dest_path = dest_dir.join(info.filename);
    let tmp_path = dest_dir.join(format!("{}.part", info.filename));
    let total_estimate = info.size_mb * 1024 * 1024;

    tracker.set(DownloadStatus {
        model_id: id.clone(),
        bytes_done: 0,
        total_bytes: total_estimate,
        state: DownloadState::Running,
        error: None,
    });

    std::thread::Builder::new()
        .name(format!("nib-model-download-{}", info.id))
        .spawn({
            let tracker = tracker.clone();
            let id_for_thread = id.clone();
            move || {
                // Spawn curl, follow redirects, write to .part atomic rename on success.
                let mut child = match std::process::Command::new("curl")
                    .arg("-L")
                    .arg("-o").arg(&tmp_path)
                    .arg(&url)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(c) => c,
                    Err(e) => {
                        tracker.update(|s| {
                            s.state = DownloadState::Failed;
                            s.error = Some(format!("curl spawn: {e}"));
                        });
                        return;
                    }
                };

                // Poll file size every 500ms while curl runs.
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let size = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
                    tracker.update(|s| s.bytes_done = size);
                    match child.try_wait() {
                        Ok(Some(status)) if status.success() => {
                            // Rename .part → final.
                            if let Err(e) = std::fs::rename(&tmp_path, &dest_path) {
                                tracker.update(|s| {
                                    s.state = DownloadState::Failed;
                                    s.error = Some(format!("rename: {e}"));
                                });
                                return;
                            }
                            tracker.update(|s| {
                                s.state = DownloadState::Done;
                                s.bytes_done = std::fs::metadata(&dest_path)
                                    .map(|m| m.len()).unwrap_or(s.bytes_done);
                                s.total_bytes = s.bytes_done;
                            });
                            eprintln!("[nib][model] download complete: {id_for_thread}");
                            if let Some(cb) = on_complete { cb(); }
                            return;
                        }
                        Ok(Some(status)) => {
                            let _ = std::fs::remove_file(&tmp_path);
                            tracker.update(|s| {
                                s.state = DownloadState::Failed;
                                s.error = Some(format!("curl exited {status}"));
                            });
                            return;
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            tracker.update(|s| {
                                s.state = DownloadState::Failed;
                                s.error = Some(format!("wait: {e}"));
                            });
                            return;
                        }
                    }
                }
            }
        })
        .expect("spawn download thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_a_bundled_default() {
        let first = &REGISTRY[0];
        assert!(first.bundled, "first registry entry must be bundled");
        assert_eq!(first.id, "lfm2.5-350m");
    }

    #[test]
    fn lookup_falls_back_to_default_on_unknown_id() {
        let m = lookup("not-a-real-model");
        assert_eq!(m.id, REGISTRY[0].id);
    }

    #[test]
    fn lookup_finds_known_models() {
        assert_eq!(lookup("nib-qwen-v2").id, "nib-qwen-v2");
    }
}
