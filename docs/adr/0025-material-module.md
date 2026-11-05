# 0025. Material module: add-on folder plus optional override script

- Status: accepted
- Date: 2026-11-05
- Scope: material module, add-on, render.py

## Context
Materials are built by the StarBreaker Blender add-on from the exporter's sidecars; packshot's own material tweaks
(`boost_emission`, `sharpen_gloss`, `glass_see_through`, `hide_screens`, `unturn_single_mesh`) sit in `render.py`.
Users want both small tweaks and fundamentally different material builds.

## Decision
A profile's material module is
- an add-on folder: by default the add-on of the pinned StarBreaker release
  ([0026](0026-starbreaker-by-release-pin.md)), replaceable by the user's own copy, and
- an optional override script that runs after the add-on and changes the finished node trees and objects.

Both work through the same mechanism: paths in the profile, hashed and distributed like the rest of it.

## Alternatives
- Override script only: cannot change how the add-on reads the sidecars (palette fallback, LayerBlend, decals)
  without monkeypatching.
- Own add-on only: every small tweak would mean maintaining a full add-on copy.

## Consequences
- An own add-on copy does not follow StarBreaker updates and must match the exporter's sidecar contract
  (`docs/decomposed-export-contract.md` in the fork).
- The tweaks in `render.py` move into the default profile's override script.
