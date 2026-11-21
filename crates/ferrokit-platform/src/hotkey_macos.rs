// SPDX-License-Identifier: LGPL-3.0-only
//! Unified macOS Carbon back end and slot router for global hotkeys.
//!
//! Serves both [`GlobalHotkeyManager`](crate::global_hotkey::GlobalHotkeyManager) (`Send + Sync`,
//! cross-thread via `MainThreadExecutor`) and
//! [`CocoaHotkeyManager`](crate::hotkey::CocoaHotkeyManager) (`!Send + !Sync`, delivered on the
//! owner thread's event loop) through one shared Carbon `InstallEventHandler` (`'FKIT'`), one
//! virtual-key/modifier translation table, and one process-wide slot table.
//!
//! Carbon's `RegisterEventHotKey` / `UnregisterEventHotKey` target `GetApplicationEventTarget()`
//! and must run on the OS main thread. `MacosGlobalHotkeyManager` preserves
//! [`GlobalHotkeyManager`](crate::global_hotkey::GlobalHotkeyManager)'s `Send + Sync` contract by
//! routing Carbon operations through a `MainThreadExecutor`:
//!
//! - Calls made **on the OS main thread** execute Carbon calls directly in place (so startup
//!   registration before `CoreApplication::exec()`, re-entrant calls from inside a hotkey handler,
//!   and `Drop` on the main thread never deadlock).
//! - Calls made **from a background thread** marshal onto the OS main thread via
//!   `ferrokit-core`'s main-thread event loop when `CoreApplication::in_exec()` is active; when no
//!   main-thread event loop is running (for example on `cargo test` worker threads), they fail
//!   immediately with `HotkeyError::Thread` rather than panicking or deadlocking.
//!
//! Everything that touches Carbon goes through `carbon` (and, for `GlobalHotkeyManager`,
//! `MacosHotkeyBackend` + `MainThreadExecutor`), so key tables, slot routing, registration
//! lifecycle, `replace()` rollback, conflict detection (`eventHotKeyExistsErr` = `-9878`), panic
//! containment, and cross-thread marshaling are unit-tested on every OS with mock back ends.

use crate::hotkey::HotkeyModifiers;
use ferrokit_core::object::ThreadId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

#[cfg(any(target_os = "macos", test))]
use crate::hotkey::HotkeyError;
#[cfg(any(target_os = "macos", test))]
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(any(target_os = "macos", test))]
use std::sync::{Arc, Weak};

// Carbon modifier masks (`Events.h`: cmdKey, shiftKey, optionKey, controlKey).
pub const CARBON_CMD_KEY: u32 = 1 << 8;
pub const CARBON_SHIFT_KEY: u32 = 1 << 9;
pub const CARBON_OPTION_KEY: u32 = 1 << 11;
pub const CARBON_CONTROL_KEY: u32 = 1 << 12;

/// Carbon `eventHotKeyInvalidErr` (`-9879`) encoded as `u32` for [`HotkeyError::Os`].
#[cfg(any(target_os = "macos", test))]
pub(crate) const CARBON_EVENT_HOT_KEY_INVALID_ERR: u32 = (-9879_i32) as u32;

/// Carbon modifier mask for `modifiers`. The flags name physical keys, as on the other back ends:
/// `ALT` is Option, `CONTROL` is Control, `SHIFT` is Shift and `WIN` (the Super/Windows key) is
/// Command. `NO_REPEAT` has no Carbon counterpart: a hotkey fires once per press, never on key
/// repeat, so it is always in effect.
pub fn carbon_modifiers(modifiers: HotkeyModifiers) -> u32 {
    let mut mask = 0;
    if modifiers.contains(HotkeyModifiers::ALT) {
        mask |= CARBON_OPTION_KEY;
    }
    if modifiers.contains(HotkeyModifiers::CONTROL) {
        mask |= CARBON_CONTROL_KEY;
    }
    if modifiers.contains(HotkeyModifiers::SHIFT) {
        mask |= CARBON_SHIFT_KEY;
    }
    if modifiers.contains(HotkeyModifiers::WIN) {
        mask |= CARBON_CMD_KEY;
    }
    mask
}

/// Carbon virtual key code (`kVK_ANSI_*`, `kVK_F*`, ... of `Events.h`, ANSI layout) of the
/// Windows-style virtual key `vk` that
/// [`PlatformHotkeyManager::register_hotkey`](crate::hotkey::PlatformHotkeyManager::register_hotkey)
/// and [`GlobalHotkeyManager::register`](crate::global_hotkey::GlobalHotkeyManager::register) take:
/// `'A'..='Z'`, `'0'..='9'`, `VK_F1..VK_F20`, the arrows, editing and numeric keypad keys and the
/// `VK_OEM_*` punctuation keys. `None` if the key has no Mac counterpart.
pub fn carbon_keycode(vk: u32) -> Option<u32> {
    Some(match vk {
        0x41 => 0,   // A
        0x53 => 1,   // S
        0x44 => 2,   // D
        0x46 => 3,   // F
        0x48 => 4,   // H
        0x47 => 5,   // G
        0x5A => 6,   // Z
        0x58 => 7,   // X
        0x43 => 8,   // C
        0x56 => 9,   // V
        0x42 => 11,  // B
        0x51 => 12,  // Q
        0x57 => 13,  // W
        0x45 => 14,  // E
        0x52 => 15,  // R
        0x59 => 16,  // Y
        0x54 => 17,  // T
        0x4F => 31,  // O
        0x55 => 32,  // U
        0x49 => 34,  // I
        0x50 => 35,  // P
        0x4C => 37,  // L
        0x4A => 38,  // J
        0x4B => 40,  // K
        0x4E => 45,  // N
        0x4D => 46,  // M
        0x31 => 18,  // 1
        0x32 => 19,  // 2
        0x33 => 20,  // 3
        0x34 => 21,  // 4
        0x35 => 23,  // 5
        0x36 => 22,  // 6
        0x37 => 26,  // 7
        0x38 => 28,  // 8
        0x39 => 25,  // 9
        0x30 => 29,  // 0
        0x0D => 36,  // VK_RETURN
        0x09 => 48,  // VK_TAB
        0x20 => 49,  // VK_SPACE
        0x08 => 51,  // VK_BACK (Delete)
        0x1B => 53,  // VK_ESCAPE
        0x2E => 117, // VK_DELETE (forward delete)
        0x2D => 114, // VK_INSERT (Help)
        0x24 => 115, // VK_HOME
        0x23 => 119, // VK_END
        0x21 => 116, // VK_PRIOR (Page Up)
        0x22 => 121, // VK_NEXT (Page Down)
        0x25 => 123, // VK_LEFT
        0x27 => 124, // VK_RIGHT
        0x28 => 125, // VK_DOWN
        0x26 => 126, // VK_UP
        0x70 => 122, // VK_F1
        0x71 => 120, // F2
        0x72 => 99,  // F3
        0x73 => 118, // F4
        0x74 => 96,  // F5
        0x75 => 97,  // F6
        0x76 => 98,  // F7
        0x77 => 100, // F8
        0x78 => 101, // F9
        0x79 => 109, // F10
        0x7A => 103, // F11
        0x7B => 111, // F12
        0x7C => 105, // F13
        0x7D => 107, // F14
        0x7E => 113, // F15
        0x7F => 106, // F16
        0x80 => 64,  // F17
        0x81 => 79,  // F18
        0x82 => 80,  // F19
        0x83 => 90,  // F20
        0x60 => 82,  // VK_NUMPAD0
        0x61 => 83,  // NUMPAD1
        0x62 => 84,  // NUMPAD2
        0x63 => 85,  // NUMPAD3
        0x64 => 86,  // NUMPAD4
        0x65 => 87,  // NUMPAD5
        0x66 => 88,  // NUMPAD6
        0x67 => 89,  // NUMPAD7
        0x68 => 91,  // NUMPAD8
        0x69 => 92,  // NUMPAD9
        0x6A => 67,  // VK_MULTIPLY
        0x6B => 69,  // VK_ADD
        0x6D => 78,  // VK_SUBTRACT
        0x6E => 65,  // VK_DECIMAL
        0x6F => 75,  // VK_DIVIDE
        0xBA => 41,  // VK_OEM_1 ;
        0xBB => 24,  // VK_OEM_PLUS =
        0xBC => 43,  // VK_OEM_COMMA ,
        0xBD => 27,  // VK_OEM_MINUS -
        0xBE => 47,  // VK_OEM_PERIOD .
        0xBF => 44,  // VK_OEM_2 /
        0xC0 => 50,  // VK_OEM_3 `
        0xDB => 33,  // VK_OEM_4 [
        0xDC => 42,  // VK_OEM_5 \
        0xDD => 30,  // VK_OEM_6 ]
        0xDE => 39,  // VK_OEM_7 '
        _ => return None,
    })
}

