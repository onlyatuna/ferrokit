// SPDX-License-Identifier: LGPL-3.0-only
//! The font families Qt's DirectWrite font database knows, built the way it builds them
//! (`QWindowsDirectWriteFontDatabase::populateFontDatabase` and `populateFamily`,
//! qwindowsdirectwritefontdatabase.cpp, with `qt_registerFont` of qfontdatabase.cpp).
//!
//! DirectWrite's system font collection in the typographic family model holds one family per
//! typographic family name ("Segoe UI") with every face of it (Light, Semibold, ...). Qt registers
//! each family under the names of its fonts: the typographic family name, the family name of the
//! font in the default locale and in English, and the GDI-compatible (`WIN32`) family name, which
//! is how "Segoe UI Semibold", "Arial Narrow" or "Calibri Light" become families of their own.
//! Each name's faces are registered with the style name, weight, slant and stretch DirectWrite
//! reports, and the writing systems the face's `OS/2` table claims. A request for a family name is
//! answered from that registry by `QtFontStyle` distance (`bestStyle`).
//!
//! Not modelled: the raster ("bitmap") fonts Qt adds through GDI (`populateBitmapFonts`), and
//! fonts that DirectWrite does not read from a local file.

use crate::text::directwrite_face::{factory6, serialized};
use crate::text::font::StyleHint;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use unicode_script::{Script, UnicodeScript};
use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, Interface, PCWSTR};

/// `QFont::Style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StyleKind {
    Normal,
    Italic,
    Oblique,
}

/// Where a registered face can be found again: the family of the system collection and the
/// position in the font list `GetMatchingFonts` returns for it.
#[derive(Clone, Copy, Debug)]
struct FaceHandle {
    family: u32,
    font: u32,
}

/// A style of a family (`QtFontStyle`): the key Qt matches a request against and the face.
#[derive(Clone, Debug)]
pub(crate) struct CatalogStyle {
    pub style_name: String,
    /// `QFont::Weight`, the DirectWrite weight (1..=999).
    pub weight: u16,
    pub kind: StyleKind,
    /// `QFont::Stretch` (100 unstretched, 0 for a stretch Qt has no name for).
    pub stretch: u16,
    handle: FaceHandle,
}

/// A family of the registry (`QtFontFamily`).
#[derive(Clone, Debug, Default)]
pub(crate) struct CatalogFamily {
    pub name: String,
    pub styles: Vec<CatalogStyle>,
    /// `QFontDatabase::WritingSystem` bits the family supports.
    pub writing_systems: u64,
    pub color: bool,
    pub fixed_pitch: bool,
    /// `QtFontFamily::populated`: a font was registered under the name.
    pub populated: bool,
}

/// What it takes to open a registered face.
#[derive(Clone, Debug)]
pub(crate) struct FaceSource {
    pub path: PathBuf,
    pub face_index: u32,
    /// The variation axes of a named instance of a variable font (`DWRITE_FONT_AXIS_TAG`, a
    /// little-endian tag, and its value); empty for a static font.
    pub axes: Vec<(u32, f32)>,
}

/// Everything the fallback family list of a script item depends on besides the registry itself
/// (`fallbacksForFamily(family, style, hint, script)` as `font_database::windows_fallback_families`
/// builds it): the style hint, whether the slant asked for is italic, whether the primary family
/// is a symbol font, whether the item is an emoji item, and the item script's writing system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FallbackListKey {
    pub hint: StyleHint,
    pub italic: bool,
    pub symbol_family: bool,
    pub emoji: bool,
    pub writing_system: usize,
}

pub(crate) struct Catalog {
    collection: IDWriteFontCollection2,
    locales: Locales,
    /// The family of the collection that owns a name (`m_populatedFonts`).
    owners: HashMap<String, u32>,
    /// The families registered so far, sorted by name, case insensitively
    /// (`QFontDatabasePrivate::family`); a family's styles appear when it is populated.
    registry: Mutex<Registry>,
    /// The fallback lists built so far (`QFontDatabasePrivate::fallbacksCache`). The registry is
    /// fixed once every family is populated, which the first list of any key does, and nothing
    /// outside this catalog enters it (application fonts are registered with the font database
    /// only), so a list never goes stale. Never held while a list is computed.
    fallback_lists: Mutex<HashMap<FallbackListKey, Arc<[String]>>>,
}

// SAFETY: `Send` requires that the value may move to another thread. `collection` is a
// DirectWrite object, which is not bound to its creating thread (see `Factory` in
// `directwrite_face.rs`), and every call on it is made under `DWRITE_LOCK` (`serialized`).
// The other fields are plain owned data.
unsafe impl Send for Catalog {}
// SAFETY: `Sync` requires that `&Catalog` may be used from several threads: `collection` is only
// used inside `serialized`, so DirectWrite never sees two threads at once; `registry` is behind a
// `Mutex`, and the rest is never mutated after `build` returns.
unsafe impl Sync for Catalog {}

