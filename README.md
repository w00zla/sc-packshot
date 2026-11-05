# packshot

Renders Star Citizen items as images: it exports each item from the game's `Data.p4k` with
[StarBreaker](https://github.com/w00zla/starbreaker-qm) and renders it in Blender, many exports and Blender processes
at once, on one machine or a cluster of nodes, with a terminal UI (runs, sets, nodes, image preview).

**Status:** just split out of the project it was built for. The code runs as it did there; the layout
(`tools/packshot/`, `src/icons/render.py`, `scripts/`) and a few defaults still carry that origin. Next comes the
modular design: profiles that combine target selection, exporter, material, photo scene and output freely
([ADR 0022](docs/adr/0022-modular-profiles.md)).

## Layout (for now)

| Path | What |
|---|---|
| `tools/packshot/` | the Rust tool (TUI, scheduler, cluster, assessment) |
| `src/icons/render.py` | the Blender render script (`--stdin` job protocol) |
| `scripts/packshot-bundle.{sh,ps1}` | build self-contained bundles per OS |
| `scripts/starbreaker-build.{sh,ps1}` | build StarBreaker from a fork checkout (to be replaced by a pinned release, [ADR 0026](docs/adr/0026-starbreaker-by-release-pin.md)) |
| `docs/adr/` | architecture decisions |

## Build

```sh
cd tools/packshot
cargo build --release
cargo test --release
```

## Security

In a cluster every node runs Python (render script, Blender add-on, override scripts) that it receives from the node
that starts a run. Anyone with the cluster token can make every node run code. Run a cluster only on machines and
with people you trust ([ADR 0028](docs/adr/0028-security-notice-only.md)).

## Game data

This repository contains no game data. Rendered images of the game's items are Cloud Imperium Games' intellectual
property; use them as fan content within CIG's guidelines.

## Licence

MIT, see [LICENSE](LICENSE).
