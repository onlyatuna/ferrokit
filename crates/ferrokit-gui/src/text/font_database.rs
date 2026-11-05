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
    /// See [`face_hint_style`].
    pub hint_style: HintStyle,
}

impl FaceStyle {
    /// The regular face (weight 400, upright) for the process-wide default hint style.
    pub fn regular() -> Self {
        Self {
            weight: 400,
            italic: false,
            hint_style: face_hint_style(HintingPreference::PreferDefaultHinting),
        }
    }

    /// The style `font` asks for.
    pub fn of(font: &Font) -> Self {
        Self {
            weight: font.weight as u16,
            italic: font.style != FontStyle::Normal,
            hint_style: face_hint_style(font.hinting_preference),
        }
    }

    /// `bestStyle` distance to a face: weight difference in steps of 10, plus 0x1000 for the
    /// wrong slant. Zero is an exact match.
    fn distance(self, weight: u16, italic: bool) -> u32 {
        let w = (i32::from(self.weight) - i32::from(weight)).unsigned_abs() / 10;
        w + if self.italic != italic { 0x1000 } else { 0 }
    }

    /// Cache key of `family` at this style and style hint; fonts that differ only in style hint
    /// do not share a cache entry so substitute resolution is cached independently, nor do faces
    /// built for different hint styles.
    fn cache_key(self, family: &str, hint: StyleHint) -> String {
        format!(
            "{family}\u{1}{}\u{1}{}\u{1}{:?}\u{1}{:?}",
            self.weight,
            u8::from(self.italic),
            hint,
            self.hint_style
        )
    }
}

/// Windows file names of the bold/italic face of the families loaded by name without a directory
/// scan. `Some(&[])` means the family is known but has no such face (the caller falls back to
/// its regular file); `None` means the family is not known by name.
fn known_styled_filenames(family_key: &str, style: FaceStyle) -> Option<&'static [&'static str]> {
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
}

/// In-memory faces [`FontDatabase`] keeps alive strongly after their last use.
const RECENT_MEMORY_FACES: usize = 4;

/// Identity of a face built from in-memory font data: the data address and length, the hint style,
/// and what the request asks for that DirectWrite would synthesize (see [`memory_face_key`]).
type MemoryFaceKey = (usize, usize, HintStyle, bool, bool);

/// `memory_faces` entry: the key of a face and the face.
type MemoryFaceEntry = (
    MemoryFaceKey,
    std::sync::Weak<dyn crate::text::glyph_face::GlyphFace>,
);

/// The `memory_faces` key of the face of `shared` for `hint_style` and a request of `weight` that
/// is `italic` or not. On Windows the face differs by whether the request is at least `DemiBold`
/// and whether it is italic, the two things Qt's DirectWrite font database synthesizes (see
/// [`parse_face_for_request`]); the other backends build one face whatever the request, so they
/// share it.
fn memory_face_key(
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
    cache: HashMap<String, SharedGlyphFace>,
    raw_cache: HashMap<String, SharedFontData>,
    file_cache: HashMap<PathBuf, SharedFontData>,
    search_paths: Vec<PathBuf>,
    /// Faces found by scanning `search_paths`; built lazily by [`FontDatabase::families`].
    system_faces: Option<Vec<FontFamilyInfo>>,
    /// Faces registered through [`FontDatabase::add_font_from_memory`].
    application_faces: Vec<FontFamilyInfo>,
    /// One engine per requested family string, shared so glyph caches persist across draw calls.
    engines: HashMap<String, FontEngine>,
    /// Faces built from `Font::font_data`, so a draw or metrics call does not re-parse the font.
    ///
    /// Weak, so the cache never pins a (possibly huge) font blob: once every engine using a face is
    /// dropped the face and its data are released. Keyed by the data buffer's address and length,
    /// which stay unique for as long as the face (which owns the data) is alive.
    ///
    /// The draw and metrics paths drop their engines after every call, so the last few faces are also
    /// held strongly ([`RECENT_MEMORY_FACES`], most recent last) to avoid re-parsing (and, on Windows,
    /// re-registering with DirectWrite) on each draw.
    recent_memory_faces: Vec<SharedGlyphFace>,
    memory_faces: Vec<MemoryFaceEntry>,
    #[cfg(all(unix, not(target_os = "macos")))]
    fontconfig_cache: HashMap<(String, bool, StyleHint), Vec<String>>,
}