static CATALOG: LazyLock<Option<Catalog>> = LazyLock::new(build);

/// The registry of the system's fonts, built on first use; `None` without DirectWrite.
pub(crate) fn catalog() -> Option<&'static Catalog> {
    CATALOG.as_ref()
}

impl Catalog {
    /// Populates the family `name` (once) the way `ensurePopulated` does, with the registry locked.
    fn ensure_populated(&self, registry: &mut Registry, name: &str) {
        let Some(index) = registry.find(name) else {
            return;
        };
        if registry.families[index].populated {
            return;
        }
        let spelling = registry.families[index].name.clone();
        if let Some(&owner) = self.owners.get(&spelling)
            && registry.populated_names.insert(spelling.clone())
        {
            serialized(|| populate(registry, &self.collection, owner, &spelling, &self.locales));
        }
    }

    /// The style of the family `name` a request of `weight` that is `italic` or not gets: the
    /// family is populated the first time it is asked for, then the style with the least distance
    /// wins (`bestStyle`, the first one at a tie, in registration order).
    fn pick_style(&self, name: &str, weight: u16, italic: bool) -> Option<CatalogStyle> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        self.ensure_populated(&mut registry, name);
        let family = &registry.families[registry.find(name)?];
        if !family.populated {
            return None;
        }
        best_style(family, weight, italic).cloned()
    }

    /// The face for a request for the family `name` of `weight` that is `italic` or not.
    pub(crate) fn resolve(&self, name: &str, weight: u16, italic: bool) -> Option<FaceSource> {
        let style = self.pick_style(name, weight, italic)?;
        self.face_source(&style)
    }

    /// Whether a family called `name` has a font registered, and its `(fixed pitch, writing
    /// systems)`.
    pub(crate) fn family_traits(&self, name: &str) -> Option<(bool, u64)> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        self.ensure_populated(&mut registry, name);
        let family = &registry.families[registry.find(name)?];
        family
            .populated
            .then_some((family.fixed_pitch, family.writing_systems))
    }

    /// Whether the face a request of `weight`/`italic` gets from the family `name` has a glyph for
    /// `ch` (`IDWriteFont::HasCharacter`, what the engine's `glyphIndex` finds).
    pub(crate) fn covers(&self, name: &str, weight: u16, italic: bool, ch: char) -> bool {
        let Some(style) = self.pick_style(name, weight, italic) else {
            return false;
        };
        serialized(|| {
            let font = self.font_of(&style)?;
            // SAFETY: `font` is a live `IDWriteFont`; the call takes a code point.
            let has = unsafe { font.HasCharacter(ch as u32) };
            Some(has.is_ok_and(|b| b.as_bool()))
        })
        .unwrap_or(false)
    }

    /// `QPlatformFontDatabase::fallbacksForFamily`'s own list: every family that supports
    /// `writing_system` (any for 0), those with a style of the requested slant first
    /// (`Preferred`), the others after them (`Other`), each in the order of the database. Every
    /// family is populated first, as the loop of that function does.
    ///
    /// Built once per `(italic, writing_system)`: the first call populates every family, after
    /// which `populate` has nothing left to do (a family either has styles or has had its turn),
    /// so the registry, and with it the list, is the same on every later call.
    pub(crate) fn families_for_writing_system(
        &self,
        italic: bool,
        writing_system: usize,
    ) -> Arc<[String]> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = registry.writing_system_lists.get(&(italic, writing_system)) {
            return list.clone();
        }
        let list: Arc<[String]> = self
            .scan_writing_system(&mut registry, italic, writing_system)
            .into();
        registry
            .writing_system_lists
            .insert((italic, writing_system), list.clone());
        list
    }

    /// [`families_for_writing_system`](Self::families_for_writing_system) without the memo.
    fn scan_writing_system(
        &self,
        registry: &mut Registry,
        italic: bool,
        writing_system: usize,
    ) -> Vec<String> {
        let names: Vec<String> = registry.families.iter().map(|f| f.name.clone()).collect();
        for name in &names {
            self.ensure_populated(registry, name);
        }
        let (mut preferred, mut other) = (Vec::new(), Vec::new());
        for family in registry.families.iter().filter(|f| f.populated) {
            if writing_system != ws::ANY
                && !supports_writing_system(family.writing_systems, writing_system)
            {
                continue;
            }
            if family
                .styles
                .iter()
                .any(|s| (s.kind != StyleKind::Normal) == italic)
            {
                preferred.push(family.name.clone());
            } else {
                other.push(family.name.clone());
            }
        }
        preferred.extend(other);
        preferred
    }

    /// The fallback list of `key`, from `compute` the first time it is asked for. `compute` runs
    /// without any lock of the catalog held; two threads asking at once both compute the same
    /// list and the first one stored wins.
    pub(crate) fn fallback_list(
        &self,
        key: FallbackListKey,
        compute: impl FnOnce() -> Vec<String>,
    ) -> Arc<[String]> {
        let cached = {
            let lists = self
                .fallback_lists
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            lists.get(&key).cloned()
        };
        if let Some(list) = cached {
            return list;
        }
        let list: Arc<[String]> = compute().into();
        let mut lists = self
            .fallback_lists
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        lists.entry(key).or_insert(list).clone()
    }

    /// The font of `style` (the caller holds the DirectWrite lock).
    fn font_of(&self, style: &CatalogStyle) -> Option<IDWriteFont> {
        // SAFETY: `self.collection` is a live collection; the index comes from this collection's
        // own enumeration (`build`).
        let family = unsafe { self.collection.GetFontFamily(style.handle.family) }.ok()?;
        matching_fonts(&family).and_then(|list|
            // SAFETY: `list` is live and the index is the position this list had at `build`.
            unsafe { list.GetFont(style.handle.font) }.ok())
    }

    /// The file, face index and variation axes of `style`.
    fn face_source(&self, style: &CatalogStyle) -> Option<FaceSource> {
        serialized(|| {
            let font = self.font_of(style)?;
            // SAFETY: `font` is a live `IDWriteFont`; the call takes no arguments.
            let face = unsafe { font.CreateFontFace() }.ok()?;
            let path = font_file_path(&face)?;
            // SAFETY: `face` is a live font face; the call takes no arguments.
            let face_index = unsafe { face.GetIndex() };
            Some(FaceSource {
                path,
                face_index,
                axes: axis_values(&face),
            })
        })
    }
}

