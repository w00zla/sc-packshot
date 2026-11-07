# 0044. The profile is chosen per run, not per set

- Status: accepted
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
- The head node sends render.py, game-tile and the run's profile with every render chunk; a node keeps them in
  `<work>/profile-cache/<hash>/` while it runs (cleared at start), and a Blender process belongs to one such hash.

## Alternatives
- Profile per set (0022 as written, a `profile` field in `set.json` inherited on branching): one more piece of set
  state for a rule a run record already gives.

## Consequences
- Whether a set is all one look is visible only from its runs, not enforced.
- A run another node starts on this head node uses the head node's `--profile`; the start message carries none yet.
- Choosing the profile in the TUI per run is not built; it is a start option today.
