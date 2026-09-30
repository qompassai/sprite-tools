//! vs-palette-variants — "palette variant tools", a VoidSprite native plugin.
//!
//! One filter, "Palette: hue shift variant": rotates the hue of every visible
//! pixel on the active layer by a configurable number of degrees and
//! optionally rescales saturation, generating alternate palette variants of
//! a sprite sheet (team colors, day/night tints). The alpha channel is
//! preserved byte-exact; fully transparent pixels are skipped so invisible
//! data is never recolored.
//!
//! Layout, top to bottom: the `sdk` module transcribes the C ABI of
//! `voidsprite_sdk_c.h` (every field offset is asserted against values
//! probed from the real header on the build machine); the `color` module
//! holds the pure, unsafe-free HSV math; the bottom of the file holds the
//! six exported entry points and the filter callback. All `unsafe` lives in
//! the thin host wrappers in `sdk`; nothing unwinds across the FFI boundary
//! (release profile sets `panic = "abort"` as a backstop).

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(warnings)]

use core::ffi::{CStr, c_char, c_int};
use std::sync::OnceLock;

use sdk::{
    VSPFilter, VSPLayer, VoidspriteSDK, host_filter_int, host_layer_info, host_new_int_parameter,
    host_post_error, host_raw_pixels, host_register_filter,
};

/* ------------------------------------------------------------------ */
/* Constants: bounds carry units.                                       */
/* ------------------------------------------------------------------ */

/// Largest single image dimension this filter will touch (px). Mirrors the
/// bound used by the companion C plugin.
const MAX_DIM_PX: i32 = 8192;

/// `VSP_LAYER_RGBA` from the SDK header.
const VSP_LAYER_RGBA: i32 = 0x01;

/// `VS_SDK_VERSION` from the SDK header.
const VS_SDK_VERSION: c_int = 1;

/// "hue shift degrees" parameter bounds and default.
const HUE_SHIFT_MIN_DEG: c_int = -180;
const HUE_SHIFT_MAX_DEG: c_int = 180;
const HUE_SHIFT_DEFAULT_DEG: c_int = 0;

/// "saturation %" parameter bounds and default.
const SATURATION_MIN_PCT: c_int = 0;
const SATURATION_MAX_PCT: c_int = 200;
const SATURATION_DEFAULT_PCT: c_int = 100;

/// Filter name shown in the host's filter list; parameter names shown in the
/// SDK-generated parameter dialog.
const FILTER_NAME: &CStr = c"Palette: hue shift variant";
const PARAM_HUE_SHIFT: &CStr = c"hue shift degrees";
const PARAM_SATURATION: &CStr = c"saturation %";

/* ------------------------------------------------------------------ */
/* Host SDK table: captured once in pluginInit, never mutated after.     */
/* ------------------------------------------------------------------ */

/// The host SDK table, captured once in `pluginInit`. Checked on every
/// entry point; never mutated afterwards.
static HOST_SDK: OnceLock<HostSdk> = OnceLock::new();

/// Pointer to the host SDK table. The host hands it to `pluginInit` once at
/// load and never mutates the table afterwards; every later use is a read
/// through the function table, exactly like the C plugin's
/// `static voidspriteSDK *g_sdk`. Sharing the pointer is therefore sound.
struct HostSdk(*mut VoidspriteSDK);

// SAFETY: the host table is written once in pluginInit and only read after.
unsafe impl Send for HostSdk {}
// SAFETY: same as above; concurrent reads of the table are safe.
unsafe impl Sync for HostSdk {}

/// Returns the captured host table, or `None` when `pluginInit` never ran
/// (or was handed a null table).
fn host_sdk() -> Option<*mut VoidspriteSDK> {
    HOST_SDK
        .get()
        .map(|host| host.0)
        .filter(|sdk| !sdk.is_null())
}

/* ------------------------------------------------------------------ */
/* Filter application: safe logic over the thin unsafe wrappers.        */
/* ------------------------------------------------------------------ */

/// Expected failures of [`apply_hue_shift`]. The layer is left unchanged on
/// every error; the caller decides whether to notify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyError {
    /// `pluginInit` never captured a host table.
    NoHost,
    /// The host passed a null layer or filter pointer.
    NullArgument,
    /// The layer is missing, not RGBA, or has out-of-bounds geometry.
    BadLayer,
    /// The host would not hand over the raw pixel buffer.
    NoPixelData,
}

