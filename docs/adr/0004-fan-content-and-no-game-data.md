# 0004. Renders are fan content; no game data in the repository

- Status: accepted; the open guideline check is settled by [0032](0032-fan-disclaimer-suffices.md)
- Date: 2026-10-25
- Recorded: 2026-11-05
- Scope: repository content, published images

## Context
The renders show models owned by Cloud Imperium Games. The maintainer considered whether publishing them (the consuming web app serves thumbnails without login) is acceptable, and concluded that fan content of this kind is fine, since the Star Citizen wiki produces its thumbnails the same way.

## Decision
- Rendered images are treated as fan content in the sense of the wiki's practice; the question was closed on that basis.
- No game data goes into the repository: no `Data.p4k` or parts of it, no extracted records, no models, textures or renders, no game fonts. Such files live in git-ignored local folders only.
- `Data.p4k` is never copied into the repository; nodes read it from the game install or from a local copy outside the checkout (see [0016](0016-data-p4k-only-on-exporters.md)).
- Test fixtures must be synthetic. Facts about game data (class names, record layouts) are fine to document.
- A consumer that publishes images shows a fan-site disclaimer (the consuming web app does).

## Alternatives
Not recorded.

## Consequences
The decision rests on the wiki precedent only. A formal check against CIG's fan-content guidelines or fan kit is not recorded and is still open; it should happen before any wider distribution. Anyone running packshot needs their own copy of the game.

## References
- sc-quartermaster docs/catalog-sources.md "Icons" (end), docs/webapp.md "Thumbnails".
- Related: [0002](0002-facts-only-from-unlicensed-projects.md).
