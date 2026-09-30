//! `vs-character-creator` — a VoidSprite native plugin (cdylib, zero dependencies).
//!
//! One editor action, **"Character creator: generate base + sheet scaffold"**,
//! driven by `~/.config/voidsprite/character_creator.cfg`. VoidSprite gives
//! parameter dialogs to filters but not to editor actions, so the action is
//! config-file driven. The action procedurally renders:
//!
//! * a 96x192 base character sprite from a small parametric body model
//!   (body type, face variant, weight, bust/chest, height, hairstyle,
//!   outfit variant, palette, accessories), and
//! * a scaffolded 24-frame mood sheet — 4 cols x 6 rows of 96x192 cells,
//!   mood rows idle/blush/wink/pout/celebrate/alarmed at 180 ms/frame —
//!   with the base frame copied into every cell, plus a JSON sidecar that
//!   names the rows.
//!
//! Output goes to PNG files under `~/character-creator-sheets/`. The SDK v1
//! surface exposes no way for an editor action to create session images or
//! frames, so file output follows the sibling `vs_lightshow` exporter, which
//! likewise writes its sheet PNG + JSON sidecar to disk.
//!
//! PNG encoding is dependency-free on purpose: a minimal writer emitting
//! zlib "stored" (uncompressed) deflate blocks, with `crc32`/`adler32`
//! implemented locally and covered by known-answer tests.
//!
//! Layout, top to bottom: FFI contract, config parser, body geometry, pixel
//! art, sheet scaffold, PNG writer, JSON sidecar, action entry, tests.

#![deny(warnings)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_char, c_double, c_int, CString};
use std::mem::{offset_of, size_of};
use std::sync::OnceLock;

/* ------------------------------------------------------------------ */
/* Canvas and sheet geometry (the light-show sprite contract).         */
/* ------------------------------------------------------------------ */

/// Width of one character cell, in pixels.
const CELL_W: i32 = 96;
/// Height of one character cell, in pixels.
const CELL_H: i32 = 192;
/// Sheet columns (4 frames per mood row).
const SHEET_COLS: i32 = 4;
/// Sheet rows (one per mood).
const SHEET_ROWS: i32 = 6;
/// Full sheet width: 4 * 96 = 384 px.
const SHEET_W: i32 = CELL_W * SHEET_COLS;
/// Full sheet height: 6 * 192 = 1152 px.
const SHEET_H: i32 = CELL_H * SHEET_ROWS;
/// Animation frame time for the pipeline, in milliseconds.
const FRAME_MS: u32 = 180;
/// Mood rows, top to bottom, matching the light-show contract.
const MOOD_NAMES: [&str; 6] = ["idle", "blush", "wink", "pout", "celebrate", "alarmed"];
/// Horizontal center of the 96 px cell.
const CENTER_X: i32 = 48;

/* ------------------------------------------------------------------ */
/* Config file bounds.                                                 */
/* ------------------------------------------------------------------ */

/// Config file name under `~/.config/voidsprite/`.
const CONFIG_FILE_NAME: &str = "character_creator.cfg";
/// Longest config line we parse (content chars, excluding the newline).
const MAX_CONFIG_LINE_LEN: usize = 256;
/// Most config lines we read.
const MAX_CONFIG_LINES: usize = 64;
/// Hard cap on config file bytes, checked before parsing.
const MAX_CONFIG_BYTES: usize = 16384;
/// Longest config key we accept.
const MAX_KEY_LEN: usize = 63;
/// Longest character name we accept.
const MAX_NAME_LEN: usize = 48;

/* ------------------------------------------------------------------ */
/* FFI contract: transcription of voidsprite_sdk_c.h (SDK v1).         */
/*                                                                     */
/* The struct is `#[repr(C)]`; every field is an 8-byte function       */
/* pointer, so the table is 36 * 8 = 288 bytes. Field order, types,    */
/* and the packed `VSPLayerInfo` layout below were verified against    */
/* values computed from the C header on the build machine (gcc):       */
/* VSPLayerInfo size 12, offsets 0/4/8; voidspriteSDK size 288 with    */
/* field offsets 0, 8, 16, ... 280 in declaration order. The const     */
/* assertions below fail the build if the transcription ever drifts.   */
/* ------------------------------------------------------------------ */

/// SDK version reported by `voidspriteSDKVersion` (VS_SDK_VERSION).
const VS_SDK_VERSION: c_int = 1;

/// Opaque VoidSprite layer handle. Never constructed here.
pub enum VspLayer {}
/// Opaque VoidSprite filter handle. Never constructed here.
pub enum VspFilter {}
/// Opaque VoidSprite file-exporter handle. Never constructed here.
pub enum VspFileExporter {}
/// Opaque VoidSprite editor-context handle. Never constructed here.
pub enum VspEditorContext {}
/// Opaque VoidSprite brush handle. Never constructed here.
pub enum VspBrush {}

/// Filter callback: `(layer, filter)`.
pub type FilterFn = unsafe extern "C" fn(*mut VspLayer, *mut VspFilter);
/// Editor action callback: `(editor)`.
pub type EditorActionFn = unsafe extern "C" fn(*mut VspEditorContext);
/// Layer importer: `(path) -> layer`.
pub type ImportFn = unsafe extern "C" fn(*mut c_char) -> *mut VspLayer;
/// Importer predicate: `(path) -> bool`.
pub type CanImportFn = unsafe extern "C" fn(*mut c_char) -> bool;
/// Layer exporter: `(layer, path) -> bool`.
pub type ExportFn = unsafe extern "C" fn(*mut VspLayer, *mut c_char) -> bool;
/// Exporter predicate: `(layer) -> bool`.
pub type CanExportFn = unsafe extern "C" fn(*mut VspLayer) -> bool;
/// Brush click callback.
pub type BrushClickFn =
    unsafe extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int);
/// Brush drag callback.
pub type BrushDragFn =
    unsafe extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int, c_int, c_int);
/// Brush release callback.
pub type BrushReleaseFn =
    unsafe extern "C" fn(*mut VspBrush, *mut VspEditorContext, c_int, c_int);

/// `struct VSPLayerInfo`, `#pragma pack(push, 1)`: 3 x int32.
#[repr(C)]
pub struct VspLayerInfo {
    pub layer_type: i32,
    pub width: i32,
    pub height: i32,
}

const _: () = assert!(size_of::<VspLayerInfo>() == 12);
const _: () = assert!(offset_of!(VspLayerInfo, layer_type) == 0);
const _: () = assert!(offset_of!(VspLayerInfo, width) == 4);
const _: () = assert!(offset_of!(VspLayerInfo, height) == 8);

/// `struct voidspriteSDK`: 36 function pointers in C-header order.
#[repr(C)]
pub struct VoidSpriteSdk {
    pub util_fopen_utf8:
        Option<unsafe extern "C" fn(*mut c_char, *const c_char) -> *mut core::ffi::c_void>,
    pub register_filter:
        Option<unsafe extern "C" fn(*const c_char, Option<FilterFn>) -> *mut VspFilter>,
    pub register_layer_importer: Option<
        unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            c_int,
            *mut VspFileExporter,
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
        ) -> *mut VspFileExporter,
    >,
    pub layer_alloc_new: Option<unsafe extern "C" fn(c_int, c_int, c_int) -> *mut VspLayer>,
    pub layer_free: Option<unsafe extern "C" fn(*mut VspLayer)>,
    pub layer_get_info: Option<unsafe extern "C" fn(*mut VspLayer) -> *mut VspLayerInfo>,
    pub layer_set_pixel: Option<unsafe extern "C" fn(*mut VspLayer, c_int, c_int, u32)>,
    pub layer_get_pixel: Option<unsafe extern "C" fn(*mut VspLayer, c_int, c_int) -> u32>,
    pub layer_get_raw_pixel_data: Option<unsafe extern "C" fn(*mut VspLayer) -> *mut u32>,
    pub filter_new_bool_parameter:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char, bool)>,
    pub filter_new_int_parameter:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char, c_int, c_int, c_int)>,
    pub filter_new_double_parameter: Option<
        unsafe extern "C" fn(*mut VspFilter, *const c_char, c_double, c_double, c_double),
    >,
    pub filter_new_double_range_parameter: Option<
        unsafe extern "C" fn(
            *mut VspFilter,
            *const c_char,
            c_double,
            c_double,
            c_double,
            c_double,
            u32,
        ),
    >,
    pub filter_get_double_value:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char) -> c_double>,
    pub filter_get_int_value:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char) -> c_int>,
    pub filter_get_range_value1:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char) -> c_double>,
    pub filter_get_range_value2:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char) -> c_double>,
    pub filter_get_bool_value:
        Option<unsafe extern "C" fn(*mut VspFilter, *const c_char) -> bool>,
    pub util_free: Option<unsafe extern "C" fn(*mut core::ffi::c_void)>,
    pub editor_get_active_color:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> u32>,
    pub editor_get_num_layers:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> c_int>,
    pub editor_get_layer:
        Option<unsafe extern "C" fn(*mut VspEditorContext, c_int) -> *mut VspLayer>,
    pub editor_get_active_layer:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> *mut VspLayer>,
    pub register_brush: Option<
        unsafe extern "C" fn(
            *const c_char,
            *const c_char,
            bool,
            Option<BrushClickFn>,
            Option<BrushDragFn>,
            Option<BrushReleaseFn>,
        ) -> *mut VspBrush,
    >,
    pub editor_set_pixel:
        Option<unsafe extern "C" fn(*mut VspEditorContext, c_int, c_int, u32)>,
    pub vsp_post_notification:
        Option<unsafe extern "C" fn(*const c_char, *const c_char, u32, c_int)>,
    pub vsp_post_success_notification:
        Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    pub vsp_post_error_notification:
        Option<unsafe extern "C" fn(*const c_char, *const c_char)>,
    pub editor_undo_push_layer_state:
        Option<unsafe extern "C" fn(*mut VspEditorContext, *mut VspLayer)>,
    pub register_editor_action:
        Option<unsafe extern "C" fn(*const c_char, Option<EditorActionFn>)>,
    pub editor_flatten_image:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> *mut VspLayer>,
    pub editor_flatten_frame:
        Option<unsafe extern "C" fn(*mut VspEditorContext, c_int) -> *mut VspLayer>,
    pub editor_get_num_frames:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> c_int>,
    pub editor_get_active_frame_index:
        Option<unsafe extern "C" fn(*mut VspEditorContext) -> c_int>,
    pub vsp_get_localized_string:
        Option<unsafe extern "C" fn(*const c_char) -> *const c_char>,
}

