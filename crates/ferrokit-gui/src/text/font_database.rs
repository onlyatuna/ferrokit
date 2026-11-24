// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::HashMap;
#[cfg(windows)]
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::text::font::{Font, FontStyle, HintingPreference, SharedFontData, StyleHint};
use crate::text::glyph_face::{
    LineMetrics, SharedGlyphFace, parse_face_for_request, parse_face_with_hint_style,
};
use crate::text::glyph_layout::FontEngine;

use rustybuzz::ttf_parser::name::Table as NameTable;
use rustybuzz::ttf_parser::{PlatformId, name_id};

/// Weight and slant requested from the font database (`QFontDatabase::bestStyle` key), and the
/// hint style the face is built for (Qt's FreeType engine fixes it when it creates the engine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaceStyle {
    /// CSS/Qt numeric weight, 1..=1000.
    pub weight: u16,
    pub italic: bool,
    /// The slant asked for is oblique rather than italic (`italic` is set too); only a family with
    /// both an italic and an oblique face tells them apart.
    pub oblique: bool,
    /// See [`face_hint_style`].
    pub hint_style: HintStyle,
}

impl FaceStyle {
    /// The regular face (weight 400, upright) for the process-wide default hint style.
    pub fn regular() -> Self {
        Self {
            weight: 400,
            italic: false,
            oblique: false,
            hint_style: face_hint_style(HintingPreference::PreferDefaultHinting),
        }
    }

    /// The style `font` asks for.
    pub fn of(font: &Font) -> Self {
        Self {
            weight: font.weight as u16,
            italic: font.style != FontStyle::Normal,
            oblique: font.style == FontStyle::Oblique,
            hint_style: face_hint_style(font.hinting_preference),
        }
    }

    /// `bestStyle` distance to a face of the unstretched request: weight difference in steps of
    /// 10, plus the difference of the stretch (a request has the stretch 100; a face without one
    /// counts as not differing), plus 0x1000 for the wrong slant (1 for italic against oblique).
    /// Zero is an exact match.
    #[cfg_attr(all(unix, not(target_os = "macos")), allow(dead_code))]
    fn distance(self, weight: u16, italic: bool, stretch: u16) -> u32 {
        let w = (i32::from(self.weight) - i32::from(weight)).unsigned_abs() / 10;
        let s = if stretch == 0 {
            0
        } else {
            (100 - i32::from(stretch)).unsigned_abs()
        };
        w + s + if self.italic != italic { 0x1000 } else { 0 }
    }

    /// The slant as a number for [`cache_key`](Self::cache_key): upright, italic, oblique.
    fn slant_code(self) -> u8 {
        u8::from(self.italic) + u8::from(self.oblique)
    }

    /// Cache key of `family` at this style and style hint; fonts that differ only in style hint
    /// do not share a cache entry so substitute resolution is cached independently, nor do faces
    /// built for different hint styles.
    fn cache_key(self, family: &str, hint: StyleHint) -> String {
        format!(
            "{family}\u{1}{}\u{1}{}\u{1}{:?}\u{1}{:?}",
            self.weight,
            self.slant_code(),
            hint,
            self.hint_style
        )
    }
}

/// Windows file names of the bold/italic face of the families loaded by name without a directory
/// scan. `Some(&[])` means the family is known but has no such face (the caller falls back to
/// its regular file); `None` means the family is not known by name.
#[cfg(not(all(unix, not(target_os = "macos"))))]
pub(super) fn known_styled_filenames(
    family_key: &str,
    style: FaceStyle,
) -> Option<&'static [&'static str]> {
    // Of the faces these families ship, regular (400) and bold (700) are the nearest to any weight.
    let bold = style.weight >= 550;
    Some(match (family_key, bold, style.italic) {
        ("segoe ui", true, false) => &["segoeuib.ttf"],
        ("segoe ui", false, true) => &["segoeuii.ttf"],
        ("segoe ui", true, true) => &["segoeuiz.ttf"],
        ("arial", true, false) => &["arialbd.ttf"],
        ("arial", false, true) => &["ariali.ttf"],
        ("arial", true, true) => &["arialbi.ttf"],
        ("consolas", true, false) => &["consolab.ttf"],
        ("consolas", false, true) => &["consolai.ttf"],
        ("consolas", true, true) => &["consolaz.ttf"],
        ("tahoma", true, _) => &["tahomabd.ttf"],
        ("courier new", true, false) => &["courbd.ttf"],
        ("courier new", false, true) => &["couri.ttf"],
        ("courier new", true, true) => &["courbi.ttf"],
        ("times new roman", true, false) => &["timesbd.ttf"],
        ("times new roman", false, true) => &["timesi.ttf"],
        ("times new roman", true, true) => &["timesbi.ttf"],
        ("microsoft jhenghei" | "msjh", true, _) => &["msjhbd.ttc"],
        ("microsoft yahei" | "msyh", true, _) => &["msyhbd.ttc"],
        (
            "segoe ui" | "arial" | "consolas" | "tahoma" | "courier new" | "times new roman"
            | "microsoft jhenghei" | "msjh" | "microsoft yahei" | "msyh" | "segoe ui symbol"
            | "segoe ui emoji" | "ms gothic",
            _,
            _,
        ) => &[],
        _ => return None,
    })
}

/// Family metadata discovered from a font face's `name` and `post` tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontFamilyInfo {
    /// Family name (typographic family when present, legacy family otherwise).
    pub family: String,
    /// File the face was found in; `None` for application fonts registered from memory.
    pub path: Option<PathBuf>,
    /// Face index inside a font collection (`.ttc`/`.otc`); 0 for single-face files.
    pub face_index: u32,
    /// `post.isFixedPitch`: every glyph has the same advance (monospaced).
    pub fixed_pitch: bool,
    /// `OS/2.usWeightClass` (400 when the face has no OS/2 table).
    pub weight: u16,
    /// `OS/2.fsSelection` italic or oblique bit.
    pub italic: bool,
    /// `QFont::Stretch` of `OS/2.usWidthClass` (100 normal, 75 condensed, ...); 100 when the face
    /// has no OS/2 table.
    pub stretch: u16,
}

/// In-memory faces [`FontDatabase`] keeps alive strongly after their last use.
pub(super) const RECENT_MEMORY_FACES: usize = 4;

/// Identity of a face built from in-memory font data: the data address and length, the hint style,
/// and what the request asks for that DirectWrite would synthesize (see [`memory_face_key`]).
pub(super) type MemoryFaceKey = (usize, usize, HintStyle, bool, bool);

/// `memory_faces` entry: the key of a face and the face.
pub(super) type MemoryFaceEntry = (
    MemoryFaceKey,
    std::sync::Weak<dyn crate::text::glyph_face::GlyphFace>,
);

