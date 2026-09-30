//! vs-lightshow — "light-show sprite tools", a native plugin for VoidSprite.
//!
//! Rust port (v1.1.0) of the C plugin `vs_lightshow.c` (v1.0.0). It serves the
//! light-show companion sprite pipeline with:
//!
//! 1. Filter "Light-show: blink eyes" — parameterised vertical squash of two
//!    eye boxes, producing closed-eye variants of a base frame.
//! 2. Filter "Light-show: breathing shift" — parameterised vertical shift
//!    with edge replication, for idle bob loops.
//! 3. Editor action "Light-show: export sheet + JSON" — packs the session's
//!    frames into a sprite-sheet PNG plus a JSON atlas sidecar. Layout comes
//!    from `~/.config/voidsprite/lightshow_export.cfg`; defaults match the
//!    game (96x192 cells, 4 cols x 6 rows, 180 ms/frame).
//!
//! The C originals live in `c-orig/` next to this crate. Behavioural parity
//! is the goal: the port keeps the same contracts, bounds, config grammar,
//! notification strings, and file formats. Where the port necessarily
//! differs (PNG encoder, config file open), `README.md` "Port notes" says
//! exactly how.
//!
//! Safety posture: `panic = "abort"` in the release profile, so a panic can
//! never unwind across the FFI boundary. `unsafe` is confined to the thin
//! host-boundary wrappers (reading the SDK function table, borrowing host
//! pixel buffers); all pixel math, config parsing, and file encoding are
//! safe code operating on validated inputs.

#![deny(warnings)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use core::ffi::{c_char, c_int, c_void};
use core::sync::atomic::{AtomicPtr, Ordering};
use std::ffi::{CStr, CString, OsStr};
use std::io::BufRead as _;
use std::os::unix::ffi::OsStrExt as _;

// ---------------------------------------------------------------------------
// Bounds: every loop, buffer, and allocation in this file is capped.
// ---------------------------------------------------------------------------

/// Largest single image dimension we touch (px).
const MAX_DIM_PX: i32 = 8192;
/// Most frames the exporter will pack.
const MAX_FRAMES: i32 = 256;
/// Longest config file line we parse, including the newline (bytes).
const MAX_CONFIG_LINE: usize = 256;
/// Most config lines we read.
const MAX_CONFIG_LINES: usize = 64;
/// Most mood names in the JSON sidecar.
const MAX_MOODS: usize = 32;
/// Longest single mood name, with NUL (bytes).
const MAX_MOOD_NAME: usize = 32;
/// Longest path we build (bytes).
const MAX_PATH: usize = 1024;
/// Config file name under `~/.config/voidsprite/`.
const EXPORT_CFG_NAME: &str = "lightshow_export.cfg";

/// Display name reported to the host (matches the C plugin).
const PLUGIN_NAME: &CStr = c"light-show sprite tools";
/// Plugin version (the C plugin reports "1.0.0"; this port is "1.1.0").
const PLUGIN_VERSION_STR: &str = "1.1.0";
/// Plugin version as a host-facing C string.
const PLUGIN_VERSION: &CStr = c"1.1.0";
/// One-line description (matches the C plugin).
const PLUGIN_DESCRIPTION: &CStr = c"Blink/breathing frame filters and a sprite-sheet + JSON exporter for the light-show companion pipeline.";
/// Authors string (matches the C plugin).
const PLUGIN_AUTHORS: &CStr = c"Pax (Qompass AI)";

// ---------------------------------------------------------------------------
// Status: expected failures return these; outputs stay unchanged on failure.
// ---------------------------------------------------------------------------

/// Expected failure of a plugin operation. Mirrors the C `ls_status_t`, plus
/// `HostTable`: a host table entry the SDK v1 contract says is filled reads
/// back null. The C plugin would segfault there; the port aborts the current
/// operation instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LsError {
    /// A required pointer argument was null.
    NullArg,
    /// Bad layer geometry (not RGBA, non-positive or absurd dimensions).
    BadGeometry,
    /// A computed size exceeds its bound.
    TooLarge,
    /// Allocation failed (or the host handed back a null pixel pointer).
    NoMemory,
    /// File I/O failed.
    Io,
    /// Config content is malformed.
    Config,
    /// A frame's size does not match the configured cell size.
    FrameMismatch,
    /// The host table (or one of its entries) is null.
    HostTable,
}

// ---------------------------------------------------------------------------
// VoidSprite SDK v1 ABI mirror. `voidsprite_sdk_c.h` (in `c-orig/`) is the
// authority: struct tags, member order, member types, and constants are
// unchanged. Field names are snake_cased; each carries its C spelling.
// ---------------------------------------------------------------------------

/// Opaque host handle: a layer. Never constructed here; only passed through.
#[repr(C)]
pub struct VSPLayer {
    _opaque: [u8; 0],
}

/// Opaque host handle: a filter registration.
#[repr(C)]
pub struct VSPFilter {
    _opaque: [u8; 0],
}

/// Opaque host handle: a file exporter registration.
#[repr(C)]
pub struct VSPFileExporter {
    _opaque: [u8; 0],
}

/// Opaque host handle: the editor session.
#[repr(C)]
pub struct VSPEditorContext {
    _opaque: [u8; 0],
}

/// Opaque host handle: a brush registration.
#[repr(C)]
pub struct VSPBrush {
    _opaque: [u8; 0],
}

/// Layer type flag: 32-bit RGBA pixels (`VSP_LAYER_RGBA`).
pub const VSP_LAYER_RGBA: i32 = 0x01;
/// Layer type flag: paletted pixels (`VSP_LAYER_INDEXED`).
pub const VSP_LAYER_INDEXED: i32 = 0x02;
/// SDK version this plugin targets (`VS_SDK_VERSION`).
pub const VS_SDK_VERSION: i32 = 1;

/// Mirror of `struct VSPLayerInfo`. The C header packs it (`pack(push, 1)`);
/// three `int32_t` pack identically either way — the assertion below pins it
/// at 12 bytes with offsets 0/4/8.
#[repr(C, packed)]
pub struct VSPLayerInfo {
    /// C field name `type` (`type` is a Rust keyword).
    pub layer_type: i32,
    /// C field `width`.
    pub width: i32,
    /// C field `height`.
    pub height: i32,
}

/// Host callback type: filter body.
pub type VsFilterFn = unsafe extern "C" fn(*mut VSPLayer, *mut VSPFilter);
/// Host callback type: editor action body.
pub type VsActionFn = unsafe extern "C" fn(*mut VSPEditorContext);
/// Host callback type: brush click/drag/release handler.
pub type VsBrushFn = unsafe extern "C" fn(*mut VSPBrush, *mut VSPEditorContext, c_int, c_int);
/// Host callback type: layer importer (null or a new layer).
pub type VsImportFn = unsafe extern "C" fn(*mut c_char) -> *mut VSPLayer;
/// Host callback type: layer exporter (true on success).
pub type VsExportFn = unsafe extern "C" fn(*mut VSPLayer, *mut c_char) -> bool;

