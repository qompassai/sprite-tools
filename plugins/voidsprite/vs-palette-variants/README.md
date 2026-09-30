# vs-palette-variants — "palette variant tools"

A VoidSprite native plugin (Rust `cdylib`, zero dependencies) providing one
filter, **"Palette: hue shift variant"**: it converts every visible pixel of
the active layer from RGB to HSV, rotates the hue by a configurable number of
degrees, optionally rescales saturation, and converts back — generating
alternate palette variants of a sprite sheet (team colors, day/night tints)
without touching the alpha channel.

## Parameters

The host builds the parameter dialog from the declared parameters, so these
appear automatically in the filter UI:

| Parameter           | Type | Range    | Default | Effect                                                     |
|--------------------|------|----------|---------|------------------------------------------------------------|
| `hue shift degrees`| int  | −180..180| 0       | Rotates hue; wraps around the color wheel (350° + 20° = 10°)|
| `saturation %`     | int  | 0..200   | 100     | Scales saturation; 0 = grayscale, 200 = doubled (clipped)  |

Behavior details:

- Alpha is preserved byte-exact.
- Fully transparent pixels (alpha 0) are skipped entirely — invisible data
  is never recolored.
- Runs on the active layer, in place, inside the host's own undo scope.
- Non-RGBA layers and out-of-bounds geometry are rejected with a host error
  notification; the layer is left unchanged.

## Build

Prerequisites: Rust 1.96.0 (pinned in `rust-toolchain.toml`; rustup
resolves it), no other dependencies.

```sh
cd plugins/voidsprite/vs-palette-variants
cargo build --release   # zero warnings; panic = "abort"
```

The output is `target/release/libvs_palette_variants.so`.

## Test

```sh
cargo test
```

Unit tests cover the pure HSV math (no host needed):

- RGB→HSV→RGB round-trip on primaries, secondaries, white, black
- +120° maps red→green and blue→red; −120° maps red→blue
- alpha preserved byte-exact; transparent pixels untouched
- saturation 0 → grayscale; hue wrap-around (350° + 20° = 10°)
- host-failure paths (null args, dead host table) leave the layer alone

## Install

Copy the built `.so` into VoidSprite's plugin directory; it loads at
startup ("Loaded plugin: palette variant tools"). The filter appears in the
filter list.

```sh
mkdir -p ~/.config/voidsprite/plugins
cp target/release/libvs_palette_variants.so ~/.config/voidsprite/plugins/
```

## ABI notes

- Exports the six host entry points with `#[unsafe(no_mangle)] pub extern "C"`:
  `pluginInit`, `voidspriteSDKVersion`, `getPluginName`,
  `getPluginVersion`, `getPluginDescription`, `getPluginAuthors`.
- `VoidspriteSDK` is a `#[repr(C, packed(1))]` transcription of
  `voidsprite_sdk_c.h`. Every field offset and both struct sizes are
  asserted in `const` blocks against values probed from the real C header
  with gcc on the build machine (288-byte table, 36 eight-byte slots;
  12-byte `VSPLayerInfo`), so an SDK layout change fails the build instead
  of silently mis-calling the host.
- All host table slots are `Option<unsafe extern "C" fn>`: a missing host
  function degrades to a safe fallback instead of a null call.
- `unsafe` is confined to thin host wrappers in the `sdk` module, each with
  a documented safety contract; the `color` module is `#![forbid(unsafe_code)]`.
- Nothing unwinds across the FFI boundary: the filter callback only calls
  panic-free code, and the release profile sets `panic = "abort"`.

## Limits

Honest boundaries of what this filter can and cannot do:

- **The shift is uniform.** Every visible pixel's hue rotates by the same
  amount. It cannot preserve skin tones or accent colors while shifting
  others — everything rotates together. A +120° shift that turns blue armor
  green also turns every other blue thing green.
- **Extreme saturation values clip.** Saturation is clamped to the valid
  range after scaling, so 200% on an already-vivid pixel just pins it at
  full saturation (detail in the most vivid areas flattens out), and 0%
  always collapses to gray.
- **Grayscale pixels are unaffected by hue shift.** A pixel with zero
  saturation has no hue to rotate, so it passes through unchanged. This is
  correct behavior, not a bug.
- **No per-region control.** For recoloring one part of a sprite (a cape but
  not the boots), use selections or separate layers instead of this filter.
