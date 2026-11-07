# Architecture

How packshot is built: the data flow, the material sidecars it moves around, its modules, the life of an item in a
run, the scheduler, the cluster, the processes, runs and sets, and the state check. The decisions behind it are in
[adr/](adr/README.md).

## One orchestrator, two worker processes

![Data flow: Data.p4k through StarBreaker and Blender into a run and its set, driven by packshot](assets/arch-overview.png)

packshot renders nothing itself. It selects what to render, cuts the work into chunks, hands them to long-lived
**StarBreaker** and **Blender** processes and stores the images. Pose and light rig come from the game's DataCore,
read by packshot itself. No database: the target list is a file per set.

| Stage | What happens |
|---|---|
| Export | StarBreaker reads an entity from `Data.p4k` and writes a decomposed package: `scene.blend`, meshes, PNG textures and the material sidecars |
| Render | Blender loads the package; the StarBreaker add-on builds the node trees from the sidecars; `render.py` calls the run's profile (default `game-tile`): pose, fit and light rig, an Eevee render to a linear EXR, the image: PNG or JPEG through the profile's tone curve, or a linear EXR |
| Store | the image goes into the run's folder, never changed afterwards, and as a hard link into the target set; consumers read `sets/main` |

## Material sidecars

![scene.json points at mesh, sidecar and palette; mesh slots map to sidecar submaterials, those to textures](assets/arch-sidecars.png)

In the game a CryEngine `.mtl` file describes a model's materials per **submaterial**: shader, feature flags,
textures, parameters. Blender cannot read it, and a mesh file carries only named material slots. So StarBreaker
translates every `.mtl` into a JSON **sidecar** (`<mtl>.materials.json`) next to meshes and textures:

- origin: the source `.mtl` and geometry paths
- per submaterial: name, shader and its family (`Illum`, `HardSurface`, `LayerBlend_V2`, …), whether it is drawn
- the `StringGenMask` feature flags, decoded (decals, POM, vertex colours, stencil, iridescence)
- texture slots with their role (base colour, normal-gloss, blend mask, …), game path and exported PNG
- layers of layered materials with their own textures, tiling and palette channel; parameters; screen effects
- the raw `.mtl` attributes, kept for diagnosis and for rules not written yet

The add-on matches each mesh slot to the submaterial of the same index, picks a builder by shader family (hard
surface, Illum, glass, screens, effects, POM decals, else a principled material), wires the textures by role and the
palette colours by the submaterial's routing. It builds an equal material once: identity is the sidecar content plus
the palette, not the file name. One sidecar serves every mesh of its `.mtl`; paint variants get their own exterior
sidecar (`paints.json`).

packshot reads sidecars for one thing: an item's **closure**, the files a renderer must fetch. The sidecars name the
textures, and since sidecars and textures are shared between items, they cross the network once per run.

## Modules

Everything runs on the main thread: the engine ticks, the TUI draws its state, the processes answer in stdout lines.
Only network reads, process output, the DataCore tables and the picker's entity list while they load, the
assessment's sweep and sampler and the GPU sampling have threads of their own, which hand over messages.

| Module | Role |
|---|---|
| `main.rs` | arguments, checkout or bundle, paths, the locks on store and work folder; modes TUI, `--plain`, `--serve`, `--dry-run`, `--report`, `--assess` |
| `ui.rs` | the terminal UI (ratatui): views Run, Sets, Nodes, dialogs, log, image preview (Sixel, Kitty, iTerm2, else half blocks); reads the engine, sends only actions |
| `engine.rs` | a run: selection by filter and scope, chunks, pool, assignment, backlog, timeouts and requeues, results into run and set |
| `cluster.rs` | the engine's multi-node part: capabilities instead of roles, head node, state check, status every second, runs and manifests from other nodes |
| `node.rs` | the work on every node, the head node too: export chunks exported and held, render chunks fetched, linked and fed to Blender |
| `assess.rs` | the compute assessment: test set, render ladder, export sweep, CPU/GPU/VRAM/RAM sampling, `assessment.json` (one entry per OS) |
| `store.rs` | runs as immutable folders, sets as hard links to the newest image of their chain, a target list per set |
| `events.rs` | the event log per run (`events.jsonl`) and `--report` |
| `net.rs` | TCP frames of JSON plus payload, hello with token and versions, UDP discovery, checks on all data from other nodes |
| `cas.rs` | an item's closure, content hashes (xxh3-128), the renderer's content cache with its size limit |
| `entities.rs` | the game's entities for the target picker (`dcb query`, display names from `global.ini`), cached per game build; the picker's selection rules |
| `config.rs` | `packshot.json`: token, listen address, peers, discovery |
| `tables.rs` | pose and light tables of the inventory and the entities' tags from the DataCore (a tagged entry wins for an entity with that tag), cached in the work folder |
| `blender.rs`, `shim/` | one command line for every Blender; on Linux the RDSEED shim via `LD_PRELOAD` |
| `blender/render.py` | runs inside Blender: loads the package, calls the profile's hooks, renders, answers `icon:` or `failed:` with phase timings |
| `profile.rs`, `profiles/` | profiles: resolution (`--profile`), content hash, the files a render chunk carries; `game-tile`, the default look |

