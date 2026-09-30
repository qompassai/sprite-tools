//! vs-defringe — "defringe tools" native plugin for VoidSprite (SDK v1).
//!
//! One filter, "Defringe: remove noise pixels": deletes stray
//! semi-transparent, isolated pixels — the typical fringe AI generators
//! leave around pixel art — from the active RGBA layer by setting them
//! fully transparent.
//!
//! Layout of this crate, top to bottom: plugin identity, the SDK
//! transcription (the FFI contract), validated parameters, the pure
//! defringe core (no host, no `unsafe`, fully unit-testable), thin FFI
//! wrappers, the six exported entry points, and the tests.
//!
//! Safety posture: `unsafe` appears only where the host hands us raw
//! memory — dereferencing the SDK table pointer once in `pluginInit`,
//! reading the `VSPLayerInfo` out-pointer, and building a slice over the
//! host-owned pixel buffer. Each of those sites carries a local SAFETY
//! comment naming the host contract it relies on. Panics never cross the
//! FFI boundary: the exported functions contain no `unwrap`, no `expect`,
//! and no indexing that the preceding validation has not already bounded;
//! release builds additionally set `panic = "abort"`.

#![deny(unsafe_op_in_unsafe_fn)]

use core::ffi::{c_char, c_double, c_int, c_void};
use core::mem::{align_of, offset_of, size_of};
use core::sync::atomic::{AtomicPtr, Ordering};
use std::ffi::CString;

/* ------------------------------------------------------------------ */
/* Plugin identity.                                                    */
/* ------------------------------------------------------------------ */

/// Display name shown by the host at load ("Loaded plugin: defringe tools").
const PLUGIN_NAME: &[u8] = b"defringe tools\0";
const PLUGIN_VERSION: &[u8] = b"1.0.0\0";
const PLUGIN_DESCRIPTION: &[u8] = b"Defringe filter: removes stray semi-transparent, isolated noise pixels (AI-generation fringe) from the active RGBA layer.\0";
const PLUGIN_AUTHORS: &[u8] = b"Pax (Qompass AI)\0";

/// Name under which the filter registers (shown in the host's filter menu).
const FILTER_DISPLAY_NAME: &[u8] = b"Defringe: remove noise pixels\0";

/// Notification title for success/error popups.
const NOTIFY_TITLE: &[u8] = b"Defringe\0";

/// SDK version this plugin targets (matches `VS_SDK_VERSION` in the C header).
const VS_SDK_VERSION: c_int = 1;

/// Layer-type bit from the C header (`VSP_LAYER_RGBA`). Only RGBA layers
/// carry 0xAARRGGBB pixels, so any other layer type is refused.
const VSP_LAYER_RGBA: i32 = 0x01;

/* ------------------------------------------------------------------ */
/* Filter parameters (declared to the host; it builds the dialog).     */
/* ------------------------------------------------------------------ */

/// Parameter names are part of the plugin's UI contract; keep them stable.
const PARAM_ALPHA_THRESHOLD: &[u8] = b"alpha threshold\0";
const PARAM_RADIUS: &[u8] = b"radius\0";
const PARAM_MIN_NEIGHBORS: &[u8] = b"min neighbors\0";

const ALPHA_THRESHOLD_MIN: c_int = 1;
const ALPHA_THRESHOLD_MAX: c_int = 254;
const ALPHA_THRESHOLD_DEFAULT: c_int = 128;

const RADIUS_MIN: c_int = 1;
const RADIUS_MAX: c_int = 3;
const RADIUS_DEFAULT: c_int = 1;

const MIN_NEIGHBORS_MIN: c_int = 1;
const MIN_NEIGHBORS_MAX: c_int = 8;
const MIN_NEIGHBORS_DEFAULT: c_int = 1;

/* ------------------------------------------------------------------ */
/* Hard bounds: every allocation and loop in the filter is capped.      */
/* ------------------------------------------------------------------ */

/// Largest layer dimension accepted per axis (px). Real pixel-art layers
/// are orders of magnitude smaller; anything beyond is a corrupt host
/// value, not art.
const DIM_MAX: i32 = 32_768;

/// Largest pixel count accepted. The algorithm snapshots the layer at 4
/// bytes/px, so this caps the transient allocation at 256 MiB.
const PIXEL_COUNT_MAX: usize = 1 << 26;

/// Fully transparent; noise pixels are rewritten to this.
const PIXEL_TRANSPARENT: u32 = 0x0000_0000;

/* ------------------------------------------------------------------ */
/* SDK transcription: mechanical Rust rendering of voidsprite_sdk_c.h   */
/* (VoidSprite SDK v1). Field order, types, and offsets match the C    */
/* header exactly; see the const assertions below.                     */
/* ------------------------------------------------------------------ */

/// Opaque host handle: a VoidSprite layer. Never dereferenced; only
/// passed back to SDK functions.
pub enum VspLayer {}

/// Opaque host handle: a registered filter (the dialog handle).
pub enum VspFilter {}

/// Opaque host handle: the editor session.
pub enum VspEditorContext {}

/// Opaque host handle: a registered brush.
pub enum VspBrush {}

/// Opaque host handle: a registered file exporter.
pub enum VspFileExporter {}

/// Opaque C `FILE` (only ever passed back to host functions).
pub enum CFile {}

