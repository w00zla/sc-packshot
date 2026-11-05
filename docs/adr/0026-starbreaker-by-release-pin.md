# 0026. StarBreaker is used through a pinned release

- Status: accepted
- Date: 2026-11-05
- Scope: exporter, bundles, build

## Context
packshot needs the StarBreaker fork (github.com/w00zla/starbreaker-qm): the exporter binary (`export-batch --stdin`)
and its Blender add-on. The fork is a real GitHub fork (fleetyards' fork with upstream merged in, MIT) and must keep
merging upstream; some of its changes may go upstream. Until now the bundle scripts built the fork's committed HEAD
from a sibling checkout.

## Decision
- The fork publishes tagged releases (`0.3.2-qm.n`: Windows and Linux binaries plus the add-on as a zip).
- packshot pins one release (tag and SHA256) in a file in this repository; the bundle scripts download and check it.
- For development a local build is used by path (`--starbreaker`, add-on folder).
- No git submodule, no vendoring of the fork into this repository.

## Alternatives
- Submodule: every clone carries the fork's history (~1300 commits), and users would build StarBreaker themselves.
- Vendoring or subtree: breaks the fork relationship and upstream merges.

## Consequences
- The fork needs a release process.
- The StarBreaker version stays part of the state check ([0014](0014-same-state-or-no-run.md)).
