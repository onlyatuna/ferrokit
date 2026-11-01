// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

/// Shared binary font data representation supporting zero-copy slicing and deduplication.
///
/// The bytes live behind an `Arc`, so every `FontEngine`/`Font` sharing a file shares one buffer.
/// The buffer is either heap memory ([`from_vec`](Self::from_vec)) or, on Windows, a read-only
/// mapping of the font file ([`from_file`](Self::from_file)). A mapping is backed by the file
/// itself: it is not charged to the process's commit, and only the pages actually read (a few
/// glyph tables out of a 16 MB CJK collection) become resident. Qt maps system font files the
/// same way.
#[derive(Clone)]
pub struct SharedFontData {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
}

impl SharedFontData {
    /// Creates a new `SharedFontData` from an owned byte vector.
    pub fn from_vec(vec: Vec<u8>) -> Self {
        Self {
            data: Arc::new(vec),
        }
    }

    /// Creates a `SharedFontData` wrapping an existing `Arc<Vec<u8>>`.
    pub fn from_arc(data: Arc<Vec<u8>>) -> Self {
        Self { data }
    }

    /// Loads the font file at `path`: on Windows maps it read-only (reading it into memory if it
    /// cannot be mapped), elsewhere reads it into memory.
    ///
    /// Windows cannot shrink a file while a view of it is mapped (`SetEndOfFile` fails with
    /// `ERROR_USER_MAPPED_FILE`), so every mapped page stays backed. On Linux and macOS any process
    /// may truncate a mapped file, after which touching the lost pages raises `SIGBUS`; a safe
    /// function cannot rule that out, so the bytes are copied there.
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            let file = std::fs::File::open(path)?;
            if file.metadata()?.len() > 0 {
                // SAFETY: the mapping is read-only and private, and Windows keeps the file from
                // shrinking under it (see above). Installed font files are not rewritten in
                // place [inferred], so the mapped bytes do not change while borrowed.
                if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
                    return Ok(Self {
                        data: Arc::new(map),
                    });
                }
            }
        }
        Ok(Self::from_vec(std::fs::read(path)?))
    }

    /// Returns the underlying byte slice.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        (*self.data).as_ref()
    }

    /// Returns the number of bytes in the font data.
    #[inline]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Returns `true` if the font data is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    /// Returns the strong reference count of this shared font buffer.
    #[inline]
    pub fn strong_count(&self) -> usize {
        Arc::strong_count(&self.data)
    }

    /// Checks if two `SharedFontData` share the exact same underlying memory buffer.
    #[inline]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.as_slice().as_ptr(), other.as_slice().as_ptr())
            && self.len() == other.len()
    }
}

impl std::fmt::Debug for SharedFontData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedFontData")
            .field("len", &self.len())
            .finish()
    }
}

impl std::ops::Deref for SharedFontData {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl AsRef<[u8]> for SharedFontData {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for SharedFontData {
    fn eq(&self, other: &Self) -> bool {
        self.ptr_eq(other) || self.as_slice() == other.as_slice()
    }
}

impl From<Vec<u8>> for SharedFontData {
    fn from(v: Vec<u8>) -> Self {
        Self::from_vec(v)
    }
}

impl From<Arc<Vec<u8>>> for SharedFontData {
    fn from(a: Arc<Vec<u8>>) -> Self {
        Self::from_arc(a)
    }
}
/// Font weight values matching Qt `QFont::Weight` and CSS standard numeric values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FontWeight {
    Thin = 100,
    ExtraLight = 200,
    Light = 300,
    #[default]
    Normal = 400,
    Medium = 500,
    SemiBold = 600,
    Bold = 700,
    ExtraBold = 800,
    Black = 900,
}

/// Font style matching Qt `QFont::Style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
    Oblique,
}

/// Font hinting preference matching Qt's `QFont::HintingPreference`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum HintingPreference {
    /// Use the default hinting style of the platform or font engine.
    #[default]
    PreferDefaultHinting = 0,
    /// Do not hint the font; sub-pixel positions are always supported.
    PreferNoHinting = 1,
    /// Hint vertically only (light hinting); sub-pixel horizontal positions are supported.
    PreferVerticalHinting = 2,
    /// Hint both horizontally and vertically (full hinting); sub-pixel positions are disabled.
    PreferFullHinting = 3,
}

