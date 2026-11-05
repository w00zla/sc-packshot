# 0001. Render item images from 3D models: StarBreaker decomposed export and headless Blender

- Status: accepted
- Date: 2026-10-24
- Recorded: 2026-11-05
- Scope: packshot, src/icons/render.py, StarBreaker fork

## Context
Star Citizen's `Data.p4k` contains no usable 2D item images (`displayIcon` is empty or a generic loadout icon); the game renders its inventory tiles at runtime from the 3D models. The consuming org tool (sc-quartermaster) needs one image per gear item, shop item, harvestable and ore chunk (6734 entities at the time). The first probe exported StarBreaker 0.3.2 as GLB and loaded it into Blender: gear came out white or blue and black paint was wrong, because GLB cannot carry the LayerBlend/HardSurface shader families (the layer's tileable detail texture ends up as base colour, vertex colours are blend masks).

## Decision
- Export with StarBreaker `--kind decomposed` (`Packages/<name>_LOD1_TEX2/scene.blend` plus sidecars and a shared `Data/` tree per chunk).
- Render headless in Blender (portable 5.2.3) with the StarBreaker add-on, which rebuilds the shader families (HardSurface, LayerBlend_V2, Illum, GlassPBR, MeshDecal, POM, palettes).
- Renderer: EEVEE, 16 samples, transparent film, 256 px PNG per catalog UUID. The add-on is "Cycles-first with EEVEE fallbacks"; no visible problem was found with EEVEE.
- Add-on and exporter must come from the same fork commit, so both are taken from the StarBreaker fork (github.com/w00zla/starbreaker-qm).
- Clothing is rendered worn, like armour.

## Alternatives
- Bundled GLB export: wrong materials, see Context.
- Headless three.js or a wgpu renderer: listed as options in the first handoff, never pursued; rebuilding the material system would take months and the look would need re-tuning, for a run that happens once per game patch.
- Cycles/OptiX: high look risk, only a half-day spike was suggested; not done.
- `bpy` as a Python module: saves only the process start, and there is no Python on the Windows dev setup.
- Several items per scene (grid): Blender caches compiled shaders per material, but light and refraction leak across cells and ruin the look.
- Other Python pipelines (StarFab, scdatatools, SCTools): judged "no advantage".
- Moving render.py into Rust: it must run inside Blender; only the PNG write (0.02 s/item) could move, which would split the look logic across two languages. Rust stays orchestration (see [0007](0007-rust-ratatui-orchestrator.md)).

## Consequences
Materials match the game's families as far as the add-on reproduces them; fixing a look often means fixing the fork. Blender is a hard runtime dependency. Ship-only parts (turret variants, thrusters, Javelin/890J parts) are out of scope: not buyable and not inventory (52 of 6734 known failures accepted).

## References
- Related: [0002](0002-facts-only-from-unlicensed-projects.md), [0003](0003-pose-and-light-from-game-data.md), [0005](0005-own-tone-curve-and-studio-world.md).
- sc-quartermaster 690ef74, 0e90f78, 2335c65, 86ac546.
