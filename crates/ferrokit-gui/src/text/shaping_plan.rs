// SPDX-License-Identifier: LGPL-3.0-only
//! Who kerns a run, decided the way Qt 6.12 and its bundled HarfBuzz 14.5 decide it.
//!
//! Qt cuts a string into script items (`QUnicodeTools::initScripts`) and shapes each with the
//! script it found. HarfBuzz then picks the kerning source of that run in
//! `hb_ot_shape_planner_t::compile` (`hb-ot-shape.cc`): the GPOS `kern` feature when the font's
//! GPOS has one *under the run's script and language*, else a `kern` table, and only a shaper
//! with `fallback_position` applies that table. A font such as Tahoma has a GPOS `kern` for
//! Arabic only, so its Latin text is kerned from the `kern` table; a check that looks at every
//! script of the font gets that wrong.
//!
//! The language: Qt hands HarfBuzz `QLocale().bcp47Name()`, which can select a language system
//! (for instance `ZHT ` under `latn`) other than the default one. Shaping passes the system
//! language to rustybuzz (`directwrite_catalog::system_language`), which picks the language
//! system itself; only the check below for a GPOS `kern` reads the default language system, which
//! differs from Qt's choice for a font whose language system for the locale has other features
//! than its default one and a `kern` table besides (none measured).

use rustybuzz::ttf_parser::Tag;
use rustybuzz::ttf_parser::opentype_layout::LayoutTable;
use unicode_script::{Script, UnicodeScript};

/// What HarfBuzz kerns a run with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KernSource {
    /// The GPOS `kern` feature (or `kerx`, or anything else rustybuzz decides itself).
    Shaper,
    /// The font's `kern` table (`apply_kern`).
    KernTable,
    /// Nothing: the font has a `kern` table but the run's shaper does not apply it.
    Nothing,
}

/// `script` as the script rustybuzz shapes with (`hb_qt_script_to_script`).
pub(crate) fn buzz_script(script: Script) -> Option<rustybuzz::Script> {
    let tag: [u8; 4] = script.short_name().as_bytes().try_into().ok()?;
    rustybuzz::Script::from_iso15924_tag(Tag::from_bytes(&tag))
}

/// Qt's script items of `text` as `(byte offset, script)` (`QUnicodeTools::initScripts`).
///
/// Characters of the Common, Inherited and Unknown scripts join the preceding item (leading ones
/// take the script of the first real character), and a combining mark never starts an item.
/// Qt tests the general category for the marks; the stand-in here is the line break class CM or
/// the bidi class NSM, which only differ for a few format characters and for spacing marks of a
/// script other than the one of their base.
pub(crate) fn script_items(text: &str) -> Vec<(usize, Script)> {
    let neutral = |s: Script| matches!(s, Script::Common | Script::Inherited | Script::Unknown);
    let mut items = Vec::new();
    let mut start = 0;
    let mut script = Script::Common;
    for (offset, ch) in text.char_indices() {
        let next = ch.script();
        if next == script || neutral(next) {
            continue;
        }
        if neutral(script) {
            script = next;
            continue;
        }
        if is_combining_mark(ch) {
            continue;
        }
        items.push((start, script));
        start = offset;
        script = next;
    }
    items.push((start, script));
    items
}

fn is_combining_mark(ch: char) -> bool {
    unicode_linebreak::break_property(ch as u32) == unicode_linebreak::BreakClass::CombiningMark
        || unicode_bidi::bidi_class(ch) == unicode_bidi::BidiClass::NSM
}

/// The OpenType script tags HarfBuzz tries for `script` (`hb_ot_all_tags_from_script`), in order.
fn script_tags(script: Script) -> ([Tag; 3], usize) {
    let short = script.short_name().as_bytes();
    let mut tags = [Tag::from_bytes(b"DFLT"); 3];
    let mut count = 0;
    let new_tag = match script {
        Script::Bengali => Some(b"bng2"),
        Script::Devanagari => Some(b"dev2"),
        Script::Gujarati => Some(b"gjr2"),
        Script::Gurmukhi => Some(b"gur2"),
        Script::Kannada => Some(b"knd2"),
        Script::Malayalam => Some(b"mlm2"),
        Script::Oriya => Some(b"ory2"),
        Script::Tamil => Some(b"tml2"),
        Script::Telugu => Some(b"tel2"),
        Script::Myanmar => Some(b"mym2"),
        _ => None,
    };
    if let Some(new_tag) = new_tag {
        // 'mym2' has no 'mym3'.
        if new_tag != b"mym2" {
            tags[count] = Tag::from_bytes(&[new_tag[0], new_tag[1], new_tag[2], b'3']);
            count += 1;
        }
        tags[count] = Tag::from_bytes(new_tag);
        count += 1;
    }
    // The old tag: the ISO 15924 tag with the first letter lowercased, Hiragana and Katakana both
    // 'kana', and the four tags that keep their trailing spaces.
    let old = match script {
        Script::Hiragana | Script::Katakana => *b"kana",
        Script::Lao => *b"lao ",
        Script::Yi => *b"yi  ",
        Script::Nko => *b"nko ",
        Script::Vai => *b"vai ",
        _ => [short[0].to_ascii_lowercase(), short[1], short[2], short[3]],
    };
    tags[count] = Tag::from_bytes(&old);
    count += 1;
    (tags, count)
}

