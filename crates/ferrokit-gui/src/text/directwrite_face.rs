// SPDX-License-Identifier: LGPL-3.0-only
//! Windows glyph backend: DirectWrite rasterizes the glyphs, as Qt's
//! `QWindowsFontEngineDirectWrite` does (qwindowsfontenginedirectwrite.cpp `imageForGlyph` /
//! `renderGlyphRun`), so text looks like Qt's.
//!
//! What is copied from Qt:
//! * one glyph per `IDWriteGlyphRunAnalysis`, at the origin, em size = the *logical* pixel size and
//!   the device pixel ratio applied as the run's transform;
//! * the rendering mode Qt derives from its default hinting preference (`determineHinting` /
//!   `hintingPreferenceToRenderingMode`): at a device pixel ratio other than 1 `NATURAL` up to
//!   16 px and `NATURAL_SYMMETRIC` above, at ratio 1 (full hinting) `NATURAL_SYMMETRIC` when
//!   `useSymmetricAntialiasing` (Qt 6.12: over 16 px or no `maxp` instructions, overridden by the
//!   font's `gasp` table) and `GDI_CLASSIC` otherwise, with grid fit default and ClearType
//!   antialiasing;
//! * the layout advance (`recalcAdvances`): the design advance at a ratio other than 1, at ratio 1
//!   `GetGdiCompatibleGlyphMetrics` with `needsNaturalMetrics` for the natural modes, converted
//!   with `DESIGN_TO_LOGICAL` and left unrounded for the shaper to round after kerning;
//! * the ClearType 3x1 texture is kept as three coverages per pixel for LCD text
//!   (`alphaRGBMapForGlyph`, [`GlyphFace::rasterize_lcd_scaled`]) and reduced with `qGray` for
//!   grey-scale text (`alphaMapForGlyph`); a BGR panel swaps red and blue first;
//! * at a device pixel ratio other than 1 (`supportsHorizontalSubPixelPositions`) the glyph is
//!   drawn at the sub-pixel offset of its pen through `DWRITE_MATRIX.dx` (bucket count from
//!   twelve probe offsets, `QTextureGlyphCache::calculateSubPixelPositionCount`), and the painter
//!   blits it at the floor of the pen position (`QRasterPaintEngine::drawCachedGlyphs`).
//!
//! Measured against Qt 6.12 (PySide6): single glyphs are bit-identical (A8 and LCD) at every
//! ratio and size tried for the fonts that need no synthesized weight or slant, and so are the
//! advances of Latin text. Not modelled yet, as listed in the Windows text series: synthesized
//! weight and slant, font collections' face index, the per-script `kern` check, the metrics of
//! fonts with embedded bitmaps. The goldens in `tests/test_directwrite_placement.rs` and
//! `tests/test_directwrite_modes.rs` pin whole strings and glyphs measured from Qt.
//!
//! The font stays plain bytes in our memory (`SharedFontData`, handed to DirectWrite through an
//! in-memory font file loader); character mapping and advances come from [`OutlineFace`], which
//! reads them straight from the file. Only the coverage bitmap comes from DirectWrite.

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics, subpixel_bucket_offset};
use crate::text::outline_face::OutlineFace;
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::{LazyLock, Mutex};
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, Interface};

/// Process-wide DirectWrite objects. `None` when DirectWrite (or the in-memory loader, which needs
/// Windows 10 1703) is unavailable; callers then use [`OutlineFace`].
struct Factory {
    base: IDWriteFactory,
    factory2: IDWriteFactory2,
    loader: IDWriteInMemoryFontFileLoader,
    /// The panel's sub-pixels run blue-green-red (`m_pixelGeometry` of Qt's DirectWrite engine,
    /// from `CreateRenderingParams`).
    bgr: bool,
}

/// Held for every DirectWrite call, including the release of each `IDWriteFontFace`, so that
/// DirectWrite never runs on two threads at once. Microsoft documents no thread safety for the
/// factory, the in-memory loader or font faces: the `DWRITE_FACTORY_TYPE_SHARED` and
/// `IDWriteFactory` pages only say the state is shared within the process, and the SDK headers
/// only single out objects that "may not be thread-safe" (`IDWriteTextFormat`,
/// `IDWriteFontFallbackBuilder`). Skia serializes some DirectWrite calls even on the shared factory
/// (`maybe_dw_mutex`, SkScalerContext_win_dw.cpp: "In versions 8 and 8.1 of Windows, some calls in
/// DWrite are not thread safe").
///
/// The lock covers this module's calls only. The shared factory is process-wide state, but this
/// module is the only DirectWrite caller in ferrokit (no other crate creates a factory). A foreign
/// component in the process using its own shared factory on another thread is outside the lock;
/// the `DWRITE_FACTORY_TYPE_SHARED` page describes that factory as shared "across multiple
/// in-process components", so DirectWrite is taken to cope with that [inferred].
static DWRITE_LOCK: Mutex<()> = Mutex::new(());

