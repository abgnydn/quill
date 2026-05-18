"""Convert a LoRA checkpoint to a quantized GGUF — entirely on your Mac.

Merges the LoRA into base Gemma 3 270M via transformers+peft, then drives
llama.cpp's `convert_hf_to_gguf.py` + `llama-quantize` to produce q4_k_m.

Run (after `modal volume get` has pulled the adapter to ./checkpoints/):

    HF_TOKEN=hf_xxx .venv/bin/python scripts/convert_local.py \\
        --checkpoint ./checkpoints/checkpoint-1200 \\
        --out        ./checkpoints/quill-q4_k_m.gguf

Prereqs (verified at runtime; the script will tell you what's missing):
    - cmake + C++ toolchain (Xcode CLT on macOS — you already have these
      since cargo works)
    - transformers, peft, torch, sentencepiece, protobuf — already in
      `train/.venv`
    - ~/quill/train/.venv/bin/python is the right interpreter
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path


LLAMA_REPO = "https://github.com/ggerganov/llama.cpp"


def need_tool(name: str) -> None:
    if shutil.which(name) is None:
        sys.exit(f"ERROR: `{name}` not found in PATH. Install it first.")


def step(msg: str) -> float:
    print(f"\n=== {msg} ===", flush=True)
    return time.time()


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--checkpoint", required=True, help="LoRA adapter dir (must contain adapter_model.safetensors + adapter_config.json)")
    ap.add_argument("--out", required=True, help="Output .gguf path (e.g. ./checkpoints/quill-q4_k_m.gguf)")
    ap.add_argument("--base", default="google/gemma-3-270m-it", help="Base model on HF (gated — needs HF_TOKEN)")
    ap.add_argument("--quant", default="q4_k_m", choices=["q4_k_m", "q5_k_m", "q8_0", "f16"])
    ap.add_argument("--llama-cpp-dir", default=str(Path.home() / ".cache" / "llama.cpp"),
                    help="Where to clone+build llama.cpp (cached across runs)")
    ap.add_argument("--merged-dir", default=None, help="Where to write the merged 16-bit HF model (default: alongside --out)")
    args = ap.parse_args()

    need_tool("git")
    need_tool("cmake")

    ckpt = Path(args.checkpoint).resolve()
    if not (ckpt / "adapter_config.json").exists():
        sys.exit(f"ERROR: {ckpt}/adapter_config.json not found — wrong path?")

    out_path = Path(args.out).resolve()
    out_path.parent.mkdir(parents=True, exist_ok=True)

    merged_dir = Path(args.merged_dir).resolve() if args.merged_dir else out_path.parent / "merged-16bit"
    if merged_dir.exists():
        shutil.rmtree(merged_dir)

    llama_dir = Path(args.llama_cpp_dir).expanduser().resolve()

    # ---- 1. Merge LoRA into base via transformers+peft ----------------------
    t = step(f"merging LoRA from {ckpt}")
    import torch
    from huggingface_hub import login
    from peft import PeftModel
    from transformers import AutoModelForCausalLM, AutoTokenizer

    if os.environ.get("HF_TOKEN"):
        login(token=os.environ["HF_TOKEN"])

    print(f"  loading base {args.base} on CPU (fp16) …", flush=True)
    base = AutoModelForCausalLM.from_pretrained(
        args.base,
        torch_dtype=torch.float16,
        device_map="cpu",
    )
    tok = AutoTokenizer.from_pretrained(args.base)
    print(f"  attaching adapter and merging …", flush=True)
    model = PeftModel.from_pretrained(base, str(ckpt))
    merged = model.merge_and_unload()
    merged.save_pretrained(str(merged_dir), safe_serialization=True)
    tok.save_pretrained(str(merged_dir))
    print(f"  merged 16-bit HF → {merged_dir}  ({time.time() - t:.1f}s)", flush=True)

    # ---- 2. Clone llama.cpp if needed ---------------------------------------
    if not llama_dir.exists():
        t = step(f"cloning llama.cpp → {llama_dir}")
        llama_dir.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "--depth", "1", LLAMA_REPO, str(llama_dir)], check=True)
        print(f"  cloned in {time.time() - t:.1f}s", flush=True)
    else:
        print(f"\n=== reusing existing llama.cpp at {llama_dir} ===", flush=True)

    # ---- 3. Install llama.cpp Python converter deps -------------------------
    t = step("ensuring llama.cpp python deps")
    subprocess.run(
        [sys.executable, "-m", "pip", "install", "-q", "-r", str(llama_dir / "requirements.txt")],
        check=True,
    )
    print(f"  done ({time.time() - t:.1f}s)", flush=True)

    # ---- 4. Convert HF → GGUF f16 -------------------------------------------
    f16_gguf = out_path.with_name("quill-f16.gguf")
    t = step(f"converting HF → GGUF f16 → {f16_gguf}")
    subprocess.run(
        [sys.executable, str(llama_dir / "convert_hf_to_gguf.py"),
         str(merged_dir), "--outfile", str(f16_gguf), "--outtype", "f16"],
        check=True,
    )
    size_mb = f16_gguf.stat().st_size / (1024 * 1024)
    print(f"  f16 GGUF: {size_mb:.1f} MB  ({time.time() - t:.1f}s)", flush=True)

    if args.quant == "f16":
        f16_gguf.rename(out_path)
        print(f"\n[quill] DONE  {out_path}  ({out_path.stat().st_size / (1024 * 1024):.1f} MB)")
        return

    # ---- 5. Build llama-quantize (cached) -----------------------------------
    quantize_bin = llama_dir / "build" / "bin" / "llama-quantize"
    if not quantize_bin.exists():
        t = step("building llama-quantize")
        subprocess.run(
            ["cmake", "-B", "build",
             "-DGGML_NATIVE=ON",
             "-DLLAMA_BUILD_TESTS=OFF",
             "-DLLAMA_BUILD_EXAMPLES=OFF",
             "-DLLAMA_BUILD_SERVER=OFF"],
            cwd=str(llama_dir), check=True,
        )
        subprocess.run(
            ["cmake", "--build", "build", "--config", "Release",
             "--target", "llama-quantize", "-j", str(os.cpu_count() or 4)],
            cwd=str(llama_dir), check=True,
        )
        print(f"  built in {time.time() - t:.1f}s", flush=True)
    else:
        print(f"\n=== reusing existing llama-quantize at {quantize_bin} ===", flush=True)

    # ---- 6. Quantize ---------------------------------------------------------
    t = step(f"quantizing f16 → {args.quant}")
    subprocess.run(
        [str(quantize_bin), str(f16_gguf), str(out_path), args.quant],
        check=True,
    )
    size_mb = out_path.stat().st_size / (1024 * 1024)
    print(f"\n[quill] DONE  {out_path}  ({size_mb:.1f} MB, {args.quant})  total {time.time() - t:.1f}s for quantize step", flush=True)


if __name__ == "__main__":
    main()