/// Applies the hue shift to every visible pixel of `layer`, in place.
///
/// Contract: `layer`/`filter` are the pointers the host passed to the filter
/// callback. Reads the "hue shift degrees" and "saturation %" parameters
/// (host dialog clamps them; re-clamped here defensively), then rewrites each
/// pixel with [`color::shift_pixel`]. Panic-free by construction: checked
/// integer math, validated dimensions, iterator-only pixel access.
fn apply_hue_shift(layer: *mut VSPLayer, filter: *mut VSPFilter) -> Result<(), ApplyError> {
    let sdk = host_sdk().ok_or(ApplyError::NoHost)?;
    if layer.is_null() || filter.is_null() {
        return Err(ApplyError::NullArgument);
    }
    // SAFETY: sdk is the live host table from pluginInit; layer is a valid
    // layer pointer. The returned info struct is freed with the host's own
    // util_free, per the header contract.
    let (layer_type, width, height) =
        unsafe { host_layer_info(sdk, layer) }.ok_or(ApplyError::BadLayer)?;
    if (layer_type & VSP_LAYER_RGBA) == 0 {
        return Err(ApplyError::BadLayer);
    }
    if !(1..=MAX_DIM_PX).contains(&width) || !(1..=MAX_DIM_PX).contains(&height) {
        return Err(ApplyError::BadLayer);
    }
    // SAFETY: same contract as above; filter is the valid filter pointer the
    // host passed to this callback.
    let hue_shift_deg =
        unsafe { host_filter_int(sdk, filter, PARAM_HUE_SHIFT, HUE_SHIFT_DEFAULT_DEG) }
            .clamp(HUE_SHIFT_MIN_DEG, HUE_SHIFT_MAX_DEG);
    let saturation_pct =
        unsafe { host_filter_int(sdk, filter, PARAM_SATURATION, SATURATION_DEFAULT_PCT) }
            .clamp(SATURATION_MIN_PCT, SATURATION_MAX_PCT);
    // width/height are in 1..=8192, so the product fits i64 and usize.
    let pixel_count =
        usize::try_from(i64::from(width) * i64::from(height)).map_err(|_| ApplyError::BadLayer)?;
    // SAFETY: same contract; the host guarantees width*height u32 pixels.
    let pixels_ptr = unsafe { host_raw_pixels(sdk, layer) };
    if pixels_ptr.is_null() {
        return Err(ApplyError::NoPixelData);
    }
    // SAFETY: pixels_ptr points at width*height host-owned u32 pixels; the
    // slice borrows them for exactly this loop and is never retained.
    let pixels: &mut [u32] = unsafe { core::slice::from_raw_parts_mut(pixels_ptr, pixel_count) };
    let hue_shift = hue_shift_deg as f32;
    let saturation_scale = saturation_pct as f32 / 100.0;
    for pixel in pixels.iter_mut() {
        *pixel = color::shift_pixel(*pixel, hue_shift, saturation_scale);
    }
    Ok(())
}

/// The filter callback handed to the host. Must never unwind across the FFI
/// boundary: every path through [`apply_hue_shift`] is panic-free (checked
/// math, validated bounds, iterator-only access), and the release profile
/// aborts on panic as a backstop.
extern "C" fn palette_filter_entry(layer: *mut VSPLayer, filter: *mut VSPFilter) {
    if let Err(err) = apply_hue_shift(layer, filter) {
        // NoHost means pluginInit never ran; there is no host to notify.
        if err != ApplyError::NoHost {
            if let Some(sdk) = host_sdk() {
                // SAFETY: sdk is the live host table from pluginInit.
                unsafe {
                    host_post_error(
                        sdk,
                        c"Palette: hue shift variant",
                        c"Layer is not RGBA or has bad geometry.",
                    );
                }
            }
        }
    }
}

/* ------------------------------------------------------------------ */
/* Exported entry points: the six symbols the host looks up.            */
/* ------------------------------------------------------------------ */

