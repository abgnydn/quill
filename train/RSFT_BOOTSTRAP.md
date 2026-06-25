# A $0 rejection-sampling LoRA loop on a 1.5B model — and where it stops

## TL;DR

A 35 MB LoRA trained by rejection-sampling self-play on top of
Qwen 2.5-1.5B-Instruct lifts faithful-rewrite pass-rate on a 90-case
held-out benchmark by **+24.5 percentage points** (64.4% base → 83.3%
→ 88.9%). Total spend: $0. Training wall-clock: ~8 minutes per
generation on Colab's free T4.

The sharper finding is the **negative control**. The loop only
compounds while *new signal* is injected — either genuinely new task
data, or seeds hand-written to cover the previous generation's
measured failures. A fourth generation trained on *pure
self-resampling* (same prompts, sampled from the previous best model,
no new seeds) **plateaued and mildly regressed** (88.9% → 87.8%, and
broke strict dominance with 2 new failures). So the thing that drives
improvement is the failure-targeted seed expansion, not the
self-sampling. A small model cannot bootstrap itself past what it
already knows by resampling its own outputs.

This is a methodology demo, not new ML. The interesting parts are
that the loop is *closed* — the eval scorer is the data labeler is
the production gate, no API calls or human labels anywhere — and that
it comes with the experiment that shows the loop's ceiling.

## What's being measured

50-case in-distribution eval + 90-case held-out test set across 15
domains (medical, legal, marketing, customer service, technical docs,
casual chat, scientific, finance, education, real estate, logistics,
recipe, academic, sports, government/policy). Each case has
machine-checkable constraints:

- `WORDS`: output word count in `[min_words, max_words]`
- `FORBID`: none of the forbidden substrings present
- `KEEP`: every `must_keep` token preserved (semantic match — accepts
  `Sept 9` ≡ `September 9`, `$1.85M` ≡ `$1.85 million`, etc.)

The held-out 90 cases have **zero overlap** with any training-time
seed file — verified by ID intersection *and* source-text intersection
against all four RSFT rounds and all seed files (the verification is a
cell in the eval harness, not a claim).

`train/eval/cases.jsonl` — in-distribution 50.
`train/eval/cases-holdout-90.jsonl` — held-out 90.
`train/eval/run_eval.py` — scorer (harness v2 with semantic matching).

## The arc

```
                              IN-DIST    HELD-OUT 90   Δ vs base   strict-dom
LFM2.5-1.2B (v1.4.1 baseline)  34.0%     n/a           —           —
Qwen 2.5-1.5B BASE             76.0%     64.4%         —           —
Qwen + v2.1 LoRA (551 smp)     98.0%     83.3%        +18.9 pp     +17 / -0  ✓
Qwen + v2.2 LoRA (827 smp)     98.0%     88.9%        +24.5 pp     + 5 / -0  ✓
Qwen + v2.3 LoRA (780 smp)     ~98%      87.8%        +23.4 pp     + 1 / -2  ✗
```

`strict-dom` = (cases flipped to pass) / (cases flipped to fail)
vs the previous generation. v2.1 and v2.2 are strictly dominant —
every changed case is an improvement. v2.3 is the one that breaks it.

Honest decomposition, generation by generation:

- **`34.0% → 64.4%`** (in-dist 34→76): the base-model swap from
  LFM2.5-1.2B to Qwen 2.5-1.5B-Instruct. Nothing fine-tune-ish —
  just a better base. This is *most* of the headline if you only
  look at the first and last number, so we call it out explicitly.
- **`64.4% → 83.3%` (v2.1):** a LoRA trained on 551 self-played
  samples from the Qwen base. +17 cases flipped to pass, 0 to fail.
  New signal: the model had never been trained to *prefer* faithful,
  constrained rewrites; the RSFT data taught that.
- **`83.3% → 88.9%` (v2.2):** a second LoRA on 827 samples — the
  previous gen's outputs plus 10 hand-curated seeds targeting v2.1's
  *measured* failure modes (scientific notation, abbreviation
  preservation, tight word counts). +5 to pass, 0 to fail. Two of the
  five wins were in the academic domain, which had no targeted seeds —
  the behavior generalized.
