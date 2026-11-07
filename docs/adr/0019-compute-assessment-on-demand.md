# 0019. Compute assessment on demand, per node

- Status: accepted; superseded in part by [0046](0046-assessment-never-stale.md) (a stale assessment stays in use, other keys) and [0047](0047-assessment-per-os.md) (one entry per OS)
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot (`assess.rs`, `assessment.json`, TUI view 3)

## Context
How many Blender and export processes a node should run depends on its CPU, GPU, VRAM and RAM, and the head node can only split chunks fairly if it knows each node's speed. The default (3/4 of the physical cores for rendering) is a guess. An assessment is expensive, so it must not run unasked.

## Decision
- The assessment runs only on request: `--assess` (on this node, then exit) or `b` in the Nodes view, also for another node; `b` again aborts. Never automatically.
- Test set: 48 targets stratified by type (largest remainder, deterministic) from the target set's list, exported once and kept in `<work>/assess/testset-<p4k>-<list>/` (fetched from an exporting node if needed).
- Render ladder: n long-lived Blender = 1, 2, 3, 4, 6, 8, 10, 12, 16, ... up to 32. Each first renders one unmeasured warm-up item, then all keep rendering the set (rotated) until each process has at least 8 images. Stop when the gain is under 5 %, items fail, or RAM/VRAM is short (10 % kept free). Workers = the smallest n within 5 % of the best. Then an export ladder e = 1..4 with the chosen Blenders running alongside. The sampler runs every 0.5 s (CPU per core, GPU and VRAM via `nvidia-smi`, RAM).
- The result goes to `assessment.json` (separate from `packshot.json`) with validity keys: host name, CPU, Blender version, GPU, `render.py` hash, add-on hash, StarBreaker commit. Any change makes it stale. The packshot build hash is deliberately not a key (7407f49).
- While valid, its counts are the default worker counts, and the head node sizes each node's chunks by its assessed items/s and prefers faster nodes.

## Alternatives
- Measuring with a fresh Blender per chunk, summed per process: made many processes look slower (one node measured 30.0 vs 36.6 items/s).
- Including the packshot build in the validity keys: every code change would invalidate every assessment.

## Consequences
- The sweep takes minutes and uses the whole machine.
- The assessed counts show in the Nodes view as e.g. export 4 (26.9/s), render 8 (38.4/s).

## References
- sc-quartermaster 4061fce, 54b882d, 7407f49, a7ab98f
- [0008](0008-blender-vulkan-threads-long-lived.md), [0015](0015-chunks-and-content-hash-fetch.md)