/// Mirror of `struct voidspriteSDK` (SDK v1). The C header wraps the struct
/// in `#pragma pack(push, 1)`; every member is an 8-byte function pointer,
/// so the packed layout is exactly what the host fills in. `Option<fn>`
/// keeps the null state representable without changing the 8-byte size.
///
/// The `HEADER_*` assertions below pin the layout against values probed from
/// the C header itself (`cc -std=c17` + `offsetof` on the build machine). If
/// the SDK header ever drifts, this crate fails to compile instead of
/// misreading the host's function table at runtime.
#[repr(C, packed)]
pub struct VoidSpriteSdk {
    /// C: `util_fopenUTF8`.
    pub util_fopen_utf8: Option<unsafe extern "C" fn(*mut c_char, *const c_char) -> *mut c_void>,
    /// C: `registerFilter`.
    pub register_filter:
        Option<unsafe extern "C" fn(*const c_char, Option<VsFilterFn>) -> *mut VSPFilter>,
    /// C: `registerLayerImporter`.
    pub register_layer_importer: Option<
        unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            c_int,
            *mut VSPFileExporter,
            Option<VsImportFn>,
            Option<unsafe extern "C" fn(*mut c_char) -> bool>,
        ),
    >,
    /// C: `registerLayerExporter`.
    pub register_layer_exporter: Option<
        unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            c_int,
            Option<VsExportFn>,
            Option<unsafe extern "C" fn(*mut VSPLayer) -> bool>,
        ) -> *mut VSPFileExporter,
    >,
    /// C: `layerAllocNew`.
    pub layer_alloc_new: Option<unsafe extern "C" fn(c_int, c_int, c_int) -> *mut VSPLayer>,
    /// C: `layerFree`.
    pub layer_free: Option<unsafe extern "C" fn(*mut VSPLayer)>,
    /// C: `layerGetInfo`.
    pub layer_get_info: Option<unsafe extern "C" fn(*mut VSPLayer) -> *mut VSPLayerInfo>,
    /// C: `layerSetPixel`.
    pub layer_set_pixel: Option<unsafe extern "C" fn(*mut VSPLayer, c_int, c_int, u32)>,
    /// C: `layerGetPixel`.
    pub layer_get_pixel: Option<unsafe extern "C" fn(*mut VSPLayer, c_int, c_int) -> u32>,
    /// C: `layerGetRawPixelData`.
    pub layer_get_raw_pixel_data: Option<unsafe extern "C" fn(*mut VSPLayer) -> *mut u32>,
    /// C: `filterNewBoolParameter`.
    pub filter_new_bool_parameter:
        Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, bool)>,
    /// C: `filterNewIntParameter`.
    pub filter_new_int_parameter:
        Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, c_int, c_int, c_int)>,
    /// C: `filterNewDoubleParameter`.
    pub filter_new_double_parameter:
        Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, f64, f64, f64)>,
    /// C: `filterNewDoubleRangeParameter`.
    pub filter_new_double_range_parameter:
        Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, f64, f64, f64, f64, u32)>,
    /// C: `filterGetDoubleValue`.
    pub filter_get_double_value: Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
    /// C: `filterGetIntValue`.
    pub filter_get_int_value: Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> c_int>,
    /// C: `filterGetRangeValue1`.
    pub filter_get_range_value1: Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
    /// C: `filterGetRangeValue2`.
    pub filter_get_range_value2: Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
    /// C: `filterGetBoolValue`.
    pub filter_get_bool_value: Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> bool>,
    /// C: `util_free`.
    pub util_free: Option<unsafe extern "C" fn(*mut c_void)>,
    /// C: `editorGetActiveColor`.
    pub editor_get_active_color: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> u32>,
    /// C: `editorGetNumLayers`.
    pub editor_get_num_layers: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
    /// C: `editorGetLayer`.
    pub editor_get_layer:
        Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int) -> *mut VSPLayer>,
    /// C: `editorGetActiveLayer`.
    pub editor_get_active_layer:
        Option<unsafe extern "C" fn(*mut VSPEditorContext) -> *mut VSPLayer>,
    /// C: `registerBrush`.
    pub register_brush: Option<
        unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            bool,
            Option<VsBrushFn>,
            Option<VsBrushFn>,
            Option<VsBrushFn>,
        ) -> *mut VSPBrush,
    >,
    /// C: `editorSetPixel`.
    pub editor_set_pixel: Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int, c_int, u32)>,
    /// C: `vspPostNotification`.
    pub vsp_post_notification:
        Option<unsafe extern "C" fn(*const c_char, *const c_char, u32, c_int)>,
    /// C: `vspPostSuccessNotification`.
    pub vsp_post_success_notification: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    /// C: `vspPostErrorNotification`.
    pub vsp_post_error_notification: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    /// C: `editorUndoPushLayerState`.
    pub editor_undo_push_layer_state:
        Option<unsafe extern "C" fn(*mut VSPEditorContext, *mut VSPLayer)>,
    /// C: `registerEditorAction`.
    pub register_editor_action: Option<unsafe extern "C" fn(*const c_char, Option<VsActionFn>)>,
    /// C: `editorFlattenImage`.
    pub editor_flatten_image: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> *mut VSPLayer>,
    /// C: `editorFlattenFrame`.
    pub editor_flatten_frame:
        Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int) -> *mut VSPLayer>,
    /// C: `editorGetNumFrames`.
    pub editor_get_num_frames: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
    /// C: `editorGetActiveFrameIndex`.
    pub editor_get_active_frame_index: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
    /// C: `vspGetLocalizedString`.
    pub vsp_get_localized_string: Option<unsafe extern "C" fn(*const c_char) -> *const c_char>,
}

// Header-drift mitigation: every expected value below was produced by
// compiling a probe against voidsprite_sdk_c.h on x86_64 Linux
// (`cc -std=c17`, `sizeof` + `offsetof`). A mismatch fails compilation.
const _: () = {
    assert!(core::mem::size_of::<VoidSpriteSdk>() == 288);
    assert!(core::mem::size_of::<VSPLayerInfo>() == 12);
    assert!(core::mem::offset_of!(VSPLayerInfo, layer_type) == 0);
    assert!(core::mem::offset_of!(VSPLayerInfo, width) == 4);
    assert!(core::mem::offset_of!(VSPLayerInfo, height) == 8);
    assert!(core::mem::offset_of!(VoidSpriteSdk, util_fopen_utf8) == 0);
    assert!(core::mem::offset_of!(VoidSpriteSdk, register_filter) == 8);
    assert!(core::mem::offset_of!(VoidSpriteSdk, register_layer_importer) == 16);
    assert!(core::mem::offset_of!(VoidSpriteSdk, register_layer_exporter) == 24);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_alloc_new) == 32);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_free) == 40);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_get_info) == 48);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_set_pixel) == 56);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_get_pixel) == 64);
    assert!(core::mem::offset_of!(VoidSpriteSdk, layer_get_raw_pixel_data) == 72);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_new_bool_parameter) == 80);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_new_int_parameter) == 88);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_new_double_parameter) == 96);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_new_double_range_parameter) == 104);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_get_double_value) == 112);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_get_int_value) == 120);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_get_range_value1) == 128);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_get_range_value2) == 136);
    assert!(core::mem::offset_of!(VoidSpriteSdk, filter_get_bool_value) == 144);
    assert!(core::mem::offset_of!(VoidSpriteSdk, util_free) == 152);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_active_color) == 160);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_num_layers) == 168);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_layer) == 176);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_active_layer) == 184);
    assert!(core::mem::offset_of!(VoidSpriteSdk, register_brush) == 192);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_set_pixel) == 200);
    assert!(core::mem::offset_of!(VoidSpriteSdk, vsp_post_notification) == 208);
    assert!(core::mem::offset_of!(VoidSpriteSdk, vsp_post_success_notification) == 216);
    assert!(core::mem::offset_of!(VoidSpriteSdk, vsp_post_error_notification) == 224);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_undo_push_layer_state) == 232);
    assert!(core::mem::offset_of!(VoidSpriteSdk, register_editor_action) == 240);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_flatten_image) == 248);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_flatten_frame) == 256);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_num_frames) == 264);
    assert!(core::mem::offset_of!(VoidSpriteSdk, editor_get_active_frame_index) == 272);
    assert!(core::mem::offset_of!(VoidSpriteSdk, vsp_get_localized_string) == 280);
};

/// The host SDK table, captured once in `pluginInit`. Checked non-null on
/// every entry point; never mutated afterwards. The host owns the table and
/// guarantees it outlives the process.
#[derive(Clone, Copy)]
struct Host {
    table: *mut VoidSpriteSdk,
}

static G_SDK: AtomicPtr<VoidSpriteSdk> = AtomicPtr::new(core::ptr::null_mut());

/// Generate one checked accessor per host function the plugin calls. Each
/// returns `None` when the table entry reads back null — a broken table
/// aborts the current operation instead of segfaulting the host process.
macro_rules! host_accessors {
    ($( $method:ident => $field:ident : $ty:ty ),* $(,)?) => {
        impl Host {
            $(
                fn $method(&self) -> Option<$ty> {
                    // SAFETY: `self.table` points to the live SDK v1 table
                    // captured in `pluginInit` (host contract). The struct is
                    // `repr(packed)`, so the entry is read unaligned; the
                    // 8-byte copy cannot tear.
                    unsafe {
                        core::ptr::read_unaligned(core::ptr::addr_of!((*self.table).$field))
                    }
                }
            )*
        }
    };
}

host_accessors! {
    register_filter => register_filter
        : unsafe extern "C" fn(*const c_char, Option<VsFilterFn>) -> *mut VSPFilter,
    filter_new_int_parameter => filter_new_int_parameter
        : unsafe extern "C" fn(*mut VSPFilter, *const c_char, c_int, c_int, c_int),
    register_editor_action => register_editor_action
        : unsafe extern "C" fn(*const c_char, Option<VsActionFn>),
    filter_get_int_value => filter_get_int_value
        : unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> c_int,
    layer_get_info => layer_get_info
        : unsafe extern "C" fn(*mut VSPLayer) -> *mut VSPLayerInfo,
    util_free => util_free
        : unsafe extern "C" fn(*mut c_void),
    layer_get_raw_pixel_data => layer_get_raw_pixel_data
        : unsafe extern "C" fn(*mut VSPLayer) -> *mut u32,
    vsp_post_error_notification => vsp_post_error_notification
        : unsafe extern "C" fn(*const c_char, *const c_char),
    vsp_post_success_notification => vsp_post_success_notification
        : unsafe extern "C" fn(*const c_char, *const c_char),
    editor_get_num_frames => editor_get_num_frames
        : unsafe extern "C" fn(*mut VSPEditorContext) -> c_int,
    editor_flatten_frame => editor_flatten_frame
        : unsafe extern "C" fn(*mut VSPEditorContext, c_int) -> *mut VSPLayer,
    layer_free => layer_free
        : unsafe extern "C" fn(*mut VSPLayer),
}

impl Host {
    /// Load the table captured by `pluginInit`; `None` when it never ran.
    fn current() -> Option<Host> {
        let table = G_SDK.load(Ordering::SeqCst);
        if table.is_null() {
            None
        } else {
            Some(Host { table })
        }
    }