const _: () = assert!(size_of::<VoidSpriteSdk>() == 288);
const _: () = assert!(offset_of!(VoidSpriteSdk, util_fopen_utf8) == 0);
const _: () = assert!(offset_of!(VoidSpriteSdk, register_filter) == 8);
const _: () = assert!(offset_of!(VoidSpriteSdk, register_layer_importer) == 16);
const _: () = assert!(offset_of!(VoidSpriteSdk, register_layer_exporter) == 24);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_alloc_new) == 32);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_free) == 40);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_get_info) == 48);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_set_pixel) == 56);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_get_pixel) == 64);
const _: () = assert!(offset_of!(VoidSpriteSdk, layer_get_raw_pixel_data) == 72);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_new_bool_parameter) == 80);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_new_int_parameter) == 88);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_new_double_parameter) == 96);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_new_double_range_parameter) == 104);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_get_double_value) == 112);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_get_int_value) == 120);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_get_range_value1) == 128);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_get_range_value2) == 136);
const _: () = assert!(offset_of!(VoidSpriteSdk, filter_get_bool_value) == 144);
const _: () = assert!(offset_of!(VoidSpriteSdk, util_free) == 152);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_active_color) == 160);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_num_layers) == 168);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_layer) == 176);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_active_layer) == 184);
const _: () = assert!(offset_of!(VoidSpriteSdk, register_brush) == 192);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_set_pixel) == 200);
const _: () = assert!(offset_of!(VoidSpriteSdk, vsp_post_notification) == 208);
const _: () = assert!(offset_of!(VoidSpriteSdk, vsp_post_success_notification) == 216);
const _: () = assert!(offset_of!(VoidSpriteSdk, vsp_post_error_notification) == 224);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_undo_push_layer_state) == 232);
const _: () = assert!(offset_of!(VoidSpriteSdk, register_editor_action) == 240);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_flatten_image) == 248);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_flatten_frame) == 256);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_num_frames) == 264);
const _: () = assert!(offset_of!(VoidSpriteSdk, editor_get_active_frame_index) == 272);
const _: () = assert!(offset_of!(VoidSpriteSdk, vsp_get_localized_string) == 280);

/* ------------------------------------------------------------------ */
/* Host table capture and thin unsafe wrappers.                        */
/*                                                                     */
/* `unsafe` lives only here: one stored pointer, three tiny call       */
/* wrappers. Everything above this point is pure safe Rust.            */
/* ------------------------------------------------------------------ */

/// The host SDK table, captured once in `pluginInit`. Never mutated after.
static SDK_TABLE: OnceLock<SdkTablePtr> = OnceLock::new();

/// Wrapper making the raw host-table pointer shareable across threads.
///
/// Safety: the pointer refers to the host's `voidspriteSDK` table, which
/// the host guarantees outlives the plugin. It is stored exactly once in
/// `pluginInit` and never written through afterwards — every use is a
/// read of the table or a call through one of its function pointers —
/// so sharing it between threads is sound.
struct SdkTablePtr(*const VoidSpriteSdk);
// Safety: see the struct documentation above.
unsafe impl Send for SdkTablePtr {}
// Safety: see the struct documentation above.
unsafe impl Sync for SdkTablePtr {}

/// Fetch the host SDK table captured in `pluginInit`.
///
/// Safety: the caller must ensure `pluginInit` ran with a valid,
/// fully-populated host table that outlives the plugin. The plugin only
/// ever stores the pointer the host itself passed, once, and never writes
/// through it except via the table's own function pointers.
unsafe fn sdk_table() -> Option<&'static VoidSpriteSdk> {
    let ptr = SDK_TABLE.get()?.0;
    // Safety: non-null (checked in pluginInit); the host guarantees the
    // table outlives the plugin; never mutated after the single store.
    Some(unsafe { &*ptr })
}

/// Register the editor action with the host. Silent no-op when the host
/// table or entry point is unavailable (e.g. under unit tests).
fn host_register_editor_action(name: &'static [u8], action: EditorActionFn) {
    // Safety: `name` is a 'static NUL-terminated byte string; `action` is a
    // non-panicking `extern "C"` fn; both outlive the registration call.
    // The table itself is valid per the `sdk_table` contract.
    unsafe {
        let Some(sdk) = sdk_table() else { return };
        let Some(register) = sdk.register_editor_action else {
            return;
        };
        register(name.as_ptr() as *const c_char, Some(action));
    }
}

/// Post a success notification. Silent no-op without a host table.
fn notify_success(title: &str, message: &str) {
    // Safety: as in `host_register_editor_action`; the C strings live for
    // the duration of the call.
    unsafe {
        let Some(sdk) = sdk_table() else { return };
        let Some(post) = sdk.vsp_post_success_notification else {
            return;
        };
        let (Ok(title_c), Ok(message_c)) = (CString::new(title), CString::new(message))
        else {
            return;
        };
        post(title_c.as_ptr(), message_c.as_ptr());
    }
}

/// Post an error notification. Silent no-op without a host table.
fn notify_error(title: &str, message: &str) {
    // Safety: as in `notify_success`.
    unsafe {
        let Some(sdk) = sdk_table() else { return };
        let Some(post) = sdk.vsp_post_error_notification else {
            return;
        };
        let (Ok(title_c), Ok(message_c)) = (CString::new(title), CString::new(message))
        else {
            return;
        };
        post(title_c.as_ptr(), message_c.as_ptr());
    }
}

/* ------------------------------------------------------------------ */
/* Plugin entry points (exact symbol names required by the host ABI).  */
/* ------------------------------------------------------------------ */

/// NUL-terminated static strings returned to the host; never freed by us.
static PLUGIN_NAME_C: &[u8] = b"character creator\0";
static PLUGIN_VERSION_C: &[u8] = b"1.0.0\0";
static PLUGIN_DESCRIPTION_C: &[u8] =
    b"Procedurally generates a parametric base character sprite plus a scaffolded 24-frame mood sheet for the light-show pipeline, driven by ~/.config/voidsprite/character_creator.cfg.\0";
static PLUGIN_AUTHORS_C: &[u8] = b"Pax (Qompass AI)\0";
static ACTION_NAME_C: &[u8] = b"Character creator: generate base + sheet scaffold\0";

/// Host entry: capture the SDK table and register the editor action.
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn pluginInit(sdk: *mut VoidSpriteSdk) {
    if sdk.is_null() {
        return;
    }
    // First init wins; the host calls this once per plugin load.
    if SDK_TABLE.set(SdkTablePtr(sdk as *const VoidSpriteSdk)).is_err() {
        return;
    }
    host_register_editor_action(ACTION_NAME_C, character_creator_action);
}

/// Report the SDK version this plugin was built against (VS_SDK_VERSION).
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn voidspriteSDKVersion() -> c_int {
    VS_SDK_VERSION
}

/// Display name shown in the host's plugin list.
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn getPluginName() -> *const c_char {
    PLUGIN_NAME_C.as_ptr() as *const c_char
}

/// Plugin version string.
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn getPluginVersion() -> *const c_char {
    PLUGIN_VERSION_C.as_ptr() as *const c_char
}

/// One-line plugin description.
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn getPluginDescription() -> *const c_char {
    PLUGIN_DESCRIPTION_C.as_ptr() as *const c_char
}

/// Plugin authors string.
#[allow(non_snake_case)] // symbol name is dictated by the host ABI
#[unsafe(no_mangle)]
pub extern "C" fn getPluginAuthors() -> *const c_char {
    PLUGIN_AUTHORS_C.as_ptr() as *const c_char
}

/* ------------------------------------------------------------------ */
/* Config: strict parser for character_creator.cfg.                     */
/*                                                                     */
/* Discipline (mirrors the sibling exporter's parser): blank lines and */
/* `#` comments are skipped; every other line must be `key=value`;     */
/* unknown keys, duplicate keys, over-long lines, and malformed values */
/* abort the whole load loudly. Parsing stages into a slot array and   */
/* commits to a `CharacterConfig` only after every cross-key rule      */
/* passes, so a failed load never yields a half-configured character.  */
/* ------------------------------------------------------------------ */

/// Character body type. Drives which torso parameter applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyType {
    Female,
    Male,
}

/// Hairstyle variant id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hairstyle {
    Ponytail,
    Braids,
    Bob,
    Long,
}

/// Base face-shape variant (the scaffold face; mood expressions are drawn
/// per mood row by the artist afterwards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Round,
    Oval,
    SquareJaw,
}

/// Body-mass step. Changes torso and limb WIDTHS, not the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Slim,
    Average,
    Heavy,
}

/// Bust scale step. Female only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BustSize {
    Small,
    Medium,
    Large,
}

/// Chest scale step. Male only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChestSize {
    Flat,
    Average,
    Muscular,
}

/// Height step. Moves between chibi and mature head-to-body proportions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Height {
    Short,
    Average,
    Tall,
}

/// Outfit variant id. Each is a defined procedural variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outfit {
    TechJacket,
    Casual,
    Formal,
    Athletic,
}

/// Accessory flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accessory {
    Visor,
    Headphones,
    Hairclip,
}

/// Fully validated character configuration. Colors are 0xAARRGGBB with
/// opaque alpha. `bust` is `Some` iff `body_type` is female; `chest` is
/// `Some` iff `body_type` is male — anything else fails validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterConfig {
    pub name: String,
    pub body_type: BodyType,
    pub hairstyle: Hairstyle,
    pub hair_color: u32,
    pub face: Face,
    pub eye_color: u32,
    pub weight: Weight,
    pub bust: Option<BustSize>,
    pub chest: Option<ChestSize>,
    pub height: Height,
    pub outfit: Outfit,
    pub outfit_color: u32,
    pub accent: u32,
    pub accessories: Vec<Accessory>,
}

