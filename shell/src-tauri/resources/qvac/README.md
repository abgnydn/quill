This directory is populated by `scripts/install-dev.sh` with the bundled
QVAC Fabric binaries (`llama-cli`, `llama-finetune-lora`, `.dylib`s,
`.metallib`). The binaries themselves are gitignored — they're built fresh
per machine (architecture-specific). This README is committed so Tauri's
resource-bundling glob has something to match before the build runs.

See `src/qvac.rs` for runtime resolution + use.
