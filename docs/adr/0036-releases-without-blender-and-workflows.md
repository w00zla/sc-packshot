# 0036. Releases are bundles without Blender; test and release workflows

- Status: accepted; superseded in part by [0050](0050-tests-on-pull-requests-only.md) (tests on pull requests only)
- Date: 2026-11-07
- Scope: bundles, `templates/bundle/`, `scripts/packshot-bundle.*`, `.github/workflows/`

## Context
A bundle carried the official Blender archive (about 400 MB per OS) and a `portable/` folder copied from the
maintainer's own set-up Blender, so only a checkout with `scripts/setup.*` run could build one. Publishing such bundles
would mean large releases and redistributing Blender (GPL) from this repository. There was no CI: tests, clippy and
shellcheck ran only by hand.

## Decision
- **Bundles hold no Blender.** They carry packshot, StarBreaker and its add-on (from the fork's pinned release,
  [0026](0026-starbreaker-by-release-pin.md)), `render.py`, the scripts and `blender.pin` (the Blender version and the
  SHA256 of the official archive for that OS, from `deps.toml`). The bundle's `setup.cmd` / `setup.sh` takes that
  archive from the bundle's folder if it is there (offline nodes), else downloads it from blender.org, checks the
  SHA256, unpacks it and builds `portable/` itself: the add-on copied in and enabled headless.
- The bundle templates move from `bundle/` to `templates/bundle/`, a home for further templates.
- **`.github/workflows/test.yml`**, on pull requests and pushes to `main`: `cargo test --release` on Linux and Windows,
  `cargo clippy --all-targets -D warnings`, `shellcheck` for `scripts/` and `templates/bundle/linux/`.
- **`.github/workflows/release.yml`**, on a tag `v<version>` (as in `Cargo.toml`) on a commit of `main`: both bundles
  built by `scripts/packshot-bundle.sh` on Linux, published as `packshot-<tag>-windows-x64.zip`,
  `packshot-<tag>-linux-x64.tar.gz` and `SHA256SUMS`.

## Alternatives
- Bundles with Blender: large releases, and distributing Blender's binaries brings its GPL obligations here.
- Only binaries plus a kit to assemble a bundle: the user does what CI does anyway.

## Consequences
- A node needs internet once for setup, or the archive put next to the bundle by hand.
- Building a bundle no longer needs a set-up local Blender; the add-on comes from the release, not the local link.
- The ruleset on `main` should require the test workflow's checks once it has run.
