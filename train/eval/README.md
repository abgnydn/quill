# Nib faithful-rewrite eval

Machine-checkable evaluation for "rewrite this in {tone}, {formality} tone,
preserving every fact" — the exact instruction Nib's rewrite panel sends to the
model. Two scoring layers:

1. **Constraint score** (`run_eval.py`) — did the model obey the rules?
2. **LLM-judge** (`--judge`, via `judge.py`) — are the rewrites actually *good*?

## 1. Constraint score

For each case in `cases.jsonl` (50 in-distribution) or `cases-holdout-90.jsonl`
(90 held-out, zero training overlap):

- **WORDS**: output word count within `[min_words, max_words]`
- **FORBID**: zero hits on a per-case banned list (e.g. `"I"`, `"you"`,
  `"committed to ensuring"`)
- **KEEP**: every `must_keep` term is preserved — semantic match (harness v2),
  so `Sept 9` ≡ `September 9`, `$1.85M` ≡ `$1.85 million`, while exact tokens
  like `$47.30`, `12%`, `/v2/search` are kept verbatim

A case **passes** iff all three hold. The prompt template + instruction mirror
`shell/src/overlay.js → composeInstruction()`, so the score tracks what a user
sees in the rewrite panel. Generation goes through Nib's own `nib-rewrite`
binary (same llama-cpp-2 engine as the app).

```bash
# Build the inference binary once:
( cd ../../shell/src-tauri && cargo build --release --features llm --bin nib-rewrite )

# Stock Qwen base vs base + Nib-Faithful adapter:
python run_eval.py --model qwen2.5-1.5b-instruct-q4_k_m.gguf \
  --cases cases-holdout-90.jsonl --label qwen-base --out runs/base.json
python run_eval.py --model qwen2.5-1.5b-instruct-q4_k_m.gguf \
  --adapter nib-faithful-f16.gguf \
  --cases cases-holdout-90.jsonl --label nib-v2 --out runs/nib-v2.json
```

`--binary PATH` overrides the default `nib-rewrite` location. Each case takes
~1–10s; the full 90-case run is a few minutes.

## 2. LLM-judge (real quality)

The constraint score can't tell you whether a rewrite is *better* — only that
it didn't break the rules. `--judge` adds a stronger model (Claude) that scores
each rewrite on four 1–5 axes (**grammaticality, faithfulness, improvement,
fluency**) plus a **better / same / worse** verdict against the source, and
reports a **win-rate** (`better / (better + worse)`).

```bash
pip install anthropic
export ANTHROPIC_API_KEY=sk-ant-...
python run_eval.py --model qwen2.5-1.5b-instruct-q4_k_m.gguf \
  --adapter nib-faithful-f16.gguf --cases cases-holdout-90.jsonl \
  --label nib-v2 --judge --out runs/nib-v2.json
```

Defaults to `claude-opus-4-8`; `--judge-model claude-sonnet-4-6` trades a little
judgment for cost. Each rewrite is one constrained API call (~512 tokens, no
thinking); empty/timed-out rewrites are scored `worse` locally without a call.
The judgment lands per-case under `scores[].judge` and aggregated under
`judge` in the JSON report.

## 3. Cloud eval (Modal — no Mac)

`modal_eval.py` runs the whole thing on a Modal Linux box: it builds
`nib-rewrite` from this repo, downloads the Qwen base + adapter GGUFs, and runs
the judged held-out eval for **both** the base and the base+LoRA, printing the
quality delta the adapter buys.

```bash
pip install modal && modal token new
modal secret create nib-eval ANTHROPIC_API_KEY=sk-ant-...

modal run modal_eval.py                # full judged eval, base vs adapter
modal run modal_eval.py --limit 8      # quick smoke test
modal run modal_eval.py --no-judge     # constraint-only (no API key needed)
```

First run compiles + downloads (~5–10 min); both are cached in a Modal Volume
for fast reruns.

## Files

- `cases.jsonl` — 50 in-distribution cases
- `cases-holdout-90.jsonl` — 90 held-out cases (zero training overlap)
- `run_eval.py` — runner + constraint scorer (+ `--judge`)
- `judge.py` — the Claude LLM-judge (rubric, schema, aggregation)
- `modal_eval.py` — cloud orchestrator (build → download → judged eval)
- `runs/` — output reports (gitignored)
