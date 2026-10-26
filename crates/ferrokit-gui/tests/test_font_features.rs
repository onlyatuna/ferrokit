// SPDX-License-Identifier: LGPL-3.0-only
//! `QFont::Tag` / `QFont::setFeature` and what the features do to shaping.
//!
//! The tag and feature-map tests mirror `QFont::Tag` and `QFont::setFeature`/`unsetFeature`/
//! `featureValue`/`isFeatureSet`/`featureTags`/`clearFeatures` (qfont.cpp, Qt 6.11.2). The
//! shaping tests need an installed Arial and run on Windows only: the numbers they were checked
//! against were measured with `QFontMetricsF::horizontalAdvance` in PySide6 6.11.2 (Arial 20 px,
//! DPR 1.25, i.e. the DirectWrite engine): "1111" 39.984375 by default and 44.4375 with `tnum` 1
//! or `kern` 0, "AVAT To" 73.65625 by default and 81.046875 with `kern` 0.

use ferrokit_gui::text::font::{Font, FontDescription, FontTag};
use std::collections::BTreeMap;

#[test]
fn tag_matches_qfont_tag() {
    // QFont::Tag("tnum").value(): the four bytes, big-endian.
    assert_eq!(FontTag::new(b"tnum").value(), 0x74_6E_75_6D);
    assert_eq!(FontTag::TNUM, FontTag::new(b"tnum"));
    assert_eq!(FontTag::new(b"tnum").to_bytes(), *b"tnum");
    assert_eq!(FontTag::new(b"tnum").to_string(), "tnum");
    assert!(FontTag::new(b"tnum").is_valid());
    // The default tag and the all-zero tag are invalid.
    assert!(!FontTag::default().is_valid());
    assert!(!FontTag::new(&[0; 4]).is_valid());
    assert_eq!(FontTag::from_value(0), None);
    assert_eq!(FontTag::from_value(0x74_6E_75_6D), Some(FontTag::TNUM));
    // fromString: exactly four characters.
    assert_eq!(FontTag::from_string("tnum"), Some(FontTag::TNUM));
    assert_eq!(FontTag::from_string("tnu"), None);
    assert_eq!(FontTag::from_string("tnum1"), None);
    assert_eq!(FontTag::from_string(""), None);
    // Characters above U+00FF become NUL (QChar::toLatin1); all of them: invalid.
    assert_eq!(
        FontTag::from_string("\u{4e00}\u{4e00}\u{4e00}\u{4e00}"),
        None
    );
    assert_eq!(
        FontTag::from_string("a\u{4e00}bc").map(FontTag::to_bytes),
        Some([b'a', 0, b'b', b'c'])
    );
    // Latin-1 above ASCII is one byte.
    assert_eq!(
        FontTag::from_string("\u{e9}abc").map(FontTag::to_bytes),
        Some([0xe9, b'a', b'b', b'c'])
    );
    // Tags order by value, as QMap keys do.
    assert!(FontTag::new(b"kern") < FontTag::new(b"liga"));
}

#[test]
fn features_follow_qfont() {
    let mut font = Font::new("Segoe UI", 12.0);
    assert!(!font.is_feature_set(FontTag::TNUM));
    assert_eq!(font.feature_value(FontTag::TNUM), 0);

    // setFeature(QFont.Tag("tnum"), 1)
    font.set_feature(FontTag::TNUM, 1);
    assert!(font.is_feature_set(FontTag::TNUM));
    assert_eq!(font.feature_value(FontTag::TNUM), 1);

    // A value of 0 is a set feature (explicitly disabled), not an unset one.
    font.set_feature(FontTag::LIGA, 0);
    assert!(font.is_feature_set(FontTag::LIGA));
    assert_eq!(font.feature_value(FontTag::LIGA), 0);

    // Values other than 0/1 are kept (`salt` takes an alternate index), and replace each other.
    font.set_feature(FontTag::new(b"salt"), 3);
    assert_eq!(font.feature_value(FontTag::new(b"salt")), 3);
    font.set_feature(FontTag::new(b"salt"), 2);
    assert_eq!(font.feature_value(FontTag::new(b"salt")), 2);

    // featureTags() is ordered by tag value.
    assert_eq!(
        font.feature_tags(),
        [FontTag::LIGA, FontTag::new(b"salt"), FontTag::TNUM]
    );

    // An invalid tag is ignored.
    font.set_feature(FontTag::default(), 1);
    assert_eq!(font.feature_tags().len(), 3);
    assert!(!font.is_feature_set(FontTag::default()));

    font.unset_feature(FontTag::LIGA);
    assert!(!font.is_feature_set(FontTag::LIGA));
    assert_eq!(font.feature_tags().len(), 2);
    // Unsetting an unset feature is fine.
    font.unset_feature(FontTag::LIGA);

    font.clear_features();
    assert!(font.feature_tags().is_empty());
}

