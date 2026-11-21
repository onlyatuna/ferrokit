// SPDX-License-Identifier: LGPL-3.0-only
//! Linux X11 global hotkeys via dynamically loaded `libX11.so.6` (`XGrabKey` / `XUngrabKey`).
//!
//! Mirrors QHotkey's `QHotkeyPrivateX11` design and ferrokit's [`GlobalHotkeyManager`](crate::GlobalHotkeyManager)
//! worker-thread architecture:
//! - Zero extra Cargo dependencies: on Linux, `libX11.so.6` is loaded on demand with
//!   `dlopen`/`dlsym`. Missing `libX11.so.6`, an unset/empty `DISPLAY`, or an unreachable X server
//!   (such as pure Wayland or headless CI) cleanly returns [`HotkeyError::Unsupported`].
//! - Every hotkey grab is expanded across the 4 CapsLock/NumLock modifier combinations (`0`,
//!   `LockMask`, `Mod2Mask`, `LockMask | Mod2Mask`) and those lock bits are masked off on incoming
//!   `KeyPress` events so hotkeys keep working regardless of CapsLock/NumLock state.
//! - All OS interaction goes through `X11HotkeyBackend` + `X11Waker`, so the full worker
//!   lifecycle, 4-variant lock-mask expansion and rollback, `BadAccess` conflict detection,
//!   `replace`, `unregister`, `Drop`, and press delivery run against `MockX11HotkeyBackend` on
//!   every operating system.

use crate::hotkey::{HotkeyError, HotkeyModifiers};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle, ThreadId};

// Standard X11 modifier masks (`<X11/X.h>`).
#[cfg(test)]
const X11_SHIFT_MASK: u32 = 1 << 0;
pub(crate) const X11_LOCK_MASK: u32 = 1 << 1;
#[cfg(test)]
const X11_CONTROL_MASK: u32 = 1 << 2;
#[cfg(test)]
const X11_MOD1_MASK: u32 = 1 << 3;
const X11_MOD2_MASK: u32 = 1 << 4;
#[cfg(test)]
const X11_MOD4_MASK: u32 = 1 << 6;

/// Lock-modifier bits ignored when matching hotkeys (`LockMask` = CapsLock, `Mod2Mask` = NumLock).
const X11_IGNORED_MODIFIER_MASK: u32 = X11_LOCK_MASK | X11_MOD2_MASK;

/// Mask of the 8 X11 keyboard modifier bits (`ShiftMask..=Mod5Mask`); higher bits in
/// `XKeyEvent::state` hold mouse-button state (`Button1Mask..=Button5Mask`) and the XKB group.
const X11_KEYBOARD_MODIFIER_BITS: u32 = 0x00FF;

/// The 4 lock-mask combinations grabbed for every hotkey (matching QHotkey).
const X11_LOCK_MASK_VARIANTS: [u32; 4] = [
    0,
    X11_LOCK_MASK,
    X11_MOD2_MASK,
    X11_LOCK_MASK | X11_MOD2_MASK,
];

/// X11 `BadValue` protocol error code (used when a virtual key has no X11 mapping).
pub(crate) const X11_BAD_VALUE: u32 = 2;

/// X11 `BadAccess` protocol error code (another client already holds the passive key grab).
pub(crate) const X11_BAD_ACCESS: u32 = HotkeyError::X11_BAD_ACCESS;

/// Maps [`HotkeyModifiers`] to the base X11 modifier mask (`ShiftMask`, `ControlMask`,
/// `Mod1Mask`, `Mod4Mask`). `NO_REPEAT` is enforced on received events rather than as a mask bit.
fn x11_modifiers(modifiers: HotkeyModifiers) -> u32 {
    crate::hotkey::UnixHotkeyManager::x11_modifiers(modifiers)
}

/// Strips CapsLock (`LockMask`), NumLock (`Mod2Mask`), pointer-button masks, and XKB group bits
/// from an X11 `XKeyEvent::state` value so it matches the base modifier mask from
/// [`x11_modifiers`].
fn normalize_x11_modifiers(state: u32) -> u32 {
    (state & X11_KEYBOARD_MODIFIER_BITS) & !X11_IGNORED_MODIFIER_MASK
}

/// Maps a Windows virtual-key code (`vk`) to an X11 KeySym (`<X11/keysymdef.h>`).
///
/// Covers `'A'..='Z'`, `'0'..='9'`, `VK_F1..=VK_F24`, navigation and editing keys, numeric keypad
/// keys, and `VK_OEM_*` ANSI punctuation keys. Returns `None` for unmapped virtual keys.
fn vk_to_x11_keysym(vk: u32) -> Option<u32> {
    Some(match vk {
        // Letters 'A'..='Z' -> XK_a..=XK_z (unshifted level-0 keysyms for XKeysymToKeycode).
        0x41..=0x5A => 0x0061 + (vk - 0x41),
        // Digits '0'..='9' -> XK_0..=XK_9.
        0x30..=0x39 => vk,
        // Function keys VK_F1..=VK_F24 -> XK_F1..=XK_F24 (0xFFBE..=0xFFD5).
        0x70..=0x87 => 0xFFBE + (vk - 0x70),
        // Editing and navigation keys.
        0x08 => 0xFF08, // VK_BACK -> XK_BackSpace
        0x09 => 0xFF09, // VK_TAB -> XK_Tab
        0x0D => 0xFF0D, // VK_RETURN -> XK_Return
        0x1B => 0xFF1B, // VK_ESCAPE -> XK_Escape
        0x20 => 0x0020, // VK_SPACE -> XK_space
        0x21 => 0xFF55, // VK_PRIOR -> XK_Page_Up
        0x22 => 0xFF56, // VK_NEXT -> XK_Page_Down
        0x23 => 0xFF57, // VK_END -> XK_End
        0x24 => 0xFF50, // VK_HOME -> XK_Home
        0x25 => 0xFF51, // VK_LEFT -> XK_Left
        0x26 => 0xFF52, // VK_UP -> XK_Up
        0x27 => 0xFF53, // VK_RIGHT -> XK_Right
        0x28 => 0xFF54, // VK_DOWN -> XK_Down
        0x2D => 0xFF63, // VK_INSERT -> XK_Insert
        0x2E => 0xFFFF, // VK_DELETE -> XK_Delete
        // Numeric keypad keys.
        0x60..=0x69 => 0xFFB0 + (vk - 0x60), // VK_NUMPAD0..=VK_NUMPAD9 -> XK_KP_0..=XK_KP_9
        0x6A => 0xFFAA,                      // VK_MULTIPLY -> XK_KP_Multiply
        0x6B => 0xFFAB,                      // VK_ADD -> XK_KP_Add
        0x6D => 0xFFAD,                      // VK_SUBTRACT -> XK_KP_Subtract
        0x6E => 0xFFAE,                      // VK_DECIMAL -> XK_KP_Decimal
        0x6F => 0xFFAF,                      // VK_DIVIDE -> XK_KP_Divide
        // ANSI OEM punctuation keys (same set as `cocoa_hotkey::carbon_keycode`).
        0xBA => 0x003B, // VK_OEM_1 ; -> XK_semicolon
        0xBB => 0x003D, // VK_OEM_PLUS = -> XK_equal
        0xBC => 0x002C, // VK_OEM_COMMA , -> XK_comma
        0xBD => 0x002D, // VK_OEM_MINUS - -> XK_minus
        0xBE => 0x002E, // VK_OEM_PERIOD . -> XK_period
        0xBF => 0x002F, // VK_OEM_2 / -> XK_slash
        0xC0 => 0x0060, // VK_OEM_3 ` -> XK_grave
        0xDB => 0x005B, // VK_OEM_4 [ -> XK_bracketleft
        0xDC => 0x005C, // VK_OEM_5 \ -> XK_backslash
        0xDD => 0x005D, // VK_OEM_6 ] -> XK_bracketright
        0xDE => 0x0027, // VK_OEM_7 ' -> XK_apostrophe
        _ => return None,
    })
}

/// An X11 `KeyPress` delivered by an [`X11HotkeyBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct X11KeyPress {
    keycode: u8,
    /// Raw X11 modifier state (`XKeyEvent::state`), before lock-mask stripping.
    state: u32,
    /// `true` when this press is an X11 auto-repeat generated while the key is held down.
    is_repeat: bool,
}

/// Outcome of blocking in [`X11HotkeyBackend::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum X11WaitResult {
    /// Control requests and/or X11 events may be pending.
    Ready,
    /// The X11 display connection closed or failed fatally; the worker must exit.
    Disconnected,
}

/// Cross-thread handle that wakes the X11 worker blocked in [`X11HotkeyBackend::wait`].
trait X11Waker: Send + Sync {
    fn wake(&self) -> Result<(), HotkeyError>;
}

