# 0003. Compute pose and light rig from the game's inventory data

- Status: accepted
- Date: 2026-10-24
- Recorded: 2026-11-05
- Scope: packshot (tables.rs), render.py

## Context
The images should look like the game's own inventory tiles, so camera orientation and lighting should come from the same data the game uses rather than from hand-tuned per-item values.

## Decision
- Pose: `PlayerChoice_PITConfig.inventoryParams` (read with StarBreaker `dcb query`): `defaultOrientation` (90, 0, 0) plus a per-type/sub-type offset (45 entries). Euler XYZ, Y-up cell, camera on -Z, screen-up -Y (not mirrored: a rifle's muzzle points right).
- Light: a light group per category (39 entries, default `inventorylightgroup_other`) pointing at 53 rig entities `inventorylightgroup_*` with key/rim/fill projectors.
- Matching: type plus sub-type exactly, then type plus UNDEFINED; entries with a tag condition are skipped; else the default.
- Fit (`--fit`, default 0.7): the item is scaled so its largest extent is 0.7 m, because the rig stands about 1 m away. Dark EMP/Radar/Tractor icons turned out to be a size problem, not a light problem.
- Spot power: 30 W per game intensity unit (`--light-scale`, default 30).
- packshot computes pose and rig itself on the exporting node (a port of the former TypeScript table loader, identical for all 6734 targets and 53 rigs). `targets.json` carries no pose or rig.

## Alternatives
- 100 W per unit: burns out white materials.
- Raising the rig fill by 10-20x: lights matt faces only.
- Per-item hand-set poses: against [0006](0006-calibrate-against-game-tiles.md).

## Consequences
New item types get a plausible pose without code changes; types without an orientation entry use the default and are unchecked against the game. Entries with `requiredTag` (sidearms receive the primary-weapon rig) are not handled. Since [0022](0022-modular-profiles.md) this is the default profile's behaviour, not a fixed part of packshot.

## References
- sc-quartermaster 0648d94; its docs/catalog-sources.md "Icons" step 3.
- Related: [0002](0002-facts-only-from-unlicensed-projects.md).
