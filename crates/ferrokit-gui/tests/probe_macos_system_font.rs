#![cfg(target_os = "macos")]
// Throwaway probe: what does CoreText call the system UI font, where is its file, does ferrokit
// draw it, and what does real Qt report for a missing family.
use ferrokit_gui::geometry::primitives::PointF;
use ferrokit_gui::paint::{Painter, Pen, Pixmap};
use ferrokit_gui::text::font::{Font, SharedFontData};
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_gui::tiny_skia::Color;
use std::ffi::{c_char, c_void};

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontCreateUIFontForLanguage(
        ui_type: i32,
        size: f64,
        language: *const c_void,
    ) -> *const c_void;
    fn CTFontCopyFontDescriptor(font: *const c_void) -> *const c_void;
    fn CTFontDescriptorCopyAttribute(d: *const c_void, attr: *const c_void) -> *const c_void;
    static kCTFontURLAttribute: *const c_void;
    static kCTFontFamilyNameAttribute: *const c_void;
    static kCTFontNameAttribute: *const c_void;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLGetFileSystemRepresentation(
        url: *const c_void,
        resolve: u8,
        buf: *mut u8,
        max: isize,
    ) -> u8;
    fn CFStringGetCString(s: *const c_void, buf: *mut c_char, size: isize, enc: u32) -> u8;
    fn CFRelease(x: *const c_void);
}

fn cfstr(s: *const c_void) -> String {
    if s.is_null() {
        return "<null>".into();
    }
    let mut buf = vec![0 as c_char; 512];
    // SAFETY: `s` is a CFString, `buf` is 512 bytes, 0x08000100 is kCFStringEncodingUTF8.
    let ok = unsafe { CFStringGetCString(s, buf.as_mut_ptr(), 512, 0x0800_0100) };
    if ok == 0 {
        return "<conv failed>".into();
    }
    // SAFETY: CFStringGetCString NUL-terminated the buffer.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

fn run(cmd: &str) -> String {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn probe() {
    let mut report = String::from("\n=== PROBE ===\n");
    for (name, ty) in [("System", 2), ("User", 0)] {
        // SAFETY: plain CoreText/CoreFoundation calls; every returned object is released.
        unsafe {
            let font = CTFontCreateUIFontForLanguage(ty, 0.0, std::ptr::null());
            if font.is_null() {
                report += &format!("{name}: null font\n");
                continue;
            }
            let desc = CTFontCopyFontDescriptor(font);
            let fam = CTFontDescriptorCopyAttribute(desc, kCTFontFamilyNameAttribute);
            let ps = CTFontDescriptorCopyAttribute(desc, kCTFontNameAttribute);
            let url = CTFontDescriptorCopyAttribute(desc, kCTFontURLAttribute);
            let mut path = vec![0u8; 1024];
            let ok = if url.is_null() {
                0
            } else {
                CFURLGetFileSystemRepresentation(url, 1, path.as_mut_ptr(), 1024)
            };
            let p = if ok != 0 {
                String::from_utf8_lossy(&path[..path.iter().position(|&b| b == 0).unwrap_or(0)])
                    .into_owned()
            } else {
                "<no url>".into()
            };
            report += &format!(
                "{name}: family={:?} postscript={:?} path={p}\n",
                cfstr(fam),
                cfstr(ps)
            );
            if ok != 0 {
                match SharedFontData::from_file(std::path::Path::new(&p)) {
                    Ok(data) => {
                        let f = Font::new("ProbeSys", 13.0).with_font_data(data);
                        let m = FontMetrics::from_font(&f);
                        let mut pm = Pixmap::with_dpr(300, 60, 1.0).unwrap();
                        let mut painter = Painter::begin(&mut pm);
                        painter.set_pen(Some(Pen::new(Color::from_rgba8(255, 255, 255, 255), 1.0)));
                        painter.draw_text(PointF::new(4.0, 30.0), "5h 42% Wg", &f);
                        drop(painter);
                        let lit = pm.data().iter().filter(|&&b| b != 0).count();
                        report += &format!(
                            "  ferrokit in-memory {name}: asc={} desc={} h={} avg={} lit_bytes={lit}\n",
                            m.ascent,
                            m.descent,
                            m.height,
                            m.rounded_average_char_width()
                        );
                    }
                    Err(e) => report += &format!("  from_file failed: {e}\n"),
                }
            }
            for x in [fam, ps, url] {
                if !x.is_null() {
                    CFRelease(x);
                }
            }
            CFRelease(desc);
            CFRelease(font);
        }
    }

    // What ferrokit does today for a missing family with the default hint.
    for fam in ["NoSuchFontXYZ", "Segoe UI", "Helvetica", "Arial"] {
        let f = Font::new(fam, 16.0);
        let m = FontMetrics::from_font(&f);
        report += &format!(
            "ferrokit today {fam:14} 16px asc={} desc={} h={} avg={} engine={:?}\n",
            m.ascent,
            m.descent,
            m.height,
            m.rounded_average_char_width(),
            FontMetrics::engine_metrics(&f)
        );
    }

    report += &coretext_outline_probe();
    // SFNS.ttf: why does it draw nothing?
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        report += &format!("SFNS bytes={}\n", bytes.len());
        if let Ok(face) = rustybuzz::ttf_parser::Face::parse(&bytes, 0) {
            report += &format!(
                "  ttf_parser: glyphs={} variable={} upem={} W={:?} 5={:?}\n",
                face.number_of_glyphs(),
                face.is_variable(),
                face.units_per_em(),
                face.glyph_index('W'),
                face.glyph_index('5')
            );
            let raw = face.raw_face();
            for t in [
                "glyf", "loca", "CFF ", "CFF2", "gvar", "fvar", "avar", "HVAR", "MVAR", "hmtx",
                "cmap",
            ] {
                report += &format!(
                    "  table {t:?} present={}\n",
                    raw.table(rustybuzz::ttf_parser::Tag::from_bytes(&{
                        let mut b = [b' '; 4];
                        b[..t.len()].copy_from_slice(t.as_bytes());
                        b
                    }))
                    .is_some()
                );
            }
            if let Some(g) = face.glyph_index('W') {
                struct B(usize);
                impl rustybuzz::ttf_parser::OutlineBuilder for B {
                    fn move_to(&mut self, _: f32, _: f32) {
                        self.0 += 1;
                    }
                    fn line_to(&mut self, _: f32, _: f32) {
                        self.0 += 1;
                    }
                    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
                        self.0 += 1;
                    }
                    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
                        self.0 += 1;
                    }
                    fn close(&mut self) {}
                }
                let mut b = B(0);
                let bb = face.outline_glyph(g, &mut b);
                report += &format!("  outline W: bbox={bb:?} segments={}\n", b.0);
            }
        }
        match fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default()) {
            Ok(fd) => {
                let (m, bm) = fd.rasterize('W', 13.0);
                report += &format!(
                    "  fontdue: glyph_count={} W metrics w={} h={} lit={}\n",
                    fd.glyph_count(),
                    m.width,
                    m.height,
                    bm.iter().filter(|&&b| b != 0).count()
                );
            }
            Err(e) => report += &format!("  fontdue failed: {e}\n"),
        }
    }

    // Real Qt on this machine.
    let script = r#"from PySide6.QtGui import QFont, QFontMetrics, QFontInfo, QGuiApplication
