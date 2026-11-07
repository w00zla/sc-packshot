# 0047. One assessment per operating system

- Status: accepted
- Date: 2026-11-07
- Scope: compute assessment ([0019](0019-compute-assessment-on-demand.md), [0046](0046-assessment-never-stale.md))

## Context
A PC that boots Windows and Linux can use the same packshot folder from both (a checkout on a shared drive). The
assessment lived in one file there, and since 0046 an assessment stays in use: Linux's counts ran on Windows and the
other way round. They differ a lot on the same hardware: one PC measured 39-48 items/s at 8-10 Blender on Linux and
22-24 on Windows, where starting and loading took about twice as long.

## Decision
The assessment file is `assessment-<os>.json` in the packshot folder (`windows`, `linux`); every OS reads and writes
its own. An `assessment.json` of older versions is not read any more.

## Alternatives
- The OS as a validity key: with 0046 a stale assessment still counts, so the wrong OS's numbers would stay in use.

## Consequences
- After the update every node needs one assessment again (an old `assessment.json` can also be renamed to the OS it
  was measured on).
