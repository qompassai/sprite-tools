---
name: "voidsprite-plugin-dev"
description: "Build native VoidSprite plugins (.so loaded via dlopen) in Matt's Tiger Style: the six C-ABI exports, repr(C) struct transcription with header-drift assertions, FFI boundary discipline (no panics/exceptions crossing into the host), filter vs editor-action parameter story, and the Xvfb load-test gate. See references/sdk-layout.md for the verified SDK struct layout and references/languages.md for the language choice guide."
---

# VoidSprite Plugin Development

> **Prerequisite:** `tiger-style-rust` (sibling skill at
> `~/workspace/skills/tiger-style-rust/SKILL.md`) — Rust is the recommended
> plugin language; apply it to everything this skill produces, and its unsafe
> policy governs every FFI boundary. For C/C++ plugins, apply the sibling
> `tiger-style-c` / `tiger-style-cpp` skills instead.

## Purpose

VoidSprite has no scripting layer: plugins are native shared libraries
(`.so` on Linux) that the host loads with `dlopen` and wires up through a
handshake of exactly six C-ABI exports. The host then fills a
function-pointer table — the `voidspriteSDK` struct — and passes it to your
`pluginInit`, which registers filters, brushes, editor actions, and file
importers/exporters as callbacks. That shape drives everything here: your
code runs *inside* the host process, called on the host's threads, with the
host's lifetime rules. Safety order, always: **Safety > Performance >
Developer Experience.** Every SDK member named in this file was verified
against the vendored SDK header transcription at
`/home/phaedrus/vs-lightshow/voidsprite_sdk_c.h` (VoidSprite SDK v1, a
mechanical C transcription of `voidsprite_sdk.h` from
[counter185/voidsprite](https://github.com/counter185/voidsprite/tree/main/voidsprite_plugin_sample)).
The sample `dllmain.cpp` in that repo's `voidsprite_plugin_sample/`
directory is the canonical usage reference. The full transcribed layout is
in `references/sdk-layout.md`; the language-choice guide is in
`references/languages.md`.

Plain-language version: you compile your code into a `.so` file, VoidSprite
opens it like a toolbox, reads six name tags to learn what it is, hands it
a control panel (a struct full of function pointers), and your setup code
plugs new tools into that panel. After that, the host calls *your*
functions — never the other way around — so your code must behave like a
guest: no crashing the host, no leaking its memory, no assuming it runs on
your terms.

## Workflow

1. Pick the language (`references/languages.md`) — Rust by default.
2. Transcribe or import the SDK struct. Prefer `@cImport`-style direct
   header reads (Zig) or a hand transcription with compile-time
   `size_of`/`offset_of` assertions (Rust) — never eyeball a 40-function
   struct and hope the offsets line up.
3. Implement the six exports with exact C-ABI signatures. In `pluginInit`,
   stash the SDK pointer and register your tools — nothing else. Keep
   `pluginInit` fast and infallible; a crash or hang here takes the whole
   host down.
4. Write each tool callback against the host's calling contract: check
   every nullable return, free exactly what the docs say you own, never
   touch what you don't.
5. Harden the FFI boundary: no panics/exceptions escaping, unsafe confined
   to thin documented wrappers, no host-thread assumptions beyond the
   documented ones.
6. Decide the parameter story up front: **filters** get host-generated
   parameter dialogs via `filterNew*Parameter` (see the filter/action
   section below); **editor actions** get no parameter UI at all — their
   inputs live in config files.
7. Gate with the Xvfb load test: run VoidSprite under Xvfb with a
   distinctly-named test build, confirm it loads, exit cleanly on SIGTERM,
   then remove the test build.

## Operating Rules

### Loading and the six exports

- The host `dlopen`s your `.so` and `dlsym`s exactly six symbols. They are
  the plugin's entire public surface; everything else in the library is
  invisible to the host. Exact signatures (verified in the SDK header):

  | Symbol | Signature | Meaning |
  |---|---|---|
  | `voidspriteSDKVersion` | `int voidspriteSDKVersion()` | Returns `VS_SDK_VERSION` (currently `1`). The host uses it to detect SDK drift. |
  | `pluginInit` | `void pluginInit(voidspriteSDK*)` | Receives the filled SDK table. Stash the pointer, register tools, return. |
  | `getPluginName` | `const char* getPluginName()` | Display name, e.g. `"voidsprite sample plugin"`. |
  | `getPluginVersion` | `const char* getPluginVersion()` | e.g. `"v1.0"`. |
  | `getPluginDescription` | `const char* getPluginDescription()` | One-line description. |
  | `getPluginAuthors` | `const char* getPluginAuthors()` | Author string. |

- The returned `const char*` values must point at memory that stays valid
  for the library's lifetime — string literals or `static` storage, never a
  freed buffer. The sample returns literals.
- Export visibility matters: on GCC/Clang, mark the six exports default
  visibility (the SDK header's `EXPORT` macro does exactly this); on MSVC,
  `__declspec(dllexport)`. A symbol hidden by `-fvisibility=hidden`
  without an explicit export attribute is a silent load failure — the host
  finds nothing to `dlsym`.
- Rust: `#[no_mangle] pub extern "C" fn pluginInit(sdk: *mut voidspriteSDK)`.
  cdylib crate type. Set `panic = "abort"` in the cdylib profile, or wrap
  every export in `catch_unwind` — a panic crossing into C++ host code is
  undefined behavior (tiger-style-rust forbids unwinding across FFI
  boundaries outright).
- C++: never let an exception cross `pluginInit` or any registered
  callback. Wrap bodies in `try`/`catch (...)` and report the failure
  through `vspPostErrorNotification`; an uncaught exception propagating
  into the host is undefined behavior.
- Keep `pluginInit` short: register, return. No network, no blocking I/O,
  no spawning threads that outlive the call without a shutdown contract.

### The SDK struct and header drift

- `voidspriteSDK` is a packed function-pointer table (~40 members in SDK
  v1). Opaque handles (`VSPLayer`, `VSPFilter`, `VSPFileExporter`,
  `VSPEditorContext`, `VSPBrush`) are forward-declared — the plugin never
  dereferences them, only passes them back to SDK functions.
- The vendored transcription (`voidsprite_sdk_c.h`) carries
  `#pragma pack(push, 1)`. On x86-64 this currently changes nothing (every
  struct field is an 8-byte pointer; `VSPLayerInfo` is three contiguous
  `int32_t`), but it is exactly the kind of silent layout detail that a
  new SDK release can change. **The mitigation is mechanical:**
  - In Rust, `#[repr(C)]` on the transcription, plus compile-time asserts:
    `assert!(size_of::<voidspriteSDK>() == N)` and `offset_of!` checks on
    several members, where `N` comes from compiling the real C header.
  - Regenerate the transcription from the new header with a scripted pass
    (`gen_c_header.sh` in the vendored tree), `diff` the result, and let
    the size/offset asserts fail loudly if the layout moved.
  - Never trust a version bump: `voidspriteSDKVersion()` tells the host
    the SDK generation your plugin was built against; the host decides
    compatibility, but your asserts catch the mismatch at build time.
- Zig sidesteps the transcription entirely with `@cImport` (see
  `references/languages.md`).

### FFI boundary discipline

- **No panics, no exceptions, no fatal errors cross into the host.** Ever.
  Rust: `panic = "abort"` or `catch_unwind` at every `extern "C"`
  boundary and every registered callback. C++: `catch (...)` at the same
  points. A callback that can fail reports through
  `vspPostErrorNotification` and returns normally.
- **Unsafe stays thin.** Confine `unsafe` to small wrappers around the
  raw function pointers; each wrapper documents its safety obligations
  (non-null SDK pointer, valid handle, in-bounds coordinates, correct
  pixel format). Default to `#![forbid(unsafe_code)]` at the crate root and
  allow it only in the boundary module. Adversarial tests target the safe
  wrappers, not the raw pointers.
- **No host-thread assumptions.** The SDK documents exactly three calls
  as thread-safe: `vspPostNotification`, `vspPostSuccessNotification`,
  and `vspPostErrorNotification` ("It's safe to call this function from a
  thread"). Assume everything else runs on the host's calling thread, and
  never call SDK functions from a plugin-spawned thread except the
  notification trio. If a callback needs background work, do the compute
  on your thread and marshal results back for the host thread to apply —
  with a documented shutdown contract so a host exit never strands a
  worker.
- **Ownership is the host's contract, not yours.** Verified rules from the
  header docs:
  - `layerGetInfo` allocates — free the returned pointer with `util_free`.
    `layerGetInfo(NULL)` returns NULL; check it.
  - `layerGetRawPixelData` is a borrowed view: do not free it, do not read
    past `width * height * 4` bytes. NULL layer → NULL.
  - `editorFlattenImage` / `editorFlattenFrame` allocate — free with
    `layerFree` when done.
  - `vspGetLocalizedString` returns a borrowed, read-only pointer — do
    not modify or free it; missing keys return `"--NO KEY"`.
  - `editorUndoPushLayerState` before modifying a layer's pixels outside
    brush and filter code — without it there is no undo for your edit.
  - Pixel format: RGBA layers are `0xAARRGGBB`; indexed layers hold a
    palette index or `-1` for transparent. `layerGetPixel` returns `0`
    on NULL/out-of-bounds; `layerSetPixel` is a no-op there.

### Filter vs editor-action parameters (the dialog asymmetry)

This is the single most consequential SDK design fact for UX:

- **Filters** get host-generated parameter dialogs. `registerFilter`
  returns a `VSPFilter*`; calls to `filterNewBoolParameter`,
  `filterNewIntParameter`, `filterNewDoubleParameter`, and
  `filterNewDoubleRangeParameter` declare typed parameters with defaults
  and ranges, and the host builds the dialog. The filter body reads values
  with `filterGetBoolValue` / `filterGetIntValue` / `filterGetDoubleValue`
  / `filterGetRangeValue1` / `filterGetRangeValue2`. The sample states the
  rule plainly: *if a filter has no parameters, it will execute instantly
  after choosing it.* Parameters are the interactive path for filters.
- **Editor actions** get no parameter UI — none is possible.
  `registerEditorAction(name, action)` takes a name and a
  `void (*)(VSPEditorContext*)` callback; there is no parameter-declaration
  API on the action path. An action that needs settings must read them
  from a config file (TOML/JSON in XDG config or beside the plugin),
  loaded at `pluginInit` or lazily on first invocation. Changing a value
  means editing the file and restarting VoidSprite (or adding an explicit
  "reload config" action). Document the file's location, format, and every
  default in the plugin's docs.
- Design consequence: when the same tool exists in both Aseprite (which
  has a real interactive `Dialog` API) and VoidSprite, the Aseprite side
  can ask the user at runtime while the VoidSprite side must be
  config-driven. See `new_character.lua` in the aseprite-scripting skill's
  `references/sheet-pipeline.md` for the worked example of that
  asymmetry: the Aseprite script uses the Dialog API; the
  `vs-character-creator` VoidSprite plugin reads a config file, because the
  SDK offers parameter dialogs to filters but not to editor actions.

### The Xvfb load-test gate

A plugin is not done until it loads in a real host process and dies
cleanly. The gate:

1. Build the plugin under a **distinct filename** (e.g.
   `myplugin_test.so`) — never overwrite or replace a user's installed
   plugin with a test build.
2. Point VoidSprite at the test build (plugin directory / env override
   per the host's install docs) and launch under Xvfb:
   `xvfb-run -a voidsprite …` (bounded timeout, captured stdout/stderr).
3. Assert: the host's output shows the plugin's display name (proof the
   six exports resolved and `pluginInit` ran); exercise each registered
   tool at least once if the host exposes a scriptable path, otherwise
   confirm registration lines only.
4. Send SIGTERM. Assert: clean exit — no hang past the timeout, no crash
   in teardown. A plugin that blocks host shutdown fails the gate.
5. Remove the test `.so` afterwards. A test artifact left in the plugin
   directory will shadow or duplicate the real plugin on the next launch.

### Paths and encodings

- Paths crossing the SDK are UTF-8 (`util_fopenUTF8`, importer/exporter
  paths). Use the SDK's `util_fopenUTF8` for file I/O on plugin-owned
  files rather than raw `fopen` when non-ASCII paths are possible.

## Activation

<skill_resources>
</skill_resources>

- **Dedup:** the harness tracks activated skills per session. If this skill
  is already in context, skip re-injection — never load it twice.
- **Subagent delegation:** the Xvfb load-test gate is the natural
  delegation unit — a build subagent compiles and runs the gate in the
  background while design work continues. Do not delegate the FFI boundary
  review; the unsafe wrappers and panic-safety of exports get a direct
  pass.

## Review Checklist

- [ ] The six exports exist with exact C-ABI signatures and default
      visibility; name/version/description/authors point at static storage.
- [ ] `pluginInit` only stashes the SDK pointer and registers tools —
      fast, infallible, no blocking I/O.
- [ ] SDK struct transcription carries `size_of`/`offset_of` compile-time
      asserts against the vendored header; regeneration is scripted.
- [ ] No panic/exception/fatal-error path crosses any `extern "C"`
      boundary or registered callback (abort profile or catch_unwind /
      catch-all, verified by review).
- [ ] `unsafe` is confined to thin documented wrappers; crate root
      forbids it elsewhere.
- [ ] Only the three `vspPost*Notification` calls happen off the host's
      calling thread; background workers have a shutdown contract.
- [ ] Every SDK allocation is freed by the documented owner (`util_free`
      for `layerGetInfo`, `layerFree` for flatten results); borrowed
      pointers are never freed or written past their bounds.
- [ ] `editorUndoPushLayerState` precedes pixel edits outside brush/filter
      code.
- [ ] Parameter story is explicit: filter dialogs via
      `filterNew*Parameter`, or a documented config file for editor
      actions.
- [ ] Xvfb gate passed with a distinct test filename; SIGTERM exits
      cleanly; the test build was removed.
