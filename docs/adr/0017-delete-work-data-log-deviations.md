# 0017. Always delete work data; keep failures reproducible through logs

- Status: accepted
- Date: 2026-11-04
- Recorded: 2026-11-05
- Scope: packshot (`engine.rs`, `events.rs`, work folder)

## Context
Export folders of blocks with failures were kept in `work/export` for diagnosis. That left 28 folders and 2.7 GB behind that nobody cleaned. A full export is several GB, so keeping data does not scale.

## Decision
- Export folders are always deleted after the render, also when items failed, when a queued chunk is cancelled, and on remote nodes. The work folder's `export/` (and the other transient folders `cas/`, `fetch/`, `send/`) is cleared at start, except for `--dry-run`.
- A failure must be reproducible instead: `--filter <entity>` repeats exactly that item, and the evidence is kept in the run: `runs/<run>/run.log` holds the console output plus the last 20 lines of the failing process (the `render.py` traceback, the tail of Blender or `export-batch`) under each failure; the `failure` event in `events.jsonl` carries the same text in a `trace` field. The TUI keeps one line per failure. `blender.log` stays in the work folder.
- Logs show deviations only: the normal case is not logged, so a failure stands out.

## Alternatives
- Keep only the data of failed items: still grows without bound.
- Clean only folders older than the last run.

## Consequences
- Diagnosing a failure needs a re-run of the single item, which takes seconds.
- A crashed process leaves no stale export data after the next start.

## References
- sc-quartermaster a4bdf98, ddbde99
- [0009](0009-event-log-and-report.md)
