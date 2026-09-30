/*
 * vs_lightshow.c — "light-show sprite tools", a native plugin for VoidSprite.
 *
 * Supports Matt's light-show companion sprite pipeline:
 *   1. Filter "Light-show: blink eyes" — parameterised vertical squash of two
 *      eye boxes, producing closed-eye variants of a base frame.
 *   2. Filter "Light-show: breathing shift" — parameterised vertical shift
 *      with edge replication, for idle bob loops.
 *   3. Editor action "Light-show: export sheet + JSON" — packs every frame of
 *      the current session into a sprite-sheet PNG plus a JSON atlas sidecar.
 *      Layout comes from ~/.config/voidsprite/lightshow_export.cfg; defaults
 *      match the game (96x192 cells, 4 cols x 6 rows, 180 ms/frame).
 *
 * Written in Matt's Tiger Style for C (C17): contracts first, status enums
 * for expected failures, assertions for invariants only, every loop and
 * allocation bounded, one owner per buffer. The VoidSprite SDK header
 * (SDK v1) is the authority on the host API; no SDK functions are invented.
 *
 * Build: ./build.sh
 */

#include <assert.h>
#include <errno.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>

/*
 * The vendored SDK header (voidsprite_sdk.h) is written for C++: default
 * member initializers ("= 0") and bare struct names. voidsprite_sdk_c.h
 * is a mechanical C transcription produced by gen_c_header.sh — same
 * struct tags, member order, member types, and constants, so the table
 * layout the host fills in is identical. The vendored original is never
 * modified.
 */
#include "voidsprite_sdk_c.h"

#define STB_IMAGE_WRITE_IMPLEMENTATION
#include <stb_image_write.h> /* vendored, unmodified; built with -isystem */

/* ------------------------------------------------------------------ */
/* Bounds: every loop, buffer, and allocation in this file is capped.  */
/* ------------------------------------------------------------------ */

#define LS_MAX_DIM_PX 8192      /* largest single image dimension we touch */
#define LS_MAX_FRAMES 256       /* most frames the exporter will pack */
#define LS_MAX_CONFIG_LINE 256  /* longest config file line we parse */
#define LS_MAX_CONFIG_LINES 64  /* most config lines we read */
#define LS_MAX_MOODS 32         /* most mood names in the JSON sidecar */
#define LS_MAX_MOOD_NAME 32     /* longest single mood name, with NUL */
#define LS_MAX_PATH 1024        /* longest path we build */
#define LS_EXPORT_CFG_NAME "lightshow_export.cfg"

/* ------------------------------------------------------------------ */
/* Status enum: expected failures return these; outputs stay unchanged */
/* on failure.                                                        */
/* ------------------------------------------------------------------ */

typedef enum ls_status {
    LS_OK = 0,
    LS_ERR_NULL_ARG,
    LS_ERR_BAD_GEOMETRY,
    LS_ERR_TOO_LARGE,
    LS_ERR_NO_MEMORY,
    LS_ERR_IO,
    LS_ERR_CONFIG,
    LS_ERR_FRAME_MISMATCH
} ls_status_t;

/* The host SDK table, captured once in pluginInit. Checked non-NULL on
 * every entry point; never mutated afterwards. */
static voidspriteSDK *g_sdk = NULL;

/* ------------------------------------------------------------------ */
/* Pure pixel math. No SDK contact: unit-testable under ASan/UBSan.     */
/* ------------------------------------------------------------------ */

/*
 * Map a destination row inside an eye box to its source row for the blink
 * squash. The box content is scaled vertically about its centre by
 * (100 - amount_pct)%; at 100% every row maps to the centre row, which
 * reads as a closed eye (a lash line), using only existing pixels.
 *
 * Contract: box_h > 0; 0 <= amount_pct <= 100; dst_row in
 * [box_y, box_y + box_h). Returns a row in the same range.
 */
static int ls_blink_src_row(int box_y, int box_h, int dst_row, int amount_pct)
{
    int center;
    int scale_pct;
    int offset;
    assert(box_h > 0);
    assert(amount_pct >= 0 && amount_pct <= 100);
    assert(dst_row >= box_y && dst_row < box_y + box_h);
    center = box_y + box_h / 2;
    scale_pct = 100 - amount_pct;
    offset = dst_row - center;
    /* |offset| <= LS_MAX_DIM_PX and scale_pct <= 100: no overflow. */
    return center + (offset * scale_pct) / 100;
}

/*
 * Map a destination row to its source row for a vertical shift, replicating
 * edge rows where the shift exposes new pixels.
 *
 * Contract: h > 0; dst_row in [0, h); |shift_px| <= LS_MAX_DIM_PX.
 * Returns a row in [0, h).
 */
static int ls_shift_src_row(int h, int dst_row, int shift_px)
{
    int src;
    assert(h > 0);
    assert(dst_row >= 0 && dst_row < h);
    assert(shift_px >= -LS_MAX_DIM_PX && shift_px <= LS_MAX_DIM_PX);
    src = dst_row - shift_px;
    if (src < 0) {
        return 0;
    }
    if (src >= h) {
        return h - 1;
    }
    return src;
}

/*
 * Convert one 0xAARRGGBB SDK pixel to RGBA byte order for PNG output.
 * Contract: out points to at least 4 writable bytes.
 */