/// `hb_ot_layout_table_select_script`: the first of `tags` the table has, else `DFLT`, `dflt`,
/// `latn`. The result is the script index and its tag.
fn select_script(table: Option<&LayoutTable>, tags: &[Tag]) -> Option<(u16, Tag)> {
    let table = table?;
    let fallbacks = [
        Tag::from_bytes(b"DFLT"),
        Tag::from_bytes(b"dflt"),
        Tag::from_bytes(b"latn"),
    ];
    tags.iter()
        .chain(fallbacks.iter())
        .find_map(|&tag| table.scripts.index(tag).map(|index| (index, tag)))
}

/// Whether the GPOS table has a `kern` feature under the script HarfBuzz picks for `tags`, in the
/// default language system (`hb_ot_layout_language_find_feature`: the required feature does not
/// count, only the listed ones do).
fn gpos_has_kern(gpos: Option<&LayoutTable>, tags: &[Tag]) -> bool {
    let Some(gpos) = gpos else {
        return false;
    };
    let Some((index, _)) = select_script(Some(gpos), tags) else {
        return false;
    };
    let Some(script) = gpos.scripts.get(index) else {
        return false;
    };
    // `hb_ot_layout_script_select_language`: a system tagged 'dflt' (a typo in many fonts) wins
    // over the default language system.
    let system = script
        .languages
        .find(Tag::from_bytes(b"dflt"))
        .or(script.default_language);
    let kern = Tag::from_bytes(b"kern");
    system.is_some_and(|system| {
        system
            .feature_indices
            .into_iter()
            .any(|index| gpos.features.get(index).is_some_and(|f| f.tag == kern))
    })
}

/// Whether the shaper HarfBuzz picks for `script` has `fallback_position`, i.e. applies a `kern`
/// table (`hb_ot_shaper_categorize`; `gsub_script` is the script it chose in the GSUB table).
/// The Thai, Khmer, Indic, Myanmar and Universal Shaping Engine shapers do not.
fn shaper_applies_kern_table(script: Script, gsub_script: Option<Tag>) -> bool {
    let default_font = |tags: &[&[u8; 4]]| {
        gsub_script.is_some_and(|tag| tags.iter().any(|t| tag == Tag::from_bytes(t)))
    };
    match script {
        Script::Thai | Script::Lao | Script::Khmer => false,
        Script::Myanmar => default_font(&[b"DFLT", b"latn", b"mymr"]),
        Script::Bengali
        | Script::Devanagari
        | Script::Gujarati
        | Script::Gurmukhi
        | Script::Kannada
        | Script::Malayalam
        | Script::Oriya
        | Script::Tamil
        | Script::Telugu => default_font(&[b"DFLT", b"latn"]),
        s if uses_universal_shaper(s) => default_font(&[b"DFLT", b"latn"]),
        _ => true,
    }
}

/// The scripts `hb_ot_shaper_categorize` sends to the Universal Shaping Engine.
fn uses_universal_shaper(script: Script) -> bool {
    matches!(
        script.short_name(),
        "Tibt"
            | "Mong"
            | "Sinh"
            | "Buhd"
            | "Hano"
            | "Tglg"
            | "Tagb"
            | "Limb"
            | "Tale"
            | "Bugi"
            | "Khar"
            | "Sylo"
            | "Tfng"
            | "Bali"
            | "Nkoo"
            | "Phag"
            | "Cham"
            | "Kali"
            | "Lepc"
            | "Rjng"
            | "Saur"
            | "Sund"
            | "Egyp"
            | "Java"
            | "Kthi"
            | "Mtei"
            | "Lana"
            | "Tavt"
            | "Batk"
            | "Brah"
            | "Mand"
            | "Cakm"
            | "Plrd"
            | "Shrd"
            | "Takr"
            | "Dupl"
            | "Gran"
            | "Khoj"
            | "Sind"
            | "Mahj"
            | "Mani"
            | "Modi"
            | "Hmng"
            | "Phlp"
            | "Sidd"
            | "Tirh"
            | "Ahom"
            | "Mult"
            | "Adlm"
            | "Bhks"
            | "Marc"
            | "Newa"
            | "Gonm"
            | "Soyo"
            | "Zanb"
            | "Dogr"
            | "Gong"
            | "Rohg"
            | "Maka"
            | "Medf"
            | "Sogo"
            | "Sogd"
            | "Elym"
            | "Nand"
            | "Hmnp"
            | "Wcho"
            | "Chrs"
            | "Diak"
            | "Kits"
            | "Yezi"
            | "Cpmn"
            | "Ougr"
            | "Tnsa"
            | "Toto"
            | "Vith"
            | "Kawi"
            | "Nagm"
            | "Gara"
            | "Gukh"
            | "Krai"
            | "Onao"
            | "Sunu"
            | "Todr"
            | "Tutg"
            | "Berf"
            | "Sidt"
            | "Tayo"
            | "Tols"
            | "Jurc"
            | "Pcun"
            | "Seal"
    )
}

