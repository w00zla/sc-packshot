# 0008. Blender on Vulkan with two threads, long-lived worker processes

- Status: accepted
- Date: 2026-11-02
- Recorded: 2026-11-05
- Scope: packshot (blender.rs, engine.rs), render.py

## Context
With 8 parallel Blenders the OpenGL render phase grew from 0.12 to 0.86 s per item (shader compilation on the CPU, GPU at about 15 %). Starting a Blender per chunk also costs time and memory behaviour was unclear.

## Decision
- `--gpu-backend vulkan`, fixed, no probe and no OpenGL fallback (the maintainer chose fixed over probe-with-fallback). Render phase 0.30 s instead of 0.86 s; 704 renders in 114 s instead of 160 s; images differ by 0.04/255 mean. Headless Vulkan worked on a Linux server without desktop.
- Every Blender gets `--threads 2`: the same or slightly more items/s for 35-55 % less CPU, and exports lose 15 % instead of 32 %.
- Default counts: Blenders = assessed count, else 3/4 of physical cores; exports = assessed count, else 1/4 of cores, at least 2 (a single export was the limit at 4 cores: 101 s versus 80 s). Throughput flattens at about 6 Blenders (6.3 to 6.7 items/s up to 12). `--workers`, `--exporters` and the TUI keys override.
- One long-lived Blender per render slot (`render.py --stdin`: prints `ready: pid`, one JSON job per line, answers `icon:` or `failed:`, EOF or `quit` ends), restarted after 200 jobs (`BLENDER_JOBS`) or above 3 GB RSS (`BLENDER_RSS`). One `export-batch --stdin` per export slot, restarted after 40 chunks or above 2 GB, with `MALLOC_ARENA_MAX=2` on Linux. Idle processes quit after 60 s. A crash fails only the job in flight.
- Gain: +12 % items/s (10.7 to 11.9) over a Blender per chunk; the first item of a chunk costs 0.15 s extra instead of 0.6 s.

## Alternatives
- OpenGL as default; OpenGL with `shader_compilation_method='SUBPROCESS'` (211 s, worse).
- Reopening the same items in one process (no shader reuse); smaller blocks (about 3 per Blender) and smaller minimum render chunk (4 versus 10): no gain.
- A persistent worker only as a TUI nicety: first judged to save about 0.03 s/item, later built for the measured gain above.

## Consequences
A node without a working Vulkan driver cannot render (a bare NVIDIA driver without libnvidia-gl lacks Vulkan). Windows renderers check path length (above 259 characters) and case collisions, since missing textures render silently white; keep `--work` short.

## References
- sc-quartermaster eb18cb1, a4bdf98; its docs/catalog-sources.md "Icons" step 4.