/// A four-character OpenType feature tag (`QFont::Tag`), held as the big-endian `u32` of its
/// bytes, so `FontTag::new(b"tnum").value()` is Qt's `QFont::Tag("tnum").value()`. The default
/// tag has value 0, which is invalid. Tags compare and order by value, as `QMap` keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FontTag(u32);

impl FontTag {
    /// `tnum`: tabular (fixed-width) figures.
    pub const TNUM: FontTag = FontTag::new(b"tnum");
    /// `kern`: pair kerning.
    pub const KERN: FontTag = FontTag::new(b"kern");
    /// `liga`: standard ligatures.
    pub const LIGA: FontTag = FontTag::new(b"liga");
    /// `clig`: contextual ligatures.
    pub const CLIG: FontTag = FontTag::new(b"clig");

    /// The tag of four bytes (`QFont::Tag(const char (&)[5])`). All zero is invalid.
    pub const fn new(bytes: &[u8; 4]) -> Self {
        Self(u32::from_be_bytes(*bytes))
    }

    /// A tag from its numeric value; `None` for 0 (`QFont::Tag::fromValue`).
    pub const fn from_value(value: u32) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    /// A tag from a string of exactly four UTF-16 units, each converted like
    /// `QChar::toLatin1` (anything above U+00FF becomes NUL); `None` when the length is
    /// wrong or the result is invalid (`QFont::Tag::fromString`, which also warns).
    pub fn from_string(text: &str) -> Option<Self> {
        let mut units = text.encode_utf16();
        let mut bytes = [0u8; 4];
        for byte in &mut bytes {
            let unit = units.next()?;
            *byte = u8::try_from(unit).unwrap_or(0);
        }
        if units.next().is_some() {
            return None;
        }
        Self::from_value(u32::from_be_bytes(bytes))
    }

    /// The numeric value (`QFont::Tag::value`).
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Whether the value is non-zero (`QFont::Tag::isValid`).
    pub const fn is_valid(self) -> bool {
        self.0 != 0
    }

    /// The four bytes of the tag (`QFont::Tag::toString`).
    pub const fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

impl std::fmt::Display for FontTag {
    /// The bytes as Latin-1 characters, like `toString()`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;
        self.to_bytes()
            .iter()
            .try_for_each(|&b| f.write_char(char::from(b)))
    }
}

/// Font attribute descriptor matching Qt `QFont`.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// Font family name (e.g. "Segoe UI", "Arial", "Consolas").
    pub family: String,
    /// Font size in points or pixels.
    pub size: f32,
    /// Font weight.
    pub weight: FontWeight,
    /// Font style.
    pub style: FontStyle,
    /// OpenType features set explicitly, by tag (`QFont::setFeature`).
    features: BTreeMap<FontTag, u32>,
    /// Extra advance in logical pixels added after every glyph cluster
    /// (`QFont::setLetterSpacing(AbsoluteSpacing, …)`, which QSS `letter-spacing` maps to).
    pub letter_spacing: f32,
    /// In-memory binary font data.
    pub font_data: Option<SharedFontData>,
    /// Font hinting preference (`QFont::hintingPreference`).
    pub hinting_preference: HintingPreference,
}

impl Font {
    /// Creates a basic font descriptor.
    pub fn new(family: impl Into<String>, size: f32) -> Self {
        Self {
            family: family.into(),
            size,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            features: BTreeMap::new(),
            letter_spacing: 0.0,
            font_data: None,
            hinting_preference: HintingPreference::PreferDefaultHinting,
        }
    }
    #[inline]
    pub fn family(&self) -> &str {
        &self.family
    }

    #[inline]
    pub fn size(&self) -> f32 {
        self.size
    }

    #[inline]
    pub fn weight(&self) -> FontWeight {
        self.weight
    }

    #[inline]
    pub fn style(&self) -> FontStyle {
        self.style
    }

    #[inline]
    pub fn set_family(&mut self, family: impl Into<String>) {
        self.family = family.into();
    }

    #[inline]
    pub fn set_size(&mut self, size: f32) {
        self.size = size;
    }

    #[inline]
    pub fn set_weight(&mut self, weight: FontWeight) {
        self.weight = weight;
    }

    #[inline]
    pub fn set_style(&mut self, style: FontStyle) {
        self.style = style;
    }
    #[inline]
    pub fn hinting_preference(&self) -> HintingPreference {
        self.hinting_preference
    }

