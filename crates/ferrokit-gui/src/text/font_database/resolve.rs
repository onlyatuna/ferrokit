// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

impl FontDatabase {
    /// Resolves the primary font engine and its platform-aware fallback chain (`QFontEngineMulti` equivalent).
    ///
    /// The resulting engines are ordered:
    /// 1. Primary requested font (or in-memory custom font if provided in `font.font_data`)
    /// 2. CJK / Multilingual script fallback fonts
    /// 3. Symbol and Emoji fallback fonts
    ///
    /// On Linux, which fallback fonts there are depends on the script of the text (Qt asks
    /// fontconfig per script item); without text, those of one character of each script are listed
    /// (see [`resolve_font_engines_for_text`](Self::resolve_font_engines_for_text)).
    pub fn resolve_font_engines(&mut self, font: &Font) -> Vec<FontEngine> {
        let _t = crate::startup_trace::span_min(0.5, || {
            format!("resolve_font_engines({:?})", font.family)
        });
        let mut engines = self.resolve_primary_engine(font);
        #[cfg(all(unix, not(target_os = "macos")))]
        self.append_probe_fallback_engines(&mut engines, font);
        #[cfg(not(all(unix, not(target_os = "macos"))))]
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
        #[cfg(all(unix, not(target_os = "macos")))]
        self.append_script_fallback_engines(&mut engines, text, font);
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        self.append_fallback_engines(&mut engines, Some(text), FaceStyle::of(font));
        engines
    }

    /// The font requested by `font` alone (empty when the family cannot be loaded).
    pub(super) fn resolve_primary_engine(&mut self, font: &Font) -> Vec<FontEngine> {
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
    pub(super) fn memory_face(
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
    pub(super) fn primary_face(&mut self, font: &Font) -> Option<SharedGlyphFace> {
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
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    pub(super) fn append_fallback_engines(
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

        for slot in SLOTS {
            if let Some(text) = needed_for {
                let covered = !engines.is_empty()
                    && !crate::text::glyph_layout::GlyphLayout::has_emoji_sequence(text)
                    && text.chars().all(|ch| {
                        matches!(ch, ' ' | '\t' | '\n' | '\r')
                            || crate::text::glyph_layout::skips_fallback(ch)
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
    ///
    /// Which family a character takes is looked up once per font request and character and
    /// remembered (see [`FallbackMemo`]); a repeated string only replays the engines it needs.
    #[cfg(windows)]
    pub(super) fn append_script_fallback_engines(
        &mut self,
        engines: &mut Vec<FontEngine>,
        text: &str,
        font: &Font,
    ) -> bool {
        use crate::text::directwrite_catalog as dw;
        use crate::text::glyph_layout::{
            ScriptChains, emoji_sequence_flags, fallback_script_key, skips_fallback,
        };
        let Some(catalog) = dw::catalog() else {
            return false;
        };
        if engines.is_empty() {
            return false;
        }
        let style = FaceStyle::of(font);
        self.fallback_memo.for_generation(font_generation());
        let context = self.fallback_memo.context(catalog, font);
        let primary = engines[0].clone();
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
                    || skips_fallback(ch)
                    || (primary.face.glyph_index(ch) != 0 && !emoji_flags[index])
                {
                    continue;
                }
                let key = fallback_script_key(ch, script, emoji_flags[index]);
                if !misses.insert((key, ch)) {
                    continue;
                }
                let list_key = fallback_list_key(
                    context.hint,
                    style.italic,
                    context.symbol_family,
                    key,
                    script,
                );
                let pick = self.fallback_pick(catalog, list_key, style, ch);
                for (at, (list_position, name)) in pick.covering.iter().enumerate() {
                    let Some(engine) = self.fallback_engine_index(engines, name, style) else {
                        continue;
                    };
                    if !(pick.has_glyph && at + 1 == pick.covering.len()) {
                        continue;
                    }
                    let chain = found.entry(key).or_default();
                    if !chain.iter().any(|&(_, i)| i == engine) {
                        chain.push((*list_position, engine));
                    }
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

    /// The families of `list_key`'s list that cover `ch`, up to the first whose face has a glyph
    /// for it, remembered per request (see [`FallbackMemo`]).
    #[cfg(windows)]
    pub(super) fn fallback_pick(
        &mut self,
        catalog: &crate::text::directwrite_catalog::Catalog,
        list_key: crate::text::directwrite_catalog::FallbackListKey,
        style: FaceStyle,
        ch: char,
    ) -> Arc<FallbackPick> {
        let key = FallbackPickKey {
            list: list_key,
            style,
            ch,
        };
        if let Some(pick) = self.fallback_memo.picks.get(&key) {
            return pick.clone();
        }
        let list = windows_fallback_families(catalog, list_key);
        let mut covering = Vec::new();
        let mut has_glyph = false;
        for (position, name) in list.iter(catalog).enumerate() {
            if !catalog.covers(name, style.weight, style.italic, ch) {
                continue;
            }
            // The engine of a family is built from this face (`fallback_engine_index`).
            let Some(face) = self.load_font_styled(name, style, StyleHint::AnyStyle) else {
                continue;
            };
            covering.push((position, Arc::<str>::from(name)));
            if face.glyph_index(ch) != 0 {
                has_glyph = true;
                break;
            }
        }
        let pick = Arc::new(FallbackPick {
            covering,
            has_glyph,
        });
        self.fallback_memo.insert_pick(key, pick.clone());
        pick
    }

    /// The index in `engines` of the engine for the family `name`, loading and appending it when
    /// it is not there.
    #[cfg(windows)]
    pub(super) fn fallback_engine_index(
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

    pub(super) fn try_add_engine(
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
}
