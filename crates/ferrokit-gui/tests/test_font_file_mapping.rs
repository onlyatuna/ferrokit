// SPDX-License-Identifier: LGPL-3.0-only
//! `SharedFontData::from_file` copies font files safely, while `SharedFontData::from_file_mmap`
//! maps immutable font files on Windows with `FILE_SHARE_READ` and copies elsewhere.
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

    let safe_data = SharedFontData::from_file(&path).unwrap();
    assert_eq!(safe_data.as_slice(), bytes.as_slice());

    // SAFETY: `path` is a private temporary test file not written while mapped.
    let data = unsafe { SharedFontData::from_file_mmap(&path) }.unwrap();
    assert_eq!(data.as_slice(), bytes.as_slice());
    assert_eq!(data.len(), bytes.len());

    let clone = data.clone();
    assert!(data.ptr_eq(&clone));
    assert_eq!(data.strong_count(), 2);
    // Same bytes in a separate buffer compare equal but are not the same buffer.
    let heap = SharedFontData::from_vec(bytes);
    assert_eq!(data, heap);
    assert!(!data.ptr_eq(&heap));

    drop((safe_data, data, clone));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn an_empty_file_loads_as_empty_data_and_a_missing_one_is_an_error() {
    let path = temp_file("empty.ttf", &[]);
    let safe_data = SharedFontData::from_file(&path).unwrap();
    assert!(safe_data.is_empty());
    // SAFETY: `path` is an empty private temporary file not written while loaded.
    let data = unsafe { SharedFontData::from_file_mmap(&path) }.unwrap();
    assert!(data.is_empty());
    std::fs::remove_file(path).unwrap();

    let missing = std::path::Path::new("definitely/not/a/font.ttf");
    assert!(SharedFontData::from_file(missing).is_err());
    // SAFETY: missing path, no mapping is created.
    assert!(unsafe { SharedFontData::from_file_mmap(missing) }.is_err());
}

/// Safe `from_file` keeps a copy even if the file is overwritten/truncated; `from_file_mmap` on
/// Windows holds a `FILE_SHARE_READ` handle that rejects writers while mapped.
#[test]
fn truncating_the_file_leaves_the_loaded_data_intact() {
    let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 249) as u8).collect();
    let path = temp_file("truncated.ttf", &bytes);

    let safe_data = SharedFontData::from_file(&path).unwrap();
    // SAFETY: `path` is a private temporary file; we test that Windows rejects write opens.
    let mapped = unsafe { SharedFontData::from_file_mmap(&path) }.unwrap();
    let write_open = std::fs::OpenOptions::new().write(true).open(&path);
    if cfg!(windows) {
        assert!(
            write_open.is_err(),
            "mapped font file must deny concurrent write opens on Windows"
        );
    } else {
        write_open.unwrap().set_len(0).unwrap();
    }
    assert_eq!(safe_data.as_slice(), bytes.as_slice());
    assert_eq!(mapped.as_slice(), bytes.as_slice());
    drop(mapped);

    // After unmapping, truncating succeeds and `safe_data` still retains its own copy.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert_eq!(safe_data.as_slice(), bytes.as_slice());
    drop(safe_data);
    std::fs::remove_file(path).unwrap();
}
