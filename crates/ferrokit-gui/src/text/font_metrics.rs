// SPDX-License-Identifier: LGPL-3.0-only
use crate::geometry::primitives::RectF;
use crate::text::font::{Font, FontStyle, FontTag, FontWeight, HintingPreference};
use crate::text::glyph_face::LineMetrics;
use std::cell::RefCell;
use std::collections::HashMap;

/// Identity of a shaped-width query: everything `horizontal_advance_exact` depends on.
struct AdvanceKey {
    text: String,
    family: String,
    size_bits: u32,
    weight: FontWeight,
    style: FontStyle,
    /// `(tag value, feature value)` of every feature set, in tag order.
    features: Vec<(u32, u32)>,
    letter_spacing_bits: u32,
    hinting_preference: HintingPreference,
}

impl AdvanceKey {
    fn new(text: &str, font: &Font) -> Self {
        Self {
            text: text.to_owned(),
            family: font.family.clone(),
            size_bits: font.size.to_bits(),
            weight: font.weight,
            style: font.style,
            features: Self::features_of(font).collect(),
            letter_spacing_bits: font.letter_spacing.to_bits(),
            hinting_preference: font.hinting_preference,
        }
    }

    fn features_of(font: &Font) -> impl Iterator<Item = (u32, u32)> + '_ {
        font.features()
            .iter()
            .map(|(tag, value)| (tag.value(), *value))
    }

    /// Whether this key is the query `(text, font)`, compared without building a key.
    fn is(&self, text: &str, font: &Font) -> bool {
        self.text == text
            && self.family == font.family
            && self.size_bits == font.size.to_bits()
            && self.weight == font.weight
            && self.style == font.style
            && self.letter_spacing_bits == font.letter_spacing.to_bits()
            && self.features.iter().copied().eq(Self::features_of(font))
            && self.hinting_preference == font.hinting_preference
    }

    /// Hash of the query `(text, font)` from borrowed data, so a cache hit allocates nothing.
    fn hash_of(text: &str, font: &Font) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        font.family.hash(&mut h);
        font.size.to_bits().hash(&mut h);
        font.weight.hash(&mut h);
        font.style.hash(&mut h);
        for feature in Self::features_of(font) {
            feature.hash(&mut h);
        }
        font.letter_spacing.to_bits().hash(&mut h);
        font.hinting_preference.hash(&mut h);
        h.finish()
    }
}

struct AdvanceCache {
    generation: u64,
    /// Entries by [`AdvanceKey::hash_of`]; a bucket holds the keys that share a hash.
    map: HashMap<u64, Vec<(AdvanceKey, f32)>>,
    /// Number of entries over all buckets.
    len: usize,
}

/// Upper bound on cached strings; the cache is simply cleared when it is exceeded.
const ADVANCE_CACHE_MAX: usize = 4096;

thread_local! {
    static ADVANCE_CACHE: RefCell<AdvanceCache> = RefCell::new(AdvanceCache {
        generation: 0,
        map: HashMap::new(),
        len: 0,
    });
    static LINE_CACHE: RefCell<LineCache> =
        RefCell::new(LineCache { generation: 0, map: HashMap::new() });
}

/// `qRound` of a value: halves round up (`QFixed::toInt`), also for negative leadings.
fn q_round(value: f32) -> f32 {
    (value + 0.5).floor()
}

/// Identity of a line-metrics query.
#[derive(Hash, PartialEq, Eq)]
struct LineKey {
    family: String,
    size_bits: u32,
    weight: FontWeight,
    style: FontStyle,
}

struct LineCache {
    generation: u64,
    map: HashMap<LineKey, (Option<LineMetrics>, Option<f32>)>,
}

