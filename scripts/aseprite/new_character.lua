-- scripts/aseprite/new_character.lua
--
-- Parametric character creator for Aseprite.
--
-- Builds a 96x192 base sprite and a 24-frame mood sheet (4 columns x 6 rows
-- = 384x1152) with six mood tags -- idle, blush, wink, pout, celebrate,
-- alarmed -- 4 frames per mood at 180 ms per frame.
--
-- HONEST FRAMING: this is a richer parametric scaffold, still an artist's
-- starting point -- not finished art. Every body parameter below moves real
-- pixels (geometry, not just color), so the sheet is a usable base for an
-- artist to paint over.
--
-- Canonical parameter set (matches the VoidSprite plugin counterpart):
--   name, body_type=female|male, height=short|average|tall,
--   weight=slim|average|heavy, bust_size=small|medium|large (female only),
--   chest_size=small|medium|large (male only),
--   face=round|oval|square-jaw, hairstyle=ponytail|braids|bob|long,
--   hair_color=#rrggbb, eye_color=#rrggbb, accent=#rrggbb,
--   outfit=tech-jacket|casual|formal|athletic,
--   accessories=csv of visor|headphones|hairclip, output=<path> (headless)
--
-- Pixel mappings (all geometry derives from these):
--   height: head-to-body ratio short ~1:4 (chibi), average ~1:5.5,
--     tall ~1:7 (mature). Implemented as per-height skeleton landmarks:
--     head rect (short 36x48, average 28x36, tall 24x28), neck/torso/
--     legs/shoes y-bands that sum to 192.
--   weight: torso half-width slim=12, average=16, heavy=21 px; arm width
--     6/8/11; leg width 7/9/12.
--   bust_size (female): extra px added per side at the bust line:
--     small=0, medium=3, large=6.
--   chest_size (male): extra px added per side at the shoulders:
--     small=0, medium=3, large=6.
--   face: round = full corner rounding r4; oval = head narrowed 4 px,
--     rounding r6; square-jaw = top corners rounded r3, jaw left square.
--   outfit: tech-jacket (accent collar/zipper/belt), casual (tee + accent
--     emblem, short sleeves), formal (dark suit + white shirt + accent
--     tie), athletic (tank with cut-in shoulders + accent side stripes,
--     shorts). Garments are shaded derivatives of the accent color
--     (base 0.5, dark trim 0.35, formal suit 0.3); the bright accent
--     carries the details so they always read against the garments.
--
-- Two modes share one generator, generate(cfg):
--   Interactive (default, GUI only): Dialog with comboboxes (body_type,
--     hairstyle, face, outfit), color pickers (hair/eyes/accent), sliders
--     (weight, height, bust_size, chest_size), accessory checkboxes, a name
--     entry, and a Generate button. Flipping body_type hides the
--     inapplicable bust/chest slider via Dialog:modify{visible=...}; if a
--     future Aseprite ever stops honoring that, Generate-time validation
--     still rejects the invalid combo loudly, so the dialog can never
--     produce a bad character. NOTE: Dialog() returns nil in batch mode,
--     so this path cannot render under -b; it is covered by code review
--     plus the shared generation core exercised headless.
--   Headless (batch): the batch gate path.
--     aseprite -b --script-param headless=1 \
--       --script-param name=Nova --script-param body_type=female \
--       --script-param height=short --script-param weight=slim \
--       --script-param bust_size=small --script-param face=round \
--       --script-param hairstyle=braids --script-param hair_color=#3a2a1a \
--       --script-param eye_color=#4a7ac8 --script-param accent=#c8a028 \
--       --script-param outfit=tech-jacket --script-param accessories=hairclip \
--       --script-param output=/tmp/nova_sheet.aseprite \
--       --script new_character.lua
--     Every param is validated; bad input fails loudly (non-zero exit).
--     Invalid combos abort: bust_size with body_type=male, chest_size with
--     body_type=female, unknown enum values. The sheet is saved to
--     <output>; the base sprite to a sibling path derived by inserting
--     "_base" before the .aseprite extension.
--
-- Batch contract observed: no UI calls in headless mode (status via
-- print, failures via error/non-zero exit), input via app.params,
-- app.sprite is never assumed (sprites are created, not opened),
-- colorMode asserted RGB before any pixel math, all document mutations
-- inside app.transaction, app.apiVersion guard, no deprecated aliases
-- (app.activeSprite & friends are never used), no os.exit/os.tmpname,
-- no os.execute/io.open.

-- ---------------------------------------------------------------------------
-- Constants
-- ---------------------------------------------------------------------------

local BASE_W, BASE_H = 96, 192
local SHEET_COLS, SHEET_ROWS = 4, 6
local SHEET_W, SHEET_H = BASE_W * SHEET_COLS, BASE_H * SHEET_ROWS -- 384x1152
local FRAMES_PER_MOOD = 4
local FRAME_COUNT = SHEET_COLS * SHEET_ROWS -- 24
local FRAME_DURATION_S = 0.18 -- 180 ms per frame
local API_VERSION_MIN = 18 -- Dialog() returns nil in batch since apiVersion 18
local CENTER_X = 48

local MOODS = { 'idle', 'blush', 'wink', 'pout', 'celebrate', 'alarmed' }

local BODY_TYPE_IDS = { 'female', 'male' }
local BODY_TYPE_SET = { female = true, male = true }
local HAIRSTYLE_IDS = { 'ponytail', 'braids', 'bob', 'long' }
local HAIRSTYLE_SET = { ponytail = true, braids = true, bob = true, long = true }
local FACE_IDS = { 'round', 'oval', 'square-jaw' }
local FACE_SET = { round = true, oval = true, ['square-jaw'] = true }
local WEIGHT_IDS = { 'slim', 'average', 'heavy' }
local WEIGHT_SET = { slim = true, average = true, heavy = true }
local HEIGHT_IDS = { 'short', 'average', 'tall' }
local HEIGHT_SET = { short = true, average = true, tall = true }
local BUST_IDS = { 'small', 'medium', 'large' }
local BUST_SET = { small = true, medium = true, large = true }
local CHEST_IDS = { 'small', 'medium', 'large' }
local CHEST_SET = { small = true, medium = true, large = true }
local OUTFIT_IDS = { 'tech-jacket', 'casual', 'formal', 'athletic' }
local OUTFIT_SET = {
  ['tech-jacket'] = true, casual = true, formal = true, athletic = true,
}
local ACCESSORY_IDS = { 'visor', 'headphones', 'hairclip' }
local ACCESSORY_SET = { visor = true, headphones = true, hairclip = true }