    #[inline]
    pub fn set_hinting_preference(&mut self, preference: HintingPreference) {
        self.hinting_preference = preference;
    }

    #[inline]
    pub fn with_hinting_preference(mut self, preference: HintingPreference) -> Self {
        self.hinting_preference = preference;
        self
    }

    /// Sets the font weight.
    pub fn with_weight(mut self, weight: FontWeight) -> Self {
        self.weight = weight;
        self
    }

    /// Sets the font style.
    pub fn with_style(mut self, style: FontStyle) -> Self {
        self.style = style;
        self
    }

    /// Applies `value` to the OpenType feature `tag` when shaping (`QFont::setFeature`). A
    /// zero value disables the feature, a non-zero one enables it; some features read it as an
    /// index (`salt`). A feature set to 0 still counts as set, and overrides what Qt would
    /// enable on its own (`kern` follows kerning, ligatures follow letter spacing). An invalid
    /// tag is ignored, as in Qt.
    ///
    /// Whether a feature has any effect depends on the font: a font without `tnum` lays
    /// digits out as usual.
    pub fn set_feature(&mut self, tag: FontTag, value: u32) {
        if tag.is_valid() {
            self.features.insert(tag, value);
        }
    }

    /// Builder form of [`set_feature`](Self::set_feature).
    pub fn with_feature(mut self, tag: FontTag, value: u32) -> Self {
        self.set_feature(tag, value);
        self
    }

    /// Removes `tag` from the explicitly set features, reverting to the default behaviour
    /// (`QFont::unsetFeature`).
    pub fn unset_feature(&mut self, tag: FontTag) {
        self.features.remove(&tag);
    }

    /// The value set for `tag`, 0 when it has not been set (`QFont::featureValue`).
    pub fn feature_value(&self, tag: FontTag) -> u32 {
        self.features.get(&tag).copied().unwrap_or(0)
    }

    /// Whether a value was set for `tag`, even 0 (`QFont::isFeatureSet`).
    pub fn is_feature_set(&self, tag: FontTag) -> bool {
        self.features.contains_key(&tag)
    }

    /// The tags of every feature that is set, ordered by tag value (`QFont::featureTags`).
    pub fn feature_tags(&self) -> Vec<FontTag> {
        self.features.keys().copied().collect()
    }

    /// The feature map, ordered by tag value.
    pub fn features(&self) -> &BTreeMap<FontTag, u32> {
        &self.features
    }

    /// Removes every explicitly set feature (`QFont::clearFeatures`).
    pub fn clear_features(&mut self) {
        self.features.clear();
    }

    /// Whether pair kerning is on: Qt's default, unless `kern` was set to 0
    /// (`QTextEngine::shapeTextWithHarfbuzzNG` lets an explicit `kern` win over `kerning()`).
    pub(crate) fn kerning_enabled(&self) -> bool {
        self.features
            .get(&FontTag::KERN)
            .is_none_or(|&value| value != 0)
    }

    /// Sets the absolute letter spacing in logical pixels.
    pub fn with_letter_spacing(mut self, spacing: f32) -> Self {
        self.letter_spacing = spacing;
        self
    }

    /// Sets in-memory binary font data.
    pub fn with_font_data(mut self, font_data: impl Into<SharedFontData>) -> Self {
        self.font_data = Some(font_data.into());
        self
    }
}

impl Default for Font {
    fn default() -> Self {
        Self::new("Segoe UI", 12.0)
    }
}

/// Cascading font description matching Qt style inheritance model.
///
/// Fields with `None` inherit their properties from the parent font.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FontDescription {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub weight: Option<FontWeight>,
    pub style: Option<FontStyle>,
    pub features: Option<BTreeMap<FontTag, u32>>,
    pub hinting_preference: Option<HintingPreference>,
}

