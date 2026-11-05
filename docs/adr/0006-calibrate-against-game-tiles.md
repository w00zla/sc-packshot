# 0006. Calibrate against the game's own tiles; general rules over per-item fixes

- Status: accepted
- Date: 2026-11-02
- Recorded: 2026-11-05
- Scope: look work in render.py, the StarBreaker fork's add-on

## Context
Look tuning can drift into endless eyeballing and one-off fixes. The aim is "looks like the game's inventory tile", and a fix for one item that breaks ten others is worse than none.

## Decision
- Reference: the game's inventory and warehouse tiles (frames from the consuming companion app's debug recordings, about 30 items) and the Star Citizen wiki's item images, which are in-game shots.
- Measure before showing: compute numbers (channel ratios, clip percentage, error in stops) against the reference, then look.
- Probe in a scratch copy of render.py or with add-on monkeypatches; never edit render.py first.
- Fix a rule for a category of items, not an item. The fork's own contribution rules say the same ("never hard-code workarounds for specific assets"; game data values come from the DataCore, not from constants).
- Rules that are fitted to one item are flagged as unproven (flat-layer glow fitted to Chairman's Club, Geist chin).
- Per-item overrides (e.g. "Bear eyes: no lens x16, no bloom") were liked but the maintainer wants general rules; that decision stays open.

## Alternatives
- Fixed per-item overrides: rejected for now.
- Judging by eye alone: replaced by measured comparison.

## Consequences
Slower per fix, but changes carry over to new patches and items. Several fits remain item-derived. Open look items: LayerBlend_V2 glow per decoded shader, Strata visor band, glass gloss and gold highlights. Since [0022](0022-modular-profiles.md) the calibration targets the default profile (game-tile), not packshot as a whole.

## References
- Related: [0003](0003-pose-and-light-from-game-data.md), [0005](0005-own-tone-curve-and-studio-world.md).
- sc-quartermaster docs/catalog-sources.md "Icons" step 6; fork AGENTS.md "Coding Practices".
