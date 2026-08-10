# Nib — local-first grammar assistant

A native macOS grammar/writing assistant that pairs a fast Rust rule engine
(Harper) with a small local LLM (GGUF via `llama-cpp-2`). The pitch: better
quality than Harper alone, better latency and footprint than Grammarly, 100%
local, no network call.

> **Naming:** the shipping product is **Nib**, and the rename is complete —
> Cargo crate (`nib` / `nib_lib`), binaries (`nib`, `nib-rewrite`), every
> `[nib]` log prefix, the `NIB_*` env vars, the
> `~/Library/Application Support/Nib/` data dir, and the GitHub repo
> (`abgnydn/nib`) are all `nib`. Only the author's original local clone keeps the
> old name — it still lives at `~/quill` (a fresh `git clone` now lands in
> `nib/`). Existing `…/Quill/` data is migrated to `…/Nib/` on first launch by
> `config::migrate_legacy_data_dir`.

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│  Tauri 2 shell (Rust + system webview), menubar-only        │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  frontend (vanilla JS, no build step)                │  │
│  │   – main window: editor, model picker, settings      │  │
│  │   – overlay: SVG underlines, popover, rewrite panel  │  │
│  └──────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  rust core                                            │  │
│  │   – harper-core 2.0   (rules, <10 ms)                │  │
│  │   – llama-cpp-2: base GGUF + optional LoRA adapter   │  │
│  │   – macOS AXUI overlay (focus track + write-back)    │  │
│  │   – personal training: local QVAC / Modal fallback   │  │
│  └──────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

Two-stage check:
1. **Harper rules** — typos, agreement, common style. Synchronous, <10 ms.
2. **Local LLM rewrite** — full-sentence rewrites for whatever the rules don't
   catch. Async. Triggered on a text selection or `⌘⇧R`.

### Models (two tiers)

| Tier | Model | Trained how | Size |
|---|---|---|---|
| **Default** | LFM2.5-350M-Instruct (q4_k_m) | stock, **bundled** in the `.app` | ~219 MB |
| **Premium** | Qwen 2.5-1.5B + **Nib-Faithful LoRA** | RSFT loop (`train/RSFT_BOOTSTRAP.md`) | base ~940 MB (download) + adapter ~36 MB |

The premium adapter layers on the shared Qwen base via llama.cpp's
`lora_adapter_init` — future generations ship as adapter swaps, no base
re-download. The default build (`tauri.conf.json`) bundles only LFM2.5 +
`resources/qvac/*`; selecting the premium tier downloads the Qwen base from
Hugging Face and then the adapter from its GitHub release URL (the download
queue in `models.rs` fetches both in order). **The `v2.1.0` release with
`nib-faithful-f16.gguf` is not published yet** — until it is, the adapter
download 404s (surfaced as a clear error; the hardened `curl --fail` path
never installs an error page as a model). `tauri.conf.full.json` builds
instead pre-bundle base + adapter from `resources/` (see
`shell/src-tauri/resources/README.md`).

## Layout

```
nib/
├── shell/                   # Tauri 2 + Rust
│   ├── src-tauri/
│   │   ├── Cargo.toml        # features: llm (llama-cpp-2), overlay (macOS AXUI)
│   │   ├── tauri.conf.json   # productName "Nib", id app.nib; tauri.conf.full.json bundles the adapter
│   │   ├── resources/        # bundled lfm2.5-350m gguf + qvac/ binaries
│   │   └── src/
│   │       ├── lib.rs        # tray, ⌘⇧R hotkey, setup/wiring
│   │       ├── wire.rs       # IPC types + Harper run (single source of truth)
│   │       ├── state.rs      # CheckerState / RewriteState + model-path resolve
│   │       ├── commands.rs   # #[tauri::command] thunks
│   │       ├── models.rs     # model registry + curl downloads
│   │       ├── inference.rs  # llama-cpp-2 wrapper: base + LoRA  (feature=llm)
│   │       ├── journal.rs    # private edit journal
│   │       ├── config.rs     # config.json
│   │       ├── training.rs / training_local.rs / training_scheduler.rs
│   │       ├── qvac.rs       # locator for bundled QVAC Fabric binaries
│   │       ├── bin/nib_rewrite.rs   # CLI used by the RSFT sampler
│   │       └── overlay/      # window, focus_tracker, engagement_policy,
│   │                         # mouse_arbiter, engaged_elem, apply, clipboard
│   └── src/                  # index/main/styles + overlay html/js/css
├── train/                    # the model pipeline — see train/README.md
│   ├── RSFT_BOOTSTRAP.md     # canonical writeup of the Qwen RSFT loop
│   ├── colab/train_nib_v2.ipynb   # live training path
│   ├── eval/run_eval.py + cases-*.jsonl   # constraint-checked eval
│   ├── data/ reports/        # RSFT generations + version-over-version results
│   └── scripts/sample_completions.py      # rejection-sampling generator
│                             # (other train/ scripts are LEGACY Gemma/CoEdIT)
└── scripts/                  # install-dev.sh, test.sh
```

