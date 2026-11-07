# CLAUDE.md

packshot: renders Star Citizen items (StarBreaker export + Blender) in parallel, on one machine or a cluster, with a
ratatui TUI. Public repository, MIT. Split out of sc-quartermaster (the consuming org tool) on 2026-11-05.
Layout: the Rust crate at the root (`src/`, `build.rs`, `shim/`), `blender/render.py` (the Blender script), `bundle/`
(bundle templates), `scripts/`, `docs/`.

Technical documentation (export, render look, scheduler, runs and sets, cluster, assessment, bundles, measurements):
[docs/pipeline.md](docs/pipeline.md).

## Rules

- **Decisions live in `docs/adr/`** (skill `adr`). Read `docs/adr/README.md` and the matching ADRs before proposing a
  design; do not re-propose what an ADR declined unless its forces changed. New ADR only after the maintainer decided.
- **English throughout**: identifiers, UI texts, comments, docs, scripts.
- **Public repo**: no game data, no personal paths, host names or tokens, no repo- or environment-specific defaults in
  help texts or messages ([0010](docs/adr/0010-english-and-environment-free-help.md)).
- **Pull requests only** ([0034](docs/adr/0034-changes-through-pull-requests.md)): branch from `main`, open a pull
  request, squash merge. A ruleset on `main` enforces it.
- Shell scripts must pass `shellcheck`. Text files are LF (`.gitattributes`).
- Doc images: flat graphics (screenshots, diagrams) as optimized PNG, photographic ones (renders, banner) as JPEG 4:4:4,
  q≈88; no WebP/AVIF.

## Commands

Run everything from the repo root. There is no pnpm here; `P` below stands for
`cargo run --release --` (or use the built `target/release/packshot`).
Folders: store `local/packshot/`, Blender `local/blender/blender-<version>-<platform>/`, StarBreaker fork build
`local/starbreaker-ps/`, downloads `local/cache/`. Versions are pinned in `deps.toml` (Blender version + archive SHA256s,
StarBreaker commit; ADR 0033): change a pin there, run `scripts/setup.*` again, rebuild the bundles.