/// The `memory_faces` key of the face of `shared` for `hint_style` and a request of `weight` that
/// is `italic` or not. On Windows the face differs by whether the request is at least `DemiBold`
/// and whether it is italic, the two things Qt's DirectWrite font database synthesizes (see
/// [`parse_face_for_request`]); the other backends build one face whatever the request, so they
/// share it.
pub(super) fn memory_face_key(
    shared: &SharedFontData,
    hint_style: HintStyle,
    weight: u16,
    italic: bool,
) -> MemoryFaceKey {
    let (bold, italic) = if cfg!(windows) {
        (weight >= crate::text::glyph_face::DEMI_BOLD, italic)
    } else {
        (false, false)
    };
    (
        shared.as_slice().as_ptr() as usize,
        shared.len(),
        hint_style,
        bold,
        italic,
    )
}

/// System font cache database (`QFontDatabase` equivalent).
///
/// Searches, loads, and caches system fonts to avoid redundant disk I/O and parsing.
pub struct FontDatabase {
    pub(super) cache: HashMap<String, SharedGlyphFace>,
    pub(super) raw_cache: HashMap<String, SharedFontData>,
    pub(super) file_cache: HashMap<PathBuf, SharedFontData>,
    pub(super) search_paths: Vec<PathBuf>,
    /// Faces found by scanning `search_paths`; built lazily by [`FontDatabase::families`].
    pub(super) system_faces: Option<Vec<FontFamilyInfo>>,
    /// Faces registered through [`FontDatabase::add_font_from_memory`].
    pub(super) application_faces: Vec<FontFamilyInfo>,
    /// One engine per requested family string, shared so glyph caches persist across draw calls.
    pub(super) engines: HashMap<String, FontEngine>,
    /// Faces built from `Font::font_data`, so a draw or metrics call does not re-parse the font.
    ///
    /// Weak, so the cache never pins a (possibly huge) font blob: once every engine using a face is
    /// dropped the face and its data are released. Keyed by the data buffer's address and length,
    /// which stay unique for as long as the face (which owns the data) is alive.
    ///
    /// The draw and metrics paths drop their engines after every call, so the last few faces are also
    /// held strongly ([`RECENT_MEMORY_FACES`], most recent last) to avoid re-parsing (and, on Windows,
    /// re-registering with DirectWrite) on each draw.
    pub(super) recent_memory_faces: Vec<SharedGlyphFace>,
    pub(super) memory_faces: Vec<MemoryFaceEntry>,
    /// What the Windows fallback lookup resolved, see [`FallbackMemo`].
    #[cfg(windows)]
    fallback_memo: FallbackMemo,
    /// Qt's registry of families and the families the primary requests resolved to (Linux).
    #[cfg(all(unix, not(target_os = "macos")))]
    linux: linux::LinuxFonts,
}

/// Bumped whenever the set of available fonts changes, so caches derived from font data
/// (e.g. text advance widths) know to drop their entries.
pub(super) static FONT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Current font-set generation; changes whenever a search path or an in-memory font is added.
pub fn font_generation() -> u64 {
    FONT_GENERATION.load(Ordering::Acquire)
}
/// `QGuiApplication::devicePixelRatio()` as bits of an `f32`; the highest ratio of any screen.
static APPLICATION_DPR_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// The ratio Qt's Windows font database consults (`qApp->devicePixelRatio()`).
pub fn application_device_pixel_ratio() -> f32 {
    f32::from_bits(APPLICATION_DPR_BITS.load(Ordering::Relaxed))
}

/// Records the application's device pixel ratio (the highest of all screens, as
/// `QGuiApplication::devicePixelRatio()` reports it). Widths measured under the previous value are
/// dropped.
pub fn set_application_device_pixel_ratio(dpr: f32) {
    if APPLICATION_DPR_BITS.swap(dpr.to_bits(), Ordering::Relaxed) != dpr.to_bits() {
        FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

/// Whether text is laid out as Qt's DirectWrite font engine lays it out for a device pixel ratio
/// other than 1 (`qFuzzyCompare`): vertical or no hinting, fractional advances and sub-pixel glyph
/// positions. Qt's Windows platform plugin creates its DirectWrite engine at every ratio
/// (`qwindowsintegration.cpp`, `qwindowsdirectwritefontdatabase.cpp`; measured against PySide6
/// 6.11.2 and 6.12.0, including a ratio of exactly 1); at a ratio of 1 the default hinting
/// preference resolves to full hinting (`determineHinting`), which rounds advances and has no
/// sub-pixel positions. The GDI font database, whose `useDirectWrite` (qwindowsfontdatabase.cpp)
/// picks DirectWrite by ratio, is only used with `windows:fontengine=gdi`; a real GDI engine
/// differs again, e.g. in `averageCharWidth` (`tmAveCharWidth`), and is not modelled.
pub fn uses_directwrite_engine() -> bool {
    let dpr = f64::from(application_device_pixel_ratio());
    (dpr - 1.0).abs() * 1_000_000_000_000.0 > dpr.min(1.0).abs()
}

/// Whether text layout keeps the fractional advances and offsets the font engine reports instead of
/// rounding them to whole pixels (`QTextEngine::shapeTextWithHarfbuzzNG`:
/// `!supportsHorizontalSubPixelPositions()`): Qt's DirectWrite engine (see
/// [`uses_directwrite_engine`]) and its CoreText engine, which always positions at sub-pixels.
///
/// Off macOS this is exactly [`uses_directwrite_engine`], independent of the hinting preference:
/// [`font_has_subpixel_positions`], which the line metrics follow, is `true` for FreeType with light
/// hinting and for any font preferring no hinting, but layout advances never followed it.
pub(crate) fn layout_has_subpixel_advances() -> bool {
    cfg!(target_os = "macos") || uses_directwrite_engine()
}

/// Font hinting style matching Qt's `QFontEngine::HintStyle` (qfontengine_p.h:60).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HintStyle {
    /// No hinting (`FT_LOAD_NO_HINTING`).
    HintNone = 0,
    /// Light/vertical hinting (`FT_LOAD_TARGET_LIGHT`, fontconfig `FC_HINT_SLIGHT` / `hintslight`).
    #[default]
    HintLight = 1,
    /// Medium hinting (`FT_LOAD_TARGET_NORMAL`, fontconfig `FC_HINT_MEDIUM`).
    HintMedium = 2,
    /// Full hinting (`FT_LOAD_TARGET_NORMAL`, fontconfig `FC_HINT_FULL` / `hintfull`).
    HintFull = 3,
}

static DEFAULT_HINT_STYLE: AtomicU8 = AtomicU8::new(initial_default_hint_style_u8());

const fn initial_default_hint_style_u8() -> u8 {
    if cfg!(target_os = "macos") {
        0 // HintNone (CoreText always uses sub-pixel positioning)
    } else if cfg!(windows) {
        3 // HintFull (DirectWrite full hinting at ratio 1 / ftInitialDefaultHintStyle on Windows)
    } else {
        1 // HintLight (FreeType default on Linux: fontconfig hintslight desktop standard)
    }
}

/// Process-wide default hinting style (Qt `QFontEngine::default_hint_style`, `qfontengine_p.h:60`).
///
/// On Linux/FreeType this defaults to [`HintStyle::HintLight`], matching fontconfig's standard
/// desktop default (`hintslight`, `qfontconfigdatabase.cpp:730`). On Windows it defaults to
/// [`HintStyle::HintFull`] (DirectWrite full hinting at ratio 1). On macOS it
/// is [`HintStyle::HintNone`].
pub fn default_hint_style() -> HintStyle {
    match DEFAULT_HINT_STYLE.load(Ordering::Acquire) {
        0 => HintStyle::HintNone,
        1 => HintStyle::HintLight,
        2 => HintStyle::HintMedium,
        3 => HintStyle::HintFull,
        _ => HintStyle::HintLight,
    }
}

/// Sets the process-wide default hinting style.
pub fn set_default_hint_style(style: HintStyle) {
    if DEFAULT_HINT_STYLE.swap(style as u8, Ordering::AcqRel) != style as u8 {
        FONT_GENERATION.fetch_add(1, Ordering::Release);
    }
}

/// The hint style a face for a font with `preference` is built for: `defaultHintStyleFromMatch`
/// (qfontconfigdatabase.cpp) with the process-wide default in place of fontconfig's
/// `hintstyle`. Only the FreeType-role backend of Linux and the other Unix systems but macOS
/// hints; elsewhere every preference shares one face ([`HintStyle::default`]).
pub(crate) fn face_hint_style(preference: HintingPreference) -> HintStyle {
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return HintStyle::default();
    }
    match preference {
        HintingPreference::PreferNoHinting => HintStyle::HintNone,
        HintingPreference::PreferVerticalHinting => HintStyle::HintLight,
        HintingPreference::PreferFullHinting => HintStyle::HintFull,
        HintingPreference::PreferDefaultHinting => default_hint_style(),
    }
}

