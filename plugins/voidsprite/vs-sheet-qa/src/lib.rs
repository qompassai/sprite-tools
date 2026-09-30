#![warn(missing_docs)]

//! `vs-sheet-qa` — sprite-sheet QA actions for VoidSprite (SDK v1).
//!
//! A native (`cdylib`) VoidSprite plugin, v1.0.0, display name
//! "sheet QA tools". It registers two editor actions (editor actions get
//! no parameter dialog from the host, so both actions read their
//! parameters from `~/.config/voidsprite/sheetqa.cfg`):
//!
//! * **Sheet QA: extract cell** — crops one sheet cell to a standalone
//!   PNG for close inspection.
//! * **Sheet QA: diff cells** — renders a diff PNG of two cells where
//!   differing pixels are opaque magenta and identical pixels are
//!   dimmed, and reports the differing-pixel count.
//!
//! The sheet under inspection is the session's *active frame*, flattened
//! with the SDK's `editorFlattenImage`. Geometry defaults match the
//! light-show contract: 96x192 cells, 4 columns x 6 rows.
//!
//! # Safety contract (FFI)
//!
//! * `unsafe` appears only in thin wrappers around SDK-provided
//!   pointers; each wrapper documents its obligations at the use site.
//! * No Rust panic may cross the FFI boundary: every `extern "C"` entry
//!   point funnels fallible work through `Result` and reports failures
//!   through the host notification API (stdout as a fallback). There is
//!   no `unwrap`/`expect` on any runtime path.

use core::ffi::{c_char, c_int, c_void};
use core::mem::{offset_of, size_of};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/* ------------------------------------------------------------------ */
/* SDK v1 ABI transcription. Field order and types match                */
/* voidsprite_sdk_c.h exactly; layout is asserted below.               */
/* ------------------------------------------------------------------ */

/// Opaque host handle: a single RGBA/indexed image layer.
pub enum VspLayer {}
/// Opaque host handle: a registered filter instance.
pub enum VspFilter {}
/// Opaque host handle: a registered file exporter.
pub enum VspFileExporter {}
/// Opaque host handle: the current editor session.
pub enum VspEditorContext {}
/// Opaque host handle: a registered brush.
pub enum VspBrush {}
/// Opaque C `FILE*`.
pub enum CFile {}

/// Mirrors `struct VSPLayerInfo` (12 bytes: three `int32_t`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VspLayerInfo {
    /// Layer kind: `1` = RGBA (`VSP_LAYER_RGBA`), `2` = indexed.
    pub layer_type: i32,
    /// Layer width in pixels.
    pub width: i32,
    /// Layer height in pixels.
    pub height: i32,
}

/// Mirrors `struct voidspriteSDK`: the host's function table.
///
/// Function pointers the host always fills are plain
/// `extern "C" fn`; the few this plugin actually *calls* are `Option`
/// so a missing entry degrades to a reported error instead of a call
/// through NULL.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VoidSpriteSdk {
    /// UTF-8 fopen equivalent.
    pub util_fopen_utf8: extern "C" fn(*mut c_char, *const c_char) -> *mut CFile,
    /// Registers a filter with its callback.
    pub register_filter: extern "C" fn(
        *const c_char,
        extern "C" fn(*mut VspLayer, *mut VspFilter),
    ) -> *mut VspFilter,
    /// Registers a single-layer file importer.
    pub register_layer_importer: extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        *mut VspFileExporter,
        extern "C" fn(*mut c_char) -> *mut VspLayer,
        extern "C" fn(*mut c_char) -> bool,
    ),
    /// Registers a single-layer file exporter.
    pub register_layer_exporter: extern "C" fn(
        *const c_char,
        *const c_char,
        c_int,
        extern "C" fn(*mut VspLayer, *mut c_char) -> bool,
        extern "C" fn(*mut VspLayer) -> bool,
    ) -> *mut VspFileExporter,
    /// Allocates a new layer; NULL on failure.
    pub layer_alloc_new: extern "C" fn(c_int, c_int, c_int) -> *mut VspLayer,
    /// Frees a layer; no-op on NULL.
    pub layer_free: Option<extern "C" fn(*mut VspLayer)>,
    /// Layer info; caller frees with util_free.
    pub layer_get_info: Option<extern "C" fn(*mut VspLayer) -> *mut VspLayerInfo>,
    /// Sets one pixel (0xAARRGGBB for RGBA).
    pub layer_set_pixel: extern "C" fn(*mut VspLayer, c_int, c_int, u32),
    /// Gets one pixel (0xAARRGGBB for RGBA).
    pub layer_get_pixel: extern "C" fn(*mut VspLayer, c_int, c_int) -> u32,
    /// Raw pixel pointer; do not free.
    pub layer_get_raw_pixel_data: Option<extern "C" fn(*mut VspLayer) -> *mut u32>,
    /// Adds a bool filter parameter.
    pub filter_new_bool_parameter: extern "C" fn(*mut VspFilter, *const c_char, bool),
    /// Adds an int filter parameter.
    pub filter_new_int_parameter: extern "C" fn(*mut VspFilter, *const c_char, c_int, c_int, c_int),
    /// Adds a double filter parameter.
    pub filter_new_double_parameter: extern "C" fn(*mut VspFilter, *const c_char, f64, f64, f64),
    /// Adds a double-range filter parameter.
    pub filter_new_double_range_parameter:
        extern "C" fn(*mut VspFilter, *const c_char, f64, f64, f64, f64, u32),
    /// Reads a double filter parameter.
    pub filter_get_double_value: extern "C" fn(*mut VspFilter, *const c_char) -> f64,
    /// Reads an int filter parameter.
    pub filter_get_int_value: extern "C" fn(*mut VspFilter, *const c_char) -> c_int,
    /// Reads a range parameter low value.
    pub filter_get_range_value1: extern "C" fn(*mut VspFilter, *const c_char) -> f64,
    /// Reads a range parameter high value.
    pub filter_get_range_value2: extern "C" fn(*mut VspFilter, *const c_char) -> f64,
    /// Reads a bool filter parameter.
    pub filter_get_bool_value: extern "C" fn(*mut VspFilter, *const c_char) -> bool,
    /// Frees host-allocated memory.
    pub util_free: Option<extern "C" fn(*mut c_void)>,
    /// Current active color.
    pub editor_get_active_color: extern "C" fn(*mut VspEditorContext) -> u32,
    /// Layer count in the session.
    pub editor_get_num_layers: extern "C" fn(*mut VspEditorContext) -> c_int,
    /// Layer by index.
    pub editor_get_layer: extern "C" fn(*mut VspEditorContext, c_int) -> *mut VspLayer,
    /// Active layer.
    pub editor_get_active_layer: extern "C" fn(*mut VspEditorContext) -> *mut VspLayer,
    /// Registers a brush with callbacks.
    pub register_brush: extern "C" fn(
        *const c_char,
        *const c_char,
        bool,
        extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int),
        extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int, c_int, c_int),
        extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int),
    ) -> *mut VspBrush,
    /// Sets a pixel in the editor.
    pub editor_set_pixel: extern "C" fn(*mut VspEditorContext, c_int, c_int, u32),
    /// Posts a notification (thread-safe).
    pub vsp_post_notification: Option<extern "C" fn(*const c_char, *const c_char, u32, c_int)>,
    /// Posts a success notification.
    pub vsp_post_success_notification: Option<extern "C" fn(*const c_char, *const c_char)>,
    /// Posts an error notification.
    pub vsp_post_error_notification: Option<extern "C" fn(*const c_char, *const c_char)>,
    /// Pushes layer state onto the undo stack.
    pub editor_undo_push_layer_state: extern "C" fn(*mut VspEditorContext, *mut VspLayer),
    /// Registers a nav-bar editor action.
    pub register_editor_action:
        Option<extern "C" fn(*const c_char, extern "C" fn(*mut VspEditorContext))>,
    /// Flattens the active frame; free with layerFree.
    pub editor_flatten_image: Option<extern "C" fn(*mut VspEditorContext) -> *mut VspLayer>,
    /// Flattens frame N; free with layerFree.
    pub editor_flatten_frame: extern "C" fn(*mut VspEditorContext, c_int) -> *mut VspLayer,
    /// Frame count in the session.
    pub editor_get_num_frames: extern "C" fn(*mut VspEditorContext) -> c_int,
    /// Index of the active frame.
    pub editor_get_active_frame_index: extern "C" fn(*mut VspEditorContext) -> c_int,
    /// Localized string lookup; do not free.
    pub vsp_get_localized_string: extern "C" fn(*const c_char) -> *const c_char,
}