```sh
scripts/setup.ps1 [-SkipStarBreaker]   # (setup.sh on Linux) a dev checkout as deps.toml pins it: clones ../starbreaker-ps if missing, Blender from the official
#   archive (SHA256 checked) into local/blender/, portable/ with the add-on linked (junction / relative symlink) and enabled in userpref.blend, StarBreaker built at the pin
cargo build --release    # the tool
cargo test --release     # tests with fake tools; real-tool tests are #[ignore]
SC_DATA_P4K=<Data.p4k> cargo test --release -- --ignored   # real StarBreaker + Blender (Data.p4k with build_manifest.id next to it)

P [--render] [--export] [--filter <terms>] [--scope missing|failed|all] [--set <name>] [--name <run>] [--plain|--serve] [--workers n] [--exporters n]
#   renders the targets: long-lived processes per node: one `starbreaker entity export-batch --stdin` per export slot (the fork, local/starbreaker-ps;
#   without --stdin in its --help one export-batch per export chunk) and one Blender + add-on (local/blender) + blender/render.py --stdin per render slot
#   (restarted after 200 jobs or 3 GB, export-batch after 40 chunks or 2 GB; a panic in an export job is answered as its failure, a crash fails only the job in flight), several in parallel, terminal dashboard with render preview
#   every Blender gets --threads 2; on Linux also the RDSEED shim via LD_PRELOAD (shim/rdseed_shim.rs, built by build.rs with rustc + rust-lld, embedded, written to <work>/rdseed-shim-<hash>.so)
#   no roles: --render (Blender) and --export (Data.p4k + StarBreaker) say what this node can do now, neither = it only starts runs, stores them, watches; counts change in the TUI (+/-, </>)
#   --export needs $SC_DATA_P4K (or --p4k) with build_manifest.id next to the Data.p4k; --targets <file> sets the target set's list; --dry-run lists what would be rendered; --size <px> (256); --chunk <n> (50); --cache-limit <GB> (20, 0 = none)
#   --store/--work/--starbreaker/--blender/--render-script override the paths; --refresh-tables rereads pose and light tables from the DataCore; --help lists everything
#   every run is runs/<date>_<time>_<name>/ (images, run.json, events.jsonl, run.log) on its head node, its images go into the target set sets/<set>/ there (hard links, default live; a new set branches off live)
#   every set carries its target list (sets/<set>/targets.json: a new set takes its base's, `t` in view 2 or --targets <file> replaces it; starting a run (Enter) asks for one while the target set has none)
#   one packshot process per store folder and per work folder (packshot.lock in each, --dry-run too, --report needs none)
#   TUI views: 1 Run (n head node, s set, a abort, / filter, Enter start), 2 Sets (Enter target set, n new set, t target list, l adopt into live, x delete, i details), 3 Nodes (workers with the assessed items/s, run throughput,
#   runs to fetch h, disk, prerequisites, status per node; details with the items each node holds; the Next run tile; live manifest m, on this node m drops it; assess b, b again aborts); e switches log/errors; Tab or 1-3 switch views
#   workers read E (export), R (render); PACKSHOT_GRAPHICS=sixel|kitty|iterm2|halfblocks forces one
#   cluster settings in packshot.json in the store (token; optional listen, peers, discovery; copy packshot.example.json), without a token all local; nodes find each other by UDP broadcast on port 47950 (TCP the same)
#   every node can start a run on itself or on another node (its head node: holds the run, hands export chunks to exporting nodes, which hold what they exported, and render chunks to rendering ones, its own items first;
#   a renderer fetches the rest straight from the holder by content hash into its cache, so the nodes must reach each other); sends its status to all; every node may abort it
#   the head node gives the state: every node needs the same packshot build, Blender, add-on and StarBreaker; the exporting nodes' Data.p4k in sync with each other; else no run starts; one run at a time in the cluster

P --assess                              # compute assessment on this node, then exit (TUI: b in view 3, also for another node)
#   a 48-item test set (stratified by type from the target set's list, exported once, here or fetched from an exporting node, kept in work/assess/), n long-lived Blender = 1, 2, 3, 4, 6, 8, ...
#   (each warmed by one unmeasured item) until < 5 % gain or RAM/VRAM, sampled every 0.5 s (CPU per core, GPU, VRAM, RAM), then e exports with them alongside (capped by RAM);
#   result + validity keys into assessment.json next to packshot.json (stale: the formula again); while valid its counts are the defaults and the head node sizes each node's chunks by its assessed items/s

P --report [log|run dir]                # evaluates a run's event log (runs/<run>/events.jsonl, default the latest run): items/s, phases p50/p95, chunk overhead and slot use of every node's Blenders,
                                        # export overhead, transfer between nodes, CPU/GPU, slowest items

cargo build --release --target x86_64-unknown-linux-musl   # the static Linux binary, linked by rust-lld (.cargo/config.toml; target once via rustup target add x86_64-unknown-linux-musl)
scripts/packshot-bundle.sh [--platform all|windows|linux] [--out <dir>] [--targets <file>]   # on Linux, both bundles: local/packshot-bundle/windows (packshot.exe cross-built with mingw: rustup target x86_64-pc-windows-gnu + mingw64-gcc;
#   StarBreaker (only with a .commit stamp equal to the pin), the pinned Blender zip (local/cache/, SHA256 from deps.toml), portable/ with the add-on, cli/render.py, targets.json, setup.cmd, run.cmd; Data.p4k + build_manifest.id go next to it)
#   and linux (static packshot, the Linux StarBreaker (glibc: it prints the version it needs), the pinned Blender tarball, portable/ with the add-on, render.py, targets.json, setup.sh, check.sh, run.sh)
scripts/packshot-bundle.ps1 [-Platform all|windows|linux] [-Out <dir>] [-Targets <file>]   # on Windows, the same two bundles (portable/ from the pinned local Windows Blender)
scripts/starbreaker-build.sh [--platform all|windows|linux] [--commit <rev>]   # (.ps1 -Platform -Commit on Windows) builds the pinned commit (git archive, --commit HEAD to try a fork change) of ../starbreaker-ps
#   into local/starbreaker-ps/ (starbreaker.exe; starbreaker for Linux via cargo-zigbuild against glibc 2.31, needs zig + cargo-zigbuild + rustup target x86_64-unknown-linux-gnu,
#   on Windows too; Windows from Linux via mingw); stamps each binary (<binary>.commit, the bundles copy it as COMMIT), COMMIT = this OS's binary
```

Fork changes (exporter or Blender add-on in `../starbreaker-ps`) follow its AGENTS.md: branch, pull request, squash merge, no
trailers. To try one before it is merged: `scripts/starbreaker-build.* -Commit HEAD` (the bundles refuse it); once merged, put the
new commit into `deps.toml` and build again.
- The add-on in `local/blender` is a junction (Windows) or relative symlink (Linux) into the fork repo, made by `scripts/setup.*` (live at once).
- Add-on tests: `local/blender/<ver>/5.2/python/bin/python.exe -m unittest discover -s tests -q` in `../starbreaker-ps/blender_addon` (no system Python).
- Exporter tests with real game data: `SC_DATA_P4K=<Data.p4k> cargo test -p starbreaker-3d` in `../starbreaker-ps`.
- Render references: the game's own inventory tiles and wiki shots (see `docs/pipeline.md`).
