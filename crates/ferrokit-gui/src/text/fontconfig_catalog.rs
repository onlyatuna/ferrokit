// SPDX-License-Identifier: LGPL-3.0-only
//! The font families Qt's Linux font database knows, built the way it builds them
//! (`QFontconfigDatabase::populateFontDatabase` and `populateFromPattern`, qfontconfigdatabase.cpp,
//! with `qt_registerFont` of qfontdatabase.cpp), and the lookups Qt runs on them
//! (`QFontDatabasePrivate::match`, `bestFoundry` and `bestStyle`, `findFont`'s fallback pass and
//! `QFontconfigDatabase::fallbacksForFamily`).
//!
//! Qt registers every font `FcFontList` returns under the names fontconfig gives it: the first
//! `FC_FAMILY` is the family, the others are aliases of it or, when their style name differs, families
//! of their own ("DejaVu Sans" with the style "Condensed" and a family "DejaVu Sans Condensed"
//! with the style "Book"). A style is keyed by its slant, weight and stretch (`FC_WIDTH`); a request
//! gets the style of the family nearest to it by weight, stretch and slant, and so the 75 % wide
//! face of a family never stands in for the regular one. A name that is no family is replaced by the
//! first family `FcFontSort` lists for it.
//!
//! The data comes from fontconfig when its library is there ([`fontconfig_ffi`]); without it the
//! fonts the directory scan of the font database found are registered instead, and no family is
//! substituted for a missing one.
//!
//! Not modelled: bitmap fonts (`FC_SCALABLE` false: no backend here draws them), foundry
//! names in a requested family (`"Family [Foundry]"` is read, no other foundry rule), named instances
//! of variable fonts (registered under their styles, opened as the default instance), and the
//! application fallback families (`QFontDatabase::addApplicationFallbackFontFamily`).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::text::font::StyleHint;
use crate::text::fontconfig_ffi::{self, CharSet, RawFace, SortRequest};

/// `QFontDatabase::WritingSystem` indices the registry needs.
mod ws {
    pub const LATIN: usize = 1;
}

/// `QFont::Style`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Slant {
    Normal,
    Italic,
    Oblique,
}

impl Slant {
    /// `FC_SLANT` of the request `fallbacksForFamily` builds.
    fn fc_slant(self) -> i32 {
        match self {
            Slant::Normal => 0,
            Slant::Italic => 100,
            Slant::Oblique => 110,
        }
    }

    fn from_fc(slant: i32) -> Self {
        match slant {
            100 => Slant::Italic,
            110 => Slant::Oblique,
            _ => Slant::Normal,
        }
    }
}

/// What a request asks of a family: `QFontDef`'s style name, weight, slant and stretch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Request<'a> {
    /// `QFontDef::styleName`: a style of this name is taken whatever its key (`bestStyle`); empty
    /// for none.
    pub style_name: &'a str,
    /// `QFont::Weight`, 1..=1000.
    pub weight: u16,
    pub slant: Slant,
    /// `QFont::Stretch`; 0 (any) is matched as 100 (unstretched).
    pub stretch: u16,
    /// Only families of color fonts are asked for (the emoji script, `match`'s `colorFont` test).
    pub color: bool,
}

impl Request<'static> {
    pub fn new(weight: u16, slant: Slant) -> Self {
        Self {
            style_name: "",
            weight,
            slant,
            stretch: 0,
            color: false,
        }
    }
}

/// Where the font of a registered style is: the file and the `FC_INDEX` in it (the face of a
/// collection in the low 16 bits, the named instance of a variable font above them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Handle {
    pub path: PathBuf,
    pub index: i32,
}

impl Handle {
    /// The face of the collection the index selects.
    pub fn face_index(&self) -> u32 {
        (self.index & 0xffff) as u32
    }
}

/// A style of a foundry (`QtFontStyle`).
#[derive(Clone, Debug)]
struct Style {
    name: String,
    weight: u16,
    slant: Slant,
    /// `QtFontStyle::Key::stretch`, a signed 12 bit field; 0 matches any stretch.
    stretch: i32,
    handle: Option<Handle>,
}

impl Style {
    /// `QtFontStyle::Key::operator==`.
    fn key_equals(&self, weight: u16, slant: Slant, stretch: i32) -> bool {
        self.slant == slant
            && self.weight == weight
            && (self.stretch == 0 || stretch == 0 || self.stretch == stretch)
    }
}

/// The styles of a family from one foundry (`QtFontFoundry`).
#[derive(Clone, Debug)]
struct Foundry {
    name: String,
    styles: Vec<Style>,
}

