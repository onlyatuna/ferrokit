// SPDX-License-Identifier: LGPL-3.0-only
//! `libfontconfig`, loaded at run time, as far as Qt's `QFontconfigDatabase` uses it
//! (`src/gui/text/unix/qfontconfigdatabase.cpp`): the list of every font file with the properties Qt
//! registers (`FcFontList`), the families fontconfig sorts for a pattern (`FcFontSort`, which
//! `fallbacksForFamily` returns) and the character set of the font that matches a family
//! (`FcFontMatch`, which `QFontEngineMultiFontConfig::shouldLoadFontEngineForCharacter` asks).
//!
//! Without the library (or under Miri, which cannot call it) every function answers `None`: the
//! font database then has no fontconfig rules to follow and reads the font directories itself.

use crate::text::font::StyleHint;
use crate::text::font_database::fc_family_for_style_hint;

/// What `FcFontList` reports about one font (one face of a collection, one named instance of a
/// variable font), the properties `populateFromPattern` reads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawFace {
    /// Every `FC_FAMILY` value; the first is the family, the others aliases or subfamilies.
    pub families: Vec<String>,
    /// `FC_STYLE` per family value (`None` where fontconfig has no value at that position).
    pub styles: Vec<Option<String>>,
    /// `FC_FAMILYLANG` per family value.
    pub family_langs: Vec<Option<String>>,
    pub weight: i32,
    pub slant: i32,
    pub spacing: i32,
    pub width: i32,
    pub file: String,
    pub index: i32,
    pub foundry: Option<String>,
    pub scalable: bool,
    pub color: bool,
    /// `QFontDatabase::WritingSystem` bits of `FC_LANG` and `FC_CAPABILITY`.
    pub writing_systems: u64,
}

/// The characters of a font (`FC_CHARSET`), as the 256-character pages `FcCharSetFirstPage` yields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CharSet {
    /// `(first character of the page, bitmap of its 256 characters)`, in ascending order.
    pages: Vec<(u32, [u32; 8])>,
}

impl CharSet {
    pub fn has(&self, ch: char) -> bool {
        let code = ch as u32;
        let base = code & !0xff;
        let bit = code & 0xff;
        self.pages
            .binary_search_by_key(&base, |&(first, _)| first)
            .is_ok_and(|i| self.pages[i].1[(bit >> 5) as usize] & (1 << (bit & 31)) != 0)
    }
}

/// What `QFontconfigDatabase::fallbacksForFamily` puts in the pattern besides the family.
pub(crate) struct SortRequest<'a> {
    pub family: &'a str,
    /// `FC_SLANT`: 0 roman, 100 italic, 110 oblique.
    pub slant: i32,
    /// The language the script of the item stands for (`specialLanguages`), when it has one;
    /// otherwise the system's default language is added for a non-empty family.
    pub language: Option<&'static str>,
    pub hint: StyleHint,
    /// The emoji script: a color font is asked for, and the weak family `emoji`.
    pub emoji: bool,
}

/// Whether the library is there.
pub(crate) fn is_available() -> bool {
    imp::available()
}

/// `FcFontList` of every font, for `populateFontDatabase`.
pub(crate) fn list_fonts() -> Option<Vec<RawFace>> {
    imp::list_fonts()
}

/// The first family name of each font `FcFontSort` lists for `request`, in order, repeats
/// included (the caller drops them).
pub(crate) fn sort_families(request: &SortRequest<'_>) -> Option<Vec<String>> {
    imp::sort_families(request)
}

/// The `FC_CHARSET` of the font `FcFontMatch` finds for the plain family `family`; `None` when
/// there is no library, no match or no character set.
pub(crate) fn matched_charset(family: &str) -> Option<CharSet> {
    imp::matched_charset(family)
}

#[cfg(miri)]
mod imp {
    use super::{CharSet, RawFace, SortRequest};

    pub fn available() -> bool {
        false
    }

    pub fn list_fonts() -> Option<Vec<RawFace>> {
        None
    }

    pub fn sort_families(_: &SortRequest<'_>) -> Option<Vec<String>> {
        None
    }

    pub fn matched_charset(_: &str) -> Option<CharSet> {
        None
    }
}

#[cfg(not(miri))]
mod imp {
    use super::{CharSet, RawFace, SortRequest, fc_family_for_style_hint};
    use std::ffi::{CStr, CString, c_char, c_int, c_void};

