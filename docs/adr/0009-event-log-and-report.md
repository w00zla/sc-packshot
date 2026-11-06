# 0009. Per-run event log and a read-only report

- Status: accepted
- Date: 2026-11-03
- Recorded: 2026-11-05
- Scope: packshot (events.rs, report), render.py

## Context
Tuning throughput (chunk sizes, process counts, export overhead) needs measurements per phase and per node, not wall-clock totals. The first finding from logs was about 1.8 s export-start overhead per block.

## Decision
- Every run writes `events.jsonl` in its run folder: `run_start` (versions, hashes, game version, p4k fingerprint), export, render, fetch, requeue and failure events, a sample every 2 s (CPU, GPU, process, queue), `run_end`.
- render.py appends phase timings to each `icon:` line: `timing load= prep= render= glass= post=`.
- `packshot --report [log|run dir]` evaluates a log read-only (default: the latest run): items/s, phase mean/p50/p95, chunk overhead, Blender start and slot use per node, export overhead, transfers, CPU/GPU, slowest items, failures. It needs no lock and creates no folders.
- Logs show deviations only (no per-item success noise in `run.log`).

## Alternatives
Not recorded. The first version wrote one `<work>/logs/<date>_<node>.jsonl` per node; this moved into the run folder with the runs layout ([0011](0011-runs-and-sets.md)).

## Consequences
Performance claims in other ADRs are reproducible from logs. Event names and JSON keys are stable interfaces for the report. Failures can be reproduced from the log ([0017](0017-delete-work-data-log-deviations.md)).

## References
- sc-quartermaster 3a6a2d8, 1f62aa5, 656bcfc.