/// Whether `font`'s engine supports sub-pixel glyph positions under its hinting preference and
/// platform rules (`QFontEngine::supportsSubPixelPositions`).
///
/// When `true`, line metrics (ascent, descent, leading) remain fractional 26.6 values.
/// When `false`, line metrics are rounded to whole pixels ([`LineMetrics::rounded`]), with
/// leading taking up the difference.
///
/// - **macOS (CoreText):** always `true` (`QFontEngineCoreText::supportsSubPixelPositions`,
///   CoreText does not round line metrics regardless of hinting preference).
/// - **Windows (DirectWrite):** `true` at ratio != 1.0 (sub-pixel / asymmetric), `false` at ratio 1.0
///   (full hinting) under default hinting.
/// - **Linux / FreeType:** `true` when the effective hint style is [`HintStyle::HintLight`]
///   (`hintslight`, the Linux desktop default) or [`HintStyle::HintNone`], or when ratio != 1.0;
///   `false` only for [`HintStyle::HintFull`] or [`HintStyle::HintMedium`] at ratio 1.0.
pub fn font_has_subpixel_positions(font: &Font) -> bool {
    if cfg!(target_os = "macos") {
        return true;
    }
    match font.hinting_preference() {
        HintingPreference::PreferNoHinting | HintingPreference::PreferVerticalHinting => true,
        HintingPreference::PreferFullHinting => false,
        HintingPreference::PreferDefaultHinting => engine_has_subpixel_positions(),
    }
}

/// Whether the default font engine at the current device pixel ratio has sub-pixel positions
/// under default hinting preference ([`font_has_subpixel_positions`]).
///
/// Explicitly keeps `cfg!(target_os = "macos")` so standalone callers without a [`Font`] reference
/// consistently observe CoreText's platform behavior.
pub fn engine_has_subpixel_positions() -> bool {
    cfg!(target_os = "macos")
        || uses_directwrite_engine()
        || matches!(
            default_hint_style(),
            HintStyle::HintNone | HintStyle::HintLight
        )
}

/// Process-wide font database shared by the painter and font-selection widgets.
static GLOBAL_FONT_DATABASE: Mutex<Option<FontDatabase>> = Mutex::new(None);

/// Runs `f` with the process-wide font database, creating it on first use.
///
/// Mirrors Qt's static `QFontDatabase` API: fonts registered here are visible to
/// text rendering and to widgets such as `FontComboBox`.
pub fn with_global_font_database<R>(f: impl FnOnce(&mut FontDatabase) -> R) -> R {
    let mut guard = GLOBAL_FONT_DATABASE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(FontDatabase::new))
}
/// Resolves the primary font engine and its platform-aware fallback chain (`QFontEngineMulti` equivalent)
/// using the process-wide global font database.
pub fn resolve_font_engines_global(font: &Font) -> Vec<FontEngine> {
    with_global_font_database(|db| db.resolve_font_engines(font))
}

/// What the primary face of `font` reports at its size: its [`LineMetrics`] and
/// `QFontEngine::averageCharWidth` (see [`GlyphFace::line_metrics`] and
/// [`GlyphFace::average_char_width`]). A font carrying in-memory data is measured from that data,
/// whichever platform backend parsed it. Both are `None` when no face resolves.
///
/// [`GlyphFace::line_metrics`]: crate::text::glyph_face::GlyphFace::line_metrics
/// [`GlyphFace::average_char_width`]: crate::text::glyph_face::GlyphFace::average_char_width
pub fn primary_face_metrics(font: &Font) -> (Option<LineMetrics>, Option<f32>) {
    with_global_font_database(|db| match db.primary_face(font) {
        Some(face) => (
            face.line_metrics(font.size),
            face.average_char_width(font.size),
        ),
        None => (None, None),
    })
}

/// Cap height in pixels of the face `font` resolves to (see [`GlyphFace::cap_height`]).
///
/// [`GlyphFace::cap_height`]: crate::text::glyph_face::GlyphFace::cap_height
pub fn primary_face_cap_height(font: &Font) -> Option<f32> {
    with_global_font_database(|db| {
        db.load_font_styled(&font.family, FaceStyle::of(font), font.style_hint)
            .and_then(|face| {
                face.cap_height(font.size)
                    .or_else(|| Some(face.metrics('H', font.size).height as f32))
            })
    })
}

