// SPDX-License-Identifier: LGPL-3.0-only
//! The font database on Linux as Qt's `QFontconfigDatabase` makes it.
//!
//! The families are the registry [`Registry`] builds from what fontconfig lists (`FcFontList`), so a
//! family name, a weight and a slant resolve to the font file Qt picks: the style nearest by weight,
//! stretch and slant (`QFontDatabasePrivate::match`), under the names fontconfig gives a font. A
//! family that is not there is replaced by the first family `FcFontSort` lists for it
//! (`QFontDatabasePrivate::findFont`). The fallback fonts of text are those of each of its script
//! items (`QFontEngineMulti`, `fallbacksForFamily` per script), tried in fontconfig's order and
//! skipped when the font fontconfig matches to the family lacks the character
//! (`QFontEngineMultiFontConfig::shouldLoadFontEngineForCharacter`).
//!
//! Without the fontconfig library the families are the fonts found in the font directories, there
//! is no substitute for a missing family, and no fallback font.

use super::*;
use crate::text::fontconfig_catalog::{Handle, Registry, Request, Slant};
use crate::text::fontconfig_ffi::{self, RawFace};
use std::collections::HashSet;
use std::sync::LazyLock;

/// What `FcFontList` reports, read once: fontconfig keeps its own cache, and Qt lists the fonts
/// once at start-up.
fn system_fonts() -> Option<&'static [RawFace]> {
    static FONTS: LazyLock<Option<Vec<RawFace>>> = LazyLock::new(fontconfig_ffi::list_fonts);
    FONTS.as_deref()
}

/// The family a primary request resolved to (`QFontEngine::fontDef.families`), which the
/// fallback fonts of the text are listed for.
#[derive(Clone)]
struct ResolvedFamily {
    name: String,
    fixed_pitch: bool,
}

/// The state of the font database that only Linux has.
pub(super) struct LinuxFonts {
    registry: Option<Registry>,
    /// How many of the search paths are the fontconfig directories every user has: fontconfig
    /// lists those, only the paths added later are read by the database itself.
    default_paths: usize,
    /// The family each cached face request resolved to, by `FaceStyle::cache_key`.
    resolved: HashMap<String, ResolvedFamily>,
}

impl LinuxFonts {
    pub(super) fn new(default_paths: usize) -> Self {
        Self {
            registry: None,
            default_paths,
            resolved: HashMap::new(),
        }
    }

    /// The registry has to be built again (a search path was added).
    pub(super) fn invalidate(&mut self) {
        self.registry = None;
        self.resolved.clear();
    }

    /// The registry, built on first use: the fonts fontconfig lists, then those found in the
    /// search paths added by the caller (registered after, so a font twice takes the later one);
    /// without fontconfig, those found in every search path.
    pub(super) fn registry(&mut self, search_paths: &[PathBuf]) -> &mut Registry {
        let default_paths = self.default_paths;
        self.registry.get_or_insert_with(|| {
            let _t = crate::startup_trace::span(|| "font registry".into());
            let (mut registry, scanned) = match system_fonts() {
                Some(fonts) => (
                    Registry::from_fontconfig(fonts),
                    &search_paths[default_paths.min(search_paths.len())..],
                ),
                None => (Registry::without_fontconfig(), search_paths),
            };
            for info in scan_font_directories(scanned) {
                let Some(path) = info.path else {
                    continue;
                };
                let slant = if info.italic {
                    Slant::Italic
                } else {
                    Slant::Normal
                };
                registry.add_font(
                    &info.family,
                    info.weight,
                    slant,
                    info.stretch,
                    info.fixed_pitch,
                    Handle {
                        path,
                        index: info.face_index as i32,
                    },
                );
            }
            registry
        })
    }

    /// Every registered font as a [`FontFamilyInfo`].
    pub(super) fn registered_faces(&mut self, search_paths: &[PathBuf]) -> Vec<FontFamilyInfo> {
        self.registry(search_paths)
            .fonts()
            .map(|(family, handle, weight, slant)| FontFamilyInfo {
                family: family.name.clone(),
                path: Some(handle.path.clone()),
                face_index: handle.face_index(),
                fixed_pitch: family.fixed_pitch,
                weight,
                italic: slant != Slant::Normal,
                stretch: 100,
            })
            .collect()
    }
}

