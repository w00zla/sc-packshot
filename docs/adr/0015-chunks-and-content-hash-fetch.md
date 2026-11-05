# 0015. Export and render chunks with node-to-node fetch by content hash

- Status: accepted
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot (`engine.rs`, `node.rs`, `cas.rs`, `net.rs`)

## Context
The first cluster design sent "packages" (export plus render on one remote node) with the head node as the only scheduler. Packages could not move flexibly between nodes, had no timeout, and re-sent whole blocks. Measured export data: about 97 % of the bytes are 8-bit RGBA PNG textures (already compressed), and 52 % of the bytes in the export folder were duplicates across blocks.

## Decision
- Two chunk kinds. An export chunk goes to one export process, a render chunk to one Blender. Both are cut when started: size = rest / (3 x processes of that kind), minimum 10, maximum `--chunk` (default 50), scaled per node by assessed items/s.
- Items move todo -> in an export chunk -> in the pool (holder remembered) -> in a render chunk -> done. Exported items stay on their holder; a render chunk may mix items from several export chunks and prefers items held on its own node.
- A renderer fetches missing files straight from the holder (never via the head). Files are identified by xxh3-128. An item's closure is its package folder plus every file named in its JSONs, followed through sidecars. The renderer keeps a per-run cache `<work>/cas/` and builds a per-chunk folder `<work>/fetch/` with hard links.
- No compression on the wire (zstd -3 gains only 1.2-1.4x on these files, at CPU cost).
- Limits: `BACKLOG` = 2 (no new export chunk while exported-but-unrendered plus in-export items reach 2 x Blenders x render chunk size), `CHUNK_TIMEOUT` 300 s, fetch 6 x 300 s, `MAX_REQUEUES` 2, then the item fails with a reason.
- Mandatory test `closure_renders_like_full`: 45 mixed items rendered from closure-only folders are bit-identical to the full export (RMSE 0.0000 %); without the largest texture it differs, so the test detects missing files. Missing textures otherwise render silently white.

## Alternatives
- Work stealing, backup tasks for stragglers, a DAG engine, a shared file system: not needed, the central queue already balances.
- Transfer through the head node: every byte would cross its link twice.
- zstd, gzip, lz4: tiny gain.

## Consequences
- Measured dedup: 20 % from cache in a two-process loopback run (806 MB transferred, 203 MB cached).
- The render cache grows unbounded per run (see [0016](0016-data-p4k-only-on-exporters.md)).
- Every node must reach every other node ([0013](0013-cluster-capabilities-and-head-node.md)).

## References
- sc-quartermaster a4bdf98