local NAME_MAX = 64 -- characters
local CONFETTI_COUNT = 10 -- celebrate confetti pixels per frame
local BOB_Y = { 0, -2, 0, 2 } -- per-frame vertical bob inside each mood

-- Height skeletons: y-landmarks summing to 192. Head-to-body ratio:
-- short ~1:4 (chibi), average ~1:5.5, tall ~1:7 (mature).
local HEIGHT_SPECS = {
  short = {
    head_x = 30, head_y = 56, head_w = 36, head_h = 48,
    neck_y = 104, neck_h = 6, torso_y = 110, torso_h = 32,
    leg_y = 142, leg_h = 30, shoe_y = 172, shoe_h = 12,
  },
  average = {
    head_x = 34, head_y = 40, head_w = 28, head_h = 36,
    neck_y = 76, neck_h = 6, torso_y = 82, torso_h = 46,
    leg_y = 128, leg_h = 44, shoe_y = 172, shoe_h = 12,
  },
  tall = {
    head_x = 36, head_y = 34, head_w = 24, head_h = 28,
    neck_y = 62, neck_h = 6, torso_y = 68, torso_h = 56,
    leg_y = 124, leg_h = 48, shoe_y = 172, shoe_h = 12,
  },
}

-- Weight: torso half-width and limb widths, in pixels.
local WEIGHT_SPECS = {
  slim = { torso_hw = 12, arm_w = 6, leg_w = 7 },
  average = { torso_hw = 16, arm_w = 8, leg_w = 9 },
  heavy = { torso_hw = 21, arm_w = 11, leg_w = 12 },
}

-- Bust/chest: extra pixels added PER SIDE at the bust/shoulder line.
-- small is the baseline (0).
local BUST_ADD = { small = 0, medium = 3, large = 6 }
local CHEST_ADD = { small = 0, medium = 3, large = 6 }

-- Face variants: dw adjusts head width (negative narrows), corner is the
-- corner-rounding radius in px, jaw_square leaves the jaw corners square.
local FACE_SPECS = {
  round = { dw = 0, corner = 4, jaw_square = false },
  oval = { dw = -4, corner = 6, jaw_square = false },
  ['square-jaw'] = { dw = 0, corner = 3, jaw_square = true },
}

-- Fixed scaffold tones; user colors drive hair/eyes/accent(+outfit shades).
local SKIN = { r = 235, g = 196, b = 164 }
local WHITE = { r = 245, g = 245, b = 245 }
local DARK = { r = 58, g = 52, b = 58 }
local MOUTH = { r = 122, g = 62, b = 66 }
local BLUSH = { r = 255, g = 150, b = 170 }

-- ---------------------------------------------------------------------------
-- Types
-- ---------------------------------------------------------------------------

---@class Rgb 0-255 channel triple
---@field r integer
---@field g integer
---@field b integer

---@class CharConfig validated character description
---@field name string
---@field body_type string 'female'|'male'
---@field height string one of HEIGHT_IDS
---@field weight string one of WEIGHT_IDS
---@field bust_size string|nil one of BUST_IDS (female only)
---@field chest_size string|nil one of CHEST_IDS (male only)
---@field face string one of FACE_IDS
---@field hairstyle string one of HAIRSTYLE_IDS
---@field hair_color Rgb
---@field eye_color Rgb
---@field accent Rgb
---@field outfit string one of OUTFIT_IDS
---@field accessories table<string, boolean> set drawn from ACCESSORY_IDS
---@field output string|nil sheet path (headless mode only)

---@class Ink packed RGBA pixels derived from a CharConfig
---@field skin integer
---@field skin_dark integer
---@field hair integer
---@field eyes integer
---@field outfit integer garment base (accent shaded)
---@field outfit_dark integer garment trim (accent shaded)
---@field suit integer formal suit (accent shaded darker)
---@field accent integer bright details
---@field white integer
---@field dark integer
---@field mouth integer
---@field blush integer
---@field clear integer transparent, for erasing

---@class BodyGeom parametric skeleton, all values in pixels
---@field head_x integer
---@field head_y integer
---@field head_w integer
---@field head_h integer
---@field corner integer head corner-rounding radius
---@field jaw_square boolean
---@field neck_y integer
---@field neck_h integer
---@field torso_x integer
---@field torso_y integer
---@field torso_w integer
---@field torso_h integer
---@field bust integer extra px per side at bust line (female, else 0)
---@field shoulder integer extra px per side at shoulders (male, else 0)
---@field bust_y integer
---@field arm_w integer
---@field arm_x_l integer
---@field arm_x_r integer
---@field arm_y integer
---@field arm_h integer
---@field hand_h integer
---@field leg_w integer
---@field leg_x_l integer
---@field leg_x_r integer
---@field leg_y integer
---@field leg_h integer
---@field shoe_y integer
---@field shoe_h integer
---@field eye_x_l integer
---@field eye_x_r integer
---@field eye_y integer
---@field eye_w integer
---@field eye_h integer
---@field mouth_x integer
---@field mouth_y integer
---@field mouth_w integer
---@field cap_h integer hair cap height above head

-- ---------------------------------------------------------------------------
-- Pixel helpers (bounded, RGB-only; caller asserts colorMode)
-- ---------------------------------------------------------------------------

local TRANSPARENT = app.pixelColor.rgba(0, 0, 0, 0)

local function clamp_channel(v)
  if v < 0 then
    return 0
  end
  if v > 255 then
    return 255
  end
  return math.floor(v)
end

---@param c Rgb
---@param factor number channel multiplier, e.g. 0.8 darkens
---@return Rgb
local function shade(c, factor)
  return {
    r = clamp_channel(c.r * factor),
    g = clamp_channel(c.g * factor),
    b = clamp_channel(c.b * factor),
  }
end

---@param c Rgb
---@param alpha integer|nil 0-255, defaults to 255 (opaque)
---@return integer packed RGBA pixel
local function pack(c, alpha)
  local a = alpha
  if a == nil then
    a = 255
  end
  return app.pixelColor.rgba(c.r, c.g, c.b, a)
end

