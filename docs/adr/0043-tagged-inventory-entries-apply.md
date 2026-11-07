# 0043. Tagged inventory entries apply; a tag beats the sub type

- Status: accepted
- Date: 2026-11-07
- Scope: `src/tables.rs` (pose and light rig per item), [0003](0003-pose-and-light-from-game-data.md)

## Context
[0003](0003-pose-and-light-from-game-data.md) skipped every `inventoryParams` entry with a `requiredTag`, so tagged
items took an untagged entry: pistols the primary-weapon rig, Pico and Orison whale plushies and Kopion horns the
generic Misc pose. Four tagged entries matter for items (`Weapon.FPS.Pistol`: sidearms rig;
`Item.Inventory.CustomOrientation.Toy.Pico` and `.Toy.Orison_Whale`: pose offsets on Misc/UNDEFINED;
`Item.Carryable.1Handed.KopionHorn`: pose offset on Misc/Harvestable); with today's 6734 targets 63 pistols, 25
plushies and 5 horns carry them. The plushies are Misc/Personal, which has an untagged entry of its own, and their
poses were wrong with it (the maintainer, 2026-11-07).

## Decision
- An entry with a `requiredTag` applies only to an entity carrying that tag, and then beats every untagged entry,
  an exact sub type included; among tagged and among untagged entries exact sub type beats UNDEFINED as before.
- packshot reads the entities' tags itself from the DataCore with the other tables (`dcb query --ids
  EntityClassDefinition.tags`, cached as `<work>/dcb/entity-tags.tsv`, refreshed with `--refresh-tables`). The target
  list format does not change.

## Alternatives
- Exact sub type before tag: gives the plushies the pose that was wrong.
- Tags in the target list: every list producer (the picker, consumers' own lists) would have to carry them, and old
  lists would silently lose the tagged poses.
- Tag hierarchies (an entry's tag matched by a child tag): not seen in the data; the four tags sit on the entities
  directly.

## Consequences
- 93 targets render with another pose or rig; their images change with the next full run.
- An exporting node needs StarBreaker with `dcb query --ids` (starbreaker-ps 0.3.2-ps.4 or newer), as the target
  picker already does.