/// Bumped whenever the set of available fonts changes, so caches derived from font data
/// (e.g. text advance widths) know to drop their entries.
static FONT_GENERATION: AtomicU64 = AtomicU64::new(0);

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
    #[cfg(miri)]
    {
        false
    }
    #[cfg(not(miri))]
    {
        fontconfig_ffi::FontConfig::get().is_some()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn query_fontconfig_fallbacks(
    family: &str,
    italic: bool,
    hint: StyleHint,
) -> Vec<String> {
    #[cfg(miri)]
    {
        let _ = (family, italic, hint);
        Vec::new()
    }
    #[cfg(not(miri))]
    {
        match fontconfig_ffi::FontConfig::get() {
            Some(fc) => fc.fallbacks_for_family(family, italic, hint),
            None => Vec::new(),
        }
    }
}

#[cfg(all(unix, not(target_os = "macos"), not(miri)))]
mod fontconfig_ffi {
    use super::*;
    use std::ffi::{CStr, CString, c_char, c_int, c_void};

    pub const RTLD_LAZY: c_int = 1;
    pub const FC_TYPE_STRING: c_int = 3;
    pub const FC_SLANT_ROMAN: c_int = 0;
    pub const FC_SLANT_ITALIC: c_int = 100;
    pub const FC_MATCH_PATTERN: c_int = 0;
    pub const FC_RESULT_MATCH: c_int = 0;
    pub const FC_TRUE: c_int = 1;
    pub const FC_FALSE: c_int = 0;

    #[repr(C)]
    pub struct FcPattern {
        _unused: [u8; 0],
    }

    #[repr(C)]
    pub struct FcFontSet {
        pub nfont: c_int,
        pub sfont: c_int,
        pub fonts: *mut *mut FcPattern,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub union FcValueUnion {
        pub s: *const u8,
        pub i: c_int,
        pub b: c_int,
        pub d: f64,
        pub p: *const c_void,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct FcValue {
        pub typ: c_int,
        pub u: FcValueUnion,
    }

    unsafe extern "C" {
        pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn dlclose(handle: *mut c_void) -> c_int;
    }

    /// Closes the `dlopen` handle when symbol resolution fails; on success it is `forget`-ed
    /// so the library stays loaded for the life of the process.
    struct HandleGuard(*mut c_void);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a non-null handle returned by `dlopen` that nothing else closes.
            unsafe { dlclose(self.0) };
        }
    }

    pub type FcPatternCreateFn = unsafe extern "C" fn() -> *mut FcPattern;
    pub type FcPatternDestroyFn = unsafe extern "C" fn(*mut FcPattern);
    pub type FcPatternAddFn =
        unsafe extern "C" fn(*mut FcPattern, *const c_char, FcValue, c_int) -> c_int;
    pub type FcPatternAddWeakFn =
        unsafe extern "C" fn(*mut FcPattern, *const c_char, FcValue, c_int) -> c_int;
    pub type FcPatternAddIntegerFn =
        unsafe extern "C" fn(*mut FcPattern, *const c_char, c_int) -> c_int;
    pub type FcPatternAddStringFn =
        unsafe extern "C" fn(*mut FcPattern, *const c_char, *const u8) -> c_int;
    pub type FcPatternGetStringFn =
        unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut *mut u8) -> c_int;
    pub type FcDefaultSubstituteFn = unsafe extern "C" fn(*mut FcPattern);
    pub type FcConfigSubstituteFn =
        unsafe extern "C" fn(*mut c_void, *mut FcPattern, c_int) -> c_int;
    pub type FcFontSortFn = unsafe extern "C" fn(
        *mut c_void,
        *mut FcPattern,
        c_int,
        *mut c_void,
        *mut c_int,
    ) -> *mut FcFontSet;
    pub type FcFontSetDestroyFn = unsafe extern "C" fn(*mut FcFontSet);

    pub struct FontConfig {
        pub pattern_create: FcPatternCreateFn,
        pub pattern_destroy: FcPatternDestroyFn,
        pub pattern_add: FcPatternAddFn,
        pub pattern_add_weak: FcPatternAddWeakFn,
        pub pattern_add_integer: FcPatternAddIntegerFn,
        pub pattern_add_string: FcPatternAddStringFn,
        pub pattern_get_string: FcPatternGetStringFn,
        pub default_substitute: FcDefaultSubstituteFn,
        pub config_substitute: FcConfigSubstituteFn,
        pub font_sort: FcFontSortFn,
        pub font_set_destroy: FcFontSetDestroyFn,
    }

    impl FontConfig {
        pub fn get() -> Option<&'static Self> {
            static INSTANCE: std::sync::LazyLock<Option<FontConfig>> =
                std::sync::LazyLock::new(FontConfig::load);
            INSTANCE.as_ref()
        }

        fn load() -> Option<Self> {
            // SAFETY: calls POSIX dlopen with a valid NUL-terminated library name string
            // literal and standard flag RTLD_LAZY.
            let handle = unsafe { dlopen(c"libfontconfig.so.1".as_ptr(), RTLD_LAZY) };
            if handle.is_null() {
                return None;
            }
            let guard = HandleGuard(handle);

            // SAFETY: each symbol is queried on the valid library `handle` with a NUL-terminated
            // symbol name string literal; transmute casts non-null function pointers to their exact
            // C function signature.
            unsafe {
                let load_sym = |name: &CStr| -> Option<*mut c_void> {
                    let ptr = dlsym(handle, name.as_ptr());
                    if ptr.is_null() { None } else { Some(ptr) }
                };

                let pattern_create: FcPatternCreateFn =
                    std::mem::transmute(load_sym(c"FcPatternCreate")?);
                let pattern_destroy: FcPatternDestroyFn =
                    std::mem::transmute(load_sym(c"FcPatternDestroy")?);
                let pattern_add: FcPatternAddFn = std::mem::transmute(load_sym(c"FcPatternAdd")?);
                let pattern_add_weak: FcPatternAddWeakFn =
                    std::mem::transmute(load_sym(c"FcPatternAddWeak")?);
                let pattern_add_integer: FcPatternAddIntegerFn =
                    std::mem::transmute(load_sym(c"FcPatternAddInteger")?);
                let pattern_add_string: FcPatternAddStringFn =
                    std::mem::transmute(load_sym(c"FcPatternAddString")?);
                let pattern_get_string: FcPatternGetStringFn =
                    std::mem::transmute(load_sym(c"FcPatternGetString")?);
                let default_substitute: FcDefaultSubstituteFn =
                    std::mem::transmute(load_sym(c"FcDefaultSubstitute")?);
                let config_substitute: FcConfigSubstituteFn =
                    std::mem::transmute(load_sym(c"FcConfigSubstitute")?);
                let font_sort: FcFontSortFn = std::mem::transmute(load_sym(c"FcFontSort")?);
                let font_set_destroy: FcFontSetDestroyFn =
                    std::mem::transmute(load_sym(c"FcFontSetDestroy")?);

                std::mem::forget(guard);
                Some(Self {
                    pattern_create,
                    pattern_destroy,
                    pattern_add,
                    pattern_add_weak,
                    pattern_add_integer,
                    pattern_add_string,
                    pattern_get_string,
                    default_substitute,
                    config_substitute,
                    font_sort,
                    font_set_destroy,
                })
            }
        }

        pub fn fallbacks_for_family(
            &self,
            family: &str,
            italic: bool,
            style_hint: StyleHint,
        ) -> Vec<String> {
            // SAFETY: calls FcPatternCreate through the loaded function pointer.
            let pattern = unsafe { (self.pattern_create)() };
            if pattern.is_null() {
                return Vec::new();
            }

            let family_bytes = match CString::new(family) {
                Ok(s) => s,
                Err(_) => {
                    // SAFETY: frees allocated pattern on CString conversion error.
                    unsafe { (self.pattern_destroy)(pattern) };
                    return Vec::new();
                }
            };

            // SAFETY: adds requested family to pattern with FC_FAMILY string value.
            unsafe {
                let val = FcValue {
                    typ: FC_TYPE_STRING,
                    u: FcValueUnion {
                        s: family_bytes.as_ptr().cast(),
                    },
                };
                (self.pattern_add)(pattern, c"family".as_ptr(), val, FC_TRUE);
            }

            let slant_val = if italic {
                FC_SLANT_ITALIC
            } else {
                FC_SLANT_ROMAN
            };
            // SAFETY: adds slant integer value to pattern.
            unsafe {
                (self.pattern_add_integer)(pattern, c"slant".as_ptr(), slant_val);
            }

            // System default language substitute when family is non-empty (matching Qt qfontconfigdatabase.cpp:857-870)
            if !family.is_empty() {
                // SAFETY: creates a dummy pattern to retrieve default system language.
                let dummy = unsafe { (self.pattern_create)() };
                if !dummy.is_null() {
                    // SAFETY: substitutes defaults into dummy pattern and queries FC_LANG.
                    unsafe {
                        (self.default_substitute)(dummy);
                        let mut lang_ptr: *mut u8 = std::ptr::null_mut();
                        let res =
                            (self.pattern_get_string)(dummy, c"lang".as_ptr(), 0, &mut lang_ptr);
                        if res == FC_RESULT_MATCH && !lang_ptr.is_null() {
                            (self.pattern_add_string)(pattern, c"lang".as_ptr(), lang_ptr);
                        }
                        (self.pattern_destroy)(dummy);
                    }
                }
            }

            if let Some(stylehint) = fc_family_for_style_hint(style_hint)
                && let Ok(hint_bytes) = CString::new(stylehint)
            {
                // SAFETY: adds weak style hint family to pattern matching Qt qfontconfigdatabase.cpp:872-876.
                unsafe {
                    let val = FcValue {
                        typ: FC_TYPE_STRING,
                        u: FcValueUnion {
                            s: hint_bytes.as_ptr().cast(),
                        },
                    };
                    (self.pattern_add_weak)(pattern, c"family".as_ptr(), val, FC_TRUE);
                }
            }

            // SAFETY: config and default substitute on pattern.
            unsafe {
                (self.config_substitute)(std::ptr::null_mut(), pattern, FC_MATCH_PATTERN);
                (self.default_substitute)(pattern);
            }

            let mut res: c_int = FC_RESULT_MATCH;
            // SAFETY: font sort sorts fonts matching pattern; pattern is destroyed immediately after.
            let font_set = unsafe {
                let fs = (self.font_sort)(
                    std::ptr::null_mut(),
                    pattern,
                    FC_FALSE,
                    std::ptr::null_mut(),
                    &mut res,
                );
                (self.pattern_destroy)(pattern);
                fs
            };

            let mut fallback_families = Vec::new();
            if !font_set.is_null() {
                // SAFETY: reads font families from font_set and frees font_set.
                unsafe {
                    let nfont = (*font_set).nfont;
                    let fonts = (*font_set).fonts;
                    if fonts.is_null() {
                        (self.font_set_destroy)(font_set);
                        return fallback_families;
                    }
                    let mut seen = std::collections::HashSet::new();
                    // Pre-seed seen with requested family (case-folded), matching Qt's QDuplicateTracker
                    seen.insert(family.to_lowercase());

                    for i in 0..nfont {
                        let font_pat = *fonts.add(i as usize);
                        if font_pat.is_null() {
                            continue;
                        }
                        let mut val_ptr: *mut u8 = std::ptr::null_mut();
                        if (self.pattern_get_string)(font_pat, c"family".as_ptr(), 0, &mut val_ptr)
                            == FC_RESULT_MATCH
                            && !val_ptr.is_null()
                        {
                            let name = CStr::from_ptr(val_ptr.cast())
                                .to_string_lossy()
                                .into_owned();
                            let name_lower = name.to_lowercase();
                            if seen.insert(name_lower) {
                                fallback_families.push(name);
                            }
                        }
                    }
                    (self.font_set_destroy)(font_set);
                }
            }

            fallback_families
        }
    }
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
            search_paths,
            system_faces: None,
            application_faces: Vec::new(),
            engines: HashMap::new(),
            recent_memory_faces: Vec::new(),
            memory_faces: Vec::new(),
            #[cfg(all(unix, not(target_os = "macos")))]
            fontconfig_cache: HashMap::new(),
        };
        db.preload_default_fonts();
        db
    }

    /// Adds a directory to the font search paths.
    pub fn add_search_path<P: AsRef<Path>>(&mut self, path: P) {
        self.search_paths.push(path.as_ref().to_path_buf());
        self.system_faces = None;
        #[cfg(all(unix, not(target_os = "macos")))]
        self.fontconfig_cache.clear();
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
        let (fixed_pitch, weight, italic) =
            read_faces(&mut std::io::Cursor::new(shared.as_slice()))
                .first()
                .map(|face| (face.fixed_pitch, face.weight, face.italic))
                .unwrap_or((false, 400, false));
        self.application_faces
            .retain(|face| !face.family.eq_ignore_ascii_case(family));
        self.application_faces.push(FontFamilyInfo {
            family: family.to_string(),
            path: None,
            face_index: 0,
            fixed_pitch,
            weight,
            italic,
        });
        self.cache.insert(key.clone(), Arc::clone(&font_arc));
        self.raw_cache.insert(key, shared);
        FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
        Ok(font_arc)
    }

    /// Sorted, de-duplicated family names of every installed and application font
    /// (`QFontDatabase::families`).
    pub fn families(&mut self) -> Vec<String> {
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

    /// Every face known to the database (system scan plus application fonts).
    fn faces(&mut self) -> impl Iterator<Item = &FontFamilyInfo> {
        if self.system_faces.is_none() {
            self.system_faces = Some(scan_font_directories(&self.search_paths));
        }
        self.system_faces
            .iter()
            .flatten()
            .chain(self.application_faces.iter())
    }

    fn collect_families(&mut self, mut keep: impl FnMut(&FontFamilyInfo) -> bool) -> Vec<String> {
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
    fn get_system_ui_font_data(&mut self) -> Option<SharedFontData> {
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
        if family.contains(',') {
            for candidate in family.split(',') {
                let clean = candidate.trim().trim_matches('\'').trim_matches('"');
                if clean.is_empty()
                    || clean.eq_ignore_ascii_case("sans-serif")
                    || clean.eq_ignore_ascii_case("monospace")
                    || clean.eq_ignore_ascii_case("serif")
                {
                    continue;
                }
                if let Some(f) = self.load_font_styled(clean, style, hint) {
                    return Some(f);
                }
            }
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
            let mut candidates = vec![family_for_style_hint(hint).to_string()];
            if let Some(catalog) = crate::text::directwrite_catalog::catalog() {
                candidates.extend(windows_fallback_families(
                    catalog,
                    &key,
                    0,
                    style.italic,
                    hint,
                    "Zyyy",
                    unicode_script::Script::Common,
                ));
            }
            for substitute in candidates {
                if key.eq_ignore_ascii_case(&substitute) {
                    continue;
                }
                let Some(fallback) = self.load_font_styled(&substitute, style, hint) else {
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

        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let fc_key = (key.clone(), style.italic, hint);
            let candidates = if let Some(cached) = self.fontconfig_cache.get(&fc_key) {
                cached.clone()
            } else {
                let fallbacks = query_fontconfig_fallbacks(&key, style.italic, hint);
                self.fontconfig_cache.insert(fc_key, fallbacks.clone());
                fallbacks
            };

            for candidate in candidates {
                let cand_key = candidate.to_ascii_lowercase();
                if cand_key == key {
                    continue;
                }
                let cand_style_key = style.cache_key(&cand_key, hint);
                if let Some(cached_face) = self.cache.get(&cand_style_key) {
                    let face = Arc::clone(cached_face);
                    self.cache.insert(style_key.clone(), Arc::clone(&face));
                    if let Some(raw) = self.raw_cache.get(&cand_style_key).cloned() {
                        self.raw_cache.insert(style_key, raw);
                    }
                    return Some(face);
                }
                if let Some((raw, face)) = self.find_and_load_font_file(&cand_key, style) {
                    self.cache.insert(cand_style_key.clone(), Arc::clone(&face));
                    self.raw_cache.insert(cand_style_key, raw.clone());
                    self.cache.insert(style_key.clone(), Arc::clone(&face));
                    self.raw_cache.insert(style_key, raw);
                    return Some(face);
                }
            }
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
        let key = style.cache_key(&family.to_ascii_lowercase(), hint);
        if let Some(data) = self.raw_cache.get(&key) {
            return Some(data.clone());
        }
        self.load_font_styled(family, style, hint);
        self.raw_cache.get(&key).cloned()
    }
    /// Resolves the primary font engine and its platform-aware fallback chain (`QFontEngineMulti` equivalent).
    ///
    /// The resulting engines are ordered:
    /// 1. Primary requested font (or in-memory custom font if provided in `font.font_data`)
    /// 2. CJK / Multilingual script fallback fonts
    /// 3. Symbol and Emoji fallback fonts
    pub fn resolve_font_engines(&mut self, font: &Font) -> Vec<FontEngine> {
        let _t = crate::startup_trace::span_min(0.5, || {
            format!("resolve_font_engines({:?})", font.family)
        });
        let mut engines = self.resolve_primary_engine(font);
        self.append_fallback_engines(&mut engines, None, FaceStyle::of(font));
        engines
    }

    /// [`resolve_font_engines`](Self::resolve_font_engines), but fallback fonts are loaded only
    /// while `text` still has a character no engine resolved so far has a glyph for.
    ///
    /// The result is a prefix of the full chain, and `GlyphLayout::partition_into_runs` only
    /// consults a fallback for characters the earlier engines lack, so shaping is unchanged.
    pub fn resolve_font_engines_for_text(&mut self, font: &Font, text: &str) -> Vec<FontEngine> {
        let _t = crate::startup_trace::span_min(0.5, || {
            format!("resolve_font_engines_for_text({:?})", font.family)
        });
        let mut engines = self.resolve_primary_engine(font);
        #[cfg(windows)]
        if self.append_script_fallback_engines(&mut engines, text, font) {
            return engines;
        }
        self.append_fallback_engines(&mut engines, Some(text), FaceStyle::of(font));
        engines
    }

    /// The font requested by `font` alone (empty when the family cannot be loaded).
    fn resolve_primary_engine(&mut self, font: &Font) -> Vec<FontEngine> {
        let mut engines: Vec<FontEngine> = Vec::with_capacity(4);

        // 1. Primary in-memory font data if specified
        if let Some(shared) = &font.font_data
            && let Some(face) =
                self.memory_face(shared, face_hint_style(font.hinting_preference), 400, false)
        {
            engines.push(
                FontEngine::new(face)
                    .with_raw_data(shared.clone())
                    .with_face_index(0),
            );
        }

        // If no in-memory font or in-memory font failed to parse, load primary by family
        if engines.is_empty() {
            self.try_add_engine(
                &mut engines,
                &font.family,
                FaceStyle::of(font),
                font.style_hint,
            );
        }
        engines
    }

    /// The face built from in-memory font data for `hint_style` and a request of `weight` that is
    /// `italic` or not, parsed once per buffer and request and remembered (see `memory_faces`);
    /// `None` when the data does not parse.
    fn memory_face(
        &mut self,
        shared: &SharedFontData,
        hint_style: HintStyle,
        weight: u16,
        italic: bool,
    ) -> Option<SharedGlyphFace> {
        self.memory_faces.retain(|(_, f)| f.strong_count() > 0);
        let key = memory_face_key(shared, hint_style, weight, italic);
        let cached = self
            .memory_faces
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, f)| f.upgrade());
        let face = match cached {
            Some(face) => Some(face),
            None => parse_face_for_request(shared, 0, hint_style, weight, italic)
                .ok()
                .inspect(|face| {
                    // Each parse re-registers the font with the platform rasterizer, so remember it.
                    self.memory_faces.push((key, Arc::downgrade(face)));
                }),
        }?;
        self.recent_memory_faces.retain(|f| !Arc::ptr_eq(f, &face));
        if self.recent_memory_faces.len() >= RECENT_MEMORY_FACES {
            self.recent_memory_faces.remove(0);
        }
        self.recent_memory_faces.push(face.clone());
        Some(face)
    }

    /// The face of the font `font` itself asks for: its in-memory data, else the family it names.
    fn primary_face(&mut self, font: &Font) -> Option<SharedGlyphFace> {
        if let Some(shared) = &font.font_data
            && let Some(face) =
                self.memory_face(shared, face_hint_style(font.hinting_preference), 400, false)
        {
            return Some(face);
        }
        self.load_font_styled(&font.family, FaceStyle::of(font), font.style_hint)
    }

    /// Appends the platform's fallback fonts in priority order: CJK first, then emoji/symbols.
    /// With `needed_for`, stops as soon as every character of that text has a glyph in `engines`.
    fn append_fallback_engines(
        &mut self,
        engines: &mut Vec<FontEngine>,
        needed_for: Option<&str>,
        style: FaceStyle,
    ) {
        #[cfg(target_os = "windows")]
        const SLOTS: &[&[&str]] = &[
            &["Microsoft JhengHei", "Microsoft YaHei"],
            &["Segoe UI Emoji", "Segoe UI Symbol"],
        ];
        // macOS: these families are ferrokit's own picks, not Qt's cascade. Qt
        // (`QCoreTextFontDatabase::fallbacksForFamily`, qcoretextfontdatabase.mm) asks CoreText for
        // `CTFontCopyDefaultCascadeListForLanguages(font, preferredLanguages)`; measured on macOS 26
        // with en-US, for the system UI font, that is `.AppleSystemFallback, .SF Georgian,
        // Helvetica Neue, .Apple Symbols Fallback, .SF Arabic, .AppleUrduFont,
        // .AppleSimplifiedChineseFont, .AppleTraditionalChineseFont, .AppleHongKongChineseFont,
        // .AppleMacaoChineseFont, .AppleJapaneseFont, .AppleKoreanFont, ...` (private families,
        // one per script). Qt moves the two last-resort families to the end, appends `Arial Unicode
        // MS` and `Apple Symbols`, and uses `.Apple Color Emoji UI` for emoji. ferrokit loads files
        // by family and has no cascade, so:
        // * `PingFang TC`, `PingFang SC`, `Heiti TC` (the CJK slot) are ferrokit's choice: one
        //   Han font, Traditional first, where Qt takes the Simplified family first for en-US
        //   (`.AppleSimplifiedChineseFont`; for Helvetica it names PingFang SC). `Heiti TC` is the
        //   system font before PingFang.
        // * `Apple Color Emoji` is ferrokit's choice (Qt: `.Apple Color Emoji UI`).
        // * `Apple Symbols` is Qt's (its own final fail-safe; the cascade has it too).
        // `AppleGothic` was added for machines without PingFang: availability, not Qt parity (Qt
        // measured `.AppleKoreanFont` or `Apple SD Gothic Neo` for Hangul, PingFang for Han), so it
        // is not here. Kana, Hangul and the other scripts of the cascade are not covered by these
        // slots.
        #[cfg(target_os = "macos")]
        const SLOTS: &[&[&str]] = &[
            &["PingFang TC", "PingFang SC", "Heiti TC"],
            &["Apple Color Emoji", "Apple Symbols"],
        ];
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        const SLOTS: &[&[&str]] = &[
            &[
                "Noto Sans CJK TC",
                "Noto Sans CJK SC",
                "WenQuanYi Micro Hei",
            ],
            &["Noto Color Emoji", "DejaVu Sans"],
        ];

        for slot in SLOTS {
            if let Some(text) = needed_for {
                let covered = !engines.is_empty()
                    && !crate::text::glyph_layout::GlyphLayout::has_emoji_sequence(text)
                    && text.chars().all(|ch| {
                        matches!(ch, ' ' | '\t' | '\n' | '\r')
                            || crate::text::glyph_layout::is_zero_width(ch)
                            || engines.iter().any(|e| e.face.glyph_index(ch) != 0)
                    });
                if covered {
                    break;
                }
            }
            for family in *slot {
                if self.try_add_engine(engines, family, style, StyleHint::AnyStyle) {
                    break;
                }
            }
        }
    }

    /// Qt's fallbacks on Windows (`QFontEngineMulti`, `QWindowsDirectWriteFontDatabase::
    /// fallbacksForFamily`): each script item of `text` has its own list of fallback families,
    /// and a character the primary font lacks takes the first family of its item's list that has
    /// a glyph for it. The engines are loaded only as far as the text needs them, appended to
    /// `engines`, and the order of each script's list is kept in the chains the primary engine
    /// carries ([`FontEngine::with_script_chains`]).
    #[cfg(windows)]
    fn append_script_fallback_engines(
        &mut self,
        engines: &mut Vec<FontEngine>,
        text: &str,
        font: &Font,
    ) -> bool {
        use crate::text::directwrite_catalog as dw;
        use crate::text::glyph_layout::{
            ScriptChains, emoji_sequence_flags, fallback_script_key, is_zero_width,
        };
        let Some(catalog) = dw::catalog() else {
            return false;
        };
        if engines.is_empty() {
            return false;
        }
        let style = FaceStyle::of(font);
        let primary_name = if catalog.family_traits(&font.family).is_some() {
            font.family.clone()
        } else {
            family_for_style_hint(font.style_hint).to_string()
        };
        let (fixed_pitch, systems) = catalog.family_traits(&primary_name).unwrap_or((false, 0));
        let hint = if font.style_hint == StyleHint::AnyStyle && fixed_pitch {
            StyleHint::TypeWriter
        } else {
            font.style_hint
        };
        let primary = engines[0].clone();
        let mut lists: HashMap<&'static str, Vec<String>> = HashMap::new();
        let mut found: HashMap<&'static str, Vec<(usize, usize)>> = HashMap::new();
        let mut misses: HashSet<(&'static str, char)> = HashSet::new();
        let items = crate::text::shaping_plan::script_items(text);
        let emoji_flags = emoji_sequence_flags(text);
        let mut position = 0usize;
        for (n, &(start, script)) in items.iter().enumerate() {
            let end = items.get(n + 1).map_or(text.len(), |next| next.0);
            for ch in text[start..end].chars() {
                let index = position;
                position += 1;
                // A character of an emoji sequence is an item of the emoji script, which has no
                // primary font (the family must be a color font): it goes to the emoji chain even
                // where the primary font has a glyph for it.
                if matches!(ch, '\t' | '\n' | '\r')
                    || is_zero_width(ch)
                    || (primary.face.glyph_index(ch) != 0 && !emoji_flags[index])
                {
                    continue;
                }
                let key = fallback_script_key(ch, script, emoji_flags[index]);
                if !misses.insert((key, ch)) {
                    continue;
                }
                let list = lists.entry(key).or_insert_with(|| {
                    windows_fallback_families(
                        catalog,
                        &primary_name,
                        systems,
                        style.italic,
                        hint,
                        key,
                        script,
                    )
                });
                for (position, name) in list.iter().enumerate() {
                    if !catalog.covers(name, style.weight, style.italic, ch) {
                        continue;
                    }
                    let Some(index) = self.fallback_engine_index(engines, name, style) else {
                        continue;
                    };
                    if engines[index].face.glyph_index(ch) == 0 {
                        continue;
                    }
                    let chain = found.entry(key).or_default();
                    if !chain.iter().any(|&(_, i)| i == index) {
                        chain.push((position, index));
                    }
                    break;
                }
            }
        }
        let mut chains = ScriptChains::new();
        for (key, mut chain) in found {
            chain.sort_by_key(|&(position, _)| position);
            chains.insert(key, chain.into_iter().map(|(_, index)| index).collect());
        }
        engines[0] = engines[0].clone().with_script_chains(chains);
        true
    }

    /// The index in `engines` of the engine for the family `name`, loading and appending it when
    /// it is not there.
    #[cfg(windows)]
    fn fallback_engine_index(
        &mut self,
        engines: &mut Vec<FontEngine>,
        name: &str,
        style: FaceStyle,
    ) -> Option<usize> {
        let face = self.load_font_styled(name, style, StyleHint::AnyStyle)?;
        if let Some(index) = engines.iter().position(|e| Arc::ptr_eq(&e.face, &face)) {
            return Some(index);
        }
        self.try_add_engine(engines, name, style, StyleHint::AnyStyle)
            .then(|| engines.len() - 1)
    }

    fn try_add_engine(
        &mut self,
        engines: &mut Vec<FontEngine>,
        fam: &str,
        style: FaceStyle,
        hint: StyleHint,
    ) -> bool {
        match self.load_font_styled(fam, style, hint) {
            Some(font_face) => {
                if engines.iter().any(|e| Arc::ptr_eq(&e.face, &font_face)) {
                    return false;
                }
                // Reuse the engine built for this exact request so its glyph caches survive across calls
                // (Qt keeps one QFontEngine per font and caches glyphs in it). Keyed by the requested
                // family string and style, not by font: a CSS-style list such as "'Segoe UI', sans-serif"
                // has no raw data while the plain name "Segoe UI" does, and the two shape differently
                // (face metrics vs rustybuzz). A replaced font has a new Arc, so a stale entry is
                // never matched.
                let engine_key = style.cache_key(fam, hint);
                if let Some(engine) = self
                    .engines
                    .get(&engine_key)
                    .filter(|e| Arc::ptr_eq(&e.face, &font_face))
                {
                    engines.push(engine.clone());
                    return true;
                }
                let raw_data = self.get_raw_font_data_styled(fam, style, hint);
                let face_index = font_face.face_index();
                let mut engine = FontEngine::new(font_face).with_face_index(face_index);
                if let Some(raw) = raw_data {
                    engine = engine.with_raw_data(raw);
                }
                self.engines.insert(engine_key, engine.clone());
                engines.push(engine);
                true
            }
            _ => false,
        }
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
    fn preload_default_fonts(&mut self) {
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
    fn load_catalog_font(
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
    fn find_and_load_font_file(
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
                .min_by_key(|face| style.distance(face.weight, face.italic))?;
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

/// Maximum directory depth scanned below each search path (Linux font trees are nested).
const MAX_SCAN_DEPTH: usize = 4;

/// Scans font directories for `.ttf`/`.otf`/`.ttc`/`.otc` files and reads their family names.
fn scan_font_directories(roots: &[PathBuf]) -> Vec<FontFamilyInfo> {
    let mut faces = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = roots.iter().map(|root| (root.clone(), 0)).collect();
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_SCAN_DEPTH {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            let is_font = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "ttf" | "otf" | "ttc" | "otc"
                    )
                })
                .unwrap_or(false);
            if !is_font {
                continue;
            }
            let Ok(mut file) = File::open(&path) else {
                continue;
            };
            for mut face in read_faces(&mut file) {
                face.path = Some(path.clone());
                faces.push(face);
            }
        }
    }
    faces
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn read_exact_at<R: Read + Seek>(reader: &mut R, offset: u64, len: usize) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Reads family metadata for every face of an sfnt file or collection, touching only
/// the header, table directory, `name` and `post` tables.
fn read_faces<R: Read + Seek>(reader: &mut R) -> Vec<FontFamilyInfo> {
    let Some(header) = read_exact_at(reader, 0, 12) else {
        return Vec::new();
    };
    let face_offsets: Vec<u32> = if &header[0..4] == b"ttcf" {
        let count = be_u32(&header, 8).unwrap_or(0).min(256) as usize;
        match read_exact_at(reader, 12, count * 4) {
            Some(table) => (0..count).filter_map(|i| be_u32(&table, i * 4)).collect(),
            None => return Vec::new(),
        }
    } else {
        vec![0]
    };
    face_offsets
        .into_iter()
        .enumerate()
        .filter_map(|(index, offset)| read_face(reader, offset as u64, index as u32))
        .collect()
}

/// Maximum `name` table size accepted (real tables are a few KiB).
const MAX_NAME_TABLE_LEN: u32 = 1 << 20;

fn read_face<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    face_index: u32,
) -> Option<FontFamilyInfo> {
    let offset_table = read_exact_at(reader, offset, 12)?;
    let num_tables = be_u16(&offset_table, 4)? as usize;
    let records = read_exact_at(reader, offset + 12, num_tables * 16)?;
    let mut name_table = None;
    let mut post_table = None;
    let mut os2_table = None;
    for i in 0..num_tables {
        let record = &records[i * 16..i * 16 + 16];
        let table_offset = be_u32(record, 8)?;
        let table_len = be_u32(record, 12)?;
        match &record[0..4] {
            b"name" => name_table = Some((table_offset, table_len)),
            b"post" => post_table = Some((table_offset, table_len)),
            b"OS/2" => os2_table = Some((table_offset, table_len)),
            _ => {}
        }
    }
    let (name_offset, name_len) = name_table?;
    if name_len > MAX_NAME_TABLE_LEN {
        return None;
    }
    let name_data = read_exact_at(reader, name_offset as u64, name_len as usize)?;
    let family = preferred_family_name(&NameTable::parse(&name_data)?)?;
    // post table: version (4), italicAngle (4), underlinePosition (2), underlineThickness (2), isFixedPitch (4).
    let fixed_pitch = post_table
        .filter(|&(_, len)| len >= 16)
        .and_then(|(post_offset, _)| read_exact_at(reader, post_offset as u64 + 12, 4))
        .and_then(|bytes| be_u32(&bytes, 0))
        .map(|flag| flag != 0)
        .unwrap_or(false);
    // OS/2 table: usWeightClass at 4, fsSelection at 62 (bit 0 italic, bit 9 oblique).
    let (weight, italic) = os2_table
        .filter(|&(_, len)| len >= 64)
        .and_then(|(os2_offset, _)| read_exact_at(reader, os2_offset as u64, 64))
        .map(|os2| {
            let weight = be_u16(&os2, 4).unwrap_or(400);
            let selection = be_u16(&os2, 62).unwrap_or(0);
            (weight, selection & ((1 << 0) | (1 << 9)) != 0)
        })
        .unwrap_or((400, false));
    Some(FontFamilyInfo {
        family,
        path: None,
        face_index,
        fixed_pitch,
        weight,
        italic,
    })
}

/// Picks the family name, preferring the typographic family (name ID 16) over the legacy
/// family (ID 1) and US-English Windows records over other languages.
fn preferred_family_name(table: &NameTable) -> Option<String> {
    const WINDOWS_EN_US: u16 = 0x0409;
    let mut best: Option<(u32, String)> = None;
    for name in table.names {
        let id_rank = match name.name_id {
            name_id::TYPOGRAPHIC_FAMILY => 0,
            name_id::FAMILY => 1,
            _ => continue,
        };
        let lang_rank = match (name.platform_id, name.language_id) {
            (PlatformId::Windows, WINDOWS_EN_US) => 0,
            (PlatformId::Unicode, _) => 1,
            (PlatformId::Macintosh, 0) => 2,
            _ => 3,
        };
        let rank = id_rank * 4 + lang_rank;
        if best
            .as_ref()
            .is_some_and(|(best_rank, _)| *best_rank <= rank)
        {
            continue;
        }
        let text = if name.is_unicode() {
            name.to_string()
        } else if name.platform_id == PlatformId::Macintosh && name.encoding_id == 0 {
            // Mac Roman: ASCII range is identical; drop anything outside it.
            Some(
                name.name
                    .iter()
                    .filter(|b| b.is_ascii())
                    .map(|&b| b as char)
                    .collect(),
            )
        } else {
            None
        };
        if let Some(text) = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, text)| text)
}

/// The families `QWindowsFontDatabaseBase::extraTryFontsForFamily` tries for the user's language
/// (`GetUserDefaultLangID`): Chinese for China and Singapore, Chinese for the other regions,
/// Japanese, Korean, and the others' list.
#[cfg(windows)]
fn extra_try_fonts() -> &'static [&'static str] {
    use windows::Win32::Globalization::GetUserDefaultLangID;
    static LIST: std::sync::LazyLock<&'static [&'static str]> = std::sync::LazyLock::new(|| {
        // SAFETY: the call takes no arguments and only reads the user's settings.
        let lid = unsafe { GetUserDefaultLangID() };
        match lid & 0xff {
            0x04 if lid == 0x0804 || lid == 0x1004 => &[
                "SimSun",
                "Arial",
                "PMingLiU",
                "Gulim",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            0x04 => &[
                "PMingLiU",
                "Arial",
                "SimSun",
                "Gulim",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            0x11 => &[
                "Yu Gothic UI",
                "MS UI Gothic",
                "Arial",
                "Gulim",
                "SimSun",
                "PMingLiU",
                "Arial Unicode MS",
            ],
            0x12 => &[
                "Malgun Gothic",
                "Gulim",
                "Arial",
                "PMingLiU",
                "SimSun",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            _ => &[
                "Arial",
                "MS UI Gothic",
                "Gulim",
                "SimSun",
                "PMingLiU",
                "Arial Unicode MS",
            ],
        }
    });
    *LIST
}

/// `fallbacksForFamily(family, style, hint, script)` of Qt's DirectWrite font database as
/// `QFontDatabasePrivate` filters it: the emoji family for the emoji script, the family of the
/// style hint, `extraTryFontsForFamily` (the user's language's families that exist, unless the
/// family is a symbol font, then the emoji and symbol families), then every family that supports
/// the script's writing system. Entries that are not families of the database are dropped; repeats
/// stay.
#[cfg(windows)]
fn windows_fallback_families(
    catalog: &crate::text::directwrite_catalog::Catalog,
    family: &str,
    family_systems: u64,
    italic: bool,
    hint: StyleHint,
    key: &str,
    script: unicode_script::Script,
) -> Vec<String> {
    use crate::text::directwrite_catalog as dw;
    let mut list: Vec<String> = Vec::new();
    if key == "Emoji" {
        list.push("Segoe UI Emoji".to_string());
    }
    list.push(family_for_style_hint(hint).to_string());
    if !dw::supports_symbol(family_systems) {
        for name in extra_try_fonts() {
            if catalog.family_traits(name).is_some() {
                list.push((*name).to_string());
            }
        }
    }
    list.push("Segoe UI Emoji".to_string());
    list.push("Segoe UI Symbol".to_string());
    let system = if key == "Emoji" {
        0
    } else {
        dw::writing_system_for_script(script)
    };
    list.extend(catalog.families_for_writing_system(italic, system));
    let _ = family;
    list.retain(|name| catalog.family_traits(name).is_some());
    list
}

impl Default for FontDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off macOS the flag layout rounds its advances by is `uses_directwrite_engine`, whatever the
    /// hinting: `font_has_subpixel_positions` (what the line metrics follow) is `true` for a font
    /// preferring no hinting, but Windows and Linux advances are still rounded without
    /// DirectWrite, as before the CoreText engine existed.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn layout_advances_follow_directwrite_not_hinting_off_macos() {
        assert_eq!(layout_has_subpixel_advances(), uses_directwrite_engine());
        let font =
            Font::new("Arial", 16.0).with_hinting_preference(HintingPreference::PreferNoHinting);
        assert!(font_has_subpixel_positions(&font));
        assert_eq!(layout_has_subpixel_advances(), uses_directwrite_engine());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn layout_keeps_fractional_advances_on_macos() {
        assert!(layout_has_subpixel_advances());
    }

    #[test]
    fn test_font_database_creation_and_lookup() {
        let mut db = FontDatabase::new();
        let font = db.load_font("Arial").or_else(|| db.load_font("Segoe UI"));
        assert!(
            font.is_some(),
            "Windows system fonts should include Arial or Segoe UI"
        );

        let cached = db.load_font("Arial").or_else(|| db.load_font("Segoe UI"));
        assert!(cached.is_some());
        assert!(Arc::ptr_eq(&font.unwrap(), &cached.unwrap()));
    }

    #[test]
    fn test_font_database_memory_registration() {
        let mut db = FontDatabase::new();
        let path = Path::new("C:/Windows/Fonts/arial.ttf");
        if path.exists() {
            let data = Arc::new(std::fs::read(path).unwrap());
            let res = db.add_font_from_memory("CustomArial", data.clone());
            assert!(res.is_ok());

            let loaded = db.load_font("CustomArial");
            assert!(loaded.is_some());

            let raw = db.get_raw_font_data("CustomArial");
            assert!(raw.is_some());
            assert_eq!(raw.unwrap().len(), data.len());
        }
    }

    /// Candidate font files for tests that need real font data on any host.
    fn any_font_bytes() -> Option<Vec<u8>> {
        [
            "C:/Windows/Fonts/arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ]
        .iter()
        .find_map(|p| std::fs::read(p).ok())
    }

    #[test]
    fn in_memory_font_face_is_built_once_per_font_data() {
        let Some(bytes) = any_font_bytes() else {
            return;
        };
        let mut db = FontDatabase::new();
        let font = Font::new("NoSuchFamily", 12.0).with_font_data(SharedFontData::from_vec(bytes));
        let a = db.resolve_font_engines(&font);
        let b = db.resolve_font_engines(&font);
        assert!(Arc::ptr_eq(&a[0].face, &b[0].face));
    }
    #[cfg(windows)]
    #[test]
    fn test_font_database_style_hint_substitute() {
        assert!(
            Path::new("C:/Windows/Fonts/tahoma.ttf").exists()
                || Path::new("C:/Windows/Fonts/Tahoma.ttf").exists(),
            "Tahoma must be installed on Windows"
        );
        assert!(
            Path::new("C:/Windows/Fonts/arial.ttf").exists()
                || Path::new("C:/Windows/Fonts/Arial.ttf").exists(),
            "Arial must be installed on Windows"
        );
        assert!(
            Path::new("C:/Windows/Fonts/cour.ttf").exists()
                || Path::new("C:/Windows/Fonts/Cour.ttf").exists(),
            "Courier New must be installed on Windows"
        );

        let mut db = FontDatabase::new();
        let missing = "NoSuchFamilyNonExistent123";

        // AnyStyle -> Tahoma
        let face_tahoma = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::AnyStyle);
        assert!(face_tahoma.is_some());
        let tahoma_direct =
            db.load_font_styled("Tahoma", FaceStyle::regular(), StyleHint::AnyStyle);
        assert!(face_tahoma.is_some() && tahoma_direct.is_some());
        assert!(Arc::ptr_eq(&face_tahoma.unwrap(), &tahoma_direct.unwrap()));

        // Helvetica -> Arial
        let face_arial = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::Helvetica);
        let arial_direct = db.load_font_styled("Arial", FaceStyle::regular(), StyleHint::Helvetica);
        assert!(face_arial.is_some() && arial_direct.is_some());
        assert!(Arc::ptr_eq(&face_arial.unwrap(), &arial_direct.unwrap()));

        // Courier -> Courier New
        let face_cour = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::Courier);
        let cour_direct =
            db.load_font_styled("Courier New", FaceStyle::regular(), StyleHint::Courier);
        assert!(face_cour.is_some() && cour_direct.is_some());
        assert!(Arc::ptr_eq(&face_cour.unwrap(), &cour_direct.unwrap()));

        // Fonts differing only in hint do not share a cache entry
        let key_any =
            FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::AnyStyle);
        let key_helv =
            FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::Helvetica);
        assert_ne!(key_any, key_helv);
        assert!(db.cache.contains_key(&key_any));
        assert!(db.cache.contains_key(&key_helv));
        assert!(!Arc::ptr_eq(&db.cache[&key_any], &db.cache[&key_helv]));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn test_font_database_style_hint_substitute_linux() {
        let mut db = FontDatabase::new();
        let missing = "NoSuchFontXYZ";

        if !is_fontconfig_available() {
            let face = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::AnyStyle);
            assert!(
                face.is_none(),
                "When fontconfig is absent, no substitute resolves"
            );
            return;
        }

        // The substitute is the first family `FcFontSort` lists that this machine can load (which
        // family that is depends on its fontconfig configuration and installed fonts, so it is
        // derived here, not hard-coded).
        let all_hints = [
            StyleHint::AnyStyle,
            StyleHint::SansSerif,
            StyleHint::Serif,
            StyleHint::Monospace,
            StyleHint::TypeWriter,
            StyleHint::Cursive,
            StyleHint::Fantasy,
            StyleHint::System,
        ];
        for hint in all_hints {
            let candidates = query_fontconfig_fallbacks(&missing.to_ascii_lowercase(), false, hint);
            let expected = candidates
                .iter()
                .find_map(|c| db.load_font_styled(c, FaceStyle::regular(), hint))
                .expect("fontconfig lists no family this machine can load");
            let face = db
                .load_font_styled(missing, FaceStyle::regular(), hint)
                .unwrap_or_else(|| panic!("no substitute resolved for {hint:?}"));
            assert!(
                Arc::ptr_eq(&face, &expected),
                "substitute for {hint:?} must be the first listed family that loads"
            );
        }

        // Fonts that differ only in hint do not share a cache entry.
        let key_any =
            FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::AnyStyle);
        let key_serif =
            FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::Serif);
        assert_ne!(key_any, key_serif);
        assert!(db.cache.contains_key(&key_any));
        assert!(db.cache.contains_key(&key_serif));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn test_fc_family_for_style_hint_pure_mapping() {
        assert_eq!(
            fc_family_for_style_hint(StyleHint::SansSerif),
            Some("sans-serif")
        );
        assert_eq!(
            fc_family_for_style_hint(StyleHint::Helvetica),
            Some("sans-serif")
        );
        assert_eq!(fc_family_for_style_hint(StyleHint::Serif), Some("serif"));
        assert_eq!(fc_family_for_style_hint(StyleHint::Times), Some("serif"));
        assert_eq!(
            fc_family_for_style_hint(StyleHint::Monospace),
            Some("monospace")
        );
        assert_eq!(
            fc_family_for_style_hint(StyleHint::TypeWriter),
            Some("monospace")
        );
        assert_eq!(
            fc_family_for_style_hint(StyleHint::Courier),
            Some("monospace")
        );
        assert_eq!(
            fc_family_for_style_hint(StyleHint::Cursive),
            Some("cursive")
        );
        assert_eq!(
            fc_family_for_style_hint(StyleHint::Fantasy),
            Some("fantasy")
        );
        assert_eq!(fc_family_for_style_hint(StyleHint::AnyStyle), None);
        assert_eq!(fc_family_for_style_hint(StyleHint::System), None);
        assert_eq!(fc_family_for_style_hint(StyleHint::OldEnglish), None);
        assert_eq!(fc_family_for_style_hint(StyleHint::Decorative), None);

        // family_for_style_hint alias
        assert_eq!(
            family_for_style_hint(StyleHint::SansSerif),
            Some("sans-serif")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_family_for_style_hint_macos_table() {
        assert_eq!(
            family_for_style_hint(StyleHint::SansSerif),
            Some("Helvetica")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::Helvetica),
            Some("Helvetica")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::Serif),
            Some("Times New Roman")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::Times),
            Some("Times New Roman")
        );
        assert_eq!(family_for_style_hint(StyleHint::Monospace), Some("Menlo"));
        assert_eq!(
            family_for_style_hint(StyleHint::Cursive),
            Some("Apple Chancery")
        );
        assert_eq!(family_for_style_hint(StyleHint::Fantasy), Some("Zapfino"));
        assert_eq!(
            family_for_style_hint(StyleHint::TypeWriter),
            Some("American Typewriter")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::Courier),
            Some("American Typewriter")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::AnyStyle),
            Some(".AppleSystemUIFont")
        );
        assert_eq!(
            family_for_style_hint(StyleHint::System),
            Some(".AppleSystemUIFont")
        );
        assert_eq!(family_for_style_hint(StyleHint::OldEnglish), None);
        assert_eq!(family_for_style_hint(StyleHint::Decorative), None);
    }

    /// A face built from memory is shared between requests unless the backend would build a
    /// different one: on Windows DirectWrite synthesizes bold from `DemiBold` up and oblique for
    /// an italic request, so those split the cache; elsewhere every request shares one face.
    #[test]
    fn memory_face_keys_split_only_on_what_the_backend_synthesizes() {
        let data = SharedFontData::from_vec(vec![0u8; 16]);
        let hint = HintStyle::HintFull;
        let key = |weight, italic| memory_face_key(&data, hint, weight, italic);
        assert_eq!(key(400, false), key(500, false));
        assert_eq!(key(300, false), key(400, false));
        assert_eq!(key(600, false) == key(400, false), !cfg!(windows));
        assert_eq!(key(600, false), key(900, false));
        assert_eq!(key(400, true) == key(400, false), !cfg!(windows));
        assert_eq!(key(400, true), key(500, true));
        assert_eq!(key(700, true) == key(700, false), !cfg!(windows));
        // The hint style always splits, and so does another buffer.
        assert_ne!(
            key(400, false),
            memory_face_key(&data, HintStyle::HintNone, 400, false)
        );
        let other = SharedFontData::from_vec(vec![0u8; 16]);
        assert_ne!(key(400, false), memory_face_key(&other, hint, 400, false));
    }

    /// The shaper must open a collection file with the face index of the glyph face, or its glyph
    /// ids are those of another member (MS PGothic is the third face of `msgothic.ttc`, whose
    /// glyph ids differ from the first face's).
    #[cfg(windows)]
    #[test]
    fn engines_of_collection_members_shape_with_their_own_face() {
        let mut db = FontDatabase::new();
        for (family, index) in [("MS Gothic", 0), ("MS UI Gothic", 1), ("MS PGothic", 2)] {
            let engines = db.resolve_font_engines(&Font::new(family, 16.0));
            let engine = &engines[0];
            assert_eq!(engine.face_index, index, "{family}");
            let shaping = engine.shaping_face(16).expect("shaping face");
            for ch in ['i', 'A', 'あ', '日'] {
                assert_eq!(
                    shaping.face().glyph_index(ch).unwrap_or_default().0,
                    engine.face.glyph_index(ch),
                    "{family}: {ch:?}"
                );
            }
        }
    }
}
