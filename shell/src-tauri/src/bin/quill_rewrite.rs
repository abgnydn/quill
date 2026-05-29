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
//! For RSFT data generation, sample multiple completions:
//!     ./target/release/quill-rewrite \
//!         --model … --text "…" --temperature 0.8 --top-p 0.95 --seed 42
//!
//! Lets you exercise the inference path without rebuilding the Tauri app.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use llama_cpp_2::sampling::LlamaSampler;
use quill_lib::inference::RewriteEngine;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let mut model: Option<PathBuf> = None;
    let mut adapter: Option<PathBuf> = None;
    let mut text: Option<String> = None;
    let mut instruction: Option<String> = None;
    let mut temperature: Option<f32> = None;
    let mut top_p: Option<f32> = None;
    let mut seed: Option<u32> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" | "-m" => {
                model = args.next().map(PathBuf::from);
            }
            "--adapter" | "-a" => {
                adapter = args.next().map(PathBuf::from);
            }
            "--text" | "-t" => {
                text = args.next();
            }
            "--instruction" | "-i" => {
                instruction = args.next();
            }
            "--temperature" | "--temp" => {
                temperature = args.next().and_then(|s| s.parse().ok());
            }
            "--top-p" | "--topp" => {
                top_p = args.next().and_then(|s| s.parse().ok());
            }
            "--seed" => {
                seed = args.next().and_then(|s| s.parse().ok());
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
    if let Some(a) = &adapter {
        eprintln!("[quill] + adapter {} …", a.display());
    }
    let t0 = Instant::now();
    let engine = match adapter.as_ref() {
        Some(a) => RewriteEngine::load_with_adapter(&model, Some(a))?,
        None => RewriteEngine::load(&model)?,
    };
    eprintln!("[quill] loaded in {:.2}s", t0.elapsed().as_secs_f32());

    let t1 = Instant::now();
    let out = match (temperature, top_p, seed) {
        (None, None, None) => {
            // Default greedy path — unchanged from previous behavior.
            engine.rewrite(&text, instruction.as_deref())?
        }
        _ => {
            // Caller asked for sampling. Build a chain matching rewrite_variants's
            // recipe but with caller-controlled parameters. RSFT data-gen flow.
            let temp = temperature.unwrap_or(0.8);
            let tp = top_p.unwrap_or(0.95);
            let sd = seed.unwrap_or(1337);
            let sampler = LlamaSampler::chain_simple([
                LlamaSampler::temp(temp),
                LlamaSampler::top_p(tp, 1),
                LlamaSampler::dist(sd),
            ]);
            eprintln!(
                "[quill] sampling temp={} top_p={} seed={}",
                temp, tp, sd
            );
            engine.rewrite_one(&text, instruction.as_deref(), sampler)?
        }
    };
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
    quill-rewrite --model PATH [--adapter PATH] --text STRING
                  [--instruction STRING]
                  [--temperature F --top-p F --seed N]

ARGS:
    -m, --model PATH         Path to base .gguf (e.g. qwen2.5-1.5b-instruct.gguf)
    -a, --adapter PATH       Optional LoRA adapter .gguf applied on top of
                             the base model (e.g. nib-faithful.gguf).
    -t, --text STRING        Source text to rewrite
    -i, --instruction STR    Optional editing directive
                             (default: \"Fix the grammar and improve clarity:\")
    --temperature F          Sampling temperature (default greedy when omitted).
                             0.7-0.9 for RSFT data-gen.
    --top-p F                Nucleus sampling threshold (default 0.95).
    --seed N                 RNG seed when temperature > 0. Distinct seeds
                             give distinct candidates from the same prompt.

EXAMPLES:
    # Greedy (production runtime behavior):
    quill-rewrite -m model.gguf -t \"hello world\"

    # Base + LoRA adapter (v2.1 adapter-only ship):
    quill-rewrite -m qwen2.5-1.5b.gguf -a nib-faithful.gguf \\
        -t \"I has a apple.\"

    # 8 diverse samples for RSFT (vary --seed):
    for s in 1 2 3 4 5 6 7 8; do
        quill-rewrite -m model.gguf -t \"hello world\" \\
            --temperature 0.8 --seed \"$s\"
    done"
    );
}