static void ls_pixel_to_rgba(uint32_t argb, unsigned char out[4])
{
    assert(out != NULL);
    out[0] = (unsigned char)((argb >> 16) & 0xFFu); /* R */
    out[1] = (unsigned char)((argb >> 8) & 0xFFu);  /* G */
    out[2] = (unsigned char)(argb & 0xFFu);         /* B */
    out[3] = (unsigned char)((argb >> 24) & 0xFFu); /* A */
}

/* ------------------------------------------------------------------ */
/* SDK helpers.                                                        */
/* ------------------------------------------------------------------ */

/*
 * Fetch an RGBA layer's dimensions. Fills *w and *h on success; leaves them
 * unchanged on failure. Rejects NULL args, non-RGBA layers, and absurd
 * sizes — never trust the host blindly.
 */
static ls_status_t ls_layer_dims(voidspriteSDK *sdk, VSPLayer *layer,
                                 int *w, int *h)
{
    VSPLayerInfo *info;
    if (sdk == NULL || layer == NULL || w == NULL || h == NULL) {
        return LS_ERR_NULL_ARG;
    }
    info = sdk->layerGetInfo(layer);
    if (info == NULL) {
        return LS_ERR_BAD_GEOMETRY;
    }
    if ((info->type & VSP_LAYER_RGBA) == 0 || info->width <= 0 ||
        info->height <= 0 || info->width > LS_MAX_DIM_PX ||
        info->height > LS_MAX_DIM_PX) {
        sdk->util_free(info);
        return LS_ERR_BAD_GEOMETRY;
    }
    *w = info->width;
    *h = info->height;
    sdk->util_free(info);
    return LS_OK;
}

/*
 * Copy a layer's raw pixels into a freshly malloc'd w*h buffer.
 * Contract: sdk/layer non-NULL, w/h already validated positive and bounded.
 * Returns NULL on allocation failure or a NULL pixel pointer. The caller
 * owns the buffer and must free() it.
 */
static uint32_t *ls_snapshot_layer(voidspriteSDK *sdk, VSPLayer *layer,
                                   int w, int h)
{
    uint32_t *buf;
    uint32_t *src;
    size_t count;
    assert(sdk != NULL && layer != NULL && w > 0 && h > 0);
    assert(w <= LS_MAX_DIM_PX && h <= LS_MAX_DIM_PX);
    count = (size_t)w * (size_t)h; /* bounded by LS_MAX_DIM_PX^2 */
    buf = (uint32_t *)malloc(count * sizeof(uint32_t));
    if (buf == NULL) {
        return NULL;
    }
    src = sdk->layerGetRawPixelData(layer);
    if (src == NULL) {
        free(buf);
        return NULL;
    }
    memcpy(buf, src, count * sizeof(uint32_t));
    return buf;
}

/*
 * Squash one eye box vertically in place. Reads from the snapshot, writes
 * to the live layer pixels, so overlapping boxes cannot corrupt each other.
 *
 * Contract: live pixels are w*h; snap is a w*h snapshot of the same layer;
 * box coordinates may lie partly outside the layer (clamped); amount_pct in
 * [0,100]. An empty intersection or amount 0 is a no-op.
 */
static void ls_apply_eye_box(uint32_t *live, const uint32_t *snap,
                             int layer_w, int layer_h,
                             int bx, int by, int bw, int bh, int amount_pct)
{
    int x0, y0, x1, y1, x, y;
    assert(live != NULL && snap != NULL);
    assert(layer_w > 0 && layer_h > 0);
    if (amount_pct <= 0) {
        return;
    }
    if (amount_pct > 100) {
        amount_pct = 100;
    }
    x0 = bx < 0 ? 0 : bx;
    y0 = by < 0 ? 0 : by;
    x1 = bx + bw > layer_w ? layer_w : bx + bw;
    y1 = by + bh > layer_h ? layer_h : by + bh;
    if (x0 >= x1 || y0 >= y1) {
        return; /* box misses the layer entirely */
    }
    for (y = y0; y < y1; y++) {
        int src_y = ls_blink_src_row(y0, y1 - y0, y, amount_pct);
        size_t dst_row = (size_t)y * (size_t)layer_w;
        size_t src_row = (size_t)src_y * (size_t)layer_w;
        for (x = x0; x < x1; x++) {
            live[dst_row + (size_t)x] = snap[src_row + (size_t)x];
        }
    }
}

/* ------------------------------------------------------------------ */
/* Filter 1: blink eyes.                                               */
/* ------------------------------------------------------------------ */

static void ls_filter_blink(VSPLayer *layer, VSPFilter *filter)
{
    voidspriteSDK *sdk = g_sdk;
    int w = 0, h = 0;
    uint32_t *snap = NULL;
    uint32_t *live;
    int lx, ly, lw, lh, rx, ry, rw, rh, amount;
    assert(sdk != NULL); /* pluginInit ran; host contract */
    if (sdk == NULL || layer == NULL || filter == NULL) {
        return;
    }
    /* Filters run inside the host's own undo scope: no manual undo push. */
    if (ls_layer_dims(sdk, layer, &w, &h) != LS_OK) {
        sdk->vspPostErrorNotification("Light-show blink",
                                      "Layer is not RGBA or has bad geometry.");
        return;
    }
    lx = sdk->filterGetIntValue(filter, "left eye x");
    ly = sdk->filterGetIntValue(filter, "left eye y");
    lw = sdk->filterGetIntValue(filter, "left eye w");
    lh = sdk->filterGetIntValue(filter, "left eye h");
    rx = sdk->filterGetIntValue(filter, "right eye x");
    ry = sdk->filterGetIntValue(filter, "right eye y");
    rw = sdk->filterGetIntValue(filter, "right eye w");
    rh = sdk->filterGetIntValue(filter, "right eye h");
    amount = sdk->filterGetIntValue(filter, "blink amount %");
    snap = ls_snapshot_layer(sdk, layer, w, h);
    if (snap == NULL) {
        sdk->vspPostErrorNotification("Light-show blink",
                                      "Out of memory snapshotting the layer.");
        return;
    }
    live = sdk->layerGetRawPixelData(layer);
    if (live == NULL) {
        free(snap);
        sdk->vspPostErrorNotification("Light-show blink",
                                      "Could not access layer pixels.");
        return;
    }
    ls_apply_eye_box(live, snap, w, h, lx, ly, lw, lh, amount);
    ls_apply_eye_box(live, snap, w, h, rx, ry, rw, rh, amount);
    free(snap);
}