/// Routing entry for [`CocoaHotkeyManager`](crate::hotkey::CocoaHotkeyManager): manager
/// `instance`, application hotkey `id`, and owning thread.
#[derive(Clone, Copy)]
pub(crate) struct CocoaRoute {
    pub(crate) instance: u64,
    pub(crate) id: u32,
    pub(crate) owner: ThreadId,
}

#[derive(Clone)]
enum RouteTarget {
    Cocoa(CocoaRoute),
    #[cfg(any(target_os = "macos", test))]
    Global {
        manager: Weak<Inner>,
        id: u32,
    },
}

static ROUTES: LazyLock<Mutex<HashMap<u32, RouteTarget>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_SLOT: AtomicU32 = AtomicU32::new(1);

// Every critical section is a handful of map/flag operations, so the data stays consistent even
// after a panic elsewhere.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn routes() -> MutexGuard<'static, HashMap<u32, RouteTarget>> {
    lock(&ROUTES)
}

pub(crate) fn allocate_slot() -> u32 {
    NEXT_SLOT.fetch_add(1, Ordering::Relaxed)
}

pub(crate) fn insert_cocoa_route(slot: u32, route: CocoaRoute) {
    routes().insert(slot, RouteTarget::Cocoa(route));
}

pub(crate) fn get_cocoa_route(slot: u32) -> Option<CocoaRoute> {
    match routes().get(&slot) {
        Some(RouteTarget::Cocoa(route)) => Some(*route),
        #[cfg(any(target_os = "macos", test))]
        Some(RouteTarget::Global { .. }) => None,
        None => None,
    }
}

pub(crate) fn remove_route(slot: u32) {
    routes().remove(&slot);
}

/// Routes a Carbon `kEventHotKeyPressed` (or mock press) for `slot` to the registered manager.
/// Returns whether `slot` belonged to a live registration.
///
/// `ROUTES` is released before dispatching, so a handler (or queued delivery) may register,
/// replace, unregister, or drop its manager without deadlocking.
pub(crate) fn route_press(slot: u32) -> bool {
    let target = { routes().get(&slot).cloned() };
    match target {
        None => false,
        Some(RouteTarget::Cocoa(route)) => {
            crate::hotkey::cocoa_hotkey::enqueue_press(slot, route);
            true
        }
        #[cfg(any(target_os = "macos", test))]
        Some(RouteTarget::Global { manager, id }) => deliver_global_press(slot, manager, id),
    }
}

#[cfg(any(target_os = "macos", test))]
type Handler = Arc<dyn Fn(u32) + Send + Sync>;
#[cfg(any(target_os = "macos", test))]
type BoxedTask = Box<dyn FnOnce() + Send>;

/// Low-level macOS hotkey operations executed on the OS main thread.
///
/// On macOS the production implementation delegates to [`carbon`] (`InstallEventHandler`,
/// `RegisterEventHotKey`, and `UnregisterEventHotKey`). Tests inject `MockMacosHotkeyBackend` so
/// the full manager runs on any OS.
#[cfg(any(target_os = "macos", test))]
trait MacosHotkeyBackend: Send {
    /// Ensures the process-wide hotkey event handler is installed.
    fn initialize(&mut self) -> Result<(), HotkeyError>;

    /// Registers `keycode` + `modifiers` (Carbon values) under process-unique `slot`.
    /// Returns an opaque non-zero handle for [`unregister`](Self::unregister).
    fn register(&mut self, slot: u32, keycode: u32, modifiers: u32) -> Result<usize, HotkeyError>;

    /// Unregisters a handle previously returned by [`register`](Self::register).
    fn unregister(&mut self, handle: usize) -> Result<(), HotkeyError>;
}

/// Runs Carbon operations on the macOS OS main thread without deadlocking.
#[cfg(any(target_os = "macos", test))]
trait MainThreadExecutor: Send + Sync {
    /// Runs `f` synchronously on the main thread (inline when already on the main thread, or
    /// marshaled when on another thread with an active main-thread event loop).
    fn run_sync(&self, f: BoxedTask) -> Result<(), HotkeyError>;

    /// Runs `f` on the main thread during `Drop` / shutdown cleanup (inline on the main thread,
    /// or best-effort asynchronous dispatch from another thread so `Drop` never deadlocks).
    fn run_cleanup(&self, f: BoxedTask);
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy)]
struct Registration {
    slot: u32,
    handle: usize,
    keycode: u32,
    modifiers: u32,
}

#[cfg(any(target_os = "macos", test))]
struct State {
    backend: Box<dyn MacosHotkeyBackend>,
    stopped: bool,
    hotkeys: HashMap<u32, Registration>,
    handler: Option<Handler>,
}

#[cfg(any(target_os = "macos", test))]
struct Inner {
    state: Mutex<State>,
    executor: Arc<dyn MainThreadExecutor>,
}

#[cfg(any(target_os = "macos", test))]
fn exec_on_main<T: Send + 'static>(
    executor: &dyn MainThreadExecutor,
    f: impl FnOnce() -> Result<T, HotkeyError> + Send + 'static,
) -> Result<T, HotkeyError> {
    let slot: Arc<Mutex<Option<Result<T, HotkeyError>>>> = Arc::new(Mutex::new(None));
    let slot_for_task = Arc::clone(&slot);
    executor.run_sync(Box::new(move || {
        let res = f();
        *lock(&slot_for_task) = Some(res);
    }))?;
    lock(&slot).take().unwrap_or(Err(HotkeyError::Stopped))
}

/// Marks the manager stopped, removes all routes and the handler, and unregisters every held
/// hotkey through `executor.run_cleanup`. Safe to call multiple times or from inside a handler.
#[cfg(any(target_os = "macos", test))]
fn shutdown(inner: &Arc<Inner>) {
    let (handles, old_handler) = {
        let mut state = lock(&inner.state);
        state.stopped = true;
        let old_handler = state.handler.take();
        let drained: Vec<Registration> = state.hotkeys.drain().map(|(_, reg)| reg).collect();
        let mut route_map = routes();
        let mut handles = Vec::with_capacity(drained.len());
        for reg in drained {
            route_map.remove(&reg.slot);
            handles.push(reg.handle);
        }
        (handles, old_handler)
    };
    // Drop the handler after releasing `inner.state` in case its `Drop` touches the manager.
    drop(old_handler);
    if !handles.is_empty() {
        let inner_for_cleanup = Arc::clone(inner);
        inner.executor.run_cleanup(Box::new(move || {
            let mut state = lock(&inner_for_cleanup.state);
            for handle in handles {
                let _ = state.backend.unregister(handle);
            }
        }));
    }
}