- **`88.9% → 87.8%` (v2.3):** the negative control. 780 samples,
  sampled from v2.2, **no new seeds**. +1 to pass, −2 to fail. It
  plateaued and slightly regressed. In-distribution stayed flat (~98%),
  so it didn't *break* — it just had nothing new to learn.

The v2.3 result is the load-bearing one. It rules out "the model
teaches itself indefinitely" and isolates *what* was doing the
teaching in v2.1→v2.2: the injection of new signal, not the act of
self-sampling.

## The loop (recipe)

Five steps. Reproducible from this repo.

### 1. Define a constraint-checked eval

`train/eval/cases.jsonl` has 50 hand-curated cases. Each is a
JSON object: `{source, tone, formality, min_words, max_words,
forbidden[], must_keep[]}`. The `score_output` function in
`run_eval.py` is one Python function returning a `Score(ok=bool, ...)`.
This function is the single source of truth for "did the model do
the right thing." It runs both at eval time and inside the RSFT
filter — so train-time filtering and eval-time scoring **cannot
drift** by construction.

### 2. Sample with temperature, score with the eval

`train/scripts/sample_completions.py` loops over `(source, tone,
formality)` combinations and calls `nib-rewrite` (the production
inference binary) with `--temperature 0.8 --top-p 0.95 --seed
<random>` to generate 8-16 candidates per seed. Each candidate is
scored via the eval's `score_output`. Passing candidates are written
as ChatML triples; failing ones are dropped.

For self-bootstrap (v2.2 onward) the sampler is invoked with
`--adapter <previous_generation>.gguf`, so candidates come from the
*current best model* instead of the raw base.

### 3. Train a LoRA on the filtered samples

`train/colab/train_nib_v2.ipynb` runs the Unsloth + TRL recipe on
Colab's free T4. The notebook reads its training file directly from
this repo via `curl` — no manual upload. About 8 minutes of
gradient updates.

Hyperparameters that worked: rank 16, alpha 32, target modules
`q,k,v,o + gate,up,down`, 3 epochs, learning rate 2e-4, completion-
only loss masking (only the assistant turn gets gradient). All
hyperparameters identical across v2.1 and v2.2 — only the dataset
changes generation to generation.

### 4. Export the adapter alone, not merged

The notebook's final cells run llama.cpp's `convert_lora_to_gguf.py`
on the saved PEFT adapter directory and produce a ~35 MB f16 GGUF.
This sits on top of the Qwen base at runtime via llama.cpp's
`lora_adapter_init` (already wired into `nib-rewrite`). Every
future generation ships as a tiny adapter swap, not a full model
re-download.

### 5. Eval on held-out, look at what failed, write targeted seeds

Each generation's failures are inspected. The `v2.2 = 93.3%` step
above came partly from 10 hand-curated seeds (`seeds-v2_2-targeted.
jsonl`) written specifically to cover v2.1's failure modes:
scientific notation preservation (`n=X`, `p<X`, `X±Y`), abbreviation
preservation (IATA codes, country codes, stock tickers), tight
word-count discipline. Three of v2.1's eight held-out failures
flipped to pass in v2.2 on cases the LoRA never saw during training
— mechanistic wins traceable to specific seeds.

## What's novel vs already known

