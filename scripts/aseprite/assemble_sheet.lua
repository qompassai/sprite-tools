-- assemble_sheet.lua
--
-- Batch-mode Aseprite script: assemble a light-show companion sprite sheet
-- from 24 per-cell PNG frames.
--
-- Contract (mirrors game/src/waifu/sprite.rs in the light-show repo):
--   96x192 cells, 4 columns x 6 rows -> 384x1152 sheet.
--   Mood rows in order: idle, blush, wink, pout, celebrate, alarmed;
--   4 frames per mood; 180 ms per frame.
--
-- Params (via --script-param, before --script):
--   frames_dir : directory holding exactly frame_00.png .. frame_23.png
--                (each 96x192, 32-bit RGB). Row-major: frame_00 is the
--                top-left cell (idle frame 1), frame_23 the bottom-right
--                cell (alarmed frame 4).
--   output     : sheet PNG path to write.
--
-- Outputs:
--   <output>           the assembled 384x1152 sheet PNG.
--   <output base>.aseprite  the same sheet as a 24-frame Aseprite document
--                carrying the six mood tags (idle frames 1-4, blush 5-8,
--                wink 9-12, pout 13-16, celebrate 17-20, alarmed 21-24),
--                each frame at 180 ms. PNG cannot store tags, so the
--                .aseprite sidecar preserves them.
--   (e.g. output=/tmp/x/sheet.png also writes /tmp/x/sheet.aseprite)
--
-- Failure: any missing/mis-sized/non-RGB frame, or any save failure,
-- aborts with error() -> non-zero exit. No UI calls; batch mode only.
--
-- Minimum: Aseprite 1.3 with scripting API >= 25 (probed 41 on 1.3.18.6-dev).

local API_VERSION_MIN = 25
if app.apiVersion == nil or app.apiVersion < API_VERSION_MIN then
    error('assemble_sheet: needs Aseprite scripting API >= ' .. API_VERSION_MIN
        .. ', have ' .. tostring(app.apiVersion))
end

local CELL_W, CELL_H = 96, 192
local COLS, ROWS = 4, 6
local FRAME_COUNT = COLS * ROWS -- 24
local SHEET_W, SHEET_H = CELL_W * COLS, CELL_H * ROWS -- 384 x 1152
local FRAME_DUR_S = 0.18 -- 180 ms; Aseprite frame.duration is in seconds
local MOODS = { 'idle', 'blush', 'wink', 'pout', 'celebrate', 'alarmed' }
local FRAMES_PER_MOOD = 4

-- --- param validation -------------------------------------------------------
local params = app.params
local frames_dir = params.frames_dir
if type(frames_dir) ~= 'string' or frames_dir == '' then
    error('assemble_sheet: missing or empty param: frames_dir')
end
local output = params.output
if type(output) ~= 'string' or output == '' then
    error('assemble_sheet: missing or empty param: output')
end
frames_dir = frames_dir:gsub('/+$', '') -- tolerate a trailing slash

-- --- load and validate the 24 frame images ----------------------------------
---@type Image[]
local cells = {}
for i = 0, FRAME_COUNT - 1 do
    local path = string.format('%s/frame_%02d.png', frames_dir, i)
    local ok, img = pcall(Image, { fromFile = path })
    if not ok or img == nil then
        error('assemble_sheet: cannot load frame image: ' .. path
            .. ' (' .. tostring(img) .. ')')
    end
    if img.width ~= CELL_W or img.height ~= CELL_H then
        error(string.format(
            'assemble_sheet: frame %s is %dx%d, expected %dx%d',
            path, img.width, img.height, CELL_W, CELL_H))
    end
    if img.bytesPerPixel ~= 4 then
        error('assemble_sheet: frame is not 32-bit RGB: ' .. path)
    end
    cells[i + 1] = img
end

-- --- compose the sheet -------------------------------------------------------
local sheet_img = Image(SHEET_W, SHEET_H, ColorMode.RGB)
sheet_img:clear(app.pixelColor.rgba(0, 0, 0, 0))
for i = 1, FRAME_COUNT do
    local col = (i - 1) % COLS
    local row = math.floor((i - 1) / COLS)
    sheet_img:drawImage(cells[i], Point(col * CELL_W, row * CELL_H))
end

-- --- build the 24-frame tagged sprite ----------------------------------------
local spr = Sprite(SHEET_W, SHEET_H, ColorMode.RGB)
app.transaction('assemble light-show sheet', function()
    for _ = 2, FRAME_COUNT do
        spr:newFrame()
    end
    local layer = spr.layers[1]
    for f = 1, FRAME_COUNT do
        spr.frames[f].duration = FRAME_DUR_S
        spr:newCel(layer, f, sheet_img:clone(), Point(0, 0))
    end
    for m = 1, #MOODS do
        local first = (m - 1) * FRAMES_PER_MOOD + 1
        local tag = spr:newTag(first, first + FRAMES_PER_MOOD - 1)
        tag.name = MOODS[m] -- newTag's name arg is ignored; set explicitly
    end
end)

-- --- save outputs --------------------------------------------------------------
local ase_path = output:gsub('%.png$', '') .. '.aseprite'
spr:saveCopyAs(ase_path)
print('assemble_sheet: wrote ' .. ase_path)
sheet_img:saveAs({ filename = output, palette = app.defaultPalette })
print('assemble_sheet: wrote ' .. output)
spr:close()
print('assemble_sheet: OK 24 frames, 6 tags, sheet ' .. SHEET_W .. 'x' .. SHEET_H)
