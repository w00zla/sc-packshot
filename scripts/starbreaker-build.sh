#!/usr/bin/env bash
# Builds our StarBreaker fork (../starbreaker-qm, github.com/w00zla/starbreaker-qm) at its HEAD for both platforms into
# local/starbreaker-qm/, where packshot and the bundle scripts take it from:
#   starbreaker      Linux, glibc build against glibc 2.31 (cargo-zigbuild), runs on every Linux from glibc 2.31 on
#   starbreaker.exe  Windows, cross-built with mingw (x86_64-pc-windows-gnu)
#   COMMIT           the commit both are built from (its first word is what packshot compares)
# Builds exactly the committed HEAD (git archive), whatever the fork's working tree holds.
#
#   scripts/starbreaker-build.sh [--platform all|windows|linux] [--fork <dir>]
#
# Needs: cargo-zigbuild and zig (Linux target), the rustup target x86_64-pc-windows-gnu and mingw64-gcc (Windows).
# scripts/starbreaker-build.ps1 does the same on Windows.
set -euo pipefail

fork=../starbreaker-qm
platform=all
while [[ $# -gt 0 ]]; do
  case $1 in
    --fork) fork=$2; shift 2 ;;
    --platform) platform=$2; shift 2 ;;
    *) echo "Usage: $0 [--platform all|windows|linux] [--fork <dir>]" >&2; exit 1 ;;
  esac
done
case $platform in
  all | windows | linux) ;;
  *) echo "ERROR: --platform all, windows or linux" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
out=local/starbreaker-qm
work=local/starbreaker-build
fork=$(cd "$fork" && pwd)
commit=$(git -C "$fork" rev-parse --short=8 HEAD)
branch=$(git -C "$fork" rev-parse --abbrev-ref HEAD)
subject=$(git -C "$fork" log -1 --format=%s)
echo "StarBreaker fork $fork at $commit ($branch): $subject"

# the committed tree, fresh; the target folder stays for incremental builds
rm -rf "$work/src"
mkdir -p "$work/src" "$out"
git -C "$fork" archive HEAD | tar -x -C "$work/src"
target=$(pwd)/$work/target

if [[ $platform == all || $platform == linux ]]; then
  echo "Building StarBreaker (Linux, glibc 2.31) ..."
  (cd "$work/src" && cargo zigbuild --release -p starbreaker --locked --target x86_64-unknown-linux-gnu.2.31 --target-dir "$target")
  cp -f "$target/x86_64-unknown-linux-gnu/release/starbreaker" "$out/starbreaker"
  if command -v objdump >/dev/null 2>&1; then
    echo "  needs $(objdump -T "$out/starbreaker" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -n 1)"
  fi
fi
if [[ $platform == all || $platform == windows ]]; then
  echo "Building StarBreaker (Windows, mingw) ..."
  (cd "$work/src" && cargo build --release -p starbreaker --locked --target x86_64-pc-windows-gnu --target-dir "$target")
  cp -f "$target/x86_64-pc-windows-gnu/release/starbreaker.exe" "$out/starbreaker.exe"
fi

# COMMIT: a hand-written description after the hash stays while the commit is the same
old=""
[[ -f $out/COMMIT ]] && old=$(cat "$out/COMMIT")
if [[ ${old%% *} != "$commit" ]]; then
  echo "$commit starbreaker-qm $branch ($subject)" >"$out/COMMIT"
fi
echo "Done: $out (COMMIT: $(cut -c1-80 "$out/COMMIT"))"
if [[ $platform != all ]]; then
  echo "Note: only $platform was built; the other binary in $out may be from another commit than COMMIT says"
fi
