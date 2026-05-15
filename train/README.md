# quill/train — Gemma 3 270M fine-tune on CoEdIT (Unsloth)

Train the local-first grammar/editing model for [Quill](../CLAUDE.md). LoRA on the [CoEdIT](https://huggingface.co/datasets/grammarly/coedit) corpus via [Unsloth](https://unsloth.ai), then export to a quantized GGUF the desktop shell consumes.

## Where to run

| Where | Wall-clock (3 epochs, 69k rows) | Notes |
| --- | --- | --- |
| **Colab free T4** | **~8 min** | The canonical path. Free, repeatable. |
| Colab Pro L4 | ~3 min | If T4 queue is long. |
| Modal A10G | ~2 min | Cheap, scriptable. |
| H100 | <1 min | Overkill, but if you have it. |
| Apple M2 / M4 | **not supported** | Unsloth depends on Triton; no MLX yet (May 2026). `mlx-tune` is the workaround if you must train locally. |

`scripts/prep_coedit.py` and `scripts/eval.py` work on Mac. Only `train.py` and `export_gguf.py` need a CUDA box.

## Pipeline

### 1. Inspect the data (any machine)

```bash
cd ~/quill/train
uv sync
uv run python scripts/prep_coedit.py
```

Confirms 69,071 train + 1,712 val rows across 6 task types (gec, simplification, paraphrase, coherence, clarity, neutralize).

### 2. Train (Colab T4 or any CUDA GPU)

```bash
huggingface-cli login   # accept the Gemma 3 license once at huggingface.co/google/gemma-3-270m-it
uv run python scripts/train.py --config configs/lora.yaml
```

Outputs LoRA adapter to `./checkpoints/gemma3-270m-coedit-lora/`.

### 3. Eval (anywhere)

```bash
uv run python scripts/eval.py --adapter ./checkpoints/gemma3-270m-coedit-lora
```

Reports exact-match + BLEU on CoEdIT validation, plus 5 qualitative samples.

### 4. Export to GGUF for the desktop shell

```bash
uv run python scripts/export_gguf.py \
    --adapter ./checkpoints/gemma3-270m-coedit-lora \
    --out     ./checkpoints/quill-q4_k_m.gguf \
    --quant   q4_k_m
```

Result: a ~60 MB GGUF the shell will load via `candle` or `llama-cpp-rs`. Other quants available: `q5_k_m` (~75 MB), `q8_0` (~290 MB, near-fp16 quality), `f16` (~540 MB, baseline).

## Why Unsloth

- 30× faster training vs `transformers + peft + trl`, 60% less VRAM.
- Free Colab T4 finishes CoEdIT in ~8 min — no cloud bill.
- Bundled GGUF export — `model.save_pretrained_gguf(...)` is one call, no separate llama.cpp clone.
- Maintains [`unsloth/gemma-3-270m-it-GGUF`](https://huggingface.co/unsloth/gemma-3-270m-it-GGUF) — handy baseline for eval before any fine-tune.

## Files

```
train/
├── pyproject.toml              # deps, with unsloth marked sys_platform != 'darwin'
├── configs/lora.yaml           # all hyperparams
└── scripts/
    ├── prep_coedit.py          # download + inspect dataset (any machine)
    ├── train.py                # Unsloth LoRA fine-tune (CUDA only)
    ├── eval.py                 # exact-match + BLEU on CoEdIT validation
    └── export_gguf.py          # adapter → quantized GGUF (CUDA only)
```

Resume / status / next steps: see `~/quill/CLAUDE.md`.
