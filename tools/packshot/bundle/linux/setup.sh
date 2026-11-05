#!/usr/bin/env bash
# Unpacks Blender, puts portable/ into it and makes packshot, StarBreaker and the scripts executable. Idempotent.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

tarball=$(find . -maxdepth 1 -name 'blender-*-linux-x64.tar.xz' | head -n 1)
if [ -z "$tarball" ]; then
  echo "ERROR: no blender-*-linux-x64.tar.xz in the folder" >&2
  exit 1
fi
if [ ! -d portable ]; then
  echo "ERROR: portable/ is missing" >&2
  exit 1
fi

tmp=$(mktemp -d ./blender-extract.XXXXXX)
trap 'rm -rf "$tmp"' EXIT
echo "Unpacking Blender ($tarball) ..."
tar -xf "$tarball" -C "$tmp" --strip-components=1
rm -rf ./blender
mv "$tmp" ./blender
trap - EXIT

rm -rf ./blender/portable
cp -r ./portable ./blender/portable
chmod +x packshot run.sh check.sh starbreaker/starbreaker

echo "Done. Next: bash check.sh, then packshot.json and ./run.sh --render --export (what this node can do)"
