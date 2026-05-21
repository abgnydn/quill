#!/usr/bin/env bash
# Quill — run the entire automated test suite.
#
#   - Rust:   `cargo test --features llm,overlay --lib`        (17 tests)
#   - Python: AST-parse every train/ script                    (8 files)
#   - Optional: `--with-model` runs the gated full-model test
#               (requires QUILL_TEST_MODEL=path/to/.gguf set).
#
# Usage:
#     ./scripts/test.sh
#     ./scripts/test.sh --with-model
#     QUILL_TEST_MODEL=~/quill/train/checkpoints/quill-q4_k_m.gguf \
#         ./scripts/test.sh --with-model

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_MODEL=0
for arg in "$@"; do
  case "$arg" in
    --with-model) WITH_MODEL=1 ;;
    -h|--help)
      sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg"; exit 2 ;;
  esac
done

bold() { printf "\033[1m%s\033[0m\n" "$*"; }
ok()   { printf "  \033[32m✓\033[0m %s\n" "$*"; }
fail() { printf "  \033[31m✗\033[0m %s\n" "$*"; exit 1; }

bold "== rust =="
( cd "$REPO/shell/src-tauri" && cargo test --features llm,overlay --lib 2>&1 ) \
  | tail -5
ok "shell/src-tauri tests"

if [[ "$WITH_MODEL" -eq 1 ]]; then
  bold "== rust (ignored / requires QUILL_TEST_MODEL) =="
  if [[ -n "${QUILL_TEST_MODEL:-}" ]]; then
    ( cd "$REPO/shell/src-tauri" && cargo test --features llm,overlay --lib -- --ignored 2>&1 ) \
      | tail -5
    ok "full model+adapter load"
  else
    echo "  QUILL_TEST_MODEL not set — skipping (export QUILL_TEST_MODEL=… to enable)"
  fi
fi

bold "== python =="
PY=$(command -v python3)
n=0
for f in $(find "$REPO/train" -maxdepth 2 -name '*.py' | sort); do
  if "$PY" -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" "$f" 2>/dev/null; then
    n=$((n + 1))
  else
    fail "syntax error in $f"
  fi
done
ok "$n python files parsed cleanly"

bold "== shell scripts =="
for f in "$REPO"/scripts/*.sh; do
  bash -n "$f" || fail "syntax error in $f"
done
ok "$(ls "$REPO"/scripts/*.sh | wc -l | tr -d ' ') shell scripts parsed cleanly"

bold "== overlay frontend =="
if [[ -f "$REPO/shell/src/overlay.js" ]] && command -v node >/dev/null 2>&1; then
  node --check "$REPO/shell/src/overlay.js" 2>/dev/null && ok "overlay.js parses"
  node --check "$REPO/shell/src/main.js"    2>/dev/null && ok "main.js parses"
fi

bold "== all green =="