/// Whether the writing system bits `systems` include `writing_system`, or for Chinese and
/// Japanese any of the three up to Japanese (`familySupportsWritingSystem`).
fn supports_writing_system(systems: u64, writing_system: usize) -> bool {
    let last = if (ws::SIMPLIFIED_CHINESE..=ws::JAPANESE).contains(&writing_system) {
        ws::JAPANESE
    } else {
        writing_system
    };
    (writing_system..=last).any(|system| systems & (1 << system) != 0)
}

/// `qt_writing_system_for_script`: the first writing system of `script`, `Any` (0) for the
/// scripts none is for.
pub(crate) fn writing_system_for_script(script: Script) -> usize {
    SCRIPT_FOR_WRITING_SYSTEM
        .iter()
        .position(|&candidate| candidate == script)
        .unwrap_or(ws::ANY)
}

/// `QFontDatabase::writingSystems(family).contains(Symbol)` for the bits of a family.
pub(crate) fn supports_symbol(systems: u64) -> bool {
    systems & (1 << ws::SYMBOL) != 0
}

/// `bestStyle` for a request of `weight` that is `italic` or not, whatever the stretch.
fn best_style(family: &CatalogFamily, weight: u16, italic: bool) -> Option<&CatalogStyle> {
    let mut best: Option<(&CatalogStyle, i32)> = None;
    for style in &family.styles {
        let mut distance = ((i32::from(weight) - i32::from(style.weight)) / 10).abs();
        if (style.kind != StyleKind::Normal) != italic {
            // One is italic and the other upright (the oblique-against-italic case, which adds 1
            // in Qt, cannot arise: the request is only italic or not).
            distance += 0x1000;
        }
        if best.is_none_or(|(_, d)| distance < d) {
            best = Some((style, distance));
        }
    }
    best.map(|(style, _)| style)
}

/// `GetMatchingFonts(REGULAR, NORMAL, NORMAL)` of the family, the list Qt registers a family from.
fn matching_fonts(family: &IDWriteFontFamily2) -> Option<IDWriteFontList> {
    let family: &IDWriteFontFamily = family;
    // SAFETY: `family` is a live family; the arguments are plain enum values.
    unsafe {
        family.GetMatchingFonts(
            DWRITE_FONT_WEIGHT_REGULAR,
            DWRITE_FONT_STRETCH_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
        )
    }
    .ok()
}

