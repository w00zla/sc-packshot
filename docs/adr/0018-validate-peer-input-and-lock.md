# 0018. Validate everything peers send; one process per folder

- Status: accepted
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot (`net.rs`, `node.rs`, `store.rs`)

## Context
Nodes accept paths, file names, hashes and run files from other nodes over TCP. A review of the chunk and assessment code found network path traversal and similar problems. A second packshot started on the same folders could delete data the first one was using.

## Decision
- Every peer-supplied path, package name, rig, run file name, entity name and UUID goes through `safe_rel`, `safe_name` or `target_problem`. Hashes must be 32 hex characters (`net::safe_hash`). On Windows `safe_rel` also rejects `:`.
- Run files are accepted only for run ids in `fetching` (keyed by peer and run); an imported run is rejected entirely if one file name is unsafe.
- Windows renderers reject paths longer than 259 UTF-16 units and case-only collisions before rendering, on the holder and on the renderer, because Blender would silently render missing textures white. The message points at a shorter `--work` folder.
- Lost or unreachable items are requeued (`Unfetched` message, `ItemsInfo.gone` triggers re-export) and one incident counts once toward `MAX_REQUEUES`.
- `packshot.lock` lives in both the work folder and the store folder (once if they are the same), taken with `std::fs::File::try_lock`, released however the process ends; `packshot.pid` names the first process. A second packshot on the same folder aborts before it deletes anything. `--dry-run` takes the locks too (it syncs sets); only `--report` runs without (read-only).
- Test ports come from 47951-48199, never the cluster port 47950, and no test sends discovery beacons.

## Alternatives
Not recorded.

## Consequences
- Some peer input that used to be accepted now fails loudly; a bundle with a long path needs a short `--work`.

## References
- sc-quartermaster a4bdf98
- [0015](0015-chunks-and-content-hash-fetch.md)
