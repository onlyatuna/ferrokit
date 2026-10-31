// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::text::font::{Font, FontStyle, SharedFontData};
use crate::text::glyph_face::{parse_face, LineMetrics, SharedGlyphFace};
use crate::text::glyph_layout::FontEngine;

use rustybuzz::ttf_parser::name::Table as NameTable;
use rustybuzz::ttf_parser::{name_id, PlatformId};

/// Weight and slant requested from the font database (`QFontDatabase::bestStyle` key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaceStyle {
    /// CSS/Qt numeric weight, 1..=1000.
    pub weight: u16,
    pub italic: bool,
}

impl FaceStyle {
    pub const REGULAR: Self = Self {
        weight: 400,
        italic: false,
    };

    /// The style `font` asks for.
    pub fn of(font: &Font) -> Self {
        Self {
            weight: font.weight as u16,
            italic: font.style != FontStyle::Normal,
        }
    }

    /// `bestStyle` distance to a face: weight difference in steps of 10, plus 0x1000 for the
    /// wrong slant. Zero is an exact match.
    fn distance(self, weight: u16, italic: bool) -> u32 {
        let w = (i32::from(self.weight) - i32::from(weight)).unsigned_abs() / 10;
        w + if self.italic != italic { 0x1000 } else { 0 }
    }