/// A family (`QtFontFamily`).
#[derive(Clone, Debug)]
pub(crate) struct Family {
    pub name: String,
    /// [`fold`] of the name.
    folded: String,
    /// [`fold`] of each alias (`QtFontFamily::aliases`, which are only ever compared folded).
    aliases: Vec<String>,
    foundries: Vec<Foundry>,
    /// `QFontDatabase::WritingSystem` bits.
    pub writing_systems: u64,
    pub fixed_pitch: bool,
    pub color: bool,
}

impl Family {
    /// `QtFontFamily::matchesFamilyName` for the [`fold`]ed `name`.
    fn matches_name(&self, folded: &str) -> bool {
        self.folded == folded || self.aliases.iter().any(|alias| alias == folded)
    }
}

/// The result of a lookup: indices into the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    pub family: usize,
    foundry: usize,
    style: usize,
}

/// Case folding for `Qt::CaseInsensitive` comparisons.
fn fold(name: &str) -> String {
    name.to_lowercase()
}

/// `QString::compare(..., Qt::CaseInsensitive)`: by UTF-16 code unit of the folded text.
fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    fold(a).encode_utf16().cmp(fold(b).encode_utf16())
}

/// `weightFromFcWeight`: fontconfig's weights (0 to 215) onto `QFont::Weight`, piecewise linear.
pub(crate) fn weight_from_fc(weight: i32) -> u16 {
    // (fontconfig weight, Qt weight) at the named weights both sides share.
    const STOPS: [(i32, i32); 10] = [
        (0, 100),
        (40, 200),
        (50, 300),
        (80, 400),
        (100, 500),
        (180, 600),
        (200, 700),
        (205, 800),
        (210, 900),
        (215, 1000),
    ];
    if weight <= STOPS[0].0 {
        return STOPS[0].1 as u16;
    }
    for pair in STOPS.windows(2) {
        let ((fc_lower, qt_lower), (fc_upper, qt_upper)) = (pair[0], pair[1]);
        if weight <= fc_upper {
            return (qt_lower
                + ((weight - fc_lower) * (qt_upper - qt_lower)) / (fc_upper - fc_lower))
                as u16;
        }
    }
    1000
}

/// `stretchFromFcWidth`: fontconfig's widths are `QFont::Stretch` values, limited to 1..=4000.
fn stretch_from_fc_width(width: i32) -> i32 {
    width.clamp(1, 4000)
}

/// The language `fallbacksForFamily` adds for the items of a script (`specialLanguages`), by
/// the ISO 15924 code of the script; `None` for the scripts that have none, which take the
/// system's language (or no language for an empty family).
pub(crate) fn special_language(script: &str) -> Option<&'static str> {
    Some(match script {
        "Dsrt" | "Shaw" => "en",
        "Grek" => "el",
        "Cyrl" => "ru",
        "Armn" => "hy",
        "Hebr" => "he",
        "Arab" => "ar",
        "Syrc" => "syr",
        "Thaa" => "dv",
        "Deva" | "Mahj" => "hi",
        "Beng" => "bn",
        "Guru" => "pa",
        "Gujr" => "gu",
        "Orya" => "or",
        "Taml" => "ta",
        "Telu" => "te",
        "Knda" => "kn",
        "Mlym" => "ml",
        "Sinh" => "si",
        "Thai" => "th",
        "Laoo" => "lo",
        "Tibt" => "bo",
        "Mymr" => "my",
        "Geor" => "ka",
        "Hang" => "ko",
        "Ethi" => "am",
        "Cher" => "chr",
        "Cans" => "cr",
        "Ogam" => "sga",
        "Runr" => "non",
        "Khmr" => "km",
        "Mong" => "mn",
        "Hira" | "Kana" => "ja",
        "Bopo" => "zh-TW",
        "Yiii" => "ii",
        "Ital" => "ett",
        "Goth" => "got",
        "Tglg" => "fil",
        "Hano" => "hnn",
        "Buhd" => "bku",
        "Tagb" => "tbw",
        "Copt" => "cop",
        "Limb" => "lif",
        "Tale" => "tdd",
        "Linb" | "Cprt" => "grc",
        "Ugar" => "uga",
        "Osma" => "so",
        "Bugi" => "bug",
        "Talu" => "khb",
        "Glag" => "cu",
        "Tfng" => "shi",
        "Sylo" => "syl",
        "Xpeo" => "peo",
        "Khar" | "Brah" => "pra",
        "Bali" => "ban",
        "Xsux" => "akk",
        "Phnx" => "phn",
        "Phag" => "lzh",
        "Nkoo" => "man",
        "Sund" => "su",
        "Lepc" => "lep",
        "Olck" => "sat",
        "Vaii" => "vai",
        "Saur" => "saz",
        "Kali" => "eky",
        "Rjng" => "rej",
        "Lyci" => "xlc",
        "Cari" => "xcr",
        "Lydi" => "xld",
        "Cham" => "cjm",
        "Lana" => "nod",
        "Tavt" => "blt",
        "Avst" => "ae",
        "Egyp" => "egy",
        "Samr" => "smp",
        "Lisu" => "lis",
        "Bamu" => "bax",
        "Java" => "jv",
        "Mtei" => "mni",
        "Armi" | "Nbat" | "Palm" => "arc",
        "Sarb" => "xsa",
        "Prti" => "xpr",
        "Phli" | "Phlp" => "pal",
        "Orkh" => "otk",
        "Kthi" => "bh",
        "Batk" => "bbc",
        "Mand" => "myz",
        "Cakm" => "ccp",
        "Merc" | "Mero" => "xmr",
        "Plrd" => "hmd",
        "Shrd" | "Gran" | "Sidd" => "sa",
        "Sora" => "srb",
        "Takr" => "doi",
        "Aghb" => "lez",
        "Bass" => "bsq",
        "Dupl" => "fr",
        "Elba" => "sq",
        "Hmng" => "hnj",
        "Khoj" | "Sind" => "sd",
        "Lina" => "lab",
        "Mani" => "xmn",
        "Mend" => "men",
        "Modi" => "mr",
        "Mroo" => "mru",
        "Narb" => "xna",
        "Pauc" => "ctd",
        "Perm" => "kv",
        "Tirh" => "mai",
        "Wara" => "hoc",
        _ => return None,
    })
}

