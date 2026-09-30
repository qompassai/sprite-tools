# vs-lightshow — Rust port (1.1.0)

A Rust re-implementation of the **vs-lightshow** VoidSprite plugin (v1.1.0).
It provides the same two filters and one editor action as the C original, in
`src/lib.rs` with zero dependencies:

- **Filter: `Light-show: blink eyes`** — vertically squashes the two
  configured eye boxes by the blink amount. At 100% every row of a box maps
  to its centre row, which reads as a closed eye using only existing pixels,
  so it works on visors too (nothing is painted, only pixels that are
  already there get moved).
- **Filter: `Light-show: breathing shift`** — shifts the frame vertically by
  the configured pixel amount, replicating edge rows.
- **Editor action: `Light-show: export sheet + JSON`** — packs the first
  `cols × rows` session frames into `<basename>.png` plus a `<basename>.json`
  sidecar documenting the atlas contract for the light-show game.

## Exports

The crate builds as a `cdylib` exporting exactly six symbols, which is the
full VoidSprite plugin contract:

| Symbol                | Purpose                                  |
| --------------------- | ---------------------------------------- |
| `pluginInit`          | Captures the SDK table; registers the two filters and the editor action |
| `voidspriteSDKVersion`| SDK version the plugin targets (`1`)     |
| `getPluginName`       | `"light-show sprite tools"`              |
| `getPluginVersion`    | `"1.1.0"`                                |
| `getPluginDescription`| One-line description                     |
| `getPluginAuthors`    | `"Pax (Qompass AI)"`                     |

## Config format (`~/.config/voidsprite/lightshow_export.cfg`)

`key=value` lines; `#` starts a full-line comment; blank lines are ignored.
A missing file is fine — defaults stand. Any malformed line, unknown key, or
out-of-range value aborts the load loudly (the export shows an error) rather
than silently keeping defaults.

| Key         | Range                  | Default        | Meaning                              |
| ----------- | ---------------------- | -------------- | ------------------------------------ |
| `cell_w`    | 1–2048                 | 96             | Frame width in px                    |
| `cell_h`    | 1–2048                 | 192            | Frame height in px                   |
| `cols`      | 1–64                   | 4              | Sheet columns                        |
| `rows`      | 1–64                   | 6              | Sheet rows                           |
| `frame_ms`  | 1–10000                | 180            | Frame duration                       |
| `moods`     | 1–32 tokens            | six defaults   | Comma-separated mood names           |
| `basename`  | token chars, < 1024    | `sheet`        | PNG/JSON stem, no extension          |
| `output_dir`| any path, < 1024      | `~/lightshow-export` | Output directory (one level is created) |

`cols × rows` is capped at 128 frames. Mood/basename tokens may only contain
`A–Z a–z 0–9 _ - .` so the values are always safe as filenames and as JSON
strings (no escaping needed). Defaults match the light-show game constants.

## Build, test, lint

Toolchain: pinned via `rust-toolchain.toml` to `nightly-2026-09-25`
(edition 2024; the crate needs no nightly features — the pin just fixes the
toolchain). Keep build artifacts out of the tree:

```sh
cargo fmt -- --check
CARGO_TARGET_DIR=/tmp/vs-lightshow-rs-target cargo build --release
CARGO_TARGET_DIR=/tmp/vs-lightshow-rs-target cargo clippy --release --all-targets
CARGO_TARGET_DIR=/tmp/vs-lightshow-rs-target cargo test --release
```

All four gates pass clean: `cargo fmt --check` reports no diff, the release
build and clippy run with zero warnings (the crate is `#![deny(warnings)]`),
and the unit tests cover the blink/shift mapping, the config parser
(including malformed/over-range/adversarial inputs), the PNG encoder (with a
decode round-trip), and the JSON contract. The `nm -D` check on the built
`.so` shows exactly the six symbols above as `T`.

## Install

VoidSprite loads every `.so` in `~/.config/voidsprite/plugins/`. Cargo emits
`libvs_lightshow.so`; the host expects the plugin file to be named
`vs_lightshow.so`, so install it under that name:

```sh
cp /tmp/vs-lightshow-rs-target/release/libvs_lightshow.so \
   ~/.config/voidsprite/plugins/vs_lightshow.so
```

## Port notes

- **ABI layout.** The SDK table is a 288-byte C struct with 1-byte packing
  (`#pragma pack(push, 1)` in the SDK header). This port mirrors it as
  `#[repr(C, packed)] VoidSpriteSdk` and pins every member offset plus both
  struct sizes as compile-time `const` assertions (values measured from the
  real header on x86_64-linux-gnu). If the header ever drifts, the crate
  fails to compile instead of corrupting memory.
- **Panic policy.** The release profile sets `panic = "abort"`. No panic
  can cross the FFI boundary, and the filter/action entry points validate
  every host pointer and every host table entry before use. A null table
  entry aborts the current operation with an editor notification; the C
  original would segfault.
- **PNG encoder.** The C plugin uses the vendored `stb_image_write`. This
  port ships a dependency-free encoder (8-bit RGBA, raw DEFLATE stored
  blocks in a zlib stream, per RFC 2083). Pixels are byte-identical; only
  the compressed bytes differ (no Huffman coding), so files are a little
  larger and every file is verified chunk-by-chunk in the test suite.
- **Config file access.** The C plugin opens the config through the host's
  `util_fopenUTF8`; on Linux that helper is byte-identical to `fopen`, so
  this port reads the file with `std::fs::File` and treats paths as raw
  bytes — no FFI needed.
- **JSON generator tag.** The sidecar reports
  `"generator": "vs-lightshow 1.1.0"` and the same `frames`/`moods` contract
  as the C writer, including `"mood": "unknown"` for rows past the mood list.
- `c-orig/` holds verbatim copies of the C sources this port was written
  from (`vs_lightshow.c`, `voidsprite_sdk_c.h`, `voidsprite_sdk.h`,
  `build.sh`, `gen_c_header.sh`, the C `README.md`).