    const RTLD_LAZY: c_int = 1;
    const FC_TYPE_STRING: c_int = 3;
    const FC_RESULT_MATCH: c_int = 0;
    const FC_TRUE: c_int = 1;
    const FC_FALSE: c_int = 0;
    const FC_MATCH_PATTERN: c_int = 0;
    const FC_LANG_DIFFERENT_LANG: c_int = 2;
    const FC_CHARSET_DONE: u32 = u32::MAX;

    /// `QFontDatabase::WritingSystem` indices and the language and capability each is asked for
    /// (`languageForWritingSystem`, `capabilityForWritingSystem`), index 0 (Any) first.
    const WRITING_SYSTEMS: [(&str, &str); 34] = [
        ("", ""),
        ("en", ""),
        ("el", ""),
        ("ru", ""),
        ("hy", ""),
        ("he", ""),
        ("ar", ""),
        ("syr", "syrc"),
        ("div", "thaa"),
        ("hi", "deva"),
        ("bn", "beng"),
        ("pa", "guru"),
        ("gu", "gujr"),
        ("or", "orya"),
        ("ta", "taml"),
        ("te", "telu"),
        ("kn", "knda"),
        ("ml", "mlym"),
        ("si", "sinh"),
        ("th", ""),
        ("lo", ""),
        ("bo", "tibt"),
        ("my", "mymr"),
        ("ka", ""),
        ("km", "khmr"),
        ("zh-cn", ""),
        ("zh-tw", ""),
        ("ja", ""),
        ("ko", ""),
        ("vi", ""),
        ("", ""),
        ("sga", ""),
        ("non", ""),
        ("man", "nko "),
    ];
    const WS_SYRIAC: usize = 7;
    const WS_SINHALA: usize = 18;
    const WS_KHMER: usize = 24;
    const WS_NKO: usize = 33;
    const WS_OTHER: usize = 30;

    /// `requiresOpenType`: only those writing systems are skipped when the font's OpenType
    /// capability lacks them.
    fn requires_open_type(system: usize) -> bool {
        (WS_SYRIAC..=WS_SINHALA).contains(&system) || system == WS_KHMER || system == WS_NKO
    }

    #[repr(C)]
    struct FcPattern {
        _unused: [u8; 0],
    }

    #[repr(C)]
    struct FcFontSet {
        nfont: c_int,
        sfont: c_int,
        fonts: *mut *mut FcPattern,
    }

    #[repr(C)]
    struct FcObjectSet {
        _unused: [u8; 0],
    }

    #[repr(C)]
    struct FcLangSet {
        _unused: [u8; 0],
    }

