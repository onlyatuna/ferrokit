// SPDX-License-Identifier: LGPL-3.0-only
//! macOS glyph backend: CoreText measures and rasterizes glyphs, following Qt's
//! `QCoreTextFontEngine` (`src/gui/text/coretext/qfontengine_coretext.mm`).
//!
//! What is copied from Qt:
//! * Line metrics: `CTFontGetAscent`, `CTFontGetDescent`, `CTFontGetLeading` truncated to 1/64 px
//!   (`initializeHeightMetrics`).
//! * Average char width: `|OS/2 xAvgCharWidth| * px / unitsPerEm`, with a fallback to the advance of
//!   `'x'` (`boundingBox(glyphIndex('x')).xoff`).
//! * Cap height: `CTFontGetCapHeight` with a fallback to the bounding box height of `'H'`.
//! * Layout advances: `CTFontGetAdvancesForGlyphs` truncated to 1/64 px (`loadAdvancesForGlyphs`).
//! * Outlines: `CTFontCreatePathForGlyph` transformed with a `(1, -1)` y-flip matrix, converting
//!   elements into a path (`addGlyphsToPath`).
//! * Glyph bitmaps (`alphaMapBoundingBox`, `imageForGlyph`, `alphaMapForGlyph`): an sRGB RGB32
//!   `CGBitmapContext` (`qt_mac_cgImageFormatForImage`), the glyph drawn with `CTFontDrawGlyphs` at the
//!   logical pixel size under a text matrix that carries the device pixel ratio, black on white
//!   (inverted afterwards) when the font smoothing is grayscale and white on black otherwise, then
//!   reduced with `qGray`. The memory rows of a `CGBitmapContext` run top to bottom, so the rows are
//!   used as they are.
//! * Sub-pixel positions: the glyph is drawn at a fractional x offset, and how many offsets there
//!   are is what CoreGraphics distinguishes for the first glyph with an outline that is drawn at
//!   a size (`QTextureGlyphCache::calculateSubPixelPositionCount`).
//! * System UI font: optical sizing and metrics come from
//!   `CTFontCreateUIFontForLanguage(kCTFontUIFontSystem)` at each size.
//!
//! Not modelled, as in the engine this replaces: synthesized bold and italic, colour glyph fonts
//! (`kCTFontColorGlyphsTrait`, drawn as ARGB by Qt), the legacy sub-pixel font smoothing that a
//! `CGFontRenderingFontSmoothingDisabled=NO` override brings back (`FontSmoothing::Subpixel`, which
//! Qt draws as LCD masks with gamma correction) and the dark appearance, where Qt draws white on
//! black (`QPlatformTheme::colorScheme`): glyphs are made for the light appearance.

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics, exceeds_glyph_budget};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct CGAffineTransform {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
}

impl CGAffineTransform {
    const IDENTITY: Self = Self::scale(1.0);

    const fn scale(s: f64) -> Self {
        Self {
            a: s,
            b: 0.0,
            c: 0.0,
            d: s,
            tx: 0.0,
            ty: 0.0,
        }
    }
}

#[repr(C)]
struct CGPathElement {
    type_: i32,
    points: *mut CGPoint,
}