/// Low-level X11 operations owned exclusively by the hotkey worker thread.
///
/// Lock-mask expansion across [`X11_LOCK_MASK_VARIANTS`] and hotkey-id bookkeeping live above this
/// trait so `MockX11HotkeyBackend` exercises the exact same logic on every OS.
trait X11HotkeyBackend {
    /// Resolves an X11 keysym (`XK_*`) to a hardware `KeyCode` (`1..=255`), or `None` if unmapped.
    fn keysym_to_keycode(&mut self, keysym: u32) -> Option<u8>;

    /// Issues `XGrabKey` for one `(keycode, modifiers)` pair and syncs X11 errors.
    /// Returns the X11 error code (e.g. [`X11_BAD_ACCESS`]) on failure.
    fn grab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32>;

    /// Issues `XUngrabKey` for one `(keycode, modifiers)` pair.
    fn ungrab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32>;

    /// Blocks until either the paired [`X11Waker`] fires or an X11 event arrives.
    fn wait(&mut self) -> X11WaitResult;

    /// Pops the next queued `KeyPress` without blocking, or `None` if no more are queued.
    fn next_press(&mut self) -> Option<X11KeyPress>;
}

type Handler = Arc<dyn Fn(u32) + Send + Sync>;

#[derive(Clone, Copy)]
struct Entry {
    modifiers: HotkeyModifiers,
    keycode: u8,
    x11_mods: u32,
}

enum Op {
    Register {
        id: u32,
        modifiers: HotkeyModifiers,
        vk: u32,
        replace: bool,
    },
    Unregister {
        id: u32,
    },
}

struct Request {
    op: Op,
    reply: mpsc::Sender<Result<(), HotkeyError>>,
}

#[derive(Default)]
struct Queue {
    /// Set by `Drop`: tells the worker loop to exit cleanly.
    shutdown: bool,
    /// Set by `ExitGuard` when the worker is leaving: no further requests or wakes are accepted.
    closed: bool,
    requests: VecDeque<Request>,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    hotkeys: Mutex<HashMap<u32, Entry>>,
    handler: Mutex<Option<Handler>>,
}

thread_local! {
    /// The worker thread's exclusively owned backend. Accessed only while executing on the
    /// manager's own worker thread (so handlers may call `register`/`replace`/`unregister`
    /// reentrantly in place without deadlocking or moving `Display*` across threads).
    static WORKER_BACKEND: RefCell<Option<Box<dyn X11HotkeyBackend>>> = const { RefCell::new(None) };
}

// Every critical section is a few map/queue operations, so the state stays valid after a panic.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn grab_all_locks(
    backend: &mut dyn X11HotkeyBackend,
    keycode: u8,
    base_modifiers: u32,
) -> Result<(), HotkeyError> {
    let mut grabbed = [false; 4];
    for (i, &lock_mask) in X11_LOCK_MASK_VARIANTS.iter().enumerate() {
        match backend.grab_key(keycode, base_modifiers | lock_mask) {
            Ok(()) => grabbed[i] = true,
            Err(code) => {
                // Roll back only the lock-mask variants that succeeded.
                for (j, &rolled_lock) in X11_LOCK_MASK_VARIANTS.iter().enumerate() {
                    if grabbed[j] {
                        let _ = backend.ungrab_key(keycode, base_modifiers | rolled_lock);
                    }
                }
                return Err(HotkeyError::Os { code });
            }
        }
    }
    Ok(())
}

fn ungrab_all_locks(
    backend: &mut dyn X11HotkeyBackend,
    keycode: u8,
    base_modifiers: u32,
) -> Result<(), HotkeyError> {
    let mut first_err: Option<u32> = None;
    for &lock_mask in &X11_LOCK_MASK_VARIANTS {
        if let Err(code) = backend.ungrab_key(keycode, base_modifiers | lock_mask)
            && first_err.is_none()
        {
            first_err = Some(code);
        }
    }
    match first_err {
        None => Ok(()),
        Some(code) => Err(HotkeyError::Os { code }),
    }
}

/// Runs one control operation on the worker thread using `WORKER_BACKEND`.
fn run_op_on_worker(shared: &Shared, op: Op) -> Result<(), HotkeyError> {
    WORKER_BACKEND.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(backend) = borrow.as_deref_mut() else {
            return Err(HotkeyError::Stopped);
        };
        let mut hotkeys = lock(&shared.hotkeys);
        match op {
            Op::Register {
                id,
                modifiers,
                vk,
                replace,
            } => {
                let old = hotkeys.get(&id).copied();
                if old.is_some() && !replace {
                    return Err(HotkeyError::IdInUse(id));
                }
                let keysym = vk_to_x11_keysym(vk).ok_or(HotkeyError::Os {
                    code: X11_BAD_VALUE,
                })?;
                let keycode = backend.keysym_to_keycode(keysym).ok_or(HotkeyError::Os {
                    code: X11_BAD_VALUE,
                })?;
                let x11_mods = x11_modifiers(modifiers);

                // X11 `XGrabKey` silently overrides an existing passive grab made by the *same*
                // client connection (`Display*`) instead of raising `BadAccess`. Reject duplicates
                // held by another id on this manager explicitly so behavior matches Windows and
                // cross-client X11 grabs.
                if hotkeys.iter().any(|(&other_id, e)| {
                    other_id != id && e.keycode == keycode && e.x11_mods == x11_mods
                }) {
                    return Err(HotkeyError::Os {
                        code: X11_BAD_ACCESS,
                    });
                }

                if let Some(old_entry) = old {
                    let _ = ungrab_all_locks(backend, old_entry.keycode, old_entry.x11_mods);
                    hotkeys.remove(&id);
                }

                match grab_all_locks(backend, keycode, x11_mods) {
                    Ok(()) => {
                        hotkeys.insert(
                            id,
                            Entry {
                                modifiers,
                                keycode,
                                x11_mods,
                            },
                        );
                        Ok(())
                    }
                    Err(e) => {
                        if let Some(old_entry) = old
                            && grab_all_locks(backend, old_entry.keycode, old_entry.x11_mods)
                                .is_ok()
                        {
                            hotkeys.insert(id, old_entry);
                        }
                        Err(e)
                    }
                }
            }
            Op::Unregister { id } => {
                let Some(entry) = hotkeys.remove(&id) else {
                    return Err(HotkeyError::NotRegistered(id));
                };
                ungrab_all_locks(backend, entry.keycode, entry.x11_mods)
            }
        }
    })
}

fn drain_requests(shared: &Shared) {
    loop {
        let request = lock(&shared.queue).requests.pop_front();
        let Some(request) = request else { return };
        let _ = request.reply.send(run_op_on_worker(shared, request.op));
    }
}

fn is_shutdown(shared: &Shared) -> bool {
    lock(&shared.queue).shutdown
}

fn next_press_on_worker() -> Option<X11KeyPress> {
    WORKER_BACKEND.with(|cell| cell.borrow_mut().as_deref_mut()?.next_press())
}

fn wait_on_worker() -> X11WaitResult {
    WORKER_BACKEND.with(|cell| {
        cell.borrow_mut()
            .as_deref_mut()
            .map_or(X11WaitResult::Disconnected, X11HotkeyBackend::wait)
    })
}

/// Dispatches a press to the registered handler with no locks or backend borrows held (so the
/// handler may reentrantly call `register`/`replace`/`unregister` or drop the manager).
fn dispatch_press(shared: &Shared, press: X11KeyPress) {
    let norm_mods = normalize_x11_modifiers(press.state);
    let matched_id = {
        let hotkeys = lock(&shared.hotkeys);
        hotkeys.iter().find_map(|(&id, entry)| {
            if entry.keycode == press.keycode && entry.x11_mods == norm_mods {
                if press.is_repeat && entry.modifiers.contains(HotkeyModifiers::NO_REPEAT) {
                    None
                } else {
                    Some(id)
                }
            } else {
                None
            }
        })
    };
    let Some(id) = matched_id else {
        return;
    };
    let handler = lock(&shared.handler).clone();
    if let Some(handler) = handler {
        handler(id);
    }
}

/// Runs whenever the worker exits (normal shutdown, disconnect, or handler panic): clears tables,
/// marks the queue closed so no further wake writes happen, ungrabs all remaining hotkeys, and
/// drops the backend (`XCloseDisplay` + wake pipe read end) on the worker thread.
struct ExitGuard<'a>(&'a Shared);

impl Drop for ExitGuard<'_> {
    fn drop(&mut self) {
        let entries: Vec<Entry> = lock(&self.0.hotkeys).drain().map(|(_, e)| e).collect();
        // Release every passive grab before publishing `queue.closed = true` so callers observing
        // `HotkeyError::Stopped` immediately see all combinations freed, while keeping the backend
        // (and its wake-pipe read end) alive until after `queue.closed` is set.
        let _ = WORKER_BACKEND.try_with(|cell| {
            if let Some(backend) = cell.borrow_mut().as_deref_mut() {
                for entry in entries {
                    let _ = ungrab_all_locks(backend, entry.keycode, entry.x11_mods);
                }
            }
        });
        {
            let mut queue = lock(&self.0.queue);
            queue.closed = true;
            queue.requests.clear();
        }
        // Now no caller can call `waker.wake()`, so closing the wake pipe and `Display*` is safe.
        let _ = WORKER_BACKEND.try_with(|cell| {
            drop(cell.borrow_mut().take());
        });
    }
}