/// A font the registry resolved and the database opened.
pub(super) struct Found {
    raw: SharedFontData,
    face: SharedGlyphFace,
    family: String,
    fixed_pitch: bool,
}

impl Found {
    /// The family of the registry the font belongs to.
    pub(super) fn family(&self) -> &str {
        &self.family
    }
}

/// The slant a face request asks for.
fn slant_of(style: FaceStyle) -> Slant {
    match (style.italic, style.oblique) {
        (false, _) => Slant::Normal,
        (true, false) => Slant::Italic,
        (true, true) => Slant::Oblique,
    }
}

fn request_of(style: FaceStyle) -> Request<'static> {
    Request::new(style.weight, slant_of(style))
}

/// `FT_Face::style_name` as FreeType 2.13 sets it for an sfnt font (`sfnt_load_face` with
/// `tt_face_get_name`, sfobjs.c), which Qt's FreeType engine makes its `fontDef.styleName`
/// (`QFontEngineFT::init`). With bit 8 of the OS/2 `fsSelection` set: the typographic
/// subfamily (name 17), else the subfamily (2); without it: the WWS subfamily (22), else 17,
/// else 2. Empty when the face has none of them.
fn freetype_style_name(data: &[u8], index: u32) -> String {
    use rustybuzz::ttf_parser::{Face, PlatformId, Tag};
    let Ok(face) = Face::parse(data, index) else {
        return String::new();
    };
    let names: Vec<_> = face.names().into_iter().collect();
    // `tt_face_get_name`: an English Windows name (or the first Windows name when there is no
    // English one), unless only a Macintosh name is English; then a Macintosh name, then a
    // Unicode one. Bytes outside printable ASCII read as '?'.
    let get = |id: u16| -> Option<String> {
        let (mut apple_roman, mut apple_english, mut win, mut unicode) = (None, None, None, None);
        let mut is_english = false;
        for (n, rec) in names.iter().enumerate() {
            if rec.name_id != id || rec.name.is_empty() {
                continue;
            }
            match rec.platform_id {
                PlatformId::Unicode | PlatformId::Iso => unicode = Some(n),
                PlatformId::Macintosh => {
                    if rec.language_id == 0 {
                        apple_english = Some(n);
                    } else if rec.encoding_id == 0 {
                        apple_roman = Some(n);
                    }
                }
                PlatformId::Windows => {
                    if (win.is_none() || rec.language_id & 0x3ff == 0x009)
                        && matches!(rec.encoding_id, 0 | 1 | 10)
                    {
                        is_english = rec.language_id & 0x3ff == 0x009;
                        win = Some(n);
                    }
                }
                PlatformId::Custom => {}
            }
        }
        let apple = apple_english.or(apple_roman);
        let ascii = |code: u32| match code {
            32..=127 => char::from(code as u8),
            _ => '?',
        };
        let utf16 = |bytes: &[u8]| -> String {
            let (pairs, _) = bytes.as_chunks::<2>();
            pairs
                .iter()
                .map(|&pair| u32::from(u16::from_be_bytes(pair)))
                .take_while(|&code| code != 0)
                .map(ascii)
                .collect()
        };
        if let Some(n) = win.filter(|_| !(apple.is_some() && !is_english)) {
            Some(utf16(names[n].name))
        } else if let Some(n) = apple {
            Some(
                names[n]
                    .name
                    .iter()
                    .take_while(|&&byte| byte != 0)
                    .map(|&byte| ascii(u32::from(byte)))
                    .collect(),
            )
        } else {
            unicode.map(|n| utf16(names[n].name))
        }
    };
    // OS/2 is read when it has at least the 78 bytes of version 0 (`tt_face_load_os2`).
    let wws_only = face
        .raw_face()
        .table(Tag::from_bytes(b"OS/2"))
        .filter(|os2| os2.len() >= 78)
        .is_some_and(|os2| u16::from_be_bytes([os2[62], os2[63]]) & 256 != 0);
    let order: &[u16] = if wws_only { &[17, 2] } else { &[22, 17, 2] };
    order.iter().find_map(|&id| get(id)).unwrap_or_default()
}

