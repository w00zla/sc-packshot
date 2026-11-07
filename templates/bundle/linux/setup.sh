#!/usr/bin/env bash
# Sets up Blender for packshot: the official archive named in blender.pin (taken from this folder if it is here, else
# downloaded from blender.org), checked against the SHA256 in blender.pin, unpacked into ./blender, with portable/
# (Blender's config, nothing in the user profile) and the StarBreaker add-on from ./addon enabled in it; makes
# packshot, StarBreaker and the scripts executable. Run it again after a new bundle.
# Needs: curl (unless the archive is here), sha256sum, tar with xz.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

pin() {
  sed -n "s/^$1=//p" blender.pin | tr -d '\r'
}
[[ -f blender.pin ]] || { echo "ERROR: blender.pin is missing" >&2; exit 1; }
[[ -f addon/starbreaker_addon/__init__.py ]] || { echo "ERROR: addon/starbreaker_addon is missing" >&2; exit 1; }
version=$(pin version)
expected=$(pin sha256)
[[ -n $version && -n $expected ]] || { echo "ERROR: blender.pin needs version and sha256" >&2; exit 1; }
tarball=blender-$version-linux-x64.tar.xz

# 1. The archive: here, or from blender.org
if [[ ! -f $tarball ]]; then
  minor=$(cut -d. -f1-2 <<<"$version")
  echo "Downloading $tarball from blender.org ..."
  if ! curl -fL --retry 3 -o "$tarball.part" "https://download.blender.org/release/Blender$minor/$tarball"; then
    rm -f "$tarball.part"
    echo "ERROR: download failed: put $tarball next to setup.sh and run it again" >&2
    exit 1
  fi
  mv -f "$tarball.part" "$tarball"
fi
actual=$(sha256sum "$tarball" | cut -d' ' -f1)
if [[ $actual != "$expected" ]]; then
  echo "ERROR: SHA256 of $tarball is $actual, blender.pin says $expected: delete it and run setup.sh again" >&2
  exit 1
fi
echo "SHA256 ok ($tarball)"

# 2. Unpacked into ./blender
tmp=$(mktemp -d ./blender-extract.XXXXXX)
trap 'rm -rf "$tmp"' EXIT
echo "Unpacking Blender $version ..."
tar -xf "$tarball" -C "$tmp" --strip-components=1
rm -rf ./blender
mv "$tmp" ./blender
trap - EXIT

# 3. portable/ with the add-on, enabled in its userpref.blend
mkdir -p blender/portable/config blender/portable/scripts/addons
cp -r addon/starbreaker_addon blender/portable/scripts/addons/
find blender/portable/scripts/addons -name __pycache__ -type d -prune -exec rm -rf {} +
chmod +x packshot run.sh check.sh starbreaker/starbreaker
check='import bpy, addon_utils; addon_utils.enable("starbreaker_addon", default_set=True, persistent=True); bpy.ops.wm.save_userpref(); print("ADDON", "starbreaker_addon" in bpy.context.preferences.addons)'
if ! ./blender/blender -b --python-expr "$check" 2>&1 | grep -q 'ADDON True'; then
  echo "ERROR: Blender did not enable the add-on (bash check.sh shows why)" >&2
  exit 1
fi

echo "Done. The archive $tarball can go. Next: bash check.sh, then packshot.json and ./run.sh --render --export"