/// Like [`resolve_font_engines_global`], but loads the fallback fonts only when `text` needs them.
///
/// Parsing a CJK/emoji fallback can cost hundreds of ms (and hundreds of MB) with an eager backend,
/// and the fallbacks are only ever consulted for characters the primary font has no glyph for
/// (`GlyphLayout::partition_into_runs`). Text the primary font fully covers is therefore shaped
/// with the primary engine alone; anything else gets the same full chain as before.
pub fn resolve_font_engines_for_text_global(font: &Font, text: &str) -> Vec<FontEngine> {
    with_global_font_database(|db| db.resolve_font_engines_for_text(font, text))
}

/// Substitute font family for a style hint on Windows, matching Qt 6 / qtbase dev
/// (`src/gui/text/windows/qwindowsfontdatabasebase.cpp:951-973`).
///
/// On Windows, when a requested font family is not installed, Qt resolves the
/// style hint to a platform substitute family:
/// - `Times` / `Serif` -> "Times New Roman"
/// - `Courier` / `TypeWriter` / `Monospace` -> "Courier New"
/// - `Cursive` -> "Comic Sans MS"
/// - `Fantasy` -> "Impact"
/// - `Decorative` / `OldEnglish` -> "Old English"
/// - `Helvetica` / `SansSerif` -> "Arial"
/// - `System` / `AnyStyle` (and every other value) -> "Tahoma"
///
/// Non-Windows platforms (macOS/Linux) retain their previous fallback behavior.
#[cfg(windows)]
pub fn family_for_style_hint(hint: StyleHint) -> &'static str {
    match hint {
        StyleHint::Times => "Times New Roman",
        StyleHint::Courier | StyleHint::Monospace => "Courier New",
        StyleHint::Cursive => "Comic Sans MS",
        StyleHint::Fantasy => "Impact",
        StyleHint::OldEnglish => "Old English",
        StyleHint::Helvetica => "Arial",
        StyleHint::System | StyleHint::AnyStyle => "Tahoma",
    }
}

/// Substitute font family for a style hint on macOS matching Qt 6 / qtbase dev
/// (`src/gui/text/coretext/qcoretextfontdatabase.mm:661-770`, specifically `descriptorForStyle` L691-706).
///
/// On macOS, when a requested font family is not installed, Qt resolves the style hint:
/// - `SansSerif` (`Helvetica`) -> "Helvetica"
/// - `Serif` (`Times`) -> "Times New Roman"
/// - `Monospace` -> "Menlo"
/// - `Cursive` -> "Apple Chancery"
/// - `Fantasy` -> "Zapfino"
/// - `TypeWriter` (`Courier`) -> "American Typewriter"
/// - `AnyStyle` / `System` -> `None` from this pure table (Qt resolves them to the system UI font
///   via `CTFontCreateUIFontForLanguage(kCTFontUIFontSystem)`; ferrokit resolves them to
///   the system UI font through CoreText in `load_font_styled`)
/// - others -> `None` [inferred]
#[cfg(target_os = "macos")]
pub fn family_for_style_hint(hint: StyleHint) -> Option<&'static str> {
    match hint {
        StyleHint::Helvetica => Some("Helvetica"),
        StyleHint::Times => Some("Times New Roman"),
        StyleHint::Monospace => Some("Menlo"),
        StyleHint::Cursive => Some("Apple Chancery"),
        StyleHint::Fantasy => Some("Zapfino"),
        StyleHint::Courier => Some("American Typewriter"),
        StyleHint::AnyStyle | StyleHint::System => Some(".AppleSystemUIFont"),
        StyleHint::OldEnglish => None,
    }
}
#[cfg(target_os = "macos")]
fn query_system_ui_font_path() -> Option<std::path::PathBuf> {
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontCreateUIFontForLanguage(
            ui_type: i32,
            size: f64,
            language: *const std::ffi::c_void,
        ) -> *const std::ffi::c_void;
        fn CTFontCopyFontDescriptor(font: *const std::ffi::c_void) -> *const std::ffi::c_void;
        fn CTFontDescriptorCopyAttribute(
            descriptor: *const std::ffi::c_void,
            attribute: *const std::ffi::c_void,
        ) -> *const std::ffi::c_void;
        static kCTFontURLAttribute: *const std::ffi::c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLGetFileSystemRepresentation(
            url: *const std::ffi::c_void,
            resolve: u8,
            buffer: *mut u8,
            max_buf_len: isize,
        ) -> u8;
        fn CFRelease(cf: *const std::ffi::c_void);
    }

    // SAFETY: standard CoreText/CoreFoundation calls to get the file path of kCTFontUIFontSystem (2).
    unsafe {
        let font = CTFontCreateUIFontForLanguage(2, 0.0, std::ptr::null());
        if font.is_null() {
            return None;
        }
        let desc = CTFontCopyFontDescriptor(font);
        CFRelease(font);
        if desc.is_null() {
            return None;
        }
        let url = CTFontDescriptorCopyAttribute(desc, kCTFontURLAttribute);
        CFRelease(desc);
        if url.is_null() {
            return None;
        }
        let mut buf = vec![0u8; 1024];
        let ok = CFURLGetFileSystemRepresentation(url, 1, buf.as_mut_ptr(), 1024);
        CFRelease(url);
        if ok == 0 {
            return None;
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
        let path_str = std::str::from_utf8(&buf[..len]).ok()?;
        Some(std::path::PathBuf::from(path_str))
    }
}

/// Fontconfig generic family for a style hint matching Qt 6 / qtbase dev
/// (`src/gui/text/unix/qfontconfigdatabase.cpp:349-370`, `getFcFamilyForStyleHint`).
///
/// In Qt, when `styleHint == QFont::AnyStyle && fixedPitch`, Qt substitutes `QFont::TypeWriter`
/// before calling `fallbacksForFamily` (`src/gui/text/qfontdatabase.cpp:846-848, 2776-2778`).
///
/// Mappings:
/// - `SansSerif` (`Helvetica`) -> "sans-serif"
/// - `Serif` (`Times`) -> "serif"
/// - `TypeWriter` (`Courier`) / `Monospace` -> "monospace"
/// - `Cursive` -> "cursive"
/// - `Fantasy` -> "fantasy"
/// - others (`AnyStyle`, `System`, `OldEnglish`) -> `None`
#[cfg(all(unix, not(target_os = "macos")))]
pub fn fc_family_for_style_hint(hint: StyleHint) -> Option<&'static str> {
    match hint {
        StyleHint::Helvetica => Some("sans-serif"),
        StyleHint::Times => Some("serif"),
        StyleHint::Courier | StyleHint::Monospace => Some("monospace"),
        StyleHint::Cursive => Some("cursive"),
        StyleHint::Fantasy => Some("fantasy"),
        StyleHint::System | StyleHint::AnyStyle | StyleHint::OldEnglish => None,
    }
}

