# 0039. packshot owns the target-list format and builds lists from the DataCore

- Status: accepted
- Date: 2026-11-07
- Scope: target lists (`targets.json`, `Target` in `src/engine.rs`), TUI view 2 (Sets), documentation

## Context
The target-list format was born in the consuming org tool (sc-quartermaster writes it from its database) and lives in
packshot only as the `Target` struct; it is documented nowhere. Since packshot is a standalone public tool
([0021](0021-standalone-public-repo.md)), a user without that consumer cannot produce a target list at all, and the
default Targets module of [0022](0022-modular-profiles.md) ("selector over the DataCore") was never built. Sample
checks such as the StarBreaker fork's sync check (its ADR 0002: a fixed sample of 45 entities, "the sample list lives
with the consumer") had no defined place either.

## Decision
- **The format is packshot's interface.** packshot documents it publicly (a JSON array of targets: `uuids` (one or
  more; the first names the image file, further ones become hard links), optional `names` (one display name per
  UUID), `entity` (class name), `type` and `subType` (the DataCore attach type), `worn` (default false)). Consumers
  write to it; a change of the format is a new ADR.
- **Two sources, one format.** Lists made elsewhere are imported as today (key `t`, `--targets`); the consumer's
  selection rules stay with the consumer. New: a target picker in the TUI (view 2, Sets) builds a list from the
  DataCore of the local `Data.p4k`, by type/subType, class-name terms or a pasted list of class names, shows the
  count and writes it into the set. This is 0022's default Targets module.
- **Sample checks are ordinary sets.** A sample is a set with a small target list, rendered and compared against
  `main` like any experiment. No set name is fixed and no code is special to samples. The list of the first fork
  check (45 entities) is kept by the maintainer outside the repository.

## Alternatives
- Keep the consumer's format undocumented: public users stay without a target list.
- packshot implements the consumer's selection (gear in stock, recipe harvestables, shop items): needs that
  consumer's database.
- A fixed sample set (e.g. `sample`) with its own compare step: hard-wires one workflow (declined by the maintainer).

## Consequences
- The picker needs an entity listing with UUID, class name, type/subType and English name from the DataCore; packshot
  already runs `starbreaker dcb query` / `dcb extract` for its pose and light tables ([0003](0003-pose-and-light-from-game-data.md)).
  Whether that covers the listing without a fork change is not verified yet.
- The format documentation, the picker and the stored sample list are follow-ups.