/// Font metrics engine (`QFontMetrics` / `QFontMetricsF` equivalent).
///
/// Used for text layout, centering, bounding box measurement, and eliding.
#[derive(Debug, Clone, PartialEq)]
pub struct FontMetrics {
    /// `QFontMetrics::ascent`: baseline to the top of a line, whole pixels.
    pub ascent: f32,
    /// `QFontMetrics::descent`: baseline to the bottom of a line, whole pixels.
    pub descent: f32,
    /// `QFontMetrics::leading`, the inter-line spacing the font asks for.
    pub line_gap: f32,
    /// `QFontMetrics::height` = `ascent + descent` ([`from_font`](Self::from_font)); with
    /// [`new`](Self::new), which has no engine to ask, it also holds the `line_gap`.
    pub height: f32,
    /// `QFontMetricsF::averageCharWidth`: the engine's own 26.6 value
    /// ([`rounded_average_char_width`](Self::rounded_average_char_width) is `QFontMetrics`'s).
    pub average_char_width: f32,
}

impl FontMetrics {
    /// Creates a new `FontMetrics` with explicit values.
    pub fn new(ascent: f32, descent: f32, line_gap: f32, average_char_width: f32) -> Self {
        let height = ascent + descent + line_gap;
        Self {
            ascent,
            descent,
            line_gap,
            height,
            average_char_width,
        }
    }

    /// Derives the metrics of `font` as `QFontMetrics` reports them.
    ///
    /// The face the font resolves to (its in-memory data, else the installed family) gives the
    /// numbers, read from the font's tables the way Qt's engine does:
    /// [`GlyphFace::line_metrics`] and [`GlyphFace::average_char_width`]. `ascent`, `descent` and
    /// `line_gap` (`leading()`) are those engine values rounded to whole pixels (`qRound`) and
    /// `height` is `ascent + descent`, which `QFontMetrics::height` is (`line_gap` is not part of
    /// it; `QFontMetrics::lineSpacing` is [`line_spacing`](Self::line_spacing)). Only a font no
    /// face resolves for has made-up numbers (0.8, 0.2 and 0.1 of the size, 0.6 for the average
    /// character width), since there is nothing to measure.
    ///
    /// [`GlyphFace::line_metrics`]: crate::text::glyph_face::GlyphFace::line_metrics
    /// [`GlyphFace::average_char_width`]: crate::text::glyph_face::GlyphFace::average_char_width
    pub fn from_font(font: &Font) -> Self {
        let size = font.size;
        let (line, average) = Self::face_metrics(font);
        let average_char_width = average.unwrap_or(size * 0.6);
        if let Some(line) = line {
            let engine = Self::engine_line_metrics(line, font);
            let (ascent, descent) = (q_round(engine.ascent), q_round(engine.descent));
            return Self {
                ascent,
                descent,
                line_gap: q_round(engine.leading),
                height: ascent + descent,
                average_char_width,
            };
        }
        let ascent = size * 0.8;
        let descent = size * 0.2;
        let line_gap = size * 0.1;
        Self::new(ascent, descent, line_gap, average_char_width)
    }

    /// `QFontMetrics::lineSpacing`: `ascent + descent + leading`, each rounded.
    #[inline]
    pub fn line_spacing(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }

    /// `QFontMetrics::averageCharWidth`, the engine's width rounded to a whole pixel.
    #[inline]
    pub fn rounded_average_char_width(&self) -> f32 {
        q_round(self.average_char_width)
    }

    /// `QFontMetricsF::capHeight`: the face's OS/2 cap height as Qt's Windows engines report it,
    /// truncated to 26.6 fixed point (`DESIGN_TO_LOGICAL` / `QFixed` division). `None` when the
    /// font cannot be resolved.
    pub fn cap_height(font: &Font) -> Option<f32> {
        if font.font_data.is_some() || font.size <= 0.0 {
            return None;
        }
        let cap = crate::text::font_database::primary_face_cap_height(font)?;
        Some((cap * 64.0).floor() / 64.0)
    }