/// The path of the local file `face` was read from.
fn font_file_path(face: &IDWriteFontFace) -> Option<PathBuf> {
    let mut count = 0u32;
    // SAFETY: `count` is a live local that receives the number of files; no array is passed.
    unsafe { face.GetFiles(&mut count, None) }.ok()?;
    let mut files: Vec<Option<IDWriteFontFile>> = vec![None; count as usize];
    // SAFETY: `files` holds `count` slots, which is what `GetFiles` is told to fill; `count` is a
    // live local.
    unsafe { face.GetFiles(&mut count, Some(files.as_mut_ptr())) }.ok()?;
    let file = files.into_iter().next()??;
    let mut key = std::ptr::null_mut();
    let mut key_size = 0u32;
    // SAFETY: `key` and `key_size` are live locals that receive the reference key; the key stays
    // owned by `file`, which outlives its use below.
    unsafe { file.GetReferenceKey(&mut key, &mut key_size) }.ok()?;
    // SAFETY: `file` is a live font file; the call takes no arguments.
    let loader = unsafe { file.GetLoader() }.ok()?;
    let local: IDWriteLocalFontFileLoader = loader.cast().ok()?;
    // SAFETY: `key` and `key_size` are what `GetReferenceKey` returned for this file.
    let length = unsafe { local.GetFilePathLengthFromKey(key, key_size) }.ok()?;
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: as above, and `buffer` has room for the path and its terminator.
    unsafe { local.GetFilePathFromKey(key, key_size, &mut buffer) }.ok()?;
    buffer.truncate(length as usize);
    Some(PathBuf::from(
        <std::ffi::OsString as std::os::windows::ffi::OsStringExt>::from_wide(&buffer),
    ))
}

/// The axis values of a face of a variable font (a named instance), empty for any other.
fn axis_values(face: &IDWriteFontFace) -> Vec<(u32, f32)> {
    let Ok(face5) = face.cast::<IDWriteFontFace5>() else {
        return Vec::new();
    };
    // SAFETY: `face5` is a live interface of a live face; the call takes no arguments.
    let count = unsafe { face5.GetFontAxisValueCount() } as usize;
    let mut values = vec![DWRITE_FONT_AXIS_VALUE::default(); count];
    // SAFETY: `values` has `count` slots, the count the face just reported.
    if count == 0 || unsafe { face5.GetFontAxisValues(&mut values) }.is_err() {
        return Vec::new();
    }
    values.iter().map(|v| (v.axisTag.0, v.value)).collect()
}

/// The string of `names` for `locale` (a NUL-terminated UTF-16 locale name), or empty
/// (`QWindowsDirectWriteFontDatabase::localeString`).
fn locale_string(names: &IDWriteLocalizedStrings, locale: &[u16]) -> String {
    let mut index = 0u32;
    let mut exists = BOOL(0);
    // SAFETY: `locale` is NUL terminated; `index` and `exists` are live locals used as
    // out-pointers.
    let found =
        unsafe { names.FindLocaleName(PCWSTR(locale.as_ptr()), &mut index, &mut exists) }.is_ok();
    if !found || !exists.as_bool() {
        return String::new();
    }
    // SAFETY: `index` was just returned by `FindLocaleName` for these strings.
    let Ok(length) = (unsafe { names.GetStringLength(index) }) else {
        return String::new();
    };
    if length == 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; length as usize + 1];
    // SAFETY: `buffer` has room for the string and its terminator, as `GetString` requires.
    if unsafe { names.GetString(index, &mut buffer) }.is_err() {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..length as usize])
}

/// A font's informational string `id` as strings in the default locale and in English.
fn informational(
    font: &IDWriteFont,
    id: DWRITE_INFORMATIONAL_STRING_ID,
    locales: &Locales,
) -> Option<(String, String)> {
    let mut names = None;
    let mut exists = BOOL(0);
    // SAFETY: `names` and `exists` are live locals used as out-pointers.
    unsafe { font.GetInformationalStrings(id, &mut names, &mut exists) }.ok()?;
    let names = names.filter(|_| exists.as_bool())?;
    Some(locales.both(&names))
}

/// The two locales Qt registers names for.
struct Locales {
    default: Option<Vec<u16>>,
    english: Vec<u16>,
}

/// The language of the system as a HarfBuzz language (Qt passes `QLocale().bcp47Name()`; the
/// Windows locale name of the user gives the same OpenType language tags, e.g. `zh-TW`).
pub(crate) fn system_language() -> Option<rustybuzz::Language> {
    static LANGUAGE: LazyLock<Option<rustybuzz::Language>> = LazyLock::new(|| {
        let locale = Locales::new().default?;
        let name = String::from_utf16_lossy(&locale);
        name.trim_end_matches('\0').parse().ok()
    });
    LANGUAGE.clone()
}

impl Locales {
    fn new() -> Self {
        let mut buffer = [0u16; 85];
        // SAFETY: `buffer` has the `LOCALE_NAME_MAX_LENGTH` (85) elements the call may write.
        let length = unsafe { GetUserDefaultLocaleName(&mut buffer) };
        Self {
            default: (length > 0).then(|| buffer[..length as usize].to_vec()),
            english: "en-us\0".encode_utf16().collect(),
        }
    }

    /// `(default locale string, English string)` of `names`.
    fn both(&self, names: &IDWriteLocalizedStrings) -> (String, String) {
        (
            self.default
                .as_deref()
                .map_or_else(String::new, |locale| locale_string(names, locale)),
            locale_string(names, &self.english),
        )
    }
}

