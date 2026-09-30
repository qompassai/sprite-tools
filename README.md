<a href="./LICENSE"><img src="https://img.shields.io/badge/License-Apache%202.0-blue.svg" alt="License: Apache 2.0"></a>

# sprite-tools

The pixel-art toolchain behind the light-show pipeline: native plugins for
[VoidSprite](https://github.com/counter185/voidsprite) (written in Rust),
automation scripts for Aseprite (Lua), and the skill docs that describe how
to develop both.

The workflow this repo supports is deliberately simple: sprites are drawn or
generated, assembled into a tagged 384×1152 sheet, verified against the
sprite contract, then cleaned up and varied with the plugins. Aseprite
scripts handle assembly and verification; the VoidSprite plugins handle
per-pixel transforms and quality checks. Nothing here is a game engine —
the game reads the sheets and builds its atlas from its own Rust constants,
so this toolchain's job is to make sure the sheets are right before the
game ever sees them.

## Directory layout

```
sprite-tools/
├── plugins/
│   └── voidsprite/
│       ├── vs-lightshow/          # "light-show sprite tools" (v1.1.0)
│       │   └── c-orig/            # original C sources (kept for reference)
│       ├── vs-sheet-qa/           # "sheet QA tools" (v1.0.0)
│       ├── vs-defringe/           # "defringe tools" (v1.0.0)
│       └── vs-palette-variants/    # "palette variant tools" (v1.0.0)
│       └── vs-character-creator/   # "character creator" (v1.0.0)
├── scripts/
│   └── aseprite/
│       ├── assemble_sheet.lua
│       ├── verify_sheet.lua
│       ├── export_sheet_json.lua
│       ├── build_montage.lua
│       └── new_character.lua
└── skills/
    ├── voidsprite-plugin-dev/
    └── aseprite-scripting/
```

### Plugins (VoidSprite, Rust)

Each plugin is an independent Cargo crate building a `cdylib` (`.so`) that
VoidSprite loads at runtime. They live under `plugins/voidsprite/` and
share nothing but the sprite contract below.

- **`vs-lightshow`** — "light-show sprite tools" (v1.1.0). A Rust port of
  the original C plugin (sources preserved under `c-orig/`). Provides the
  `blink-eyes` filter, the `breathing-shift` filter, and an editor action
  that exports the sheet together with its JSON sidecar.
- **`vs-sheet-qa`** — "sheet QA tools" (v1.0.0). Two editor actions for
  checking animation consistency: `extract-one-cell` pulls a single frame
  out of a sheet, and `diff-two-cells` highlights differences between two
  cells so drift between frames is visible instead of felt.
- **`vs-defringe`** — "defringe tools" (v1.0.0). A filter that removes
  stray semi-transparent and isolated noise pixels — the halo junk that
  AI-generated pixel art tends to leave around silhouettes.
- **`vs-palette-variants`** — "palette variant tools" (v1.0.0). A
  hue-shift filter that produces alternate palette variants of a sprite
  without touching the artwork itself.
- **`vs-character-creator`** — "character creator" (v1.0.0). An editor
  action driven by `~/.config/voidsprite/character_creator.cfg`: `body_type`
  (female/male), `hairstyle`, `hair_color`, `face` shape, `eye_color`,
  `weight` (slim/average/heavy), `bust_size`/`chest_size`, `height`
  (short/average/tall, mapping onto chibi-to-mature proportions), `outfit`
  variant (tech-jacket, casual, formal, athletic), and accessory flags.
  Every scale step changes real body geometry, not just the palette, and
  invalid combinations (e.g. `bust_size` on a male body) fail loudly.
  Generates a procedural 96×192 base character sprite plus a scaffolded
  24-frame mood sheet (4×6, six mood tags at 180 ms/frame) as a starting
  point for the artist — a richer parametric scaffold, still not finished
  art.

### Scripts (Aseprite, Lua)

`scripts/aseprite/` holds the pipeline glue — small Lua scripts that run
inside Aseprite and keep every sheet conforming to the contract:

- **`assemble_sheet.lua`** — packs the 24 source frames into the tagged
  384×1152 sheet at 180 ms per frame.
- **`verify_sheet.lua`** — asserts the sheet's dimensions, tags, and frame
  durations; fails loudly on any mismatch so a bad sheet never reaches the
  game.
- **`export_sheet_json.lua`** — exports the PNG sheet plus its JSON sidecar.
- **`build_montage.lua`** — renders a 2× montage PNG for reviewing sheets
  on a phone.
- **`new_character.lua`** — interactive Aseprite counterpart to the
  character-creator plugin: a real Dialog with color pickers, dropdowns
  for hairstyle/face/outfit/body type, sliders for weight/bust/chest/
  height (the dialog shows bust *or* chest size depending on body type),
  accessory checkboxes, and a Generate button. Same parametric body model
  and output contract (96×192 base sprite plus the 24-frame tagged sheet);
  accepts `--script-param headless=1` with field overrides for
  non-interactive runs.

### Skills

- **`skills/voidsprite-plugin-dev/`** — how to develop, build, and test a
  VoidSprite native plugin in this repo (crate layout, filter/action
  entry points, the Xvfb load-test gate).
- **`skills/aseprite-scripting/`** — how to write and run the Aseprite Lua
  scripts (API surface used, batch invocation, sheet-contract helpers).

## Building

Each plugin builds independently with Cargo. Run these on a machine with
the Rust toolchain and VoidSprite installed (VoidSprite is a Qt application;
plugin development needs its plugin SDK headers available):

```sh
cd plugins/voidsprite/vs-lightshow
cargo build --release   # produces target/release/libvs_lightshow.so
cargo test              # runs the crate's unit tests
```

Repeat per plugin — `vs-sheet-qa`, `vs-defringe`, `vs-palette-variants`,
and `vs-character-creator` follow the same commands. Install a built `.so`
into VoidSprite's plugin directory and restart VoidSprite (or reload
plugins) to pick it up.