---@param cfg CharConfig
---@return Ink
local function resolve_colors(cfg)
  -- The outfit's garments are shaded derivatives of the single accent
  -- color; the bright accent itself carries collars, zippers, emblems,
  -- ties, stripes and soles so details always read against garments.
  return {
    skin = pack(SKIN),
    skin_dark = pack(shade(SKIN, 0.82)),
    hair = pack(cfg.hair_color),
    eyes = pack(cfg.eye_color),
    outfit = pack(shade(cfg.accent, 0.5)),
    outfit_dark = pack(shade(cfg.accent, 0.35)),
    suit = pack(shade(cfg.accent, 0.3)),
    accent = pack(cfg.accent),
    white = pack(WHITE),
    dark = pack(DARK),
    mouth = pack(MOUTH),
    blush = pack(BLUSH),
    clear = TRANSPARENT,
  }
end

-- Fill a rect, clamped to the image bounds. Zero/negative w/h fills nothing.
---@param img Image
---@param color integer packed pixel
local function fill_rect(img, x, y, w, h, color)
  local x0 = math.max(0, x)
  local y0 = math.max(0, y)
  local x1 = math.min(img.width - 1, x + w - 1)
  local y1 = math.min(img.height - 1, y + h - 1)
  for yy = y0, y1 do
    for xx = x0, x1 do
      img:drawPixel(xx, yy, color)
    end
  end
end

-- ---------------------------------------------------------------------------
-- Param validation: external input is validated here, never asserted.
-- Invalid combinations (bust+male, chest+female, unknown enums) abort.
-- ---------------------------------------------------------------------------

---@param s any
---@return Rgb|nil, string|nil
local function parse_hex_color(s)
  if type(s) ~= 'string' then
    return nil, 'color must be a string'
  end
  local r, g, b = s:match('^#?(%x%x)(%x%x)(%x%x)$')
  if r == nil then
    return nil, 'bad hex color "' .. s .. '" (want #rrggbb)'
  end
  return { r = tonumber(r, 16), g = tonumber(g, 16), b = tonumber(b, 16) }, nil
end

---@param s any
---@param set table<string, boolean>
---@param ids string[]
---@param what string param name for errors
---@return string|nil, string|nil
local function parse_enum(s, set, ids, what)
  if type(s) ~= 'string' or not set[s] then
    return nil, 'bad ' .. what .. ' "' .. tostring(s) .. '" (want one of: '
      .. table.concat(ids, ', ') .. ')'
  end
  return s, nil
end

---@param s any
---@return table<string, boolean>|nil, string|nil
local function parse_accessories(s)
  local set = {}
  if s == nil or s == '' then
    return set, nil -- accessories are optional; none is valid
  end
  if type(s) ~= 'string' then
    return nil, 'accessories must be a comma-separated string'
  end
  for token in s:gmatch('[^,]+') do
    local id = token:match('^%s*(.-)%s*$'):lower()
    if not ACCESSORY_SET[id] then
      return nil, 'bad accessory "' .. token .. '" (want csv of: '
        .. table.concat(ACCESSORY_IDS, ', ') .. ')'
    end
    set[id] = true
  end
  return set, nil
end

---@param s any
---@return string|nil, string|nil
local function parse_name(s)
  if type(s) ~= 'string' then
    return nil, 'name must be a string'
  end
  local name = s:match('^%s*(.-)%s*$')
  if name == '' then
    return nil, 'name is empty'
  end
  if #name > NAME_MAX then
    return nil, 'name longer than ' .. NAME_MAX .. ' characters'
  end
  return name, nil
end

---@param params table app.params
---@return CharConfig|nil, string|nil
local function config_from_params(params)
  local name, name_err = parse_name(params.name)
  if name == nil then
    return nil, name_err
  end

  local body_type, bt_err =
    parse_enum(params.body_type, BODY_TYPE_SET, BODY_TYPE_IDS, 'body_type')
  if body_type == nil then
    return nil, bt_err
  end

  local height, h_err = parse_enum(params.height, HEIGHT_SET, HEIGHT_IDS, 'height')
  if height == nil then
    return nil, h_err
  end

  local weight, w_err = parse_enum(params.weight, WEIGHT_SET, WEIGHT_IDS, 'weight')
  if weight == nil then
    return nil, w_err
  end

  -- bust/chest are mutually exclusive with the other body_type: loud abort.
  local bust_size, chest_size = nil, nil
  if body_type == 'female' then
    if params.chest_size ~= nil and params.chest_size ~= '' then
      return nil, 'chest_size is male-only; body_type=female'
    end
    bust_size, h_err = parse_enum(params.bust_size, BUST_SET, BUST_IDS, 'bust_size')
    if bust_size == nil then
      return nil, h_err
    end
  else
    if params.bust_size ~= nil and params.bust_size ~= '' then
      return nil, 'bust_size is female-only; body_type=male'
    end
    chest_size, h_err = parse_enum(params.chest_size, CHEST_SET, CHEST_IDS, 'chest_size')
    if chest_size == nil then
      return nil, h_err
    end
  end

  local face, f_err = parse_enum(params.face, FACE_SET, FACE_IDS, 'face')
  if face == nil then
    return nil, f_err
  end

  local hairstyle, hs_err =
    parse_enum(params.hairstyle, HAIRSTYLE_SET, HAIRSTYLE_IDS, 'hairstyle')
  if hairstyle == nil then
    return nil, hs_err
  end

  local colors = {}
  for _, key in ipairs({ 'hair_color', 'eye_color', 'accent' }) do
    local raw = params[key]
    if type(raw) ~= 'string' or raw == '' then
      return nil, 'missing --script-param ' .. key .. '=#rrggbb'
    end
    local c, c_err = parse_hex_color(raw)
    if c == nil then
      return nil, c_err
    end
    colors[key] = c
  end

  local outfit, o_err = parse_enum(params.outfit, OUTFIT_SET, OUTFIT_IDS, 'outfit')
  if outfit == nil then
    return nil, o_err
  end

  local accessories, acc_err = parse_accessories(params.accessories)
  if accessories == nil then
    return nil, acc_err
  end

  local output = params.output
  if type(output) ~= 'string' or output == '' then
    return nil, 'missing --script-param output=<path>'
  end

  return {
    name = name,
    body_type = body_type,
    height = height,
    weight = weight,
    bust_size = bust_size,
    chest_size = chest_size,
    face = face,
    hairstyle = hairstyle,
    hair_color = colors.hair_color,
    eye_color = colors.eye_color,
    accent = colors.accent,
    outfit = outfit,
    accessories = accessories,
    output = output,
  }, nil
end