    /// Post an error notification. Silently drops the message when the host
    /// entry is missing or the text cannot be NUL-terminated (impossible for
    /// our literal and validated-ASCII strings).
    fn post_error(&self, title: &str, message: &str) {
        if let (Some(post), Ok(title_c), Ok(message_c)) = (
            self.vsp_post_error_notification(),
            CString::new(title),
            CString::new(message),
        ) {
            // SAFETY: host call with valid NUL-terminated UTF-8.
            unsafe {
                post(title_c.as_ptr(), message_c.as_ptr());
            }
        }
    }

    /// Post a success notification (same fallibility contract as above).
    fn post_success(&self, title: &str, message: &str) {
        if let (Some(post), Ok(title_c), Ok(message_c)) = (
            self.vsp_post_success_notification(),
            CString::new(title),
            CString::new(message),
        ) {
            // SAFETY: host call with valid NUL-terminated UTF-8.
            unsafe {
                post(title_c.as_ptr(), message_c.as_ptr());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure pixel math. No SDK contact: unit-testable.
// ---------------------------------------------------------------------------

/// Map a destination row inside an eye box to its source row for the blink
/// squash. The box content is scaled vertically about its centre by
/// `(100 - amount_pct)%`; at 100% every row maps to the centre row, which
/// reads as a closed eye (a lash line), using only existing pixels.
///
/// Contract: `box_h > 0`; `0 <= amount_pct <= 100`; `dst_row` in
/// `[box_y, box_y + box_h)`. Returns a row in the same range.
fn blink_src_row(box_y: i32, box_h: i32, dst_row: i32, amount_pct: i32) -> i32 {
    assert!(box_h > 0);
    assert!((0..=100).contains(&amount_pct));
    assert!(dst_row >= box_y && dst_row < box_y + box_h);
    let center = box_y + box_h / 2;
    let scale_pct = 100 - amount_pct;
    let offset = dst_row - center;
    // |offset| <= MAX_DIM_PX and scale_pct <= 100: no overflow.
    center + (offset * scale_pct) / 100
}

/// Map a destination row to its source row for a vertical shift, replicating
/// edge rows where the shift exposes new pixels.
///
/// Contract: `h > 0`; `dst_row` in `[0, h)`; `|shift_px| <= MAX_DIM_PX`.
/// Returns a row in `[0, h)`.
fn shift_src_row(h: i32, dst_row: i32, shift_px: i32) -> i32 {
    assert!(h > 0);
    assert!(dst_row >= 0 && dst_row < h);
    assert!((-MAX_DIM_PX..=MAX_DIM_PX).contains(&shift_px));
    let src = dst_row - shift_px;
    if src < 0 {
        0
    } else if src >= h {
        h - 1
    } else {
        src
    }
}

/// Convert one 0xAARRGGBB SDK pixel to RGBA byte order for PNG output.
fn pixel_to_rgba(argb: u32) -> [u8; 4] {
    [
        ((argb >> 16) & 0xFF) as u8, // R
        ((argb >> 8) & 0xFF) as u8,  // G
        (argb & 0xFF) as u8,         // B
        ((argb >> 24) & 0xFF) as u8, // A
    ]
}

// ---------------------------------------------------------------------------
// SDK helpers.
// ---------------------------------------------------------------------------

/// Fetch an RGBA layer's dimensions. Rejects null args, non-RGBA layers, and
/// absurd sizes — never trust the host blindly.
fn layer_dims(host: &Host, layer: *mut VSPLayer) -> Result<(i32, i32), LsError> {
    if layer.is_null() {
        return Err(LsError::NullArg);
    }
    let layer_get_info = host.layer_get_info().ok_or(LsError::HostTable)?;
    let util_free = host.util_free().ok_or(LsError::HostTable)?;
    // SAFETY: `layer` is non-null; the call follows the SDK v1 contract.
    let info = unsafe { layer_get_info(layer) };
    if info.is_null() {
        return Err(LsError::BadGeometry);
    }
    // `VSPLayerInfo` is repr(packed): read its fields unaligned.
    let (layer_type, width, height) = unsafe {
        (
            core::ptr::read_unaligned(core::ptr::addr_of!((*info).layer_type)),
            core::ptr::read_unaligned(core::ptr::addr_of!((*info).width)),
            core::ptr::read_unaligned(core::ptr::addr_of!((*info).height)),
        )
    };
    // SAFETY: `info` came from `layerGetInfo`, which transfers ownership to
    // the caller; `util_free` releases it.
    unsafe {
        util_free(info.cast::<c_void>());
    }
    if (layer_type & VSP_LAYER_RGBA) == 0
        || width <= 0
        || height <= 0
        || width > MAX_DIM_PX
        || height > MAX_DIM_PX
    {
        return Err(LsError::BadGeometry);
    }
    Ok((width, height))
}

/// Copy a layer's raw pixels into a fresh `w*h` buffer.
/// Contract: `w`/`h` already validated positive and bounded by `layer_dims`.
/// The caller owns the returned buffer.
fn snapshot_layer(host: &Host, layer: *mut VSPLayer, w: i32, h: i32) -> Result<Vec<u32>, LsError> {
    assert!(w > 0 && h > 0);
    assert!(w <= MAX_DIM_PX && h <= MAX_DIM_PX);
    let len = (w as usize) * (h as usize); // bounded by MAX_DIM_PX^2
    let layer_get_raw_pixel_data = host.layer_get_raw_pixel_data().ok_or(LsError::HostTable)?;
    // SAFETY: `layer` is non-null (validated by the caller via `layer_dims`).
    let src = unsafe { layer_get_raw_pixel_data(layer) };
    if src.is_null() {
        return Err(LsError::NoMemory);
    }
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| LsError::NoMemory)?;
    // SAFETY: capacity for `len` u32s is reserved; `src` points to `len`
    // valid u32s per the SDK contract (w*h*4 bytes of pixel data).
    unsafe {
        buf.set_len(len);
        core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), len);
    }
    Ok(buf)
}

/// Squash one eye box vertically in place. Reads from the snapshot, writes
/// to the live layer pixels, so overlapping boxes cannot corrupt each other.
/// Works on visors too: only existing pixels are moved, no fill is invented.
///
/// Contract: `live`/`snap` are `layer_w*layer_h` pixels; box coordinates may
/// lie partly outside the layer (clamped); `amount_pct` is clamped to
/// `[0, 100]`. An empty intersection or amount 0 is a no-op.
#[allow(clippy::too_many_arguments)]
fn apply_eye_box(
    live: &mut [u32],
    snap: &[u32],
    layer_w: i32,
    layer_h: i32,
    bx: i32,
    by: i32,
    bw: i32,
    bh: i32,
    amount_pct: i32,
) {
    debug_assert_eq!(live.len(), snap.len());
    debug_assert_eq!(live.len(), (layer_w as usize) * (layer_h as usize));
    if amount_pct <= 0 {
        return;
    }
    let amount_pct = amount_pct.min(100);
    // i64 intermediates: a hostile host could hand back huge parameter
    // values, and `bx + bw` must not wrap.
    let x0 = (bx as i64).max(0);
    let y0 = (by as i64).max(0);
    let x1 = ((bx as i64) + (bw as i64)).min(layer_w as i64);
    let y1 = ((by as i64) + (bh as i64)).min(layer_h as i64);
    if x0 >= x1 || y0 >= y1 {
        return; // box misses the layer entirely
    }
    let stride = layer_w as usize;
    for y in y0..y1 {
        // The squash is computed against the clamped box, like the C code.
        let src_y = blink_src_row(y0 as i32, (y1 - y0) as i32, y as i32, amount_pct);
        let dst_row = (y as usize) * stride;
        let src_row = (src_y as usize) * stride;
        for x in x0..x1 {
            live[dst_row + (x as usize)] = snap[src_row + (x as usize)];
        }
    }
}

/// Shift every row of `live` vertically by `shift` px, replicating edge rows.
/// Reads from `snap`, writes to `live`.
fn shift_rows(live: &mut [u32], snap: &[u32], w: i32, h: i32, shift: i32) {
    debug_assert_eq!(live.len(), snap.len());
    debug_assert_eq!(live.len(), (w as usize) * (h as usize));
    let shift = shift.clamp(-MAX_DIM_PX, MAX_DIM_PX);
    let stride = w as usize;
    for y in 0..h {
        let src_y = shift_src_row(h, y, shift);
        let dst = (y as usize) * stride;
        let src = (src_y as usize) * stride;
        live[dst..dst + stride].copy_from_slice(&snap[src..src + stride]);
    }
}

// ---------------------------------------------------------------------------
// Exporter configuration. Grammar: `key=value` lines, `#` full-line
// comments, blank lines ignored. Unknown keys and malformed values abort the
// load loudly — a typo'd key must never silently keep a default.
// ---------------------------------------------------------------------------

/// Export layout. The byte buffers are NUL-terminated C-style strings
/// holding validated ASCII; `buf_str` views them.
#[derive(Clone, Copy)]
struct ExportCfg {
    cell_w: i32,
    cell_h: i32,
    cols: i32,
    rows: i32,
    frame_ms: i32,
    moods: [[u8; MAX_MOOD_NAME]; MAX_MOODS],
    mood_count: usize,
    output_dir: [u8; MAX_PATH], // empty = $HOME/lightshow-export
    basename: [u8; MAX_PATH],   // PNG/JSON stem, no extension
}

/// Write `bytes` into a NUL-terminated buffer. Callers pass validated
/// lengths (`bytes.len() < N`).
fn buf_set<const N: usize>(buf: &mut [u8; N], bytes: &[u8]) {
    assert!(bytes.len() < N);
    buf[..bytes.len()].copy_from_slice(bytes);
    buf[bytes.len()] = 0;
}

/// View a NUL-terminated buffer as `&str`. Every writer stores validated
/// ASCII, so the conversion cannot fail; the empty fallback is unreachable.
fn buf_str(buf: &[u8]) -> &str {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..end]).unwrap_or("")
}

