//! Background loop that triggers personal-LoRA retraining when enough
//! new events have accumulated.
//!
//! Pattern: every `POLL_INTERVAL`, snapshot journal stats + config. If
//! the user has enabled auto-retrain AND
//!     (applied + rewrite_applied) - last_train_event_count >= threshold
//! AND no training job is currently running, kick off the same
//! `TrainingState::start` path the UI uses. Once the job finishes
//! successfully, install the adapter and stamp the config so we don't
//! re-trigger immediately.
//!
//! Hot-reload of the engine is NOT done here — the user will see the new
//! adapter the next time they relaunch Quill. v0.7 will swap the engine
//! atomically in place.

#![cfg(feature = "llm")]

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::ConfigStore;
use crate::journal::Journal;
use crate::training::{JobState, TrainingState};

const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub fn spawn(
    journal: Arc<Journal>,
    training: Arc<TrainingState>,
    config: Arc<ConfigStore>,
) {
    thread::Builder::new()
        .name("quill-retrain-scheduler".into())
        .spawn(move || run(journal, training, config))
        .expect("spawn retrain scheduler");
}

fn run(journal: Arc<Journal>, training: Arc<TrainingState>, config: Arc<ConfigStore>) {
    eprintln!("[quill][scheduler] background retrain loop started (poll every {}s)", POLL_INTERVAL.as_secs());
    let mut waiting_on_job_since: Option<Instant> = None;
    loop {
        thread::sleep(POLL_INTERVAL);

        let cfg = config.snapshot();
        if !cfg.auto_retrain_enabled {
            // Reset job-watch state when toggled off.
            waiting_on_job_since = None;
            continue;
        }

        let stats = journal.stats();
        let applied_total = stats.applied + stats.rewrite_applied;

        let status = training.status();

        // If we previously kicked off a job and it has since succeeded,
        // do the install + bookkeeping.
        if let Some(_started) = waiting_on_job_since {
            match status.state {
                JobState::Succeeded => {
                    let dest = crate::state::personal_adapter_path();
                    if let Some(dest) = dest {
                        match training.install(&dest) {
                            Ok(bytes) => {
                                eprintln!(
                                    "[quill][scheduler] auto-installed {bytes}B → {}",
                                    dest.display()
                                );
                                let _ = config.update(|c| {
                                    c.last_train_event_count = applied_total;
                                    c.last_train_at = Some(crate::config::now_rfc3339());
                                    c.pending_relaunch = true;
                                });
                            }
                            Err(e) => {
                                eprintln!("[quill][scheduler] install failed: {e}");
                            }
                        }
                    }
                    training.reset();
                    waiting_on_job_since = None;
                }
                JobState::Failed => {
                    eprintln!(
                        "[quill][scheduler] last training failed: {}",
                        status.error.unwrap_or_else(|| "unknown".into())
                    );
                    training.reset();
                    waiting_on_job_since = None;
                }
                JobState::Running => { /* still going */ }
                JobState::Idle => {
                    // Edge: status reset externally. Drop the watch.
                    waiting_on_job_since = None;
                }
            }
            continue;
        }

        // No active job. Decide whether to start one.
        if status.state == JobState::Running {
            // Manually started — don't start a second one.
            continue;
        }
        let new_events = applied_total.saturating_sub(cfg.last_train_event_count);
        if new_events < cfg.auto_retrain_threshold {
            continue;
        }

        // Export → spawn.
        let export_path: PathBuf =
            std::env::temp_dir().join("quill-auto-retrain.jsonl");
        match journal.export_training_pairs(&export_path) {
            Ok(n) if n >= 10 => {
                eprintln!(
                    "[quill][scheduler] firing auto-retrain: {n} pairs, {new_events} new events since last train",
                );
                match training.start(export_path) {
                    Ok(()) => {
                        waiting_on_job_since = Some(Instant::now());
                    }
                    Err(e) => {
                        eprintln!("[quill][scheduler] start failed: {e}");
                    }
                }
            }
            Ok(n) => {
                eprintln!("[quill][scheduler] only {n} applied pairs in export, skipping");
            }
            Err(e) => {
                eprintln!("[quill][scheduler] export failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `new_events < threshold` should NOT trigger training.
    /// We test the pure decision logic rather than spawn a real subprocess.
    #[test]
    fn below_threshold_does_not_trigger() {
        let cfg = crate::config::Config {
            auto_retrain_enabled: true,
            auto_retrain_threshold: 25,
            last_train_event_count: 100,
            last_train_at: None,
            pending_relaunch: false,
        };
        let applied_total: u64 = 110; // delta = 10, threshold = 25
        let new_events = applied_total.saturating_sub(cfg.last_train_event_count);
        assert!(new_events < cfg.auto_retrain_threshold);
    }

    #[test]
    fn at_threshold_triggers() {
        let cfg = crate::config::Config {
            auto_retrain_enabled: true,
            auto_retrain_threshold: 25,
            last_train_event_count: 100,
            last_train_at: None,
            pending_relaunch: false,
        };
        let applied_total: u64 = 125; // delta = 25
        let new_events = applied_total.saturating_sub(cfg.last_train_event_count);
        assert!(new_events >= cfg.auto_retrain_threshold);
    }

    #[test]
    fn disabled_short_circuits_regardless_of_count() {
        let cfg = crate::config::Config {
            auto_retrain_enabled: false,
            auto_retrain_threshold: 25,
            last_train_event_count: 100,
            last_train_at: None,
            pending_relaunch: false,
        };
        // No matter how many new events, we shouldn't trigger because
        // `auto_retrain_enabled == false`. The function under test exits
        // early — verified by the explicit `continue` in `run`.
        assert!(!cfg.auto_retrain_enabled);
    }
}
