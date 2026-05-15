# Quill — local-first grammar assistant

A native desktop grammar/writing assistant that targets a **~80 MB total bundle** by combining a fast Rust rule engine (Harper) with a small fine-tuned LLM (Gemma 3 270M). The pitch: better quality than Harper alone, better latency and footprint than Grammarly, 100% local, no network call.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│  Tauri 2 shell (Rust + system webview)            ~8 MB    │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  frontend (vanilla JS, no build step)                │  │
│  │   – textarea, suggestion panel, debounced IPC        │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  rust core                                            │  │
│  │   – harper-core 2.0   (rules, <10 ms)        ~2 MB   │  │
│  │   – [planned] candle + gemma 270m INT4      ~65 MB   │  │
│  │   – [planned] gemma fires only when rules don't hit  │  │
│  └──────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

Two-stage check:
1. **Harper rules** — typos, agreement, common style. Synchronous, <10 ms.
2. **Gemma 270M fine-tuned on CoEdIT** — full-sentence rewrites for whatever rules don't catch. Async, ~30 ms target on Apple Silicon.

## Layout

```
quill/
├── shell/                   # Tauri 2 + Rust + Harper
│   ├── src-tauri/
│   │   ├── Cargo.toml
│   │   ├── tauri.conf.json
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   └── lib.rs       # `check` command + Harper wiring + tests
│   │   └── icons/           # placeholder RGBA PNGs (replace before ship)
│   └── src/                 # plain HTML/JS/CSS, no build step
│       ├── index.html
│       ├── main.js
│       └── styles.css
└── train/                   # Python: Gemma 3 270M LoRA on CoEdIT
    ├── pyproject.toml
    ├── configs/lora.yaml
    └── scripts/
        ├── prep_coedit.py
        ├── train.py
        └── eval.py
```

## Status (2026-05-15)

- ✅ `~/quill` skeleton + git init + .gitignore
- ✅ Tauri 2.11 shell scaffolded with `harper-core 2.0` wired through a `check` IPC command
- ✅ Vanilla JS frontend: textarea, debounced check, suggestion buttons (apply replace/insert/remove)
- ✅ Tauri CLI 2.11.1 installed; `cargo tauri dev` launched cleanly, window opened, 4 lints on seed text
- ✅ Perf hoist: `LintGroup` built once in `setup()`, shared via `Mutex` (was rebuilding per-call → first-call cost 3349 ms; expected steady-state 10-50 ms, **not yet externally measured**)
- ✅ Train env validated: `uv sync` installs cleanly (py 3.11.14, torch 2.12, transformers 5.8.1, trl 1.4.0, datasets 4.8.5, peft 0.19.1)
- ✅ CoEdIT dataset confirmed: 69,071 train + 1,712 val, 6 task types (gec, neutralize, simplification, paraphrase, coherence, clarity)
- ✅ `train.py` updated for TRL 1.4 API renames (`max_seq_length` → `max_length`, `tokenizer=` → `processing_class=`); SFTConfig dry-build succeeds from `configs/lora.yaml`
- ✅ Inference scaffold: `inference.rs` + `rewrite`/`capabilities` Tauri commands behind `llm` Cargo feature; `quill-rewrite` CLI binary at `src/bin/quill_rewrite.rs` for offline GGUF testing
- ✅ Frontend Rewrite button + "Apply / Dismiss" UX; shows `harper + llm (no model)` in the header until `QUILL_MODEL` is set
- ✅ Train workstream migrated to Unsloth + self-contained `train/colab.ipynb` (Colab T4, ~8 min)
- ⏳ Training kicked off in Colab browser tab (artifacts → `/content/drive/MyDrive/quill/`)
- ⏳ No GGUF wired into the shell yet — `cargo build --features llm` will pull llama-cpp-2 + compile llama.cpp from source (~2-5 min first time; needs cmake + C++ toolchain)
- ⏳ Brain writeup: `~/brain/projects/quill.md` + `~/brain/research-vault/experiments/E38-quill-shell-bootstrap.md` shipped

## 🎯 Resume here (on "continue")

**Bare `continue` = run these steps in order, no re-briefing.**

1. Confirm shell still compiles: `cd ~/quill/shell/src-tauri && cargo check` — should finish in <5s on a warm cache.
2. Ask the user which fork to push on next:
   - **A) Launch the shell** — install Tauri CLI, run `cargo tauri dev`, smoke-test Harper suggestions in the GUI, then start designing the candle-based LLM inference path.
   - **B) Kick off fine-tune** — `cd ~/quill/train && uv sync`, accept Gemma license on HF, run `scripts/prep_coedit.py` first, then `scripts/train.py` on a cloud GPU (Modal/Colab L4).
   - **C) Stretch goal** — replace Gemma 270M with BitNet b1.58 distilled to ~500M for the ~30 MB total bundle. Research-grade detour, real paper potential.
3. After the user picks, mark the relevant `train/` or `shell/` README's next step as in_progress and execute.

## Known gaps / next concrete tasks

- Replace placeholder 256×256 solid-color RGBA icons with a real icon set before any ship.
- Inference runtime decided: **llama-cpp-2** (won over candle for mature GGUF support + best CPU perf for quantized small models + future BitNet path via bitnet.cpp).
- Once Gemma LoRA trains in Colab, pull `quill-q4_k_m.gguf` from Drive to `~/quill/train/checkpoints/`, then:
  - Test inference: `cargo build --features llm --release --bin quill-rewrite && ./target/release/quill-rewrite --model <path> --text "I has a apple"`
  - Test in GUI: `QUILL_MODEL=<path-to-gguf> cargo tauri dev --features llm`
- Harper's `Span<char>` uses Unicode char offsets, which differ from JS UTF-16 offsets on non-BMP chars. Fine for English ASCII; will break on emoji/CJK. Convert in `check_text` if we ever target multilingual.

## References

- Harper Rust core: <https://github.com/Automattic/harper>
- Gemma 3 270M (gated): <https://huggingface.co/google/gemma-3-270m-it>
- CoEdIT dataset: <https://huggingface.co/datasets/grammarly/coedit>
- Tauri 2 docs: <https://v2.tauri.app/>
- Candle: <https://github.com/huggingface/candle>
- BitNet b1.58 / bitnet.cpp: <https://github.com/microsoft/BitNet>