/// Every way a config load can fail. Carries the 1-based line number so
/// the error notification tells the user exactly where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// File exceeds `MAX_CONFIG_BYTES`.
    TooLarge,
    /// More than `MAX_CONFIG_LINES` lines.
    TooManyLines,
    /// A line exceeds `MAX_CONFIG_LINE_LEN` content chars.
    LineTooLong(usize),
    /// A non-blank, non-comment line without `=`.
    MissingEquals(usize),
    /// Empty key before `=`.
    EmptyKey(usize),
    /// Key longer than `MAX_KEY_LEN`.
    KeyTooLong(usize),
    /// Key is not one of the 14 known keys.
    UnknownKey { line: usize, key: String },
    /// Key appeared twice.
    DuplicateKey { line: usize, key: &'static str },
    /// Value fails the key's format rules.
    BadValue {
        line: usize,
        key: &'static str,
        reason: &'static str,
    },
    /// A required key is absent.
    MissingKey(&'static str),
    /// `bust_size` given with `body_type=male`.
    BustWithMale(usize),
    /// `chest_size` given with `body_type=female`.
    ChestWithFemale(usize),
}

impl ConfigError {
    /// Human-readable message for the error notification. Bounded: keys
    /// are capped at `MAX_KEY_LEN` chars by the parser.
    pub fn message(&self) -> String {
        match self {
            ConfigError::TooLarge => {
                format!("Config too large (max {MAX_CONFIG_BYTES} bytes).")
            }
            ConfigError::TooManyLines => {
                format!("Config has more than {MAX_CONFIG_LINES} lines.")
            }
            ConfigError::LineTooLong(line) => {
                format!("Line {line}: too long (max {MAX_CONFIG_LINE_LEN} chars).")
            }
            ConfigError::MissingEquals(line) => {
                format!("Line {line}: expected `key=value`.")
            }
            ConfigError::EmptyKey(line) => format!("Line {line}: empty key."),
            ConfigError::KeyTooLong(line) => {
                format!("Line {line}: key too long (max {MAX_KEY_LEN} chars).")
            }
            ConfigError::UnknownKey { line, key } => {
                format!("Line {line}: unknown key `{key}`.")
            }
            ConfigError::DuplicateKey { line, key } => {
                format!("Line {line}: duplicate key `{key}`.")
            }
            ConfigError::BadValue { line, key, reason } => {
                format!("Line {line}: bad value for `{key}`: {reason}.")
            }
            ConfigError::MissingKey(key) => format!("Missing required key `{key}`."),
            ConfigError::BustWithMale(line) => format!(
                "Line {line}: `bust_size` is female-only; `body_type` is male."
            ),
            ConfigError::ChestWithFemale(line) => format!(
                "Line {line}: `chest_size` is male-only; `body_type` is female."
            ),
        }
    }
}

/// Known keys in slot order; index doubles as the parse-dispatch key.
const KNOWN_KEYS: [&str; 14] = [
    "name",
    "body_type",
    "hairstyle",
    "hair_color",
    "face",
    "eye_color",
    "weight",
    "bust_size",
    "chest_size",
    "height",
    "outfit",
    "outfit_color",
    "accent",
    "accessories",
];

/// A parsed-but-not-yet-cross-validated value, in slot order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Name(String),
    BodyType(BodyType),
    Hairstyle(Hairstyle),
    Color(u32),
    Face(Face),
    Weight(Weight),
    BustSize(BustSize),
    ChestSize(ChestSize),
    Height(Height),
    Outfit(Outfit),
    Accessories(Vec<Accessory>),
}

/// Parse `#rrggbb` (`#` prefix required, exactly 6 hex digits) into
/// opaque 0xAARRGGBB. Returns `None` on any deviation.
fn parse_hex_color(text: &str) -> Option<u32> {
    let bytes = text.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let mut rgb: u32 = 0;
    for &digit in &bytes[1..] {
        let nibble = match digit {
            b'0'..=b'9' => digit - b'0',
            b'a'..=b'f' => digit - b'a' + 10,
            b'A'..=b'F' => digit - b'A' + 10,
            _ => return None,
        };
        rgb = (rgb << 4) | u32::from(nibble);
    }
    Some(0xFF00_0000 | rgb)
}

/// Validate the character name: 1..=`MAX_NAME_LEN` ASCII chars from
/// `[A-Za-z0-9 _-]`. The restricted charset keeps it safe for filenames
/// and JSON without escaping.
fn parse_name(text: &str) -> Option<String> {
    if text.is_empty() || text.len() > MAX_NAME_LEN {
        return None;
    }
    let ok = text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_');
    if ok { Some(text.to_string()) } else { None }
}

/// Parse the comma-separated accessory flags. Empty value means none;
/// unknown or duplicate flags are errors.
fn parse_accessories(text: &str, line: usize) -> Result<Vec<Accessory>, ConfigError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for token in text.split(',') {
        let flag = token.trim_matches(|ch: char| ch == ' ' || ch == '\t');
        let accessory = match flag {
            "visor" => Accessory::Visor,
            "headphones" => Accessory::Headphones,
            "hairclip" => Accessory::Hairclip,
            _ => {
                return Err(ConfigError::BadValue {
                    line,
                    key: "accessories",
                    reason: "unknown accessory flag (want visor, headphones, hairclip)",
                });
            }
        };
        if out.contains(&accessory) {
            return Err(ConfigError::BadValue {
                line,
                key: "accessories",
                reason: "duplicate accessory flag",
            });
        }
        out.push(accessory);
    }
    Ok(out)
}

/// Parse one known key's value into a staged `Value`.
fn parse_value(slot: usize, text: &str, line: usize) -> Result<Value, ConfigError> {
    let bad = |key: &'static str, reason: &'static str| ConfigError::BadValue {
        line,
        key,
        reason,
    };
    match slot {
        0 => parse_name(text)
            .map(Value::Name)
            .ok_or_else(|| bad("name", "want 1-48 chars of [A-Za-z0-9 _-]")),
        1 => match text {
            "female" => Ok(Value::BodyType(BodyType::Female)),
            "male" => Ok(Value::BodyType(BodyType::Male)),
            _ => Err(bad("body_type", "want female or male")),
        },
        2 => match text {
            "ponytail" => Ok(Value::Hairstyle(Hairstyle::Ponytail)),
            "braids" => Ok(Value::Hairstyle(Hairstyle::Braids)),
            "bob" => Ok(Value::Hairstyle(Hairstyle::Bob)),
            "long" => Ok(Value::Hairstyle(Hairstyle::Long)),
            _ => Err(bad("hairstyle", "want ponytail, braids, bob, or long")),
        },
        3 => parse_hex_color(text)
            .map(Value::Color)
            .ok_or_else(|| bad("hair_color", "want #rrggbb (6 hex digits)")),
        4 => match text {
            "round" => Ok(Value::Face(Face::Round)),
            "oval" => Ok(Value::Face(Face::Oval)),
            "square-jaw" => Ok(Value::Face(Face::SquareJaw)),
            _ => Err(bad("face", "want round, oval, or square-jaw")),
        },
        5 => parse_hex_color(text)
            .map(Value::Color)
            .ok_or_else(|| bad("eye_color", "want #rrggbb (6 hex digits)")),
        6 => match text {
            "slim" => Ok(Value::Weight(Weight::Slim)),
            "average" => Ok(Value::Weight(Weight::Average)),
            "heavy" => Ok(Value::Weight(Weight::Heavy)),
            _ => Err(bad("weight", "want slim, average, or heavy")),
        },
        7 => match text {
            "small" => Ok(Value::BustSize(BustSize::Small)),
            "medium" => Ok(Value::BustSize(BustSize::Medium)),
            "large" => Ok(Value::BustSize(BustSize::Large)),
            _ => Err(bad("bust_size", "want small, medium, or large")),
        },
        8 => match text {
            "flat" => Ok(Value::ChestSize(ChestSize::Flat)),
            "average" => Ok(Value::ChestSize(ChestSize::Average)),
            "muscular" => Ok(Value::ChestSize(ChestSize::Muscular)),
            _ => Err(bad("chest_size", "want flat, average, or muscular")),
        },
        9 => match text {
            "short" => Ok(Value::Height(Height::Short)),
            "average" => Ok(Value::Height(Height::Average)),
            "tall" => Ok(Value::Height(Height::Tall)),
            _ => Err(bad("height", "want short, average, or tall")),
        },
        10 => match text {
            "tech-jacket" => Ok(Value::Outfit(Outfit::TechJacket)),
            "casual" => Ok(Value::Outfit(Outfit::Casual)),
            "formal" => Ok(Value::Outfit(Outfit::Formal)),
            "athletic" => Ok(Value::Outfit(Outfit::Athletic)),
            _ => Err(bad(
                "outfit",
                "want tech-jacket, casual, formal, or athletic",
            )),
        },
        11 => parse_hex_color(text)
            .map(Value::Color)
            .ok_or_else(|| bad("outfit_color", "want #rrggbb (6 hex digits)")),
        12 => parse_hex_color(text)
            .map(Value::Color)
            .ok_or_else(|| bad("accent", "want #rrggbb (6 hex digits)")),
        13 => parse_accessories(text, line).map(Value::Accessories),
        _ => {
            // Unreachable: `slot` always comes from `KNOWN_KEYS` lookup.
            debug_assert!(false, "parse_value called with bad slot");
            Err(bad("accessories", "internal error"))
        }
    }
}

/// Take a required slot, or report the missing key.
fn take_slot(
    slots: &mut [Option<(usize, Value)>],
    slot: usize,
    key: &'static str,
) -> Result<(usize, Value), ConfigError> {
    slots[slot].take().ok_or(ConfigError::MissingKey(key))
}

/// Commit staged values to a `CharacterConfig`, enforcing the cross-key
/// rules: `bust_size` is female-only, `chest_size` is male-only, and the
/// matching one is required.
fn finish_config(
    slots: &mut [Option<(usize, Value)>],
) -> Result<CharacterConfig, ConfigError> {
    let (_, value) = take_slot(slots, 0, "name")?;
    let Value::Name(name) = value else {
        return Err(ConfigError::MissingKey("name"));
    };
    let (_, value) = take_slot(slots, 1, "body_type")?;
    let Value::BodyType(body_type) = value else {
        return Err(ConfigError::MissingKey("body_type"));
    };
    let (_, value) = take_slot(slots, 2, "hairstyle")?;
    let Value::Hairstyle(hairstyle) = value else {
        return Err(ConfigError::MissingKey("hairstyle"));
    };
    let (_, value) = take_slot(slots, 3, "hair_color")?;
    let Value::Color(hair_color) = value else {
        return Err(ConfigError::MissingKey("hair_color"));
    };
    let (_, value) = take_slot(slots, 4, "face")?;
    let Value::Face(face) = value else {
        return Err(ConfigError::MissingKey("face"));
    };
    let (_, value) = take_slot(slots, 5, "eye_color")?;
    let Value::Color(eye_color) = value else {
        return Err(ConfigError::MissingKey("eye_color"));
    };
    let (_, value) = take_slot(slots, 6, "weight")?;
    let Value::Weight(weight) = value else {
        return Err(ConfigError::MissingKey("weight"));
    };
    let (_, value) = take_slot(slots, 9, "height")?;
    let Value::Height(height) = value else {
        return Err(ConfigError::MissingKey("height"));
    };
    let (_, value) = take_slot(slots, 10, "outfit")?;
    let Value::Outfit(outfit) = value else {
        return Err(ConfigError::MissingKey("outfit"));
    };
    let (_, value) = take_slot(slots, 11, "outfit_color")?;
    let Value::Color(outfit_color) = value else {
        return Err(ConfigError::MissingKey("outfit_color"));
    };
    let (_, value) = take_slot(slots, 12, "accent")?;
    let Value::Color(accent) = value else {
        return Err(ConfigError::MissingKey("accent"));
    };
    let (_, value) = take_slot(slots, 13, "accessories")?;
    let Value::Accessories(accessories) = value else {
        return Err(ConfigError::MissingKey("accessories"));
    };

    // Torso-scale cross-validation: invalid combinations fail loudly,
    // never silently ignored.
    let (bust, chest) = match body_type {
        BodyType::Female => {
            if let Some((line, _)) = slots[8].take() {
                return Err(ConfigError::ChestWithFemale(line));
            }
            let (_line, value) = take_slot(slots, 7, "bust_size")?;
            let Value::BustSize(bust) = value else {
                return Err(ConfigError::MissingKey("bust_size"));
            };
            (Some(bust), None)
        }
        BodyType::Male => {
            if let Some((line, _)) = slots[7].take() {
                return Err(ConfigError::BustWithMale(line));
            }
            let (_line, value) = take_slot(slots, 8, "chest_size")?;
            let Value::ChestSize(chest) = value else {
                return Err(ConfigError::MissingKey("chest_size"));
            };
            (None, Some(chest))
        }
    };

    Ok(CharacterConfig {
        name,
        body_type,
        hairstyle,
        hair_color,
        face,
        eye_color,
        weight,
        bust,
        chest,
        height,
        outfit,
        outfit_color,
        accent,
        accessories,
    })
}