/// Opens the font of a registered style, reading its file once.
fn load_face(
    file_cache: &mut HashMap<PathBuf, SharedFontData>,
    handle: &Handle,
    style: FaceStyle,
) -> Option<(SharedFontData, SharedGlyphFace)> {
    let canonical = handle
        .path
        .canonicalize()
        .unwrap_or_else(|_| handle.path.clone());
    let shared = match file_cache.get(&canonical) {
        Some(shared) => shared.clone(),
        None => {
            let _t = crate::startup_trace::span(|| format!("map {}", canonical.display()));
            let shared = SharedFontData::from_file(&canonical).ok()?;
            file_cache.insert(canonical, shared.clone());
            shared
        }
    };
    let _t = crate::startup_trace::span(|| {
        format!(
            "parse_face {} ({} KB)",
            handle.path.display(),
            shared.len() / 1024
        )
    });
    let face = parse_face_for_request(
        &shared,
        handle.face_index(),
        style.hint_style,
        style.weight,
        style.italic,
    )
    .ok()?;
    Some((shared, face))
}

/// One character of each script and of the symbol, arrow and emoji blocks that text mixes in:
/// the text [`FontDatabase::resolve_font_engines`] lists fallback fonts for when it has none.
const FULL_CHAIN_PROBE: &str = "Aa \u{3a9}\u{3c9} \u{42f}\u{44f} \u{531}\u{561} \u{5d0}\u{5d1} \u{639}\u{628} \u{710} \u{780} \u{928} \u{995} \u{b95} \u{c15} \u{d15} \u{dc3} \u{e01} \u{e81} \u{f40} \u{1000} \u{10d0} \u{1780} \u{3042}\u{30ab} \u{4e2d} \u{d55c} \u{a9}\u{20ac}\u{2192}\u{2211}\u{2605}\u{2713}\u{2665} \u{1F600}";

impl FontDatabase {
    /// The font of `family` the registry resolves a request of `style` to, opened.
    pub(super) fn resolve_registered(&mut self, family: &str, style: FaceStyle) -> Option<Found> {
        self.resolve_registered_named(family, style, "")
    }

    /// [`resolve_registered`](Self::resolve_registered) for a request with a style name: the
    /// style of that name, if the family has one, whatever its weight and slant.
    fn resolve_registered_named(
        &mut self,
        family: &str,
        style: FaceStyle,
        style_name: &str,
    ) -> Option<Found> {
        let request = Request {
            style_name,
            ..request_of(style)
        };
        let registry = self.linux.registry(&self.search_paths);
        let file_cache = &mut self.file_cache;
        let mut loaded = None;
        let mut blacklist = Vec::new();
        registry.find_exact(
            family,
            &request,
            &mut blacklist,
            &mut |found, handle| match load_face(file_cache, handle, style) {
                Some((raw, face)) => {
                    loaded = Some(Found {
                        raw,
                        face,
                        family: found.name.clone(),
                        fixed_pitch: found.fixed_pitch,
                    });
                    true
                }
                None => false,
            },
        )?;
        loaded
    }

    /// The font that replaces the missing `family` (its fonts first, none for an empty name).
    pub(super) fn resolve_substitute(
        &mut self,
        family: &str,
        style: FaceStyle,
        hint: StyleHint,
    ) -> Option<Found> {
        let request = request_of(style);
        let registry = self.linux.registry(&self.search_paths);
        let file_cache = &mut self.file_cache;
        let mut loaded = None;
        let mut blacklist = Vec::new();
        registry.find_substitute(
            family,
            &request,
            hint,
            &mut blacklist,
            &mut |found, handle| match load_face(file_cache, handle, style) {
                Some((raw, face)) => {
                    loaded = Some(Found {
                        raw,
                        face,
                        family: found.name.clone(),
                        fixed_pitch: found.fixed_pitch,
                    });
                    true
                }
                None => false,
            },
        )?;
        loaded
    }

    /// Caches a resolved font under the key of the request, with the family it came from.
    pub(super) fn store_resolved(&mut self, style_key: String, found: Found) -> SharedGlyphFace {
        self.cache
            .insert(style_key.clone(), Arc::clone(&found.face));
        self.raw_cache.insert(style_key.clone(), found.raw);
        self.linux.resolved.insert(
            style_key,
            ResolvedFamily {
                name: found.family,
                fixed_pitch: found.fixed_pitch,
            },
        );
        found.face
    }