    #[repr(C)]
    struct FcCharSet {
        _unused: [u8; 0],
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    union FcValueUnion {
        s: *const u8,
        i: c_int,
        b: c_int,
        d: f64,
        p: *const c_void,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    struct FcValue {
        typ: c_int,
        u: FcValueUnion,
    }

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
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

    /// Declares the table of fontconfig entry points: one function pointer field per symbol.
    macro_rules! entry_points {
        ($($field:ident = $symbol:literal: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
            struct FontConfig {
                $($field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            }

            impl FontConfig {
                fn load() -> Option<Self> {
                    // SAFETY: calls POSIX dlopen with a valid NUL-terminated library name string
                    // literal and the standard flag RTLD_LAZY.
                    let handle = unsafe { dlopen(c"libfontconfig.so.1".as_ptr(), RTLD_LAZY) };
                    if handle.is_null() {
                        return None;
                    }
                    let guard = HandleGuard(handle);
                    let table = Self {
                        $($field: {
                            // SAFETY: `handle` is a valid library handle and the symbol name is a
                            // NUL-terminated string literal.
                            let address = unsafe {
                                dlsym(handle, concat!($symbol, "\0").as_ptr().cast())
                            };
                            if address.is_null() {
                                return None;
                            }
                            // SAFETY: the address is the non-null entry point of the C function
                            // `$symbol`, whose signature is the one declared here (fontconfig.h).
                            unsafe {
                                std::mem::transmute::<
                                    *mut c_void,
                                    unsafe extern "C" fn($($arg),*) $(-> $ret)?,
                                >(address)
                            }
                        },)*
                    };
                    std::mem::forget(guard);
                    Some(table)
                }
            }
        };
    }

    entry_points! {
        init = "FcInit": fn() -> c_int;
        pattern_create = "FcPatternCreate": fn() -> *mut FcPattern;
        pattern_destroy = "FcPatternDestroy": fn(*mut FcPattern);
        pattern_add = "FcPatternAdd": fn(*mut FcPattern, *const c_char, FcValue, c_int) -> c_int;
        pattern_add_weak =
            "FcPatternAddWeak": fn(*mut FcPattern, *const c_char, FcValue, c_int) -> c_int;
        pattern_add_integer =
            "FcPatternAddInteger": fn(*mut FcPattern, *const c_char, c_int) -> c_int;
        pattern_add_bool = "FcPatternAddBool": fn(*mut FcPattern, *const c_char, c_int) -> c_int;
        pattern_add_langset =
            "FcPatternAddLangSet": fn(*mut FcPattern, *const c_char, *const FcLangSet) -> c_int;
        pattern_get_string =
            "FcPatternGetString": fn(*const FcPattern, *const c_char, c_int, *mut *mut u8) -> c_int;
        pattern_get_integer =
            "FcPatternGetInteger": fn(*const FcPattern, *const c_char, c_int, *mut c_int) -> c_int;
        pattern_get_bool =
            "FcPatternGetBool": fn(*const FcPattern, *const c_char, c_int, *mut c_int) -> c_int;
        pattern_get_langset = "FcPatternGetLangSet": fn(
            *const FcPattern,
            *const c_char,
            c_int,
            *mut *mut FcLangSet
        ) -> c_int;
        pattern_get_charset = "FcPatternGetCharSet": fn(
            *const FcPattern,
            *const c_char,
            c_int,
            *mut *mut FcCharSet
        ) -> c_int;
        langset_create = "FcLangSetCreate": fn() -> *mut FcLangSet;
        langset_add = "FcLangSetAdd": fn(*mut FcLangSet, *const u8) -> c_int;
        langset_destroy = "FcLangSetDestroy": fn(*mut FcLangSet);
        langset_has_lang = "FcLangSetHasLang": fn(*const FcLangSet, *const u8) -> c_int;
        charset_first_page =
            "FcCharSetFirstPage": fn(*const FcCharSet, *mut u32, *mut u32) -> u32;
        charset_next_page = "FcCharSetNextPage": fn(*const FcCharSet, *mut u32, *mut u32) -> u32;
        object_set_create = "FcObjectSetCreate": fn() -> *mut FcObjectSet;
        object_set_add = "FcObjectSetAdd": fn(*mut FcObjectSet, *const c_char) -> c_int;
        object_set_destroy = "FcObjectSetDestroy": fn(*mut FcObjectSet);
        font_list =
            "FcFontList": fn(*mut c_void, *mut FcPattern, *mut FcObjectSet) -> *mut FcFontSet;
        font_match =
            "FcFontMatch": fn(*mut c_void, *mut FcPattern, *mut c_int) -> *mut FcPattern;
        default_substitute = "FcDefaultSubstitute": fn(*mut FcPattern);
        config_substitute =
            "FcConfigSubstitute": fn(*mut c_void, *mut FcPattern, c_int) -> c_int;
        font_sort = "FcFontSort": fn(
            *mut c_void,
            *mut FcPattern,
            c_int,
            *mut c_void,
            *mut c_int
        ) -> *mut FcFontSet;
        font_set_destroy = "FcFontSetDestroy": fn(*mut FcFontSet);
    }

    fn library() -> Option<&'static FontConfig> {
        static INSTANCE: std::sync::LazyLock<Option<FontConfig>> = std::sync::LazyLock::new(|| {
            let Some(library) = FontConfig::load() else {
                // Qt cannot run without fontconfig; ferrokit still draws text, but not with the
                // fonts Qt would choose. Said once, when the library is first needed.
                eprintln!(
                    "ferrokit: libfontconfig.so.1 could not be loaded; fonts are taken from the \
                     font directories, missing families are not substituted and there are no \
                     fallback fonts, so text differs from Qt's"
                );
                return None;
            };
            // SAFETY: `FcInit` takes no arguments; Qt calls it before listing the fonts.
            unsafe { (library.init)() };
            Some(library)
        });
        INSTANCE.as_ref()
    }

    pub fn available() -> bool {
        library().is_some()
    }

    /// An `FcPattern` that is destroyed with it.
    struct Pattern<'a> {
        fc: &'a FontConfig,
        raw: *mut FcPattern,
    }

    impl<'a> Pattern<'a> {
        fn new(fc: &'a FontConfig) -> Option<Self> {
            // SAFETY: `FcPatternCreate` takes no arguments.
            let raw = unsafe { (fc.pattern_create)() };
            (!raw.is_null()).then_some(Self { fc, raw })
        }

        fn add_string(&mut self, object: &CStr, value: &CStr, weak: bool) {
            let value = FcValue {
                typ: FC_TYPE_STRING,
                u: FcValueUnion {
                    s: value.as_ptr().cast(),
                },
            };
            let add = if weak {
                self.fc.pattern_add_weak
            } else {
                self.fc.pattern_add
            };
            // SAFETY: `self.raw` is a live pattern, `object` and the string value are
            // NUL-terminated, and fontconfig copies the value before the call returns.
            unsafe { add(self.raw, object.as_ptr(), value, FC_TRUE) };
        }

        fn add_integer(&mut self, object: &CStr, value: i32) {
            // SAFETY: `self.raw` is a live pattern and `object` is NUL-terminated.
            unsafe { (self.fc.pattern_add_integer)(self.raw, object.as_ptr(), value) };
        }

        fn add_bool(&mut self, object: &CStr, value: bool) {
            // SAFETY: `self.raw` is a live pattern and `object` is NUL-terminated.
            unsafe {
                (self.fc.pattern_add_bool)(self.raw, object.as_ptr(), c_int::from(value));
            }
        }

        fn string(&self, object: &CStr, n: i32) -> Option<String> {
            let mut value: *mut u8 = std::ptr::null_mut();
            // SAFETY: `self.raw` is a live pattern; fontconfig stores a pointer to a
            // NUL-terminated string it owns in `value` when it reports a match.
            let result =
                unsafe { (self.fc.pattern_get_string)(self.raw, object.as_ptr(), n, &mut value) };
            if result != FC_RESULT_MATCH || value.is_null() {
                return None;
            }
            // SAFETY: on a match `value` points to a NUL-terminated string that lives as long as
            // the pattern, and it is copied right here.
            Some(
                unsafe { CStr::from_ptr(value.cast()) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }

        fn integer(&self, object: &CStr) -> Option<i32> {
            let mut value: c_int = 0;
            // SAFETY: `self.raw` is a live pattern and `value` is a valid destination.
            let result =
                unsafe { (self.fc.pattern_get_integer)(self.raw, object.as_ptr(), 0, &mut value) };
            (result == FC_RESULT_MATCH).then_some(value)
        }

        fn boolean(&self, object: &CStr) -> Option<bool> {
            let mut value: c_int = 0;
            // SAFETY: `self.raw` is a live pattern and `value` is a valid destination.
            let result =
                unsafe { (self.fc.pattern_get_bool)(self.raw, object.as_ptr(), 0, &mut value) };
            (result == FC_RESULT_MATCH).then_some(value != FC_FALSE)
        }
    }

    impl Drop for Pattern<'_> {
        fn drop(&mut self) {
            // SAFETY: `self.raw` is a live pattern nothing else frees.
            unsafe { (self.fc.pattern_destroy)(self.raw) };
        }
    }

    /// A borrowed font pattern of a font set (`fonts[i]`), valid while the set is.
    struct SetPattern<'a> {
        fc: &'a FontConfig,
        raw: *mut FcPattern,
    }

    impl SetPattern<'_> {
        fn as_pattern(&self) -> std::mem::ManuallyDrop<Pattern<'_>> {
            // `ManuallyDrop`: the set owns the pattern, which must not be destroyed here.
            std::mem::ManuallyDrop::new(Pattern {
                fc: self.fc,
                raw: self.raw,
            })
        }
    }

