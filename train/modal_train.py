"""Train Gemma 3 270M LoRA on CoEdIT on a Modal L4, end-to-end.

Why this instead of Colab:
- L4 supports bf16 → real ~8-15 min wall-clock (not 4.5 h fp32 on T4).
- Reliable: Modal doesn't randomly kill jobs.
- No Drive / mount / notebook-kernel fragility — script runs from your Mac
  terminal, GGUF lands directly in `./checkpoints/quill-q4_k_m.gguf`.

Setup (one time, ~2 min):
    pip install modal
    modal token new

Run (from `~/quill/train`):
    HF_TOKEN=hf_xxx modal run modal_train.py

This streams logs to your terminal. When it finishes, the GGUF is downloaded
to `./checkpoints/quill-q4_k_m.gguf` and an `adapter/` directory contains
the LoRA weights (in case you want to retrain quantization later without
redoing training).

Cost on Modal: L4 at ~$0.80/hr × ~15 min ≈ $0.20. New accounts get $30/mo
free credit, so likely $0 for the first run.
"""

import os
import pathlib
import subprocess

import modal

APP_NAME = "quill-train"

# --- Image: CUDA Linux, exactly the deps that work for Gemma 3 270M -----------

image = (
    modal.Image.debian_slim(python_version="3.11")
    .apt_install("git", "build-essential", "cmake")
    # CUDA-enabled torch first — Modal mounts NVIDIA drivers at runtime, so we
    # only need the cu124 wheels here.
    .pip_install(
        "torch==2.5.1",
        extra_options="--index-url https://download.pytorch.org/whl/cu124",
    )
    # Let unsloth resolve its own compatible transformers/trl/peft/accelerate/
    # bitsandbytes/xformers — the version cross-product is brittle to pin by
    # hand and unsloth's wheel knows what it wants.
    .pip_install("unsloth")
    # GGUF conversion essentials (the llama.cpp converter pulls additional
    # deps from its own requirements.txt at runtime).
    .pip_install("sentencepiece", "protobuf")
)

app = modal.App(APP_NAME, image=image)

# Volume holds artifacts across runs and lets us download the GGUF cheaply.
volume = modal.Volume.from_name("quill-artifacts", create_if_missing=True)

GEMMA_CHAT_TEMPLATE = (
    "<start_of_turn>user\n{src}<end_of_turn>\n"
    "<start_of_turn>model\n{tgt}<end_of_turn>"
)


