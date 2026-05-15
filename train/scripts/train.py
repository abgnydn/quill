"""LoRA fine-tune Gemma 3 270M on CoEdIT via Unsloth.

Run (Colab T4 free tier or any CUDA GPU):
    huggingface-cli login   # accept the Gemma license once
    uv run python scripts/train.py --config configs/lora.yaml

Wall-clock on free Colab T4 for full CoEdIT (69k rows, 3 epochs): ~8 minutes.
On L4 / A10G: 2-3 minutes. On H100: <1 minute.

Mac users: this script requires Unsloth (Triton-based) which has no macOS support
yet. Run on Colab. `scripts/prep_coedit.py` and `scripts/eval.py` work locally.
"""

from __future__ import annotations

import argparse
import sys

import yaml
from datasets import load_dataset


GEMMA_CHAT_TEMPLATE = (
    "<start_of_turn>user\n{src}<end_of_turn>\n"
    "<start_of_turn>model\n{tgt}<end_of_turn>"
)


def format_coedit(row: dict) -> dict:
    return {"text": GEMMA_CHAT_TEMPLATE.format(src=row["src"], tgt=row["tgt"])}


def load_cfg(path: str) -> dict:
    with open(path) as f:
        return yaml.safe_load(f)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", default="configs/lora.yaml")
    args = ap.parse_args()
    cfg = load_cfg(args.config)

    # Lazy imports — Unsloth has no macOS support, so importing at module-top
    # would break `python -c "import scripts.train"` on Mac. Keep them inside main.
    try:
        from unsloth import FastLanguageModel, is_bfloat16_supported
    except ImportError as e:
        print(
            "ERROR: unsloth not installed. On macOS, unsloth does not support "
            "MPS yet — run this script on Colab T4 / Linux CUDA instead.\n"
            f"Underlying error: {e}",
            file=sys.stderr,
        )
        sys.exit(1)

    from trl import SFTConfig, SFTTrainer

    model, tokenizer = FastLanguageModel.from_pretrained(
        model_name=cfg["model"]["name"],
        max_seq_length=cfg["model"]["max_seq_length"],
        dtype=cfg["model"]["dtype"],
        load_in_4bit=cfg["model"]["load_in_4bit"],
    )

    model = FastLanguageModel.get_peft_model(
        model,
        r=cfg["lora"]["r"],
        lora_alpha=cfg["lora"]["alpha"],
        lora_dropout=cfg["lora"]["dropout"],
        bias=cfg["lora"]["bias"],
        target_modules=cfg["lora"]["target_modules"],
        use_gradient_checkpointing="unsloth",
        random_state=cfg["train"]["seed"],
        use_rslora=False,
    )

    ds = load_dataset(cfg["dataset"]["name"])
    train_ds = ds[cfg["dataset"]["split_train"]].map(
        format_coedit, remove_columns=ds[cfg["dataset"]["split_train"]].column_names
    )
    eval_split = cfg["dataset"].get("split_eval")
    eval_ds = None
    if eval_split and eval_split in ds:
        eval_ds = ds[eval_split].map(
            format_coedit, remove_columns=ds[eval_split].column_names
        )
        if cfg["dataset"].get("max_eval_samples"):
            eval_ds = eval_ds.select(
                range(min(cfg["dataset"]["max_eval_samples"], len(eval_ds)))
            )

    if cfg["dataset"].get("max_train_samples"):
        train_ds = train_ds.select(
            range(min(cfg["dataset"]["max_train_samples"], len(train_ds)))
        )

    use_bf16 = is_bfloat16_supported()
    sft_cfg = SFTConfig(
        output_dir=cfg["train"]["output_dir"],
        num_train_epochs=cfg["train"]["num_train_epochs"],
        per_device_train_batch_size=cfg["train"]["per_device_train_batch_size"],
        per_device_eval_batch_size=cfg["train"]["per_device_eval_batch_size"],
        gradient_accumulation_steps=cfg["train"]["gradient_accumulation_steps"],
        learning_rate=cfg["train"]["learning_rate"],
        warmup_ratio=cfg["train"]["warmup_ratio"],
        lr_scheduler_type=cfg["train"]["lr_scheduler_type"],
        weight_decay=cfg["train"]["weight_decay"],
        bf16=use_bf16,
        fp16=not use_bf16,
        logging_steps=cfg["train"]["logging_steps"],
        eval_strategy=cfg["train"]["eval_strategy"] if eval_ds else "no",
        eval_steps=cfg["train"]["eval_steps"],
        save_strategy=cfg["train"]["save_strategy"],
        save_steps=cfg["train"]["save_steps"],
        save_total_limit=cfg["train"]["save_total_limit"],
        report_to=cfg["train"]["report_to"],
        seed=cfg["train"]["seed"],
        max_length=cfg["model"]["max_seq_length"],
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
    trainer.train()
    trainer.save_model(cfg["train"]["output_dir"])
    tokenizer.save_pretrained(cfg["train"]["output_dir"])
    print(f"saved LoRA adapter to {cfg['train']['output_dir']}")


if __name__ == "__main__":
    main()
