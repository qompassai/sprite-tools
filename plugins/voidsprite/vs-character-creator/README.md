# vs-character-creator

A VoidSprite native plugin (Rust cdylib, zero dependencies), v1.0.0.
One editor action — **"Character creator: generate base + sheet scaffold"** —
that procedurally renders a parametric base character sprite plus a
scaffolded 24-frame mood sheet for the light-show sprite pipeline.

Display name: `character creator`. Authors: Pax (Qompass AI).

## What it does

The action reads `~/.config/voidsprite/character_creator.cfg`
(VoidSprite gives parameter dialogs to filters but not to editor actions,
so the action is config-file driven) and writes three files to
`~/character-creator-sheets/`:

- `<name>_base.png` — the 96x192 base character sprite,
- `<name>_sheet.png` — the 384x1152 scaffold sheet (4 cols x 6 rows of
  96x192 cells; every cell is an exact copy of the base frame),
- `<name>_sheet.json` — sidecar naming the six mood rows
  (idle, blush, wink, pout, celebrate, alarmed) and the pipeline contract
  (cell 96x192, 4x6, 180 ms/frame).

The sheet is a *scaffold*: each mood row starts as four copies of the base
frame so the artist (or an art pass) only redraws what each expression
changes. The SDK v1 surface exposes no way for an editor action to create
session images or frames, so file output follows the sibling `vs_lightshow`
exporter's pattern.

## Config format

Strict parsing, mirroring the sibling exporter's discipline: blank lines
and `#` comments are skipped; every other line must be `key=value`;
**unknown keys, duplicate keys, over-long lines (>256 chars), and malformed
values abort the whole load loudly** — the action posts an error
notification naming the line and does not write anything.

```ini
# ~/.config/voidsprite/character_creator.cfg
name=Nova
body_type=female            # female | male
hairstyle=ponytail          # ponytail | braids | bob | long
hair_color=#3b2a20          # #rrggbb, '#' required, exactly 6 hex digits
face=oval                  # round | oval | square-jaw
eye_color=#2a7de1           # #rrggbb
weight=average              # slim | average | heavy
bust_size=medium            # female only: small | medium | large
height=average              # short | average | tall
outfit=tech-jacket          # tech-jacket | casual | formal | athletic
outfit_color=#23272e        # #rrggbb
accent=#e14a7d              # #rrggbb
accessories=visor,hairclip  # csv of visor, headphones, hairclip; empty = none
```

Key reference:

| Key | Values |
|---|---|
| `name` | 1–48 chars of `[A-Za-z0-9 _-]`; used for output file naming |
| `body_type` | `female` \| `male` — selects which torso param applies |
| `hairstyle` | `ponytail` \| `braids` \| `bob` \| `long` |
| `hair_color` | `#rrggbb` |
| `face` | `round` \| `oval` \| `square-jaw` — base face shape (expressions are per-mood art) |
| `eye_color` | `#rrggbb` |
| `weight` | `slim` \| `average` \| `heavy` — torso/limb widths |
| `bust_size` | `small` \| `medium` \| `large` — female only |
| `chest_size` | `flat` \| `average` \| `muscular` — male only |
| `height` | `short` \| `average` \| `tall` — head-to-body proportion |
| `outfit` | `tech-jacket` \| `casual` \| `formal` \| `athletic` |
| `outfit_color` | `#rrggbb` |
| `accent` | `#rrggbb` |
| `accessories` | csv of `visor`, `headphones`, `hairclip`; empty value = none |

All keys are required except the torso pair, which follows the rules below.

### Validation rules

- `body_type=female` **requires** `bust_size` and **forbids** `chest_size`;
  `body_type=male` **requires** `chest_size` and **forbids** `bust_size`.
  Invalid combinations (e.g. `bust_size` with `body_type=male`) fail loudly —
  they are never silently ignored.
- Unknown enum values abort (e.g. `weight=chonky`, `outfit=spacesuit`).
- Colors must be exactly `#` + 6 hex digits (`red`, `#fff`, `ff0000` all abort).
- Unknown accessory flags and duplicate flags abort.
- `name` rejects path separators, dots, and anything outside `[A-Za-z0-9 _-]`
  so it is safe for filenames and JSON without escaping.