/// `QFont::Stretch` for a DirectWrite stretch.
fn stretch_of(stretch: DWRITE_FONT_STRETCH) -> u16 {
    match stretch {
        DWRITE_FONT_STRETCH_ULTRA_CONDENSED => 50,
        DWRITE_FONT_STRETCH_EXTRA_CONDENSED => 62,
        DWRITE_FONT_STRETCH_CONDENSED => 75,
        DWRITE_FONT_STRETCH_SEMI_CONDENSED => 87,
        DWRITE_FONT_STRETCH_NORMAL => 100,
        DWRITE_FONT_STRETCH_SEMI_EXPANDED => 112,
        DWRITE_FONT_STRETCH_EXPANDED => 125,
        DWRITE_FONT_STRETCH_EXTRA_EXPANDED => 150,
        DWRITE_FONT_STRETCH_ULTRA_EXPANDED => 200,
        _ => 0,
    }
}

fn kind_of(style: DWRITE_FONT_STYLE) -> StyleKind {
    match style {
        DWRITE_FONT_STYLE_OBLIQUE => StyleKind::Oblique,
        DWRITE_FONT_STYLE_ITALIC => StyleKind::Italic,
        _ => StyleKind::Normal,
    }
}

/// What `registerFont` is given.
struct Registration {
    family: String,
    style_name: String,
    weight: u16,
    kind: StyleKind,
    stretch: u16,
    fixed_pitch: bool,
    color: bool,
    writing_systems: u64,
    handle: FaceHandle,
}

/// The registry while it is built: families sorted by lower-cased name.
#[derive(Default)]
struct Registry {
    families: Vec<CatalogFamily>,
    /// The names `populate` has run for.
    populated_names: HashSet<String>,
    /// `families_for_writing_system` per `(italic, writing system)`.
    writing_system_lists: HashMap<(bool, usize), Arc<[String]>>,
}

impl Registry {
    /// The family called `name`, case insensitively, if one was registered.
    fn find(&self, name: &str) -> Option<usize> {
        let key = name.to_lowercase();
        self.families
            .binary_search_by(|f| f.name.to_lowercase().cmp(&key))
            .ok()
    }

    /// `QFontDatabasePrivate::family(name, EnsureCreated)`.
    fn family_index(&mut self, name: &str) -> usize {
        let key = name.to_lowercase();
        match self
            .families
            .binary_search_by(|f| f.name.to_lowercase().cmp(&key))
        {
            Ok(index) => index,
            Err(index) => {
                self.families.insert(
                    index,
                    CatalogFamily {
                        name: name.to_string(),
                        ..CatalogFamily::default()
                    },
                );
                index
            }
        }
    }

    /// `qt_registerFont`: a style with the same key and style name keeps its place and gets the
    /// new face.
    fn register(&mut self, registration: Registration) {
        let index = self.family_index(&registration.family);
        let family = &mut self.families[index];
        family.fixed_pitch = registration.fixed_pitch;
        family.color = registration.color;
        family.writing_systems |= registration.writing_systems;
        family.populated = true;
        let style = CatalogStyle {
            style_name: registration.style_name,
            weight: registration.weight,
            kind: registration.kind,
            stretch: registration.stretch,
            handle: registration.handle,
        };
        match family.styles.iter_mut().find(|s| {
            s.style_name == style.style_name
                && s.weight == style.weight
                && s.kind == style.kind
                && s.stretch == style.stretch
        }) {
            Some(existing) => *existing = style,
            None => family.styles.push(style),
        }
    }
}

/// `populateFontDatabase` followed by `populateFamily` of every family, in the order Qt's
/// fallback search does it (`QPlatformFontDatabase::fallbacksForFamily`).
fn build() -> Option<Catalog> {
    let factory = factory6()?;
    serialized(|| {
        // SAFETY: `factory` is a live `IDWriteFactory6`; the arguments are plain values.
        let collection =
            unsafe { factory.GetSystemFontCollection(false, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC) }
                .ok()?;
        let locales = Locales::new();
        let mut registry = Registry::default();
        // The family of the collection that owns a name (`m_populatedFonts`).
        let mut owners: HashMap<String, u32> = HashMap::new();
        let mut registered: HashSet<String> = HashSet::new();

        // SAFETY: `collection` is live; the call takes no arguments.
        let count = unsafe { collection.GetFontFamilyCount() };
        for index in 0..count {
            // SAFETY: `index` is below the family count just read.
            let Ok(family) = (unsafe { collection.GetFontFamily(index) }) else {
                continue;
            };
            let mut register_family = |name: &str| {
                if name.is_empty() || !registered.insert(name.to_string()) {
                    return;
                }
                registry.family_index(name);
                owners.insert(name.to_string(), index);
            };
            // SAFETY: `family` is a live family; the call takes no arguments.
            if let Ok(names) = unsafe { family.GetFamilyNames() } {
                let (default, english) = locales.both(&names);
                register_family(&default);
                register_family(&english);
            }
            // SAFETY: as above.
            let fonts = unsafe { family.GetFontCount() };
            for font_index in 0..fonts {
                // SAFETY: `font_index` is below the font count just read.
                let Ok(font) = (unsafe { family.GetFont(font_index) }) else {
                    continue;
                };
                // `collectAdditionalNames`: GDI family, typographic family.
                let font: &IDWriteFont = &font;
                for id in [
                    DWRITE_INFORMATIONAL_STRING_WIN32_FAMILY_NAMES,
                    DWRITE_INFORMATIONAL_STRING_TYPOGRAPHIC_FAMILY_NAMES,
                ] {
                    if let Some((default, english)) = informational(font, id, &locales) {
                        register_family(&english);
                        register_family(&default);
                    }
                }
            }
        }

        Some(Catalog {
            collection,
            locales,
            owners,
            registry: Mutex::new(registry),
            fallback_lists: Mutex::new(HashMap::new()),
        })
    })
}

