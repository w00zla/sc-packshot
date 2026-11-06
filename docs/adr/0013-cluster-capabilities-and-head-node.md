# 0013. Cluster of capability nodes with a head node

- Status: accepted
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot (`cluster.rs`, `net.rs`, `engine.rs`, `node.rs`)

## Context
A single machine rendered a full run in about 52 minutes (4 hours for the first run). Several machines of different strength are available. A first design had fixed roles ("coordinator" holds Data.p4k and distributes, "node" only renders) and a `coordinators` address list. The maintainer found fixed roles and the config pointless once nodes could discover each other.

## Decision
- No roles. A node starts with what it can do now: `--render` (needs Blender), `--export` (needs `Data.p4k`, `build_manifest.id` and StarBreaker). With neither it only starts runs, stores results and watches. Worker counts change at runtime in the TUI.
- Any node can start a run on itself or on a chosen head node. The head node holds the run folder, set, images and event log, schedules chunks and sends a compact status to all nodes once a second. Any node can abort. Only one run runs at a time in the cluster.
- Discovery: UDP broadcast beacon every 2 s (magic, token hash, instance id, TCP port) on port 47950; TCP on the same port. Full mesh. Framing: u32 JSON length, JSON, u64 payload length, payload. Identity is the host name from the handshake.
- `packshot.json` needs only `token`; optional `listen`, `peers` (static fallback for other subnets), `discovery`. Without a token everything stays local.
- `--serve` runs without a TUI and without a run of its own.

## Alternatives
- Fixed coordinator/node roles with an address list: replaced.
- Item packages exported and rendered on one remote node: a 4-core node received 5 packages of 50 items and stopped responding; replaced by chunks ([0015](0015-chunks-and-content-hash-fetch.md)).
- Port 47800: taken by another application on the dev machine, hence 47950.

## Consequences
- Windows needs UDP and TCP in the firewall; discovery works within one L2 segment.
- Every node must reach every other node directly.
- Discovery was tested on one machine at recording time; real multi-node runs were still to be done.

## References
- sc-quartermaster 4497d65, 9959881, 4061fce
