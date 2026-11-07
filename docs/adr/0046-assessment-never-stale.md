# 0046. An assessment stays in use until a new one replaces it

- Status: accepted
- Date: 2026-11-07
- Scope: compute assessment ([0019](0019-compute-assessment-on-demand.md))
- Supersedes in part: [0019](0019-compute-assessment-on-demand.md) ("any change makes it stale")

## Context
0019 made an assessment stale when any of its keys changed (host name, CPU, Blender version, GPU, `render.py`, add-on,
StarBreaker); a stale one fell back to the share of the cores. Since `render.py` is the core and the look moved into
profiles (0041-0045), every change to `render.py` or `game-tile` invalidated every node's assessment, although such
changes barely move the throughput, and a sweep takes minutes and the whole machine.

## Decision
- An existing assessment is always used: its process counts are the defaults and the head node sizes chunks by its
  items/s, whatever changed since.
- The keys are still recorded; when one differs the assessment is shown as stale, with what differs, but still used. A new
  assessment (`--assess` or `b`) replaces the old one.
- Profile parts are no keys: neither `game-tile` (the look the assessment renders) nor the add-on (the profile's
  material module, [0025](0025-material-module.md)), and no host name. The keys are CPU, RAM (total, in GB), Blender version, GPU, `render.py` and
  StarBreaker.

## Alternatives
- Fewer keys (only Blender, GPU, StarBreaker): still throws away a measurement the user did not ask to drop.
- Keep 0019: one sweep per node after every core or look change.

## Consequences
- An assessment measured on other hardware (e.g. a store copied to another machine) stays in use until reassessed;
  it shows as stale when CPU, RAM or GPU differ.
