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
- ✅ v0.5 phase 1 (personalization journal): every apply/rewrite_apply event lands in `~/Library/Application Support/Quill/journal.jsonl`; main-window panel shows count + applied + range; export → `{src,tgt}` JSONL for the train pipeline; reset wipes locally. 8/8 tests pass.
- ✅ v0.5 phase 2 (personal LoRA pipeline): `RewriteEngine::load_with_adapter` accepts an optional LoRA via `llama-cpp-2`'s `lora_adapter_init` + `lora_adapter_set`. Auto-detects `~/Library/Application Support/Quill/personal-adapter.gguf` on startup. `Capabilities.personal_adapter_loaded` exposes status to the UI — main window shows a green "personal" pill when adapter is active, "base only" otherwise. `train/modal_train_personal.py` is a real end-to-end Modal L4 script: takes a journal export, interleaves with CoEdIT rehearsal 50/50, trains a fresh LoRA, converts to GGUF via `llama.cpp/convert_lora_to_gguf.py`, downloads to `./checkpoints/personal-adapter.gguf`.

## 🎯 Resume here (on "continue")

**Bare `continue` = run these steps in order, no re-briefing.**

Volatile state (phase / commit count / counts) lives in `git log -10`
and `~/Library/Application Support/Quill/{journal.jsonl,config.json}`.
This block describes the current direction, not the current commit.

1. **Verify state:** `cd ~/quill && git log -3 --oneline`, then
   `pgrep -fl Quill.app/Contents/MacOS/quill`. If Quill isn't running,
   reinstall: `./scripts/install-dev.sh --build` (first-time on this
   machine will also `cmake --build` QVAC into `~/.cache/qvac/`, ~5 min
   one-time; subsequent builds reuse the cache).
2. **Run tests:** `./scripts/test.sh` — should be 31 + 2 ignored at
   v0.9 phase 1. If it drops below that count, regression.
3. **Continue the v0.9 QVAC integration.** Phase 1 (bundling) shipped
   on `8ddf0a3`. Next:
   - **Phase 2 — local LoRA training (replace Modal subprocess).**
     New `src/training_local.rs` that wraps the bundled
     `llama-finetune-lora` binary the same way `training.rs` wraps
     `modal run`. `training_scheduler.rs` prefers local when QVAC is
     bundled. Personal training goes from 15 min Modal job + $0.20 to
     ~5 min local + free.
   - **Phase 3 — BitNet inference path.** New `RewriteEngine::Qvac`
     variant shelling out to bundled `llama-cli` for BitNet GGUFs.
     Pick a base: BitDistill Qwen3-0.6B (~150 MB), Falcon3-1B-1.58bit
     (~210 MB), or BitNet-from-scratch on CoEdIT via QVAC's trainer.
   - **Phase 4 — adapter hot-reload.** Auto-swap engine after a
     successful retrain instead of showing the "relaunch to apply"
     badge.
4. **If user pushes back on v0.9 direction**, the unshipped v0.6+
   alternatives still standing: BitNet swap straight to bitnet.cpp
   (rejected per E43 — would be a regression on llama-cpp-2);
   per-app coverage matrix (manual testing, ~2 hrs); repo public +
   launch post.

See [[E43-quill-qvac-integration]] for the recon numbers, why we
picked the QVAC subprocess path over a Rust FFI fork, and the wrong
turns to avoid.

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