/// Delivers a press to [`MacosGlobalHotkeyManager`]'s handler without holding `ROUTES` or
/// `inner.state`. Panics in the handler are caught (never unwinding into Carbon's C stack) and stop
/// the manager after unregistering its hotkeys, matching the Windows back end.
#[cfg(any(target_os = "macos", test))]
fn deliver_global_press(slot: u32, weak_inner: Weak<Inner>, id: u32) -> bool {
    let Some(inner) = weak_inner.upgrade() else {
        remove_route(slot);
        return false;
    };
    let (active, handler) = {
        let state = lock(&inner.state);
        let matches_slot = state.hotkeys.get(&id).is_some_and(|r| r.slot == slot);
        let active = !state.stopped && matches_slot;
        let handler = if active { state.handler.clone() } else { None };
        (active, handler)
    };
    if !active {
        return false;
    }
    if let Some(handler) = handler
        && catch_unwind(AssertUnwindSafe(|| handler(id))).is_err()
    {
        shutdown(&inner);
    }
    true
}

/// Thread-safe (`Send + Sync`) macOS global-hotkey manager used behind
/// [`GlobalHotkeyManager`](crate::global_hotkey::GlobalHotkeyManager) on `target_os = "macos"`.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct MacosGlobalHotkeyManager {
    inner: Arc<Inner>,
}

#[cfg(any(target_os = "macos", test))]
impl std::fmt::Debug for MacosGlobalHotkeyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosGlobalHotkeyManager")
            .field("registered_ids", &self.registered_ids())
            .finish_non_exhaustive()
    }
}

#[cfg(any(target_os = "macos", test))]
impl MacosGlobalHotkeyManager {
    /// Creates a manager backed by Carbon on macOS (or returns [`HotkeyError::Unsupported`] off
    /// macOS).
    ///
    /// On macOS this executes `InstallEventHandler` on the OS main thread directly when called on
    /// the main thread, marshals via `CoreApplication`'s active event loop when called from a
    /// background thread while `CoreApplication::in_exec()` is true, or returns
    /// [`HotkeyError::Thread`] if called off the main thread with no running main-thread loop.
    pub(crate) fn new() -> Result<Self, HotkeyError> {
        #[cfg(target_os = "macos")]
        {
            Self::with_backend_and_executor(
                Box::new(carbon::CarbonGlobalBackend),
                Arc::new(native_executor::NativeMainThreadExecutor),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(HotkeyError::Unsupported)
        }
    }

    /// Creates a manager with an explicit `MacosHotkeyBackend` and `MainThreadExecutor`.
    /// Initializes the backend on the executor's main thread before returning.
    fn with_backend_and_executor(
        backend: Box<dyn MacosHotkeyBackend>,
        executor: Arc<dyn MainThreadExecutor>,
    ) -> Result<Self, HotkeyError> {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                backend,
                stopped: false,
                hotkeys: HashMap::new(),
                handler: None,
            }),
            executor,
        });
        let inner_for_init = Arc::clone(&inner);
        exec_on_main(&*inner.executor, move || {
            let mut state = lock(&inner_for_init.state);
            state.backend.initialize()
        })?;
        Ok(Self { inner })
    }

    /// Registers `id` for `modifiers` + `vk` (or replaces its existing combination when `replace`
    /// is true, rolling back to the old combination if registering the new one fails).
    pub(crate) fn register(
        &self,
        id: u32,
        modifiers: HotkeyModifiers,
        vk: u32,
        replace: bool,
    ) -> Result<(), HotkeyError> {
        if lock(&self.inner.state).stopped {
            return Err(HotkeyError::Stopped);
        }
        let inner = Arc::clone(&self.inner);
        exec_on_main(&*self.inner.executor, move || {
            let mut state = lock(&inner.state);
            if state.stopped {
                return Err(HotkeyError::Stopped);
            }
            let old = state.hotkeys.get(&id).copied();
            if old.is_some() && !replace {
                return Err(HotkeyError::IdInUse(id));
            }
            let Some(keycode) = carbon_keycode(vk) else {
                return Err(HotkeyError::Os {
                    code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
                });
            };
            let mods = carbon_modifiers(modifiers);

            if let Some(old_reg) = old {
                // Release the old combination first (matching Windows `replace` semantics).
                remove_route(old_reg.slot);
                state.hotkeys.remove(&id);
                let _ = state.backend.unregister(old_reg.handle);
            }

            let slot = allocate_slot();
            match state.backend.register(slot, keycode, mods) {
                Ok(handle) => {
                    state.hotkeys.insert(
                        id,
                        Registration {
                            slot,
                            handle,
                            keycode,
                            modifiers: mods,
                        },
                    );
                    routes().insert(
                        slot,
                        RouteTarget::Global {
                            manager: Arc::downgrade(&inner),
                            id,
                        },
                    );
                    Ok(())
                }
                Err(err) => {
                    if let Some(old_reg) = old {
                        let rollback_slot = allocate_slot();
                        if let Ok(rollback_handle) = state.backend.register(
                            rollback_slot,
                            old_reg.keycode,
                            old_reg.modifiers,
                        ) {
                            state.hotkeys.insert(
                                id,
                                Registration {
                                    slot: rollback_slot,
                                    handle: rollback_handle,
                                    keycode: old_reg.keycode,
                                    modifiers: old_reg.modifiers,
                                },
                            );
                            routes().insert(
                                rollback_slot,
                                RouteTarget::Global {
                                    manager: Arc::downgrade(&inner),
                                    id,
                                },
                            );
                        }
                    }
                    Err(err)
                }
            }
        })
    }

    /// Releases `id` ([`HotkeyError::NotRegistered`] if not held by this manager).
    pub(crate) fn unregister(&self, id: u32) -> Result<(), HotkeyError> {
        if lock(&self.inner.state).stopped {
            return Err(HotkeyError::Stopped);
        }
        let inner = Arc::clone(&self.inner);
        exec_on_main(&*self.inner.executor, move || {
            let mut state = lock(&inner.state);
            if state.stopped {
                return Err(HotkeyError::Stopped);
            }
            let Some(reg) = state.hotkeys.remove(&id) else {
                return Err(HotkeyError::NotRegistered(id));
            };
            remove_route(reg.slot);
            state.backend.unregister(reg.handle)
        })
    }

    /// Whether this manager currently holds `id`.
    pub(crate) fn is_registered(&self, id: u32) -> bool {
        lock(&self.inner.state).hotkeys.contains_key(&id)
    }

    /// Ids this manager currently holds, sorted ascending.
    pub(crate) fn registered_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = lock(&self.inner.state).hotkeys.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Sets or clears the callback invoked when a registered hotkey is pressed.
    pub(crate) fn set_handler(&self, handler: Option<Handler>) {
        let dropped = {
            let mut state = lock(&self.inner.state);
            if state.stopped {
                handler
            } else {
                std::mem::replace(&mut state.handler, handler)
            }
        };
        // Drop outside `inner.state` in case the closure's `Drop` touches the manager.
        drop(dropped);
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for MacosGlobalHotkeyManager {
    fn drop(&mut self) {
        shutdown(&self.inner);
    }
}

/// One hotkey held by a `MockMacosHotkeyBackend`.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct MockMacosHotkey {
    handle: usize,
    slot: u32,
    keycode: u32,
    modifiers: u32,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct MockState {
    initialized: bool,
    fail_init: Option<HotkeyError>,
    hotkeys: Vec<MockMacosHotkey>,
    /// Combinations occupied by "another application".
    occupied: Vec<(u32, u32)>,
    /// Scripted outcomes for upcoming `register` calls (`None` falls back to normal conflict check).
    scripted_register_errors: std::collections::VecDeque<Option<HotkeyError>>,
    next_handle: usize,
    op_threads: Vec<std::thread::ThreadId>,
}

/// Thread-safe in-memory Carbon hotkey backend for cross-platform unit tests.
///
/// Clones share the underlying table so a test keeps a handle to inspect registrations, inject
/// conflicts (`occupy`), and trigger hotkey presses (`press`) while the manager owns another clone.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct MockMacosHotkeyBackend {
    state: Arc<Mutex<MockState>>,
}

