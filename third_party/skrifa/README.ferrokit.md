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

## Licences, file by file

Upstream is `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`, kept unchanged; the upstream files have
no per-file headers). The comparison is against the unmodified crates.io 0.49.0 package and against the
FreeType 2.14.2 sources:

| Files | Content | Licence |
|---|---|---|
| everything not listed below (`src/**`, `generated/`, `README.md`) | upstream, unchanged | `MIT OR Apache-2.0` |
| `src/outline/hint.rs`, `src/outline/mod.rs`, `src/scaler/tests.rs` | upstream with ferrokit's plumbing: the `cff_stem_darkening` option, `DrawSettings::with_cff_stem_darkening`, `ErasedPen::implied_point`, option literals in tests. Original work, not copied from FreeType | `MIT OR Apache-2.0` (ferrokit's changes under the same terms) |
| `Cargo.toml` | upstream with the test targets and dev-dependencies removed, the licence expression below and the table in `[package.metadata.ferrokit-licenses]` | n/a |
| `src/outline/glyf/hint/graphics.rs`, `projection.rs`, `zone.rs` | upstream with the FreeType 2.14.2 arithmetic of `Compute_Funcs`, `Direct_Move`, `Direct_Move_Orig`, `Compute_Point_Displacement` (src/truetype/ttinterp.c) and `FT_MulFix` (src/base/ftcalc.c) | `(MIT OR Apache-2.0) AND FTL` |
| `src/outline/glyf/mod.rs`, `src/scaler/truetype.rs` | upstream with the test of `FT_Load_Glyph` (src/base/ftobjs.c) for the auto-hinter | `(MIT OR Apache-2.0) AND FTL` |
| `src/outline/cff/hint/darken.rs` (new), `src/outline/cff/hint.rs`, `src/outline/cff/mod.rs` | upstream with the CFF stem darkening of src/psaux/psfont.c, pshints.c, psblues.c (Adobe) and `cff_slot_load` (src/cff/cffgload.c) | `(MIT OR Apache-2.0) AND FTL` |

The `AND` in the last three rows is per file: the file combines upstream code (MIT OR Apache-2.0) with
FreeType-derived code (FTL, which asks for credit and for no use of the FreeType Project's name for
endorsement; the credit and the Adobe notice with its patent licence are in each file's first lines and
in the `NOTICE` file of the repository, the FTL text in `LICENSES/FTL.txt`). The Adobe files of psaux
are available only under the FTL (not the GPL), so the FTL is the one chosen for all FreeType code.
The package's `Cargo.toml` says `(MIT OR Apache-2.0) AND FTL` because a manifest has one expression
per package: it lists the licences of the files a user of the package meets, and the scope is in the
table above and in `[package.metadata.ferrokit-licenses]`. A user who takes only the files marked
`MIT OR Apache-2.0` is under the upstream terms.

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

## Vertical separation adjustment (FreeType 2.14.0, `afadjust.c`)

Ported in `src/outline/autohint/adjust/` (`database.rs` is `adjustment_database` in FreeType's table
order, which is not sorted: `Q` is never found, as in FreeType) with the tilde pre-pass, the
ignore-top/bottom edge flags (`NO_BLUE`, segment points marked `IGNORE`), the accent height limit and
the separation itself of `aflatin.c`. Also ported from 2.14: the real-serif check in
`hint/edges.rs` (`aflatin.c:4657-4722`), which the Liberation Sans C-cedilla and c-cedilla needed.
The bottom blue zone properties of `afblue.dat` are set by hand in `generated/generated_autohint_styles.rs`
(23 entries; the generator `skrifa/scripts/gen_autohint_styles.py` of fontations predates them and is not
vendored; every blue string and property of the table was checked against FreeType 2.14.2's `afblue.dat`).
The reference FreeType (Ubuntu 2.14.2) loads HarfBuzz at run time, so more is ported:
`adjust/gsub.rs` extends the reverse character map through the GSUB single and alternate
substitutions of the style's script (`af_parse_gsub`, `af_map_lookup`, `add_substitute`, with the hash
table of `fthash.c` for the visiting order and the first-glyph-of-the-last-range quirk of
`af_coverage_iterator`); `shape.rs` makes the blue strings of Indic, Khmer, Myanmar and USE scripts
measure the dotted circle that HarfBuzz inserts before a lone dependent sign (Khmer and USE always,
Indic if the font has GSUB data for the script, Myanmar only with the script tag `mym2`), hides the
default ignorable characters (U+200D in the Mongolian blue strings), applies the first alternate of
an alternate substitution in feature styles and counts alternate lookups in the blue string check
(Verdana's `c2sc`). `STYLE_RANGES` of `generated/generated_autohint_styles.rs` is FreeType 2.14.2's
map (derived from `afranges.c`, see `adjust/mod.rs`). Known differences: see the module documentation
of `skrifa_face.rs` in `ferrokit-gui`.

To update: copy the new crates.io release, redo change 1 (about 30 lines), change 2 and change 3 (the
new file `darken.rs` and the hooks listed there), then run the FreeType comparison tests of
`ferrokit-gui` (`text::skrifa_face`).
