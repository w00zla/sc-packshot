# 0029. No set comparison in the TUI

- Status: accepted
- Date: 2026-11-05
- Scope: TUI

## Context
A comparison view for sets was left out when the three TUI views were designed (2026-11-02). With experimenting as
the main use ([0022](0022-modular-profiles.md)) it came up again.

## Decision
The TUI gets no comparison of sets or runs. Images are compared with dedicated image tools on the set folders.

## Consequences
- Set folders stay plain image folders that such tools can open.