## Status (v2.3)

The arc, oldest → newest:

- ✅ v0.x: Tauri + harper-core scaffold; `LintGroup` hoisted into a shared
  `Mutex` (was rebuilt per call).
- ✅ Overlay shipping: AXUI focus tracker + mouse arbiter + click-through
  window + SVG inline underlines + hover popover + AXUI write-back + clipboard
  fallback for web/Electron. `lib.rs`/`overlay.html` split into modules.
- ✅ Personalization: every apply/rewrite lands in `journal.jsonl`; export →
  `{src,tgt}` JSONL; personal LoRA loads on top of the base if present.
- ✅ Menubar mode (LSUIElement) + background auto-retrain scheduler.
- ✅ Multi-model picker + curl downloads + per-app overrides + dictionary + pauses.
- ✅ **Renamed Quill → Nib throughout** (productName, tray, bundle id `app.nib`,
  model display names, Cargo crate `nib`/`nib_lib`, binaries `nib`/`nib-rewrite`,
  `[nib]` logs, `NIB_*` env, data dir → `…/Nib` with a one-time startup
  migration). Only the author's original local clone dir `~/quill` keeps the old
  name; the GitHub repo is now `abgnydn/nib`.
- ✅ QVAC Fabric binaries bundled (`qvac.rs` locator); **local LoRA training**
  via `training_local.rs` wrapping `llama-finetune-lora` on Metal — the
  scheduler prefers it over Modal whenever bundled.
- ✅ **v2.0 model pivot**: dropped Gemma 3 270M. LFM2.5-350M is the bundled
  default; Qwen 2.5-1.5B + an RSFT LoRA is the premium tier.
- ✅ **v2.1–v2.3 RSFT loop** (`train/RSFT_BOOTSTRAP.md`): rejection-sampling
  self-play on Qwen lifts a 90-case held-out benchmark 64.4% → 83.3% → 88.9%
  (legacy substring scorer; 81.1% under the strict word-boundary scorer that
  is now the `run_eval.py` default — see `train/reports/repro-v2.2-*`),
  then a negative-control 4th generation plateaus (87.8%) — proving the loop
  only compounds with new injected signal, not pure self-resampling.
- ✅ Tests green via `./scripts/test.sh` (~67 Rust + 2 ignored, Python AST,
  JS `--check`). The same checks run in CI on every push/PR
  (`.github/workflows/ci.yml`): a **linux** job for the non-overlay Rust suite
  (`--features llm`) plus the Python/JS/shell checks, and a **macos** job for the
  full `--features llm,overlay` suite (the AXUI overlay only compiles on macOS).
  Both stub the unbundled model GGUF (`touch resources/lfm2.5-350m-q4_k_m.gguf`)
  so Tauri's resource-path check passes.

## 🎯 Resume here (on "continue")

**Bare `continue` = run these steps in order, no re-briefing.**

Volatile state lives in `git log -10` and
`~/Library/Application Support/Nib/{journal.jsonl,config.json}`. This block
is the current *direction*, not the current commit.

1. **Verify state:** `cd` into the clone (`~/quill` on the author's original
   machine, `~/dev/nib` on newer setups) `&& git log -3 --oneline`, then
   `pgrep -fl Nib.app/Contents/MacOS/nib`. If Nib isn't running, reinstall:
   `./scripts/install-dev.sh --build` (first build on a fresh machine also
   `cmake --build`s QVAC into `~/.cache/qvac/`, ~5 min one-time).
