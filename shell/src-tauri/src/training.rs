//! In-app personal LoRA training trigger.
//!
//! Spawns `modal run modal_train_personal.py` as a subprocess from the Quill
//! main window, polls its progress, and on success copies the resulting
//! `personal-adapter.gguf` into the spot Quill auto-detects at startup.
//!
//! Why subprocess rather than calling Modal's Python API directly: Modal's
//! orchestration is its CLI tool; replicating that from Rust is far more
//! work than just exec'ing the binary the user already has.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Idle,
    Running,
    Succeeded,
    Failed,
}

#[derive(Serialize, Clone, Debug)]
pub struct TrainingStatus {
    pub state: JobState,
    pub elapsed_secs: f64,
    /// Last meaningful line from stdout we managed to scrape, useful for
    /// telling the user what stage they're in ("Loading model…",
    /// "trained in X min", etc.).
    pub stage: Option<String>,
    pub error: Option<String>,
    /// Set when state == Succeeded — the local path Quill will install from.
    pub output_adapter: Option<String>,
    /// Which backend ran this job — UI surfaces "local (free)" vs "Modal ($0.20)".
    pub backend: Backend,
}

impl Default for TrainingStatus {
    fn default() -> Self {
        Self {
            state: JobState::Idle,
            elapsed_secs: 0.0,
            stage: None,
            error: None,
            output_adapter: None,
            backend: Backend::None,
        }
    }
}

struct Job {
    child: Option<Child>,
    started_at: Option<Instant>,
    state: JobState,
    stage: Option<String>,
    error: Option<String>,
    output_adapter: Option<PathBuf>,
    /// Working dir of the spawned process — used by the Modal backend so
    /// install can find `checkpoints/personal-adapter.gguf` after the
    /// modal CLI downloads it. Unset for the local backend (we already
    /// know the exact output path).
    cwd: Option<PathBuf>,
    /// Pre-known output path for backends that produce a deterministic
    /// adapter file (i.e. the local llama-finetune-lora path, which we
    /// pass via `--output-adapter`). Set at spawn time so status()
    /// doesn't have to guess.
    expected_output: Option<PathBuf>,
    /// Which backend this job is running on — surfaced to the UI so the
    /// user can see "training locally" vs "training on Modal".
    backend: Backend,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Modal,
    Local,
    /// No job has run yet — distinguish from a default that lies.
    None,
}

impl Default for Job {
    fn default() -> Self {
        Self {
            child: None,
            started_at: None,
            state: JobState::Idle,
            stage: None,
            error: None,
            output_adapter: None,
            cwd: None,
            expected_output: None,
            backend: Backend::None,
        }
    }
}

#[derive(Default)]
pub struct TrainingState {
    inner: Mutex<Job>,
}

pub type SharedTraining = Arc<TrainingState>;

#[derive(Debug)]
pub enum StartError {
    AlreadyRunning,
    NoHfToken,
    ModalNotFound(String),
    TrainDirMissing(PathBuf),
    Spawn(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "a training job is already running"),
            Self::NoHfToken => write!(
                f,
                "HF_TOKEN env var not set — launch Quill from a terminal with `export HF_TOKEN=hf_...`"
            ),
            Self::ModalNotFound(p) => write!(f, "modal CLI not found at {p}"),
            Self::TrainDirMissing(p) => write!(f, "train dir missing: {}", p.display()),
            Self::Spawn(e) => write!(f, "failed to spawn modal: {e}"),
        }
    }
}

/// Resolve the train directory. Production .app deployments hardcode
/// `~/quill/train` (Baris-only single-user assumption for now); v0.6 will
/// add a config file override.
pub fn default_train_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut p = PathBuf::from(home);
    p.push("quill");
    p.push("train");
    Some(p)
}

