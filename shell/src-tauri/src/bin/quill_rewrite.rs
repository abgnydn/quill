//! Standalone CLI: rewrite a sentence using a quantized GGUF.
//!
//! Build:
//!     cargo build --features llm --bin quill-rewrite --release
//!
//! Run:
//!     ./target/release/quill-rewrite \
//!         --model ~/quill/train/checkpoints/quill-q4_k_m.gguf \
//!         --text  "This is an test of the Harper grammer checker."
//!
//! Lets you exercise the inference path without rebuilding the Tauri app.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use quill_lib::inference::RewriteEngine;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let mut model: Option<PathBuf> = None;
    let mut text: Option<String> = None;
    let mut instruction: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => {
                model = args.next().map(PathBuf::from);
            }
            "--text" | "-t" => {
                text = args.next();
            }
            "--instruction" | "-i" => {
                instruction = args.next();
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => {
                eprintln!("unknown arg: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }

    let model = model.ok_or_else(|| anyhow::anyhow!("--model PATH required"))?;
    let text = text.ok_or_else(|| anyhow::anyhow!("--text STRING required"))?;

    eprintln!("[quill] loading {} …", model.display());
    let t0 = Instant::now();
    let engine = RewriteEngine::load(&model)?;
    eprintln!("[quill] loaded in {:.2}s", t0.elapsed().as_secs_f32());

    let t1 = Instant::now();
    let out = engine.rewrite(&text, instruction.as_deref())?;
    let dt = t1.elapsed();
    eprintln!(
        "[quill] rewrote in {:.2}s ({} chars in, {} chars out)",
        dt.as_secs_f32(),
        text.len(),
        out.len()
    );

    println!("{out}");
    Ok(())
}

fn print_help() {
    eprintln!(
        "quill-rewrite — single-shot rewrite via GGUF

USAGE:
    quill-rewrite --model PATH --text STRING [--instruction STRING]

ARGS:
    -m, --model PATH         Path to .gguf (e.g. quill-q4_k_m.gguf)
    -t, --text STRING        Source text to rewrite
    -i, --instruction STR    Optional editing directive
                             (default: \"Fix the grammar and improve clarity:\")"
    );
}