/// `populateFamily(familyName)`: registers the fonts of the owning typographic family under all
/// their names.
fn populate(
    registry: &mut Registry,
    collection: &IDWriteFontCollection2,
    owner: u32,
    family_name: &str,
    locales: &Locales,
) {
    // SAFETY: `owner` is an index this collection gave out while it was enumerated.
    let Ok(family) = (unsafe { collection.GetFontFamily(owner) }) else {
        return;
    };
    let Some(list) = matching_fonts(&family) else {
        return;
    };
    // SAFETY: `list` is live; the call takes no arguments.
    let fonts = unsafe { list.GetFontCount() };
    for position in 0..fonts {
        // SAFETY: `position` is below the font count just read.
        let Ok(font) = (unsafe { list.GetFont(position) }) else {
            continue;
        };
        let Ok(font2) = font.cast::<IDWriteFont2>() else {
            continue;
        };
        let (mut default_family, mut english_family) = (String::new(), String::new());
        let (mut default_gdi, mut english_gdi) = (String::new(), String::new());
        // SAFETY: `font2` is a live font; the call takes no arguments.
        if let Ok(font_family) = unsafe { font2.GetFontFamily() }
            // SAFETY: `font_family` is live; the call takes no arguments.
            && let Ok(names) = unsafe { font_family.GetFamilyNames() }
        {
            (default_family, english_family) = locales.both(&names);
        }
        if let Some(names) = informational(
            &font,
            DWRITE_INFORMATIONAL_STRING_WIN32_FAMILY_NAMES,
            locales,
        ) {
            (default_gdi, english_gdi) = names;
        }
        if default_family.is_empty()
            && english_family.is_empty()
            && english_gdi.is_empty()
            && default_gdi.is_empty()
        {
            english_family = family_name.to_string();
        }
        // SAFETY: `font2` is a live font; the calls take no arguments.
        let (weight, kind, stretch, fixed_pitch, color) = unsafe {
            (
                font2.GetWeight().0,
                kind_of(font2.GetStyle()),
                stretch_of(font2.GetStretch()),
                font2.IsMonospacedFont().as_bool(),
                font2.IsColorFont().as_bool(),
            )
        };
        // SAFETY: `font` is a live font; the call takes no arguments.
        let Ok(face) = (unsafe { font.CreateFontFace() }) else {
            continue;
        };
        let writing_systems = face_writing_systems(&face);
        let handle = FaceHandle {
            family: owner,
            font: position,
        };
        let mut register = |family: &str, style_name: &str| {
            registry.register(Registration {
                family: family.to_string(),
                style_name: style_name.to_string(),
                weight: weight.clamp(0, i32::from(u16::MAX)) as u16,
                kind,
                stretch,
                fixed_pitch,
                color,
                writing_systems,
                handle,
            });
        };
        // The standard face names.
        // SAFETY: `font2` is a live font; the call takes no arguments.
        if let Ok(names) = unsafe { font2.GetFaceNames() } {
            let (default_style, english_style) = locales.both(&names);
            register(&english_family, &english_style);
            if !default_family.is_empty() && default_family != english_family {
                register(&default_family, &default_style);
            }
        }
        // The GDI compatible names, when they differ from the main ones.
        if !default_gdi.is_empty() || !english_gdi.is_empty() {
            let Some((default_gdi_style, english_gdi_style)) = informational(
                &font,
                DWRITE_INFORMATIONAL_STRING_WIN32_SUBFAMILY_NAMES,
                locales,
            ) else {
                continue;
            };
            if !english_gdi.is_empty() && english_gdi != english_family {
                register(&english_gdi, &english_gdi_style);
            }
            if !default_gdi.is_empty()
                && default_gdi != english_gdi
                && default_gdi != default_family
            {
                register(&default_gdi, &default_gdi_style);
            }
        }
    }
}