    /// An `FcFontSet` that is destroyed with it.
    struct FontSet<'a> {
        fc: &'a FontConfig,
        raw: *mut FcFontSet,
    }

    impl<'a> FontSet<'a> {
        fn patterns(&self) -> Vec<SetPattern<'a>> {
            // SAFETY: `self.raw` is a live font set whose `fonts` array holds `nfont` pointers.
            unsafe {
                let set = &*self.raw;
                if set.fonts.is_null() || set.nfont <= 0 {
                    return Vec::new();
                }
                (0..set.nfont as usize)
                    .map(|i| SetPattern {
                        fc: self.fc,
                        raw: *set.fonts.add(i),
                    })
                    .filter(|p| !p.raw.is_null())
                    .collect()
            }
        }
    }

    impl Drop for FontSet<'_> {
        fn drop(&mut self) {
            // SAFETY: `self.raw` is a live font set nothing else frees.
            unsafe { (self.fc.font_set_destroy)(self.raw) };
        }
    }

    /// The `FC_LANG` of the default pattern (`FcDefaultSubstitute` on an empty one), which is the
    /// system's language.
    fn default_language(fc: &FontConfig) -> Option<CString> {
        let dummy = Pattern::new(fc)?;
        // SAFETY: `dummy.raw` is a live pattern.
        unsafe { (fc.default_substitute)(dummy.raw) };
        dummy
            .string(c"lang", 0)
            .and_then(|lang| CString::new(lang).ok())
    }

    pub fn list_fonts() -> Option<Vec<RawFace>> {
        let fc = library()?;
        let mut pattern = Pattern::new(fc)?;
        // Qt asks for the fonts that are not variable: the named instances of variable fonts and
        // the static fonts, not the variable font itself.
        pattern.add_bool(c"variable", false);
        // SAFETY: the object set is created, filled with NUL-terminated names, passed to
        // `FcFontList` together with the live pattern, and destroyed after the call; the font set
        // the call returns is owned by `FontSet`.
        let set = unsafe {
            let object_set = (fc.object_set_create)();
            if object_set.is_null() {
                return None;
            }
            for property in [
                c"family",
                c"style",
                c"weight",
                c"slant",
                c"spacing",
                c"file",
                c"index",
                c"lang",
                c"charset",
                c"foundry",
                c"scalable",
                c"pixelsize",
                c"width",
                c"familylang",
                c"capability",
                c"color",
            ] {
                (fc.object_set_add)(object_set, property.as_ptr());
            }
            let raw = (fc.font_list)(std::ptr::null_mut(), pattern.raw, object_set);
            (fc.object_set_destroy)(object_set);
            if raw.is_null() {
                return None;
            }
            FontSet { fc, raw }
        };
        let mut faces = Vec::new();
        for entry in set.patterns() {
            let pattern = entry.as_pattern();
            if let Some(face) = raw_face(fc, &pattern) {
                faces.push(face);
            }
        }
        Some(faces)
    }

    /// The part of `populateFromPattern` that reads a pattern.
    fn raw_face(fc: &FontConfig, pattern: &Pattern<'_>) -> Option<RawFace> {
        let mut families = Vec::new();
        while let Some(name) = pattern.string(c"family", families.len() as i32) {
            families.push(name);
        }
        if families.is_empty() {
            return None;
        }
        let styles = (0..families.len())
            .map(|k| pattern.string(c"style", k as i32))
            .collect();
        let family_langs = (0..families.len())
            .map(|k| pattern.string(c"familylang", k as i32))
            .collect();
        let file = pattern.string(c"file", 0).unwrap_or_default();
        Some(RawFace {
            families,
            styles,
            family_langs,
            weight: pattern.integer(c"weight").unwrap_or(80),
            slant: pattern.integer(c"slant").unwrap_or(0),
            spacing: pattern.integer(c"spacing").unwrap_or(0),
            width: pattern.integer(c"width").unwrap_or(100),
            file,
            index: pattern.integer(c"index").unwrap_or(0),
            foundry: pattern.string(c"foundry", 0),
            scalable: pattern.boolean(c"scalable").unwrap_or(true),
            color: pattern.boolean(c"color").unwrap_or(false),
            writing_systems: writing_systems(fc, pattern),
        })
    }

    /// The writing systems of a font: `FC_LANG` against the language of each, minus those that
    /// need OpenType layout the font's `FC_CAPABILITY` lacks; `Other` when the font has no
    /// language set or none that is known.
    fn writing_systems(fc: &FontConfig, pattern: &Pattern<'_>) -> u64 {
        let mut langset: *mut FcLangSet = std::ptr::null_mut();
        // SAFETY: `pattern.raw` is a live pattern and `langset` a valid destination.
        let result =
            unsafe { (fc.pattern_get_langset)(pattern.raw, c"lang".as_ptr(), 0, &mut langset) };
        if result != FC_RESULT_MATCH || langset.is_null() {
            return 1 << WS_OTHER;
        }
        let mut systems = 0u64;
        let mut capability: Option<Option<String>> = None;
        for (system, &(lang, wanted)) in WRITING_SYSTEMS.iter().enumerate().skip(1) {
            let Ok(lang) = CString::new(lang) else {
                continue;
            };
            // SAFETY: `langset` belongs to the live pattern and `lang` is NUL-terminated.
            let found = unsafe { (fc.langset_has_lang)(langset, lang.as_ptr().cast()) };
            if found == FC_LANG_DIFFERENT_LANG {
                continue;
            }
            if !wanted.is_empty() && requires_open_type(system) {
                let capability = capability.get_or_insert_with(|| pattern.string(c"capability", 0));
                if capability
                    .as_deref()
                    .is_some_and(|capability| !capability.contains(wanted))
                {
                    continue;
                }
            }
            systems |= 1 << system;
        }
        if systems == 0 {
            systems = 1 << WS_OTHER;
        }
        systems
    }

    pub fn sort_families(request: &SortRequest<'_>) -> Option<Vec<String>> {
        let fc = library()?;
        let mut pattern = Pattern::new(fc)?;
        let family = CString::new(request.family).ok()?;
        pattern.add_string(c"family", &family, false);
        if request.emoji {
            pattern.add_bool(c"color", true);
            pattern.add_string(c"family", c"emoji", true);
        }
        pattern.add_integer(c"slant", request.slant);
        match request.language {
            Some(language) => {
                let language = CString::new(language).ok()?;
                // SAFETY: the language set is created, filled with a NUL-terminated language,
                // copied into the live pattern and destroyed.
                unsafe {
                    let set = (fc.langset_create)();
                    if !set.is_null() {
                        (fc.langset_add)(set, language.as_ptr().cast());
                        (fc.pattern_add_langset)(pattern.raw, c"lang".as_ptr(), set);
                        (fc.langset_destroy)(set);
                    }
                }
            }
            None if !request.family.is_empty() => {
                if let Some(lang) = default_language(fc) {
                    // The string value is copied; weakness does not apply (`FcPatternAddString`).
                    pattern.add_string(c"lang", &lang, false);
                }
            }
            None => {}
        }
        if let Some(generic) = fc_family_for_style_hint(request.hint)
            && let Ok(generic) = CString::new(generic)
        {
            pattern.add_string(c"family", &generic, true);
        }
        let mut result: c_int = FC_RESULT_MATCH;
        // SAFETY: `pattern.raw` is a live pattern; the sorted set is owned by `FontSet`.
        let set = unsafe {
            (fc.config_substitute)(std::ptr::null_mut(), pattern.raw, FC_MATCH_PATTERN);
            (fc.default_substitute)(pattern.raw);
            let raw = (fc.font_sort)(
                std::ptr::null_mut(),
                pattern.raw,
                FC_FALSE,
                std::ptr::null_mut(),
                &mut result,
            );
            if raw.is_null() {
                return Some(Vec::new());
            }
            FontSet { fc, raw }
        };
        Some(
            set.patterns()
                .iter()
                .filter_map(|entry| entry.as_pattern().string(c"family", 0))
                .collect(),
        )
    }

    pub fn matched_charset(family: &str) -> Option<CharSet> {
        let fc = library()?;
        let mut request = Pattern::new(fc)?;
        request.add_string(c"family", &CString::new(family).ok()?, false);
        let mut result: c_int = FC_RESULT_MATCH;
        // SAFETY: `request.raw` is a live pattern; the match, when there is one, is owned by
        // `matched` and destroyed with it.
        let matched = unsafe {
            let raw = (fc.font_match)(std::ptr::null_mut(), request.raw, &mut result);
            if raw.is_null() {
                return None;
            }
            Pattern { fc, raw }
        };
        let mut charset: *mut FcCharSet = std::ptr::null_mut();
        // SAFETY: `matched.raw` is a live pattern and `charset` a valid destination.
        let found =
            unsafe { (fc.pattern_get_charset)(matched.raw, c"charset".as_ptr(), 0, &mut charset) };
        if found != FC_RESULT_MATCH || charset.is_null() {
            return None;
        }
        let mut pages = Vec::new();
        let mut map = [0u32; 8];
        let mut next = 0u32;
        // SAFETY: `charset` belongs to the live pattern; `map` has the 256 bits a page needs
        // (`FC_CHARSET_MAP_SIZE` 32-bit words are 8) and `next` is fontconfig's iterator state.
        unsafe {
            let mut first = (fc.charset_first_page)(charset, map.as_mut_ptr(), &mut next);
            while first != FC_CHARSET_DONE {
                pages.push((first, map));
                first = (fc.charset_next_page)(charset, map.as_mut_ptr(), &mut next);
            }
        }
        Some(CharSet { pages })
    }
}