/// Trim ASCII spaces, tabs, and carriage returns from both ends.
fn trim_ws(text: &str) -> &str {
    text.trim_matches(|ch: char| ch == ' ' || ch == '\t' || ch == '\r')
}

/// Parse a full config file. Contract: `text` is at most
/// `MAX_CONFIG_BYTES` bytes (checked by the caller before parsing).
/// Returns the first error encountered; the staged values are discarded.
pub fn parse_config(text: &str) -> Result<CharacterConfig, ConfigError> {
    if text.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }
    if text.lines().count() > MAX_CONFIG_LINES {
        return Err(ConfigError::TooManyLines);
    }
    let mut slots: [Option<(usize, Value)>; 14] = Default::default();
    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        if raw_line.len() > MAX_CONFIG_LINE_LEN {
            return Err(ConfigError::LineTooLong(line_no));
        }
        let line = trim_ws(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else {
            return Err(ConfigError::MissingEquals(line_no));
        };
        let key = trim_ws(&line[..eq]);
        let value = trim_ws(&line[eq + 1..]);
        if key.is_empty() {
            return Err(ConfigError::EmptyKey(line_no));
        }
        if key.len() > MAX_KEY_LEN {
            return Err(ConfigError::KeyTooLong(line_no));
        }
        let Some(slot) = KNOWN_KEYS.iter().position(|known| *known == key) else {
            return Err(ConfigError::UnknownKey {
                line: line_no,
                key: key.to_string(),
            });
        };
        if slots[slot].is_some() {
            return Err(ConfigError::DuplicateKey {
                line: line_no,
                key: KNOWN_KEYS[slot],
            });
        }
        slots[slot] = Some((line_no, parse_value(slot, value, line_no)?));
    }
    finish_config(&mut slots)
}

/* ------------------------------------------------------------------ */
/* Body geometry: the parametric model. Every scale step changes PIXEL */
/* dimensions (documented in the README "Parametric body" section).    */
/* ------------------------------------------------------------------ */

/// Pixel dimensions derived from the config's scale steps. Pure function
/// of `CharacterConfig`: no allocation, no I/O, fully unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyGeometry {
    pub head_x: i32,
    pub head_y: i32,
    pub head_w: i32,
    pub head_h: i32,
    pub head_bottom: i32,
    pub torso_x: i32,
    pub torso_y: i32,
    pub torso_w: i32,
    pub torso_h: i32,
    /// Upper-torso (bust/chest zone) half width, incl. the bust/chest delta.
    pub zone_half_w: i32,
    /// Muscular male only: draw an accent shoulder bar.
    pub shoulder_bar: bool,
    pub arm_w: i32,
    pub leg_w: i32,
    pub leg_y: i32,
    pub leg_h: i32,
    pub eye_x0: i32,
    pub eye_x1: i32,
    pub eye_y: i32,
    pub eye_w: i32,
    pub eye_h: i32,
    pub mouth_x: i32,
    pub mouth_y: i32,
    pub mouth_w: i32,
}

/// Derive pixel geometry from the config.
///
/// Height sets the head box (head-to-body ratio on the 192 px canvas):
/// short = 48 px head (1:4 chibi), average = 35 px (1:5.5),
/// tall = 27 px (1:7). Face adjusts the box: oval narrows and lengthens,
/// square-jaw widens. Weight sets torso half-width (slim 14, average 18,
/// heavy 24 px) and limb widths. Bust/chest adjust the upper-torso zone
/// half-width: bust small/medium/large = +0/+3/+6 px per side,
/// chest flat/average/muscular = -2/+0/+4 px per side.
pub fn body_geometry(cfg: &CharacterConfig) -> BodyGeometry {
    let (mut head_x, head_y, mut head_w, mut head_h) = match cfg.height {
        Height::Short => (24, 8, 48, 48),
        Height::Average => (31, 16, 34, 35),
        Height::Tall => (34, 22, 28, 27),
    };
    match cfg.face {
        Face::Round => {}
        Face::Oval => {
            head_x += 2;
            head_w -= 4;
            head_h += 6;
        }
        Face::SquareJaw => {
            head_x -= 2;
            head_w += 4;
        }
    }
    let head_bottom = head_y + head_h;

    let (torso_half_w, arm_w, leg_w) = match cfg.weight {
        Weight::Slim => (14, 8, 9),
        Weight::Average => (18, 10, 11),
        Weight::Heavy => (24, 13, 15),
    };
    // Bust/chest delta on the upper-torso zone, per side. finish_config
    // guarantees the matching variant is Some; a hand-built config with
    // None falls back to no delta rather than inventing one.
    let zone_delta: i32 = match (cfg.body_type, cfg.bust, cfg.chest) {
        (BodyType::Female, Some(BustSize::Small), _) => 0,
        (BodyType::Female, Some(BustSize::Medium), _) => 3,
        (BodyType::Female, Some(BustSize::Large), _) => 6,
        (BodyType::Male, _, Some(ChestSize::Flat)) => -2,
        (BodyType::Male, _, Some(ChestSize::Average)) => 0,
        (BodyType::Male, _, Some(ChestSize::Muscular)) => 4,
        _ => 0,
    };

    let torso_y = head_bottom + 6;
    let torso_h = (CELL_H - head_bottom) * 45 / 100;
    let leg_y = torso_y + torso_h;
    // 10 px boots below the legs.
    let leg_h = CELL_H - leg_y - 10;

    let eye_w = (head_w / 9).max(3);
    let eye_h = (head_h / 6).max(5);
    let eye_y = head_y + head_h * 45 / 100;
    let eye_x0 = CENTER_X - head_w / 4 - eye_w / 2;
    let eye_x1 = CENTER_X + head_w / 4 - eye_w / 2;
    let mouth_w = (head_w / 5).max(3);

    BodyGeometry {
        head_x,
        head_y,
        head_w,
        head_h,
        head_bottom,
        torso_x: CENTER_X - torso_half_w,
        torso_y,
        torso_w: torso_half_w * 2,
        torso_h,
        zone_half_w: torso_half_w + zone_delta,
        shoulder_bar: matches!(
            (cfg.body_type, cfg.chest),
            (BodyType::Male, Some(ChestSize::Muscular))
        ),
        arm_w,
        leg_w,
        leg_y,
        leg_h,
        eye_x0,
        eye_x1,
        eye_y,
        eye_w,
        eye_h,
        mouth_x: CENTER_X - mouth_w / 2,
        mouth_y: head_y + head_h * 72 / 100,
        mouth_w,
    }
}

/* ------------------------------------------------------------------ */
/* Pixel art: canvas, color helpers, and the procedural renderer.      */
/*                                                                     */
/* Pixels are 0xAARRGGBB. The style is a geometric mannequin: body     */
/* shapes in outfit/accent, hair in hair_color, eyes in eye_color.     */
/* Every coordinate derives from `BodyGeometry`; `Canvas::set` clips,  */
/* so drawing can never panic or write out of bounds.                  */
/* ------------------------------------------------------------------ */

/// Fully transparent pixel.
const TRANSPARENT: u32 = 0x0000_0000;

/// A pixel buffer with clipped writes. Invariant: `px.len() == w * h`,
/// `w > 0`, `h > 0` — established by `new`, so indexing never panics.
pub struct Canvas {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
}

impl Canvas {
    /// Create a transparent canvas. `w`/`h` are our own small constants.
    pub fn new(w: i32, h: i32) -> Self {
        assert!(w > 0 && h > 0, "canvas dimensions must be positive");
        // w, h <= 1152 by construction: the product cannot overflow.
        let len = w as usize * h as usize;
        Self {
            w,
            h,
            px: vec![TRANSPARENT; len],
        }
    }

    /// Clipped pixel write: out-of-bounds writes are dropped silently.
    pub fn set(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        let index = y as usize * self.w as usize + x as usize;
        self.px[index] = color;
    }

    /// Clipped pixel read: out-of-bounds reads yield transparency.
    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return TRANSPARENT;
        }
        self.px[y as usize * self.w as usize + x as usize]
    }
}

/// Scale each RGB channel of a 0xAARRGGBB color by `num/den`; alpha kept.
fn shade_rgb(color: u32, num: u32, den: u32) -> u32 {
    let scale = |channel: u32| ((channel * num) / den).min(255);
    (color & 0xFF00_0000)
        | (scale((color >> 16) & 0xFF) << 16)
        | (scale((color >> 8) & 0xFF) << 8)
        | scale(color & 0xFF)
}

/// Move each RGB channel one third of the way toward white; alpha kept.
fn brighten(color: u32) -> u32 {
    let lift = |channel: u32| channel + (255 - channel) / 3;
    (color & 0xFF00_0000)
        | (lift((color >> 16) & 0xFF) << 16)
        | (lift((color >> 8) & 0xFF) << 8)
        | lift(color & 0xFF)
}

