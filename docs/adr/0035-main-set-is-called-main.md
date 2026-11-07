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
- `Store::open` migrates an old store once: `sets/live` becomes `sets/main` (its hard links move with the folder), its
  `set.json` name and the `base` of sets branched off it become main, an inherited target list's source `<live>`
  becomes `<main>`.
- Runs stay as they are (run.json is never changed, [0011](0011-runs-and-sets.md)); `Store::run` reads their set
  `live` as main, so the run lists other nodes fetch carry main.
- Consumers read `sets/main`.

## Alternatives
- `default`: names the role at the start only, not branching and adopting.
- `master`: the same git picture with git's older name.
- Rename without migration: every node's store would need a hand edit.

## Consequences
- Old stores need no step; a node with an older packshot build differs in state anyway and takes part in no run.
- `store::OLD_MAIN` stays for the migration and old run.json files.
- The consuming org tool reads `sets/main` from now on.
