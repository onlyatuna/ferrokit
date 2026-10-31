// SPDX-License-Identifier: LGPL-3.0-only
//! Lists the fonts the text engine and an application ask for and what this machine has, for CI logs.
//!
//! A family that is not installed is not an error in the font database: `load_font_styled` quietly
//! substitutes Segoe UI, then Arial (font_database.rs:376-384), so a runner without the expected fonts
//! renders and measures with other glyphs and the layout tests see other numbers. This prints, per
//! requested family, whether it is INSTALLED, SUBSTITUTED or MISSING, then the numbers the Windows
//! layout tests depend on, then every installed family.
//!
//! On GitHub Actions each non-installed family is also emitted as a `::notice` annotation, so the
//! missing dependencies show on the run page without counting as warnings: the Windows and macOS
//! system families cannot be installed on the other runners. Output only: this never fails the build.
//!
//! Run it by hand or from a CI job on every OS.
use ferrokit_gui::text::font::{Font, FontWeight};
use ferrokit_gui::text::font_database::{
    primary_face_metrics, set_application_device_pixel_ratio, with_global_font_database,
};
use ferrokit_gui::text::font_metrics::FontMetrics;

/// `(family, OS it is meant for or "all", who asks for it)`; a family meant for another OS is listed
/// but not reported as missing. An application's own requests come from its `Font::new` calls and the
/// style sheets' `font-family`; the fallbacks are the per-OS slots of `append_fallback_engines`.
const REQUESTED: &[(&str, &str, &str)] = &[
    (
        "Microsoft JhengHei UI",
        "all",
        "ferrokit-widgets APP_DEFAULT_FAMILY (label default; Qt's default on a zh-TW Windows)",
    ),
    (
        "Segoe UI",
        "all",
        "application Font::new calls, style sheets, the database's own fallback",
    ),
    (
        "Consolas",
        "all",
        "application Font::new calls (monospace text)",
    ),
    (
        "Courier New",
        "all",
        "style sheet font-family list after Consolas",
    ),
    (
        "SF Pro Display",
        "macos",
        "style sheet font-family list (macOS first choice)",
    ),
    (
        "Microsoft JhengHei",
        "windows",
        "style sheet list; Windows CJK fallback slot",
    ),
    (
        "Segoe UI Emoji",
        "windows",
        "Windows emoji fallback slot; style sheet font lists",
    ),
    ("Segoe UI Symbol", "windows", "Windows symbol fallback slot"),
    ("Segoe Fluent Icons", "windows", "icon font (Windows 11)"),
    ("Segoe MDL2 Assets", "windows", "icon font (Windows 10)"),
    ("Microsoft YaHei", "windows", "Windows CJK fallback slot"),
    ("PingFang TC", "macos", "macOS CJK fallback slot"),
    ("PingFang SC", "macos", "macOS CJK fallback slot"),
    ("Heiti TC", "macos", "macOS CJK fallback slot"),
    ("Apple Color Emoji", "macos", "macOS emoji fallback slot"),
    ("Apple Symbols", "macos", "macOS symbol fallback slot"),
    ("Noto Sans CJK TC", "linux", "Linux CJK fallback slot"),
    ("Noto Sans CJK SC", "linux", "Linux CJK fallback slot"),
    ("WenQuanYi Micro Hei", "linux", "Linux CJK fallback slot"),
    ("Noto Color Emoji", "linux", "Linux emoji fallback slot"),
    ("DejaVu Sans", "linux", "Linux symbol fallback slot"),
    ("Arial", "all", "the database's last-resort fallback"),
];

#[derive(PartialEq)]
enum Status {
    Installed,
    /// Not installed, but the database still hands back a face (Segoe UI, else Arial).
    Substituted,
    Missing,
}

fn status(family: &str) -> Status {
    with_global_font_database(|db| {
        if db.has_family(family) {
            Status::Installed
        } else if db.load_font(family).is_some() {
            Status::Substituted
        } else {
            Status::Missing
        }
    })
}