    /// [`store_resolved`](Self::store_resolved) for a font that replaces a missing family: the face
    /// is the one a request for the replacing family itself gets (Qt has one engine per font), so
    /// it is shared with that entry, which this makes if it is not there yet.
    pub(super) fn store_substitute(
        &mut self,
        style_key: String,
        own_key: String,
        mut found: Found,
    ) -> SharedGlyphFace {
        match (
            self.cache.get(&own_key).cloned(),
            self.raw_cache.get(&own_key).cloned(),
        ) {
            (Some(face), Some(raw)) => {
                found.face = face;
                found.raw = raw;
            }
            _ => {
                self.cache.insert(own_key.clone(), Arc::clone(&found.face));
                self.raw_cache.insert(own_key, found.raw.clone());
            }
        }
        self.store_resolved(style_key, found)
    }

    /// Makes the family a request resolved to also that of `to` (a list of families the first
    /// loadable of which was used).
    pub(super) fn copy_resolved_family(&mut self, from: &str, to: String) {
        if let Some(resolved) = self.linux.resolved.get(from).cloned() {
            self.linux.resolved.insert(to, resolved);
        }
    }

    /// The font of the family `family_key` for `style`, opened (no substitute).
    pub(super) fn find_and_load_font_file(
        &mut self,
        family_key: &str,
        style: FaceStyle,
    ) -> Option<(SharedFontData, SharedGlyphFace)> {
        self.resolve_registered(family_key, style)
            .map(|found| (found.raw, found.face))
    }

    /// `QFontDatabase::families`: the registry's families and the application fonts.
    pub(super) fn registry_families(&mut self) -> Vec<String> {
        let mut names: Vec<String> = self
            .linux
            .registry(&self.search_paths)
            .families()
            .iter()
            .map(|family| family.name.clone())
            .chain(
                self.application_faces
                    .iter()
                    .map(|face| face.family.clone()),
            )
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    }

    /// Whether the registry has a family answering to `family` (its name or an alias).
    pub(super) fn registry_has_family(&mut self, family: &str) -> bool {
        self.linux.registry(&self.search_paths).has_family(family)
    }

    /// The family a request for `font` resolved to, and whether it is fixed pitch; the requested
    /// family itself when it has not been resolved (a font from memory).
    fn primary_family(&self, font: &Font) -> (String, bool) {
        if font.font_data.is_none() {
            let key =
                FaceStyle::of(font).cache_key(&font.family.to_ascii_lowercase(), font.style_hint);
            if let Some(resolved) = self.linux.resolved.get(&key) {
                return (resolved.name.clone(), resolved.fixed_pitch);
            }
        }
        (font.family.clone(), false)
    }

    /// The family of `font` when it names a registered family with a font behind it (Qt's family
    /// name match pass finds it), with whether it is fixed pitch; a font from memory counts as its
    /// own family.
    fn exact_family(&mut self, font: &Font, style: FaceStyle) -> Option<(String, bool)> {
        if font.font_data.is_some() {
            return Some((font.family.clone(), false));
        }
        let request = request_of(style);
        let registry = self.linux.registry(&self.search_paths);
        let names: Vec<&str> = if font.family.contains(',') {
            font.family
                .split(',')
                .map(|n| n.trim().trim_matches('\'').trim_matches('"'))
                .filter(|n| {
                    !n.is_empty()
                        && !["sans-serif", "serif", "monospace"]
                            .iter()
                            .any(|g| n.eq_ignore_ascii_case(g))
                })
                .collect()
        } else {
            vec![font.family.as_str()]
        };
        names.into_iter().find_map(|name| {
            let found = registry.find(name, "", &request, &[])?;
            registry.handle(&found)?;
            let family = registry.family(&found);
            Some((family.name.clone(), family.fixed_pitch))
        })
    }

    /// The first family named `name` (any family for "") with a font that opens and that `accept`s,
    /// trying the best style of each matching family in `QFontDatabasePrivate::match`'s order.
    fn find_loadable(
        &mut self,
        name: &str,
        style: FaceStyle,
        request: &Request,
        accept: &dyn Fn(&Found) -> bool,
    ) -> Option<Found> {
        let mut blacklist = Vec::new();
        loop {
            let registry = self.linux.registry(&self.search_paths);
            let found = registry.find(name, "", request, &blacklist)?;
            let handle = registry.handle(&found).cloned();
            let family = registry.family(&found);
            let (family_name, fixed_pitch) = (family.name.clone(), family.fixed_pitch);
            if let Some(handle) = handle
                && let Some((raw, face)) = load_face(&mut self.file_cache, &handle, style)
            {
                let loaded = Found {
                    raw,
                    face,
                    family: family_name,
                    fixed_pitch,
                };
                if accept(&loaded) {
                    return Some(loaded);
                }
            }
            blacklist.push(found.family);
        }
    }

