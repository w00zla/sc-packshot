# packshot pipeline

Technical reference: what packshot does to an entity, how the work is scheduled, what ends up on disk, and the
numbers behind the constants. The reasons for the big decisions are in the [ADRs](adr/README.md); this file holds the
facts.

Contents: [Overview](#overview) · [Export](#export) · [Render](#render) · [Pose and light](#pose-and-light) ·
[Output: runs and sets](#output-runs-and-sets) · [Scheduler](#scheduler) · [Processes and the RDSEED shim](#processes-and-the-rdseed-shim) ·
[Terminal UI](#terminal-ui) · [Event log and report](#event-log-and-report) · [Cluster](#cluster) ·
[Compute assessment](#compute-assessment) · [Builds and bundles](#builds-and-bundles) · [Measurements](#measurements) ·
[Known failures](#known-failures) · [References for review](#references-for-review) · [Open and unproven](#open-and-unproven) ·
[Consumers: sc-quartermaster](#consumers-sc-quartermaster)

## Overview

`Data.p4k` holds no 2D item images (`displayIcon` is empty or a generic loadout icon), so packshot renders them from
the models ([ADR 0001](adr/0001-render-icons-from-3d-models.md)):

1. **Export** with StarBreaker (`entity export-batch`): a decomposed package per entity (`scene.blend`, meshes,
   textures, materials).
2. **Render** in a portable Blender with the StarBreaker add-on and `src/icons/render.py`: Eevee, 16 samples,
   transparent film, 256 px (`--size`), pose and light rig computed from the game's own inventory data.
3. **Store** the PNG in a run folder and link it into a set.

The orchestrator is `tools/packshot/` (Rust, ratatui, ratatui-image; [ADR 0007](adr/0007-rust-ratatui-orchestrator.md)).
It runs export and render with several long-lived processes at once, on one machine or a cluster. Nothing here needs
Node or a database: the target list is a file (`targets.json`), pose and rig are read from the DataCore by the
exporting node itself.

Fan content of this kind is fine (the Star Citizen wiki renders its thumbnails the same way,
[ADR 0004](adr/0004-fan-content-and-no-game-data.md)).

## Export

Command per export slot: `starbreaker entity export-batch --stdin --kind decomposed --variant-tags --icon-files`.

- **Long-lived:** the process loads P4k and DataCore once (~0.75 s), then reads one JSON job per line (the same object
  as one entry of a jobs file) and answers each with a result line. `{"end_chunk": "<dir>"}` closes an export chunk
  (answered `{"chunk_done": "<dir>"}`), EOF or `{"quit": true}` ends it. ~0.09 s per entity against 0.13 s in batch
  mode. packshot detects the mode in `export-batch --help`; a StarBreaker without `--stdin` runs
  `export-batch <jobs.json>` once per export chunk of up to 50 entities (P4k and DataCore load per process, ~1.5 s; an
  entity takes ~0.2 s instead of ~1.9 s for a single `entity export`).
- **Jobs** name the exact class and `worn` for armour and clothing (the worn model instead of the loot crate; without
  a usable worn model the exporter falls back to the default geometry).
- `--variant-tags` gives paint variants (`..._tan01`) their own tagged SubGeometry. `--icon-files` leaves out animation
  clips and diagnostics.
- **Layout:** each package lands in `Packages/<name>_LOD1_TEX2/scene.blend`; all packages of a chunk share one `Data/`
  tree (`<work>/export/x<id>/`). A mesh file carries its material in the name
  (`ui_ore_raw_1_a_LOD1_ui_ore_gold_1_a.blend`): its slots are named after the material, and variants on one geometry
  overwrote each other before ("no meshes in the scene").
- **Material source:** a record's material comes from `SGeometryResourceParams.Geometry.Geometry.Material`, else from
  its `Material` node (`SMaterialNodeParams`, variants by tags); 70 items (54 backpacks, paint editions of weapons)
  name it only there.
- **Worn or paint-variant export** takes the chosen SubGeometry's mesh and its node table (182d229f); with the default
  geometry's table, the model's parts landed on the crate's helpers by index (floating shoulder pads, a shirt as its
  cardboard box; 615 records).
- **Palette fallback:** an entity without a palette of its own takes its material's `DefaultPalette`, then the game's
  `TintPalettes/default` (primary white, secondary sRGB 112, tertiary 21; 559cb793).
- **Windows path limit:** paths inside an export reach ~160 characters; Blender on Windows reads none over 259 (missing
  textures render silently white). A Windows exporting node reports an export chunk with a longer path ("paths over 259
  characters": move `--work` to a shorter folder). A Windows rendering node checks the paths of every fetched item as
  linked under its `fetch/` folder before Blender starts, and those of an item it holds itself (its closure in the
  export folder); an item with a path over 259, or with a path differing from another in that folder only in case (a
  Linux holder can have both, NTFS keeps one file), fails with that reason (naming `--work`). Only the paths actually
  linked count.
- An export chunk's folder is deleted once every item of it is rendered or failed.

**Bundled GLB is unusable:** for LayerBlend/HardSurface shaders it carries one layer's tileable detail texture as base
colour, and the vertex colours are blend masks. Hence the decomposed export plus the add-on.

### The StarBreaker fork

The fork **starbreaker-ps** (`local/starbreaker-ps/`, built from the repo `../starbreaker-ps`, MIT, version
`0.3.2-qm.n`, commit in `COMMIT`) is required ([ADR 0026](adr/0026-starbreaker-by-release-pin.md)). It is the
[fleetyards fork](https://github.com/fleetyards/StarBreaker) with upstream (diogotr7) merged in (merge commit
9aa7ce15): fleetyards' item, material and add-on work, upstream's rest (skinning, characters, `--geometry-tag`,
quick_xml, fixes such as the Sion compensator's texture). The Blender add-on is fleetyards' (both rebuilt the decal
system, a mix did not run) plus our fixes. Remotes `fleetyards` and `upstream`; a sync is a merge there, checked by
rendering a sample against the current images.

Why not the others: the 0.3.2 release misplaced weapon magazines and its add-on did not stack LayerBlend layers.
Upstream main fixed the magazines but rendered armour as its loot crate (worn model only via `--geometry-tag Male`,
T-posed arms, white palettes), lost paint variants and drew visors flat.

Changes of qm.2 and after (all 2026-11-01): `export-batch`, the material node, the material in mesh file names, the
worn node table (182d229f), the palette fallback (559cb793), Organic and ore veins (2435146f), palette metals
(bfb057e9), character packages without the material suffix (ddf92d00). Later: 23679ae6 (sidecar submaterials of paint
variants: the part materials their CDF appends were missing from the variant's sidecar when the base entity of the
batch wrote it first; 35 warnings in a full run), `--stdin` mode.

## Render

`blender -b --python src/icons/render.py -- --stdin` in a portable Blender (`local/blender/<version>/`, config under
its `portable/`; [ADR 0008](adr/0008-blender-vulkan-threads-long-lived.md)):

- Blender starts once, prints `ready: pid <pid>`, then takes one JSON job per stdin line (the same object as one entry
  of the `--jobs <file>` list) and answers each with its `icon:` / `failed:` line. EOF or `quit` ends it. Each job opens
  its own file, so every job starts from the same clean state. `--jobs` and `<src> <out>` still work for direct use.
  With `--jobs` and `--stdin` it turns on the add-on's path index shared per export root,
  `manifest.share_path_indexes(True)`, when the add-on has it (off for interactive use: a hit is re-checked and a miss
  rescans once).
- `portable/scripts/addons/starbreaker_addon` is a junction (Windows) or relative symlink (Linux) to the fork repo's
  `blender_addon/starbreaker_addon`, so add-on and exporter always match; bundles copy the content. The add-on builds
  every package's materials when `scene.blend` opens; in background mode at once and in full, as
  `starbreaker.refresh_materials` does (its UI path waits for a timer that background mode never runs).
- Blender runs on **Vulkan** (`--gpu-backend vulkan`) with `--threads 2` ([measurements](#measurements)).
- `render.py` appends its phases to every `icon:` line: `timing load=... prep=... render=... glass=... post=...`
  (opening incl. the add-on's material build; material tweaks/pose/camera/light/world; shader compilation plus render
  and EXR; the glass pass; `write_png`). The TUI shows their means and logs them per item.

### Look

The render is written as a linear multi-layer EXR (combined and emission, read with Blender's OpenImageIO);
`render.py` turns it into the PNG itself ([ADR 0005](adr/0005-own-tone-curve-and-studio-world.md)):

1. the emission bloom (below),
2. a contrast below mid-grey: luminance Y < 0.18 maps to 0.18 * (Y/0.18)^1.45, one factor for all channels so hues
   stay (black lacquer reflects a few percent under the key light and rendered dark grey without it; applied over the
   whole range it lifted highlights too and white burnt out again),
3. the shoulder of Khronos PBR Neutral without its toe: linear up to 0.8, the brightest channel compressed softly
   towards 1 above, slightly desaturated.

Blender's own transforms did not fit (probe of 2026-11-01 over 19 items, scene-linear colours checked against wiki
shots and inventory tiles): Standard clipped light albedos hard (a flat white area, 5.5 % of the light items' pixels
even at -0.5 EV); PBR Neutral's toe subtracts min(R,G,B) - 6.25*min^2 from every channel, so dark colours turned black
or garish; AgX and Filmic keep colours and highlights but turn white grey.

**World:** a studio, not a grey sphere: a softbox above (strength 0.4 from 20 to 80 % of the screen-up direction), 0.05
from the camera side, 0.02 elsewhere (`STUDIO` in `render.py`). Eevee lights diffuse and glossy from the same world
probe: a former `Is Glossy Ray` mix never had an effect (checked by pixel-identical renders). A uniform grey world laid
a veil over black lacquer; no world left chrome reflecting black.

**Glass and screens:**
- Ray tracing is on. The add-on gives `GlassPBR` materials raytraced refraction (dithered); they are pure
  transmission, and without it Eevee only refracts the world and visors render flat grey.
- A glass takes the palette's glass colour only with `%USE_TINT_PALETTE`, otherwise its own `Diffuse`, and keeps its
  hue (the template's hue node stood at 0.57 and turned every tint by +25 degrees; fixed in the fork 2026-11-02).
  Visors stay darker than in the game tiles: the game adds a thin-film sheen (`%THIN_FILM`) the add-on does not draw.
- Screens (`Monitor`, `DisplayScreen`, `UIPlane`) render dithered, not blended; a `Monitor` glows in its texture times
  its `Diffuse`, and reflects with its `Specular` as F0 at roughness 0.35 (authored none; emission alone drew eyeballs
  black). Eevee's raytraced refraction does not see blended surfaces, so blended screens behind lenses drew as grey
  discs.
- A glass's `RefractionGloss` 0 counts as clear, not frosted (15 of 791 character glasses author 0).

**Emission and bloom:**
- A LayerBlend_V2 material's flat glowing layers emit the geometric mean of `Emissive` and that layer's tint (scaled
  to Emissive's peak); fitted to one item's eye strip, no shader source.
- `render.py` multiplies every emission by 4 (`EMISSION_GAIN`, `boost_emission`): at the data's strength glows were
  barely visible.
- The game blooms its lights: the emission pass is added blurred (`BLOOM` 2, Gaussian sigmas 3/8 px at 512 px weighted
  0.6/0.4, picked from ten variants; a 20 px sigma spread the halo wider than the game tile shows; the halo outside the
  item covers by its brightness).
- Light behind glass is missing from the emission pass, so an item with glass renders a second time at 4 samples with
  every glass see-through in its tint (`glass_see_through`, ~0.1-0.2 s); the bloom takes that pass's emission.
- Emission strength of materials without their own emission input is `Glow` (x `EmissiveOpacity`), not the mean of
  the `Emissive` colour. The Illum path keeps its own rule (mean of `Emissive`); with `Glow` there, LEDs came out
  darker than in game.

**Gloss:** glossy surfaces (metal, glass) render at 0.85x their roughness (`sharpen_gloss`): gold stayed one even tone
without it. 0.45x matched the glints but turned black lacquer black; 0.85 was picked by eye between 1.0 and 0.7.

**Materials worth knowing:**
- *Organic* (minerals): colour is the first layer's `TintColor` (the crystal; layer 2 is grey sand) times that layer
  material's own `Diffuse`, doubled for the grey diffuse (a detail map), with the layer material's coloured `Specular`
  as specular tint and level; fitted to the game's warehouse tiles.
- *Ore thumbnails* (`ui_ore_*_1_a`, HardSurface with `WEAR_LAYER`) have no wear mask of their own: their layer
  materials carry the vein pattern `ore_raw_1_a_blend.tif` as `TexSlot8`, used as the wear factor (red, smoothstep
  0.6-0.8, not scaled by the scene's wear strength).
- A metal LayerBlend layer on a palette channel takes that channel's finish specular in place of its own, times the
  slot's `TintColor` (its F0), not tint x palette tint; otherwise gold and gun metal drew black or a bright steel blue
  about 4x the game's dark slate (measured against wiki shots of six helmets).
- LayerBlend_V2 `%DECALS` draws its decals "via 2nd UV set" (`Shaders/LayerBlend_V2.ext` in
  `Engine/ShaderCache_EXT.pak`). A mesh without a second-UV stream keeps that set in its vertex colour, two 12-bit
  fields (`R<<4 | G>>4`, `(G&15)<<8 | B`, 1/1024 steps, shifted by half); the exporter decodes it when the mesh's
  material set has such a submaterial (not for HardSurface `%VERTCOLORS`, real colours). Undecorated vertices (160, 21,
  254) land on the sheet's empty corner. The plain sheet is TexSlot9 (`DiffuseDecalSpec` as F0 through the Khronos
  spec-to-metal solve, `DiffuseDecalGloss`).
- Some materials carry a parallax occlusion set in TexSlot1/2/4/8 (colour + coverage, normal, spec, height); it is
  laid over through the second UV set (normal in its own tangent frame, height as bump, no ray-marched parallax) and is
  not the material's own base texture.
- Add-on fixes of 2026-11-01: a submaterial name with a trailing space (`"ore_hephaestanite "`, 5 ores) never matched
  its slot and rendered white.

**Meshes `render.py` handles specially:**
- Meshes that carry only UI screen materials (template `screen_hud`, e.g. the mobiGlas display, invisible without UI
  content) are hidden and left out of the fit, unless they are all the item has (a 25 x 41 cm screen plane shrank a
  bracelet to a dot).
- An item of exactly one mesh renders without that mesh's node rotation: the game shows it in the mesh's own axes. The
  FPS minerals' root node turns +90 degrees about X and they stood upright where the warehouse tiles show them lying
  (8 of 10; all 10 free tiles match after). The fork keeps the rotation and links the same mesh .blend into assemblies
  where it holds (upstream test `collapsed_wrapper_node_preserves_non_identity_root_transform`). In 70 other sampled
  items no single mesh carried a rotation.

Calibration is done against the game's own inventory tiles (30 items: armour, weapons, ship components, minerals;
[ADR 0006](adr/0006-calibrate-against-game-tiles.md)); general rules over per-item fixes. The tile under the cursor in
a warehouse recording is covered by the tooltip: take the same cell from a later frame.

## Pose and light

From the game data ([ADR 0003](adr/0003-pose-and-light-from-game-data.md)); facts only, the pointer to these records
came from a renderer without a licence ([ADR 0002](adr/0002-facts-only-from-unlicensed-projects.md)).

- `PlayerChoice_PITConfig.inventoryParams` (`dcb query`) gives `defaultOrientation` (90, 0, 0) plus an offset per item
  type/sub type (45 entries) and a light group per category (39 entries, default `inventorylightgroup_other`). The
  groups are entities under `libs/foundry/records/entities/lights/scene/inventory/` (53, `dcb extract`, directory
  globs only) with key/rim/fill projector lights.
- Matching: type + sub type exact before type + UNDEFINED; entries with a tag condition are skipped.
- The model is turned by Euler XYZ of default + offset. The cell is then Y-up with the camera on -Z and screen-up -Y
  (not mirrored: rifle muzzle to the right, as in game).
- The rig sits at the cell centre turned by (90, 180, 0); each projector shines along its local +X; Blender spots at
  30 W per game intensity unit (100 W burnt white materials out under the Standard view transform); the studio world
  is the ambient (the rigs' fill light is 0.1, practically off; raising it x10-20 lit only matt faces, chrome needs
  something to reflect).
- The item is scaled so its largest extent is 0.7 m (the game fits it into the cell; the rigs' lights stand ~1 m out,
  a 3 m cooler would have them inside).
- packshot's port of the loader (`tables.rs`, `loadGameTables` before) was identical for all 6734 targets and the 53
  rigs on 2026-11-01. `--refresh-tables` reads the tables from the DataCore again.

## Output: runs and sets

([ADR 0011](adr/0011-runs-and-sets.md); `tools/packshot/src/store.rs`.)

**Folder layout**, the same on every node: `packshot.json` (cluster config), `assessment.json`, `targets.json` (the file
offered as a set's target list), `runs/`, `sets/`, `manifests/` and `work/` (cache: exports, renders, DataCore tables,
logs). In the repo that is `local/packshot/`; in a bundle the bundle folder itself (`--store`, `--work`).

**Render to run:** Blender renders one image per entity into `<work>/render/<class>.png`. It then moves into the
**run**, `runs/<YYYY-MM-DD_HH-MM>_<name>/` (name from `--name`, else the filter terms, what the scope does (`missing`,
`retry`, `full`) and the first 6 characters of the add-on's and `render.py`'s hashes, e.g.
`arms_full_addon-6ea0d4_renderpy-6da3cb`), as `<Name> (<class>) <uuid>.png` per target UUID (English name, so renders
can be checked by eye in a file browser; further UUIDs of the entity are hard links), read-only.

A run holds:
- `run.json`: its set, node, start and end, the origin as in the event log's `run_start` (filter, scope, arguments,
  starting node and nodes taking part, hashes of `render.py` and the add-on, Blender version, StarBreaker commit, game
  version, fingerprint and size of the exporting nodes' Data.p4k; `p4k_from` names the node it was taken from; hotfixes
  change it between runs of one set), per entity ok or the error and its files. Saved after every chunk.
- `events.jsonl` ([event log](#event-log-and-report)) and `run.log`.

A finished run is not changed any more; one without a single result is removed again.

**Sets:** `sets/<name>/` holds every image as a hard link to a run's image (per entity the latest run of its chain that
has an image), plus `set.json` (base, the base's chain at the branch, its own runs, the files and the run each links
to, where its target list came from) and **`targets.json`, the set's target list**.
- A new set takes its base's list; `t` in view 2 (or `--targets <file>`) replaces it with a file (default: `targets.json`
  in the packshot folder; asks before replacing). While the target set has none, view 1 says so and Enter opens that
  dialog instead of starting. The list changes with the game's updates, so each set keeps the one it was rendered with.
- `live` is the main set. A run's images are linked into its target set as they arrive (`--set`, default live; a name
  that does not exist yet branches off live).
- An experiment set takes over into live by appending its own runs to live's chain (no new run, the origin stays). A
  run can be deleted from the middle of a set's chain; older runs then show through again (the question says how many
  images fall back and how many disappear). Deleting an experiment set removes its folder and those of its runs no other
  set uses.
- Scope `missing` = not in the target set; `failed` = the latest attempt in the set's chain failed; `all` replaces
  (asks first).
- At start packshot checks the set folder against its chain and repairs it (missing links, stray PNGs).
- No store by content hash: renders are not bit-identical, deduplication gains nothing.

**Logs:** `run.log` in the run folder (the console plus full error messages; under every failure the last 20 lines of
its process: `render.py`'s traceback, Blender's or export-batch's last lines, also from other nodes; in `events.jsonl`
as the failure's `trace`) and `blender.log` (Blender's whole output) in the work directory, appended per run.

**Cleanup** ([ADR 0017](adr/0017-delete-work-data-log-deviations.md)): export folders go once their items are done,
failures included (a chunk holds up to hundreds of MB; `--filter <entity>` repeats a failure). `work/export/`, `cas/`,
`fetch/` and `send/` are cleared at start.

**One process per folder** ([ADR 0018](adr/0018-validate-peer-input-and-lock.md)): `packshot.lock` in the packshot
folder and in the work folder is locked (by the OS, released however the process ends) before anything is touched. A
second process on either exits with a message naming the first one's process id (from `packshot.pid` next to the lock;
Windows does not let another process read a locked file). `--dry-run` takes the locks too (it syncs the set, `--targets`
writes it, the DataCore tables go into the work folder); only `--report` reads and needs none, and creates nothing in
the packshot folder.

**Between nodes**, only on a key press, never by itself: in view 3, `h` on another node fetches its finished runs for
its live that are missing here (folders copied; the same id from another node gets `-<node>`) and appends them to the
local live in order (shown with a fetch arrow). `m` fetches its live manifest (`<store>/manifests/`), so "missing" on
the local live also counts what the other one already has (view 1 says "manifest of <node>"; the selection is
recomputed when it arrives); `m` on this node's own row drops it again, its file too.

## Scheduler

Selection: a filter (terms separated by blanks or commas, any of them in the class, the name or the type) and a scope
(above) over the target set's `targets.json`. `--plain` starts a run on this node without the TUI; `--filter` / `--scope`
preset both.

**Two kinds of chunk** ([ADR 0015](adr/0015-chunks-and-content-hash-fetch.md)): an export chunk goes to one export
process of an exporting node, a render chunk to one Blender of a rendering node (one chunk per process at a time).
- Both are cut only when they start: rest / (3 x processes of that kind in the cluster), at least 10, at most `--chunk`
  (50), and for small rests so that every process gets work; they shrink towards the end of a run (the export chunk over
  the items not exported yet, the render chunk over those not in a render chunk yet). A smaller minimum (4 instead of
  10) gained nothing (44.8 s on the reference set), so `guided()` keeps 10.
- The size is then scaled per node by its assessed rate: its items/s per process (at the count it runs now, from the
  assessment's steps, which the hello carries; linear between them, flat beyond the last) against the mean per process
  of the nodes taking part that have a valid assessment (at least 1, at most `--chunk` and the rest). A slow node gets
  smaller chunks, so the tail does not wait for it; a node without a valid assessment, or a cluster without any, gets
  the plain size.

**Pool:** between the stages every exported item waits in the head node's pool, held by the node that exported it; a
render chunk may mix items of different export chunks.

**Assignment** (head node, on a free process):
- Export chunks go to the fastest node with a free export process (by assessed rate per process), then the one with the
  most free.
- A free Blender takes the items its own node holds when there are a chunk's worth, else those of the holder with the
  most waiting (no waiting for locality). Among the free Blenders those holding a chunk's worth come first, then the
  fastest nodes. With less than a chunk waiting there it waits while exports are going, unless a chunk's worth waits in
  all.
- **Backlog** (`BACKLOG`): no new export chunk while the exported-but-not-rendered items plus those being exported reach
  2 x Blenders in the cluster x render chunk size; this bounds the holders' disk.

**Process counts:** `--render` defaults to the counts of the node's valid assessment, else 3/4 of the physical cores
(`--workers`); `--export` to the assessment, else 1/4 of the cores but at least 2 (`--exporters`). They change while it
runs (`+`/`-`, `<`/`>` or `[`/`]`); the capabilities do not. At least 2 exports: on 4 cores (emulated by affinity) a
single export held back 3 Blender (101 s instead of 80 s for 459 items).

**No roles:** every node starts with what it can do now. `--render` needs Blender; `--export` needs Data.p4k with
`build_manifest.id` and StarBreaker. Without both it only starts runs, stores them and watches (it says so at start).
`--serve` runs without the TUI and without a run of its own (it prints the log). `--export` opens the TUI at once and
reads the DataCore tables in the background (steps in the log, the time in the run header); the node offers no export
workers until they are there, and stays without export if reading fails.

## Processes and the RDSEED shim

Processes are long-lived on every node ([ADR 0008](adr/0008-blender-vulkan-threads-long-lived.md)):
- **One Blender per render slot** (`render.py --stdin`): a render chunk's jobs are fed one at a time with one more
  waiting in its stdin, so the main loop's tick does not delay it. Restarted after 200 jobs (`BLENDER_JOBS`) or above
  3 GB resident (`BLENDER_RSS`).
- **One export-batch per export slot** with `--stdin` (started with `MALLOC_ARENA_MAX=2` on Linux: glibc's arenas grew
  it 0.75 to 1.4 GB over 300 jobs, ~1.0 GB with it). Restarted after 40 export chunks (`EXPORT_CHUNKS`) or above 2 GB
  (`EXPORT_RSS`), between chunks.
- A chunk takes an idle process of its kind (a render chunk one with the same `render.py`, by content hash) or starts
  one. An idle process quits after 60 s (`IDLE_STOP`) and when a run ends (its memory goes back); one over the slots
  quits at once; one asked to quit and still there after 10 s is killed.
- A process that dies at work (Blender's segfault, a panic in export-batch) fails only the job it was on, with its last
  lines as the trace (`Blender ended while rendering it`, `export-batch ended while exporting it`); the chunk's other
  jobs go to a new process (the export into the same folder). One that dies before it is ready (`ready:`,
  `{"ready": true}`; `{"ready": false, "error": ...}` gives why) or with nothing in it ends its chunk: its open items
  fail with that reason.
- Every Blender gets `--threads 2` (`src/blender.rs`): with 6 Blender the same or slightly more items/s for 35-55 %
  less CPU; exports alongside lost 15 % instead of 32 %.

**RDSEED shim** ([ADR 0020](adr/0020-rdseed-shim.md)): on Linux each Blender also gets the shim in `LD_PRELOAD`
(prepended to an existing value). Blender 5.2 builds a `std::random_device` on every node tree update
(`blenkernel/intern/node_tree_update.cc` ~2164 -> `blenlib/intern/rand.cc` ~290); libstdc++ reads it via RDSEED, a slow
hardware source shared by all cores (~27 us per call), and render.py's node writes make ~1860 calls per item. That
capped the whole PC at ~13-14 items/s however many Blender ran.
- The shim (`tools/packshot/shim/rdseed_shim.rs`) replaces only `std::random_device::_M_getval()` in the Blender
  process (splitmix64 over an atomic counter, seeded once by `getrandom`; thread-safe, no libc).
- `build.rs` builds it for Linux targets only, as a `no_std` cdylib with rustc and rust-lld (no C compiler, also when
  the musl binary is built on Windows). packshot embeds it and writes it at start to `<work>/rdseed-shim-<hash>.so`
  (silently; only a shim that cannot be used is logged). A work path with a blank or colon cannot go into `LD_PRELOAD`:
  the start says "shim off" and the Blenders run without it.
- Windows has no shim (MSVC's `random_device` is believed not to use RDSEED; not verified).
- Result: 6 Blender went from 12.9 to 22.1 items/s (25.0 with `--threads 2`), images unchanged. Check of 2026-11-05 (459
  `_arms_`, 6 Blender, 4 exports, StarBreaker 38e26bc3): 12.1 items/s without both (twice), 12.1 with `--threads 2` only
  (CPU 84 to 52 %), 25.0/25.2 with both; all 459 images bit-identical across the five runs; a varied sample (506
  targets, 47 types) as close as two runs without the shim (490 against 489 identical, the same turret varying). The
  assessment's render sweep went from 16.0 to 42.5 items/s at n = 8 and now stops at the VRAM (16 GB).

## Terminal UI

Three views on every node (Tab or 1-3); log and errors below every view (`e` switches). The log starts with the start
lines (version, node, capabilities, folders, target set and list, tools, cluster). Questions and inputs (run name, new
set, a set's target list, confirmations) open as a dialog over the view; the target list dialog offers the lists in the
packshot folder with count and date, or another path. The lists take the middle, the log a third of the screen;
throughput (renders per minute) and pool (exported items not rendered yet: near 0 the exports hold the run back, at the
limit of the backpressure, shown on the head node, the renderers do) sit side by side at the bottom.

Images are shown via Sixel, Kitty or iTerm2, half blocks otherwise. The log says which protocol and cell size the
terminal gave; `PACKSHOT_GRAPHICS=sixel|kitty|iterm2|halfblocks` forces one. In Konsole packshot takes Sixel itself:
ratatui-image leaves Sixel out there and falls back to blurry half blocks, while Konsole's Sixel shows the previews
fine.

**1 Run.** Without a run: the selection with its **head node** (`n` cycles through this node and the connected ones) and
target set there (`s`), filter and scope, the to-render list and the selected item's image. Enter asks for the run name
(default: filter terms and scope) and starts it, here or on the head node; with no target list it opens the target list
dialog instead. During a run on this node: progress and ETA, the workers (local and on other nodes: `E` an export
chunk, `R` a render chunk (Blender), labelled `host/R1`, `host/E2`; phase export, fetch while a render chunk's files
come from their holders, render), below them one row with the items waiting for export, the pool (exported, not
rendered yet) with those of it waiting for a Blender, and the exported ones, renders per minute and CPU, and the latest
render as an image (left/right pages back). During a run on another node: its progress, workers and failures as its
head node reports them, and the latest image rendered here. `a` aborts the run going on from any node (asks).

**2 Sets.** This node's sets with their own runs (date, name, node, filter, images, add-on hash; a fetch arrow marks a
run fetched from another node); on the right the selected run's result and up to 8 of its images; `i` adds hashes,
Blender, StarBreaker, p4k and workers. Enter makes a set the target set, `n` creates one from live, `t` gives it a target
list, `l` adopts an experiment into live, `x` deletes a run or set (all ask). With another node chosen as head node the
view shows that node's sets and runs (from its state and run list) read only; Enter chooses the target set there;
changes only on that node itself. No set comparison in the TUI ([ADR 0029](adr/0029-no-set-comparison-in-tui.md)).

**3 Nodes.** One row per node (this one, the connected ones, addresses not reached): node, connection, workers with the
assessment (`ok export 4 (26.9/s) - render 8 (38.4/s)`: valid, `!` stale, blank without one; per type the count it runs
now and, assessed, the items/s measured at that count), Run (during a run: the node's items/s over the last 2 minutes
and its images so far, sent per node in the head node's status), runs to fetch (`h`), free disk with the share in use,
Prerequisites (what must be the same on every node; versions and the p4k fingerprint in the details), status (ready /
head node / assessing / another state, a differing node in red). Below, the selected node's details: Node (capabilities,
OS, IP, connection), Hardware (CPU and cores, RAM, GPU with VRAM from `nvidia-smi`), Disk (free and total of the disk
with the packshot folder, and of the work folder's if it is elsewhere), Workers, Holds (exported items of the run going
on that the node holds, not rendered yet), Assessed (`b`: the chosen counts with their items/s and the date, stale in
yellow, or the assessment going on); for other nodes Fetch (`h`) and Manifest (`m`); App (packshot build, StarBreaker
commit), p4k (version, fingerprint, size), Blender with add-on.

Next to it the **Next run** tile: where it starts (head node, set, items to render with filter and scope) or why it does
not (first difference in state, a run going, no target list, nobody to export or render), the export and render workers
taking part, an estimate of the time and of the room the images need on the head node (both from its latest finished
run with at least 50 results: items per second, mean PNG size; a remote head node sends them with its state), and the
free space there.

## Event log and report

Every run writes `events.jsonl` in its run folder ([ADR 0009](adr/0009-event-log-and-report.md)), one JSON line per
event with `t` (seconds since the start):
- `run_start`: run, set, head node, starting node and nodes taking part, process counts, cores, Blender version and
  backend, hashes of `render.py` and the add-on, StarBreaker commit, game version and size of the exporting nodes'
  Data.p4k.
- `export_start` / `export_item` / `export_end` per export chunk (`block` its id; seconds per item, process wall time,
  paths over 259 characters).
- `render_start` per render chunk (its holders, how long its items waited in the pool) / `render_item` (wall and
  phases) / `render_end`.
- `fetch`: a render chunk's files from other nodes (holders, bytes fetched, bytes the node's cache already had, files,
  seconds).
- `requeue` (why, how often, counted or not), `failure`.
- `sample` every 2 s: CPU, GPU via `nvidia-smi` if present, running exports and Blender, items to export, in the pool
  and held, the cache's bytes, the cluster's Blender slots.
- `run_end`.

`--report [log|run folder]` (default: the latest run) evaluates one: result and items/s, phases with mean/p50/p95, a
render chunk's first-item overhead, the slot use of every node's Blenders (from the worker labels `host/R<n>`: each
node's render chunk time against its slots, the cluster's slots from the samples; "Blender for export" against them
too), export overhead per chunk, the transfer (bytes fetched between nodes, the share the caches already had, fetch
time), how long items waited for a Blender, CPU/GPU, slowest items, failures.

## Cluster

([ADR 0013](adr/0013-cluster-capabilities-and-head-node.md), [0014](adr/0014-same-state-or-no-run.md),
[0016](adr/0016-data-p4k-only-on-exporters.md), [0018](adr/0018-validate-peer-input-and-lock.md);
`tools/packshot/src/net.rs`, `cluster.rs`, `node.rs`.)

**Config** in `packshot.json` in the packshot folder (in a bundle next to the exe): `token`, optional `listen` (default
`0.0.0.0:47950`; all nodes need the same port for the discovery), `peers` (DNS names or IPs with an optional port, for
nodes outside the own network segment) and `discovery` (default on). Without a token everything stays local. Every node
is known by its host name, which the handshake carries (a DNS name may differ from it).

**Discovery:** every node sends a beacon every 2 s by UDP to the broadcast address of each of its IPv4 networks on the
same port as TCP (JSON: a hash of the token, never the token, its host name, a random instance id per process, its TCP
port). Nodes that hear a beacon of the same token dial its sender (the address comes from the packet, not from its
content). Every node listens and connects to every other one (heard in the last 15 s or in `peers`), retrying every 3 s;
if both dial at once, the connection set up by the smaller name stays. A second process on the same node gets no UDP
port and only sends. Firewalls must let TCP and UDP 47950 in from the local network (Windows: allow packshot.exe for
private networks; firewalld or ufw: open both); the Windows firewall's silent drop shows as "connection timed out".

**Wire format:** TCP, one frame per message (u32 JSON length, JSON, u64 payload length, payload); files as payload
written straight into an inbox folder. Everything from a peer is validated: paths from the wire (no `..`, no drive, on
Windows no `:`), including the paths and hashes in a holder's description of its items (which fail their item
otherwise), run ids and set names asked for or sent, the entities and files in a fetched run's `run.json` (else the run
is not taken; a run is taken only when this node asked the sending node for it), the entity and UUIDs of every target a
start from another node brings (else no run, with the reason), and a peer's p4k fingerprint in the test set's folder
name.

**Hello:** carries token, instance, capabilities, packshot build, Blender version, add-on hash and StarBreaker commit
(always: every bundle carries Blender and StarBreaker), Blender and export counts, cores, CPU, RAM, GPU, the disks,
whether a run is going there, and with `--export` the Data.p4k fingerprint (FNV over its size in bytes and Branch,
Version and BuildId from `build_manifest.id` next to it: reads no archive data) and the game version. A wrong token is
rejected. A node sends it again right after every new connection (the welcome of an accepted connection comes from its
hello at the start: a node offers export workers only once its DataCore tables are there) and whenever its counts, its
run or its free disk (more than 1 GB, checked every 10 s) change. Ping every 10 s; a connection silent for 45 s is
dropped.

**Same state or no run:** the head node gives the state (the one chosen with `n`, during a run the run's). Every
connected node needs its packshot build (the version carries a hash of the source, `build.rs`, equal on Windows and
Linux), Blender version, add-on and StarBreaker commit, whatever it does; the exporting nodes' Data.p4k must be in sync
with each other (the head node needs none). Otherwise no run starts and the TUI says what differs (in red: the tab bar,
the node's row and status; the Next run tile names the first difference, the log all versions). packshot syncs nothing
itself: every bundle carries everything, update the bundles.

**Head node:** every node can start a run, on itself or on the node chosen with `n`, which then holds it (run folder,
set, images, event log). For another node the starter asks it for its sets and the state and target list of the chosen
set (images, last failures), computes the selection over that list and sends the selected targets; the head node
appends them to its own list, creates the set from live if it does not exist (with live's target list) and starts. It
needs neither Data.p4k nor Blender, only someone connected who exports and someone who renders. **One run at a time:**
while a node has a run going, no other one starts; who is connected at the start takes part. Every node works through
the same code for the head node's chunks, the head node itself included (`node.rs`, through `LOCAL` instead of a
connection). Every second the head node sends every node the run's status (counts, rate, workers, the latest 50
failures, phases; once more at the end); every node may ask it to abort. Log lines and workers carry the host name of
the node that did the item (`host/R2`). If the head node drops, its run is over for the others.

**Holding and fetching:** an exporting node takes pose and rig from its own DataCore tables, exports the chunk into
`<work>/export/x<id>/`, reports every exported item (every view shows "export 23/50") and then **holds** the chunk (rigs
copied in) until the head node releases it: once each of its items is rendered or failed, or at the run's end or when
the head node drops. A rendering node renders the items it holds straight from their export folder; for the others the
head node names only the item and its holder, and the renderer fetches the files **straight from the holder** over
their connection:
- It asks for the items; the holder answers with each item's **closure** (its package folder plus every file a JSON
  file of it names, followed through the material sidecars, as written or case-insensitively like the add-on's
  `resolve_path`), with size and xxh3-128 hash, each file hashed once.
- It fetches only the hashes it has neither in its **cache** nor on their way (`<work>/cas/`, hash to file, per run,
  gone at the run's end or when its head node drops), and links each render chunk's folder (`<work>/fetch/`) from the
  cache, laid out as on the holder, one subfolder per export chunk, so the paths in the export stay valid.
- No compression (90 % of the bytes are PNG textures).
- So **every node must reach every other one**: discovery does it in one network segment; with `peers` the nodes have
  to list each other. An item whose holder is not connected to its renderer goes back to the head node (counted) and
  fails with that reason after `MAX_REQUEUES`.
- Every render chunk renders with the head node's `render.py` and sends each PNG with its `icon:` line back; the head
  node files it like a local render.

**Failure handling:**
- A renderer drops: its render chunks' open items go back into the pool (their holder still has them). A holder
  drops: its exported items not rendered yet go back to be exported again, also those render chunks elsewhere were
  still fetching. An export chunk on a dropped node: its open items too.
- A renderer that cannot get an item's files reports it back before its chunk ends (`Unfetched`, never dropped
  silently). Its holder disconnected from it or is not connected to it: back into the pool for another renderer (never
  placed with that renderer again) while the head node still has the holder and some renderer has not reported it
  cannot reach it, else to be exported again (the exporter may then be a node it reaches). The holder no longer has a
  file (or it arrived damaged) or no longer holds the export chunk at all (restarted, released): exported again (only
  the items waiting for it from that holder; the same content from elsewhere or asked later is fetched as usual). Each
  of these counts once per incident.
- The holder keeps every held file per hash (a released chunk takes only its own; the same texture in another held
  chunk is still sent); an answer to a fetch request nobody waits for any more is dropped.
- A chunk without progress (no item reported) for 300 s (`CHUNK_TIMEOUT`) is cancelled on its node (an export chunk
  held there already is dropped with it) and its open items go back, the one it was stuck on to the back of the queue.
  Each item goes back at most twice (`MAX_REQUEUES`; a timeout counts only for the item it was stuck on, every item
  when it was stuck on none: an export that reported everything and never ended), then it fails with the reason.
- A render chunk still fetching its files gets 6 x 300 s (`FETCH_TIMEOUTS`; a slow link is no stall), then every one of
  its items counts.
- An `ExportDone` for an export chunk the head node gave up already releases it on its node.
- An item of a render chunk that the node itself held once but no longer holds (released, node restarted) goes back to
  be exported again, counted, like one whose holder answers that it does not hold the chunk.
- A chunk that fails on a node is deleted there like any other; why and its process's last lines go to the head node's
  `run.log`.

**Checks:**
- A decomposed export renders bit-identically from a moved folder (`scene.json` holds no absolute paths), also from
  Windows to Linux (same item, RMSE 0.05 %).
- Closure check (`closure_renders_like_full`, 45 items across armour, clothing, weapons, ship parts, ores, food,
  mobiGlas, 2026-11-04): each rendered from a folder holding only its closure is bit-identical to its render from the
  whole export folder (RMSE 0.0000 % for all 45; the same item without its largest texture differs, 0.38 %, so a
  missing file is seen); the 45 closures add up to 317 MB against 265 MB for the whole chunk.
- Discovery was checked on one PC against a script (beacon heard and dialed, own beacons sent); not yet across real
  nodes.

**Tests:**
- With fake tools (nodes listen on 127.0.0.1 ports from 47951-48199 handed out by `net::test_port()`, never 47950 and
  never ephemeral, each held from the check to the listen; no test sends discovery beacons): a start on another node
  with its status to the end and the selection afterwards (over the target list of its set), an abort from another
  node, the capacity check, the state against the head node, Data.p4k out of sync, the current hello for a connection
  accepted after a change, the DataCore tables failing in the background, target lists per set, the run statistics for
  the estimate, the views drawn incl. dialogs.
- With fake export-batch and Blender scripts (`chunk_tests`): exported on one node and rendered on another (the second
  render chunk takes the shared texture from the cache and fetches only its own files; folders and cache gone at the
  end), a holder dropping, a renderer dropping (open items rendered elsewhere, fetched from the holder again, nothing
  exported twice; its cache goes when the head node drops), timeouts with the requeue limit for an export and a render,
  an export that never ends after its last item, a late `ExportDone` released, a renderer not connected to the holder
  (the only renderer: exported again, then failed with the reason), a render chunk that never gets its files (cancelled
  after `FETCH_TIMEOUTS`, not before), the backlog, the long-lived processes (one Blender and one export-batch `--stdin`
  for every chunk of a run, a Blender restarted at its job limit in the middle of a chunk, idle ones gone after the
  run), a crash of either failing only the job in flight, a StarBreaker without `--stdin` and one that is not ready, the
  final status of `--plain` after its last tick, an assessment aborted here and on request, assessments on request.
- Unit tests: the pick (plateau, memory cap), the rate at the count a node runs now, the placement by rate, the report's
  slot use across nodes, the pruned assessment logs, staleness, the `packshot.json` merge and the chunk sizes by rate,
  a fetched run's `run.json` and a start's targets checked, a run not asked for refused, an unreachable item placed with
  another renderer or exported again and counted once; on a node: unsafe paths and hashes from a holder, a fetch answer
  from the wrong connection, unreachable holders still ending in `Fetched`, a holder without the chunk, a lost hash
  hitting only its request, a holder dropping reported back, a cancel dropping a held chunk, the Windows path checks
  (also for held items; a failed link does not block a case variant), the same texture in two held chunks after one is
  released, the packshot and work folders' locks.
- With real tools (`cargo test --release -- --ignored`, `SC_DATA_P4K`): a node exports a chunk, holds it, renders from
  it and drops it on release (an unknown entity fails in the export); a head node without capabilities hands the whole
  run to a node that exports and renders, and to an exporting node without Blender plus a rendering one that fetches
  from it; and the closure check.

## Compute assessment

([ADR 0019](adr/0019-compute-assessment-on-demand.md); `tools/packshot/src/assess.rs`.)

On demand only: `--assess` (this node, needs `--render`; prints the steps and the choice, saves it and exits) or `b` in
view 3 for the selected node (asks first; another node gets a request and runs it itself). Refused while a run is going
in the cluster, while the node works on chunks or assesses already. `b` on a node that is assessing asks to abort it
(the sweep's processes stop, the test set's export or fetch is cancelled and released, its requester is told).
Meanwhile the node reports `assessing` in its hello and gets no work.

**Test set:** 48 targets of the target set's list, stratified by type (largest remainder over the types' shares, evenly
spread within a type by class name: the same list gives the same set), exported once as one export chunk on this node
if it exports, else by a connected exporting node with the same state and fetched from it like a render chunk's files.
Kept in `<work>/assess/testset-<p4k>-<list>/` (the export under `x/`, `testset.json` with package, rig and pose per
item; 237 MB) and reused while the Data.p4k fingerprint and list are the same. An assessment that fails or is stopped
while getting it cancels and releases its export chunk on the exporting node and stops its fetch here. Nothing goes
into sets or runs; the log goes to `<work>/assess/<date>_<time>/assess.log` (the latest 5 such folders are kept).

**Render sweep:**
- An unmeasured warm-up (one Blender over the whole set: file cache, Blender's own caches; items failing there leave
  the set).
- Then n Blender at once for n = 1, 2, 3, 4, 6, 8, 10, 12, 16, ... (at most 32), long-lived and fed one job at a time as
  in a run. Each first renders one item unmeasured (its start-up and first shader compilations, which a run pays once
  per process), then all keep rendering the set round (rotated so they do not render the same item at once) until at
  least 8 images per process (and the whole set) have come in. items/s = those images / that window.
- The ladder stops when a step gains less than 5 % over the one before (or drops), when items fail, or when the next
  step would not fit RAM or VRAM (the most any step needed per process over what was in use before it; 10 % of each kept
  free).
- A sampler records every 0.5 s the CPU (all cores and the busiest one), GPU utilisation and used VRAM
  (`nvidia-smi -lms 500`, first GPU, Linux and Windows; empty columns without it) and used RAM. A step's loads are the
  means while all its processes were measured, VRAM and RAM the peaks.
- Workers = the smallest n within 5 % of the best, at most what fits in memory.

**Export sweep** (exporting nodes): the chosen n Blender keep rendering (fed the set round), an unmeasured export first
(Data.p4k into the file cache), then e = 1 ... 4 export-batch at once over one chunk each (with `--stdin` as in a run:
measured from the chunk going in to `chunk_done`, its start-up not counted; else the whole process). Exporters = the
smallest e within 5 % of the best min(export, render meanwhile) items/s, at most what fits RAM next to the chosen
Blenders with the most resident memory one export process took (`RAM fits <n> export` in the summary).

**Result** in its own file, `assessment.json` in the packshot folder (replaced as a whole; written to a temporary file
and renamed; `packshot.json` stays the cluster config only): workers, exporters, the items/s of render (on an exporting
node with its exports alongside) and export, every step (counts, items/s, render items/s meanwhile, start-up, load and
render phase, CPU all and busiest core, GPU mean and peak, VRAM, RAM, rises per process, failures, seconds), why the
ladder stopped, the memory caps (Blender, export), date, duration and the keys: host name, CPU, Blender version, GPU
name, `render.py` hash, add-on hash, StarBreaker commit. Host name and CPU are keys so that an `assessment.json` copied
to another machine is stale there (also one without `nvidia-smi`); the packshot build is not, since any source change
makes a new one without changing the load. A key that differs at start makes it stale: the formula counts again (the
start lines say why, view 3 shows `!`). After an assessment the node takes its counts at once unless
`--workers`/`--exporters` set them; `+`/`-` and `<`/`>` still change them. The hello carries the assessed items/s, the
counts they were measured with, every step's count and items/s and whether they are valid; the head node sizes the
chunks by them.

Example (2026-11-04, Ryzen 9800X3D / RTX 5080 node, long-lived Blenders, 2.7 min in all): n = 1/2/3/4/6/8 gave
5.97/10.15/12.86/13.98/15.66/15.99 items/s (+2 % at 8: stop), CPU 33/54/68/76/87/92 %, start-up 0.35-0.53 s once per
process, load phase 0.08 to 0.34 s, render phase 0.07 to 0.12 s; with 6 Blender 1/2/3/4 exports made 5.3/10.8/15.4/19.8
items/s with the render at 15.0-15.6: 6 render, 3 export. Earlier, on tmpfs and a Blender per chunk, start-up counted:
n = 1/2/3/4/6/8 gave 4.1/6.5/8.2/8.9/9.6/10.0 items/s, GPU 7-18 %, 1.2 GB VRAM and 1.8 GB RAM per Blender: the CPU
saturates, the GPU does not. The rates are the test set's (a mix); what matters for the chunks is how the nodes compare.

## Builds and bundles

([ADR 0012](adr/0012-bundles-per-os.md).)

**packshot:** builds for Windows (`cargo build --release`) and as a static Linux binary
(`--target x86_64-unknown-linux-musl`, linked by Rust's own `rust-lld` per `tools/packshot/.cargo/config.toml`, no C
toolchain needed; static PIE without interpreter or shared libraries; the embedded RDSEED shim is built the same way, so
the bundles need nothing extra). Windows exes also build on Linux with mingw (`--target x86_64-pc-windows-gnu`,
`mingw64-gcc` as linker; they need only Windows' own DLLs).

**StarBreaker:** `scripts/starbreaker-build.{sh,ps1} [--platform all|windows|linux]` builds the fork's committed HEAD
(`git archive`, so the working tree does not matter) into `local/starbreaker-ps/` with one `COMMIT`. Windows natively or
from Linux with mingw; Linux with cargo-zigbuild against glibc 2.31 (the binary needs glibc 2.30: Ubuntu 20.04 and later;
a build on a current Fedora-based distro needed 2.43, more than Ubuntu 24.04's 2.39). Same export time as a native glibc
build (20 armour pieces 2.70 s each, identical output). A musl build would run anywhere but exported 58 % slower (4.3
against 2.7 s, musl's allocator), and the export is the limit.

**Blender:** on Linux `local/blender/blender-5.2.3-linux-x64` (the official tarball) with `portable/config` from the
Windows one and the add-on as a relative symlink into the fork (ntfs3 cannot follow the Windows junction).

**`scripts/packshot-bundle.sh [--platform all|windows|linux] [--out <dir>]`** (Linux) writes both bundles; the `.ps1`
builds the same two on Windows (the Linux StarBreaker in `local/starbreaker-ps/` must have been built on Linux).
- `windows`: packshot.exe from mingw, StarBreaker with `COMMIT`, the official Blender zip checked against its
  `blender-<ver>.sha256` and left packed, `portable/` with config and the add-on copied from the fork, `cli/render.py`,
  `targets.json`, `setup.cmd` (unpacks the zip into `blender\` with Windows' own `tar` and puts `portable/` into it),
  `run.cmd` (refuses `--render` without it), `README.txt`, `packshot.example.json`; texts with CRLF; ~425 MB, unpacked
  ~920 MB.
- `linux`: the static binary, the Linux StarBreaker with `COMMIT` (printing the glibc it needs), the official Blender
  tarball from download.blender.org checked against its `.sha256` and left packed so symlinks and exec bits survive, the
  same `portable/` with the add-on as real files, `cli/render.py`, `targets.json`, `setup.sh` (unpack and install),
  `check.sh` (tests the add-on and a headless EEVEE render on Vulkan), `run.sh` (passes the capabilities on:
  `./run.sh --render --export`), `README.txt`, `packshot.example.json`.
- The templates live in `tools/packshot/bundle/`.
- Data.p4k and `build_manifest.id` go next to the bundle on exporting nodes.

**Checked:** 2026-11-03 with the bundles of then (node-only Linux bundle, unpacked Windows Blender): both on their
nodes, and on an Ubuntu 24.04 server without a desktop (RTX 3090, full NVIDIA driver from `ubuntu-drivers install`; the
`--gpgpu` variant lacks `libnvidia-gl` and with it Vulkan) `check.sh` passed: the add-on loads and EEVEE renders
headless on Vulkan, no display server needed.

**Not yet checked on a node:** the Windows bundle with its zip and `setup.cmd` (built and its unpack steps simulated
with bsdtar on Linux only), the ps1 scripts (no pwsh on Linux), the zigbuild StarBreaker on a node.

## Measurements

Reference set: 459 `_arms_` entities unless noted; Ryzen 9800X3D (8 cores) / RTX 5080 node.

- **Before packshot** (single CLI loop, 89 entities, one export and one Blender at a time, 2026-11-01): 0.17 s export
  and 0.55 s render per entity; the full run of the 3793 gear targets took 52 min on a 4-core i7-4790K node (the first
  one, single exports, 4 h).
- **OpenGL, 2026-11-02:** 1 Blender 209 s, 4 Blender 97 s, 6 Blender 98 s, 8 Blender (3 exports) 94 s; a render takes
  0.45 s alone and 0.65/0.82/1.00 s with 4/6/8 in parallel. CPU is the limit: with 8 Blender it sits at 90-100 % through
  the render phase, the GPU averages ~15 %, the disk ~2 %. Smaller blocks (~3 per Blender) did not help (4 Blender
  109 s).
- **Phases, same set:** alone load 0.19, prep 0.01, render 0.12, glass 0.00, post 0.02 s; with 8 Blender load 0.28 and
  render 0.67 s. The render phase grows under load (5.5x) while the GPU idles: most likely shader compilation on the CPU.
- **Vulkan** removed most of that (2026-11-02, fixed set of 88 armour pieces, Blender directly): with 8 Blender the
  render phase takes 0.30 s against 0.86 s on OpenGL (Blender 5.2's default here), 114 s against 160 s for 704 renders;
  alone Vulkan is slightly slower (0.29 against 0.21 s). Images match (mean deviation 0.04/255, at most 0.08 % of the
  pixels off by more than 16: edge sampling). Not helpful: a second open of the same items in one process (no shader
  reuse across files, render 0.14 against 0.15 s) and OpenGL with `shader_compilation_method = 'SUBPROCESS'` (211 s).
- **Vulkan scaling** (items/s): 1 -> 1.3, 4 -> 5.3, 6 -> 6.3, 8 -> 6.2, 12 -> 6.7; hence 3/4 of the physical cores.
- **Whole pipeline:** 6 Blender and 2 exports 78 s (OpenGL with 4 Blender: 97 s). Restricted to 4 cores by CPU affinity
  (as on the i7-4790K node): 3 Blender with 1 export 101 s, 4 Blender the same (the single export is the limit), 3
  Blender with 2 exports 80 s; hence at least 2 exports.
- **Load phase:** under Vulkan it is the larger share (0.25 against render 0.23 s with 6 Blender). A cProfile over 30
  items put ~0.11 s of each load into the add-on's `PackageBundle.resolve_path`, which built its case-insensitive path
  index (an `rglob` over the whole block's export root, ~37 000 `stat` calls) before every lookup. The fork now tries
  every candidate directly and builds the index only when none exists: same 88 items, 8 Blender, warm caches: load
  0.85 to 0.73 s, 104 to 94 s; images identical (at most 1/255). The rest of the load is the node-tree build itself
  (~0.2 s per item alone).
- **Cluster loopback, 2026-11-04** (6 Blender, 2 exports, warm caches): alone 58.6 s against 59.4 s with the blocks
  before; as two processes on the same PC (head node exporting only, the other rendering only): 59.2 s, 45 render
  chunks fetched 806 MB and took 203 MB from the cache (20 % deduplicated), 0.15 s per fetch over loopback. Not yet run
  across more than a few real nodes.
- **Long-lived Blenders against one per render chunk** (same PC and set, `--plain`, warm caches, interleaved runs on a
  quiet machine, 2026-11-04, export-batch still per chunk): with 2 exports (the export is the limit) 42.4/42.7/44.8 s
  against 44.9/45.7/45.7 s; with 4 exports 38.3/38.6 s against 43.1/43.1 s (10.7 to 11.9 items/s, +12 %); a render
  chunk's first item costs 0.15 s more than the others instead of 0.6 s. Images: 28 of 29 varied items (armour,
  clothing, weapons, ship parts, food, a flight controller) bit-identical between `--jobs` and `--stdin` in the same
  order; the 29th differs as much between two `--jobs` runs (mean 0.004/255).
- **First multi-node run** (2026-11-03, 6734 items in 949 s, 6 Blender and 2 exports on the main PC plus a 4-core
  remote node, with blocks) showed the export as the limit: 2 export-batch processes made ~7.3 items/s, no exported
  block was ever waiting, the local Blender were 3.7 of 6 busy, and the remote node got only 6 blocks (161 items,
  2.4 %) because a ready block went to a free local Blender first. With item packages (exported and rendered on the
  node, until 2026-11-04) the 4-core node got 5 packages of 50 at once, needed ~63 s to export one and then stopped
  responding (packages were then held to 20 items). These led to the chunk and pool design.
- **RDSEED shim and threads:** see [Processes and the RDSEED shim](#processes-and-the-rdseed-shim).
- **Assessment:** see [Compute assessment](#compute-assessment).
- **Tone curve probe and calibration:** see [Render](#render).

## Known failures

52 of 6734 entities failed in the last full run (2026-11-01); not fixable on the renderer's side:
- 23 `file not found in P4k: Data\m`: the record names no geometry at all (every `path` of its
  `SGeometryResourceParams` empty; the message is the exporter's and misleading). Ship parts (a cooler, 890J/Idris
  parts) but also some food boxes, a fire extinguisher magazine, a spewgun magazine, a trouser.
- 15 whose geometry file is missing from `Data.p4k` (a consumable, a cutter, a pistol, cargo pods, several `MRCK_*`).
- 11 without a geometry component (`Flair_*` models, docking tubes, a flight controller).
- 3 ship-built racks whose scene has no mesh.

Ship-only parts are neither bought nor kept in an inventory, so they are left out like the turrets' paint-tag variants,
`Default_Fixed_Mount_S4` (no material) and the thrusters. `sc_nvy_medical_corpsman_boots_02` renders its shoe box:
`Data.p4k` holds only its textures (Squadron 42 content), the box is its default geometry.

Other failure reasons you may see: paths over 259 characters or case-colliding paths (Windows), `Blender ended while
rendering it`, `export-batch ended while exporting it`, a chunk timeout, `MAX_REQUEUES` exceeded (see
[Cluster](#cluster)).

## References for review

- **The game's own tiles:** a consumer with a screen reader can supply frames of the game's inventory and warehouse
  tiles; they are ~100 px at 1080p: good for colour, brightness and pose, not for fine detail. A hovered warehouse tile
  is covered by its tooltip: take the same grid cell from a later frame where the cursor has moved on.
- **Wiki item pages** (starcitizen.tools) carry in-game screenshots of the worn item, good for decals and colours.
- Fixing pose, light or material, then `--filter <class> --scope all`, into an experiment set first (`--set <name>`),
  compare, then adopt into live.

## Open and unproven

Open:
- In-game comparison for the types without an orientation entry (radar, EMP, QDMP, tractor beam, container, shield).
- The `requiredTag` entries (sidearms get the primary weapons rig).
- Paint variants sharing a model are rendered separately.

Unproven:
- The palette-metal slot tint is fitted to wiki shots (the calibration scatters by about 2x, no shader source in
  `Data.p4k`).
- One item's ochre blossoms stay more ochre in game than through the Khronos spec-to-metal solve.
- POM decals are not ray-marched (<= 5 mm, below a pixel at 256 px; matters for larger renders).
- Studio values and contrast were chosen by eye on 19 items (2026-11-02); dark coloured surfaces keep their hue but end
  up very dark (one chin sRGB about 5).
- Silicon's wear flecks render brighter than in game (its wear layer is a light metal).
- Visors are darker than in the tiles (`%THIN_FILM` not drawn).

Shader facts from `Engine/ShaderCache_D3D11.pak` (`Shaders/CFXIB/D3D11/LayerBlend_V2.cfxb`, decoded 2026-11-02, not
built yet):
- A LayerBlend_V2 `Glow` lights base layer 1 by the blend map (`Emissive += Glow x layer-1 albedo x blend R`,
  `EmissiveMode` 0; the `.mtl`'s `Emissive` colour is not read), so 416 character materials with `Glow` and a black
  `Emissive` draw unlit, and the flat-layer rule above takes `Emissive` where the shader takes layer 1.
- TexSlot13 (`HueAoLumMap`, "HAL Map") is hue shift / AO / luminance on the albedo, not a glow mask (its red channel is
  a flat 0.5 with BC1 noise).
- A green band along one visor rim in the game tile is not explained by `Glow` (layer 1 has no weight on its padding);
  likely the visor (tinted glass, `%THIN_FILM`).
- Harmless for pristine images and therefore not read: the extra `IVOCOLORS2` stream of 290 helmet skins (wear/dirt-like)
  and the main colour's alpha (255 - submaterial index).

## Consumers: sc-quartermaster

sc-quartermaster (the org tool packshot was built for) is only a consumer. It feeds in:
- **`targets.json`**, written from its database (`pnpm packshot:targets` there): a list of targets with `uuids` (the
  consumer's ids; further UUIDs of one entity become hard links), `names` (optional, one English display name per UUID), `entity` (class name),
  `type`, `subType` and `worn` (armour and clothing render worn). Gear items, harvestables, ore chunks and shop items
  make up the 6734 targets; the consumer's selection rules stay on its side.

It reads:
- **`sets/live/`**: the PNG per UUID (`<Name> (<class>) <uuid>.png`) is turned into AVIF thumbnails by its own tool
  (`pnpm thumbs:build` there; [ADR 0027](adr/0027-output-formats-thumbnails-are-users.md)). packshot knows nothing about
  thumbnails.