## Parametric body

Every scale step changes **geometry** (pixel widths, torso shape,
proportions), not just the palette. Canvas is 96x192, origin top-left,
character centered on x=48.

**Height → head box** (head-to-body ratio on the 192 px canvas):

| `height` | head box (x, y, w, h) | ratio |
|---|---|---|
| `short` | (24, 8, 48, 48) | 192/48 = **1:4** (chibi) |
| `average` | (31, 16, 34, 35) | 192/35 ≈ **1:5.5** |
| `tall` | (34, 22, 28, 27) | 192/27 ≈ **1:7** (mature) |

**Face** adjusts the head box: `oval` narrows 4 px and lengthens 6 px;
`square-jaw` widens 4 px with square bottom corners; `round` keeps the box
with fully rounded corners.

**Weight → widths** (torso half-width / arm width / leg width, px):

| `weight` | torso half-w | torso full w | arm w | leg w |
|---|---|---|---|---|
| `slim` | 14 | 28 | 8 | 9 |
| `average` | 18 | 36 | 10 | 11 |
| `heavy` | 24 | 48 | 13 | 15 |

**Bust/chest → upper-torso zone** (top 20 px of torso), per-side delta on
the torso half-width: `bust_size` small/medium/large = +0/+3/+6;
`chest_size` flat/average/muscular = −2/+0/+4. `muscular` additionally
draws an accent shoulder bar 8 px wider per side.

**Outfit variants** (procedural, drawn in `accent` over the `outfit_color`
torso): `tech-jacket` (collar tabs, zipper, shoulder pads), `casual`
(mid-torso stripe), `formal` (lapels + tie), `athletic` (torso side
stripes + arm bands).

**Hairstyles**: `bob` (chin-length panels), `long` (panels to mid-torso
plus a back panel), `ponytail` (tail sweeping down-right with an accent
tie), `braids` (twin segmented columns beside the head).

**Accessories**: `visor` (band across the eyes), `headphones` (band + ear
cups), `hairclip` (brightened-accent clip in the hair).

## Build

Pinned toolchain: nightly-2026-04-16 (`rust-toolchain.toml`). On the build
machine (primo), from this directory:

```sh
cargo build --release   # must be zero warnings
```

Output: `target/release/libvs_character_creator.so` (cdylib,
`panic = "abort"`).

## Test

```sh
cargo test
```

The suite covers: config parser (good file, missing key, unknown key,
duplicate key, malformed hex, unknown enum values, unknown/duplicate
accessories, bad names, line discipline, oversize input), adversarial
cross-key validation (male+bust_size, female+chest_size, missing torso
param per body type), geometry (weight/height/bust/chest steps change
pixel widths), palette application (configured colors land on the right
pixels), hairstyle signatures (four ids produce pairwise-distinct hair
pixel sets plus per-id silhouette checks), sheet scaffold (24 cells, cell
origins, every cell equals the base frame), PNG writer (magic, IHDR,
chunk CRCs, stored-deflate round-trip with Adler-32, known-answer
crc32/adler32 vectors), the JSON sidecar, and an end-to-end run of the
real `extern "C"` action entry against a scratch `$HOME` asserting the
three output files.

## Limits (honest)

- This produces a base **scaffold** for the artist, not finished character
  art. Proportions are simple geometric blocks; the face is a mannequin
  base in the outfit color with flat eye/mouth marks; mood expressions
  are **not** drawn — every sheet cell is the base frame, and the
  expressions still need human (or AI-art) finishing per mood row.
- The parametric model is deliberately small: 2 body types, 3 faces,
  3 weights, 3 bust/chest steps, 3 heights, 4 hairstyles, 4 outfits,
  3 accessories. It is a starting grid, not a full character creator;
  a richer scaffold means less manual redrawing, not zero redrawing.
- No in-app preview: VoidSprite gives parameter dialogs to filters, not
  to editor actions, so iteration is edit-config → run action → open the
  PNG. Keep the config file next to the reference doc.
- PNG output only (dependency-free writer, uncompressed deflate blocks —
  files are larger than an optimized encoder would produce).
- The action writes files; it cannot inject the sheet into the open
  VoidSprite session (SDK v1 has no such entry point).
