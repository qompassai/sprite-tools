# Current Work

sprite-tools — the pixel-art toolchain behind light-show: five Rust
VoidSprite plugins (cdylib crates under `plugins/voidsprite/`), five
Aseprite Lua automation scripts (`scripts/aseprite/`), and the skill
docs for developing both (`skills/`).

## Status

- All five plugin crates verified 2026-10-11: `cargo test` green
  across the board — 90 tests, 0 failures (each crate under its
  pinned toolchain).
- Aseprite scripts parse clean (`luac -p`); they run only inside
  Aseprite.
- Root `.gitignore` added this pass (target/, Cargo.lock, SCIP
  artifacts were previously unignored).
- `index.scip` is a generated, gitignored artifact at the repo root
  covering the Lua scripts (see the inbox note for the corpus
  workaround scip-lua needs for this layout).

## Standing rules

- License: Apache-2.0 (LICENSE at repo root).
- Code changes are pushed only under Matt's explicit direction
  (the 2026-10-11 overnight campaign authorization covered this pass).