fn worker_main<B, F>(
    shared: Arc<Shared>,
    open: F,
    ready: mpsc::Sender<Result<Arc<dyn X11Waker>, HotkeyError>>,
) where
    B: X11HotkeyBackend + 'static,
    F: FnOnce() -> Result<(B, Arc<dyn X11Waker>), HotkeyError>,
{
    let (backend, waker) = match open() {
        Ok(pair) => pair,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    WORKER_BACKEND.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(backend));
    });
    let _exit = ExitGuard(&shared);
    if ready.send(Ok(waker)).is_err() {
        return;
    }

    loop {
        drain_requests(&shared);
        if is_shutdown(&shared) {
            break;
        }
        while let Some(press) = next_press_on_worker() {
            dispatch_press(&shared, press);
            drain_requests(&shared);
            if is_shutdown(&shared) {
                return;
            }
        }
        if wait_on_worker() == X11WaitResult::Disconnected {
            break;
        }
    }
}

/// X11 global hotkey manager backed by a dedicated `"ferrokit-hotkey"` worker thread.
///
/// Matches [`GlobalHotkeyManager`](crate::GlobalHotkeyManager)'s exact contract: `Send + Sync`,
/// rollback-on-failure `replace`, reentrant calls from the handler, and full cleanup on `Drop`.
pub(crate) struct X11GlobalHotkeyManager {
    shared: Arc<Shared>,
    waker: Arc<dyn X11Waker>,
    worker_thread: ThreadId,
    worker: Option<JoinHandle<()>>,
}

// Compile-time guarantee required by `GlobalHotkeyManager`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<X11GlobalHotkeyManager>();
};

impl std::fmt::Debug for X11GlobalHotkeyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("X11GlobalHotkeyManager")
            .field("registered_ids", &self.registered_ids())
            .finish_non_exhaustive()
    }
}

impl X11GlobalHotkeyManager {
    /// Starts the X11 hotkey worker thread on Linux.
    ///
    /// Returns [`HotkeyError::Unsupported`] on non-Linux platforms, when `DISPLAY` is unset/empty,
    /// when `libX11.so.6` is unavailable, or when `XOpenDisplay` fails.
    pub(crate) fn new() -> Result<Self, HotkeyError> {
        #[cfg(target_os = "linux")]
        {
            x11::preflight_check()?;
            Self::with_backend(x11::X11Backend::open)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(HotkeyError::Unsupported)
        }
    }

    /// Starts a manager whose worker thread constructs its backend via `open`.
    fn with_backend<B, F>(open: F) -> Result<Self, HotkeyError>
    where
        B: X11HotkeyBackend + 'static,
        F: FnOnce() -> Result<(B, Arc<dyn X11Waker>), HotkeyError> + Send + 'static,
    {
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("ferrokit-hotkey".to_owned())
            .spawn(move || worker_main(worker_shared, open, ready_tx))
            .map_err(|e| HotkeyError::Thread(e.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(waker)) => Ok(Self {
                shared,
                waker,
                worker_thread: handle.thread().id(),
                worker: Some(handle),
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                Err(HotkeyError::Thread(
                    "the worker thread ended during start-up".to_owned(),
                ))
            }
        }
    }

    /// Starts a manager backed by a `MockX11HotkeyBackend` (runs on every OS in tests).
    #[cfg(test)]
    pub(crate) fn with_mock(mock: MockX11HotkeyBackend) -> Result<Self, HotkeyError> {
        Self::with_backend(move || {
            let waker = mock.waker();
            Ok((mock, waker))
        })
    }

    fn call(&self, op: Op) -> Result<(), HotkeyError> {
        if thread::current().id() == self.worker_thread {
            if lock(&self.shared.queue).closed {
                return Err(HotkeyError::Stopped);
            }
            return run_op_on_worker(&self.shared, op);
        }
        let (reply, answer) = mpsc::channel();
        {
            let mut queue = lock(&self.shared.queue);
            if queue.closed {
                return Err(HotkeyError::Stopped);
            }
            queue.requests.push_back(Request { op, reply });
            if let Err(e) = self.waker.wake() {
                queue.requests.pop_back();
                return Err(e);
            }
        }
        answer.recv().unwrap_or(Err(HotkeyError::Stopped))
    }

    pub(crate) fn register(
        &self,
        id: u32,
        modifiers: HotkeyModifiers,
        vk: u32,
        replace: bool,
    ) -> Result<(), HotkeyError> {
        self.call(Op::Register {
            id,
            modifiers,
            vk,
            replace,
        })
    }

    pub(crate) fn unregister(&self, id: u32) -> Result<(), HotkeyError> {
        self.call(Op::Unregister { id })
    }

    pub(crate) fn is_registered(&self, id: u32) -> bool {
        lock(&self.shared.hotkeys).contains_key(&id)
    }

    pub(crate) fn registered_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = lock(&self.shared.hotkeys).keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub(crate) fn set_handler(&self, handler: Option<Handler>) {
        *lock(&self.shared.handler) = handler;
    }

    /// Returns the registered `(modifiers, keycode, x11_mods)` tuple for `id`, if any.
    #[cfg(test)]
    fn entry_for(&self, id: u32) -> Option<(HotkeyModifiers, u8, u32)> {
        lock(&self.shared.hotkeys)
            .get(&id)
            .map(|e| (e.modifiers, e.keycode, e.x11_mods))
    }
}

impl Drop for X11GlobalHotkeyManager {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        {
            let mut queue = lock(&self.shared.queue);
            queue.shutdown = true;
            if !queue.closed {
                let _ = self.waker.wake();
            }
        }
        if worker.thread().id() != thread::current().id() {
            let _ = worker.join();
        }
    }
}

// ---- Cross-platform mock backend (for unit tests on every OS) -----------------------------------

#[cfg(test)]
#[derive(Debug, Default)]
struct MockInner {
    grabs: Vec<(u8, u32)>,
    occupied: Vec<(u8, u32)>,
    unmapped_keysyms: HashSet<u32>,
    pending_presses: VecDeque<X11KeyPress>,
    woken: bool,
    disconnected: bool,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct MockState {
    inner: Mutex<MockInner>,
    condvar: Condvar,
}

#[cfg(test)]
struct MockWaker {
    state: Arc<MockState>,
}

#[cfg(test)]
impl X11Waker for MockWaker {
    fn wake(&self) -> Result<(), HotkeyError> {
        let mut inner = lock(&self.state.inner);
        inner.woken = true;
        self.state.condvar.notify_all();
        Ok(())
    }
}

/// In-memory X11 hotkey backend modelled on `XGrabKey`/`XUngrabKey` and `BadAccess` conflicts.
///
/// Clones share the same underlying state, so a test retains one handle while the manager's worker
/// thread owns the other.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub(crate) struct MockX11HotkeyBackend {
    state: Arc<MockState>,
}

#[cfg(test)]
impl MockX11HotkeyBackend {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Deterministic non-zero hardware `KeyCode` (`8..=254`) assigned by this mock to `keysym`.
    fn mock_keycode_for_keysym(keysym: u32) -> Option<u8> {
        match keysym {
            0 => None,
            0x0020..=0x007E => Some(((keysym - 0x0020) as u8) + 8),
            0xFF08 => Some(110),
            0xFF09 => Some(111),
            0xFF0D => Some(112),
            0xFF1B => Some(113),
            0xFF50..=0xFF57 => Some(120 + (keysym - 0xFF50) as u8),
            0xFF63 => Some(128),
            0xFFFF => Some(129),
            0xFFAA..=0xFFB9 => Some(140 + (keysym - 0xFFAA) as u8),
            0xFFBE..=0xFFD5 => Some(160 + (keysym - 0xFFBE) as u8),
            other => Some(200 + (other % 55) as u8),
        }
    }

    /// Convenience helper: maps a Windows `vk` through [`vk_to_x11_keysym`] and
    /// [`mock_keycode_for_keysym`](Self::mock_keycode_for_keysym).
    pub(crate) fn mock_keycode_for_vk(vk: u32) -> Option<u8> {
        vk_to_x11_keysym(vk).and_then(Self::mock_keycode_for_keysym)
    }

    fn waker(&self) -> Arc<dyn X11Waker> {
        Arc::new(MockWaker {
            state: Arc::clone(&self.state),
        })
    }

    /// Currently active `(keycode, modifiers)` grabs (including all 4 lock-mask variants).
    pub(crate) fn grabs(&self) -> Vec<(u8, u32)> {
        lock(&self.state.inner).grabs.clone()
    }

