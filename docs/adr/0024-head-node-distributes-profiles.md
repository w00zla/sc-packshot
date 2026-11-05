# 0024. The head node distributes profiles to the nodes

- Status: accepted
- Date: 2026-11-05
- Scope: cluster, profiles
- Supersedes in part: [0014](0014-same-state-or-no-run.md) ("packshot syncs nothing itself")

## Context
Until now `render.py` travelled with each render chunk, but the add-on did not: every node had to be updated by hand,
and a run refused to start while states differed. With modular profiles ([0022](0022-modular-profiles.md)) every
experiment brings its own render script, add-on, override script and scene template; updating every node by hand per
experiment defeats the purpose.

## Decision
The head node sends a run's profile (render script, add-on folder, override script, scene template, parameters) to
the nodes that work for the run, keyed by content hash, as it does with `render.py` today. Nodes keep received
profiles in their cache.

## Alternatives
- Keep "same state or no run" for everything: one manual update per experiment and node.

## Consequences
- The equality check of [0014](0014-same-state-or-no-run.md) still holds for everything outside a profile
  (packshot build, Blender version, StarBreaker binary, Data.p4k among exporters).
- Nodes execute Python they receive: [0028](0028-security-notice-only.md).