/// Called once by VoidSprite at load time. Captures the host SDK table and
/// registers the filter; the host builds the parameter dialog from the
/// declared int parameters.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn pluginInit(sdk: *mut VoidspriteSDK) {
    if sdk.is_null() {
        return;
    }
    // First call wins; the host only calls this once per load.
    let _ = HOST_SDK.set(HostSdk(sdk));
    // SAFETY: sdk is the non-null host table; FILTER_NAME is NUL-terminated;
    // the returned filter pointer is host-owned.
    let filter = unsafe { host_register_filter(sdk, FILTER_NAME, palette_filter_entry) };
    if filter.is_null() {
        return;
    }
    // SAFETY: same contract; filter is the valid host-owned filter.
    unsafe {
        host_new_int_parameter(
            sdk,
            filter,
            PARAM_HUE_SHIFT,
            HUE_SHIFT_MIN_DEG,
            HUE_SHIFT_MAX_DEG,
            HUE_SHIFT_DEFAULT_DEG,
        );
        host_new_int_parameter(
            sdk,
            filter,
            PARAM_SATURATION,
            SATURATION_MIN_PCT,
            SATURATION_MAX_PCT,
            SATURATION_DEFAULT_PCT,
        );
    }
}

/// The SDK version this plugin was built against (`VS_SDK_VERSION`).
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn voidspriteSDKVersion() -> c_int {
    VS_SDK_VERSION
}

/// Display name shown by the host ("Loaded plugin: palette variant tools").
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn getPluginName() -> *const c_char {
    c"palette variant tools".as_ptr()
}

/// Plugin version.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn getPluginVersion() -> *const c_char {
    c"1.0.0".as_ptr()
}

/// One-line description shown by the host.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn getPluginDescription() -> *const c_char {
    c"Hue-shift filter that generates alternate palette variants of a sprite sheet. Alpha is preserved; fully transparent pixels are skipped.".as_ptr()
}

/// Plugin authors.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn getPluginAuthors() -> *const c_char {
    c"Pax (Qompass AI)".as_ptr()
}

/* ------------------------------------------------------------------ */
/* sdk: C ABI transcription of voidsprite_sdk_c.h plus thin wrappers.    */
/* ------------------------------------------------------------------ */

mod sdk {
    //! Thin, unsafe-only wrappers over the host SDK table.
    //!
    //! [`VoidspriteSDK`] is a `#[repr(C, packed(1))]` transcription of the
    //! `voidspriteSDK` struct in `voidsprite_sdk_c.h` (which itself carries
    //! `#pragma pack(push, 1)`). Field order and the layout assertions below
    //! were checked against a probe program compiled from the real header on
    //! the build machine; if the SDK header ever changes, these `const`
    //! assertions fail the build instead of silently mis-calling the host.
    //!
    //! Every wrapper documents its safety contract locally. All function
    //! table slots are `Option<unsafe extern "C" fn>` so a host that leaves
    //! a slot null degrades to `None` instead of a null call.

    use core::ffi::{CStr, c_char, c_int, c_void};

    /// Opaque host handles. Never constructed or dereferenced on the Rust
    /// side; only carried as pointers.
    pub enum VSPLayer {}
    pub enum VSPFilter {}
    pub enum VSPFileExporter {}
    pub enum VSPEditorContext {}
    pub enum VSPBrush {}

    /// Callback type for `registerFilter`, matching the header.
    pub type FilterFn = unsafe extern "C" fn(*mut VSPLayer, *mut VSPFilter);
    type ImportFn = unsafe extern "C" fn(*mut c_char) -> *mut VSPLayer;
    type CanImportFn = unsafe extern "C" fn(*mut c_char) -> bool;
    type ExportFn = unsafe extern "C" fn(*mut VSPLayer, *mut c_char) -> bool;
    type CanExportFn = unsafe extern "C" fn(*mut VSPLayer) -> bool;
    type BrushClickFn = unsafe extern "C" fn(*mut VSPBrush, *mut VSPEditorContext, c_int, c_int);
    type BrushDragFn =
        unsafe extern "C" fn(*mut VSPBrush, *mut VSPEditorContext, c_int, c_int, c_int, c_int);
    type EditorActionFn = unsafe extern "C" fn(*mut VSPEditorContext);