/// Try the venv binary first, fall back to PATH lookup.
fn resolve_modal_bin(train_dir: &Path) -> Option<PathBuf> {
    let venv_modal = train_dir.join(".venv/bin/modal");
    if venv_modal.exists() {
        return Some(venv_modal);
    }
    // Fallback — `which modal`. We do a small PATH walk rather than pulling
    // in the `which` crate just for this.
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("modal");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

impl TrainingState {
    /// Snapshot the current status — cheap, called from the JS poller every
    /// 2-3 seconds. Reaps the child if it has exited.
    pub fn status(&self) -> TrainingStatus {
        let mut g = match self.inner.lock() {
            Ok(g) => g,
            Err(_) => {
                return TrainingStatus {
                    state: JobState::Failed,
                    error: Some("training mutex poisoned".into()),
                    ..Default::default()
                };
            }
        };

        // If we were running, see if the child finished since last poll.
        if g.state == JobState::Running {
            let mut transition: Option<(JobState, Option<String>)> = None;
            if let Some(child) = g.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if status.success() {
                            transition = Some((JobState::Succeeded, None));
                        } else {
                            let code = status.code().unwrap_or(-1);
                            transition = Some((
                                JobState::Failed,
                                Some(format!("modal exited with code {code}")),
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        transition = Some((JobState::Failed, Some(format!("try_wait: {e}"))));
                    }
                }
            }
            if let Some((new_state, err)) = transition {
                g.state = new_state.clone();
                g.error = err;
                // On success, point the UI at the generated adapter.
                if new_state == JobState::Succeeded {
                    // Prefer the pre-known output path (local backend);
                    // fall back to the Modal cwd-relative default.
                    let candidate = g.expected_output.clone().or_else(|| {
                        g.cwd.as_ref().map(|c| c.join("checkpoints/personal-adapter.gguf"))
                    });
                    if let Some(out) = candidate {
                        if out.exists() {
                            g.output_adapter = Some(out);
                        }
                    }
                }
                g.child = None;
            }
        }

        let elapsed = g
            .started_at
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);

        TrainingStatus {
            state: g.state.clone(),
            elapsed_secs: elapsed,
            stage: g.stage.clone(),
            error: g.error.clone(),
            output_adapter: g.output_adapter.as_ref().map(|p| p.display().to_string()),
            backend: g.backend,
        }
    }

    /// Spawn the Modal training subprocess.
    pub fn start(&self, journal_path: PathBuf) -> Result<(), StartError> {
        let mut g = self.inner.lock().map_err(|_| StartError::Spawn("mutex".into()))?;
        if g.state == JobState::Running {
            return Err(StartError::AlreadyRunning);
        }
        let hf_token = std::env::var("HF_TOKEN").map_err(|_| StartError::NoHfToken)?;
        let train_dir = default_train_dir().ok_or_else(|| {
            StartError::TrainDirMissing(PathBuf::from("HOME not set"))
        })?;
        if !train_dir.exists() {
            return Err(StartError::TrainDirMissing(train_dir));
        }
        let modal_bin = resolve_modal_bin(&train_dir)
            .ok_or_else(|| StartError::ModalNotFound(format!("{}/.venv/bin/modal or in PATH", train_dir.display())))?;

        eprintln!(
            "[quill][train] spawning {} run modal_train_personal.py --journal {}",
            modal_bin.display(),
            journal_path.display()
        );

        let child = Command::new(&modal_bin)
            .current_dir(&train_dir)
            .env("HF_TOKEN", hf_token)
            .arg("run")
            .arg("modal_train_personal.py")
            .arg("--journal")
            .arg(&journal_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| StartError::Spawn(e.to_string()))?;

        *g = Job {
            child: Some(child),
            started_at: Some(Instant::now()),
            state: JobState::Running,
            stage: Some("starting modal job…".into()),
            error: None,
            output_adapter: None,
            cwd: Some(train_dir),
            expected_output: None,
            backend: Backend::Modal,
        };
        Ok(())
    }

    /// Spawn `llama-finetune-lora` from the bundled QVAC binaries — runs
    /// the whole training loop on the user's Mac, no Modal, no network.
    /// `output_adapter` is the destination GGUF path passed to QVAC via
    /// `--output-adapter`; we record it so `install()` can find it later.
    #[cfg(feature = "llm")]
    pub fn start_local(
        &self,
        qvac_bin: PathBuf,
        base_model: PathBuf,
        journal_export: PathBuf,
        output_adapter: PathBuf,
    ) -> Result<(), StartError> {
        let mut g = self.inner.lock().map_err(|_| StartError::Spawn("mutex".into()))?;
        if g.state == JobState::Running {
            return Err(StartError::AlreadyRunning);
        }
        let child = crate::training_local::spawn(
            &qvac_bin,
            &base_model,
            &journal_export,
            &output_adapter,
        )
        .map_err(|e| StartError::Spawn(e.to_string()))?;
        *g = Job {
            child: Some(child),
            started_at: Some(Instant::now()),
            state: JobState::Running,
            stage: Some("starting local training on Metal…".into()),
            error: None,
            output_adapter: None,
            cwd: None,
            expected_output: Some(output_adapter),
            backend: Backend::Local,
        };
        Ok(())
    }

    /// Copy a previously-trained adapter into Quill's Application Support
    /// dir so it's auto-detected on the next launch. When the local
    /// backend wrote the adapter directly to `dest` (we pass it via
    /// `--output-adapter`), the copy is a no-op and we just return the
    /// existing size.
    pub fn install(&self, dest: &Path) -> Result<u64, String> {
        let src = {
            let g = self.inner.lock().map_err(|_| "mutex".to_string())?;
            g.output_adapter
                .clone()
                .ok_or_else(|| "no adapter produced yet".to_string())?
        };
        if !src.exists() {
            return Err(format!("source missing: {}", src.display()));
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Same file? Nothing to do — local backend already wrote here.
        if let (Ok(s), Ok(d)) = (src.canonicalize(), dest.canonicalize()) {
            if s == d {
                return std::fs::metadata(dest).map(|m| m.len()).map_err(|e| e.to_string());
            }
        }
        std::fs::copy(&src, dest).map_err(|e| e.to_string())
    }

    /// Reset state to Idle so the user can run another job.
    pub fn reset(&self) {
        if let Ok(mut g) = self.inner.lock() {
            // If a child is still alive (shouldn't be — only reset Idle/
            // Succeeded/Failed), best-effort kill so we don't leak.
            if let Some(child) = g.child.as_mut() {
                let _ = child.kill();
            }
            *g = Job::default();
        }
    }
}

/// Side-channel: drain whatever's currently in the child's stdout (non-
/// blocking), parse a meaningful "stage" line from the tail. Called
/// opportunistically; safe to drop bytes since we only care about a hint.
/// Not used in v0.5 phase 3 MVP — kept as a hook for v0.6.
#[allow(dead_code)]
fn drain_stage(child: &mut Child) -> Option<String> {
    let mut buf = String::new();
    if let Some(mut out) = child.stdout.take() {
        let mut tmp = Vec::with_capacity(4096);
        if out.read_to_end(&mut tmp).is_ok() {
            buf.push_str(&String::from_utf8_lossy(&tmp));
        }
        child.stdout = Some(out);
    }
    buf.lines()
        .filter(|l| l.contains("[quill") || l.contains("trained in") || l.contains("MB"))
        .last()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_status_after_new() {
        let s = TrainingState::default();
        let st = s.status();
        assert_eq!(st.state, JobState::Idle);
        assert_eq!(st.elapsed_secs, 0.0);
        assert!(st.error.is_none());
    }

    #[test]
    fn start_errors_with_no_hf_token() {
        // Save & restore HF_TOKEN — tests share env.
        let saved = std::env::var("HF_TOKEN").ok();
        unsafe { std::env::remove_var("HF_TOKEN"); }
        let s = TrainingState::default();
        let r = s.start(PathBuf::from("/tmp/quill-journal.jsonl"));
        match r {
            Err(StartError::NoHfToken) => {}
            other => panic!("expected NoHfToken, got {other:?}"),
        }
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("HF_TOKEN", v);
            }
        }
    }

    #[test]
    fn default_train_dir_resolves_against_home() {
        let saved = std::env::var("HOME").ok();
        unsafe { std::env::set_var("HOME", "/tmp/qhome"); }
        let d = default_train_dir().unwrap();
        assert_eq!(d, PathBuf::from("/tmp/qhome/quill/train"));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn install_errors_when_no_adapter() {
        let s = TrainingState::default();
        let r = s.install(&PathBuf::from("/tmp/quill-dst.gguf"));
        assert!(r.is_err(), "should error when no job has produced an adapter");
    }

    #[test]
    fn reset_returns_to_idle() {
        let s = TrainingState::default();
        // Force a non-Idle state via direct mutation (we can't easily start
        // a real subprocess in tests without modal).
        if let Ok(mut g) = s.inner.lock() {
            g.state = JobState::Failed;
            g.error = Some("simulated".into());
        }
        s.reset();
        assert_eq!(s.status().state, JobState::Idle);
    }

    #[test]
    fn start_error_messages_are_actionable() {
        let strs = [
            format!("{}", StartError::NoHfToken),
            format!("{}", StartError::AlreadyRunning),
            format!("{}", StartError::ModalNotFound("/x".into())),
            format!("{}", StartError::TrainDirMissing(PathBuf::from("/y"))),
        ];
        // Each error mentions what to do or where to look.
        assert!(strs[0].contains("HF_TOKEN"));
        assert!(strs[1].contains("already"));
        assert!(strs[2].contains("modal"));
        assert!(strs[3].contains("/y"));
    }
}
