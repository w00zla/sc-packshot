# 0002. Take facts, not code, from projects without a licence

- Status: accepted
- Date: 2026-10-24
- Recorded: 2026-11-05
- Scope: packshot, render.py

## Context
Community projects already solve parts of the problem. VeeLume/sc-render renders items like the game's inventory; sc-holotable and ScDataDumper (and svarog) read the relevant DataCore records. None of them carries a licence. Without one, copying code or depending on it is not permitted. The maintainer had applied the same rule earlier to another community bot in the consuming project.

## Decision
- No code and no dependency from sc-render, sc-holotable, ScDataDumper, svarog or similar unlicensed projects.
- What may be taken are facts about the game's data: which records exist, which fields mean what. They served as pointers to the records, which were then read and implemented independently from the DataCore (see [0003](0003-pose-and-light-from-game-data.md)).
- Their code was read only to learn how a value is computed, not copied.
- The same stance holds for p4kx (evaluated, not adopted): its decision records serve as hints, which must be verified on own material.

## Alternatives
- Use or depend on sc-render: rejected, no licence.
- Rebuild the sc-render concept in three.js: estimated "a week", and see [0001](0001-render-icons-from-3d-models.md).

## Consequences
Pose, light and target selection are computed from the DataCore by packshot itself. Findings taken from other projects count as hints, not proof. If one of those projects gains a licence, this ADR can be revisited.

## References
- sc-quartermaster docs/catalog-sources.md "Icons" ("facts from the game data only").