    /// `voidspriteSDK`, field-for-field in header order. All slots are
    /// `Option` so missing host functions are `None`, never null calls.
    /// (8-byte pointers throughout, so `packed(1)` changes nothing, but the
    /// attribute mirrors the header exactly.)
    #[repr(C, packed(1))]
    pub struct VoidspriteSDK {
        pub util_fopen_utf8:
            Option<unsafe extern "C" fn(*mut c_char, *const c_char) -> *mut c_void>,
        pub register_filter:
            Option<unsafe extern "C" fn(*const c_char, Option<FilterFn>) -> *mut VSPFilter>,
        pub register_layer_importer: Option<
            unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                c_int,
                *mut VSPFileExporter,
                Option<ImportFn>,
                Option<CanImportFn>,
            ),
        >,
        pub register_layer_exporter: Option<
            unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                c_int,
                Option<ExportFn>,
                Option<CanExportFn>,
            ) -> *mut VSPFileExporter,
        >,
        pub layer_alloc_new: Option<unsafe extern "C" fn(c_int, c_int, c_int) -> *mut VSPLayer>,
        pub layer_free: Option<unsafe extern "C" fn(*mut VSPLayer)>,
        pub layer_get_info: Option<unsafe extern "C" fn(*mut VSPLayer) -> *mut VSPLayerInfo>,
        pub layer_set_pixel: Option<unsafe extern "C" fn(*mut VSPLayer, c_int, c_int, u32)>,
        pub layer_get_pixel: Option<unsafe extern "C" fn(*mut VSPLayer, c_int, c_int) -> u32>,
        pub layer_get_raw_pixel_data: Option<unsafe extern "C" fn(*mut VSPLayer) -> *mut u32>,
        pub filter_new_bool_parameter:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, bool)>,
        pub filter_new_int_parameter:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, c_int, c_int, c_int)>,
        pub filter_new_double_parameter:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, f64, f64, f64)>,
        pub filter_new_double_range_parameter:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char, f64, f64, f64, f64, u32)>,
        pub filter_get_double_value:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
        pub filter_get_int_value:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> c_int>,
        pub filter_get_range_value1:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
        pub filter_get_range_value2:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> f64>,
        pub filter_get_bool_value:
            Option<unsafe extern "C" fn(*mut VSPFilter, *const c_char) -> bool>,
        pub util_free: Option<unsafe extern "C" fn(*mut c_void)>,
        pub editor_get_active_color: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> u32>,
        pub editor_get_num_layers: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
        pub editor_get_layer:
            Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int) -> *mut VSPLayer>,
        pub editor_get_active_layer:
            Option<unsafe extern "C" fn(*mut VSPEditorContext) -> *mut VSPLayer>,
        pub register_brush: Option<
            unsafe extern "C" fn(
                *const c_char,
                *const c_char,
                bool,
                Option<BrushClickFn>,
                Option<BrushDragFn>,
                Option<BrushClickFn>,
            ) -> *mut VSPBrush,
        >,
        pub editor_set_pixel:
            Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int, c_int, u32)>,
        pub vsp_post_notification:
            Option<unsafe extern "C" fn(*const c_char, *const c_char, u32, c_int)>,
        pub vsp_post_success_notification:
            Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
        pub vsp_post_error_notification: Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
        pub editor_undo_push_layer_state:
            Option<unsafe extern "C" fn(*mut VSPEditorContext, *mut VSPLayer)>,
        pub register_editor_action:
            Option<unsafe extern "C" fn(*const c_char, Option<EditorActionFn>)>,
        pub editor_flatten_image:
            Option<unsafe extern "C" fn(*mut VSPEditorContext) -> *mut VSPLayer>,
        pub editor_flatten_frame:
            Option<unsafe extern "C" fn(*mut VSPEditorContext, c_int) -> *mut VSPLayer>,
        pub editor_get_num_frames: Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
        pub editor_get_active_frame_index:
            Option<unsafe extern "C" fn(*mut VSPEditorContext) -> c_int>,
        pub vsp_get_localized_string: Option<unsafe extern "C" fn(*const c_char) -> *const c_char>,
    }

    /// `VSPLayerInfo`: three packed `int32_t`, freed with `util_free`.
    #[repr(C, packed(1))]
    pub struct VSPLayerInfo {
        pub layer_type: i32,
        pub width: i32,
        pub height: i32,
    }

    /// Layout contract with `voidsprite_sdk_c.h`, probed 2026-09-30 on the
    /// build machine (x86_64, gcc). If the SDK header ever changes, these
    /// fail the build instead of silently mis-calling the host.
    const _: () = {
        assert!(core::mem::size_of::<VoidspriteSDK>() == 288);
        assert!(core::mem::offset_of!(VoidspriteSDK, util_fopen_utf8) == 0);
        assert!(core::mem::offset_of!(VoidspriteSDK, register_filter) == 8);
        assert!(core::mem::offset_of!(VoidspriteSDK, register_layer_importer) == 16);
        assert!(core::mem::offset_of!(VoidspriteSDK, register_layer_exporter) == 24);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_alloc_new) == 32);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_free) == 40);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_get_info) == 48);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_set_pixel) == 56);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_get_pixel) == 64);
        assert!(core::mem::offset_of!(VoidspriteSDK, layer_get_raw_pixel_data) == 72);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_new_bool_parameter) == 80);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_new_int_parameter) == 88);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_new_double_parameter) == 96);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_new_double_range_parameter) == 104);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_get_double_value) == 112);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_get_int_value) == 120);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_get_range_value1) == 128);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_get_range_value2) == 136);
        assert!(core::mem::offset_of!(VoidspriteSDK, filter_get_bool_value) == 144);
        assert!(core::mem::offset_of!(VoidspriteSDK, util_free) == 152);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_active_color) == 160);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_num_layers) == 168);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_layer) == 176);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_active_layer) == 184);
        assert!(core::mem::offset_of!(VoidspriteSDK, register_brush) == 192);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_set_pixel) == 200);
        assert!(core::mem::offset_of!(VoidspriteSDK, vsp_post_notification) == 208);
        assert!(core::mem::offset_of!(VoidspriteSDK, vsp_post_success_notification) == 216);
        assert!(core::mem::offset_of!(VoidspriteSDK, vsp_post_error_notification) == 224);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_undo_push_layer_state) == 232);
        assert!(core::mem::offset_of!(VoidspriteSDK, register_editor_action) == 240);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_flatten_image) == 248);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_flatten_frame) == 256);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_num_frames) == 264);
        assert!(core::mem::offset_of!(VoidspriteSDK, editor_get_active_frame_index) == 272);
        assert!(core::mem::offset_of!(VoidspriteSDK, vsp_get_localized_string) == 280);
        assert!(core::mem::size_of::<VSPLayerInfo>() == 12);
        assert!(core::mem::offset_of!(VSPLayerInfo, layer_type) == 0);
        assert!(core::mem::offset_of!(VSPLayerInfo, width) == 4);
        assert!(core::mem::offset_of!(VSPLayerInfo, height) == 8);
    };

    /// Returns `(layer_type, width, height)` for a layer, or `None` when the
    /// host has no `layerGetInfo` or it returns null.
    ///
    /// # Safety
    /// `sdk` must be the live host table from `pluginInit`; `layer` must be
    /// a valid layer pointer. The info struct is freed with the host's own
    /// `util_free`, per the header contract ("Free the returned
    /// VSPLayerInfo pointer after use").
    pub unsafe fn host_layer_info(
        sdk: *mut VoidspriteSDK,
        layer: *mut VSPLayer,
    ) -> Option<(i32, i32, i32)> {
        unsafe {
            let get_info = (*sdk).layer_get_info?;
            let info = get_info(layer);
            if info.is_null() {
                return None;
            }
            // By-value copies of Copy fields: no reference into the packed
            // struct is ever formed.
            let layer_type: i32 = (*info).layer_type;
            let width: i32 = (*info).width;
            let height: i32 = (*info).height;
            if let Some(free_fn) = (*sdk).util_free {
                free_fn(info as *mut c_void);
            }
            Some((layer_type, width, height))
        }
    }

    /// Returns the layer's raw pixel buffer, or null when unavailable.
    ///
    /// # Safety
    /// `sdk` must be the live host table; `layer` must be valid. The pointer
    /// is host-owned: never freed, never read/written out of bounds.
    pub unsafe fn host_raw_pixels(sdk: *mut VoidspriteSDK, layer: *mut VSPLayer) -> *mut u32 {
        unsafe {
            match (*sdk).layer_get_raw_pixel_data {
                Some(get_pixels) => get_pixels(layer),
                None => core::ptr::null_mut(),
            }
        }
    }

    /// Registers a filter with the host. Returns the host-owned filter
    /// handle, or null when the host has no `registerFilter`.
    ///
    /// # Safety
    /// `sdk` must be the live host table; `name` must be NUL-terminated.
    pub unsafe fn host_register_filter(
        sdk: *mut VoidspriteSDK,
        name: &CStr,
        func: FilterFn,
    ) -> *mut VSPFilter {
        unsafe {
            match (*sdk).register_filter {
                Some(register) => register(name.as_ptr(), Some(func)),
                None => core::ptr::null_mut(),
            }
        }
    }

    /// Declares an int parameter on a filter (drives the host's parameter
    /// dialog). No-op when the host has no `filterNewIntParameter`.
    ///
    /// # Safety
    /// `sdk` must be the live host table; `filter` a valid host-owned
    /// filter; `name` NUL-terminated.
    pub unsafe fn host_new_int_parameter(
        sdk: *mut VoidspriteSDK,
        filter: *mut VSPFilter,
        name: &CStr,
        min: c_int,
        max: c_int,
        default: c_int,
    ) {
        unsafe {
            if let Some(new_parameter) = (*sdk).filter_new_int_parameter {
                new_parameter(filter, name.as_ptr(), min, max, default);
            }
        }
    }

    /// Reads an int parameter value, falling back to `default` when the host
    /// has no `filterGetIntValue`.
    ///
    /// # Safety
    /// `sdk` must be the live host table; `filter` a valid host-owned
    /// filter; `name` NUL-terminated.
    pub unsafe fn host_filter_int(
        sdk: *mut VoidspriteSDK,
        filter: *mut VSPFilter,
        name: &CStr,
        default: c_int,
    ) -> c_int {
        unsafe {
            match (*sdk).filter_get_int_value {
                Some(get_value) => get_value(filter, name.as_ptr()),
                None => default,
            }
        }
    }

    /// Posts an error notification. No-op when the host has no
    /// `vspPostErrorNotification`.
    ///
    /// # Safety
    /// `sdk` must be the live host table; `title`/`message` NUL-terminated.
    pub unsafe fn host_post_error(sdk: *mut VoidspriteSDK, title: &CStr, message: &CStr) {
        unsafe {
            if let Some(post_error) = (*sdk).vsp_post_error_notification {
                post_error(title.as_ptr(), message.as_ptr());
            }
        }
    }
}