/* ------------------------------------------------------------------ */
/* Filter 2: breathing shift.                                          */
/* ------------------------------------------------------------------ */

static void ls_filter_breathe(VSPLayer *layer, VSPFilter *filter)
{
    voidspriteSDK *sdk = g_sdk;
    int w = 0, h = 0, shift, y;
    uint32_t *snap = NULL;
    uint32_t *live;
    assert(sdk != NULL);
    if (sdk == NULL || layer == NULL || filter == NULL) {
        return;
    }
    if (ls_layer_dims(sdk, layer, &w, &h) != LS_OK) {
        sdk->vspPostErrorNotification("Light-show breathe",
                                      "Layer is not RGBA or has bad geometry.");
        return;
    }
    shift = sdk->filterGetIntValue(filter, "shift px");
    if (shift < -LS_MAX_DIM_PX) {
        shift = -LS_MAX_DIM_PX;
    }
    if (shift > LS_MAX_DIM_PX) {
        shift = LS_MAX_DIM_PX;
    }
    snap = ls_snapshot_layer(sdk, layer, w, h);
    if (snap == NULL) {
        sdk->vspPostErrorNotification("Light-show breathe",
                                      "Out of memory snapshotting the layer.");
        return;
    }
    live = sdk->layerGetRawPixelData(layer);
    if (live == NULL) {
        free(snap);
        sdk->vspPostErrorNotification("Light-show breathe",
                                      "Could not access layer pixels.");
        return;
    }
    for (y = 0; y < h; y++) {
        int src_y = ls_shift_src_row(h, y, shift);
        memcpy(&live[(size_t)y * (size_t)w],
               &snap[(size_t)src_y * (size_t)w],
               (size_t)w * sizeof(uint32_t));
    }
    free(snap);
}

/* ------------------------------------------------------------------ */
/* Exporter configuration.                                             */
/* ------------------------------------------------------------------ */

struct ls_export_cfg {
    int cell_w;
    int cell_h;
    int cols;
    int rows;
    int frame_ms;
    char moods[LS_MAX_MOODS][LS_MAX_MOOD_NAME];
    int mood_count;
    char output_dir[LS_MAX_PATH]; /* empty = $HOME/lightshow-export */
    char basename[LS_MAX_PATH];   /* PNG/JSON stem, no extension */
};

/* Defaults match the light-show game constants (game/src/waifu/sprite.rs):
 * 96x192 cells, 4 cols x 6 rows, 180 ms frames. */
static void ls_default_cfg(struct ls_export_cfg *cfg)
{
    static const char *const default_moods[6] = {
        "idle", "blush", "wink", "pout", "celebrate", "alarmed"
    };
    int i;
    assert(cfg != NULL);
    cfg->cell_w = 96;
    cfg->cell_h = 192;
    cfg->cols = 4;
    cfg->rows = 6;
    cfg->frame_ms = 180;
    cfg->mood_count = 6;
    for (i = 0; i < 6; i++) {
        snprintf(cfg->moods[i], LS_MAX_MOOD_NAME, "%s", default_moods[i]);
    }
    cfg->output_dir[0] = '\0';
    snprintf(cfg->basename, LS_MAX_PATH, "%s", "sheet");
}

/* Strict integer parser: rejects junk, overflow (errno), and range errors.
 * *out unchanged on failure. */
static ls_status_t ls_parse_int_value(const char *val, int min_v, int max_v,
                                      int *out)
{
    char *end = NULL;
    long v;
    assert(val != NULL && out != NULL && min_v <= max_v);
    errno = 0;
    v = strtol(val, &end, 10);
    if (errno != 0 || end == val) {
        return LS_ERR_CONFIG;
    }
    while (*end == ' ' || *end == '\t' || *end == '\n' || *end == '\r') {
        end++;
    }
    if (*end != '\0') {
        return LS_ERR_CONFIG;
    }
    if (v < min_v || v > max_v) {
        return LS_ERR_CONFIG;
    }
    *out = (int)v;
    return LS_OK;
}

/* A mood/file-stem token may only contain filename- and JSON-safe chars. */
static int ls_token_ok(const char *tok, size_t len)
{
    size_t i;
    if (len == 0 || len >= LS_MAX_MOOD_NAME) {
        return 0;
    }
    for (i = 0; i < len; i++) {
        char c = tok[i];
        int ok = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                 (c >= '0' && c <= '9') || c == '_' || c == '-' || c == '.';
        if (!ok) {
            return 0;
        }
    }
    return 1;
}