    /// The font a script item of `key` is shaped with (`QFontDatabasePrivate::findFont` for the
    /// script): the requested family when it exists and, for a script that needs OpenType layout,
    /// supports the script; else the first family `fallbacksForFamily` lists for the script (then
    /// the first family of the database) that does; for emoji, color fonts first and then the
    /// requested family and the listed ones whatever they are; the box engine when nothing can
    /// shape the item. Returns the engine's index in `engines`, its family and whether that is
    /// fixed pitch.
    ///
    /// Qt's emoji pass also takes a listed family whose emoji engine an earlier request of the
    /// same `QFontDef` left in the `QFontCache` (`findMatchingFallback` looks the cache up before
    /// it requires a color font, qfontdatabase.cpp:2797), so in one process its result depends on
    /// what was drawn before and on the cache's cost and timer driven eviction
    /// (`QFontCache::decreaseCache`). This is the result with that cache empty, as in a fresh
    /// process.
    fn item_font(
        &mut self,
        engines: &mut Vec<FontEngine>,
        font: &Font,
        style: FaceStyle,
        key: &'static str,
        own: &(String, bool),
        exact: bool,
    ) -> (usize, String, bool) {
        use unicode_script::Script;
        let emoji = key == "Emoji";
        let script = Script::from_short_name(key).unwrap_or(Script::Common);
        let needs_open_type = matches!(
            script,
            Script::Syriac
                | Script::Thaana
                | Script::Devanagari
                | Script::Bengali
                | Script::Gurmukhi
                | Script::Gujarati
                | Script::Oriya
                | Script::Tamil
                | Script::Telugu
                | Script::Kannada
                | Script::Malayalam
                | Script::Sinhala
                | Script::Khmer
                | Script::Nko
        );
        let supports = |raw: &SharedFontData, index: u32| {
            !needs_open_type
                || crate::text::shaping_plan::font_supports_open_type_script(
                    raw.as_slice(),
                    index,
                    script,
                )
        };
        if key == "Zyyy" || (!emoji && exact) {
            let own_supports = engines[0]
                .raw_data
                .as_ref()
                .is_none_or(|raw| supports(raw, engines[0].face_index));
            if key == "Zyyy" || own_supports {
                return (0, own.0.clone(), own.1);
            }
        }
        let slant = slant_of(style);
        let mut request = request_of(style);
        request.color = emoji;
        let mut candidates = self
            .linux
            .registry(&self.search_paths)
            .fallbacks_for_family(&font.family, slant, font.style_hint, key);
        // The first font matching the script, for every script after Common.
        candidates.push(String::new());
        let accept = |found: &Found| supports(&found.raw, found.face.face_index());
        let mut chosen = candidates
            .iter()
            .find_map(|name| self.find_loadable(name, style, &request, &accept));
        if chosen.is_none() && emoji {
            // No color font (`findFont`'s final pass, qfontdatabase.cpp:2842-2850): the requested
            // family, then the listed ones, whatever they are.
            request.color = false;
            if exact {
                candidates.insert(0, own.0.clone());
            }
            chosen = candidates
                .iter()
                .find_map(|name| self.find_loadable(name, style, &request, &accept));
        }
        match chosen {
            Some(found) => match self.fallback_engine_index(engines, &found.family, style, "") {
                Some(index) => (index, found.family, found.fixed_pitch),
                None => self.box_engine(engines),
            },
            None => self.box_engine(engines),
        }
    }

    /// The box engine of `engines`, added when it is not there.
    fn box_engine(&mut self, engines: &mut Vec<FontEngine>) -> (usize, String, bool) {
        let face: SharedGlyphFace = Arc::new(crate::text::box_face::BoxFace);
        engines.push(FontEngine::new(face).with_raw_data(crate::text::box_face::shaping_font()));
        (engines.len() - 1, String::new(), false)
    }

