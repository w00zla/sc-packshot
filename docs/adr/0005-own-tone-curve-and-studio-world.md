# 0005. Own tone curve, studio world and glow handling instead of Blender view transforms

- Status: accepted
- Date: 2026-11-02
- Recorded: 2026-11-05
- Scope: render.py

## Context
Blender's Standard view transform clipped light albedos (Snow Camo shell turned flat white). Khronos PBR Neutral has a toe that subtracts `min(R,G,B) - 6.25*min^2`, which turned dark colours black or garish (Whiteout chin B/R 13-27 against 1.9 in the game). AgX and Filmic grey out white. A uniform grey world put a veil on black lacquer; no world made chrome black. The old claim that a "half world in glossy rays" mix helped was found never to have had an effect in EEVEE.

## Decision
render.py writes a linear multi-layer EXR (combined plus emission) and maps it to PNG itself in numpy:
- Contrast below mid-grey only: Y < 0.18 becomes 0.18*(Y/0.18)^1.45 (`CONTRAST`), hue kept.
- Then the Khronos PBR Neutral shoulder (from 0.8) without the toe.
- Studio world `STUDIO`: softbox above 0.4, camera side 0.05, elsewhere 0.02. The game's three rig spots stay.
- Glow: `EMISSION_GAIN` 4, bloom from the emission pass (`BLOOM` 2, Gaussian sigmas 3 and 8 px at 512 px, weights 0.6/0.4), plus a second 4-sample pass with see-through tinted glass so light behind glass blooms.
- Roughness x0.85 for glossy surfaces (`sharpen_gloss`), a compromise between 1.0 and 0.7.

Values were tuned by eye on 19 items; the maintainer picked from contact sheets.

## Alternatives
- Standard (clips, 5.5 % at -0.5 exposure), PBR Neutral with or without exposure shifts (toe stays), luminance-based toe (still crushes dark colours), AgX, Filmic Medium Contrast (colour-true, white turns grey).
- World at 0 (black chrome, hard shadows), uniform grey 0.11 ("too grey").
- Contrast 1.45 over the full range (white burns, 5.9-9 % clipped pixels).
- Roughness x0.45 (black lacquer turned black); bloom sigma 20 px (halo wider than the tile).
- A custom OCIO config or compositor node: numpy was simpler.

## Consequences
Clip 0.5-0.7 %, channel nulling 1.2 % (4.8 % with the toe). The game's reflection probe behind the tile is not reproduced; gold highlights and glass sheen are still weaker than in-game tiles. Since [0022](0022-modular-profiles.md) this is the default profile's behaviour, not a fixed part of packshot.

## References
- sc-quartermaster c8a2093, f44dafa, eb572360 (session ids omitted).
- Related: [0006](0006-calibrate-against-game-tiles.md).