/* ------------------------------------------------------------------ */
/* color: pure HSV pixel math. No SDK contact, no unsafe.                */
/* ------------------------------------------------------------------ */

mod color {
    //! Pure HSV pixel math. No SDK contact, no `unsafe`: unit-testable.
    #![forbid(unsafe_code)]

    /// Converts 8-bit RGB to HSV.
    ///
    /// Returns `(hue_deg, saturation, value)` with `hue_deg` in `[0, 360)`
    /// and `saturation`/`value` in `[0, 1]`. Grayscale inputs yield
    /// saturation 0 and hue 0.
    pub fn rgb_to_hsv(red: u8, green: u8, blue: u8) -> (f32, f32, f32) {
        let r = f32::from(red) / 255.0;
        let g = f32::from(green) / 255.0;
        let b = f32::from(blue) / 255.0;
        let max_c = r.max(g).max(b);
        let min_c = r.min(g).min(b);
        let delta = max_c - min_c;
        let hue = if delta <= 0.0 {
            0.0
        } else if max_c == r {
            60.0 * (((g - b) / delta) % 6.0)
        } else if max_c == g {
            60.0 * ((b - r) / delta + 2.0)
        } else {
            60.0 * ((r - g) / delta + 4.0)
        };
        // The red sector can produce a negative hue (magenta side); fold it
        // into [0, 360) once, here, so callers never see a negative hue.
        let hue = if hue < 0.0 { hue + 360.0 } else { hue };
        let saturation = if max_c <= 0.0 { 0.0 } else { delta / max_c };
        (hue, saturation, max_c)
    }

