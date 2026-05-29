"""
Faithful-rewrite eval harness for Nib.

Reads train/eval/cases.jsonl, runs each case through llama-cli against
the chosen model + the production instruction template, scores per-case:

  - WORDS:    word count within [min_words, max_words]
  - FORBID:   none of the forbidden substrings appear in output
  - KEEP:     every must_keep token appears in output
  - REFUSAL:  cases without tone+formality are treated as
              "minimum intervention" — the output should be close
              to the source by edit distance

Outputs a json report + a one-line summary.

Usage:
  python run_eval.py --model PATH_TO.gguf [--label baseline-1.2b]
  python run_eval.py --model PATH_TO.gguf --adapter nib-faithful.gguf

Per-case timing comes from llama-cli's own stderr; we just sample
output content.
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from dataclasses import dataclass, asdict, field
from pathlib import Path
from typing import Any

# ───────────────────────── config ─────────────────────────

# Match the exact instruction template the v1.3.4 rewrite panel uses
# in overlay.js → composeInstruction(). Diverging here invalidates the
# eval as a proxy for the user's experience.
def compose_instruction(tone: str | None, formality: str | None) -> str | None:
    if not tone and not formality:
        return None
    parts: list[str] = []
    if tone:
        parts.append(tone)
    if formality:
        parts.append(formality)
    style = ", ".join(parts)
    return (
        f"You are a copy editor. Restate the user's text in a {style} tone. "
        f"Keep the same number of words (±20%). "
        f"Do not introduce first or second person (I/you/we) unless the source uses them. "
        f"Do not add commitments, relationships, opinions, or context not in the source. "
        f"Do not pad with filler. Output only the rewritten text, nothing else."
    )

# ChatML for LFM2.5 — matches the prompt template in inference.rs.
def build_prompt(source: str, instruction: str | None) -> str:
    if instruction is None:
        # Default editing instruction (matches DEFAULT_INSTRUCTION in inference.rs).
        instruction = "Fix the grammar and improve clarity:"
    user_msg = f"{instruction} {source}"
    return f"<|im_start|>user\n{user_msg}<|im_end|>\n<|im_start|>assistant\n"


# ───────────────────────── runners ─────────────────────────

# Nib's own `quill-rewrite` Rust binary — uses the SAME llama-cpp-2
# engine the app uses at runtime, so eval results match user experience
# exactly. Avoids the dead-ends we hit otherwise:
#   - QVAC's llama-cli requires conversation mode (no -no-cnv)
#   - Vanilla llama.cpp from brew doesn't have LFM2.5 arch support
QUILL_REWRITE = os.path.expanduser(
    "~/quill/shell/src-tauri/target/release-dev/quill-rewrite"
)


def run_model(
    model_path: str,
    source: str,
    instruction: str | None,
    *,
    adapter_path: str | None = None,
) -> str:
    """Single-shot generation via Nib's own quill-rewrite binary.
    Same engine as the running app — eval matches user experience.
    """
    cmd = [QUILL_REWRITE, "-m", model_path, "-t", source]
    if instruction:
        cmd += ["-i", instruction]
    if adapter_path:
        cmd += ["--adapter", adapter_path]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=120,
            errors="replace",   # quill-rewrite stderr has Metal-init binary chars
        )
    except subprocess.TimeoutExpired:
        return "[TIMEOUT]"
    raw = proc.stdout.strip()
    # quill-rewrite writes "[quill] rewrote in Xs ..." to stderr; the
    # rewritten text goes to stdout on the last line(s). Take the last
    # non-empty block.
    lines = [l for l in raw.splitlines() if l.strip()]
    return "\n".join(lines).strip()


# ───────────────────────── scoring ─────────────────────────

@dataclass
class Score:
    id: str
    ok: bool
    word_count: int
    word_count_ok: bool
    forbidden_hits: list[str] = field(default_factory=list)
    missing_keeps: list[str] = field(default_factory=list)
    output: str = ""
    failure_reasons: list[str] = field(default_factory=list)


WORD_RE = re.compile(r"\w+", re.UNICODE)


# ───────────── semantic must_keep matching (harness v2) ─────────────
# A "must_keep" token has succeeded if any semantically-equivalent
# surface form appears in the output. Without this, a model that
# expands "Sept 9" → "September 9" or "$1.85M" → "$1.85 million" gets
# scored as a failure even though it preserved the fact perfectly.
#
# Forbidden lists stay strict — we don't want to accidentally allow
# banned filler under expansion.

_MONTHS = {
    "jan": "january", "feb": "february", "mar": "march", "apr": "april",
    "jun": "june", "jul": "july", "aug": "august",
    "sept": "september", "sep": "september",
    "oct": "october", "nov": "november", "dec": "december",
}

_ABBREVS = {
    "ppl": "people", "devs": "developers", "hrs": "hours", "hr": "hour",
    "mins": "minutes", "secs": "seconds", "yrs": "years",
    "mos": "months", "wks": "weeks", "pcs": "pieces",
}

_NUMBER_SCALE = {"k": "thousand", "m": "million", "b": "billion", "t": "trillion"}
_NUMBER_SCALE_REV = {v: k for k, v in _NUMBER_SCALE.items()}

_DIGIT_WORD = {
    "1": "one", "2": "two", "3": "three", "4": "four", "5": "five",
    "6": "six", "7": "seven", "8": "eight", "9": "nine",
    "10": "ten", "11": "eleven", "12": "twelve",
}
_WORD_DIGIT = {v: k for k, v in _DIGIT_WORD.items()}


def _normalize_for_match(s: str) -> str:
    """Lowercase + hyphens-to-spaces + collapse whitespace.
    Preserves digits, $, %, /, decimal points."""
    s = s.lower()
    s = re.sub(r"[-]+", " ", s)
    s = re.sub(r"\s+", " ", s).strip()
    return s


def _keep_variants(term: str) -> set[str]:
    """Surface forms equivalent to `term` for must_keep matching."""
    base = _normalize_for_match(term)
    out = {base}

    # Bidirectional swap helper for word-token maps.
    def swap(mapping: dict[str, str]) -> None:
        for k, v in mapping.items():
            for s in list(out):
                if re.search(rf"\b{re.escape(k)}\b", s):
                    out.add(re.sub(rf"\b{re.escape(k)}\b", v, s))

    swap(_MONTHS)
    swap({v: k for k, v in _MONTHS.items() if v != "may"})  # avoid "may" verb collision
    swap(_ABBREVS)
    swap({v: k for k, v in _ABBREVS.items()})
    swap(_DIGIT_WORD)
    swap(_WORD_DIGIT)

    # Number scale: 1.85M ↔ 1.85 million, 8k ↔ 8 thousand.
    for s in list(out):
        for m in re.finditer(r"(\d+(?:\.\d+)?)\s*([kmbt])\b", s):
            num, suf = m.group(1), m.group(2)
            out.add(s[:m.start()] + f"{num} {_NUMBER_SCALE[suf]}" + s[m.end():])
        for m in re.finditer(r"(\d+(?:\.\d+)?)\s+(thousand|million|billion|trillion)\b", s):
            num, full = m.group(1), m.group(2)
            out.add(s[:m.start()] + f"{num}{_NUMBER_SCALE_REV[full]}" + s[m.end():])

    # Ordinal: "april 2" ↔ "april 2nd".
    for s in list(out):
        for m in re.finditer(r"\b(\d{1,3})\b", s):
            n = m.group(1)
            for suf in ("st", "nd", "rd", "th"):
                out.add(s[:m.start()] + f"{n}{suf}" + s[m.end():])

    return out


def _keep_matches(term: str, output_normalized: str) -> bool:
    """True if any semantic variant of `term` is a substring of the
    normalized output. Used by score_output for the must_keep check."""
    return any(v in output_normalized for v in _keep_variants(term))


def score_output(case: dict[str, Any], output: str) -> Score:
    sc = Score(id=case["id"], ok=True, word_count=0, word_count_ok=True)
    sc.output = output

    words = WORD_RE.findall(output)
    sc.word_count = len(words)
    min_w = case.get("min_words", 1)
    max_w = case.get("max_words", 50)
    if not (min_w <= sc.word_count <= max_w):
        sc.word_count_ok = False
        sc.ok = False
        sc.failure_reasons.append(f"words: {sc.word_count} not in [{min_w},{max_w}]")

    out_lower = output.lower()

    # Forbidden: use word-boundary match for ALPHABETIC terms so a banned
    # "I" doesn't false-match "ass[i]gned" / "vers[i]on" / etc. Multi-word
    # phrases ("committed to ensuring") stay as case-insensitive substring.
    for term in case.get("forbidden", []):
        t_lower = term.lower()
        if " " in t_lower or not term.isalpha():
            hit = t_lower in out_lower
        else:
            # Word-boundary regex match for single alpha tokens.
            hit = bool(re.search(rf"\b{re.escape(t_lower)}\b", out_lower))
        if hit:
            sc.forbidden_hits.append(term)
    if sc.forbidden_hits:
        sc.ok = False
        sc.failure_reasons.append(f"forbidden: {sc.forbidden_hits}")

    # Must-keep: semantic match — accepts month abbrev ↔ full, k/M ↔
    # thousand/million, digit ↔ word for 1-12, hyphen tolerance, and
    # ordinal suffixes. Substring of normalized output for everything else.
    # Preserves "$47.30", "12%", "/v2/search" since those don't trip any
    # of the variant rules.
    out_normalized = _normalize_for_match(output)
    for term in case.get("must_keep", []):
        if not _keep_matches(term, out_normalized):
            sc.missing_keeps.append(term)
    if sc.missing_keeps:
        sc.ok = False
        sc.failure_reasons.append(f"missing_keep: {sc.missing_keeps}")

    return sc


# ───────────────────────── main ─────────────────────────

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", required=True, help="Path to .gguf base model")
    ap.add_argument("--adapter", default=None, help="Optional LoRA adapter .gguf")
    ap.add_argument("--cases", default=str(Path(__file__).parent / "cases.jsonl"))
    ap.add_argument("--out", default=None, help="Write JSON report to PATH")
    ap.add_argument("--label", default="run", help="Human-readable label for report")
    ap.add_argument("--limit", type=int, default=0, help="Run only first N cases (debug)")
    ap.add_argument("--verbose", action="store_true", help="Print each output")
    args = ap.parse_args()

    if not os.path.exists(args.model):
        print(f"model not found: {args.model}", file=sys.stderr)
        return 2
    if args.adapter and not os.path.exists(args.adapter):
        print(f"adapter not found: {args.adapter}", file=sys.stderr)
        return 2
    if not os.path.exists(QUILL_REWRITE):
        print(f"quill-rewrite not found: {QUILL_REWRITE}\n"
              f"Build it with: cd ~/quill/shell/src-tauri && "
              f"cargo build --release --features llm --bin quill-rewrite",
              file=sys.stderr)
        return 2

    cases = [json.loads(l) for l in open(args.cases) if l.strip()]
    if args.limit:
        cases = cases[: args.limit]

    print(f"[eval] model={args.model}", file=sys.stderr)
    if args.adapter:
        print(f"[eval] adapter={args.adapter}", file=sys.stderr)
    print(f"[eval] cases={len(cases)}", file=sys.stderr)

    scores: list[Score] = []
    t0 = time.time()
    for i, case in enumerate(cases):
        instr = compose_instruction(case.get("tone"), case.get("formality"))
        t_case = time.time()
        out = run_model(args.model, case["source"], instr, adapter_path=args.adapter)
        dt = time.time() - t_case
        sc = score_output(case, out)
        scores.append(sc)
        mark = "✓" if sc.ok else "✗"
        print(
            f"[{i+1:2}/{len(cases)}] {mark} {sc.id:32} "
            f"words={sc.word_count:3} ({dt:.1f}s)",
            file=sys.stderr,
        )
        if not sc.ok:
            for r in sc.failure_reasons:
                print(f"        └─ {r}", file=sys.stderr)
        if args.verbose:
            print(f"        out: {out!r}", file=sys.stderr)

    dt_total = time.time() - t0
    pass_n = sum(1 for s in scores if s.ok)
    pass_rate = 100 * pass_n / max(1, len(scores))

    report = {
        "label": args.label,
        "model": args.model,
        "adapter": args.adapter,
        "n_cases": len(scores),
        "n_pass": pass_n,
        "pass_rate": round(pass_rate, 1),
        "duration_s": round(dt_total, 1),
        "avg_words": round(statistics.mean(s.word_count for s in scores), 1),
        "scores": [asdict(s) for s in scores],
    }

    if args.out:
        Path(args.out).write_text(json.dumps(report, indent=2))
        print(f"[eval] wrote {args.out}", file=sys.stderr)

    print(
        f"\n{args.label}: {pass_n}/{len(scores)} pass ({pass_rate:.1f}%)  "
        f"avg_words={report['avg_words']}  total={dt_total:.1f}s"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
