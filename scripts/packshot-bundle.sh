#!/usr/bin/env bash
# Packs everything packshot (this repository) needs on another node, one folder per platform, built on Linux:
#   <out>/windows: packshot.exe (cross-built with mingw), starbreaker/ (starbreaker.exe and COMMIT),
#                  addon/starbreaker_addon, cli/render.py, blender.pin, targets/targets.json, setup.cmd, check.cmd, run.cmd, README.txt,
#                  packshot.example.json (the Windows texts with CRLF)
#   <out>/linux:   packshot (static musl binary), starbreaker/ (the Linux binary and COMMIT), addon/starbreaker_addon,
#                  cli/render.py, blender.pin, targets/targets.json, setup.sh, check.sh, run.sh, README.txt,
#                  packshot.example.json
# No Blender in the bundles: setup.cmd / setup.sh take the official archive named in blender.pin (next to them, else
# from blender.org), check its SHA256 and put portable/ with the add-on into it.
#
#   scripts/packshot-bundle.sh [--platform all|windows|linux] [--out <dir>] [--targets <targets.json>]
#
# Blender and StarBreaker as deps.toml pins them: the StarBreaker binaries only when their <binary>.commit stamp is
# the pinned commit (a missing one comes from the pinned release: scripts/lib/deps.sh install_starbreaker), the
# add-on from the pinned release. targets.json (default local/packshot/targets/targets.json) goes into both bundles as
# targets/targets.json when it exists, else they carry none.
# Needs: rustup targets x86_64-unknown-linux-musl and x86_64-pc-windows-gnu, mingw64-gcc (the Windows linker), curl,
# sha256sum, unzip and rsync.
set -euo pipefail

out=local/packshot-bundle
platform=all
targets=local/packshot/targets/targets.json
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
tpl=templates/bundle
blender_version=$(pin blender version)
mkdir -p "$out"

# A StarBreaker binary with its stamp at the pinned commit: a missing one comes from the pinned release, a build of
# another commit is refused
starbreaker_for() {
  local platform=linux
  if [[ $1 == *.exe ]]; then platform=windows; fi
  install_starbreaker "$platform" || exit 1
}

# The pinned release's add-on as real files and the Blender pin for one platform (windows-x64 | linux-x64); what
# older bundles carried instead (the Blender archive, portable/) goes
addon=$(release_addon)
addon_and_pin() {
  local dir=$1 platform=$2
  rm -rf "$dir/addon" "$dir/portable" "$dir"/blender-*.zip "$dir"/blender-*.tar.xz
  mkdir -p "$dir/addon"
  rsync -a --exclude __pycache__ "$addon/" "$dir/addon/starbreaker_addon/"
  printf 'version=%s\nsha256=%s\n' "$blender_version" "$(pin blender "sha256-$platform")" >"$dir/blender.pin"
}

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
    mkdir -p "$dir/targets"
    cp "$targets" "$dir/targets/targets.json"
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
  starbreaker_for starbreaker.exe
  echo "Building packshot (Windows, mingw) ..."
  (cargo build --release --target x86_64-pc-windows-gnu)
  mkdir -p "$win/starbreaker"
  cp target/x86_64-pc-windows-gnu/release/packshot.exe "$win/"
  cp local/starbreaker-ps/starbreaker.exe "$win/starbreaker/"
  cp local/starbreaker-ps/starbreaker.exe.commit "$win/starbreaker/COMMIT"
  addon_and_pin "$win" windows-x64
  sed -i 's/$/\r/' "$win/blender.pin"
  texts "$win" windows
  crlf "$tpl/windows/setup.cmd" "$win/setup.cmd"
  crlf "$tpl/windows/check.cmd" "$win/check.cmd"
  crlf "$tpl/windows/run.cmd" "$win/run.cmd"
  rm -f "$win/LIESMICH.txt"
  summary "$win"
fi

if [[ $do_lin == 1 ]]; then
  lin=$out/linux
  sb=local/starbreaker-ps/starbreaker
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
  # 3. the add-on and the Blender pin (setup.sh fetches Blender)
  addon_and_pin "$lin" linux-x64
  # 4. render.py, target list, scripts and texts
  texts "$lin" linux
  cp "$tpl/linux/setup.sh" "$tpl/linux/check.sh" "$tpl/linux/run.sh" "$lin/"
  chmod +x "$lin/packshot" "$lin/starbreaker/starbreaker" "$lin/setup.sh" "$lin/check.sh" "$lin/run.sh"
  rm -f "$lin/run-node.sh" "$lin/LIESMICH.txt"
  summary "$lin"
fi
