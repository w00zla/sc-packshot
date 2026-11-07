#!/usr/bin/env bash
# Sets up a checkout for development on Linux, exactly as deps.toml pins it (run again after changing a pin; what is
# already right is left as it is):
#   1. the StarBreaker fork next to the repo (deps.toml starbreaker.repo), cloned if it is missing
#   2. the pinned Blender in local/blender/blender-<version>-linux-x64/, from the official archive (SHA256 checked)
#   3. its portable/ folder (Blender keeps its config there, nothing in the user profile) with the fork's add-on linked
#      in (a relative symlink: changes in the fork apply at once) and enabled in portable/config/userpref.blend
#   4. StarBreaker built at the pinned commit into local/starbreaker-ps/ (Linux binary)
#
#   scripts/setup.sh [--skip-starbreaker]
#
# The bundles (scripts/packshot-bundle.sh) take portable/ from here, so set it up before building them.
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=lib/deps.sh
. scripts/lib/deps.sh

skip_sb=0
while [[ $# -gt 0 ]]; do
  case $1 in
    --skip-starbreaker) skip_sb=1; shift ;;
    *) echo "Usage: $0 [--skip-starbreaker]" >&2; exit 1 ;;
  esac
done

# 1. the fork
fork=$(fork_dir)
if [[ ! -d $fork/.git ]]; then
  echo "Cloning the StarBreaker fork into $fork ..."
  git clone --quiet "$(pin starbreaker url)" "$fork"
fi
if [[ ! -f $fork/blender_addon/starbreaker_addon/__init__.py ]]; then
  echo "ERROR: no add-on in $fork/blender_addon/starbreaker_addon" >&2
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

# 3. portable/ with the add-on as a relative symlink into the fork (portable/scripts/addons is 6 levels below the repo)
addons=$dir/portable/scripts/addons
mkdir -p "$addons" "$dir/portable/config"
link=$addons/starbreaker_addon
if [[ $fork == /* ]]; then
  rel=$fork/blender_addon/starbreaker_addon
else
  rel=../../../../../../$fork/blender_addon/starbreaker_addon
fi
if [[ -e $link && ! -L $link ]]; then
  echo "ERROR: $link is a real folder, not a link to the fork: move it away, then run this again" >&2
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

# 4. StarBreaker at the pin
if [[ $skip_sb == 0 ]]; then
  stamp=""
  [[ -f local/starbreaker-ps/starbreaker.commit ]] && stamp=$(cut -d' ' -f1 local/starbreaker-ps/starbreaker.commit)
  if [[ $stamp == "$(pinned_commit)" ]]; then
    echo "StarBreaker $stamp is there (local/starbreaker-ps)"
  else
    scripts/starbreaker-build.sh --platform linux
  fi
fi
echo "Ready: cargo run --release -- --render --export"
