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
- ✅ Inference scaffold: `inference.rs` + `rewrite`/`capabilities` Tauri commands behind `llm` Cargo feature; `quill-rewrite` CLI binary
- ✅ Train: Modal L4 → 241 MB `quill-q4_k_m.gguf` at `train/checkpoints/`
- ✅ Inference: CLI 0.6s cold, GUI **292 ms steady-state** on M2 Metal
- ✅ Drag-install `.app` at `~/Applications/Quill.app` (260 MB, GGUF bundled as Tauri resource)
- ✅ **Grammarly-style overlay shipping (v0.3)**: AXUI focus tracker + mouse arbiter + click-through window + SVG inline underlines + hover popover + AXUI write-back + fallback panel for web/Electron. See [[E40-quill-overlay-shipping]].
- ✅ `lib.rs` split into `wire.rs` / `state.rs` / `commands.rs`; `overlay.html` split into html + css + js
- ✅ `scripts/install-dev.sh` for the kill+install+codesign+launch dance
- ✅ Tests: 5/5 passing (`cargo test --features overlay --lib`)
- ✅ Brain writeups: `projects/quill.md` + `research-vault/experiments/{E38,E39,E40}-quill-*.md`
- ✅ v0.5 phase 1 (personalization journal): every apply/rewrite_apply event lands in `~/Library/Application Support/Quill/journal.jsonl`; main-window panel shows count + applied + range; export → `{src,tgt}` JSONL for the train pipeline; reset wipes locally. 7/7 tests pass.
- ⏳ v0.5 phase 2: `train/scripts/train_personal.py` skeleton in place — phase 2 implements the actual 50/50-rehearsal LoRA fine-tune on a user journal + base CoEdIT adapter.

## 🎯 Resume here (on "continue")

**Bare `continue` = run these steps in order, no re-briefing.**

1. Confirm tests still pass: `cd ~/quill/shell/src-tauri && cargo test --features overlay --lib` — should be 5/5 in <10s warm.
2. Rebuild and reinstall locally: `./scripts/install-dev.sh --build --tail` — produces a fresh `~/Applications/Quill.app` and streams the relevant `[quill]` / `focus-update` / `cursor-*` / `overlay-js` lines from `/tmp/quill.log`.
3. Ask the user which v0.4 fork to push on:
   - **A — Per-app coverage matrix (E41a)**: Test in 20 common apps; document which expose `kAXBoundsForRangeParameterizedAttribute` vs which need the fallback panel. Builds the "compatible apps" data and is mostly observation, no new code.
   - **B — Clipboard write-back fallback (E41b)**: When `kAXSelectedTextAttribute` set fails (Safari, Chrome, Electron), simulate ⌘C → mutate clipboard → ⌘V via `CGEventPost`. Recovers click-to-fix in the ~50% of apps where AXUI write-back silently no-ops.
   - **C — Menubar mode (E41c)**: Drop the dock icon, replace with a menubar item. Quill becomes ambient — no main window, just the overlay everywhere.
4. After the user picks, mark a new `~/brain/research-vault/experiments/E41-...md` and execute.

## Known gaps / next concrete tasks

- Replace placeholder 256×256 solid-color RGBA icons with a real icon set.
- Local-build prereq for `--features llm`: requires a fresh patch to `~/.cache/llama.cpp/conversion/base.py` (s/< vocab_size/<= vocab_size/) if you ever blow away the cache — see [[E39-quill-coedit-lora]] § "wrong turns" #7.
- Bundle currently ships GGUF inside `.app` (~245 MB total). Future v0.2: quantization sweep (q3_k_s / q2_k_s / embedding pruning) to hit <100 MB without retraining.
- Harper's `Span<char>` uses Unicode char offsets, which differ from JS UTF-16 offsets on non-BMP chars. Fine for English ASCII; will break on emoji/CJK. Convert in `check_text` if we ever target multilingual.

## References

- Harper Rust core: <https://github.com/Automattic/harper>
- Gemma 3 270M (gated): <https://huggingface.co/google/gemma-3-270m-it>
- CoEdIT dataset: <https://huggingface.co/datasets/grammarly/coedit>
- Tauri 2 docs: <https://v2.tauri.app/>
- Candle: <https://github.com/huggingface/candle>
- BitNet b1.58 / bitnet.cpp: <https://github.com/microsoft/BitNet>
