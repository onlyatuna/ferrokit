// SPDX-License-Identifier: LGPL-3.0-only
//! The OLE drop target: interface discovery, decoding of the real data object, effects.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_platform::drag_drop::win32_ole::{IDropTargetVtbl, OleDropTarget};
use ferrokit_platform::drag_drop::DropEvent;
use ferrokit_platform::integration::platform;
use ferrokit_platform::window::WindowFlags;
use parking_lot::Mutex;
use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{HWND, POINT, POINTL};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};

const S_OK: i32 = 0;
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;
const DV_E_FORMATETC: i32 = 0x8004_0064_u32 as i32;
const E_NOTIMPL: i32 = 0x8000_4001_u32 as i32;

const IID_IMARSHAL: GUID = GUID::from_u128(0x00000003_0000_0000_c000_000000000046);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IDROPTARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);

fn target_with(
    hwnd: HWND,
    events: &Arc<Mutex<Vec<DropEvent>>>,
) -> (*mut OleDropTarget, &'static IDropTargetVtbl) {
    let sink = Arc::clone(events);
    let t = OleDropTarget::new(hwnd, move |ev| {
        let proposed = match &ev {
            DropEvent::Enter { effect, .. }
            | DropEvent::Over { effect, .. }
            | DropEvent::Drop { effect, .. } => *effect,
            DropEvent::Leave => 0,
        };
        sink.lock().push(ev);
        proposed
    });
    let vtbl = unsafe { &*(*t).vtbl };
    (t, vtbl)
}

#[test]
fn query_interface_answers_only_the_interfaces_it_implements() {
    let events = Arc::default();
    let (t, vtbl) = target_with(std::ptr::null_mut(), &events);
    let this = t as *mut c_void;
    unsafe {
        let mut out: *mut c_void = std::ptr::dangling_mut::<c_void>();
        let before = (*t).ref_count.load(Ordering::SeqCst);
        assert_eq!(
            (vtbl.query_interface)(this, &IID_IMARSHAL, &mut out),
            E_NOINTERFACE
        );
        assert!(out.is_null(), "no pointer for a foreign interface");
        assert_eq!(
            (*t).ref_count.load(Ordering::SeqCst),
            before,
            "no reference taken"
        );

        for iid in [&IID_IUNKNOWN, &IID_IDROPTARGET] {
            assert_eq!((vtbl.query_interface)(this, iid, &mut out), S_OK);
            assert_eq!(out, this);
            (vtbl.release)(this);
        }
        assert_eq!((*t).ref_count.load(Ordering::SeqCst), before);
        (vtbl.release)(this);
    }
}

// ---- a minimal IDataObject ----------------------------------------------------------------

#[repr(C)]
struct FormatEtc {
    cf_format: u16,
    ptd: *mut c_void,
    dw_aspect: u32,
    lindex: i32,
    tymed: u32,
}

#[repr(C)]
struct StgMedium {
    tymed: u32,
    h_global: *mut c_void,
    p_unk: *mut c_void,
}

#[repr(C)]
struct MockVtbl {
    qi: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc, *mut StgMedium) -> i32,
    get_data_here: usize,
    query_get_data: unsafe extern "system" fn(*mut c_void, *const FormatEtc) -> i32,
    rest: [usize; 6],
}

#[repr(C)]
struct MockData {
    vtbl: *const MockVtbl,
    text: Option<&'static str>,
    files: Option<&'static [&'static str]>,
}

unsafe extern "system" fn m_qi(_: *mut c_void, _: *const GUID, _: *mut *mut c_void) -> i32 {
    E_NOTIMPL
}
unsafe extern "system" fn m_ref(_: *mut c_void) -> u32 {
    1
}

fn block(bytes: &[u8]) -> *mut c_void {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        let p = GlobalLock(h) as *mut u8;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        h
    }
}

fn supported(this: &MockData, fmt: &FormatEtc) -> bool {
    fmt.tymed & 1 != 0
        && ((fmt.cf_format == CF_UNICODETEXT && this.text.is_some())
            || (fmt.cf_format == CF_HDROP && this.files.is_some()))
}

