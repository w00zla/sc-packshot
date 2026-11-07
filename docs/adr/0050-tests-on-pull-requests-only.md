# 0050. Tests run on pull requests only, not again on main

- Status: accepted
- Date: 2026-11-08
- Scope: CI (`.github/workflows/test.yml`)
- Supersedes in part: [0036](0036-releases-without-blender-and-workflows.md) (test.yml "on pull requests and pushes
  to `main`")

## Context
`main` changes only through pull requests ([0034](0034-changes-through-pull-requests.md)), whose test runs the ruleset
requires. The same tests ran again on the push to `main` after every merge: the same code twice, minutes of Actions
time per change, and a release waited on both.

## Decision
`test.yml` runs on pull requests and by hand (`workflow_dispatch`), not on pushes to `main`.

## Alternatives
- Keep the run on `main`: it also fills the Rust cache that later pull requests and the release tag read.

## Consequences
- A new pull request and the release tag start with a colder Rust cache (no `main` run fills it), so they take
  somewhat longer.
- A pull request should be up to date with `main` before it is merged, so what the run tested is what lands.
