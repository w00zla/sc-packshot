#!/usr/bin/env bash
# Checks Blender with the StarBreaker addon and an EEVEE render on Vulkan.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"

failed=0
png=$(mktemp --suffix=.png)
trap 'rm -f "$png"' EXIT

if command -v nvidia-smi >/dev/null 2>&1; then
  nvidia-smi --query-gpu=name,driver_version --format=csv,noheader || true
fi

# 1: Addon
out=$(./blender/blender -b --python-expr "import addon_utils; print('ADDON', addon_utils.check('starbreaker_addon'))" 2>&1 || true)
if printf '%s\n' "$out" | grep -E '^ADDON ' | grep -q 'True)\?$'; then
  echo "OK     Addon starbreaker_addon"
else
  echo "ERROR  Addon starbreaker_addon"
  printf '%s\n' "$out" | tail -n 15
  failed=1
fi

# 2: EEVEE on Vulkan, headless
rm -f "$png"
expr="import bpy; s=bpy.context.scene; s.render.engine='BLENDER_EEVEE'; s.render.resolution_x=64; s.render.resolution_y=64; s.render.filepath=r'$png'; bpy.ops.render.render(write_still=True)"
out=$(./blender/blender -b --factory-startup --gpu-backend vulkan --python-expr "$expr" 2>&1 || true)
if [ -s "$png" ]; then
  echo "OK     EEVEE render (Vulkan)"
else
  echo "ERROR  EEVEE render (Vulkan)"
  printf '%s\n' "$out" | tail -n 15
  failed=1
fi

exit "$failed"
