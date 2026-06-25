"""Cloud eval for Nib — no Mac required.

Builds the real `nib-rewrite` inference binary from this repo on a Modal Linux
box, downloads the Qwen base + Nib-Faithful adapter GGUFs, and scores the
90-case held-out set TWO ways for BOTH the stock base and the base+LoRA:

  1. the constraint eval (`run_eval.py`)  — word count / forbidden / must_keep
  2. an LLM-as-judge (Claude)             — grammaticality / faithfulness /
     improvement / fluency + better/same/worse  (see judge.py)

So you get the quality delta the adapter actually buys, graded by a stronger
model — the thing the constraint score alone can't tell you. Same inference
engine + prompt template as the shipping app, so results match user experience.

Setup (one time):
    pip install modal && modal token new
    modal secret create nib-eval ANTHROPIC_API_KEY=sk-ant-...

Run:
    modal run train/eval/modal_eval.py                 # full 90-case judged eval
    modal run train/eval/modal_eval.py --limit 8       # quick smoke test
    modal run train/eval/modal_eval.py --no-judge      # constraint-only (no API key needed)
    modal run train/eval/modal_eval.py --judge-model claude-sonnet-4-6

Notes:
  - First run compiles llama.cpp + Tauri (~5-10 min) and downloads the 940 MB
    Qwen base; both are cached in the `nib-eval-cache` Volume, so reruns are fast.
  - The judge runs wherever ANTHROPIC_API_KEY is set — you can also skip Modal
    entirely and run `python eval/run_eval.py --judge` against a local binary.
"""

import json
import os
from pathlib import Path

import modal

REPO_ROOT = Path(__file__).resolve().parents[2]

# Qwen 2.5-1.5B base (public HF GGUF) + the Nib-Faithful adapter (GitHub release)
# — the exact files models.rs resolves at runtime.
QWEN_URL = ("https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/"
            "qwen2.5-1.5b-instruct-q4_k_m.gguf?download=true")
ADAPTER_URL = ("https://github.com/abgnydn/nib/releases/download/v2.1.0/"
               "nib-faithful-f16.gguf")

# Image: Rust + cmake/C++ to build nib-rewrite (which links Tauri, hence the
# GTK/WebKit -dev libs even though this binary never opens a window), plus the
# Python deps the judge needs.
image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install(
        "git", "build-essential", "cmake", "curl", "pkg-config", "libssl-dev",
        # Tauri's Linux build/link prerequisites:
        "libgtk-3-dev", "libwebkit2gtk-4.1-dev", "libsoup-3.0-dev",
        "libjavascriptcoregtk-4.1-dev",
    )
    .run_commands("curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal")
    .env({"PATH": "/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"})
    .pip_install("anthropic", "huggingface_hub")
)

app = modal.App("nib-eval", image=image)

# Repo source (minus build artifacts + big binaries) is mounted fresh each run
# so we always build the exact local code.
repo_mount = modal.Mount.from_local_dir(
    str(REPO_ROOT),
    remote_path="/root/quill",
    condition=lambda p: not (
        "/target/" in p or "/.git/" in p or "/node_modules/" in p
        or p.endswith(".gguf") or p.endswith(".dmg")
    ),
)

# Persists the GGUFs and the cargo target dir across runs.
cache = modal.Volume.from_name("nib-eval-cache", create_if_missing=True)


def _eval_one(binary: str, base: str, adapter, label: str, cases: str,
              limit: int, judge: bool, judge_model: str) -> dict:
    """Run run_eval.py once and return its parsed report (sans per-case scores)."""
    import subprocess
    import sys

    out_path = f"/tmp/{label}.json"
    cmd = [
        sys.executable, "/root/quill/train/eval/run_eval.py",
        "--model", base, "--binary", binary, "--cases", cases,
        "--label", label, "--out", out_path,
    ]
    if adapter:
        cmd += ["--adapter", adapter]
    if limit:
        cmd += ["--limit", str(limit)]
    if judge:
        cmd += ["--judge", "--judge-model", judge_model]
    subprocess.run(cmd, check=True)
    rep = json.loads(Path(out_path).read_text())
    rep.pop("scores", None)  # keep the returned payload small
    return rep


@app.function(
    mounts=[repo_mount],
    volumes={"/cache": cache},
    secrets=[modal.Secret.from_name("nib-eval")],
    cpu=4.0,
    memory=8192,
    timeout=2 * 60 * 60,
)
def run(limit: int = 0, judge: bool = True, judge_model: str = "claude-opus-4-8") -> dict:
    import subprocess

    src = "/root/quill/shell/src-tauri"
    cases = "/root/quill/train/eval/cases-holdout-90.jsonl"
    env = {**os.environ, "CARGO_TARGET_DIR": "/cache/target"}

    # 1. Build the production inference binary (incremental — fast after run 1).
    print("[modal-eval] building nib-rewrite (cargo --features llm)…", flush=True)
    subprocess.run(
        ["cargo", "build", "--release", "--locked", "--features", "llm", "--bin", "nib-rewrite"],
        cwd=src, env=env, check=True,
    )
    binary = "/cache/target/release/nib-rewrite"
    cache.commit()

    # 2. Fetch the GGUFs into the cache volume (skip if already there).
    base = "/cache/qwen2.5-1.5b-instruct-q4_k_m.gguf"
    adapter = "/cache/nib-faithful-f16.gguf"
    for path, url in ((base, QWEN_URL), (adapter, ADAPTER_URL)):
        if not Path(path).exists():
            print(f"[modal-eval] downloading {Path(path).name}…", flush=True)
            subprocess.run(["curl", "-fL", "-o", path, url], check=True)
    cache.commit()

    # 3. Eval the stock base and the base+LoRA, both judged.
    if judge and not os.environ.get("ANTHROPIC_API_KEY"):
        raise RuntimeError(
            "ANTHROPIC_API_KEY not in the `nib-eval` secret — run with --no-judge "
            "or `modal secret create nib-eval ANTHROPIC_API_KEY=sk-ant-...`"
        )
    base_rep = _eval_one(binary, base, None, "qwen-base", cases, limit, judge, judge_model)
    adapter_rep = _eval_one(binary, base, adapter, "nib-faithful-v2", cases, limit, judge, judge_model)
    return {"base": base_rep, "adapter": adapter_rep}


def _fmt(rep: dict) -> str:
    line = f"{rep['label']:18}  pass {rep['n_pass']:3}/{rep['n_cases']:<3} ({rep['pass_rate']:.1f}%)"
    j = rep.get("judge")
    if j and j.get("n_judged"):
        v = j.get("verdicts", {})
        line += (f"   judge {j.get('mean_overall')}/5  "
                 f"(g{j.get('mean_grammaticality')} f{j.get('mean_faithfulness')} "
                 f"i{j.get('mean_improvement')} u{j.get('mean_fluency')})  "
                 f"b/s/w {v.get('better')}/{v.get('same')}/{v.get('worse')}  win {j.get('win_rate')}%")
    return line


@app.local_entrypoint()
def main(limit: int = 0, no_judge: bool = False, judge_model: str = "claude-opus-4-8"):
    res = run.remote(limit=limit, judge=not no_judge, judge_model=judge_model)
    print("\n=== Nib held-out eval (constraint + Claude judge) ===")
    print(_fmt(res["base"]))
    print(_fmt(res["adapter"]))
    # Print the raw reports too, for the record.
    print("\n" + json.dumps(res, indent=2))