    /// Converts HSV back to 8-bit RGB, rounding each channel to nearest.
    ///
    /// Contract: `hue_deg` in `[0, 360)`, `saturation` and `value` in
    /// `[0, 1]`. Debug builds assert the contract; callers
    /// ([`shift_pixel`]) guarantee it, so release performs no clamping of
    /// the hue itself.
    pub fn hsv_to_rgb(hue_deg: f32, saturation: f32, value: f32) -> (u8, u8, u8) {
        debug_assert!(
            (0.0..360.0).contains(&hue_deg),
            "hue_deg out of range: {hue_deg}"
        );
        debug_assert!(
            (0.0..=1.0).contains(&saturation),
            "saturation out of range: {saturation}"
        );
        debug_assert!((0.0..=1.0).contains(&value), "value out of range: {value}");
        let chroma = value * saturation;
        let sector_pos = hue_deg / 60.0;
        let x = chroma * (1.0 - (sector_pos % 2.0 - 1.0).abs());
        let (r1, g1, b1) = match sector_pos.floor() as i32 {
            0 => (chroma, x, 0.0),
            1 => (x, chroma, 0.0),
            2 => (0.0, chroma, x),
            3 => (0.0, x, chroma),
            4 => (x, 0.0, chroma),
            _ => (chroma, 0.0, x),
        };
        let m = value - chroma;
        (
            channel_to_u8(r1 + m),
            channel_to_u8(g1 + m),
            channel_to_u8(b1 + m),
        )
    }

