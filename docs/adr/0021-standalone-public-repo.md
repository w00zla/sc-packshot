# 0021. packshot is a standalone public repository; sc-quartermaster is a consumer

- Status: accepted
- Date: 2026-11-05
- Scope: repository, licensing, consumers

## Context
packshot grew inside sc-quartermaster (the consuming org tool) as its icon renderer: the code sat in
`tools/packshot`, the Blender script in `src/icons/render.py`, the bundle scripts in `scripts/`, its documentation in
that project's docs. Its main use is shifting from "render the org tool's icons" to experimenting with materials and
the photo scene ([0022](0022-modular-profiles.md)), which is useful to anyone rendering Star Citizen items.

## Decision
- packshot lives in its own repository (`sc-packshot`), public, under the MIT licence.
- It starts with a fresh history. The development history stays in sc-quartermaster; the decisions made there are
  recorded here as backfilled ADRs (0001–0020).
- The first commit keeps sc-quartermaster's layout (`tools/packshot/`, `src/icons/render.py`, `scripts/`) so the code
  runs unchanged; the modularisation restructures it.
- sc-quartermaster becomes a consumer like any other user: it produces its own target list, renders with a profile
  and makes its own thumbnails ([0027](0027-output-formats-thumbnails-are-users.md)).
- The StarBreaker fork stays a separate repository ([0026](0026-starbreaker-by-release-pin.md)).

## Alternatives
- Keep packshot inside sc-quartermaster: that project is private, and its own concerns (database, bot, web app)
  would stay tangled with packshot's.
- Split with history (`git filter-repo`): not wanted; the history is full of the consumer's specifics.

## Consequences
- Repo- and environment-specific defaults (`local/…`, `.env`, `pnpm packshot:targets`) must go
  ([0010](0010-english-and-environment-free-help.md)); until then the layout check in `main.rs` still looks for
  `local/` and `src/icons`.
- The target list can no longer come from the consumer's database alone ([0022](0022-modular-profiles.md)).