-- Map a dialog slider value (0..2) to an enum id list.
---@param v any
---@param ids string[]
---@param what string
---@return string|nil, string|nil
local function slider_enum(v, ids, what)
  if type(v) ~= 'number' then
    return nil, what .. ' slider returned non-number'
  end
  local idx = math.floor(v + 0.5) + 1
  if ids[idx] == nil then
    return nil, what .. ' slider out of range'
  end
  return ids[idx], nil
end

---@param data table Dialog data table
---@return CharConfig|nil, string|nil
local function config_from_dialog_data(data)
  local name, name_err = parse_name(data.name)
  if name == nil then
    return nil, name_err
  end
  local body_type, bt_err =
    parse_enum(data.body_type, BODY_TYPE_SET, BODY_TYPE_IDS, 'body_type')
  if body_type == nil then
    return nil, bt_err
  end
  local height, h_err = slider_enum(data.height, HEIGHT_IDS, 'height')
  if height == nil then
    return nil, h_err
  end
  local weight, w_err = slider_enum(data.weight, WEIGHT_IDS, 'weight')
  if weight == nil then
    return nil, w_err
  end
  -- Generate-time backstop: even if the bust/chest slider toggle ever
  -- misbehaves, an invalid combination can never produce a character.
  local bust_size, chest_size = nil, nil
  if body_type == 'female' then
    bust_size, h_err = slider_enum(data.bust_size, BUST_IDS, 'bust_size')
    if bust_size == nil then
      return nil, h_err
    end
  else
    chest_size, h_err = slider_enum(data.chest_size, CHEST_IDS, 'chest_size')
    if chest_size == nil then
      return nil, h_err
    end
  end
  local face, f_err = parse_enum(data.face, FACE_SET, FACE_IDS, 'face')
  if face == nil then
    return nil, f_err
  end
  local hairstyle, hs_err =
    parse_enum(data.hairstyle, HAIRSTYLE_SET, HAIRSTYLE_IDS, 'hairstyle')
  if hairstyle == nil then
    return nil, hs_err
  end
  local colors = {}
  for _, key in ipairs({ 'hair_color', 'eye_color', 'accent' }) do
    local picked = data[key]
    if picked == nil then
      return nil, key .. ' color picker returned nothing'
    end
    -- Dialog color widgets yield Color objects with 0-255 channels.
    colors[key] = {
      r = clamp_channel(picked.red),
      g = clamp_channel(picked.green),
      b = clamp_channel(picked.blue),
    }
  end
  local outfit, o_err = parse_enum(data.outfit, OUTFIT_SET, OUTFIT_IDS, 'outfit')
  if outfit == nil then
    return nil, o_err
  end
  local accessories = {}
  for _, id in ipairs(ACCESSORY_IDS) do
    if data['acc_' .. id] == true then
      accessories[id] = true
    end
  end
  return {
    name = name,
    body_type = body_type,
    height = height,
    weight = weight,
    bust_size = bust_size,
    chest_size = chest_size,
    face = face,
    hairstyle = hairstyle,
    hair_color = colors.hair_color,
    eye_color = colors.eye_color,
    accent = colors.accent,
    outfit = outfit,
    accessories = accessories,
    output = nil, -- interactive mode: sprites stay open, user saves
  }, nil
end

-- ---------------------------------------------------------------------------
-- Parametric skeleton: every body pixel derives from cfg + these tables
-- ---------------------------------------------------------------------------

---@param cfg CharConfig
---@return BodyGeom
local function build_body_geometry(cfg)
  local hs = HEIGHT_SPECS[cfg.height]
  local ws = WEIGHT_SPECS[cfg.weight]
  local fs = FACE_SPECS[cfg.face]

  local head_x = hs.head_x - math.floor(fs.dw / 2)
  local head_w = hs.head_w + fs.dw
  local head_y, head_h = hs.head_y, hs.head_h

  local torso_x = CENTER_X - ws.torso_hw
  local torso_w = ws.torso_hw * 2
  local torso_y, torso_h = hs.torso_y, hs.torso_h

  local arm_w = ws.arm_w
  local arm_y = torso_y + 4
  local arm_h = torso_h - 2

  local leg_w = ws.leg_w

  local bust, shoulder = 0, 0
  if cfg.body_type == 'female' then
    bust = BUST_ADD[cfg.bust_size]
  else
    shoulder = CHEST_ADD[cfg.chest_size]
  end

  local eye_w = math.max(3, math.floor(head_w * 0.16))
  local eye_h = math.max(3, math.floor(head_h * 0.12))
  local eye_y = head_y + math.floor(head_h * 0.45)
  local mouth_w = math.max(4, math.floor(head_w * 0.25))

  return {
    head_x = head_x, head_y = head_y, head_w = head_w, head_h = head_h,
    corner = fs.corner, jaw_square = fs.jaw_square,
    neck_y = hs.neck_y, neck_h = hs.neck_h,
    torso_x = torso_x, torso_y = torso_y, torso_w = torso_w, torso_h = torso_h,
    bust = bust, shoulder = shoulder,
    bust_y = torso_y + math.floor(torso_h * 0.2),
    arm_w = arm_w,
    arm_x_l = torso_x - arm_w, arm_x_r = torso_x + torso_w,
    arm_y = arm_y, arm_h = arm_h, hand_h = 8,
    leg_w = leg_w,
    leg_x_l = CENTER_X - leg_w - 2, leg_x_r = CENTER_X + 2,
    leg_y = hs.leg_y, leg_h = hs.leg_h,
    shoe_y = hs.shoe_y, shoe_h = hs.shoe_h,
    eye_x_l = head_x + math.floor(head_w * 0.22),
    eye_x_r = head_x + math.floor(head_w * 0.62),
    eye_y = eye_y, eye_w = eye_w, eye_h = eye_h,
    mouth_x = head_x + math.floor((head_w - mouth_w) / 2),
    mouth_y = head_y + math.floor(head_h * 0.76),
    mouth_w = mouth_w,
    cap_h = math.floor(head_h * 0.45),
  }
end

-- ---------------------------------------------------------------------------
-- Base character drawing (96x192 scaffold; draw order back-to-front)
-- ---------------------------------------------------------------------------

local function draw_back_hair(img, cfg, ink, b)
  if cfg.hairstyle ~= 'long' then
    return
  end
  local top = b.head_y - b.cap_h
  local bottom = b.torso_y + math.floor(b.torso_h * 0.4)
  fill_rect(img, b.head_x - 6, top, b.head_w + 12, bottom - top, ink.hair)
