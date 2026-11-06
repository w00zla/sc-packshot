# 0016. Data.p4k as a local copy on exporting nodes only

- Status: accepted; the render-node cache is limited since [0031](0031-render-cache-limit.md)
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot cluster, deployment

## Context
`Data.p4k` is about 158 GB. StarBreaker reads it with a pool of file handles and `pread` (no mmap): about 650 random small reads (roughly 40 MB) per item spread over 127 GB, plus about 500 MB per `export-batch` start (central directory 469 MB, `Game2.dcb` 337 MB). The question was whether the file can live centrally or be preprocessed so not every node needs it.

## Decision
Every exporting node keeps its own local copy of `Data.p4k` with `build_manifest.id` next to it. Render-only nodes need no p4k at all, because exports reach them as chunks ([0015](0015-chunks-and-content-hash-fetch.md)). The p4k fingerprint is compared among exporting nodes only ([0014](0014-same-state-or-no-run.md)). A network share is at best a master copy for `rsync`/copy (about 25 minutes at 1 GbE).

## Alternatives
- P4K on SMB/NFS: HDD seek times (about 10 ms) are hopeless; with an SSD, 1 GbE caps the cluster, 2.5 GbE is marginal, 10 GbE could work. iSCSI gives no gain.
- Trimmed p4k (about 70 GB instead of 158 GB): only worth it for a node without a game install; the maintainer declined it.
- `TEX3` instead of `TEX2` for icon textures (about a quarter of the texture bytes, possibly less sharp): the maintainer decided to keep `TEX2`.
- LRU limit for the render-node cache: the maintainer decided against it; the cache stays unbounded per run.
- An entry server inside packshot: a new feature, not pursued.

## Consequences
- Each exporting node needs the disk space and the same game build.
- The maintainer excluded a NAS evaluation from the follow-up work; this ADR keeps these ideas from being proposed again unless the forces change (for example a 10 GbE SSD share).

## References
- sc-quartermaster a4bdf98