/// Clipped filled rectangle.
fn fill_rect(canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: u32) {
    if w <= 0 || h <= 0 {
        return;
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = x.saturating_add(w).min(canvas.w);
    let y1 = y.saturating_add(h).min(canvas.h);
    for yy in y0..y1 {
        for xx in x0..x1 {
            let index = yy as usize * canvas.w as usize + xx as usize;
            canvas.px[index] = color;
        }
    }
}

/// Which corner of a box is being rounded.
#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Punch a quarter-circle of radius `r` out of one box corner (back to
/// transparent), giving the head its rounded silhouette.
fn punch_corner(canvas: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32, corner: Corner) {
    if r <= 0 {
        return;
    }
    // Anchor: the box corner the circle is centered just inside of.
    let (qx0, qy0) = match corner {
        Corner::TopLeft => (x, y),
        Corner::TopRight => (x + w - r, y),
        Corner::BottomLeft => (x, y + h - r),
        Corner::BottomRight => (x + w - r, y + h - r),
    };
    for dy in 0..r {
        for dx in 0..r {
            // Pixel centers outside the circle get cleared.
            let ox = dx - r + 1;
            let oy = dy - r + 1;
            if ox * ox + oy * oy >= r * r {
                canvas.set(qx0 + dx, qy0 + dy, TRANSPARENT);
            }
        }
    }
}

/// Draw the head silhouette with per-face corner rounding.
fn draw_head(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    let (top_r, bottom_r) = match cfg.face {
        Face::Round => (8, 8),
        Face::Oval => (6, 6),
        Face::SquareJaw => (4, 0),
    };
    fill_rect(
        canvas,
        geo.head_x,
        geo.head_y,
        geo.head_w,
        geo.head_h,
        cfg.outfit_color,
    );
    punch_corner(
        canvas,
        geo.head_x,
        geo.head_y,
        geo.head_w,
        geo.head_h,
        top_r,
        Corner::TopLeft,
    );
    punch_corner(
        canvas,
        geo.head_x,
        geo.head_y,
        geo.head_w,
        geo.head_h,
        top_r,
        Corner::TopRight,
    );
    punch_corner(
        canvas,
        geo.head_x,
        geo.head_y,
        geo.head_w,
        geo.head_h,
        bottom_r,
        Corner::BottomLeft,
    );
    punch_corner(
        canvas,
        geo.head_x,
        geo.head_y,
        geo.head_w,
        geo.head_h,
        bottom_r,
        Corner::BottomRight,
    );
}

/// Eyes in the eye color, mouth as a darkened-accent line.
fn draw_face(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    fill_rect(canvas, geo.eye_x0, geo.eye_y, geo.eye_w, geo.eye_h, cfg.eye_color);
    fill_rect(canvas, geo.eye_x1, geo.eye_y, geo.eye_w, geo.eye_h, cfg.eye_color);
    fill_rect(
        canvas,
        geo.mouth_x,
        geo.mouth_y,
        geo.mouth_w,
        2,
        shade_rgb(cfg.accent, 1, 2),
    );
}

/// Neck, torso (with the bust/chest zone), arms, hands, legs, boots.
fn draw_body(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    let dark_outfit = shade_rgb(cfg.outfit_color, 3, 4);
    let dark_accent = shade_rgb(cfg.accent, 2, 3);

    // Legs with boots.
    let leg_left_x = CENTER_X - 3 - geo.leg_w;
    let leg_right_x = CENTER_X + 3;
    fill_rect(canvas, leg_left_x, geo.leg_y, geo.leg_w, geo.leg_h, cfg.accent);
    fill_rect(canvas, leg_right_x, geo.leg_y, geo.leg_w, geo.leg_h, cfg.accent);
    fill_rect(canvas, leg_left_x, geo.leg_y + geo.leg_h, geo.leg_w, 10, dark_outfit);
    fill_rect(
        canvas,
        leg_right_x,
        geo.leg_y + geo.leg_h,
        geo.leg_w,
        10,
        dark_outfit,
    );

    // Arms with hands.
    let arm_left_x = geo.torso_x - 2 - geo.arm_w;
    let arm_right_x = geo.torso_x + geo.torso_w + 2;
    let arm_h = geo.torso_h - 8;
    fill_rect(canvas, arm_left_x, geo.torso_y + 2, geo.arm_w, arm_h, cfg.accent);
    fill_rect(canvas, arm_right_x, geo.torso_y + 2, geo.arm_w, arm_h, cfg.accent);
    fill_rect(canvas, arm_left_x, geo.torso_y + 2 + arm_h, geo.arm_w, 5, dark_accent);
    fill_rect(
        canvas,
        arm_right_x,
        geo.torso_y + 2 + arm_h,
        geo.arm_w,
        5,
        dark_accent,
    );

    // Torso, then the bust/chest zone (upper 20 px) at its adjusted width.
    fill_rect(canvas, geo.torso_x, geo.torso_y, geo.torso_w, geo.torso_h, cfg.outfit_color);
    let zone_x = CENTER_X - geo.zone_half_w;
    let zone_h = 20.min(geo.torso_h);
    fill_rect(canvas, zone_x, geo.torso_y, geo.zone_half_w * 2, zone_h, cfg.outfit_color);

    // Muscular shoulder bar.
    if geo.shoulder_bar {
        fill_rect(
            canvas,
            CENTER_X - geo.zone_half_w - 8,
            geo.torso_y - 2,
            (geo.zone_half_w + 8) * 2,
            6,
            cfg.accent,
        );
    }

    // Neck.
    fill_rect(
        canvas,
        CENTER_X - 5,
        geo.head_bottom,
        10,
        6,
        shade_rgb(cfg.outfit_color, 4, 5),
    );

    draw_outfit_details(canvas, cfg, geo);
}

/// Procedural outfit variants, drawn over the torso in the accent color.
fn draw_outfit_details(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    let accent = cfg.accent;
    match cfg.outfit {
        Outfit::TechJacket => {
            // Collar tabs, zipper, shoulder pads.
            fill_rect(canvas, CENTER_X - 13, geo.torso_y, 8, 5, accent);
            fill_rect(canvas, CENTER_X + 5, geo.torso_y, 8, 5, accent);
            fill_rect(canvas, CENTER_X - 1, geo.torso_y + 5, 2, geo.torso_h - 5, accent);
            fill_rect(canvas, geo.torso_x - 4, geo.torso_y, 10, 5, accent);
            fill_rect(canvas, geo.torso_x + geo.torso_w - 6, geo.torso_y, 10, 5, accent);
        }
        Outfit::Casual => {
            fill_rect(canvas, geo.torso_x, geo.torso_y + geo.torso_h / 2, geo.torso_w, 4, accent);
        }
        Outfit::Formal => {
            // Lapels and tie.
            fill_rect(canvas, CENTER_X - 9, geo.torso_y, 4, 22, accent);
            fill_rect(canvas, CENTER_X + 5, geo.torso_y, 4, 22, accent);
            fill_rect(canvas, CENTER_X - 1, geo.torso_y + 4, 3, 26, shade_rgb(accent, 1, 2));
        }
        Outfit::Athletic => {
            // Torso side stripes and arm bands.
            fill_rect(canvas, geo.torso_x, geo.torso_y, 2, geo.torso_h, accent);
            fill_rect(canvas, geo.torso_x + geo.torso_w - 2, geo.torso_y, 2, geo.torso_h, accent);
            let arm_left_x = geo.torso_x - 2 - geo.arm_w;
            let arm_right_x = geo.torso_x + geo.torso_w + 2;
            let band_y = geo.torso_y + geo.torso_h / 2;
            fill_rect(canvas, arm_left_x, band_y, geo.arm_w, 4, accent);
            fill_rect(canvas, arm_right_x, band_y, geo.arm_w, 4, accent);
        }
    }
}

/// Hair behind the head (only `long` has a back panel).
fn draw_hair_back(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    if cfg.hairstyle != Hairstyle::Long {
        return;
    }
    let y = geo.head_y - 2;
    let h = (geo.torso_y + 14) - y;
    fill_rect(canvas, geo.head_x - 4, y, geo.head_w + 8, h, cfg.hair_color);
}

/// Crown cap plus the per-id style. Each id has a distinct silhouette;
/// the unit tests assert the four pixel sets are pairwise different.
fn draw_hair_front(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    let hair = cfg.hair_color;
    // Crown cap.
    fill_rect(
        canvas,
        geo.head_x - 2,
        geo.head_y - 5,
        geo.head_w + 4,
        geo.head_h * 35 / 100 + 5,
        hair,
    );
    match cfg.hairstyle {
        Hairstyle::Bob => {
            let top = geo.head_y + geo.head_h * 15 / 100;
            let bottom = geo.head_y + geo.head_h * 75 / 100;
            fill_rect(canvas, geo.head_x - 5, top, 7, bottom - top, hair);
            fill_rect(canvas, geo.head_x + geo.head_w - 2, top, 7, bottom - top, hair);
        }
        Hairstyle::Long => {
            let top = geo.head_y + geo.head_h * 15 / 100;
            let bottom = geo.torso_y + 14;
            fill_rect(canvas, geo.head_x - 5, top, 7, bottom - top, hair);
            fill_rect(canvas, geo.head_x + geo.head_w - 2, top, 7, bottom - top, hair);
        }
        Hairstyle::Ponytail => {
            // Tail sweeps down-right from the crown; at most ~90 rows.
            let x0 = geo.head_x + geo.head_w - 4;
            let y0 = geo.head_y + 6;
            let len = geo.head_h + 42;
            for i in 0..len {
                let x = x0 + i * 6 / len;
                fill_rect(canvas, x, y0 + i, 9, 1, hair);
            }
            // Tie band at the tail root.
            fill_rect(canvas, x0 - 1, y0 - 2, 11, 4, cfg.accent);
        }
        Hairstyle::Braids => {
            let dark = shade_rgb(hair, 3, 4);
            let top = geo.head_y + geo.head_h * 20 / 100;
            let bottom = geo.head_y + geo.head_h + 34;
            let left_x = geo.head_x - 11;
            let right_x = geo.head_x + geo.head_w + 2;
            // Segmented 6 px bands alternate hair / darkened hair.
            let mut y = top;
            let mut band = 0;
            while y < bottom {
                let h = (bottom - y).min(6);
                let color = if band % 2 == 0 { hair } else { dark };
                fill_rect(canvas, left_x, y, 9, h, color);
                fill_rect(canvas, right_x, y, 9, h, color);
                y += 6;
                band += 1;
            }
        }
    }
}

/// Accessories toggled per flag, in the accent color.
fn draw_accessories(canvas: &mut Canvas, cfg: &CharacterConfig, geo: &BodyGeometry) {
    for accessory in &cfg.accessories {
        match accessory {
            Accessory::Visor => {
                fill_rect(
                    canvas,
                    geo.head_x + 2,
                    geo.eye_y - 3,
                    geo.head_w - 4,
                    geo.eye_h + 6,
                    cfg.accent,
                );
            }
            Accessory::Headphones => {
                fill_rect(canvas, geo.head_x - 2, geo.head_y - 7, geo.head_w + 4, 5, cfg.accent);
                fill_rect(canvas, geo.head_x - 8, geo.eye_y - 3, 7, 12, cfg.accent);
                fill_rect(canvas, geo.head_x + geo.head_w + 1, geo.eye_y - 3, 7, 12, cfg.accent);
            }
            Accessory::Hairclip => {
                fill_rect(canvas, geo.head_x + 5, geo.head_y + 1, 8, 5, brighten(cfg.accent));
            }
        }
    }
}

/// Render the 96x192 base character sprite. Pure: no I/O, no SDK contact.
pub fn render_base(cfg: &CharacterConfig) -> Canvas {
    let geo = body_geometry(cfg);
    let mut canvas = Canvas::new(CELL_W, CELL_H);
    draw_hair_back(&mut canvas, cfg, &geo);
    draw_body(&mut canvas, cfg, &geo);
    draw_head(&mut canvas, cfg, &geo);
    draw_face(&mut canvas, cfg, &geo);
    draw_hair_front(&mut canvas, cfg, &geo);
    draw_accessories(&mut canvas, cfg, &geo);
    canvas
}

/* ------------------------------------------------------------------ */
/* Sheet scaffold: 24 cells, base frame copied into every cell.        */
/* ------------------------------------------------------------------ */

/// Top-left pixel of a sheet cell. `row` in 0..6, `col` in 0..4.
pub fn sheet_cell_origin(row: i32, col: i32) -> (i32, i32) {
    (col * CELL_W, row * CELL_H)
}

/// Build the 384x1152 sheet: every one of the 24 cells gets an exact copy
/// of the base frame, the starting point for per-mood art.
pub fn render_sheet(base: &Canvas) -> Canvas {
    // Internal invariant: the base is always a 96x192 cell we rendered.
    assert!(
        base.w == CELL_W && base.h == CELL_H,
        "sheet base must be one 96x192 cell"
    );
    let mut sheet = Canvas::new(SHEET_W, SHEET_H);
    for row in 0..SHEET_ROWS {
        for col in 0..SHEET_COLS {
            let (ox, oy) = sheet_cell_origin(row, col);
            for y in 0..CELL_H {
                let src = y as usize * CELL_W as usize;
                let dst = (oy + y) as usize * SHEET_W as usize + ox as usize;
                sheet.px[dst..dst + CELL_W as usize]
                    .copy_from_slice(&base.px[src..src + CELL_W as usize]);
            }
        }
    }
    sheet
}

/* ------------------------------------------------------------------ */
/* Minimal PNG writer: 8-bit RGBA, zlib "stored" deflate blocks.       */
/* No dependencies; checksums covered by known-answer tests.           */
/* ------------------------------------------------------------------ */

/// CRC-32 (ISO 3309, polynomial 0xEDB88320), bitwise — no table needed.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let lsb = crc & 1;
            crc >>= 1;
            if lsb != 0 {
                crc ^= 0xEDB8_8320;
            }
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// Adler-32, chunked per the RFC 1950 NMAX bound.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    const NMAX: usize = 5552;
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Append one PNG chunk: length, tag, data, CRC over tag+data.
fn write_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(tag);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Encode `px` (0xAARRGGBB, row-major, `w * h` pixels) as a PNG.
/// Internal invariant: the buffer matches the dimensions by construction
/// (`Canvas` guarantees it).
pub fn png_encode(w: u32, h: u32, px: &[u32]) -> Vec<u8> {
    assert!(
        (w as usize) * (h as usize) == px.len(),
        "pixel buffer must match dimensions"
    );
    let mut out = Vec::new();
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&w.to_be_bytes());
    ihdr[4..8].copy_from_slice(&h.to_be_bytes());
    ihdr[8] = 8; // bit depth
    ihdr[9] = 6; // color type: RGBA
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Raw scanlines: filter byte 0 (None) + RGBA bytes.
    let stride = 1 + w as usize * 4;
    let mut raw = Vec::with_capacity(h as usize * stride);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let p = px[y * w as usize + x];
            raw.push((p >> 16) as u8);
            raw.push((p >> 8) as u8);
            raw.push(p as u8);
            raw.push((p >> 24) as u8);
        }
    }

    // zlib stream: header 0x78 0x01 (FCHECK-clean), then stored blocks
    // (BTYPE=00, max 65535 bytes each), then Adler-32 of the raw data.
    let mut idat = Vec::with_capacity(raw.len() + 16);
    idat.extend_from_slice(&[0x78, 0x01]);
    let block_count = raw.chunks(65535).len();
    for (index, block) in raw.chunks(65535).enumerate() {
        idat.push(if index + 1 == block_count { 1 } else { 0 });
        let len = block.len() as u16;
        idat.extend_from_slice(&len.to_le_bytes());
        idat.extend_from_slice(&(!len).to_le_bytes());
        idat.extend_from_slice(block);
    }
    idat.extend_from_slice(&adler32(&raw).to_be_bytes());
    write_chunk(&mut out, b"IDAT", &idat);

    write_chunk(&mut out, b"IEND", &[]);
    out
}