impl FontDescription {
    /// Creates an empty font description (inherits all attributes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the font family name.
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }

    /// Sets the font size.
    pub fn with_size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    /// Sets the font weight.
    pub fn with_weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// Sets the font style.
    pub fn with_style(mut self, style: FontStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// Sets the OpenType feature map. Like `QFont::FeaturesResolved`, features resolve as a
    /// whole: a description that sets any replaces the parent's map, an empty map included.
    pub fn with_features(mut self, features: BTreeMap<FontTag, u32>) -> Self {
        self.features = Some(features);
        self
    }
    pub fn with_hinting_preference(mut self, preference: HintingPreference) -> Self {
        self.hinting_preference = Some(preference);
        self
    }

    /// Merges this description with parent font, overriding specified properties.
    pub fn merge_with_parent(&self, parent: &Font) -> Font {
        Font {
            family: self.family.clone().unwrap_or_else(|| parent.family.clone()),
            size: self.size.unwrap_or(parent.size),
            weight: self.weight.unwrap_or(parent.weight),
            style: self.style.unwrap_or(parent.style),
            features: self
                .features
                .clone()
                .unwrap_or_else(|| parent.features.clone()),
            letter_spacing: parent.letter_spacing,
            font_data: parent.font_data.clone(),
            hinting_preference: self.hinting_preference.unwrap_or(parent.hinting_preference),
        }
    }
}

impl From<&Font> for FontDescription {
    fn from(font: &Font) -> Self {
        Self {
            family: Some(font.family.clone()),
            size: Some(font.size),
            weight: Some(font.weight),
            style: Some(font.style),
            features: Some(font.features.clone()),
            hinting_preference: Some(font.hinting_preference),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_creation_and_builder() {
        let font = Font::new("Consolas", 14.0)
            .with_weight(FontWeight::Bold)
            .with_style(FontStyle::Italic);

        assert_eq!(font.family, "Consolas");
        assert_eq!(font.size, 14.0);
        assert_eq!(font.weight, FontWeight::Bold);
        assert_eq!(font.style, FontStyle::Italic);
        assert!(font.feature_tags().is_empty());
        assert!(font.font_data.is_none());
    }

    #[test]
    fn test_font_with_memory_data() {
        let fake_data = Arc::new(vec![0x00, 0x01, 0x00, 0x00]);
        let font = Font::new("CustomFont", 16.0).with_font_data(fake_data.clone());
        assert_eq!(
            font.font_data.as_ref().unwrap().as_slice(),
            &[0x00, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn test_font_description_cascading_merge() {
        let mut parent = Font::new("Segoe UI", 12.0)
            .with_weight(FontWeight::Normal)
            .with_style(FontStyle::Normal);
        parent.set_feature(FontTag::new(b"liga"), 0);

        // 1. None inherits parent
        let desc_empty = FontDescription::new();
        let merged_empty = desc_empty.merge_with_parent(&parent);
        assert_eq!(merged_empty, parent);

        // 2. Partial override
        let desc_partial = FontDescription::new()
            .with_size(18.0)
            .with_weight(FontWeight::Bold);
        let merged_partial = desc_partial.merge_with_parent(&parent);

        assert_eq!(merged_partial.family, "Segoe UI");
        assert_eq!(merged_partial.size, 18.0);
        assert_eq!(merged_partial.weight, FontWeight::Bold);
        assert_eq!(merged_partial.style, FontStyle::Normal);
        assert_eq!(merged_partial.feature_tags(), [FontTag::new(b"liga")]);

        // 3. Full override. Features resolve as a whole (`QFont::FeaturesResolved`): the
        // description's map replaces the parent's instead of merging with it.
        let mut features = BTreeMap::new();
        features.insert(FontTag::TNUM, 1);
        let desc_full = FontDescription::new()
            .with_family("Consolas")
            .with_size(24.0)
            .with_weight(FontWeight::Black)
            .with_style(FontStyle::Italic)
            .with_features(features);
        let merged_full = desc_full.merge_with_parent(&parent);

        assert_eq!(merged_full.family, "Consolas");
        assert_eq!(merged_full.size, 24.0);
        assert_eq!(merged_full.weight, FontWeight::Black);
        assert_eq!(merged_full.style, FontStyle::Italic);
        assert_eq!(merged_full.feature_tags(), [FontTag::TNUM]);
        assert!(!merged_full.is_feature_set(FontTag::new(b"liga")));

        // 4. An explicitly empty map clears the parent's features.
        let cleared = FontDescription::new()
            .with_features(BTreeMap::new())
            .merge_with_parent(&parent);
        assert!(cleared.feature_tags().is_empty());

        // A description taken from a font reproduces its features.
        assert_eq!(
            FontDescription::from(&merged_full).merge_with_parent(&parent),
            merged_full
        );
    }
}