/// `kCGImageAlphaNoneSkipFirst | kCGBitmapByteOrder32Host`: what `qt_mac_cgImageFormatForImage`
/// gives `QImage::Format_RGB32`.
const RGB32_BITMAP_INFO: u32 = 6 | 0x2000;
/// `kCFStringEncodingUTF8`.
const CF_UTF8: u32 = 0x0800_0100;
/// `kCTFontUIFontSystem`.
const UI_FONT_SYSTEM: i32 = 2;
/// The `OS/2` table tag.
const TAG_OS2: u32 = 0x4F53_2F32;

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateUIFontForLanguage(
        ui_type: i32,
        size: f64,
        language: *const c_void,
    ) -> *const c_void;
    fn CTFontCreateWithGraphicsFont(
        graphics_font: *const c_void,
        size: f64,
        matrix: *const CGAffineTransform,
        attributes: *const c_void,
    ) -> *const c_void;
    fn CTFontCreateWithFontDescriptor(
        descriptor: *const c_void,
        size: f64,
        matrix: *const CGAffineTransform,
    ) -> *const c_void;
    fn CTFontCreateWithName(
        name: *const c_void,
        size: f64,
        matrix: *const CGAffineTransform,
    ) -> *const c_void;
    fn CTFontManagerCreateFontDescriptorsFromData(data: *const c_void) -> *const c_void;
    fn CTFontGetGlyphsForCharacters(
        font: *const c_void,
        characters: *const u16,
        glyphs: *mut u16,
        count: isize,
    ) -> u8;
    fn CTFontGetBoundingRectsForGlyphs(
        font: *const c_void,
        orientation: i32,
        glyphs: *const u16,
        bounding_rects: *mut CGRect,
        count: isize,
    ) -> CGRect;
    fn CTFontGetAdvancesForGlyphs(
        font: *const c_void,
        orientation: i32,
        glyphs: *const u16,
        advances: *mut CGSize,
        count: isize,
    ) -> f64;
    fn CTFontGetAscent(font: *const c_void) -> f64;
    fn CTFontGetDescent(font: *const c_void) -> f64;
    fn CTFontGetLeading(font: *const c_void) -> f64;
    fn CTFontGetCapHeight(font: *const c_void) -> f64;
    fn CTFontGetUnitsPerEm(font: *const c_void) -> u32;
    fn CTFontCopyTable(font: *const c_void, table: u32, options: u32) -> *const c_void;
    fn CTFontCreatePathForGlyph(
        font: *const c_void,
        glyph: u16,
        matrix: *const CGAffineTransform,
    ) -> *const c_void;
    fn CTFontDrawGlyphs(
        font: *const c_void,
        glyphs: *const u16,
        positions: *const CGPoint,
        count: usize,
        context: *mut c_void,
    );
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    static kCGColorBlack: *const c_void;
    static kCGColorWhite: *const c_void;
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *const c_void;
    fn CGFontCreateWithDataProvider(provider: *const c_void) -> *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *const c_void;
    fn CGColorGetConstantColor(name: *const c_void) -> *const c_void;
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const c_void,
        bitmap_info: u32,
    ) -> *mut c_void;
    fn CGContextSetShouldAntialias(context: *mut c_void, should_antialias: u8);
    fn CGContextSetShouldSmoothFonts(context: *mut c_void, should_smooth: u8);
    fn CGContextSetTextDrawingMode(context: *mut c_void, mode: i32);
    fn CGContextSetFillColorWithColor(context: *mut c_void, color: *const c_void);
    fn CGContextSetGrayFillColor(context: *mut c_void, gray: f64, alpha: f64);
    fn CGContextSetTextMatrix(context: *mut c_void, transform: CGAffineTransform);
    fn CGContextSetTextPosition(context: *mut c_void, x: f64, y: f64);
    fn CGContextRelease(context: *mut c_void);
    fn CGPathApply(
        path: *const c_void,
        info: *mut c_void,
        function: extern "C" fn(*mut c_void, *const CGPathElement),
    );
    fn CGPathIsEmpty(path: *const c_void) -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFDataGetLength(the_data: *const c_void) -> isize;
    fn CFDataGetBytePtr(the_data: *const c_void) -> *const u8;
    fn CFArrayGetCount(the_array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        c_str: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// `QFixed::fromReal` as a real: truncated toward zero to 1/64 (`(int)(r * 64)`, qfixed_p.h).
fn q_fixed(v: f64) -> f64 {
    (v * 64.0).trunc() / 64.0
}

/// How CoreGraphics smooths text on this system (`QCoreTextFontEngine::FontSmoothing`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FontSmoothing {
    Disabled,
    Subpixel,
    Grayscale,
}

/// An RGB32 `CGBitmapContext` over `pixels` (`width` x `height`), as Qt makes one over a
/// `QImage::Format_RGB32` (an sRGB colour space). Null when CoreGraphics refuses.
///
/// # Safety
/// `pixels` must stay alive and unmoved until the returned context is released.
unsafe fn rgb32_context(pixels: &mut [u32], width: usize, height: usize) -> *mut c_void {
    debug_assert!(pixels.len() >= width * height);
    // SAFETY: kCGColorSpaceSRGB is a CoreGraphics constant; the colour space is released below.
    let space = unsafe { CGColorSpaceCreateWithName(kCGColorSpaceSRGB) };
    if space.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `pixels` is writable for `width * height * 4` bytes and the caller keeps it alive.
    let ctx = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            space,
            RGB32_BITMAP_INFO,
        )
    };
    // SAFETY: `space` was created above and the context keeps its own reference.
    unsafe { CFRelease(space) };
    ctx
}

