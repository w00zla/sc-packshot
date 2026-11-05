# 0027. packshot writes common image formats; thumbnails are the user's business

- Status: accepted
- Date: 2026-11-05
- Scope: output module

## Context
packshot writes 256 px transparent PNGs. sc-quartermaster turns them into AVIF thumbnails in three sizes with its own
tool. A public tool has users with other needs (sizes, formats, pipelines).

## Decision
- packshot supports the most common output formats; the exact list is not decided yet.
- Thumbnails, sprite sheets or web sizes are not packshot's job; consumers make them from packshot's output.
  sc-quartermaster keeps its thumbnail tool.

## Alternatives
- Move the thumbnail tool into packshot: one consumer's needs in the core.

## Consequences
- Output size and format are profile parameters ([0022](0022-modular-profiles.md)).
