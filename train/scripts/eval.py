"""Evaluate a fine-tuned (or base) Gemma 3 270M on CoEdIT validation + JFLEG.

Computes:
- exact-match accuracy on CoEdIT validation
- corpus BLEU vs. CoEdIT targets
- sample qualitative outputs

Run:
    uv run python scripts/eval.py --adapter ./checkpoints/gemma3-270m-coedit-lora
    uv run python scripts/eval.py  # base model only
"""

from __future__ import annotations

import argparse

import sacrebleu
from datasets import load_dataset
from peft import PeftModel
from transformers import AutoModelForCausalLM, AutoTokenizer


def build_prompt(src: str) -> str:
    return f"<start_of_turn>user\n{src}<end_of_turn>\n<start_of_turn>model\n"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="google/gemma-3-270m-it")
    ap.add_argument("--adapter", default=None)
    ap.add_argument("--n", type=int, default=200)
    ap.add_argument("--max-new-tokens", type=int, default=128)
    args = ap.parse_args()

    tok = AutoTokenizer.from_pretrained(args.base)
    model = AutoModelForCausalLM.from_pretrained(args.base, torch_dtype="bfloat16")
    if args.adapter:
        model = PeftModel.from_pretrained(model, args.adapter)
    model.eval()

    ds = load_dataset("grammarly/coedit", split="validation").select(range(args.n))

    preds, refs, exact = [], [], 0
    for i, row in enumerate(ds):
        prompt = build_prompt(row["src"])
        inputs = tok(prompt, return_tensors="pt").to(model.device)
        out = model.generate(
            **inputs,
            max_new_tokens=args.max_new_tokens,
            do_sample=False,
            pad_token_id=tok.pad_token_id or tok.eos_token_id,
        )
        decoded = tok.decode(out[0][inputs.input_ids.shape[1]:], skip_special_tokens=True).strip()
        decoded = decoded.split("<end_of_turn>")[0].strip()
        preds.append(decoded)
        refs.append(row["tgt"])
        if decoded.strip() == row["tgt"].strip():
            exact += 1
        if i < 5:
            print(f"\n[{i}]")
            print(f"  src : {row['src'][:200]}")
            print(f"  pred: {decoded[:200]}")
            print(f"  ref : {row['tgt'][:200]}")

    bleu = sacrebleu.corpus_bleu(preds, [refs])
    print(f"\nexact-match: {exact}/{len(ds)} = {exact/len(ds):.3f}")
    print(f"BLEU: {bleu.score:.2f}")


if __name__ == "__main__":
    main()
