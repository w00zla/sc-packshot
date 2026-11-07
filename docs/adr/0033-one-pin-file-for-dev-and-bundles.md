# 0033. One pin file for Blender and StarBreaker, used by the dev setup and the bundles

- Status: accepted
- Date: 2026-11-07
- Scope: build, bundles, dev setup, `scripts/`, `deps.toml`

## Context
The bundle scripts downloaded and checked Blender on their own (version as a script default, SHA256 from Blender's
published list) and packed whatever StarBreaker build lay in `local/starbreaker-ps/` (built from the fork's HEAD). A
developer had no way to get the same Blender: `local/blender/` with its `portable/` folder, the linked add-on and its
`userpref.blend` was set up by hand, and the bundles copied that hand-made `portable/` to every node. One shared
`COMMIT` file for both StarBreaker binaries could claim a commit that only one of them was built from.

## Decision
- `deps.toml` at the repository root pins Blender (version and the SHA256 of each platform's official archive) and
  StarBreaker (fork URL, checkout path, commit). Plain `key = "value"` lines, read by the scripts without a library.
- `scripts/setup.{ps1,sh}` sets up a development checkout from it: clones the fork if missing, downloads and checks
  Blender into `local/cache/`, unpacks it into `local/blender/blender-<version>-<platform>/`, creates `portable/`,
  links the fork's add-on in (junction or relative symlink), enables it in `userpref.blend` and builds StarBreaker at
  the pinned commit. It is safe to run again; what is already right stays.
- `scripts/starbreaker-build.*` builds the pinned commit (`-Commit`/`--commit` overrides, e.g. `HEAD` to try a change)
  and stamps each binary (`<binary>.commit`); `COMMIT` mirrors this OS's binary.
- `scripts/packshot-bundle.*` take the Blender version, archives and checks from the same pins and refuse StarBreaker
  binaries whose stamp is not the pinned commit; each bundle's `COMMIT` is its binary's stamp.
- packshot in a checkout prefers the Blender of the pinned version in `local/blender/`.

## Alternatives
- Keep the defaults inside the bundle scripts and document the manual dev setup: two ways to the same Blender that
  drift apart.
- Check against Blender's published SHA256 list at download time: needs the network and trusts the list of the day;
  a pinned hash makes the setup reproducible.

## Consequences
- Changing Blender or StarBreaker is one edit in `deps.toml` plus `scripts/setup.*`; the bundles follow.
- Once the fork publishes releases ([0026](0026-starbreaker-by-release-pin.md)), the `[starbreaker]` pin becomes a
  release tag with SHA256 and the setup downloads instead of building.