/// `QFontDatabase::WritingSystem` indices.
mod ws {
    pub const ANY: usize = 0;
    pub const LATIN: usize = 1;
    pub const GREEK: usize = 2;
    pub const CYRILLIC: usize = 3;
    pub const HEBREW: usize = 5;
    pub const ARABIC: usize = 6;
    pub const THAI: usize = 19;
    pub const SIMPLIFIED_CHINESE: usize = 25;
    pub const TRADITIONAL_CHINESE: usize = 26;
    pub const JAPANESE: usize = 27;
    pub const KOREAN: usize = 28;
    pub const VIETNAMESE: usize = 29;
    pub const SYMBOL: usize = 30;
    pub const COUNT: usize = 34;
}

/// The Unicode subset bit that makes a font support each writing system
/// (`requiredUnicodeBits`); 126 for the ones the code page bits decide, 127 for no second bit.
const REQUIRED_UNICODE_BITS: [(u8, u8); ws::COUNT] = [
    (127, 127), // Any
    (0, 127),   // Latin
    (7, 127),   // Greek
    (9, 127),   // Cyrillic
    (10, 127),  // Armenian
    (11, 127),  // Hebrew
    (13, 127),  // Arabic
    (71, 127),  // Syriac
    (72, 127),  // Thaana
    (15, 127),  // Devanagari
    (16, 127),  // Bengali
    (17, 127),  // Gurmukhi
    (18, 127),  // Gujarati
    (19, 127),  // Oriya
    (20, 127),  // Tamil
    (21, 127),  // Telugu
    (22, 127),  // Kannada
    (23, 127),  // Malayalam
    (73, 127),  // Sinhala
    (24, 127),  // Thai
    (25, 127),  // Lao
    (70, 127),  // Tibetan
    (74, 127),  // Myanmar
    (26, 127),  // Georgian
    (80, 127),  // Khmer
    (126, 127), // SimplifiedChinese
    (126, 127), // TraditionalChinese
    (126, 127), // Japanese
    (56, 127),  // Korean
    (0, 127),   // Vietnamese
    (126, 127), // Symbol
    (78, 127),  // Ogham
    (79, 127),  // Runic
    (14, 127),  // Nko
];

/// The script each writing system is for (`scriptForWritingSystem`).
const SCRIPT_FOR_WRITING_SYSTEM: [Script; ws::COUNT] = [
    Script::Common,
    Script::Latin,
    Script::Greek,
    Script::Cyrillic,
    Script::Armenian,
    Script::Hebrew,
    Script::Arabic,
    Script::Syriac,
    Script::Thaana,
    Script::Devanagari,
    Script::Bengali,
    Script::Gurmukhi,
    Script::Gujarati,
    Script::Oriya,
    Script::Tamil,
    Script::Telugu,
    Script::Kannada,
    Script::Malayalam,
    Script::Sinhala,
    Script::Thai,
    Script::Lao,
    Script::Tibetan,
    Script::Myanmar,
    Script::Georgian,
    Script::Khmer,
    Script::Han,
    Script::Han,
    Script::Han,
    Script::Hangul,
    Script::Latin,
    Script::Common,
    Script::Ogham,
    Script::Runic,
    Script::Nko,
];

/// `QPlatformFontDatabase::writingSystemsFromTrueTypeBits` for the `OS/2` unicode and code page
/// ranges.
fn writing_systems_from_bits(unicode_range: [u32; 4], code_page_range: [u32; 2]) -> u64 {
    let mut systems = 0u64;
    let mut has_script = false;
    for (index, &(first, second)) in REQUIRED_UNICODE_BITS.iter().enumerate() {
        let bit = |b: u8| unicode_range[usize::from(b / 32)] & (1 << (b & 31)) != 0;
        if first != 126 && bit(first) && (second == 127 || bit(second)) {
            systems |= 1 << index;
            has_script = true;
        }
    }
    let code_page = |bits: u32| code_page_range[0] & bits != 0;
    // The `CsbBits`: Latin1 0, CentralEurope 1, Turkish 4, Baltic 7, Cyrillic 2, Greek 3, Hebrew 5,
    // Arabic 6, Vietnamese 8, SimplifiedChinese 18, TraditionalChinese 20, Thai 16, Japanese 17,
    // Korean 19, KoreanJohab 21, Symbol 31.
    for (bits, system) in [
        ((1 << 0) | (1 << 1) | (1 << 4) | (1 << 7), ws::LATIN),
        (1 << 2, ws::CYRILLIC),
        (1 << 3, ws::GREEK),
        (1 << 5, ws::HEBREW),
        (1 << 6, ws::ARABIC),
        (1 << 16, ws::THAI),
        (1 << 8, ws::VIETNAMESE),
        (1 << 18, ws::SIMPLIFIED_CHINESE),
        (1 << 20, ws::TRADITIONAL_CHINESE),
        (1 << 17, ws::JAPANESE),
        ((1 << 19) | (1 << 21), ws::KOREAN),
    ] {
        if code_page(bits) {
            systems |= 1 << system;
            has_script = true;
        }
    }
    if code_page_range[0] & (1 << 31) != 0 {
        systems = 0;
        has_script = false;
    }
    if !has_script {
        systems |= 1 << ws::SYMBOL;
    }
    systems
}

