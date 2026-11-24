// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

/// Maximum directory depth scanned below each search path (Linux font trees are nested).
pub(super) const MAX_SCAN_DEPTH: usize = 4;

/// Scans font directories for `.ttf`/`.otf`/`.ttc`/`.otc` files and reads their family names.
pub(super) fn scan_font_directories(roots: &[PathBuf]) -> Vec<FontFamilyInfo> {
    let mut faces = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = roots.iter().map(|root| (root.clone(), 0)).collect();
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_SCAN_DEPTH {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            let is_font = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "ttf" | "otf" | "ttc" | "otc"
                    )
                })
                .unwrap_or(false);
            if !is_font {
                continue;
            }
            let Ok(mut file) = File::open(&path) else {
                continue;
            };
            for mut face in read_faces(&mut file) {
                face.path = Some(path.clone());
                faces.push(face);
            }
        }
    }
    faces
}

pub(super) fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

pub(super) fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

pub(super) fn read_exact_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    len: usize,
) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Reads family metadata for every face of an sfnt file or collection, touching only
/// the header, table directory, `name` and `post` tables.
pub(super) fn read_faces<R: Read + Seek>(reader: &mut R) -> Vec<FontFamilyInfo> {
    let Some(header) = read_exact_at(reader, 0, 12) else {
        return Vec::new();
    };
    let face_offsets: Vec<u32> = if &header[0..4] == b"ttcf" {
        let count = be_u32(&header, 8).unwrap_or(0).min(256) as usize;
        match read_exact_at(reader, 12, count * 4) {
            Some(table) => (0..count).filter_map(|i| be_u32(&table, i * 4)).collect(),
            None => return Vec::new(),
        }
    } else {
        vec![0]
    };
    face_offsets
        .into_iter()
        .enumerate()
        .filter_map(|(index, offset)| read_face(reader, offset as u64, index as u32))
        .collect()
}

/// Maximum `name` table size accepted (real tables are a few KiB).
pub(super) const MAX_NAME_TABLE_LEN: u32 = 1 << 20;

pub(super) fn read_face<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    face_index: u32,
) -> Option<FontFamilyInfo> {
    let offset_table = read_exact_at(reader, offset, 12)?;
    let num_tables = be_u16(&offset_table, 4)? as usize;
    let records = read_exact_at(reader, offset + 12, num_tables * 16)?;
    let mut name_table = None;
    let mut post_table = None;
    let mut os2_table = None;
    for i in 0..num_tables {
        let record = &records[i * 16..i * 16 + 16];
        let table_offset = be_u32(record, 8)?;
        let table_len = be_u32(record, 12)?;
        match &record[0..4] {
            b"name" => name_table = Some((table_offset, table_len)),
            b"post" => post_table = Some((table_offset, table_len)),
            b"OS/2" => os2_table = Some((table_offset, table_len)),
            _ => {}
        }
    }
    let (name_offset, name_len) = name_table?;
    if name_len > MAX_NAME_TABLE_LEN {
        return None;
    }
    let name_data = read_exact_at(reader, name_offset as u64, name_len as usize)?;
    let family = preferred_family_name(&NameTable::parse(&name_data)?)?;
    // post table: version (4), italicAngle (4), underlinePosition (2), underlineThickness (2), isFixedPitch (4).
    let fixed_pitch = post_table
        .filter(|&(_, len)| len >= 16)
        .and_then(|(post_offset, _)| read_exact_at(reader, post_offset as u64 + 12, 4))
        .and_then(|bytes| be_u32(&bytes, 0))
        .map(|flag| flag != 0)
        .unwrap_or(false);
    // OS/2 table: usWeightClass at 4, usWidthClass at 6, fsSelection at 62 (bit 0 italic, bit 9
    // oblique).
    let (weight, italic, stretch) = os2_table
        .filter(|&(_, len)| len >= 64)
        .and_then(|(os2_offset, _)| read_exact_at(reader, os2_offset as u64, 64))
        .map(|os2| {
            let weight = be_u16(&os2, 4).unwrap_or(400);
            // The `QFont::Stretch` fontconfig derives from the width class (`FC_WIDTH`).
            let stretch = match be_u16(&os2, 6).unwrap_or(5) {
                1 => 50,
                2 => 63,
                3 => 75,
                4 => 87,
                6 => 113,
                7 => 125,
                8 => 150,
                9 => 200,
                _ => 100,
            };
            let selection = be_u16(&os2, 62).unwrap_or(0);
            (weight, selection & ((1 << 0) | (1 << 9)) != 0, stretch)
        })
        .unwrap_or((400, false, 100));
    Some(FontFamilyInfo {
        family,
        path: None,
        face_index,
        fixed_pitch,
        weight,
        italic,
        stretch,
    })
}

/// Picks the family name, preferring the typographic family (name ID 16) over the legacy
/// family (ID 1) and US-English Windows records over other languages.
pub(super) fn preferred_family_name(table: &NameTable) -> Option<String> {
    const WINDOWS_EN_US: u16 = 0x0409;
    let mut best: Option<(u32, String)> = None;
    for name in table.names {
        let id_rank = match name.name_id {
            name_id::TYPOGRAPHIC_FAMILY => 0,
            name_id::FAMILY => 1,
            _ => continue,
        };
        let lang_rank = match (name.platform_id, name.language_id) {
            (PlatformId::Windows, WINDOWS_EN_US) => 0,
            (PlatformId::Unicode, _) => 1,
            (PlatformId::Macintosh, 0) => 2,
            _ => 3,
        };
        let rank = id_rank * 4 + lang_rank;
        if best
            .as_ref()
            .is_some_and(|(best_rank, _)| *best_rank <= rank)
        {
            continue;
        }
        let text = if name.is_unicode() {
            name.to_string()
        } else if name.platform_id == PlatformId::Macintosh && name.encoding_id == 0 {
            // Mac Roman: ASCII range is identical; drop anything outside it.
            Some(
                name.name
                    .iter()
                    .filter(|b| b.is_ascii())
                    .map(|&b| b as char)
                    .collect(),
            )
        } else {
            None
        };
        if let Some(text) = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            best = Some((rank, text));
        }
    }
    best.map(|(_, text)| text)
}
