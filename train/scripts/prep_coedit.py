"""Download CoEdIT and inspect it. CoEdIT rows are already instruction-style:
each row has `src` (an editing instruction + source text) and `tgt` (the edited text).

Run:
    uv run python scripts/prep_coedit.py
"""

from __future__ import annotations

from datasets import load_dataset


def main() -> None:
    ds = load_dataset("grammarly/coedit")
    for split in ds.keys():
        print(f"--- {split}: {len(ds[split])} rows ---")
        for row in ds[split].select(range(min(3, len(ds[split])))):
            print({k: (v[:120] + "…" if isinstance(v, str) and len(v) > 120 else v) for k, v in row.items()})
        print()

    if "train" in ds:
        tasks = ds["train"].unique("task") if "task" in ds["train"].column_names else None
        print("task distribution:", tasks)


if __name__ == "__main__":
    main()
