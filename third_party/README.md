# Vendored crates

Copies of crates.io packages that `ferrokit-gui` builds from source, with the changes that make its glyph
output equal to Qt's FreeType engine. They are excluded from the workspace (`exclude` in the root
`Cargo.toml`) and used as path dependencies (`crates/ferrokit-gui/Cargo.toml` -> `skrifa`, which has a path
dependency on `read-fonts`).

| Directory | Upstream | Version | Upstream commit | Upstream licence | Licence files | Local changes |
|---|---|---|---|---|---|---|
| `skrifa/` | <https://github.com/googlefonts/fontations> (`skrifa`) | 0.49.0 (crates.io) | `47a301615fd95acf0da9fb7dbcd0ea7c7df36943` (`.cargo_vcs_info.json` of the package) | `MIT OR Apache-2.0`, Copyright (c) 2019 Fontations Developers | `LICENSE-MIT`, `LICENSE-APACHE` (byte-identical to the package) | yes: `skrifa/README.ferrokit.md`. 17 files are also FreeType 2.14.2 derived, `(MIT OR Apache-2.0) AND FTL` (the manifest says so, with the file list in `[package.metadata.ferrokit-licenses]`) |
| `read-fonts/` | <https://github.com/googlefonts/fontations> (`read-fonts`) | 0.46.0 (crates.io) | `47a301615fd95acf0da9fb7dbcd0ea7c7df36943` | `MIT OR Apache-2.0`, Copyright (c) 2019 Fontations Developers | `LICENSE-MIT`, `LICENSE-APACHE` (byte-identical to the package) | yes: `read-fonts/README.ferrokit.md`. Original changes only, no FreeType-derived code |

Not vendored from the packages: `tests/`, `benches/`, `scripts/`, `Cargo.lock`, `Cargo.toml.orig` and
`.cargo_vcs_info.json`. Each `Cargo.toml` is the package's with the test/bench targets and dev-dependencies
removed, a `path` for the sibling dependency (skrifa) and, in skrifa, the licence expression and the
per-file table. The package's own `README.md` is kept unchanged next to `README.ferrokit.md`.

Licence texts for the FreeType-derived files (FTL) are in `LICENSES/FTL.txt`; their credits, the Adobe
notice of FreeType's CFF engine and the permission notice of FreeType's `fthash.c` are in the repository's
`NOTICE` file (section "FreeType" and "Vendored crates"). Adobe's glyph list data of `read-fonts`
(`data/glyphlist.txt`, `data/zapfdingbats.txt`) carries its own notice in the files.

## Updating

Copy the new crates.io release, redo the changes listed in the `README.ferrokit.md` of the directory,
update the version and commit above, `NOTICE` and the per-file licence tables, and compare against the
package (for example `cargo package`-style extraction of the `.crate` and a recursive diff) so that every
differing file is listed there.
