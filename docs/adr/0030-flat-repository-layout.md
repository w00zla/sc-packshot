# 0030. Flat repository layout

- Status: accepted; the bundle templates moved to `templates/bundle/` ([0036](0036-releases-without-blender-and-workflows.md))
- Date: 2026-11-06
- Scope: repository layout, scripts, docs

## Context
The first commit kept sc-quartermaster's layout so the code ran unchanged
([0021](0021-standalone-public-repo.md)): the crate in `tools/packshot/`, the Blender script in `src/icons/render.py`,
the bundle templates in `tools/packshot/bundle/`. In a repository that holds only packshot this nesting says nothing,
and `src/` held a Python script while the Rust sources sat two levels down.

## Decision
- The Rust crate is the repository root: `Cargo.toml`, `Cargo.lock`, `build.rs`, `.cargo/`, `src/` (Rust only),
  `shim/` (the RDSEED shim), `target/` (ignored).
- `blender/render.py` holds what runs inside Blender.
- `bundle/` holds the bundle templates, `scripts/` the bundle and StarBreaker build scripts, `docs/` the
  documentation and the ADRs.
- packshot recognises a checkout by `local/` plus `blender/render.py` (else it runs as a bundle, paths next to the
  exe); the bundles keep their own layout (`cli/render.py`).

## Alternatives
- `profiles/game-tile/render.py`: anticipates the profile layout of [0022](0022-modular-profiles.md), whose format is
  not decided yet.
- Keep the crate in `tools/packshot/` and only move `render.py`: less churn, but the nesting stays.

## Consequences
- Plain `cargo build`/`cargo test`/`cargo run` from the root; no `--manifest-path`.
- The render script's hash changed (header comment), so existing assessments are stale and run names change once.
