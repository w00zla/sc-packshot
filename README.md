<p align="center"><img src="docs/assets/banner.jpg" alt="packshot" width="800"></p>

<h1 align="center">packshot</h1>

<p align="center">
  <a href="https://github.com/w00zla/sc-packshot/actions/workflows/test.yml"><img src="https://github.com/w00zla/sc-packshot/actions/workflows/test.yml/badge.svg?event=pull_request" alt="Tests"></a>
  <a href="https://github.com/w00zla/sc-packshot/releases"><img src="https://img.shields.io/github/v/release/w00zla/sc-packshot?color=54adf7" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-54adf7" alt="License: MIT"></a>
  <a href="Cargo.toml"><img src="https://img.shields.io/badge/rust-1.89%2B-54adf7?logo=rust" alt="Rust 1.89+"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-54adf7" alt="Windows | Linux">
</p>

<p align="center">
  Pictures of Star Citizen items from your own copy of the game: packshot exports each entity from
  <code>Data.p4k</code> with <a href="https://github.com/w00zla/starbreaker-ps">starbreaker-ps</a> (a StarBreaker fork)
  and renders it in Blender, driven from a terminal UI. It started as the icon renderer of the Quartermaster org tool.
</p>

- **Bulk:** thousands of entities per run; many long-lived export and Blender processes at once, fed one job per line.
- **One PC or a cluster:** nodes find each other on the LAN, any node can start a run, files move by content hash.
- **The game's own staging:** pose and light rig per item type (and tag) from the game's inventory data.
- **Your own look:** a profile (parameters plus four Python hooks) decides materials, scene and output: PNG, JPEG, EXR.
- **Runs and sets:** every run is kept as it was; sets pick the newest image per entity, experiments branch off.
- **Measured, not guessed:** an assessment finds how many processes a node carries; `--report` reads a run's log.

## Requirements

- Windows 10/11 or Linux, a GPU with Vulkan (rendering).
- For exporting and the target picker: the game's `Data.p4k` with its `build_manifest.id` next to it.
- Internet once for the setup (it fetches Blender), or Blender's official archive put into the bundle folder (next to
  `setup.cmd` / `setup.sh`).
- To build from source: Rust 1.89+ and git.

## Use

