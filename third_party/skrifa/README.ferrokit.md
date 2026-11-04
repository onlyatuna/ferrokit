# skrifa, vendored by ferrokit

* Upstream: `skrifa` 0.49.0 from crates.io (<https://github.com/googlefonts/fontations>, `skrifa`
  directory, commit `47a301615fd95acf0da9fb7dbcd0ea7c7df36943`).
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, unchanged). The upstream README is
  `README.md`.
* Used through `[patch.crates-io]` in the workspace `Cargo.toml` by `ferrokit-gui`
  (`crates/ferrokit-gui/src/text/skrifa_face.rs`), together with the vendored `../read-fonts`.

## Why

ferrokit's Linux glyph backend has to produce the glyph bitmaps of Qt's FreeType engine, and the
FreeType that the reference Qt links is 2.14.2. skrifa's TrueType interpreter follows an older
FreeType (2.13) in one place and a newer one (2.15) in another; with the change below its hinted
outlines equal FreeType 2.14.2's point for point.

## Changes against upstream

1. TrueType interpreter, FreeType 2.14 arithmetic for moves along a vector that is neither axis
   (`src/outline/glyf/hint/{graphics,projection,zone}.rs`):
   * `GraphicsState::update_projection_state` ports FreeType 2.14.2's `Compute_Funcs`: `F_dot_P` is
     rounded (`+ 0x2000`), there is a `moveVector` (freedom vector over `F_dot_P` as 16.16, new field
     `move_vector`), and a tiny `F_dot_P` zeroes that vector. Upstream clamps `fdotp` to 1.0 below
     `0x400` instead.
   * `move_original`, `move_point` and `point_displacement` move by `FT_MulFix(distance,
     moveVector)` (`mul_fix`) instead of `FT_MulDiv(distance, v, F_dot_P)` (FreeType 2.13).
   * The Comic Sans MS exception of `move_point` (a move measured along a horizontal projection
     vector is made even in backward-compatibility mode) is removed: FreeType added it in 2.15.0, not
     in 2.14. Measured with a `ttinterp.c` bisection on arialbd.ttf and arialbi.ttf, glyph `×`.
   Result against FreeType 2.14.2 `FT_LOAD_TARGET_NORMAL`: 73,344 of 73,344 TrueType glyph renders
   (24 fonts, sizes 9 to 24) have the same outline segments; without it 26 glyph renders differ
   (`×` in arialbd and arialbi, `ø` in ariali).
2. `Cargo.toml`: the `[[test]]` targets and the `[dev-dependencies]` are removed and the `tests/`,
   `scripts/` and `Cargo.lock` of the package are not vendored. Unit tests inside `src/` are
   untouched; the ones that pin the replaced arithmetic (`zone.rs`) are stale and are not run.

## Not implemented here (a known difference)

FreeType 2.14.0 added a vertical separation adjustment to its auto-hinter (`afadjust.c`, commit
`7099b09e9`, "[autofit] Implement vertical separation adjustment"). skrifa's auto-hinter has no such
code, and neither does this copy. It affects Light-hinted TrueType glyphs with accents and the
dots of `i` and `j`; see the module documentation of `skrifa_face.rs`.

To update: copy the new crates.io release and redo change 1 (about 30 lines), then run the
FreeType comparison tests of `ferrokit-gui` (`text::skrifa_face`).