unsafe extern "system" fn m_query(this: *mut c_void, fmt: *const FormatEtc) -> i32 {
    if supported(&*(this as *const MockData), &*fmt) {
        S_OK
    } else {
        DV_E_FORMATETC
    }
}

unsafe extern "system" fn m_get(
    this: *mut c_void,
    fmt: *const FormatEtc,
    out: *mut StgMedium,
) -> i32 {
    let this = &*(this as *const MockData);
    let fmt = &*fmt;
    if !supported(this, fmt) {
        return DV_E_FORMATETC;
    }
    let bytes: Vec<u8> = if fmt.cf_format == CF_UNICODETEXT {
        this.text
            .unwrap()
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        // DROPFILES: pFiles = 20, pt, fNC, fWide = 1; then UTF-16 names, double NUL.
        let mut b = Vec::new();
        b.extend_from_slice(&20u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]);
        b.extend_from_slice(&0i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        for f in this.files.unwrap() {
            for u in f.encode_utf16().chain([0]) {
                b.extend_from_slice(&u.to_le_bytes());
            }
        }
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    };
    *out = StgMedium {
        tymed: 1,
        h_global: block(&bytes),
        p_unk: std::ptr::null_mut(),
    };
    S_OK
}

static MOCK_VTBL: MockVtbl = MockVtbl {
    qi: m_qi,
    add_ref: m_ref,
    release: m_ref,
    get_data: m_get,
    get_data_here: 0,
    query_get_data: m_query,
    rest: [0; 6],
};

