#!/usr/bin/env bash
# Packs everything packshot (tools/packshot) needs on another node, one folder per platform, built on Linux:
#   <out>/windows: packshot.exe (cross-built with mingw), starbreaker/ (starbreaker.exe and COMMIT from
#                  local/starbreaker-qm/), the official Blender zip (SHA256 checked, left packed: setup.cmd unpacks
#                  it on the node), portable/ (config and add-on), cli/render.py, targets.json, setup.cmd, run.cmd,
#                  README.txt, packshot.example.json (the Windows texts with CRLF)
#   <out>/linux:   packshot (static musl binary), starbreaker/ (the Linux build and COMMIT), the official Blender
#                  tarball (SHA256 checked, left packed: setup.sh unpacks it on the node), portable/ (config and
#                  add-on), cli/render.py, targets.json, setup.sh, check.sh, run.sh, README.txt, packshot.example.json
# portable/ comes from the Linux Blender for both, the add-on from the fork as real files (the Windows Blender's
# junction cannot be read on Linux).
#
#   scripts/packshot-bundle.sh [--platform all|windows|linux] [--out <dir>] [--blender-version 5.2.3]
#
# Needs: rustup targets x86_64-unknown-linux-musl and x86_64-pc-windows-gnu, mingw64-gcc (the Windows linker),
# local/blender/blender-<ver>-linux-x64 (its portable/), the StarBreaker builds in local/starbreaker-qm/, the DB for
# pnpm packshot:targets (DATABASE_URL from .env), curl, sha256sum and rsync.
set -euo pipefail

out=local/packshot-bundle
blender_version=5.2.3
platform=all
while [[ $# -gt 0 ]]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --blender-version) blender_version=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    *) echo "Usage: $0 [--platform all|windows|linux] [--out <dir>] [--blender-version <x.y.z>]" >&2; exit 1 ;;
  esac
done
case $platform in
  all) do_win=1; do_lin=1 ;;
  windows) do_win=1; do_lin=0 ;;
  linux) do_win=0; do_lin=1 ;;
  *) echo "ERROR: --platform all, windows or linux" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
tpl=tools/packshot/bundle
cache=$out/cache
mkdir -p "$cache"

# The add-on as real files: the Linux Blender's portable/ links into the fork
blender_lin=local/blender/blender-$blender_version-linux-x64
addon=$blender_lin/portable/scripts/addons/starbreaker_addon
if ! compgen -G "$addon/*.py" >/dev/null; then
  echo "ERROR: no add-on in $addon (the symlink into ../starbreaker-qm/blender_addon)" >&2
  exit 1
fi

# The target list once for both
echo "Target list from the DB ..."
pnpm packshot:targets --out "$cache/targets.json"

# Shared texts: README per platform, the example config
texts() {
  local dir=$1 os=$2
  mkdir -p "$dir/cli"
  cp src/icons/render.py "$dir/cli/"
  cp "$cache/targets.json" "$dir/"
  if [[ $os == windows ]]; then
    crlf "$tpl/windows/README.txt" "$dir/README.txt"
    crlf "$tpl/packshot.example.json" "$dir/packshot.example.json"
  else
    cp "$tpl/$os/README.txt" "$tpl/packshot.example.json" "$dir/"
  fi
}

# Template with CRLF line endings for the Windows bundle (the working tree is LF)
crlf() {
  sed 's/\r$//; s/$/\r/' "$1" >"$2"
}

# Blender archive (blender-<ver>-<platform>): downloaded once into cache/, checked against the published SHA256
fetch_blender() {
  local name=$1 minor base expected actual
  minor=$(echo "$blender_version" | cut -d. -f1-2)
  base=https://download.blender.org/release/Blender$minor
  if [[ ! -f $cache/$name ]]; then
    echo "Downloading $name ..."
    curl -fL -o "$cache/$name.part" "$base/$name"
    mv "$cache/$name.part" "$cache/$name"
  fi
  expected=$(curl -fsSL "$base/blender-$blender_version.sha256" | grep -F "$name" | head -n 1 | cut -d' ' -f1)
  if [[ -z $expected ]]; then
    echo "ERROR: no SHA256 for $name in $base/blender-$blender_version.sha256" >&2
    exit 1
  fi
  actual=$(sha256sum "$cache/$name" | cut -d' ' -f1)
  if [[ $actual != "$expected" ]]; then
    rm -f "$cache/$name"
    echo "ERROR: SHA256 mismatch ($actual, expected $expected): download deleted" >&2
    exit 1
  fi
  echo "SHA256 ok ($name)"
}