/// Which source kerns a run of `script` in `face` (`hb_ot_shape_planner_t::compile`).
pub(crate) fn kern_source(face: &rustybuzz::Face, script: Script) -> KernSource {
    let tables = face.tables();
    // A font with `kerx` is kerned through `kerx` or GPOS, never through `kern`.
    if tables.kerx.is_some() {
        return KernSource::Shaper;
    }
    let (tags, count) = script_tags(script);
    let tags = &tags[..count];
    if tables.gpos.is_some() && gpos_has_kern(tables.gpos.as_ref(), tags) {
        return KernSource::Shaper;
    }
    if tables.kern.is_none() {
        return KernSource::Shaper;
    }
    let gsub_script = select_script(tables.gsub.as_ref(), tags).map(|(_, tag)| tag);
    if shaper_applies_kern_table(script, gsub_script) {
        KernSource::KernTable
    } else {
        KernSource::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(script: Script) -> Vec<[u8; 4]> {
        let (tags, count) = script_tags(script);
        tags[..count].iter().map(|t| t.to_bytes()).collect()
    }

    #[test]
    fn script_items_follow_qt_init_scripts() {
        // Leading neutral characters take the script of the first real one, neutral ones stay
        // with the item before them, and a combining mark never starts an item.
        assert_eq!(script_items("12 ab"), [(0, Script::Latin)]);
        assert_eq!(
            script_items("ab 12 \u{05d0}\u{05d1} 3"),
            [(0, Script::Latin), (6, Script::Hebrew)]
        );
        assert_eq!(
            script_items("a\u{0483}\u{0300}b"),
            [(0, Script::Latin)],
            "Cyrillic and inherited marks stay with their Latin base"
        );
        assert_eq!(
            script_items("日本語テキスト"),
            [(0, Script::Han), (9, Script::Katakana)]
        );
        assert_eq!(script_items("123"), [(0, Script::Common)]);
    }

    #[test]
    fn script_tags_follow_hb_ot_all_tags_from_script() {
        assert_eq!(tags(Script::Latin), [*b"latn"]);
        assert_eq!(tags(Script::Arabic), [*b"arab"]);
        assert_eq!(tags(Script::Common), [*b"zyyy"]);
        assert_eq!(tags(Script::Devanagari), [*b"dev3", *b"dev2", *b"deva"]);
        assert_eq!(tags(Script::Myanmar), [*b"mym2", *b"mymr"]);
        assert_eq!(tags(Script::Hiragana), [*b"kana"]);
        assert_eq!(tags(Script::Katakana), [*b"kana"]);
        assert_eq!(tags(Script::Lao), [*b"lao "]);
        assert_eq!(tags(Script::Han), [*b"hani"]);
    }

    #[test]
    fn only_some_shapers_apply_a_kern_table() {
        let dflt = Some(Tag::from_bytes(b"DFLT"));
        let none = None;
        assert!(shaper_applies_kern_table(Script::Latin, none));
        assert!(shaper_applies_kern_table(Script::Hangul, none));
        assert!(shaper_applies_kern_table(Script::Arabic, none));
        assert!(!shaper_applies_kern_table(Script::Thai, dflt));
        assert!(!shaper_applies_kern_table(Script::Khmer, dflt));
        // Indic, Myanmar and USE scripts use the default shaper only for a font made for
        // 'DFLT'/'latn' (or 'mymr'); a font with no script at all gets the specific shaper.
        assert!(shaper_applies_kern_table(Script::Devanagari, dflt));
        assert!(!shaper_applies_kern_table(Script::Devanagari, none));
        assert!(!shaper_applies_kern_table(
            Script::Devanagari,
            Some(Tag::from_bytes(b"dev2"))
        ));
        assert!(shaper_applies_kern_table(
            Script::Myanmar,
            Some(Tag::from_bytes(b"mymr"))
        ));
        assert!(!shaper_applies_kern_table(Script::Tibetan, none));
        assert!(shaper_applies_kern_table(Script::Tibetan, dflt));
    }
}
