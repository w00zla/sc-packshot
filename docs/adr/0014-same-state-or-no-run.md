# 0014. Same state on all nodes or no run

- Status: superseded in part by [0024](0024-head-node-distributes-profiles.md) (the "syncs nothing" part)
- Date: 2026-11-03
- Recorded: 2026-11-05
- Scope: packshot (`cluster.rs`, state check in the TUI)

## Context
Missing or different pieces change the output silently: another add-on version builds other materials, another Blender version shades differently, a different game build exports different data. Mixed results inside one run would be impossible to reproduce.

## Decision
All connected nodes must report the same packshot build (hash over the source and `build.rs`, equal on Windows and Linux), Blender version, add-on hash and StarBreaker commit. `render.py` is part of the compared state. Exporting nodes must additionally hold the same `Data.p4k`: fingerprint = FNV over file size plus Branch/Version/BuildId from `build_manifest.id` (first version: size plus the first and last 4 MB). The head node defines the reference; it needs no `Data.p4k`. If anything differs, no run starts; the Nodes view turns red and the "Next run" tile names the first difference. Every node reports all versions regardless of `--render`/`--export`. packshot itself syncs nothing; the maintainer keeps the bundles current.

## Alternatives
- Pushing the add-on and config from the head node to outdated nodes: implemented first, then removed ("packshot syncs nothing itself").
- Using "first node with the capability" as the reference: arbitrary, dropped for the head node.

## Consequences
- Every code change changes the build hash, so all bundles must be refilled.
- The game version is recorded per run from the exporting nodes, not pinned on the set.
- The build hash was later removed from the assessment's validity keys ([0019](0019-compute-assessment-on-demand.md)).

## References
- sc-quartermaster 3898614, 4dc90fd, b412f49
- [0013](0013-cluster-capabilities-and-head-node.md), [0016](0016-data-p4k-only-on-exporters.md)