## An item through a run

![An item's states in a run, with the requeue and failure paths](assets/arch-lifecycle.png)

Every item has exactly one state at the head node. Export and render are decoupled: in between the item waits in the
**pool**, held on the node that exported it until the head node releases its export chunk. A chunk without progress
for 300 s goes back, an item at most twice; a render chunk still fetching its files gets six times as long.

## Scheduler

The head node cuts a chunk only when a process frees up. Its size (`guided()` in `engine.rs`): the items left divided
by three times the processes of that kind, at least 10, at most `--chunk` (50), never more than the items left divided
by the processes. Chunks shrink towards the end, so all processes finish together.

- **Per node:** with an assessment (stale ones too, ADR 0046) the size scales with the node's measured rate per
  process against the mean of the nodes taking part; a slow node gets smaller chunks, so the run's end does not wait for it.
- **Assignment:** an export chunk goes to the fastest node with a free export slot. A free Blender takes items its own
  node holds first, else those of the holder with the most waiting items.
- **Backlog:** no new export chunk while exported but unrendered items plus running exports reach
  `2 × Blenders × chunk size`; that bounds the holders' disk.

## Cluster

Every node starts with what it can do now: `--render`, `--export`, both or neither. Any node can start a run, on
itself or on another node, which then holds it as the **head node**. Nodes find each other by UDP broadcast and
connect to each other over TCP (port 47950). One run at a time in the cluster; the head node sends its status every
second, every node may abort it.

![A render chunk with items another node holds](assets/arch-fetch.png)

- **Closure:** an item needs part of its export folder only: its package folder and every file one of its JSON files
  names, through the sidecars. A test makes sure such a partial folder renders bit-identically to the full export.
- **No compression:** 90 % of the bytes are PNG textures, compressed already. Instead, files go by content hash:
  equal textures cross the network once per run, and the renderer's cache keeps them (`--cache-limit`, 20 GB).
- **Everything is checked:** paths, names, hashes and run data from other nodes are validated (no `..`, no drives); a
  wrong token is turned away.

## Processes

Every node runs a number of **export slots** and **render slots**, each with one long-lived process fed jobs over
stdin. A restart costs: StarBreaker reloads the `Data.p4k` index and the DataCore (about 0.75 s), Blender needs about
1.9 s.

| Process | Restarted after |
|---|---|
| Blender | 200 jobs or 3 GB resident memory |
| export-batch | 40 export chunks or 2 GB resident memory |
| both | 60 s idle |

A job that panics in StarBreaker is answered as a failure and the process goes on. A process that crashes fails only
the job in flight; the rest of its chunk goes to a new one. On Linux, Blender 5.2 asks `std::random_device` for
randomness on every node update, which libstdc++ serves with the slow RDSEED instruction; that capped a whole PC at
about 13 items/s. packshot embeds a tiny library replacing that one function and loads it via `LD_PRELOAD`: 25 items/s,
bit-identical images.

## Runs and sets

![Set main with its chain of runs and an experiment set branched off it](assets/arch-sets.png)

A **run** is a folder that never changes after its end. A **set** is a view: per entity a hard link to the image of
the newest run in its chain. Experiments branch off `main` and are adopted by appending their runs.

```
packshot.json      cluster: token, port, peers
assessment.json    measured process counts, one entry per OS (windows, linux)
targets/
  targets.json     the list the dialog offers first
  <name>.json      lists from the picker or by hand (subfolders are listed too)
profiles/
  <name>/          own profiles: profile.toml, hooks.py (hide a bundled one of the same name)
runs/
  2026-11-05_14-30_full/
    <Name> (<class>) <uuid>.png   read-only (.jpg/.exr with another format)
    run.json  events.jsonl  run.log
sets/
  main/            hard links · set.json (chain, size and format) · targets.json
  glow-test/       an experiment
manifests/         other nodes' main sets, for "missing"
work/
  export/          export chunks, deleted after use
  cas/             content cache of fetched files
  fetch/           render chunks, linked from cas/
  render/          images from the Blenders before they move into the run
  inbox/           files from other nodes as they arrive
  send/            files on their way to another node
  dcb/             pose and light tables, entity tags, the picker's entity list
  profile-cache/   render.py and profiles received for render chunks, by content hash; started/: starters' profiles
  assess/          the assessment's test set
  preview.<ext>    the latest image rendered on this node
  blender.log      the Blenders' output
  rdseed-shim-<hash>.so   Linux: the RDSEED shim
```

At every start packshot clears `export/`, `cas/`, `fetch/`, `send/` and `profile-cache/` (with a cluster `inbox/`
too): what an earlier session left there is stale.

## Same state or no run

Another add-on builds other materials, another game build exports other data. So before a start the head node compares
the versions of all connected nodes; if anything differs, no run starts and the TUI says what.

| What | From | Applies to |
|---|---|---|
| packshot build | hash over the source, the same on Windows and Linux | all nodes |
| Blender version | the node's Blender | all nodes |
| add-on | hash over the add-on's files | all nodes |
| StarBreaker | the commit in the `COMMIT` file next to the binary | all nodes |
| Data.p4k | size plus branch, version and build id from `build_manifest.id` | exporting nodes, among each other |
| `render.py`, profile | travel with every render chunk from the head node (`render.py` and `game-tile` its own, the profile the starter's) | distributed, not compared |

## Compute assessment

Hardware differs a lot: a node with 16 GB VRAM, one with four cores, a server without a screen. Instead of rules of
thumb the assessment measures, on request (`--assess`, key `b`), how many processes a node carries: a 48-item test
set stratified by type, exported once; one unmeasured warm-up; a render ladder of 1, 2, 3, 4, 6, 8 … Blenders until
the gain drops below 5 % or RAM or VRAM run out; then an export sweep with the Blenders alongside. Samples every 0.5 s
(CPU per core, GPU, VRAM, RAM). The result goes into `assessment.json`, one entry per operating system, so a PC booting
both keeps two ([ADR 0047](adr/0047-assessment-per-os.md)); it sets the default counts and the chunk sizes until a new
one replaces it ([ADR 0046](adr/0046-assessment-never-stale.md)). When CPU, RAM, GPU, Blender, `render.py` or
StarBreaker differ from what it was measured with, it shows as stale, with what differs, and stays in use.

## Profiles

The look is a **profile** ([ADRs 0022](adr/0022-modular-profiles.md), [0041](adr/0041-profile-hooks.md),
[0042](adr/0042-profile-folder-and-hook-signatures.md), [0044](adr/0044-profile-per-run.md)); cluster, chunks, runs,
sets and assessment do not depend on it.

- **Core and look:** `render.py` is the core: it opens the item, calls the profile's hooks (`before_open`,
  `after_open`, `stage`, `post`), renders a linear multi-layer EXR and hands it to `post`, which writes the image. The
  bundled `game-tile` is the default and the base of every profile: its parameters lie under a profile's, its hooks
  run where a profile has none.
- **Extras:** a profile may bring a `scene` .blend (world, cameras, lights; `ctx.use_scene_blend()`) and its own copy
  of the StarBreaker add-on (`addon`, [ADR 0025](adr/0025-material-module.md)), which `render.py` enables instead of the installed one for its whole
  process; the equality check still compares the installed add-on.
- **Choice:** per run, by the starter: `--profile` (a name, or a folder) or the TUI's start dialog. A start on another
  node picks the target set there (its target list comes with the set's state) and sends the selected targets, filter
  and scope and the profile's folder along, so the head node need not have the profile.
- **Size and format:** the target set's ([ADR 0045](adr/0045-size-and-format-per-set.md)), fixed when the set is
  created (`main`: 256 px PNG; a set from before them counts as that); the head node puts them into every job. A
  `profile.toml` that sets `size` or `format` is refused.
- **Distribution:** the head node sends `render.py`, `game-tile` (both its own) and the run's profile with every render
  chunk ([ADR 0024](adr/0024-head-node-distributes-profiles.md)); a node keeps them in `work/profile-cache/` by content
  hash and starts a Blender per `render.py` and profile.
- **Record:** the run records `render.py`'s hash (and whether it came from `--render-script`), the profile (name,
  origin, hash, scene), the set's size and format, and the add-on it used; the Sets view shows the profile and
  the add-on in the run's tile, size and format on the set. The assessment always measures
  `game-tile` as PNG.