@app.function(
    gpu="L4",
    timeout=60 * 60,  # 1 h cap; expect ~15 min
    volumes={"/artifacts": volume},
    secrets=[modal.Secret.from_dict({"HF_TOKEN": os.environ.get("HF_TOKEN", "")})],
)
def train_and_export() -> dict:
    import subprocess
    import time
    from pathlib import Path

    import torch
    from datasets import load_dataset
    from huggingface_hub import login
    from unsloth import FastLanguageModel, is_bfloat16_supported

    hf_token = os.environ.get("HF_TOKEN")
    if not hf_token:
        raise RuntimeError("HF_TOKEN not set — Gemma 3 is license-gated.")
    login(token=hf_token)

    print(f"[quill] GPU: {torch.cuda.get_device_name(0)}")
    print(f"[quill] bf16 supported: {is_bfloat16_supported()}")

    # CoEdIT prompts + targets fit comfortably in 256 tokens (95th percentile
    # is around 180). Shorter = quadratically less attention compute.
    MAX_SEQ = 256
    OUT = Path("/artifacts/gemma3-270m-coedit-lora")
    OUT.mkdir(parents=True, exist_ok=True)

    # --- Load + LoRA -------------------------------------------------------
    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name="unsloth/gemma-3-270m-it",
        max_seq_length=MAX_SEQ,
        dtype=None,
        load_in_4bit=True,
    )
    model = FastLanguageModel.get_peft_model(
        model,
        r=16,
        lora_alpha=32,
        # 0 keeps Unsloth's fast LoRA path. 0.05 forces a "patch everything
        # except LoRA" mode that's ~5-10× slower on small models.
        lora_dropout=0.0,
        bias="none",
        target_modules=[
            "q_proj", "k_proj", "v_proj", "o_proj",
            "gate_proj", "up_proj", "down_proj",
        ],
        use_gradient_checkpointing="unsloth",
        random_state=1337,
        use_rslora=False,
    )

    # --- Data --------------------------------------------------------------
    def fmt(row):
        return {"text": GEMMA_CHAT_TEMPLATE.format(src=row["src"], tgt=row["tgt"])}

    ds = load_dataset("grammarly/coedit")
    train_ds = ds["train"].map(fmt, remove_columns=ds["train"].column_names)
    eval_ds = ds["validation"].map(fmt, remove_columns=ds["validation"].column_names).select(range(1000))
    print(f"[quill] train rows={len(train_ds)} eval rows={len(eval_ds)}")

    # --- Train -------------------------------------------------------------
    from trl import SFTConfig, SFTTrainer

    use_bf16 = is_bfloat16_supported()
    sft_cfg = SFTConfig(
        output_dir=str(OUT),
        num_train_epochs=1,
        per_device_train_batch_size=32,
        per_device_eval_batch_size=32,
        gradient_accumulation_steps=1,
        learning_rate=1e-4,
        warmup_ratio=0.03,
        lr_scheduler_type="cosine",
        weight_decay=0.0,
        bf16=use_bf16,
        fp16=not use_bf16,
        logging_steps=25,
        eval_strategy="steps",
        eval_steps=200,
        save_strategy="steps",
        save_steps=400,
        save_total_limit=2,
        report_to="none",
        seed=1337,
        max_length=MAX_SEQ,
        packing=False,
        optim="adamw_8bit",
    )
    trainer = SFTTrainer(
        model=model,
        args=sft_cfg,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        processing_class=tokenizer,
    )

    t0 = time.time()
    train_result = trainer.train()
    train_seconds = time.time() - t0
    print(f"[quill] training done in {train_seconds/60:.1f} min")
    trainer.save_model(str(OUT))
    tokenizer.save_pretrained(str(OUT))

    # --- Merge to 16-bit HF -----------------------------------------------
    MERGED = Path("/artifacts/merged-16bit")
    if MERGED.exists():
        subprocess.run(["rm", "-rf", str(MERGED)], check=True)
    model.save_pretrained_merged(str(MERGED), tokenizer, save_method="merged_16bit")
    print(f"[quill] merged 16-bit saved to {MERGED}")

    # --- Manual llama.cpp conversion (bypasses broken unsloth_zoo path) ---
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
    subprocess.run(
        [
            "python", str(LLAMA_DIR / "convert_hf_to_gguf.py"),
            str(MERGED),
            "--outfile", str(F16_GGUF),
            "--outtype", "f16",
        ],
        check=True,
    )

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
         "--target", "llama-quantize", "-j", "4"],
        cwd=str(LLAMA_DIR), check=True,
    )

    Q4_GGUF = Path("/artifacts/quill-q4_k_m.gguf")
    subprocess.run(
        [str(LLAMA_DIR / "build" / "bin" / "llama-quantize"),
         str(F16_GGUF), str(Q4_GGUF), "q4_k_m"],
        check=True,
    )

    size_mb = Q4_GGUF.stat().st_size / (1024 * 1024)
    print(f"[quill] q4_k_m GGUF: {size_mb:.1f} MB at {Q4_GGUF}")

    volume.commit()
    return {
        "train_seconds": train_seconds,
        "gguf_path": str(Q4_GGUF),
        "gguf_size_mb": size_mb,
        "train_metrics": train_result.metrics,
    }


@app.local_entrypoint()
def main():
    print("[quill] kicking off Modal L4 training …")
    result = train_and_export.remote()
    print(f"[quill] done: {result}")

    # Stream artifacts back to ./checkpoints/
    local_dir = pathlib.Path("./checkpoints")
    local_dir.mkdir(parents=True, exist_ok=True)
    print(f"[quill] downloading GGUF to {local_dir}/quill-q4_k_m.gguf …")
    subprocess.run(
        ["modal", "volume", "get", "--force", "quill-artifacts",
         "quill-q4_k_m.gguf", str(local_dir / "quill-q4_k_m.gguf")],
        check=True,
    )
    print(f"[quill] also fetching the LoRA adapter dir …")
    subprocess.run(
        ["modal", "volume", "get", "--force", "quill-artifacts",
         "gemma3-270m-coedit-lora", str(local_dir / "gemma3-270m-coedit-lora")],
        check=True,
    )
    print("[quill] artifacts:")
    subprocess.run(["ls", "-lah", str(local_dir)], check=True)
