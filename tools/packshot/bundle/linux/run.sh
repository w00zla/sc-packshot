#!/usr/bin/env bash
# Starts packshot (TUI) with what this node can do now, e.g. ./run.sh --render --export (both), ./run.sh --render
# (render only) or ./run.sh (only start runs, store them, watch). Further arguments go to packshot (--workers 6,
# --serve for no TUI, --help).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

for a in "$@"; do
  if [ "$a" = --render ] && [ ! -x ./blender/blender ]; then
    echo "ERROR: --render needs Blender in ./blender: run bash setup.sh first" >&2
    exit 1
  fi
done
if [ ! -f packshot.json ]; then
  echo "Note: no packshot.json, this node works alone (cp packshot.example.json packshot.json and set the token)" >&2
fi

exec ./packshot "$@"
