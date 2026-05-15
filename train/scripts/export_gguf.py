"""Export the fine-tuned Gemma 3 270M LoRA adapter to a quantized GGUF for
llama.cpp / candle / llama-cpp-rs consumption in `~/quill/shell/`.

Run (after training):
    uv run python scripts/export_gguf.py \\
        --adapter ./checkpoints/gemma3-270m-coedit-lora \\
        --out      ./checkpoints/quill-q4_k_m.gguf \\
        --quant    q4_k_m

Recommended quants for a 270M model:
    q4_k_m  ~60 MB on disk, ~30 ms/token on M2 CPU — our shipping target
    q5_k_m  ~75 MB, marginally better quality
    q8_0    ~290 MB, near-fp16 quality — useful for eval
    f16     ~540 MB, baseline

Mac users can run this — it just calls Unsloth's GGUF exporter, which under the
hood drives llama.cpp's quantization tool. No GPU needed.
"""

from __future__ import annotations

import argparse
import sys


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--adapter", required=True, help="LoRA adapter dir from train.py")
    ap.add_argument("--out", required=True, help="Output .gguf path")
    ap.add_argument(
        "--quant",
        default="q4_k_m",
        choices=["q4_k_m", "q5_k_m", "q8_0", "f16"],
        help="GGUF quantization method (default q4_k_m)",
    )
    ap.add_argument("--base", default="unsloth/gemma-3-270m-it")
    args = ap.parse_args()

    try:
        from unsloth import FastLanguageModel
    except ImportError as e:
        print(
            "ERROR: unsloth not installed. Export needs Unsloth's GGUF tooling.\n"
            "Run on the same machine you trained on (Colab/Linux CUDA).\n"
            f"Underlying error: {e}",
            file=sys.stderr,
        )
        sys.exit(1)

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=args.base,
        max_seq_length=512,
        load_in_4bit=False,
        dtype=None,
    )
    model.load_adapter(args.adapter)
    model.save_pretrained_gguf(args.out, tokenizer, quantization_method=args.quant)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
