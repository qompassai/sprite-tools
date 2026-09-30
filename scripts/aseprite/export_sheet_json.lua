-- export_sheet_json.lua
--
-- Batch-mode Aseprite script: export a light-show companion sprite sheet
-- plus a JSON sidecar documenting the contract (mirrors
-- game/src/waifu/sprite.rs). The JSON field names match the vs-lightshow
-- plugin exporter: cell_w, cell_h, cols, rows, sheet_w, sheet_h,
-- frame_ms, moods[], frames[] ({mood, frame, x, y, w, h} per cell).
--
-- NOTE: the game never parses this JSON; it builds its atlas from the
-- Rust constants. The sidecar is documentation only.
--
-- Params (via --script-param, before --script):
--   input       : sheet PNG or .aseprite (must be 384x1152 RGB).
--   output_base : basename for outputs, e.g. /tmp/x/seraphine_sheet
--
-- Outputs:
--   <output_base>.png  : the sheet, written via the Aseprite image API.
--   <output_base>.json : NOT written to disk by this script. Aseprite's
--     batch Lua cannot reliably write files (os.exit/os.tmpname are
--     unavailable; io.open/os.execute may prompt for permission and hang
--     headless), so the JSON is printed to stdout between marker lines;
--     the caller redirects it, e.g.:
--       aseprite -b --script-param input=... --script-param output_base=... \
--           --script export_sheet_json.lua \
--           | sed -n '/^---BEGIN /,/^---END /p' | sed '1d;$d' > sheet.json
--
-- If the input carries tags they must be exactly the six canonical mood
-- tags in row order; any mismatch aborts with error() (fail loudly --
-- the sidecar must never describe a non-conforming sheet).
--
-- Minimum: Aseprite 1.3 with scripting API >= 25 (json.encode arrived in
-- 1.3-rc5 / API 25; probed 41 on 1.3.18.6-dev).

local API_VERSION_MIN = 25
if app.apiVersion == nil or app.apiVersion < API_VERSION_MIN then
    error('export_sheet_json: needs Aseprite scripting API >= ' .. API_VERSION_MIN
        .. ', have ' .. tostring(app.apiVersion))
end

local CELL_W, CELL_H = 96, 192
local COLS, ROWS = 4, 6
local SHEET_W, SHEET_H = 384, 1152
local FRAME_MS = 180
local MOODS = { 'idle', 'blush', 'wink', 'pout', 'celebrate', 'alarmed' }
local FRAMES_PER_MOOD = 4

local params = app.params
local input = params.input
if type(input) ~= 'string' or input == '' then
    error('export_sheet_json: missing or empty param: input')
end
local output_base = params.output_base
if type(output_base) ~= 'string' or output_base == '' then
    error('export_sheet_json: missing or empty param: output_base')
end

local spr = app.open(input)
if spr == nil then
    error('export_sheet_json: cannot open: ' .. input)
end
if spr.colorMode ~= ColorMode.RGB then
    error('export_sheet_json: expected RGB color mode, got ' .. tostring(spr.colorMode))
end
if spr.width ~= SHEET_W or spr.height ~= SHEET_H then
    error(string.format('export_sheet_json: sheet is %dx%d, expected %dx%d',
        spr.width, spr.height, SHEET_W, SHEET_H))
end

-- Tags, if present, must be exactly the canonical mood set in row order.
if #spr.tags > 0 then
    if #spr.tags ~= #MOODS then
        error(string.format('export_sheet_json: %d tags present, expected %d',
            #spr.tags, #MOODS))
    end
    for m = 1, #MOODS do
        local tag = spr.tags[m]
        local want_first = (m - 1) * FRAMES_PER_MOOD + 1
        if tag.name ~= MOODS[m]
            or tag.frames ~= FRAMES_PER_MOOD
            or tag.fromFrame.frameNumber ~= want_first
            or tag.toFrame.frameNumber ~= want_first + FRAMES_PER_MOOD - 1 then
            error(string.format(
                "export_sheet_json: tag %d is '%s' frames %d-%d; expected '%s' frames %d-%d",
                m, tostring(tag.name), tag.fromFrame.frameNumber, tag.toFrame.frameNumber,
                MOODS[m], want_first, want_first + FRAMES_PER_MOOD - 1))
        end
    end
end

-- --- build the sidecar document ----------------------------------------------
---@type table[]
local frames = {}
for cell = 0, COLS * ROWS - 1 do
    local mood_idx = math.floor(cell / FRAMES_PER_MOOD) + 1
    frames[cell + 1] = {
        mood = MOODS[mood_idx],
        frame = cell % FRAMES_PER_MOOD, -- 0-based frame index within the mood
        x = (cell % COLS) * CELL_W,
        y = math.floor(cell / COLS) * CELL_H,
        w = CELL_W,
        h = CELL_H,
    }
end

local doc = {
    cell_w = CELL_W,
    cell_h = CELL_H,
    cols = COLS,
    rows = ROWS,
    sheet_w = SHEET_W,
    sheet_h = SHEET_H,
    frame_ms = FRAME_MS,
    moods = MOODS,
    frames = frames,
}

-- --- write the PNG (API save), print the JSON (stdout redirect) ---------------
local png_path = output_base .. '.png'
local sheet_img = Image(spr) -- composite of the current (first) frame
sheet_img:saveAs({ filename = png_path, palette = app.defaultPalette })
print('export_sheet_json: wrote ' .. png_path)
spr:close()

print('---BEGIN light-show sheet JSON (save stdout between markers as '
    .. output_base .. '.json)---')
print(json.encode(doc))
print('---END light-show sheet JSON---')
