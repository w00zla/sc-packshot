# 0022. Modular profiles: targets, exporter, material, scene and output are replaceable

- Status: accepted
- Date: 2026-11-05
- Scope: architecture, render.py, add-on, CLI

## Context
packshot's main use will be changing materials and the photo scene and comparing the results. Today the look is
fixed: `render.py` mixes three concerns (the job pipeline; material tweaks such as `boost_emission`,
`sharpen_gloss`, `glass_see_through`; the scene: `STUDIO`, `EMISSION_GAIN`, `BLOOM`, `ROUGHNESS`, `CONTRAST`, tone
curve, camera and light as constants), and the Blender add-on is whatever the StarBreaker fork ships. The
orchestration (cluster, chunks, runs and sets, assessment) is already almost independent of the look.

## Decision
packshot is a core plus modules, combined freely by a **profile**:

| Module | Default | Replaceable by |
|---|---|---|
| Targets | selector over the DataCore | an explicit list |
| Exporter | pinned StarBreaker release ([0026](0026-starbreaker-by-release-pin.md)) | another build |
| Material | add-on + optional override script ([0025](0025-material-module.md)) | own add-on copy, own script |
| Scene | `.blend` template + parameters ([0023](0023-scene-as-blend-template.md)) | own template |
| Output | formats per [0027](0027-output-formats-thumbnails-are-users.md) | — |

- A set is a target list plus a profile. The hash over all modules of a profile is part of the state the nodes share
  and of the default run name.
- Two stable interfaces: the job protocol between packshot and the render script (exists: `--stdin`, one JSON job per
  line, `icon:`/`failed:` answers with phase timings) and a hook contract the render script offers to profiles
  (prepare scene, load item, material, render, post-process).
- Today's look becomes the bundled default profile `game-tile`.
- The profile file format and the exact hook signatures are not decided yet.

## Alternatives
- More CLI flags on the fixed pipeline: does not let users change materials or the scene.

## Consequences
- Profiles must reach every rendering node: [0024](0024-head-node-distributes-profiles.md).
- The backfilled look decisions (0003, 0005, 0006) describe the default profile, not packshot itself.
