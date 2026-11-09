// SPDX-License-Identifier: (MIT OR Apache-2.0) AND FTL
//
// The GSUB extension of the adjustment database's reverse character map, ported from FreeType
// 2.14.2 (`src/autofit/afadjust.c`, `src/autofit/afgsub.c` and the hash table of
// `src/base/fthash.c`). Part of the skrifa crate of the fontations project (MIT OR Apache-2.0),
// changed by ferrokit; the FreeType-derived code is under the FreeType licence.
//
// The reference FreeType loads HarfBuzz at run time (Ubuntu's 2.14.2), so this is the branch it
// takes: the glyphs that a single or alternate substitution of the style's script makes from a
// glyph of the map get the code point of that glyph (small capitals of accented letters, for
// example) and so the same adjustment flags.

use alloc::{collections::BTreeSet, vec::Vec};

use raw::{
    tables::{
        gsub::{AlternateSubst, SingleSubst, SubstitutionLookup, SubstitutionSubtables},
        layout::{CoverageTable, ScriptTags},
    },
    types::Tag,
};

use super::{
    super::{
        context::AutohintContext,
        shape::{Shaper, ShaperMode},
    },
    ReverseCharMap,
};

/// FreeType's `FT_Hash` of integer keys (fthash.c): 241 buckets, open addressing that probes
/// backwards, doubling when a third is used. The order of the buckets is the order in which
/// `ft_hash_num_iterator` visits the entries, and the reverse map depends on it: when two
/// glyphs can be substituted by one, the one that is visited first gives its code point.
pub(super) struct FtHash {
    table: Vec<Option<(i32, usize)>>,
    used: usize,
    limit: usize,
}

const INITIAL_HT_SIZE: usize = 241;

impl FtHash {
    pub(super) fn new() -> Self {
        Self {
            table: alloc::vec![None; INITIAL_HT_SIZE],
            used: 0,
            limit: INITIAL_HT_SIZE / 3,
        }
    }

    // fthash.c:hash_num_lookup, the "Mocklisp" hash of the four bytes of the key
    fn hash(key: i32) -> u64 {
        let num = key as i64 as u64;
        let mut res = num & 0xFF;
        for shift in [8, 16, 24] {
            res = (res << 5)
                .wrapping_sub(res)
                .wrapping_add((num >> shift) & 0xFF);
        }
        res
    }

    // fthash.c:hash_bucket
    fn bucket(&self, key: i32) -> usize {
        let size = self.table.len();
        let mut ix = (Self::hash(key) % size as u64) as usize;
        while let Some((k, _)) = self.table[ix] {
            if k == key {
                break;
            }
            ix = if ix == 0 { size - 1 } else { ix - 1 };
        }
        ix
    }

    // fthash.c:hash_rehash
    fn rehash(&mut self) {
        let old = core::mem::take(&mut self.table);
        self.table = alloc::vec![None; old.len() * 2];
        self.limit = self.table.len() / 3;
        for entry in old.into_iter().flatten() {
            let ix = self.bucket(entry.0);
            self.table[ix] = Some(entry);
        }
    }

    // fthash.c:hash_insert, without overwriting
    pub(super) fn insert_no_overwrite(&mut self, key: i32, data: usize) -> bool {
        let ix = self.bucket(key);
        if self.table[ix].is_some() {
            return false;
        }
        self.table[ix] = Some((key, data));
        if self.used >= self.limit {
            self.rehash();
        }
        self.used += 1;
        true
    }

    pub(super) fn get(&self, key: i32) -> Option<usize> {
        self.table[self.bucket(key)].map(|(_, data)| data)
    }

    fn get_mut(&mut self, key: i32) -> Option<&mut usize> {
        let ix = self.bucket(key);
        self.table[ix].as_mut().map(|(_, data)| data)
    }

    // fthash.c:ft_hash_num_iterator
    fn entry_at(&self, index: usize) -> Option<(i32, usize)> {
        self.table.get(index).copied().flatten()
    }

    fn len_buckets(&self) -> usize {
        self.table.len()
    }
}

// afgsub.c:af_hash_insert. A glyph with several substitutes S1..Sn is stored as
// G -> S1 + ((n - 1) << 16) and G + (i << 16) -> S(i+1).
fn hash_insert(map: &mut FtHash, glyph: u16, substitute: u16) {
    let glyph = i32::from(glyph);
    match map.get_mut(glyph) {
        None => {
            map.insert_no_overwrite(glyph, usize::from(substitute));
        }
        Some(value) => {
            let mask = (*value as u32 & 0xFFFF_0000).wrapping_add(0x1_0000);
            *value += 0x1_0000;
            map.insert_no_overwrite((glyph as u32 | mask) as i32, usize::from(substitute));
        }
    }
}