    /// Height of one line of text as Qt's text layout measures it (`QFontMetrics::boundingRect`,
    /// hence `QLabel::sizeHint`), before the final `ceil`: the engine's ascent plus its descent,
    /// which are 26.6 values (a 12.5 + 3.5 line is 16 px) where the engine has sub-pixel positions
    /// and whole pixels where it has not. `QFontMetrics::height` rounds the two separately (13 + 4
    /// = 17 px for that line), so it can be taller; Microsoft JhengHei UI at 15px under Qt's
    /// DirectWrite engine is 15.234375 + 3.8125 = 19.046875, a 20px line against `height()`'s 19.
    pub fn layout_height(font: &Font) -> f32 {
        match Self::face_metrics(font).0 {
            Some(line) => {
                let engine = Self::engine_line_metrics(line, font);
                engine.ascent + engine.descent
            }
            None => Self::from_font(font).height,
        }
    }

    /// The height of a line of text in `font` (`QTextLine::height`, qtextlayout.cpp:1524:
    /// [`layout_height`](Self::layout_height) rounded up).
    pub fn line_height(font: &Font) -> f32 {
        Self::layout_height(font).ceil()
    }

    /// `QFontMetricsF::ascent`, `descent` and `leading` of `font`: the engine's own 26.6 values
    /// (`None` for a font no face resolves for).
    pub fn engine_metrics(font: &Font) -> Option<LineMetrics> {
        Self::face_metrics(font)
            .0
            .map(|line| Self::engine_line_metrics(line, font))
    }

    /// `QFontMetricsF::leading`: the engine's own value, a 26.6 number where it has sub-pixel
    /// positions and a whole one otherwise (negative when rounding the ascent and descent took
    /// more than rounding the whole line, as for 12.5 + 3.5), 0 for a font no face resolves for.
    pub fn layout_leading(font: &Font) -> f32 {
        Self::face_metrics(font)
            .0
            .map_or(0.0, |line| Self::engine_line_metrics(line, font).leading)
    }

    /// The distance from one line of a plain text to the next, as `qt_format_text` stacks them
    /// (qpainter.cpp:7370-7388, what `QLabel` and `QFontMetrics::boundingRect` lay a text out
    /// by): the leading is added between lines and the top of each line rounded up
    /// (`height = qCeil(height + leading)`), so a text of `n` lines is
    /// `(n - 1) * line_pitch + line_height` tall.
    pub fn line_pitch(font: &Font) -> f32 {
        (Self::layout_height(font) + Self::layout_leading(font)).ceil()
    }

    /// Ascent and descent, in pixels, of one line of `text` when it uses fallback font engines,
    /// else `None` (the line is the primary font's own). Qt's line takes the largest ascent and the
    /// largest descent of the engines its text items use, fallback engines included
    /// (`QTextEngine::shapeText`, qtextengine.cpp:1479-1480: the item's ascent becomes the larger of
    /// its own and the actual engine's, descent likewise). The values are the engines' own, as
    /// [`layout_height`](Self::layout_height) takes them.
    pub fn fallback_line_metrics(font: &Font, text: &str) -> Option<(f32, f32)> {
        if font.font_data.is_some() || font.size <= 0.0 || text.is_empty() {
            return None;
        }
        let engines = crate::text::font_database::resolve_font_engines_for_text_global(font, text);
        if engines.len() < 2 {
            return None;
        }
        let runs = crate::text::glyph_layout::GlyphLayout::partition_into_runs(text, &engines);
        if runs.iter().all(|run| run.engine_index == 0) {
            return None;
        }
        let (mut ascent, mut descent) = (0.0f32, 0.0f32);
        for run in &runs {
            // A face with no metrics does not enter the stacking; the others still do.
            let Some(line) = engines[run.engine_index].face.line_metrics(font.size) else {
                continue;
            };
            let engine = Self::engine_line_metrics(line, font);
            ascent = ascent.max(engine.ascent);
            descent = descent.max(engine.descent);
        }
        Some((ascent, descent))
    }