/* Parse a comma-separated mood list into cfg. Rejects empty lists and bad
 * tokens; cfg unchanged on failure. */
static ls_status_t ls_parse_moods(struct ls_export_cfg *cfg, const char *val)
{
    char staged[LS_MAX_MOODS][LS_MAX_MOOD_NAME];
    int count = 0;
    const char *p = val;
    assert(cfg != NULL && val != NULL);
    for (;;) {
        const char *comma;
        size_t len;
        while (*p == ' ' || *p == '\t') {
            p++;
        }
        comma = strchr(p, ',');
        len = comma == NULL ? strlen(p) : (size_t)(comma - p);
        while (len > 0 &&
               (p[len - 1] == ' ' || p[len - 1] == '\t' ||
                p[len - 1] == '\n' || p[len - 1] == '\r')) {
            len--;
        }
        if (!ls_token_ok(p, len) || count >= LS_MAX_MOODS) {
            return LS_ERR_CONFIG;
        }
        memcpy(staged[count], p, len);
        staged[count][len] = '\0';
        count++;
        if (comma == NULL) {
            break;
        }
        p = comma + 1;
    }
    if (count == 0) {
        return LS_ERR_CONFIG;
    }
    memcpy(cfg->moods, staged, sizeof(staged));
    cfg->mood_count = count;
    return LS_OK;
}

/* Copy a validated token into a path buffer. */
static ls_status_t ls_parse_token_field(char *field, const char *val)
{
    size_t len;
    assert(field != NULL && val != NULL);
    while (*val == ' ' || *val == '\t') {
        val++;
    }
    len = strlen(val);
    while (len > 0 &&
           (val[len - 1] == ' ' || val[len - 1] == '\t' ||
            val[len - 1] == '\n' || val[len - 1] == '\r')) {
        len--;
    }
    if (!ls_token_ok(val, len) || len >= LS_MAX_PATH) {
        return LS_ERR_CONFIG;
    }
    memcpy(field, val, len);
    field[len] = '\0';
    return LS_OK;
}

/*
 * Parse one config line. Blank lines and '#' comments are accepted and
 * ignored. Unknown keys are an error — a typo'd key must never silently
 * keep a default.
 */
static ls_status_t ls_parse_config_line(struct ls_export_cfg *cfg,
                                        const char *line, int *handled)
{
    const char *eq;
    const char *val;
    char key[64];
    size_t key_len;
    assert(cfg != NULL && line != NULL && handled != NULL);
    *handled = 0;
    while (*line == ' ' || *line == '\t') {
        line++;
    }
    if (*line == '\0' || *line == '\n' || *line == '\r' || *line == '#') {
        return LS_OK;
    }
    eq = strchr(line, '=');
    if (eq == NULL) {
        return LS_ERR_CONFIG;
    }
    key_len = (size_t)(eq - line);
    if (key_len == 0 || key_len >= sizeof(key)) {
        return LS_ERR_CONFIG;
    }
    memcpy(key, line, key_len);
    key[key_len] = '\0';
    while (key_len > 0 &&
           (key[key_len - 1] == ' ' || key[key_len - 1] == '\t')) {
        key[--key_len] = '\0';
    }
    val = eq + 1;
    *handled = 1;
    if (strcmp(key, "cell_w") == 0) {
        return ls_parse_int_value(val, 1, 2048, &cfg->cell_w);
    }
    if (strcmp(key, "cell_h") == 0) {
        return ls_parse_int_value(val, 1, 2048, &cfg->cell_h);
    }
    if (strcmp(key, "cols") == 0) {
        return ls_parse_int_value(val, 1, 64, &cfg->cols);
    }
    if (strcmp(key, "rows") == 0) {
        return ls_parse_int_value(val, 1, 64, &cfg->rows);
    }
    if (strcmp(key, "frame_ms") == 0) {
        return ls_parse_int_value(val, 1, 10000, &cfg->frame_ms);
    }
    if (strcmp(key, "moods") == 0) {
        return ls_parse_moods(cfg, val);
    }
    if (strcmp(key, "output_dir") == 0) {
        while (*val == ' ' || *val == '\t') {
            val++;
        }
        {
            size_t len = strlen(val);
            while (len > 0 &&
                   (val[len - 1] == '\n' || val[len - 1] == '\r' ||
                    val[len - 1] == ' ' || val[len - 1] == '\t')) {
                len--;
            }
            if (len == 0 || len >= LS_MAX_PATH) {
                return LS_ERR_CONFIG;
            }
            memcpy(cfg->output_dir, val, len);
            cfg->output_dir[len] = '\0';
        }
        return LS_OK;
    }
    if (strcmp(key, "basename") == 0) {
        return ls_parse_token_field(cfg->basename, val);
    }
    return LS_ERR_CONFIG; /* unknown key */
}

/*
 * Load the exporter config file. A missing file is not an error: defaults
 * stand. Any parse error aborts the load and leaves cfg at whatever the
 * caller set (callers start from ls_default_cfg).
 */