// afgsub.c:af_coverage_iterator. The iterator of format 2 stops as soon as it has read the
// last range, so it returns only the first glyph of that range; the arrays of substitutes are
// read in step.
fn coverage_glyphs(coverage: &CoverageTable) -> Option<Vec<u16>> {
    match coverage {
        CoverageTable::Format1(table) => Some(
            table
                .glyph_array()
                .iter()
                .map(|glyph| glyph.get().to_u16())
                .collect(),
        ),
        CoverageTable::Format2(table) => {
            let ranges = table.range_records();
            let mut glyphs = Vec::new();
            for (ix, range) in ranges.iter().enumerate() {
                let (start, end) = (
                    range.start_glyph_id().to_u16(),
                    range.end_glyph_id().to_u16(),
                );
                if start > end {
                    return None;
                }
                if ix + 1 == ranges.len() {
                    glyphs.push(start);
                } else {
                    glyphs.extend(start..=end);
                }
            }
            Some(glyphs)
        }
        _ => None,
    }
}

// afgsub.c:af_validate_coverage: the number of glyphs a coverage table has
fn coverage_count(coverage: &CoverageTable) -> Option<usize> {
    match coverage {
        CoverageTable::Format1(table) => Some(table.glyph_array().len()),
        CoverageTable::Format2(table) => table.range_records().iter().try_fold(0usize, |n, r| {
            let (start, end) = (r.start_glyph_id().to_u16(), r.end_glyph_id().to_u16());
            (start <= end).then(|| n + usize::from(end - start) + 1)
        }),
        _ => None,
    }
}

// afgsub.c:af_validate_lookup_table: the lookup is used only if it is a single or an alternate
// substitution (or an extension of one kind of them) whose subtables are all well formed.
fn validate_lookup(lookup: &SubstitutionLookup) -> bool {
    let Ok(subtables) = lookup.subtables() else {
        return false;
    };
    match subtables {
        SubstitutionSubtables::Single(tables) => tables.iter().all(|table| match table {
            Ok(SingleSubst::Format1(t)) => t.coverage().is_ok_and(|c| coverage_count(&c).is_some()),
            Ok(SingleSubst::Format2(t)) => t
                .coverage()
                .ok()
                .and_then(|c| coverage_count(&c))
                .is_some_and(|n| n == usize::from(t.glyph_count())),
            _ => false,
        }),
        SubstitutionSubtables::Alternate(tables) => tables.iter().all(|table| match table {
            Ok(AlternateSubst::Format1(t)) => t
                .coverage()
                .ok()
                .and_then(|c| coverage_count(&c))
                .is_some_and(|n| n == usize::from(t.alternate_set_count())),
            _ => false,
        }),
        // An extension lookup without subtables passes the validation and maps nothing.
        SubstitutionSubtables::EmptyExtension => true,
        _ => false,
    }
}

// afgsub.c:af_map_lookup (af_map_single_subst1, af_map_single_subst2, af_map_alternate)
fn map_lookup(map: &mut FtHash, lookup: &SubstitutionLookup) {
    let Ok(subtables) = lookup.subtables() else {
        return;
    };
    match subtables {
        SubstitutionSubtables::Single(tables) => {
            for table in tables.iter().filter_map(|table| table.ok()) {
                match table {
                    SingleSubst::Format1(t) => {
                        let Some(glyphs) = t.coverage().ok().and_then(|c| coverage_glyphs(&c))
                        else {
                            continue;
                        };
                        for glyph in glyphs {
                            // `deltaGlyphID` needs modulo 65536 arithmetic
                            hash_insert(map, glyph, glyph.wrapping_add(t.delta_glyph_id() as u16));
                        }
                    }
                    SingleSubst::Format2(t) => {
                        let Some(glyphs) = t.coverage().ok().and_then(|c| coverage_glyphs(&c))
                        else {
                            continue;
                        };
                        for (glyph, subst) in glyphs.into_iter().zip(t.substitute_glyph_ids()) {
                            hash_insert(map, glyph, subst.get().to_u16());
                        }
                    }
                    _ => {}
                }
            }
        }
        SubstitutionSubtables::Alternate(tables) => {
            for table in tables.iter().filter_map(|table| table.ok()) {
                let AlternateSubst::Format1(t) = table else {
                    continue;
                };
                let Some(glyphs) = t.coverage().ok().and_then(|c| coverage_glyphs(&c)) else {
                    continue;
                };
                let sets = t.alternate_sets();
                for (ix, glyph) in glyphs.into_iter().enumerate() {
                    let Ok(set) = sets.get(ix) else {
                        continue;
                    };
                    for alternate in set.alternate_glyph_ids() {
                        hash_insert(map, glyph, alternate.get().to_u16());
                    }
                }
            }
        }
        _ => {}
    }
}

