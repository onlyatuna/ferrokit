// SPDX-License-Identifier: LGPL-3.0-only
//! Linux X11 clipboard backend (`CLIPBOARD` selection via `x11rb`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SelectionNotifyEvent, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::clipboard::{MimeData, PlatformClipboard};

struct OwnerState {
    data: Mutex<MimeData>,
    owns_selection: AtomicBool,
    stop: AtomicBool,
    /// Wake-up target for stopping the selection-owner thread cleanly on Drop.
    wake: Mutex<Option<(Arc<RustConnection>, u32, Atom)>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl OwnerState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(MimeData::new()),
            owns_selection: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            wake: Mutex::new(None),
            thread: Mutex::new(None),
        })
    }

    fn stop_thread(&self) {
        self.stop.store(true, Ordering::Release);
        self.owns_selection.store(false, Ordering::Release);
        if let Some((conn, win, atom)) = ferrokit_core::poison::lock_unpoisoned(&self.wake).take() {
            let msg = ClientMessageEvent::new(32, win, atom, [0u32; 5]);
            let _ = conn.send_event(false, win, EventMask::NO_EVENT, msg);
            let _ = conn.flush();
        }
        if let Some(handle) = ferrokit_core::poison::lock_unpoisoned(&self.thread).take() {
            let _ = handle.join();
        }
    }
}

/// X11 `CLIPBOARD` selection implementation of [`PlatformClipboard`].
pub struct X11Clipboard {
    owner: Arc<OwnerState>,
}

impl Default for X11Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl X11Clipboard {
    pub fn new() -> Self {
        Self {
            owner: OwnerState::new(),
        }
    }

    fn ensure_owner_thread(&self) -> Result<(), &'static str> {
        let guard = ferrokit_core::poison::lock_unpoisoned(self.thread_slot());
        if guard.is_some() && self.owner.owns_selection.load(Ordering::Acquire) {
            return Ok(());
        }
        drop(guard);

        // Restart cleanly if a previous owner thread exited (e.g. SelectionClear).
        self.owner.stop_thread();
        self.owner.stop.store(false, Ordering::Release);

        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|_| "Failed to connect to X11 for clipboard")?;
        let conn = Arc::new(conn);
        let root = conn.setup().roots[screen_num].root;
        let win = conn
            .generate_id()
            .map_err(|_| "Failed to generate X11 clipboard window id")?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|_| "Failed to create X11 clipboard window")?;

        let clipboard_atom = intern_atom(&conn, b"CLIPBOARD")?;
        let targets_atom = intern_atom(&conn, b"TARGETS")?;
        let utf8_atom = intern_atom(&conn, b"UTF8_STRING")?;
        let string_atom: Atom = AtomEnum::STRING.into();
        let text_plain_atom = intern_atom(&conn, b"text/plain")?;
        let text_plain_utf8_atom = intern_atom(&conn, b"text/plain;charset=utf-8")?;
        let wake_atom = intern_atom(&conn, b"FERROKIT_CLIPBOARD_WAKE")?;

        conn.set_selection_owner(win, clipboard_atom, x11rb::CURRENT_TIME)
            .map_err(|_| "set_selection_owner failed")?;
        let _ = conn.flush();

        let owner_check = conn
            .get_selection_owner(clipboard_atom)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.owner);
        if owner_check != Some(win) {
            let _ = conn.destroy_window(win);
            let _ = conn.flush();
            return Err("Failed to acquire X11 CLIPBOARD ownership");
        }

        self.owner.owns_selection.store(true, Ordering::Release);
        *ferrokit_core::poison::lock_unpoisoned(&self.owner.wake) =
            Some((Arc::clone(&conn), win, wake_atom));

        let state = Arc::clone(&self.owner);
        let handle = std::thread::Builder::new()
            .name("ferrokit-x11-clipboard".to_string())
            .spawn(move || {
                while !state.stop.load(Ordering::Acquire) {
                    let event = match conn.wait_for_event() {
                        Ok(ev) => ev,
                        Err(_) => break,
                    };
                    match event {
                        Event::SelectionClear(ev) if ev.selection == clipboard_atom => {
                            state.owns_selection.store(false, Ordering::Release);
                            break;
                        }
                        Event::SelectionRequest(req) if req.selection == clipboard_atom => {
                            let property = if req.property == Atom::from(AtomEnum::NONE) {
                                req.target
                            } else {
                                req.property
                            };
                            let data_snapshot =
                                ferrokit_core::poison::lock_unpoisoned(&state.data).clone();
                            let served = if req.target == targets_atom {
                                let mut atoms: Vec<u32> = vec![targets_atom];
                                if data_snapshot.has_format("text/plain") {
                                    atoms.extend_from_slice(&[
                                        utf8_atom,
                                        string_atom,
                                        text_plain_atom,
                                        text_plain_utf8_atom,
                                    ]);
                                }
                                for fmt in data_snapshot.formats() {
                                    if fmt != "text/plain"
                                        && let Ok(a) = intern_atom(&conn, fmt.as_bytes())
                                    {
                                        atoms.push(a);
                                    }
                                }
                                conn.change_property32(
                                    PropMode::REPLACE,
                                    req.requestor,
                                    property,
                                    AtomEnum::ATOM,
                                    &atoms,
                                )
                                .is_ok()
                            } else if req.target == utf8_atom
                                || req.target == string_atom
                                || req.target == text_plain_atom
                                || req.target == text_plain_utf8_atom
                            {
                                if let Some(bytes) = data_snapshot.data("text/plain") {
                                    conn.change_property8(
                                        PropMode::REPLACE,
                                        req.requestor,
                                        property,
                                        req.target,
                                        bytes,
                                    )
                                    .is_ok()
                                } else {
                                    false
                                }
                            } else if let Some(name) = atom_name(&conn, req.target) {
                                if let Some(bytes) = data_snapshot.data(&name) {
                                    conn.change_property8(
                                        PropMode::REPLACE,
                                        req.requestor,
                                        property,
                                        req.target,
                                        bytes,
                                    )
                                    .is_ok()
                                } else {
                                    false
                                }
                            } else {
                                false
                            };

                            let notify = SelectionNotifyEvent {
                                response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
                                sequence: 0,
                                time: req.time,
                                requestor: req.requestor,
                                selection: req.selection,
                                target: req.target,
                                property: if served {
                                    property
                                } else {
                                    AtomEnum::NONE.into()
                                },
                            };
                            let _ =
                                conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify);
                            let _ = conn.flush();
                        }
                        Event::ClientMessage(msg) if msg.type_ == wake_atom => break,
                        _ => {}
                    }
                }
                let _ = conn.destroy_window(win);
                let _ = conn.flush();
            })
            .map_err(|_| "Failed to spawn X11 clipboard thread")?;

        *ferrokit_core::poison::lock_unpoisoned(&self.owner.thread) = Some(handle);
        Ok(())
    }

    fn thread_slot(&self) -> &Mutex<Option<std::thread::JoinHandle<()>>> {
        &self.owner.thread
    }
}