a = QGuiApplication([])
print('platform', a.platformName(), 'default font', repr(QGuiApplication.font().family()))
for fam in ['NoSuchFontXYZ', 'Segoe UI', 'Helvetica', 'Arial']:
    for name in ['AnyStyle', 'System', 'SansSerif', 'Serif', 'Monospace', 'TypeWriter', 'Cursive', 'Fantasy']:
        for px in (12, 16, 20):
            f = QFont(fam)
            f.setStyleHint(getattr(QFont.StyleHint, name))
            f.setPixelSize(px)
            m = QFontMetrics(f)
            print(fam, name, px, repr(QFontInfo(f).family()), 'asc', m.ascent(), 'desc', m.descent(), 'h', m.height(), 'avg', m.averageCharWidth())
"#;
    std::fs::write("/tmp/q.py", script).unwrap();
    report += &run(
        "python3 -m venv /tmp/qtv && /tmp/qtv/bin/python -m pip install -q PySide6 2>&1 | tail -2; \
         QT_QPA_PLATFORM=offscreen /tmp/qtv/bin/python /tmp/q.py 2>&1 | head -120",
    );
    panic!("{report}");
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
#[repr(C)]
struct CGPathElement {
    kind: i32,
    points: *mut CGPoint,
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontGetGlyphsForCharacters(
        font: *const c_void,
        chars: *const u16,
        glyphs: *mut u16,
        count: isize,
    ) -> u8;
    fn CTFontCreatePathForGlyph(font: *const c_void, glyph: u16, t: *const c_void)
    -> *const c_void;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPathApply(
        path: *const c_void,
        info: *mut c_void,
        f: extern "C" fn(*mut c_void, *const CGPathElement),
    );
    fn CGPathGetBoundingBox(path: *const c_void) -> CGRect;
}

extern "C" fn count_elem(info: *mut c_void, _e: *const CGPathElement) {
    // SAFETY: `info` is the `usize` counter passed below.
    unsafe { *(info as *mut usize) += 1 };
}

fn coretext_outline_probe() -> String {
    let mut out = String::new();
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNS.ttf") {
        let n = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        let tags: Vec<String> = (0..n)
            .map(|i| String::from_utf8_lossy(&bytes[12 + i * 16..16 + i * 16]).into_owned())
            .collect();
        out += &format!("SFNS tables ({n}): {tags:?}\n");
    }
    // SAFETY: plain CoreText/CoreGraphics calls on objects we own and release.
    unsafe {
        let font = CTFontCreateUIFontForLanguage(2, 2048.0, std::ptr::null());
        for ch in ['W', '5', '%'] {
            let u = ch as u16;
            let mut g = 0u16;
            let ok = CTFontGetGlyphsForCharacters(font, &u, &mut g, 1);
            let path = CTFontCreatePathForGlyph(font, g, std::ptr::null());
            if path.is_null() {
                out += &format!("CoreText {ch:?}: ok={ok} glyph={g} path=null\n");
                continue;
            }
            let mut n = 0usize;
            CGPathApply(path, &mut n as *mut usize as *mut c_void, count_elem);
            let bb = CGPathGetBoundingBox(path);
            out += &format!("CoreText {ch:?}: ok={ok} glyph={g} elements={n} bbox={bb:?}\n");
            CFRelease(path);
        }
        CFRelease(font);
    }
    out
}
