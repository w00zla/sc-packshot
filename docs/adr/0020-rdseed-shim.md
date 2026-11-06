# 0020. Embedded RDSEED shim for Blender on Linux

- Status: accepted
- Date: 2026-11-05
- Recorded: 2026-11-05
- Scope: packshot (`tools/packshot/shim/rdseed_shim.rs`, `build.rs`, Blender launch on Linux)

## Context
Blender 5.2 builds a `std::random_device` on every node-tree update. libstdc++ implements it with the RDSEED instruction (about 27 us per call, on a hardware source shared between cores). One item causes about 1860 such calls, so a whole machine was capped at roughly 13 items/s no matter how many Blender processes ran.

## Decision
- A `no_std` Rust cdylib replaces only `std::random_device::_M_getval()` with splitmix64 over an atomic counter, seeded once via `getrandom`.
- `build.rs` compiles it with `rustc` and `rust-lld` for Linux targets only (no C compiler needed, also for the musl build made on Windows). The `.so` is embedded in the packshot binary, written at start to `<work>/rdseed-shim-<hash>.so` and passed to every Blender through `LD_PRELOAD` (prepended).
- Always on for Linux; there is no switch. It is skipped when the work path contains a blank or a colon (the log says "shim off").
- Windows gets no shim: the MSVC `random_device` is believed not to use RDSEED, which was not verified.

## Alternatives
None recorded.

## Consequences
- Measured with `--threads 2`: 6 Blender went from 12.1 to 25.0 items/s, 459 images bit-identical; the assessment sweep went from 16.0 to 42.5 items/s.
- Depends on the libstdc++ symbol name; a different Blender build may need a new shim.
- Random numbers inside Blender's node updates are no longer hardware random; the renders stay identical, so nothing visible depends on them.

## References
- sc-quartermaster 4061fce
- [0008](0008-blender-vulkan-threads-long-lived.md)