/// The fallback families of a request, by what `fallbacksForFamily` is asked
/// (`QtFontFallbacksCacheKey`); the script is the key of
/// [`fallback_script_key`](crate::text::glyph_layout::fallback_script_key).
type FallbackKey = (String, Slant, StyleHint, &'static str);

/// Qt's registry of families.
#[derive(Default)]
pub(crate) struct Registry {
    /// Sorted by name, case insensitively (`QFontDatabasePrivate::family`).
    families: Vec<Family>,
    fallbacks: HashMap<FallbackKey, Vec<String>>,
    charsets: HashMap<String, Option<CharSet>>,
    /// Whether fontconfig is the source of the rules for substitutes and fallbacks.
    fontconfig: bool,
}

impl Registry {
    /// The registry of `faces`, a fontconfig font list, as `populateFontDatabase` builds it.
    pub fn from_fontconfig(faces: &[RawFace]) -> Self {
        let mut registry = Self {
            fontconfig: true,
            ..Self::default()
        };
        for face in faces {
            registry.register_face(face);
        }
        // `Serif`, `Sans Serif` and `Monospace` are registered without a font behind them.
        for (name, fixed) in [("Serif", false), ("Sans Serif", false), ("Monospace", true)] {
            for slant in [Slant::Normal, Slant::Italic, Slant::Oblique] {
                registry.register(Registration {
                    family: name,
                    style_name: "",
                    foundry: None,
                    weight: 400,
                    slant,
                    stretch: 100,
                    fixed_pitch: fixed,
                    color: false,
                    writing_systems: 1 << ws::LATIN,
                    handle: None,
                });
            }
        }
        registry
    }

    /// A registry without fontconfig: only what [`add_font`](Self::add_font) is given.
    pub fn without_fontconfig() -> Self {
        Self::default()
    }

    /// Registers a font found by scanning a directory (no fontconfig properties: no style name, no
    /// foundry, the Latin writing system).
    #[allow(clippy::too_many_arguments)]
    pub fn add_font(
        &mut self,
        family: &str,
        weight: u16,
        slant: Slant,
        stretch: u16,
        fixed_pitch: bool,
        handle: Handle,
    ) {
        self.register(Registration {
            family,
            style_name: "",
            foundry: None,
            weight,
            slant,
            stretch: i32::from(stretch),
            fixed_pitch,
            color: false,
            writing_systems: 1 << ws::LATIN,
            handle: Some(handle),
        });
    }

    /// `populateFromPattern`, minus the application font bookkeeping.
    fn register_face(&mut self, face: &RawFace) {
        if !face.scalable {
            return;
        }
        let family = &face.families[0];
        let style_name = face.styles[0].clone().unwrap_or_default();
        let slant = Slant::from_fc(face.slant);
        let weight = weight_from_fc(face.weight);
        let stretch = stretch_from_fc_width(face.width);
        let fixed_pitch = face.spacing >= 100;
        let handle = Handle {
            path: PathBuf::from(&face.file),
            index: face.index,
        };
        self.register(Registration {
            family,
            style_name: &style_name,
            foundry: face.foundry.as_deref(),
            weight,
            slant,
            stretch,
            fixed_pitch,
            color: face.color,
            writing_systems: face.writing_systems,
            handle: Some(handle.clone()),
        });
        let family_lang = face.family_langs[0].as_deref().unwrap_or_default();
        for k in 1..face.families.len() {
            let alt_family = &face.families[k];
            // Extra family names can be aliases or subfamilies: one with another style name
            // (in the same language) is registered as a family of its own, so that only members
            // of the subfamily are matched when it is requested.
            let alt_style = face.styles[k].clone().unwrap_or_else(|| style_name.clone());
            let alt_lang = face.family_langs[k].as_deref().unwrap_or(family_lang);
            if family_lang == alt_lang && alt_style != style_name {
                self.register(Registration {
                    family: alt_family,
                    style_name: &alt_style,
                    foundry: face.foundry.as_deref(),
                    weight,
                    slant,
                    stretch,
                    fixed_pitch,
                    color: face.color,
                    writing_systems: face.writing_systems,
                    handle: Some(handle.clone()),
                });
            } else {
                self.add_alias(family, alt_family);
            }
        }
    }

    /// `qt_registerFont`: the family is created when missing, its flags are those of the last
    /// registration, the writing systems accumulate, and a style with the same name and key as
    /// an earlier one takes over its font.
    fn register(&mut self, registration: Registration<'_>) {
        let index = match self
            .families
            .binary_search_by(|f| compare_names(&f.name, registration.family))
        {
            Ok(index) => index,
            Err(index) => {
                self.families.insert(
                    index,
                    Family {
                        name: registration.family.to_string(),
                        folded: fold(registration.family),
                        aliases: Vec::new(),
                        foundries: Vec::new(),
                        writing_systems: 0,
                        fixed_pitch: false,
                        color: false,
                    },
                );
                index
            }
        };
        let family = &mut self.families[index];
        family.fixed_pitch = registration.fixed_pitch;
        family.color = registration.color;
        family.writing_systems |= registration.writing_systems;
        // `QtFontFamily::foundry`: a font without a foundry joins the only foundry a family has;
        // otherwise the foundry is the one of that name, created when missing.
        let foundry = match registration.foundry {
            None if family.foundries.len() == 1 => 0,
            name => {
                let name = name.unwrap_or_default();
                match family
                    .foundries
                    .iter()
                    .position(|f| fold(&f.name) == fold(name))
                {
                    Some(position) => position,
                    None => {
                        family.foundries.push(Foundry {
                            name: name.to_string(),
                            styles: Vec::new(),
                        });
                        family.foundries.len() - 1
                    }
                }
            }
        };
        let foundry = &mut family.foundries[foundry];
        // `QtFontFoundry::style` with every retrieval flag: the style name and the key must match.
        let existing = foundry.styles.iter_mut().find(|s| {
            s.name == registration.style_name
                && s.key_equals(
                    registration.weight,
                    registration.slant,
                    registration.stretch,
                )
        });
        match existing {
            Some(style) => style.handle = registration.handle,
            None => foundry.styles.push(Style {
                name: registration.style_name.to_string(),
                weight: registration.weight,
                slant: registration.slant,
                stretch: registration.stretch,
                handle: registration.handle,
            }),
        }
    }

    /// `qt_registerAliasToFontFamily`.
    fn add_alias(&mut self, family: &str, alias: &str) {
        if alias.is_empty() {
            return;
        }
        let Ok(index) = self
            .families
            .binary_search_by(|f| compare_names(&f.name, family))
        else {
            return;
        };
        let (alias, aliases) = (fold(alias), &mut self.families[index].aliases);
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }

    /// The families in the order of the registry, which is the order of `QFontDatabase::families`.
    pub fn families(&self) -> &[Family] {
        &self.families
    }

    /// The family `name` names (its own name only, not an alias).
    #[cfg(test)]
    pub fn family_named(&self, name: &str) -> Option<&Family> {
        self.families
            .binary_search_by(|f| compare_names(&f.name, name))
            .ok()
            .map(|i| &self.families[i])
    }

    /// Whether some family answers to `name`.
    pub fn has_family(&self, name: &str) -> bool {
        let folded = fold(name);
        self.families.iter().any(|f| f.matches_name(&folded))
    }

    /// Every registered font with the family it is registered under, in registry order: the
    /// family, the file, the face index, whether the family is fixed pitch, the weight and the
    /// slant.
    pub fn fonts(&self) -> impl Iterator<Item = (&Family, &Handle, u16, Slant)> {
        self.families.iter().flat_map(|family| {
            family.foundries.iter().flat_map(move |foundry| {
                foundry.styles.iter().filter_map(move |style| {
                    style
                        .handle
                        .as_ref()
                        .map(|handle| (family, handle, style.weight, style.slant))
                })
            })
        })
    }

    /// The file of a match, `None` for a family registered without one (the generic families).
    pub fn handle(&self, found: &Match) -> Option<&Handle> {
        self.families[found.family].foundries[found.foundry].styles[found.style]
            .handle
            .as_ref()
    }

    pub fn family(&self, found: &Match) -> &Family {
        &self.families[found.family]
    }

    /// `bestStyle`: the style named as the request's style name, else the style nearest to the
    /// request by weight, stretch and slant (the first of equals).
    fn best_style(foundry: &Foundry, request: &Request, stretch: i32) -> usize {
        let mut best = 0;
        let mut distance = 0xffff;
        for (i, style) in foundry.styles.iter().enumerate() {
            if !request.style_name.is_empty() && request.style_name == style.name {
                return i;
            }
            let mut d = ((i32::from(request.weight) - i32::from(style.weight)) / 10).abs();
            if stretch != 0 && style.stretch != 0 {
                d += (stretch - style.stretch).abs();
            }
            if request.slant != style.slant {
                if request.slant != Slant::Normal && style.slant != Slant::Normal {
                    // One is italic, the other oblique.
                    d += 0x0001;
                } else {
                    d += 0x1000;
                }
            }
            if d < distance {
                best = i;
                distance = d;
            }
        }
        best
    }

    /// `bestFoundry` for scalable fonts: the foundry of the family whose best style scores
    /// below `score` (the penalty is 0x2000 for a style that is not exactly the one asked for);
    /// returns the winning score and the foundry and style, if any beat `score`.
    fn best_foundry(
        family: &Family,
        score: u32,
        foundry_name: &str,
        request: &Request,
        stretch: i32,
    ) -> (u32, Option<(usize, usize)>) {
        let mut score = score;
        let mut found = None;
        for (x, foundry) in family.foundries.iter().enumerate() {
            if !foundry_name.is_empty() && fold(&foundry.name) != fold(foundry_name) {
                continue;
            }
            if foundry.styles.is_empty() {
                continue;
            }
            let style = Self::best_style(foundry, request, stretch);
            let mut this_score = 0u32;
            if !foundry.styles[style].key_equals(request.weight, request.slant, stretch) {
                this_score += 0x2000;
            }
            if this_score < score {
                score = this_score;
                found = Some((x, style));
            }
        }
        (score, found)
    }

    /// `QFontDatabasePrivate::match` for a request in no particular script: the family named
    /// `family_name` (any family for an empty name; an alias counts) whose best style scores
    /// lowest, the first of equals, skipping the families in `blacklist`.
    pub fn find(
        &self,
        family_name: &str,
        foundry_name: &str,
        request: &Request,
        blacklist: &[usize],
    ) -> Option<Match> {
        // Prefer a stretch closest to 100.
        let stretch = if request.stretch != 0 {
            i32::from(request.stretch)
        } else {
            100
        };
        let wanted = fold(family_name);
        let mut score = u32::MAX;
        let mut result = None;
        for (x, family) in self.families.iter().enumerate() {
            if blacklist.contains(&x) || (request.color && !family.color) {
                continue;
            }
            if !family_name.is_empty() && !family.matches_name(&wanted) {
                continue;
            }
            let (mut new_score, mut found) =
                Self::best_foundry(family, score, foundry_name, request, stretch);
            if found.is_none() && !foundry_name.is_empty() {
                // The foundry was not found: any foundry will do.
                (new_score, found) = Self::best_foundry(family, score, "", request, stretch);
            }
            if new_score < score
                && let Some((foundry, style)) = found
            {
                result = Some(Match {
                    family: x,
                    foundry,
                    style,
                });
                score = new_score;
            }
            if new_score < 10 {
                break;
            }
        }
        result
    }

    /// The family named `requested` ("Family [Foundry]" is read as
    /// `QFontDatabasePrivate::parseFontName` does) with its best style for `request`
    /// (`findFont`'s family name match pass). `load` says whether the font of a match can be
    /// opened; a family it refuses is put in `blacklist`, as Qt does for a font engine it could
    /// not create.
    pub fn find_exact(
        &self,
        requested: &str,
        request: &Request,
        blacklist: &mut Vec<usize>,
        load: &mut impl FnMut(&Family, &Handle) -> bool,
    ) -> Option<Match> {
        let (family_name, foundry_name) = parse_font_name(requested);
        let found = self.find(&family_name, &foundry_name, request, blacklist)?;
        let usable = self
            .handle(&found)
            .is_some_and(|handle| load(self.family(&found), handle));
        if usable {
            return Some(found);
        }
        blacklist.push(found.family);
        None
    }

    /// The first family `fallbacksForFamily` lists for the missing family `requested` that can
    /// be loaded, with its best style (`findFont`'s fallback pass, for a request in no
    /// particular script).
    pub fn find_substitute(
        &mut self,
        requested: &str,
        request: &Request,
        hint: StyleHint,
        blacklist: &mut Vec<usize>,
        load: &mut impl FnMut(&Family, &Handle) -> bool,
    ) -> Option<Match> {
        if requested.is_empty() {
            return None;
        }
        for name in self.fallbacks_for_family(requested, request.slant, hint, "Zyyy") {
            while let Some(found) = self.find(&name, "", request, blacklist) {
                let usable = self
                    .handle(&found)
                    .is_some_and(|handle| load(self.family(&found), handle));
                if usable {
                    return Some(found);
                }
                blacklist.push(found.family);
            }
        }
        None
    }

    /// `QFontDatabasePrivate::findFont` for a request in no particular script: the family
    /// `requested`, else the first family that replaces it.
    #[cfg(test)]
    pub fn find_font(
        &mut self,
        requested: &str,
        request: &Request,
        hint: StyleHint,
        mut load: impl FnMut(&Family, &Handle) -> bool,
    ) -> Option<Match> {
        let mut blacklist = Vec::new();
        self.find_exact(requested, request, &mut blacklist, &mut load)
            .or_else(|| self.find_substitute(requested, request, hint, &mut blacklist, &mut load))
    }

    /// `fallbacksForFamily` (the platform's list, then `QFontDatabase`'s filter): the families
    /// fontconfig sorts for the request, each once and in its order, without `family` itself and
    /// without any that is not in the registry. `script` is the key of
    /// [`fallback_script_key`](crate::text::glyph_layout::fallback_script_key): the ISO 15924
    /// code of the item's script, or `Emoji`.
    pub fn fallbacks_for_family(
        &mut self,
        family: &str,
        slant: Slant,
        hint: StyleHint,
        script: &'static str,
    ) -> Vec<String> {
        let key = (family.to_string(), slant, hint, script);
        if let Some(cached) = self.fallbacks.get(&key) {
            return cached.clone();
        }
        let mut list = Vec::new();
        if self.fontconfig
            && let Some(sorted) = fontconfig_ffi::sort_families(&SortRequest {
                family,
                slant: slant.fc_slant(),
                language: special_language(script),
                hint,
                emoji: script == "Emoji",
            })
        {
            let mut seen = vec![fold(family)];
            for name in sorted {
                let folded = fold(&name);
                if seen.contains(&folded) {
                    continue;
                }
                seen.push(folded);
                if self.has_family(&name) {
                    list.push(name);
                }
            }
        }
        self.fallbacks.insert(key, list.clone());
        list
    }

    /// `QFontEngineMultiFontConfig::shouldLoadFontEngineForCharacter`: whether the font
    /// fontconfig matches to the fallback family `name` has `ch`; true when it cannot tell.
    pub fn fallback_may_have(&mut self, name: &str, ch: char) -> bool {
        if !self.fontconfig {
            return true;
        }
        let charset = self
            .charsets
            .entry(name.to_string())
            .or_insert_with(|| fontconfig_ffi::matched_charset(name));
        charset.as_ref().is_none_or(|charset| charset.has(ch))
    }
}

/// One call of `qt_registerFont`.
struct Registration<'a> {
    family: &'a str,
    style_name: &'a str,
    foundry: Option<&'a str>,
    weight: u16,
    slant: Slant,
    stretch: i32,
    fixed_pitch: bool,
    color: bool,
    writing_systems: u64,
    handle: Option<Handle>,
}