/// `void (*filterFunction)(VSPLayer* layer, VSPFilter* filter)`
pub type FilterCallback = Option<extern "C" fn(layer: *mut VspLayer, filter: *mut VspFilter)>;
/// `VSPLayer* (*importFunction)(char* path)`
pub type LayerImportFn = Option<extern "C" fn(path: *mut c_char) -> *mut VspLayer>;
/// `bool (*canImportFunction)(char* path)`
pub type CanImportFn = Option<extern "C" fn(path: *mut c_char) -> bool>;
/// `bool (*exportFunction)(VSPLayer* layer, char* path)`
pub type LayerExportFn = Option<extern "C" fn(layer: *mut VspLayer, path: *mut c_char) -> bool>;
/// `bool (*canExportFunction)(VSPLayer* layer)`
pub type CanExportFn = Option<extern "C" fn(layer: *mut VspLayer) -> bool>;
/// `void (*clickAt)(VSPBrush*, VSPEditorContext*, int x, int y)`
pub type BrushClickFn =
    Option<extern "C" fn(brush: *mut VspBrush, editor: *mut VspEditorContext, x: c_int, y: c_int)>;
/// `void (*dragAt)(VSPBrush*, VSPEditorContext*, int xFrom, int yFrom, int xTo, int yTo)`
pub type BrushDragFn = Option<
    extern "C" fn(
        brush: *mut VspBrush,
        editor: *mut VspEditorContext,
        x_from: c_int,
        y_from: c_int,
        x_to: c_int,
        y_to: c_int,
    ),
>;
/// `void (*releaseAt)(VSPBrush*, VSPEditorContext*, int x, int y)`
pub type BrushReleaseFn =
    Option<extern "C" fn(brush: *mut VspBrush, editor: *mut VspEditorContext, x: c_int, y: c_int)>;
/// `void (*action)(VSPEditorContext* editor)`
pub type EditorActionFn = Option<extern "C" fn(editor: *mut VspEditorContext)>;

/// `struct voidspriteSDK` — the host's function table.
///
/// Each C function pointer becomes `Option<extern "C" fn(..)>`, which is
/// null-pointer optimized, so NULL table entries are representable and
/// every use site checks for them.
///
/// Ground truth, measured on the build host by compiling a probe against
/// the real `voidsprite_sdk_c.h`: size 288, fields at 8-byte strides from
/// offset 0. The C header `#pragma pack(1)`s the struct to align 1; Rust
/// `#[repr(C)]` uses align 8. All 36 members are 8-byte pointers, so every
/// offset — and the total size — is identical; alignment is the only
/// difference, and it is immaterial when the host hands us a pointer to
/// its own table.
///
/// Contract: the host fills this table once and it stays valid for the
/// plugin's lifetime. Individual entries may be NULL.
#[repr(C)]
pub struct VoidSpriteSdk {
    pub util_fopen_utf8:
        Option<extern "C" fn(path_utf8: *mut c_char, mode: *const c_char) -> *mut CFile>,
    pub register_filter: Option<
        extern "C" fn(name: *const c_char, filter_function: FilterCallback) -> *mut VspFilter,
    >,
    pub register_layer_importer: Option<
        extern "C" fn(
            name: *const c_char,
            extension: *const c_char,
            layer_types: c_int,
            matching_exporter: *mut VspFileExporter,
            import_function: LayerImportFn,
            can_import_function: CanImportFn,
        ),
    >,
    pub register_layer_exporter: Option<
        extern "C" fn(
            name: *const c_char,
            extension: *const c_char,
            layer_types: c_int,
            export_function: LayerExportFn,
            can_export_function: CanExportFn,
        ) -> *mut VspFileExporter,
    >,
    pub layer_alloc_new:
        Option<extern "C" fn(layer_type: c_int, width: c_int, height: c_int) -> *mut VspLayer>,
    pub layer_free: Option<extern "C" fn(layer: *mut VspLayer)>,
    pub layer_get_info: Option<extern "C" fn(layer: *mut VspLayer) -> *mut VspLayerInfo>,
    pub layer_set_pixel:
        Option<extern "C" fn(layer: *mut VspLayer, x: c_int, y: c_int, color: u32)>,
    pub layer_get_pixel: Option<extern "C" fn(layer: *mut VspLayer, x: c_int, y: c_int) -> u32>,
    pub layer_get_raw_pixel_data: Option<extern "C" fn(layer: *mut VspLayer) -> *mut u32>,
    pub filter_new_bool_parameter:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char, default_value: bool)>,
    pub filter_new_int_parameter: Option<
        extern "C" fn(
            filter: *mut VspFilter,
            name: *const c_char,
            min_value: c_int,
            max_value: c_int,
            default_value: c_int,
        ),
    >,
    pub filter_new_double_parameter: Option<
        extern "C" fn(
            filter: *mut VspFilter,
            name: *const c_char,
            min_value: c_double,
            max_value: c_double,
            default_value: c_double,
        ),
    >,
    pub filter_new_double_range_parameter: Option<
        extern "C" fn(
            filter: *mut VspFilter,
            name: *const c_char,
            min_value: c_double,
            max_value: c_double,
            default_value_low: c_double,
            default_value_high: c_double,
            color: u32,
        ),
    >,
    pub filter_get_double_value:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char) -> c_double>,
    pub filter_get_int_value:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char) -> c_int>,
    pub filter_get_range_value1:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char) -> c_double>,
    pub filter_get_range_value2:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char) -> c_double>,
    pub filter_get_bool_value:
        Option<extern "C" fn(filter: *mut VspFilter, name: *const c_char) -> bool>,
    pub util_free: Option<extern "C" fn(ptr: *mut c_void)>,
    pub editor_get_active_color: Option<extern "C" fn(editor: *mut VspEditorContext) -> u32>,
    pub editor_get_num_layers: Option<extern "C" fn(editor: *mut VspEditorContext) -> c_int>,
    pub editor_get_layer:
        Option<extern "C" fn(editor: *mut VspEditorContext, index: c_int) -> *mut VspLayer>,
    pub editor_get_active_layer:
        Option<extern "C" fn(editor: *mut VspEditorContext) -> *mut VspLayer>,
    pub register_brush: Option<
        extern "C" fn(
            name: *const c_char,
            tooltip: *const c_char,
            double_pos_precision: bool,
            click_at: BrushClickFn,
            drag_at: BrushDragFn,
            release_at: BrushReleaseFn,
        ) -> *mut VspBrush,
    >,
    pub editor_set_pixel:
        Option<extern "C" fn(editor: *mut VspEditorContext, x: c_int, y: c_int, color: u32)>,
    pub vsp_post_notification: Option<
        extern "C" fn(title: *const c_char, message: *const c_char, color: u32, duration_ms: c_int),
    >,
    pub vsp_post_success_notification:
        Option<extern "C" fn(title: *const c_char, message: *const c_char)>,
    pub vsp_post_error_notification:
        Option<extern "C" fn(title: *const c_char, message: *const c_char)>,
    pub editor_undo_push_layer_state:
        Option<extern "C" fn(editor: *mut VspEditorContext, layer: *mut VspLayer)>,
    pub register_editor_action: Option<extern "C" fn(name: *const c_char, action: EditorActionFn)>,
    pub editor_flatten_image: Option<extern "C" fn(editor: *mut VspEditorContext) -> *mut VspLayer>,
    pub editor_flatten_frame:
        Option<extern "C" fn(editor: *mut VspEditorContext, index: c_int) -> *mut VspLayer>,
    pub editor_get_num_frames: Option<extern "C" fn(editor: *mut VspEditorContext) -> c_int>,
    pub editor_get_active_frame_index:
        Option<extern "C" fn(editor: *mut VspEditorContext) -> c_int>,
    pub vsp_get_localized_string: Option<extern "C" fn(key: *const c_char) -> *const c_char>,
}