2. **Run tests:** `./scripts/test.sh` — ~67 + 2 ignored. A drop is a regression.
   (CI re-runs these on every push via `.github/workflows/ci.yml`: linux for the
   non-overlay suite + py/js/sh, macos for the overlay.)
3. **Open threads, roughly in priority order:**
   - **Rebase or retire the Modal personal-training path.**
     `modal_train_personal.py` still trains a *Gemma* adapter that won't load on
     the LFM2.5/Qwen bases; the app's `training.rs` Modal fallback invokes it.
     The local QVAC path (`training_local.rs`) already works and is preferred —
     either rebase the Modal script onto the v2.x bases or drop the fallback.
   - **BitNet inference path.** Add a `RewriteEngine::Qvac` variant shelling out
     to the bundled `llama-cli` for ternary (b1.58) GGUFs — the route to the
     sub-100 MB bundle the original pitch wanted. Pick a base (BitDistill
     Qwen3-0.6B, Falcon3-1B-1.58bit, or BitNet-from-scratch via QVAC's trainer).
   - **Adapter hot-reload.** Swap the engine in place after a successful
     retrain instead of the "relaunch to apply" badge.
   - **Finish the v2.3 ablation.** `train/data/rsft-ablation-nofilter.jsonl`
     exists but the vanilla-SFT (filter-off) control run is still pending — it
     isolates how much the RSFT filter contributes vs. raw teacher distillation.
   - **Next RSFT generation:** more targeted seeds for v2.2's remaining failure
     cluster (scientific notation `n=`/`p<`, dense numeric reports, IATA codes),
     or a larger base run through the identical recipe.

## Known gaps / next concrete tasks

- The Quill→Nib rename is complete (crate/binaries/logs/`NIB_*` env/data-dir
  all `nib`, with a startup migration of the old `…/Quill/` data dir via
  `config::migrate_legacy_data_dir`). Only the author's original local clone dir
  `~/quill` retains the old name (a fresh clone now lands in `nib/`); the GitHub
  repo is now `abgnydn/nib`.
- The default `.app` bundle is **~260 MB** (LFM2.5 default), not the original
  ~80 MB north star — BitNet is the path back toward it.
- Cargo `default = []` — a bare `cargo build` is Harper-only; the real app
  needs `--features llm,overlay`.
- **Publish the `v2.1.0` release asset**: `gh release create v2.1.0` with the
  v2.2 adapter exported as `nib-faithful-f16.gguf` — the in-app premium
  download points there and 404s until it exists.
- The Modal train dir resolves `NIB_TRAIN_DIR` → `~/quill/train` →
  `~/dev/nib/train` (see `training::default_train_dir`). The whole Modal path
  is additionally gated behind the `allow_cloud_training` config flag
  (default **off** — the journal is the user's typed text and must not be
  uploaded implicitly).
- The AXUI apply path now converts Harper's char offsets to UTF-16 for
  `kAXSelectedTextRangeAttribute` (emoji/CJK-safe). The overlay *rendering*
  path (`bounds_for_range`) still passes char offsets — underline positions
  can drift after non-BMP chars even though applies land correctly.
- The overlay is a single fixed 4096×3072 window anchored at the primary
  display; secondary displays get no underlines (the plausibility gate also
  rejects far-negative global coords). Multi-display needs per-display
  windows.
- Replace the placeholder solid-color RGBA icons with a real icon set.

## References

- This repo: `train/RSFT_BOOTSTRAP.md` — the canonical RSFT writeup (recipe,
  results, negative control, limitations).
- Harper Rust core: <https://github.com/Automattic/harper>
- llama.cpp / `llama-cpp-2`: <https://github.com/ggerganov/llama.cpp> ·
  <https://github.com/utilityai/llama-cpp-rs>
- Qwen 2.5: <https://huggingface.co/Qwen> · LFM2.5: <https://huggingface.co/LiquidAI>
- CoEdIT (eval-seed lineage): <https://huggingface.co/datasets/grammarly/coedit>
- Tauri 2 docs: <https://v2.tauri.app/>
- BitNet b1.58 / bitnet.cpp: <https://github.com/microsoft/BitNet>
- External brain writeups (author's research-vault): `E38`–`E43` ledger entries
  cover the shell bootstrap, the fine-tune/GGUF saga, the overlay/TCC fights,
  and the QVAC integration recon.
