//! Local LoRA training via the bundled QVAC `llama-finetune-lora` binary.
//!
//! Runs entirely on the user's machine — no Modal, no network — using the
//! Metal kernels QVAC ships. Operates on the same `{"src","tgt"}` JSONL
//! produced by `Journal::export_training_pairs`; the only conversion is
//! wrapping each pair in ChatML so `--assistant-loss-only` only masks-in
//! loss on the assistant's tokens.
//!
//! Spawned process is owned by `training::TrainingState` (same shape as
//! the Modal path) so the existing scheduler / install / pending-relaunch
//! UI all just work.

#![cfg(feature = "llm")]

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::Deserialize;
use serde_json::json;

#[derive(Debug)]
pub enum LocalStartError {
    QvacMissing,
    ModelMissing(PathBuf),
    JournalRead(String),
    ChatmlWrite(String),
    NoPairs,
    Spawn(String),
}

impl std::fmt::Display for LocalStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QvacMissing => write!(f, "bundled QVAC `llama-finetune-lora` not found"),
            Self::ModelMissing(p) => write!(f, "base model missing: {}", p.display()),
            Self::JournalRead(e) => write!(f, "reading journal export: {e}"),
            Self::ChatmlWrite(e) => write!(f, "writing chatml dataset: {e}"),
            Self::NoPairs => write!(f, "journal export contained zero valid pairs"),
            Self::Spawn(e) => write!(f, "spawning llama-finetune-lora: {e}"),
        }
    }
}

/// One row of `journal::export_training_pairs`.
#[derive(Deserialize)]
struct JournalPair {
    src: String,
    tgt: String,
}

/// Re-emit the journal's `{src,tgt}` JSONL as ChatML conversations.
/// Each row becomes `{"messages":[{role:user,...},{role:assistant,...}]}`
/// which `llama-finetune-lora --assistant-loss-only` understands.
pub fn convert_journal_to_chatml(src: &Path, dst: &Path) -> Result<usize, LocalStartError> {
    let f = File::open(src).map_err(|e| LocalStartError::JournalRead(e.to_string()))?;
    let mut out = File::create(dst).map_err(|e| LocalStartError::ChatmlWrite(e.to_string()))?;
    let mut written = 0usize;
    for (lineno, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| LocalStartError::JournalRead(e.to_string()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Tolerate single bad row — the journal can contain partial / weird
        // rows; we'd rather train on N-1 than refuse the whole job.
        let pair: JournalPair = match serde_json::from_str(trimmed) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[quill][train_local] skipping line {lineno}: {e}");
                continue;
            }
        };
        if pair.src.trim().is_empty() || pair.tgt.trim().is_empty() {
            continue;
        }
        // Fixed user-side instruction matches the inference-time prompt
        // (see inference.rs::DEFAULT_INSTRUCTION). Stays a single string
        // so the LoRA learns the editing distribution, not a meta-prompt.
        let row = json!({
            "messages": [
                {"role": "user", "content": format!("Fix grammar and clarity: {}", pair.src)},
                {"role": "assistant", "content": pair.tgt},
            ]
        });
        writeln!(out, "{}", row)
            .map_err(|e| LocalStartError::ChatmlWrite(e.to_string()))?;
        written += 1;
    }
    if written == 0 {
        return Err(LocalStartError::NoPairs);
    }
    Ok(written)
}

