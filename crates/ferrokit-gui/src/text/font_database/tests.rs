// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

/// Off macOS the flag layout rounds its advances by is `uses_directwrite_engine`, whatever the
/// hinting: `font_has_subpixel_positions` (what the line metrics follow) is `true` for a font
/// preferring no hinting, but Windows and Linux advances are still rounded without
/// DirectWrite, as before the CoreText engine existed.
#[cfg(not(target_os = "macos"))]
#[test]
fn layout_advances_follow_directwrite_not_hinting_off_macos() {
    assert_eq!(layout_has_subpixel_advances(), uses_directwrite_engine());
    let font = Font::new("Arial", 16.0).with_hinting_preference(HintingPreference::PreferNoHinting);
    assert!(font_has_subpixel_positions(&font));
    assert_eq!(layout_has_subpixel_advances(), uses_directwrite_engine());
}

#[cfg(target_os = "macos")]
#[test]
fn layout_keeps_fractional_advances_on_macos() {
    assert!(layout_has_subpixel_advances());
}

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

/// A family list (`'A', B, monospace`) is the first of its names that is a family, not the
/// first one a substitute is found for (Qt's font database matching), and its raw
/// font data is that family's, so the engine shapes with the face it measures with. Without
/// the data text is laid out from the face's unrounded advances (92.99 px for Consolas 10 px
/// where Qt measures 96).
#[test]
fn a_family_list_resolves_to_its_first_installed_family_with_its_font_data() {
    let Some(bytes) = any_font_bytes() else {
        return;
    };
    let mut db = FontDatabase::new();
    db.add_font_from_memory("FerrokitListProbe", SharedFontData::from_vec(bytes.clone()))
        .expect("the font parses");
    let own = db
        .load_font("FerrokitListProbe")
        .expect("the application font loads");
    let list = "'FerrokitNoSuchFamily', 'FerrokitListProbe', monospace";
    let face = db
        .load_font_styled(list, FaceStyle::regular(), StyleHint::AnyStyle)
        .expect("the list resolves");
    assert!(Arc::ptr_eq(&face, &own), "the list is its installed family");
    let raw = db
        .get_raw_font_data_styled(list, FaceStyle::regular(), StyleHint::AnyStyle)
        .expect("the list has the font data of its family");
    assert_eq!(raw.as_slice(), bytes.as_slice());
    let engines = db.resolve_font_engines(&Font::new(list, 12.0));
    assert!(Arc::ptr_eq(&engines[0].face, &own));
    assert!(
        engines[0].raw_data.is_some(),
        "the engine keeps the family's font data"
    );
}
#[cfg(windows)]
#[test]
fn test_font_database_style_hint_substitute() {
    assert!(
        Path::new("C:/Windows/Fonts/tahoma.ttf").exists()
            || Path::new("C:/Windows/Fonts/Tahoma.ttf").exists(),
        "Tahoma must be installed on Windows"
    );
    assert!(
        Path::new("C:/Windows/Fonts/arial.ttf").exists()
            || Path::new("C:/Windows/Fonts/Arial.ttf").exists(),
        "Arial must be installed on Windows"
    );
    assert!(
        Path::new("C:/Windows/Fonts/cour.ttf").exists()
            || Path::new("C:/Windows/Fonts/Cour.ttf").exists(),
        "Courier New must be installed on Windows"
    );

    let mut db = FontDatabase::new();
    let missing = "NoSuchFamilyNonExistent123";

    // AnyStyle -> Tahoma
    let face_tahoma = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::AnyStyle);
    assert!(face_tahoma.is_some());
    let tahoma_direct = db.load_font_styled("Tahoma", FaceStyle::regular(), StyleHint::AnyStyle);
    assert!(face_tahoma.is_some() && tahoma_direct.is_some());
    assert!(Arc::ptr_eq(&face_tahoma.unwrap(), &tahoma_direct.unwrap()));

    // Helvetica -> Arial
    let face_arial = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::Helvetica);
    let arial_direct = db.load_font_styled("Arial", FaceStyle::regular(), StyleHint::Helvetica);
    assert!(face_arial.is_some() && arial_direct.is_some());
    assert!(Arc::ptr_eq(&face_arial.unwrap(), &arial_direct.unwrap()));

    // Courier -> Courier New
    let face_cour = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::Courier);
    let cour_direct = db.load_font_styled("Courier New", FaceStyle::regular(), StyleHint::Courier);
    assert!(face_cour.is_some() && cour_direct.is_some());
    assert!(Arc::ptr_eq(&face_cour.unwrap(), &cour_direct.unwrap()));

    // Fonts differing only in hint do not share a cache entry
    let key_any =
        FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::AnyStyle);
    let key_helv =
        FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::Helvetica);
    assert_ne!(key_any, key_helv);
    assert!(db.cache.contains_key(&key_any));
    assert!(db.cache.contains_key(&key_helv));
    assert!(!Arc::ptr_eq(&db.cache[&key_any], &db.cache[&key_helv]));
}
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn test_font_database_style_hint_substitute_linux() {
    let mut db = FontDatabase::new();
    let missing = "NoSuchFontXYZ";

    if !is_fontconfig_available() {
        let face = db.load_font_styled(missing, FaceStyle::regular(), StyleHint::AnyStyle);
        assert!(
            face.is_none(),
            "When fontconfig is absent, no substitute resolves"
        );
        return;
    }

    // The substitute is the first family `FcFontSort` lists that this machine can load (which
    // family that is depends on its fontconfig configuration and installed fonts, so it is
    // derived here, not hard-coded).
    let all_hints = [
        StyleHint::AnyStyle,
        StyleHint::SansSerif,
        StyleHint::Serif,
        StyleHint::Monospace,
        StyleHint::TypeWriter,
        StyleHint::Cursive,
        StyleHint::Fantasy,
        StyleHint::System,
    ];
    for hint in all_hints {
        let candidates = db.linux.registry(&db.search_paths).fallbacks_for_family(
            missing,
            crate::text::fontconfig_catalog::Slant::Normal,
            hint,
            "Zyyy",
        );
        let expected = candidates
            .iter()
            .find_map(|c| db.load_font_styled(c, FaceStyle::regular(), hint))
            .expect("fontconfig lists no family this machine can load");
        let face = db
            .load_font_styled(missing, FaceStyle::regular(), hint)
            .unwrap_or_else(|| panic!("no substitute resolved for {hint:?}"));
        assert!(
            Arc::ptr_eq(&face, &expected),
            "substitute for {hint:?} must be the first listed family that loads"
        );
    }

    // Fonts that differ only in hint do not share a cache entry.
    let key_any =
        FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::AnyStyle);
    let key_serif = FaceStyle::regular().cache_key(&missing.to_ascii_lowercase(), StyleHint::Serif);
    assert_ne!(key_any, key_serif);
    assert!(db.cache.contains_key(&key_any));
    assert!(db.cache.contains_key(&key_serif));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn test_fc_family_for_style_hint_pure_mapping() {
    assert_eq!(
        fc_family_for_style_hint(StyleHint::SansSerif),
        Some("sans-serif")
    );
    assert_eq!(
        fc_family_for_style_hint(StyleHint::Helvetica),
        Some("sans-serif")
    );
    assert_eq!(fc_family_for_style_hint(StyleHint::Serif), Some("serif"));
    assert_eq!(fc_family_for_style_hint(StyleHint::Times), Some("serif"));
    assert_eq!(
        fc_family_for_style_hint(StyleHint::Monospace),
        Some("monospace")
    );
    assert_eq!(
        fc_family_for_style_hint(StyleHint::TypeWriter),
        Some("monospace")
    );
    assert_eq!(
        fc_family_for_style_hint(StyleHint::Courier),
        Some("monospace")
    );
    assert_eq!(
        fc_family_for_style_hint(StyleHint::Cursive),
        Some("cursive")
    );
    assert_eq!(
        fc_family_for_style_hint(StyleHint::Fantasy),
        Some("fantasy")
    );
    assert_eq!(fc_family_for_style_hint(StyleHint::AnyStyle), None);
    assert_eq!(fc_family_for_style_hint(StyleHint::System), None);
    assert_eq!(fc_family_for_style_hint(StyleHint::OldEnglish), None);
    assert_eq!(fc_family_for_style_hint(StyleHint::Decorative), None);

    // family_for_style_hint alias
    assert_eq!(
        family_for_style_hint(StyleHint::SansSerif),
        Some("sans-serif")
    );
}

