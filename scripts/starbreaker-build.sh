#!/usr/bin/env bash
# Builds our StarBreaker fork at the commit pinned in deps.toml (or --commit) into local/starbreaker-ps/, where packshot
# and the bundle scripts take it from:
#   starbreaker              Linux, glibc build against glibc 2.31 (cargo-zigbuild), runs on every Linux from glibc 2.31 on
#   starbreaker.exe          Windows, cross-built with mingw (x86_64-pc-windows-gnu)
#   <binary>.commit          the commit each binary is built from (the bundles copy it as their COMMIT)
#   COMMIT                   the commit of this OS's binary (its first word is what packshot compares)
# Builds exactly that commit (git archive), whatever the fork's working tree holds.
#
#   scripts/starbreaker-build.sh [--platform all|windows|linux] [--commit <rev>] [--fork <dir>]
#
# --commit HEAD builds the fork's current commit, to try a change before pinning it.
# Needs: cargo-zigbuild and zig (Linux target), the rustup target x86_64-pc-windows-gnu and mingw64-gcc (Windows).
# scripts/starbreaker-build.ps1 does the same on Windows.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=lib/deps.sh
. scripts/lib/deps.sh

fork=""
commit=""
platform=all
while [[ $# -gt 0 ]]; do
  case $1 in
    --fork) fork=$2; shift 2 ;;
    --commit) commit=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    *) echo "Usage: $0 [--platform all|windows|linux] [--commit <rev>] [--fork <dir>]" >&2; exit 1 ;;
  esac
done
case $platform in
  all | windows | linux) ;;
  *) echo "ERROR: --platform all, windows or linux" >&2; exit 1 ;;
esac
[[ -n $fork ]] || fork=$(fork_dir)
[[ -n $commit ]] || commit=$(pin starbreaker commit)
if [[ ! -d $fork/.git ]]; then
  echo "ERROR: no StarBreaker checkout at $fork (scripts/setup.sh clones it)" >&2
  exit 1
fi

out=local/starbreaker-ps
work=local/starbreaker-build
fork=$(cd "$fork" && pwd)
# the commit, fetched if the checkout does not have it yet
if ! git -C "$fork" rev-parse --verify --quiet "$commit^{commit}" >/dev/null; then
  git -C "$fork" fetch --quiet origin
fi
full=$(git -C "$fork" rev-parse --verify "$commit^{commit}")
short=${full:0:8}
subject=$(git -C "$fork" log -1 --format=%s "$full")
echo "StarBreaker fork $fork at $short: $subject"

# the committed tree, fresh; the target folder stays for incremental builds
rm -rf "$work/src"
mkdir -p "$work/src" "$out"
git -C "$fork" archive "$full" | tar -x -C "$work/src"
target=$(pwd)/$work/target
stamp="$short starbreaker-ps ($subject)"

if [[ $platform == all || $platform == linux ]]; then
  echo "Building StarBreaker (Linux, glibc 2.31) ..."
  (cd "$work/src" && cargo zigbuild --release -p starbreaker --locked --target x86_64-unknown-linux-gnu.2.31 --target-dir "$target")
  cp -f "$target/x86_64-unknown-linux-gnu/release/starbreaker" "$out/starbreaker"
  echo "$stamp" >"$out/starbreaker.commit"
  if command -v objdump >/dev/null 2>&1; then
    echo "  needs $(objdump -T "$out/starbreaker" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1)"
  fi
fi
if [[ $platform == all || $platform == windows ]]; then
  echo "Building StarBreaker (Windows, mingw) ..."
  (cd "$work/src" && cargo build --release -p starbreaker --locked --target x86_64-pc-windows-gnu --target-dir "$target")
  cp -f "$target/x86_64-pc-windows-gnu/release/starbreaker.exe" "$out/starbreaker.exe"
  echo "$stamp" >"$out/starbreaker.exe.commit"
fi

# COMMIT: this OS's binary (what packshot in this checkout runs)
if [[ -f $out/starbreaker.commit ]]; then
  cp -f "$out/starbreaker.commit" "$out/COMMIT"
fi
echo "Done: $out ($short; $platform)"
if [[ $full != "$(pin starbreaker commit)" ]]; then
  echo "Note: $short is not the commit deps.toml pins; the bundle scripts refuse it until deps.toml says so"
fi
