// SPDX-License-Identifier: LGPL-3.0-only
//! macOS Carbon back end for [`GlobalHotkeyManager`](crate::global_hotkey::GlobalHotkeyManager).
//!
//! Carbon's `RegisterEventHotKey` / `UnregisterEventHotKey` target `GetApplicationEventTarget()`
//! and must run on the OS main thread. [`MacosGlobalHotkeyManager`] preserves
//! [`GlobalHotkeyManager`](crate::global_hotkey::GlobalHotkeyManager)'s `Send + Sync` contract by
//! routing Carbon operations through a [`MainThreadExecutor`]:
//!
//! - Calls made **on the OS main thread** execute Carbon calls directly in place (so startup
//!   registration before `CoreApplication::exec()`, re-entrant calls from inside a hotkey handler,
//!   and `Drop` on the main thread never deadlock).
//! - Calls made **from a background thread** marshal onto the OS main thread via
//!   `ferrokit-core`'s main-thread event loop when `CoreApplication::in_exec()` is active; when no
//!   main-thread event loop is running (for example on `cargo test` worker threads), they fail
//!   immediately with [`HotkeyError::Thread`] rather than panicking or deadlocking.
//!
//! Everything that touches Carbon goes through `MacosHotkeyBackend` and `MainThreadExecutor`,
//! so the registration lifecycle, `replace()` rollback, conflict detection
//! (`eventHotKeyExistsErr` = `-9878`), press routing, panic containment, and cross-thread
//! marshaling are unit-tested on every OS with `MockMacosHotkeyBackend`.

use crate::hotkey::cocoa_hotkey::{carbon_keycode, carbon_modifiers};
use crate::hotkey::{HotkeyError, HotkeyModifiers};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};

/// Carbon `eventHotKeyInvalidErr` (`-9879`) encoded as `u32` for [`HotkeyError::Os`].
const CARBON_EVENT_HOT_KEY_INVALID_ERR: u32 = (-9879_i32) as u32;

type Handler = Arc<dyn Fn(u32) + Send + Sync>;
type BoxedTask = Box<dyn FnOnce() + Send>;

/// Low-level macOS hotkey operations executed on the OS main thread.
///
/// On macOS the production implementation calls Carbon's `InstallEventHandler`,
/// `RegisterEventHotKey`, and `UnregisterEventHotKey`. Tests inject `MockMacosHotkeyBackend` so
/// the full manager runs on any OS.
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
trait MainThreadExecutor: Send + Sync {
    /// Runs `f` synchronously on the main thread (inline when already on the main thread, or
    /// marshaled when on another thread with an active main-thread event loop).
    fn run_sync(&self, f: BoxedTask) -> Result<(), HotkeyError>;

    /// Runs `f` on the main thread during `Drop` / shutdown cleanup (inline on the main thread,
    /// or best-effort asynchronous dispatch from another thread so `Drop` never deadlocks).
    fn run_cleanup(&self, f: BoxedTask);
}

#[derive(Clone, Copy)]
struct Registration {
    slot: u32,
    handle: usize,
    keycode: u32,
    modifiers: u32,
}

struct State {
    backend: Box<dyn MacosHotkeyBackend>,
    stopped: bool,
    hotkeys: HashMap<u32, Registration>,
    handler: Option<Handler>,
}

struct Inner {
    state: Mutex<State>,
    executor: Arc<dyn MainThreadExecutor>,
}

struct Route {
    manager: Weak<Inner>,
    id: u32,
}

static ROUTES: LazyLock<Mutex<HashMap<u32, Route>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_SLOT: AtomicU32 = AtomicU32::new(1);

// Every critical section is a handful of map/flag operations, so the data stays consistent even
// after a panic elsewhere.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn routes() -> MutexGuard<'static, HashMap<u32, Route>> {
    lock(&ROUTES)
}

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

/// Routes a Carbon `kEventHotKeyPressed` (or mock press) for `slot` to the registered manager's
/// handler. Returns whether `slot` belonged to a live registration.
///
/// No lock is held while the user's handler runs, so the handler may call `register`, `replace`,
/// `unregister`, or drop the manager in place. Panics in the handler are caught (never unwinding
/// into Carbon's C stack) and stop the manager after unregistering its hotkeys, matching the
/// Windows backend.
pub(crate) fn route_press(slot: u32) -> bool {
    let target = {
        let route_map = routes();
        route_map
            .get(&slot)
            .map(|r| (Weak::clone(&r.manager), r.id))
    };
    let Some((weak_inner, id)) = target else {
        return false;
    };
    let Some(inner) = weak_inner.upgrade() else {
        routes().remove(&slot);
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
pub(crate) struct MacosGlobalHotkeyManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MacosGlobalHotkeyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosGlobalHotkeyManager")
            .field("registered_ids", &self.registered_ids())
            .finish_non_exhaustive()
    }
}

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
                routes().remove(&old_reg.slot);
                state.hotkeys.remove(&id);
                let _ = state.backend.unregister(old_reg.handle);
            }

            let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
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
                        Route {
                            manager: Arc::downgrade(&inner),
                            id,
                        },
                    );
                    Ok(())
                }
                Err(err) => {
                    if let Some(old_reg) = old {
                        let rollback_slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
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
                                Route {
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
            routes().remove(&reg.slot);
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
    use super::*;
    use ferrokit_core::application::CoreApplication;
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::event_loop::{get_thread_event_sender, post_event_to_thread};
    use ferrokit_core::object::{ObjectId, ThreadContext};
    use std::ffi::c_int;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    const MAIN_LOOP_POLL_INTERVAL: Duration = Duration::from_millis(50);

    pub(super) fn is_os_main_thread() -> bool {
        unsafe extern "C" {
            fn pthread_main_np() -> c_int;
        }
        // SAFETY: `pthread_main_np` takes no arguments, touches no caller memory, and is
        // documented by Apple libSystem as thread-safe. It returns > 0 only on the process's
        // initial (main) thread (0 on other threads, -1 before initialization).
        unsafe { pthread_main_np() > 0 }
    }

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
mod carbon {
    use super::native_executor::is_os_main_thread;
    use super::*;
    use crate::hotkey::cocoa_hotkey::carbon::{
        EVENT_CLASS_KEYBOARD, EVENT_HOT_KEY_PRESSED, EVENT_NOT_HANDLED_ERR,
        EVENT_PARAM_DIRECT_OBJECT, EventHandlerCallRef, EventHandlerRef, EventHotKeyId,
        EventHotKeyRef, EventRef, EventTypeSpec, GetApplicationEventTarget, GetEventParameter,
        InstallEventHandler, NO_ERR, OSStatus, RegisterEventHotKey, TYPE_EVENT_HOT_KEY_ID,
        UnregisterEventHotKey,
    };
    use std::ffi::c_void;

    /// Distinct from `CocoaHotkeyManager`'s `'FKIT'` signature so both managers coexist cleanly on
    /// `GetApplicationEventTarget()`.
    const SIGNATURE: u32 = u32::from_be_bytes(*b"FKGH");

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

    fn ensure_handler_installed() -> Result<(), HotkeyError> {
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

        fn unregister(&mut self, handle: usize) -> Result<(), HotkeyError> {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::cocoa_hotkey::{
        CARBON_CMD_KEY, CARBON_CONTROL_KEY, CARBON_OPTION_KEY, CARBON_SHIFT_KEY,
    };
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
}
