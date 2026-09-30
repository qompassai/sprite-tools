---
name: "aseprite-scripting"
description: "Automate Aseprite with Lua scripting and the headless CLI in Matt's Tiger Style: the batch-mode script contract, argv-array CLI discipline, headless pipeline patterns, and the sprite-tools sheet pipeline (assemble/verify/export/montage plus the interactive new_character.lua creator). Aseprite has no native plugin SDK — Lua scripts and the CLI are the entire automation surface. See references/sheet-pipeline.md for the repo's five scripts and their contracts."
---

# Aseprite Scripting

> **Prerequisite:** `tiger-style-aseprite` (sibling skill at
> `~/workspace/skills/tiger-style-aseprite/SKILL.md`) — it holds the
> fully verified CLI flag table, the per-version scripting API table, and
> the LibreSprite divergence table. This skill is the repo-specific layer
> on top: the batch contract distilled to what the pipeline needs, the
> headless invocation patterns, and the `scripts/aseprite/` pipeline
> contracts. When the two disagree on a flag or API member, the sibling
> skill's verified tables win.

## Purpose

Aseprite exposes no native plugin SDK — its "extensions" are Lua scripts
zipped with a `package.json` (`.aseprite-extension`). The scripting API
plus the headless CLI are therefore the *entire* automation surface: the
CLI opens files, selects frames and layers, and runs scripts; Lua does the
pixel work. This skill covers that surface as used by the sprite-tools
repo: batch-mode scripts driven by `--script-param`, invoked with argv
arrays under bounded timeouts, with the exit code as the success signal.
Official sources: CLI <https://www.aseprite.org/docs/cli/>, scripting
<https://www.aseprite.org/docs/scripting/>, API index
<https://www.aseprite.org/api/>, API changes
<https://aseprite.com/api/Changes>.

Plain-language version: Aseprite can be driven two ways — clicking
through its UI, or running it headless with a script that does the
clicking for you. This skill is about the second way: small Lua programs
that open a sprite file, rearrange or check its pixels, and exit with a
clear pass/fail, all wired so a build pipeline (or another agent) can run
them unattended.

## Workflow

1. Detect and probe the binary (`ASEPRITE_BIN`, then PATH). Never assume
   it exists or is the right version; require ≥ 1.3 for the pipeline
   scripts.
2. Reach for direct CLI flags when they cover the job (`--save-as`,
   `--sheet`, `--data`, `--scale`); use `--script` only for pixel-level
   logic the flags cannot express.
3. Generate the Lua into a temp file with a distinctive suffix
   (`_sprite_tools_<name>.lua`), invoke with an argv array, bounded
   timeout (default 120 s), bounded output capture.
4. Check the exit code: `0` is success; anything else — or a Lua error on
   stderr — is failure, even if output files exist. Verify expected
   outputs exist after a `0`.
5. Remove the temp script whether the run succeeded or failed.
6. For repo pipeline scripts (`scripts/aseprite/`), validate inputs and
   assert the sheet contract (96×192 frames, 4×6 grid, 384×1152 sheet,
   180 ms/frame) before producing output.

## Operating Rules

### The batch contract (distilled)

Every script run via `-b --script` must satisfy all of these. The full
contract with rationale lives in `tiger-style-aseprite`; this is the
non-negotiable core:

1. **No UI, ever.** In batch mode `app.isUIAvailable` is `false` and
   `Dialog()` returns `nil` (since v1.2.35). Never call `Dialog`,
   `app.alert`, or `app.tip` in a batch script — status goes to stdout
   via `print()`, failure via `error()` or a non-zero exit.
2. **Input through `app.params`.** CLI `--script-param name=value` pairs
   arrive as strings in `app.params`. Coerce and validate every param at
   the top; fail loudly on missing or malformed values. `--script-param`
   must precede `--script` on the command line.
3. **`app.sprite` can be `nil`.** Check before touching. Never assume the
   CLI opened what you expected.
4. **Color mode first.** Pixel math with `app.pixelColor.rgbaR/G/B/A`
   assumes RGBA — read `sprite.colorMode` and compare to `ColorMode.RGB`
   before interpreting bytes. RGB components are not premultiplied.
5. **Transactions for edits.** Wrap mutations in
   `app.transaction([label], function() ... end)` for atomicity and undo
   grouping.
6. **print/error discipline.** `print()` for status and machine-readable
   results on stdout; `error(msg)` for failures (non-zero exit is the
   failure signal the pipeline reads). Never print a success message and
   exit non-zero, or vice versa.