/// `QCoreTextFontEngine::fontSmoothing`: draws a Helvetica `X` with and without font smoothing and
/// reads the algorithm from the difference.
fn font_smoothing() -> FontSmoothing {
    static CACHED: LazyLock<FontSmoothing> = LazyLock::new(detect_font_smoothing);
    *CACHED
}

fn detect_font_smoothing() -> FontSmoothing {
    const SIZE: usize = 10;
    // SAFETY: a NUL-terminated ASCII name; the string is released below.
    let name =
        unsafe { CFStringCreateWithCString(std::ptr::null(), c"Helvetica".as_ptr(), CF_UTF8) };
    if name.is_null() {
        return FontSmoothing::Grayscale;
    }
    // SAFETY: `name` is a valid CFString; the font is released below.
    let font = unsafe { CTFontCreateWithName(name, SIZE as f64, std::ptr::null()) };
    // SAFETY: `name` was created above.
    unsafe { CFRelease(name) };
    if font.is_null() {
        return FontSmoothing::Grayscale;
    }
    let character = u16::from(b'X');
    let mut glyph = 0u16;
    // SAFETY: one character in, one glyph out; `font` is valid.
    unsafe { CTFontGetGlyphsForCharacters(font, &character, &mut glyph, 1) };
    let draw = |smooth: bool| -> Vec<u32> {
        let mut pixels = vec![0u32; SIZE * SIZE];
        // SAFETY: `pixels` outlives the context, which is released before returning; `font`
        // and `glyph` are valid.
        unsafe {
            let ctx = rgb32_context(&mut pixels, SIZE, SIZE);
            if !ctx.is_null() {
                CGContextSetTextDrawingMode(ctx, 0);
                CGContextSetGrayFillColor(ctx, 1.0, 1.0);
                CGContextSetShouldSmoothFonts(ctx, u8::from(smooth));
                CTFontDrawGlyphs(font, &glyph, &CGPoint::default(), 1, ctx);
                CGContextRelease(ctx);
            }
        }
        pixels
    };
    let non_smoothed = draw(false);
    let smoothed = draw(true);
    // SAFETY: `font` was created above.
    unsafe { CFRelease(font) };

    let mut result = FontSmoothing::Disabled;
    for (&s, &n) in smoothed.iter().zip(&non_smoothed) {
        let (r, g, b) = ((s >> 16) & 0xff, (s >> 8) & 0xff, s & 0xff);
        if r != g || r != b {
            return FontSmoothing::Subpixel;
        }
        if (s & 0x00ff_ffff) != (n & 0x00ff_ffff) {
            result = FontSmoothing::Grayscale;
        }
    }
    result
}

/// `QCoreTextFontEngine::shouldSmoothFont` for a font that is antialiased and has no colour glyphs.
fn should_smooth_font() -> bool {
    font_smoothing() != FontSmoothing::Disabled
}

/// `QFontEngine::alphaMapBoundingBox` of a glyph (`QCoreTextFontEngine`, `Format_A8`): the glyph's
/// bounding box at the logical size, scaled by the device pixel ratio, with the pixel Qt adds on
/// the left, right (twice) and above and below to keep antialiased edges from being clipped. All
/// values are 26.6 numbers held as reals.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AlphaBox {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl AlphaBox {
    fn new(rect: CGRect, scale: f64) -> Self {
        let mut width = q_fixed(rect.size.width);
        let mut height = q_fixed(rect.size.height);
        let mut x = q_fixed(rect.origin.x);
        let mut y = -q_fixed(rect.origin.y) - height;
        if scale != 1.0 {
            width = q_fixed(width * scale);
            height = q_fixed(height * scale);
            x = q_fixed(x * scale);
            y = q_fixed(y * scale);
        }
        Self {
            x: x - 1.0,
            y: y - 1.0,
            width: width.abs() + 3.0,
            height: height.abs() + 2.0,
        }
    }

    fn pixel_width(self) -> usize {
        self.width.ceil() as usize
    }

    fn pixel_height(self) -> usize {
        self.height.ceil() as usize
    }
}

