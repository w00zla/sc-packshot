# 0035. The main set is called main

- Status: accepted
- Date: 2026-11-07
- Scope: store (`store.rs`), TUI, cluster, consumers

## Context
[0011](0011-runs-and-sets.md) named the set consumers read `live`. Sets work like git branches: experiments branch
off it and are adopted back by appending their runs. "live" says nothing about that, and next to "adopt into live"
and "live manifest" it read like a state rather than a name.

## Decision
- The set is called `main` (`store::MAIN`); every text and key hint says main ("adopt into main", "main manifest").
- No migration code: an existing store is renamed by hand once (`sets/live` to `sets/main`, the `name` in its
  `set.json`, the `base` of sets branched off it, `"set": "live"` in the runs' `run.json`).
- Consumers read `sets/main`.

## Alternatives
- `default`: names the role at the start only, not branching and adopting.
- `master`: the same git picture with git's older name.
- Migration on `Store::open` (rename the folder, rewrite the set files, read old runs' `live` as main): code that
  stays forever for a handful of existing stores, all of them the maintainer's.

## Consequences
- A store not renamed by hand starts with a new, empty `main` next to its old `live`.
- The consuming org tool reads `sets/main` from now on.