/// `QWindowsDirectWriteFontDatabase::supportedWritingSystems`: from the `OS/2` table, else from the
/// first character of each Unicode range the face covers.
fn face_writing_systems(face: &IDWriteFontFace) -> u64 {
    let mut systems = 1u64 << ws::ANY;
    let Ok(face1) = face.cast::<IDWriteFontFace1>() else {
        return systems;
    };
    let mut data = std::ptr::null_mut();
    let mut size = 0u32;
    let mut context = std::ptr::null_mut();
    let mut exists = BOOL(0);
    let tag = u32::from_le_bytes(*b"OS/2");
    // SAFETY: the out-pointers are live locals; a table found is released below, before `data`
    // goes out of use.
    let found =
        unsafe { face.TryGetFontTable(tag, &mut data, &mut size, &mut context, &mut exists) }
            .is_ok()
            && exists.as_bool();
    if found {
        // SAFETY: `TryGetFontTable` reported `size` readable bytes at `data`, valid until
        // `ReleaseFontTable`.
        let table = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size as usize) };
        systems = if table.len() >= 86 {
            let be = |at: usize| {
                u32::from_be_bytes([table[at], table[at + 1], table[at + 2], table[at + 3]])
            };
            writing_systems_from_bits([be(42), be(46), be(50), be(54)], [be(78), be(82)])
        } else {
            0
        };
        // SAFETY: `context` is the one `TryGetFontTable` returned for this table.
        unsafe { face.ReleaseFontTable(context) };
        return systems;
    }
    let mut count = 0u32;
    // SAFETY: a null range array with a zero count asks for the number of ranges.
    unsafe { face1.GetUnicodeRanges(None, &mut count) }.ok();
    if count > 0 {
        let mut ranges = vec![DWRITE_UNICODE_RANGE::default(); count as usize];
        // SAFETY: `ranges` has `count` slots.
        if unsafe { face1.GetUnicodeRanges(Some(&mut ranges), &mut count) }.is_ok() {
            for range in &ranges[..count as usize] {
                let script = char::from_u32(range.first).map(|c| c.script());
                let system = script.and_then(|script| {
                    SCRIPT_FOR_WRITING_SYSTEM
                        .iter()
                        .position(|&candidate| candidate == script)
                });
                if let Some(system) = system.filter(|&s| s > ws::ANY && s < ws::COUNT) {
                    systems |= 1 << system;
                }
            }
        }
    }
    systems
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list of writing-system families is scanned once per `(italic, writing system)`, and what
    /// is remembered is what a scan of the registry, which the first one populated, gives.
    #[test]
    fn writing_system_lists_are_built_once_and_equal_a_fresh_scan() {
        let Some(catalog) = catalog() else {
            return;
        };
        for (italic, system) in [
            (false, ws::ANY),
            (true, ws::ANY),
            (false, ws::LATIN),
            (false, ws::TRADITIONAL_CHINESE),
            (true, ws::JAPANESE),
        ] {
            let first = catalog.families_for_writing_system(italic, system);
            let again = catalog.families_for_writing_system(italic, system);
            assert!(Arc::ptr_eq(&first, &again), "{italic} {system}");
            let mut registry = catalog.registry.lock().unwrap_or_else(|e| e.into_inner());
            let fresh = catalog.scan_writing_system(&mut registry, italic, system);
            assert_eq!(&*first, fresh.as_slice(), "{italic} {system}");
        }
        assert!(
            !catalog
                .families_for_writing_system(false, ws::ANY)
                .is_empty()
        );
    }

    /// A fallback list is computed for the first request of a key only, and the same list answers
    /// every later one.
    #[test]
    fn fallback_lists_are_computed_once_per_key() {
        let Some(catalog) = catalog() else {
            return;
        };
        let key = FallbackListKey {
            hint: StyleHint::Fantasy,
            italic: true,
            symbol_family: true,
            emoji: false,
            // The last writing system: no other test asks for this key.
            writing_system: ws::COUNT - 1,
        };
        let first = catalog.fallback_list(key, || vec!["A".to_string(), "B".to_string()]);
        let again = catalog.fallback_list(key, || panic!("computed twice"));
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(&*first, ["A", "B"]);
        let other = FallbackListKey { emoji: true, ..key };
        assert_eq!(
            &*catalog.fallback_list(other, || vec!["C".to_string()]),
            ["C"]
        );
    }
}