/// Layout assertions against the C header, measured on the build host
/// with `offsetof`/`sizeof` (see README). A mismatch here means the
/// transcription drifted from `voidsprite_sdk_c.h` and must be fixed
/// before the plugin ever loads.
const _: () = {
    assert!(size_of::<VspLayerInfo>() == 12);
    assert!(offset_of!(VspLayerInfo, layer_type) == 0);
    assert!(offset_of!(VspLayerInfo, width) == 4);
    assert!(offset_of!(VspLayerInfo, height) == 8);
    assert!(size_of::<VoidSpriteSdk>() == 288);
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
};

/* ------------------------------------------------------------------ */
/* Bounds: every loop, buffer, and allocation in this file is capped.  */
/* ------------------------------------------------------------------ */

/// Largest single image dimension we touch (px).
const MAX_DIM_PX: i64 = 8192;
/// Most cells in the sheet grid (`cols * rows`).
const MAX_CELLS: i64 = 256;
/// Longest config file line we parse (bytes).
const MAX_CONFIG_LINE: usize = 256;
/// Most config lines we read.
const MAX_CONFIG_LINES: usize = 64;
/// Largest config file we read (bytes); the real file is < 1 KiB.
const MAX_CONFIG_FILE_BYTES: usize = 65_536;
/// Longest notification message we build (chars).
const MAX_MSG_CHARS: usize = 220;
/// Light-show contract defaults: cell size, grid.
const DEF_CELL_W: i64 = 96;
const DEF_CELL_H: i64 = 192;
const DEF_COLS: i64 = 4;
const DEF_ROWS: i64 = 6;
/// Layer kind constant from the SDK header (`VSP_LAYER_RGBA`).
const LAYER_RGBA: i32 = 1;
/// Diff highlight: opaque magenta in the SDK's 0xAARRGGBB pixel format.
pub const DIFF_HIGHLIGHT: u32 = 0xFF_FF00FF;

/* ------------------------------------------------------------------ */
/* Host table capture + thin safe wrappers.                            */
/* ------------------------------------------------------------------ */

/// The host's SDK table, captured once in `pluginInit`. All entries
/// are plain function pointers (`Copy`), so the table itself is
/// immutable after capture.
static SDK_TABLE: OnceLock<VoidSpriteSdk> = OnceLock::new();

/// The captured SDK table, or `None` if `pluginInit` never ran (or was
/// passed NULL). Every action checks this first.
fn sdk() -> Option<&'static VoidSpriteSdk> {
    SDK_TABLE.get()
}

/// RAII owner of a host-allocated layer: `layerFree` on drop. The
/// host's `layerFree` is documented as a no-op on NULL.
struct SdkLayer<'s> {
    sdk: &'s VoidSpriteSdk,
    layer: *mut VspLayer,
}

impl Drop for SdkLayer<'_> {
    fn drop(&mut self) {
        if let Some(free_layer) = self.sdk.layer_free {
            free_layer(self.layer);
        }
    }
}

/// Copy the SDK's raw pixel buffer into owned memory.
///
/// # Safety obligations (verified at the call sites)
/// * `pixels` points to `len` readable, 4-byte-aligned `u32`s.
/// * The buffer stays valid and unmodified for the duration of the
///   copy; the owning `SdkLayer` guard outlives this call and the
///   action runs synchronously on the host's action thread.
unsafe fn copy_raw_pixels(pixels: *mut u32, len: usize, _owner: &SdkLayer<'_>) -> Vec<u32> {
    // SAFETY: upheld by the caller per the obligations above.
    let view = unsafe { core::slice::from_raw_parts(pixels as *const u32, len) };
    view.to_vec()
}