/// `struct VSPLayerInfo` — three packed `int32`s; the C `#pragma pack(1)`
/// changes nothing here since every member is already 4-byte aligned.
#[repr(C)]
pub struct VspLayerInfo {
    pub layer_type: i32,
    pub width: i32,
    pub height: i32,
}

/// Layout assertions against the C header ground truth (offsets measured
/// by compiling a probe against `voidsprite_sdk_c.h` on the build host).
/// If the SDK ever gains, loses, or reorders a field, these fail loudly
/// at compile time instead of misreading the host's table.
const _: () = {
    assert!(size_of::<VoidSpriteSdk>() == 288);
    assert!(align_of::<VoidSpriteSdk>() == 8);
    assert!(offset_of!(VoidSpriteSdk, util_fopen_utf8) == 0);
    assert!(offset_of!(VoidSpriteSdk, register_filter) == 8);
    assert!(offset_of!(VoidSpriteSdk, register_layer_importer) == 16);
    assert!(offset_of!(VoidSpriteSdk, register_layer_exporter) == 24);
    assert!(offset_of!(VoidSpriteSdk, layer_alloc_new) == 32);
    assert!(offset_of!(VoidSpriteSdk, layer_free) == 40);
    assert!(offset_of!(VoidSpriteSdk, layer_get_info) == 48);
    assert!(offset_of!(VoidSpriteSdk, layer_set_pixel) == 56);
    assert!(offset_of!(VoidSpriteSdk, layer_get_pixel) == 64);
    assert!(offset_of!(VoidSpriteSdk, layer_get_raw_pixel_data) == 72);
    assert!(offset_of!(VoidSpriteSdk, filter_new_bool_parameter) == 80);
    assert!(offset_of!(VoidSpriteSdk, filter_new_int_parameter) == 88);
    assert!(offset_of!(VoidSpriteSdk, filter_new_double_parameter) == 96);
    assert!(offset_of!(VoidSpriteSdk, filter_new_double_range_parameter) == 104);
    assert!(offset_of!(VoidSpriteSdk, filter_get_double_value) == 112);
    assert!(offset_of!(VoidSpriteSdk, filter_get_int_value) == 120);
    assert!(offset_of!(VoidSpriteSdk, filter_get_range_value1) == 128);
    assert!(offset_of!(VoidSpriteSdk, filter_get_range_value2) == 136);
    assert!(offset_of!(VoidSpriteSdk, filter_get_bool_value) == 144);
    assert!(offset_of!(VoidSpriteSdk, util_free) == 152);
    assert!(offset_of!(VoidSpriteSdk, editor_get_active_color) == 160);
    assert!(offset_of!(VoidSpriteSdk, editor_get_num_layers) == 168);
    assert!(offset_of!(VoidSpriteSdk, editor_get_layer) == 176);
    assert!(offset_of!(VoidSpriteSdk, editor_get_active_layer) == 184);
    assert!(offset_of!(VoidSpriteSdk, register_brush) == 192);
    assert!(offset_of!(VoidSpriteSdk, editor_set_pixel) == 200);
    assert!(offset_of!(VoidSpriteSdk, vsp_post_notification) == 208);
    assert!(offset_of!(VoidSpriteSdk, vsp_post_success_notification) == 216);
    assert!(offset_of!(VoidSpriteSdk, vsp_post_error_notification) == 224);
    assert!(offset_of!(VoidSpriteSdk, editor_undo_push_layer_state) == 232);
    assert!(offset_of!(VoidSpriteSdk, register_editor_action) == 240);
    assert!(offset_of!(VoidSpriteSdk, editor_flatten_image) == 248);
    assert!(offset_of!(VoidSpriteSdk, editor_flatten_frame) == 256);
    assert!(offset_of!(VoidSpriteSdk, editor_get_num_frames) == 264);
    assert!(offset_of!(VoidSpriteSdk, editor_get_active_frame_index) == 272);
    assert!(offset_of!(VoidSpriteSdk, vsp_get_localized_string) == 280);
    assert!(size_of::<VspLayerInfo>() == 12);
    assert!(offset_of!(VspLayerInfo, layer_type) == 0);
    assert!(offset_of!(VspLayerInfo, width) == 4);
    assert!(offset_of!(VspLayerInfo, height) == 8);
};