static ls_status_t ls_load_config(voidspriteSDK *sdk,
                                  struct ls_export_cfg *cfg, const char *path)
{
    FILE *f;
    char line[LS_MAX_CONFIG_LINE];
    int nlines = 0;
    struct ls_export_cfg staged;
    assert(cfg != NULL && path != NULL);
    staged = *cfg; /* parse into a copy; commit only on full success */
    if (sdk != NULL) {
        f = sdk->util_fopenUTF8((char *)path, "r");
    } else {
        f = fopen(path, "r");
    }
    if (f == NULL) {
        return LS_OK; /* no config file: defaults stand */
    }
    while (fgets(line, sizeof(line), f) != NULL) {
        int handled = 0;
        ls_status_t st;
        if (++nlines > LS_MAX_CONFIG_LINES) {
            fclose(f);
            return LS_ERR_CONFIG;
        }
        /* A line that fills the buffer without a newline is too long. */
        if (strchr(line, '\n') == NULL && !feof(f)) {
            fclose(f);
            return LS_ERR_CONFIG;
        }
        st = ls_parse_config_line(&staged, line, &handled);
        if (st != LS_OK) {
            fclose(f);
            return st;
        }
        (void)handled;
    }
    fclose(f);
    if ((long)staged.cols * (long)staged.rows > LS_MAX_FRAMES) {
        return LS_ERR_CONFIG;
    }
    *cfg = staged;
    return LS_OK;
}

/* ------------------------------------------------------------------ */
/* JSON sidecar. The game builds its Bevy atlas from Rust constants;   */
/* this file documents the exact pipeline contract next to the PNG.    */
/* ------------------------------------------------------------------ */

static ls_status_t ls_write_json(const char *path,
                                 const struct ls_export_cfg *cfg,
                                 const char *png_name)
{
    FILE *f;
    int r, c;
    assert(path != NULL && cfg != NULL && png_name != NULL);
    f = fopen(path, "w");
    if (f == NULL) {
        return LS_ERR_IO;
    }
    fprintf(f, "{\n");
    fprintf(f, "  \"generator\": \"vs-lightshow 1.0.0\",\n");
    fprintf(f, "  \"image\": \"%s\",\n", png_name);
    fprintf(f, "  \"cell_w\": %d,\n", cfg->cell_w);
    fprintf(f, "  \"cell_h\": %d,\n", cfg->cell_h);
    fprintf(f, "  \"cols\": %d,\n", cfg->cols);
    fprintf(f, "  \"rows\": %d,\n", cfg->rows);
    fprintf(f, "  \"frame_ms\": %d,\n", cfg->frame_ms);
    fprintf(f, "  \"moods\": [");
    for (r = 0; r < cfg->mood_count; r++) {
        fprintf(f, "%s\"%s\"", r == 0 ? "" : ", ", cfg->moods[r]);
    }
    fprintf(f, "],\n");
    fprintf(f, "  \"frames\": [");
    for (r = 0; r < cfg->rows; r++) {
        for (c = 0; c < cfg->cols; c++) {
            int idx = r * cfg->cols + c;
            const char *mood = r < cfg->mood_count ? cfg->moods[r] : "unknown";
            fprintf(f, "%s\n    {\"index\": %d, \"col\": %d, \"row\": %d, ",
                    idx == 0 ? "" : ",", idx, c, r);
            fprintf(f, "\"x\": %d, \"y\": %d, \"w\": %d, \"h\": %d, "
                       "\"mood\": \"%s\"}",
                    c * cfg->cell_w, r * cfg->cell_h,
                    cfg->cell_w, cfg->cell_h, mood);
        }
    }
    fprintf(f, "\n  ],\n");
    fprintf(f, "  \"note\": \"light-show builds its Bevy TextureAtlasLayout "
               "from Rust constants (game/src/waifu/sprite.rs); this file "
               "documents the pipeline contract.\"\n");
    fprintf(f, "}\n");
    if (ferror(f)) {
        fclose(f);
        return LS_ERR_IO;
    }
    if (fclose(f) != 0) {
        return LS_ERR_IO;
    }
    return LS_OK;
}

/* ------------------------------------------------------------------ */
/* Editor action: export sheet + JSON.                                 */
/* ------------------------------------------------------------------ */

/* Create one directory level; EEXIST is fine. */
static ls_status_t ls_mkdir_one(const char *path)
{
    assert(path != NULL);
    if (mkdir(path, 0755) != 0 && errno != EEXIST) {
        return LS_ERR_IO;
    }
    return LS_OK;
}

