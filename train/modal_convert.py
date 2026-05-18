"""Salvage: convert an already-trained LoRA checkpoint in the quill-artifacts
Modal volume into a quantized GGUF. CPU-only — no GPU contention, costs cents.

Use this when training was interrupted but at least one checkpoint exists in
`/artifacts/gemma3-270m-coedit-lora/checkpoint-NNN/`.

Run:
    HF_TOKEN=hf_xxx .venv/bin/modal run modal_convert.py
    # or to point at a specific checkpoint:
    HF_TOKEN=hf_xxx .venv/bin/modal run modal_convert.py --checkpoint checkpoint-1200
"""

import os
import pathlib
import subprocess

import modal

APP_NAME = "quill-convert"

image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("git", "build-essential", "cmake")
    .pip_install(
        "torch==2.5.1",
        extra_options="--index-url https://download.pytorch.org/whl/cpu",
    )
    .pip_install(
        "transformers>=4.50",
        "peft>=0.13",
        "accelerate>=1.0",
        "huggingface_hub>=0.26",
        "sentencepiece",
        "protobuf",
        "safetensors",
    )
)

app = modal.App(APP_NAME, image=image)
volume = modal.Volume.from_name("quill-artifacts", create_if_missing=True)


@app.function(
    cpu=8,
    memory=16384,
    timeout=30 * 60,
    volumes={"/artifacts": volume},
    secrets=[modal.Secret.from_dict({"HF_TOKEN": os.environ.get("HF_TOKEN", "")})],
)
def convert(checkpoint: str = "auto") -> dict:
    import shutil
    import time
    from pathlib import Path

    import torch
    from huggingface_hub import login
    from peft import PeftModel
    from transformers import AutoModelForCausalLM, AutoTokenizer

    hf_token = os.environ.get("HF_TOKEN")
    if not hf_token:
        raise RuntimeError("HF_TOKEN not set — base Gemma 3 is license-gated.")
    login(token=hf_token)

    # --- Pick the checkpoint --------------------------------------------------
    train_dir = Path("/artifacts/gemma3-270m-coedit-lora")
    if not train_dir.exists():
        raise RuntimeError(f"{train_dir} missing from volume — did training run at all?")

    if checkpoint == "auto":
        ckpts = sorted(
            [p for p in train_dir.iterdir() if p.name.startswith("checkpoint-")],
            key=lambda p: int(p.name.split("-")[1]),
        )
        if not ckpts:
            raise RuntimeError(f"no checkpoint-* dirs in {train_dir}")
        ckpt_path = ckpts[-1]
    else:
        ckpt_path = train_dir / checkpoint
        if not ckpt_path.exists():
            raise RuntimeError(f"{ckpt_path} not found")

    print(f"[quill] using LoRA checkpoint: {ckpt_path}")
    print(f"[quill] checkpoint contents: {[p.name for p in ckpt_path.iterdir()]}")

    # --- Load base + merge LoRA ----------------------------------------------
    BASE = "google/gemma-3-270m-it"
    print(f"[quill] loading base {BASE} (CPU, fp16) …")
    t0 = time.time()
    base = AutoModelForCausalLM.from_pretrained(
        BASE,
        torch_dtype=torch.float16,
        device_map="cpu",
    )
    tok = AutoTokenizer.from_pretrained(BASE)
    print(f"[quill] base loaded in {time.time() - t0:.1f}s")

    print(f"[quill] applying LoRA from {ckpt_path} …")
    model = PeftModel.from_pretrained(base, str(ckpt_path))
    print(f"[quill] merging LoRA into base weights …")
    merged = model.merge_and_unload()

    MERGED = Path("/artifacts/merged-16bit")
    if MERGED.exists():
        shutil.rmtree(MERGED)
    merged.save_pretrained(str(MERGED), safe_serialization=True)
    tok.save_pretrained(str(MERGED))
    print(f"[quill] merged 16-bit HF saved to {MERGED}")
    print(f"[quill] merged dir size: ", end="")
    subprocess.run(["du", "-sh", str(MERGED)], check=True)

    # --- llama.cpp converter -------------------------------------------------
    LLAMA_DIR = Path("/tmp/llama.cpp")
    if not LLAMA_DIR.exists():
        subprocess.run(
            ["git", "clone", "--depth", "1",
             "https://github.com/ggerganov/llama.cpp", str(LLAMA_DIR)],
            check=True,
        )
    subprocess.run(
        ["pip", "install", "-q", "-r", str(LLAMA_DIR / "requirements.txt")],
        check=True,
    )

    F16_GGUF = Path("/artifacts/quill-f16.gguf")
    print(f"[quill] converting HF → GGUF f16 …")
    t1 = time.time()
    subprocess.run(
        ["python", str(LLAMA_DIR / "convert_hf_to_gguf.py"),
         str(MERGED), "--outfile", str(F16_GGUF), "--outtype", "f16"],
        check=True,
    )
    print(f"[quill] f16 GGUF written in {time.time() - t1:.1f}s: ", end="")
    subprocess.run(["du", "-h", str(F16_GGUF)], check=True)

    print(f"[quill] building llama-quantize …")
    subprocess.run(
        ["cmake", "-B", "build",
         "-DGGML_NATIVE=OFF",
         "-DLLAMA_BUILD_TESTS=OFF",
         "-DLLAMA_BUILD_EXAMPLES=OFF",
         "-DLLAMA_BUILD_SERVER=OFF"],
        cwd=str(LLAMA_DIR), check=True,
    )
    subprocess.run(
        ["cmake", "--build", "build", "--config", "Release",
         "--target", "llama-quantize", "-j", "8"],
        cwd=str(LLAMA_DIR), check=True,
    )

    Q4_GGUF = Path("/artifacts/quill-q4_k_m.gguf")
    print(f"[quill] quantizing → q4_k_m …")
    subprocess.run(
        [str(LLAMA_DIR / "build" / "bin" / "llama-quantize"),
         str(F16_GGUF), str(Q4_GGUF), "q4_k_m"],
        check=True,
    )
    size_mb = Q4_GGUF.stat().st_size / (1024 * 1024)
    print(f"[quill] DONE  q4_k_m GGUF: {size_mb:.1f} MB at {Q4_GGUF}")

    volume.commit()
    return {
        "checkpoint_used": str(ckpt_path),
        "gguf_path": str(Q4_GGUF),
        "gguf_size_mb": size_mb,
    }


@app.local_entrypoint()
def main(checkpoint: str = "auto"):
    print(f"[quill] kicking off CPU-only conversion (checkpoint={checkpoint}) …")
    result = convert.remote(checkpoint=checkpoint)
    print(f"[quill] modal result: {result}")

    local_dir = pathlib.Path("./checkpoints")
    local_dir.mkdir(parents=True, exist_ok=True)
    out = local_dir / "quill-q4_k_m.gguf"
    print(f"[quill] downloading GGUF → {out} …")
    subprocess.run(
        ["modal", "volume", "get", "--force", "quill-artifacts",
         "quill-q4_k_m.gguf", str(out)],
        check=True,
    )
    print("[quill] also fetching the LoRA adapter dir …")
    subprocess.run(
        ["modal", "volume", "get", "--force", "quill-artifacts",
         "gemma3-270m-coedit-lora", str(local_dir / "gemma3-270m-coedit-lora")],
        check=True,
    )
    subprocess.run(["ls", "-lah", str(local_dir)], check=True)
