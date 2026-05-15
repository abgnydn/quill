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
- ✅ `cargo check` passes on `shell/src-tauri/`
- ✅ `train/` scaffolded: pyproject.toml, lora.yaml config, prep / train / eval scripts
- ⏳ Nothing run yet: no `cargo tauri dev`, no `uv sync`, no training kicked off
- ⏳ No tauri CLI installed yet (`cargo install tauri-cli --version "^2"` to launch)

## 🎯 Resume here (on "continue")

**Bare `continue` = run these steps in order, no re-briefing.**

1. Confirm shell still compiles: `cd ~/quill/shell/src-tauri && cargo check` — should finish in <5s on a warm cache.
2. Ask the user which fork to push on next:
   - **A) Launch the shell** — install Tauri CLI, run `cargo tauri dev`, smoke-test Harper suggestions in the GUI, then start designing the candle-based LLM inference path.
   - **B) Kick off fine-tune** — `cd ~/quill/train && uv sync`, accept Gemma license on HF, run `scripts/prep_coedit.py` first, then `scripts/train.py` on a cloud GPU (Modal/Colab L4).
   - **C) Stretch goal** — replace Gemma 270M with BitNet b1.58 distilled to ~500M for the ~30 MB total bundle. Research-grade detour, real paper potential.
3. After the user picks, mark the relevant `train/` or `shell/` README's next step as in_progress and execute.

## Known gaps / next concrete tasks

- Wire `cargo install tauri-cli` into the dev workflow doc.
- Replace placeholder 256×256 solid-color RGBA icons with a real icon set before any ship.
- Once Gemma LoRA trains, decide inference runtime: **candle** (pure Rust, our pick) vs **llama-cpp-rs** (faster, more deps).
- INT4 QAT export path is not written yet. Currently we'd export to GGUF via llama.cpp's `convert.py`. Candle path needs separate quantization tooling.
- Harper's `Span<char>` uses Unicode char offsets, which differ from JS UTF-16 offsets on non-BMP chars. Fine for English ASCII; will break on emoji/CJK. Convert in `check_text` if we ever target multilingual.

## References

- Harper Rust core: <https://github.com/Automattic/harper>
- Gemma 3 270M (gated): <https://huggingface.co/google/gemma-3-270m-it>
- CoEdIT dataset: <https://huggingface.co/datasets/grammarly/coedit>
- Tauri 2 docs: <https://v2.tauri.app/>
- Candle: <https://github.com/huggingface/candle>
- BitNet b1.58 / bitnet.cpp: <https://github.com/microsoft/BitNet>
