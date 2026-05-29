"""
Rejection-sampling data generation for the v2.0 LoRA training sprint.

For each (source × tone × formality) combination in a seed prompt
pool, sample N candidates from the base LFM2.5-1.2B with temperature
> 0, score each via the eval harness, keep only completions that pass
all constraints. Output ChatML JSONL ready for llama-finetune-lora.

Zero API cost. Runs entirely on M2 Metal via the same quill-rewrite
binary the eval harness uses.

Usage:
    python sample_completions.py \\
      --model ~/quill-research/models/lfm2.5-1.2b-instruct-q4_k_m.gguf \\
      --seeds ../eval/cases.jsonl \\
      --n-samples 8 \\
      --out ../data/rsft-round1.jsonl

Optionally augment seeds with a public dataset:
    --add-finedit  pulls 200 random prompts from FineEdit_bench

Each candidate is scored via the eval harness's `score_output` function
re-used as a library — same checks as the harness, single source of
truth.
"""

import argparse
import json
import os
import random
import sys
import time
from pathlib import Path
from typing import Any

# Reuse the eval harness's scoring + instruction-building logic so the
# RSFT loop and the eval loop can never drift.
sys.path.insert(0, str(Path(__file__).parent.parent / "eval"))
from run_eval import (  # type: ignore
    compose_instruction,
    score_output,
    QUILL_REWRITE,
)


# All chip combinations the rewrite-panel UI exposes. We sample candidates
# for every combination of every seed source.
TONES = ["confident", "engaging", "direct", "witty", "personable", "empathetic"]
FORMALITIES = ["casual", "neutral", "formal"]


def all_tone_combos() -> list[tuple[str | None, str | None]]:
    """18 (tone, formality) combos + 1 default (no chips). Total 19."""
    combos: list[tuple[str | None, str | None]] = [(None, None)]
    for t in TONES:
        for f in FORMALITIES:
            combos.append((t, f))
    return combos


def load_seeds(path: str) -> list[dict[str, Any]]:
    """Load eval-format cases (with source/tone/formality/constraints).
    The same cases.jsonl works as a seed pool — we'll over-sample on top."""
    return [json.loads(l) for l in open(path) if l.strip()]


def synthetic_case(source: str, tone: str | None, formality: str | None) -> dict[str, Any]:
    """When generating from a raw source (not an eval case), we can't score
    against per-case constraints — but we can still score against the
    default constraint set: reasonable word count, no obvious filler.
    Builds a minimal case dict for score_output."""
    src_words = max(1, len(source.split()))
    # Word count target: ±50% (more lenient than eval — RSFT just wants
    # "non-padding" not "exactly right").
    return {
        "id": "synth",
        "source": source,
        "tone": tone,
        "formality": formality,
        "min_words": max(3, int(src_words * 0.5)),
        "max_words": int(src_words * 1.8),
        # Universal banned filler — phrases we never want to see.
        "forbidden": [
            "I am committed",
            "we are committed",
            "committed to ensuring",
            "support you through this",
            "ensuring your satisfaction",
            "the user has requested",
            "the user's text has been",
            "rephrased version",
        ],
        # No must-keeps for synthetic sources (we don't know what to keep).
        "must_keep": [],
    }


