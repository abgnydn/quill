"""Train a per-user LoRA adapter on top of the existing CoEdIT-trained model.

INPUT:  ~/Library/Application Support/Quill/journal.jsonl  (exported via the
        Quill main window's "⤓ Export" button to a path you choose, OR fed
        in directly via --journal)
        Format: {"src": "...", "tgt": "...", "kind": "..."}  one per line.

OUTPUT: a small GGUF LoRA adapter (~15 MB at rank 16) that Quill loads on
        top of its base model. Drops into `shell/src-tauri/resources/`
        next to the base `quill-q4_k_m.gguf`.

REHEARSAL: to prevent catastrophic forgetting on a tiny personal dataset,
we mix 50% personal pairs with 50% random CoEdIT pairs each step.

This is the v0.5-phase-1 skeleton. The actual training infrastructure
is the same as modal_train.py; phase 2 makes this runnable end-to-end.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--journal", required=True, help="Exported user journal JSONL")
    ap.add_argument("--base-adapter", default="checkpoints/gemma3-270m-coedit-lora",
                    help="The CoEdIT-trained adapter to start from")
    ap.add_argument("--coedit-rehearsal-frac", type=float, default=0.5,
                    help="Fraction of each batch drawn from CoEdIT (vs personal)")
    ap.add_argument("--max-personal-pairs", type=int, default=2000,
                    help="Cap on personal events used (anti-overfit)")
    ap.add_argument("--epochs", type=int, default=3)
    ap.add_argument("--lr", type=float, default=5e-5,
                    help="Lower than CoEdIT's 1e-4 — fewer steps, smaller deltas")
    ap.add_argument("--out", default="checkpoints/quill-personal-q4_k_m.gguf")
    args = ap.parse_args()

    j = Path(args.journal)
    if not j.exists():
        sys.exit(f"journal not found at {j}")
    pairs = []
    with j.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "src" in row and "tgt" in row and row["tgt"]:
                pairs.append(row)
    print(f"[quill.train_personal] loaded {len(pairs)} personal pairs from {j}")

    if len(pairs) < 10:
        sys.exit("need at least 10 applied edits before personal training is useful")

    # TODO (v0.5 phase 2):
    # 1. Load base + CoEdIT adapter via FastLanguageModel / transformers+peft
    # 2. Build interleaved Dataset: rehearsal_frac from CoEdIT, rest from `pairs`
    # 3. SFTTrainer with very low LR + early stopping on val (held-out 10%)
    # 4. Merge new adapter, run llama.cpp convert + quantize → args.out
    # 5. Drop result into ~/quill/shell/src-tauri/resources/ and notify Quill

    print(f"[quill.train_personal] phase 1 done — phase 2 (actual training) is the next session")
    print(f"[quill.train_personal] would produce {args.out}")


if __name__ == "__main__":
    main()
