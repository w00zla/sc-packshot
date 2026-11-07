# 0037. Renders are allowed in example screenshots

- Status: accepted
- Date: 2026-11-07
- Scope: documentation (`docs/assets/`), TUI screenshots (`src/ui/shots.rs`)

## Context
[0004](0004-fan-content-and-no-game-data.md) keeps renders out of the repository. The README's TUI screenshots
(`docs/assets/tui-run.png`, `tui-sets.png`, a4c28c8) show real packshot renders in the preview tile, because the
screenshot generator can take its images from a real set (`PACKSHOT_SHOT_RENDERS`). Procedural placeholders show
the layout but not what the tool produces.

## Decision
- Example screenshots in the documentation may show renders of game items, as part of a screenshot of the tool.
- Everything else of 0004 holds: no `Data.p4k` or parts of it, no extracted records, models, textures or game fonts,
  and no render sets or loose rendered images in the repository.
- The fan-content disclaimer in the README ([0032](0032-fan-disclaimer-suffices.md)) covers them.

## Alternatives
- Procedural previews only: keeps 0004 literal, but the screenshots no longer show the result.

## Consequences
- Regenerating the screenshots with `PACKSHOT_SHOT_RENDERS` is fine; the set folder used for it stays outside the repository.