static void ls_action_export(VSPEditorContext *editor)
{
    voidspriteSDK *sdk = g_sdk;
    struct ls_export_cfg cfg;
    char cfg_path[LS_MAX_PATH];
    char out_dir[LS_MAX_PATH];
    char png_path[LS_MAX_PATH];
    char json_path[LS_MAX_PATH];
    char png_name[LS_MAX_PATH];
    const char *home;
    int nframes, want, i;
    size_t sheet_w, sheet_h;
    unsigned char *rgba = NULL;
    ls_status_t st = LS_OK;
    char msg[128];

    assert(sdk != NULL);
    if (sdk == NULL || editor == NULL) {
        return;
    }
    /* This action only reads the session; nothing to push onto undo. */
    ls_default_cfg(&cfg);
    home = getenv("HOME");
    if (home != NULL && home[0] != '\0' &&
        (size_t)snprintf(cfg_path, sizeof(cfg_path),
                         "%s/.config/voidsprite/%s",
                         home, LS_EXPORT_CFG_NAME) < sizeof(cfg_path)) {
        if (ls_load_config(sdk, &cfg, cfg_path) != LS_OK) {
            sdk->vspPostErrorNotification(
                "Light-show export",
                "Config file has errors; fix it or delete it.");
            return;
        }
    }
    assert(cfg.cell_w > 0 && cfg.cell_h > 0 && cfg.cols > 0 && cfg.rows > 0);
    assert((long)cfg.cols * (long)cfg.rows <= LS_MAX_FRAMES);

    nframes = sdk->editorGetNumFrames(editor);
    want = cfg.cols * cfg.rows;
    if (nframes < want) {
        snprintf(msg, sizeof(msg),
                 "Session has %d frames but the sheet needs %d.",
                 nframes, want);
        sdk->vspPostErrorNotification("Light-show export", msg);
        return;
    }

    /* Explicit config wins; otherwise ~/lightshow-export; then /tmp. */
    if (cfg.output_dir[0] != '\0') {
        snprintf(out_dir, sizeof(out_dir), "%s", cfg.output_dir);
    } else if (home != NULL && home[0] != '\0' &&
               (size_t)snprintf(out_dir, sizeof(out_dir),
                                 "%s/lightshow-export",
                                 home) < sizeof(out_dir)) {
        /* out_dir set */
    } else {
        snprintf(out_dir, sizeof(out_dir), "%s", "/tmp/lightshow-export");
    }
    if (ls_mkdir_one(out_dir) != LS_OK) {
        sdk->vspPostErrorNotification("Light-show export",
                                      "Cannot create the output directory.");
        return;
    }
    if ((size_t)snprintf(png_name, sizeof(png_name), "%s.png",
                         cfg.basename) >= sizeof(png_name) ||
        (size_t)snprintf(png_path, sizeof(png_path), "%s/%s",
                         out_dir, png_name) >= sizeof(png_path) ||
        (size_t)snprintf(json_path, sizeof(json_path), "%s/%s.json",
                         out_dir, cfg.basename) >= sizeof(json_path)) {
        sdk->vspPostErrorNotification("Light-show export", "Path too long.");
        return;
    }

    sheet_w = (size_t)cfg.cols * (size_t)cfg.cell_w;
    sheet_h = (size_t)cfg.rows * (size_t)cfg.cell_h;
    /* Config caps keep this small (64*2048 px/side max), but the byte
     * count still gets an explicit overflow check before calloc. */
    if (sheet_w > SIZE_MAX / sheet_h ||
        sheet_w * sheet_h > SIZE_MAX / 4) {
        sdk->vspPostErrorNotification("Light-show export", "Sheet too large.");
        return;
    }
    rgba = (unsigned char *)calloc(sheet_w * sheet_h, 4);
    if (rgba == NULL) {
        sdk->vspPostErrorNotification("Light-show export", "Out of memory.");
        return;
    }

    for (i = 0; i < want; i++) {
        VSPLayer *frame = sdk->editorFlattenFrame(editor, i);
        uint32_t *src;
        int fw = 0, fh = 0, y;
        int dst_col = i % cfg.cols;
        int dst_row = i / cfg.cols;
        if (frame == NULL) {
            st = LS_ERR_IO;
            break;
        }
        st = ls_layer_dims(sdk, frame, &fw, &fh);
        if (st == LS_OK && (fw != cfg.cell_w || fh != cfg.cell_h)) {
            st = LS_ERR_FRAME_MISMATCH;
        }
        if (st == LS_OK) {
            src = sdk->layerGetRawPixelData(frame);
            if (src == NULL) {
                st = LS_ERR_BAD_GEOMETRY;
            } else {
                for (y = 0; y < fh; y++) {
                    int x;
                    size_t dst_base =
                        ((size_t)(dst_row * cfg.cell_h + y) * sheet_w +
                         (size_t)(dst_col * cfg.cell_w)) * 4;
                    for (x = 0; x < fw; x++) {
                        unsigned char px[4];
                        ls_pixel_to_rgba(
                            src[(size_t)y * (size_t)fw + (size_t)x], px);
                        memcpy(&rgba[dst_base + (size_t)x * 4], px, 4);
                    }
                }
            }
        }
        sdk->layerFree(frame);
        if (st != LS_OK) {
            break;
        }
    }

    if (st == LS_OK) {
        int ok_png = stbi_write_png(png_path, (int)sheet_w, (int)sheet_h,
                                    4, rgba, (int)(sheet_w * 4));
        if (!ok_png) {
            st = LS_ERR_IO;
        }
    }
    if (st == LS_OK) {
        st = ls_write_json(json_path, &cfg, png_name);
    }
    free(rgba);

    if (st != LS_OK) {
        const char *why = "Export failed.";
        if (st == LS_ERR_FRAME_MISMATCH) {
            why = "A frame's size does not match cell_w x cell_h.";
        }
        sdk->vspPostErrorNotification("Light-show export", why);
        return;
    }
    /* png_name is bounded by the %.100s precision: worst case 6+11+1+100+1
     * +1 = 120 bytes < 128, so no truncation is possible. */
    snprintf(msg, sizeof(msg), "Wrote %d frames to %.100s.", want, png_name);
    sdk->vspPostSuccessNotification("Light-show export", msg);
}

/* ------------------------------------------------------------------ */
/* Plugin entry points.                                                */
/* ------------------------------------------------------------------ */