#[test]
fn features_take_part_in_font_equality_and_cloning() {
    let plain = Font::new("Segoe UI", 12.0);
    let tnum = plain.clone().with_feature(FontTag::TNUM, 1);
    assert_ne!(plain, tnum);
    assert_eq!(tnum, tnum.clone());
    assert_ne!(tnum, plain.clone().with_feature(FontTag::TNUM, 0));
    // Setting a feature on a clone leaves the original alone (QFont detaches).
    assert!(!plain.is_feature_set(FontTag::TNUM));
}

#[test]
fn description_resolves_features_as_a_whole() {
    let mut parent = Font::new("Segoe UI", 12.0);
    parent.set_feature(FontTag::LIGA, 0);

    // No features in the description: the parent's are inherited.
    let inherited = FontDescription::new()
        .with_size(18.0)
        .merge_with_parent(&parent);
    assert_eq!(inherited.feature_tags(), [FontTag::LIGA]);

    // Any features in the description replace the parent's (QFont::FeaturesResolved).
    let mut features = BTreeMap::new();
    features.insert(FontTag::TNUM, 1);
    let replaced = FontDescription::new()
        .with_features(features)
        .merge_with_parent(&parent);
    assert_eq!(replaced.feature_tags(), [FontTag::TNUM]);
    assert!(!replaced.is_feature_set(FontTag::LIGA));

    // An empty map clears them.
    let cleared = FontDescription::new()
        .with_features(BTreeMap::new())
        .merge_with_parent(&parent);
    assert!(cleared.feature_tags().is_empty());

    // A description taken from a font reproduces its features.
    assert_eq!(
        FontDescription::from(&replaced).merge_with_parent(&parent),
        replaced
    );
}

#[cfg(windows)]
mod shaping {
    use super::*;
    use ferrokit_gui::text::font_database::set_application_device_pixel_ratio;
    use ferrokit_gui::text::font_metrics::FontMetrics;

    /// `QFontMetricsF::horizontalAdvance`, with the DirectWrite engine the HUD's screen uses.
    fn advance(text: &str, font: &Font) -> f32 {
        set_application_device_pixel_ratio(1.25);
        FontMetrics::from_font(font).horizontal_advance_exact(text, font)
    }

    fn arial() -> Font {
        Font::new("Arial", 20.0)
    }

    #[test]
    fn kern_feature_overrides_kerning() {
        let plain = arial();
        let no_kern = arial().with_feature(FontTag::KERN, 0);
        // "AV", "AT", "To" are kerned pairs: kerning off makes the run wider.
        assert!(advance("AVAT To", &no_kern) > advance("AVAT To", &plain) + 1.0);
        // An explicit `kern` 1 is what Qt does anyway.
        let kern_on = arial().with_feature(FontTag::KERN, 1);
        assert_eq!(advance("AVAT To", &kern_on), advance("AVAT To", &plain));
        // Unsetting goes back to the default.
        let mut unset = no_kern.clone();
        unset.unset_feature(FontTag::KERN);
        assert_eq!(advance("AVAT To", &unset), advance("AVAT To", &plain));
    }

    #[test]
    fn features_are_part_of_the_measurement_identity() {
        // The width cache must not hand a plain font's answer to a font with a feature set.
        let plain = arial();
        let no_kern = arial().with_feature(FontTag::KERN, 0);
        let before = advance("AVAT To", &plain);
        let with = advance("AVAT To", &no_kern);
        assert_ne!(before, with);
        assert_eq!(advance("AVAT To", &plain), before);
        assert_eq!(advance("AVAT To", &no_kern), with);
    }

    #[test]
    fn tnum_feature_reaches_the_shaper() {
        // Measured in Qt: Arial's "1111" is 39.984375 (the `11` pair is kerned), and 44.4375
        // once `tnum` is on, the width of "8888".
        let plain = arial();
        let tnum = arial().with_feature(FontTag::TNUM, 1);
        assert!(advance("1111", &tnum) > advance("1111", &plain) + 1.0);
        assert_eq!(advance("1111", &tnum), advance("8888", &tnum));
        // `tnum` 0 is what Qt does by default.
        let tnum_off = arial().with_feature(FontTag::TNUM, 0);
        assert_eq!(advance("1111", &tnum_off), advance("1111", &plain));
    }

    #[test]
    fn explicit_feature_wins_over_the_default_kern() {
        // setFeature takes precedence over what Qt itself would pick (QFlatMap::insert keeps
        // the first value): `tnum` and `kern` 0 together measure as `kern` 0 alone here.
        let both = arial()
            .with_feature(FontTag::TNUM, 1)
            .with_feature(FontTag::KERN, 0);
        let kern_off = arial().with_feature(FontTag::KERN, 0);
        assert_eq!(advance("AVAT To", &both), advance("AVAT To", &kern_off));
        assert_eq!(advance("1111", &both), advance("1111", &kern_off));
    }
}
