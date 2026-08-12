"""Claude-API LLM-judge for Nib rewrites.

The constraint eval (`run_eval.py`) checks word count / forbidden / must_keep —
it answers *"did the model obey the constraints"*, not *"is the rewrite any
good"*. This module adds the missing axis: a stronger model (Claude) scores
each rewrite on four 1–5 dimensions plus a better/same/worse verdict against
the source. It's the cloud-runnable proxy for "would a real person prefer this
rewrite", which is the thing the constraint score can't measure.

Runs anywhere `ANTHROPIC_API_KEY` is set — Modal, Colab, or a local Mac:

    pip install anthropic
    ANTHROPIC_API_KEY=sk-ant-... python eval/run_eval.py \
        --model qwen.gguf --adapter nib-faithful.gguf \
        --cases eval/cases-holdout-90.jsonl --judge

Defaults to `claude-opus-4-8` (override with `--judge-model`). The judge is a
single constrained call per rewrite; no thinking, ~512 output tokens.
"""

from __future__ import annotations

import json
import re
from typing import Any

# Default judge model. Opus is the strongest grader; override via --judge-model
# (e.g. claude-sonnet-4-6) if you want to trade a little judgment for cost.
DEFAULT_JUDGE_MODEL = "claude-opus-4-8"

JUDGE_SYSTEM = (
    "You are a meticulous copy-editing judge. You are given a SOURCE text and a "
    "REWRITE of it produced by a small language model. Score the REWRITE on four "
    "axes, each an integer 1-5 (5 = best):\n"
    "  - grammaticality: is the rewrite free of grammar/spelling errors?\n"
    "  - faithfulness:  does it preserve the source's meaning, facts, numbers, "
    "names, and technical tokens without adding or dropping information?\n"
    "  - improvement:   is it actually better than the source (clearer, cleaner)? "
    "A faithful copy of an already-good source is a 3; real improvement is 4-5; "
    "introducing errors or drifting from the meaning is 1-2.\n"
    "  - fluency:       does it read naturally, like fluent human writing?\n"
    "Then give an overall verdict comparing REWRITE to SOURCE: 'better', 'same', "
    "or 'worse'. Penalize hallucination, padding, meta-commentary (e.g. 'Here is "
    "the rewrite'), and any change of meaning. Be strict. Output only the JSON."
)

# Structured-output schema. Numeric range is enforced via `enum` (json-schema
# `minimum`/`maximum` aren't supported by structured outputs).
JUDGE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "grammaticality": {"type": "integer", "enum": [1, 2, 3, 4, 5]},
        "faithfulness": {"type": "integer", "enum": [1, 2, 3, 4, 5]},
        "improvement": {"type": "integer", "enum": [1, 2, 3, 4, 5]},
        "fluency": {"type": "integer", "enum": [1, 2, 3, 4, 5]},
        "verdict": {"type": "string", "enum": ["better", "same", "worse"]},
        "rationale": {"type": "string"},
    },
    "required": [
        "grammaticality", "faithfulness", "improvement", "fluency",
        "verdict", "rationale",
    ],
    "additionalProperties": False,
}

_AXES = ("grammaticality", "faithfulness", "improvement", "fluency")


def make_client():
    """Construct an Anthropic client. Imported lazily so `run_eval.py` works
    without `anthropic` installed when --judge is off."""
    import anthropic  # noqa: PLC0415

    return anthropic.Anthropic()


def _parse_json(text: str) -> dict[str, Any]:
    """Tolerant JSON extraction: handles ```json fences and leading prose."""
    t = text.strip()
    if t.startswith("```"):
        t = t.strip("`")
        t = t[4:] if t.lower().startswith("json") else t
    try:
        return json.loads(t)
    except json.JSONDecodeError:
        m = re.search(r"\{.*\}", t, re.DOTALL)
        if not m:
            raise
        return json.loads(m.group(0))


def judge_rewrite(
    source: str,
    rewrite: str,
    instruction: str | None,
    *,
    client,
    model: str = DEFAULT_JUDGE_MODEL,
) -> dict[str, Any]:
    """Score one rewrite. Returns the parsed judgment dict (the JUDGE_SCHEMA
    fields) plus `judge_model`. On a refusal or API/parse error returns
    `{"error": ...}` so a single bad case never kills the run. Empty / timed-out
    rewrites are scored 'worse' locally without spending an API call."""
    if not rewrite.strip() or rewrite.strip() == "[TIMEOUT]":
        return {
            "grammaticality": 1, "faithfulness": 1, "improvement": 1, "fluency": 1,
            "verdict": "worse", "rationale": "empty or timed-out output",
            "judge_model": "local", "local": True,
        }

    user = f"SOURCE:\n{source}\n\nREWRITE:\n{rewrite}"
    if instruction:
        user = f"EDITING INSTRUCTION GIVEN TO THE MODEL:\n{instruction}\n\n{user}"

    try:
        # Structured outputs guarantee a single valid-JSON text block. Fall back
        # to a plain call if the installed SDK predates `output_config`.
        try:
            resp = client.messages.create(
                model=model,
                max_tokens=512,
                system=JUDGE_SYSTEM,
                messages=[{"role": "user", "content": user}],
                output_config={"format": {"type": "json_schema", "schema": JUDGE_SCHEMA}},
            )
        except TypeError:
            resp = client.messages.create(
                model=model,
                max_tokens=512,
                system=JUDGE_SYSTEM + " Respond with a single JSON object and nothing else.",
                messages=[{"role": "user", "content": user}],
            )
    except Exception as e:  # noqa: BLE001 — surface any API error as a per-case error
        return {"error": f"{type(e).__name__}: {e}", "judge_model": model}

    if getattr(resp, "stop_reason", None) == "refusal":
        return {"error": "refusal", "judge_model": model}

    text = next((b.text for b in resp.content if getattr(b, "type", None) == "text"), "")
    try:
        data = _parse_json(text)
    except (json.JSONDecodeError, ValueError) as e:
        return {"error": f"unparseable judge output: {e}", "raw": text[:200], "judge_model": model}

    data["judge_model"] = model
    return data


def summarize_judgments(judgments: list[dict[str, Any]]) -> dict[str, Any]:
    """Aggregate per-case judgments into means + a verdict distribution.
    Cases whose judgment carries an `error` are excluded from the means but
    counted in `n_errors`."""
    scored = [j for j in judgments if j and "error" not in j and "verdict" in j]
    n = len(scored)
    out: dict[str, Any] = {
        "n_judged": n,
        "n_errors": sum(1 for j in judgments if j and "error" in j),
    }
    if n:
        for axis in _AXES:
            vals = [j[axis] for j in scored if isinstance(j.get(axis), int)]
            out[f"mean_{axis}"] = round(sum(vals) / len(vals), 2) if vals else None
        verdicts = {"better": 0, "same": 0, "worse": 0}
        for j in scored:
            v = j.get("verdict")
            if v in verdicts:
                verdicts[v] += 1
        out["verdicts"] = verdicts
        # "win rate" = better / (better + worse); ignores the no-change cases.
        decisive = verdicts["better"] + verdicts["worse"]
        out["win_rate"] = round(100 * verdicts["better"] / decisive, 1) if decisive else None
        overall = [
            sum(j[a] for a in _AXES) / len(_AXES)
            for j in scored
            if all(isinstance(j.get(a), int) for a in _AXES)
        ]
        out["mean_overall"] = round(sum(overall) / len(overall), 2) if overall else None
    return out
