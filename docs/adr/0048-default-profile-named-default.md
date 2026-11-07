# 0048. The default profile is called `default`, a reserved name

- Status: accepted
- Date: 2026-11-07
- Scope: profiles ([0022](0022-modular-profiles.md), [0042](0042-profile-folder-and-hook-signatures.md),
  [0044](0044-profile-per-run.md)), TUI
- Supersedes in part: [0022](0022-modular-profiles.md) and the ADRs after it that call the default profile `game-tile`

## Context
The bundled profile that mimics the game's inventory tiles was named `game-tile`; the TUI then showed "game-tile ·
bundled" and the add-on as "installed". For a user the useful distinction is default or not: whether a run used the
look and add-on that come with packshot, or something of their own.

## Decision
- The bundled profile is `profiles/default/` and is called `default`. The name is reserved: a profile of that name in
  the packshot folder is refused, and a starter cannot send one under it.
- The TUI shows no origin for the default profile ("bundled" is gone); other profiles keep theirs (store, folder,
  sent by a node). The add-on the default uses shows as "default" instead of "installed"; a profile's own copy stays
  "the profile's copy".
- Its look is unchanged: it still mimics the game's inventory tiles ([0006](0006-calibrate-against-game-tiles.md)).

## Alternatives
- Keep `game-tile`: describes the look, but users ask "which is the default?" first.

## Consequences
- Run records of 1.0.0 say `game-tile`; they load and show as they were.
- Older ADRs keep the name `game-tile` (immutable); it means `default`.