/// Read a layer's dimensions via `layerGetInfo`, freeing the host's
/// info struct with `util_free` afterwards.
fn layer_dims(sdk: &VoidSpriteSdk, layer: *mut VspLayer) -> Result<(i32, i32, i32), ActionError> {
    let get_info = sdk
        .layer_get_info
        .ok_or(ActionError::MissingApi("layerGetInfo"))?;
    let free_mem = sdk.util_free.ok_or(ActionError::MissingApi("util_free"))?;
    let info = get_info(layer);
    if info.is_null() {
        return Err(ActionError::Host("layerGetInfo returned NULL".into()));
    }
    // SAFETY: the host returned a valid `VSPLayerInfo`; we copy the
    // three `i32` fields out before freeing it, and never touch the
    // pointer again afterwards.
    let (layer_type, width, height) =
        unsafe { ((*info).layer_type, (*info).width, (*info).height) };
    free_mem(info as *mut c_void);
    Ok((layer_type, width, height))
}

/// Post an error notification; stdout is the fallback when the host
/// entry is unavailable (per the task brief).
fn notify_error(message: &str) {
    let message = truncate_msg(message);
    match sdk().and_then(|s| s.vsp_post_error_notification) {
        Some(notify) => {
            if let Ok(text) = CString::new(message.as_str()) {
                notify(c"Sheet QA".as_ptr(), text.as_ptr());
            }
        }
        None => eprintln!("[sheet-qa] ERROR: {message}"),
    }
}

/// Post a success notification; stdout is the fallback.
fn notify_success(message: &str) {
    let message = truncate_msg(message);
    match sdk().and_then(|s| s.vsp_post_success_notification) {
        Some(notify) => {
            if let Ok(text) = CString::new(message.as_str()) {
                notify(c"Sheet QA".as_ptr(), text.as_ptr());
            }
        }
        None => println!("[sheet-qa] {message}"),
    }
}

/// Keep notification text bounded: the host renders a small popup,
/// and unbounded user-influenced text has no business in it.
fn truncate_msg(message: &str) -> String {
    if message.chars().count() <= MAX_MSG_CHARS {
        return message.to_string();
    }
    let head: String = message.chars().take(MAX_MSG_CHARS - 1).collect();
    format!("{head}…")
}

/* ------------------------------------------------------------------ */
/* Pure pixel math. No SDK contact: unit-testable.                     */
/* ------------------------------------------------------------------ */

/// Row-major cell origin in pixels: `x = frame * cell_w`,
/// `y = row * cell_h`.
///
/// Contract: `0 <= frame`, `0 <= row`, `cell_w > 0`, `cell_h > 0`; the
/// action layer range-checks `frame < cols` and `row < rows` before
/// calling. Products stay far below `i64` range by the config bounds.
pub fn cell_origin(frame: i64, row: i64, cell_w: i64, cell_h: i64) -> (i64, i64) {
    debug_assert!(frame >= 0 && row >= 0 && cell_w > 0 && cell_h > 0);
    (frame * cell_w, row * cell_h)
}

/// Crop the cell rectangle out of a row-major sheet buffer.
///
/// Returns `None` when the rectangle is out of bounds. `sheet` holds
/// `sheet_w * sheet_h` pixels in the SDK's 0xAARRGGBB format.
pub fn crop_cell(
    sheet: &[u32],
    sheet_w: i64,
    origin_x: i64,
    origin_y: i64,
    cell_w: i64,
    cell_h: i64,
) -> Option<Vec<u32>> {
    if sheet_w <= 0 || cell_w <= 0 || cell_h <= 0 || origin_x < 0 || origin_y < 0 {
        return None;
    }
    let end_x = origin_x.checked_add(cell_w)?;
    let end_y = origin_y.checked_add(cell_h)?;
    let sheet_h = sheet.len() as i64 / sheet_w;
    if sheet.len() as i64 % sheet_w != 0 || end_x > sheet_w || end_y > sheet_h {
        return None;
    }
    let mut out = Vec::with_capacity((cell_w * cell_h) as usize);
    for row in 0..cell_h {
        let src = ((origin_y + row) * sheet_w + origin_x) as usize;
        out.extend_from_slice(&sheet[src..src + cell_w as usize]);
    }
    Some(out)
}

/// Dim one 0xAARRGGBB pixel: halve R, G, B, keep alpha. Used for the
/// identical (non-differing) pixels in a diff image.
pub fn dim_pixel(pixel: u32) -> u32 {
    let alpha = pixel & 0xFF00_0000;
    let red = ((pixel >> 16) & 0xFF) / 2;
    let green = ((pixel >> 8) & 0xFF) / 2;
    let blue = (pixel & 0xFF) / 2;
    alpha | (red << 16) | (green << 8) | blue
}

/// Diff two same-size cell buffers.
///
/// Returns `None` on size mismatch. The output pixel is
/// [`DIFF_HIGHLIGHT`] where the inputs differ, otherwise the dimmed
/// pixel from `a`. The `u64` is the differing-pixel count.
pub fn diff_cells(a: &[u32], b: &[u32]) -> Option<(Vec<u32>, u64)> {
    if a.len() != b.len() {
        return None;
    }
    let mut out = Vec::with_capacity(a.len());
    let mut differing: u64 = 0;
    for (&pa, &pb) in a.iter().zip(b.iter()) {
        if pa == pb {
            out.push(dim_pixel(pa));
        } else {
            out.push(DIFF_HIGHLIGHT);
            differing += 1;
        }
    }
    Some((out, differing))
}

/* ------------------------------------------------------------------ */
/* Minimal PNG encoder (zero dependencies). Stored (uncompressed)      */
/* DEFLATE blocks inside a zlib stream: valid PNG, no compression.     */
/* ------------------------------------------------------------------ */

/// CRC-32 (ISO 3309), bitwise implementation. Validated against the
/// standard check value in the unit tests.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Adler-32 for the zlib wrapper. Validated against a known vector in
/// the unit tests.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut lo: u32 = 1;
    let mut hi: u32 = 0;
    for &byte in data {
        lo = (lo + u32::from(byte)) % MOD;
        hi = (hi + lo) % MOD;
    }
    (hi << 16) | lo
}