#[cfg(target_os = "macos")]
#[test]
fn test_family_for_style_hint_macos_table() {
    assert_eq!(
        family_for_style_hint(StyleHint::SansSerif),
        Some("Helvetica")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::Helvetica),
        Some("Helvetica")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::Serif),
        Some("Times New Roman")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::Times),
        Some("Times New Roman")
    );
    assert_eq!(family_for_style_hint(StyleHint::Monospace), Some("Menlo"));
    assert_eq!(
        family_for_style_hint(StyleHint::Cursive),
        Some("Apple Chancery")
    );
    assert_eq!(family_for_style_hint(StyleHint::Fantasy), Some("Zapfino"));
    assert_eq!(
        family_for_style_hint(StyleHint::TypeWriter),
        Some("American Typewriter")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::Courier),
        Some("American Typewriter")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::AnyStyle),
        Some(".AppleSystemUIFont")
    );
    assert_eq!(
        family_for_style_hint(StyleHint::System),
        Some(".AppleSystemUIFont")
    );
    assert_eq!(family_for_style_hint(StyleHint::OldEnglish), None);
    assert_eq!(family_for_style_hint(StyleHint::Decorative), None);
}

/// A face built from memory is shared between requests unless the backend would build a
/// different one: on Windows DirectWrite synthesizes bold from `DemiBold` up and oblique for
/// an italic request, so those split the cache; elsewhere every request shares one face.
#[test]
fn memory_face_keys_split_only_on_what_the_backend_synthesizes() {
    let data = SharedFontData::from_vec(vec![0u8; 16]);
    let hint = HintStyle::HintFull;
    let key = |weight, italic| memory_face_key(&data, hint, weight, italic);
    assert_eq!(key(400, false), key(500, false));
    assert_eq!(key(300, false), key(400, false));
    assert_eq!(key(600, false) == key(400, false), !cfg!(windows));
    assert_eq!(key(600, false), key(900, false));
    assert_eq!(key(400, true) == key(400, false), !cfg!(windows));
    assert_eq!(key(400, true), key(500, true));
    assert_eq!(key(700, true) == key(700, false), !cfg!(windows));
    // The hint style always splits, and so does another buffer.
    assert_ne!(
        key(400, false),
        memory_face_key(&data, HintStyle::HintNone, 400, false)
    );
    let other = SharedFontData::from_vec(vec![0u8; 16]);
    assert_ne!(key(400, false), memory_face_key(&other, hint, 400, false));
}

