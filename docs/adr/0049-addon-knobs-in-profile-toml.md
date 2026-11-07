# 0049. The add-on's look knobs are profile parameters (`[addon]` in profile.toml)

- Status: accepted
- Date: 2026-11-08
- Scope: profiles ([0025](0025-material-module.md), [0041](0041-profile-hooks.md), [0042](0042-profile-folder-and-hook-signatures.md)),
  `blender/render.py`

## Context
The StarBreaker add-on keeps its look values tuned by eye (monitor roughness, glow, ore veins, organic tint, POM
depth, lights, skin, hair) in `runtime/constants.py` and reads them at use time (fork ADR 0015). A profile could set
them in `before_open`, but a few numbers should not need Python.

## Decision
- `profile.toml` takes a section `[addon]`: names of `runtime/constants.py` of the add-on the run uses (the installed
  one, or the profile's own copy) with their values. `render.py` sets them on that module before every item's file
  opens; `before_open` still runs after it, for anything beyond values.
- A name the add-on does not define, or a value whose type differs from the constant's (number, list, bool, string),
  is refused: a start check in the TUI, an error in console modes, and `render.py` refuses it too.
- The `default` profile sets none: the defaults live in the add-on only. The README lists the knobs with their default
  and meaning.
- The section is part of the profile folder, so it is hashed and distributed with it.

## Alternatives
- Only `before_open`: one line of Python per value, and no check for a mistyped name.
- The default profile repeating every default: two places to keep in step.

## Consequences
- Which names exist depends on the add-on version; a profile written for a newer add-on is refused by an older one.