end

local function draw_legs(img, cfg, ink, b)
  if cfg.outfit == 'athletic' then
    -- shorts: garment top, bare legs below
    local shorts_h = math.floor(b.leg_h * 0.4)
    fill_rect(img, b.leg_x_l, b.leg_y, b.leg_w, shorts_h, ink.outfit_dark)
    fill_rect(img, b.leg_x_r, b.leg_y, b.leg_w, shorts_h, ink.outfit_dark)
    fill_rect(img, b.leg_x_l, b.leg_y + shorts_h, b.leg_w, b.leg_h - shorts_h, ink.skin)
    fill_rect(img, b.leg_x_r, b.leg_y + shorts_h, b.leg_w, b.leg_h - shorts_h, ink.skin)
  else
    local pants = ink.outfit_dark
    if cfg.outfit == 'formal' then
      pants = ink.dark
    end
    fill_rect(img, b.leg_x_l, b.leg_y, b.leg_w, b.leg_h, pants)
    fill_rect(img, b.leg_x_r, b.leg_y, b.leg_w, b.leg_h, pants)
  end
  local sw = b.leg_w + 3
  fill_rect(img, b.leg_x_l - 1, b.shoe_y, sw, b.shoe_h, ink.dark)
  fill_rect(img, b.leg_x_r - 1, b.shoe_y, sw, b.shoe_h, ink.dark)
  fill_rect(img, b.leg_x_l - 1, b.shoe_y + b.shoe_h - 3, sw, 3, ink.accent)
  fill_rect(img, b.leg_x_r - 1, b.shoe_y + b.shoe_h - 3, sw, 3, ink.accent)
end

-- Torso top band, widened by bust (female) or shoulder (male) extras.
-- Exactly one of b.bust / b.shoulder is non-zero.
local function draw_torso_top(img, b, color)
  local extra = b.bust + b.shoulder
  fill_rect(img, b.torso_x - extra, b.torso_y, b.torso_w + 2 * extra, 14, color)
end