/* ------------------------------------------------------------------ */
/* JSON sidecar: names the mood rows for the pipeline.                 */
/* ------------------------------------------------------------------ */

/// Build the `<name>_sheet.json` sidecar. The character name is charset-
/// restricted to `[A-Za-z0-9 _-]`, so no JSON escaping is needed.
pub fn build_sidecar_json(cfg: &CharacterConfig, base_file: &str, sheet_file: &str) -> String {
    let mut moods = String::new();
    for (index, mood) in MOOD_NAMES.iter().enumerate() {
        if index > 0 {
            moods.push_str(", ");
        }
        moods.push('"');
        moods.push_str(mood);
        moods.push('"');
    }
    format!(
        "{{\n  \"character\": \"{name}\",\n  \"generator\": \"vs-character-creator 1.0.0\",\n  \"base\": \"{base}\",\n  \"sheet\": \"{sheet}\",\n  \"cell_w\": 96,\n  \"cell_h\": 192,\n  \"cols\": 4,\n  \"rows\": 6,\n  \"frame_ms\": {frame_ms},\n  \"moods\": [{moods}]\n}}\n",
        name = cfg.name,
        base = base_file,
        sheet = sheet_file,
        moods = moods,
        frame_ms = FRAME_MS,
    )
}

/* ------------------------------------------------------------------ */
/* Editor action: read config, render, write files, notify.            */
/* Never panics: every failure becomes an error notification.          */
/* ------------------------------------------------------------------ */

/// Every way the action can fail; each renders as an error notification.
enum ActionError {
    NoHome,
    ConfigMissing(String),
    ConfigTooLarge,
    ConfigNotUtf8,
    Config(ConfigError),
    Mkdir(String),
    Write(String),
}

impl ActionError {
    fn message(&self) -> String {
        match self {
            ActionError::NoHome => "HOME is not set; cannot locate the config file.".to_string(),
            ActionError::ConfigMissing(path) => format!(
                "Config file not found: {path}. Create it (see the plugin README)."
            ),
            ActionError::ConfigTooLarge => "Config file too large.".to_string(),
            ActionError::ConfigNotUtf8 => "Config file is not valid UTF-8.".to_string(),
            ActionError::Config(err) => err.message(),
            ActionError::Mkdir(detail) => format!("Cannot create output directory: {detail}"),
            ActionError::Write(detail) => format!("Cannot write output file: {detail}"),
        }
    }
}

/// Keep I/O error detail bounded for the notification text.
fn truncate_detail(detail: String) -> String {
    const LIMIT: usize = 200;
    if detail.len() > LIMIT {
        format!("{}...", &detail[..LIMIT])
    } else {
        detail
    }
}

/// Action body: config -> base -> sheet -> PNG files + JSON sidecar.
fn run_character_creator() -> Result<String, ActionError> {
    let home = std::env::var("HOME").map_err(|_| ActionError::NoHome)?;
    let cfg_path = format!("{home}/.config/voidsprite/{CONFIG_FILE_NAME}");
    let bytes =
        std::fs::read(&cfg_path).map_err(|_| ActionError::ConfigMissing(cfg_path.clone()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ActionError::ConfigTooLarge);
    }
    let text = String::from_utf8(bytes).map_err(|_| ActionError::ConfigNotUtf8)?;
    let cfg = parse_config(&text).map_err(ActionError::Config)?;

    let base = render_base(&cfg);
    let sheet = render_sheet(&base);

    let out_dir = format!("{home}/character-creator-sheets");
    std::fs::create_dir_all(&out_dir)
        .map_err(|err| ActionError::Mkdir(truncate_detail(err.to_string())))?;

    let base_file = format!("{}_base.png", cfg.name);
    let sheet_file = format!("{}_sheet.png", cfg.name);
    let json_file = format!("{}_sheet.json", cfg.name);
    let base_png = png_encode(CELL_W as u32, CELL_H as u32, &base.px);
    let sheet_png = png_encode(SHEET_W as u32, SHEET_H as u32, &sheet.px);
    let sidecar = build_sidecar_json(&cfg, &base_file, &sheet_file);

    std::fs::write(format!("{out_dir}/{base_file}"), &base_png)
        .map_err(|err| ActionError::Write(truncate_detail(err.to_string())))?;
    std::fs::write(format!("{out_dir}/{sheet_file}"), &sheet_png)
        .map_err(|err| ActionError::Write(truncate_detail(err.to_string())))?;
    std::fs::write(format!("{out_dir}/{json_file}"), sidecar)
        .map_err(|err| ActionError::Write(truncate_detail(err.to_string())))?;

    Ok(format!(
        "Wrote {base_file}, {sheet_file}, {json_file} to {out_dir}/"
    ))
}

/// The registered editor action. The session is not modified — output
/// goes to files — so the editor handle is intentionally unused.
extern "C" fn character_creator_action(editor: *mut VspEditorContext) {
    let _ = editor;
    match run_character_creator() {
        Ok(summary) => notify_success("Character creator", &summary),
        Err(err) => notify_error("Character creator", &err.message()),
    }
}