    /// Cache key of `family` at this style; the regular style keeps the plain family key.
    fn cache_key(self, family: &str) -> String {
        if self == Self::REGULAR {
            family.to_string()
        } else {
            format!("{family}\u{1}{}{}", self.weight, u8::from(self.italic))
        }
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
        ("microsoft jhenghei" | "msjh", true, _) => &["msjhbd.ttc"],
        ("microsoft yahei" | "msyh", true, _) => &["msyhbd.ttc"],
        (
            "segoe ui" | "arial" | "consolas" | "tahoma" | "microsoft jhenghei" | "msjh"
            | "microsoft yahei" | "msyh" | "segoe ui symbol" | "segoe ui emoji" | "ms gothic",
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
    memory_faces: Vec<(
        (usize, usize),
        std::sync::Weak<dyn crate::text::glyph_face::GlyphFace>,
    )>,
}

/// Bumped whenever the set of available fonts changes, so caches derived from font data
/// (e.g. text advance widths) know to drop their entries.
static FONT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Current font-set generation; changes whenever a search path or an in-memory font is added.
pub fn font_generation() -> u64 {
    FONT_GENERATION.load(Ordering::Relaxed)
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

/// Whether text is laid out as Qt's DirectWrite font engine lays it out rather than as its GDI
/// one: any application device pixel ratio other than 1 (`qFuzzyCompare`). In the source,
/// `useDirectWrite` (qwindowsfontdatabase.cpp:42-61) picks DirectWrite for a ratio other than 1 with
/// the default hinting preference; that at a ratio of 1 the engine is still DirectWrite, with full
/// hinting, was observed (PySide6 6.11.2 under `QT_SCALE_FACTOR=0.8` on a 125 % display), not read
/// from the source. A real GDI engine (`windows:nodirectwrite`) differs again, e.g. in
/// `averageCharWidth` (`tmAveCharWidth`), and is not modelled.
pub fn uses_directwrite_engine() -> bool {
    let dpr = f64::from(application_device_pixel_ratio());
    (dpr - 1.0).abs() * 1_000_000_000_000.0 > dpr.min(1.0).abs()
}

/// `QFontEngine::supportsSubPixelPositions` of the engine Qt lays text out with, which decides
/// whether its line metrics stay fractional or are rounded to whole pixels
/// ([`LineMetrics::rounded`]). Windows: DirectWrite positions glyphs at sub-pixel offsets unless
/// it hints fully, which is what a ratio of 1 gives (`determineHinting`,
/// qwindowsfontenginedirectwrite.cpp:167-183; measured: 12.5 + 3.5 at 1.25, 13 + 4 with a leading
/// of -1 at 1). macOS: CoreText always does (qfontengine_coretext_p.h:60).
///
/// **Known deviation (Linux and other FreeType platforms):** Qt's FreeType engine is sub-pixel
/// only for the light or no hint style (`default_hint_style`, qfontengine_ft_p.h:170-179), which
/// comes from fontconfig and the platform theme; ferrokit has no such input, so FreeType follows
/// the DirectWrite rule here (fractional metrics at a ratio other than 1, as the advances do). A
/// Linux desktop with `hintslight` (the common default) is therefore rounded at a ratio of 1
/// where Qt is not. [inferred]
pub fn engine_has_subpixel_positions() -> bool {
    cfg!(target_os = "macos") || uses_directwrite_engine()
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
        db.load_font_styled(&font.family, FaceStyle::of(font))
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
        };

        db.preload_default_fonts();
        db
    }

    /// Adds a directory to the font search paths.
    pub fn add_search_path<P: AsRef<Path>>(&mut self, path: P) {
        self.search_paths.push(path.as_ref().to_path_buf());
        self.system_faces = None;
        FONT_GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    /// Registers a custom font from memory.
    pub fn add_font_from_memory(
        &mut self,
        family: &str,
        data: impl Into<SharedFontData>,
    ) -> Result<SharedGlyphFace, String> {
        let shared = data.into();
        let font_arc =
            parse_face(&shared, 0).map_err(|e| format!("Failed to parse font from memory: {e}"))?;
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
        self.load_font_styled(family, FaceStyle::REGULAR)
    }

    /// Loads the face of `family` nearest to `style` (`QFontDatabase::bestStyle`), caching the result.
    /// A family without a face for that style yields its nearest one (e.g. regular for bold).
    pub fn load_font_styled(&mut self, family: &str, style: FaceStyle) -> Option<SharedGlyphFace> {
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
                if let Some(f) = self.load_font_styled(clean, style) {
                    return Some(f);
                }
            }
        }

        let key = family.to_ascii_lowercase();
        let style_key = style.cache_key(&key);
        if let Some(font) = self.cache.get(&style_key) {
            return Some(Arc::clone(font));
        }

        if let Some((raw_data, face)) = self.find_and_load_font_file(&key, style) {
            self.cache.insert(style_key.clone(), Arc::clone(&face));
            self.raw_cache.insert(style_key, raw_data);
            return Some(face);
        }

        // An in-memory (application) font has a single face: use it for any style.
        if style != FaceStyle::REGULAR {
            if let Some(face) = self.cache.get(&key).cloned() {
                self.cache.insert(style_key.clone(), Arc::clone(&face));
                if let Some(raw) = self.raw_cache.get(&key).cloned() {
                    self.raw_cache.insert(style_key, raw);
                }
                return Some(face);
            }
        }

        if key != "segoe ui" && key != "arial" {
            if let Some(fallback) = self
                .load_font_styled("segoe ui", style)
                .or_else(|| self.load_font_styled("arial", style))
            {
                return Some(fallback);
            }
        }

        None
    }

    /// Returns the raw binary font data of the regular face for OpenType shaping.
    pub fn get_raw_font_data(&mut self, family: &str) -> Option<SharedFontData> {
        self.get_raw_font_data_styled(family, FaceStyle::REGULAR)
    }

    /// Returns the raw binary font data of the face [`load_font_styled`](Self::load_font_styled) picks.
    pub fn get_raw_font_data_styled(
        &mut self,
        family: &str,
        style: FaceStyle,
    ) -> Option<SharedFontData> {
        let key = style.cache_key(&family.to_ascii_lowercase());
        if let Some(data) = self.raw_cache.get(&key) {
            return Some(data.clone());
        }
        self.load_font_styled(family, style);
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
        self.append_fallback_engines(&mut engines, Some(text), FaceStyle::of(font));
        engines
    }

