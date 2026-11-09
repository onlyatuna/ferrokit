# read-fonts, vendored by ferrokit

* Upstream: `read-fonts` 0.46.0 from crates.io (<https://github.com/googlefonts/fontations>,
  `read-fonts` directory, commit `47a301615fd95acf0da9fb7dbcd0ea7c7df36943`).
* Licence: MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, unchanged). The upstream README is
  `README.md`.
* Used as a path dependency of the vendored `../skrifa`, the only consumer (`ferrokit-gui` uses skrifa
  for its FreeType-style Linux glyph backend, `crates/ferrokit-gui/src/text/skrifa_face.rs`).

## Licences, file by file

Upstream is `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`, kept unchanged; the upstream files have no
per-file headers). Compared with the unmodified crates.io 0.46.0 package: `src/model/glyph/outline/path.rs`
and `src/tables/glyf/outline/path.rs` carry ferrokit's `implied_point` hook (original work; FreeType's
`FT_Outline_Decompose` has no such hook and none of its code is used), `src/tables/glyf.rs` has the new
`PointMarker::IGNORE` constant (`0x40`, the point flag the vertical separation adjustment of the vendored
skrifa auto-hinter needs; original work), `Cargo.toml` has its bench targets and dev-dependencies removed,
and every other file is unchanged (`src/tables/name/language.rs` differs from the package only in its line
endings, which the repository's `.gitattributes` normalises). The whole crate is therefore
`MIT OR Apache-2.0`, ferrokit's changes under the same terms; no file contains FreeType-derived code.
`data/glyphlist.txt` and `data/zapfdingbats.txt` (Adobe Glyph List and ZapfDingbats list, used by the `agl`
feature, which ferrokit does not enable) are Adobe's and carry their own BSD-3-Clause style notice in
their first lines, unchanged from upstream.

## Changes against upstream

1. `PathSink::implied_point` (new, defaulted, no-op; `src/model/glyph/outline/path.rs`, forwarded by
   `TransformSink` and `&mut P`; skrifa's `ErasedPen` forwards it too) and its calls in
   `src/tables/glyf/outline/path.rs`: the conversion of a TrueType contour to path commands calls
   it right before the `move_to`, `quad_to` or `curve_to` whose end point it computed instead of
   reading it from the outline: the on-curve point implied between two consecutive off-curve
   points, and the start of a contour whose first point is off-curve (with the closing curve that
   returns to it). A sink that rebuilds FreeType's `FT_Outline` drops that point. Upstream hands
   the midpoint to the pen as an explicit point rounded to the integer coordinate type
   (`PointCoord::midpoint`, 1/64 pixel for the 26.6 scaler), but FreeType's point array does not
   have it and its rasterizer (`gray_conic_to`) uses the exact midpoint. Glyphs drawn through
   the upstream pen therefore differ from FreeType's by 1/64 pixel wherever the sum of the two
   off-curve coordinates is odd, which changed the bitmap of 46,034 of 1,567,728 measured glyph
   renders (all TrueType). With the hook `SlotPen` in `skrifa_face.rs` reproduces FreeType's
   point arrays, and every other pen behaves as before.
2. `Cargo.toml`: the `[[bench]]` targets and the `[dev-dependencies]` are removed, and the
   `benches/`, `scripts/` and `Cargo.lock` of the package are not vendored (nothing here is built
   as a test or bench target). The unit tests inside `src/` need the `font-test-data` crate of the
   fontations repository (not vendored): they are run by copying `src/` over a checkout at the commit
   above (`cargo test -p read-fonts`), all pass (1252 unit tests), none was edited.

To update: copy the new crates.io release, redo change 1 (a few lines in the two `path.rs` files
and the forwarding in skrifa's `ErasedPen`), and run the FreeType comparison tests of
`ferrokit-gui` (`text::skrifa_face`).