/* ------------------------------------------------------------------ */
/* Tests.                                                              */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SAMPLE_CFG_TEXT: &str = "\
# Nova - sample character config
name=Nova
body_type=female
hairstyle=ponytail
hair_color=#3b2a20
face=oval
eye_color=#2a7de1
weight=average
bust_size=medium
height=average
outfit=tech-jacket
outfit_color=#23272e
accent=#e14a7d
accessories=visor,hairclip
";

    fn sample_config() -> CharacterConfig {
        CharacterConfig {
            name: "Nova".to_string(),
            body_type: BodyType::Female,
            hairstyle: Hairstyle::Ponytail,
            hair_color: 0xFF3B_2A20,
            face: Face::Oval,
            eye_color: 0xFF2A_7DE1,
            weight: Weight::Average,
            bust: Some(BustSize::Medium),
            chest: None,
            height: Height::Average,
            outfit: Outfit::TechJacket,
            outfit_color: 0xFF23_272E,
            accent: 0xFFE1_4A7D,
            accessories: vec![Accessory::Visor, Accessory::Hairclip],
        }
    }

    fn male_config() -> CharacterConfig {
        let mut cfg = sample_config();
        cfg.body_type = BodyType::Male;
        cfg.bust = None;
        cfg.chest = Some(ChestSize::Average);
        cfg
    }

    fn with_key(text: &str, key: &str, value: &str) -> String {
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let name = line.split('=').next().unwrap_or("");
            if name == key {
                out.push_str(key);
                out.push('=');
                out.push_str(value);
                out.push('\n');
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    fn without_key(text: &str, key: &str) -> String {
        text.lines()
            .filter(|line| line.split('=').next().unwrap_or("") != key)
            .map(|line| format!("{line}\n"))
            .collect()
    }

    /* Config: happy path and line discipline. */

    #[test]
    fn config_parses_full_example() {
        let cfg = parse_config(SAMPLE_CFG_TEXT).expect("sample config must parse");
        assert_eq!(cfg.name, "Nova");
        assert_eq!(cfg.body_type, BodyType::Female);
        assert_eq!(cfg.hairstyle, Hairstyle::Ponytail);
        assert_eq!(cfg.hair_color, 0xFF3B_2A20);
        assert_eq!(cfg.face, Face::Oval);
        assert_eq!(cfg.eye_color, 0xFF2A_7DE1);
        assert_eq!(cfg.weight, Weight::Average);
        assert_eq!(cfg.bust, Some(BustSize::Medium));
        assert_eq!(cfg.chest, None);
        assert_eq!(cfg.height, Height::Average);
        assert_eq!(cfg.outfit, Outfit::TechJacket);
        assert_eq!(cfg.outfit_color, 0xFF23_272E);
        assert_eq!(cfg.accent, 0xFFE1_4A7D);
        assert_eq!(cfg.accessories, vec![Accessory::Visor, Accessory::Hairclip]);
    }

    #[test]
    fn config_accepts_empty_accessories_as_none() {
        let text = with_key(SAMPLE_CFG_TEXT, "accessories", "");
        let cfg = parse_config(&text).expect("empty accessories must parse");
        assert!(cfg.accessories.is_empty());
    }

    #[test]
    fn config_skips_comments_and_blank_lines() {
        let text = format!("# leading comment\n\n{SAMPLE_CFG_TEXT}\n   \n# trailing\n");
        assert!(parse_config(&text).is_ok());
    }

    #[test]
    fn config_rejects_unknown_key() {
        let text = format!("{SAMPLE_CFG_TEXT}frobnicate=yes\n");
        match parse_config(&text) {
            Err(ConfigError::UnknownKey { line, key }) => {
                assert_eq!(key, "frobnicate");
                assert!(line > 0);
            }
            other => panic!("expected UnknownKey, got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_duplicate_key() {
        let text = format!("{SAMPLE_CFG_TEXT}weight=slim\n");
        match parse_config(&text) {
            Err(ConfigError::DuplicateKey { key, .. }) => assert_eq!(key, "weight"),
            other => panic!("expected DuplicateKey, got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_missing_required_key() {
        let text = without_key(SAMPLE_CFG_TEXT, "eye_color");
        match parse_config(&text) {
            Err(ConfigError::MissingKey(key)) => assert_eq!(key, "eye_color"),
            other => panic!("expected MissingKey, got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_line_without_equals() {
        let text = format!("{SAMPLE_CFG_TEXT}this line has no equals\n");
        assert!(matches!(parse_config(&text), Err(ConfigError::MissingEquals(_))));
    }

    #[test]
    fn config_rejects_line_too_long() {
        let long = "x".repeat(MAX_CONFIG_LINE_LEN + 1);
        let text = format!("{SAMPLE_CFG_TEXT}name={long}\n");
        assert!(matches!(parse_config(&text), Err(ConfigError::LineTooLong(_))));
    }

    #[test]
    fn config_rejects_too_many_lines() {
        let mut text = String::from(SAMPLE_CFG_TEXT);
        for _ in 0..=MAX_CONFIG_LINES {
            text.push_str("# filler\n");
        }
        assert!(matches!(parse_config(&text), Err(ConfigError::TooManyLines)));
    }

    #[test]
    fn config_rejects_oversize_file() {
        let text = "x".repeat(MAX_CONFIG_BYTES + 1);
        assert!(matches!(parse_config(&text), Err(ConfigError::TooLarge)));
    }

    /* Config: malformed values. */

    #[test]
    fn config_rejects_malformed_hex_colors() {
        for bad in [
            "red",
            "#fff",
            "#ff00000",
            "ff0000",
            "#gggggg",
            "#12345 ",
            "#12 345",
            "",
        ] {
            let text = with_key(SAMPLE_CFG_TEXT, "hair_color", bad);
            assert!(
                parse_config(&text).is_err(),
                "hair_color={bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn config_accepts_uppercase_hex() {
        let text = with_key(SAMPLE_CFG_TEXT, "hair_color", "#3B2A20");
        let cfg = parse_config(&text).expect("uppercase hex must parse");
        assert_eq!(cfg.hair_color, 0xFF3B_2A20);
    }

    #[test]
    fn config_rejects_unknown_enum_values() {
        let cases = [
            ("body_type", "other"),
            ("hairstyle", "mohawk"),
            ("face", "triangle"),
            ("weight", "chonky"),
            ("bust_size", "huge"),
            ("chest_size", "tiny"),
            ("height", "gigantic"),
            ("outfit", "spacesuit"),
        ];
        for (key, value) in cases {
            // `chest_size` is absent from the female sample text: append it.
            let text = if SAMPLE_CFG_TEXT.contains(&format!("{key}=")) {
                with_key(SAMPLE_CFG_TEXT, key, value)
            } else {
                format!("{SAMPLE_CFG_TEXT}{key}={value}\n")
            };
            assert!(
                parse_config(&text).is_err(),
                "{key}={value} must be rejected"
            );
        }
    }

    #[test]
    fn config_rejects_unknown_accessory() {
        let text = with_key(SAMPLE_CFG_TEXT, "accessories", "visor,jetpack");
        assert!(matches!(
            parse_config(&text),
            Err(ConfigError::BadValue { key: "accessories", .. })
        ));
    }

    #[test]
    fn config_rejects_duplicate_accessory() {
        let text = with_key(SAMPLE_CFG_TEXT, "accessories", "visor,visor");
        assert!(parse_config(&text).is_err());
    }

    #[test]
    fn config_rejects_bad_names() {
        for bad in ["", "a/b", "a\\b", "a.b", "evil..name", &"n".repeat(49)] {
            let text = with_key(SAMPLE_CFG_TEXT, "name", bad);
            assert!(parse_config(&text).is_err(), "name={bad:?} must be rejected");
        }
    }

    /* Config: adversarial cross-key validation. */

    #[test]
    fn config_rejects_male_with_bust_size() {
        let text = with_key(SAMPLE_CFG_TEXT, "body_type", "male");
        let text = format!("{text}chest_size=average\n");
        // bust_size still present from the sample -> must fail loudly.
        assert!(matches!(
            parse_config(&text),
            Err(ConfigError::BustWithMale(_))
        ));
    }

    #[test]
    fn config_rejects_female_with_chest_size() {
        let text = format!("{SAMPLE_CFG_TEXT}chest_size=flat\n");
        assert!(matches!(
            parse_config(&text),
            Err(ConfigError::ChestWithFemale(_))
        ));
    }

    #[test]
    fn config_rejects_male_missing_chest_size() {
        let text = with_key(SAMPLE_CFG_TEXT, "body_type", "male");
        let text = without_key(&text, "bust_size");
        match parse_config(&text) {
            Err(ConfigError::MissingKey(key)) => assert_eq!(key, "chest_size"),
            other => panic!("expected MissingKey(chest_size), got {other:?}"),
        }
    }

    #[test]
    fn config_rejects_female_missing_bust_size() {
        let text = without_key(SAMPLE_CFG_TEXT, "bust_size");
        match parse_config(&text) {
            Err(ConfigError::MissingKey(key)) => assert_eq!(key, "bust_size"),
            other => panic!("expected MissingKey(bust_size), got {other:?}"),
        }
    }

    #[test]
    fn config_accepts_valid_male_config() {
        let text = with_key(SAMPLE_CFG_TEXT, "body_type", "male");
        let text = without_key(&text, "bust_size");
        let text = format!("{text}chest_size=muscular\n");
        let cfg = parse_config(&text).expect("valid male config must parse");
        assert_eq!(cfg.body_type, BodyType::Male);
        assert_eq!(cfg.chest, Some(ChestSize::Muscular));
        assert_eq!(cfg.bust, None);
    }

    /* Geometry: scale steps change pixel dimensions. */

    #[test]
    fn geometry_weight_changes_torso_width() {
        let (mut slim, mut average, mut heavy) = (sample_config(), sample_config(), sample_config());
        slim.weight = Weight::Slim;
        average.weight = Weight::Average;
        heavy.weight = Weight::Heavy;
        let (gs, ga, gh) = (body_geometry(&slim), body_geometry(&average), body_geometry(&heavy));
        assert_eq!((gs.torso_w, ga.torso_w, gh.torso_w), (28, 36, 48));
        assert!(gs.arm_w < ga.arm_w && ga.arm_w < gh.arm_w);
        assert!(gs.leg_w < ga.leg_w && ga.leg_w < gh.leg_w);
    }

    #[test]
    fn geometry_height_sets_head_proportions() {
        let (mut short, mut average, mut tall) = (sample_config(), sample_config(), sample_config());
        short.height = Height::Short;
        average.height = Height::Average;
        tall.height = Height::Tall;
        // Round face: no box adjustment, so the raw height steps are asserted.
        short.face = Face::Round;
        average.face = Face::Round;
        tall.face = Face::Round;
        let (gs, ga, gt) = (body_geometry(&short), body_geometry(&average), body_geometry(&tall));
        // 192/48 = 1:4 chibi, 192/35 ~= 1:5.5, 192/27 ~= 1:7.
        assert_eq!((gs.head_h, ga.head_h, gt.head_h), (48, 35, 27));
        assert!(gs.head_w > ga.head_w && ga.head_w > gt.head_w);
    }

    #[test]
    fn geometry_bust_steps_widen_zone() {
        let (mut small, mut medium, mut large) = (sample_config(), sample_config(), sample_config());
        small.bust = Some(BustSize::Small);
        medium.bust = Some(BustSize::Medium);
        large.bust = Some(BustSize::Large);
        let (gs, gm, gl) = (body_geometry(&small), body_geometry(&medium), body_geometry(&large));
        // Average weight: torso half-width 18, deltas +0/+3/+6 per side.
        assert_eq!((gs.zone_half_w, gm.zone_half_w, gl.zone_half_w), (18, 21, 24));
    }

    #[test]
    fn geometry_chest_steps_widen_zone() {
        let (mut flat, mut average, mut muscular) = (male_config(), male_config(), male_config());
        flat.chest = Some(ChestSize::Flat);
        average.chest = Some(ChestSize::Average);
        muscular.chest = Some(ChestSize::Muscular);
        let (gf, ga, gm) = (body_geometry(&flat), body_geometry(&average), body_geometry(&muscular));
        // Average weight: torso half-width 18, deltas -2/+0/+4 per side.
        assert_eq!((gf.zone_half_w, ga.zone_half_w, gm.zone_half_w), (16, 18, 22));
        assert!(!gf.shoulder_bar && !ga.shoulder_bar && gm.shoulder_bar);
    }

    /* Art: palette application and hairstyle signatures. */

    #[test]
    fn palette_colors_land_on_expected_pixels() {
        let mut cfg = sample_config();
        // No visor: it would cover the eyes by design.
        cfg.accessories = vec![Accessory::Hairclip];
        let geo = body_geometry(&cfg);
        let base = render_base(&cfg);
        assert_eq!(base.w, CELL_W);
        assert_eq!(base.h, CELL_H);
        // Torso carries the outfit color (off-center: the tech-jacket
        // zipper runs down the middle in the accent color).
        assert_eq!(
            base.get(CENTER_X + 8, geo.torso_y + geo.torso_h / 2),
            cfg.outfit_color
        );
        // Eyes carry the eye color.
        assert_eq!(base.get(geo.eye_x0 + 1, geo.eye_y + 1), cfg.eye_color);
        assert_eq!(base.get(geo.eye_x1 + 1, geo.eye_y + 1), cfg.eye_color);
        // Crown cap carries the hair color.
        assert_eq!(
            base.get(geo.head_x + geo.head_w / 2, geo.head_y - 2),
            cfg.hair_color
        );
    }

    /// Pixels drawn by the hair pass only, for one hairstyle id.
    fn hair_pixels(style: Hairstyle) -> BTreeSet<(i32, i32)> {
        let mut cfg = sample_config();
        cfg.hairstyle = style;
        let geo = body_geometry(&cfg);
        let mut canvas = Canvas::new(CELL_W, CELL_H);
        draw_hair_back(&mut canvas, &cfg, &geo);
        draw_hair_front(&mut canvas, &cfg, &geo);
        canvas
            .px
            .iter()
            .enumerate()
            .filter(|&(_, &pixel)| pixel != TRANSPARENT)
            .map(|(index, _)| ((index % CELL_W as usize) as i32, (index / CELL_W as usize) as i32))
            .collect()
    }

    #[test]
    fn hairstyles_produce_distinct_pixel_sets() {
        let styles = [Hairstyle::Ponytail, Hairstyle::Braids, Hairstyle::Bob, Hairstyle::Long];
        let sets: Vec<BTreeSet<(i32, i32)>> = styles.iter().map(|s| hair_pixels(*s)).collect();
        for set in &sets {
            assert!(!set.is_empty(), "every hairstyle must draw hair");
        }
        for i in 0..sets.len() {
            for j in (i + 1)..sets.len() {
                assert_ne!(sets[i], sets[j], "hairstyles {i} and {j} must differ");
            }
        }
    }

    #[test]
    fn hairstyle_silhouettes_match_their_ids() {
        let cfg = sample_config();
        let geo = body_geometry(&cfg);
        let right_of_head = geo.head_x + geo.head_w;
        let below_chin = geo.head_y + geo.head_h + 10;

        let ponytail = hair_pixels(Hairstyle::Ponytail);
        assert!(
            ponytail.iter().any(|&(x, _)| x > right_of_head),
            "ponytail tail must sweep right of the head"
        );
        let braids = hair_pixels(Hairstyle::Braids);
        assert!(
            braids.iter().any(|&(x, _)| x < geo.head_x - 6),
            "braids must hang left of the head"
        );
        assert!(
            braids.iter().any(|&(x, _)| x > right_of_head + 6),
            "braids must hang right of the head"
        );
        let bob = hair_pixels(Hairstyle::Bob);
        assert!(
            bob.iter().all(|&(_, y)| y < geo.head_y + geo.head_h),
            "bob must stay above the chin"
        );
        let long = hair_pixels(Hairstyle::Long);
        assert!(
            long.iter().any(|&(_, y)| y > below_chin),
            "long hair must fall well below the chin"
        );
    }

    /* Sheet scaffold geometry. */

    #[test]
    fn sheet_cell_origin_math() {
        assert_eq!(sheet_cell_origin(0, 0), (0, 0));
        assert_eq!(sheet_cell_origin(2, 4 - 2), (192, 384));
        // Row 5, frame 3: the alarmed row's last cell.
        assert_eq!(sheet_cell_origin(5, 3), (288, 960));
    }

    #[test]
    fn sheet_has_24_cells_matching_base() {
        let base = render_base(&sample_config());
        let sheet = render_sheet(&base);
        assert_eq!((sheet.w, sheet.h), (384, 1152));
        for row in 0..SHEET_ROWS {
            for col in 0..SHEET_COLS {
                let (ox, oy) = sheet_cell_origin(row, col);
                for y in 0..CELL_H {
                    let src = y as usize * CELL_W as usize;
                    let dst = (oy + y) as usize * SHEET_W as usize + ox as usize;
                    assert_eq!(
                        sheet.px[dst..dst + CELL_W as usize],
                        base.px[src..src + CELL_W as usize],
                        "cell ({row}, {col}) row {y} must equal the base frame"
                    );
                }
            }
        }
    }

    /* PNG writer and checksums. */

    #[test]
    fn crc32_matches_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_known_vector() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Minimal stored-deflate decoder: just enough to validate our own
    /// encoder output (zlib header, stored blocks, Adler-32).
    fn decode_stored_zlib(stream: &[u8], raw_len: usize) -> Vec<u8> {
        assert_eq!(&stream[0..2], &[0x78, 0x01], "zlib header");
        let mut out = Vec::new();
        let mut pos = 2;
        loop {
            let header = stream[pos];
            pos += 1;
            assert_eq!(header & 0x06, 0, "BTYPE must be 00 (stored)");
            let len = u16::from_le_bytes([stream[pos], stream[pos + 1]]) as usize;
            let nlen = u16::from_le_bytes([stream[pos + 2], stream[pos + 3]]);
            pos += 4;
            assert_eq!(nlen, !len as u16, "NLEN must complement LEN");
            out.extend_from_slice(&stream[pos..pos + len]);
            pos += len;
            if header & 0x01 != 0 {
                break;
            }
        }
        assert_eq!(out.len(), raw_len);
        let adler = u32::from_be_bytes([stream[pos], stream[pos + 1], stream[pos + 2], stream[pos + 3]]);
        assert_eq!(adler, adler32(&out), "Adler-32 must match");
        out
    }

    fn idat_payload(png: &[u8]) -> Vec<u8> {
        let mut pos = 8;
        let mut idat = Vec::new();
        loop {
            let len = u32::from_be_bytes([png[pos], png[pos + 1], png[pos + 2], png[pos + 3]]) as usize;
            let tag = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            if tag == b"IDAT" {
                idat.extend_from_slice(data);
            }
            // CRC covers tag+data; verify it while walking.
            let mut crc_input = Vec::with_capacity(4 + len);
            crc_input.extend_from_slice(tag);
            crc_input.extend_from_slice(data);
            let crc = u32::from_be_bytes([
                png[pos + 8 + len],
                png[pos + 9 + len],
                png[pos + 10 + len],
                png[pos + 11 + len],
            ]);
            assert_eq!(crc, crc32(&crc_input), "chunk CRC must match");
            pos += 12 + len;
            if tag == b"IEND" {
                break;
            }
        }
        idat
    }

    #[test]
    fn png_round_trips_through_stored_deflate() {
        let px = [0xFFFF_0000u32, 0xFF00_FF00, 0xFF00_00FF, 0x0000_0000];
        let png = png_encode(2, 2, &px);
        assert_eq!(&png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10], "PNG magic");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes([png[16], png[17], png[18], png[19]]), 2);
        assert_eq!(u32::from_be_bytes([png[20], png[21], png[22], png[23]]), 2);
        // Last chunk is IEND with empty data.
        let n = png.len();
        assert_eq!(&png[n - 12..n - 8], &0u32.to_be_bytes());
        assert_eq!(&png[n - 8..n - 4], b"IEND");

        let idat = idat_payload(&png);
        // 2 rows of (filter byte + 2 px * 4 bytes).
        let raw = decode_stored_zlib(&idat, 2 * 9);
        assert_eq!(raw[0], 0, "filter byte None");
        assert_eq!(&raw[1..5], &[0xFF, 0x00, 0x00, 0xFF], "red px as RGBA");
        assert_eq!(&raw[5..9], &[0x00, 0xFF, 0x00, 0xFF], "green px as RGBA");
        assert_eq!(raw[9], 0, "filter byte None");
        assert_eq!(&raw[10..14], &[0x00, 0x00, 0xFF, 0xFF], "blue px as RGBA");
        assert_eq!(&raw[14..18], &[0x00, 0x00, 0x00, 0x00], "transparent px as RGBA");
    }

    /* Sidecar JSON. */

    #[test]
    fn sidecar_names_moods_and_contract() {
        let json = build_sidecar_json(&sample_config(), "Nova_base.png", "Nova_sheet.png");
        assert!(json.contains("\"character\": \"Nova\""));
        assert!(json.contains("\"cell_w\": 96"));
        assert!(json.contains("\"cell_h\": 192"));
        assert!(json.contains("\"frame_ms\": 180"));
        assert!(json.contains(
            "\"moods\": [\"idle\", \"blush\", \"wink\", \"pout\", \"celebrate\", \"alarmed\"]"
        ));
    }

    /* End-to-end: the real action entry with a scratch HOME. */

    #[test]
    fn action_writes_png_files_end_to_end() {
        use std::ffi::OsString;
        // Safety: no other test reads or writes HOME; tests run in one
        // process, so this is scoped to this test's execution.
        let scratch: std::path::PathBuf = std::env::temp_dir().join(format!(
            "vscc-test-{}",
            std::process::id()
        ));
        let cfg_dir = scratch.join(".config/voidsprite");
        std::fs::create_dir_all(&cfg_dir).expect("create scratch config dir");
        std::fs::write(cfg_dir.join(CONFIG_FILE_NAME), SAMPLE_CFG_TEXT)
            .expect("write scratch config");

        let old_home: Option<OsString> = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &scratch) };

        // Call the real extern "C" entry with a null editor (the action
        // never touches the session; notifications no-op without a host).
        character_creator_action(core::ptr::null_mut());

        if let Some(home) = old_home {
            unsafe { std::env::set_var("HOME", home) };
        } else {
            unsafe { std::env::remove_var("HOME") };
        }

        let out_dir = scratch.join("character-creator-sheets");
        for file in ["Nova_base.png", "Nova_sheet.png", "Nova_sheet.json"] {
            let path = out_dir.join(file);
            assert!(path.is_file(), "{file} must exist");
        }
        let base_png = std::fs::read(out_dir.join("Nova_base.png")).expect("read base png");
        assert_eq!(&base_png[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
        assert_eq!(u32::from_be_bytes([base_png[16], base_png[17], base_png[18], base_png[19]]), 96);
        assert_eq!(u32::from_be_bytes([base_png[20], base_png[21], base_png[22], base_png[23]]), 192);
        let sheet_png = std::fs::read(out_dir.join("Nova_sheet.png")).expect("read sheet png");
        assert_eq!(u32::from_be_bytes([sheet_png[16], sheet_png[17], sheet_png[18], sheet_png[19]]), 384);
        assert_eq!(u32::from_be_bytes([sheet_png[20], sheet_png[21], sheet_png[22], sheet_png[23]]), 1152);
        let sidecar = std::fs::read_to_string(out_dir.join("Nova_sheet.json")).expect("read json");
        assert!(sidecar.contains("\"moods\""));

        std::fs::remove_dir_all(&scratch).expect("clean scratch dir");
    }
}