    /// The font requested by `font` alone (empty when the family cannot be loaded).
    fn resolve_primary_engine(&mut self, font: &Font) -> Vec<FontEngine> {
        let mut engines: Vec<FontEngine> = Vec::with_capacity(4);

        // 1. Primary in-memory font data if specified
        if let Some(shared) = &font.font_data {
            if let Some(face) = self.memory_face(shared) {
                engines.push(
                    FontEngine::new(face)
                        .with_raw_data(shared.clone())
                        .with_face_index(0),
                );
            }
        }

        // If no in-memory font or in-memory font failed to parse, load primary by family
        if engines.is_empty() {
            self.try_add_engine(&mut engines, &font.family, FaceStyle::of(font));
        }
        engines
    }

    /// The face built from in-memory font data, parsed once per buffer and remembered (see
    /// `memory_faces`); `None` when the data does not parse.
    fn memory_face(&mut self, shared: &SharedFontData) -> Option<SharedGlyphFace> {
        self.memory_faces.retain(|(_, f)| f.strong_count() > 0);
        let key = (shared.as_slice().as_ptr() as usize, shared.len());
        let cached = self
            .memory_faces
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, f)| f.upgrade());
        let face = match cached {
            Some(face) => Some(face),
            None => parse_face(shared, 0).ok().inspect(|face| {
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
        if let Some(shared) = &font.font_data {
            if let Some(face) = self.memory_face(shared) {
                return Some(face);
            }
        }
        self.load_font_styled(&font.family, FaceStyle::of(font))
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
                if self.try_add_engine(engines, family, style) {
                    break;
                }
            }
        }
    }

    fn try_add_engine(
        &mut self,
        engines: &mut Vec<FontEngine>,
        fam: &str,
        style: FaceStyle,
    ) -> bool {
        if let Some(font_face) = self.load_font_styled(fam, style) {
            if engines.iter().any(|e| Arc::ptr_eq(&e.face, &font_face)) {
                return false;
            }
            // Reuse the engine built for this exact request so its glyph caches survive across calls
            // (Qt keeps one QFontEngine per font and caches glyphs in it). Keyed by the requested
            // family string and style, not by font: a CSS-style list such as "'Segoe UI', sans-serif"
            // has no raw data while the plain name "Segoe UI" does, and the two shape differently
            // (face metrics vs rustybuzz). A replaced font has a new Arc, so a stale entry is
            // never matched.
            let engine_key = style.cache_key(fam);
            if let Some(engine) = self
                .engines
                .get(&engine_key)
                .filter(|e| Arc::ptr_eq(&e.face, &font_face))
            {
                engines.push(engine.clone());
                return true;
            }
            let raw_data = self.get_raw_font_data_styled(fam, style);
            let mut engine = FontEngine::new(font_face).with_face_index(0);
            if let Some(raw) = raw_data {
                engine = engine.with_raw_data(raw);
            }
            self.engines.insert(engine_key, engine.clone());
            engines.push(engine);
            true
        } else {
            false
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
            if let Some((raw_data, face)) = self.find_and_load_font_file(fam, FaceStyle::REGULAR) {
                self.cache.insert((*fam).to_string(), face);
                self.raw_cache.insert((*fam).to_string(), raw_data);
                break;
            }
        }
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
        let candidate_filenames: Vec<String> = if style == FaceStyle::REGULAR {
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
                    if let Some(shared) = self.file_cache.get(&canonical) {
                        if let Ok(face) = parse_face(shared, 0) {
                            return Some((shared.clone(), face));
                        }
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
                        if let Ok(face) = parse_face(&shared, 0) {
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
            let face = parse_face(shared, face_index).ok()?;
            return Some((shared.clone(), face));
        }

        let _t = crate::startup_trace::span(|| {
            format!("map+parse (family index) {}", canonical.display())
        });
        let shared = SharedFontData::from_file(&canonical).ok()?;
        let face = parse_face(&shared, face_index).ok()?;
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

impl Default for FontDatabase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