/// Runs `f`, which calls DirectWrite, under [`DWRITE_LOCK`]. The lock guards no data, so a
/// poisoned lock is taken over.
///
/// The lock is not reentrant: `f` must not call `serialized` again, and must never drop a
/// `DirectWriteFace` (its `Drop` takes the lock), or the thread deadlocks.
fn serialized<R>(f: impl FnOnce() -> R) -> R {
    let _guard = DWRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

// SAFETY: `Send`/`Sync` require that every field may be used from any thread. Every DirectWrite
// call on `base`, `factory2` and `loader` is made under `DWRITE_LOCK` (see there), so they are
// never used by two threads at once; what remains is that they are not bound to the creating
// thread: DirectWrite objects are not COM apartment objects (`DWriteCreateFactory` needs no
// `CoInitialize`) [inferred: not stated by Microsoft's documentation]. `bgr` is a plain `bool`
// written once in `FACTORY`'s initializer, before any other thread can see the value.
unsafe impl Send for Factory {}
// SAFETY: as for `Send` above: shared access to the interfaces is serialized by `DWRITE_LOCK`,
// and `bgr` is never mutated after initialization.
unsafe impl Sync for Factory {}

// SAFETY: every call needs live COM objects and valid out-pointers. The factory chain is
// created by `DWriteCreateFactory` and owned by the locals/`Factory` here; each call receives
// references to those owned objects only, and the failure of any step returns `None` instead of
// using an object that was not created. `RegisterFontFileLoader` gets a loader this closure just
// created; the `Factory` lives in a `static` and is never dropped, so the registered loader
// outlives every font file created from it.
static FACTORY: LazyLock<Option<Factory>> = LazyLock::new(|| unsafe {
    let factory5: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
    let loader = factory5.CreateInMemoryFontFileLoader().ok()?;
    let loader_base: IDWriteFontFileLoader = loader.cast().ok()?;
    factory5.RegisterFontFileLoader(&loader_base).ok()?;
    let base: IDWriteFactory = factory5.cast().ok()?;
    let factory2: IDWriteFactory2 = factory5.cast().ok()?;
    let bgr = base
        .CreateRenderingParams()
        .is_ok_and(|params| params.GetPixelGeometry() == DWRITE_PIXEL_GEOMETRY_BGR);
    Some(Factory {
        base,
        factory2,
        loader,
        bgr,
    })
});

/// `DWRITE_MATRIX.dx` for a sub-pixel offset in 26.6 pixels (`QFixed::toReal`).
fn subpixel_dx(subpixel: i32) -> f32 {
    subpixel as f32 / 64.0
}

/// Whether DirectWrite can be used on this machine.
pub fn is_available() -> bool {
    FACTORY.is_some()
}

/// A font face rasterized by DirectWrite.
pub struct DirectWriteFace {
    /// Released in `Drop` under `DWRITE_LOCK`, before `base` (whose font bytes DirectWrite's
    /// font face reads) is dropped.
    face: ManuallyDrop<IDWriteFontFace>,
    base: OutlineFace,
    upem: f32,
    /// `QFontEngine::hasHinting`: the `maxp` table says the font has instructions.
    has_hinting: bool,
    /// The `(rangeMaxPPEM, rangeGaspBehavior)` records of the `gasp` table, in file order.
    gasp: Vec<(u16, u16)>,
    /// Qt's `m_subPixelPositionCount` per `(size, scale)`, see [`Self::subpixel_count`].
    subpixel_counts: Mutex<HashMap<(u32, u32), i64>>,
}

// SAFETY: `Send` requires that the value may move to another thread. Every call on `face`,
// including its release in `Drop`, is made under `DWRITE_LOCK`, and DirectWrite objects are not
// bound to their creating thread (see `Factory`'s `Send`); `base` (`OutlineFace`) is owned data
// (`Face` over the immutable shared font bytes plus a `Mutex`-guarded cache); `upem` is a plain
// `f32`.
unsafe impl Send for DirectWriteFace {}
// SAFETY: `Sync` requires that `&DirectWriteFace` may be used from several threads. Every
// `IDWriteFontFace` call made through `&self` (`GetDesignGlyphMetrics`,
// `GetGdiCompatibleGlyphMetrics`, glyph-run analysis, the reference the run takes) holds
// `DWRITE_LOCK`, so DirectWrite never sees two threads at once; `OutlineFace` is only read, apart
// from its `Mutex`-guarded cache; `upem` is never mutated.
unsafe impl Sync for DirectWriteFace {}

impl Drop for DirectWriteFace {
    fn drop(&mut self) {
        // SAFETY: `face` is dropped exactly once, here, and never used afterwards; `base`, whose
        // bytes the face reads, is only dropped after this body.
        serialized(|| unsafe { ManuallyDrop::drop(&mut self.face) });
    }
}

/// `QFontEngine::hasHinting` (qfontengine.cpp): `maxSizeOfInstructions > 0` for a version 1.0
/// `maxp` table, true for any other version that is long enough to be read, false for a missing
/// or short table (version 0.5, the CFF one, is 6 bytes).
fn maxp_has_hinting(maxp: Option<&[u8]>) -> bool {
    let Some(maxp) = maxp.filter(|t| t.len() >= 28) else {
        return false;
    };
    if u32::from_be_bytes([maxp[0], maxp[1], maxp[2], maxp[3]]) == 0x0001_0000 {
        u16::from_be_bytes([maxp[26], maxp[27]]) > 0
    } else {
        true
    }
}

/// The records of the `gasp` table as `QWindowsFontEngineDirectWrite::useSymmetricAntialiasing`
/// reads them: nothing for a table of 4 bytes or less, or one shorter than its `numRanges`
/// announces.
fn gasp_ranges(gasp: Option<&[u8]>) -> Vec<(u16, u16)> {
    let Some(gasp) = gasp.filter(|t| t.len() > 4) else {
        return Vec::new();
    };
    let count = usize::from(u16::from_be_bytes([gasp[2], gasp[3]]));
    if gasp.len() < 4 + count * 4 {
        return Vec::new();
    }
    (0..count)
        .map(|i| {
            let record = &gasp[4 + i * 4..];
            (
                u16::from_be_bytes([record[0], record[1]]),
                u16::from_be_bytes([record[2], record[3]]),
            )
        })
        .collect()
}

/// `QWindowsFontEngineDirectWrite::useSymmetricAntialiasing` (Qt 6.12): symmetric for sizes over
/// 16 px and for fonts without hinting instructions, unless the first `gasp` range whose
/// `rangeMaxPPEM` is at least the size says otherwise (`GASP_SYMMETRIC_SMOOTHING`, 0x8).
fn symmetric_antialiasing(size: f32, has_hinting: bool, gasp: &[(u16, u16)]) -> bool {
    let mut symmetric = size > 16.0 || !has_hinting;
    if let Some(&(_, behavior)) = gasp
        .iter()
        .find(|&&(max_ppem, _)| f32::from(max_ppem) >= size)
    {
        symmetric = behavior & 0x8 != 0;
    }
    symmetric
}

impl DirectWriteFace {
    /// Opens face `face_index` of the font file `data` with DirectWrite.
    pub fn new(data: SharedFontData, face_index: u32) -> Result<Self, String> {
        let base = OutlineFace::new(data.clone(), face_index)?;
        // Before the lock: `FACTORY`'s first use creates it.
        let factory = FACTORY.as_ref().ok_or("DirectWrite is unavailable")?;
        let (face, upem) = serialized(|| {
            // SAFETY: the in-memory font file reference stores the raw pointer without owning the
            // bytes (no owner object is passed), so the bytes must outlive the file and the face
            // created from it. They do: `data.as_slice()` is kept alive by `base`, which holds a
            // clone of the `SharedFontData` and is dropped after `face` (`DirectWriteFace`'s
            // `Drop`), and the file reference itself is moved into `CreateFontFace`. `data.len()`
            // is the length of that same slice; `supported`, `file_type`, `face_type` and `faces`
            // are live locals used as out-pointers, and `face_index < faces` is checked before
            // `CreateFontFace`.
            let face = unsafe {
                let file = factory
                    .loader
                    .CreateInMemoryFontFileReference(
                        &factory.base,
                        data.as_slice().as_ptr() as *const c_void,
                        data.len() as u32,
                        None::<&windows::core::IUnknown>,
                    )
                    .map_err(|e| format!("CreateInMemoryFontFileReference: {e}"))?;

                let mut supported = BOOL(0);
                let mut file_type = DWRITE_FONT_FILE_TYPE_UNKNOWN;
                let mut face_type = DWRITE_FONT_FACE_TYPE_UNKNOWN;
                let mut faces = 0u32;
                file.Analyze(
                    &mut supported,
                    &mut file_type,
                    Some(&mut face_type),
                    &mut faces,
                )
                .map_err(|e| format!("IDWriteFontFile::Analyze: {e}"))?;
                if !supported.as_bool() || face_index >= faces {
                    return Err("DirectWrite does not support this font".to_string());
                }
                factory
                    .base
                    .CreateFontFace(
                        face_type,
                        &[Some(file)],
                        face_index,
                        DWRITE_FONT_SIMULATIONS_NONE,
                    )
                    .map_err(|e| format!("CreateFontFace: {e}"))?
            };
            let mut fm = DWRITE_FONT_METRICS::default();
            // SAFETY: `face` was just created and is live; `&mut fm` is a live local that receives
            // one `DWRITE_FONT_METRICS`.
            unsafe { face.GetMetrics(&mut fm) };
            Ok((face, fm.designUnitsPerEm as f32))
        })?;
        let has_hinting = maxp_has_hinting(base.sfnt_table(b"maxp"));
        let gasp = gasp_ranges(base.sfnt_table(b"gasp"));
        Ok(Self {
            face: ManuallyDrop::new(face),
            base,
            upem,
            has_hinting,
            gasp,
            subpixel_counts: Mutex::new(HashMap::new()),
        })
    }

    /// Qt's `determineHinting` + `hintingPreferenceToRenderingMode` for default hinting, plus the
    /// measuring mode from `renderModeToMeasureMode`.
    ///
    /// With a device pixel ratio other than 1, Qt's default hinting resolves to vertical hinting up
    /// to 16 px (`NATURAL`, "asymmetric") and none above (`NATURAL_SYMMETRIC`), the sizes Microsoft
    /// recommends. At ratio 1 Qt's DirectWrite engine (the only engine its Windows platform
    /// plugin creates; measured against PySide6 6.12 at ratio 1) uses full hinting, and
    /// `hintingPreferenceToRenderingMode` then takes `NATURAL_SYMMETRIC` when
    /// [`symmetric_antialiasing`] (`useSymmetricAntialiasing`, new in Qt 6.12: the size, the
    /// `maxp` hinting check and the font's `gasp` table decide) and `GDI_CLASSIC` otherwise; it
    /// does not imitate GDI.
    fn rendering_mode(
        &self,
        size: f32,
        scale: f32,
    ) -> (DWRITE_RENDERING_MODE, DWRITE_MEASURING_MODE) {
        let scaled = (scale - 1.0).abs() > f32::EPSILON;
        if scaled && size <= 16.0 {
            (DWRITE_RENDERING_MODE_NATURAL, DWRITE_MEASURING_MODE_NATURAL)
        } else if scaled || symmetric_antialiasing(size, self.has_hinting, &self.gasp) {
            (
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        } else {
            (
                DWRITE_RENDERING_MODE_GDI_CLASSIC,
                DWRITE_MEASURING_MODE_GDI_CLASSIC,
            )
        }
    }

    /// Qt's `imageForGlyph`: the ClearType 3x1 texture, three coverages (red, green, blue sub-pixel,
    /// swapped for a BGR panel as in `renderGlyphRun`) per pixel. `None` when DirectWrite fails for
    /// this glyph.
    fn rasterize_texture(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        dx: f32,
    ) -> Option<(GlyphMetrics, Vec<u8>)> {
        let factory = FACTORY.as_ref()?;
        let advance = self.base.metrics_indexed(glyph_id, size * scale);
        let blank = GlyphMetrics {
            advance_width: advance.advance_width,
            advance_height: advance.advance_height,
            ..GlyphMetrics::default()
        };

        let (mode, measuring) = self.rendering_mode(size, scale);
        // Everything DirectWrite does here, including releasing the analysis and the run's face
        // reference, happens under the lock. `None` inside: an empty or over-budget bitmap.
        let rendered = serialized(|| {
            let glyph_index = glyph_id;
            let glyph_advance = 0.0f32;
            let glyph_offset = DWRITE_GLYPH_OFFSET::default();
            let mut run = DWRITE_GLYPH_RUN {
                fontFace: ManuallyDrop::new(Some((*self.face).clone())),
                fontEmSize: size,
                glyphCount: 1,
                glyphIndices: &glyph_index,
                glyphAdvances: &glyph_advance,
                glyphOffsets: &glyph_offset,
                isSideways: BOOL(0),
                bidiLevel: 0,
            };
            let transform = DWRITE_MATRIX {
                m11: scale,
                m12: 0.0,
                m21: 0.0,
                m22: scale,
                dx,
                dy: 0.0,
            };
            // SAFETY: `run` and `transform` are live locals; the pointers in `run`
            // (`glyphIndices`, `glyphAdvances`, `glyphOffsets`) point to `glyph_index`,
            // `glyph_advance` and `glyph_offset`, each valid for the one glyph (`glyphCount: 1`),
            // and `run.fontFace` holds a live face (the clone made above). All of them outlive
            // this call. [inferred from the `IDWriteFactory2::CreateGlyphRunAnalysis`
            // documentation, which does not say that the run's pointers are kept after the call;
            // they stay valid for the whole closure either way.]
            let analysis = unsafe {
                factory.factory2.CreateGlyphRunAnalysis(
                    &run,
                    Some(&transform),
                    mode,
                    measuring,
                    DWRITE_GRID_FIT_MODE_DEFAULT,
                    DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
                    0.0,
                    0.0,
                )
            };
            // Release the reference taken for the run now that the analysis owns what it needs.
            // SAFETY: `ManuallyDrop::take` is valid only on a not-yet-dropped value that is never
            // used again. `run.fontFace` was initialized with the clone above and is taken exactly
            // once, here; `run` is not read afterwards and, being `ManuallyDrop`, is not dropped
            // again with the struct, so the clone's reference is released exactly once.
            drop(unsafe { ManuallyDrop::take(&mut run.fontFace) });
            let analysis = analysis.ok()?;

            // SAFETY: `analysis` is a live `IDWriteGlyphRunAnalysis` (the `Err` case returned
            // above); the texture type is a valid enum value and the call has no other inputs.
            let bounds =
                unsafe { analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_CLEARTYPE_3x1) }.ok()?;
            let (width, height) = (
                (bounds.right - bounds.left).max(0) as usize,
                (bounds.bottom - bounds.top).max(0) as usize,
            );
            if width == 0
                || height == 0
                || crate::text::glyph_face::exceeds_glyph_budget(width, height)
            {
                return Some(None);
            }

            let mut texture = vec![0u8; width * height * 3];
            // SAFETY: `analysis` is the live analysis created above from this `bounds`' texture
            // type. `texture` has `width * height * 3` bytes where `width`/`height` are `bounds`'
            // extent (zero-sized and over-budget cases returned earlier), which is the size
            // DirectWrite requires for a `CLEARTYPE_3x1` texture of `bounds`.
            unsafe {
                analysis.CreateAlphaTexture(DWRITE_TEXTURE_CLEARTYPE_3x1, &bounds, &mut texture)
            }
            .ok()?;
            Some(Some((bounds.left, bounds.bottom, width, height, texture)))
        })?;
        let Some((left, bottom, width, height, mut texture)) = rendered else {
            return Some((blank, Vec::new()));
        };

        if factory.bgr {
            for px in texture.as_chunks_mut::<3>().0 {
                px.swap(0, 2);
            }
        }
        // Texture bounds are relative to the glyph origin on the baseline, rows growing downwards;
        // `ymin` is the bitmap's bottom edge above the baseline.
        let metrics = GlyphMetrics {
            xmin: left,
            ymin: -bottom,
            width,
            height,
            ..blank
        };
        Some((metrics, texture))
    }

    /// `QFontEngine::m_subPixelPositionCount` at `size`: the number of different textures
    /// DirectWrite draws for a glyph at twelve offsets of a pixel, decided by the first glyph with
    /// an outline that is positioned (`QTextureGlyphCache::calculateSubPixelPositionCount`,
    /// `populate`). 0 while no such glyph has been seen.
    fn subpixel_count(&self, glyph_id: u16, size: f32, scale: f32) -> i64 {
        let key = (size.to_bits(), scale.to_bits());
        let mut counts = self
            .subpixel_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(&count) = counts.get(&key) {
            return count;
        }
        if self.base.glyph_outline(glyph_id, size).is_none() {
            return 0;
        }
        let mut images: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        for i in 0..12 {
            // `QFixed::fromReal(i / 12.0)`: truncated to 26.6.
            let offset = (f64::from(i) / 12.0 * 64.0).trunc() as i32;
            let (metrics, texture) = self
                .rasterize_texture(glyph_id, size, scale, subpixel_dx(offset))
                .unwrap_or_default();
            let image = (metrics.width, metrics.height, texture);
            if !images.contains(&image) {
                images.push(image);
            }
        }
        let count = images.len() as i64;
        counts.insert(key, count);
        count
    }

    /// The advance Qt's DirectWrite engine gives a glyph when laying text out
    /// (`QWindowsFontEngineDirectWrite::recalcAdvances`, called back from HarfBuzz for every
    /// glyph), as a 26.6 value: `DESIGN_TO_LOGICAL`, `QFixed::fromReal(design / unitsPerEm *
    /// pixelSize)`, truncated toward zero. It is not rounded to whole pixels here; Qt rounds the
    /// advances of an engine without sub-pixel positions once, after HarfBuzz added its kerning
    /// (`qt_glyph_metrics`).
    ///
    /// * `direct_write == true` (a device pixel ratio other than 1): `determineHinting` is not full
    ///   hinting, so the design advance (`GetDesignGlyphMetrics`) is taken. The device pixel ratio
    ///   does not enter, so 125%, 150% and 200% share these advances.
    /// * `direct_write == false` (ratio 1, full hinting): `GetGdiCompatibleGlyphMetrics` at the
    ///   logical size, with `needsNaturalMetrics` when the rendering mode is a natural one
    ///   ([`Self::rendering_mode`]: `NATURAL` or `NATURAL_SYMMETRIC`). The classic ones are whole
    ///   pixel advances stored in font units, which `DESIGN_TO_LOGICAL` then truncates again (Arial
    ///   Italic's `A` at 9 px: 6 px is 1365 units, 5.984375 px); the natural ones are the design
    ///   advances.
    pub fn layout_advance_width(
        &self,
        glyph_id: u16,
        size: f32,
        direct_write: bool,
    ) -> Option<f32> {
        if self.upem <= 0.0 {
            return None;
        }
        let mut metric = DWRITE_GLYPH_METRICS::default();
        if direct_write {
            // SAFETY: `self.face` is a live `IDWriteFontFace`; `&glyph_id` points to one `u16`
            // and the glyph count passed is 1, and `&mut metric` is a live local receiving
            // exactly one `DWRITE_GLYPH_METRICS`.
            serialized(|| unsafe {
                self.face
                    .GetDesignGlyphMetrics(&glyph_id, 1, &mut metric, false)
            })
            .ok()?;
        } else {
            let (mode, _) = self.rendering_mode(size, 1.0);
            let natural = mode == DWRITE_RENDERING_MODE_NATURAL
                || mode == DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
            // SAFETY: `self.face` is a live `IDWriteFontFace`; `&glyph_id` points to one `u16` and
            // the glyph count passed is 1, and `&mut metric` is a live local receiving exactly one
            // `DWRITE_GLYPH_METRICS`; the optional transform is `None` (no pointer is passed).
            serialized(|| unsafe {
                self.face.GetGdiCompatibleGlyphMetrics(
                    size,
                    1.0,
                    None,
                    natural,
                    &glyph_id,
                    1,
                    &mut metric,
                    false,
                )
            })
            .ok()?;
        }
        let pixels = f64::from(metric.advanceWidth) / f64::from(self.upem) * f64::from(size);
        Some(((pixels * 64.0).trunc() / 64.0) as f32)
    }
}

impl GlyphFace for DirectWriteFace {
    fn glyph_index(&self, ch: char) -> u16 {
        self.base.glyph_index(ch)
    }

    /// Qt's DirectWrite engine fills its ascent, descent and leading from `DWRITE_FONT_METRICS`
    /// and then lets `QFontEngine::initializeHeightMetrics` replace them with the values of the
    /// font's own tables (qwindowsfontenginedirectwrite.cpp:353-363), which are what is read here.
    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        self.base.line_metrics(px)
    }

    /// `boundingBox('x').xoff` (the design advance of `x`): Qt's Windows engines do not use the
    /// OS/2 average width.
    fn average_char_width(&self, px: f32) -> Option<f32> {
        self.base.x_advance(px)
    }

    fn cap_height(&self, px: f32) -> Option<f32> {
        self.base.cap_height(px)
    }

    fn glyph_outline(&self, glyph_id: u16, px: f32) -> Option<tiny_skia::Path> {
        self.base.glyph_outline(glyph_id, px)
    }

    /// Outline metrics (advance widths are unhinted, as in Qt's natural layout). The placement of
    /// the DirectWrite bitmap itself is returned by the rasterize methods.
    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        self.base.metrics_indexed(glyph_id, px)
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        self.rasterize_scaled(glyph_id, px, 1.0)
    }

    /// Qt's `alphaMapForGlyph`: the texture reduced with `qGray`.
    fn rasterize_scaled(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Vec<u8>) {
        self.rasterize_subpixel(glyph_id, size, scale, 0)
    }

    /// Qt's `alphaRGBMapForGlyph`: the texture itself.
    fn rasterize_lcd_scaled(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
    ) -> Option<(GlyphMetrics, Vec<u8>)> {
        self.rasterize_lcd_subpixel(glyph_id, size, scale, 0)
    }

    /// `QWindowsFontEngineDirectWrite::supportsHorizontalSubPixelPositions`: `determineHinting` is
    /// not full hinting, which for the default hinting preference is a device pixel ratio other
    /// than 1.
    fn supports_horizontal_subpixel_positions(&self) -> bool {
        crate::text::font_database::uses_directwrite_engine()
    }

    fn subpixel_position(&self, glyph_id: u16, size: f32, scale: f32, x: i64) -> i32 {
        subpixel_bucket_offset(x, self.subpixel_count(glyph_id, size, scale))
    }

    fn rasterize_subpixel(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: i32,
    ) -> (GlyphMetrics, Vec<u8>) {
        if size <= 0.0 || scale <= 0.0 {
            return (GlyphMetrics::default(), Vec::new());
        }
        match self.rasterize_texture(glyph_id, size, scale, subpixel_dx(subpixel)) {
            Some((metrics, texture)) => {
                let grey = texture
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|p| ((p[0] as u32 * 11 + p[1] as u32 * 16 + p[2] as u32 * 5) / 32) as u8)
                    .collect();
                (metrics, grey)
            }
            None => self.base.rasterize_indexed(glyph_id, size * scale),
        }
    }

    fn rasterize_lcd_subpixel(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: i32,
    ) -> Option<(GlyphMetrics, Vec<u8>)> {
        if size <= 0.0 || scale <= 0.0 {
            return None;
        }
        self.rasterize_texture(glyph_id, size, scale, subpixel_dx(subpixel))
    }

    fn layout_advance_width(&self, glyph_id: u16, size: f32, direct_write: bool) -> Option<f32> {
        self.layout_advance_width(glyph_id, size, direct_write)
    }

    /// The same design metrics DirectWrite reports (`rightSideBearing`); read from the font file
    /// like the advances are (`right_side_bearings_match_directwrite` compares them).
    fn right_side_bearing(&self, glyph_id: u16, size: f32) -> Option<f32> {
        self.base.right_side_bearing(glyph_id, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maxp(version: u32, size_of_instructions: u16, len: usize) -> Vec<u8> {
        let mut table = vec![0u8; len];
        table[..4].copy_from_slice(&version.to_be_bytes());
        table[26..28].copy_from_slice(&size_of_instructions.to_be_bytes());
        table
    }

    fn gasp(ranges: &[(u16, u16)]) -> Vec<u8> {
        let mut table = vec![0, 1];
        table.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (max_ppem, behavior) in ranges {
            table.extend_from_slice(&max_ppem.to_be_bytes());
            table.extend_from_slice(&behavior.to_be_bytes());
        }
        table
    }

    /// `QFontEngine::hasHinting` reads `maxSizeOfInstructions` of a version 1.0 `maxp`, assumes
    /// hinting for another version, and none for a missing or short table.
    #[test]
    fn maxp_decides_whether_the_font_has_hinting() {
        assert!(maxp_has_hinting(Some(&maxp(0x0001_0000, 1, 32))));
        assert!(!maxp_has_hinting(Some(&maxp(0x0001_0000, 0, 32))));
        assert!(maxp_has_hinting(Some(&maxp(0x0001_0000, 300, 28))));
        assert!(maxp_has_hinting(Some(&maxp(0x0002_0000, 0, 28))));
        assert!(!maxp_has_hinting(Some(&maxp(0x0001_0000, 5, 28)[..27])));
        assert!(!maxp_has_hinting(Some(&[0, 0, 0x50, 0, 0, 9])));
        assert!(!maxp_has_hinting(None));
    }

    /// `useSymmetricAntialiasing` ignores a `gasp` table of 4 bytes or less and one that is shorter
    /// than its `numRanges`, and reads the records otherwise in file order.
    #[test]
    fn gasp_records_are_read_as_qt_reads_them() {
        let arial = gasp(&[(8, 10), (17, 5), (65535, 15)]);
        assert_eq!(gasp_ranges(Some(&arial)), [(8, 10), (17, 5), (65535, 15)]);
        assert_eq!(gasp_ranges(Some(&arial[..arial.len() - 1])), []);
        assert_eq!(gasp_ranges(Some(&[0, 1, 0, 0])), []);
        assert_eq!(gasp_ranges(None), []);
        let mut padded = gasp(&[(9, 2)]);
        padded.extend_from_slice(&[0xff; 8]);
        assert_eq!(gasp_ranges(Some(&padded)), [(9, 2)]);
    }

    /// `size > 16 || !hasHinting()`, then the first `gasp` range with `rangeMaxPPEM >= size`
    /// replaces it by bit 0x8 of its behaviour (Arial's table: 17 px is not symmetric, though it
    /// is over 16 px).
    #[test]
    fn symmetric_antialiasing_follows_size_hinting_and_gasp() {
        assert!(!symmetric_antialiasing(16.0, true, &[]));
        assert!(symmetric_antialiasing(16.5, true, &[]));
        assert!(symmetric_antialiasing(12.0, false, &[]));
        let arial = [(8, 10), (17, 5), (65535, 15)];
        assert!(symmetric_antialiasing(8.0, true, &arial));
        assert!(!symmetric_antialiasing(9.0, true, &arial));
        assert!(!symmetric_antialiasing(16.0, true, &arial));
        assert!(!symmetric_antialiasing(17.0, true, &arial));
        assert!(symmetric_antialiasing(18.0, true, &arial));
        assert!(symmetric_antialiasing(40.0, true, &arial));
        // No range reaches the size: the default stays.
        assert!(symmetric_antialiasing(30.0, true, &[(20, 0)]));
        assert!(symmetric_antialiasing(12.0, true, &[(20, 0x8 | 0x2)]));
        // A fractional size is compared as the float it is.
        assert!(!symmetric_antialiasing(17.0, true, &[(17, 0)]));
        assert!(symmetric_antialiasing(17.5, true, &[(17, 0)]));
    }

    /// `right_side_bearing` is what DirectWrite itself reports (`GetDesignGlyphMetrics`, the call
    /// Qt's `QWindowsFontEngineDirectWrite::boundingBox(glyph_t)` makes) converted as Qt's
    /// `DESIGN_TO_LOGICAL` does, for every Latin-1 glyph of Arial, Arial Italic and Segoe UI.
    #[test]
    fn right_side_bearings_match_directwrite() {
        // Like `tests/test_directwrite_face.rs`: DirectWrite's in-memory loader needs Windows 10
        // 1703, and on an older system the crate itself falls back to `OutlineFace`, so there is
        // nothing to compare. A missing font file below does fail the test.
        if !is_available() {
            return;
        }
        let mut compared = 0;
        for file in ["arial.ttf", "ariali.ttf", "segoeui.ttf"] {
            let bytes = std::fs::read(format!("C:/Windows/Fonts/{file}"))
                .unwrap_or_else(|e| panic!("the test needs C:/Windows/Fonts/{file}: {e}"));
            let face = DirectWriteFace::new(SharedFontData::from_vec(bytes), 0).expect(file);
            for ch in ('!'..='~')
                .chain('\u{a1}'..='\u{ff}')
                .filter(|&c| c != '\u{ad}')
            {
                let glyph = face.glyph_index(ch);
                let mut metric = DWRITE_GLYPH_METRICS::default();
                // SAFETY: `face.face` is the live `IDWriteFontFace` of `face`; `&glyph` points to
                // one `u16` and the glyph count passed is 1, and `&mut metric` is a live local
                // that receives exactly one `DWRITE_GLYPH_METRICS`.
                serialized(|| unsafe {
                    face.face
                        .GetDesignGlyphMetrics(&glyph, 1, &mut metric, false)
                })
                .expect("metrics");
                for size in [11.0f32, 13.0, 16.0, 24.0, 31.0] {
                    let design = f64::from(metric.rightSideBearing);
                    let pixels = design / f64::from(face.upem) * f64::from(size);
                    let want = ((pixels * 64.0).trunc() / 64.0) as f32;
                    assert_eq!(
                        face.right_side_bearing(glyph, size),
                        Some(want),
                        "{file}: {ch:?} at {size} px"
                    );
                    compared += 1;
                }
            }
        }
        assert!(compared > 1000, "only {compared} glyphs compared");
    }
}