Download the bundle for your OS from the [releases](https://github.com/w00zla/sc-packshot/releases) and unpack it
anywhere. It brings packshot, StarBreaker with its Blender add-on, `render.py` and the bundled profiles; the setup adds
Blender (from blender.org, or the official archive you put next to it).

```bat
setup.cmd                     :: Blender and the add-on (Linux: bash setup.sh)
check.cmd                     :: the add-on loads, Eevee renders on Vulkan (Linux: bash check.sh)
run.cmd --render --export     :: the TUI; --help lists everything (Linux: ./run.sh)
```

On Linux run the check first: a headless Blender depends on the GPU driver and its Vulkan setup.

- `--render` lets the node run Blender, `--export` StarBreaker (it needs `Data.p4k`); with neither it only starts,
  stores and watches runs.
- The TUI always opens: a **Start** tile in the Run view shows what is missing and what to do, and turns off only what
  needs it. In a bundle, `run.cmd` / `run.sh` refuse `--render` until the setup has run.
- Starting a run asks for its name, its [profile](#profiles) (the look; `default` mimics the game's inventory tiles)
  and, while the set has none, a [target list](#target-lists). Size and format belong to the set.
- Several PCs can work on one run as a **cluster** ([Cluster](#cluster)).
- Process counts follow the node's measured **assessment** (`--assess` or `b`), else a share of its cores.

## The terminal UI

Three views, switched with `1` `2` `3` or `Tab`; the last line always lists the keys that work right now. In every
view `e` switches between log and errors, `a` aborts the run going on, and `q` or `Esc` quits (while a run is going
here it asks first: quitting aborts it).

### Run

<p align="center"><a href="docs/assets/tui-run.png"><img src="docs/assets/tui-run.png" alt="The Run view: a run on three nodes with its workers, throughput, pool and the latest render" width="600"></a><br><sub>click for full size</sub></p>

The selection to render (head node, target set, filter, scope), or the run going on: progress and ETA, every worker on
every node with the entity it is on, throughput and the pool of exported items waiting for a Blender, the latest
render.

| Key | |
|---|---|
| `n` · `s` | head node · target set for the next run |
| `/` · `x` · `u` | filter · clear it · scope (missing, failed, all) |
| `Enter` | start a run: asks for its name and profile |
| `+` `-` · `<` `>` | more or fewer Blender · export processes on this node |
| `p` | pause the run |
| `←` `→` | browse the latest renders |

### Sets

<p align="center"><a href="docs/assets/tui-sets.png"><img src="docs/assets/tui-sets.png" alt="The Sets view: sets with their runs, a finished run's tile and images" width="600"></a><br><sub>click for full size</sub></p>

The sets with their runs, newest first; per set its size and format, per run its profile. On the right the selected
set's or run's details and the run's images.

| Key | |
|---|---|
| `Enter` | make the selected set the target set |
| `n` · `t` | new set (branches off `main`; asks for its size and format) · its target list (a file, or the target picker) |
| `l` | adopt an experiment into `main` |
| `x` | delete a run or a set |

### Nodes

<p align="center"><a href="docs/assets/tui-nodes.png"><img src="docs/assets/tui-nodes.png" alt="The Nodes view: the nodes with their workers and assessment, one node's details, the next run" width="600"></a><br><sub>click for full size</sub></p>

Every node with its workers and assessed speed, its share of the run, runs to fetch, free disk and whether its state
matches; below the selected node's details and where the next run would start.

| Key | |
|---|---|
| `b` | assess the selected node (again: abort) |
| `h` | fetch the selected node's runs that are missing here and append them to `main` |
| `m` | fetch its `main` manifest, so "missing" counts what it has (on this node: drop it) |
| `r` | ask the node for its run list again |

### Images in the terminal

Renders show in full on terminals with Sixel, Kitty or iTerm2 graphics (Windows Terminal, WezTerm, kitty, iTerm2, …),
else as half blocks. packshot detects the protocol itself; to set one, give the environment variable
`PACKSHOT_GRAPHICS` one of `sixel`, `kitty`, `iterm2` or `halfblocks`.

## How it works

<p align="center"><a href="docs/assets/arch-overview.png"><img src="docs/assets/arch-overview.png" alt="Data flow: Data.p4k through StarBreaker and Blender into a run and its set, driven by packshot" width="600"></a></p>

packshot renders nothing itself. It cuts the selected targets into chunks and hands them to long-lived processes:

| Stage | Process | Result |
|---|---|---|
| Export | `starbreaker entity export-batch --stdin`, one per export slot | a decomposed package per entity: `scene.blend`, meshes, PNG textures, material sidecars |
| Render | `blender -b --python render.py -- --stdin`, one per render slot | the add-on builds the materials, the profile stages and renders one image per entity |
| Store | packshot | the image in the run's folder, hard-linked into the target set |

Between the stages every exported item waits in the **pool** on the node that exported it; a render chunk fetches
its files from there. Details: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### StarBreaker

Export and materials come from [starbreaker-ps](https://github.com/w00zla/starbreaker-ps), a fork of
[StarBreaker](https://github.com/diogotr7/StarBreaker) made for packshot:

| Part | What packshot gets |
|---|---|
| `entity export-batch --stdin` | a long-lived exporter: loads `Data.p4k` and the DataCore once, then exports one entity per stdin line; a job that fails or panics is answered as a failure and the process goes on |
| `dcb query --ids` | the entity list for the target picker (record id, class name, item type and subtype) and the entities' tags, each in about a second |
| `dcb query`, `dcb extract` | the inventory's pose and light tables (`PlayerChoice_PITConfig`) and its light groups |
| `p4k extract` | the English `global.ini`, for the entities' display names in the picker |
| Blender add-on | builds Blender's material node trees from the package's sidecars: layer blends, tint palettes, decals, glass, glow |

packshot pins a release of the fork in `deps.toml`.

### Blender

Every render slot runs a portable Blender 5.2.3 (nothing in the user profile).

| | |
|---|---|
| Renderer | Eevee on **Vulkan**, transparent film, at the set's size and format (`main`: 256 px PNG) |
| Staging | pose and light rig from the game's inventory data per item type and tag, the item scaled to 0.7 m |
| Look | (`default`) its own tone curve, a studio world, boosted emission with bloom, light behind glass |

### Runs and sets

| | |
|---|---|
| **Run** | `runs/<date>_<time>_<name>/`: the images (`<Name> (<class>) <uuid>.png`), `run.json`, `events.jsonl`, `run.log`; never changed after its end |
| **Set** | `sets/<name>/`: per entity a hard link to the newest image in its chain of runs, plus its own `targets.json`; one size and one format (PNG, JPEG, or linear EXR without tone curve), fixed when the set is created |
| **main** | the set consumers read; an experiment set branches off it and is adopted by appending its runs |

Everything packshot writes lives in one folder: `local/packshot/` in a checkout, the bundle's own folder in a bundle.
`packshot --report` evaluates a run's event log: throughput, phase timings, slowest entities.

### Target lists

A target list says which entities a run renders; every set keeps its own copy. The lists live in `targets/` of the
packshot folder (subfolders are fine); `t` in the Sets view lists every list there, `targets/targets.json` first, and
offers the target picker.

The **target picker** ("From Data.p4k / DataCore …") builds one from the game's entities: the ticked types (none
ticked: all), narrowed by the search, make the list. Its entity list is read once per game build (a few seconds) and
cached in the work folder.

| Key | |
|---|---|
| typing | search: class names with `*`, display names, types; terms are or'ed. Several exact class names pasted make exactly that list (a fixed sample to compare builds) |
| `Tab` | switches between search and type tree |
| `Space` · `+` · `-` | in the tree: tick a type or subtype · tick every type shown · clear all ticks |
| `→` · `←` | unfold · fold a type's subtypes |
| `Enter` | saves the list as `targets/<name>.json` and takes it as the set's list |
| `Esc` | cancels the picker |

The file is a JSON array, one object per entity; other tools can write it, packshot reads only these fields:

| Field | |
|---|---|
| `uuids` | one or more ids; the first names the image file, further ones become hard links to it |
| `names` | optional, one display name per id (shown in the TUI and the image file name) |
| `entity` | the entity's class name in the DataCore (`behr_rifle_ballistic_01`) |
| `type` · `subType` | the item type and subtype (`WeaponPersonal` · `Medium`); with the entity's tags (read from the DataCore) they pick pose and light rig |
| `worn` | optional, `true` renders armour and clothing worn instead of as their loot crate |

```json
[{ "uuids": ["02d4cd2e-fa98-4086-aee1-6b2dfce8ea27"], "names": ["P4-AR Rifle"], "entity": "behr_rifle_ballistic_01",
   "type": "WeaponPersonal", "subType": "Medium", "worn": false }]
```

## Profiles

A profile is a folder with two files. Anything it does not set falls back to `default`, the profile packshot comes with:

| File | |
|---|---|
| `profile.toml` | `[params]` and `[addon]` (below); optionally `scene = "<file>.blend"`, whose world, cameras and lights `stage` may take |
| `hooks.py` | up to four functions (below), each taking a context `ctx` with the job, the parameters and the scene |

A profile's parameters lie over `default`'s; size and format are not among them, they are the set's:

| Parameter | `default` | |
|---|---|---|
| `samples` | `16` | Eevee render samples |
| `quality` | `90` | JPEG quality, 0 to 100 |
| `background` | `[0.0, 0.0, 0.0]` | JPEG: the colour under the item, sRGB `[r, g, b]` from 0 to 1 |
| `fit` | `0.7` | with a game pose the item is scaled so its largest edge is this many metres (the game's light rig is made for that) |
| `margin` | `1.06` | room around the item in the image (`1.0`: it touches the edges) |
| `light_scale` | `30.0` | watts per intensity unit of the game's lights |
| `mirror` | `false` | the camera from the other side (a mirrored image) |
| `az` · `el` | `20.0` · `15.0` | without a game pose: camera azimuth and elevation in degrees |

`[addon]` sets up the StarBreaker add-on: `override` names a copy of the add-on in the profile folder, used instead
of the default one; every UPPER_CASE key sets one of its look knobs (below). Unknown names and wrong types are refused
before a run; `false` unsets a knob that may be None.

```toml
[addon]
POM_SCALE_MULTIPLIER = 45.0
SKIN_SUBSURFACE_RADIUS = [1.0, 0.4, 0.25]
```

The look knobs of the add-on (StarBreaker 0.3.2-ps.6), with their defaults:

| Knob | Default | |
|---|---|---|
| `MONITOR_ROUGHNESS` | `0.35` | roughness of monitor-like surfaces (screens, eyeballs); 0.15 mirror-like, 0.6 matt |
| `LAYER_BLEND_GLOW_TINT_WEIGHT` | `0.5` | colour of a glowing layer: 0 its emissive colour, 1 the layer's tint, between a mix |
| `ORE_VEIN_SMOOTHSTEP` | `[0.6, 0.8]` | the range of an ore's vein map that turns rock into vein (lower: more vein) |
| `ORGANIC_TINT_GAIN` | `2.0` | brightness of organic materials (minerals such as Hadanite, Beradom) |
| `METAL_TINT_HUE_ONLY_BELOW` | `0.1` | a metal tint darker than this keeps only its hue in the reflection, a brighter one scales it |
| `LAYER_BLEND_WEAR_MASK_GAIN` | `4.0` | how strongly the wear mask hides layered materials' edge wear; `false`: not at all, the authored edge wear shows |
| `WEAR_STRENGTH_OVERRIDE` | none | wear of two-layer wear materials (e.g. some weapons), 0 to 2; unset or `false`: pristine |
| `POM_SCALE_MULTIPLIER` | `30.0` | parallax (POM) depth: the material's displacement times this |
| `POM_SCALE_MAX` | `3.0` | the cap on that depth (higher makes the surface swim) |
| `LIGHT_VISUAL_GAIN` | `20.0` | brightness of the item's own lights |
| `HEADLIGHT_GOBO_THROW_GAIN` | `10.0` | brightness of headlights with a projector texture |
| `SKIN_SUBSURFACE_RADIUS` | `[1.0, 0.35, 0.2]` | skin: how far red, green and blue scatter under the surface, relative |
| `SKIN_SUBSURFACE_SCALE` | `0.004` | skin: the scatter distance in metres |
| `SKIN_TRANSMITTANCE_SCALE_GAIN` | `4.0` | skin: how much thin parts (ears, nostrils) widen the scatter |
| `HAIR_DIFFUSE_SCALE` | `0.5` | hair: the share of the hair colour in its diffuse light |

Hooks are Python functions that `render.py` calls at four points of every render; each may change the scene. A profile
defines only the ones it needs, the others come from `default`:

| Hook | When | `default` does |
|---|---|---|
| `before_open(ctx)` | before the item's file opens, after `[addon]`'s knobs | nothing (the place to replace the add-on's methods) |
| `after_open(ctx)` | after the add-on built the materials | sharper gloss on metal and glass, stronger emission, hides empty UI screens |
| `stage(ctx)` | then | the game's pose, the camera fit, the game's light rig, a studio world, Eevee settings |
| `post(ctx, exr_paths)` | after the render (a linear EXR) | bloom, contrast, tone curve; writes the image and returns its files |

A profile for JPEGs on white needs only a `profile.toml` (rendered into a set made for JPEG):

```toml
[params]
background = [1.0, 1.0, 1.0]
```

Your own profiles go into `profiles/` of the packshot folder (in a bundle: the bundle's `profiles/`); `default` is
reserved. Every run records its profile and the profile's hash.

## Cluster

Several PCs can work on one run together. To set it up, on every node:

- copy `packshot.example.json` (in a checkout: `templates/bundle/`) to `packshot.json` in the packshot folder and set
  the same `token` everywhere;
- open port 47950 (TCP and UDP) on the LAN; nodes on another subnet go into `peers`.

The nodes then find each other and show up in the Nodes view. Any node can start a run, on itself or on another node,
which then holds it as the **head node** and hands out the work; one run at a time per cluster. What a run renders
(target set, filter, scope, profile) is the starter's choice.

All nodes need the same packshot, Blender, add-on and StarBreaker, the exporting ones the same `Data.p4k`; otherwise
no run starts and the TUI shows what differs.

## Develop

| Path | What |
|---|---|
| `src/` | the orchestrator (Rust): selection, scheduler, cluster, runs and sets, assessment, TUI |
| `blender/render.py` | runs inside Blender: opens the item, calls the profile's hooks, renders |
| `profiles/` | the bundled profile `default` |
| `shim/` | RDSEED shim for Blender on Linux |
| `templates/bundle/` | templates of the per-OS bundles |
| `deps.toml` | the pinned Blender and StarBreaker versions |
| `scripts/` | `setup.*` (a dev checkout), `starbreaker-build.*` (the fork), `packshot-bundle.*` (the bundles) |
| `docs/` | [`ARCHITECTURE.md`](docs/ARCHITECTURE.md), `releases/` (release notes), `assets/` (images and their sources) |

The setup and the bundles follow the pins in `deps.toml`:

```powershell
scripts/setup.ps1                              # Blender, add-on and StarBreaker from the pinned release (Linux: scripts/setup.sh)
scripts/setup.ps1 -FromSource                  # the same with the fork next to the repo: add-on linked into it, StarBreaker built (Linux: --from-source)
cargo run --release -- --render --export       # store in local/packshot/, Blender in local/blender/
cargo test --release                           # real-tool tests are #[ignore] and need SC_DATA_P4K
scripts/packshot-bundle.ps1                    # both bundles into local/packshot-bundle/ (Linux: scripts/packshot-bundle.sh)
```

To move to another Blender or StarBreaker: change `deps.toml`, run the setup again, rebuild the bundles. Building the
fork itself needs zig and cargo-zigbuild for its Linux binary. Changes land on `main` through pull requests.

## Security

In a cluster every rendering node runs Python it receives (`render.py`, profiles). Anyone with the token can make every
node run code, so share a cluster only with machines and people you trust.

## Game data and fan content

This repository contains no game data; packshot reads your own copy of the game.

packshot is an unofficial Star Citizen fan project, not endorsed by or affiliated with the Cloud Imperium group of
companies. Game content and materials, including everything packshot renders, are copyright Cloud Imperium Rights LLC
and Cloud Imperium Rights Ltd. Star Citizen® is a registered trademark of Cloud Imperium Rights LLC.

## Credits

[StarBreaker](https://github.com/diogotr7/StarBreaker) by Diogo Trindade and the
[fleetyards fork](https://github.com/fleetyards/StarBreaker) it builds on; [Blender](https://www.blender.org/).

## License

MIT, see [LICENSE](LICENSE).
