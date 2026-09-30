# Sheet Pipeline (`scripts/aseprite/`)

The repo's Aseprite-side pipeline: five Lua scripts that assemble,
verify, describe, and present sprite sheets, plus one interactive
character creator. They share a single sheet contract and a single
invocation discipline (batch mode, `--script-param` inputs, exit-code
success signal — see `SKILL.md`).

> Status note: this page documents the scripts' declared contracts as
> specified for the repo. If a script on disk disagrees with this page,
> the script on disk wins for behavior, and this page needs updating —
> the contract numbers below are the authority both sides code against.

## The sheet contract

Every sheet the pipeline produces or consumes obeys these numbers:

| Item | Value |
|---|---|
| Base sprite (one frame) | 96 × 192 px |
| Sheet grid | 4 columns × 6 rows = 24 frames |
| Sheet dimensions | 384 × 1152 px (96·4 × 192·6) |
| Frame duration | 180 ms / frame |
| Animation tags | six mood tags over the 24 frames |

A script that produces a sheet asserts these dimensions before writing;
a script that consumes one asserts them before reading. A mismatch is a
loud failure, not a silent rescale.

The JSON sidecars produced by `export_sheet_json` **document** this
contract — frame rectangles, durations, tag ranges — but the game does
not parse them. The game builds its atlas from Rust constants compiled
into the binary; the JSON exists so humans and tooling can read the
contract without opening the PNG, and so `verify_sheet` has a
machine-readable expectation to check against.

## The four pipeline scripts

### assemble_sheet

Takes the per-frame source art and assembles the 24-frame sheet.

- **Inputs:** source frames (individual images or an `.aseprite` file
  with tagged frames), via `--script-param` (source path, tag order,
  output path).
- **Asserts:** exactly 24 frames; every frame 96 × 192; color mode
  RGBA before pixel work; writes frames into the 4×6 grid in tag order.
- **Output:** the 384 × 1152 sheet PNG. Overwrites its output path —
  the named output in the invocation is the confirmation.
- **Fails:** wrong frame count, wrong frame dimensions, missing source.

### verify_sheet

The gate. Takes a sheet PNG (and optionally its JSON sidecar) and checks
it against the contract.

- **Asserts:** image is exactly 384 × 1152; divisible into a 4 × 6 grid
  of 96 × 192 cells; if a sidecar is given, frame rectangles and the six
  mood-tag ranges match the JSON; frame durations are 180 ms throughout.
- **Output:** `print()`-ed pass/fail summary on stdout; exit `0` on
  pass, non-zero with a `error()` naming the first violated assertion
  on fail.
- **Fails:** any contract violation. Partial passes do not exist.

### export_sheet_json

Writes the JSON sidecar for a sheet.

- **Inputs:** the sheet PNG (or the assembly parameters),
  `--script-param` for output path.
- **Output:** JSON documenting the contract: sheet dimensions, grid
  (4 × 6), frame rectangles in order, per-frame duration (180 ms), the
  six mood-tag ranges. Written with a temp-file + rename so a failed run
  never leaves a half-written sidecar.
- **Contract note:** the game never parses this file — it builds the
  atlas from Rust constants. The JSON is documentation and a verification
  input, not a runtime dependency.

### build_montage

Builds a presentation montage from one or more finished sheets.

- **Inputs:** sheet PNGs, layout params (`--script-param`: columns,
  padding, background, output path).
- **Asserts:** every input sheet satisfies the sheet contract (delegates
  to the same checks as `verify_sheet`); refuses to montage a sheet that
  fails verification.
- **Output:** the montage PNG.

## new_character.lua — the interactive creator

The fifth script and the deliberate exception to the headless-only
pattern: an interactive character creator built on Aseprite's real
`Dialog` API.

- **Interactive UI:** color pickers for hair, eyes, outfit, and accent;
  a hairstyle dropdown; accessory checkboxes; a Generate button that
  builds the character from the chosen options.
- **Output contract — identical to the `vs-character-creator` VoidSprite
  plugin:** a 96 × 192 base sprite plus the 24-frame sheet (4 × 6 grid,
  384 × 1152, six mood tags at 180 ms/frame). Same art contract, two
  hosts.
- **Headless operation:** dialogs cannot render under `--batch`
  (`Dialog()` returns `nil` there), so the script accepts
  `--script-param headless=1` plus one param per dialog field
  (same names as the dialog controls) as overrides. Batch path:
  validate every param at the top, generate, write outputs, exit `0`.
  Interactive path: show the dialog, generate on button press.
  The script must never call `Dialog` when `app.isUIAvailable` is false.

### The design point: why the two creators take different input paths

This is the one place Aseprite's interactive Dialog API is the right
tool — and the reason the VoidSprite counterpart cannot use the
equivalent approach:

- Aseprite **has** a first-class interactive dialog API. `new_character.lua`
  asks the user for hair/eye/outfit/accent colors, hairstyle, and
  accessories at runtime, in the app, with native controls.
- The VoidSprite SDK gives parameter dialogs **to filters only**
  (`filterNew*Parameter` → host-generated dialog; verified in
  `references/sdk-layout.md` of the `voidsprite-plugin-dev` skill).
  `registerEditorAction` — the path `vs-character-creator` uses — takes
  a name and a callback and offers **no parameter UI at all**. There is
  no dialog to pop, so the plugin's inputs live in a config file read at
  startup.

Same job (design a character from a fixed option set), same output
contract (96 × 192 + 24-frame sheet), different input paths — dictated
by each host's SDK shape, not by preference. When porting an interactive
Aseprite script to a VoidSprite editor action, budget for the config-file
UX from the start; when going the other direction, the dialog is
available and expected.

### Parametric bodies — geometry, not palette

`new_character.lua` (and its VoidSprite twin `vs-character-creator`)
generate **parametric** characters, not palette swaps. The parameter set:

- **Identity:** `name`, `body_type` (`feminine` / `masculine`),
  `face`, `eye_color`, `hair_color`, `hairstyle`.
- **Body geometry:** `weight`, `height`, `bust_size` (feminine bodies)
  or `chest_size` (masculine bodies).
- **Wardrobe:** `outfit` (`tech-jacket` / `casual` / `formal` /
  `athletic`), `outfit_color`, `accent`, `accessories`.

The geometry parameters move real pixels — that is the contract:

- **Weight** changes torso width (roughly 28 / 36 / 48 px across the
  light / average / heavy settings).
- **Height** changes head-to-body proportion: short ≈ 1:4, average ≈
  1:5.5, tall ≈ 1:7 (head 48 / 35 / 27 px respectively).
- **Outfits** are procedural variants, not recolors — tech-jacket,
  casual, formal, and athletic each draw distinct garments.

Invalid combinations fail loudly instead of silently producing a wrong
sprite: a masculine body with a `bust_size` value, or a feminine body
with `chest_size`, is an error, as is an unknown outfit or body type.
The headless path validates all params before generating; the dialog
path switches the visible bust/chest control when `body_type` changes
and re-validates on Generate. Keep this framing honest when writing the
character docs: this is a richer parametric scaffold and an artist's
starting point, not a finished character renderer.