/// Append one PNG chunk (`length | type | data | crc`).
fn png_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(tag);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode RGBA pixels as a PNG.
///
/// `pixels` are 0xAARRGGBB, row-major, `width * height` of them.
/// Returns `None` when the pixel count does not match or the
/// dimensions exceed [`MAX_DIM_PX`].
pub fn encode_png_rgba(width: u32, height: u32, pixels: &[u32]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width as i64 > MAX_DIM_PX || height as i64 > MAX_DIM_PX {
        return None;
    }
    let count = (width as usize).checked_mul(height as usize)?;
    if pixels.len() != count {
        return None;
    }
    let row_bytes = (width as usize).checked_mul(4)?;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // color type: truecolor + alpha
    png_chunk(&mut out, b"IHDR", &ihdr);

    // Raw scanlines: one filter byte (0 = none) + RGBA bytes per row.
    let mut raw: Vec<u8> = Vec::with_capacity(height as usize * (1 + row_bytes));
    for row in 0..height as usize {
        raw.push(0);
        for &pixel in &pixels[row * width as usize..(row + 1) * width as usize] {
            raw.push((pixel >> 16) as u8); // R
            raw.push((pixel >> 8) as u8); // G
            raw.push(pixel as u8); // B
            raw.push((pixel >> 24) as u8); // A
        }
    }

    // zlib stream: header + stored DEFLATE blocks (<= 65535 bytes each).
    let mut zlib: Vec<u8> = Vec::new();
    zlib.extend_from_slice(&[0x78, 0x01]);
    let mut rest = raw.as_slice();
    while !rest.is_empty() {
        let take = rest.len().min(65_535);
        let (block, tail) = rest.split_at(take);
        rest = tail;
        zlib.push(u8::from(rest.is_empty())); // BFINAL
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    png_chunk(&mut out, b"IDAT", &zlib);
    png_chunk(&mut out, b"IEND", &[]);

    Some(out)
}

/* ------------------------------------------------------------------ */
/* Config file: ~/.config/voidsprite/sheetqa.cfg. Strict parse:        */
/* unknown keys and malformed values abort loudly. A missing file is   */
/* not an error — geometry defaults stand, but the cell selectors     */
/* (row/frame, …) have no defaults and the action aborts without them. */
/* ------------------------------------------------------------------ */

/// Parsed `sheetqa.cfg`. Geometry fields always hold a value
/// (defaults or file); selectors are `None` until set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetQaConfig {
    /// Cell width in pixels (default 96).
    pub cell_w: i64,
    /// Cell height in pixels (default 192).
    pub cell_h: i64,
    /// Sheet columns (default 4).
    pub cols: i64,
    /// Sheet rows (default 6).
    pub rows: i64,
    /// Extract action: 0-based row (no default).
    pub row: Option<i64>,
    /// Extract action: 0-based frame/column (no default).
    pub frame: Option<i64>,
    /// Diff action: cell A row (no default).
    pub row_a: Option<i64>,
    /// Diff action: cell A frame (no default).
    pub frame_a: Option<i64>,
    /// Diff action: cell B row (no default).
    pub row_b: Option<i64>,
    /// Diff action: cell B frame (no default).
    pub frame_b: Option<i64>,
    /// Output directory override (default
    /// `~/.config/voidsprite/sheetqa-output`).
    pub output_dir: Option<String>,
}

/// What went wrong while reading the config file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The file could not be read (other than "missing", which is OK).
    Io(String),
    /// The file is not valid UTF-8.
    NotUtf8,
    /// The file exceeds [`MAX_CONFIG_FILE_BYTES`].
    TooLarge,
    /// A line is malformed; carries the 1-based line number.
    BadLine(usize, String),
    /// A value is malformed or out of range; carries key + line.
    BadValue(String, usize, String),
    /// An unknown key; carries key + line.
    UnknownKey(String, usize),
}

impl core::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(why) => write!(f, "cannot read config: {why}"),
            Self::NotUtf8 => write!(f, "config is not valid UTF-8"),
            Self::TooLarge => write!(f, "config exceeds {MAX_CONFIG_FILE_BYTES} bytes"),
            Self::BadLine(line, text) => write!(f, "config line {line}: malformed ({text})"),
            Self::BadValue(key, line, val) => {
                write!(f, "config line {line}: bad value for '{key}' ({val})")
            }
            Self::UnknownKey(key, line) => {
                write!(f, "config line {line}: unknown key '{key}'")
            }
        }
    }
}

impl SheetQaConfig {
    /// Geometry defaults = the light-show contract; no selectors set.
    pub fn defaults() -> Self {
        Self {
            cell_w: DEF_CELL_W,
            cell_h: DEF_CELL_H,
            cols: DEF_COLS,
            rows: DEF_ROWS,
            row: None,
            frame: None,
            row_a: None,
            frame_a: None,
            row_b: None,
            frame_b: None,
            output_dir: None,
        }
    }
}

/// Parse one integer config value with an inclusive range.
fn parse_int_value(
    key: &str,
    val: &str,
    line_no: usize,
    min: i64,
    max: i64,
) -> Result<i64, ConfigError> {
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::BadValue(
            key.into(),
            line_no,
            "empty value".into(),
        ));
    }
    let parsed: i64 = trimmed.parse().map_err(|_| {
        ConfigError::BadValue(key.into(), line_no, format!("not an integer: '{trimmed}'"))
    })?;
    if parsed < min || parsed > max {
        return Err(ConfigError::BadValue(
            key.into(),
            line_no,
            format!("{parsed} out of range [{min}, {max}]"),
        ));
    }
    Ok(parsed)
}

/// Apply one parsed `key = value` line. Unknown keys are an error: a
/// typo'd key must never silently keep a default.
fn apply_config_line(
    cfg: &mut SheetQaConfig,
    key: &str,
    val: &str,
    line_no: usize,
) -> Result<(), ConfigError> {
    match key {
        "cell_w" => cfg.cell_w = parse_int_value(key, val, line_no, 1, 2048)?,
        "cell_h" => cfg.cell_h = parse_int_value(key, val, line_no, 1, 2048)?,
        "cols" => cfg.cols = parse_int_value(key, val, line_no, 1, 64)?,
        "rows" => cfg.rows = parse_int_value(key, val, line_no, 1, 64)?,
        "row" => cfg.row = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "frame" => cfg.frame = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "row_a" => cfg.row_a = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "frame_a" => cfg.frame_a = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "row_b" => cfg.row_b = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "frame_b" => cfg.frame_b = Some(parse_int_value(key, val, line_no, 0, MAX_DIM_PX)?),
        "output_dir" => {
            let trimmed = val.trim();
            if trimmed.is_empty() || trimmed.len() > 1024 {
                return Err(ConfigError::BadValue(
                    key.into(),
                    line_no,
                    "empty or too long".into(),
                ));
            }
            cfg.output_dir = Some(trimmed.to_string());
        }
        _ => return Err(ConfigError::UnknownKey(key.into(), line_no)),
    }
    Ok(())
}

