#!/usr/bin/env bash
# kread builder trampoline -> forwards to unified Python engine
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if command -v uv &>/dev/null; then
    exec uv run python3 "$SCRIPT_DIR/build.py" "$@"
else
    exec python3 "$SCRIPT_DIR/build.py" "$@"
fi
