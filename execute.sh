#!/bin/bash
# Build runner-manager, then run it with ~/Projects as the tree root.
set -euo pipefail

# Absolute path of this script's directory (the project root), so the script
# works no matter where it is invoked from.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Build (release if `--release`/`-r` is passed, otherwise debug).
PROFILE="debug"
CARGO_ARGS=()
if [[ "${1:-}" == "--release" || "${1:-}" == "-r" ]]; then
  PROFILE="release"
  CARGO_ARGS+=(--release)
fi
# Note: `${CARGO_ARGS[@]+...}` guards against macOS's bash 3.2, where expanding
# an empty array under `set -u` errors with "unbound variable".
cargo build --manifest-path "$SCRIPT_DIR/Cargo.toml" ${CARGO_ARGS[@]+"${CARGO_ARGS[@]}"}

# Resolve and verify the full path of the built binary.
BIN="$SCRIPT_DIR/target/$PROFILE/runner-manager"
if [[ ! -x "$BIN" ]]; then
  echo "error: built binary not found at $BIN" >&2
  exit 1
fi

# Run it from ~/Projects so that directory is the tree root.
cd ~/Projects
exec "$BIN"