/// Reads `xAvgCharWidth` (`|OS/2 + 2|`, an `i16`) of a `CTFont`.
fn os2_avg_char_width(font: *const c_void) -> Option<f32> {
    // SAFETY: `font` is a valid CTFontRef; the copied table is released below.
    let table = unsafe { CTFontCopyTable(font, TAG_OS2, 0) };
    if table.is_null() {
        return None;
    }
    // SAFETY: `table` is a valid CFDataRef.
    let (len, ptr) = unsafe { (CFDataGetLength(table), CFDataGetBytePtr(table)) };
    let width = if len >= 10 && !ptr.is_null() {
        // SAFETY: `len >= 10`, so bytes 2 and 3 are inside the table.
        let w = unsafe { i16::from_be_bytes([*ptr.add(2), *ptr.add(3)]) };
        Some(f32::from(w.unsigned_abs()))
    } else {
        None
    };
    // SAFETY: `table` was copied above and is not used again.
    unsafe { CFRelease(table) };
    width
}

/// A font face rasterized and measured by Apple CoreText.
pub struct CoreTextFace {
    /// Glyph lookup and tables, parsed from `_data`.
    face: Option<rustybuzz::ttf_parser::Face<'static>>,
    _data: SharedFontData,
    is_system_ui: bool,
    descriptor: *const c_void,
    cg_font: *const c_void,
    units_per_em: f32,
    os2_avg_char_width: Option<f32>,
    /// `CTFontRef` per pixel size (bits of the `f32`), `null` when CoreText made none.
    ct_cache: Mutex<HashMap<u32, *const c_void>>,
    /// `QFontEngine::m_subPixelPositionCount` per pixel size (bits of the `f32`); absent until a
    /// glyph with an outline has been positioned.
    subpixel_counts: Mutex<HashMap<u32, i64>>,
}

// SAFETY: All CoreFoundation/CoreText/CoreGraphics objects referenced (CTFontDescriptor, CGFont,
// and cached CTFont pointers) are immutable handles designed for multithreaded read access.
// Mutation of the size cache is synchronized by a Mutex.
unsafe impl Send for CoreTextFace {}
// SAFETY: Immutable access to fields and synchronized cache access via Mutex guarantee thread safety.
unsafe impl Sync for CoreTextFace {}

impl CoreTextFace {
    /// Builds the face of face `face_index` of the font file `data`.
    pub fn new(data: SharedFontData, face_index: u32) -> Result<Self, String> {
        Self::build(data, face_index, false)
    }

    /// Builds the face of the system UI font, whose file is `data`: its CTFonts come from
    /// `CTFontCreateUIFontForLanguage(kCTFontUIFontSystem)` at each size, not from the file, so that
    /// the optical size of the variable font follows the pixel size as it does in Qt.
    pub(crate) fn system_ui(data: SharedFontData) -> Result<Self, String> {
        Self::build(data, 0, true)
    }

    fn build(data: SharedFontData, face_index: u32, is_system_ui: bool) -> Result<Self, String> {
        let mut descriptor = std::ptr::null();
        let mut cg_font = std::ptr::null();

        let (upem, os2_avg) = if is_system_ui {
            // SAFETY: UI type 2 is kCTFontUIFontSystem; the font is released below.
            let font =
                unsafe { CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, 0.0, std::ptr::null()) };
            if font.is_null() {
                return Err("Failed to create system UI font".into());
            }
            // SAFETY: `font` is a valid CTFontRef.
            let upem = unsafe { CTFontGetUnitsPerEm(font) as f32 };
            let os2_avg = os2_avg_char_width(font);
            // SAFETY: releasing the temporary CTFontRef.
            unsafe { CFRelease(font) };
            (upem, os2_avg)
        } else {
            let bytes = data.as_slice();
            // SAFETY: CFDataCreate copies the byte slice for CoreText.
            let cf_data =
                unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
            if cf_data.is_null() {
                return Err("Failed to create CFData from font bytes".into());
            }

            // SAFETY: parses the TTC or TTF descriptors of `cf_data`.
            let descriptors = unsafe { CTFontManagerCreateFontDescriptorsFromData(cf_data) };
            if !descriptors.is_null() {
                // SAFETY: descriptors is a valid CFArrayRef.
                let count = unsafe { CFArrayGetCount(descriptors) };
                if (face_index as isize) < count {
                    // SAFETY: index checked against count; the descriptor is retained to outlive
                    // the array that owns it.
                    unsafe {
                        let desc = CFArrayGetValueAtIndex(descriptors, face_index as isize);
                        descriptor = CFRetain(desc);
                    }
                }
                // SAFETY: releasing descriptors array.
                unsafe { CFRelease(descriptors) };
            }

            if descriptor.is_null() {
                // SAFETY: wraps `cf_data`; the provider is released once the font holds it.
                unsafe {
                    let provider = CGDataProviderCreateWithCFData(cf_data);
                    if !provider.is_null() {
                        cg_font = CGFontCreateWithDataProvider(provider);
                        CFRelease(provider);
                    }
                }
            }

            // SAFETY: releasing cf_data, which CoreText has copied or retained as needed.
            unsafe { CFRelease(cf_data) };

            if descriptor.is_null() && cg_font.is_null() {
                return Err("Failed to create CoreText font descriptor or CGFont".into());
            }

            // SAFETY: creating temporary font to query units per em and tables.
            let tmp_font = unsafe {
                if !descriptor.is_null() {
                    CTFontCreateWithFontDescriptor(descriptor, 0.0, std::ptr::null())
                } else {
                    CTFontCreateWithGraphicsFont(cg_font, 0.0, std::ptr::null(), std::ptr::null())
                }
            };
            if tmp_font.is_null() {
                // SAFETY: releasing what was created above, which `Drop` would otherwise miss.
                unsafe {
                    if !descriptor.is_null() {
                        CFRelease(descriptor);
                    }
                    if !cg_font.is_null() {
                        CFRelease(cg_font);
                    }
                }
                return Err("Failed to instantiate CTFontRef".into());
            }

            // SAFETY: tmp_font is a valid CTFontRef.
            let upem = unsafe { CTFontGetUnitsPerEm(tmp_font) as f32 };
            let os2_avg = os2_avg_char_width(tmp_font);
            // SAFETY: releasing tmp_font.
            unsafe { CFRelease(tmp_font) };
            (upem, os2_avg)
        };

