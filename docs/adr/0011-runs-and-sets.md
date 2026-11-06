# 0011. Immutable run folders and sets of hard links

- Status: accepted
- Date: 2026-11-03
- Recorded: 2026-11-05
- Scope: packshot (store layout, `store.rs`)

## Context
The old render loop wrote into one flat icons folder. Experiments (a new pose, a new tone curve) overwrote good images, and nobody could tell which add-on or `render.py` had produced a given image. packshot needs to keep results per run and let a user try changes without touching the images the consumer reads.

## Decision
The packshot folder has the same layout on every node: `packshot.json`, `assessment.json`, `targets.json`, `runs/`, `sets/`, `manifests/`, `work/` (`--store`, `--work` override).

- A run is `runs/<YYYY-MM-DD_HH-MM>_<name>/` with read-only images `<Name> (<class>) <uuid>.png`, `run.json`, `events.jsonl`, `run.log`. Further UUIDs of the same entity are hard links. A finished run is immutable; a run without results is removed.
- Default run name: filter terms, scope action, and the first 6 hex digits of the add-on and `render.py` hashes.
- A set is `sets/<name>/` holding hard links to the newest image per entity across its chain of runs, plus `set.json` and its own `targets.json` (the target list changes with game patches, so it is per set).
- `live` always exists, has no base, cannot be deleted and is what the consumer reads. Experiments branch off `live` (`--set <name>`) and are adopted by appending their runs to the `live` chain. A run can be deleted from the middle of a chain. At start, set folders are repaired against their chain.

## Alternatives
- Content-hash store for finished renders: renders are not bit-identical, so deduplication gains nothing.
- One flat output folder: no provenance, experiments overwrite results.

## Consequences
- Readable names for eyeballing in a file browser; the consumer only needs the UUID in the file name.
- Hard links need one file system per packshot folder.
- A run fetched from another node is appended to `live` only.

## References
- sc-quartermaster 8b99788, 9959881
- [0015](0015-chunks-and-content-hash-fetch.md) (hashing is used for transfer, not for storage)