7. **Version guards, no deprecated aliases.** Gate post-1.3-beta API
   features on `app.apiVersion`; never use `app.activeSprite`,
   `app.activeLayer`, `app.activeFrame`, `app.activeCel`,
   `app.activeImage`, `app.activeTag`, `app.activeTool`, `app.activeBrush`,
   or `putPixel`/`putImage`/`putSprite`.
8. **Stdlib limits.** Aseprite embeds Lua 5.3; `os.exit` and `os.tmpname`
   are unavailable, `os.execute`/`io.open` may prompt — avoid them in
   batch scripts.

### Headless pipeline patterns

- **argv arrays always** — one element per argument, never an interpolated
  shell string. Paths with spaces stay single elements.
- **Option ordering:** selection/export options (`--tag`,
  `--frame-range`, `--layer`, `--all-layers`, …) go *before* the input
  `.aseprite` file; `--script-param name=value` goes *before* `--script`;
  `--scale` applies to sprites opened *before* it. Wrong order is a
  silent wrong result, not an error — this is the most common pipeline
  bug.
- **Exit-code contract:** `0` means the script completed; verify the
  expected output files exist before reporting success. A non-zero exit
  or a Lua traceback on stderr fails the run even when partial outputs
  exist.
- **Temp-file hygiene:** generated scripts get a distinctive temp suffix
  and are deleted after the run, success or failure. Intermediate images
  the pipeline produces go to the repo's scratch layout, never beside the
  source `.aseprite` unless the invocation names that path explicitly.
- **Bounded everything:** explicit timeout per run, bounded stdout/stderr
  capture. A script that hangs must die by timeout, not by operator
  patience.

### Interactive scripts and the Dialog exception

`new_character.lua` is the one script allowed to use Aseprite's real
`Dialog` API — color pickers, a dropdown, checkboxes, a Generate button —
because its job is interactive character design, and the Dialog API is
the right tool for that job. The rule:

- The script detects batch mode (`app.isUIAvailable == false`, or
  `--script-param headless=1`) and degrades to a pure `app.params`
  path: every dialog field has a same-named script-param override.
  Dialog-first, params-fallback — never params-only for the interactive
  case, never dialog-only for the pipeline case.
- This is also the documented asymmetry with the VoidSprite side (see
  `references/sheet-pipeline.md`): Aseprite *has* an interactive dialog
  API, so its character creator asks the user at runtime; the VoidSprite
  SDK gives parameter dialogs to filters but not to editor actions, so
  the `vs-character-creator` plugin reads a config file for the same
  inputs. Same job, different input paths — dictated by each host's SDK
  shape, not by taste.

### LibreSprite boundary

One paragraph, then the pointer: LibreSprite 1.1 is not API-compatible
with Aseprite 1.3 scripts — different `app` surface (the names Aseprite
deprecated), `app.createDialog()` instead of `Dialog()`, `app.version` a
string, no documented `app.params`/`app.apiVersion`/`app.isUIAvailable`,
a reduced `Image`/`Sprite` API, and no `--script-param` in its scripting
docs. The pipeline scripts target Aseprite ≥ 1.3; porting any of them to
LibreSprite means consulting the full divergence table in
`tiger-style-aseprite`'s `references/aseprite-scripting-api.md` and
writing a feature-detected compat path — not assuming the flags carry
over.

## Activation

<skill_resources>
</skill_resources>

- **Dedup:** the harness tracks activated skills per session. If this skill
  is already in context, skip re-injection — never load it twice.
- **Subagent delegation:** pipeline runs (assemble → verify → export →
  montage) are the natural delegation unit — a build subagent runs the
  chain in the background and reports gate numbers. Do not delegate the
  review of a new script's batch-contract compliance; that gets a direct
  pass.

## Review Checklist

- [ ] Binary detected, version probed and reported; every flag is in the
      sibling skill's verified table.
- [ ] argv arrays everywhere; `--script-param` before `--script`;
      selection options before the input file.
- [ ] Bounded timeout and output; exit code checked; outputs verified
      after exit 0; temp scripts removed either way.
- [ ] Script satisfies the batch contract: no UI calls, `app.params`
      validated, `app.sprite` nil-checked, colorMode checked before
      pixel math, mutations in `app.transaction`, print/error discipline,
      version guards, no deprecated aliases.
- [ ] Interactive scripts (`new_character.lua`) degrade cleanly to the
      `headless=1` param path when `app.isUIAvailable` is false.
- [ ] Sheet contract asserted where produced/consumed: 96×192 frames,
      4×6 grid, 384×1152 sheet, 180 ms/frame, six mood tags.
- [ ] LibreSprite divergence consulted if the target might be
      LibreSprite.