        let upem_final = if upem <= 0.0 { 1000.0 } else { upem };

        let face = {
            let bytes: &[u8] = data.as_slice();
            // SAFETY: `SharedFontData` is an `Arc` over font bytes that are never moved or written
            // while a clone exists; `_data` keeps one for as long as `face` lives, and the face is
            // only lent out for the lifetime of `&self`.
            let bytes_static: &'static [u8] =
                unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
            rustybuzz::ttf_parser::Face::parse(bytes_static, face_index).ok()
        };

        Ok(Self {
            face,
            _data: data,
            is_system_ui,
            descriptor,
            cg_font,
            units_per_em: upem_final,
            os2_avg_char_width: os2_avg,
            ct_cache: Mutex::new(HashMap::new()),
            subpixel_counts: Mutex::new(HashMap::new()),
        })
    }

    /// The `CTFontRef` at pixel size `px` (`QCoreTextFontEngine::ctfont`), made once per size;
    /// `None` when CoreText cannot make one.
    fn ctfont(&self, px: f32) -> Option<*const c_void> {
        let px = if px > 0.0 { px } else { 0.0 };
        let mut cache = self.ct_cache.lock().unwrap_or_else(|e| e.into_inner());
        let font = *cache.entry(px.to_bits()).or_insert_with(|| {
            // SAFETY: creates a CTFontRef at pixel size `px` from the handles owned by `self`.
            unsafe {
                if self.is_system_ui {
                    CTFontCreateUIFontForLanguage(UI_FONT_SYSTEM, f64::from(px), std::ptr::null())
                } else if !self.descriptor.is_null() {
                    CTFontCreateWithFontDescriptor(self.descriptor, f64::from(px), std::ptr::null())
                } else {
                    CTFontCreateWithGraphicsFont(
                        self.cg_font,
                        f64::from(px),
                        std::ptr::null(),
                        std::ptr::null(),
                    )
                }
            }
        });
        (!font.is_null()).then_some(font)
    }

    /// `QCoreTextFontEngine::boundingBox` of a glyph: the bounding rectangle and the advance.
    fn glyph_rect(ctfont: *const c_void, glyph_id: u16) -> (CGRect, CGSize) {
        let mut rect = CGRect::default();
        let mut advance = CGSize::default();
        // SAFETY: `ctfont` is valid; `glyph_id`, `rect` and `advance` are single values.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(ctfont, 0, &glyph_id, &mut rect, 1);
            CTFontGetAdvancesForGlyphs(ctfont, 0, &glyph_id, &mut advance, 1);
        }
        (rect, advance)
    }

    /// Placement of the bitmap `QCoreTextFontEngine` makes of a glyph: what
    /// `QTextureGlyphCache::populate` stores as the glyph's coordinates.
    fn bitmap_metrics(alpha_box: AlphaBox, advance: CGSize) -> GlyphMetrics {
        let width = alpha_box.pixel_width();
        let height = alpha_box.pixel_height();
        GlyphMetrics {
            xmin: alpha_box.x.floor() as i32,
            ymin: -(alpha_box.y.floor() as i32) - height as i32,
            width,
            height,
            advance_width: (q_fixed(advance.width)) as f32,
            advance_height: (q_fixed(advance.height)) as f32,
        }
    }

    /// `QCoreTextFontEngine::imageForGlyph` reduced to an alpha map (`alphaMapForGlyph`): the
    /// glyph drawn at the logical `size` under a device pixel ratio of `scale`, shifted right by
    /// the sub-pixel position `subpixel` (a fraction of a pixel). Top row first.
    fn render(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: f64,
    ) -> (GlyphMetrics, Vec<u8>) {
        if size <= 0.0 {
            return (GlyphMetrics::default(), Vec::new());
        }
        let Some(ctfont) = self.ctfont(size) else {
            return (GlyphMetrics::default(), Vec::new());
        };
        let scale = f64::from(scale);
        let (rect, advance) = Self::glyph_rect(ctfont, glyph_id);
        let alpha_box = AlphaBox::new(rect, scale);
        let metrics = Self::bitmap_metrics(alpha_box, advance);
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            // Nothing to draw (a space): Qt's blank bitmap changes no pixel.
            return (
                GlyphMetrics {
                    advance_width: metrics.advance_width,
                    advance_height: metrics.advance_height,
                    ..GlyphMetrics::default()
                },
                Vec::new(),
            );
        }
        let (width, height) = (metrics.width, metrics.height);
        if width == 0 || height == 0 || exceeds_glyph_budget(width, height) {
            return (metrics, Vec::new());
        }

        let smooth = should_smooth_font();
        // Grayscale smoothing darkens by the fill colour, so Qt draws what it will blit, black
        // on white, and inverts the result into an alpha map.
        let black_on_white = smooth && font_smoothing() == FontSmoothing::Grayscale;
        let mut pixels = vec![if black_on_white { 0xffff_ffffu32 } else { 0 }; width * height];
        let pos_x = -alpha_box.x.floor() + subpixel;
        let pos_y = height as f64 + alpha_box.y;
        // SAFETY: `pixels` outlives the context, which is released before the block ends; the
        // constants are CoreGraphics colours and `ctfont` is valid.
        unsafe {
            let ctx = rgb32_context(&mut pixels, width, height);
            if ctx.is_null() {
                return (metrics, Vec::new());
            }
            CGContextSetShouldAntialias(ctx, 1);
            CGContextSetShouldSmoothFonts(ctx, u8::from(smooth));
            let text_matrix = if scale == 1.0 {
                CGAffineTransform::IDENTITY
            } else {
                CGAffineTransform::scale(scale)
            };
            CGContextSetTextMatrix(ctx, text_matrix);
            let color = CGColorGetConstantColor(if black_on_white {
                kCGColorBlack
            } else {
                kCGColorWhite
            });
            CGContextSetFillColorWithColor(ctx, color);
            CGContextSetTextDrawingMode(ctx, 0);
            CGContextSetTextPosition(ctx, pos_x, pos_y);
            CTFontDrawGlyphs(ctfont, &glyph_id, &CGPoint::default(), 1, ctx);
            CGContextRelease(ctx);
        }

        // `qGray` of the (possibly inverted) RGB32 pixel.
        let coverage = pixels
            .iter()
            .map(|&px| {
                let px = if black_on_white { !px } else { px };
                let (r, g, b) = ((px >> 16) & 0xff, (px >> 8) & 0xff, px & 0xff);
                ((r * 11 + g * 16 + b * 5) / 32) as u8
            })
            .collect();
        (metrics, coverage)
    }

    /// Whether the glyph has an outline (`QPainterPath::isEmpty` of `addGlyphsToPath`).
    fn has_outline(&self, glyph_id: u16, size: f32) -> bool {
        let Some(ctfont) = self.ctfont(size) else {
            return false;
        };
        // SAFETY: `ctfont` is valid; a null path or an empty one is an empty glyph.
        unsafe {
            let path = CTFontCreatePathForGlyph(ctfont, glyph_id, std::ptr::null());
            if path.is_null() {
                return false;
            }
            let empty = CGPathIsEmpty(path) != 0;
            CFRelease(path);
            !empty
        }
    }

    /// `QFontEngine::m_subPixelPositionCount` at `size`: the number of different bitmaps CoreGraphics
    /// draws for a glyph at twelve offsets of a pixel, decided by the first glyph with an outline
    /// that is positioned (`QTextureGlyphCache::calculateSubPixelPositionCount`, `populate`).
    /// 0 while no such glyph has been seen.
    fn subpixel_count(&self, glyph_id: u16, size: f32, scale: f32) -> i64 {
        let key = size.to_bits();
        let mut counts = self
            .subpixel_counts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(&count) = counts.get(&key) {
            return count;
        }
        if !self.has_outline(glyph_id, size) {
            return 0;
        }
        let mut images: Vec<(usize, usize, Vec<u8>)> = Vec::new();
        for i in 0..12 {
            let (metrics, bitmap) =
                self.render(glyph_id, size, scale, q_fixed(f64::from(i) / 12.0));
            let image = (metrics.width, metrics.height, bitmap);
            if !images.contains(&image) {
                images.push(image);
            }
        }
        let count = images.len() as i64;
        counts.insert(key, count);
        count
    }
}

