# nib/train — the model pipeline

Trains the local LLM that [Nib](../CLAUDE.md) layers on top of Harper. The
current pipeline is a **$0 rejection-sampling LoRA loop on Qwen 2.5-1.5B-
Instruct** — the full recipe, results, and the negative-control experiment
live in **[`RSFT_BOOTSTRAP.md`](./RSFT_BOOTSTRAP.md)** (read that first).

> **History note.** This directory began life as a *Gemma 3 270M on CoEdIT*
> fine-tune (Unsloth/Modal). Nib v2.x dropped Gemma for **LFM2.5-350M** as the
> bundled default and **Qwen 2.5-1.5B + an RSFT LoRA** as the premium tier. The
> Gemma scripts are still here but marked `LEGACY` at the top of each file —
> they are kept for reference, not run. See [Legacy scripts](#legacy-scripts).

## What ships

| Tier | Model | How it's trained | Size |
| --- | --- | --- | --- |
| **Default** | LFM2.5-350M-Instruct (q4_k_m) | stock, bundled in the `.app` | ~219 MB |
| **Premium** | Qwen 2.5-1.5B-Instruct + **Nib-Faithful LoRA** | RSFT loop below | base ~940 MB (download once) + adapter ~36 MB |

The premium adapter is a delta on top of the stock Qwen base, applied at
runtime via llama.cpp's `lora_adapter_init`. Each new generation (v2.1, v2.2,
…) ships as a tiny adapter swap — no base re-download.

## The current pipeline — RSFT on Qwen

A closed loop where the eval scorer *is* the data labeler. No API calls, no
human labels, no preference data. Five steps, all reproducible from this repo:

1. **Constraint-checked eval** — `eval/cases.jsonl` (50 in-dist) +
   `eval/cases-holdout-90.jsonl` (90 held-out, 15 domains). Each case carries
   machine-checkable `WORDS` / `FORBID` / `KEEP` constraints. `eval/run_eval.py`'s
   `score_output()` is the single source of truth.
2. **Sample + score** — `scripts/sample_completions.py` drives the production
   `quill-rewrite` CLI at `--temperature 0.8 --top-p 0.95` to generate 8–16
   candidates per seed, scores each with `score_output`, keeps the passers.
   From v2.2 on it samples through the previous adapter (`--adapter <prev>.gguf`).
3. **Train a LoRA** — `colab/train_nib_v2.ipynb` (Unsloth + TRL on free Colab
   T4, ~8 min). rank 16, alpha 32, target `q,k,v,o + gate,up,down`, 3 epochs,
   LR 2e-4, completion-only loss. Only the dataset changes generation to generation.
4. **Export the adapter alone** — the notebook runs llama.cpp's
   `convert_lora_to_gguf.py` → a ~35 MB f16 GGUF.
5. **Eval held-out, inspect failures, write targeted seeds** — hand-curated
   seeds covering the previous generation's *measured* failure modes are the
   thing that actually moves the metric (see the negative control in
   `RSFT_BOOTSTRAP.md`).

### Results (90-case held-out, greedy)

| Gen | Held-out pass | Δ vs base | Strict dominance |
| --- | --- | --- | --- |
| Qwen 2.5-1.5B base | 64.4% | — | — |
| v2.1 (551 samples) | 83.3% | +18.9 pp | +17 / −0 ✓ |
| v2.2 (827 samples) | 88.9% | +24.5 pp | +5 / −0 ✓ |
| v2.3 (negative control, 780) | 87.8% | +23.4 pp | +1 / −2 ✗ |

The v2.3 plateau is the load-bearing result: pure self-resampling does **not**
compound once the model already passes its own filter (~90%+ keep rate). To
keep improving you need new signal, not more iterations.

## Layout

```
train/
├── RSFT_BOOTSTRAP.md          # ← the canonical writeup (recipe + results + limits)
├── colab/train_nib_v2.ipynb   # ← the live training path (Qwen RSFT LoRA)
├── eval/
│   ├── run_eval.py            # constraint-checked scorer (harness v2, semantic KEEP)
│   ├── cases.jsonl            # 50 in-distribution
│   └── cases-holdout-90.jsonl # 90 held-out, zero train overlap (verified)
├── data/                      # rsft-round{1..4}*.jsonl + seeds-* (the RSFT generations)
├── reports/                   # h2-*.json — version-over-version eval results
├── scripts/
│   └── sample_completions.py  # ← live: rejection-sampling data generator
└── (legacy Gemma/CoEdIT scripts — see below)
```

## Reproduce

```bash
git clone https://github.com/abgnydn/quill && cd quill/train

# Eval an adapter against held-out cases (needs the quill-rewrite binary built
# from ../shell/src-tauri with --features llm):
python eval/run_eval.py \
    --model  <qwen-base.gguf> \
    --adapter <your-adapter.gguf> \
    --cases  eval/cases-holdout-90.jsonl \
    --label  your-run

# Run an RSFT round:
python scripts/sample_completions.py \
    --model   <qwen-base.gguf> \
    --adapter <previous-adapter.gguf>   # omit for round 1 \
    --seeds   your-seeds.jsonl \
    --n-samples 12 \
    --out     your-round.jsonl

# Train: open colab/train_nib_v2.ipynb, set DATA_FILE in Cell 2, Run all (~8 min).
```

## Legacy scripts

These predate the v2.x pivot and target **Gemma 3 270M / CoEdIT**. Each has a
`LEGACY` banner at the top. Not part of the current pipeline; kept for history.

- `modal_train.py`, `modal_convert.py` — Gemma LoRA on Modal L4 + salvage convert.
- `scripts/train.py` + `configs/lora.yaml` — Unsloth Gemma trainer + its hyperparams.
- `scripts/export_gguf.py`, `scripts/convert_local.py` — Gemma adapter → GGUF.
- `scripts/eval.py`, `scripts/prep_coedit.py` — CoEdIT exact-match/BLEU eval + dataset prep.
- `scripts/train_personal.py` — v0.5 personal-LoRA skeleton (never finished).
- `modal_train_personal.py` — **still invoked by the app's Modal fallback**, but
  trains a Gemma adapter that won't load on the LFM2.5/Qwen bases. The supported
  personal-training path is local QVAC (`shell/.../training_local.rs`). Treat the
  Modal path as unmaintained until rebased.
- `colab.ipynb` (root) — the original Gemma Colab notebook; superseded by
  `colab/train_nib_v2.ipynb`.

Resume / status / next steps: see [`../CLAUDE.md`](../CLAUDE.md).
