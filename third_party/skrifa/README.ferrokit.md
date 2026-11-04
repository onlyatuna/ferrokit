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
FreeType (2.13) in one place and a newer one (2.15) in another, and skrifa does not darken the
stems of CFF glyphs, which Qt's FreeType engine does. The changes below make its outlines equal
FreeType 2.14.2's.

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
2. Which hinter a TrueType font gets (`src/outline/glyf/mod.rs`, `prefer_interpreter`): FreeType 2.14.0
   changed the test of `FT_Load_Glyph` (ftobjs.c) that sends a TrueType font to the auto-hinter in
   `FT_LOAD_TARGET_NORMAL` mode from "no `fpgm`, an empty `prep` and `maxSizeOfInstructions == 0`"
   to "no `fpgm` and a `prep` of at most 7 bytes". The copy follows 2.14.2. It matters for fonts
   whose only bytecode is in glyph programs or in a `prep` of a few bytes.
3. CFF stem darkening, a port of FreeType 2.14.2's (`src/psaux/psfont.c`, `src/psaux/pshints.c`,
   `src/psaux/psblues.c`, `src/psaux/psft.c`). skrifa's docs say "We don't support stem
   darkening", but Qt's FreeType engine turns it on (`qt_getFreetypeData` sets the `cff` driver
   property `no-stem-darkening` to false), so the CFF glyphs of the reference Qt, hinted or not,
   are darkened. Off by default, so nothing changes for a caller that does not ask:
   * the switches are `HintingOptions::cff_stem_darkening` (new field, for the hinted glyphs of a
     `HintingInstance`), `DrawSettings::with_cff_stem_darkening` (for unhinted glyphs) and
     `cff::Outlines::subfont_with` (`subfont` is `subfont_with(.., false)`). Only outlines drawn
     for a size are darkened, as in FreeType (`scaled` in `cf2_decoder_parse_charstrings`);
   * `src/outline/cff/hint/darken.rs` (new): `Darkening::new` is `cf2_computeDarkening` and the
     `darkenX`/`darkenY` part of `cf2_font_setup` (parameters 500, 400, 1000, 275, 1667, 275, 2333, 0;
     `StdVW` and `StdHW` of the Private DICT, which `cff/mod.rs` now reads; the pixel size
     `y_ppem` is the 26.6 size rounded to whole pixels), and `GlyphPath` is `CF2_GlyphPath`
     (`cf2_glyphpath_computeOffset`, `computeIntersection`, `pushPrevElem`, `pushMove`, `moveTo`,
     `lineTo`, `curveTo`, `closeOpenPath`, with `cf2_getWindingMomentum`) in FreeType's integer
     16.16 arithmetic. skrifa's `HintingSink` converts each point as it comes; when edges move it
     drives that queue of one element instead, with the hint maps as FreeType delays them
     (`firstHintMap` for the closing line);
   * `src/outline/cff/hint.rs`: `HintState` has `darken_x`, `darken_y` (which was there as
     zero) and `stem_darkened`; `darken_y` moves the top blue zones, the em box hints, the top
     hint edges and the family blue flat edges up by twice its amount, and `stem_darkened` (not
     `darken_y != 0`, as in `cf2_blues_init`) turns off the blue boost. `HintingSink::unhinted` is
     the sink for glyphs drawn without hints: FreeType's engine still runs its glyph path then, at
     scale 1/64 and returning font units (`cf2_getScaleAndHintFlag`), and `cff_slot_load` applies
     the font matrix and the scale (`UnhintedScalingSink` in `cff/mod.rs`);
   * `cff::Outlines::draw` runs the charstring twice when an amount is not zero, like
     `cf2_getGlyphOutline`: the first run only measures the winding momentum and the second one
     darkens the other way when it is negative (a clockwise outline);
   * a `seac` component is darkened with its own glyph path and hint maps, as in FreeType; its
     hint origin (`hintOriginY` of `cf2_interpT2CharString`) stays zero as in upstream skrifa.
   Not darkened, as in FreeType: outlines that the auto-hinter loads (`FT_LOAD_NO_SCALE`), and
   TrueType ones.
4. `Cargo.toml`: the `[[test]]` targets and the `[dev-dependencies]` are removed and the `tests/`,
   `scripts/` and `Cargo.lock` of the package are not vendored. The unit tests inside `src/` need the
   `font-test-data` crate of the fontations repository, which is not vendored, so ferrokit's build
   does not run them. They are run by copying `src/` over a checkout of fontations at the commit above
   and running `cargo test -p skrifa -p read-fonts` there: all pass (skrifa 480 unit tests). Tests
   edited for the changes above: `outline/glyf/hint/zone.rs` (the horizontal-projection test now pins
   2.14's behaviour; one test for the move vector added) and `scaler/tests.rs` (the new
   `cff_stem_darkening` field in option literals). The `scaler` module's own `prefer_interpreter`
   (`scaler/truetype.rs`) follows change 2.

## Not implemented here (a known difference)

FreeType 2.14.0 added a vertical separation adjustment to its auto-hinter (`afadjust.c`, commit
`7099b09e9`, "[autofit] Implement vertical separation adjustment"). skrifa's auto-hinter has no such
code, and neither does this copy. It affects Light-hinted TrueType glyphs with accents and the
dots of `i` and `j`; see the module documentation of `skrifa_face.rs`.

To update: copy the new crates.io release, redo change 1 (about 30 lines), change 2 and change 3 (the
new file `darken.rs` and the hooks listed there), then run the FreeType comparison tests of
`ferrokit-gui` (`text::skrifa_face`).
