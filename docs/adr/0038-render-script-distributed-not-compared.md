# 0038. The render script is distributed, not compared

- Status: accepted
- Date: 2026-11-07
- Scope: cluster (`cluster.rs`, `engine.rs`), build id (`build.rs`)
- Recorded: 2026-11-07, from the code at a4c28c8 and `docs/ARCHITECTURE.md` ("distributed, not compared")

## Context
[0014](0014-same-state-or-no-run.md) made `render.py` part of the compared state and the build hash cover the source
and `build.rs`. The code does something else: the head node sends its `render.py` with every render chunk to the
other nodes, and the state check compares only packshot build, Blender, add-on and StarBreaker (plus the `Data.p4k`
fingerprint among exporters). 0014's status pointed at [0024](0024-head-node-distributes-profiles.md), which is not
built.

## Decision
- `render.py` travels with each render chunk from the head node (`engine.rs`, render start); every node renders with
  the head node's copy, so it is not part of the compared state.
- The packshot build id hashes the top-level `src/*.rs` files and the RDSEED shim source (`build.rs`), with CRLF
  normalised; `build.rs` itself and subfolders such as `src/ui/` are not part of it.
- The rest of 0014 holds: Blender, add-on and StarBreaker must match the head node, or no run starts.

## Alternatives
- Compare `render.py` as 0014 wrote: refuses runs over a file the head node already hands out.

## Consequences
- Changing `render.py` on the head node needs no bundle refill; changing the add-on still does until 0024 is built.
- A change only in `build.rs` or `src/ui/` (e.g. the screenshot generator) does not change the build id.
