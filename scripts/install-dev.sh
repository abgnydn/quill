#!/usr/bin/env bash
# Install + relaunch dance for the Quill .app during local development.
#
#   Why this exists: each rebuild needs to (a) kill any running Quill, (b)
#   replace ~/Applications/Nib.app, (c) re-codesign with the stable
#   `app.nib` identifier so the TCC Accessibility grant survives, and
#   (d) launch a fresh process with stderr piped to /tmp/nib.log so the
#   focus-tracker + arbiter logs are tailable.
#
#   Usage:
#       ./scripts/install-dev.sh                # uses the existing build
#       ./scripts/install-dev.sh --build        # full release rebuild (~15 min)
#       ./scripts/install-dev.sh --fast         # fast dev-release rebuild (~3 min)
#       ./scripts/install-dev.sh --build --tail # rebuild then tail the log
#
#   Codesigning note: we ad-hoc sign with `-` so the binary's identifier
#   stays `app.nib` across rebuilds (Tauri's default per-build random
#   identifier invalidates TCC grants).
#
#   --fast vs --build:
#     --fast  uses [profile.release-dev]: opt-level=1, codegen-units=16,
#             no LTO, no strip, no DMG bundling. Use during iteration.
#     --build uses [profile.release]:     opt-level=z, codegen-units=1,
#             full LTO, stripped, DMG generated. Use for ship builds.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Nib.app"
BUILT_APP="$REPO_ROOT/shell/src-tauri/target/release/bundle/macos/$APP_NAME"
INSTALL_DIR="$HOME/Applications"
INSTALL_PATH="$INSTALL_DIR/$APP_NAME"
LOG="/tmp/nib.log"
QVAC_CACHE="$HOME/.cache/qvac/qvac-fabric-llm.cpp"
QVAC_RESOURCES="$REPO_ROOT/shell/src-tauri/resources/qvac"

# Build QVAC (BitNet + on-device LoRA training engine) once, cache the
# binaries under ~/.cache/qvac/, then copy them into the Tauri resources
# directory so they get bundled inside Nib.app/Contents/Resources/qvac/.
prepare_qvac() {
  mkdir -p "$QVAC_RESOURCES"
  local need_build=0
  for bin in llama-cli llama-finetune-lora; do
    if [[ ! -x "$QVAC_CACHE/build/bin/$bin" ]]; then
      need_build=1
    fi
  done
  if [[ "$need_build" -eq 1 ]]; then
    echo "[quill] QVAC binaries missing — cloning + building (one-time, ~5 min)…"
    if [[ ! -d "$QVAC_CACHE" ]]; then
      mkdir -p "$(dirname "$QVAC_CACHE")"
      git clone --depth 1 https://github.com/tetherto/qvac-fabric-llm.cpp \
        "$QVAC_CACHE"
    fi
    (
      cd "$QVAC_CACHE"
      cmake -B build -DGGML_METAL=ON -DLLAMA_CURL=OFF -DGGML_NATIVE=ON \
        -DBUILD_SHARED_LIBS=ON >/dev/null
      cmake --build build --config Release \
        --target llama-cli llama-finetune-lora -j "$(sysctl -n hw.ncpu)"
    )
  else
    echo "[quill] QVAC cache hit at $QVAC_CACHE"
  fi
  echo "[quill] staging QVAC binaries → $QVAC_RESOURCES"
  # Collect candidates without aborting on no-match glob expansion.
  shopt -s nullglob
  local files=(
    "$QVAC_CACHE/build/bin/llama-cli"
    "$QVAC_CACHE/build/bin/llama-finetune-lora"
    "$QVAC_CACHE/build/bin"/*.dylib
    "$QVAC_CACHE/build/bin"/*.metallib
  )
  shopt -u nullglob
  for f in "${files[@]}"; do
    [[ -e "$f" ]] || continue
    # Skip symlinked versions — only copy concrete files.
    [[ -L "$f" ]] && continue
    cp -f "$f" "$QVAC_RESOURCES/"
  done
  echo "[quill] QVAC staged: $(ls "$QVAC_RESOURCES" | wc -l | tr -d ' ') files, \
$(du -sh "$QVAC_RESOURCES" | cut -f1)"
}

BUILD=0
FAST=0
TAIL=0
for arg in "$@"; do
  case "$arg" in
    --build) BUILD=1 ;;
    --fast)  FAST=1 ;;
    --tail)  TAIL=1 ;;
    -h|--help)
      sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg"; exit 2 ;;
  esac
done

if [[ "$BUILD" -eq 1 && "$FAST" -eq 1 ]]; then
  echo "[quill] --build and --fast are mutually exclusive" >&2
  exit 2
fi

if [[ "$BUILD" -eq 1 ]]; then
  prepare_qvac
  echo "[quill] building release with llm + overlay features (full opt, ~15 min)…"
  ( cd "$REPO_ROOT/shell/src-tauri" && cargo tauri build --features llm,overlay )
fi

if [[ "$FAST" -eq 1 ]]; then
  prepare_qvac
  echo "[quill] FAST build (profile=release-dev, no DMG, ~3 min)…"
  ( cd "$REPO_ROOT/shell/src-tauri" && \
    cargo tauri build --features llm,overlay --bundles app -- --profile release-dev )
  # `--profile release-dev` makes cargo put the binary at target/release-dev/
  # but Tauri's bundler looks at target/release/ for the .app it produces.
  # Tauri 2 actually emits to target/<profile>/bundle/macos/. Adjust path.
  BUILT_APP="$REPO_ROOT/shell/src-tauri/target/release-dev/bundle/macos/$APP_NAME"
fi

if [[ ! -d "$BUILT_APP" ]]; then
  echo "[quill] no built app at $BUILT_APP — run with --build" >&2
  exit 1
fi

echo "[quill] killing any running Quill processes…"
pkill -9 -f "$APP_NAME/Contents/MacOS/quill" 2>/dev/null || true
sleep 2

mkdir -p "$INSTALL_DIR"
echo "[quill] replacing $INSTALL_PATH"
rm -rf "$INSTALL_PATH"
cp -R "$BUILT_APP" "$INSTALL_PATH"
xattr -dr com.apple.quarantine "$INSTALL_PATH" 2>/dev/null || true

echo "[quill] ad-hoc codesign with stable identifier…"
codesign --force --deep --sign - "$INSTALL_PATH" 2>&1 | tail -1
codesign --display --verbose=2 "$INSTALL_PATH" 2>&1 | grep -E "Identifier|Signature" || true

rm -f "$LOG"
echo "[quill] launching with stderr → $LOG"
"$INSTALL_PATH/Contents/MacOS/quill" > "$LOG" 2>&1 &
QUILL_PID=$!
sleep 4
echo "[quill] pid=$QUILL_PID  log=$LOG"

if [[ "$TAIL" -eq 1 ]]; then
  echo "[quill] tailing $LOG (Ctrl-C to stop)…"
  exec tail -f "$LOG" | grep --line-buffered -E "\[quill\]|focus-update|cursor|overlay-js|apply"
fi
