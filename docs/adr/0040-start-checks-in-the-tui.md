# 0040. Start checks run inside the TUI

- Status: accepted
- Date: 2026-11-07
- Scope: start-up (`main.rs`), TUI, DataCore tables and entity list

## Context
Before the TUI opens, packshot looks for Blender, StarBreaker, `Data.p4k` and its `build_manifest.id`, stops with a
console message when one is missing and prints set, cluster and capability lines; only the DataCore tables load
later, inside the TUI. A missing file means: read the message, fix, start again. The target picker
([0039](0039-target-lists-owned-by-packshot.md)) adds one more DataCore step (the entity list).

## Decision
- The TUI always opens. A start tile in view 1 (in place of the worker list) lists each step with its state (done,
  running with time, waiting, failed, skipped): Blender (version, pin), StarBreaker (version, pin), `Data.p4k` (game
  version from `build_manifest.id`), DataCore tables, entity list. A failed step says what to do; after the fix
  packshot is started again. Once every step is done the tile goes; its lines stay in the log.
- A missing piece turns off only what needs it (no Blender: this node does not render; no `Data.p4k`: it does not
  export); sets and nodes work as usual. Paths and capabilities do not change while packshot runs.
- The set, target-list and cluster lines printed before the TUI today go to the log only.
- The entity list is cached per game build (`<work>/dcb/entities-<p4k fingerprint>.json`) and read with
  `starbreaker dcb query --ids` (starbreaker-ps 0.3.2-ps.4 or newer).
- `--plain`, `--serve`, `--dry-run`, `--assess` and `--report` keep stopping with a console message: there is no TUI to
  show it.

## Alternatives
- Only tidy the console output: the message is gone once the TUI takes the screen.
- A key that checks again without a restart: paths, render/export capability, the cluster hello, Blender slots and
  the assessment keys would have to change at run time, for a rare set-up case (declined by the maintainer).
- Let the TUI also download missing StarBreaker or Blender builds: duplicates the setup scripts
  ([0033](0033-one-pin-file-for-dev-and-bundles.md)); not wanted now.

## Consequences
- `main.rs` no longer bails on missing tools in TUI mode; checks move into a background step list the TUI draws.
- The pinned StarBreaker release must be bumped to one with `dcb query --ids`.
