# 2026-10-11 — Overnight campaign pass: all five plugin crates tested green, Lua scripts parse, SCIP indexed, .gitignore created

Facts established during the overnight completion pass (verified on primo):

- All five VoidSprite plugin crates build and test green under their
  pinned toolchains (every pin is installed on primo):
  vs-defringe 8/8 (1.96.0), vs-palette-variants 9/9 (1.96.0),
  vs-sheet-qa 25/25 (no toolchain file; default), vs-lightshow 13/13
  (nightly-2026-09-25), vs-character-creator 35/35
  (nightly-2026-04-16) — **90 tests, 0 failures** total.
- The five Aseprite Lua scripts all parse cleanly (`luac -p`).
  They cannot be executed outside Aseprite (they use the `app` API),
  so parse-checking is the available gate here.
- The repo had no `.gitignore` at all: `cargo test` left a Cargo.lock
  and target/ in every crate, all untracked. A root `.gitignore` now
  covers `target/` and `Cargo.lock`, plus the SCIP artifacts below.
- SCIP: scip-lua only indexes a corpus's `lua/` directory, and this
  repo's Lua lives in `scripts/aseprite/`. Indexed via a scratch
  corpus with `lua/` symlinked to `scripts/aseprite/`: 5 files, 757
  definitions, 2667 occurrences (758 unresolved = the Aseprite `app`
  API). `index.scip` sits at the repo root, gitignored.
- README fix: in the layout tree, `vs-palette-variants` was drawn
  with the last-child glyph, leaving `vs-character-creator` looking
  nested under it; corrected to a mid-list glyph.
- `current.md` carried the language-mirror template; refreshed.