/// Build (don't spawn yet) the `llama-finetune-lora` command.
/// Caller handles the actual spawn so it can stash the `Child` into
/// `TrainingState`.
pub fn build_command(
    qvac_bin: &Path,
    base_model: &Path,
    dataset: &Path,
    output_adapter: &Path,
    lora_rank: u32,
    lora_alpha: u32,
    num_epochs: u32,
    learning_rate: f32,
) -> Command {
    let mut c = Command::new(qvac_bin);
    c.arg("-m").arg(base_model)
        .arg("-f").arg(dataset)
        .arg("--assistant-loss-only")
        .arg("--output-adapter").arg(output_adapter)
        .arg("--lora-rank").arg(lora_rank.to_string())
        .arg("--lora-alpha").arg(lora_alpha.to_string())
        .arg("--lora-modules").arg("attn_q,attn_k,attn_v,attn_o")
        .arg("--num-epochs").arg(num_epochs.to_string())
        .arg("--learning-rate").arg(learning_rate.to_string())
        .arg("--lr-scheduler").arg("cosine")
        .arg("--warmup-ratio").arg("0.03")
        .arg("-ngl").arg("999")
        .arg("-c").arg("512")
        .arg("-b").arg("512")
        .arg("-ub").arg("512")
        .arg("-fa").arg("off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// End-to-end: convert journal → chatml in temp dir, build command,
/// spawn, return the Child. Caller stashes into TrainingState.
pub fn spawn(
    qvac_bin: &Path,
    base_model: &Path,
    journal_export: &Path,
    output_adapter: &Path,
) -> Result<Child, LocalStartError> {
    if !qvac_bin.exists() {
        return Err(LocalStartError::QvacMissing);
    }
    if !base_model.exists() {
        return Err(LocalStartError::ModelMissing(base_model.to_path_buf()));
    }
    let chatml = std::env::temp_dir().join(format!(
        "quill-local-chatml-{}.jsonl",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    let n = convert_journal_to_chatml(journal_export, &chatml)?;
    eprintln!("[quill][train_local] {n} pairs → {}", chatml.display());

    let mut cmd = build_command(
        qvac_bin,
        base_model,
        &chatml,
        output_adapter,
        /* lora_rank */ 16,
        /* lora_alpha */ 32,
        /* num_epochs */ 2,
        /* learning_rate */ 5e-5,
    );
    eprintln!(
        "[quill][train_local] spawning {} (model={} dataset={} -> {})",
        qvac_bin.display(),
        base_model.display(),
        chatml.display(),
        output_adapter.display()
    );
    cmd.spawn().map_err(|e| LocalStartError::Spawn(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_journal(rows: &[(&str, &str)]) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "quill-tl-test-{}.jsonl",
            std::process::id()
        ));
        let mut f = File::create(&p).unwrap();
        for (s, t) in rows {
            writeln!(f, "{}", json!({"src": s, "tgt": t})).unwrap();
        }
        p
    }

    #[test]
    fn conversion_writes_expected_chatml_count() {
        let src = write_journal(&[
            ("i has a apple", "I have an apple"),
            ("this is an test", "this is a test"),
            ("their going", "they're going"),
        ]);
        let dst = std::env::temp_dir().join("quill-tl-out.jsonl");
        let n = convert_journal_to_chatml(&src, &dst).unwrap();
        assert_eq!(n, 3);
        // Validate the first line parses + has the expected shape.
        let line = std::fs::read_to_string(&dst).unwrap();
        let first: serde_json::Value =
            serde_json::from_str(line.lines().next().unwrap()).unwrap();
        let messages = first.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["content"], "I have an apple");
        std::fs::remove_file(&src).ok();
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    fn conversion_skips_empty_and_malformed_rows() {
        let p = std::env::temp_dir().join(format!(
            "quill-tl-bad-{}.jsonl",
            std::process::id()
        ));
        let mut f = File::create(&p).unwrap();
        writeln!(f, r#"{{"src": "ok", "tgt": "fixed"}}"#).unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "not valid json").unwrap();
        writeln!(f, r#"{{"src": "", "tgt": "x"}}"#).unwrap();
        writeln!(f, r#"{{"src": "y", "tgt": ""}}"#).unwrap();
        writeln!(f, r#"{{"src": "good2", "tgt": "fixed2"}}"#).unwrap();
        let dst = std::env::temp_dir().join("quill-tl-bad-out.jsonl");
        let n = convert_journal_to_chatml(&p, &dst).unwrap();
        // Only the two well-formed non-empty rows survive.
        assert_eq!(n, 2);
        std::fs::remove_file(&p).ok();
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    fn conversion_errors_when_no_valid_pairs() {
        let p = std::env::temp_dir().join(format!(
            "quill-tl-empty-{}.jsonl",
            std::process::id()
        ));
        let mut f = File::create(&p).unwrap();
        writeln!(f, "").unwrap();
        writeln!(f, "garbage").unwrap();
        let dst = std::env::temp_dir().join("quill-tl-empty-out.jsonl");
        let err = convert_journal_to_chatml(&p, &dst).unwrap_err();
        assert!(matches!(err, LocalStartError::NoPairs));
        std::fs::remove_file(&p).ok();
        std::fs::remove_file(&dst).ok();
    }

    #[test]
    fn spawn_errors_when_qvac_bin_missing() {
        let fake_bin = std::env::temp_dir().join("definitely-not-real-llama");
        let _ = std::fs::remove_file(&fake_bin);
        let fake_model = std::env::temp_dir().join("nope.gguf");
        let fake_journal = std::env::temp_dir().join("nope.jsonl");
        let fake_out = std::env::temp_dir().join("nope-out.gguf");
        let r = spawn(&fake_bin, &fake_model, &fake_journal, &fake_out);
        assert!(matches!(r, Err(LocalStartError::QvacMissing)));
    }
}
