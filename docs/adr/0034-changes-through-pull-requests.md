# 0034. Changes land on main through pull requests

- Status: accepted
- Date: 2026-11-07
- Scope: repository policy

## Context
Until 2026-11-07 commits went straight to `main`. The StarBreaker fork already works through pull requests (its
AGENTS.md and ADR 0011 there), and a public repository should show reviewed, squashed steps rather than a stream of
direct pushes. GitHub enforces branch rules on a private repository only with a paid plan, so the repository went
public the same day.

## Decision
- Every change goes on a branch from `main` and lands through a pull request, squash merge only. No direct pushes to
  `main`, no force pushes.
- A ruleset on `main` enforces it: pull request required (0 approvals, one maintainer), squash merge only, no force
  push, no deletion, no bypass. Once a test workflow exists, its checks become required too.

## Alternatives
- Keep pushing to `main`: simpler for one maintainer, but nothing checks a change before it lands.
- Pay for a plan to enforce it while private: not needed, the repository was meant to be public anyway
  ([0021](0021-standalone-public-repo.md)).

## Consequences
- Earlier commits on `main` stay as they are.
- Adding the test workflow includes adding its checks to the ruleset.

## References
starbreaker-ps ADR 0011 and 0012.