summary() {
  echo "Bundle $(basename "$1"): $1 ($(du -sh "$1" | cut -f1), $(find "$1" -type f | wc -l) files)"
}

if [[ $do_win == 1 ]]; then
  win=$out/windows
  if [[ ! -d $blender_lin/portable || ! -f local/starbreaker-qm/starbreaker.exe ]]; then
    echo "ERROR: $blender_lin/portable or local/starbreaker-qm/starbreaker.exe is missing" >&2
    exit 1
  fi
  echo "Building packshot (Windows, mingw) ..."
  (cd tools/packshot && cargo build --release --target x86_64-pc-windows-gnu)
  mkdir -p "$win/starbreaker"
  cp tools/packshot/target/x86_64-pc-windows-gnu/release/packshot.exe "$win/"
  cp local/starbreaker-qm/starbreaker.exe local/starbreaker-qm/COMMIT "$win/starbreaker/"
  # Blender zip, packed (setup.cmd unpacks it); the unpacked blender/ of older bundles goes
  zip_name=blender-$blender_version-windows-x64.zip
  fetch_blender "$zip_name"
  rm -rf "$win/blender"
  rm -f "$win"/blender-*-windows-x64.zip
  cp "$cache/$zip_name" "$win/"
  # portable/: real files instead of the add-on symlink, without __pycache__
  rm -rf "$win/portable"
  rsync -aL --exclude __pycache__ "$blender_lin/portable/" "$win/portable/"
  texts "$win" windows
  crlf "$tpl/windows/setup.cmd" "$win/setup.cmd"
  crlf "$tpl/windows/run.cmd" "$win/run.cmd"
  rm -f "$win/LIESMICH.txt"
  summary "$win"
fi

if [[ $do_lin == 1 ]]; then
  lin=$out/linux
  sb=local/starbreaker-qm/starbreaker
  if [[ ! -d $blender_lin/portable || ! -x $sb ]]; then
    echo "ERROR: $blender_lin/portable or $sb (Linux build of the fork) is missing" >&2
    exit 1
  fi
  # 1. packshot: static, runs on any x86-64 Linux
  echo "Building packshot (Linux, musl) ..."
  (cd tools/packshot && cargo build --release --target x86_64-unknown-linux-musl)
  mkdir -p "$lin/starbreaker"
  cp tools/packshot/target/x86_64-unknown-linux-musl/release/packshot "$lin/"
  # 2. StarBreaker: dynamically linked against glibc, so the node needs at least the glibc it was built with
  cp "$sb" local/starbreaker-qm/COMMIT "$lin/starbreaker/"
  if command -v objdump >/dev/null 2>&1; then
    glibc=$(objdump -T "$sb" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1 || true)
    echo "StarBreaker needs ${glibc:-no glibc (static)} on the node (ldd --version shows its glibc)"
  fi
  # 3. Blender tarball, packed (setup.sh unpacks it)
  tar_name=blender-$blender_version-linux-x64.tar.xz
  fetch_blender "$tar_name"
  rm -f "$lin"/blender-*-linux-x64.tar.xz
  cp "$cache/$tar_name" "$lin/"
  # 4. portable/: real files instead of the add-on symlink, without __pycache__
  rm -rf "$lin/portable"
  rsync -aL --exclude __pycache__ "$blender_lin/portable/" "$lin/portable/"
  # 5. render.py, target list, scripts and texts
  texts "$lin" linux
  cp "$tpl/linux/setup.sh" "$tpl/linux/check.sh" "$tpl/linux/run.sh" "$lin/"
  chmod +x "$lin/packshot" "$lin/starbreaker/starbreaker" "$lin/setup.sh" "$lin/check.sh" "$lin/run.sh"
  rm -f "$lin/run-node.sh" "$lin/LIESMICH.txt"
  summary "$lin"
fi