local function draw_outfit_tech_jacket(img, ink, b)
  draw_torso_top(img, b, ink.outfit)
  fill_rect(img, b.torso_x, b.torso_y + 14, b.torso_w, b.torso_h - 14, ink.outfit)
  fill_rect(img, b.torso_x, b.torso_y, b.torso_w, 5, ink.accent) -- collar
  fill_rect(img, CENTER_X - 1, b.torso_y + 5, 2, b.torso_h - 13, ink.accent) -- zip
  fill_rect(img, b.torso_x, b.torso_y + b.torso_h - 8, b.torso_w, 6, ink.accent) -- belt
  fill_rect(img, b.arm_x_l, b.arm_y, b.arm_w, b.arm_h, ink.outfit_dark)
  fill_rect(img, b.arm_x_r, b.arm_y, b.arm_w, b.arm_h, ink.outfit_dark)
  fill_rect(img, b.arm_x_l, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
  fill_rect(img, b.arm_x_r, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
end

local function draw_outfit_casual(img, ink, b)
  draw_torso_top(img, b, ink.outfit)
  fill_rect(img, b.torso_x, b.torso_y + 14, b.torso_w, b.torso_h - 14, ink.outfit)
  fill_rect(img, CENTER_X - 3, b.torso_y + 10, 6, 6, ink.accent) -- emblem
  local sleeve_h = math.floor(b.arm_h / 2) -- short sleeves, bare forearms
  fill_rect(img, b.arm_x_l, b.arm_y, b.arm_w, sleeve_h, ink.outfit_dark)
  fill_rect(img, b.arm_x_r, b.arm_y, b.arm_w, sleeve_h, ink.outfit_dark)
  fill_rect(img, b.arm_x_l, b.arm_y + sleeve_h, b.arm_w, b.arm_h - sleeve_h, ink.skin)
  fill_rect(img, b.arm_x_r, b.arm_y + sleeve_h, b.arm_w, b.arm_h - sleeve_h, ink.skin)
  fill_rect(img, b.arm_x_l, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
  fill_rect(img, b.arm_x_r, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
end

local function draw_outfit_formal(img, ink, b)
  draw_torso_top(img, b, ink.suit)
  fill_rect(img, b.torso_x, b.torso_y + 14, b.torso_w, b.torso_h - 14, ink.suit)
  fill_rect(img, CENTER_X - 4, b.torso_y + 2, 8, 12, ink.white) -- shirt
  fill_rect(img, CENTER_X - 1, b.torso_y + 8, 3, 22, ink.accent) -- tie
  fill_rect(img, b.arm_x_l, b.arm_y, b.arm_w, b.arm_h, ink.suit)
  fill_rect(img, b.arm_x_r, b.arm_y, b.arm_w, b.arm_h, ink.suit)
  fill_rect(img, b.arm_x_l, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
  fill_rect(img, b.arm_x_r, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
end

local function draw_outfit_athletic(img, ink, b)
  draw_torso_top(img, b, ink.outfit)
  fill_rect(img, b.torso_x, b.torso_y + 14, b.torso_w, b.torso_h - 14, ink.outfit)
  fill_rect(img, b.torso_x, b.torso_y, 6, 10, ink.skin) -- cut-in shoulders
  fill_rect(img, b.torso_x + b.torso_w - 6, b.torso_y, 6, 10, ink.skin)
  fill_rect(img, b.torso_x, b.torso_y + 10, 2, b.torso_h - 10, ink.accent)
  fill_rect(img, b.torso_x + b.torso_w - 2, b.torso_y + 10, 2, b.torso_h - 10, ink.accent)
  fill_rect(img, b.arm_x_l, b.arm_y, b.arm_w, b.arm_h, ink.skin) -- bare arms
  fill_rect(img, b.arm_x_r, b.arm_y, b.arm_w, b.arm_h, ink.skin)
  fill_rect(img, b.arm_x_l, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
  fill_rect(img, b.arm_x_r, b.arm_y + b.arm_h, b.arm_w, b.hand_h, ink.skin)
end

local function draw_outfit(img, cfg, ink, b)
  local v = cfg.outfit
  if v == 'tech-jacket' then
    draw_outfit_tech_jacket(img, ink, b)
  elseif v == 'casual' then
    draw_outfit_casual(img, ink, b)
  elseif v == 'formal' then
    draw_outfit_formal(img, ink, b)
  elseif v == 'athletic' then
    draw_outfit_athletic(img, ink, b)
  else
    error('new_character: unknown outfit "' .. tostring(v) .. '"', 0)
  end
  -- bust line (female): a soft band across the widened top, in torso color
  if b.bust > 0 then
    local torso_color = ink.outfit
    if v == 'formal' then
      torso_color = ink.suit
    end
    fill_rect(img, b.torso_x - b.bust, b.bust_y, b.torso_w + 2 * b.bust, 12, torso_color)
    fill_rect(img, b.torso_x - b.bust, b.bust_y + 10, b.torso_w + 2 * b.bust, 2, ink.accent)
  end
  fill_rect(img, CENTER_X - 4, b.neck_y, 8, b.neck_h, ink.skin_dark)
end

local function draw_head(img, ink, b)
  fill_rect(img, b.head_x, b.head_y, b.head_w, b.head_h, ink.skin)
  local r = b.corner
  fill_rect(img, b.head_x, b.head_y, r, r, ink.clear)
  fill_rect(img, b.head_x + b.head_w - r, b.head_y, r, r, ink.clear)
  if not b.jaw_square then
    fill_rect(img, b.head_x, b.head_y + b.head_h - r, r, r, ink.clear)
    fill_rect(img, b.head_x + b.head_w - r, b.head_y + b.head_h - r, r, r, ink.clear)
  end
  fill_rect(img, b.head_x - 3, b.eye_y - 2, 3, 8, ink.skin) -- ears
  fill_rect(img, b.head_x + b.head_w, b.eye_y - 2, 3, 8, ink.skin)
end

local function draw_face(img, ink, b)
  fill_rect(img, b.eye_x_l, b.eye_y, b.eye_w, b.eye_h, ink.eyes)
  fill_rect(img, b.eye_x_r, b.eye_y, b.eye_w, b.eye_h, ink.eyes)
  img:drawPixel(b.eye_x_l + 1, b.eye_y + 1, ink.white) -- catchlights
  img:drawPixel(b.eye_x_r + 1, b.eye_y + 1, ink.white)
  fill_rect(img, b.mouth_x, b.mouth_y, b.mouth_w, 2, ink.mouth) -- smile
  img:drawPixel(b.mouth_x - 1, b.mouth_y - 1, ink.mouth)
  img:drawPixel(b.mouth_x + b.mouth_w, b.mouth_y - 1, ink.mouth)
end

local function draw_front_hair(img, cfg, ink, b)
  local style = cfg.hairstyle
  fill_rect(img, b.head_x - 4, b.head_y - b.cap_h, b.head_w + 8, b.cap_h + 2, ink.hair)
  fill_rect(img, b.head_x, b.head_y, b.head_w, 6, ink.hair) -- fringe
  if style == 'bob' then
    local ch = math.floor(b.head_h * 0.7)
    fill_rect(img, b.head_x - 7, b.head_y + 4, 8, ch, ink.hair)
    fill_rect(img, b.head_x + b.head_w - 1, b.head_y + 4, 8, ch, ink.hair)
  elseif style == 'long' then
    fill_rect(img, b.head_x - 7, b.head_y + 4, 8, b.head_h, ink.hair)
    fill_rect(img, b.head_x + b.head_w - 1, b.head_y + 4, 8, b.head_h, ink.hair)
  elseif style == 'ponytail' then
    local tx = b.head_x + b.head_w - 6
    fill_rect(img, tx - 3, b.head_y + 2, 9, 7, ink.accent) -- tie
    fill_rect(img, tx, b.head_y + 8, 8, 12, ink.hair) -- tail down-right
    fill_rect(img, tx + 1, b.head_y + 20, 9, 14, ink.hair)
    fill_rect(img, tx + 2, b.head_y + 34, 10, 16, ink.hair)
    fill_rect(img, tx + 3, b.head_y + 50, 9, 12, ink.hair)
  elseif style == 'braids' then
    local braid_top = b.head_y + 12
    local braid_len = (b.torso_y + 20) - braid_top
    fill_rect(img, b.head_x - 11, braid_top, 8, braid_len, ink.hair)
    fill_rect(img, b.head_x + b.head_w + 3, braid_top, 8, braid_len, ink.hair)
    for _, tie_y in ipairs({ b.head_y + 26, b.head_y + 42 }) do
      fill_rect(img, b.head_x - 11, tie_y, 8, 4, ink.accent)
      fill_rect(img, b.head_x + b.head_w + 3, tie_y, 8, 4, ink.accent)
    end
    fill_rect(img, b.head_x - 10, braid_top + braid_len - 6, 6, 6, ink.hair)
    fill_rect(img, b.head_x + b.head_w + 4, braid_top + braid_len - 6, 6, 6, ink.hair)
  end
end

local function draw_accessories(img, cfg, ink, b)
  local acc = cfg.accessories
  if acc.visor then
    fill_rect(img, b.head_x - 2, b.head_y + 8, b.head_w + 4, 6, ink.accent)
    fill_rect(img, b.head_x + math.floor(b.head_w / 2), b.head_y + 10,
      math.floor(b.head_w * 0.75), 5, ink.accent)
  end
  if acc.headphones then
    fill_rect(img, b.head_x - 2, b.head_y - b.cap_h - 6, b.head_w + 4, 6, ink.dark)
    fill_rect(img, b.head_x - 12, b.eye_y - 6, 10, 16, ink.accent)
    fill_rect(img, b.head_x + b.head_w + 2, b.eye_y - 6, 10, 16, ink.accent)
    fill_rect(img, b.head_x - 10, b.eye_y - 2, 6, 10, ink.dark)
    fill_rect(img, b.head_x + b.head_w + 4, b.eye_y - 2, 6, 10, ink.dark)
  end
  if acc.hairclip then
    fill_rect(img, b.head_x + b.head_w - 10, b.head_y - b.cap_h + 4, 8, 6, ink.accent)
    img:drawPixel(b.head_x + b.head_w - 9, b.head_y - b.cap_h + 5, ink.white)
    img:drawPixel(b.head_x + b.head_w - 8, b.head_y - b.cap_h + 5, ink.white)
  end
end

---@param cfg CharConfig
---@param ink Ink
---@param b BodyGeom
---@return Image 96x192 RGB character on a transparent background
local function build_base_image(cfg, ink, b)
  local img = Image(BASE_W, BASE_H, ColorMode.RGB)
  img:clear(TRANSPARENT)
  draw_back_hair(img, cfg, ink, b)
  draw_legs(img, cfg, ink, b)
  draw_outfit(img, cfg, ink, b)
  draw_head(img, ink, b)
  draw_face(img, ink, b)
  draw_front_hair(img, cfg, ink, b)
  draw_accessories(img, cfg, ink, b)
  return img
end

-- ---------------------------------------------------------------------------
-- Mood overlays: mutate a clone of the base image per mood + frame
-- ---------------------------------------------------------------------------

---@param img Image clone of the base image, mutated in place
---@param mood string one of MOODS
---@param frame_idx integer 1..FRAMES_PER_MOOD
---@param ink Ink
---@param b BodyGeom
local function apply_mood(img, mood, frame_idx, ink, b)
  if mood == 'idle' then
    return -- base frame as-is; only the composite bob animates
  elseif mood == 'blush' then
    fill_rect(img, b.eye_x_l - 2, b.eye_y + b.eye_h + 2, b.eye_w + 3, 3, ink.blush)
    fill_rect(img, b.eye_x_r - 2, b.eye_y + b.eye_h + 2, b.eye_w + 3, 3, ink.blush)
  elseif mood == 'wink' then
    fill_rect(img, b.eye_x_r, b.eye_y, b.eye_w, b.eye_h, ink.skin)
    fill_rect(img, b.eye_x_r, b.eye_y + math.floor(b.eye_h / 2), b.eye_w, 2, ink.mouth)
  elseif mood == 'pout' then
    fill_rect(img, b.mouth_x, b.mouth_y, b.mouth_w, 2, ink.skin)
    fill_rect(img, b.mouth_x - 1, b.mouth_y - 2, b.mouth_w + 2, 6, ink.mouth)
    fill_rect(img, b.mouth_x, b.mouth_y - 1, b.mouth_w, 4, ink.skin)
  elseif mood == 'celebrate' then
    fill_rect(img, b.eye_x_l, b.eye_y, b.eye_w, b.eye_h, ink.skin)
    fill_rect(img, b.eye_x_r, b.eye_y, b.eye_w, b.eye_h, ink.skin)
    fill_rect(img, b.eye_x_l, b.eye_y + 1, b.eye_w, 2, ink.mouth) -- happy eyes
    fill_rect(img, b.eye_x_r, b.eye_y + 1, b.eye_w, 2, ink.mouth)
    fill_rect(img, b.mouth_x, b.mouth_y, b.mouth_w, 2, ink.skin)
    fill_rect(img, b.mouth_x - 1, b.mouth_y - 2, b.mouth_w + 2, 5, ink.mouth)
    fill_rect(img, b.mouth_x, b.mouth_y - 1, b.mouth_w, 3, ink.skin)
    fill_rect(img, b.arm_x_l, b.arm_y, b.arm_w, b.arm_h + b.hand_h, ink.clear)
    fill_rect(img, b.arm_x_r, b.arm_y, b.arm_w, b.arm_h + b.hand_h, ink.clear)
    fill_rect(img, b.arm_x_l - 6, b.torso_y - 30, b.arm_w, 34, ink.outfit_dark)
    fill_rect(img, b.arm_x_r + 6, b.torso_y - 30, b.arm_w, 34, ink.outfit_dark)
    fill_rect(img, b.arm_x_l - 6, b.torso_y - 38, b.arm_w, 9, ink.skin)
    fill_rect(img, b.arm_x_r + 6, b.torso_y - 38, b.arm_w, 9, ink.skin)
    for i = 1, CONFETTI_COUNT do -- deterministic per frame
      local cx = (frame_idx * 31 + i * 53) % 90 + 3
      local cy = (frame_idx * 17 + i * 29) % 60 + 4
      if i % 2 == 0 then
        img:drawPixel(cx, cy, ink.accent)
      else
        img:drawPixel(cx, cy, ink.white)
      end
    end
  elseif mood == 'alarmed' then
    fill_rect(img, b.eye_x_l - 1, b.eye_y - 2, b.eye_w + 2, b.eye_h + 4, ink.white)
    fill_rect(img, b.eye_x_r - 1, b.eye_y - 2, b.eye_w + 2, b.eye_h + 4, ink.white)
    fill_rect(img, b.eye_x_l + 1, b.eye_y, 2, 2, ink.eyes)
    fill_rect(img, b.eye_x_r + 1, b.eye_y, 2, 2, ink.eyes)
    fill_rect(img, b.eye_x_l - 1, b.eye_y - 6, b.eye_w + 2, 2, ink.dark)
    fill_rect(img, b.eye_x_r - 1, b.eye_y - 6, b.eye_w + 2, 2, ink.dark)
    fill_rect(img, b.mouth_x, b.mouth_y, b.mouth_w, 2, ink.skin)
    fill_rect(img, b.mouth_x - 1, b.mouth_y - 2, b.mouth_w + 2, 7, ink.mouth)
    fill_rect(img, b.mouth_x, b.mouth_y - 1, b.mouth_w, 5, ink.skin)
  else
    error('new_character: unknown mood "' .. tostring(mood) .. '"', 0)
  end
end

-- ---------------------------------------------------------------------------
-- Sprite assembly
-- ---------------------------------------------------------------------------

---@param base_img Image 96x192 base character
---@return Sprite 96x192 single-frame base sprite
local function build_base_sprite(base_img)
  local spr = Sprite(BASE_W, BASE_H, ColorMode.RGB)
  assert(spr.colorMode == ColorMode.RGB, 'new_character: base sprite is not RGB')
  app.transaction('new_character base', function()
    spr.cels[1].image = base_img -- single undoable replacement
  end)
  return spr
end

---@param ink Ink
---@param base_img Image 96x192 base character
---@param b BodyGeom
---@return Sprite 384x1152, 24 frames, 6 mood tags at 180 ms
local function build_mood_sheet(ink, base_img, b)
  local spr = Sprite(SHEET_W, SHEET_H, ColorMode.RGB)
  assert(spr.colorMode == ColorMode.RGB, 'new_character: sheet sprite is not RGB')
  for _ = 2, FRAME_COUNT do
    spr:newEmptyFrame()
  end
  app.transaction('new_character sheet', function()
    for _, cel in ipairs(spr.cels) do
      spr:deleteCel(cel) -- deterministic: rebuild every cel below
    end
    local layer = spr.layers[1]
    for frame_num = 1, FRAME_COUNT do
      spr.frames[frame_num].duration = FRAME_DURATION_S
      local mood_idx = math.floor((frame_num - 1) / FRAMES_PER_MOOD) + 1
      local frame_idx = ((frame_num - 1) % FRAMES_PER_MOOD) + 1
      local mood_img = base_img:clone()
      apply_mood(mood_img, MOODS[mood_idx], frame_idx, ink, b)
      local cell = Image(SHEET_W, SHEET_H, ColorMode.RGB)
      cell:clear(TRANSPARENT)
      local cx = ((frame_num - 1) % SHEET_COLS) * BASE_W
      local cy = (mood_idx - 1) * BASE_H + BOB_Y[frame_idx]
      cell:drawImage(mood_img, cx, cy)
      spr:newCel(layer, frame_num, cell)
    end
    for mood_idx, mood in ipairs(MOODS) do
      local from = (mood_idx - 1) * FRAMES_PER_MOOD + 1
      -- NOTE: newTag() silently ignores a trailing name argument on
      -- Aseprite 1.3.18 (verified empirically: the tag kept the default
      -- name "Tag"); assign tag.name explicitly instead.
      local tag = spr:newTag(from, from + FRAMES_PER_MOOD - 1)
      tag.name = mood
    end
  end)
  return spr
end

-- Shared generation core: both modes validate into a CharConfig, then call
-- this. It creates (never opens) sprites, so app.sprite is never assumed.
---@param cfg CharConfig
---@return table { base: Sprite, sheet: Sprite }
local function generate(cfg)
  assert(cfg ~= nil, 'new_character: generate got nil cfg')
  local ink = resolve_colors(cfg)
  local body = build_body_geometry(cfg)
  local base_img = build_base_image(cfg, ink, body)
  local base_spr = build_base_sprite(base_img)
  local sheet_spr = build_mood_sheet(ink, base_img, body)
  return { base = base_spr, sheet = sheet_spr }
end

---@param output string sheet path
---@return string sibling path for the base sprite
local function derive_base_path(output)
  local stem = output:match('^(.*)%.aseprite$')
  if stem ~= nil then
    return stem .. '_base.aseprite'
  end
  return output .. '_base.aseprite'
end

-- ---------------------------------------------------------------------------
-- Modes
-- ---------------------------------------------------------------------------

---@param params table app.params
local function run_headless(params)
  local cfg, cfg_err = config_from_params(params)
  if cfg == nil then
    error('new_character: ' .. tostring(cfg_err), 0)
  end
  local sprites = generate(cfg)
  local base_path = derive_base_path(cfg.output)
  sprites.sheet:saveCopyAs(cfg.output)
  sprites.base:saveCopyAs(base_path)
  print('new_character: saved sheet ' .. cfg.output .. ' (384x1152, 24 frames, 6 tags)')
  print('new_character: saved base ' .. base_path .. ' (96x192)')
  sprites.sheet:close()
  sprites.base:close()
  print('new_character: OK name=' .. cfg.name)
end

local function run_interactive()
  if not app.isUIAvailable then
    error('new_character: no UI available; rerun with -b and headless=1', 0)
  end
  local dlg = Dialog('New Character')
  if dlg == nil then
    error('new_character: Dialog() returned nil', 0)
  end
  -- Flipping body_type hides the inapplicable bust/chest slider. This uses
  -- Dialog:modify{visible=...}; Generate-time validation is the backstop.
  dlg:combobox({
    id = 'body_type',
    label = 'Body type',
    option = 'female',
    options = BODY_TYPE_IDS,
    onchange = function()
      local show_bust = dlg.data.body_type == 'female'
      dlg:modify({ id = 'bust_size', visible = show_bust })
      dlg:modify({ id = 'chest_size', visible = not show_bust })
    end,
  })
  dlg:slider({ id = 'height', label = 'Height (short..tall)', min = 0, max = 2, value = 1 })
  dlg:slider({ id = 'weight', label = 'Weight (slim..heavy)', min = 0, max = 2, value = 1 })
  dlg:slider({ id = 'bust_size', label = 'Bust (small..large)', min = 0, max = 2, value = 1 })
  dlg:slider({ id = 'chest_size', label = 'Chest (small..large)', min = 0, max = 2, value = 1 })
  dlg:modify({ id = 'chest_size', visible = false }) -- default body_type=female
  dlg:combobox({ id = 'face', label = 'Face', option = 'round', options = FACE_IDS })
  dlg:combobox({
    id = 'hairstyle', label = 'Hairstyle', option = 'bob', options = HAIRSTYLE_IDS,
  })
  dlg:combobox({ id = 'outfit', label = 'Outfit', option = 'tech-jacket', options = OUTFIT_IDS })
  dlg:color({ id = 'hair_color', label = 'Hair', color = Color({ r = 58, g = 42, b = 26 }) })
  dlg:color({ id = 'eye_color', label = 'Eyes', color = Color({ r = 74, g = 122, b = 200 }) })
  dlg:color({ id = 'accent', label = 'Accent', color = Color({ r = 200, g = 160, b = 40 }) })
  dlg:check({ id = 'acc_visor', label = 'Visor', selected = false })
  dlg:check({ id = 'acc_headphones', label = 'Headphones', selected = false })
  dlg:check({ id = 'acc_hairclip', label = 'Hairclip', selected = false })
  dlg:entry({ id = 'name', label = 'Name', text = 'Nova' })
  dlg:separator()
  dlg:button({
    id = 'generate',
    text = 'Generate',
    onclick = function()
      local cfg, cfg_err = config_from_dialog_data(dlg.data)
      if cfg == nil then
        print('new_character: invalid input: ' .. tostring(cfg_err))
        return -- keep the dialog open so the user can fix the input
      end
      generate(cfg) -- sprites stay open in the editor; the user saves them
      print('new_character: generated "' .. cfg.name .. '" (base 96x192 + sheet 384x1152)')
      dlg:close()
    end,
  })
  dlg:button({
    id = 'cancel',
    text = 'Cancel',
    onclick = function()
      dlg:close()
    end,
  })
  dlg:show({ wait = true })
end

-- ---------------------------------------------------------------------------
-- Entry
-- ---------------------------------------------------------------------------

if app.apiVersion ~= nil and app.apiVersion < API_VERSION_MIN then
  error('new_character: needs Aseprite with apiVersion >= ' .. API_VERSION_MIN, 0)
end

local params = app.params
if params == nil then
  params = {}
end

if params.headless == '1' then
  run_headless(params)
else
  run_interactive()
end
