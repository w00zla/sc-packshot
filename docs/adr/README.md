# Architecture Decision Records

One decision per file, `NNNN-title.md`. Accepted records are not rewritten; a changed decision gets a new record that
supersedes the old one. 0001–0020 were recorded afterwards (2026-11-05) from the development in sc-quartermaster.

| No. | Title | Status | Date |
|---|---|---|---|
| [0001](0001-render-icons-from-3d-models.md) | Render item images from 3D models: StarBreaker decomposed export and headless Blender | accepted | 2026-10-24 |
| [0002](0002-facts-only-from-unlicensed-projects.md) | Take facts, not code, from projects without a licence | accepted | 2026-10-24 |
| [0003](0003-pose-and-light-from-game-data.md) | Compute pose and light rig from the game's inventory data | accepted | 2026-10-24 |
| [0004](0004-fan-content-and-no-game-data.md) | Renders are fan content; no game data in the repository | accepted | 2026-10-25 |
| [0005](0005-own-tone-curve-and-studio-world.md) | Own tone curve, studio world and glow handling instead of Blender view transforms | accepted | 2026-11-02 |
| [0006](0006-calibrate-against-game-tiles.md) | Calibrate against the game's own tiles; general rules over per-item fixes | accepted | 2026-11-02 |
| [0007](0007-rust-ratatui-orchestrator.md) | Rust and ratatui orchestrator, named packshot | accepted | 2026-11-01 |
| [0008](0008-blender-vulkan-threads-long-lived.md) | Blender on Vulkan with two threads, long-lived worker processes | accepted | 2026-11-02 |
| [0009](0009-event-log-and-report.md) | Per-run event log and a read-only report | accepted | 2026-11-03 |
| [0010](0010-english-and-environment-free-help.md) | English throughout; help text free of repository and environment specifics | accepted | 2026-11-03 |
| [0011](0011-runs-and-sets.md) | Immutable run folders and sets of hard links | accepted | 2026-11-03 |
| [0012](0012-bundles-per-os.md) | Per-OS bundles built by script | accepted | 2026-11-03 |
| [0013](0013-cluster-capabilities-and-head-node.md) | Cluster of capability nodes with a head node | accepted | 2026-11-04 |
| [0014](0014-same-state-or-no-run.md) | Same state on all nodes or no run | superseded in part by [0024](0024-head-node-distributes-profiles.md) | 2026-11-03 |
| [0015](0015-chunks-and-content-hash-fetch.md) | Export and render chunks with node-to-node fetch by content hash | accepted | 2026-11-04 |
| [0016](0016-data-p4k-only-on-exporters.md) | Data.p4k as a local copy on exporting nodes only | accepted | 2026-11-04 |
| [0017](0017-delete-work-data-log-deviations.md) | Always delete work data; keep failures reproducible through logs | accepted | 2026-11-04 |
| [0018](0018-validate-peer-input-and-lock.md) | Validate everything peers send; one process per folder | accepted | 2026-11-04 |
| [0019](0019-compute-assessment-on-demand.md) | Compute assessment on demand, per node | accepted | 2026-11-04 |
| [0020](0020-rdseed-shim.md) | Embedded RDSEED shim for Blender on Linux | accepted | 2026-11-05 |
| [0021](0021-standalone-public-repo.md) | packshot is a standalone public repository; sc-quartermaster is a consumer | accepted | 2026-11-05 |
| [0022](0022-modular-profiles.md) | Modular profiles: targets, exporter, material, scene and output are replaceable | accepted | 2026-11-05 |
| [0023](0023-scene-as-blend-template.md) | The photo scene is a .blend template plus parameters | accepted | 2026-11-05 |
| [0024](0024-head-node-distributes-profiles.md) | The head node distributes profiles to the nodes | accepted | 2026-11-05 |
| [0025](0025-material-module.md) | Material module: add-on folder plus optional override script | accepted | 2026-11-05 |
| [0026](0026-starbreaker-by-release-pin.md) | StarBreaker is used through a pinned release | accepted | 2026-11-05 |
| [0027](0027-output-formats-thumbnails-are-users.md) | packshot writes common image formats; thumbnails are the user's business | accepted | 2026-11-05 |
| [0028](0028-security-notice-only.md) | Executing distributed profiles: a documented notice, no extra code | accepted | 2026-11-05 |
| [0029](0029-no-set-comparison-in-tui.md) | No set comparison in the TUI | accepted | 2026-11-05 |
