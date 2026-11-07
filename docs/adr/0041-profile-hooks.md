# 0041. Profile hooks: before_open, after_open, stage, post

- Status: accepted
- Date: 2026-11-07
- Scope: profiles ([0022](0022-modular-profiles.md)), material module ([0025](0025-material-module.md)), `blender/render.py`

## Context
0022 asked for a hook contract the render script offers to profiles and left its signatures open. The StarBreaker
add-on has no hook mechanism of its own; what works without patching it: setting module constants or replacing
methods before a package opens (methods resolve at call time), and a pass over the finished materials after it
opened (under `blender -b` the add-on builds them synchronously on load; every material carries stamps such as
`starbreaker_template_key` and `starbreaker_submaterial_json`). Look values tuned by eye move into the add-on's
`runtime/constants.py` (starbreaker-ps ADR 0015), so most material taste is a constant. `render.py` already runs in
these phases: open (the add-on's material build), material tweaks, pose/camera/light/world, render, post-processing.

## Decision
A profile's script may define four hooks, all optional; the defaults are today's `render.py` behaviour:
- `before_open`: set add-on constants, replace add-on methods (material module).
- `after_open`: a pass over the scene's materials and objects after the add-on built them (material module; today's
  `sharpen_gloss`, `boost_emission`, `hide_screens`, `unturn_single_mesh` become the default).
- `stage`: pose, camera, light rig, world (scene module).
- `post`: tone curve, bloom, image output (output module).
The profile file format and the exact signatures are decided when profiles are built.

## Alternatives
- One hook at the end of the add-on's material build: needs an add-on change and does not cover the scene and the
  output.

## Consequences
- The game-tile look becomes the default profile's hooks and constants.