#[cfg(test)]
impl MockMacosHotkeyBackend {
    fn new() -> Self {
        Self::default()
    }

    /// Whether [`MacosHotkeyBackend::initialize`] succeeded on this backend.
    fn is_initialized(&self) -> bool {
        lock(&self.state).initialized
    }

    /// Makes [`MacosHotkeyBackend::initialize`] fail with `err`.
    fn fail_initialize(&self, err: HotkeyError) {
        lock(&self.state).fail_init = Some(err);
    }

    /// Hotkeys currently registered, in registration order.
    fn hotkeys(&self) -> Vec<MockMacosHotkey> {
        lock(&self.state).hotkeys.clone()
    }

    /// Threads on which backend operations (`initialize`, `register`, `unregister`) ran.
    fn op_threads(&self) -> Vec<std::thread::ThreadId> {
        lock(&self.state).op_threads.clone()
    }

    /// Marks Carbon `keycode` + `modifiers` as held by another application (triggers
    /// `HotkeyError::Os { code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR }`).
    fn occupy(&self, keycode: u32, modifiers: u32) {
        lock(&self.state).occupied.push((keycode, modifiers));
    }

    /// Scripts the next `register` call (`Some(err)` forces `Err(err)`, `None` uses normal logic).
    fn push_register_outcome(&self, outcome: Option<HotkeyError>) {
        lock(&self.state)
            .scripted_register_errors
            .push_back(outcome);
    }

    /// Simulates a user pressing Carbon `keycode` + `modifiers`. Returns whether a registered
    /// hotkey handled it.
    fn press(&self, keycode: u32, modifiers: u32) -> bool {
        let slot = {
            let state = lock(&self.state);
            state
                .hotkeys
                .iter()
                .find(|h| h.keycode == keycode && h.modifiers == modifiers)
                .map(|h| h.slot)
        };
        // Lock released before routing so a re-entrant handler can register/unregister freely.
        slot.is_some_and(route_press)
    }

    /// Simulates Carbon delivering a raw `slot` press (useful for testing stale/unknown slots).
    fn press_slot(&self, slot: u32) -> bool {
        route_press(slot)
    }
}

#[cfg(test)]
impl MacosHotkeyBackend for MockMacosHotkeyBackend {
    fn initialize(&mut self) -> Result<(), HotkeyError> {
        let mut state = lock(&self.state);
        state.op_threads.push(std::thread::current().id());
        if let Some(err) = state.fail_init.clone() {
            return Err(err);
        }
        state.initialized = true;
        Ok(())
    }

    fn register(&mut self, slot: u32, keycode: u32, modifiers: u32) -> Result<usize, HotkeyError> {
        let mut state = lock(&self.state);
        state.op_threads.push(std::thread::current().id());
        if let Some(Some(err)) = state.scripted_register_errors.pop_front() {
            return Err(err);
        }
        let key = (keycode, modifiers);
        if state.occupied.contains(&key)
            || state
                .hotkeys
                .iter()
                .any(|h| (h.keycode, h.modifiers) == key)
        {
            return Err(HotkeyError::Os {
                code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR,
            });
        }
        state.next_handle += 1;
        let handle = state.next_handle;
        state.hotkeys.push(MockMacosHotkey {
            handle,
            slot,
            keycode,
            modifiers,
        });
        Ok(handle)
    }

    fn unregister(&mut self, handle: usize) -> Result<(), HotkeyError> {
        let mut state = lock(&self.state);
        state.op_threads.push(std::thread::current().id());
        match state.hotkeys.iter().position(|h| h.handle == handle) {
            Some(i) => {
                state.hotkeys.remove(i);
                Ok(())
            }
            None => Err(HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
            }),
        }
    }
}

/// Inline [`MainThreadExecutor`] for single-thread tests (runs every task immediately in place).
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy)]
struct InlineMainThreadExecutor;

#[cfg(test)]
impl MainThreadExecutor for InlineMainThreadExecutor {
    fn run_sync(&self, f: BoxedTask) -> Result<(), HotkeyError> {
        f();
        Ok(())
    }

    fn run_cleanup(&self, f: BoxedTask) {
        f();
    }
}

#[cfg(target_os = "macos")]
mod native_executor {
    use super::carbon::is_os_main_thread;
    use super::*;
    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::event_loop::{get_thread_event_sender, post_event_to_thread};
    use ferrokit_core::object::{ObjectId, ThreadContext};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    const MAIN_LOOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

    pub(super) struct NativeMainThreadExecutor;