### Aseprite scripts

Run any script headless with Aseprite's batch mode:

```sh
aseprite -b --script scripts/aseprite/assemble_sheet.lua
aseprite -b --script scripts/aseprite/verify_sheet.lua
aseprite -b --script scripts/aseprite/export_sheet_json.lua
aseprite -b --script scripts/aseprite/build_montage.lua
aseprite -b --script scripts/aseprite/new_character.lua
```

For non-interactive runs, `new_character.lua` accepts field overrides via
script parameters:

```sh
aseprite -b --script-param headless=1 \
  --script scripts/aseprite/new_character.lua

### Xvfb load-test gate

Before a plugin build is accepted, it passes a load test under a headless X
server: Xvfb provides the display VoidSprite needs, VoidSprite is launched
headless with the freshly built `.so` in its plugin path, and each of the
plugin's filters and editor actions is invoked against a fixture sprite.
The gate fails the build if the plugin crashes, fails to load, or produces
output that breaks the sprite contract — a red plugin never ships to the
live tree.

## Sprite contract

Every sheet in this pipeline conforms to one layout. In plain terms: each
character is a single 96×192-pixel frame; a sheet holds 24 of them in 4
columns and 6 rows; the rows are the six moods in a fixed order.

The exact terms:

- **Cell size:** 96 × 192 px
- **Grid:** 4 columns × 6 rows (24 frames per sheet)
- **Sheet size:** 384 × 1152 px
- **Row order (top to bottom):** idle, blush, wink, pout, celebrate, alarmed
- **Frame duration:** 180 ms

The game's Rust constants are the source of truth for this layout — the
game builds its texture atlas from them, and they are authoritative. The
JSON sidecars produced by `export_sheet_json.lua` only *document* the
contract; nothing in the game reads them at runtime.

## License

Apache-2.0 — see [LICENSE](./LICENSE).
