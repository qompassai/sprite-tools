#!/bin/sh
# Build vs_lightshow.so — the "light-show sprite tools" VoidSprite plugin.
# Tiger Style: the .so must compile clean under the strict warning set,
# and the unit-test binary must pass under ASan/UBSan.
set -eu
cd "$(dirname "$0")"

./gen_c_header.sh

echo "== building vs_lightshow.so =="
cc -std=c17 -O2 -shared -fPIC -fvisibility=hidden \
  -Wall -Wextra -Wpedantic -Wconversion -Wshadow \
  -isystem third_party \
  -o vs_lightshow.so vs_lightshow.c

echo "== building + running unit tests (ASan/UBSan) =="
cc -std=c17 -O1 -g -fsanitize=address,undefined \
  -Wall -Wextra -Wpedantic -Wconversion -Wshadow \
  -isystem third_party -DVS_LIGHTSHOW_UNIT_TEST \
  -o vs_lightshow_test vs_lightshow.c
./vs_lightshow_test

echo "== exported symbols =="
nm -D vs_lightshow.so | grep ' T '