    impl MainThreadExecutor for NativeMainThreadExecutor {
        fn run_sync(&self, f: BoxedTask) -> Result<(), HotkeyError> {
            if is_os_main_thread() {
                f();
                return Ok(());
            }

            let Some(main_tid) = ThreadContext::main_thread_id() else {
                return Err(HotkeyError::Thread(
                    "macOS global hotkeys require the OS main thread or a running CoreApplication event loop"
                        .to_owned(),
                ));
            };
            if !CoreApplication::in_exec() || get_thread_event_sender(main_tid).is_none() {
                return Err(HotkeyError::Thread(
                    "macOS global hotkeys require the OS main thread or a running main-thread event loop"
                        .to_owned(),
                ));
            }

            let (tx, rx) = mpsc::channel::<Result<(), HotkeyError>>();
            let claimed = Arc::new(AtomicBool::new(false));
            let claimed_for_task = Arc::clone(&claimed);
            post_event_to_thread(
                main_tid,
                ObjectId(0),
                Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                    if claimed_for_task.swap(true, Ordering::AcqRel) {
                        return;
                    }
                    if !is_os_main_thread() {
                        let _ = tx.send(Err(HotkeyError::Thread(
                            "CoreApplication main thread is not the macOS OS main thread"
                                .to_owned(),
                        )));
                        return;
                    }
                    f();
                    let _ = tx.send(Ok(()));
                }))),
            );

            loop {
                match rx.recv_timeout(MAIN_LOOP_POLL_INTERVAL) {
                    Ok(result) => return result,
                    Err(RecvTimeoutError::Disconnected) => return Err(HotkeyError::Stopped),
                    Err(RecvTimeoutError::Timeout) => {
                        if !CoreApplication::in_exec()
                            || get_thread_event_sender(main_tid).is_none()
                        {
                            if !claimed.swap(true, Ordering::AcqRel) {
                                // Cancelled before the main thread picked up the task.
                                return Err(HotkeyError::Stopped);
                            }
                            // The task already started on the main thread; wait for its reply.
                            return rx.recv().unwrap_or(Err(HotkeyError::Stopped));
                        }
                    }
                }
            }
        }

        fn run_cleanup(&self, f: BoxedTask) {
            if is_os_main_thread() {
                f();
                return;
            }
            let Some(main_tid) = ThreadContext::main_thread_id() else {
                return;
            };
            if get_thread_event_sender(main_tid).is_some() {
                post_event_to_thread(
                    main_tid,
                    ObjectId(0),
                    Event::new(EventKind::MetaCallNoTarget(Box::new(move || {
                        if is_os_main_thread() {
                            f();
                        }
                    }))),
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod carbon {
    use super::*;
    use std::ffi::{c_int, c_void};

    type OSStatus = i32;
    type EventTargetRef = *mut c_void;
    type EventHandlerRef = *mut c_void;
    type EventHandlerCallRef = *mut c_void;
    type EventRef = *mut c_void;
    type EventHotKeyRef = *mut c_void;
    type EventHandlerProc = extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

    #[repr(C)]
    struct EventTypeSpec {
        event_class: u32,
        event_kind: u32,
    }

    /// `EventHotKeyID`, passed to `RegisterEventHotKey` by value.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct EventHotKeyId {
        signature: u32,
        id: u32,
    }

    const NO_ERR: OSStatus = 0;
    const EVENT_NOT_HANDLED_ERR: OSStatus = -9874;
    const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
    const EVENT_HOT_KEY_PRESSED: u32 = 5;
    const EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
    const TYPE_EVENT_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
    /// Unified `EventHotKeyID::signature` ('FKIT') for both `CocoaHotkeyManager` and
    /// `GlobalHotkeyManager`.
    const SIGNATURE: u32 = u32::from_be_bytes(*b"FKIT");

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn GetApplicationEventTarget() -> EventTargetRef;
        fn InstallEventHandler(
            target: EventTargetRef,
            handler: EventHandlerProc,
            num_types: usize,
            list: *const EventTypeSpec,
            user_data: *mut c_void,
            out_ref: *mut EventHandlerRef,
        ) -> OSStatus;
        fn GetEventParameter(
            event: EventRef,
            name: u32,
            desired_type: u32,
            actual_type: *mut u32,
            buffer_size: usize,
            actual_size: *mut usize,
            out_data: *mut c_void,
        ) -> OSStatus;
        fn RegisterEventHotKey(
            key_code: u32,
            modifiers: u32,
            hot_key_id: EventHotKeyId,
            target: EventTargetRef,
            options: u32,
            out_ref: *mut EventHotKeyRef,
        ) -> OSStatus;
        fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
    }

    /// Returns whether the calling thread is the process's macOS OS main thread (`pthread_main_np`).
    pub(crate) fn is_os_main_thread() -> bool {
        unsafe extern "C" {
            fn pthread_main_np() -> c_int;
        }
        // SAFETY: `pthread_main_np` takes no arguments, touches no caller memory, and is
        // documented by Apple libSystem as thread-safe. It returns > 0 only on the process's
        // initial (main) thread (0 on other threads, -1 before initialization).
        unsafe { pthread_main_np() > 0 }
    }

    /// Carbon invokes this on the OS main thread for every `kEventHotKeyPressed`.
    extern "C" fn hot_key_pressed(
        _call: EventHandlerCallRef,
        event: EventRef,
        _user_data: *mut c_void,
    ) -> OSStatus {
        if event.is_null() {
            return EVENT_NOT_HANDLED_ERR;
        }
        let mut id = EventHotKeyId {
            signature: 0,
            id: 0,
        };
        // SAFETY: `event` was checked non-null above and is the live `EventRef` Carbon passed to
        // this callback; `id` is a writable `EventHotKeyId` local whose exact byte size is
        // supplied, and the optional `actual_type` / `actual_size` out pointers are permitted to
        // be null by Carbon.
        let status = unsafe {
            GetEventParameter(
                event,
                EVENT_PARAM_DIRECT_OBJECT,
                TYPE_EVENT_HOT_KEY_ID,
                std::ptr::null_mut(),
                std::mem::size_of::<EventHotKeyId>(),
                std::ptr::null_mut(),
                (&raw mut id).cast::<c_void>(),
            )
        };
        // Never allow a Rust panic to unwind across Carbon's C stack frames (UB).
        let routed =
            catch_unwind(|| status == NO_ERR && id.signature == SIGNATURE && route_press(id.id))
                .unwrap_or(false);
        if routed {
            NO_ERR
        } else {
            EVENT_NOT_HANDLED_ERR
        }
    }

    static HANDLER_INSTALLED: Mutex<bool> = Mutex::new(false);

    /// Ensures the single process-wide Carbon `kEventHotKeyPressed` handler (`'FKIT'`) is
    /// installed on `GetApplicationEventTarget()`. Retries cleanly if a prior attempt failed.
    pub(crate) fn ensure_handler_installed() -> Result<(), HotkeyError> {
        if !is_os_main_thread() {
            return Err(HotkeyError::Thread(
                "macOS Carbon hotkey operations must run on the OS main thread".to_owned(),
            ));
        }
        let mut installed = lock(&HANDLER_INSTALLED);
        if *installed {
            return Ok(());
        }
        // SAFETY: `GetApplicationEventTarget` takes no arguments and returns the process-wide
        // Carbon application target, valid for the lifetime of the process; `is_os_main_thread()`
        // was verified above.
        let target = unsafe { GetApplicationEventTarget() };
        if target.is_null() {
            return Err(HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
            });
        }
        let spec = EventTypeSpec {
            event_class: EVENT_CLASS_KEYBOARD,
            event_kind: EVENT_HOT_KEY_PRESSED,
        };
        let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
        // SAFETY: `is_os_main_thread()` was verified above and `target` was checked non-null;
        // `hot_key_pressed` has the exact `EventHandlerProc` ABI; `spec` is a live `EventTypeSpec`
        // (count 1) copied by Carbon; `user_data` is unused (null); `handler_ref` is a writable
        // out pointer. The installed process-wide handler carries no per-manager state and stays
        // valid for the process lifetime.
        let status = unsafe {
            InstallEventHandler(
                target,
                hot_key_pressed,
                1,
                &spec,
                std::ptr::null_mut(),
                &mut handler_ref,
            )
        };
        if status == NO_ERR {
            *installed = true;
            Ok(())
        } else {
            Err(HotkeyError::Os {
                code: status as u32,
            })
        }
    }

    /// Registers Carbon `keycode` + `modifiers` under process-unique `slot`.
    pub(crate) fn register_hotkey(
        slot: u32,
        keycode: u32,
        modifiers: u32,
    ) -> Result<usize, HotkeyError> {
        ensure_handler_installed()?;
        // SAFETY: `GetApplicationEventTarget` takes no arguments and returns the process-wide
        // application target; `ensure_handler_installed` verified `is_os_main_thread()` above.
        let target = unsafe { GetApplicationEventTarget() };
        if target.is_null() {
            return Err(HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
            });
        }
        let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
        // SAFETY: `is_os_main_thread()` and `ensure_handler_installed` were verified above;
        // `target` is checked non-null; `hot_key` is a live writable out pointer read only
        // when Carbon returns `NO_ERR`.
        let status = unsafe {
            RegisterEventHotKey(
                keycode,
                modifiers,
                EventHotKeyId {
                    signature: SIGNATURE,
                    id: slot,
                },
                target,
                0,
                &mut hot_key,
            )
        };
        match status {
            NO_ERR if !hot_key.is_null() => Ok(hot_key as usize),
            NO_ERR => Err(HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
            }),
            err => Err(HotkeyError::Os { code: err as u32 }),
        }
    }

    /// Unregisters a Carbon `EventHotKeyRef` handle previously returned by [`register_hotkey`].
    pub(crate) fn unregister_hotkey(handle: usize) -> Result<(), HotkeyError> {
        if !is_os_main_thread() {
            return Err(HotkeyError::Thread(
                "macOS Carbon hotkey operations must run on the OS main thread".to_owned(),
            ));
        }
        if handle == 0 {
            return Err(HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR,
            });
        }
        // SAFETY: `is_os_main_thread()` was verified above; `handle` is a non-null
        // `EventHotKeyRef` returned by a prior successful `RegisterEventHotKey`, passed here
        // at most once because the manager removes the registration entry before calling
        // `unregister`.
        let status = unsafe { UnregisterEventHotKey(handle as EventHotKeyRef) };
        if status == NO_ERR {
            Ok(())
        } else {
            Err(HotkeyError::Os {
                code: status as u32,
            })
        }
    }

    pub(super) struct CarbonGlobalBackend;

    impl MacosHotkeyBackend for CarbonGlobalBackend {
        fn initialize(&mut self) -> Result<(), HotkeyError> {
            ensure_handler_installed()
        }

        fn register(
            &mut self,
            slot: u32,
            keycode: u32,
            modifiers: u32,
        ) -> Result<usize, HotkeyError> {
            register_hotkey(slot, keycode, modifiers)
        }

        fn unregister(&mut self, handle: usize) -> Result<(), HotkeyError> {
            unregister_hotkey(handle)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle, ThreadId};

    const VK_A: u32 = 0x41;
    const VK_B: u32 = 0x42;
    const VK_C: u32 = 0x43;
    const VK_F13: u32 = 0x7C;
    const VK_F14: u32 = 0x7D;
    const VK_F21: u32 = 0x84; // unmapped on macOS (Carbon maps F1..=F20)

    const CARBON_A: u32 = 0;
    const CARBON_B: u32 = 11;
    const CARBON_C: u32 = 8;
    const CARBON_F13: u32 = 105;
    const CARBON_F14: u32 = 107;

    fn inline_manager() -> (MacosGlobalHotkeyManager, MockMacosHotkeyBackend) {
        let mock = MockMacosHotkeyBackend::new();
        let mgr = MacosGlobalHotkeyManager::with_backend_and_executor(
            Box::new(mock.clone()),
            Arc::new(InlineMainThreadExecutor),
        )
        .expect("mock init");
        (mgr, mock)
    }

    /// Simulates a macOS main thread running an event loop that executes marshaled tasks, while
    /// executing tasks inline when already on that thread (mirroring `NativeMainThreadExecutor`).
    struct ThreadedMainExecutor {
        main_thread_id: ThreadId,
        active: Arc<AtomicBool>,
        tx: Mutex<Option<mpsc::Sender<BoxedTask>>>,
        handle: Mutex<Option<JoinHandle<()>>>,
    }

    impl ThreadedMainExecutor {
        fn start() -> Arc<Self> {
            let (tx, rx) = mpsc::channel::<BoxedTask>();
            let (id_tx, id_rx) = mpsc::channel::<ThreadId>();
            let active = Arc::new(AtomicBool::new(true));
            let active_worker = Arc::clone(&active);
            let handle = thread::Builder::new()
                .name("mock-macos-main".to_owned())
                .spawn(move || {
                    let _ = id_tx.send(thread::current().id());
                    while let Ok(task) = rx.recv() {
                        task();
                    }
                    active_worker.store(false, Ordering::SeqCst);
                })
                .expect("spawn mock main thread");
            let main_thread_id = id_rx.recv().expect("mock main thread id");
            Arc::new(Self {
                main_thread_id,
                active,
                tx: Mutex::new(Some(tx)),
                handle: Mutex::new(Some(handle)),
            })
        }

        fn stop(&self) {
            self.active.store(false, Ordering::SeqCst);
            let _ = lock(&self.tx).take();
            if let Some(handle) = lock(&self.handle).take()
                && handle.thread().id() != thread::current().id()
            {
                let _ = handle.join();
            }
        }
    }

    impl Drop for ThreadedMainExecutor {
        fn drop(&mut self) {
            self.stop();
        }
    }

    impl MainThreadExecutor for ThreadedMainExecutor {
        fn run_sync(&self, f: BoxedTask) -> Result<(), HotkeyError> {
            if thread::current().id() == self.main_thread_id {
                f();
                return Ok(());
            }
            if !self.active.load(Ordering::SeqCst) {
                return Err(HotkeyError::Thread(
                    "mock main-thread event loop is not running".to_owned(),
                ));
            }
            let (done_tx, done_rx) = mpsc::channel::<()>();
            let sender = lock(&self.tx).clone();
            let Some(sender) = sender else {
                return Err(HotkeyError::Stopped);
            };
            sender
                .send(Box::new(move || {
                    f();
                    let _ = done_tx.send(());
                }))
                .map_err(|_| HotkeyError::Stopped)?;
            done_rx.recv().map_err(|_| HotkeyError::Stopped)
        }

        fn run_cleanup(&self, f: BoxedTask) {
            if thread::current().id() == self.main_thread_id {
                f();
                return;
            }
            if let Some(sender) = lock(&self.tx).clone() {
                let _ = sender.send(f);
            }
        }
    }

    /// Executor simulating a background thread with no active main-thread event loop (e.g.
    /// `cargo test` worker threads on macOS CI).
    struct InactiveMainExecutor;

    impl MainThreadExecutor for InactiveMainExecutor {
        fn run_sync(&self, _f: BoxedTask) -> Result<(), HotkeyError> {
            Err(HotkeyError::Thread(
                "no active main-thread event loop".to_owned(),
            ))
        }

        fn run_cleanup(&self, _f: BoxedTask) {}
    }

    /// Executor that reports the main loop stopped mid-flight.
    struct StoppedMainExecutor;

    impl MainThreadExecutor for StoppedMainExecutor {
        fn run_sync(&self, _f: BoxedTask) -> Result<(), HotkeyError> {
            Err(HotkeyError::Stopped)
        }

        fn run_cleanup(&self, _f: BoxedTask) {}
    }

    #[test]
    fn macos_global_hotkey_manager_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MacosGlobalHotkeyManager>();
        assert_send_sync::<MockMacosHotkeyBackend>();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn new_off_macos_returns_unsupported() {
        assert_eq!(
            MacosGlobalHotkeyManager::new().unwrap_err(),
            HotkeyError::Unsupported
        );
    }

    #[test]
    fn register_unregister_and_re_register_lifecycle() {
        let (mgr, mock) = inline_manager();
        assert!(mock.is_initialized());
        assert!(mgr.registered_ids().is_empty());

        mgr.register(
            20,
            HotkeyModifiers::WIN | HotkeyModifiers::SHIFT | HotkeyModifiers::NO_REPEAT,
            VK_C,
            false,
        )
        .unwrap();
        mgr.register(
            10,
            HotkeyModifiers::ALT | HotkeyModifiers::CONTROL,
            VK_A,
            false,
        )
        .unwrap();

        assert!(mgr.is_registered(10));
        assert!(mgr.is_registered(20));
        assert_eq!(mgr.registered_ids(), [10, 20]);

        let held = mock.hotkeys();
        assert_eq!(held.len(), 2);
        assert_eq!(
            (held[0].keycode, held[0].modifiers),
            (CARBON_C, CARBON_CMD_KEY | CARBON_SHIFT_KEY)
        );
        assert_eq!(
            (held[1].keycode, held[1].modifiers),
            (CARBON_A, CARBON_OPTION_KEY | CARBON_CONTROL_KEY)
        );

        mgr.unregister(10).unwrap();
        assert!(!mgr.is_registered(10));
        assert_eq!(mgr.registered_ids(), [20]);
        assert_eq!(mock.hotkeys().len(), 1);

        // Re-registering the freed combination succeeds.
        mgr.register(
            10,
            HotkeyModifiers::ALT | HotkeyModifiers::CONTROL,
            VK_A,
            false,
        )
        .unwrap();
        assert_eq!(mgr.registered_ids(), [10, 20]);
    }

    #[test]
    fn bookkeeping_and_unmapped_key_errors_leave_state_unchanged() {
        let (mgr, mock) = inline_manager();
        mgr.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();

        assert_eq!(
            mgr.register(1, HotkeyModifiers::CONTROL, VK_B, false),
            Err(HotkeyError::IdInUse(1))
        );
        assert_eq!(mgr.unregister(99), Err(HotkeyError::NotRegistered(99)));

        // Unmapped virtual keys (0xFF and VK_F21) fail with Carbon invalid-hotkey OS error
        // without touching the backend or existing registrations.
        for bad_vk in [0xFF, VK_F21] {
            let err = mgr
                .register(2, HotkeyModifiers::ALT, bad_vk, false)
                .unwrap_err();
            assert_eq!(
                err,
                HotkeyError::Os {
                    code: CARBON_EVENT_HOT_KEY_INVALID_ERR
                }
            );
            assert!(!err.is_already_registered());
            assert!(!mgr.is_registered(2));
        }

        // Replacing an existing id with an unmapped key preserves the old registration untouched.
        let err = mgr
            .register(1, HotkeyModifiers::ALT, VK_F21, true)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: CARBON_EVENT_HOT_KEY_INVALID_ERR
            }
        );
        assert!(mgr.is_registered(1));
        assert_eq!(mock.hotkeys().len(), 1);
        assert_eq!(mock.hotkeys()[0].keycode, CARBON_A);
    }

    #[test]
    fn conflict_reports_carbon_exists_err_and_is_already_registered() {
        let (mgr, mock) = inline_manager();
        mgr.register(10, HotkeyModifiers::ALT, VK_F13, false)
            .unwrap();
        assert_eq!(mock.hotkeys()[0].keycode, CARBON_F13);

        // Same combination under another id: Carbon reports eventHotKeyExistsErr (-9878).
        let err = mgr
            .register(11, HotkeyModifiers::ALT, VK_F13, false)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR
            }
        );
        assert_eq!(err.os_code(), Some((-9878_i32) as u32));
        #[cfg(target_os = "macos")]
        assert!(err.is_already_registered());
        assert!(!mgr.is_registered(11));

        // Combination held by another application.
        mock.occupy(CARBON_F14, CARBON_CONTROL_KEY);
        let err = mgr
            .register(12, HotkeyModifiers::CONTROL, VK_F14, false)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR
            }
        );
        #[cfg(target_os = "macos")]
        assert!(err.is_already_registered());
        assert!(!mgr.is_registered(12));
        assert_eq!(mgr.registered_ids(), [10]);
    }

    #[test]
    fn replace_moves_combination_and_rolls_back_on_conflict() {
        let (mgr, mock) = inline_manager();
        let hits = Arc::new(Mutex::new(Vec::<u32>::new()));
        let hits_for_handler = Arc::clone(&hits);
        mgr.set_handler(Some(Arc::new(move |id| {
            lock(&hits_for_handler).push(id);
        })));

        // Free id: plain registration.
        mgr.register(40, HotkeyModifiers::ALT, VK_A, true).unwrap();
        assert_eq!(mgr.registered_ids(), [40]);
        let old_slot_a = mock.hotkeys()[0].slot;

        // Move id 40 from A to B: A is released, B is held.
        mgr.register(40, HotkeyModifiers::ALT, VK_B, true).unwrap();
        assert_eq!(mgr.registered_ids(), [40]);
        assert!(!mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert!(!mock.press_slot(old_slot_a));
        assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits), [40]);

        // Move id 40 onto an occupied combination C: fails with eventHotKeyExistsErr, and rolls
        // back to B so B still fires id 40.
        mock.occupy(CARBON_C, CARBON_OPTION_KEY);
        let err = mgr
            .register(40, HotkeyModifiers::ALT, VK_C, true)
            .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR
            }
        );
        #[cfg(target_os = "macos")]
        assert!(err.is_already_registered());
        assert!(mgr.is_registered(40));
        assert_eq!(mock.hotkeys().len(), 1);
        assert_eq!(mock.hotkeys()[0].keycode, CARBON_B);
        assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits), [40, 40]);

        // Double failure (new combo fails AND rollback fails): returns the NEW combination's error
        // and leaves id 40 unregistered.
        let new_err = HotkeyError::Os {
            code: HotkeyError::CARBON_EVENT_HOT_KEY_EXISTS_ERR,
        };
        let rollback_err = HotkeyError::Os {
            code: (-50_i32) as u32,
        };
        mock.push_register_outcome(Some(new_err.clone()));
        mock.push_register_outcome(Some(rollback_err));
        let err = mgr
            .register(40, HotkeyModifiers::ALT, VK_A, true)
            .unwrap_err();
        assert_eq!(err, new_err);
        assert!(!mgr.is_registered(40));
        assert!(mock.hotkeys().is_empty());
    }

    #[test]
    fn drop_unregisters_all_hotkeys_and_releases_handler() {
        let (mgr, mock) = inline_manager();
        let sentinel = Arc::new(());
        let sentinel_in_handler = Arc::clone(&sentinel);
        mgr.set_handler(Some(Arc::new(move |_| {
            let _keep = &sentinel_in_handler;
        })));
        mgr.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();
        mgr.register(2, HotkeyModifiers::ALT, VK_B, false).unwrap();
        let slot_a = mock.hotkeys()[0].slot;
        assert_eq!(mock.hotkeys().len(), 2);
        assert_eq!(Arc::strong_count(&sentinel), 2);

        drop(mgr);
        assert!(mock.hotkeys().is_empty());
        assert_eq!(Arc::strong_count(&sentinel), 1);
        assert!(!mock.press_slot(slot_a));
    }

    #[test]
    fn press_delivery_ignores_unknown_unregistered_and_cleared_handler() {
        let (mgr, mock) = inline_manager();
        let hits = Arc::new(Mutex::new(Vec::<u32>::new()));
        let hits_clone = Arc::clone(&hits);

        mgr.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();
        mgr.register(2, HotkeyModifiers::ALT, VK_B, false).unwrap();
        let slot_a = mock.hotkeys()[0].slot;

        // Press before any handler is set: routed cleanly, ignored.
        assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert!(lock(&hits).is_empty());

        mgr.set_handler(Some(Arc::new(move |id| lock(&hits_clone).push(id))));
        assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
        assert!(!mock.press_slot(u32::MAX));
        assert_eq!(*lock(&hits), [1, 2]);

        // After unregistering id 1, stale presses for its slot are dropped.
        mgr.unregister(1).unwrap();
        assert!(!mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert!(!mock.press_slot(slot_a));
        assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits), [1, 2, 2]);

        // Clearing the handler stops further deliveries.
        mgr.set_handler(None);
        assert!(mock.press(CARBON_B, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits), [1, 2, 2]);
    }

    #[test]
    fn handler_may_unregister_and_register_reentrantly() {
        let (mgr, mock) = inline_manager();
        let mgr = Arc::new(mgr);
        let weak_cell: Arc<OnceLock<Weak<MacosGlobalHotkeyManager>>> = Arc::default();
        let results = Arc::new(Mutex::new(Vec::<Result<(), HotkeyError>>::new()));

        mgr.register(3, HotkeyModifiers::ALT, VK_A, false).unwrap();
        let weak_for_handler = Arc::clone(&weak_cell);
        let results_for_handler = Arc::clone(&results);
        mgr.set_handler(Some(Arc::new(move |id| {
            let m = weak_for_handler.get().unwrap().upgrade().unwrap();
            let r1 = m.unregister(id);
            let r2 = m.register(4, HotkeyModifiers::ALT, VK_B, false);
            let mut log = lock(&results_for_handler);
            log.push(r1);
            log.push(r2);
        })));
        weak_cell.set(Arc::downgrade(&mgr)).unwrap();

        assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert_eq!(*lock(&results), [Ok(()), Ok(())]);
        assert!(!mgr.is_registered(3));
        assert!(mgr.is_registered(4));
    }

    #[test]
    fn dropping_last_manager_reference_inside_handler_does_not_deadlock() {
        let (mgr, mock) = inline_manager();
        let mgr = Arc::new(mgr);
        mgr.register(4, HotkeyModifiers::ALT, VK_A, false).unwrap();

        let held = Mutex::new(Some(Arc::clone(&mgr)));
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        mgr.set_handler(Some(Arc::new(move |_| {
            drop(lock(&held).take());
            lock(&tx).send(()).unwrap();
        })));
        drop(mgr);

        // Handler holds the sole remaining reference and drops it mid-delivery.
        assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));
        assert_eq!(rx.try_recv(), Ok(()));
        // The handler closure (and its sender) was freed once delivery finished.
        assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
        assert!(mock.hotkeys().is_empty());
    }

    #[test]
    fn panicking_handler_is_caught_and_stops_manager() {
        let (mgr, mock) = inline_manager();
        mgr.register(5, HotkeyModifiers::ALT, VK_A, false).unwrap();
        mgr.set_handler(Some(Arc::new(|_| {
            panic!("handler panic (expected by this test)");
        })));

        // Must not unwind out of route_press / mock.press.
        assert!(mock.press(CARBON_A, CARBON_OPTION_KEY));

        assert_eq!(
            mgr.register(6, HotkeyModifiers::ALT, VK_B, false),
            Err(HotkeyError::Stopped)
        );
        assert_eq!(mgr.unregister(5), Err(HotkeyError::Stopped));
        assert!(mgr.registered_ids().is_empty());
        assert!(mock.hotkeys().is_empty());
    }

    #[test]
    fn background_threads_marshal_to_main_thread_and_drop_cleans_up() {
        let executor = ThreadedMainExecutor::start();
        let main_tid = executor.main_thread_id;
        assert_ne!(thread::current().id(), main_tid);

        let mock = MockMacosHotkeyBackend::new();
        let mgr = Arc::new(
            MacosGlobalHotkeyManager::with_backend_and_executor(
                Box::new(mock.clone()),
                Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
            )
            .expect("init marshaled to main thread"),
        );

        let handles: Vec<_> = [(60, VK_A), (61, VK_B), (62, VK_C)]
            .into_iter()
            .map(|(id, vk)| {
                let mgr = Arc::clone(&mgr);
                thread::spawn(move || mgr.register(id, HotkeyModifiers::ALT, vk, false))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert_eq!(mgr.registered_ids(), [60, 61, 62]);

        // Unregister from another background thread.
        let mgr_bg = Arc::clone(&mgr);
        thread::spawn(move || mgr_bg.unregister(61))
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(mgr.registered_ids(), [60, 62]);

        // Drop from a background thread; cleanup is dispatched onto the mock main thread.
        let mgr_drop = Arc::try_unwrap(mgr).expect("sole owner");
        thread::spawn(move || drop(mgr_drop)).join().unwrap();

        // Stop joins the mock main thread after draining all posted cleanup tasks.
        executor.stop();
        assert!(mock.hotkeys().is_empty());

        // Every single backend call ran on the mock main thread, never on caller threads.
        let threads = mock.op_threads();
        assert!(!threads.is_empty());
        assert!(
            threads.iter().all(|&tid| tid == main_tid),
            "all backend ops must run on main thread {main_tid:?}, saw {threads:?}"
        );
    }

    #[test]
    fn background_thread_without_main_loop_returns_thread_error_without_deadlock() {
        let mock = MockMacosHotkeyBackend::new();
        let err = MacosGlobalHotkeyManager::with_backend_and_executor(
            Box::new(mock.clone()),
            Arc::new(InactiveMainExecutor),
        )
        .unwrap_err();
        assert!(matches!(err, HotkeyError::Thread(_)), "{err:?}");
        assert!(!mock.is_initialized());

        // Also verify register/unregister when the main loop stops after construction.
        let executor = ThreadedMainExecutor::start();
        let mgr = MacosGlobalHotkeyManager::with_backend_and_executor(
            Box::new(mock.clone()),
            Arc::clone(&executor) as Arc<dyn MainThreadExecutor>,
        )
        .unwrap();
        mgr.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();
        executor.stop();

        let reg_err = mgr
            .register(2, HotkeyModifiers::ALT, VK_B, false)
            .unwrap_err();
        assert!(matches!(reg_err, HotkeyError::Thread(_)), "{reg_err:?}");
        let unreg_err = mgr.unregister(1).unwrap_err();
        assert!(matches!(unreg_err, HotkeyError::Thread(_)), "{unreg_err:?}");
        // Drop must not hang or panic when the main loop is gone.
        drop(mgr);
    }

    #[test]
    fn stopped_executor_reports_stopped() {
        let mock = MockMacosHotkeyBackend::new();
        let err = MacosGlobalHotkeyManager::with_backend_and_executor(
            Box::new(mock),
            Arc::new(StoppedMainExecutor),
        )
        .unwrap_err();
        assert_eq!(err, HotkeyError::Stopped);
    }

    #[test]
    fn failed_initialize_surfaces_os_error() {
        let mock = MockMacosHotkeyBackend::new();
        mock.fail_initialize(HotkeyError::Os {
            code: (-50_i32) as u32,
        });
        let err = MacosGlobalHotkeyManager::with_backend_and_executor(
            Box::new(mock),
            Arc::new(InlineMainThreadExecutor),
        )
        .unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: (-50_i32) as u32
            }
        );
    }

    #[test]
    fn separate_managers_route_independently() {
        let (a, mock_a) = inline_manager();
        let (b, mock_b) = inline_manager();
        let hits_a = Arc::new(Mutex::new(Vec::<u32>::new()));
        let hits_b = Arc::new(Mutex::new(Vec::<u32>::new()));
        let ha = Arc::clone(&hits_a);
        let hb = Arc::clone(&hits_b);
        a.set_handler(Some(Arc::new(move |id| lock(&ha).push(id))));
        b.set_handler(Some(Arc::new(move |id| lock(&hb).push(id))));

        a.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();
        b.register(1, HotkeyModifiers::ALT, VK_A, false).unwrap();

        assert!(mock_a.press(CARBON_A, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits_a), [1]);
        assert!(lock(&hits_b).is_empty());

        drop(a);
        assert!(mock_b.press(CARBON_A, CARBON_OPTION_KEY));
        assert_eq!(*lock(&hits_b), [1]);
    }

    #[test]
    fn cocoa_and_global_managers_share_slot_router_without_collision() {
        use crate::hotkey::PlatformHotkeyManager;
        use crate::hotkey::cocoa_hotkey::{CocoaHotkeyManager, MockHotkeyBackend};

        let cocoa_mock = MockHotkeyBackend::new();
        let mut cocoa = CocoaHotkeyManager::with_backend(Box::new(cocoa_mock.clone()));
        let (global, global_mock) = inline_manager();
        let global_hits = Arc::new(Mutex::new(Vec::<u32>::new()));
        let hits_for_global = Arc::clone(&global_hits);
        global.set_handler(Some(Arc::new(move |id| lock(&hits_for_global).push(id))));

        cocoa
            .register_hotkey(1, HotkeyModifiers::ALT, VK_A)
            .unwrap();
        global
            .register(1, HotkeyModifiers::ALT, VK_B, false)
            .unwrap();

        let cocoa_slot = cocoa_mock.hotkeys()[0].slot;
        let global_slot = global_mock.hotkeys()[0].slot;
        assert_ne!(cocoa_slot, global_slot);

        // Global slot routes exclusively to `MacosGlobalHotkeyManager`.
        assert!(get_cocoa_route(global_slot).is_none());
        assert!(route_press(global_slot));
        assert_eq!(*lock(&global_hits), [1]);

        // Cocoa slot routes exclusively to `CocoaHotkeyManager` and does not hit `global`.
        let cocoa_route = get_cocoa_route(cocoa_slot).expect("cocoa route present");
        assert_eq!(cocoa_route.id, 1);
        assert!(route_press(cocoa_slot));
        assert_eq!(*lock(&global_hits), [1]);

        // Unregistering `cocoa` clears only `cocoa_slot`; `global_slot` continues to deliver.
        cocoa.unregister_hotkey(1).unwrap();
        assert!(get_cocoa_route(cocoa_slot).is_none());
        assert!(!route_press(cocoa_slot));
        assert!(route_press(global_slot));
        assert_eq!(*lock(&global_hits), [1, 1]);

        // Dropping `global` clears `global_slot`.
        drop(global);
        assert!(!route_press(global_slot));
    }
}
