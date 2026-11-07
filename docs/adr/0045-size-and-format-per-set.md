# 0045. Output size and format belong to the set, without overrides

- Status: accepted
- Date: 2026-11-07
- Scope: sets ([0011](0011-runs-and-sets.md)), profiles ([0042](0042-profile-folder-and-hook-signatures.md),
  [0044](0044-profile-per-run.md)), CLI
- Supersedes in part: [0042](0042-profile-folder-and-hook-signatures.md) (size and format as profile parameters with
  CLI overrides), [0044](0044-profile-per-run.md) (size and format chosen with the run)

## Context
With size and format chosen per run (profile parameters, overridden by `--size` and `--format`), a set picks per
entity the newest run's image, so one set could end up with 256 px PNGs next to 1024 px JPEGs: a consumer reading the
set could no longer rely on what it gets.

## Decision
- A set has one output size and one format, fixed when it is created: the new-set dialog asks for them, preset with
  its base's; `main` is 256 px PNG, and a set without the fields (older stores) counts as 256 px PNG. A set created
  from the command line (`--set <new>`) takes its base's. They cannot be changed: other values need a new set.
- Every run renders at its target set's size and format; on another head node the target set lives there, so its
  values hold.
- `size` and `format` are no profile parameters any more, and `--size` and `--format` are gone. A profile keeps the
  look, JPEG's `quality` and `background` included. The profile stays chosen per run.

## Alternatives
- Per run with overrides (as built first): mixed images in one set.
- Changeable per set: the same mix, only later.

## Consequences
- The runs table shows size and format once per set, after its image count; the run's details show them too.
- A different size or format for the same targets is an experiment set off `main` (or off another set).