    /// Fonts for the script items of `text` as Qt chooses them (see the module documentation):
    /// each script's item font and the fallbacks of the characters it lacks, listed per script,
    /// loaded as far as the text needs them, appended to `engines`; the chain of each script is
    /// kept in the primary engine ([`FontEngine::with_item_chains`]).
    pub(super) fn append_script_fallback_engines(
        &mut self,
        engines: &mut Vec<FontEngine>,
        text: &str,
        font: &Font,
    ) {
        use crate::text::glyph_layout::{ScriptChains, emoji_sequence_flags, item_script_key};
        if engines.is_empty() || !is_fontconfig_available() {
            return;
        }
        let style = FaceStyle::of(font);
        let exact = self.exact_family(font, style);
        let own = exact.clone().unwrap_or_else(|| self.primary_family(font));
        let slant = slant_of(style);
        // Per script key: the item font (index in `engines`, family, fixed pitch), the style name
        // its FreeType face has, the fallback list of that family and the fallbacks found so far
        // with their position in the list.
        struct Item {
            engine: usize,
            family: String,
            style_name: String,
            list: Option<Vec<String>>,
            found: Vec<(usize, usize)>,
        }
        let mut chains_of: HashMap<&'static str, Item> = HashMap::new();
        let mut misses: HashSet<(&'static str, char)> = HashSet::new();
        // The fallback families loaded so far (by folded name and the style name asked for) and
        // where their engine is in `engines`; `None` for one that cannot be loaded.
        let mut loaded: HashMap<(String, String), Option<usize>> = HashMap::new();
        let mut fixed_of: HashMap<&'static str, bool> = HashMap::new();
        // `QFontDatabasePrivate::load` (qfontdatabase.cpp:2947-2953): once a script's font is a
        // symbol font, every script that has no font yet gets it too; a symbol font is not
        // wrapped in a `QFontEngineMulti` (qfontdatabase.cpp:835), so its items have no fallback.
        let is_symbol = |engine: &FontEngine| {
            engine.raw_data.as_ref().is_some_and(|raw| {
                crate::text::outline_face::OutlineFace::is_symbol_font(
                    raw.as_slice(),
                    engine.face_index,
                )
            })
        };
        let mut symbol_everywhere: Option<(usize, String, bool)> = None;
        let items = crate::text::shaping_plan::script_items(text);
        let emoji_flags = emoji_sequence_flags(text);
        let mut position = 0usize;
        for (n, &(start, script)) in items.iter().enumerate() {
            let end = items.get(n + 1).map_or(text.len(), |next| next.0);
            for ch in text[start..end].chars() {
                let index = position;
                position += 1;
                let key = item_script_key(ch, script, emoji_flags[index]);
                if !chains_of.contains_key(key) {
                    let (engine, family, fixed) = match &symbol_everywhere {
                        Some(symbol) => symbol.clone(),
                        None => self.item_font(engines, font, style, key, &own, exact.is_some()),
                    };
                    if symbol_everywhere.is_none() && is_symbol(&engines[engine]) {
                        symbol_everywhere = Some((engine, family.clone(), fixed));
                    }
                    fixed_of.insert(key, fixed);
                    // `QFontEngineMulti::loadEngine` asks for each fallback family with the
                    // `fontDef` of the item font, whose style name is that of its FreeType face
                    // (`QFontEngineFT::init`): `bestStyle` then takes the fallback family's style
                    // of that name, whatever weight and slant were asked for.
                    let style_name = engines[engine]
                        .raw_data
                        .as_ref()
                        .map(|raw| freetype_style_name(raw.as_slice(), engines[engine].face_index))
                        .unwrap_or_default();
                    chains_of.insert(
                        key,
                        Item {
                            engine,
                            family,
                            style_name,
                            list: None,
                            found: Vec::new(),
                        },
                    );
                }
                let item = chains_of.get_mut(key).expect("inserted above");
                if crate::text::glyph_layout::skips_fallback(ch)
                    || symbol_everywhere
                        .as_ref()
                        .is_some_and(|symbol| symbol.0 == item.engine)
                    || engines[item.engine].face.glyph_index(ch) != 0
                    || !misses.insert((key, ch))
                {
                    continue;
                }
                // `QFontDatabasePrivate::loadEngine`: a fixed pitch family asks for a typewriter
                // font.
                let hint = if font.style_hint == StyleHint::AnyStyle && fixed_of[key] {
                    StyleHint::TypeWriter
                } else {
                    font.style_hint
                };
                if item.list.is_none() {
                    let list = self
                        .linux
                        .registry(&self.search_paths)
                        .fallbacks_for_family(&item.family, slant, hint, key);
                    item.list = Some(list);
                }
                let list = item.list.clone().unwrap_or_default();
                let style_name = item.style_name.clone();
                for (position, name) in list.iter().enumerate() {
                    let slot_key = (name.to_lowercase(), style_name.clone());
                    let engine_index = match loaded.get(&slot_key) {
                        Some(&slot) => slot,
                        None => {
                            if !self
                                .linux
                                .registry(&self.search_paths)
                                .fallback_may_have(name, ch)
                            {
                                continue;
                            }
                            let slot =
                                self.fallback_engine_index(engines, name, style, &style_name);
                            loaded.insert(slot_key, slot);
                            slot
                        }
                    };
                    let Some(engine_index) = engine_index else {
                        continue;
                    };
                    if engines[engine_index].face.glyph_index(ch) == 0 {
                        continue;
                    }
                    let item = chains_of.get_mut(key).expect("present");
                    if !item.found.iter().any(|&(_, i)| i == engine_index) {
                        item.found.push((position, engine_index));
                    }
                    break;
                }
            }
        }
        let mut chains = ScriptChains::new();
        for (key, mut item) in chains_of {
            item.found.sort_by_key(|&(position, _)| position);
            let mut chain = vec![item.engine];
            chain.extend(item.found.into_iter().map(|(_, index)| index));
            chains.insert(key, chain);
        }
        engines[0] = engines[0].clone().with_item_chains(chains);
    }

    /// The index in `engines` of the engine for the family `name` asked for with `style` and
    /// `style_name` (`QFontEngineMulti::loadEngine`), loading and appending it when it is not
    /// there.
    fn fallback_engine_index(
        &mut self,
        engines: &mut Vec<FontEngine>,
        name: &str,
        style: FaceStyle,
        style_name: &str,
    ) -> Option<usize> {
        if !style_name.is_empty() {
            let request = request_of(style);
            let registry = self.linux.registry(&self.search_paths);
            let named = registry.find(
                name,
                "",
                &Request {
                    style_name,
                    ..request
                },
                &[],
            );
            if named.is_some() && named != registry.find(name, "", &request, &[]) {
                // The style name picks another face than weight and slant do: cached apart.
                let key = format!(
                    "{}\u{1}{style_name}",
                    style.cache_key(&name.to_ascii_lowercase(), StyleHint::AnyStyle)
                );
                let face = match self.cache.get(&key) {
                    Some(face) => Arc::clone(face),
                    None => {
                        let found = self.resolve_registered_named(name, style, style_name)?;
                        self.store_resolved(key.clone(), found)
                    }
                };
                if let Some(index) = engines.iter().position(|e| Arc::ptr_eq(&e.face, &face)) {
                    return Some(index);
                }
                let engine = match self.engines.get(&key) {
                    Some(engine) if Arc::ptr_eq(&engine.face, &face) => engine.clone(),
                    _ => {
                        let mut engine =
                            FontEngine::new(Arc::clone(&face)).with_face_index(face.face_index());
                        if let Some(raw) = self.raw_cache.get(&key).cloned() {
                            engine = engine.with_raw_data(raw);
                        }
                        self.engines.insert(key, engine.clone());
                        engine
                    }
                };
                engines.push(engine);
                return Some(engines.len() - 1);
            }
        }
        let face = self.load_font_styled(name, style, StyleHint::AnyStyle)?;
        if let Some(index) = engines.iter().position(|e| Arc::ptr_eq(&e.face, &face)) {
            return Some(index);
        }
        self.try_add_engine(engines, name, style, StyleHint::AnyStyle)
            .then(|| engines.len() - 1)
    }

    /// The engines a font gets when there is no text to list the fallback fonts of.
    pub(super) fn append_probe_fallback_engines(
        &mut self,
        engines: &mut Vec<FontEngine>,
        font: &Font,
    ) {
        self.append_script_fallback_engines(engines, FULL_CHAIN_PROBE, font);
    }
}
