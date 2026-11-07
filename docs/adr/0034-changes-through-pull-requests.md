# 0034. Changes land on main through pull requests

- Status: accepted
- Date: 2026-11-07
- Scope: repository policy

## Context
Until 2026-11-07 commits went straight to `main`. The StarBreaker fork already works through pull requests (its
AGENTS.md and ADR 0011 there), and a public repository should show reviewed, squashed steps rather than a stream of
direct pushes. GitHub enforces branch rules on a private repository only with a paid plan.

## Decision
- Every change goes on a branch from `main` and lands through a pull request, squash merge only. No direct pushes to
  `main`, no force pushes.
- Once the repository is public, a ruleset on `main` enforces it: pull request required (0 approvals, one maintainer),
  the test workflow green, squash merge only, no force push, no deletion, no bypass. Until then it is a rule in
  `CLAUDE.md`.

## Alternatives
- Keep pushing to `main`: simpler for one maintainer, but nothing checks a change before it lands.
- Pay for a plan to enforce it while private: not worth it for the short time until the repository goes public.

## Consequences
- Earlier commits on `main` stay as they are.
- The ruleset is part of making the repository public.

## References
starbreaker-ps ADR 0011 and 0012.