impl Drop for X11Clipboard {
    fn drop(&mut self) {
        self.owner.stop_thread();
    }
}

impl PlatformClipboard for X11Clipboard {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str> {
        *ferrokit_core::poison::lock_unpoisoned(&self.owner.data) = data.clone();
        self.ensure_owner_thread()
    }

    fn mime_data(&self) -> Result<MimeData, &'static str> {
        if self.owner.owns_selection.load(Ordering::Acquire) {
            return Ok(ferrokit_core::poison::lock_unpoisoned(&self.owner.data).clone());
        }

        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|_| "Failed to connect to X11 for clipboard")?;
        let root = conn.setup().roots[screen_num].root;
        let win = conn
            .generate_id()
            .map_err(|_| "Failed to generate X11 clipboard reader window")?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(|_| "Failed to create X11 clipboard reader window")?;

        let clipboard_atom = intern_atom(&conn, b"CLIPBOARD")?;
        let prop_atom = intern_atom(&conn, b"FERROKIT_CLIPBOARD_DATA")?;
        let utf8_atom = intern_atom(&conn, b"UTF8_STRING")?;
        let html_atom = intern_atom(&conn, b"text/html")?;

        let mut out = MimeData::new();
        if let Some(bytes) = request_selection(&conn, win, clipboard_atom, utf8_atom, prop_atom) {
            out.set_data("text/plain", bytes);
        } else if let Some(bytes) = request_selection(
            &conn,
            win,
            clipboard_atom,
            AtomEnum::STRING.into(),
            prop_atom,
        ) {
            out.set_data("text/plain", bytes);
        }
        if let Some(bytes) = request_selection(&conn, win, clipboard_atom, html_atom, prop_atom) {
            out.set_data("text/html", bytes);
        }

        let _ = conn.destroy_window(win);
        let _ = conn.flush();
        Ok(out)
    }

    fn clear(&self) -> Result<(), &'static str> {
        ferrokit_core::poison::lock_unpoisoned(&self.owner.data).clear();
        self.owner.stop_thread();
        Ok(())
    }
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, &'static str> {
    conn.intern_atom(false, name)
        .map_err(|_| "intern_atom failed")?
        .reply()
        .map(|r| r.atom)
        .map_err(|_| "intern_atom reply failed")
}

fn atom_name(conn: &RustConnection, atom: Atom) -> Option<String> {
    let reply = conn.get_atom_name(atom).ok()?.reply().ok()?;
    String::from_utf8(reply.name).ok()
}

fn request_selection(
    conn: &RustConnection,
    win: u32,
    selection: Atom,
    target: Atom,
    property: Atom,
) -> Option<Vec<u8>> {
    conn.convert_selection(win, selection, target, property, x11rb::CURRENT_TIME)
        .ok()?;
    let _ = conn.flush();
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match conn.poll_for_event().ok()? {
            Some(Event::SelectionNotify(ev))
                if ev.selection == selection && ev.target == target =>
            {
                if ev.property == Atom::from(AtomEnum::NONE) {
                    return None;
                }
                let reply = conn
                    .get_property(true, win, ev.property, AtomEnum::ANY, 0, u32::MAX)
                    .ok()?
                    .reply()
                    .ok()?;
                return Some(reply.value);
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    None
}
