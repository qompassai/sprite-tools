#!/bin/sh
# Generate voidsprite_sdk_c.h — a C-compatible transcription of the vendored
# C++ SDK header (voidsprite_sdk.h, VoidSprite SDK v1).
#
# Transformations (mechanical, layout-preserving):
#   1. Strip C++ default member initializers (" = 0") from the
#      function-pointer table. Initializers do not affect struct layout,
#      so struct voidspriteSDK keeps the exact shape the host fills in.
#   2. Add <stdbool.h> for the bool-typed members.
#   3. Add C typedefs for the bare struct names the header uses
#      ("VSPLayer *" etc.), which are only valid C++ as written.
#   4. Drop the extern "C" braces (linkage specifiers are C++-only;
#      C has no name mangling, so nothing is lost).
#   5. Spell binary constants in hex (0b01 -> 0x01): same values,
#      no -Wpedantic noise in C17 mode.
# Struct tags, member order, member types, and constants are unchanged.
set -eu
cd "$(dirname "$0")"
SRC=voidsprite_sdk.h
OUT=voidsprite_sdk_c.h
{
printf '%s\n' \
'/*' \
' * voidsprite_sdk_c.h — GENERATED, do not edit.' \
' *' \
' * Mechanical C transcription of the vendored voidsprite_sdk.h' \
' * (VoidSprite SDK v1, counter185/voidsprite). Regenerate with' \
' * ./gen_c_header.sh after replacing voidsprite_sdk.h, then diff to' \
' * confirm only the documented transformations changed.' \
' */' \
'' \
'#pragma once' \
'' \
'#include <stdbool.h>' \
'#include <stdint.h>' \
'#include <stdio.h>' \
'' \
'/* C shims for the C++-spelled struct tags used bare below. */' \
'typedef struct VSPLayer VSPLayer;' \
'typedef struct VSPFilter VSPFilter;' \
'typedef struct VSPFileExporter VSPFileExporter;' \
'typedef struct VSPEditorContext VSPEditorContext;' \
'typedef struct VSPBrush VSPBrush;' \
'typedef struct VSPLayerInfo VSPLayerInfo;' \
'typedef struct voidspriteSDK voidspriteSDK;' \
''
sed -e 's/ = 0;$/;/' \
    -e 's/0b01/0x01/' -e 's/0b10/0x02/' \
    -e '/^extern "C" {$/d' -e '/^}$/d' \
    -e '/^#pragma once$/d' \
    -e '/^#include <stdint.h>$/d' \
    -e '/^#include <stdio.h>$/d' \
    "$SRC"
} > "$OUT"
echo "generated $OUT"