impl Drop for CoreTextFace {
    fn drop(&mut self) {
        if let Ok(mut cache) = self.ct_cache.lock() {
            for (_, ctfont) in cache.drain() {
                if !ctfont.is_null() {
                    // SAFETY: releasing cached CTFontRef.
                    unsafe { CFRelease(ctfont) };
                }
            }
        }
        if !self.descriptor.is_null() {
            // SAFETY: releasing descriptor retained in `build`.
            unsafe { CFRelease(self.descriptor) };
        }
        if !self.cg_font.is_null() {
            // SAFETY: releasing CGFontRef created in `build`.
            unsafe { CFRelease(self.cg_font) };
        }
    }
}

impl GlyphFace for CoreTextFace {
    fn glyph_index(&self, ch: char) -> u16 {
        if let Some(face) = &self.face
            && let Some(gid) = face.glyph_index(ch)
        {
            return gid.0;
        }
        let mut buf = [0u16; 2];
        let encoded = ch.encode_utf16(&mut buf);
        let mut glyphs = [0u16; 2];
        let Some(ctfont) = self.ctfont(0.0) else {
            return 0;
        };
        // SAFETY: ctfont is valid; encoded and glyphs buffers match length.
        let ok = unsafe {
            CTFontGetGlyphsForCharacters(
                ctfont,
                encoded.as_ptr(),
                glyphs.as_mut_ptr(),
                encoded.len() as isize,
            )
        };
        if ok != 0 { glyphs[0] } else { 0 }
    }