/* ------------------------------------------------------------------ */
/* Validated parameters.                                               */
/* ------------------------------------------------------------------ */

/// Validated defringe parameters.
///
/// Invariants (established by `from_raw`, which clamps): `alpha_threshold`
/// in 1..=254, `radius` in 1..=3, `min_neighbors` in 1..=8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DefringeParams {
    alpha_threshold: u8,
    radius: u8,
    min_neighbors: u8,
}

impl DefringeParams {
    /// Build from raw host dialog values, clamping each into its declared
    /// range. Clamping (rather than rejecting) is deliberate: the host
    /// dialog already constrains the range, so an out-of-range value can
    /// only come from a misbehaving host, and failing the whole filter
    /// over it would be worse than a sane clamp.
    fn from_raw(alpha_threshold: c_int, radius: c_int, min_neighbors: c_int) -> Self {
        Self {
            alpha_threshold: clamp_param(alpha_threshold, ALPHA_THRESHOLD_MIN, ALPHA_THRESHOLD_MAX),
            radius: clamp_param(radius, RADIUS_MIN, RADIUS_MAX),
            min_neighbors: clamp_param(min_neighbors, MIN_NEIGHBORS_MIN, MIN_NEIGHBORS_MAX),
        }
    }
}

/// Clamp a host-supplied `c_int` into `[lo, hi]` (both within `u8` range).
fn clamp_param(value: c_int, lo: c_int, hi: c_int) -> u8 {
    debug_assert!((0..=u8::MAX as c_int).contains(&lo));
    debug_assert!((0..=u8::MAX as c_int).contains(&hi));
    debug_assert!(lo <= hi);
    value.clamp(lo, hi) as u8
}

/* ------------------------------------------------------------------ */
/* Pure defringe core: no host, no unsafe, fully unit-testable.         */
/* ------------------------------------------------------------------ */

/// Alpha channel of a 0xAARRGGBB SDK pixel.
fn alpha_of(pixel: u32) -> u8 {
    (pixel >> 24) as u8
}

/// "Opaque-ish": solid enough to count as structure rather than fringe.
fn is_opaque_ish(pixel: u32, params: DefringeParams) -> bool {
    alpha_of(pixel) >= params.alpha_threshold
}

/// Noise predicate. A pixel is noise when it is semi-transparent
/// (`0 < alpha < threshold`) and fewer than `min_neighbors` opaque-ish
/// pixels exist in its radius neighborhood (Chebyshev square of side
/// `2 * radius + 1`, center excluded).
///
/// Reads `snapshot` only — never the buffer being written — so a removed
/// pixel cannot cascade into its neighbors' decisions.
///
/// Contract: `snapshot.len() == width * height`, `width >= 1`,
/// `height >= 1`, `x < width`, `y < height`. `run_defringe` asserts the
/// global part once per run; the per-pixel part holds by construction of
/// its loops.
fn is_noise_pixel(
    snapshot: &[u32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    params: DefringeParams,
) -> bool {
    let center_alpha = alpha_of(snapshot[y * width + x]);
    // Fully transparent pixels are already clean: never count or rewrite
    // them. Opaque-ish pixels are art: never touch them.
    if center_alpha == 0 || center_alpha >= params.alpha_threshold {
        return false;
    }
    let radius = params.radius as usize; // 1..=3 by construction
    // Neighbor window, clamped to the layer edges. At radius 3 at most
    // 7*7-1 = 48 pixels are examined: bounded, no heap, no overflow
    // (`x + radius <= width - 1 + 3`, far below `usize::MAX`).
    let x_lo = x.saturating_sub(radius);
    let x_hi = (x + radius).min(width - 1);
    let y_lo = y.saturating_sub(radius);
    let y_hi = (y + radius).min(height - 1);
    let mut solid_neighbors: u8 = 0; // at most 48: cannot overflow u8
    for ny in y_lo..=y_hi {
        for nx in x_lo..=x_hi {
            if nx == x && ny == y {
                continue;
            }
            if is_opaque_ish(snapshot[ny * width + nx], params) {
                solid_neighbors += 1;
                if solid_neighbors >= params.min_neighbors {
                    return false; // enough structure nearby: keep it
                }
            }
        }
    }
    true
}

/// Run the filter over `pixels` in place; returns the number of pixels
/// cleared to transparent.
///
/// Contract: `width >= 1`, `height >= 1`, `pixels.len() == width * height`.
/// Violations fail loudly (assert) rather than corrupting a layer.
fn run_defringe(pixels: &mut [u32], width: usize, height: usize, params: DefringeParams) -> u64 {
    assert!(width >= 1 && height >= 1, "defringe: empty layer geometry");
    assert!(
        pixels.len() == width * height,
        "defringe: buffer/geometry size mismatch"
    );
    // Snapshot: neighbor tests must see the pre-filter layer, so decisions
    // cannot cascade. Owner: this stack frame; freed on return.
    let snapshot: Vec<u32> = pixels.to_vec();
    let mut removed: u64 = 0;
    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if is_noise_pixel(&snapshot, width, height, x, y, params) {
                pixels[idx] = PIXEL_TRANSPARENT;
                removed += 1;
            }
        }
    }
    removed
}

/* ------------------------------------------------------------------ */
/* Thin host wrappers: validate, then delegate to the pure core.        */
/* ------------------------------------------------------------------ */

