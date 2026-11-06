# 0007. Rust and ratatui orchestrator, named packshot

- Status: accepted
- Date: 2026-11-01
- Recorded: 2026-11-05
- Scope: tools/packshot

## Context
The original `pnpm icons:render` (TypeScript) ran one export and one Blender at a time: 46-52 minutes for a full run on the render PC (first run 4 hours), CPU and GPU mostly idle, about 0.40 s per item. The maintainer wanted a cross-platform tool with image preview, good filter and re-run controls, and parallel processing. The bundle also needed a portable Node.

## Decision
- A Rust program (ratatui plus ratatui-image; Sixel/Kitty/iTerm2 with half-block fallback) orchestrates several StarBreaker exports and Blender processes in parallel. It reads `targets.json`, computes pose and rig itself ([0003](0003-pose-and-light-from-game-data.md)) and needs neither Node nor a database. The wire format to render.py (`icon:`/`failed:` lines) stayed unchanged at first.
- Filter terms (class name, display name, type) and scope: missing, failed last time, all (asks for confirmation).
- `--plain` runs without the TUI.
- Name `packshot` (2026-11-02), chosen for describing the purpose; "Foundry" was avoided because it is CIG's `libs/foundry`. The first working name was `tools/icons-tui`.
- The old TypeScript render loop and the Node-based bundle script were removed; the consuming project kept only a target-list writer.

## Alternatives
- Ink (Node) with ink-picture: images bypass Ink's render pipeline, flicker and leftovers.
- .NET (Spectre.Console, Terminal.Gui): halfblock resolution only for images, and not the maintainer's stack for this tool.
- Extending the TypeScript CLI: rejected.
- Other names: Softbox (rejected), Lightbox, Darkroom, Mint, Thumbfarm, Itemshot, Gearshot.

## Consequences
One static binary per OS, no Node on render machines. The TUI grew views for runs, sets and nodes (see [0011](0011-runs-and-sets.md), [0013](0013-cluster-capabilities-and-head-node.md)). Konsole disables Sixel in ratatui-image and falls back to blurry half blocks, so `PACKSHOT_GRAPHICS` can force a protocol.

## References
- sc-quartermaster 3a6a2d8, 8b99788.