/// Defaults match the light-show game constants
/// (`game/src/waifu/sprite.rs`): 96x192 cells, 4 cols x 6 rows, 180 ms.
fn default_cfg() -> ExportCfg {
    let mut cfg = ExportCfg {
        cell_w: 96,
        cell_h: 192,
        cols: 4,
        rows: 6,
        frame_ms: 180,
        moods: [[0u8; MAX_MOOD_NAME]; MAX_MOODS],
        mood_count: 6,
        output_dir: [0u8; MAX_PATH],
        basename: [0u8; MAX_PATH],
    };
    for (slot, mood) in
        cfg.moods
            .iter_mut()
            .zip(["idle", "blush", "wink", "pout", "celebrate", "alarmed"])
    {
        buf_set(slot, mood.as_bytes());
    }
    buf_set(&mut cfg.basename, b"sheet");
    cfg
}

/// Strict integer parser with `strtol` semantics: skips leading ASCII
/// whitespace, takes an optional sign plus digits, then allows only space,
/// tab, newline, carriage return as trailing bytes. Rejects junk, overflow,
/// and out-of-range values.
fn parse_int_value(val: &[u8], min_v: i32, max_v: i32) -> Result<i32, LsError> {
    debug_assert!(min_v <= max_v);
    let mut s = val;
    while let Some((&b, rest)) = s.split_first() {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
            s = rest;
        } else {
            break;
        }
    }
    let neg = match s.first() {
        Some(b'-') => {
            s = &s[1..];
            true
        }
        Some(b'+') => {
            s = &s[1..];
            false
        }
        _ => false,
    };
    let mut acc: i64 = 0;
    let mut digits = 0usize;
    while let Some(&b) = s.get(digits) {
        if !b.is_ascii_digit() {
            break;
        }
        acc = acc
            .checked_mul(10)
            .and_then(|a| a.checked_add((b - b'0') as i64))
            .ok_or(LsError::Config)?;
        digits += 1;
    }
    if digits == 0 {
        return Err(LsError::Config);
    }
    // The C trailing skip set is space/tab/newline/CR only.
    if !s[digits..]
        .iter()
        .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        return Err(LsError::Config);
    }
    let v = if neg { -acc } else { acc };
    if v < min_v as i64 || v > max_v as i64 {
        return Err(LsError::Config);
    }
    Ok(v as i32)
}

/// A mood/file-stem token may only contain filename- and JSON-safe chars,
/// and must fit the fixed buffers (like the C `ls_token_ok`).
fn token_ok(tok: &[u8]) -> bool {
    !tok.is_empty()
        && tok.len() < MAX_MOOD_NAME
        && tok
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Parse a comma-separated mood list into `cfg`. Rejects empty lists and bad
/// tokens; `cfg` is unchanged on failure (parsed into a staging copy).
fn parse_moods(cfg: &mut ExportCfg, val: &[u8]) -> Result<(), LsError> {
    let mut staged = [[0u8; MAX_MOOD_NAME]; MAX_MOODS];
    let mut count = 0usize;
    let mut p = val;
    loop {
        while p.first() == Some(&b' ') || p.first() == Some(&b'\t') {
            p = &p[1..];
        }
        let comma = p.iter().position(|&b| b == b',');
        let mut len = comma.unwrap_or(p.len());
        while len > 0 && matches!(p[len - 1], b' ' | b'\t' | b'\n' | b'\r') {
            len -= 1;
        }
        let tok = &p[..len];
        if !token_ok(tok) || count >= MAX_MOODS {
            return Err(LsError::Config);
        }
        staged[count][..len].copy_from_slice(tok);
        count += 1;
        match comma {
            None => break,
            Some(i) => p = &p[i + 1..],
        }
    }
    if count == 0 {
        return Err(LsError::Config);
    }
    cfg.moods = staged;
    cfg.mood_count = count;
    Ok(())
}

/// Copy a validated token into a path buffer (the `basename` key).
fn parse_token_field(field: &mut [u8; MAX_PATH], val: &[u8]) -> Result<(), LsError> {
    let mut v = val;
    while v.first() == Some(&b' ') || v.first() == Some(&b'\t') {
        v = &v[1..];
    }
    let mut len = v.len();
    while len > 0 && matches!(v[len - 1], b' ' | b'\t' | b'\n' | b'\r') {
        len -= 1;
    }
    let tok = &v[..len];
    // `token_ok` caps the length at MAX_MOOD_NAME; the MAX_PATH check below
    // mirrors the C code's second bound (subsumed, kept for parity).
    if !token_ok(tok) || len >= MAX_PATH {
        return Err(LsError::Config);
    }
    buf_set(field, tok);
    Ok(())
}

/// Parse one config line. Returns whether the line carried a key/value pair.
/// Blank lines and `#` comments are accepted and ignored; unknown keys are
/// an error.
fn parse_config_line(cfg: &mut ExportCfg, line: &[u8]) -> Result<bool, LsError> {
    let mut l = line;
    while l.first() == Some(&b' ') || l.first() == Some(&b'\t') {
        l = &l[1..];
    }
    match l.first() {
        None | Some(b'\n') | Some(b'\r') | Some(b'#') => return Ok(false),
        _ => {}
    }
    let eq = l.iter().position(|&b| b == b'=').ok_or(LsError::Config)?;
    let mut key = &l[..eq];
    if key.is_empty() || key.len() >= 64 {
        return Err(LsError::Config);
    }
    while key.last() == Some(&b' ') || key.last() == Some(&b'\t') {
        key = &key[..key.len() - 1];
    }
    let val = &l[eq + 1..];
    match key {
        b"cell_w" => cfg.cell_w = parse_int_value(val, 1, 2048)?,
        b"cell_h" => cfg.cell_h = parse_int_value(val, 1, 2048)?,
        b"cols" => cfg.cols = parse_int_value(val, 1, 64)?,
        b"rows" => cfg.rows = parse_int_value(val, 1, 64)?,
        b"frame_ms" => cfg.frame_ms = parse_int_value(val, 1, 10000)?,
        b"moods" => parse_moods(cfg, val)?,
        b"output_dir" => {
            let mut v = val;
            while v.first() == Some(&b' ') || v.first() == Some(&b'\t') {
                v = &v[1..];
            }
            let mut len = v.len();
            while len > 0 && matches!(v[len - 1], b'\n' | b'\r' | b' ' | b'\t') {
                len -= 1;
            }
            let dir = &v[..len];
            // Unlike `basename`, a directory may contain `/` and friends;
            // only emptiness and the path cap are enforced.
            if dir.is_empty() || dir.len() >= MAX_PATH {
                return Err(LsError::Config);
            }
            buf_set(&mut cfg.output_dir, dir);
        }
        b"basename" => parse_token_field(&mut cfg.basename, val)?,
        _ => return Err(LsError::Config), // unknown key
    }
    Ok(true)
}

/// Load the exporter config file. A missing file is not an error: defaults
/// stand. Any parse error aborts the load; `cfg` keeps whatever the caller
/// set (callers start from `default_cfg`).
///
/// Port note: the C plugin opens the file through the host's
/// `util_fopenUTF8`; on Linux that helper is byte-identical to `fopen`, so
/// this port reads with `std::fs::File` and keeps paths as raw bytes.
fn load_config(path: &OsStr, cfg: &mut ExportCfg) -> Result<(), LsError> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        // Any open failure (missing file, permissions, ...) leaves the
        // defaults in place, exactly like the C `fopen` NULL check.
        Err(_) => return Ok(()),
    };
    // Parse into a copy; commit only on full success.
    let mut staged = *cfg;
    let mut reader = std::io::BufReader::new(file);
    let mut line: Vec<u8> = Vec::new();
    let mut nlines = 0usize;
    loop {
        line.clear();
        // A read error ends the loop like EOF does: this mirrors the C
        // loader, whose `fgets` loop cannot tell the two apart.
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        nlines += 1;
        if nlines > MAX_CONFIG_LINES {
            return Err(LsError::Config);
        }
        // A line that fills the 255-byte C buffer without a newline is too
        // long (matches the fgets/strchr/feof check in the C loader).
        if line.len() > MAX_CONFIG_LINE - 1 {
            return Err(LsError::Config);
        }
        parse_config_line(&mut staged, &line)?;
    }
    if (staged.cols as i64) * (staged.rows as i64) > MAX_FRAMES as i64 {
        return Err(LsError::Config);
    }
    *cfg = staged;
    Ok(())
}

