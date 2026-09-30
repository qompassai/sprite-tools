# SDK Layout (VoidSprite SDK v1)

Verified against `/home/phaedrus/vs-lightshow/voidsprite_sdk_c.h`, the
vendored mechanical C transcription of `voidsprite_sdk.h`
(counter185/voidsprite, SDK v1). Member order, names, and signatures are
transcribed exactly; the doc comments are condensed from the header's own
`<summary>` notes. **Do not invent members** — if a name is not on this
page, the plugin cannot call it.

## Version and constants

```c
#define VS_SDK_VERSION 1          /* returned by voidspriteSDKVersion() */

#define VSP_LAYER_RGBA    0x01    /* layer-type bitmask */
#define VSP_LAYER_INDEXED 0x02
```

`VSP_LAYER_RGBA | VSP_LAYER_INDEXED` is the bitmask passed as
`layerTypes` to the importer/exporter registrations.

## Opaque handles

```c
typedef struct VSPLayer        VSPLayer;        /* a layer */
typedef struct VSPFilter       VSPFilter;       /* a registered filter */
typedef struct VSPFileExporter VSPFileExporter; /* a registered exporter */
typedef struct VSPEditorContext VSPEditorContext; /* the editor session */
typedef struct VSPBrush        VSPBrush;        /* a registered brush */
```

Forward-declared only. The plugin holds them as opaque pointers and
passes them back to SDK functions — never dereferences, sizes, or copies
them.

## VSPLayerInfo

```c
struct VSPLayerInfo {
    int32_t type;    /* VSP_LAYER_RGBA or VSP_LAYER_INDEXED */
    int32_t width;
    int32_t height;
};
```

Returned by `layerGetInfo`; **free with `util_free`** after use. NULL
layer in → NULL out.

## The six exports

```c
EXPORT int         voidspriteSDKVersion();   /* return VS_SDK_VERSION */
EXPORT void        pluginInit(voidspriteSDK*);
EXPORT const char* getPluginName();
EXPORT const char* getPluginVersion();
EXPORT const char* getPluginDescription();
EXPORT const char* getPluginAuthors();
```

`EXPORT` is `__declspec(dllexport)` on MSVC,
`__attribute__((visibility("default")))` on GCC/Clang. The returned
strings must live for the library's lifetime (literals or `static`).

## struct voidspriteSDK

A function-pointer table the host fills before calling `pluginInit`. The
transcription carries `#pragma pack(push, 1)`; on x86-64 this changes
nothing (all fields are 8-byte pointers; `VSPLayerInfo` needs no
padding), but it is a layout fact to assert, not to assume — see the
header-drift rules in `SKILL.md`.

### File utilities

```c
FILE* (*util_fopenUTF8)(char* path_utf8, const char* mode);
/* fopen with a UTF-8 path. Use for plugin-owned files when non-ASCII
   paths are possible. */

void (*util_free)(void*);
/* Frees SDK-allocated memory the docs assign to the plugin —
   specifically the VSPLayerInfo* from layerGetInfo. */
```

### Registration: filters

```c
VSPFilter* (*registerFilter)(
    const char* name,
    void (*filterFunction)(VSPLayer* layer, VSPFilter* filter));
```

Returns the filter handle used by the parameter APIs below. Filter
parameter declaration (these are what make the host generate a parameter
dialog — no parameters means the filter runs instantly on selection):

```c
void (*filterNewBoolParameter)(VSPFilter* filter, const char* name,
                               bool defaultValue);
void (*filterNewIntParameter)(VSPFilter* filter, const char* name,
                              int minValue, int maxValue, int defaultValue);
void (*filterNewDoubleParameter)(VSPFilter* filter, const char* name,
                                 double minValue, double maxValue,
                                 double defaultValue);
void (*filterNewDoubleRangeParameter)(VSPFilter* filter, const char* name,
                                      double minValue, double maxValue,
                                      double defaultValueLow,
                                      double defaultValueHigh,
                                      uint32_t color);
```

Filter parameter reads (inside the filter body):

```c
double (*filterGetDoubleValue)(VSPFilter* filter, const char* name);
int    (*filterGetIntValue)(VSPFilter* filter, const char* name);
double (*filterGetRangeValue1)(VSPFilter* filter, const char* name);
double (*filterGetRangeValue2)(VSPFilter* filter, const char* name);
bool   (*filterGetBoolValue)(VSPFilter* filter, const char* name);
```

### Registration: file importers / exporters

