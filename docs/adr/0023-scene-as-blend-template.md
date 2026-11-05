# 0023. The photo scene is a .blend template plus parameters

- Status: accepted
- Date: 2026-11-05
- Scope: scene module, render.py

## Context
Camera, lights, world, colour management and compositing are built in code today (`render.py` constants and
functions). Experimenting with the scene by editing Python and re-running is slow and needs code knowledge.

## Decision
A profile's scene is a `.blend` file the user edits in Blender (camera, lights, world, compositor, render settings),
plus parameters in the profile (for example output size, samples, fit). packshot brings each item into the template
and renders it.

## Alternatives
- Parameters only: every scene feature would need its own parameter in packshot.
- Code only (today): hard to experiment with.

## Consequences
- Template and parameters are part of the profile hash ([0022](0022-modular-profiles.md)) and are distributed with it
  ([0024](0024-head-node-distributes-profiles.md)).
- The game-driven pose and light ([0003](0003-pose-and-light-from-game-data.md)) must fit into the template model;
  how is open.