/// The OpenType script tag that `hb_ot_tags_from_script_and_language` puts first for the script
/// of a style (FreeType asks for one tag); scripts of FreeType that are not Unicode scripts
/// borrow the tag of the script whose glyphs they hold.
fn script_tag(style_script: Tag) -> Tag {
    let unicode = match &style_script.to_be_bytes() {
        b"Latb" | b"Latp" => Tag::new(b"Latn"),
        b"Khms" => Tag::new(b"Khmr"),
        _ => style_script,
    };
    ScriptTags::from_unicode(unicode)[0]
}

// afadjust.c:1392-1396 (hb_ot_layout_collect_lookups with the script, all languages, all
// features): the lookups of every language system of the script, in increasing order.
fn script_lookups(shaper: &Shaper, script: Tag) -> BTreeSet<u16> {
    let mut lookups = BTreeSet::new();
    let Some(gsub) = shaper.gsub() else {
        return lookups;
    };
    let Ok(script_list) = gsub.script_list() else {
        return lookups;
    };
    let Some(script) = script_list
        .index_for_tag(script)
        .and_then(|ix| script_list.script_records().get(ix as usize))
        .and_then(|rec| rec.script(script_list.offset_data()).ok())
    else {
        return lookups;
    };
    let mut features = BTreeSet::new();
    for langsys in script
        .lang_sys_records()
        .iter()
        .filter_map(|rec| rec.lang_sys(script.offset_data()).ok())
        .chain(script.default_lang_sys().transpose().ok().flatten())
    {
        if langsys.required_feature_index() != 0xFFFF {
            features.insert(langsys.required_feature_index());
        }
        features.extend(langsys.feature_indices().iter().map(|ix| ix.get()));
    }
    for feature in features {
        if let Some(indices) = shaper.lookup_indices(feature, true) {
            lookups.extend(indices);
        }
    }
    lookups
}