    /// Marks one `(keycode, modifiers)` pair as held by another X11 client (`BadAccess`).
    fn occupy(&self, keycode: u8, modifiers: u32) {
        lock(&self.state.inner).occupied.push((keycode, modifiers));
    }

    /// Marks all 4 lock-mask variants of `(keycode, base_modifiers)` as held by another client.
    fn occupy_all_locks(&self, keycode: u8, base_modifiers: u32) {
        let mut inner = lock(&self.state.inner);
        for &lock_mask in &X11_LOCK_MASK_VARIANTS {
            inner.occupied.push((keycode, base_modifiers | lock_mask));
        }
    }

    /// Causes `keysym_to_keycode(keysym)` to return `None` (simulating a layout without that key).
    fn unmap_keysym(&self, keysym: u32) {
        lock(&self.state.inner).unmapped_keysyms.insert(keysym);
    }

    /// Simulates an X11 `KeyPress` event with raw `state` (may include `LockMask` / `Mod2Mask`).
    pub(crate) fn press(&self, keycode: u8, state: u32) {
        let mut inner = lock(&self.state.inner);
        inner.pending_presses.push_back(X11KeyPress {
            keycode,
            state,
            is_repeat: false,
        });
        self.state.condvar.notify_all();
    }

    /// Simulates an X11 auto-repeat `KeyPress` event.
    fn press_repeat(&self, keycode: u8, state: u32) {
        let mut inner = lock(&self.state.inner);
        inner.pending_presses.push_back(X11KeyPress {
            keycode,
            state,
            is_repeat: true,
        });
        self.state.condvar.notify_all();
    }

    /// Simulates the X server closing the connection.
    pub(crate) fn disconnect(&self) {
        let mut inner = lock(&self.state.inner);
        inner.disconnected = true;
        self.state.condvar.notify_all();
    }
}

#[cfg(test)]
impl X11HotkeyBackend for MockX11HotkeyBackend {
    fn keysym_to_keycode(&mut self, keysym: u32) -> Option<u8> {
        if lock(&self.state.inner).unmapped_keysyms.contains(&keysym) {
            return None;
        }
        Self::mock_keycode_for_keysym(keysym)
    }

    fn grab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32> {
        let mut inner = lock(&self.state.inner);
        let pair = (keycode, modifiers);
        if inner.occupied.contains(&pair) {
            return Err(X11_BAD_ACCESS);
        }
        if !inner.grabs.contains(&pair) {
            inner.grabs.push(pair);
        }
        Ok(())
    }

    fn ungrab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32> {
        let mut inner = lock(&self.state.inner);
        let pair = (keycode, modifiers);
        if let Some(pos) = inner.grabs.iter().position(|&g| g == pair) {
            inner.grabs.remove(pos);
        }
        Ok(())
    }

    fn wait(&mut self) -> X11WaitResult {
        let mut inner = lock(&self.state.inner);
        while !inner.woken && inner.pending_presses.is_empty() && !inner.disconnected {
            inner = self
                .state
                .condvar
                .wait(inner)
                .unwrap_or_else(|e| e.into_inner());
        }
        inner.woken = false;
        if inner.disconnected {
            X11WaitResult::Disconnected
        } else {
            X11WaitResult::Ready
        }
    }

    fn next_press(&mut self) -> Option<X11KeyPress> {
        lock(&self.state.inner).pending_presses.pop_front()
    }
}

// ---- Real Linux X11 backend (`dlopen("libX11.so.6")` + `UnixStream::pair()` + `poll`) ----------

#[cfg(target_os = "linux")]
mod x11 {
    use super::*;
    use std::ffi::{CStr, c_char, c_int, c_long, c_short, c_uint, c_ulong, c_void};
    use std::io::{ErrorKind, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::ptr;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    const RTLD_LAZY: c_int = 1;
    const RTLD_LOCAL: c_int = 0;

    const POLLIN: c_short = 0x0001;
    const POLLERR: c_short = 0x0008;
    const POLLHUP: c_short = 0x0010;
    const POLLNVAL: c_short = 0x0020;

    const KEY_PRESS: c_int = 2;
    const KEY_RELEASE: c_int = 3;
    const GRAB_MODE_ASYNC: c_int = 1;
    const QUEUED_AFTER_READING: c_int = 1;

    #[repr(C)]
    struct PollFd {
        fd: c_int,
        events: c_short,
        revents: c_short,
    }

    #[repr(C)]
    struct Display {
        _private: [u8; 0],
    }

    // C ABI layout from `<X11/Xlib.h>`; not every field is read by Rust.
    #[repr(C)]
    #[allow(dead_code)]
    struct XErrorEvent {
        event_type: c_int,
        display: *mut Display,
        resourceid: c_ulong,
        serial: c_ulong,
        error_code: u8,
        request_code: u8,
        minor_code: u8,
    }

    // C ABI layout from `<X11/Xlib.h>`; not every field is read by Rust.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct XKeyEvent {
        event_type: c_int,
        serial: c_ulong,
        send_event: c_int,
        display: *mut Display,
        window: c_ulong,
        root: c_ulong,
        subwindow: c_ulong,
        time: c_ulong,
        x: c_int,
        y: c_int,
        x_root: c_int,
        y_root: c_int,
        state: c_uint,
        keycode: c_uint,
        same_screen: c_int,
    }

    #[repr(C)]
    union XEvent {
        event_type: c_int,
        xkey: XKeyEvent,
        _pad: [c_long; 24],
    }

    type XErrorHandlerFn = unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int;
    type XErrorHandler = Option<XErrorHandlerFn>;

    type XOpenDisplayFn = unsafe extern "C" fn(*const c_char) -> *mut Display;
    type XCloseDisplayFn = unsafe extern "C" fn(*mut Display) -> c_int;
    type XDefaultRootWindowFn = unsafe extern "C" fn(*mut Display) -> c_ulong;
    type XConnectionNumberFn = unsafe extern "C" fn(*mut Display) -> c_int;
    type XKeysymToKeycodeFn = unsafe extern "C" fn(*mut Display, c_ulong) -> u8;
    type XGrabKeyFn =
        unsafe extern "C" fn(*mut Display, c_int, c_uint, c_ulong, c_int, c_int, c_int) -> c_int;
    type XUngrabKeyFn = unsafe extern "C" fn(*mut Display, c_int, c_uint, c_ulong) -> c_int;
    type XSyncFn = unsafe extern "C" fn(*mut Display, c_int) -> c_int;
    type XPendingFn = unsafe extern "C" fn(*mut Display) -> c_int;
    type XEventsQueuedFn = unsafe extern "C" fn(*mut Display, c_int) -> c_int;
    type XNextEventFn = unsafe extern "C" fn(*mut Display, *mut XEvent) -> c_int;
    type XPeekEventFn = unsafe extern "C" fn(*mut Display, *mut XEvent) -> c_int;
    type XSetErrorHandlerFn = unsafe extern "C" fn(XErrorHandler) -> XErrorHandler;

