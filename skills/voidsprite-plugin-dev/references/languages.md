# Plugin Languages

A VoidSprite plugin must be a native shared library exporting six C-ABI
symbols for `dlopen`/`dlsym`. That single requirement decides which
languages can play and which cannot.

## Rust (recommended)

The default choice. `crate-type = ["cdylib"]`, `#[no_mangle] pub extern
"C"` on the six exports, `*mut voidspriteSDK` as the SDK pointer type
with `#[repr(C)]` transcriptions of the SDK structs. The tiger-style-rust
unsafe policy applies in full: `#![forbid(unsafe_code)]` at the crate
root, a single boundary module allowed to use unsafe, every wrapper
documenting allocation/provenance/alignment/bounds/lifetime obligations,
`#![deny(unsafe_op_in_unsafe_fn)]`, and `panic = "abort"` in the cdylib
profile (or `catch_unwind` at each export — abort is simpler and honest:
a plugin that panics has already violated its contract).

Rust earns the recommendation three ways: the borrow checker catches the
use-after-`layerFree` and double-`util_free` mistakes that C lets through
silently; `extern "C"` ABI control is first-class; and the ecosystem's
TOML/JSON config story (for editor-action parameters) is mature.

## Zig's `@cImport` advantage

Zig reads the C header directly: `@cImport(@cInclude("voidsprite_sdk.h"))`
generates the struct bindings from the header itself, which kills the
transcription problem outright — there is no hand-maintained struct to
drift from the header, and a new SDK release is a recompile, not a
re-transcription. `export fn pluginInit(sdk: *voidspriteSDK) void` gives
the C-ABI exports, and `comptime` asserts on `@sizeOf`/`@offsetOf` cost
nothing.

The caveat: `@cImport` parses C, not C++. The vendored original
`voidsprite_sdk.h` is C++-flavored (C++-spelled struct tags, C++ comment
style, and the transcription notes shim typedefs were needed to make it
C-clean). Point `@cImport` at a C-clean transcription like
`voidsprite_sdk_c.h`, not at the raw C++-flavored header — feeding the
latter to the C parser produces cryptic failures. When the transcription
is regenerated for a new SDK, `@cImport` picks it up automatically, which
is precisely the workflow the transcription script exists to support.

## C / C++ (baseline)

The sample plugin is C++ (`dllmain.cpp`), so the baseline is proven. C is
the honest, dependency-free option: the header is already C, the exports
are plain functions, and there is no runtime to negotiate with. The cost
is that every safety property the other languages get for free becomes a
discipline item: null checks on every SDK return, exact pairing of
`util_free`/`layerFree`, no exception crossing the boundary (wrap every
export and callback in `try`/`catch (...)` and route failures to
`vspPostErrorNotification`), and manual review of every struct
transcription against the header.

Prefer C over C++ unless the plugin genuinely needs C++: exceptions,
constructors with side effects, and STL allocations at the host boundary
are all sharp edges with no upside here.

## Why interpreted languages cannot do this

Python, Lua, and JavaScript are out — not for lack of enthusiasm, but
structurally:

1. **No native artifact.** The host `dlopen`s a `.so` and `dlsym`s six
   symbols. Interpreted languages do not produce native shared libraries
   with C-ABI exports; there is nothing for the host to load.
2. **The loading direction is fixed.** The host loads the plugin. An
   embedded interpreter reverses this: the *plugin* would have to host
   the runtime, ship it, initialize it inside `pluginInit`, and marshal
   every callback through it — a second process's worth of machinery
   inside a guest library, with startup latency paid on every host
   launch and no host support for any of it.
3. **No panic-safety story.** A guest-language exception or GC pause
   crossing a C-ABI callback into a C++ host is undefined behavior or a
   visible stall, and there is no sanctioned mechanism to prevent it.

If a workflow needs a scripting language, that workflow belongs on the
Aseprite side of the repo (`aseprite-scripting` skill), where Lua is the
host's own automation surface — not smuggled into a VoidSprite plugin.