// afadjust.c:1176 add_substitute (the recursion is a stack here: the order of the visits is the
// same, and a chain of substitutions in a font cannot overflow the call stack).
fn add_substitute(
    glyph_idx: i32,
    value: usize,
    codepoint: u32,
    reverse: &mut ReverseCharMap,
    subst_map: &FtHash,
) {
    struct Frame {
        glyph_idx: i32,
        value: usize,
        stage: usize,
    }
    let mut stack = alloc::vec![Frame {
        glyph_idx,
        value,
        stage: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        let (glyph_idx, value, stage) = (frame.glyph_idx, frame.value, frame.stage);
        frame.stage += 1;
        let target = if stage == 0 {
            // The first substitute; the others are the keys `glyph | (i << 16)`.
            (value & 0xFFFF) as i32
        } else if stage <= value >> 16 {
            match subst_map.get(glyph_idx.wrapping_add((stage << 16) as i32)) {
                Some(substitute) => substitute as i32,
                None => continue,
            }
        } else {
            stack.pop();
            continue;
        };
        if reverse.insert_no_overwrite(target as u32, codepoint) {
            if let Some(substitute) = subst_map.get(target) {
                stack.push(Frame {
                    glyph_idx: target,
                    value: substitute,
                    stage: 0,
                });
            }
        }
    }
}

/// Adds the glyphs that single and alternate substitutions of the script make from the glyphs
/// of `reverse` (afadjust.c:1356-1511).
pub(super) fn extend_reverse_map(
    reverse: &mut ReverseCharMap,
    context: &dyn AutohintContext,
    style_script: Tag,
) {
    let shaper = Shaper::new(context, ShaperMode::BestEffort);
    let Some(gsub) = shaper.gsub() else {
        return;
    };
    let Ok(lookup_list) = gsub.lookup_list() else {
        return;
    };
    let count = lookup_list.lookup_count();
    let mut subst_map = FtHash::new();
    for index in script_lookups(&shaper, script_tag(style_script)) {
        if index >= count {
            continue;
        }
        if let Ok(lookup) = lookup_list.lookups().get(index as usize) {
            if validate_lookup(&lookup) {
                map_lookup(&mut subst_map, &lookup);
            }
        }
    }
    // afadjust.c:1477-1501
    for bucket in 0..subst_map.len_buckets() {
        let Some((glyph_idx, value)) = subst_map.entry_at(bucket) else {
            continue;
        };
        // keys that are not the first substitute are skipped
        if glyph_idx as u32 & 0xFFFF_0000 != 0 {
            continue;
        }
        let Some(codepoint) = reverse.codepoint(glyph_idx as u32) else {
            continue;
        };
        add_substitute(glyph_idx, value, codepoint, reverse, &subst_map);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raw::{FontData, FontRead};

    // fthash.c: 5 * 31^3 = 148955, and 148955 % 241 = 17
    #[test]
    fn hash_buckets_follow_fthash() {
        let mut map = FtHash::new();
        assert!(map.insert_no_overwrite(5, 50));
        assert_eq!(map.entry_at(17), Some((5, 50)));
        assert!(!map.insert_no_overwrite(5, 51));
        assert_eq!(map.get(5), Some(50));
        // A key in the same bucket goes to the one before it.
        let other = (6..100_000)
            .find(|&k| FtHash::hash(k) % 241 == 17)
            .expect("a colliding key");
        assert!(map.insert_no_overwrite(other, 7));
        assert_eq!(map.entry_at(16), Some((other, 7)));
        // Bucket 0 wraps to the last one.
        let first = (6..100_000)
            .find(|&k| FtHash::hash(k) % 241 == 0)
            .expect("a key for bucket 0");
        assert!(map.insert_no_overwrite(first, 1));
        let second = (first + 1..100_000)
            .find(|&k| FtHash::hash(k) % 241 == 0)
            .expect("a second key for bucket 0");
        assert!(map.insert_no_overwrite(second, 2));
        assert_eq!(map.entry_at(240), Some((second, 2)));
    }

    #[test]
    fn hash_doubles_when_a_third_is_used() {
        let mut map = FtHash::new();
        for key in 0..80 {
            map.insert_no_overwrite(key, key as usize);
        }
        assert_eq!(map.len_buckets(), 241);
        // The 81st entry finds `used >= limit` (80) and doubles the table.
        map.insert_no_overwrite(80, 80);
        assert_eq!(map.len_buckets(), 482);
        assert!((0..=80).all(|key| map.get(key) == Some(key as usize)));
    }

    #[test]
    fn several_substitutes_are_stored_with_a_count() {
        let mut map = FtHash::new();
        for substitute in [20, 21, 22] {
            hash_insert(&mut map, 10, substitute);
        }
        assert_eq!(map.get(10), Some(20 + (2 << 16)));
        assert_eq!(map.get(10 | (1 << 16)), Some(21));
        assert_eq!(map.get(10 | (2 << 16)), Some(22));
    }

    // afgsub.c:af_coverage_iterator ends with the read of the last range of a format 2 table and
    // so returns only the first glyph of it.
    #[test]
    fn coverage_iterator_returns_the_first_glyph_of_the_last_range() {
        // format 2, two ranges: 1..=3 (index 0) and 10..=12 (index 3)
        let bytes = [0, 2, 0, 2, 0, 1, 0, 3, 0, 0, 0, 10, 0, 12, 0, 3];
        let coverage = CoverageTable::read(FontData::new(&bytes)).unwrap();
        assert_eq!(coverage_glyphs(&coverage), Some(alloc::vec![1, 2, 3, 10]));
        assert_eq!(coverage_count(&coverage), Some(6));
        // format 1 is complete
        let bytes = [0, 1, 0, 3, 0, 4, 0, 6, 0, 9];
        let coverage = CoverageTable::read(FontData::new(&bytes)).unwrap();
        assert_eq!(coverage_glyphs(&coverage), Some(alloc::vec![4, 6, 9]));
        // a range that ends before it starts is not valid
        let bytes = [0, 2, 0, 1, 0, 5, 0, 3, 0, 0];
        let coverage = CoverageTable::read(FontData::new(&bytes)).unwrap();
        assert_eq!(coverage_glyphs(&coverage), None);
        assert_eq!(coverage_count(&coverage), None);
    }

    // afadjust.c:add_substitute: depth first, no overwriting, the further substitutes of a glyph
    // after the first one's own.
    #[test]
    fn substitutes_get_the_code_point_without_overwriting() {
        let mut subst = FtHash::new();
        hash_insert(&mut subst, 10, 20);
        hash_insert(&mut subst, 10, 30);
        hash_insert(&mut subst, 20, 40);
        hash_insert(&mut subst, 40, 10); // a cycle: 10 is already in the map
        let mut reverse =
            ReverseCharMap::from_cmap(|cp| (cp == 0xE9).then(|| raw::types::GlyphId::new(10)));
        assert!(reverse.insert_no_overwrite(30, 0x41));
        let value = subst.get(10).unwrap();
        add_substitute(10, value, 0xE9, &mut reverse, &subst);
        assert_eq!(reverse.codepoint(10), Some(0xE9));
        assert_eq!(reverse.codepoint(20), Some(0xE9));
        assert_eq!(reverse.codepoint(40), Some(0xE9));
        // 30 had its own code point
        assert_eq!(reverse.codepoint(30), Some(0x41));
    }
}