// ---------------------------------------------------------------------------
// PNG writer. The C plugin uses the vendored stb_image_write; this port
// emits 8-bit RGBA PNGs with a dependency-free encoder: raw DEFLATE stored
// blocks inside a zlib stream. Pixels land identically; only the compressed
// bytes differ (stb applies Huffman coding, this writer stores rows
// verbatim). Every emitted file is a valid PNG per RFC 2083.
// ---------------------------------------------------------------------------

/// PNG file signature.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// CRC-32 (ISO HDLC) lookup table, generated at compile time.
const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
}

const CRC32_TABLE: [u32; 256] = crc32_table();

/// Fold `bytes` into a running CRC-32 (init `0xFFFF_FFFF`, xor-out at the end).
fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc = CRC32_TABLE[((crc ^ (b as u32)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// Adler-32 (zlib checksum), RFC 1950.
fn adler32(bytes: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in bytes {
        a = (a + (byte as u32)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Append one PNG chunk: length (BE32), type, data, CRC-32(type || data).
fn png_chunk(png: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(chunk_type);
    png.extend_from_slice(data);
    let crc = crc32_update(crc32_update(0xFFFF_FFFF, chunk_type), data);
    png.extend_from_slice(&(crc ^ 0xFFFF_FFFF).to_be_bytes());
}

/// Encode 8-bit RGBA pixels as a complete PNG file image.
/// Contract: `width, height >= 1`; `rgba.len() == width*height*4`.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, LsError> {
    if width == 0 || height == 0 {
        return Err(LsError::BadGeometry);
    }
    let (w, h) = (width as usize, height as usize);
    let stride = w.checked_mul(4).ok_or(LsError::TooLarge)?;
    let row_len = stride.checked_add(1).ok_or(LsError::TooLarge)?; // filter byte + pixels
    let raw_len = row_len.checked_mul(h).ok_or(LsError::TooLarge)?;
    if rgba.len() != stride * h {
        return Err(LsError::BadGeometry);
    }
    // Filtered scanlines: one 0x00 (None) filter byte per row.
    let mut raw = Vec::new();
    raw.try_reserve_exact(raw_len)
        .map_err(|_| LsError::NoMemory)?;
    for y in 0..h {
        raw.push(0x00);
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }
    // zlib stream: 2-byte header, DEFLATE stored blocks, Adler-32.
    let mut zlib = Vec::new();
    let blocks = raw_len.div_ceil(65535);
    let zlib_len = 2usize
        .checked_add(blocks.checked_mul(5).ok_or(LsError::TooLarge)?)
        .ok_or(LsError::TooLarge)?
        .checked_add(raw_len)
        .ok_or(LsError::TooLarge)?
        .checked_add(4)
        .ok_or(LsError::TooLarge)?;
    zlib.try_reserve_exact(zlib_len)
        .map_err(|_| LsError::NoMemory)?;
    zlib.extend_from_slice(&[0x78, 0x01]); // CMF/FLG: 32K window, no preset dictionary
    let mut offset = 0usize;
    while offset < raw.len() {
        let chunk_len = (raw.len() - offset).min(65535);
        let last = offset + chunk_len == raw.len();
        zlib.push(if last { 0x01 } else { 0x00 }); // BFINAL | BTYPE=00 (stored)
        let len16 = chunk_len as u16;
        zlib.extend_from_slice(&len16.to_le_bytes());
        zlib.extend_from_slice(&(!len16).to_le_bytes());
        zlib.extend_from_slice(&raw[offset..offset + chunk_len]);
        offset += chunk_len;
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    // PNG container: signature, IHDR, one IDAT, IEND.
    let mut png = Vec::new();
    png.try_reserve_exact(8 + (12 + 13) + (12 + zlib.len()) + 12)
        .map_err(|_| LsError::NoMemory)?;
    png.extend_from_slice(&PNG_SIGNATURE);
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // color type: truecolor with alpha
    // ihdr[10..13] stay 0: deflate, no filter method extras, no interlace.
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// Write `rgba` (`width*height*4` bytes, 8-bit RGBA) to `path` as a PNG.
fn write_png(path: &OsStr, width: u32, height: u32, rgba: &[u8]) -> Result<(), LsError> {
    let png = encode_png(width, height, rgba)?;
    std::fs::write(path, png).map_err(|_| LsError::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON sidecar. The game builds its Bevy TextureAtlasLayout from Rust
// constants; this file documents the exact pipeline contract next to the
// PNG for tooling and humans. Byte-identical in shape to the C writer.
// ---------------------------------------------------------------------------

/// Write the atlas sidecar. `png_name` and the mood strings are
/// token-validated ASCII, so no JSON escaping is needed (as in the C code).
fn write_json(path: &OsStr, cfg: &ExportCfg, png_name: &str) -> Result<(), LsError> {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"generator\": \"vs-lightshow {}\",\n",
        PLUGIN_VERSION_STR
    ));
    out.push_str(&format!("  \"image\": \"{}\",\n", png_name));
    out.push_str(&format!("  \"cell_w\": {},\n", cfg.cell_w));
    out.push_str(&format!("  \"cell_h\": {},\n", cfg.cell_h));
    out.push_str(&format!("  \"cols\": {},\n", cfg.cols));
    out.push_str(&format!("  \"rows\": {},\n", cfg.rows));
    out.push_str(&format!("  \"frame_ms\": {},\n", cfg.frame_ms));
    out.push_str("  \"moods\": [");
    for (i, mood) in cfg.moods[..cfg.mood_count].iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        out.push_str(buf_str(mood));
        out.push('"');
    }
    out.push_str("],\n");
    out.push_str("  \"frames\": [");
    for r in 0..cfg.rows {
        for c in 0..cfg.cols {
            let idx = r * cfg.cols + c;
            let mood = if (r as usize) < cfg.mood_count {
                buf_str(&cfg.moods[r as usize])
            } else {
                "unknown"
            };
            out.push_str(&format!(
                "{}\n    {{\"index\": {}, \"col\": {}, \"row\": {}, \"x\": {}, \"y\": {}, \"w\": {}, \"h\": {}, \"mood\": \"{}\"}}",
                if idx == 0 { "" } else { "," },
                idx,
                c,
                r,
                c * cfg.cell_w,
                r * cfg.cell_h,
                cfg.cell_w,
                cfg.cell_h,
                mood,
            ));
        }
    }
    out.push_str("\n  ],\n");
    out.push_str(
        "  \"note\": \"light-show builds its Bevy TextureAtlasLayout from Rust constants \
         (game/src/waifu/sprite.rs); this file documents the pipeline contract.\"\n",
    );
    out.push_str("}\n");
    std::fs::write(path, out).map_err(|_| LsError::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Editor action: export sheet + JSON.
// ---------------------------------------------------------------------------

/// Join path components with `/`, enforcing the `MAX_PATH` byte cap.
/// Returns `None` when the result would not fit — the C `snprintf`
/// truncation check.
fn join_path(parts: &[&[u8]]) -> Option<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(part);
        if out.len() >= MAX_PATH {
            return None;
        }
    }
    Some(out)
}

/// Truncate `s` to `max` bytes on a character boundary. Mirrors the C
/// `%.100s` precision; all our inputs are ASCII, so this is a plain cut.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Editor action "Light-show: export sheet + JSON". Packs the first
/// `cols*rows` session frames into `<basename>.png` plus a `<basename>.json`
/// sidecar. This action only reads the session — nothing to undo.
unsafe extern "C" fn action_export(editor: *mut VSPEditorContext) {
    // SAFETY: entry-point contract — the host calls this with the editor
    // context it owns; every host call below goes through the checked `Host`
    // accessors, every raw pointer is validated before use, and every
    // allocation is bounded.
    unsafe {
        let Some(host) = Host::current() else { return };
        if editor.is_null() {
            return;
        }
        // This action only reads the session; nothing to push onto undo.

        let mut cfg = default_cfg();
        let home: Option<Vec<u8>> = std::env::var_os("HOME")
            .map(|h| h.as_bytes().to_vec())
            .filter(|h| !h.is_empty());
        if let Some(home) = home.as_ref()
            && let Some(cfg_path) = join_path(&[
                home.as_slice(),
                b".config/voidsprite",
                EXPORT_CFG_NAME.as_bytes(),
            ])
            && load_config(OsStr::from_bytes(&cfg_path), &mut cfg).is_err()
        {
            host.post_error(
                "Light-show export",
                "Config file has errors; fix it or delete it.",
            );
            return;
        }
        // Overlong config path: the C loader skips loading, defaults stand.

        let Some(editor_get_num_frames) = host.editor_get_num_frames() else {
            host.post_error("Light-show export", "Export failed.");
            return;
        };
        // SAFETY: host call per SDK contract.
        let nframes = editor_get_num_frames(editor);
        let want = cfg.cols * cfg.rows; // cols, rows in 1..=64: no overflow
        if nframes < want {
            host.post_error(
                "Light-show export",
                &format!("Session has {nframes} frames but the sheet needs {want}."),
            );
            return;
        }

        // Explicit config wins; otherwise ~/lightshow-export; then /tmp.
        let out_dir: Vec<u8> = if cfg.output_dir[0] != 0 {
            buf_str(&cfg.output_dir).as_bytes().to_vec()
        } else if let Some(dir) = home
            .as_ref()
            .and_then(|h| join_path(&[h.as_slice(), b"lightshow-export"]))
        {
            dir
        } else {
            b"/tmp/lightshow-export".to_vec()
        };
        let out_dir_os = OsStr::from_bytes(&out_dir);
        // Create one directory level; EEXIST is fine (mirrors `mkdir 0755`).
        if std::fs::create_dir(out_dir_os)
            .is_err_and(|e| e.kind() != std::io::ErrorKind::AlreadyExists)
        {
            host.post_error("Light-show export", "Cannot create the output directory.");
            return;
        }

        let basename = buf_str(&cfg.basename);
        let png_name = format!("{basename}.png");
        let json_name = format!("{basename}.json");
        let (Some(png_path), Some(json_path)) = (
            join_path(&[&out_dir, png_name.as_bytes()]),
            join_path(&[&out_dir, json_name.as_bytes()]),
        ) else {
            host.post_error("Light-show export", "Path too long.");
            return;
        };

        let sheet_w = (cfg.cols as u64) * (cfg.cell_w as u64);
        let sheet_h = (cfg.rows as u64) * (cfg.cell_h as u64);
        // Config caps keep this small (64*2048 px/side max), but the byte
        // count still gets an explicit overflow check before allocation.
        let sheet_px = match sheet_w.checked_mul(sheet_h) {
            Some(px) if px <= (usize::MAX as u64) / 4 => px as usize,
            _ => {
                host.post_error("Light-show export", "Sheet too large.");
                return;
            }
        };
        let mut rgba: Vec<u8> = Vec::new();
        if rgba.try_reserve_exact(sheet_px * 4).is_err() {
            host.post_error("Light-show export", "Out of memory.");
            return;
        }
        rgba.resize(sheet_px * 4, 0); // zeroed, like calloc

        let (Some(editor_flatten_frame), Some(layer_free)) =
            (host.editor_flatten_frame(), host.layer_free())
        else {
            host.post_error("Light-show export", "Export failed.");
            return;
        };
        let sheet_w_usize = sheet_w as usize;
        let mut st: Result<(), LsError> = Ok(());
        for i in 0..want {
            // SAFETY: host call per SDK contract; ownership of the returned
            // layer transfers to us (`editorFlattenFrame` contract).
            let frame = editor_flatten_frame(editor, i);
            if frame.is_null() {
                st = Err(LsError::Io);
                break;
            }
            st = (|| -> Result<(), LsError> {
                let (fw, fh) = layer_dims(&host, frame)?;
                if fw != cfg.cell_w || fh != cfg.cell_h {
                    return Err(LsError::FrameMismatch);
                }
                let layer_get_raw_pixel_data =
                    host.layer_get_raw_pixel_data().ok_or(LsError::HostTable)?;
                // SAFETY: `frame` is non-null; host contract.
                let src = layer_get_raw_pixel_data(frame);
                if src.is_null() {
                    return Err(LsError::BadGeometry);
                }
                // SAFETY: `src` points to fw*fh valid u32s (host contract).
                let src_slice = core::slice::from_raw_parts(src, (fw as usize) * (fh as usize));
                let dst_col = (i % cfg.cols) as usize;
                let dst_row = (i / cfg.cols) as usize;
                let (cell_w, cell_h) = (cfg.cell_w as usize, cfg.cell_h as usize);
                let fw_usize = fw as usize;
                for y in 0..cell_h {
                    let dst_base = ((dst_row * cell_h + y) * sheet_w_usize + dst_col * cell_w) * 4;
                    for x in 0..fw_usize {
                        let px = pixel_to_rgba(src_slice[y * fw_usize + x]);
                        rgba[dst_base + x * 4..dst_base + x * 4 + 4].copy_from_slice(&px);
                    }
                }
                Ok(())
            })();
            // SAFETY: `frame` came from `editorFlattenFrame`; the SDK
            // contract requires `layerFree` after use.
            layer_free(frame);
            if st.is_err() {
                break;
            }
        }

        if st.is_ok()
            && write_png(
                OsStr::from_bytes(&png_path),
                sheet_w as u32,
                sheet_h as u32,
                &rgba,
            )
            .is_err()
        {
            st = Err(LsError::Io);
        }
        if st.is_ok() && write_json(OsStr::from_bytes(&json_path), &cfg, &png_name).is_err() {
            st = Err(LsError::Io);
        }

        match st {
            Ok(()) => {
                let shown = truncate_bytes(&png_name, 100);
                host.post_success(
                    "Light-show export",
                    &format!("Wrote {want} frames to {shown}."),
                );
            }
            Err(LsError::FrameMismatch) => {
                host.post_error(
                    "Light-show export",
                    "A frame's size does not match cell_w x cell_h.",
                );
            }
            Err(_) => {
                host.post_error("Light-show export", "Export failed.");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filter 1: blink eyes.
// ---------------------------------------------------------------------------

/// Filter "Light-show: blink eyes". Squashes the two configured eye boxes
/// vertically by the blink amount. Runs inside the host's undo scope, so no
/// manual undo push is needed.
unsafe extern "C" fn filter_blink(layer: *mut VSPLayer, filter: *mut VSPFilter) {
    // SAFETY: entry-point contract — non-null host pointers; every host call
    // goes through the checked `Host` accessors.
    unsafe {
        let Some(host) = Host::current() else { return };
        if layer.is_null() || filter.is_null() {
            return;
        }
        // Filters run inside the host's own undo scope: no manual undo push.
        let (w, h) = match layer_dims(&host, layer) {
            Ok(dims) => dims,
            Err(_) => {
                host.post_error("Light-show blink", "Layer is not RGBA or has bad geometry.");
                return;
            }
        };
        let Some(filter_get_int_value) = host.filter_get_int_value() else {
            return;
        };
        // SAFETY: host calls per SDK contract.
        let param = |name: &CStr| filter_get_int_value(filter, name.as_ptr());
        let lx = param(c"left eye x");
        let ly = param(c"left eye y");
        let lw = param(c"left eye w");
        let lh = param(c"left eye h");
        let rx = param(c"right eye x");
        let ry = param(c"right eye y");
        let rw = param(c"right eye w");
        let rh = param(c"right eye h");
        let amount = param(c"blink amount %");
        let snap = match snapshot_layer(&host, layer, w, h) {
            Ok(snap) => snap,
            Err(_) => {
                host.post_error("Light-show blink", "Out of memory snapshotting the layer.");
                return;
            }
        };
        let Some(layer_get_raw_pixel_data) = host.layer_get_raw_pixel_data() else {
            return;
        };
        // SAFETY: host call per SDK contract.
        let live_ptr = layer_get_raw_pixel_data(layer);
        if live_ptr.is_null() {
            host.post_error("Light-show blink", "Could not access layer pixels.");
            return;
        }
        // SAFETY: `live_ptr` addresses w*h live u32 pixels (host contract);
        // borrowed for the two box applications, then released.
        let live = core::slice::from_raw_parts_mut(live_ptr, (w as usize) * (h as usize));
        apply_eye_box(live, &snap, w, h, lx, ly, lw, lh, amount);
        apply_eye_box(live, &snap, w, h, rx, ry, rw, rh, amount);
    }
}

// ---------------------------------------------------------------------------
// Filter 2: breathing shift.
// ---------------------------------------------------------------------------

/// Filter "Light-show: breathing shift". Shifts the frame vertically by the
/// configured pixel amount, replicating edge rows.
unsafe extern "C" fn filter_breathe(layer: *mut VSPLayer, filter: *mut VSPFilter) {
    // SAFETY: entry-point contract — see `filter_blink`.
    unsafe {
        let Some(host) = Host::current() else { return };
        if layer.is_null() || filter.is_null() {
            return;
        }
        let (w, h) = match layer_dims(&host, layer) {
            Ok(dims) => dims,
            Err(_) => {
                host.post_error(
                    "Light-show breathe",
                    "Layer is not RGBA or has bad geometry.",
                );
                return;
            }
        };
        // SAFETY: host call per SDK contract.
        let shift = match host.filter_get_int_value() {
            Some(get) => get(filter, c"shift px".as_ptr()),
            None => return,
        };
        let snap = match snapshot_layer(&host, layer, w, h) {
            Ok(snap) => snap,
            Err(_) => {
                host.post_error(
                    "Light-show breathe",
                    "Out of memory snapshotting the layer.",
                );
                return;
            }
        };
        // SAFETY: host call per SDK contract.
        let live_ptr = match host.layer_get_raw_pixel_data() {
            Some(get) => get(layer),
            None => return,
        };
        if live_ptr.is_null() {
            host.post_error("Light-show breathe", "Could not access layer pixels.");
            return;
        }
        // SAFETY: `live_ptr` addresses w*h live u32 pixels (host contract).
        let live = core::slice::from_raw_parts_mut(live_ptr, (w as usize) * (h as usize));
        shift_rows(live, &snap, w, h, shift);
    }
}

// ---------------------------------------------------------------------------
// Plugin entry points.
// ---------------------------------------------------------------------------

/// Register the two filters and the editor action with the host. Captures
/// the host SDK table for the process lifetime.
///
/// # Safety
///
/// `sdk` must point to a fully initialized SDK v1 table that stays valid for
/// the rest of the process, exactly as the host guarantees when it loads a
/// plugin. A null `sdk` is accepted and only records the absence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pluginInit(sdk: *mut VoidSpriteSdk) {
    G_SDK.store(sdk, Ordering::SeqCst);
    if sdk.is_null() {
        return;
    }
    // SAFETY: the host guarantees the table is fully initialized and lives
    // for the process lifetime.
    let host = Host { table: sdk };
    // SAFETY: host registration calls per the SDK v1 contract.
    unsafe {
        if let Some(register_filter) = host.register_filter() {
            let blink = register_filter(c"Light-show: blink eyes".as_ptr(), Some(filter_blink));
            if !blink.is_null()
                && let Some(new_int) = host.filter_new_int_parameter()
            {
                for (name, min, max, default) in [
                    (c"left eye x", 0, MAX_DIM_PX, 0),
                    (c"left eye y", 0, MAX_DIM_PX, 0),
                    (c"left eye w", 0, MAX_DIM_PX, 0),
                    (c"left eye h", 0, MAX_DIM_PX, 0),
                    (c"right eye x", 0, MAX_DIM_PX, 0),
                    (c"right eye y", 0, MAX_DIM_PX, 0),
                    (c"right eye w", 0, MAX_DIM_PX, 0),
                    (c"right eye h", 0, MAX_DIM_PX, 0),
                    (c"blink amount %", 0, 100, 80),
                ] {
                    new_int(blink, name.as_ptr(), min, max, default);
                }
            }
            let breathe = register_filter(
                c"Light-show: breathing shift".as_ptr(),
                Some(filter_breathe),
            );
            if !breathe.is_null()
                && let Some(new_int) = host.filter_new_int_parameter()
            {
                new_int(breathe, c"shift px".as_ptr(), -32, 32, 2);
            }
        }
        if let Some(register_action) = host.register_editor_action() {
            register_action(
                c"Light-show: export sheet + JSON".as_ptr(),
                Some(action_export),
            );
        }
    }
}

/// SDK version this plugin targets (`VS_SDK_VERSION`).
#[unsafe(no_mangle)]
pub extern "C" fn voidspriteSDKVersion() -> c_int {
    VS_SDK_VERSION
}

/// Display name shown by the host.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginName() -> *const c_char {
    PLUGIN_NAME.as_ptr()
}

/// Plugin version string.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginVersion() -> *const c_char {
    PLUGIN_VERSION.as_ptr()
}

/// One-line description.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginDescription() -> *const c_char {
    PLUGIN_DESCRIPTION.as_ptr()
}

/// Authors string.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginAuthors() -> *const c_char {
    PLUGIN_AUTHORS.as_ptr()
}

// ---------------------------------------------------------------------------
// Unit tests: the pure pixel math, the config parser, and the PNG/JSON
// writers — the host-independent parts of the plugin. Ported from the C
// `VS_LIGHTSHOW_UNIT_TEST` cases, plus adversarial coverage of the safe
// wrappers around the unsafe boundary.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_src_row_mapping() {
        // amount 0: identity.
        assert_eq!(blink_src_row(10, 20, 10, 0), 10);
        assert_eq!(blink_src_row(10, 20, 29, 0), 29);
        assert_eq!(blink_src_row(10, 20, 20, 0), 20);
        // amount 100: everything collapses to the centre row (10 + 20/2).
        assert_eq!(blink_src_row(10, 20, 10, 100), 20);
        assert_eq!(blink_src_row(10, 20, 29, 100), 20);
        assert_eq!(blink_src_row(10, 20, 20, 100), 20);
        // amount 50 on box [0,10): centre 5; row 0 -> 5 + (0-5)/2 = 3.
        assert_eq!(blink_src_row(0, 10, 0, 50), 3);
        assert_eq!(blink_src_row(0, 10, 9, 50), 7);
        assert_eq!(blink_src_row(0, 10, 5, 50), 5);
    }

    #[test]
    fn shift_src_row_mapping() {
        assert_eq!(shift_src_row(10, 0, 0), 0);
        assert_eq!(shift_src_row(10, 5, 3), 2);
        assert_eq!(shift_src_row(10, 0, 3), 0); // top edge replicates
        assert_eq!(shift_src_row(10, 9, -3), 9); // bottom edge replicates
        assert_eq!(shift_src_row(10, 9, 3), 6);
        assert_eq!(shift_src_row(1, 0, 100), 0);
    }

    #[test]
    fn pixel_to_rgba_conversion() {
        assert_eq!(pixel_to_rgba(0xFF112233), [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(pixel_to_rgba(0x00123456), [0x12, 0x34, 0x56, 0x00]);
    }

    fn tmp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "vs_lightshow_rs_test_{}_{}.cfg",
            std::process::id(),
            tag
        ))
    }

    fn write_tmp(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).expect("test setup: write temp file");
    }

    #[test]
    fn config_good_file() {
        let path = tmp_path("good");
        write_tmp(
            &path,
            "# comment\n\ncell_w=96\ncell_h=192\ncols=4\nrows=6\n\
             frame_ms=180\nmoods=idle,blush,wink\n\
             basename=seraphine_sheet_fullbody\noutput_dir=/tmp/ls-out\n",
        );
        let mut cfg = default_cfg();
        assert_eq!(load_config(path.as_os_str(), &mut cfg), Ok(()));
        assert_eq!((cfg.cell_w, cfg.cell_h), (96, 192));
        assert_eq!((cfg.cols, cfg.rows, cfg.frame_ms), (4, 6, 180));
        assert_eq!(cfg.mood_count, 3);
        assert_eq!(buf_str(&cfg.moods[0]), "idle");
        assert_eq!(buf_str(&cfg.moods[2]), "wink");
        assert_eq!(buf_str(&cfg.basename), "seraphine_sheet_fullbody");
        assert_eq!(buf_str(&cfg.output_dir), "/tmp/ls-out");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn config_missing_file_keeps_defaults() {
        let mut cfg = default_cfg();
        let missing = tmp_path("missing-xyz");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(load_config(missing.as_os_str(), &mut cfg), Ok(()));
        assert_eq!(cfg.cell_w, 96);
        assert_eq!(cfg.mood_count, 6);
        assert_eq!(buf_str(&cfg.basename), "sheet");
    }

    #[test]
    fn config_rejects_bad_values_and_keeps_defaults() {
        for body in [
            "cell_w=banana\n",                  // malformed int
            "cel_w=96\n",                       // unknown key
            "cols=64\nrows=64\n",               // cols*rows over the frame cap
            "cell_w=0\n",                       // out of range (low)
            "cell_w=99999\n",                   // out of range (high)
            "cell_w=99999999999999999999999\n", // integer overflow
            "moods=idle,not a mood!\n",         // bad mood token
            "moods=\n",                         // empty mood list
            "moods=idle,\n",                    // trailing comma: empty token
            "cell_w=96 # trailing\n",           // trailing junk is rejected
            "=96\n",                            // empty key
            "cell_w\n",                         // missing '='
        ] {
            let path = tmp_path("bad");
            write_tmp(&path, body);
            let mut cfg = default_cfg();
            assert_eq!(
                load_config(path.as_os_str(), &mut cfg),
                Err(LsError::Config),
                "body {body:?} must be rejected"
            );
            assert_eq!(cfg.cell_w, 96, "cfg must be unchanged on failure");
            assert_eq!(cfg.mood_count, 6, "cfg must be unchanged on failure");
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn config_parser_edges() {
        let path = tmp_path("edges");
        // Whitespace around keys and values is tolerated; later keys win.
        write_tmp(
            &path,
            "  cell_w  =  128 \ncell_w=64\n\tframe_ms\t=\t90\nmoods = idle , blush \n",
        );
        let mut cfg = default_cfg();
        assert_eq!(load_config(path.as_os_str(), &mut cfg), Ok(()));
        assert_eq!(cfg.cell_w, 64);
        assert_eq!(cfg.frame_ms, 90);
        assert_eq!(cfg.mood_count, 2);
        assert_eq!(buf_str(&cfg.moods[0]), "idle");
        assert_eq!(buf_str(&cfg.moods[1]), "blush");
        // A 300-char line fills the 255-byte C buffer without a newline.
        write_tmp(&path, &format!("cell_w={}\n", "9".repeat(300)));
        let mut cfg = default_cfg();
        assert_eq!(
            load_config(path.as_os_str(), &mut cfg),
            Err(LsError::Config)
        );
        // 65 lines exceed the line cap.
        write_tmp(&path, &"# pad\n".repeat(65));
        let mut cfg = default_cfg();
        assert_eq!(
            load_config(path.as_os_str(), &mut cfg),
            Err(LsError::Config)
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn eye_box_clamps_and_never_invents_pixels() {
        // 8x8 layer: pixel (x,y) holds its own index, so any invented pixel
        // would be detectable.
        let (w, h) = (8i32, 8i32);
        let snap: Vec<u32> = (0..64u32).map(|i| 0xFF00_0000 | i).collect();
        // Box hangs off the left edge; amount 100 collapses rows to the
        // clamped box centre (y0=2, box_h=4 -> centre row 4).
        let mut live = snap.clone();
        apply_eye_box(&mut live, &snap, w, h, -2, 2, 6, 4, 100);
        for y in 0..8 {
            for x in 0..8 {
                let in_box = (2..6).contains(&y) && (0..4).contains(&x);
                let expect = snap[((if in_box { 4 } else { y }) * 8 + x) as usize];
                assert_eq!(live[(y * 8 + x) as usize], expect, "pixel ({x},{y})");
            }
        }
        // Amount 0 is a no-op even with a wild box.
        let mut live = snap.clone();
        apply_eye_box(&mut live, &snap, w, h, -100, -100, 10000, 10000, 0);
        assert_eq!(live, snap);
        // A box missing the layer entirely is a no-op.
        let mut live = snap.clone();
        apply_eye_box(&mut live, &snap, w, h, 100, 100, 10, 10, 100);
        assert_eq!(live, snap);
        // Hostile parameter magnitudes cannot wrap the clamping arithmetic.
        let mut live = snap.clone();
        apply_eye_box(
            &mut live,
            &snap,
            w,
            h,
            i32::MIN,
            i32::MIN,
            i32::MAX,
            i32::MAX,
            100,
        );
        for px in &live {
            assert!(
                snap.contains(px),
                "every output pixel must come from the input"
            );
        }
    }

    #[test]
    fn shift_rows_replicates_edges() {
        // 4-wide, 3-tall: row r holds r.
        let (w, h) = (4i32, 3i32);
        let snap: Vec<u32> = (0..12).map(|i| (i / 4) as u32).collect();
        let mut live = vec![0xDEADu32; 12];
        shift_rows(&mut live, &snap, w, h, 1);
        // dst row y takes src row y-1, clamped: [0, 0, 1].
        assert_eq!(&live[0..4], &[0, 0, 0, 0]);
        assert_eq!(&live[4..8], &[0, 0, 0, 0]);
        assert_eq!(&live[8..12], &[1, 1, 1, 1]);
        let mut live = vec![0xDEADu32; 12];
        shift_rows(&mut live, &snap, w, h, -1);
        // rows = [1, 2, 2].
        assert_eq!(&live[0..4], &[1, 1, 1, 1]);
        assert_eq!(&live[4..8], &[2, 2, 2, 2]);
        assert_eq!(&live[8..12], &[2, 2, 2, 2]);
    }

    /// Concatenate all IDAT payloads in a PNG, verifying every chunk CRC.
    fn concat_idat(png: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut p = 8usize;
        while p + 8 <= png.len() {
            let len = u32::from_be_bytes(png[p..p + 4].try_into().unwrap()) as usize;
            let typ = &png[p + 4..p + 8];
            let data = &png[p + 8..p + 8 + len];
            if typ == b"IDAT" {
                out.extend_from_slice(data);
            }
            let stored = u32::from_be_bytes(png[p + 8 + len..p + 12 + len].try_into().unwrap());
            let crc = crc32_update(crc32_update(0xFFFF_FFFF, typ), data);
            assert_eq!(stored, crc ^ 0xFFFF_FFFF, "chunk CRC mismatch");
            p += 12 + len;
        }
        out
    }

    /// Inflate a zlib stream holding only stored (uncompressed) DEFLATE
    /// blocks — everything this encoder emits — verifying the Adler-32.
    fn inflate_stored_zlib(zlib: &[u8]) -> Vec<u8> {
        assert!(zlib.len() >= 6, "zlib stream too short");
        assert_eq!(&zlib[0..2], &[0x78, 0x01], "zlib header");
        let mut out = Vec::new();
        let mut p = 2usize;
        loop {
            let header = zlib[p];
            p += 1;
            let (bfinal, btype) = (header & 1, (header >> 1) & 0x03);
            assert_eq!(btype, 0, "only stored blocks expected");
            assert_eq!(header >> 3, 0, "reserved bits must be zero");
            let len = u16::from_le_bytes([zlib[p], zlib[p + 1]]) as usize;
            let nlen = u16::from_le_bytes([zlib[p + 2], zlib[p + 3]]);
            assert_eq!(nlen, !(len as u16), "NLEN must complement LEN");
            p += 4;
            out.extend_from_slice(&zlib[p..p + len]);
            p += len;
            if bfinal == 1 {
                let stored = u32::from_be_bytes(zlib[p..p + 4].try_into().unwrap());
                assert_eq!(stored, adler32(&out), "adler32 mismatch");
                assert_eq!(p + 4, zlib.len(), "trailing bytes after zlib stream");
                break;
            }
        }
        out
    }

    #[test]
    fn png_signature_ihdr_and_pixel_round_trip() {
        // 2 cols x 1 row of 2x2 RGBA cells, checkerboarded through the real
        // pixel converter (mirrors the C export test's 4x2 sheet).
        let mut rgba = [0u8; 4 * 2 * 4];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let argb = if i % 2 == 0 {
                0xFFAA1122u32
            } else {
                0xFF33BB44u32
            };
            px.copy_from_slice(&pixel_to_rgba(argb));
        }
        let png = encode_png(4, 2, &rgba).expect("encode test PNG");
        // PNG signature.
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        // IHDR chunk: length(4) + "IHDR", then width/height big-endian.
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &[0, 0, 0, 4]); // width 4
        assert_eq!(&png[20..24], &[0, 0, 0, 2]); // height 2
        assert_eq!(png[24], 8); // bit depth
        assert_eq!(png[25], 6); // color type: truecolor with alpha
        // Round-trip: inflate the stored-block IDATs and compare pixels.
        let zlib = concat_idat(&png);
        let raw = inflate_stored_zlib(&zlib);
        let mut pixels = Vec::new();
        for row in raw.as_chunks::<{ 1 + 4 * 4 }>().0 {
            assert_eq!(row[0], 0x00, "filter type None");
            pixels.extend_from_slice(&row[1..]);
        }
        assert_eq!(pixels, rgba);
        // IEND trailer present (type + well-known CRC 0xAE426082).
        assert!(png.ends_with(b"IEND\xae\x42\x60\x82"));
    }

    #[test]
    fn png_rejects_bad_geometry() {
        let rgba = [0u8; 16];
        assert_eq!(encode_png(0, 2, &rgba), Err(LsError::BadGeometry));
        assert_eq!(encode_png(2, 2, &[0u8; 15]), Err(LsError::BadGeometry));
    }

    #[test]
    fn json_contract_fields() {
        let mut cfg = default_cfg();
        cfg.cell_w = 2;
        cfg.cell_h = 2;
        cfg.cols = 2;
        cfg.rows = 1;
        cfg.frame_ms = 180;
        cfg.mood_count = 1;
        buf_set(&mut cfg.moods[0], b"idle");
        let path = tmp_path("json");
        write_json(path.as_os_str(), &cfg, "sheet.png").expect("write test JSON");
        let body = std::fs::read_to_string(&path).expect("read test JSON");
        for needle in [
            "\"generator\": \"vs-lightshow 1.1.0\"",
            "\"image\": \"sheet.png\"",
            "\"cell_w\": 2",
            "\"cell_h\": 2",
            "\"cols\": 2",
            "\"rows\": 1",
            "\"frame_ms\": 180",
            "\"moods\": [\"idle\"]",
            "\"index\": 0",
            "\"index\": 1",
            // Second frame sits at x = 1 cell * 2 px.
            "\"x\": 2, \"y\": 0",
            "\"mood\": \"idle\"",
            "\"note\": \"light-show builds its Bevy TextureAtlasLayout",
        ] {
            assert!(body.contains(needle), "JSON missing {needle:?}:\n{body}");
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn json_unknown_mood_beyond_list() {
        // More rows than moods: extra rows are labelled "unknown" (C parity).
        let mut cfg = default_cfg();
        cfg.cols = 1;
        cfg.rows = 2;
        cfg.mood_count = 1;
        buf_set(&mut cfg.moods[0], b"idle");
        let path = tmp_path("json2");
        write_json(path.as_os_str(), &cfg, "sheet.png").expect("write test JSON");
        let body = std::fs::read_to_string(&path).expect("read test JSON");
        assert!(body.contains("\"row\": 1, \"x\": 0, \"y\": 192"));
        assert!(body.contains("\"mood\": \"unknown\""));
        std::fs::remove_file(&path).ok();
    }
}
