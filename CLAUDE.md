# CLAUDE.md

packshot: renders Star Citizen items (StarBreaker export + Blender) in parallel, on one machine or a cluster, with a
ratatui TUI. Public repository, MIT. Split out of sc-quartermaster (the consuming org tool) on 2026-11-05; the
layout still mirrors it (`tools/packshot/`, `src/icons/render.py`, `scripts/`).

## Rules

- **Decisions live in `docs/adr/`** (skill `adr`). Read `docs/adr/README.md` and the matching ADRs before proposing a
  design; do not re-propose what an ADR declined unless its forces changed. New ADR only after the maintainer decided.
- **English throughout**: identifiers, UI texts, comments, docs, scripts.
- **Public repo**: no game data, no personal paths, host names or tokens, no repo- or environment-specific defaults in
  help texts or messages ([0010](docs/adr/0010-english-and-environment-free-help.md)).
- Shell scripts must pass `shellcheck`. Text files are LF (`.gitattributes`).

## Commands

```sh
cd tools/packshot && cargo build --release      # the tool
cd tools/packshot && cargo test --release       # tests; real-tool tests are #[ignore] and need SC_DATA_P4K
scripts/packshot-bundle.sh [--platform all|windows|linux]   # bundles (Linux); .ps1 on Windows
```