EXPORT void pluginInit(voidspriteSDK *sdk)
{
    VSPFilter *blink;
    VSPFilter *breathe;
    g_sdk = sdk;
    if (sdk == NULL) {
        return;
    }
    blink = sdk->registerFilter("Light-show: blink eyes", ls_filter_blink);
    if (blink != NULL) {
        sdk->filterNewIntParameter(blink, "left eye x", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "left eye y", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "left eye w", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "left eye h", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "right eye x", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "right eye y", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "right eye w", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "right eye h", 0, LS_MAX_DIM_PX, 0);
        sdk->filterNewIntParameter(blink, "blink amount %", 0, 100, 80);
    }
    breathe = sdk->registerFilter("Light-show: breathing shift",
                                  ls_filter_breathe);
    if (breathe != NULL) {
        sdk->filterNewIntParameter(breathe, "shift px", -32, 32, 2);
    }
    sdk->registerEditorAction("Light-show: export sheet + JSON",
                              ls_action_export);
}

EXPORT const char *getPluginName(void)
{
    return "light-show sprite tools";
}

EXPORT const char *getPluginVersion(void)
{
    return "1.0.0";
}

EXPORT const char *getPluginDescription(void)
{
    return "Blink/breathing frame filters and a sprite-sheet + JSON exporter "
           "for the light-show companion pipeline.";
}

EXPORT const char *getPluginAuthors(void)
{
    return "Pax (Qompass AI)";
}

/* ------------------------------------------------------------------ */
/* Unit tests: compiled only with -DVS_LIGHTSHOW_UNIT_TEST. Exercises  */
/* the pure pixel math and the config parser under ASan/UBSan.         */
/* ------------------------------------------------------------------ */

#ifdef VS_LIGHTSHOW_UNIT_TEST

#include <unistd.h>

static int ls_test_failures = 0;