/// The shaper must open a collection file with the face index of the glyph face, or its glyph
/// ids are those of another member (MS PGothic is the third face of `msgothic.ttc`, whose
/// glyph ids differ from the first face's).
#[cfg(windows)]
#[test]
fn engines_of_collection_members_shape_with_their_own_face() {
    let mut db = FontDatabase::new();
    for (family, index) in [("MS Gothic", 0), ("MS UI Gothic", 1), ("MS PGothic", 2)] {
        let engines = db.resolve_font_engines(&Font::new(family, 16.0));
        let engine = &engines[0];
        assert_eq!(engine.face_index, index, "{family}");
        let shaping = engine.shaping_face(16).expect("shaping face");
        for ch in ['i', 'A', 'あ', '日'] {
            assert_eq!(
                shaping.face().glyph_index(ch).unwrap_or_default().0,
                engine.face.glyph_index(ch),
                "{family}: {ch:?}"
            );
        }
    }
}

/// Han, a geometric shape, an emoji, Thai and Hebrew after Latin text: characters Segoe UI
/// lacks, in several scripts.
#[cfg(windows)]
const FALLBACK_TEXT: &str = "5 \u{5c0f}\u{6642} \u{25a5} \u{1F600} \u{e01} \u{5d0}";

/// The engines a request resolved to (the address of each face) and the fallback chain of
/// each script they carry.
#[cfg(windows)]
fn fallback_signature(engines: &[FontEngine]) -> (Vec<usize>, Vec<(&'static str, Vec<usize>)>) {
    let faces = engines
        .iter()
        .map(|e| Arc::as_ptr(&e.face).cast::<()>() as usize)
        .collect();
    let mut chains: Vec<_> = engines[0]
        .script_chains()
        .expect("a request with fallbacks carries chains")
        .iter()
        .map(|(key, chain)| (*key, chain.clone()))
        .collect();
    chains.sort();
    (faces, chains)
}

/// A memo entry no request makes (U+10FFFF is a noncharacter).
#[cfg(windows)]
fn sentinel_pick() -> (FallbackPickKey, Arc<FallbackPick>) {
    let key = FallbackPickKey {
        list: fallback_list_key(
            StyleHint::AnyStyle,
            false,
            false,
            "Zyyy",
            unicode_script::Script::Common,
        ),
        style: FaceStyle::regular(),
        ch: '\u{10ffff}',
    };
    let pick = Arc::new(FallbackPick {
        covering: Vec::new(),
        has_glyph: false,
    });
    (key, pick)
}

/// A string resolved again gets the same engines and chains from the remembered picks as the
/// first time, when every pick was searched for.
#[cfg(windows)]
#[test]
fn remembered_fallback_picks_give_the_engines_of_the_first_lookup() {
    let mut db = FontDatabase::new();
    let font = Font::new("Segoe UI", 12.0);
    let cold = fallback_signature(&db.resolve_font_engines_for_text(&font, FALLBACK_TEXT));
    assert!(!cold.1.is_empty(), "the text needs fallback families");
    assert!(!db.fallback_memo.picks.is_empty());
    // Another test adding a font empties the memo; try again then.
    let remembered = (0..100).any(|_| {
        db.resolve_font_engines_for_text(&font, FALLBACK_TEXT);
        let generation = font_generation();
        db.fallback_memo.for_generation(generation);
        let (key, pick) = sentinel_pick();
        db.fallback_memo.insert_pick(key, pick);
        let warm = fallback_signature(&db.resolve_font_engines_for_text(&font, FALLBACK_TEXT));
        assert_eq!(warm, cold);
        let (key, _) = sentinel_pick();
        font_generation() == generation && db.fallback_memo.picks.contains_key(&key)
    });
    assert!(remembered, "a second lookup must keep the remembered picks");
}

/// Adding a font to the database drops what was remembered, and the lookup after it gives what a
/// database that had the font from the start gives.
#[cfg(windows)]
#[test]
fn adding_a_font_drops_the_remembered_fallback_picks() {
    let Some(bytes) = any_font_bytes() else {
        return;
    };
    let mut db = FontDatabase::new();
    let font = Font::new("Segoe UI", 12.0);
    let before = fallback_signature(&db.resolve_font_engines_for_text(&font, FALLBACK_TEXT));
    let (key, pick) = sentinel_pick();
    db.fallback_memo.insert_pick(key, pick);
    db.add_font_from_memory(
        "FerrokitCacheProbe",
        SharedFontData::from_vec(bytes.clone()),
    )
    .expect("the font parses");
    let (key, _) = sentinel_pick();
    assert!(
        db.fallback_memo.picks.contains_key(&key),
        "the memo is emptied when it is used, not when the font is added"
    );
    let after = fallback_signature(&db.resolve_font_engines_for_text(&font, FALLBACK_TEXT));
    assert!(
        !db.fallback_memo.picks.contains_key(&key),
        "a font added since the picks were made must drop them"
    );

    let mut fresh = FontDatabase::new();
    fresh
        .add_font_from_memory("FerrokitCacheProbe", SharedFontData::from_vec(bytes))
        .expect("the font parses");
    let expected = fallback_signature(&fresh.resolve_font_engines_for_text(&font, FALLBACK_TEXT));
    assert_eq!(after.1, expected.1);
    assert_eq!(after.0.len(), expected.0.len());
    // An application font under a new name is no fallback family: nothing changes.
    assert_eq!(after.1, before.1);
}

/// A fallback list is the same object on every request, and walked it is the list the registry
/// gives once every family has been populated in the order of Qt's loop.
#[cfg(windows)]
#[test]
fn fallback_lists_are_remembered_and_equal_the_eager_build() {
    let Some(catalog) = crate::text::directwrite_catalog::catalog() else {
        return;
    };
    let Some(eager) = crate::text::directwrite_catalog::eager_catalog() else {
        return;
    };
    let walked = |catalog: &crate::text::directwrite_catalog::Catalog,
                  key: crate::text::directwrite_catalog::FallbackListKey| {
        compute_windows_fallback_families(catalog, key)
            .iter(catalog)
            .map(str::to_string)
            .collect::<Vec<String>>()
    };
    let mut keys = Vec::new();
    for hint in [
        StyleHint::AnyStyle,
        StyleHint::Helvetica,
        StyleHint::TypeWriter,
        StyleHint::Fantasy,
    ] {
        for italic in [false, true] {
            for symbol in [false, true] {
                for script in [
                    unicode_script::Script::Han,
                    unicode_script::Script::Hangul,
                    unicode_script::Script::Hiragana,
                    unicode_script::Script::Thai,
                    unicode_script::Script::Latin,
                    unicode_script::Script::Arabic,
                    unicode_script::Script::Common,
                ] {
                    keys.push(fallback_list_key(hint, italic, symbol, "Zyyy", script));
                }
            }
        }
    }
    keys.push(fallback_list_key(
        StyleHint::AnyStyle,
        false,
        false,
        "Emoji",
        unicode_script::Script::Common,
    ));
    for key in keys {
        let first = windows_fallback_families(catalog, key);
        let again = windows_fallback_families(catalog, key);
        assert!(Arc::ptr_eq(&first, &again), "{key:?}");
        let list: Vec<&str> = first.iter(catalog).collect();
        assert_eq!(list, walked(&eager, key), "{key:?}");
    }
    let emoji = fallback_list_key(
        StyleHint::AnyStyle,
        false,
        false,
        "Emoji",
        unicode_script::Script::Common,
    );
    let list = windows_fallback_families(catalog, emoji);
    assert_eq!(list.iter(catalog).next(), Some("Segoe UI Emoji"));
}

/// A walk that stops in the head of a list does not build its tail (where the catalog allows
/// it), and walking on gives the same entries again.
#[cfg(windows)]
#[test]
fn a_walk_that_stops_in_the_head_leaves_the_tail_unbuilt() {
    let Some(catalog) = crate::text::directwrite_catalog::catalog() else {
        return;
    };
    if !catalog.is_independent() {
        return;
    }
    // No other test asks for this key.
    let key = fallback_list_key(
        StyleHint::Cursive,
        true,
        false,
        "Zyyy",
        unicode_script::Script::Ogham,
    );
    let list = windows_fallback_families(catalog, key);
    if list.head_len() == 0 {
        return;
    }
    let first = list.iter(catalog).next();
    assert!(first.is_some());
    assert!(!list.tail_built());
    let all: Vec<&str> = list.iter(catalog).collect();
    assert!(list.tail_built());
    assert_eq!(all.first().copied(), first);
}