/// `QFontDatabasePrivate::parseFontName`: `"Family [Foundry]"` into family and foundry. The family
/// and foundry are capitalized per word, which the case insensitive matching does not see.
fn parse_font_name(name: &str) -> (String, String) {
    if let (Some(open), Some(close)) = (name.find('['), name.rfind(']'))
        && open < close
    {
        let foundry = name[open + 1..close].to_string();
        let family = name[..open].strip_suffix(' ').unwrap_or(&name[..open]);
        return (family.to_string(), foundry);
    }
    (name.to_string(), String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(families: &[(&str, &str)], weight: i32, slant: i32, width: i32, file: &str) -> RawFace {
        RawFace {
            families: families.iter().map(|(f, _)| (*f).to_string()).collect(),
            styles: families
                .iter()
                .map(|(_, s)| Some((*s).to_string()))
                .collect(),
            family_langs: families.iter().map(|_| Some("en".to_string())).collect(),
            weight,
            slant,
            spacing: 0,
            width,
            file: file.to_string(),
            index: 0,
            foundry: Some("PfEd".to_string()),
            scalable: true,
            color: false,
            writing_systems: 1 << ws::LATIN,
        }
    }

    fn file_of(registry: &Registry, family: &str, request: Request) -> Option<String> {
        let found = registry.find(family, "", &request, &[])?;
        Some(registry.handle(&found)?.path.to_string_lossy().into_owned())
    }

    #[test]
    fn fontconfig_weights_map_piecewise_onto_qt_weights() {
        assert_eq!(weight_from_fc(0), 100);
        assert_eq!(weight_from_fc(40), 200);
        assert_eq!(weight_from_fc(50), 300);
        assert_eq!(weight_from_fc(80), 400);
        assert_eq!(weight_from_fc(100), 500);
        assert_eq!(weight_from_fc(180), 600);
        assert_eq!(weight_from_fc(200), 700);
        assert_eq!(weight_from_fc(210), 900);
        assert_eq!(weight_from_fc(215), 1000);
        assert_eq!(weight_from_fc(500), 1000);
        // Between two named weights, linear: halfway from Normal (80) to Medium (100).
        assert_eq!(weight_from_fc(90), 450);
        assert_eq!(weight_from_fc(-5), 100);
    }

    /// The 75 % wide face of a family registered under the same family name (DejaVu Sans
    /// Condensed has the family "DejaVu Sans" and the style "Condensed") never stands in for the
    /// regular face, whichever comes first: a request has the stretch 100.
    #[test]
    fn a_condensed_face_does_not_replace_the_regular_one() {
        for order in [[0, 1], [1, 0]] {
            let faces = [
                face(&[("DejaVu Sans", "Book")], 80, 0, 100, "/f/DejaVuSans.ttf"),
                face(
                    &[
                        ("DejaVu Sans", "Condensed"),
                        ("DejaVu Sans Condensed", "Book"),
                    ],
                    80,
                    0,
                    75,
                    "/f/DejaVuSansCondensed.ttf",
                ),
            ];
            let registry =
                Registry::from_fontconfig(&[faces[order[0]].clone(), faces[order[1]].clone()]);
            assert_eq!(
                file_of(&registry, "DejaVu Sans", Request::new(400, Slant::Normal)).as_deref(),
                Some("/f/DejaVuSans.ttf")
            );
            // The subfamily is a family of its own with only the condensed face.
            assert_eq!(
                file_of(
                    &registry,
                    "DejaVu Sans Condensed",
                    Request::new(400, Slant::Normal)
                )
                .as_deref(),
                Some("/f/DejaVuSansCondensed.ttf")
            );
            // Asked for a stretch of 75, the condensed face of the family wins.
            let narrow = Request {
                stretch: 75,
                ..Request::new(400, Slant::Normal)
            };
            assert_eq!(
                file_of(&registry, "DejaVu Sans", narrow).as_deref(),
                Some("/f/DejaVuSansCondensed.ttf")
            );
        }
    }

    #[test]
    fn styles_are_chosen_by_weight_and_slant_distance() {
        let registry = Registry::from_fontconfig(&[
            face(&[("Lib", "Regular")], 80, 0, 100, "/f/regular"),
            face(&[("Lib", "Bold")], 200, 0, 100, "/f/bold"),
            face(&[("Lib", "Italic")], 80, 100, 100, "/f/italic"),
            face(&[("Lib", "Bold Italic")], 200, 100, 100, "/f/bolditalic"),
        ]);
        let pick = |weight, slant| file_of(&registry, "lib", Request::new(weight, slant));
        assert_eq!(pick(400, Slant::Normal).as_deref(), Some("/f/regular"));
        assert_eq!(pick(700, Slant::Normal).as_deref(), Some("/f/bold"));
        assert_eq!(pick(400, Slant::Italic).as_deref(), Some("/f/italic"));
        assert_eq!(pick(700, Slant::Italic).as_deref(), Some("/f/bolditalic"));
        // An oblique request takes the italic face (1, against 0x1000 for the upright one).
        assert_eq!(pick(400, Slant::Oblique).as_deref(), Some("/f/italic"));
        // Between Regular and Bold the nearer one wins and the first one at a tie (550).
        assert_eq!(pick(560, Slant::Normal).as_deref(), Some("/f/bold"));
        assert_eq!(pick(540, Slant::Normal).as_deref(), Some("/f/regular"));
        assert_eq!(pick(550, Slant::Normal).as_deref(), Some("/f/regular"));
    }

    /// A font registered twice with the same style name and key takes the later font, as
    /// `qt_registerFont` replaces the handle.
    #[test]
    fn the_last_registration_of_a_style_wins() {
        let registry = Registry::from_fontconfig(&[
            face(
                &[("Lib", "Regular")],
                80,
                0,
                100,
                "/usr/share/fonts/Lib.ttf",
            ),
            face(&[("Lib", "Regular")], 80, 0, 100, "/home/u/.fonts/Lib.ttf"),
        ]);
        assert_eq!(
            file_of(&registry, "Lib", Request::new(400, Slant::Normal)).as_deref(),
            Some("/home/u/.fonts/Lib.ttf")
        );
    }

    /// A second family name of a face with the same style is an alias; matching ignores case.
    #[test]
    fn extra_family_names_are_aliases() {
        let registry = Registry::from_fontconfig(&[face(
            &[
                ("Noto Sans CJK JP", "Regular"),
                ("Noto Sans CJK JP Regular", "Regular"),
            ],
            80,
            0,
            100,
            "/f/noto",
        )]);
        assert!(registry.family_named("Noto Sans CJK JP Regular").is_none());
        assert_eq!(
            file_of(
                &registry,
                "noto sans cjk jp regular",
                Request::new(400, Slant::Normal)
            )
            .as_deref(),
            Some("/f/noto")
        );
    }

    /// The generic families have no font behind them: they are listed but never load.
    #[test]
    fn generic_families_have_no_handle() {
        let registry =
            Registry::from_fontconfig(&[face(&[("Arial", "Regular")], 80, 0, 100, "/f/arial")]);
        let found = registry
            .find("Sans Serif", "", &Request::new(400, Slant::Normal), &[])
            .expect("registered");
        assert!(registry.handle(&found).is_none());
        let names: Vec<&str> = registry
            .families()
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(names, ["Arial", "Monospace", "Sans Serif", "Serif"]);
    }

    /// An empty family name matches every family: the first by name with the best style.
    #[test]
    fn the_empty_family_is_the_first_family_with_the_best_style() {
        let registry = Registry::from_fontconfig(&[
            face(&[("Zed", "Regular")], 80, 0, 100, "/f/zed"),
            face(&[("Alpha", "Bold")], 200, 0, 100, "/f/alpha-bold"),
            face(&[("Beta", "Regular")], 80, 0, 100, "/f/beta"),
        ]);
        assert_eq!(
            file_of(&registry, "", Request::new(400, Slant::Normal)).as_deref(),
            Some("/f/beta")
        );
    }

    #[test]
    fn bitmap_fonts_are_not_registered() {
        let mut bitmap = face(&[("Fixed", "Regular")], 80, 0, 100, "/f/fixed.pcf");
        bitmap.scalable = false;
        let registry = Registry::from_fontconfig(&[bitmap]);
        assert!(registry.family_named("Fixed").is_none());
    }

    #[test]
    fn scanned_fonts_register_without_fontconfig() {
        let mut registry = Registry::without_fontconfig();
        let handle = |p: &str| Handle {
            path: PathBuf::from(p),
            index: 2,
        };
        registry.add_font("Face", 400, Slant::Normal, 100, false, handle("/f/a"));
        registry.add_font("Face", 700, Slant::Normal, 100, false, handle("/f/b"));
        let found = registry
            .find("face", "", &Request::new(700, Slant::Normal), &[])
            .expect("registered");
        assert_eq!(registry.handle(&found).map(Handle::face_index), Some(2));
        assert_eq!(
            file_of(&registry, "FACE", Request::new(700, Slant::Normal)).as_deref(),
            Some("/f/b")
        );
        // No fontconfig rules: nothing replaces a missing family.
        assert!(
            registry
                .find_font(
                    "Missing",
                    &Request::new(400, Slant::Normal),
                    StyleHint::AnyStyle,
                    |_, _| true
                )
                .is_none()
        );
    }

    #[test]
    fn the_foundry_is_read_from_the_requested_name() {
        assert_eq!(
            parse_font_name("Arial [Monotype]"),
            ("Arial".to_string(), "Monotype".to_string())
        );
        assert_eq!(
            parse_font_name("Arial"),
            ("Arial".to_string(), String::new())
        );
    }

    #[test]
    fn the_language_of_a_script_is_qts_special_language() {
        // Latin items use the engine of the Common script, so they take the system language.
        assert_eq!(special_language("Latn"), None);
        assert_eq!(special_language("Dsrt"), Some("en"));
        assert_eq!(special_language("Arab"), Some("ar"));
        assert_eq!(special_language("Bopo"), Some("zh-TW"));
        assert_eq!(special_language("Hani"), None);
        assert_eq!(special_language("Zyyy"), None);
        assert_eq!(special_language("Emoji"), None);
    }
}
