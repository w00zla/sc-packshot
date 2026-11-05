# 0028. Executing distributed profiles: a documented notice, no extra code

- Status: accepted
- Date: 2026-11-05
- Scope: cluster, documentation

## Context
With [0024](0024-head-node-distributes-profiles.md) every node runs Python (render script, add-on, override script)
that it received from the head node. The cluster is protected by a shared token.

## Decision
The documentation states plainly that every node in a cluster runs code from any node that holds the token, so a
cluster belongs to people who trust each other. No sandboxing, signing or allow-listing is built.

## Alternatives
- Signed profiles or a sandbox: effort without need in the intended setting (one owner's machines).

## Consequences
- The token remains the only protection.
