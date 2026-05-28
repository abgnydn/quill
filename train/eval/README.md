# Nib faithful-rewrite eval

Machine-checkable evaluation for "rewrite this in {tone}, {formality}
tone, preserving every fact" — the exact instruction Nib's rewrite
panel sends to the model.

## What it measures

For each of 50 hand-curated cases in `cases.jsonl`:

- **WORDS**: output word count within `[min_words, max_words]`
- **FORBID**: zero hits on a per-case banned list (e.g. `"I"`,
  `"you"`, `"committed to ensuring"`, `"support you through"`)
- **KEEP**: every `must_keep` term appears in output verbatim
  (preserves prices like `$47.30`, dates like `May 30`, symbols
  like `/v2/search`, percentages like `8%`)

A case **passes** iff all three constraints hold.

The prompt template + instruction exactly mirror what
`shell/src/overlay.js → composeInstruction()` builds at runtime, so
the score is a real proxy for what a user sees in the rewrite panel.

## Run

```bash
# Baseline: current 1.2B Instruct
python run_eval.py \
  --model ~/quill-research/models/lfm2.5-1.2b-instruct-q4_k_m.gguf \
  --label baseline-1.2b \
  --out runs/baseline-1.2b.json

# After training the LoRA, compare:
python run_eval.py \
  --model ~/quill-research/models/lfm2.5-1.2b-instruct-q4_k_m.gguf \
  --adapter ../checkpoints/nib-faithful.gguf \
  --label faithful-lora \
  --out runs/faithful-lora.json
```

Each case takes ~3-10s on M2 Metal. Full 50-case run: ~3-8 min.

## Target

- Baseline (raw 1.2B, with v1.3.4 instruction): expect ~30-50%
- After LoRA: target ≥ 80%
- Stretch: ≥ 90%

If a single category dominates failures (e.g. KEEP always misses
dollar amounts), augment training data to cover that pattern.
The harness is the iteration loop.

## Files

- `cases.jsonl` — 50 cases with constraints
- `run_eval.py` — runner + scorer (subprocess llama-cli)
- `runs/` — output reports (gitignored)
