// SPDX-License-Identifier: LGPL-3.0-only
//! `CocoaClipboard` and the Quartz cursor position against fake `NSPasteboard` / Quartz
//! backends: the call sequences and the Qt mapping are checked on any OS.

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_platform::clipboard_cocoa::{
    CocoaClipboard, PasteboardBackend, mime_for_pasteboard_type, pasteboard_type_for_mime,
};
use ferrokit_platform::cursor_cocoa::{CursorServices, cursor_pos, set_cursor_pos};
use ferrokit_platform::{MimeData, PlatformClipboard};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// `NSPasteboard` semantics: `clearContents` bumps `changeCount` and drops everything;
/// `setData:forType:` appends a type; the log records the call order.
#[derive(Default)]
struct FakePasteboard {
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    count: isize,
    items: Vec<(String, Vec<u8>)>,
    log: Vec<String>,
    reject_type: Option<String>,
}

impl FakePasteboard {
    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    /// Another application takes the pasteboard.
    fn foreign_write(&self, ty: &str, data: &[u8]) {
        let mut s = self.state.lock().unwrap();
        s.count += 1;
        s.items = vec![(ty.to_owned(), data.to_vec())];
    }
}

impl PasteboardBackend for FakePasteboard {
    fn change_count(&self) -> isize {
        self.state.lock().unwrap().count
    }
    fn clear_contents(&self) -> isize {
        let mut s = self.state.lock().unwrap();
        s.count += 1;
        s.items.clear();
        s.log.push("clear".into());
        s.count
    }
    fn types(&self) -> Vec<String> {
        let s = self.state.lock().unwrap();
        s.items.iter().map(|(t, _)| t.clone()).collect()
    }
    fn data_for_type(&self, ty: &str) -> Option<Vec<u8>> {
        let s = self.state.lock().unwrap();
        s.items
            .iter()
            .find(|(t, _)| t == ty)
            .map(|(_, d)| d.clone())
    }
    fn set_data_for_type(&self, data: &[u8], ty: &str) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.reject_type.as_deref() == Some(ty) {
            return false;
        }
        s.log.push(format!("set {ty} {}", data.len()));
        s.items.push((ty.to_owned(), data.to_vec()));
        true
    }
}

fn clipboard() -> (CocoaClipboard, Arc<FakePasteboard>) {
    let pb = Arc::new(FakePasteboard::default());
    (CocoaClipboard::with_backend(pb.clone()), pb)
}

#[test]
fn mime_types_map_to_pasteboard_types_both_ways() {
    assert_eq!(
        pasteboard_type_for_mime("text/plain"),
        "public.utf8-plain-text"
    );
    assert_eq!(pasteboard_type_for_mime("text/html"), "public.html");
    assert_eq!(pasteboard_type_for_mime("image/png"), "public.png");
    assert_eq!(
        pasteboard_type_for_mime("application/x-ferrokit"),
        "com.trolltech.anymime.application--x-ferrokit"
    );
    for mime in [
        "text/plain",
        "text/html",
        "image/png",
        "image/tiff",
        "image/jpeg",
        "application/x-a/b",
    ] {
        assert_eq!(
            mime_for_pasteboard_type(&pasteboard_type_for_mime(mime)).as_deref(),
            Some(mime)
        );
    }
    assert_eq!(mime_for_pasteboard_type("public.url"), None);
    assert_eq!(mime_for_pasteboard_type("com.trolltech.anymime."), None);
}

#[test]
fn set_clears_first_then_writes_every_format() {
    let (cb, pb) = clipboard();
    let mut data = MimeData::new();
    data.set_text("h\u{e9}llo \u{1f680}");
    data.set_html("<b>x</b>");
    data.set_data("image/png", [0u8, 1, 2, 255]);
    data.set_data("application/x-ferrokit", [9u8, 0, 8]);
    cb.set_mime_data(&data).unwrap();

    let log = pb.log();
    assert_eq!(log[0], "clear");
    assert_eq!(log.len(), 5, "one clear, four writes: {log:?}");
    assert!(log.contains(&"set public.png 4".to_string()));
    assert_eq!(cb.mime_data().unwrap(), data);
    assert_eq!(cb.text().unwrap(), "h\u{e9}llo \u{1f680}");
}

#[test]
fn second_set_replaces_the_first() {
    let (cb, _pb) = clipboard();
    cb.set_text("one").unwrap();
    let mut other = MimeData::new();
    other.set_html("<i>two</i>");
    cb.set_mime_data(&other).unwrap();
    let read = cb.mime_data().unwrap();
    assert!(!read.has_format("text/plain"));
    assert_eq!(read.html().as_deref(), Some("<i>two</i>"));
}