    /// Wraps any finite hue into `[0, 360)`.
    pub fn wrap_hue(hue_deg: f32) -> f32 {
        (hue_deg % 360.0 + 360.0) % 360.0
    }

    /// Applies a hue rotation and saturation scale to one 0xAARRGGBB pixel.
    ///
    /// Contract: `hue_shift_deg` is any finite value (wrapped into range
    /// internally); `saturation_scale` is a multiplier where 1.0 keeps
    /// saturation, 0.0 yields grayscale, and 2.0 doubles it (clipped at
    /// full saturation). Alpha is preserved byte-exact. Fully transparent
    /// pixels (alpha 0) are returned bit-identical: invisible data is never
    /// recolored.
    pub fn shift_pixel(pixel: u32, hue_shift_deg: f32, saturation_scale: f32) -> u32 {
        let alpha = pixel >> 24;
        if alpha == 0 {
            return pixel;
        }
        let red = ((pixel >> 16) & 0xFF) as u8;
        let green = ((pixel >> 8) & 0xFF) as u8;
        let blue = (pixel & 0xFF) as u8;
        let (hue, saturation, value) = rgb_to_hsv(red, green, blue);
        let (r, g, b) = hsv_to_rgb(
            wrap_hue(hue + hue_shift_deg),
            (saturation * saturation_scale).clamp(0.0, 1.0),
            value,
        );
        (alpha << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
    }

    /// Scales a `[0, 1]` channel to a byte with round-to-nearest, clamping
    /// float error at the rails so 1.0 can never become 256.
    fn channel_to_u8(channel: f32) -> u8 {
        (channel * 255.0).round().clamp(0.0, 255.0) as u8
    }
}

/* ------------------------------------------------------------------ */
/* Unit tests: pure math plus the host-failure paths.                    */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::color::{hsv_to_rgb, rgb_to_hsv, shift_pixel, wrap_hue};
    use super::sdk::{VSPFilter, VSPLayer, VoidspriteSDK};
    use super::{ApplyError, HOST_SDK, HostSdk, apply_hue_shift, host_sdk};
    use core::ptr;

    /// Packs an 0xAARRGGBB pixel.
    const fn argb(alpha: u8, red: u8, green: u8, blue: u8) -> u32 {
        (alpha as u32) << 24 | (red as u32) << 16 | (green as u32) << 8 | blue as u32
    }

    #[test]
    fn rgb_hsv_rgb_round_trip_on_primaries_secondaries() {
        let cases = [
            (255, 0, 0),     // red
            (0, 255, 0),     // green
            (0, 0, 255),     // blue
            (0, 255, 255),   // cyan
            (255, 0, 255),   // magenta
            (255, 255, 0),   // yellow
            (255, 255, 255), // white
            (0, 0, 0),       // black
        ];
        for (r, g, b) in cases {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            assert_eq!(
                hsv_to_rgb(h, s, v),
                (r, g, b),
                "round trip failed for ({r}, {g}, {b})"
            );
        }
    }

