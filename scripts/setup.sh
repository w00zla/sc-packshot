#!/usr/bin/env bash
# Sets up a checkout for development on Linux, exactly as deps.toml pins it (run again after changing a pin; what is
# already right is left as it is):
#   1. the StarBreaker add-on of the pinned release (local/starbreaker-addon/<release>/, SHA256 checked); with
#      --from-source the fork next to the repo (deps.toml starbreaker.repo) instead, cloned if it is missing
#   2. the pinned Blender in local/blender/blender-<version>-linux-x64/, from the official archive (SHA256 checked)
#   3. its portable/ folder (Blender keeps its config there, nothing in the user profile) with the add-on linked in (a
#      relative symlink; with --from-source into the fork, so changes there apply at once) and enabled in
#      portable/config/userpref.blend
#   4. StarBreaker (Linux binary) into local/starbreaker-ps/: from the pinned release, with --from-source built at the
#      pinned commit
#
#   scripts/setup.sh [--from-source] [--skip-starbreaker]
#
# The bundles (scripts/packshot-bundle.sh) do not need this: they carry no Blender, their own setup.sh fetches it by blender.pin.
# Needs: curl, sha256sum, tar, unzip; with --from-source git and the StarBreaker build's tools.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=lib/deps.sh
. scripts/lib/deps.sh

skip_sb=0
from_source=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --from-source) from_source=1; shift ;;
    --skip-starbreaker) skip_sb=1; shift ;;
    *) echo "Usage: $0 [--from-source] [--skip-starbreaker]" >&2; exit 1 ;;
  esac
done

# 1. the add-on: the release's, or the fork's (both relative to the repo root)
if [[ $from_source == 1 ]]; then
  fork=$(fork_dir)
  if [[ ! -d $fork/.git ]]; then
    echo "Cloning the StarBreaker fork into $fork ..."
    git clone --quiet "$(pin starbreaker url)" "$fork"
  fi
  addon_src=$fork/blender_addon/starbreaker_addon
else
  addon_src=$(release_addon)
fi
if [[ ! -f $addon_src/__init__.py ]]; then
  echo "ERROR: no add-on in $addon_src" >&2
  exit 1
fi

# 2. Blender
version=$(pin blender version)
dir=local/blender/blender-$version-linux-x64
if [[ -x $dir/blender ]]; then
  echo "Blender $version is there ($dir)"
else
  archive=$(blender_archive linux-x64)
  mkdir -p local/blender
  echo "Unpacking Blender $version ..."
  tar -xf "$archive" -C local/blender
  [[ -x $dir/blender ]] || { echo "ERROR: the archive did not unpack to $dir" >&2; exit 1; }
fi

# 3. portable/ with the add-on as a relative symlink (portable/scripts/addons is 6 levels below the repo)
addons=$dir/portable/scripts/addons
mkdir -p "$addons" "$dir/portable/config"
link=$addons/starbreaker_addon
if [[ $addon_src == /* ]]; then
  rel=$addon_src
else
  rel=../../../../../../$addon_src
fi
if [[ -e $link && ! -L $link ]]; then
  echo "ERROR: $link is a real folder, not a link: move it away, then run this again" >&2
  exit 1
fi
if [[ ! -L $link || $(readlink "$link") != "$rel" ]]; then
  ln -sfn "$rel" "$link"
  echo "Add-on linked: $link -> $rel"
fi
[[ -f $link/__init__.py ]] || { echo "ERROR: $link does not resolve" >&2; exit 1; }
check='import bpy, addon_utils; addon_utils.enable("starbreaker_addon", default_set=True, persistent=True); bpy.ops.wm.save_userpref(); print("ADDON", "starbreaker_addon" in bpy.context.preferences.addons)'
if ! "$dir/blender" -b --python-expr "$check" 2>&1 | grep -q 'ADDON True'; then
  echo "ERROR: Blender did not enable the add-on" >&2
  exit 1
fi
echo "Add-on enabled in portable/config/userpref.blend"

# 4. StarBreaker at the pin: the release's binary, or built from the fork
if [[ $skip_sb == 0 ]]; then
  if [[ $from_source == 1 ]]; then
    stamp=""
    [[ -f local/starbreaker-ps/starbreaker.commit ]] && stamp=$(cut -d' ' -f1 local/starbreaker-ps/starbreaker.commit)
    if [[ $stamp == "$(pinned_commit)" ]]; then
      echo "StarBreaker $stamp is there (local/starbreaker-ps)"
    else
      scripts/starbreaker-build.sh --platform linux
    fi
  else
    install_starbreaker linux
  fi
fi
echo "Ready: cargo run --release -- --render --export"
