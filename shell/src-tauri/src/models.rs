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
    /// Set on adapter-only entries: the registry id of the base model
    /// this adapter layers on top of. `None` for standalone models.
    /// When set, [`resolve_paths`] returns the base path + the adapter
    /// path separately, and `is_installed` requires *both* files present.
    pub requires_base: Option<&'static str>,
}

/// Bundle of paths needed to load a registry entry. For standalone
/// models, `adapter` is `None`; for adapter entries it carries the
/// LoRA `.gguf` on top of the base.
#[derive(Clone, Debug)]
pub struct ModelPaths {
    pub base: PathBuf,
    pub adapter: Option<PathBuf>,
}

/// All models Nib can run. Order = display order in the settings panel.
/// Base entries (no `requires_base`) come first so adapter entries can
/// reference them by id.
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
        requires_base: None,
    },
    // Stock Qwen 2.5-1.5B base — the substrate every Nib adapter v2.x+
    // layers on top of. Standalone-usable but the registry hides it
    // unless an adapter that needs it is selected.
    ModelInfo {
        id: "qwen2.5-1.5b-instruct",
        display_name: "Qwen 2.5 1.5B Instruct (base)",
        params: "1.5B",
        size_mb: 940,
        blurb: "Stock Qwen base. Nib adapters (v2.x+) layer on top of \
                this — download once, reuse across every future Nib LoRA.",
        bundled: false,
        url: Some("https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf?download=true"),
        filename: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
        requires_base: None,
    },
    // Nib's faithful-rewrite LoRA — ships bundled in the .app (~50 MB),
    // applied at runtime on top of the Qwen base. Each future iteration
    // (v2.1, v2.2, …) ships as a tiny adapter swap, no base re-download.
    ModelInfo {
        id: "nib-qwen-v2",
        display_name: "Nib-Faithful v2 (Qwen 1.5B + LoRA)",
        params: "1.5B + LoRA",
        size_mb: 36,
        blurb: "Premium. Nib's faithful-rewrite LoRA layered on Qwen 2.5-\
                1.5B — preserves facts, numbers, and technical tokens. \
                70% pass on our internal eval (vs 34% for the 350M \
                default). Adapter is ~36 MB and ships bundled; the 940 MB \
                Qwen base downloads once, reusable for any future Nib \
                adapter.",
        bundled: true,
        url: Some("https://github.com/abgnydn/quill/releases/download/v2.1.0/nib-faithful-f16.gguf"),
        filename: "nib-faithful-f16.gguf",
        requires_base: Some("qwen2.5-1.5b-instruct"),
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
/// For adapter entries this requires *both* the adapter and its base
/// to be present — anything else and the engine couldn't load it.
pub fn is_installed<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    id: &str,
) -> bool {
    resolve_paths(app, id).is_some()
}

/// Resolve every file needed to actually load `id`. For standalone
/// models this is just the one path; for adapter entries it's `(base,
/// Some(adapter))`. Returns `None` if any required file is missing.
pub fn resolve_paths<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    id: &str,
) -> Option<ModelPaths> {
    let info = lookup(id);
    match info.requires_base {
        Some(base_id) => {
            let base = resolve_path(app, base_id)?;
            let adapter = resolve_path(app, id)?;
            Some(ModelPaths { base, adapter: Some(adapter) })
        }
        None => {
            let base = resolve_path(app, id)?;
            Some(ModelPaths { base, adapter: None })
        }
    }
}

/// What to download when the user clicks "install" on `id`. For
/// standalone models this is `id` itself; for an adapter entry it's
/// the base (since the adapter ships bundled in the .app). Returns
/// `None` if everything's already on disk.
pub fn download_target<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    id: &str,
) -> Option<&'static str> {
    let info = lookup(id);
    match info.requires_base {
        Some(base_id) => {
            // Adapter entry: download the base if absent.
            if resolve_path(app, base_id).is_none() {
                Some(base_id)
            } else {
                None
            }
        }
        None => {
            if resolve_path(app, id).is_none() {
                Some(info.id)
            } else {
                None
            }
        }
    }
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
        assert_eq!(lookup("qwen2.5-1.5b-instruct").id, "qwen2.5-1.5b-instruct");
    }

    #[test]
    fn adapter_entries_point_at_real_base() {
        for m in REGISTRY {
            if let Some(base_id) = m.requires_base {
                let base = lookup(base_id);
                assert_eq!(
                    base.id, base_id,
                    "adapter {} → base {} is not in registry",
                    m.id, base_id,
                );
                assert!(
                    base.requires_base.is_none(),
                    "adapter base must be standalone (no nested adapters)",
                );
            }
        }
    }
}