    #[cfg_attr(target_env = "gnu", link(name = "dl"))]
    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }

    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
    }

    struct DlHandle(*mut c_void);

    impl Drop for DlHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the live `dlopen` handle created in `X11Lib::load`, closed once
            // if symbol resolution fails before transferring ownership to `X11_LIB`.
            unsafe {
                dlclose(self.0);
            }
        }
    }

    struct X11Lib {
        x_open_display: XOpenDisplayFn,
        x_close_display: XCloseDisplayFn,
        x_default_root_window: XDefaultRootWindowFn,
        x_connection_number: XConnectionNumberFn,
        x_keysym_to_keycode: XKeysymToKeycodeFn,
        x_grab_key: XGrabKeyFn,
        x_ungrab_key: XUngrabKeyFn,
        x_sync: XSyncFn,
        x_pending: XPendingFn,
        x_events_queued: XEventsQueuedFn,
        x_next_event: XNextEventFn,
        x_peek_event: XPeekEventFn,
        x_set_error_handler: XSetErrorHandlerFn,
    }

    static X11_LIB: OnceLock<Option<X11Lib>> = OnceLock::new();

    impl X11Lib {
        fn get() -> Option<&'static Self> {
            X11_LIB.get_or_init(Self::load).as_ref()
        }

        fn load() -> Option<Self> {
            // SAFETY: `c"libX11.so.6"` is a valid NUL-terminated library soname and
            // `RTLD_LAZY | RTLD_LOCAL` are valid Linux `dlopen` flags.
            let raw = unsafe { dlopen(c"libX11.so.6".as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
            if raw.is_null() {
                return None;
            }
            let handle = DlHandle(raw);

            let sym = |name: &CStr| -> Option<*mut c_void> {
                // SAFETY: `handle.0` is a live `dlopen` handle and `name` is a NUL-terminated
                // symbol string.
                let ptr = unsafe { dlsym(handle.0, name.as_ptr()) };
                (!ptr.is_null()).then_some(ptr)
            };

            let p_open = sym(c"XOpenDisplay")?;
            let p_close = sym(c"XCloseDisplay")?;
            let p_root = sym(c"XDefaultRootWindow")?;
            let p_conn = sym(c"XConnectionNumber")?;
            let p_keysym = sym(c"XKeysymToKeycode")?;
            let p_grab = sym(c"XGrabKey")?;
            let p_ungrab = sym(c"XUngrabKey")?;
            let p_sync = sym(c"XSync")?;
            let p_pending = sym(c"XPending")?;
            let p_queued = sym(c"XEventsQueued")?;
            let p_next = sym(c"XNextEvent")?;
            let p_peek = sym(c"XPeekEvent")?;
            let p_err_handler = sym(c"XSetErrorHandler")?;

            // Keep `libX11.so.6` loaded for the process lifetime so function pointers remain
            // `'static` and we never `dlclose` Xlib while other threads may touch it.
            std::mem::forget(handle);

            // SAFETY: Every pointer above is non-null and resolved from `libX11.so.6` by its exact
            // Xlib symbol name; each target function-pointer type matches the `<X11/Xlib.h>` C ABI.
            // POSIX.1-2008 (`dlsym`) guarantees object-to-function pointer conversion.
            unsafe {
                Some(Self {
                    x_open_display: std::mem::transmute::<*mut c_void, XOpenDisplayFn>(p_open),
                    x_close_display: std::mem::transmute::<*mut c_void, XCloseDisplayFn>(p_close),
                    x_default_root_window: std::mem::transmute::<*mut c_void, XDefaultRootWindowFn>(
                        p_root,
                    ),
                    x_connection_number: std::mem::transmute::<*mut c_void, XConnectionNumberFn>(
                        p_conn,
                    ),
                    x_keysym_to_keycode: std::mem::transmute::<*mut c_void, XKeysymToKeycodeFn>(
                        p_keysym,
                    ),
                    x_grab_key: std::mem::transmute::<*mut c_void, XGrabKeyFn>(p_grab),
                    x_ungrab_key: std::mem::transmute::<*mut c_void, XUngrabKeyFn>(p_ungrab),
                    x_sync: std::mem::transmute::<*mut c_void, XSyncFn>(p_sync),
                    x_pending: std::mem::transmute::<*mut c_void, XPendingFn>(p_pending),
                    x_events_queued: std::mem::transmute::<*mut c_void, XEventsQueuedFn>(p_queued),
                    x_next_event: std::mem::transmute::<*mut c_void, XNextEventFn>(p_next),
                    x_peek_event: std::mem::transmute::<*mut c_void, XPeekEventFn>(p_peek),
                    x_set_error_handler: std::mem::transmute::<*mut c_void, XSetErrorHandlerFn>(
                        p_err_handler,
                    ),
                })
            }
        }
    }

    // ---- Scoped X11 error handler ---------------------------------------------------------------

    /// Serializes installation of our scoped Xlib error handler across threads.
    static X11_ERROR_LOCK: Mutex<()> = Mutex::new(());
    static ERROR_TARGET_DISPLAY: AtomicUsize = AtomicUsize::new(0);
    static CAPTURED_ERROR_CODE: AtomicU8 = AtomicU8::new(0);
    static PREV_ERROR_HANDLER: AtomicUsize = AtomicUsize::new(0);

    #[inline]
    fn self_handler_addr() -> usize {
        let f: XErrorHandlerFn = x11_error_handler;
        f as usize
    }

    unsafe extern "C" fn x11_error_handler(
        display: *mut Display,
        event: *mut XErrorEvent,
    ) -> c_int {
        // Never allow a Rust panic to unwind across libX11's C frames.
        std::panic::catch_unwind(|| {
            let target = ERROR_TARGET_DISPLAY.load(Ordering::Acquire);
            if !display.is_null() && display as usize == target {
                if !event.is_null() {
                    // SAFETY: `event` is non-null and points to the live `XErrorEvent` Xlib passed
                    // for the duration of this callback.
                    let code = unsafe { (*event).error_code };
                    if code != 0 {
                        let _ = CAPTURED_ERROR_CODE.compare_exchange(
                            0,
                            code,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                    }
                }
                0
            } else {
                let prev_addr = PREV_ERROR_HANDLER.load(Ordering::Acquire);
                if prev_addr != 0 && prev_addr != self_handler_addr() {
                    // SAFETY: `prev_addr` is a non-null handler pointer returned by
                    // `XSetErrorHandler`, stored in `ErrorHandlerGuard::install`, and verified not
                    // to be `x11_error_handler` itself (preventing infinite self-recursion).
                    let prev = unsafe { std::mem::transmute::<usize, XErrorHandlerFn>(prev_addr) };
                    // SAFETY: Forwarding Xlib's original `display` and `event` pointers to the
                    // previously installed X11 error handler.
                    unsafe { prev(display, event) }
                } else {
                    0
                }
            }
        })
        .unwrap_or(0)
    }

    struct ErrorHandlerGuard {
        _lock: MutexGuard<'static, ()>,
        lib: &'static X11Lib,
        prev: XErrorHandler,
    }

    impl ErrorHandlerGuard {
        fn install(lib: &'static X11Lib, display: *mut Display) -> Self {
            let guard = lock(&X11_ERROR_LOCK);
            CAPTURED_ERROR_CODE.store(0, Ordering::Release);
            ERROR_TARGET_DISPLAY.store(display as usize, Ordering::Release);
            // SAFETY: `x11_error_handler` matches Xlib's `XErrorHandler` ABI and catches panics;
            // `X11_ERROR_LOCK` serializes installation and restoration across threads.
            let prev = unsafe { (lib.x_set_error_handler)(Some(x11_error_handler)) };
            // Guard against external code having saved/restored `x11_error_handler` itself.
            let self_addr = self_handler_addr();
            let prev_addr = match prev {
                Some(f) if (f as usize) != self_addr => f as usize,
                _ => 0,
            };
            PREV_ERROR_HANDLER.store(prev_addr, Ordering::Release);
            Self {
                _lock: guard,
                lib,
                prev,
            }
        }

        fn take_error(&self) -> u8 {
            CAPTURED_ERROR_CODE.swap(0, Ordering::AcqRel)
        }
    }

    impl Drop for ErrorHandlerGuard {
        fn drop(&mut self) {
            let self_addr = self_handler_addr();
            let restore = match self.prev {
                Some(f) if (f as usize) == self_addr => None,
                other => other,
            };
            // SAFETY: `restore` is either the non-self handler returned by `XSetErrorHandler` in
            // `install` or `None` (Xlib default), and `_lock` is held until after `Drop::drop`.
            unsafe {
                (self.lib.x_set_error_handler)(restore);
            }
            ERROR_TARGET_DISPLAY.store(0, Ordering::Release);
            PREV_ERROR_HANDLER.store(0, Ordering::Release);
            CAPTURED_ERROR_CODE.store(0, Ordering::Release);
        }
    }

    // ---- Wake socket helpers --------------------------------------------------------------------

    fn open_wake_pair() -> Result<(UnixStream, UnixStream), HotkeyError> {
        let (reader, writer) =
            UnixStream::pair().map_err(|e| HotkeyError::Thread(e.to_string()))?;
        reader
            .set_nonblocking(true)
            .map_err(|e| HotkeyError::Thread(e.to_string()))?;
        writer
            .set_nonblocking(true)
            .map_err(|e| HotkeyError::Thread(e.to_string()))?;
        Ok((reader, writer))
    }

    struct SocketWaker {
        writer: UnixStream,
    }

    impl X11Waker for SocketWaker {
        fn wake(&self) -> Result<(), HotkeyError> {
            let mut stream = &self.writer;
            loop {
                match stream.write(&[1]) {
                    Ok(_) => return Ok(()),
                    Err(err) => match err.kind() {
                        ErrorKind::Interrupted => continue,
                        // Socket buffer already holds unread wake bytes; poll will wake on POLLIN.
                        ErrorKind::WouldBlock => return Ok(()),
                        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset => {
                            return Err(HotkeyError::Stopped);
                        }
                        _ => {
                            return Err(HotkeyError::Os {
                                code: err.raw_os_error().unwrap_or(0) as u32,
                            });
                        }
                    },
                }
            }
        }
    }

    // ---- Real X11 backend -----------------------------------------------------------------------

    /// Cheap check before spawning a worker thread: fails with [`HotkeyError::Unsupported`] when
    /// `DISPLAY` is unset/empty or `libX11.so.6` is not loadable.
    pub(super) fn preflight_check() -> Result<(), HotkeyError> {
        if std::env::var_os("DISPLAY").is_none_or(|v| v.is_empty()) {
            return Err(HotkeyError::Unsupported);
        }
        if X11Lib::get().is_none() {
            return Err(HotkeyError::Unsupported);
        }
        Ok(())
    }

    pub(super) struct X11Backend {
        lib: &'static X11Lib,
        display: *mut Display,
        root: c_ulong,
        x11_fd: c_int,
        wake_reader: UnixStream,
        /// Set once `x11_fd` hangs up or `poll` fails fatally so teardown never calls back into
        /// Xlib (`XUngrabKey`/`XSync`/`XCloseDisplay`), which would trigger `_XIOError` (`exit(1)`).
        disconnected: bool,
    }

    impl X11Backend {
        /// Opens a dedicated `Display*` and wake socket pair on the worker thread.
        pub(super) fn open() -> Result<(Self, Arc<dyn X11Waker>), HotkeyError> {
            let Some(lib) = X11Lib::get() else {
                return Err(HotkeyError::Unsupported);
            };
            let (wake_reader, wake_writer) = open_wake_pair()?;

            // SAFETY: `lib.x_open_display` is resolved from `libX11.so.6`; NULL tells Xlib to use
            // the `DISPLAY` environment variable.
            let display = unsafe { (lib.x_open_display)(ptr::null()) };
            if display.is_null() {
                return Err(HotkeyError::Unsupported);
            }
            // SAFETY: `display` is a non-null `Display*` just returned by `XOpenDisplay`.
            let root = unsafe { (lib.x_default_root_window)(display) };
            // SAFETY: `display` is a live `Display*` owned exclusively by this worker thread.
            let x11_fd = unsafe { (lib.x_connection_number)(display) };

            let waker: Arc<dyn X11Waker> = Arc::new(SocketWaker {
                writer: wake_writer,
            });
            Ok((
                Self {
                    lib,
                    display,
                    root,
                    x11_fd,
                    wake_reader,
                    disconnected: false,
                },
                waker,
            ))
        }

        fn drain_wake_socket(&self) {
            let mut stream = &self.wake_reader;
            let mut buf = [0u8; 64];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => continue,
                    Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
    }

    impl X11HotkeyBackend for X11Backend {
        fn keysym_to_keycode(&mut self, keysym: u32) -> Option<u8> {
            if self.disconnected {
                return None;
            }
            // SAFETY: `self.display` is our live `Display*` owned exclusively by the worker thread.
            let code =
                unsafe { (self.lib.x_keysym_to_keycode)(self.display, c_ulong::from(keysym)) };
            (code != 0).then_some(code)
        }

        fn grab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32> {
            if self.disconnected {
                return Err(0);
            }
            let guard = ErrorHandlerGuard::install(self.lib, self.display);
            // SAFETY: `self.display` is our live `Display*`, `self.root` is its default root
            // window, `keycode` is a valid X11 keycode, `owner_events = 0` (`False`), and
            // `GRAB_MODE_ASYNC` (1) is valid. `XSync(display, 0)` flushes the request and delivers
            // any protocol error synchronously while `guard` is installed.
            unsafe {
                (self.lib.x_grab_key)(
                    self.display,
                    c_int::from(keycode),
                    modifiers,
                    self.root,
                    0,
                    GRAB_MODE_ASYNC,
                    GRAB_MODE_ASYNC,
                );
                (self.lib.x_sync)(self.display, 0);
            }
            let err = guard.take_error();
            if err == 0 {
                Ok(())
            } else {
                Err(u32::from(err))
            }
        }

        fn ungrab_key(&mut self, keycode: u8, modifiers: u32) -> Result<(), u32> {
            if self.disconnected {
                // The X server already released all grabs for this client when the socket closed;
                // calling `XUngrabKey` + `XSync` on a dead `Display*` would trigger `_XIOError`.
                return Ok(());
            }
            let guard = ErrorHandlerGuard::install(self.lib, self.display);
            // SAFETY: `self.display` is our live `Display*`, `self.root` is its default root
            // window, and `XSync(display, 0)` flushes the ungrab and captures any error under
            // `guard`.
            unsafe {
                (self.lib.x_ungrab_key)(self.display, c_int::from(keycode), modifiers, self.root);
                (self.lib.x_sync)(self.display, 0);
            }
            let err = guard.take_error();
            if err == 0 {
                Ok(())
            } else {
                Err(u32::from(err))
            }
        }

        fn wait(&mut self) -> X11WaitResult {
            if self.disconnected {
                return X11WaitResult::Disconnected;
            }
            // Events read during `XSync` sit in Xlib's in-memory queue and will not re-trigger
            // `poll` on `x11_fd`, so check `XPending` first.
            // SAFETY: `self.display` is our live `Display*` owned exclusively by the worker thread.
            if unsafe { (self.lib.x_pending)(self.display) } > 0 {
                return X11WaitResult::Ready;
            }
            let mut fds = [
                PollFd {
                    fd: self.x11_fd,
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: self.wake_reader.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                },
            ];
            loop {
                // SAFETY: `fds` is a valid 2-element `PollFd` array and both fds are open.
                let ret = unsafe { poll(fds.as_mut_ptr(), 2, -1) };
                if ret < 0 {
                    if std::io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                        continue;
                    }
                    self.disconnected = true;
                    return X11WaitResult::Disconnected;
                }
                break;
            }
            if (fds[1].revents & POLLIN) != 0 {
                self.drain_wake_socket();
            }
            if (fds[0].revents & (POLLERR | POLLHUP | POLLNVAL)) != 0 {
                self.disconnected = true;
                return X11WaitResult::Disconnected;
            }
            if (fds[1].revents & (POLLERR | POLLHUP | POLLNVAL)) != 0
                && (fds[1].revents & POLLIN) == 0
            {
                return X11WaitResult::Disconnected;
            }
            X11WaitResult::Ready
        }

        fn next_press(&mut self) -> Option<X11KeyPress> {
            if self.disconnected {
                return None;
            }
            loop {
                // SAFETY: `self.display` is our live `Display*` owned exclusively by the worker.
                if unsafe { (self.lib.x_pending)(self.display) } <= 0 {
                    return None;
                }
                // SAFETY: `XEvent` is a C union of primitive integers and pointers; all-zero is
                // valid.
                let mut ev: XEvent = unsafe { std::mem::zeroed() };
                // SAFETY: `self.display` is live and `&mut ev` is a writable `XEvent`.
                unsafe {
                    (self.lib.x_next_event)(self.display, &mut ev);
                }
                // SAFETY: `XNextEvent` initialized `ev`, and every X11 event starts with `type`.
                let ev_type = unsafe { ev.event_type };
                match ev_type {
                    KEY_RELEASE => {
                        // SAFETY: `ev_type == KEY_RELEASE`, so `ev.xkey` is the active union field.
                        let release = unsafe { ev.xkey };
                        // Detect X11 auto-repeat: a `KeyRelease` immediately followed by a
                        // `KeyPress` with the identical `keycode` and `time`.
                        // SAFETY: `self.display` is live and `QUEUED_AFTER_READING` (1) is valid.
                        if unsafe { (self.lib.x_events_queued)(self.display, QUEUED_AFTER_READING) }
                            > 0
                        {
                            // SAFETY: `XEvent` is plain C data; all-zero is valid.
                            let mut next_ev: XEvent = unsafe { std::mem::zeroed() };
                            // SAFETY: `self.display` is live, at least one event is queued, and
                            // `&mut next_ev` is a writable `XEvent`.
                            unsafe {
                                (self.lib.x_peek_event)(self.display, &mut next_ev);
                            }
                            // SAFETY: `XPeekEvent` initialized `next_ev`'s `event_type`.
                            if unsafe { next_ev.event_type } == KEY_PRESS {
                                // SAFETY: `next_ev.event_type == KEY_PRESS`, so `xkey` is active.
                                let next_key = unsafe { next_ev.xkey };
                                if next_key.keycode == release.keycode
                                    && next_key.time == release.time
                                {
                                    // SAFETY: `self.display` is live and the peeked event is queued.
                                    unsafe {
                                        (self.lib.x_next_event)(self.display, &mut next_ev);
                                    }
                                    if let Ok(keycode) = u8::try_from(next_key.keycode) {
                                        return Some(X11KeyPress {
                                            keycode,
                                            state: next_key.state,
                                            is_repeat: true,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    KEY_PRESS => {
                        // SAFETY: `ev_type == KEY_PRESS`, so `ev.xkey` is the active union field.
                        let key = unsafe { ev.xkey };
                        if let Ok(keycode) = u8::try_from(key.keycode) {
                            return Some(X11KeyPress {
                                keycode,
                                state: key.state,
                                is_repeat: false,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    impl Drop for X11Backend {
        fn drop(&mut self) {
            if self.disconnected {
                // Avoid calling `XCloseDisplay` on a hung-up X11 socket because libX11 flushes
                // via `XSync` internally and would invoke `_XIOError` (`exit(1)`).
                return;
            }
            // SAFETY: `self.display` is the non-null `Display*` opened in `X11Backend::open`,
            // owned exclusively by the worker thread, and closed once here after `ExitGuard` has
            // released all passive grabs.
            unsafe {
                (self.lib.x_close_display)(self.display);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    const PRESS_TIMEOUT: Duration = Duration::from_secs(5);

    fn mock_manager() -> (X11GlobalHotkeyManager, MockX11HotkeyBackend) {
        let mock = MockX11HotkeyBackend::new();
        let mgr = X11GlobalHotkeyManager::with_mock(mock.clone()).expect("mock manager");
        (mgr, mock)
    }

    #[test]
    fn vk_to_x11_keysym_covers_letters_digits_fkeys_nav_numpad_and_oem() {
        let cases: &[(u32, u32)] = &[
            (0x41, 0x0061), // A -> XK_a
            (0x43, 0x0063), // C -> XK_c
            (0x5A, 0x007A), // Z -> XK_z
            (0x30, 0x0030), // 0 -> XK_0
            (0x39, 0x0039), // 9 -> XK_9
            (0x70, 0xFFBE), // F1 -> XK_F1
            (0x7B, 0xFFC9), // F12 -> XK_F12
            (0x7C, 0xFFCA), // F13 -> XK_F13
            (0x83, 0xFFD1), // F20 -> XK_F20
            (0x87, 0xFFD5), // F24 -> XK_F24
            (0x08, 0xFF08), // VK_BACK -> XK_BackSpace
            (0x09, 0xFF09), // VK_TAB -> XK_Tab
            (0x0D, 0xFF0D), // VK_RETURN -> XK_Return
            (0x1B, 0xFF1B), // VK_ESCAPE -> XK_Escape
            (0x20, 0x0020), // VK_SPACE -> XK_space
            (0x21, 0xFF55), // VK_PRIOR -> XK_Page_Up
            (0x22, 0xFF56), // VK_NEXT -> XK_Page_Down
            (0x23, 0xFF57), // VK_END -> XK_End
            (0x24, 0xFF50), // VK_HOME -> XK_Home
            (0x25, 0xFF51), // VK_LEFT -> XK_Left
            (0x26, 0xFF52), // VK_UP -> XK_Up
            (0x27, 0xFF53), // VK_RIGHT -> XK_Right
            (0x28, 0xFF54), // VK_DOWN -> XK_Down
            (0x2D, 0xFF63), // VK_INSERT -> XK_Insert
            (0x2E, 0xFFFF), // VK_DELETE -> XK_Delete
            (0x60, 0xFFB0), // VK_NUMPAD0 -> XK_KP_0
            (0x69, 0xFFB9), // VK_NUMPAD9 -> XK_KP_9
            (0x6A, 0xFFAA), // VK_MULTIPLY -> XK_KP_Multiply
            (0x6B, 0xFFAB), // VK_ADD -> XK_KP_Add
            (0x6D, 0xFFAD), // VK_SUBTRACT -> XK_KP_Subtract
            (0x6E, 0xFFAE), // VK_DECIMAL -> XK_KP_Decimal
            (0x6F, 0xFFAF), // VK_DIVIDE -> XK_KP_Divide
            (0xBA, 0x003B), // ;
            (0xBB, 0x003D), // =
            (0xBC, 0x002C), // ,
            (0xBD, 0x002D), // -
            (0xBE, 0x002E), // .
            (0xBF, 0x002F), // /
            (0xC0, 0x0060), // `
            (0xDB, 0x005B), // [
            (0xDC, 0x005C), // \
            (0xDD, 0x005D), // ]
            (0xDE, 0x0027), // '
        ];
        for &(vk, expected) in cases {
            assert_eq!(vk_to_x11_keysym(vk), Some(expected), "vk {vk:#x}");
        }
        for vk in [0x00, 0x07, 0x0E, 0x88, 0xFF, 0x1_0000] {
            assert_eq!(vk_to_x11_keysym(vk), None, "vk {vk:#x}");
        }

        // Every mapped VK in `0..=0xFF` produces a unique keysym and unique mock keycode.
        let mut seen_keysyms = HashSet::new();
        let mut seen_keycodes = HashSet::new();
        for vk in 0..=0xFFu32 {
            if let Some(ks) = vk_to_x11_keysym(vk) {
                assert!(
                    seen_keysyms.insert(ks),
                    "duplicate X11 keysym {ks:#x} for vk {vk:#x}"
                );
                let kc = MockX11HotkeyBackend::mock_keycode_for_keysym(ks).unwrap();
                assert_ne!(kc, 0);
                assert!(
                    seen_keycodes.insert(kc),
                    "duplicate mock keycode {kc} for vk {vk:#x}"
                );
            }
        }
    }

    #[test]
    fn modifier_mapping_and_lock_normalization() {
        assert_eq!(x11_modifiers(HotkeyModifiers::empty()), 0);
        assert_eq!(x11_modifiers(HotkeyModifiers::SHIFT), X11_SHIFT_MASK);
        assert_eq!(x11_modifiers(HotkeyModifiers::CONTROL), X11_CONTROL_MASK);
        assert_eq!(x11_modifiers(HotkeyModifiers::ALT), X11_MOD1_MASK);
        assert_eq!(x11_modifiers(HotkeyModifiers::WIN), X11_MOD4_MASK);
        assert_eq!(
            x11_modifiers(HotkeyModifiers::ALT | HotkeyModifiers::NO_REPEAT),
            X11_MOD1_MASK
        );

        let base = X11_MOD1_MASK | X11_SHIFT_MASK;
        for &lock_mask in &X11_LOCK_MASK_VARIANTS {
            assert_eq!(normalize_x11_modifiers(base | lock_mask), base);
        }
        // Mouse button mask (Button1Mask = 1<<8) and XKB group bits (1<<13) are masked out.
        assert_eq!(
            normalize_x11_modifiers(base | X11_LOCK_MASK | (1 << 8) | (1 << 13)),
            base
        );
        // Unrelated keyboard modifier (Mod5Mask = 1<<7) is preserved so it does not falsely match.
        assert_eq!(normalize_x11_modifiers(base | (1 << 7)), base | (1 << 7));
    }

    #[test]
    fn register_expands_four_lock_mask_variants_and_unregister_releases_all() {
        let (mgr, mock) = mock_manager();
        let kc = MockX11HotkeyBackend::mock_keycode_for_vk(b'C' as u32).unwrap();
        let mods = HotkeyModifiers::ALT | HotkeyModifiers::SHIFT;
        let base = x11_modifiers(mods);

        mgr.register(9527, mods, b'C' as u32, false).unwrap();
        assert!(mgr.is_registered(9527));
        assert_eq!(mgr.registered_ids(), [9527]);
        assert_eq!(
            mock.grabs(),
            vec![
                (kc, base),
                (kc, base | X11_LOCK_MASK),
                (kc, base | X11_MOD2_MASK),
                (kc, base | X11_LOCK_MASK | X11_MOD2_MASK),
            ]
        );

        mgr.unregister(9527).unwrap();
        assert!(!mgr.is_registered(9527));
        assert!(mgr.registered_ids().is_empty());
        assert!(mock.grabs().is_empty());
    }

    #[test]
    fn bad_access_on_partial_lock_variant_rolls_back_earlier_grabs() {
        let (mgr, mock) = mock_manager();
        let kc = MockX11HotkeyBackend::mock_keycode_for_vk(b'A' as u32).unwrap();
        let base = x11_modifiers(HotkeyModifiers::ALT);
        // Another X11 client owns ONLY the NumLock variant: variants 0 and 1 succeed, variant 2
        // fails with BadAccess, and variants 0 and 1 must be ungrabbed cleanly.
        mock.occupy(kc, base | X11_MOD2_MASK);

        let err = mgr
            .register(1, HotkeyModifiers::ALT, b'A' as u32, false)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: X11_BAD_ACCESS
            }
        );
        #[cfg(target_os = "linux")]
        assert!(err.is_already_registered());
        assert!(!mgr.is_registered(1));
        assert!(mock.grabs().is_empty(), "partial grabs must be rolled back");
    }

    #[test]
    fn duplicate_id_and_duplicate_combo_and_unmapped_key_errors() {
        let (mgr, mock) = mock_manager();
        mgr.register(1, HotkeyModifiers::ALT, b'C' as u32, false)
            .unwrap();

        // Duplicate id does not hit the backend.
        assert_eq!(
            mgr.register(1, HotkeyModifiers::CONTROL, b'D' as u32, false),
            Err(HotkeyError::IdInUse(1))
        );
        // Same combination under another id reports BadAccess.
        let conflict = mgr
            .register(
                2,
                HotkeyModifiers::ALT | HotkeyModifiers::NO_REPEAT,
                b'C' as u32,
                false,
            )
            .unwrap_err();
        assert_eq!(
            conflict,
            HotkeyError::Os {
                code: X11_BAD_ACCESS
            }
        );
        #[cfg(target_os = "linux")]
        assert!(conflict.is_already_registered());
        assert!(!mgr.is_registered(2));

        // Unmapped VK returns BadValue without touching grabs.
        assert_eq!(
            mgr.register(3, HotkeyModifiers::ALT, 0xFF, false),
            Err(HotkeyError::Os {
                code: X11_BAD_VALUE
            })
        );
        // Keysym absent in active X11 keymap also returns BadValue.
        mock.unmap_keysym(vk_to_x11_keysym(b'Z' as u32).unwrap());
        assert_eq!(
            mgr.register(4, HotkeyModifiers::ALT, b'Z' as u32, false),
            Err(HotkeyError::Os {
                code: X11_BAD_VALUE
            })
        );
        // Unknown id on unregister.
        assert_eq!(mgr.unregister(99), Err(HotkeyError::NotRegistered(99)));
        assert_eq!(mgr.registered_ids(), [1]);
        assert_eq!(mock.grabs().len(), 4);
    }

    #[test]
    fn replace_swaps_combination_and_rolls_back_on_conflict() {
        let (mgr, mock) = mock_manager();
        let kc_a = MockX11HotkeyBackend::mock_keycode_for_vk(b'A' as u32).unwrap();
        let kc_b = MockX11HotkeyBackend::mock_keycode_for_vk(b'B' as u32).unwrap();
        let kc_c = MockX11HotkeyBackend::mock_keycode_for_vk(b'C' as u32).unwrap();
        let base = x11_modifiers(HotkeyModifiers::CONTROL);

        // Free id: plain register via replace.
        mgr.register(10, HotkeyModifiers::CONTROL, b'A' as u32, true)
            .unwrap();
        assert_eq!(
            mgr.entry_for(10),
            Some((HotkeyModifiers::CONTROL, kc_a, base))
        );

        // Replace A -> B releases A and grabs B.
        mgr.register(10, HotkeyModifiers::CONTROL, b'B' as u32, true)
            .unwrap();
        assert_eq!(
            mgr.entry_for(10),
            Some((HotkeyModifiers::CONTROL, kc_b, base))
        );
        assert!(mock.grabs().iter().all(|&(kc, _)| kc == kc_b));
        assert_eq!(mock.grabs().len(), 4);

        // Replace B -> occupied C fails with BadAccess and rolls back to B.
        mock.occupy_all_locks(kc_c, base);
        let err = mgr
            .register(10, HotkeyModifiers::CONTROL, b'C' as u32, true)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: X11_BAD_ACCESS
            }
        );
        #[cfg(target_os = "linux")]
        assert!(err.is_already_registered());
        assert!(mgr.is_registered(10));
        assert_eq!(
            mgr.entry_for(10),
            Some((HotkeyModifiers::CONTROL, kc_b, base))
        );
        assert!(mock.grabs().iter().all(|&(kc, _)| kc == kc_b));
        assert_eq!(mock.grabs().len(), 4);

        // A is still free.
        mgr.register(11, HotkeyModifiers::CONTROL, b'A' as u32, false)
            .unwrap();
        assert!(mock.grabs().iter().any(|&(kc, _)| kc == kc_a));
    }

    #[test]
    fn press_delivery_strips_lock_masks_and_honors_no_repeat() {
        let (mgr, mock) = mock_manager();
        let kc = MockX11HotkeyBackend::mock_keycode_for_vk(b'C' as u32).unwrap();
        let base = x11_modifiers(HotkeyModifiers::ALT);
        mgr.register(
            7,
            HotkeyModifiers::ALT | HotkeyModifiers::NO_REPEAT,
            b'C' as u32,
            false,
        )
        .unwrap();

        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        mgr.set_handler(Some(Arc::new(move |id| {
            let name = thread::current().name().map(str::to_owned);
            tx.lock().unwrap().send((id, name)).unwrap();
        })));

        // All 4 lock-mask combinations deliver hotkey 7 on the worker thread.
        for &lock_mask in &X11_LOCK_MASK_VARIANTS {
            mock.press(kc, base | lock_mask);
            let (id, name) = rx.recv_timeout(PRESS_TIMEOUT).unwrap();
            assert_eq!(id, 7);
            assert_eq!(name.as_deref(), Some("ferrokit-hotkey"));
        }

        // Repeat press is suppressed when NO_REPEAT is set; a subsequent fresh press arrives.
        mock.press_repeat(kc, base | X11_LOCK_MASK);
        mock.press(kc, base);
        let (id, _) = rx.recv_timeout(PRESS_TIMEOUT).unwrap();
        assert_eq!(id, 7);
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    }

    #[test]
    fn reentrant_unregister_and_drop_from_handler_and_panic_shutdown() {
        // 1. Reentrant unregister inside handler on worker thread.
        let (mgr, mock) = mock_manager();
        let mgr = Arc::new(mgr);
        let kc = MockX11HotkeyBackend::mock_keycode_for_vk(b'K' as u32).unwrap();
        let base = x11_modifiers(HotkeyModifiers::ALT);
        mgr.register(3, HotkeyModifiers::ALT, b'K' as u32, false)
            .unwrap();

        let weak_cell: Arc<OnceLock<std::sync::Weak<X11GlobalHotkeyManager>>> = Arc::default();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let cell_clone = Arc::clone(&weak_cell);
        mgr.set_handler(Some(Arc::new(move |id| {
            let m = cell_clone.get().unwrap().upgrade().unwrap();
            let res = m.unregister(id);
            tx.lock().unwrap().send(res).unwrap();
        })));
        weak_cell.set(Arc::downgrade(&mgr)).unwrap();
        mock.press(kc, base);
        assert_eq!(rx.recv_timeout(PRESS_TIMEOUT).unwrap(), Ok(()));
        assert!(!mgr.is_registered(3));
        assert!(mock.grabs().is_empty());

        // 2. Dropping the last Arc inside the handler ends the worker without self-join deadlock.
        let (mgr, mock) = mock_manager();
        let mgr = Arc::new(mgr);
        mgr.register(4, HotkeyModifiers::ALT, b'K' as u32, false)
            .unwrap();
        let held = Mutex::new(Some(Arc::clone(&mgr)));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        mgr.set_handler(Some(Arc::new(move |_| {
            drop(held.lock().unwrap().take());
            tx.lock().unwrap().send(()).unwrap();
        })));
        drop(mgr);
        mock.press(kc, base);
        rx.recv_timeout(PRESS_TIMEOUT).unwrap();
        assert_eq!(
            rx.recv_timeout(PRESS_TIMEOUT),
            Err(RecvTimeoutError::Disconnected)
        );
        assert!(mock.grabs().is_empty());

        // 3. Handler panic cleans up grabs and transitions manager to Stopped.
        let (mgr, mock) = mock_manager();
        mgr.register(5, HotkeyModifiers::ALT, b'K' as u32, false)
            .unwrap();
        mgr.set_handler(Some(Arc::new(|_| {
            panic!("expected test panic in x11 hotkey handler")
        })));
        mock.press(kc, base);
        let deadline = std::time::Instant::now() + PRESS_TIMEOUT;
        loop {
            match mgr.register(6, HotkeyModifiers::ALT, b'M' as u32, false) {
                Err(HotkeyError::Stopped) => break,
                Ok(()) => {
                    let _ = mgr.unregister(6);
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
            assert!(std::time::Instant::now() < deadline, "worker still alive");
            thread::yield_now();
        }
        assert_eq!(mgr.unregister(5), Err(HotkeyError::Stopped));
        assert!(mgr.registered_ids().is_empty());
        assert!(mock.grabs().is_empty());
    }

    #[test]
    fn drop_unregisters_all_hotkeys() {
        let (mgr, mock) = mock_manager();
        mgr.register(1, HotkeyModifiers::ALT, b'A' as u32, false)
            .unwrap();
        mgr.register(2, HotkeyModifiers::CONTROL, b'B' as u32, false)
            .unwrap();
        assert_eq!(mock.grabs().len(), 8);
        drop(mgr);
        assert!(mock.grabs().is_empty());
    }

    #[test]
    fn disconnect_stops_worker_and_clears_hotkeys() {
        let (mgr, mock) = mock_manager();
        mgr.register(1, HotkeyModifiers::ALT, b'A' as u32, false)
            .unwrap();
        mock.disconnect();
        let deadline = std::time::Instant::now() + PRESS_TIMEOUT;
        loop {
            match mgr.register(2, HotkeyModifiers::ALT, b'B' as u32, false) {
                Err(HotkeyError::Stopped) => break,
                Ok(()) => {
                    let _ = mgr.unregister(2);
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
            assert!(std::time::Instant::now() < deadline, "worker still alive");
            thread::yield_now();
        }
        assert!(mgr.registered_ids().is_empty());
        assert!(mock.grabs().is_empty());
    }
}