/// Parse config text into `cfg` (staged: callers start from
/// [`SheetQaConfig::defaults`]). Cross-field check: `cols * rows`
/// must not exceed [`MAX_CELLS`].
pub fn parse_config_text(cfg: &mut SheetQaConfig, text: &str) -> Result<(), ConfigError> {
    let mut staged = cfg.clone();
    let mut line_count = 0usize;
    for (idx, raw_line) in text.lines().enumerate() {
        let line_no = idx + 1;
        line_count += 1;
        if line_count > MAX_CONFIG_LINES {
            return Err(ConfigError::BadLine(line_no, "too many lines".into()));
        }
        if raw_line.len() > MAX_CONFIG_LINE {
            return Err(ConfigError::BadLine(line_no, "line too long".into()));
        }
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, val) = line
            .split_once('=')
            .ok_or_else(|| ConfigError::BadLine(line_no, format!("missing '=': '{line}'")))?;
        let key = key.trim();
        if key.is_empty() || key.len() > 64 {
            return Err(ConfigError::BadLine(line_no, "bad key".into()));
        }
        apply_config_line(&mut staged, key, val, line_no)?;
    }
    if staged.cols * staged.rows > MAX_CELLS {
        return Err(ConfigError::BadValue(
            "cols*rows".into(),
            line_count,
            format!("{} cells exceeds {MAX_CELLS}", staged.cols * staged.rows),
        ));
    }
    *cfg = staged;
    Ok(())
}

/// Load the config file. A *missing* file is not an error: defaults
/// stand. Any other I/O or parse error aborts loudly.
pub fn load_config_file(path: &Path) -> Result<SheetQaConfig, ConfigError> {
    let mut cfg = SheetQaConfig::defaults();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(cfg),
        Err(err) => return Err(ConfigError::Io(err.to_string())),
    };
    if bytes.len() > MAX_CONFIG_FILE_BYTES {
        return Err(ConfigError::TooLarge);
    }
    let text = core::str::from_utf8(&bytes).map_err(|_| ConfigError::NotUtf8)?;
    parse_config_text(&mut cfg, text)?;
    Ok(cfg)
}

/* ------------------------------------------------------------------ */
/* Action machinery: flatten active frame -> validate -> compute ->    */
/* write PNG -> notify. Every failure becomes an ActionError with a    */
/* message for the host notification; nothing panics.                  */
/* ------------------------------------------------------------------ */

/// What went wrong while running an action.
#[derive(Clone, Debug)]
pub enum ActionError {
    /// `pluginInit` never captured the SDK table.
    NoSdk,
    /// A needed host entry is NULL.
    MissingApi(&'static str),
    /// Host call returned NULL / bad data.
    Host(String),
    /// Config problem; the message names the cause.
    Config(ConfigError),
    /// A required selector (`row=`, …) is missing.
    MissingSelector(&'static str),
    /// A selector is outside the sheet grid.
    SelectorOutOfRange(&'static str, i64, i64),
    /// The flattened frame is not RGBA or is smaller than the sheet.
    BadSheet(String),
    /// Output file could not be written.
    Output(String),
}

impl core::fmt::Display for ActionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoSdk => write!(f, "plugin not initialised (pluginInit never ran)"),
            Self::MissingApi(api) => write!(f, "host API unavailable: {api}"),
            Self::Host(why) => write!(f, "host error: {why}"),
            Self::Config(err) => write!(f, "sheetqa.cfg: {err}"),
            Self::MissingSelector(key) => write!(
                f,
                "'{key}=' is required in sheetqa.cfg (no default); set it and retry"
            ),
            Self::SelectorOutOfRange(key, val, limit) => {
                write!(f, "'{key}'={val} is outside the sheet grid (limit {limit})")
            }
            Self::BadSheet(why) => write!(f, "active frame is not a usable sheet: {why}"),
            Self::Output(why) => write!(f, "cannot write output: {why}"),
        }
    }
}

/// `~/.config/voidsprite/sheetqa.cfg`.
fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let mut path = PathBuf::from(home);
    path.push(".config/voidsprite/sheetqa.cfg");
    Some(path)
}

/// Default output dir: `~/.config/voidsprite/sheetqa-output`.
fn default_output_dir() -> Result<PathBuf, ActionError> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        ActionError::Output("HOME is not set; set output_dir= in sheetqa.cfg".into())
    })?;
    let mut dir = PathBuf::from(home);
    dir.push(".config/voidsprite/sheetqa-output");
    Ok(dir)
}

/// Require a selector, then range-check it against the grid limit.
fn require_selector(
    cfg_value: Option<i64>,
    key: &'static str,
    limit: i64,
) -> Result<i64, ActionError> {
    let val = cfg_value.ok_or(ActionError::MissingSelector(key))?;
    if val < 0 || val >= limit {
        return Err(ActionError::SelectorOutOfRange(key, val, limit - 1));
    }
    Ok(val)
}

