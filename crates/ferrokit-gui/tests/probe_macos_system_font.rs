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
    fn CTFontCreateUIFontForLanguage(ui_type: i32, size: f64, language: *const c_void)
    -> *const c_void;
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
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().unwrap();
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
            report += &format!("{name}: family={:?} postscript={:?} path={p}\n", cfstr(fam), cfstr(ps));
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
                            m.ascent, m.descent, m.height, m.rounded_average_char_width()
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

    // Real Qt on this machine.
    report += &run(
        "python3 -m venv /tmp/qtv && /tmp/qtv/bin/python -m pip install -q PySide6 2>&1 | tail -2; \
         cat > /tmp/q.py <<'E'\n\
from PySide6.QtGui import QFont, QFontMetrics, QFontInfo, QGuiApplication\n\
a=QGuiApplication([])\n\
print('platform',a.platformName(),'default font',QGuiApplication.font().family())\n\
for fam in ['NoSuchFontXYZ','Segoe UI','Helvetica','Arial']:\n\
  for name in ['AnyStyle','System','SansSerif','Serif','Monospace','TypeWriter','Cursive','Fantasy']:\n\
    for px in (12,16,20):\n\
        f=QFont(fam); f.setStyleHint(getattr(QFont,name)); f.setPixelSize(px)\n\
        m=QFontMetrics(f)\n\
        print(fam,name,px,repr(QFontInfo(f).family()),'asc',m.ascent(),'desc',m.descent(),'h',m.height(),'avg',m.averageCharWidth())\n\
E\n\
QT_QPA_PLATFORM=offscreen /tmp/qtv/bin/python /tmp/q.py 2>&1 | head -120",
    );
    panic!("{report}");
}
