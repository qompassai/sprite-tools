-- build_montage.lua
--
-- Batch-mode Aseprite script: build a 2x nearest-neighbor montage of the
-- 24 sheet cells in grid order, for phone review of a light-show
-- companion sprite sheet.
--
-- Params (via --script-param, before --script):
--   input  : sheet PNG or .aseprite (must be 384x1152 RGB).
--   output : montage PNG path to write (768x2304).
--
-- Method: render frame 1, then scale each pixel to a 2x2 block by direct
-- pixel copy (nearest-neighbor is exact here -- no interpolation).
-- Work is bounded: exactly 384*1152 pixel copies, no UI, batch only.
--
-- Minimum: Aseprite 1.3 with scripting API >= 25 (probed 41 on 1.3.18.6-dev).

local API_VERSION_MIN = 25
if app.apiVersion == nil or app.apiVersion < API_VERSION_MIN then
    error('build_montage: needs Aseprite scripting API >= ' .. API_VERSION_MIN
        .. ', have ' .. tostring(app.apiVersion))
end

local SHEET_W, SHEET_H = 384, 1152
local SCALE = 2
local OUT_W, OUT_H = SHEET_W * SCALE, SHEET_H * SCALE -- 768 x 2304

local params = app.params
local input = params.input
if type(input) ~= 'string' or input == '' then
    error('build_montage: missing or empty param: input')
end
local output = params.output
if type(output) ~= 'string' or output == '' then
    error('build_montage: missing or empty param: output')
end

local spr = app.open(input)
if spr == nil then
    error('build_montage: cannot open: ' .. input)
end
if spr.colorMode ~= ColorMode.RGB then
    spr:close()
    error('build_montage: expected RGB color mode, got ' .. tostring(spr.colorMode))
end
if spr.width ~= SHEET_W or spr.height ~= SHEET_H then
    spr:close()
    error(string.format('build_montage: sheet is %dx%d, expected %dx%d',
        spr.width, spr.height, SHEET_W, SHEET_H))
end

local src = Image(spr) -- composite of the current (first) frame
spr:close()

local dst = Image(OUT_W, OUT_H, ColorMode.RGB)
for px in src:pixels() do
    local v = px() -- raw pixel int (RGB mode: 32-bit, channels not premultiplied)
    local dx, dy = px.x * SCALE, px.y * SCALE
    dst:drawPixel(dx, dy, v)
    dst:drawPixel(dx + 1, dy, v)
    dst:drawPixel(dx, dy + 1, v)
    dst:drawPixel(dx + 1, dy + 1, v)
end

dst:saveAs({ filename = output, palette = app.defaultPalette })
print('build_montage: wrote ' .. output .. ' (' .. OUT_W .. 'x' .. OUT_H .. ')')