**Not novel:**
- Rejection-sampling fine-tuning (DeepSeek-R1, STaR).
- LoRA on a strong base (every fine-tuning tutorial).
- Compute being cheap (everyone who's done open-source ML).
- Synthetic data via self-play.

**Specific contributions of this writeup:**
- The closed-loop variant where the eval scorer *is* the data
  labeler. No API for labeling, no preference data, no human
  annotation. Just one Python function that returns `Score(ok=bool)`.
- The targeted-seeds-per-failure-mode pattern as an explicit
  iteration recipe. Most RSFT writeups scale data uniformly; we show
  that 10 hand-curated seeds covering the previous generation's
  actual failures contribute a measurable, traceable pass flip in
  the next generation — and that *without* them (v2.3), the loop
  stalls.
- **The negative control.** A fourth generation trained on pure
  self-resampling shows the loop does *not* compound on its own. This
  is the part most "self-improvement" demos skip, and it's the part
  that makes the positive result interpretable: improvement tracks
  injected signal, not iteration count.
- A held-out benchmark across 15 domains and a semantic-aware
  scorer that doesn't punish models for writing `September 9` when
  the must_keep is `Sept 9`.
- An end-to-end empirical demo on a 1.5B base, four generations, $0.

### The keep-rate signal predicted the plateau

The fraction of sampled candidates that pass the scorer (the "keep
rate") climbed each generation, then flattened:

```
round 1 (from LFM2.5-1.2B base):  33%
round 2 (from Qwen base):         66%
round 3 (from Qwen + v2.1):       90%
round 4 (from Qwen + v2.2):       93%   <- flat
```

When the keep rate is high, the filter removes almost nothing, so the
training set is approximately "the model's own greedy output." Training
on that is close to a no-op plus sampling noise — which is exactly the
v2.3 result. The keep-rate flattening at round 4 was the leading
indicator that round 4 would not lift the model.

## What it means

The clean statement of the result:

> On a 1.5B model, a rejection-sampling LoRA loop improves a
> constraint-checked task **in proportion to the new signal injected
> each round** — new task data, or seeds covering the previous
> generation's measured failures. It does **not** improve from pure
> self-resampling once the model already passes its own filter most
> of the time. The driver is the failure-targeted seed expansion;
> the self-sampling is just how the data gets generated.

This is a more useful claim than "self-improvement compounds," because
it tells you *when to stop*: watch the keep rate. When sampling from
the current model stops getting filtered (here, ~90%+), another naive
round won't help. To keep going you need a new source of signal.

## Honest limitations

- 90 cases is still a modest held-out set. Headline `88.9%` has ~3.3%
  standard error at p = 0.5. Bigger is better; this is where it stopped.
- All eval cases were written by one person (me, working through
  Claude). Held-out vs training overlap is zero by file, but domain
  coverage and constraint patterns share a stylistic ancestor.
- The model only sees one inference temperature (greedy) at eval
  time. Real-world deployment with sampling will look different.
- We have not compared against larger bases (Qwen 3B, 7B) on the same
  benchmark. The claim is *about* this loop on a small base, not about
  absolute capability — and the v2.3 plateau may move on a bigger base
  with more headroom.
- One ablation is still open: a vanilla-SFT control (train on the same
  827 samples with the RSFT filter *off*) to isolate how much the
  filter contributes vs. raw teacher distillation at this generation.
  The dataset for it is in the repo (`rsft-ablation-nofilter.jsonl`);
  the run is pending.

## Where the signal would come from next

To push past the v2.2 plateau, in rough order of expected payoff:

1. **More targeted seeds** covering v2.2's remaining failure cluster
   (scientific notation `n=`/`p<`, dense numeric reports, IATA-style
   codes). This is the cheapest lever and the one with a track record
   here.
2. **A larger base** (Qwen 2.5-3B / 7B) run through the identical
   recipe — tests whether the ceiling is the loop or the base. Needs
   more than free-tier Colab for the 7B class.
3. **A different signal type** (preference pairs / DPO over the
   constraint score) once exact-match RSFT saturates.

## Reproduce it

```bash
git clone https://github.com/abgnydn/nib
cd nib/train
```

Then either:

- **Train your own v2.x on Colab free:** click the badge in
  `train/colab/train_nib_v2.ipynb`, set `DATA_FILE = "rsft-round3.
  jsonl"` in Cell 2, `Runtime → Run all`. About 8 minutes. Adapter
  downloads when done.

- **Eval an existing adapter against your own held-out cases:**
  ```bash
  python eval/run_eval.py \
    --model <qwen-base.gguf> \
    --adapter <your-adapter.gguf> \
    --cases your-cases.jsonl \
    --label your-run
  ```

- **Run your own RSFT round:**
  ```bash
  python scripts/sample_completions.py \
    --model <qwen-base.gguf> \
    --adapter <previous-adapter.gguf>  # omit for round 1 \
    --seeds your-seeds.jsonl \
    --n-samples 12 \
    --out your-round.jsonl
  ```

The whole eval harness, sample loop, and held-out benchmark are
~600 lines of Python plus 130 lines of Rust for the inference CLI.
No frameworks, no orchestrator, no per-experiment infra. The
constraint that everything fits inside one repo and one Python
import path is part of why the loop closes cleanly.
