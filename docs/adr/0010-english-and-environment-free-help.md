# 0010. English throughout; help text free of repository and environment specifics

- Status: accepted
- Date: 2026-11-03
- Recorded: 2026-11-05
- Scope: tools/packshot, bundle templates, scripts, render.py, CLI help

## Context
packshot was built inside a project that writes German comments and UI texts. The tool is meant to be usable by others, and it started to contain wording that was only true on the maintainer's machines (for example a help line naming port 47950, wrong for any other setup).

## Decision
- All text in `tools/packshot`, its bundle templates, `scripts/packshot-bundle.*` and `scripts/starbreaker-build.*` is English: TUI, help, messages, errors, `--report` (dot as decimal separator), comments, file and module names (`verbund.rs` became `cluster.rs`, the German readme became `README.txt`), render.py texts. Folder terms are English too (`runs/`, `sets/`).
- JSON keys and event names were not changed by this.
- CLI help (2026-11-05): about 35 lines of at most 100 characters, one line per option, groups "What this node does / What to render / Modes / Tuning / Paths", TUI key hints in parentheses. No explanatory texts about files, cluster port or docs; usage is `packshot [--render] [--export] [options]` without a package manager. No repository-specific defaults (`local/...`, `.env`); only defaults fixed in the code are shown (`--chunk` 50, `--size` 256, `--set` live, `--scope` missing, `work/` in the packshot folder, `$SC_DATA_P4K`, the 3/4 and 1/4 core formulas).

## Alternatives
Not recorded (German comments like the rest of the original project were the status quo).

## Consequences
Contributors and users outside the original project can read everything. Help and messages must be checked for host names, paths and ports before release. The decision matched the plan to make packshot standalone.

## References
- sc-quartermaster 3a6a2d8, 8b99788, 4061fce.