fn data_object(text: Option<&'static str>, files: Option<&'static [&'static str]>) -> MockData {
    MockData {
        vtbl: &MOCK_VTBL,
        text,
        files,
    }
}

#[test]
fn a_drop_delivers_the_text_of_the_data_object() {
    let events = Arc::default();
    let (t, vtbl) = target_with(std::ptr::null_mut(), &events);
    let mut obj = data_object(Some("héllo wörld"), None);
    let mut effect = 3u32; // copy | move offered
    unsafe {
        let this = t as *mut c_void;
        let o = &mut obj as *mut MockData as *mut c_void;
        assert_eq!(
            (vtbl.drag_enter)(this, o, 0, POINTL { x: 5, y: 6 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 1, "plain drag proposes copy");
        assert_eq!(
            (vtbl.drop)(this, o, 0, POINTL { x: 5, y: 6 }, &mut effect),
            S_OK
        );
        (vtbl.release)(this);
    }
    let events = events.lock();
    match &events[0] {
        DropEvent::Enter {
            formats, effect, ..
        } => {
            assert_eq!(formats, &vec!["text/plain".to_string()]);
            assert_eq!(*effect, 1);
        }
        other => panic!("{other:?}"),
    }
    match &events[1] {
        DropEvent::Drop {
            formats,
            data,
            effect,
            ..
        } => {
            assert_eq!(formats, &vec!["text/plain".to_string()]);
            assert_eq!(
                data,
                &vec![("text/plain".to_string(), "héllo wörld".as_bytes().to_vec())]
            );
            assert_eq!(*effect, 1);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_drop_of_files_is_a_uri_list_and_modifier_keys_choose_the_effect() {
    let events = Arc::default();
    let (t, vtbl) = target_with(std::ptr::null_mut(), &events);
    let mut obj = data_object(None, Some(&["C:\\a dir\\f.txt", "\\\\srv\\share\\g.txt"]));
    let mut effect = 7u32;
    unsafe {
        let this = t as *mut c_void;
        let o = &mut obj as *mut MockData as *mut c_void;
        // Shift asks for a move: an accepting target returns DROPEFFECT_MOVE (2).
        assert_eq!(
            (vtbl.drag_enter)(this, o, 0x4, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 2);
        // The source only offers move: move is proposed when accepted.
        effect = 2;
        assert_eq!(
            (vtbl.drag_over)(this, 0x4, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 2);
        // Ctrl+Shift asks for a link (4).
        effect = 7;
        assert_eq!(
            (vtbl.drag_over)(this, 0x0c, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 4);
        assert_eq!(
            (vtbl.drop)(this, o, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        (vtbl.release)(this);
    }
    let events = events.lock();
    let Some(DropEvent::Drop { formats, data, .. }) = events.last() else {
        panic!("{events:?}")
    };
    assert_eq!(formats, &vec!["text/uri-list".to_string()]);
    assert_eq!(
        String::from_utf8(data[0].1.clone()).unwrap(),
        "file:///C:/a%20dir/f.txt\r\nfile://srv/share/g.txt"
    );
}

#[test]
fn a_drag_without_a_decodable_format_is_refused() {
    let events = Arc::default();
    let (t, vtbl) = target_with(std::ptr::null_mut(), &events);
    let mut obj = data_object(None, None);
    let mut effect = 7u32;
    unsafe {
        let this = t as *mut c_void;
        let o = &mut obj as *mut MockData as *mut c_void;
        assert_eq!(
            (vtbl.drag_enter)(this, o, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 0);
        effect = 7;
        assert_eq!(
            (vtbl.drag_over)(this, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 0);
        (vtbl.release)(this);
    }
}

#[test]
fn drag_positions_are_logical_client_coordinates() {
    let win = platform()
        .create_window("Drop", Rect::new(120, 90, 400, 300), WindowFlags::NORMAL)
        .expect("create window");
    let hwnd = win.native_handle() as HWND;
    let events = Arc::default();
    let (t, vtbl) = target_with(hwnd, &events);
    let mut origin = POINT { x: 0, y: 0 };
    unsafe {
        ClientToScreen(hwnd, &mut origin);
    }
    let dpr = ferrokit_platform::window::get_window_dpr(hwnd) as f64;
    let mut effect = 1u32;
    unsafe {
        let this = t as *mut c_void;
        let pt = POINTL {
            x: origin.x + 40,
            y: origin.y + 60,
        };
        (vtbl.drag_over)(this, 0, pt, &mut effect);
        (vtbl.release)(this);
    }
    let events = events.lock();
    let DropEvent::Over { pos, .. } = &events[0] else {
        panic!("{events:?}")
    };
    let expect = Point::new((40.0 / dpr).round() as i32, (60.0 / dpr).round() as i32);
    assert_eq!(
        *pos, expect,
        "screen point (origin + 40,60) is client (40,60) / dpr {dpr}"
    );
}

#[test]
fn a_refusing_callback_writes_dropeffect_none_even_for_decodable_formats_and_shift_move() {
    let t = OleDropTarget::new(std::ptr::null_mut(), |_ev| 0);
    let vtbl = unsafe { &*(*t).vtbl };
    let mut obj = data_object(Some("refused"), None);
    unsafe {
        let this = t as *mut c_void;
        let o = &mut obj as *mut MockData as *mut c_void;
        let mut effect = 7u32;
        assert_eq!(
            (vtbl.drag_enter)(this, o, 0x4, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 0, "refused DragEnter reports DROPEFFECT_NONE");
        effect = 7;
        assert_eq!(
            (vtbl.drag_over)(this, 0x4, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 0, "refused DragOver reports DROPEFFECT_NONE");
        effect = 7;
        assert_eq!(
            (vtbl.drop)(this, o, 0x4, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(
            effect, 0,
            "refused Drop reports DROPEFFECT_NONE so the source never deletes"
        );
        (vtbl.release)(this);
    }
}

#[test]
fn a_callback_may_change_the_effect_but_never_beyond_what_the_source_allows() {
    // The callback answers LINK (4) whatever was proposed.
    let t = OleDropTarget::new(std::ptr::null_mut(), |_ev| 4);
    let vtbl = unsafe { &*(*t).vtbl };
    let mut obj = data_object(Some("relink"), None);
    unsafe {
        let this = t as *mut c_void;
        let o = &mut obj as *mut MockData as *mut c_void;
        let mut effect = 7u32; // copy | move | link
        assert_eq!(
            (vtbl.drag_enter)(this, o, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 4, "the effect the callback chose is reported");
        effect = 7;
        assert_eq!(
            (vtbl.drag_over)(this, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(effect, 4, "DragOver reports the callback's effect");
        effect = 3; // the source allows copy | move only
        assert_eq!(
            (vtbl.drop)(this, o, 0, POINTL { x: 0, y: 0 }, &mut effect),
            S_OK
        );
        assert_eq!(
            effect, 0,
            "an effect the source does not allow is a refusal"
        );
        (vtbl.release)(this);
    }
}