/// Flatten the active frame and copy its pixels into owned memory,
/// validating it is an RGBA image large enough for the sheet grid.
fn read_sheet_pixels(
    sdk: &VoidSpriteSdk,
    editor: *mut VspEditorContext,
    cfg: &SheetQaConfig,
) -> Result<(Vec<u32>, i64, i64), ActionError> {
    let flatten = sdk
        .editor_flatten_image
        .ok_or(ActionError::MissingApi("editorFlattenImage"))?;
    let raw_layer = flatten(editor);
    if raw_layer.is_null() {
        return Err(ActionError::Host("editorFlattenImage returned NULL".into()));
    }
    let guard = SdkLayer {
        sdk,
        layer: raw_layer,
    };

    let (layer_type, width, height) = layer_dims(sdk, raw_layer)?;
    if layer_type != LAYER_RGBA {
        return Err(ActionError::BadSheet(format!(
            "layer type {layer_type} is not RGBA"
        )));
    }
    let need_w = cfg.cols * cfg.cell_w;
    let need_h = cfg.rows * cfg.cell_h;
    if i64::from(width) < need_w || i64::from(height) < need_h {
        return Err(ActionError::BadSheet(format!(
            "frame is {}x{} but the sheet needs {}x{}",
            width, height, need_w, need_h
        )));
    }

    let raw = sdk
        .layer_get_raw_pixel_data
        .ok_or(ActionError::MissingApi("layerGetRawPixelData"))?;
    let pixels_ptr = raw(raw_layer);
    if pixels_ptr.is_null() {
        return Err(ActionError::Host(
            "layerGetRawPixelData returned NULL".into(),
        ));
    }
    let len = (i64::from(width) as usize)
        .checked_mul(i64::from(height) as usize)
        .ok_or_else(|| ActionError::Host("frame dimensions overflow".into()))?;
    // SAFETY: `pixels_ptr` came from this layer's raw-data call; the
    // `SdkLayer` guard keeps the layer alive for the copy, which runs
    // synchronously on the host's action thread.
    let pixels = unsafe { copy_raw_pixels(pixels_ptr, len, &guard) };
    Ok((pixels, i64::from(width), i64::from(height)))
}

/// Write `png` bytes to `dir/file_name` atomically (temp file +
/// rename), creating the directory first.
fn write_png_atomic(dir: &Path, file_name: &str, png: &[u8]) -> Result<PathBuf, ActionError> {
    std::fs::create_dir_all(dir)
        .map_err(|err| ActionError::Output(format!("cannot create {dir:?}: {err}")))?;
    let dest = dir.join(file_name);
    let tmp = dir.join(format!("{file_name}.tmp"));
    std::fs::write(&tmp, png)
        .map_err(|err| ActionError::Output(format!("cannot write {tmp:?}: {err}")))?;
    std::fs::rename(&tmp, &dest)
        .map_err(|err| ActionError::Output(format!("cannot rename {tmp:?}: {err}")))?;
    Ok(dest)
}

/// Shared preamble: SDK, config, output dir.
fn action_preamble() -> Result<(SheetQaConfig, PathBuf), ActionError> {
    let sdk = sdk().ok_or(ActionError::NoSdk)?;
    let _ = sdk; // the table is re-fetched by the action body
    let cfg = match config_path() {
        Some(path) => load_config_file(&path).map_err(ActionError::Config)?,
        None => SheetQaConfig::defaults(),
    };
    let out_dir = match cfg.output_dir.as_deref() {
        Some(dir) => PathBuf::from(dir),
        None => default_output_dir()?,
    };
    Ok((cfg, out_dir))
}

/// "Sheet QA: extract cell" — crop one cell to a standalone PNG.
fn run_extract_cell(editor: *mut VspEditorContext) -> Result<String, ActionError> {
    let (cfg, out_dir) = action_preamble()?;
    let sdk = sdk().ok_or(ActionError::NoSdk)?;
    let row = require_selector(cfg.row, "row", cfg.rows)?;
    let frame = require_selector(cfg.frame, "frame", cfg.cols)?;

    let (sheet, sheet_w, _sheet_h) = read_sheet_pixels(sdk, editor, &cfg)?;
    let (origin_x, origin_y) = cell_origin(frame, row, cfg.cell_w, cfg.cell_h);
    let cell = crop_cell(&sheet, sheet_w, origin_x, origin_y, cfg.cell_w, cfg.cell_h)
        .ok_or_else(|| ActionError::BadSheet("cell rectangle is out of bounds".into()))?;
    let png = encode_png_rgba(cfg.cell_w as u32, cfg.cell_h as u32, &cell)
        .ok_or_else(|| ActionError::Output("PNG encoding failed".into()))?;
    let file_name = format!("cell_r{row}_f{frame}.png");
    let dest = write_png_atomic(&out_dir, &file_name, &png)?;
    Ok(format!(
        "Cell row {row}, frame {frame} -> {}",
        dest.display()
    ))
}

/// "Sheet QA: diff cells" — diff PNG of two cells + differing count.
fn run_diff_cells(editor: *mut VspEditorContext) -> Result<String, ActionError> {
    let (cfg, out_dir) = action_preamble()?;
    let sdk = sdk().ok_or(ActionError::NoSdk)?;
    let row_a = require_selector(cfg.row_a, "row_a", cfg.rows)?;
    let frame_a = require_selector(cfg.frame_a, "frame_a", cfg.cols)?;
    let row_b = require_selector(cfg.row_b, "row_b", cfg.rows)?;
    let frame_b = require_selector(cfg.frame_b, "frame_b", cfg.cols)?;

    let (sheet, sheet_w, _sheet_h) = read_sheet_pixels(sdk, editor, &cfg)?;
    let crop_one = |row: i64, frame: i64| -> Result<Vec<u32>, ActionError> {
        let (ox, oy) = cell_origin(frame, row, cfg.cell_w, cfg.cell_h);
        crop_cell(&sheet, sheet_w, ox, oy, cfg.cell_w, cfg.cell_h)
            .ok_or_else(|| ActionError::BadSheet("cell rectangle is out of bounds".into()))
    };
    let cell_a = crop_one(row_a, frame_a)?;
    let cell_b = crop_one(row_b, frame_b)?;
    let (diff, differing) = diff_cells(&cell_a, &cell_b)
        .ok_or_else(|| ActionError::Host("cell size mismatch".into()))?;
    let png = encode_png_rgba(cfg.cell_w as u32, cfg.cell_h as u32, &diff)
        .ok_or_else(|| ActionError::Output("PNG encoding failed".into()))?;
    let file_name = format!("diff_r{row_a}_f{frame_a}_vs_r{row_b}_f{frame_b}.png");
    let dest = write_png_atomic(&out_dir, &file_name, &png)?;

    let total = cell_a.len() as u64;
    let pct = if total > 0 {
        differing as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    Ok(format!(
        "{differing} of {total} pixels differ ({pct:.1}%) -> {}",
        dest.display()
    ))
}

extern "C" fn action_extract_cell(editor: *mut VspEditorContext) {
    match run_extract_cell(editor) {
        Ok(report) => notify_success(&report),
        Err(err) => notify_error(&err.to_string()),
    }
}

extern "C" fn action_diff_cells(editor: *mut VspEditorContext) {
    match run_diff_cells(editor) {
        Ok(report) => notify_success(&report),
        Err(err) => notify_error(&err.to_string()),
    }
}

/* ------------------------------------------------------------------ */
/* Plugin entry points.                                                */
/* ------------------------------------------------------------------ */

/// Called once by the host on load. Captures the SDK table and
/// registers the two editor actions. A NULL table is ignored.
#[unsafe(no_mangle)]
pub extern "C" fn pluginInit(table: *mut VoidSpriteSdk) {
    if table.is_null() {
        return;
    }
    // SAFETY: the host passes a pointer to its fully-populated
    // `voidspriteSDK` table, valid for the plugin's lifetime. We copy
    // the 36 function pointers out once and never touch `table` again.
    let copied = unsafe { *table };
    let _ = SDK_TABLE.set(copied);
    if let Some(register) = copied.register_editor_action {
        register(c"Sheet QA: extract cell".as_ptr(), action_extract_cell);
        register(c"Sheet QA: diff cells".as_ptr(), action_diff_cells);
    }
}

/// SDK version this plugin was built against (must be 1).
#[unsafe(no_mangle)]
pub extern "C" fn voidspriteSDKVersion() -> c_int {
    1
}

/// Display name shown in the host's plugin list.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginName() -> *const c_char {
    c"sheet QA tools".as_ptr()
}