    fn metrics_indexed(&self, glyph_id: u16, px: f32) -> GlyphMetrics {
        if px <= 0.0 {
            return GlyphMetrics::default();
        }
        let Some(ctfont) = self.ctfont(px) else {
            return GlyphMetrics::default();
        };
        let (rect, advance) = Self::glyph_rect(ctfont, glyph_id);
        let metrics = Self::bitmap_metrics(AlphaBox::new(rect, 1.0), advance);
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return GlyphMetrics {
                advance_width: metrics.advance_width,
                advance_height: metrics.advance_height,
                ..GlyphMetrics::default()
            };
        }
        metrics
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, px, 1.0, 0.0)
    }

    fn rasterize_scaled(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, size, scale, 0.0)
    }

    fn supports_horizontal_subpixel_positions(&self) -> bool {
        true
    }

    fn subpixel_position(&self, glyph_id: u16, size: f32, scale: f32, x: i64) -> i32 {
        let count = self.subpixel_count(glyph_id, size, scale);
        if count <= 1 || x == 0 {
            return 0;
        }
        // `QFontEngine::subPixelPositionFor`: the fraction of the position, plus one 26.6 unit
        // against the precision lost in fixed point, floored to a multiple of 1 / count.
        let v = (x & 63) + 1;
        let fraction = (v * count) >> 6;
        ((fraction << 6) / count) as i32
    }

    fn rasterize_subpixel(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
        subpixel: i32,
    ) -> (GlyphMetrics, Vec<u8>) {
        self.render(glyph_id, size, scale, f64::from(subpixel) / 64.0)
    }

    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        if px <= 0.0 {
            return None;
        }
        let ctfont = self.ctfont(px)?;
        // SAFETY: ctfont is a valid CTFontRef.
        let (asc, desc, lead) = unsafe {
            (
                CTFontGetAscent(ctfont),
                CTFontGetDescent(ctfont),
                CTFontGetLeading(ctfont),
            )
        };
        Some(LineMetrics {
            ascent: q_fixed(asc) as f32,
            descent: q_fixed(desc) as f32,
            leading: q_fixed(lead) as f32,
        })
    }

    fn average_char_width(&self, px: f32) -> Option<f32> {
        if px <= 0.0 {
            return None;
        }
        if let Some(w) = self.os2_avg_char_width {
            Some(q_fixed(f64::from(w) * f64::from(px) / f64::from(self.units_per_em)) as f32)
        } else {
            let gid = self.glyph_index('x');
            if gid != 0 {
                Some(self.metrics_indexed(gid, px).advance_width)
            } else {
                None
            }
        }
    }

    fn cap_height(&self, px: f32) -> Option<f32> {
        if px <= 0.0 {
            return None;
        }
        let ctfont = self.ctfont(px)?;
        // SAFETY: ctfont is a valid CTFontRef.
        let c = unsafe { CTFontGetCapHeight(ctfont) };
        if c > 0.0 {
            Some(q_fixed(c) as f32)
        } else {
            let gid = self.glyph_index('H');
            if gid != 0 {
                Some(self.metrics_indexed(gid, px).height as f32)
            } else {
                None
            }
        }
    }

    fn glyph_outline(&self, glyph_id: u16, px: f32) -> Option<tiny_skia::Path> {
        if px <= 0.0 {
            return None;
        }
        let ctfont = self.ctfont(px)?;
        // Matrix with a (1, -1) y-flip matching Qt's addGlyphsToPath.
        let matrix = CGAffineTransform {
            d: -1.0,
            ..CGAffineTransform::IDENTITY
        };
        // SAFETY: ctfont is a valid CTFontRef.
        let path = unsafe { CTFontCreatePathForGlyph(ctfont, glyph_id, &matrix) };
        if path.is_null() {
            return None;
        }

        struct BuilderCtx {
            builder: tiny_skia::PathBuilder,
        }
        extern "C" fn apply_elem(info: *mut c_void, element: *const CGPathElement) {
            // SAFETY: `info` is the live `BuilderCtx` passed to `CGPathApply` below and `element`
            // is valid for the call, its `points` holding as many points as its type needs.
            unsafe {
                let ctx = &mut *(info as *mut BuilderCtx);
                let elem = &*element;
                match elem.type_ {
                    0 => {
                        let pt = *elem.points;
                        ctx.builder.move_to(pt.x as f32, pt.y as f32);
                    }
                    1 => {
                        let pt = *elem.points;
                        ctx.builder.line_to(pt.x as f32, pt.y as f32);
                    }
                    2 => {
                        let pt1 = *elem.points;
                        let pt2 = *elem.points.add(1);
                        ctx.builder
                            .quad_to(pt1.x as f32, pt1.y as f32, pt2.x as f32, pt2.y as f32);
                    }
                    3 => {
                        let pt1 = *elem.points;
                        let pt2 = *elem.points.add(1);
                        let pt3 = *elem.points.add(2);
                        ctx.builder.cubic_to(
                            pt1.x as f32,
                            pt1.y as f32,
                            pt2.x as f32,
                            pt2.y as f32,
                            pt3.x as f32,
                            pt3.y as f32,
                        );
                    }
                    4 => {
                        ctx.builder.close();
                    }
                    _ => {}
                }
            }
        }

        let mut ctx = BuilderCtx {
            builder: tiny_skia::PathBuilder::new(),
        };
        // SAFETY: path is a valid CGPathRef, apply_elem matches the callback signature and `ctx`
        // outlives the call.
        unsafe {
            CGPathApply(path, &mut ctx as *mut BuilderCtx as *mut c_void, apply_elem);
            CFRelease(path);
        }
        ctx.builder.finish()
    }

    fn layout_advance_width(&self, glyph_id: u16, size: f32, _direct_write: bool) -> Option<f32> {
        if size <= 0.0 {
            return None;
        }
        let ctfont = self.ctfont(size)?;
        let (_, advance) = Self::glyph_rect(ctfont, glyph_id);
        Some(q_fixed(advance.width) as f32)
    }

    fn right_side_bearing(&self, glyph_id: u16, size: f32) -> Option<f32> {
        if size <= 0.0 {
            return None;
        }
        let ctfont = self.ctfont(size)?;
        let (rect, advance) = Self::glyph_rect(ctfont, glyph_id);
        let rsb = q_fixed(advance.width) - q_fixed(rect.origin.x) - q_fixed(rect.size.width);
        Some(rsb as f32)
    }
}