#define LS_CHECK(cond)                                                    \
    do {                                                                  \
        if (!(cond)) {                                                    \
            printf("FAIL line %d: %s\n", __LINE__, #cond);                 \
            ls_test_failures++;                                           \
        }                                                                 \
    } while (0)

static void ls_test_blink(void)
{
    /* amount 0: identity. */
    LS_CHECK(ls_blink_src_row(10, 20, 10, 0) == 10);
    LS_CHECK(ls_blink_src_row(10, 20, 29, 0) == 29);
    LS_CHECK(ls_blink_src_row(10, 20, 20, 0) == 20);
    /* amount 100: everything collapses to the centre row (10 + 20/2). */
    LS_CHECK(ls_blink_src_row(10, 20, 10, 100) == 20);
    LS_CHECK(ls_blink_src_row(10, 20, 29, 100) == 20);
    LS_CHECK(ls_blink_src_row(10, 20, 20, 100) == 20);
    /* amount 50 on box [0,10): centre 5; row 0 -> 5 + (0-5)/2 = 3. */
    LS_CHECK(ls_blink_src_row(0, 10, 0, 50) == 3);
    LS_CHECK(ls_blink_src_row(0, 10, 9, 50) == 7);
    LS_CHECK(ls_blink_src_row(0, 10, 5, 50) == 5);
}

static void ls_test_shift(void)
{
    LS_CHECK(ls_shift_src_row(10, 0, 0) == 0);
    LS_CHECK(ls_shift_src_row(10, 5, 3) == 2);
    LS_CHECK(ls_shift_src_row(10, 0, 3) == 0);  /* top edge replicates */
    LS_CHECK(ls_shift_src_row(10, 9, -3) == 9); /* bottom edge replicates */
    LS_CHECK(ls_shift_src_row(10, 9, 3) == 6);
    LS_CHECK(ls_shift_src_row(1, 0, 100) == 0);
}

static void ls_test_pixel(void)
{
    unsigned char px[4];
    ls_pixel_to_rgba(0xFF112233u, px);
    LS_CHECK(px[0] == 0x11 && px[1] == 0x22 && px[2] == 0x33 && px[3] == 0xFF);
    ls_pixel_to_rgba(0x00123456u, px);
    LS_CHECK(px[0] == 0x12 && px[1] == 0x34 && px[2] == 0x56 && px[3] == 0x00);
}

static void ls_write_tmp_cfg(const char *path, const char *body)
{
    FILE *f = fopen(path, "w");
    LS_CHECK(f != NULL);
    if (f != NULL) {
        fputs(body, f);
        fclose(f);
    }
}

/* Exercise the PNG + JSON writers (the host-independent half of the
 * exporter) with synthetic pixels, then verify the artifacts on disk:
 * PNG signature + IHDR dimensions, and the JSON contract fields. */
static void ls_test_export_files(void)
{
    struct ls_export_cfg cfg;
    char png_path[64];
    char json_path[64];
    unsigned char rgba[4 * 2 * 4]; /* 2 cols x 1 row of 2x2 RGBA cells */
    unsigned char px[4];
    int i;
    FILE *f;
    unsigned char hdr[33];
    size_t n;

    snprintf(png_path, sizeof(png_path), "/tmp/ls_sheet_%d.png",
             (int)getpid());
    snprintf(json_path, sizeof(json_path), "/tmp/ls_sheet_%d.json",
             (int)getpid());

    /* Checkerboard the 4x2 sheet through the real pixel converter. */
    for (i = 0; i < 8; i++) {
        uint32_t argb = (i % 2 == 0) ? 0xFFAA1122u : 0xFF33BB44u;
        ls_pixel_to_rgba(argb, px);
        memcpy(&rgba[(size_t)i * 4], px, 4);
    }
    LS_CHECK(stbi_write_png(png_path, 4, 2, 4, rgba, 4 * 4) != 0);

    f = fopen(png_path, "rb");
    LS_CHECK(f != NULL);
    if (f != NULL) {
        n = fread(hdr, 1, sizeof(hdr), f);
        fclose(f);
        LS_CHECK(n == sizeof(hdr));
        /* PNG signature. */
        LS_CHECK(hdr[0] == 0x89 && hdr[1] == 'P' && hdr[2] == 'N' &&
                 hdr[3] == 'G');
        /* IHDR chunk: length(4) + "IHDR" then width/height big-endian. */
        LS_CHECK(hdr[12] == 'I' && hdr[13] == 'H' && hdr[14] == 'D' &&
                 hdr[15] == 'R');
        LS_CHECK(hdr[16] == 0 && hdr[17] == 0 && hdr[18] == 0 &&
                 hdr[19] == 4); /* width 4 */
        LS_CHECK(hdr[20] == 0 && hdr[21] == 0 && hdr[22] == 0 &&
                 hdr[23] == 2); /* height 2 */
    }

    ls_default_cfg(&cfg);
    cfg.cell_w = 2;
    cfg.cell_h = 2;
    cfg.cols = 2;
    cfg.rows = 1;
    cfg.frame_ms = 180;
    cfg.mood_count = 1;
    snprintf(cfg.moods[0], LS_MAX_MOOD_NAME, "%s", "idle");
    LS_CHECK(ls_write_json(json_path, &cfg, "sheet.png") == LS_OK);

    f = fopen(json_path, "r");
    LS_CHECK(f != NULL);
    if (f != NULL) {
        char buf[2048];
        n = fread(buf, 1, sizeof(buf) - 1, f);
        fclose(f);
        buf[n] = '\0';
        LS_CHECK(strstr(buf, "\"cell_w\": 2") != NULL);
        LS_CHECK(strstr(buf, "\"cell_h\": 2") != NULL);
        LS_CHECK(strstr(buf, "\"cols\": 2") != NULL);
        LS_CHECK(strstr(buf, "\"rows\": 1") != NULL);
        LS_CHECK(strstr(buf, "\"frame_ms\": 180") != NULL);
        LS_CHECK(strstr(buf, "\"image\": \"sheet.png\"") != NULL);
        /* Second frame sits at x = 1 cell * 2 px. */
        LS_CHECK(strstr(buf, "\"index\": 1") != NULL);
        LS_CHECK(strstr(buf, "\"x\": 2, \"y\": 0") != NULL);
        LS_CHECK(strstr(buf, "\"mood\": \"idle\"") != NULL);
    }
    remove(png_path);
    remove(json_path);
}

static void ls_test_config(void)
{
    struct ls_export_cfg cfg;
    char path[64];
    snprintf(path, sizeof(path), "/tmp/ls_cfg_%d.cfg", (int)getpid());

    /* Good config. */
    ls_write_tmp_cfg(path,
                     "# comment\n\ncell_w=96\ncell_h=192\ncols=4\nrows=6\n"
                     "frame_ms=180\nmoods=idle,blush,wink\n"
                     "basename=seraphine_sheet_fullbody\n"
                     "output_dir=/tmp/ls-out\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_OK);
    LS_CHECK(cfg.cell_w == 96 && cfg.cell_h == 192);
    LS_CHECK(cfg.cols == 4 && cfg.rows == 6 && cfg.frame_ms == 180);
    LS_CHECK(cfg.mood_count == 3);
    LS_CHECK(strcmp(cfg.moods[0], "idle") == 0);
    LS_CHECK(strcmp(cfg.moods[2], "wink") == 0);
    LS_CHECK(strcmp(cfg.basename, "seraphine_sheet_fullbody") == 0);
    LS_CHECK(strcmp(cfg.output_dir, "/tmp/ls-out") == 0);

    /* Missing file keeps defaults. */
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, "/tmp/ls_cfg_missing_xyz.cfg") ==
             LS_OK);
    LS_CHECK(cfg.cell_w == 96 && cfg.mood_count == 6);

    /* Bad values are rejected and leave defaults in place. */
    ls_write_tmp_cfg(path, "cell_w=banana\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);
    LS_CHECK(cfg.cell_w == 96); /* unchanged */

    /* Unknown keys are rejected: typos must not silently keep defaults. */
    ls_write_tmp_cfg(path, "cel_w=96\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);

    /* cols*rows over the frame cap is rejected. */
    ls_write_tmp_cfg(path, "cols=64\nrows=64\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);

    /* Out-of-range ints are rejected. */
    ls_write_tmp_cfg(path, "cell_w=0\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);
    ls_write_tmp_cfg(path, "cell_w=99999\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);

    /* Bad mood tokens are rejected. */
    ls_write_tmp_cfg(path, "moods=idle,not a mood!\n");
    ls_default_cfg(&cfg);
    LS_CHECK(ls_load_config(NULL, &cfg, path) == LS_ERR_CONFIG);

    remove(path);
}

int main(void)
{
    ls_test_blink();
    ls_test_shift();
    ls_test_pixel();
    ls_test_config();
    ls_test_export_files();
    if (ls_test_failures == 0) {
        printf("vs-lightshow unit tests: all passed\n");
        return 0;
    }
    printf("vs-lightshow unit tests: %d FAILURES\n", ls_test_failures);
    return 1;
}

#endif /* VS_LIGHTSHOW_UNIT_TEST */
