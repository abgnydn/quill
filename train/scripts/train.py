"""LoRA fine-tune Gemma 3 270M on CoEdIT.

Run:
    huggingface-cli login   # accept Gemma license once
    uv run python scripts/train.py --config configs/lora.yaml

Hardware target: a single 24 GB GPU comfortably handles this; an M2 Pro with MPS
will work but be slower. For Colab / Modal use an L4 or A10G."""

from __future__ import annotations

import argparse

import yaml
from datasets import load_dataset
from peft import LoraConfig, get_peft_model
from transformers import (
    AutoModelForCausalLM,
    AutoTokenizer,
)
from trl import SFTConfig, SFTTrainer


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

    tok = AutoTokenizer.from_pretrained(cfg["model"]["name"])
    if tok.pad_token is None:
        tok.pad_token = tok.eos_token

    model = AutoModelForCausalLM.from_pretrained(
        cfg["model"]["name"],
        torch_dtype=cfg["model"]["dtype"],
        attn_implementation=cfg["model"].get("attn_implementation", "eager"),
    )

    peft_cfg = LoraConfig(
        r=cfg["lora"]["r"],
        lora_alpha=cfg["lora"]["alpha"],
        lora_dropout=cfg["lora"]["dropout"],
        bias=cfg["lora"]["bias"],
        task_type=cfg["lora"]["task_type"],
        target_modules=cfg["lora"]["target_modules"],
    )
    model = get_peft_model(model, peft_cfg)
    model.print_trainable_parameters()

    ds = load_dataset(cfg["dataset"]["name"])
    train_ds = ds[cfg["dataset"]["split_train"]].map(format_coedit, remove_columns=ds[cfg["dataset"]["split_train"]].column_names)
    eval_split = cfg["dataset"].get("split_eval")
    eval_ds = None
    if eval_split and eval_split in ds:
        eval_ds = ds[eval_split].map(format_coedit, remove_columns=ds[eval_split].column_names)
        if cfg["dataset"].get("max_eval_samples"):
            eval_ds = eval_ds.select(range(min(cfg["dataset"]["max_eval_samples"], len(eval_ds))))

    if cfg["dataset"].get("max_train_samples"):
        train_ds = train_ds.select(range(min(cfg["dataset"]["max_train_samples"], len(train_ds))))

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
        bf16=cfg["train"]["bf16"],
        logging_steps=cfg["train"]["logging_steps"],
        eval_strategy=cfg["train"]["eval_strategy"] if eval_ds else "no",
        eval_steps=cfg["train"]["eval_steps"],
        save_strategy=cfg["train"]["save_strategy"],
        save_steps=cfg["train"]["save_steps"],
        save_total_limit=cfg["train"]["save_total_limit"],
        report_to=cfg["train"]["report_to"],
        seed=cfg["train"]["seed"],
        max_length=cfg["tokenizer"]["max_length"],
        packing=False,
    )

    trainer = SFTTrainer(
        model=model,
        args=sft_cfg,
        train_dataset=train_ds,
        eval_dataset=eval_ds,
        processing_class=tok,
    )
    trainer.train()
    trainer.save_model(cfg["train"]["output_dir"])
    tok.save_pretrained(cfg["train"]["output_dir"])
    print(f"saved LoRA adapter to {cfg['train']['output_dir']}")


if __name__ == "__main__":
    main()
