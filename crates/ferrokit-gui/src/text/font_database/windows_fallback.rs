// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

/// The families `QWindowsFontDatabaseBase::extraTryFontsForFamily` tries for the user's language
/// (`GetUserDefaultLangID`): Chinese for China and Singapore, Chinese for the other regions,
/// Japanese, Korean, and the others' list.
#[cfg(windows)]
pub(super) fn extra_try_fonts() -> &'static [&'static str] {
    use windows::Win32::Globalization::GetUserDefaultLangID;
    static LIST: std::sync::LazyLock<&'static [&'static str]> = std::sync::LazyLock::new(|| {
        // SAFETY: the call takes no arguments and only reads the user's settings.
        let lid = unsafe { GetUserDefaultLangID() };
        match lid & 0xff {
            0x04 if lid == 0x0804 || lid == 0x1004 => &[
                "SimSun",
                "Arial",
                "PMingLiU",
                "Gulim",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            0x04 => &[
                "PMingLiU",
                "Arial",
                "SimSun",
                "Gulim",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            0x11 => &[
                "Yu Gothic UI",
                "MS UI Gothic",
                "Arial",
                "Gulim",
                "SimSun",
                "PMingLiU",
                "Arial Unicode MS",
            ],
            0x12 => &[
                "Malgun Gothic",
                "Gulim",
                "Arial",
                "PMingLiU",
                "SimSun",
                "MS UI Gothic",
                "Arial Unicode MS",
            ],
            _ => &[
                "Arial",
                "MS UI Gothic",
                "Gulim",
                "SimSun",
                "PMingLiU",
                "Arial Unicode MS",
            ],
        }
    });
    *LIST
}

/// The key of `windows_fallback_families`' list for a character of the item script `script`
/// whose chain is `key` ([`fallback_script_key`](crate::text::glyph_layout::fallback_script_key)).
/// The emoji chain asks for no writing system.
#[cfg(windows)]
pub(super) fn fallback_list_key(
    hint: StyleHint,
    italic: bool,
    symbol_family: bool,
    key: &str,
    script: unicode_script::Script,
) -> crate::text::directwrite_catalog::FallbackListKey {
    let emoji = key == "Emoji";
    crate::text::directwrite_catalog::FallbackListKey {
        hint,
        italic,
        symbol_family,
        emoji,
        writing_system: if emoji {
            0
        } else {
            crate::text::directwrite_catalog::writing_system_for_script(script)
        },
    }
}

/// `fallbacksForFamily(family, style, hint, script)` of Qt's DirectWrite font database as
/// `QFontDatabasePrivate` filters it: the emoji family for the emoji script, the family of the
/// style hint, `extraTryFontsForFamily` (the user's language's families that exist, unless the
/// family is a symbol font, then the emoji and symbol families), then every family that supports
/// the script's writing system. Entries that are not families of the database are dropped; repeats
/// stay. Built once per key and kept by the catalog (`QFontDatabasePrivate::fallbacksCache`); the
/// families that support the writing system are listed when a walk of the list gets to them
/// (see [`FallbackList`](crate::text::directwrite_catalog::FallbackList)).
#[cfg(windows)]
pub(super) fn windows_fallback_families(
    catalog: &crate::text::directwrite_catalog::Catalog,
    key: crate::text::directwrite_catalog::FallbackListKey,
) -> Arc<crate::text::directwrite_catalog::FallbackList> {
    catalog.fallback_list(key, || compute_windows_fallback_families(catalog, key))
}

/// [`windows_fallback_families`] without the memo.
#[cfg(windows)]
pub(super) fn compute_windows_fallback_families(
    catalog: &crate::text::directwrite_catalog::Catalog,
    key: crate::text::directwrite_catalog::FallbackListKey,
) -> crate::text::directwrite_catalog::FallbackList {
    let mut head: Vec<String> = Vec::new();
    if key.emoji {
        head.push("Segoe UI Emoji".to_string());
    }
    head.push(family_for_style_hint(key.hint).to_string());
    if !key.symbol_family {
        for name in extra_try_fonts() {
            if catalog.family_traits(name).is_some() {
                head.push((*name).to_string());
            }
        }
    }
    head.push("Segoe UI Emoji".to_string());
    head.push("Segoe UI Symbol".to_string());
    let mut list = crate::text::directwrite_catalog::FallbackList::new(head, key);
    if !catalog.is_independent() {
        // The registry is complete before the head is filtered, as Qt's loop leaves it.
        list.tail(catalog);
    }
    list.retain_head(|name| catalog.family_traits(name).is_some());
    list
}

/// What `append_script_fallback_engines` depends on in the family of a request alone: the style
/// hint its fallback lists use (a fixed-pitch primary family makes `AnyStyle` a typewriter) and
/// whether the family is a symbol font.
#[cfg(windows)]
#[derive(Clone, Copy)]
pub(super) struct FallbackContext {
    pub(super) hint: StyleHint,
    pub(super) symbol_family: bool,
}

/// What one family takes for one character: the families of a fallback list that cover the
/// character and whose face loads, in list order with their positions, up to the first whose face
/// has a glyph for it (`has_glyph`, then it is the last of `covering`).
#[cfg(windows)]
pub(super) struct FallbackPick {
    pub(super) covering: Vec<(usize, Arc<str>)>,
    pub(super) has_glyph: bool,
}

/// A [`FallbackPick`] is the answer for this list, request style and character.
#[cfg(windows)]
#[derive(PartialEq, Eq, Hash)]
pub(super) struct FallbackPickKey {
    pub(super) list: crate::text::directwrite_catalog::FallbackListKey,
    pub(super) style: FaceStyle,
    pub(super) ch: char,
}

/// Most picks kept before the memo starts over; a character needs one entry per request style.
#[cfg(windows)]
pub(super) const MAX_FALLBACK_PICKS: usize = 1 << 16;

/// The part of the Windows fallback lookup that depends on the font database's own state, kept
/// so a string measured or drawn again does not search the registry again (Qt keeps its
/// `fallbacksCache` the same way and drops it when a font is added).
///
/// A pick depends on the faces `load_font_styled` returns, which change when a font or search
/// path is added to the database; every such change (and a change of the default hint style or
/// device pixel ratio) bumps [`font_generation`], and the memo is emptied the next time it is
/// used under another generation. The DirectWrite catalog the lists and `covers` come from is
/// built once per process from the system collection, as Qt builds its database, so a font
/// installed in the operating system while the process runs is not seen by either.
#[cfg(windows)]
#[derive(Default)]
pub(super) struct FallbackMemo {
    /// The [`font_generation`] the entries below belong to.
    pub(super) generation: u64,
    /// [`FallbackContext`] per family string and style hint of a request.
    pub(super) contexts: HashMap<String, Vec<(StyleHint, FallbackContext)>>,
    pub(super) picks: HashMap<FallbackPickKey, Arc<FallbackPick>>,
}

#[cfg(windows)]
impl FallbackMemo {
    /// Drops every entry made under another generation than `generation`.
    pub(super) fn for_generation(&mut self, generation: u64) {
        if self.generation != generation {
            self.contexts.clear();
            self.picks.clear();
            self.generation = generation;
        }
    }

    pub(super) fn insert_pick(&mut self, key: FallbackPickKey, pick: Arc<FallbackPick>) {
        if self.picks.len() >= MAX_FALLBACK_PICKS {
            self.picks.clear();
        }
        self.picks.insert(key, pick);
    }

    /// The context of `font`'s request: the family it names decides the symbol flag if the
    /// catalog knows it, else the family of its style hint does; a fixed-pitch family turns
    /// `AnyStyle` into `TypeWriter`.
    pub(super) fn context(
        &mut self,
        catalog: &crate::text::directwrite_catalog::Catalog,
        font: &Font,
    ) -> FallbackContext {
        if let Some(&(_, context)) = self
            .contexts
            .get(font.family.as_str())
            .and_then(|entries| entries.iter().find(|(hint, _)| *hint == font.style_hint))
        {
            return context;
        }
        let primary_name = if catalog.family_traits(&font.family).is_some() {
            font.family.clone()
        } else {
            family_for_style_hint(font.style_hint).to_string()
        };
        let (fixed_pitch, systems) = catalog.family_traits(&primary_name).unwrap_or((false, 0));
        let context = FallbackContext {
            hint: if font.style_hint == StyleHint::AnyStyle && fixed_pitch {
                StyleHint::TypeWriter
            } else {
                font.style_hint
            },
            symbol_family: crate::text::directwrite_catalog::supports_symbol(systems),
        };
        self.contexts
            .entry(font.family.clone())
            .or_default()
            .push((font.style_hint, context));
        context
    }
}