/// The SDK table captured once in `pluginInit`. The host initializes the
/// plugin exactly once, before any filter runs; `SeqCst` makes the
/// handoff unambiguous. Null until `pluginInit` runs — every filter entry
/// fails closed on null.
static SDK_TABLE: AtomicPtr<VoidSpriteSdk> = AtomicPtr::new(core::ptr::null_mut());

/// Load the host's SDK table, or `None` if `pluginInit` never ran.
/// The table outlives the plugin (host contract), so the reference is
/// `'static` once the null check passes.
fn sdk_table() -> Option<&'static VoidSpriteSdk> {
    let ptr = SDK_TABLE.load(Ordering::SeqCst);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null (checked above); the pointer was stored by
    // `pluginInit` from the host-owned table, which the host guarantees
    // stays valid for the plugin's lifetime.
    Some(unsafe { &*ptr })
}

/// Layer geometry copied out of the host's `VSPLayerInfo`.
struct LayerGeometry {
    kind: i32,
    width: i32,
    height: i32,
}

/// Fetch and validate layer geometry. Returns `None` when the host gives
/// us no info, a non-RGBA layer, or insane dimensions. The info struct is
/// freed with the host's `util_free` on every path (C header contract).
fn layer_geometry(sdk: &VoidSpriteSdk, layer: *mut VspLayer) -> Option<LayerGeometry> {
    let get_info = sdk.layer_get_info?;
    let free_mem = sdk.util_free?;
    // SAFETY: `layer` is non-null (checked by the caller). The host
    // returns either null or a valid 12-byte `VSPLayerInfo`.
    let info_ptr = get_info(layer);
    if info_ptr.is_null() {
        return None;
    }
    // SAFETY: non-null per the check above; the host guarantees a valid
    // struct for exactly one read before we free it.
    let info = unsafe { &*info_ptr };
    let geometry = LayerGeometry {
        kind: info.layer_type,
        width: info.width,
        height: info.height,
    };
    // SAFETY: `info_ptr` came from `layerGetInfo`; freed exactly once here.
    free_mem(info_ptr as *mut c_void);
    if (geometry.kind & VSP_LAYER_RGBA) == 0 {
        return None; // indexed layers hold palette indices, not 0xAARRGGBB
    }
    if geometry.width < 1
        || geometry.height < 1
        || geometry.width > DIM_MAX
        || geometry.height > DIM_MAX
    {
        return None;
    }
    Some(geometry)
}

/// Borrow the layer's live pixel buffer for `pixel_count` pixels.
/// Returns `None` when the host gives us no buffer.
fn raw_pixels_mut(
    sdk: &VoidSpriteSdk,
    layer: *mut VspLayer,
    pixel_count: usize,
) -> Option<&mut [u32]> {
    let get_raw = sdk.layer_get_raw_pixel_data?;
    // `pixel_count` was computed from the same validated width/height, so
    // the host's `width * height * 4` byte guarantee covers exactly this
    // many `u32`s.
    let ptr = get_raw(layer);
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null (checked); host contract: the pointer covers
    // `width * height * 4` bytes, is `u32`-aligned, and stays valid for
    // the duration of the filter call. No other live Rust reference to
    // this memory exists.
    Some(unsafe { core::slice::from_raw_parts_mut(ptr, pixel_count) })
}

/// Post an error notification; silently drops when the host offers no
/// error reporter (better than crashing the filter over a popup).
fn post_error(sdk: &VoidSpriteSdk, message: &str) {
    let Some(post) = sdk.vsp_post_error_notification else {
        return;
    };
    if let Ok(c_message) = CString::new(message) {
        post(NOTIFY_TITLE.as_ptr() as *const c_char, c_message.as_ptr());
    }
}

/// Post a success notification; silently drops when the host offers no
/// success reporter.
fn post_success(sdk: &VoidSpriteSdk, message: &str) {
    let Some(post) = sdk.vsp_post_success_notification else {
        return;
    };
    if let Ok(c_message) = CString::new(message) {
        post(NOTIFY_TITLE.as_ptr() as *const c_char, c_message.as_ptr());
    }
}

/// The filter callback handed to `registerFilter`. Thin validation shell
/// around the pure `run_defringe`; every failure mode returns early with
/// the layer untouched (validate → prepare → commit → observe).
///
/// Panic-freedom: no `unwrap`/`expect`, no indexing before validation,
/// checked/saturating arithmetic only, and `Option`s matched explicitly.
/// Under `panic = "abort"` (release) an impossible internal failure
/// aborts instead of unwinding into the host; in test builds the pure
/// core is exercised directly.
extern "C" fn defringe_filter_entry(layer: *mut VspLayer, filter: *mut VspFilter) {
    let Some(sdk) = sdk_table() else {
        return; // pluginInit never ran: nothing we could even report through
    };
    if layer.is_null() || filter.is_null() {
        return;
    }
    let Some(get_param) = sdk.filter_get_int_value else {
        return;
    };
    // The name pointers are static NUL-terminated byte strings, so they
    // stay valid for the call. Values are clamped by `from_raw`.
    let params = DefringeParams::from_raw(
        get_param(filter, PARAM_ALPHA_THRESHOLD.as_ptr() as *const c_char),
        get_param(filter, PARAM_RADIUS.as_ptr() as *const c_char),
        get_param(filter, PARAM_MIN_NEIGHBORS.as_ptr() as *const c_char),
    );
    let Some(geometry) = layer_geometry(sdk, layer) else {
        post_error(sdk, "Defringe: layer is not RGBA or has bad geometry.");
        return;
    };
    // Width/height are 1..=DIM_MAX: the `as usize` casts cannot truncate.
    let width = geometry.width as usize;
    let height = geometry.height as usize;
    let Some(pixel_count) = width.checked_mul(height) else {
        return; // unreachable given DIM_MAX, but never trust arithmetic
    };
    if pixel_count > PIXEL_COUNT_MAX {
        post_error(sdk, "Defringe: layer is too large to process safely.");
        return;
    }
    let Some(pixels) = raw_pixels_mut(sdk, layer, pixel_count) else {
        post_error(sdk, "Defringe: could not access layer pixels.");
        return;
    };
    let removed = run_defringe(pixels, width, height, params);
    post_success(sdk, &format!("Removed {removed} noise pixel(s)."));
}

