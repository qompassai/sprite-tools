# vs-sheet-qa — "sheet QA tools"

A native VoidSprite plugin (Rust `cdylib`, v1.0.0) for sprite-sheet QA.
Display name: **sheet QA tools**. Authors: **Pax (Qompass AI)**.

It registers two editor actions in VoidSprite's navigation bar:

1. **Sheet QA: extract cell** — crops one cell out of the sheet image
   (the session's active frame, flattened) into a standalone PNG for
   close inspection.
2. **Sheet QA: diff cells** — compares two cells pixel-by-pixel and
   writes a diff PNG where differing pixels are opaque magenta and
   identical pixels are dimmed, reporting the differing-pixel count.
   Built for animation-consistency checks (e.g. verifying only the
   mouth changed between two idle frames).

Editor actions get no parameter dialog from the host, so both actions
read their parameters from `~/.config/voidsprite/sheetqa.cfg`. Geometry
defaults match the light-show contract (96x192 cells, 4 cols x 6 rows);
the cell selectors have no defaults — a missing selector aborts the
action with a clear message. Unknown keys and malformed values abort
loudly, matching the strict-parse discipline of `vs_lightshow.c`.

## Config format (`~/.config/voidsprite/sheetqa.cfg`)

```ini
# Geometry (optional; defaults shown)
cell_w=96
cell_h=192
cols=4
rows=6

# Extract action: which cell to crop (REQUIRED, 0-based, no defaults)
row=2
frame=3

# Diff action: which two cells to compare (REQUIRED, 0-based, no defaults)
row_a=0
frame_a=0
row_b=0
frame_b=1

# Optional: where the PNGs go (default: ~/.config/voidsprite/sheetqa-output)
output_dir=/home/phaedrus/sheetqa-output
```

- Blank lines and `#` comments are allowed. Keys are case-sensitive.
- Integer values are strict: no floats, no hex, no trailing junk.
  Ranges: `cell_w`/`cell_h` in [1, 2048], `cols`/`rows` in [1, 64],
  selectors >= 0, and `cols * rows` <= 256.
- A **missing file is not an error**: geometry defaults stand, but the
  selectors are still required, so the action reports e.g.
  `'row=' is required in sheetqa.cfg (no default); set it and retry`.
- An **unknown key or malformed value aborts**: the action posts an
  error notification naming the key and line number. A typo'd key never
  silently keeps a default.

## Output

- Extract writes `cell_r{row}_f{frame}.png` into the output dir.
- Diff writes `diff_r{ra}_f{fa}_vs_r{rb}_f{fb}.png` plus a success
  notification like `37 of 18432 pixels differ (0.2%) -> /path/to/png`.
- Writes are atomic (temp file + rename); the directory is created if
  missing.

## Build

Zero dependencies. Release profile uses `panic = "abort"`.

```sh
cargo build --release        # -> target/release/libvs_sheetqa.so
cargo test                   # unit tests (pixel math, config parser, PNG encoder)
cargo fmt --check            # 4-space indent, 100 columns, edition 2024
```

Install for VoidSprite by copying the `.so` into the host's plugin
directory (filename is yours to choose):

```sh
cp target/release/libvs_sheetqa.so ~/.config/voidsprite/plugins/vs_sheetqa.so
```

## SDK ABI

The plugin transcribes the SDK v1 C table (`voidsprite_sdk_c.h`) as a
`#[repr(C)]` Rust struct. Layout is pinned by compile-time assertions
in `src/lib.rs`:

- `size_of::<VoidSpriteSdk>() == 288` (36 function pointers x 8 bytes)
- every field offset asserted against values computed from the C header
  with `offsetof` on the build host (see below)
- `size_of::<VspLayerInfo>() == 12`, offsets 0/4/8

To re-verify against a new SDK header, compile the offset probe:

```sh
gcc -o /tmp/sdk_off /tmp/sdk_off.c   # source: probe in the build notes
/tmp/sdk_off
```

and compare with the assertions in `src/lib.rs`. If the header ever
moves a field, compilation fails loudly instead of corrupting the
table.

## Safety notes

- `unsafe` is confined to three thin wrappers: copying the SDK table
  in `pluginInit`, copying the raw pixel buffer (with an owning
  `SdkLayer` guard), and reading the three `i32` fields of the
  host-allocated `VSPLayerInfo` before `util_free`. Each documents its
  obligations at the use site.
- No panic crosses the FFI boundary: every `extern "C"` entry point
  funnels fallible work through `Result`; failures become host error
  notifications (stdout fallback when the notification entry is
  unavailable). There is no `unwrap`/`expect` on runtime paths.

## Honest limits

- SDK v1 exposes **no API to open a new editor image/tab from a
  plugin**, so QA results land as PNG files on disk rather than new
  images in the editor. This is a host limitation, not a bug.
- The inspected sheet is the session's **active frame, flattened**
  (`editorFlattenImage`); the plugin does not enumerate frames.
- The flattened frame must be RGBA and at least `cols*cell_w` by
  `rows*cell_h`; anything else aborts with a message.
- Diff compares raw pixel values (0xAARRGGBB), including alpha.
  Semi-transparent edge pixels that differ by one LSB count as
  differing.
- PNGs are written with stored (uncompressed) DEFLATE blocks: valid
  PNG, larger than a compressed one. Fine for QA-sized cells.
- `HOME` must be set (or `output_dir=` given) for the default paths.
