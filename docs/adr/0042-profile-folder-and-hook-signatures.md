# 0042. A profile is a folder with profile.toml and hooks.py; hooks take one context

- Status: accepted
- Date: 2026-11-07
- Scope: profiles ([0022](0022-modular-profiles.md)), scene ([0023](0023-scene-as-blend-template.md)), output
  ([0027](0027-output-formats-thumbnails-are-users.md)), hooks ([0041](0041-profile-hooks.md)), `blender/render.py`
- Supersedes in part: [0022](0022-modular-profiles.md) (the exporter is not a profile module)

## Context
0022, 0023, 0027 and 0041 left the details open: the profile file format, the hook signatures, how the game-driven
pose and light rig ([0003](0003-pose-and-light-from-game-data.md)) fit the `.blend` template, the list of output
formats, and the tension between "exporter as a module" (0022) and StarBreaker in the equality check
([0014](0014-same-state-or-no-run.md), [0024](0024-head-node-distributes-profiles.md)). `render.py` already runs in
the phases the hooks name: load (the add-on builds the materials), material tweaks, pose/camera/light/world, render,
tone curve and bloom.

## Decision
- **Format:** a profile is a folder: `profile.toml` (paths to its parts and parameters such as `size`, `samples`,
  `fit`, `format`) and `hooks.py`; optional a `scene.blend` and its own add-on copy. TOML as in `deps.toml`. The
  profile hash is taken over the whole folder. The profile's parameters are defaults: a CLI flag overrides them (for
  example `--size` over `size`). One output size per run; further sizes are the consumers' job (0027).
- **Hooks:** every hook takes one context `ctx` (the job, the profile parameters, the scene, the paths):
  `before_open(ctx)`, `after_open(ctx)`, `stage(ctx)` (sets camera and light), `post(ctx, exr_paths)` returning the
  output files. A hook the profile does not define runs the `game-tile` version; today's `render.py` functions become
  exactly these defaults.
- **Pose and light:** stay job data from the game (`job.pose`, `job.rig`, computed by packshot as today). The template
  brings world, render settings and compositor; `stage` decides whether to apply the game's pose and rig. `game-tile`
  applies them, a studio profile may ignore them.
- **Output formats:** PNG, EXR (linear, for own grading) and JPEG. No WebP/AVIF. GLB comes later as an output module of
  its own, after fleetyards' `feat/web-glb` work.
- **Exporter:** stays in the equality check and is not a profile module, as long as only pinned StarBreaker releases
  run ([0026](0026-starbreaker-by-release-pin.md)).

## Alternatives
- JSON or a Python file as the profile file: JSON has no comments, a Python file makes the parameters code.
- One hook signature per phase with its own arguments: every new piece of state would change the signatures.
- Pose and rig inside the template: the game's per-category values would have to be baked into every template.
- Exporter as a profile module: a profile could change the exports, so exporting nodes could no longer share them
  across runs and the equality check loses StarBreaker.

## Consequences
- Building profiles follows these details; where profiles are stored and how a set names its profile are decided when
  they are built.
- A profile that wants another exporter needs another packshot state (a new pin), not a profile.
