# 0031. The render-node cache has a size limit

- Status: accepted
- Date: 2026-11-06
- Scope: cluster, render nodes (`cas.rs`, `node.rs`)
- Supersedes in part: [0016](0016-data-p4k-only-on-exporters.md) (no LRU for the render-node cache)

## Context
A rendering node keeps every file it fetched from other nodes in its content-addressed cache (`<work>/cas/`,
[0015](0015-chunks-and-content-hash-fetch.md)) until the run ends. A full run on a node that renders much but holds
little grows that cache to the size of most of the export. The limit was declined on 2026-11-04 and is wanted now.

## Decision
- `--cache-limit <GB>` (default 20, `0` = no limit) caps the cache of a rendering node.
- After each pass over the waiting render chunks the cache drops its least recently used files until it fits; a file
  counts as used when a chunk links it or finds it cached. Files that waiting chunks still need are never dropped.
- Render chunks hold hard links (or copies) of their files, so dropping a cached file does not touch a chunk in
  flight; a later chunk that needs the file again fetches it again. A file that cannot be deleted (Windows: open
  without share-delete) stays until a later pass.

## Alternatives
- No limit (until now): unbounded disk use per run.
- Limit by free disk space: depends on what else is on the disk; a fixed number is predictable.

## Consequences
- With a small limit, files shared by many chunks (textures) may be fetched more than once.
- The limit is per node and per process, not part of the state nodes must share.