/// Substitute font family for a style hint on Linux/Unix platforms matching Qt's
/// `getFcFamilyForStyleHint` (`src/gui/text/unix/qfontconfigdatabase.cpp:349-370`).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn family_for_style_hint(hint: StyleHint) -> Option<&'static str> {
    fc_family_for_style_hint(hint)
}

/// Returns whether fontconfig runtime library is available.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn is_fontconfig_available() -> bool {
    crate::text::fontconfig_ffi::is_available()
}

/// Qt's Linux font database (`QFontconfigDatabase`): the registry of families, the family a
/// request resolves to and the fallback fonts per script.
#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
/// The fallback families Qt lists for `family` (`fallbacksForFamily` of its font database) from the
/// process-wide database, for tests that derive what a missing family resolves to.
#[cfg(all(test, unix, not(target_os = "macos")))]
pub(crate) fn query_fontconfig_fallbacks(
    family: &str,
    italic: bool,
    hint: StyleHint,
) -> Vec<String> {
    use crate::text::fontconfig_catalog::Slant;
    with_global_font_database(|db| {
        db.linux.registry(&db.search_paths).fallbacks_for_family(
            family,
            if italic { Slant::Italic } else { Slant::Normal },
            hint,
            "Zyyy",
        )
    })
}

impl FontDatabase {
    /// Creates a new font database and preloads default fonts.
    pub fn new() -> Self {
        let _t = crate::startup_trace::span(|| "FontDatabase::new".into());
        let mut search_paths = Vec::new();

        #[cfg(target_os = "windows")]
        {
            if let Ok(windir) = std::env::var("WINDIR") {
                search_paths.push(PathBuf::from(windir).join("Fonts"));
            } else {
                search_paths.push(PathBuf::from("C:\\Windows\\Fonts"));
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // fontconfig's default directories (fonts.conf: /usr/share/fonts,
            // /usr/local/share/fonts, $XDG_DATA_HOME/fonts, ~/.fonts), which Qt's
            // QFontconfigDatabase scans on Linux, plus the macOS system/user font folders.
            search_paths.push(PathBuf::from("/usr/share/fonts"));
            search_paths.push(PathBuf::from("/usr/local/share/fonts"));
            search_paths.push(PathBuf::from("/System/Library/Fonts"));
            search_paths.push(PathBuf::from("/Library/Fonts"));
            let home = std::env::var_os("HOME").map(PathBuf::from);
            match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
                Some(data) => search_paths.push(PathBuf::from(data).join("fonts")),
                None => {
                    if let Some(home) = &home {
                        search_paths.push(home.join(".local/share/fonts"));
                    }
                }
            }
            if let Some(home) = &home {
                search_paths.push(home.join(".fonts"));
                search_paths.push(home.join("Library/Fonts"));
            }
        }

        let mut db = Self {
            cache: HashMap::new(),
            raw_cache: HashMap::new(),
            file_cache: HashMap::new(),
            #[cfg(all(unix, not(target_os = "macos")))]
            linux: linux::LinuxFonts::new(search_paths.len()),
            search_paths,
            system_faces: None,
            application_faces: Vec::new(),
            engines: HashMap::new(),
            recent_memory_faces: Vec::new(),
            memory_faces: Vec::new(),
            #[cfg(windows)]
            fallback_memo: FallbackMemo::default(),
        };
        db.preload_default_fonts();
        db
    }

