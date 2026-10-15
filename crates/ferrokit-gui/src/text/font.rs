// SPDX-License-Identifier: LGPL-3.0-only
use std::path::Path;
use std::sync::Arc;

/// Shared binary font data representation supporting zero-copy slicing and deduplication.
///
/// The bytes live behind an `Arc`, so every `FontEngine`/`Font` sharing a file shares one buffer.
/// The buffer is either heap memory ([`from_vec`](Self::from_vec)) or a read-only mapping of the
/// font file ([`from_file`](Self::from_file)). A mapping is backed by the file itself: it is not
/// charged to the process's commit, and only the pages actually read (a few glyph tables out of a
/// 16 MB CJK collection) become resident. Qt maps system font files the same way.
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

    /// Maps the font file at `path` read-only; reads it into memory if it cannot be mapped.
    ///
    /// As with any file mapping, the file must not be truncated or rewritten while the mapping
    /// lives. Installed font files are not modified in place, and Windows refuses to delete or
    /// overwrite a file that is mapped.
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 0 {
            // SAFETY: the mapping is read-only and private; see the caveat above.
            if let Ok(map) = unsafe { memmap2::Mmap::map(&file) } {
                return Ok(Self { data: Arc::new(map) });
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
        f.debug_struct("SharedFontData").field("len", &self.len()).finish()
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
    /// Enables OpenType 'tnum' (tabular numbers) feature.
    pub tabular_numbers: bool,
    /// Extra advance in logical pixels added after every glyph cluster
    /// (`QFont::setLetterSpacing(AbsoluteSpacing, …)`, which QSS `letter-spacing` maps to).
    pub letter_spacing: f32,
    /// In-memory binary font data.
    pub font_data: Option<SharedFontData>,
}

impl Font {
    /// Creates a basic font descriptor.
    pub fn new(family: impl Into<String>, size: f32) -> Self {
        Self {
            family: family.into(),
            size,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            tabular_numbers: false,
            letter_spacing: 0.0,
            font_data: None,
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

    /// Sets whether tabular numbers ('tnum') are enabled.
    pub fn with_tabular_numbers(mut self, enabled: bool) -> Self {
        self.tabular_numbers = enabled;
        self
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
    pub tabular_numbers: Option<bool>,
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

    /// Sets whether tabular numbers are enabled.
    pub fn with_tabular_numbers(mut self, enabled: bool) -> Self {
        self.tabular_numbers = Some(enabled);
        self
    }

    /// Merges this description with parent font, overriding specified properties.
    pub fn merge_with_parent(&self, parent: &Font) -> Font {
        Font {
            family: self.family.clone().unwrap_or_else(|| parent.family.clone()),
            size: self.size.unwrap_or(parent.size),
            weight: self.weight.unwrap_or(parent.weight),
            style: self.style.unwrap_or(parent.style),
            tabular_numbers: self.tabular_numbers.unwrap_or(parent.tabular_numbers),
            letter_spacing: parent.letter_spacing,
            font_data: parent.font_data.clone(),
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
            tabular_numbers: Some(font.tabular_numbers),
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
            .with_style(FontStyle::Italic)
            .with_tabular_numbers(true);

        assert_eq!(font.family, "Consolas");
        assert_eq!(font.size, 14.0);
        assert_eq!(font.weight, FontWeight::Bold);
        assert_eq!(font.style, FontStyle::Italic);
        assert!(font.tabular_numbers);
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
        let parent = Font::new("Segoe UI", 12.0)
            .with_weight(FontWeight::Normal)
            .with_style(FontStyle::Normal)
            .with_tabular_numbers(false);

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
        assert!(!merged_partial.tabular_numbers);

        // 3. Full override
        let desc_full = FontDescription::new()
            .with_family("Consolas")
            .with_size(24.0)
            .with_weight(FontWeight::Black)
            .with_style(FontStyle::Italic)
            .with_tabular_numbers(true);
        let merged_full = desc_full.merge_with_parent(&parent);

        assert_eq!(merged_full.family, "Consolas");
        assert_eq!(merged_full.size, 24.0);
        assert_eq!(merged_full.weight, FontWeight::Black);
        assert_eq!(merged_full.style, FontStyle::Italic);
        assert!(merged_full.tabular_numbers);
    }
}
