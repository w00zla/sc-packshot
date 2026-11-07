#!/usr/bin/env bash
# Packs everything packshot (this repository) needs on another node, one folder per platform, built on Linux:
#   <out>/windows: packshot.exe (cross-built with mingw), starbreaker/ (starbreaker.exe and COMMIT from
#                  local/starbreaker-ps/), the official Blender zip (SHA256 checked, left packed: setup.cmd unpacks
#                  it on the node), portable/ (config and add-on), cli/render.py, targets.json, setup.cmd, run.cmd,
#                  README.txt, packshot.example.json (the Windows texts with CRLF)
#   <out>/linux:   packshot (static musl binary), starbreaker/ (the Linux build and COMMIT), the official Blender
#                  tarball (SHA256 checked, left packed: setup.sh unpacks it on the node), portable/ (config and
#                  add-on), cli/render.py, targets.json, setup.sh, check.sh, run.sh, README.txt, packshot.example.json
# portable/ comes from the Linux Blender for both, the add-on from the fork as real files (the Windows Blender's
# junction cannot be read on Linux).
#
#   scripts/packshot-bundle.sh [--platform all|windows|linux] [--out <dir>] [--targets <targets.json>]
#
# Blender and StarBreaker as deps.toml pins them: the Blender archive from local/cache/ (scripts/lib/deps.sh), the
# StarBreaker binaries only when their <binary>.commit stamp is the pinned commit (scripts/starbreaker-build.sh).
# targets.json (default local/packshot/targets.json) goes into both bundles when it exists, else they carry none.
# Needs: rustup targets x86_64-unknown-linux-musl and x86_64-pc-windows-gnu, mingw64-gcc (the Windows linker),
# local/blender/blender-<ver>-linux-x64 (its portable/, from scripts/setup.sh), the StarBreaker builds in
# local/starbreaker-ps/, curl, sha256sum and rsync.
set -euo pipefail

out=local/packshot-bundle
platform=all
targets=local/packshot/targets.json
while [[ $# -gt 0 ]]; do
  case $1 in
    --out) out=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    --targets) targets=$2; shift 2 ;;
    *) echo "Usage: $0 [--platform all|windows|linux] [--out <dir>] [--targets <file>]" >&2; exit 1 ;;
  esac
done
case $platform in
  all) do_win=1; do_lin=1 ;;
  windows) do_win=1; do_lin=0 ;;
  linux) do_win=0; do_lin=1 ;;
  *) echo "ERROR: --platform all, windows or linux" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
# shellcheck source=lib/deps.sh
. scripts/lib/deps.sh
tpl=bundle
blender_version=$(pin blender version)
pinned=$(pinned_commit)
mkdir -p "$out"

# A StarBreaker binary with its stamp, refused unless built from the pinned commit
starbreaker_for() {
  local bin=$1 stamp
  if [[ ! -f local/starbreaker-ps/$bin || ! -f local/starbreaker-ps/$bin.commit ]]; then
    echo "ERROR: local/starbreaker-ps/$bin or its .commit stamp is missing (scripts/starbreaker-build.sh)" >&2
    exit 1
  fi
  stamp=$(cut -d" " -f1 "local/starbreaker-ps/$bin.commit")
  if [[ $stamp != "$pinned" ]]; then
    echo "ERROR: local/starbreaker-ps/$bin is $stamp, deps.toml pins $pinned (scripts/starbreaker-build.sh)" >&2
    exit 1
  fi
}

# The add-on as real files: the Linux Blender's portable/ links into the fork
blender_lin=local/blender/blender-$blender_version-linux-x64
addon=$blender_lin/portable/scripts/addons/starbreaker_addon
if ! compgen -G "$addon/*.py" >/dev/null; then
  echo "ERROR: no add-on in $addon (the symlink into ../starbreaker-ps/blender_addon)" >&2
  exit 1
fi

# The target list, if there is one
if [[ -f $targets ]]; then
  echo "Target list: $targets"
else
  echo "Note: no target list at $targets, the bundles carry none" >&2
  targets=
fi

# Shared texts: README per platform, the example config
texts() {
  local dir=$1 os=$2
  mkdir -p "$dir/cli"
  cp blender/render.py "$dir/cli/"
  if [[ -n $targets ]]; then
    cp "$targets" "$dir/targets.json"
  fi
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

summary() {
  echo "Bundle $(basename "$1"): $1 ($(du -sh "$1" | cut -f1), $(find "$1" -type f | wc -l) files)"
}

if [[ $do_win == 1 ]]; then
  win=$out/windows
  [[ -d $blender_lin/portable ]] || { echo "ERROR: $blender_lin/portable is missing (scripts/setup.sh)" >&2; exit 1; }
  starbreaker_for starbreaker.exe
  echo "Building packshot (Windows, mingw) ..."
  (cargo build --release --target x86_64-pc-windows-gnu)
  mkdir -p "$win/starbreaker"
  cp target/x86_64-pc-windows-gnu/release/packshot.exe "$win/"
  cp local/starbreaker-ps/starbreaker.exe "$win/starbreaker/"
  cp local/starbreaker-ps/starbreaker.exe.commit "$win/starbreaker/COMMIT"
  # Blender zip, packed (setup.cmd unpacks it); the unpacked blender/ of older bundles goes
  zip=$(blender_archive windows-x64)
  rm -rf "$win/blender"
  rm -f "$win"/blender-*-windows-x64.zip
  cp "$zip" "$win/"
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
  sb=local/starbreaker-ps/starbreaker
  [[ -d $blender_lin/portable ]] || { echo "ERROR: $blender_lin/portable is missing (scripts/setup.sh)" >&2; exit 1; }
  starbreaker_for starbreaker
  # 1. packshot: static, runs on any x86-64 Linux
  echo "Building packshot (Linux, musl) ..."
  (cargo build --release --target x86_64-unknown-linux-musl)
  mkdir -p "$lin/starbreaker"
  cp target/x86_64-unknown-linux-musl/release/packshot "$lin/"
  # 2. StarBreaker: dynamically linked against glibc, so the node needs at least the glibc it was built with
  cp "$sb" "$lin/starbreaker/"
  cp local/starbreaker-ps/starbreaker.commit "$lin/starbreaker/COMMIT"
  if command -v objdump >/dev/null 2>&1; then
    glibc=$(objdump -T "$sb" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1 || true)
    echo "StarBreaker needs ${glibc:-no glibc (static)} on the node (ldd --version shows its glibc)"
  fi
  # 3. Blender tarball, packed (setup.sh unpacks it)
  tarball=$(blender_archive linux-x64)
  rm -f "$lin"/blender-*-linux-x64.tar.xz
  cp "$tarball" "$lin/"
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