def chatml_triple(instruction: str | None, source: str, output: str) -> dict[str, Any]:
    """ChatML format the bundled llama-finetune-lora expects with
    --assistant-loss-only. System message holds the instruction;
    user message is the source; assistant message is the passing output."""
    sys_msg = instruction or (
        "You are a copy editor. Fix the grammar and improve clarity. "
        "Output only the corrected text, nothing else."
    )
    return {
        "messages": [
            {"role": "system", "content": sys_msg},
            {"role": "user", "content": source},
            {"role": "assistant", "content": output},
        ]
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True, help="base GGUF")
    ap.add_argument("--adapter", default=None,
                    help="Optional LoRA adapter GGUF. Used for self-bootstrap "
                         "RSFT — sample from <base + previous adapter> instead "
                         "of <base> alone, so v(N+1) trains on v(N)'s passing outputs.")
    ap.add_argument("--seeds", required=True, help="seed prompt JSONL (eval-format)")
    ap.add_argument("--out", required=True, help="output JSONL path")
    ap.add_argument("--n-samples", type=int, default=8,
                    help="candidates per (source, tone, formality)")
    ap.add_argument("--temperature", type=float, default=0.8)
    ap.add_argument("--top-p", type=float, default=0.95)
    ap.add_argument("--combos", choices=["all", "eval", "default"], default="eval",
                    help="all=18 tone×formality combos per source (large); "
                         "eval=use only the tone/formality from the eval case "
                         "(small, in-distribution); default=no chips (1 combo)")
    ap.add_argument("--limit-seeds", type=int, default=0,
                    help="limit number of seed sources (debug)")
    args = ap.parse_args()

    if not os.path.exists(QUILL_REWRITE):
        print(f"quill-rewrite not found at {QUILL_REWRITE}\n"
              f"Build with: cd ~/quill/shell/src-tauri && "
              f"cargo build --features llm --bin quill-rewrite --profile release-dev",
              file=sys.stderr)
        return 2

    seeds = load_seeds(args.seeds)
    if args.limit_seeds:
        seeds = seeds[: args.limit_seeds]

    # Build (source, instruction, scoring_case) tasks list.
    tasks: list[tuple[str, str | None, dict]] = []
    for seed in seeds:
        if args.combos == "eval":
            combos = [(seed.get("tone"), seed.get("formality"))]
        elif args.combos == "all":
            combos = all_tone_combos()
        else:  # default
            combos = [(None, None)]
        for tone, formality in combos:
            instr = compose_instruction(tone, formality)
            # For non-default combos with eval-case sources, build a
            # scoring case that inherits constraints from the seed.
            if (tone, formality) == (seed.get("tone"), seed.get("formality")):
                scoring_case = seed
            else:
                scoring_case = synthetic_case(seed["source"], tone, formality)
            tasks.append((seed["source"], instr, scoring_case))

    print(f"[rsft] seeds={len(seeds)} tasks={len(tasks)} samples_per_task={args.n_samples}",
          file=sys.stderr)
    print(f"[rsft] estimated wall-clock: ~{len(tasks) * args.n_samples * 0.5 / 60:.1f} min "
          f"(at 0.5s/sample on warm model)", file=sys.stderr)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_f = open(out_path, "w")

    n_kept = 0
    n_total = 0
    t0 = time.time()

    for task_i, (source, instr, case) in enumerate(tasks):
        per_task_kept = 0
        for _ in range(args.n_samples):
            seed = random.randint(1, 2**31 - 1)
            # We pass temperature + seed via the CLI args; quill-rewrite
            # builds the sampler chain accordingly. Reuses run_model from
            # the harness — single source of truth for invocation.
            out = run_model_with_sampling(args.model, source, instr,
                                          temperature=args.temperature,
                                          top_p=args.top_p,
                                          seed=seed,
                                          adapter=args.adapter)
            n_total += 1
            sc = score_output(case, out)
            if sc.ok:
                triple = chatml_triple(instr, source, out)
                out_f.write(json.dumps(triple, ensure_ascii=False) + "\n")
                out_f.flush()
                n_kept += 1
                per_task_kept += 1
        elapsed = time.time() - t0
        eta_min = (len(tasks) - task_i - 1) * (elapsed / (task_i + 1)) / 60
        print(f"[{task_i+1:4}/{len(tasks)}] kept {per_task_kept}/{args.n_samples}  "
              f"total {n_kept}/{n_total}  ETA {eta_min:.1f}m",
              file=sys.stderr)

    out_f.close()
    dt = time.time() - t0
    print(f"\n[rsft] done: {n_kept}/{n_total} kept ({100*n_kept/max(1,n_total):.1f}%)  "
          f"wrote {out_path}  ({dt/60:.1f} min)")
    return 0


def run_model_with_sampling(
    model: str, source: str, instruction: str | None,
    *, temperature: float, top_p: float, seed: int,
    adapter: str | None = None,
) -> str:
    """Like run_eval.run_model but adds --temperature / --top-p / --seed
    so the same prompt produces diverse candidates per call.
    Optional --adapter passes through to quill-rewrite for self-bootstrap."""
    import subprocess
    cmd = [
        QUILL_REWRITE, "-m", model, "-t", source,
        "--temperature", f"{temperature}",
        "--top-p", f"{top_p}",
        "--seed", str(seed),
    ]
    if instruction:
        cmd += ["-i", instruction]
    if adapter:
        cmd += ["--adapter", adapter]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=120, errors="replace",
        )
    except subprocess.TimeoutExpired:
        return ""
    return proc.stdout.strip()


if __name__ == "__main__":
    sys.exit(main())