    /// [`layout_height`](Self::layout_height) of one line of `text`, with fallback engines
    /// ([`fallback_line_metrics`](Self::fallback_line_metrics)).
    pub fn layout_height_for_text(font: &Font, text: &str) -> f32 {
        match Self::fallback_line_metrics(font, text) {
            Some((ascent, descent)) => ascent + descent,
            None => Self::layout_height(font),
        }
    }

    /// What the face `font` resolves to says about it: its line metrics and average character
    /// width. System fonts are remembered per family, size, weight and style (until the font set
    /// changes); in-memory fonts are not (their identity is not part of the key).
    fn face_metrics(font: &Font) -> (Option<LineMetrics>, Option<f32>) {
        if font.size <= 0.0 {
            return (None, None);
        }
        if font.font_data.is_some() {
            return crate::text::font_database::primary_face_metrics(font);
        }
        let generation = crate::text::font_database::font_generation();
        let key = LineKey {
            family: font.family.clone(),
            size_bits: font.size.to_bits(),
            weight: font.weight,
            style: font.style,
        };
        let hit = LINE_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.generation != generation {
                c.map.clear();
                c.generation = generation;
            }
            c.map.get(&key).copied()
        });
        if let Some(found) = hit {
            return found;
        }
        let found = crate::text::font_database::primary_face_metrics(font);
        LINE_CACHE.with(|c| c.borrow_mut().map.insert(key, found));
        found
    }

    /// The engine's height metrics as Qt holds them: the face's own values where the engine has
    /// sub-pixel positions, else rounded to whole pixels with the leading taking up the rounding
    /// of the line (`QFontEngine::initializeHeightMetrics`).
    fn engine_line_metrics(line: LineMetrics, font: &Font) -> LineMetrics {
        if crate::text::font_database::font_has_subpixel_positions(font) {
            line
        } else {
            line.rounded()
        }
    }
    /// Interline leading spacing (`QFontMetrics::leading`).
    #[inline]
    pub fn leading(&self) -> f32 {
        self.line_gap
    }

    /// Calculates the horizontal advance width of a string (`QFontMetricsF::horizontalAdvance`).
    pub fn horizontal_advance(&self, text: &str, font: &Font) -> f32 {
        let mut total_width = 0.0;

        for ch in text.chars() {
            let w = if font.feature_value(FontTag::TNUM) != 0 && ch.is_ascii_digit() {
                self.average_char_width * 1.05
            } else {
                match ch {
                    // No width in Qt in any font; text made of such characters alone shapes to
                    // no width and lands here.
                    ch if crate::text::glyph_layout::is_zero_width(ch) => 0.0,
                    ' ' => self.average_char_width * 0.5,
                    '.' | ',' | ':' | ';' | '!' | '|' | '\'' | '`' => {
                        self.average_char_width * 0.35
                    }
                    'i' | 'l' | 'j' | 'I' | 't' => self.average_char_width * 0.45,
                    'w' | 'm' | 'W' | 'M' => self.average_char_width * 1.3,
                    ch if ch.is_ascii() => self.average_char_width,
                    _ => self.average_char_width * 1.8,
                }
            };
            total_width += w;
        }

        total_width
    }
    /// Calculates the exact horizontal advance width using the global font database and OpenType shaper.
    /// Eliminates heuristic rounding discrepancies between layout bounding boxes and rasterized glyphs.
    pub fn horizontal_advance_exact(&self, text: &str, font: &Font) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        // Shaping costs ~100 us per call and layout asks for the same widths on every pass.
        // Fonts carrying in-memory data are not cached (their identity is not part of the key).
        if font.font_data.is_some() {
            return self.shape_advance(text, font);
        }
        let generation = crate::text::font_database::font_generation();
        let hash = AdvanceKey::hash_of(text, font);
        let hit = ADVANCE_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.generation != generation {
                c.map.clear();
                c.len = 0;
                c.generation = generation;
            }
            c.map
                .get(&hash)?
                .iter()
                .find(|(key, _)| key.is(text, font))
                .map(|&(_, w)| w)
        });
        if let Some(w) = hit {
            return w;
        }
        let w = {
            let _t = crate::startup_trace::span_min(0.5, || {
                format!(
                    "shape_advance miss {:?} {:?} {}px",
                    text, font.family, font.size
                )
            });
            self.shape_advance(text, font)
        };
        ADVANCE_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len >= ADVANCE_CACHE_MAX {
                c.map.clear();
                c.len = 0;
            }
            c.map
                .entry(hash)
                .or_default()
                .push((AdvanceKey::new(text, font), w));
            c.len += 1;
        });
        w
    }

    fn shape_advance(&self, text: &str, font: &Font) -> f32 {
        // The line and paragraph separators end Qt's shaping items and have no width
        // (`QTextEngine::itemize` flags them `LineOrParagraphSeparator`); no font shapes them.
        if text.contains(['\u{2028}', '\u{2029}']) {
            return text
                .split(['\u{2028}', '\u{2029}'])
                .filter(|piece| !piece.is_empty())
                .map(|piece| self.shape_advance(piece, font))
                .sum();
        }
        let engines = crate::text::font_database::resolve_font_engines_for_text_global(font, text);
        if !engines.is_empty() {
            let layout =
                crate::text::glyph_layout::GlyphLayout::shape_with_engines(text, font, &engines);
            if layout.width > 0.0 {
                return layout.width;
            }
        }

        self.horizontal_advance(text, font)
    }

    /// The advance of every grapheme of `text`, shaped as one string so kerning, ligatures and
    /// contextual forms across grapheme boundaries are measured as Qt measures them
    /// (`QTextEngine` shapes a whole item, then divides it over the characters). The advances
    /// add up to `horizontal_advance_exact(text, font)`.
    pub fn grapheme_advances(&self, text: &str, font: &Font) -> Vec<f32> {
        use unicode_segmentation::UnicodeSegmentation;
        let mut starts: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
        starts.push(text.len());
        self.grapheme_advances_at(text, &starts, font)
    }

    /// [`grapheme_advances`](Self::grapheme_advances) for a text whose grapheme start offsets
    /// (followed by the text length) the caller already has.
    pub(crate) fn grapheme_advances_at(
        &self,
        text: &str,
        starts: &[usize],
        font: &Font,
    ) -> Vec<f32> {
        match Self::shape_paragraph(text, font) {
            Some((_, layout)) => layout.grapheme_advances(starts),
            None => self.heuristic_grapheme_advances(text, starts, font),
        }
    }

    /// Shapes `text` as one paragraph: the font engines resolved for it and the glyph layout they
    /// produce. `None` when no font resolves or the shaped text has no width, where callers fall
    /// back to [`heuristic_grapheme_advances`](Self::heuristic_grapheme_advances). Advances and
    /// drawing both take the layout from here so the pen positions they use are the same.
    pub(crate) fn shape_paragraph(
        text: &str,
        font: &Font,
    ) -> Option<(
        Vec<crate::text::glyph_layout::FontEngine>,
        crate::text::glyph_layout::GlyphLayout,
    )> {
        Self::shape_paragraph_directed(text, font, None)
    }

    /// [`shape_paragraph`](Self::shape_paragraph) with the direction fixed (see
    /// [`GlyphLayout::shape_with_engines_directed`](crate::text::glyph_layout::GlyphLayout::shape_with_engines_directed)).
    pub(crate) fn shape_paragraph_directed(
        text: &str,
        font: &Font,
        right_to_left: Option<bool>,
    ) -> Option<(
        Vec<crate::text::glyph_layout::FontEngine>,
        crate::text::glyph_layout::GlyphLayout,
    )> {
        if text.is_empty() {
            return None;
        }
        let engines = crate::text::font_database::resolve_font_engines_for_text_global(font, text);
        if engines.is_empty() {
            return None;
        }
        let layout = crate::text::glyph_layout::GlyphLayout::shape_with_engines_directed(
            text,
            font,
            &engines,
            right_to_left,
        );
        (layout.width > 0.0).then_some((engines, layout))
    }

    /// Per-grapheme advances from the character-class heuristic, for text no font shapes.
    pub(crate) fn heuristic_grapheme_advances(
        &self,
        text: &str,
        starts: &[usize],
        font: &Font,
    ) -> Vec<f32> {
        starts
            .windows(2)
            .map(|w| self.horizontal_advance(&text[w[0]..w[1]], font))
            .collect()
    }

    /// Calculates the exact bounding rectangle of a string using OpenType shaping.
    pub fn bounding_rect_exact(&self, text: &str, font: &Font) -> RectF {
        let width = self.horizontal_advance_exact(text, font);
        RectF {
            x: 0.0,
            y: -self.ascent,
            width,
            height: self.height,
        }
    }

    /// Calculates the bounding rectangle of a string (`QFontMetricsF::boundingRect`).
    pub fn bounding_rect(&self, text: &str, font: &Font) -> RectF {
        let width = self.horizontal_advance(text, font);
        RectF {
            x: 0.0,
            y: -self.ascent,
            width,
            height: self.height,
        }
    }

    /// Returns an elided string ending with "..." if text exceeds max_width (`QFontMetricsF::elidedText`).
    pub fn elided_text(&self, text: &str, max_width: f32, font: &Font) -> String {
        if self.horizontal_advance(text, font) <= max_width {
            return text.to_string();
        }
        let ellipsis = "...";
        let ellipsis_width = self.horizontal_advance(ellipsis, font);
        if ellipsis_width >= max_width {
            return String::new();
        }

        let mut result = String::new();
        for ch in text.chars() {
            let mut candidate = result.clone();
            candidate.push(ch);
            if self.horizontal_advance(&candidate, font) + ellipsis_width > max_width {
                break;
            }
            result.push(ch);
        }
        result.push_str(ellipsis);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::font::Font;

    /// Default ignorable characters and the line and paragraph separators have no width in Qt
    /// (PySide6 6.11.2, oracle font and Arial: `QFontMetricsF::horizontalAdvance(c)` is 0 and
    /// `"a" + c + "b"` is as wide as `"ab"` for every character here), whatever glyph a fallback
    /// font of the platform has for them. Text made of them alone shapes to no width and falls
    /// back to the heuristic, which must not give them one either. Every failing character is
    /// reported at once.
    #[test]
    fn zero_width_characters_have_no_width_as_in_qt() {
        let font = Font::new("FerrokitOracleText", 16.0).with_font_data(
            crate::text::font::SharedFontData::from_vec(
                include_bytes!("../../tests/data/oracle_text.ttf").to_vec(),
            ),
        );
        let metrics = FontMetrics::from_font(&font);
        let ab = metrics.horizontal_advance_exact("ab", &font);
        let ab_graphemes = metrics.grapheme_advances("ab", &font);
        let mut wrong = Vec::new();
        for c in [
            '\u{2029}', '\u{2028}', '\u{61c}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{fe00}', '\u{fe0f}',
            '\u{ad}', '\u{200b}', '\u{200d}', '\u{2060}', '\u{feff}',
        ] {
            let alone = c.to_string();
            let between = format!("a{c}b");
            let heuristic = metrics.horizontal_advance(&alone, &font);
            let exact = metrics.horizontal_advance_exact(&alone, &font);
            let exact_between = metrics.horizontal_advance_exact(&between, &font);
            let graphemes = metrics.grapheme_advances(&between, &font);
            // A joiner or variation selector extends the `a` into one grapheme.
            let expected_graphemes = if graphemes.len() == 2 {
                ab_graphemes.clone()
            } else {
                vec![ab_graphemes[0], 0.0, ab_graphemes[1]]
            };
            if heuristic != 0.0
                || exact != 0.0
                || exact_between != ab
                || graphemes != expected_graphemes
            {
                wrong.push(format!(
                    "{c:?}: heuristic {heuristic}, exact {exact}, exact a_b {exact_between} \
                     (ab {ab}), grapheme advances {graphemes:?} (want {expected_graphemes:?})"
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        assert!(metrics.horizontal_advance("a", &font) > 0.0);
    }

    /// The cache is looked up with the borrowed query: a key matches its own query and no query
    /// that differs in any field, and equal queries hash alike.
    #[test]
    fn advance_keys_match_only_their_own_query() {
        let font = Font::new("Arial", 12.0).with_feature(FontTag::TNUM, 1);
        let key = AdvanceKey::new("Wa", &font);
        assert!(key.is("Wa", &font));
        assert_eq!(
            AdvanceKey::hash_of("Wa", &font),
            AdvanceKey::hash_of("Wa", &font.clone())
        );
        let mut spaced = font.clone();
        spaced.letter_spacing = 1.0;
        let others = [
            Font::new("Arial", 12.0),
            Font::new("Arial", 12.0).with_feature(FontTag::TNUM, 0),
            Font::new("Arial", 13.0).with_feature(FontTag::TNUM, 1),
            Font::new("Arial Black", 12.0).with_feature(FontTag::TNUM, 1),
            font.clone().with_weight(FontWeight::Bold),
            font.clone().with_style(FontStyle::Italic),
            spaced,
        ];
        for other in &others {
            assert!(!key.is("Wa", other), "{other:?}");
        }
        assert!(!key.is("Wa ", &font));
        assert!(!key.is("W", &font));
    }

    /// Whole-pixel ascent/descent of Segoe UI as `QFontMetrics` reports them (measured with
    /// PySide6 6.11.2 on Windows: 11px -> 12/3 = 15, 12px -> 13/3 = 16).
    #[test]
    fn test_font_metrics_follow_the_face_like_qt() {
        if !std::path::Path::new("C:/Windows/Fonts/segoeui.ttf").exists() {
            return;
        }
        let m11 = FontMetrics::from_font(&Font::new("Segoe UI", 11.0));
        assert_eq!((m11.ascent, m11.descent, m11.height), (12.0, 3.0, 15.0));
        let m12 = FontMetrics::from_font(&Font::new("Segoe UI", 12.0));
        assert_eq!((m12.ascent, m12.descent, m12.height), (13.0, 3.0, 16.0));
    }

    #[test]
    fn test_font_metrics_bounding_rect() {
        let font = Font::new("Segoe UI", 10.0);
        let metrics = FontMetrics::from_font(&font);
        let rect = metrics.bounding_rect("Hello", &font);

        assert_eq!(rect.x, 0.0);
        assert_eq!(rect.y, -metrics.ascent);
        assert_eq!(rect.height, metrics.height);
        assert!(rect.width > 0.0);
    }

    #[test]
    fn test_tabular_numbers_stability() {
        let font_tnum = Font::new("Consolas", 12.0).with_feature(FontTag::TNUM, 1);
        let metrics = FontMetrics::from_font(&font_tnum);

        let w1 = metrics.horizontal_advance("1111", &font_tnum);
        let w2 = metrics.horizontal_advance("8888", &font_tnum);
        assert_eq!(
            w1, w2,
            "Tabular numbers advance for 1111 and 8888 must match"
        );
    }

    #[test]
    fn test_elided_text() {
        let font = Font::new("Segoe UI", 12.0);
        let metrics = FontMetrics::from_font(&font);
        let full_text = "Claude HUD Monitor Real-Time Status Notification";

        let elided = metrics.elided_text(full_text, 100.0, &font);
        assert!(elided.ends_with("..."));
        assert!(metrics.horizontal_advance(&elided, &font) <= 100.0);

        let short_text = "OK";
        assert_eq!(metrics.elided_text(short_text, 200.0, &font), "OK");
    }
}