#[test]
fn clear_and_empty_pasteboard_read_as_empty() {
    let (cb, _pb) = clipboard();
    assert_eq!(cb.mime_data().unwrap(), MimeData::new());
    assert_eq!(cb.text().unwrap(), "");
    cb.set_text("x").unwrap();
    cb.clear().unwrap();
    assert_eq!(cb.mime_data().unwrap(), MimeData::new());
}

#[test]
fn rejected_write_reports_an_error() {
    let (cb, pb) = clipboard();
    pb.state.lock().unwrap().reject_type = Some("public.html".into());
    let mut data = MimeData::new();
    data.set_html("<b>x</b>");
    assert!(cb.set_mime_data(&data).is_err());
}

#[test]
fn foreign_and_unknown_types_are_handled_on_read() {
    let (cb, pb) = clipboard();
    pb.foreign_write("com.example.opaque", b"http://x");
    assert_eq!(cb.mime_data().unwrap(), MimeData::new());
    pb.foreign_write("public.utf8-plain-text", b"outside");
    assert_eq!(cb.text().unwrap(), "outside");
}

#[test]
fn data_changed_follows_the_change_count() {
    let (cb, pb) = clipboard();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let _conn = cb.data_changed().connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    assert!(!cb.poll_changes(), "nothing changed yet");
    cb.set_text("mine").unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 1, "own write emits once");
    assert!(!cb.poll_changes(), "already seen");

    pb.foreign_write("public.utf8-plain-text", b"theirs");
    assert!(cb.poll_changes());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(!cb.poll_changes());

    cb.clear().unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert_eq!(cb.change_count(), 3);
}

#[test]
fn url_and_legacy_and_utf16_types_are_read() {
    let (cb, pb) = clipboard();
    pb.foreign_write("public.file-url", b"file:///tmp/a.txt");
    assert_eq!(
        cb.mime_data().unwrap().data("text/uri-list"),
        Some(&b"file:///tmp/a.txt"[..])
    );
    pb.foreign_write("public.url", b"https://example.com/");
    assert!(cb.mime_data().unwrap().has_format("text/uri-list"));

    pb.foreign_write("NSStringPboardType", "legacy \u{e9}".as_bytes());
    assert_eq!(cb.text().unwrap(), "legacy \u{e9}");
    pb.foreign_write("NSStringPboardType", &[0xE9, 0xFF]);
    assert_eq!(
        cb.mime_data().unwrap(),
        MimeData::new(),
        "non-UTF-8 legacy data is skipped"
    );

    let utf16: Vec<u8> = "h\u{e9}\u{1f680}"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    pb.foreign_write("public.utf16-plain-text", &utf16);
    assert_eq!(cb.text().unwrap(), "h\u{e9}\u{1f680}");
    let mut bom = vec![0xFE, 0xFF];
    bom.extend("hi".encode_utf16().flat_map(u16::to_be_bytes));
    pb.foreign_write("public.utf16-plain-text", &bom);
    assert_eq!(cb.text().unwrap(), "hi");
}

#[test]
fn utf8_text_wins_over_fallback_types() {
    let (cb, pb) = clipboard();
    {
        let mut s = pb.state.lock().unwrap();
        s.items.push(("NSStringPboardType".into(), b"old".to_vec()));
        s.items
            .push(("public.utf8-plain-text".into(), b"new".to_vec()));
    }
    assert_eq!(cb.text().unwrap(), "new");
}

struct FakeMouse {
    at: Mutex<Option<(f64, f64)>>,
}

impl CursorServices for FakeMouse {
    fn location(&self) -> Option<(f64, f64)> {
        *self.at.lock().unwrap()
    }
    fn warp(&self, x: f64, y: f64) {
        *self.at.lock().unwrap() = Some((x, y));
    }
}

#[test]
fn cursor_position_round_trips_in_top_left_points() {
    let mouse = FakeMouse {
        at: Mutex::new(None),
    };
    assert_eq!(cursor_pos(&mouse), None);
    set_cursor_pos(&mouse, Point::new(-300, 1200));
    assert_eq!(*mouse.at.lock().unwrap(), Some((-300.0, 1200.0)));
    assert_eq!(cursor_pos(&mouse), Some(Point::new(-300, 1200)));
    *mouse.at.lock().unwrap() = Some((10.5, 20.4));
    assert_eq!(cursor_pos(&mouse), Some(Point::new(11, 20)));
    *mouse.at.lock().unwrap() = Some((f64::NAN, 1.0));
    assert_eq!(cursor_pos(&mouse), None);
}