    /// Adds a directory to the font search paths.
    pub fn add_search_path<P: AsRef<Path>>(&mut self, path: P) {
        self.search_paths.push(path.as_ref().to_path_buf());
        self.system_faces = None;
        #[cfg(all(unix, not(target_os = "macos")))]
        self.linux.invalidate();
        FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    /// Registers a custom font from memory.
    pub fn add_font_from_memory(
        &mut self,
        family: &str,
        data: impl Into<SharedFontData>,
    ) -> Result<SharedGlyphFace, String> {
        let shared = data.into();
        let hint_style = face_hint_style(HintingPreference::PreferDefaultHinting);
        let font_arc = parse_face_with_hint_style(&shared, 0, hint_style)
            .map_err(|e| format!("Failed to parse font from memory: {e}"))?;
        // Faces of this font for other hint styles are built from the same data (see
        // `load_font_styled`); this one is found there too.
        self.memory_faces.push((
            memory_face_key(&shared, hint_style, 400, false),
            Arc::downgrade(&font_arc),
        ));
        let key = family.to_ascii_lowercase();
        let (fixed_pitch, weight, italic, stretch) =
            read_faces(&mut std::io::Cursor::new(shared.as_slice()))
                .first()
                .map(|face| (face.fixed_pitch, face.weight, face.italic, face.stretch))
                .unwrap_or((false, 400, false, 100));
        self.application_faces
            .retain(|face| !face.family.eq_ignore_ascii_case(family));
        self.application_faces.push(FontFamilyInfo {
            family: family.to_string(),
            path: None,
            face_index: 0,
            fixed_pitch,
            weight,
            italic,
            stretch,
        });
        self.cache.insert(key.clone(), Arc::clone(&font_arc));
        self.raw_cache.insert(key, shared);
        FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(font_arc)
    }

    /// Sorted, de-duplicated family names of every installed and application font
    /// (`QFontDatabase::families`). On Linux that is Qt's registry: the families fontconfig lists
    /// (a family a font gives a second name and style, such as "DejaVu Sans Condensed", is one
    /// too) and the generic `Monospace`, `Sans Serif` and `Serif`.
    pub fn families(&mut self) -> Vec<String> {
        #[cfg(all(unix, not(target_os = "macos")))]
        return self.registry_families();
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        self.collect_families(|_| true)
    }

    /// Families whose faces are all fixed-pitch (`QFontDatabase::isFixedPitch` filter).
    pub fn monospaced_families(&mut self) -> Vec<String> {
        let fixed: Vec<String> = self.collect_families(|face| face.fixed_pitch);
        fixed
            .into_iter()
            .filter(|family| self.is_fixed_pitch(family))
            .collect()
    }

    /// Families with at least one proportional (non fixed-pitch) face.
    pub fn proportional_families(&mut self) -> Vec<String> {
        self.collect_families(|face| !face.fixed_pitch)
    }

    /// Returns `true` if a family with this name (case-insensitive) is installed or registered.
    pub fn has_family(&mut self, family: &str) -> bool {
        #[cfg(all(unix, not(target_os = "macos")))]
        if self.registry_has_family(family) {
            return true;
        }
        self.faces()
            .any(|face| face.family.eq_ignore_ascii_case(family))
    }

    /// Returns `true` if every known face of `family` is fixed-pitch (`QFontDatabase::isFixedPitch`).
    pub fn is_fixed_pitch(&mut self, family: &str) -> bool {
        let mut matched = false;
        for face in self.faces() {
            if face.family.eq_ignore_ascii_case(family) {
                if !face.fixed_pitch {
                    return false;
                }
                matched = true;
            }
        }
        matched
    }

    /// Every face known to the database (system fonts plus application fonts).
    pub(super) fn faces(&mut self) -> impl Iterator<Item = &FontFamilyInfo> {
        if self.system_faces.is_none() {
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                self.system_faces = Some(self.linux.registered_faces(&self.search_paths));
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            {
                self.system_faces = Some(scan_font_directories(&self.search_paths));
            }
        }
        self.system_faces
            .iter()
            .flatten()
            .chain(self.application_faces.iter())
    }

    pub(super) fn collect_families(
        &mut self,
        mut keep: impl FnMut(&FontFamilyInfo) -> bool,
    ) -> Vec<String> {
        let mut names: Vec<String> = self
            .faces()
            .filter(|face| keep(face))
            .map(|face| face.family.clone())
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    }

    /// Loads the regular face of a family, caching the result.
    pub fn load_font(&mut self, family: &str) -> Option<SharedGlyphFace> {
        self.load_font_styled(family, FaceStyle::regular(), StyleHint::AnyStyle)
    }

    /// The file of macOS's system UI font (`CTFontCreateUIFontForLanguage(kCTFontUIFontSystem)`),
    /// read once and kept under a pseudo path that no real file has.
    #[cfg(target_os = "macos")]
    pub(super) fn get_system_ui_font_data(&mut self) -> Option<SharedFontData> {
        let canonical = std::path::PathBuf::from(".applesystemuifont");
        if let Some(shared) = self.file_cache.get(&canonical) {
            return Some(shared.clone());
        }
        let path = query_system_ui_font_path()
            .unwrap_or_else(|| std::path::PathBuf::from("/System/Library/Fonts/SFNS.ttf"));
        let data = SharedFontData::from_file(&path).ok()?;
        self.file_cache.insert(canonical, data.clone());
        Some(data)
    }

    /// Whether `name` is a family of the system's or the application's fonts, and so one that
    /// `load_font_styled` loads as itself rather than replaces by a substitute.
    pub(super) fn is_installed_family(&mut self, name: &str) -> bool {
        #[cfg(windows)]
        if let Some(catalog) = crate::text::directwrite_catalog::catalog() {
            return catalog.family_traits(name).is_some()
                || self
                    .application_faces
                    .iter()
                    .any(|face| face.family.eq_ignore_ascii_case(name));
        }
        self.has_family(name)
    }

    /// The name of a comma separated family list (`'Consolas', monospace`) that is used, with its
    /// face; `None` when `family` is not a list or none of its names loads. `QFontDatabasePrivate::
    /// load` looks the first family up on its own (`findFont`, passes 1 and 2) and hands the others
    /// on as `fallBackFamilies`, which the fallback pass (pass 4) tries in order before any
    /// substitute (`fallbacksForFamily`): the first name that is an installed family wins, and only
    /// when none is does the first name's substitute count. The generic names (`sans-serif`,
    /// `serif`, `monospace`) are skipped. Everything keyed by the requested string (raw font data,
    /// engines) has to follow this one choice, so it is made in one place.
    pub(super) fn load_first_listed(
        &mut self,
        family: &str,
        style: FaceStyle,
        hint: StyleHint,
    ) -> Option<(String, SharedGlyphFace)> {
        if !family.contains(',') {
            return None;
        }
        let names: Vec<&str> = family
            .split(',')
            .map(|name| name.trim().trim_matches('\'').trim_matches('"'))
            .filter(|name| {
                !(name.is_empty()
                    || name.eq_ignore_ascii_case("sans-serif")
                    || name.eq_ignore_ascii_case("monospace")
                    || name.eq_ignore_ascii_case("serif"))
            })
            .collect();
        for installed_only in [true, false] {
            for &name in &names {
                if installed_only && !self.is_installed_family(name) {
                    continue;
                }
                if let Some(face) = self.load_font_styled(name, style, hint) {
                    // The family list resolved to the family of the name that loaded.
                    #[cfg(all(unix, not(target_os = "macos")))]
                    self.copy_resolved_family(
                        &style.cache_key(&name.to_ascii_lowercase(), hint),
                        style.cache_key(&family.to_ascii_lowercase(), hint),
                    );
                    return Some((name.to_owned(), face));
                }
            }
        }
        None
    }

    /// Loads the face of `family` nearest to `style` (`QFontDatabase::bestStyle`), caching the result.
    /// A family without a face for that style yields its nearest one (e.g. regular for bold); on
    /// Windows that face is then created with DirectWrite's bold or oblique simulation, as Qt
    /// does ([`parse_face_for_request`]).
    pub fn load_font_styled(
        &mut self,
        family: &str,
        style: FaceStyle,
        hint: StyleHint,
    ) -> Option<SharedGlyphFace> {
        if let Some((_, face)) = self.load_first_listed(family, style, hint) {
            return Some(face);
        }

        let key = family.to_ascii_lowercase();
        let style_key = style.cache_key(&key, hint);
        if let Some(font) = self.cache.get(&style_key) {
            return Some(Arc::clone(font));
        }

        // Qt's DirectWrite database decides what a family name is: the typographic family names,
        // the GDI-compatible names of styled faces ("Segoe UI Semibold"), the named instances of
        // variable fonts, each with its styles and `bestStyle` (see `directwrite_catalog`). A name
        // it does not know (a font only in a search path added by the caller) is searched for in
        // the font files.
        #[cfg(windows)]
        if let Some((raw_data, face)) = self.load_catalog_font(&key, style) {
            self.cache.insert(style_key.clone(), Arc::clone(&face));
            self.raw_cache.insert(style_key, raw_data);
            return Some(face);
        }

        // Linux: the family registry Qt builds from fontconfig decides what a family name is and
        // which of its styles a request gets (see `font_database_linux`).
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(found) = self.resolve_registered(family, style) {
            return Some(self.store_resolved(style_key, found));
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        if let Some((raw_data, face)) = self.find_and_load_font_file(&key, style) {
            self.cache.insert(style_key.clone(), Arc::clone(&face));
            self.raw_cache.insert(style_key, raw_data);
            return Some(face);
        }

        // An in-memory (application) font has a single face: use it for any style, built for the
        // hint style asked for (and, on Windows, with DirectWrite's simulations for a weight or
        // slant it lacks, as Qt does for an application font).
        if let Some(face) = self.cache.get(&key).cloned() {
            let raw = self.raw_cache.get(&key).cloned();
            let face = raw
                .as_ref()
                .and_then(|raw| self.memory_face(raw, style.hint_style, style.weight, style.italic))
                .unwrap_or(face);
            self.cache.insert(style_key.clone(), Arc::clone(&face));
            if let Some(raw) = raw {
                self.raw_cache.insert(style_key, raw);
            }
            return Some(face);
        }

        #[cfg(windows)]
        {
            // A family that is not there is replaced by the first family that exists of what Qt's
            // fallback pass lists (`QFontDatabasePrivate::findFont`, step 4): the family of the
            // style hint, then the ones `extraTryFontsForFamily` names for the user's language.
            let hint_family = family_for_style_hint(hint);
            let catalog = crate::text::directwrite_catalog::catalog();
            let list = catalog.map(|catalog| {
                windows_fallback_families(
                    catalog,
                    fallback_list_key(
                        hint,
                        style.italic,
                        crate::text::directwrite_catalog::supports_symbol(0),
                        "Zyyy",
                        unicode_script::Script::Common,
                    ),
                )
            });
            // The list is walked as far as the first substitute that loads.
            let listed = catalog
                .zip(list.as_deref())
                .into_iter()
                .flat_map(|(catalog, list)| list.iter(catalog));
            for substitute in std::iter::once(hint_family).chain(listed) {
                if key.eq_ignore_ascii_case(substitute) {
                    continue;
                }
                let Some(fallback) = self.load_font_styled(substitute, style, hint) else {
                    continue;
                };
                self.cache.insert(style_key.clone(), Arc::clone(&fallback));
                let sub_key = style.cache_key(&substitute.to_ascii_lowercase(), hint);
                if let Some(raw) = self.raw_cache.get(&sub_key).cloned() {
                    self.raw_cache.insert(style_key, raw);
                }
                return Some(fallback);
            }
        }

        #[cfg(target_os = "macos")]
        {
            if let Some(substitute) = family_for_style_hint(hint)
                && !key.eq_ignore_ascii_case(substitute)
                && let Some(fallback) = self.load_font_styled(substitute, style, hint)
            {
                self.cache.insert(style_key.clone(), Arc::clone(&fallback));
                let sub_key = style.cache_key(&substitute.to_ascii_lowercase(), hint);
                if let Some(raw) = self.raw_cache.get(&sub_key).cloned() {
                    self.raw_cache.insert(style_key, raw);
                }
                return Some(fallback);
            }

            if key == ".applesystemuifont"
                && let Some(sys_data) = self.get_system_ui_font_data()
                && let Ok(face) = crate::text::glyph_face::parse_system_ui_face(&sys_data)
            {
                self.cache.insert(style_key.clone(), Arc::clone(&face));
                self.raw_cache.insert(style_key, sys_data);
                return Some(face);
            }
        }

        // A family that is not there is replaced by the first family fontconfig lists for it that
        // exists (`QFontDatabasePrivate::findFont`, step 4).
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(found) = self.resolve_substitute(family, style, hint) {
            let own_key = style.cache_key(&found.family().to_ascii_lowercase(), hint);
            return Some(self.store_substitute(style_key, own_key, found));
        }

        // Last resort on every non-Windows target: the pre-existing guess, which is not Qt's. On
        // Linux it applies only when fontconfig is unavailable (or lists nothing loadable, and
        // under Miri, which cannot call it). On macOS it applies only if CoreText fails to load
        // the style hint substitute or system UI font.
        #[cfg(not(windows))]
        {
            if key != "segoe ui"
                && key != "arial"
                && let Some(fallback) = self
                    .load_font_styled("segoe ui", style, hint)
                    .or_else(|| self.load_font_styled("arial", style, hint))
            {
                self.cache.insert(style_key.clone(), Arc::clone(&fallback));
                return Some(fallback);
            }
        }

        None
    }

    /// Returns the raw binary font data of the regular face for OpenType shaping.
    pub fn get_raw_font_data(&mut self, family: &str) -> Option<SharedFontData> {
        self.get_raw_font_data_styled(family, FaceStyle::regular(), StyleHint::AnyStyle)
    }

    /// Returns the raw binary font data of the face [`load_font_styled`](Self::load_font_styled) picks.
    pub fn get_raw_font_data_styled(
        &mut self,
        family: &str,
        style: FaceStyle,
        hint: StyleHint,
    ) -> Option<SharedFontData> {
        // A family list has no entry of its own: its raw data is that of the name `load_font_styled`
        // took from it, so the engine shapes with the face it measures with.
        if let Some((name, _)) = self.load_first_listed(family, style, hint) {
            return self.get_raw_font_data_styled(&name, style, hint);
        }
        let key = style.cache_key(&family.to_ascii_lowercase(), hint);
        if let Some(data) = self.raw_cache.get(&key) {
            return Some(data.clone());
        }
        self.load_font_styled(family, style, hint);
        self.raw_cache.get(&key).cloned()
    }

    /// Returns the raw binary font data by canonical or relative file path.
    pub fn get_raw_font_data_by_path(&mut self, path: &Path) -> Option<SharedFontData> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(data) = self.file_cache.get(&canonical) {
            return Some(data.clone());
        }
        if let Ok(shared) = SharedFontData::from_file(&canonical) {
            self.file_cache.insert(canonical, shared.clone());
            return Some(shared);
        }
        None
    }

    /// Returns the total bytes across all cached binary font files.
    pub fn total_raw_bytes(&self) -> usize {
        self.file_cache.values().map(|d| d.len()).sum()
    }

    /// Returns the number of distinct font files cached in memory.
    pub fn cached_files_count(&self) -> usize {
        self.file_cache.len()
    }

    /// Purges cached font data that is no longer referenced outside the database cache.
    ///
    /// Mirrors Qt's `QFontCache::decreaseCache`: inspects strong reference counts and
    /// purges unused raw buffers from memory.
    pub fn cleanup_unused_fonts(&mut self) -> usize {
        let mut freed_bytes = 0;
        self.file_cache.retain(|_path, data| {
            if data.strong_count() <= 2 {
                freed_bytes += data.len();
                false
            } else {
                true
            }
        });
        self.raw_cache.retain(|_fam, data| data.strong_count() > 1);
        self.cache.retain(|_fam, font| Arc::strong_count(font) > 1);
        freed_bytes
    }

    /// Preloads common default system fonts.
    pub(super) fn preload_default_fonts(&mut self) {
        let default_families = ["segoe ui", "arial", "consolas"];
        for fam in &default_families {
            if let Some((raw_data, face)) = self.find_and_load_font_file(fam, FaceStyle::regular())
            {
                self.cache.insert((*fam).to_string(), face);
                self.raw_cache.insert((*fam).to_string(), raw_data);
                break;
            }
        }
    }

    /// The face Qt's DirectWrite font database resolves the family name `family_key` to: the
    /// style of the registered family nearest to `style` (`bestStyle`), including the family
    /// names of styled faces ("Segoe UI Semibold") and the named instances of variable fonts
    /// ([`directwrite_catalog`](crate::text::directwrite_catalog)).
    #[cfg(windows)]
    pub(super) fn load_catalog_font(
        &mut self,
        family_key: &str,
        style: FaceStyle,
    ) -> Option<(SharedFontData, SharedGlyphFace)> {
        let catalog = crate::text::directwrite_catalog::catalog()?;
        let source = catalog.resolve(family_key, style.weight, style.italic)?;
        let canonical = source
            .path
            .canonicalize()
            .unwrap_or_else(|_| source.path.clone());
        let shared = match self.file_cache.get(&canonical) {
            Some(shared) => shared.clone(),
            None => {
                let shared = SharedFontData::from_file(&canonical).ok()?;
                self.file_cache.insert(canonical, shared.clone());
                shared
            }
        };
        let face = crate::text::glyph_face::parse_face_for_instance(
            &shared,
            source.face_index,
            style.hint_style,
            style.weight,
            style.italic,
            &source.axes,
        )
        .ok()?;
        Some((shared, face))
    }

    /// Searches search paths for a font file matching family key, reusing existing
    /// cached binary buffers from `file_cache` to eliminate redundant disk I/O and memory duplication.
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    pub(super) fn find_and_load_font_file(
        &mut self,
        family_key: &str,
        style: FaceStyle,
    ) -> Option<(SharedFontData, SharedGlyphFace)> {
        let _t = crate::startup_trace::span(|| format!("find_and_load_font_file({family_key:?})"));
        let regular_filenames: Vec<String> = match family_key {
            "segoe ui" => vec!["segoeui.ttf".into(), "SegoeUI.ttf".into()],
            "segoe ui symbol" => vec!["seguisym.ttf".into()],
            "segoe ui emoji" => vec!["seguiemj.ttf".into()],
            "arial" => vec!["arial.ttf".into(), "Arial.ttf".into()],
            "consolas" => vec![
                "consola.ttf".into(),
                "Consola.ttf".into(),
                "consolas.ttf".into(),
            ],
            "tahoma" => vec!["tahoma.ttf".into(), "Tahoma.ttf".into()],
            "courier new" => vec!["cour.ttf".into(), "Cour.ttf".into()],
            "times new roman" => vec!["times.ttf".into(), "Times.ttf".into()],
            "microsoft jhenghei" | "msjh" => vec![
                "msjh.ttc".into(),
                "msjh.ttf".into(),
                "msjhl.ttc".into(),
                "msjhbd.ttc".into(),
            ],
            "microsoft yahei" | "msyh" => vec![
                "msyh.ttc".into(),
                "msyh.ttf".into(),
                "msyhl.ttc".into(),
                "msyhbd.ttc".into(),
            ],
            "ms gothic" => vec!["msgothic.ttc".into(), "msgothic.ttf".into()],
            other => {
                let no_space = other.replace(' ', "");
                vec![
                    format!("{}.ttf", other),
                    format!("{}.otf", other),
                    format!("{}.ttf", no_space),
                    format!("{}.otf", no_space),
                ]
            }
        };
        // A styled request first tries the family's bold/italic file by name (no directory scan),
        // then the regular file: the nearest face, like `QFontDatabase::bestStyle`, for the
        // families that ship no such variant. Families not known by name go to the face index.
        let candidate_filenames: Vec<String> = if style.weight == 400 && !style.italic {
            regular_filenames
        } else {
            match known_styled_filenames(family_key, style) {
                Some(styled) => styled
                    .iter()
                    .map(|name| (*name).to_string())
                    .chain(regular_filenames)
                    .collect(),
                None => Vec::new(),
            }
        };

        for dir in &self.search_paths {
            for filename in &candidate_filenames {
                let file_path = dir.join(filename);
                if file_path.is_file() {
                    let canonical = file_path
                        .canonicalize()
                        .unwrap_or_else(|_| file_path.clone());
                    if let Some(shared) = self.file_cache.get(&canonical)
                        && let Ok(face) = parse_face_for_request(
                            shared,
                            0,
                            style.hint_style,
                            style.weight,
                            style.italic,
                        )
                    {
                        return Some((shared.clone(), face));
                    }
                    let read =
                        crate::startup_trace::span(|| format!("map {}", canonical.display()));
                    if let Ok(shared) = SharedFontData::from_file(&canonical) {
                        drop(read);
                        let _p = crate::startup_trace::span(|| {
                            format!(
                                "parse_face {} ({} KB)",
                                canonical.display(),
                                shared.len() / 1024
                            )
                        });
                        if let Ok(face) = parse_face_for_request(
                            &shared,
                            0,
                            style.hint_style,
                            style.weight,
                            style.italic,
                        ) {
                            self.file_cache.insert(canonical, shared.clone());
                            return Some((shared, face));
                        }
                    }
                }
            }
        }

        // Fall back to the family index (file names rarely match family names, e.g. "Times New Roman" -> times.ttf).
        if self.system_faces.is_none() {
            let _t = crate::startup_trace::span(|| {
                format!("scan_font_directories (needed by {family_key:?})")
            });
            self.system_faces = Some(scan_font_directories(&self.search_paths));
        }
        let (face_path, face_index) = {
            // `QFontDatabase::bestStyle`: the face of the family with the smallest style distance
            // (first one wins a tie).
            let face = self
                .system_faces
                .as_ref()?
                .iter()
                .filter(|face| face.path.is_some() && face.family.eq_ignore_ascii_case(family_key))
                .min_by_key(|face| style.distance(face.weight, face.italic, face.stretch))?;
            (face.path.as_ref()?.clone(), face.face_index)
        };

        let canonical = face_path
            .canonicalize()
            .unwrap_or_else(|_| face_path.clone());

        if let Some(shared) = self.file_cache.get(&canonical) {
            let face = parse_face_for_request(
                shared,
                face_index,
                style.hint_style,
                style.weight,
                style.italic,
            )
            .ok()?;
            return Some((shared.clone(), face));
        }

        let _t = crate::startup_trace::span(|| {
            format!("map+parse (family index) {}", canonical.display())
        });
        let shared = SharedFontData::from_file(&canonical).ok()?;
        let face = parse_face_for_request(
            &shared,
            face_index,
            style.hint_style,
            style.weight,
            style.italic,
        )
        .ok()?;
        self.file_cache.insert(canonical, shared.clone());
        Some((shared, face))
    }
}

impl Default for FontDatabase {
    fn default() -> Self {
        Self::new()
    }
}

mod resolve;
mod scan;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_fallback;

#[allow(unused_imports)]
use scan::{preferred_family_name, read_faces, scan_font_directories};
#[cfg(windows)]
#[allow(unused_imports)]
use windows_fallback::{
    FallbackContext, FallbackMemo, FallbackPick, FallbackPickKey,
    compute_windows_fallback_families, extra_try_fonts, fallback_list_key,
    windows_fallback_families,
};