/// Font files (`.ttf`/`.otf`/`.ttc`/`.otc`) under `dir`, recursively; `None` if `dir` is absent.
fn font_files(dir: &std::path::Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut n = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            n += font_files(&path).unwrap_or(0);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "ttf" | "otf" | "ttc" | "otc"
            )
        }) {
            n += 1;
        }
    }
    Some(n)
}

fn main() {
    let ci = std::env::var_os("GITHUB_ACTIONS").is_some();
    println!(
        "os={} arch={}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    set_application_device_pixel_ratio(1.0);

    // The directories `FontDatabase::new` scans are fixed (font_database.rs:200-212): `%WINDIR%\Fonts`
    // on Windows, `/usr/share/fonts` and `/System/Library/Fonts` elsewhere. Fonts of the other usual
    // locations are invisible to it; their file counts show what that hides.
    println!("--- font directories (searched by the database: *) ---");
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs: Vec<(String, bool)> = vec![
        (
            format!("{}\\Fonts", std::env::var("WINDIR").unwrap_or_default()),
            cfg!(windows),
        ),
        ("/usr/share/fonts".into(), !cfg!(windows)),
        ("/System/Library/Fonts".into(), !cfg!(windows)),
        ("/System/Library/Fonts/Supplemental".into(), false),
        ("/Library/Fonts".into(), false),
        (format!("{home}/Library/Fonts"), false),
        ("/usr/local/share/fonts".into(), false),
        (format!("{home}/.fonts"), false),
        (format!("{home}/.local/share/fonts"), false),
    ];
    for (dir, searched) in dirs {
        if dir.starts_with('\\') {
            continue; // `%WINDIR%` is unset off Windows
        }
        let count = font_files(std::path::Path::new(&dir));
        let mark = if searched { '*' } else { ' ' };
        match count {
            Some(n) => println!("{mark} {dir}: {n} font files"),
            None => println!("{mark} {dir}: absent"),
        }
    }

    println!("--- requested families ---");
    let mut absent = Vec::new();
    for (family, only, who) in REQUESTED {
        let s = status(family);
        if *only != "all" && *only != std::env::consts::OS && s != Status::Installed {
            println!("n/a (other OS)  {family:<24} {who}");
            continue;
        }
        let word = match s {
            Status::Installed => "INSTALLED  ",
            Status::Substituted => "SUBSTITUTED",
            Status::Missing => "MISSING    ",
        };
        println!("{word} {family:<24} {who}");
        if s != Status::Installed {
            absent.push((*family, *who, s));
        }
    }

    // What the Windows layout tests measure: 14px label height (18 under JhengHei UI, 19 under Segoe
    // UI) and an advance, at device pixel ratio 1. A substituted family gives the substitute's numbers.
    println!("--- numbers behind the Windows-only layout tests ---");
    for family in ["Microsoft JhengHei UI", "Segoe UI", "Consolas"] {
        let font = Font::new(family, 14.0);
        let m = FontMetrics::from_font(&font);
        let bold = font.clone().with_weight(FontWeight::Bold);
        println!(
            "{family:?} 14px: layout_height={} (line metrics, average char width)={:?} advance(\"SESSION 5H\")={:.3} bold={:.3}",
            FontMetrics::layout_height(&font),
            primary_face_metrics(&font),
            m.horizontal_advance_exact("SESSION 5H", &font),
            m.horizontal_advance_exact("SESSION 5H", &bold),
        );
    }

    let families = with_global_font_database(|db| db.families());
    println!("--- installed families ({}) ---", families.len());
    for f in &families {
        println!("{f}");
    }

    println!("--- summary ---");
    if absent.is_empty() {
        println!("every requested family is installed");
    }
    for (family, who, s) in &absent {
        let kind = if *s == Status::Substituted {
            "substituted by Segoe UI/Arial"
        } else {
            "missing, no substitute"
        };
        println!("NOT INSTALLED: {family} ({kind}) <- {who}");
        if ci {
            println!(
                "::notice title=font not installed ({})::{family}: {kind}. Needed by: {who}",
                std::env::consts::OS
            );
        }
    }
}
