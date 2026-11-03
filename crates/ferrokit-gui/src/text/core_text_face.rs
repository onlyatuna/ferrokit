// SPDX-License-Identifier: LGPL-3.0-only
//! macOS glyph backend: CoreText rasterizes glyphs, following Qt's `QCoreTextFontEngine`
//! (`src/gui/text/coretext/qfontengine_coretext.mm`).
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
//! * Glyph rasterization: `CGBitmapContextCreate` (RGB32 context matching Qt's `imageForGlyph`,
//!   rendered black-on-white and inverted to match Mojave+ grayscale stem darkening dilation),
//!   drawn with `CTFontDrawGlyphs` and extracted top-row-first.
//! * System UI font: optical sizing and metrics come from `CTFontCreateUIFontForLanguage(kCTFontUIFontSystem)`.

use crate::text::font::SharedFontData;
use crate::text::glyph_face::{GlyphFace, GlyphMetrics, LineMetrics, exceeds_glyph_budget};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;

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

#[repr(C)]
struct CGPathElement {
    type_: i32,
    points: *mut CGPoint,
}

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
        boundingRects: *mut CGRect,
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
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *const c_void;
    fn CGFontCreateWithDataProvider(provider: *const c_void) -> *const c_void;
    fn CGColorSpaceCreateDeviceRGB() -> *const c_void;
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
    fn CGContextSetAllowsFontSmoothing(context: *mut c_void, allows_font_smoothing: u8);
    fn CGContextSetAllowsFontSubpixelPositioning(context: *mut c_void, allows: u8);
    fn CGContextSetAllowsFontSubpixelQuantization(context: *mut c_void, allows: u8);
    fn CGContextSetTextDrawingMode(context: *mut c_void, mode: i32);
    fn CGContextSetRGBFillColor(context: *mut c_void, red: f64, green: f64, blue: f64, alpha: f64);
    fn CGContextSetTextPosition(context: *mut c_void, x: f64, y: f64);
    fn CGContextRelease(context: *mut c_void);
    fn CGPathApply(
        path: *const c_void,
        info: *mut c_void,
        function: extern "C" fn(*mut c_void, *const CGPathElement),
    );
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFDataGetLength(the_data: *const c_void) -> isize;
    fn CFDataGetBytePtr(the_data: *const c_void) -> *const u8;
    fn CFArrayGetCount(the_array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(the_array: *const c_void, idx: isize) -> *const c_void;
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// A font face rasterized and measured by Apple CoreText.
pub struct CoreTextFace {
    face: Option<rustybuzz::ttf_parser::Face<'static>>,
    _data: SharedFontData,
    _face_index: u32,
    is_system_ui: bool,
    descriptor: *const c_void,
    cg_font: *const c_void,
    units_per_em: f32,
    os2_avg_char_width: Option<f32>,
    ct_cache: Mutex<HashMap<u32, *const c_void>>,
}

// SAFETY: All CoreFoundation/CoreText/CoreGraphics objects referenced (CTFontDescriptor, CGFont,
// and cached CTFont pointers) are immutable handles designed for multithreaded read access.
// Mutation of the size cache is synchronized by a Mutex.
unsafe impl Send for CoreTextFace {}
// SAFETY: Immutable access to fields and synchronized cache access via Mutex guarantee thread safety.
unsafe impl Sync for CoreTextFace {}

impl CoreTextFace {
    /// Builds a new CoreText-backed font face.
    pub fn new(data: SharedFontData, face_index: u32, is_system_ui: bool) -> Result<Self, String> {
        let mut descriptor = std::ptr::null();
        let mut cg_font = std::ptr::null();

        let (upem, os2_avg) = if is_system_ui {
            // SAFETY: UI type 2 is kCTFontUIFontSystem.
            let font = unsafe { CTFontCreateUIFontForLanguage(2, 0.0, std::ptr::null()) };
            if font.is_null() {
                return Err("Failed to create system UI font".into());
            }
            // SAFETY: `font` is a valid CTFontRef.
            let upem = unsafe { CTFontGetUnitsPerEm(font) as f32 };
            // SAFETY: Tag 0x4F532F32 is "OS/2".
            let os2_table = unsafe { CTFontCopyTable(font, 0x4F53_2F32, 0) };
            let os2_avg = if !os2_table.is_null() {
                // SAFETY: `os2_table` is a valid CFDataRef.
                let len = unsafe { CFDataGetLength(os2_table) };
                // SAFETY: `os2_table` is a valid CFDataRef.
                let ptr = unsafe { CFDataGetBytePtr(os2_table) };
                let val = if len >= 10 && !ptr.is_null() {
                    // SAFETY: offset 2 is xAvgCharWidth (i16).
                    let w = unsafe { i16::from_be_bytes([*ptr.add(2), *ptr.add(3)]) };
                    Some(w.unsigned_abs() as f32)
                } else {
                    None
                };
                // SAFETY: releasing owned CFDataRef.
                unsafe { CFRelease(os2_table) };
                val
            } else {
                None
            };
            // SAFETY: releasing temporary CTFontRef.
            unsafe { CFRelease(font) };
            (upem, os2_avg)
        } else {
            let bytes = data.as_slice();
            // SAFETY: CFDataCreate wraps byte slice for CoreText descriptors.
            let cf_data =
                unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize) };
            if cf_data.is_null() {
                return Err("Failed to create CFData from font bytes".into());
            }

            // SAFETY: CTFontManagerCreateFontDescriptorsFromData parses TTC or TTF descriptors.
            let descriptors = unsafe { CTFontManagerCreateFontDescriptorsFromData(cf_data) };
            if !descriptors.is_null() {
                // SAFETY: descriptors is a valid CFArrayRef.
                let count = unsafe { CFArrayGetCount(descriptors) };
                if (face_index as isize) < count {
                    // SAFETY: index checked against count.
                    let desc = unsafe { CFArrayGetValueAtIndex(descriptors, face_index as isize) };
                    // SAFETY: retaining the descriptor to keep it alive for the face.
                    unsafe { CFRetain(desc) };
                    descriptor = desc;
                }
                // SAFETY: releasing descriptors array.
                unsafe { CFRelease(descriptors) };
            }

            if descriptor.is_null() {
                // SAFETY: CGDataProviderCreateWithCFData wraps font data.
                let provider = unsafe { CGDataProviderCreateWithCFData(cf_data) };
                if !provider.is_null() {
                    // SAFETY: provider is valid CGDataProviderRef.
                    cg_font = unsafe { CGFontCreateWithDataProvider(provider) };
                    // SAFETY: releasing provider after font creation.
                    unsafe { CFRelease(provider) };
                }
            }

            // SAFETY: releasing cf_data.
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
                return Err("Failed to instantiate CTFontRef".into());
            }

            // SAFETY: tmp_font is a valid CTFontRef.
            let upem = unsafe { CTFontGetUnitsPerEm(tmp_font) as f32 };
            // SAFETY: Tag 0x4F532F32 is "OS/2".
            let os2_table = unsafe { CTFontCopyTable(tmp_font, 0x4F53_2F32, 0) };
            let os2_avg = if !os2_table.is_null() {
                // SAFETY: `os2_table` is a valid CFDataRef.
                let len = unsafe { CFDataGetLength(os2_table) };
                // SAFETY: `os2_table` is a valid CFDataRef.
                let ptr = unsafe { CFDataGetBytePtr(os2_table) };
                let val = if len >= 10 && !ptr.is_null() {
                    // SAFETY: offset 2 is xAvgCharWidth (i16).
                    let w = unsafe { i16::from_be_bytes([*ptr.add(2), *ptr.add(3)]) };
                    Some(w.unsigned_abs() as f32)
                } else {
                    None
                };
                // SAFETY: releasing owned CFDataRef.
                unsafe { CFRelease(os2_table) };
                val
            } else {
                None
            };
            // SAFETY: releasing tmp_font.
            unsafe { CFRelease(tmp_font) };
            (upem, os2_avg)
        };

        let upem_final = if upem <= 0.0 { 1000.0 } else { upem };

        let face = {
            let bytes: &[u8] = data.as_slice();
            // SAFETY: `bytes` points into the buffer kept alive by `_data`.
            let bytes_static: &'static [u8] =
                unsafe { std::mem::transmute::<&[u8], &'static [u8]>(bytes) };
            rustybuzz::ttf_parser::Face::parse(bytes_static, face_index).ok()
        };

        Ok(Self {
            face,
            _data: data,
            _face_index: face_index,
            is_system_ui,
            descriptor,
            cg_font,
            units_per_em: upem_final,
            os2_avg_char_width: os2_avg,
            ct_cache: Mutex::new(HashMap::new()),
        })
    }

    /// Gets or creates a `CTFontRef` at the given pixel size `px`.
    fn get_or_create_ctfont(&self, px: f32) -> *const c_void {
        let px_clamped = if px <= 0.0 { 0.0 } else { px };
        let key = px_clamped.to_bits();
        let mut cache = self.ct_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&font) = cache.get(&key) {
            return font;
        }

        // SAFETY: creating CTFontRef at pixel size `px_clamped`.
        let font = unsafe {
            if self.is_system_ui {
                CTFontCreateUIFontForLanguage(2, px_clamped as f64, std::ptr::null())
            } else if !self.descriptor.is_null() {
                CTFontCreateWithFontDescriptor(self.descriptor, px_clamped as f64, std::ptr::null())
            } else {
                CTFontCreateWithGraphicsFont(
                    self.cg_font,
                    px_clamped as f64,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            }
        };
        cache.insert(key, font);
        font
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
            // SAFETY: releasing descriptor retained in `new`.
            unsafe { CFRelease(self.descriptor) };
        }
        if !self.cg_font.is_null() {
            // SAFETY: releasing CGFontRef created in `new`.
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
        let ctfont = self.get_or_create_ctfont(0.0);
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
        let ctfont = self.get_or_create_ctfont(px);
        let mut rect = CGRect::default();
        let mut adv = CGSize::default();
        let g = glyph_id;
        // SAFETY: ctfont is valid; g, rect, and adv point to single values.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(ctfont, 0, &g, &mut rect, 1);
            CTFontGetAdvancesForGlyphs(ctfont, 0, &g, &mut adv, 1);
        }
        let advance_width = ((adv.width * 64.0).trunc() / 64.0) as f32;
        let advance_height = ((adv.height * 64.0).trunc() / 64.0) as f32;

        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            return GlyphMetrics {
                advance_width,
                advance_height,
                ..GlyphMetrics::default()
            };
        }

        let fixed = |v: f64| (v * 64.0).trunc() / 64.0;
        let mut br_w = fixed(rect.size.width);
        let mut br_h = fixed(rect.size.height);
        let mut br_x = fixed(rect.origin.x);
        let mut br_y = -fixed(rect.origin.y) - br_h;

        // Qt's QCoreTextFontEngine::alphaMapBoundingBox padding for Format_A8:
        // x -= 1, width += 3, y -= 1, height += 2.
        br_x -= 1.0;
        br_w += 3.0;
        br_y -= 1.0;
        br_h += 2.0;

        let width = br_w.ceil() as usize;
        let height = br_h.ceil() as usize;
        let xmin = br_x.floor() as i32;
        let ymin = -br_y.floor() as i32 - height as i32;

        GlyphMetrics {
            xmin,
            ymin,
            width,
            height,
            advance_width,
            advance_height,
        }
    }

    fn rasterize_indexed(&self, glyph_id: u16, px: f32) -> (GlyphMetrics, Vec<u8>) {
        self.rasterize_scaled(glyph_id, px, 1.0)
    }

    fn rasterize_scaled(&self, glyph_id: u16, size: f32, scale: f32) -> (GlyphMetrics, Vec<u8>) {
        let px = size * scale;
        if px <= 0.0 {
            return (GlyphMetrics::default(), Vec::new());
        }
        let m = self.metrics_indexed(glyph_id, px);
        if m.width == 0 || m.height == 0 || exceeds_glyph_budget(m.width, m.height) {
            return (m, Vec::new());
        }

        let ctfont = self.get_or_create_ctfont(px);
        let mut rect = CGRect::default();
        let g = glyph_id;
        // SAFETY: ctfont is valid; rect receives bounding box of glyph g.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(ctfont, 0, &g, &mut rect, 1);
        }
        let fixed = |v: f64| (v * 64.0).trunc() / 64.0;
        let br_h = fixed(rect.size.height);
        let br_x = fixed(rect.origin.x) - 1.0;
        let br_y = -fixed(rect.origin.y) - br_h - 1.0;

        let pos_x = -br_x.floor();
        let pos_y = m.height as f64 + br_y;

        // Allocate RGB32 buffer (matching Qt's QCoreTextFontEngine::imageForGlyph)
        let mut buf = vec![0xFFFFFFFFu32; m.width * m.height];
        // SAFETY: creating RGB CGBitmapContext, configuring antialiasing/smoothing flags,
        // drawing the glyph at (pos_x, pos_y), and releasing context.
        unsafe {
            let cs = CGColorSpaceCreateDeviceRGB();
            if cs.is_null() {
                return (m, Vec::new());
            }
            let ctx = CGBitmapContextCreate(
                buf.as_mut_ptr() as *mut c_void,
                m.width,
                m.height,
                8,
                m.width * 4,
                cs,
                // kCGImageAlphaNoneSkipFirst | kCGBitmapByteOrder32Host (6 | 0x2000 = 8198)
                8198,
            );
            CFRelease(cs);
            if ctx.is_null() {
                return (m, Vec::new());
            }
            CGContextSetShouldAntialias(ctx, 1);
            CGContextSetShouldSmoothFonts(ctx, 1);
            CGContextSetAllowsFontSmoothing(ctx, 1);
            CGContextSetAllowsFontSubpixelPositioning(ctx, 1);
            CGContextSetAllowsFontSubpixelQuantization(ctx, 1);
            CGContextSetRGBFillColor(ctx, 0.0, 0.0, 0.0, 1.0);
            CGContextSetTextDrawingMode(ctx, 0); // kCGTextFill = 0
            CGContextSetTextPosition(ctx, pos_x, pos_y);
            let pt = CGPoint { x: 0.0, y: 0.0 };
            CTFontDrawGlyphs(ctfont, &g, &pt, 1, ctx);
            CGContextRelease(ctx);
        }

        // Convert to top-row-first 8-bit coverage using Qt's light-mode inverted qGray formula.
        // In CGBitmapContext, row 0 in memory is bottom row (y = 0).
        let mut coverage = vec![0u8; m.width * m.height];
        for r in 0..m.height {
            let src_row = (m.height - 1 - r) * m.width;
            let dst_row = r * m.width;
            for c in 0..m.width {
                let val = buf[src_row + c];
                let b = 255 - (val & 0xFF) as u8;
                let g_col = 255 - ((val >> 8) & 0xFF) as u8;
                let r_col = 255 - ((val >> 16) & 0xFF) as u8;
                let gray = ((r_col as u32 * 11 + g_col as u32 * 16 + b as u32 * 5) / 32) as u8;
                coverage[dst_row + c] = gray;
            }
        }
        (m, coverage)
    }

    fn rasterize_lcd_scaled(
        &self,
        glyph_id: u16,
        size: f32,
        scale: f32,
    ) -> Option<(GlyphMetrics, Vec<u8>)> {
        let px = size * scale;
        if px <= 0.0 {
            return None;
        }
        let m = self.metrics_indexed(glyph_id, px);
        if m.width == 0 || m.height == 0 || exceeds_glyph_budget(m.width, m.height) {
            return Some((m, Vec::new()));
        }

        let ctfont = self.get_or_create_ctfont(px);
        let mut rect = CGRect::default();
        let g = glyph_id;
        // SAFETY: ctfont is valid; rect receives bounding box of glyph g.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(ctfont, 0, &g, &mut rect, 1);
        }
        let fixed = |v: f64| (v * 64.0).trunc() / 64.0;
        let br_h = fixed(rect.size.height);
        let br_x = fixed(rect.origin.x) - 1.0;
        let br_y = -fixed(rect.origin.y) - br_h - 1.0;

        let pos_x = -br_x.floor();
        let pos_y = m.height as f64 + br_y;

        let mut buf = vec![0xFFFFFFFFu32; m.width * m.height];
        // SAFETY: creating RGB CGBitmapContext and drawing glyph.
        unsafe {
            let cs = CGColorSpaceCreateDeviceRGB();
            if cs.is_null() {
                return None;
            }
            let ctx = CGBitmapContextCreate(
                buf.as_mut_ptr() as *mut c_void,
                m.width,
                m.height,
                8,
                m.width * 4,
                cs,
                8198,
            );
            CFRelease(cs);
            if ctx.is_null() {
                return None;
            }
            CGContextSetShouldAntialias(ctx, 1);
            CGContextSetShouldSmoothFonts(ctx, 1);
            CGContextSetAllowsFontSmoothing(ctx, 1);
            CGContextSetAllowsFontSubpixelPositioning(ctx, 1);
            CGContextSetAllowsFontSubpixelQuantization(ctx, 1);
            CGContextSetRGBFillColor(ctx, 0.0, 0.0, 0.0, 1.0);
            CGContextSetTextDrawingMode(ctx, 0);
            CGContextSetTextPosition(ctx, pos_x, pos_y);
            let pt = CGPoint { x: 0.0, y: 0.0 };
            CTFontDrawGlyphs(ctfont, &g, &pt, 1, ctx);
            CGContextRelease(ctx);
        }

        let mut texture = vec![0u8; m.width * m.height * 3];
        for r in 0..m.height {
            let src_row = (m.height - 1 - r) * m.width;
            let dst_row = r * m.width * 3;
            for c in 0..m.width {
                let val = buf[src_row + c];
                let b = 255 - (val & 0xFF) as u8;
                let g_col = 255 - ((val >> 8) & 0xFF) as u8;
                let r_col = 255 - ((val >> 16) & 0xFF) as u8;
                let out_idx = dst_row + c * 3;
                texture[out_idx] = r_col;
                texture[out_idx + 1] = g_col;
                texture[out_idx + 2] = b;
            }
        }
        Some((m, texture))
    }

    fn line_metrics(&self, px: f32) -> Option<LineMetrics> {
        if px <= 0.0 {
            return None;
        }
        let ctfont = self.get_or_create_ctfont(px);
        // SAFETY: ctfont is a valid CTFontRef.
        let (asc, desc, lead) = unsafe {
            (
                CTFontGetAscent(ctfont),
                CTFontGetDescent(ctfont),
                CTFontGetLeading(ctfont),
            )
        };
        let fixed = |v: f64| ((v * 64.0).trunc() / 64.0) as f32;
        Some(LineMetrics {
            ascent: fixed(asc),
            descent: fixed(desc),
            leading: fixed(lead),
        })
    }

    fn average_char_width(&self, px: f32) -> Option<f32> {
        if px <= 0.0 {
            return None;
        }
        if let Some(w) = self.os2_avg_char_width {
            let fixed = ((f64::from(w) * f64::from(px) / f64::from(self.units_per_em) * 64.0)
                .trunc()
                / 64.0) as f32;
            Some(fixed)
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
        let ctfont = self.get_or_create_ctfont(px);
        // SAFETY: ctfont is a valid CTFontRef.
        let c = unsafe { CTFontGetCapHeight(ctfont) };
        if c > 0.0 {
            let fixed = ((c * 64.0).trunc() / 64.0) as f32;
            Some(fixed)
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
        let ctfont = self.get_or_create_ctfont(px);
        // Matrix with a (1, -1) y-flip matching Qt's addGlyphsToPath.
        let matrix = CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            tx: 0.0,
            ty: 0.0,
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
            // SAFETY: info points to live BuilderCtx; element is valid.
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
        // SAFETY: path is a valid CGPathRef, apply_elem matches signature.
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
        let ctfont = self.get_or_create_ctfont(size);
        let mut advance = CGSize::default();
        let g = glyph_id;
        // SAFETY: ctfont is valid; advance receives glyph advance.
        unsafe {
            CTFontGetAdvancesForGlyphs(ctfont, 0, &g, &mut advance, 1);
        }
        let fixed = ((advance.width * 64.0).trunc() / 64.0) as f32;
        Some(fixed)
    }

    fn right_side_bearing(&self, glyph_id: u16, size: f32) -> Option<f32> {
        if size <= 0.0 {
            return None;
        }
        let ctfont = self.get_or_create_ctfont(size);
        let mut rect = CGRect::default();
        let mut adv = CGSize::default();
        let g = glyph_id;
        // SAFETY: ctfont is valid.
        unsafe {
            CTFontGetBoundingRectsForGlyphs(ctfont, 0, &g, &mut rect, 1);
            CTFontGetAdvancesForGlyphs(ctfont, 0, &g, &mut adv, 1);
        }
        let fixed = |v: f64| (v * 64.0).trunc() / 64.0;
        let rsb = fixed(adv.width) - fixed(rect.origin.x) - fixed(rect.size.width);
        Some(rsb as f32)
    }
}
