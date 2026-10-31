// SPDX-License-Identifier: LGPL-3.0-only
//! `SharedFontData::from_file` maps the font file on Windows instead of copying it, and copies it
//! elsewhere, where a mapped file can be truncated under the mapping.
use ferrokit_gui::text::font::SharedFontData;

fn temp_file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("ferrokit-font-map-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn mapped_data_equals_the_file_and_is_shared_by_clones() {
    let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let path = temp_file("data.ttf", &bytes);

    let data = SharedFontData::from_file(&path).unwrap();
    assert_eq!(data.as_slice(), bytes.as_slice());
    assert_eq!(data.len(), bytes.len());

    let clone = data.clone();
    assert!(data.ptr_eq(&clone));
    assert_eq!(data.strong_count(), 2);
    // Same bytes in a separate buffer compare equal but are not the same buffer.
    let heap = SharedFontData::from_vec(bytes);
    assert_eq!(data, heap);
    assert!(!data.ptr_eq(&heap));

    drop((data, clone));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn an_empty_file_loads_as_empty_data_and_a_missing_one_is_an_error() {
    let path = temp_file("empty.ttf", &[]);
    let data = SharedFontData::from_file(&path).unwrap();
    assert!(data.is_empty());
    std::fs::remove_file(path).unwrap();

    assert!(SharedFontData::from_file(std::path::Path::new("definitely/not/a/font.ttf")).is_err());
}

/// Truncating the file after loading never takes bytes away from the loaded data: Windows refuses
/// to shrink a mapped file, and elsewhere the data is a copy (a mapping would raise `SIGBUS`).
#[test]
fn truncating_the_file_leaves_the_loaded_data_intact() {
    let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 249) as u8).collect();
    let path = temp_file("truncated.ttf", &bytes);
    let data = SharedFontData::from_file(&path).unwrap();
    let truncated = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0);
    assert_eq!(truncated.is_ok(), !cfg!(windows));
    assert_eq!(data.as_slice(), bytes.as_slice());
    drop(data);
    std::fs::remove_file(path).unwrap();
}
