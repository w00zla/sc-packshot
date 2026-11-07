# 0047. One assessment per operating system, in one file

- Status: accepted
- Date: 2026-11-07
- Scope: compute assessment ([0019](0019-compute-assessment-on-demand.md), [0046](0046-assessment-never-stale.md))

## Context
A PC that boots Windows and Linux can use the same packshot folder from both (a checkout on a shared drive). The
assessment was one entry there, and since 0046 an assessment stays in use: Linux's counts ran on Windows and the other
way round. They differ a lot on the same hardware: one PC measured 39-48 items/s at 8-10 Blender on Linux and 22-24
on Windows, where starting and loading took about twice as long.

## Decision
`assessment.json` in the packshot folder holds one entry per operating system (`windows`, `linux`). Every OS reads
only its own entry and replaces only its own entry when it assesses; the other one stays. A file of the older layout
(a single assessment, measured on an unknown OS) reads as none and is rewritten with the next assessment.

## Alternatives
- One file per OS (`assessment-<os>.json`): the same, in two files (the maintainer preferred one).
- The OS as a validity key: with 0046 a stale assessment still counts, so the wrong OS's numbers would stay in use.

## Consequences
- After the update every node needs one assessment per OS it runs on.
