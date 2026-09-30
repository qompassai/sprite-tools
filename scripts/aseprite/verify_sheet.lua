-- verify_sheet.lua
--
-- Batch-mode Aseprite script: verify a light-show companion sprite sheet
-- against the contract (mirrors game/src/waifu/sprite.rs):
--   384x1152 sheet; if tags are present, exactly the six mood tags
--   (idle, blush, wink, pout, celebrate, alarmed) in row order, each
--   spanning exactly 4 frames, every frame at 180 ms.
--
-- Params (via --script-param, before --script):
--   input : a .aseprite document or a sheet PNG.
--
-- Report: one [PASS]/[FAIL]/[SKIP] line per check on stdout.
-- Any failure -> error() -> non-zero exit with specifics.
-- A sheet PNG carries no tags, so tag checks are skipped for PNG input.
--
-- Minimum: Aseprite 1.3 with scripting API >= 25 (probed 41 on 1.3.18.6-dev).

local API_VERSION_MIN = 25
if app.apiVersion == nil or app.apiVersion < API_VERSION_MIN then
    error('verify_sheet: needs Aseprite scripting API >= ' .. API_VERSION_MIN
        .. ', have ' .. tostring(app.apiVersion))
end

local SHEET_W, SHEET_H = 384, 1152
local FRAME_MS = 180
local MOODS = { 'idle', 'blush', 'wink', 'pout', 'celebrate', 'alarmed' }
local FRAMES_PER_MOOD = 4
local DUR_TOLERANCE_MS = 0.5 -- frame.duration is float seconds; allow 0.5 ms

local params = app.params
local input = params.input
if type(input) ~= 'string' or input == '' then
    error('verify_sheet: missing or empty param: input')
end

local spr = app.open(input)
if spr == nil then
    error('verify_sheet: cannot open: ' .. input)
end
if spr.colorMode ~= ColorMode.RGB then
    error('verify_sheet: expected RGB color mode, got ' .. tostring(spr.colorMode))
end

local failures = {}

---@param name string
---@param ok boolean
---@param detail string
local function check(name, ok, detail)
    if ok then
        print('[PASS] ' .. name .. (detail ~= '' and (' -- ' .. detail) or ''))
    else
        print('[FAIL] ' .. name .. (detail ~= '' and (' -- ' .. detail) or ''))
        failures[#failures + 1] = name .. ': ' .. detail
    end
end

check('dimensions', spr.width == SHEET_W and spr.height == SHEET_H,
    string.format('got %dx%d, expected %dx%d', spr.width, spr.height, SHEET_W, SHEET_H))

if #spr.tags == 0 then
    print('[SKIP] tags: sprite carries no tags (e.g. sheet PNG input)')
else
    check('tag count', #spr.tags == #MOODS,
        string.format('got %d tags, expected %d', #spr.tags, #MOODS))
    for m = 1, #MOODS do
        local tag = spr.tags[m]
        if tag == nil then
            check('tag ' .. m .. ' present', false, 'missing tag at position ' .. m)
        else
            check('tag ' .. m .. ' name', tag.name == MOODS[m],
                "got '" .. tostring(tag.name) .. "', expected '" .. MOODS[m] .. "'")
            check('tag ' .. m .. ' span (' .. MOODS[m] .. ')',
                tag.frames == FRAMES_PER_MOOD,
                'tag spans ' .. tostring(tag.frames) .. ' frames, expected ' .. FRAMES_PER_MOOD)
            local first_n = tag.fromFrame.frameNumber
            local last_n = tag.toFrame.frameNumber
            local want_first = (m - 1) * FRAMES_PER_MOOD + 1
            check('tag ' .. m .. ' range (' .. MOODS[m] .. ')',
                first_n == want_first and last_n == want_first + FRAMES_PER_MOOD - 1,
                string.format('got frames %d-%d, expected %d-%d',
                    first_n, last_n, want_first, want_first + FRAMES_PER_MOOD - 1))
            for f = first_n, last_n do
                local dur_ms = spr.frames[f].duration * 1000
                check(string.format('frame %d duration', f),
                    math.abs(dur_ms - FRAME_MS) <= DUR_TOLERANCE_MS,
                    string.format('got %.3f ms, expected %d ms', dur_ms, FRAME_MS))
            end
        end
    end
end

spr:close()

if #failures > 0 then
    print('verify_sheet: ' .. #failures .. ' check(s) failed')
    for _, f in ipairs(failures) do
        print('  - ' .. f)
    end
    error('verify_sheet: FAILED (' .. #failures .. ' failing check(s))')
end
print('verify_sheet: OK all checks passed for ' .. input)
