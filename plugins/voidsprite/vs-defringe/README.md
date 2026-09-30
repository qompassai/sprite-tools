# vs-defringe — "defringe tools" for VoidSprite

A native Rust plugin (VoidSprite SDK v1, `cdylib`) that cleans up
AI-generated pixel art: it finds stray semi-transparent pixels floating
in transparent areas — the fringe that generators leave around sprites —
and deletes them (sets them fully transparent).

- Display name: **defringe tools**
- Version: **1.0.0**
- Authors: **Pax (Qompass AI)**
- Filter: **Defringe: remove noise pixels**
- Zero dependencies. Release profile uses `panic = "abort"` so a panic
  can never unwind across the FFI boundary into the host.

## The heuristic

A pixel is treated as noise when **both** hold:

1. Its alpha is strictly below the **alpha threshold** (and above zero —
   fully transparent pixels are already clean and are never touched), and
2. Within its **radius** neighborhood (a Chebyshev square of side
   `2*radius + 1`, center excluded) there are fewer than **min neighbors**
   "opaque-ish" pixels — pixels whose alpha is at or above the threshold.

Noise pixels are set to fully transparent (`0x00000000`).

Concretely, with the defaults (threshold 128, radius 1, min-neighbors 1):
a lone semi-transparent pixel in empty space is deleted; the same pixel
touching any pixel with alpha ≥ 128 survives; fully opaque pixels are
never candidates at all.

Two properties worth knowing:

- **Decisions are made from a snapshot.** The filter copies the layer,
  classifies every pixel against the *original* image, then writes. A
  removed pixel can never cause its neighbor to be removed in the same
  pass (no cascade deletions).
- **Operates on the active layer only.** The host passes the active layer
  to the filter; other layers are untouched.

## Parameters (host-built dialog)

| Parameter         | Range  | Default | Meaning                                              |
|-------------------|--------|---------|------------------------------------------------------|
| `alpha threshold` | 1–254  | 128     | Below this alpha, a pixel is a noise *candidate*     |
| `radius`          | 1–3    | 1       | Chebyshev radius of the neighborhood window          |
| `min neighbors`   | 1–8    | 1       | Opaque-ish neighbors required to *keep* a candidate |

Values read back from the host are clamped into range defensively
(`DefringeParams::from_raw`); the dialog already constrains them, so a
clamp firing means a misbehaving host, and a sane clamp beats failing
the whole filter.

Radius 1 examines up to 8 neighbors; radius 3 up to 48. The window is
clamped at layer edges (no wrapping).

## Build

Prerequisites: a Rust toolchain (pinned: see `rust-toolchain.toml`;
validated on 1.96.0 stable) and — for the load test — VoidSprite plus
Xvfb on the test machine.

```sh
cargo build --release        # zero warnings expected
```

The release artifact is `target/release/libvs_defringe.so`.

### Install (manual)

```sh
mkdir -p ~/.config/voidsprite/plugins
cp target/release/libvs_defringe.so ~/.config/voidsprite/plugins/
```

VoidSprite loads it at startup and logs
`Loaded plugin: defringe tools`. The filter appears in the filter menu
as **Defringe: remove noise pixels**, with the three parameters above in
a dialog. Filters run inside the host's own undo scope, so the operation
is undoable.

## Test

```sh
cargo test
```

The suite has two halves:

- **Pure core** (`run_defringe` / the noise predicate on synthetic
  buffers, no host involved): isolated semi-transparent pixel removed;
  same pixel with an opaque neighbor kept; fully opaque pixel never
  touched; fully transparent buffer untouched; radius-2 behavior on a
  2-pixel cluster (a pixel exactly 2 away from solid art survives at
  radius 2 but not at radius 1; semi-transparent cluster mates never
  count as structure for each other); `min neighbors = 2` needs two
  solid neighbors; parameter clamping at the range edges.
- **FFI boundary** (a fake in-process host implementing just the SDK
  entries the plugin uses): `pluginInit` registers the filter under the
  right display name with the three correctly-ranged parameters; the
  real `extern "C"` filter callback cleans a synthetic RGBA layer and
  reports the count; a non-RGBA (indexed) layer is refused with an error
  and left byte-identical.

## Limits (read before running this on real art)

- **It cannot tell intentional single-pixel detail from noise.**
  Sparkles, stars, single-pixel highlights, dithering — at radius 1 +
  min-neighbors 1 the filter is aggressive and will eat them. If your
  sprite has deliberate isolated pixels, raise the alpha threshold, or
  don't run the filter on that layer.
- **It only cleans fringes against transparency.** Noise *embedded
  inside* opaque regions is invisible to this heuristic by design: every
  candidate's neighbors are opaque-ish, so interior pixels are always
  kept. It is a fringe cleaner, not a general denoiser.
- **It works per layer.** A halo spread across multiple layers needs one
  run per layer; the filter never looks at (or touches) other layers.
- **Always run on a copy.** The operation is destructive by nature —
  deleted pixels are gone. The host provides undo for the filter, but a
  backup layer (or file) is the only real safety net.

## ABI notes

- Exports, all `extern "C"`: `pluginInit`, `voidspriteSDKVersion`,
  `getPluginName`, `getPluginVersion`, `getPluginDescription`,
  `getPluginAuthors`.
- `src/lib.rs` carries a `#[repr(C)]` transcription of `struct
  voidspriteSDK` with compile-time `size_of`/`offset_of` assertions
  against values measured from the real `voidsprite_sdk_c.h` (size 288,
  fields at 8-byte strides; see the `const _` assertion block). The C
  header packs the struct to align 1 while Rust uses align 8 — every
  offset and the total size are identical, so the transcription reads the
  host's table correctly.
- `unsafe` is confined to three thin wrappers: dereferencing the SDK
  table pointer in `pluginInit`, reading the `VSPLayerInfo` out-pointer
  (freed with the host's `util_free` on every path), and borrowing the
  host-owned pixel buffer as a slice. Each site documents the host
  contract it relies on.
- Non-RGBA (indexed) layers are refused: their "pixels" are palette
  indices, not `0xAARRGGBB`, and interpreting them as alpha values would
  corrupt art.
- Hard caps: dimensions above 32768 px per axis are rejected; layers
  above 2²⁶ pixels (~256 MiB snapshot) are rejected with a notification
  instead of attempting the allocation.

## License

Apache-2.0, matching the organization's convention.