```c
void (*registerLayerImporter)(
    const char* name,
    const char* extension,
    int layerTypes,                       /* VSP_LAYER_RGBA | VSP_LAYER_INDEXED */
    VSPFileExporter* matchingExporter,    /* may be NULL */
    VSPLayer* (*importFunction)(char* path),
    bool (*canImportFunction)(char* path) /* may be NULL; e.g. magic-number check */
);

VSPFileExporter* (*registerLayerExporter)(
    const char* name,
    const char* extension,
    int layerTypes,
    bool (*exportFunction)(VSPLayer* layer, char* path),  /* true on success */
    bool (*canExportFunction)(VSPLayer* layer)            /* may be NULL */
);
```

Paths are UTF-8. The import function returns NULL or a valid layer. For
single-layer file types only.

### Registration: brushes

```c
VSPBrush* (*registerBrush)(
    const char* name,
    const char* tooltip,
    bool doublePosPrecision,
    void (*clickAt)(VSPBrush*, VSPEditorContext* editor, int x, int y),
    void (*dragAt)(VSPBrush*, VSPEditorContext* editor,
                   int xFrom, int yFrom, int xTo, int yTo),
    void (*releaseAt)(VSPBrush*, VSPEditorContext* editor, int x, int y));
```

Any of the three callbacks may be NULL (the sample passes NULL for
`dragAt` and `releaseAt`).

### Registration: editor actions

```c
void (*registerEditorAction)(
    const char* name,
    void (*action)(VSPEditorContext* editor));
/* "Registers a new editor action accessible in the navigation bar." */
```

Note the asymmetry documented in `SKILL.md`: there is **no parameter API
on this path**. An action needing settings reads a config file.

### Layers: allocation and pixels

```c
VSPLayer* (*layerAllocNew)(int type, int width, int height);
/* NULL on failure (e.g. out of memory). */

void (*layerFree)(VSPLayer* layer);   /* no-op on NULL */

VSPLayerInfo* (*layerGetInfo)(VSPLayer*);  /* free with util_free; NULL in → NULL out */

void     (*layerSetPixel)(VSPLayer* layer, int x, int y, uint32_t color);
/* RGBA: 0xAARRGGBB. Indexed: palette index, or -1 for transparent.
   No-op on NULL layer or out-of-bounds position. */

uint32_t (*layerGetPixel)(VSPLayer* layer, int x, int y);
/* RGBA: 0xAARRGGBB. Indexed: palette index, or -1 for transparent.
   Returns 0 on NULL/out-of-bounds. */

uint32_t* (*layerGetRawPixelData)(VSPLayer* layer);
/* Borrowed view of width*height*4 bytes (both layer types). Do NOT free;
   do not read past the bounds. NULL layer → NULL. */
```

### Editor session

```c
uint32_t (*editorGetActiveColor)(VSPEditorContext* editor);
/* RGBA session: 0xAARRGGBB. Indexed: palette index. NULL editor → 0. */

int (*editorGetNumLayers)(VSPEditorContext* editor);
VSPLayer* (*editorGetLayer)(VSPEditorContext* editor, int index);
VSPLayer* (*editorGetActiveLayer)(VSPEditorContext* editor);

void (*editorSetPixel)(VSPEditorContext* editor, int x, int y, uint32_t color);

void (*editorUndoPushLayerState)(VSPEditorContext* editor, VSPLayer* layer);
/* Push before modifying a layer's pixels outside brush/filter code —
   without it your edit has no undo. */

VSPLayer* (*editorFlattenImage)(VSPEditorContext* editor);
VSPLayer* (*editorFlattenFrame)(VSPEditorContext* editor, int index);
/* Both allocate — free the result with layerFree when done. */

int (*editorGetNumFrames)(VSPEditorContext* editor);
int (*editorGetActiveFrameIndex)(VSPEditorContext* editor);
```

### Notifications (thread-safe)

The only SDK calls documented as safe from any thread:

```c
void (*vspPostNotification)(const char* title, const char* message,
                            uint32_t color, int durationMS);
/* color is 0xAARRGGBB; duration in milliseconds. UTF-8 text. */

void (*vspPostSuccessNotification)(const char* title, const char* message);
/* Green (#FFD9FFBA), 5 s, success icon. */

void (*vspPostErrorNotification)(const char* title, const char* message);
/* Red (#FFFFBABA), 5 s, error icon. */
```

Use these — and only these — from plugin-spawned threads.

### Localization

```c
const char* (*vspGetLocalizedString)(const char* key);
/* e.g. "vsp.cmn.error" → "Error". Missing key → "--NO KEY".
   Borrowed, read-only: do not modify or free. */
```

## Drift checklist for a new SDK release

1. Re-run the transcription script against the new `voidsprite_sdk.h`
   and `diff` — confirm only documented transformations changed.
2. Let the `size_of`/`offset_of` asserts fail or pass; do not eyeball.
3. Check `VS_SDK_VERSION` — if it moved, the host's compatibility
   decision may have moved with it.
4. Re-read the `<summary>` docs on any member you call: ownership and
   thread-safety notes are the parts that change silently.
