# 0012. Per-OS bundles built by script

- Status: accepted
- Date: 2026-11-03
- Recorded: 2026-11-05
- Scope: `scripts/packshot-bundle.{sh,ps1}`, `tools/packshot/bundle/`

## Context
The full run happens on render machines without a development environment. The first bundle shipped portable Node plus the TypeScript CLI and was about 1 GB (951 MB, 6568 files for the Windows variant). A Rust binary needs neither Node nor a database, which lets bundles shrink and cover Linux nodes.

## Decision
`scripts/packshot-bundle.{sh,ps1} [--platform all|windows|linux] [--out <dir>]` builds one folder per OS.

- Windows: `packshot.exe` (native or cross-built with mingw), StarBreaker with a `COMMIT` file, the official Blender zip checked by SHA256 and left packed, `portable/` (Blender config and add-on as real files), `render.py`, `targets.json`, `setup.cmd` (unpacks with the Windows `tar`), `run.cmd` (refuses `--render` without Blender), README, `packshot.example.json`; text files CRLF. About 425 MB.
- Linux: static `packshot` (musl target, linked by `rust-lld`, no C toolchain), glibc 2.30+ StarBreaker, the official Blender tarball kept packed (symlinks, exec bits), `setup.sh`, `check.sh` (add-on test and a headless EEVEE render on Vulkan), `run.sh`. About 398 MB.
- `Data.p4k` and `build_manifest.id` go next to the bundle, not into it.
- Templates live in `tools/packshot/bundle/`. StarBreaker is built from the fork's committed HEAD by `scripts/starbreaker-build.{sh,ps1}`.

## Alternatives
- Portable Node plus CLI in the bundle: about 1 GB, dropped with the TypeScript loop.
- musl-built StarBreaker: exports 58 % slower (4.3 s vs 2.7 s per armour piece); export is the limit. Only packshot itself is musl.
- NVIDIA `--gpgpu` driver variant: lacks `libnvidia-gl`, so no Vulkan.

## Consequences
- Every code change changes the build hash, so all bundles must be refilled (see [0014](0014-same-state-or-no-run.md)).
- Not verified end to end at recording time: the Windows zip and `setup.cmd`, the `.ps1` scripts.

## References
- sc-quartermaster eb18cb1, 4dc90fd
- [0016](0016-data-p4k-only-on-exporters.md)