/* ------------------------------------------------------------------ */
/* Exported entry points: the six symbols the host looks up.           */
/* ------------------------------------------------------------------ */

/// Called once by the host at load. Captures the SDK table and registers
/// the filter plus its three dialog parameters. Never panics: every
/// pointer is null-checked, every table entry is `Option`-matched.
#[unsafe(no_mangle)]
pub extern "C" fn pluginInit(sdk: *mut VoidSpriteSdk) {
    if sdk.is_null() {
        return;
    }
    SDK_TABLE.store(sdk, Ordering::SeqCst);
    // SAFETY: `sdk` is non-null (checked above). The host passes its own
    // table, valid for the plugin's lifetime, and calls `pluginInit`
    // exactly once before any filter runs.
    let table = unsafe { &*sdk };
    let (Some(register_filter), Some(new_int_param)) =
        (table.register_filter, table.filter_new_int_parameter)
    else {
        return; // host without filter support: nothing to register
    };
    // The callback is a Rust function item and the name/param strings are
    // `'static` byte strings: every pointer handed to the host stays
    // valid for the plugin's lifetime.
    let handle = register_filter(
        FILTER_DISPLAY_NAME.as_ptr() as *const c_char,
        Some(defringe_filter_entry),
    );
    if handle.is_null() {
        return;
    }
    new_int_param(
        handle,
        PARAM_ALPHA_THRESHOLD.as_ptr() as *const c_char,
        ALPHA_THRESHOLD_MIN,
        ALPHA_THRESHOLD_MAX,
        ALPHA_THRESHOLD_DEFAULT,
    );
    new_int_param(
        handle,
        PARAM_RADIUS.as_ptr() as *const c_char,
        RADIUS_MIN,
        RADIUS_MAX,
        RADIUS_DEFAULT,
    );
    new_int_param(
        handle,
        PARAM_MIN_NEIGHBORS.as_ptr() as *const c_char,
        MIN_NEIGHBORS_MIN,
        MIN_NEIGHBORS_MAX,
        MIN_NEIGHBORS_DEFAULT,
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn voidspriteSDKVersion() -> c_int {
    VS_SDK_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn getPluginName() -> *const c_char {
    PLUGIN_NAME.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn getPluginVersion() -> *const c_char {
    PLUGIN_VERSION.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn getPluginDescription() -> *const c_char {
    PLUGIN_DESCRIPTION.as_ptr() as *const c_char
}

#[unsafe(no_mangle)]
pub extern "C" fn getPluginAuthors() -> *const c_char {
    PLUGIN_AUTHORS.as_ptr() as *const c_char
}

/* ------------------------------------------------------------------ */
/* Tests.                                                              */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::sync::{Mutex, MutexGuard};

    /// Build a 0xAARRGGBB pixel.
    const fn rgba(alpha: u8, red: u8, green: u8, blue: u8) -> u32 {
        ((alpha as u32) << 24) | ((red as u32) << 16) | ((green as u32) << 8) | (blue as u32)
    }

    fn default_params() -> DefringeParams {
        DefringeParams::from_raw(
            ALPHA_THRESHOLD_DEFAULT,
            RADIUS_DEFAULT,
            MIN_NEIGHBORS_DEFAULT,
        )
    }

    #[test]
    fn isolated_semitransparent_pixel_is_removed() {
        let mut pixels = vec![0u32; 9];
        pixels[4] = rgba(100, 255, 0, 0);
        let removed = run_defringe(&mut pixels, 3, 3, default_params());
        assert_eq!(removed, 1);
        assert_eq!(pixels[4], PIXEL_TRANSPARENT);
    }

    #[test]
    fn semitransparent_pixel_with_opaque_neighbor_is_kept() {
        let mut pixels = vec![0u32; 9];
        pixels[4] = rgba(100, 255, 0, 0);
        pixels[5] = rgba(255, 0, 255, 0); // opaque-ish neighbor (alpha 255)
        let removed = run_defringe(&mut pixels, 3, 3, default_params());
        assert_eq!(removed, 0);
        assert_eq!(pixels[4], rgba(100, 255, 0, 0));
        assert_eq!(pixels[5], rgba(255, 0, 255, 0));
    }

    #[test]
    fn fully_opaque_pixel_is_never_touched() {
        let mut pixels = vec![0u32; 9];
        pixels[4] = rgba(255, 10, 20, 30);
        let removed = run_defringe(&mut pixels, 3, 3, default_params());
        assert_eq!(removed, 0);
        assert_eq!(pixels[4], rgba(255, 10, 20, 30));
    }

    #[test]
    fn fully_transparent_pixels_are_untouched() {
        let mut pixels = vec![0u32; 16];
        let removed = run_defringe(&mut pixels, 4, 4, default_params());
        assert_eq!(removed, 0);
        assert!(pixels.iter().all(|&p| p == PIXEL_TRANSPARENT));
    }

    #[test]
    fn radius_two_behavior_on_two_pixel_cluster() {
        // 5x5. Two-pixel semi-transparent cluster at (2,2) and (3,2); one
        // fully opaque pixel at (0,0). Radius 2, min-neighbors 1.
        let at = |x: usize, y: usize| y * 5 + x;
        let mut pixels = vec![0u32; 25];
        pixels[at(2, 2)] = rgba(100, 255, 0, 0);
        pixels[at(3, 2)] = rgba(100, 255, 0, 0);
        pixels[at(0, 0)] = rgba(255, 0, 0, 255);

        let removed = run_defringe(&mut pixels, 5, 5, DefringeParams::from_raw(128, 2, 1));
        // (2,2) is Chebyshev distance 2 from the opaque pixel: kept.
        // (3,2) is distance 3: isolated, removed. The two semi-transparent
        // cluster mates never count as opaque-ish neighbors for each other.
        assert_eq!(removed, 1);
        assert_eq!(pixels[at(2, 2)], rgba(100, 255, 0, 0));
        assert_eq!(pixels[at(3, 2)], PIXEL_TRANSPARENT);

        // Same scene at radius 1: the opaque pixel is out of reach, so both
        // cluster pixels are removed.
        let mut pixels = vec![0u32; 25];
        pixels[at(2, 2)] = rgba(100, 255, 0, 0);
        pixels[at(3, 2)] = rgba(100, 255, 0, 0);
        pixels[at(0, 0)] = rgba(255, 0, 0, 255);
        let removed = run_defringe(&mut pixels, 5, 5, DefringeParams::from_raw(128, 1, 1));
        assert_eq!(removed, 2);
    }

    #[test]
    fn min_neighbors_two_requires_two_opaque_neighbors() {
        let mut pixels = vec![0u32; 9];
        pixels[4] = rgba(100, 255, 0, 0);
        pixels[1] = rgba(200, 0, 0, 0); // one opaque-ish neighbor (alpha 200)
        let removed = run_defringe(&mut pixels, 3, 3, DefringeParams::from_raw(128, 1, 2));
        assert_eq!(removed, 1); // one opaque-ish neighbor < 2 required
        assert_eq!(pixels[4], PIXEL_TRANSPARENT);
    }

    #[test]
    fn params_clamp_host_values_into_range() {
        assert_eq!(
            DefringeParams::from_raw(0, 0, 0),
            DefringeParams {
                alpha_threshold: 1,
                radius: 1,
                min_neighbors: 1,
            }
        );
        assert_eq!(
            DefringeParams::from_raw(300, 9, 99),
            DefringeParams {
                alpha_threshold: 254,
                radius: 3,
                min_neighbors: 8,
            }
        );
        assert_eq!(
            DefringeParams::from_raw(-7, -2, -3),
            DefringeParams {
                alpha_threshold: 1,
                radius: 1,
                min_neighbors: 1,
            }
        );
    }

    /* ---- Fake host: exercises the real `extern "C"` entry points ---- */
    /* ---- without VoidSprite. Test-only; never shipped.            ---- */

    /// A stand-in for the host's `VSPLayer`: the fake SDK stubs cast the
    /// opaque pointer back to this.
    struct FakeLayer {
        layer_type: i32,
        width: i32,
        height: i32,
        pixels: Vec<u32>,
    }

    struct FakeHostState {
        filter_name: String,
        filter_callback: FilterCallback,
        int_params: Vec<(String, i32, i32, i32)>,
        alpha: i32,
        radius: i32,
        min_neighbors: i32,
        success: Vec<(String, String)>,
        errors: Vec<(String, String)>,
    }

    impl FakeHostState {
        const fn new() -> Self {
            Self {
                filter_name: String::new(),
                filter_callback: None,
                int_params: Vec::new(),
                alpha: 0,
                radius: 0,
                min_neighbors: 0,
                success: Vec::new(),
                errors: Vec::new(),
            }
        }
    }

    static FAKE_HOST: Mutex<FakeHostState> = Mutex::new(FakeHostState::new());

    fn fake_host() -> MutexGuard<'static, FakeHostState> {
        FAKE_HOST.lock().expect("fake host mutex poisoned")
    }

    fn cstr_to_string(ptr: *const c_char) -> String {
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: test-only; pointers come from static NUL-terminated
        // strings or from `CString`s alive for the call.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }

    extern "C" fn fake_register_filter(
        name: *const c_char,
        callback: FilterCallback,
    ) -> *mut VspFilter {
        let mut host = fake_host();
        host.filter_name = cstr_to_string(name);
        host.filter_callback = callback;
        0x1 as *mut VspFilter
    }

    extern "C" fn fake_new_int_param(
        _filter: *mut VspFilter,
        name: *const c_char,
        min: c_int,
        max: c_int,
        default: c_int,
    ) {
        fake_host()
            .int_params
            .push((cstr_to_string(name), min, max, default));
    }

    extern "C" fn fake_get_int_value(_filter: *mut VspFilter, name: *const c_char) -> c_int {
        let host = fake_host();
        match cstr_to_string(name).as_str() {
            "alpha threshold" => host.alpha,
            "radius" => host.radius,
            "min neighbors" => host.min_neighbors,
            _ => 0,
        }
    }

    extern "C" fn fake_layer_get_info(layer: *mut VspLayer) -> *mut VspLayerInfo {
        if layer.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: test-only; `layer` is a `FakeLayer` box leaked by the test.
        let fake = unsafe { &*(layer as *const FakeLayer) };
        Box::into_raw(Box::new(VspLayerInfo {
            layer_type: fake.layer_type,
            width: fake.width,
            height: fake.height,
        }))
    }

    extern "C" fn fake_util_free(ptr: *mut c_void) {
        if !ptr.is_null() {
            // SAFETY: every pointer freed here came from
            // `fake_layer_get_info`'s `Box::into_raw`; freed exactly once.
            unsafe {
                drop(Box::from_raw(ptr as *mut VspLayerInfo));
            }
        }
    }

    extern "C" fn fake_get_raw(layer: *mut VspLayer) -> *mut u32 {
        if layer.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: test-only; `layer` is a leaked `FakeLayer` whose `pixels`
        // outlives the filter call.
        unsafe { (*(layer as *mut FakeLayer)).pixels.as_mut_ptr() }
    }

    extern "C" fn fake_post_success(title: *const c_char, message: *const c_char) {
        fake_host()
            .success
            .push((cstr_to_string(title), cstr_to_string(message)));
    }

    extern "C" fn fake_post_error(title: *const c_char, message: *const c_char) {
        fake_host()
            .errors
            .push((cstr_to_string(title), cstr_to_string(message)));
    }

    /// A `VoidSpriteSdk` wired to the fake stubs; every unused entry is
    /// `None`, which also proves the plugin fails closed on null entries.
    fn fake_sdk_table() -> VoidSpriteSdk {
        VoidSpriteSdk {
            util_fopen_utf8: None,
            register_filter: Some(fake_register_filter),
            register_layer_importer: None,
            register_layer_exporter: None,
            layer_alloc_new: None,
            layer_free: None,
            layer_get_info: Some(fake_layer_get_info),
            layer_set_pixel: None,
            layer_get_pixel: None,
            layer_get_raw_pixel_data: Some(fake_get_raw),
            filter_new_bool_parameter: None,
            filter_new_int_parameter: Some(fake_new_int_param),
            filter_new_double_parameter: None,
            filter_new_double_range_parameter: None,
            filter_get_double_value: None,
            filter_get_int_value: Some(fake_get_int_value),
            filter_get_range_value1: None,
            filter_get_range_value2: None,
            filter_get_bool_value: None,
            util_free: Some(fake_util_free),
            editor_get_active_color: None,
            editor_get_num_layers: None,
            editor_get_layer: None,
            editor_get_active_layer: None,
            register_brush: None,
            editor_set_pixel: None,
            vsp_post_notification: None,
            vsp_post_success_notification: Some(fake_post_success),
            vsp_post_error_notification: Some(fake_post_error),
            editor_undo_push_layer_state: None,
            register_editor_action: None,
            editor_flatten_image: None,
            editor_flatten_frame: None,
            editor_get_num_frames: None,
            editor_get_active_frame_index: None,
            vsp_get_localized_string: None,
        }
    }

    #[test]
    fn ffi_filter_end_to_end_through_fake_host() {
        *fake_host() = FakeHostState::new();
        // Leak the fake table; `pluginInit` stores the pointer globally,
        // exactly as the real host's table outlives the plugin.
        let table = Box::leak(Box::new(fake_sdk_table()));
        pluginInit(table as *mut VoidSpriteSdk);

        // Registration: right display name, right callback, three params
        // with the declared ranges and defaults.
        let callback = {
            let host = fake_host();
            assert_eq!(host.filter_name, "Defringe: remove noise pixels");
            assert_eq!(host.int_params.len(), 3);
            assert!(
                host.int_params
                    .contains(&("alpha threshold".to_string(), 1, 254, 128))
            );
            assert!(host.int_params.contains(&("radius".to_string(), 1, 3, 1)));
            assert!(
                host.int_params
                    .contains(&("min neighbors".to_string(), 1, 8, 1))
            );
            host.filter_callback.expect("filter callback registered")
        };

        // Param values the fake dialog returns for this run.
        {
            let mut host = fake_host();
            host.alpha = 128;
            host.radius = 1;
            host.min_neighbors = 1;
        }

        // 3x3 RGBA layer: one opaque pixel in a far corner (untouched) +
        // one isolated semi-transparent pixel in the opposite corner
        // (removed: Chebyshev distance 2 from the opaque pixel, radius 1).
        let mut layer = FakeLayer {
            layer_type: VSP_LAYER_RGBA,
            width: 3,
            height: 3,
            pixels: vec![0u32; 9],
        };
        layer.pixels[8] = rgba(255, 1, 2, 3);
        layer.pixels[0] = rgba(100, 9, 9, 9);
        let layer_ptr = Box::into_raw(Box::new(layer)) as *mut VspLayer;

        callback(layer_ptr, 0x1 as *mut VspFilter);

        // SAFETY: reclaim the box to inspect the pixels and free it.
        let layer = unsafe { Box::from_raw(layer_ptr as *mut FakeLayer) };
        assert_eq!(layer.pixels[8], rgba(255, 1, 2, 3));
        assert_eq!(layer.pixels[0], PIXEL_TRANSPARENT);

        {
            let host = fake_host();
            assert!(host.errors.is_empty());
            assert_eq!(host.success.len(), 1);
            assert_eq!(host.success[0].0, "Defringe");
            assert!(host.success[0].1.contains("1 noise pixel"));
        }

        // Non-RGBA layer: refused with an error, pixels untouched.
        {
            let mut host = fake_host();
            host.success.clear();
            host.errors.clear();
        }
        let layer = FakeLayer {
            layer_type: 0x02, // VSP_LAYER_INDEXED: palette indices, not AARRGGBB
            width: 3,
            height: 3,
            pixels: vec![rgba(100, 9, 9, 9); 9],
        };
        let before = layer.pixels.clone();
        let layer_ptr = Box::into_raw(Box::new(layer)) as *mut VspLayer;
        callback(layer_ptr, 0x1 as *mut VspFilter);
        // SAFETY: reclaim the box to inspect the pixels and free it.
        let layer = unsafe { Box::from_raw(layer_ptr as *mut FakeLayer) };
        assert_eq!(layer.pixels, before);
        {
            let host = fake_host();
            assert!(host.success.is_empty());
            assert_eq!(host.errors.len(), 1);
        }

        // Leave the global as we found it (null = uninitialized).
        SDK_TABLE.store(core::ptr::null_mut(), Ordering::SeqCst);
    }
}