    #[test]
    fn plus_120_deg_maps_red_to_green() {
        assert_eq!(
            shift_pixel(argb(255, 255, 0, 0), 120.0, 1.0),
            argb(255, 0, 255, 0)
        );
    }

    #[test]
    fn plus_120_deg_maps_blue_to_red() {
        assert_eq!(
            shift_pixel(argb(255, 0, 0, 255), 120.0, 1.0),
            argb(255, 255, 0, 0)
        );
    }

    #[test]
    fn minus_120_deg_maps_red_to_blue() {
        assert_eq!(
            shift_pixel(argb(255, 255, 0, 0), -120.0, 1.0),
            argb(255, 0, 0, 255)
        );
    }

    #[test]
    fn alpha_preserved_byte_exact() {
        // Semi-transparent red shifted +120 deg keeps its alpha byte.
        assert_eq!(
            shift_pixel(argb(0x7F, 255, 0, 0), 120.0, 1.0),
            argb(0x7F, 0, 255, 0)
        );
    }

    #[test]
    fn transparent_pixels_untouched() {
        // Invisible data (alpha 0) must survive bit-identical, whatever the
        // parameters, even saturation 0.
        let invisible = argb(0, 200, 100, 50);
        assert_eq!(shift_pixel(invisible, 120.0, 1.0), invisible);
        assert_eq!(shift_pixel(invisible, -45.0, 0.0), invisible);
    }

    #[test]
    fn saturation_zero_yields_grayscale() {
        // Red at 0% saturation keeps its value as gray (white here).
        assert_eq!(
            shift_pixel(argb(255, 255, 0, 0), 0.0, 0.0),
            argb(255, 255, 255, 255)
        );
        // Any other color collapses to r == g == b.
        let gray = shift_pixel(argb(255, 30, 128, 220), 33.0, 0.0);
        let (r, g, b) = (
            ((gray >> 16) & 0xFF) as u8,
            ((gray >> 8) & 0xFF) as u8,
            (gray & 0xFF) as u8,
        );
        assert_eq!((r, g, b), (r, r, r), "expected gray, got ({r}, {g}, {b})");
    }

    #[test]
    fn hue_wraps_around() {
        assert!((wrap_hue(350.0 + 20.0) - 10.0).abs() < 1e-4);
        assert!((wrap_hue(-120.0) - 240.0).abs() < 1e-4);
        assert!((wrap_hue(360.0) - 0.0).abs() < 1e-4);
        assert!((wrap_hue(720.0 + 45.0) - 45.0).abs() < 1e-4);
    }

    /// Builds an all-`None` host table: every host callback is unavailable.
    /// Leaked for `'static`; exercises the no-host-callback paths.
    fn dead_host_table() -> *mut VoidspriteSDK {
        // SAFETY: every field is Option<fn>, whose None is the null niche,
        // so zeroed memory is a valid all-None table.
        let table: Box<VoidspriteSDK> = Box::new(unsafe { core::mem::zeroed() });
        Box::leak(table)
    }

    #[test]
    fn host_failure_paths_leave_nothing_to_do() {
        // Install the dead table. Only this test touches HOST_SDK, and the
        // expect below verifies the install, so ignoring set's Result is
        // safe (a second set would just mean a table is already there).
        let _ = HOST_SDK.set(HostSdk(dead_host_table()));
        let sdk = host_sdk().expect("dead host table must be installed");
        assert!(!sdk.is_null());
        // Null arguments are rejected before any host contact.
        assert_eq!(
            apply_hue_shift(ptr::null_mut(), ptr::null_mut()),
            Err(ApplyError::NullArgument)
        );
        // A layer the host knows nothing about (dead table: layerGetInfo is
        // None) reports BadLayer. The 0x8 pointers are never dereferenced:
        // the None callback short-circuits first.
        let fake_layer = 0x8 as *mut VSPLayer;
        let fake_filter = 0x8 as *mut VSPFilter;
        assert_eq!(
            apply_hue_shift(fake_layer, fake_filter),
            Err(ApplyError::BadLayer)
        );
    }
}
