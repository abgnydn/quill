# quill/train — Gemma 3 270M fine-tune on CoEdIT

Local-first grammar/editing model for the Quill desktop app.

## Setup

```bash
cd ~/quill/train
uv sync
huggingface-cli login   # accept the Gemma license at huggingface.co/google/gemma-3-270m-it
```

## Pipeline

```bash
uv run python scripts/prep_coedit.py                       # inspect CoEdIT
uv run python scripts/train.py --config configs/lora.yaml  # LoRA fine-tune
uv run python scripts/eval.py --adapter ./checkpoints/gemma3-270m-coedit-lora
```

## Hardware

| Setup | Train wall-clock (3 epochs, 69k rows) |
| --- | --- |
| L4 (Colab Pro) | ~1.5 h |
| A10G (Modal) | ~1 h |
| H100 | ~20 min |
| M2 Pro MPS | not benchmarked; expect 6–10 h |

## Next

After LoRA training:
1. Merge adapter into base weights.
2. Quantize to INT4 with `torch.ao.quantization` or convert to GGUF for llama.cpp.
3. Or do BitNet-style ternary distillation for the <30 MB stretch goal.
4. Wire the quantized model into `shell/` via candle or llama-cpp-rs.

See top-level `~/quill/CLAUDE.md` for the resume-here block and full architecture.