/// Plugin version.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginVersion() -> *const c_char {
    c"1.0.0".as_ptr()
}

/// One-line description.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginDescription() -> *const c_char {
    c"Sprite-sheet QA: extract one cell to a PNG, and diff two cells with a highlighted diff image."
        .as_ptr()
}

/// Authors.
#[unsafe(no_mangle)]
pub extern "C" fn getPluginAuthors() -> *const c_char {
    c"Pax (Qompass AI)".as_ptr()
}

/* ------------------------------------------------------------------ */
/* Unit tests: pure pixel math, config parser, PNG encoder.            */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn good_cfg_text() -> &'static str {
        "# sheet QA config\ncell_w=96\ncell_h=192\ncols=4\nrows=6\nrow=2\nframe=3\nrow_a=0\nframe_a=0\nrow_b=0\nframe_b=1\noutput_dir=/tmp/sheetqa-out\n"
    }

    #[test]
    fn cell_origin_basic() {
        assert_eq!(cell_origin(3, 2, 96, 192), (288, 384));
    }

    #[test]
    fn cell_origin_zero() {
        assert_eq!(cell_origin(0, 0, 96, 192), (0, 0));
    }

    #[test]
    fn cell_origin_last_cell() {
        assert_eq!(cell_origin(3, 5, 96, 192), (288, 960));
    }

    /// Synthetic sheet: 8x4 px, two 4x2 cells side by side. The second
    /// cell's pixels are the first cell's + 1000 (distinct values).
    fn synthetic_sheet() -> Vec<u32> {
        let mut sheet = Vec::with_capacity(32);
        for y in 0..4i64 {
            for x in 0..8i64 {
                let base = (y * 8 + x) as u32;
                sheet.push(if x < 4 { base } else { base + 1000 });
            }
        }
        sheet
    }

    #[test]
    fn crop_cell_extracts_right_pixels() {
        let sheet = synthetic_sheet();
        let cell = crop_cell(&sheet, 8, 4, 2, 4, 2).expect("in bounds");
        assert_eq!(cell.len(), 8);
        // Rows y=2,3 of the sheet, columns x=4..8.
        let mut want = Vec::new();
        for y in 2..4i64 {
            for x in 4..8i64 {
                want.push((y * 8 + x) as u32 + 1000);
            }
        }
        assert_eq!(cell, want);
    }

    #[test]
    fn crop_cell_rejects_out_of_bounds() {
        let sheet = synthetic_sheet();
        assert!(crop_cell(&sheet, 8, 5, 0, 4, 2).is_none()); // x overflow
        assert!(crop_cell(&sheet, 8, 0, 3, 4, 2).is_none()); // y overflow
        assert!(crop_cell(&sheet, 8, 0, 0, 0, 2).is_none()); // empty cell
    }

    #[test]
    fn dim_pixel_halves_channels_keeps_alpha() {
        assert_eq!(dim_pixel(0xFF_80_40_20), 0xFF_40_20_10);
        assert_eq!(dim_pixel(0x00_FF_FF_FF), 0x00_7F_7F_7F);
    }

    #[test]
    fn diff_identical_buffers_zero_diffs() {
        let buf = vec![0xFF_11_22_33u32; 16];
        let (out, differing) = diff_cells(&buf, &buf).expect("same size");
        assert_eq!(differing, 0);
        assert!(out.iter().all(|&p| p == dim_pixel(0xFF_11_22_33)));
    }

    #[test]
    fn diff_single_pixel_change_exactly_one() {
        let a = vec![0xFF_00_00_00u32; 16];
        let mut b = a.clone();
        b[7] = 0xFF_FF_FF_FF;
        let (out, differing) = diff_cells(&a, &b).expect("same size");
        assert_eq!(differing, 1);
        assert_eq!(out[7], DIFF_HIGHLIGHT);
        assert_eq!(out[7], 0xFF_FF00FF);
        assert!(out.iter().enumerate().all(|(i, &p)| {
            if i == 7 {
                true
            } else {
                p == dim_pixel(0xFF_00_00_00)
            }
        }));
    }

    #[test]
    fn diff_size_mismatch_is_none() {
        assert!(diff_cells(&[1u32; 4], &[1u32; 5]).is_none());
    }

    #[test]
    fn config_good_parses() {
        let mut cfg = SheetQaConfig::defaults();
        parse_config_text(&mut cfg, good_cfg_text()).expect("good config");
        assert_eq!(cfg.cell_w, 96);
        assert_eq!(cfg.cell_h, 192);
        assert_eq!(cfg.cols, 4);
        assert_eq!(cfg.rows, 6);
        assert_eq!(cfg.row, Some(2));
        assert_eq!(cfg.frame, Some(3));
        assert_eq!(cfg.row_a, Some(0));
        assert_eq!(cfg.frame_a, Some(0));
        assert_eq!(cfg.row_b, Some(0));
        assert_eq!(cfg.frame_b, Some(1));
        assert_eq!(cfg.output_dir.as_deref(), Some("/tmp/sheetqa-out"));
    }

    #[test]
    fn config_missing_selectors_stay_none() {
        let mut cfg = SheetQaConfig::defaults();
        parse_config_text(&mut cfg, "cell_w=64\n").expect("geometry only");
        assert_eq!(cfg.cell_w, 64);
        assert_eq!(cfg.row, None);
        assert_eq!(cfg.frame, None);
        assert_eq!(cfg.row_a, None);
    }

    #[test]
    fn config_unknown_key_aborts() {
        let mut cfg = SheetQaConfig::defaults();
        let err = parse_config_text(&mut cfg, "cell_wdith=96\n").expect_err("unknown key");
        assert!(matches!(err, ConfigError::UnknownKey(k, 1) if k == "cell_wdith"));
    }

    #[test]
    fn config_malformed_values_abort() {
        for bad in [
            "cell_w=abc\n",
            "cell_w=\n",
            "cell_w=12.5\n",
            "cell_w=0x60\n",
            "cell_w=1; DROP TABLE\n",
        ] {
            let mut cfg = SheetQaConfig::defaults();
            assert!(
                parse_config_text(&mut cfg, bad).is_err(),
                "should reject: {bad:?}"
            );
        }
    }

    #[test]
    fn config_out_of_range_aborts() {
        for bad in [
            "cell_w=0\n",
            "cell_w=99999\n",
            "cols=0\n",
            "rows=65\n",
            "row=-1\n",
        ] {
            let mut cfg = SheetQaConfig::defaults();
            assert!(
                parse_config_text(&mut cfg, bad).is_err(),
                "should reject: {bad:?}"
            );
        }
    }

    #[test]
    fn config_too_many_cells_aborts() {
        let mut cfg = SheetQaConfig::defaults();
        // 64x64 = 4096 > 256.
        let err = parse_config_text(&mut cfg, "cols=64\nrows=64\n").expect_err("too many");
        assert!(matches!(err, ConfigError::BadValue(_, _, _)));
    }

    #[test]
    fn config_long_line_and_many_lines_abort() {
        let mut cfg = SheetQaConfig::defaults();
        let long = format!("cell_w={}\n", "9".repeat(300));
        assert!(parse_config_text(&mut cfg, &long).is_err());
        let many = "cell_w=96\n".repeat(70);
        assert!(parse_config_text(&mut cfg, &many).is_err());
    }

    #[test]
    fn config_line_without_equals_aborts() {
        let mut cfg = SheetQaConfig::defaults();
        assert!(parse_config_text(&mut cfg, "cell_w 96\n").is_err());
    }

    #[test]
    fn config_missing_file_gives_defaults() {
        let missing = Path::new("/tmp/sheetqa-definitely-missing-12345.cfg");
        let cfg = load_config_file(missing).expect("missing file is OK");
        assert_eq!(cfg, SheetQaConfig::defaults());
    }

    #[test]
    fn config_roundtrip_through_file() {
        let path = std::env::temp_dir().join(format!("sheetqa-test-{}.cfg", std::process::id()));
        let mut file = std::fs::File::create(&path).expect("create temp cfg");
        file.write_all(good_cfg_text().as_bytes())
            .expect("write temp cfg");
        drop(file);
        let cfg = load_config_file(&path).expect("load temp cfg");
        assert_eq!(cfg.row, Some(2));
        std::fs::remove_file(&path).expect("cleanup temp cfg");
    }

    #[test]
    fn require_selector_missing_and_out_of_range() {
        assert!(matches!(
            require_selector(None, "row", 6),
            Err(ActionError::MissingSelector("row"))
        ));
        assert!(require_selector(Some(5), "row", 6).is_ok());
        assert!(matches!(
            require_selector(Some(6), "row", 6),
            Err(ActionError::SelectorOutOfRange("row", 6, 5))
        ));
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_known_vector() {
        assert_eq!(adler32(b"123456789"), 0x091E_01DE);
    }

    #[test]
    fn png_signature_and_iend() {
        let pixels = vec![0xFF_11_22_33u32; 4];
        let png = encode_png_rgba(2, 2, &pixels).expect("encode 2x2");
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        // Last 12 bytes are the IEND chunk: len 0, "IEND", crc AE426082.
        let tail = &png[png.len() - 12..];
        assert_eq!(&tail[4..8], b"IEND");
        assert_eq!(&tail[8..12], &[0xAE, 0x42, 0x60, 0x82]);
    }

    #[test]
    fn png_rejects_bad_inputs() {
        assert!(encode_png_rgba(2, 2, &[0u32; 3]).is_none());
        assert!(encode_png_rgba(0, 2, &[]).is_none());
        assert!(encode_png_rgba(9000, 2, &vec![0u32; 18000]).is_none());
    }

    #[test]
    fn png_idat_crc_covers_type_and_data() {
        // Recompute the IDAT crc independently: chunk layout is
        // len(4) | "IDAT" | data | crc(4); verify the stored crc.
        let pixels = vec![0xFF_00_11_22u32; 6];
        let png = encode_png_rgba(3, 2, &pixels).expect("encode");
        // Walk chunks: after the 8-byte signature, IHDR is first.
        let mut pos = 8usize;
        let mut idat: Option<&[u8]> = None;
        while pos + 8 <= png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let tag = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            let stored = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            let mut input = Vec::with_capacity(4 + len);
            input.extend_from_slice(tag);
            input.extend_from_slice(data);
            assert_eq!(crc32(&input), stored, "chunk {tag:?} crc");
            if tag == b"IDAT" {
                idat = Some(data);
            }
            pos += 12 + len;
        }
        assert!(idat.is_some());
        // zlib header 78 01, then a final stored block.
        let zlib = idat.unwrap();
        assert_eq!(&zlib[0..2], &[0x78, 0x01]);
        assert_eq!(zlib[2] & 1, 1); // BFINAL
    }
}
