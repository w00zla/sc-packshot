# 0044. The profile is chosen per run, not per set

- Status: accepted; superseded in part by [0045](0045-size-and-format-per-set.md) (size and format are the set's, not chosen with the run)
- Date: 2026-11-07
- Scope: profiles ([0022](0022-modular-profiles.md), [0042](0042-profile-folder-and-hook-signatures.md)), runs and sets
  ([0011](0011-runs-and-sets.md)), distribution ([0024](0024-head-node-distributes-profiles.md))
- Supersedes in part: [0022](0022-modular-profiles.md) ("a set is a target list plus a profile")

## Context
0042 left open where profiles are stored and how a set names its profile. Binding a profile to a set would need a set
field, its inheritance on branching and a rule for changing it; choosing it per run is simpler, and a set already
records which run each of its images came from.

## Decision
- A run renders with one profile: `--profile <name|folder>` (default `game-tile`). The run record and `run_start`
  carry its name, its content hash (over the profile's folder and game-tile's, whose hooks are the defaults) and the
  size and format used; the Sets view's runs table shows it.
- A set may hold images of different profiles; per target the newest run's image wins, as before, also across
  formats (a newer `.jpg` replaces an older `.png`).
- Profiles live in `profiles/<name>/` next to the tool (repository and bundle; `game-tile` is bundled) and in
  `<store>/profiles/<name>/`; one of the store's hides a bundled one of the same name. A store profile may not be
  called `game-tile`: it travels next to the bundled one.
- The profile is chosen when a run starts (TUI: after its name; console: `--profile`). The starter decides, not the
  head node, as with the target list: a run started on another node carries the starter's profile with its files;
  render.py and game-tile come from the head node ([0038](0038-render-script-distributed-not-compared.md)).
- The head node sends render.py, game-tile and the run's profile with every render chunk; a node keeps them in
  `<work>/profile-cache/<hash>/` while it runs (cleared at start), and a Blender process belongs to one such hash.
- A profile may bring its own add-on copy (`addon` in profile.toml, [0025](0025-material-module.md)); the process then
  uses it instead of the installed add-on. The run record names the add-on actually used.

## Alternatives
- Profile per set (0022 as written, a `profile` field in `set.json` inherited on branching): one more piece of set
  state for a rule a run record already gives.

## Consequences
- Whether a set is all one look is visible only from its runs, not enforced.
- The equality check ([0014](0014-same-state-or-no-run.md)) still compares the installed add-on, not a profile's copy.
