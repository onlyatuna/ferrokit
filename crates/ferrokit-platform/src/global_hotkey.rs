// SPDX-License-Identifier: LGPL-3.0-only
//! Application-wide hotkeys that need no window of their own.
//!
//! [`GlobalHotkeyManager`] owns a worker thread (Windows: a thread with a message queue, to which
//! `RegisterHotKey(NULL, ...)` binds the hotkeys), so an application that has no `HWND`, or whose
//! GUI thread does not pump a ferrokit event loop, can still register hotkeys, learn why the
//! system refused one, and be told when one is pressed.
//!
//! Delivery: the handler set with [`GlobalHotkeyManager::set_handler`] runs **on the manager's
//! worker thread**, never on the thread that registered the hotkey. It is the equivalent of a
//! `Qt::DirectConnection` from a thread other than the receiver's: forward the press to the GUI
//! thread yourself (for example with `run_on_main_thread`) when it touches GUI state. The handler
//! may call [`register`](GlobalHotkeyManager::register) and
//! [`unregister`](GlobalHotkeyManager::unregister) on the manager (they then run in place, on the
//! worker thread). A press the system queued for a hotkey just before it was unregistered is
//! dropped if the id is still unregistered when the worker reaches it; if the id was registered
//! again (or replaced) by then, the stale press is delivered for the new registration.
//!
//! Registering or unregistering from another thread waits for the worker thread, which is busy
//! while the handler runs: do not call them while holding something the handler waits for, or
//! the two deadlock.
//!
//! Only Windows has a back end here; elsewhere [`GlobalHotkeyManager::new`] fails with
//! [`HotkeyError::Unsupported`] (macOS: use [`CocoaHotkeyManager`](crate::hotkey::CocoaHotkeyManager),
//! which delivers on the owner thread's event loop).

use crate::hotkey::{HotkeyError, HotkeyModifiers};

/// Registers system-wide hotkeys from a worker thread that the manager owns.
///
/// `Send + Sync`: share it behind an `Arc`. Dropping the manager unregisters every hotkey, stops
/// the worker thread and waits for it to exit (nothing outlives the manager). Drop it from the
/// handler itself and the thread is left to finish on its own instead, since a thread cannot wait
/// for itself.
///
/// `vk` is a Windows virtual-key code on every platform (as with
/// [`PlatformHotkeyManager`](crate::hotkey::PlatformHotkeyManager)); include
/// [`HotkeyModifiers::NO_REPEAT`] to be told once per press instead of once per auto-repeat.
pub struct GlobalHotkeyManager {
    imp: imp::Manager,
}

impl std::fmt::Debug for GlobalHotkeyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobalHotkeyManager")
            .field("registered_ids", &self.registered_ids())
            .finish_non_exhaustive()
    }
}

impl GlobalHotkeyManager {
    /// Starts the worker thread. Returns once it is ready for requests.
    pub fn new() -> Result<Self, HotkeyError> {
        imp::Manager::new().map(|imp| Self { imp })
    }

    /// Registers `id` for the combination `modifiers` + `vk`.
    ///
    /// Fails with [`HotkeyError::IdInUse`] if this manager already holds `id`, and with
    /// [`HotkeyError::Os`] (carrying the system error code, e.g.
    /// [`HotkeyError::ERROR_HOTKEY_ALREADY_REGISTERED`]) if the system refuses the combination.
    /// A failed registration changes nothing.
    pub fn register(
        &self,
        id: u32,
        modifiers: HotkeyModifiers,
        vk: u32,
    ) -> Result<(), HotkeyError> {
        self.imp.register(id, modifiers, vk, false)
    }

    /// Gives `id` a new combination (registers it if it was free). The old combination is released
    /// first (an id cannot be held twice), so if the system then refuses the new one, the old one
    /// is registered again. That second registration can itself fail (another program may have
    /// taken the combination in between): the error returned is then still the new
    /// combination's, and `id` is left unregistered, as [`is_registered`](Self::is_registered)
    /// shows.
    pub fn replace(&self, id: u32, modifiers: HotkeyModifiers, vk: u32) -> Result<(), HotkeyError> {
        self.imp.register(id, modifiers, vk, true)
    }

    /// Releases `id` ([`HotkeyError::NotRegistered`] if the manager does not hold it). The
    /// combination is free again for this or another program once this returns.
    pub fn unregister(&self, id: u32) -> Result<(), HotkeyError> {
        self.imp.unregister(id)
    }

    /// Whether this manager currently holds `id`.
    pub fn is_registered(&self, id: u32) -> bool {
        self.imp.is_registered(id)
    }

    /// Ids this manager currently holds, ascending.
    pub fn registered_ids(&self) -> Vec<u32> {
        self.imp.registered_ids()
    }

    /// Sets the function called with the id of a hotkey when it is pressed, on the worker thread
    /// (see the module documentation). Replaces any earlier handler. A panic in the handler ends
    /// the worker thread: its hotkeys are released and later requests fail with
    /// [`HotkeyError::Stopped`].
    pub fn set_handler(&self, handler: impl Fn(u32) + Send + Sync + 'static) {
        self.imp.set_handler(Some(std::sync::Arc::new(handler)));
    }

    /// Removes the handler; presses are then ignored.
    pub fn clear_handler(&self) {
        self.imp.set_handler(None);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    use std::sync::Arc;

    pub(super) struct Manager;

    impl Manager {
        pub(super) fn new() -> Result<Self, HotkeyError> {
            Err(HotkeyError::Unsupported)
        }

        pub(super) fn register(
            &self,
            _id: u32,
            _modifiers: HotkeyModifiers,
            _vk: u32,
            _replace: bool,
        ) -> Result<(), HotkeyError> {
            Err(HotkeyError::Unsupported)
        }

        pub(super) fn unregister(&self, _id: u32) -> Result<(), HotkeyError> {
            Err(HotkeyError::Unsupported)
        }

        pub(super) fn is_registered(&self, _id: u32) -> bool {
            false
        }

        pub(super) fn registered_ids(&self) -> Vec<u32> {
            Vec::new()
        }

        pub(super) fn set_handler(&self, _handler: Option<Arc<dyn Fn(u32) + Send + Sync>>) {}
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{mpsc, Arc, Mutex, MutexGuard};
    use std::thread::{self, JoinHandle, ThreadId};
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY, WM_QUIT,
    };

    type Handler = Arc<dyn Fn(u32) + Send + Sync>;

    /// Posted to the worker thread: there are requests in the queue.
    const WM_WAKE: u32 = WM_APP;

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
        /// Set when the worker is gone: nothing queued after this is ever answered.
        closed: bool,
        requests: VecDeque<Request>,
    }

    #[derive(Default)]
    struct Shared {
        queue: Mutex<Queue>,
        /// What the worker thread has registered. Written only by the worker thread (or by the
        /// caller running in place on it); read from anywhere.
        hotkeys: Mutex<HashMap<u32, (HotkeyModifiers, u32)>>,
        handler: Mutex<Option<Handler>>,
    }

    // Every critical section is a few map/queue operations, so the data is valid after a panic.
    fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn os_register(id: u32, modifiers: HotkeyModifiers, vk: u32) -> Result<(), HotkeyError> {
        // SAFETY: by-value arguments only, no pointers; the NULL window is permitted by the
        // RegisterHotKey documentation and binds the hotkey to the calling thread, which is the
        // worker (`os_register` is reached only via `run_op`, see its doc). Failure is reported
        // through the return value.
        if unsafe { RegisterHotKey(std::ptr::null_mut(), id as i32, modifiers.bits(), vk) } != 0 {
            Ok(())
        } else {
            // Read before anything else can overwrite it.
            Err(HotkeyError::Os {
                // SAFETY: `GetLastError` takes no arguments and reads the calling thread's error
                // slot; it runs immediately after the failed `RegisterHotKey` on the same thread.
                code: unsafe { GetLastError() },
            })
        }
    }

    fn os_unregister(id: u32) -> Result<(), HotkeyError> {
        // SAFETY: by-value arguments only, no pointers; NULL window as at registration, so this
        // targets the calling worker thread's hotkeys (`os_unregister` runs only on the worker or
        // in `ExitGuard::drop` on it). An unknown id merely makes the call fail.
        if unsafe { UnregisterHotKey(std::ptr::null_mut(), id as i32) } != 0 {
            Ok(())
        } else {
            Err(HotkeyError::Os {
                // SAFETY: `GetLastError` takes no arguments and reads the calling thread's error
                // slot; it runs immediately after the failed `UnregisterHotKey` on the same thread.
                code: unsafe { GetLastError() },
            })
        }
    }

    /// Runs one request. Only ever called on the worker thread.
    fn run_op(shared: &Shared, op: Op) -> Result<(), HotkeyError> {
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
                if old.is_some() {
                    // The same id cannot be held twice: the old combination goes first.
                    let _ = os_unregister(id);
                    hotkeys.remove(&id);
                }
                match os_register(id, modifiers, vk) {
                    Ok(()) => {
                        hotkeys.insert(id, (modifiers, vk));
                        Ok(())
                    }
                    Err(e) => {
                        if let Some((old_mods, old_vk)) = old {
                            if os_register(id, old_mods, old_vk).is_ok() {
                                hotkeys.insert(id, (old_mods, old_vk));
                            }
                        }
                        Err(e)
                    }
                }
            }
            Op::Unregister { id } => {
                if hotkeys.remove(&id).is_none() {
                    return Err(HotkeyError::NotRegistered(id));
                }
                os_unregister(id)
            }
        }
    }

    /// Answers every queued request.
    fn drain(shared: &Shared) {
        loop {
            let request = lock(&shared.queue).requests.pop_front();
            let Some(request) = request else { return };
            // A caller that stopped waiting has dropped its receiver; nothing to tell it.
            let _ = request.reply.send(run_op(shared, request.op));
        }
    }

    /// Delivers a press to the handler, with no lock held (the handler may call back in).
    fn dispatch(shared: &Shared, id: u32) {
        if !lock(&shared.hotkeys).contains_key(&id) {
            return;
        }
        let handler = lock(&shared.handler).clone();
        if let Some(handler) = handler {
            handler(id);
        }
    }

    /// Runs when the worker ends, however it ends: refuses and cancels requests, releases hotkeys.
    struct ExitGuard<'a>(&'a Shared);

    impl Drop for ExitGuard<'_> {
        fn drop(&mut self) {
            // The table first: a caller told `Stopped` below then also sees no registered hotkeys.
            let ids: Vec<u32> = lock(&self.0.hotkeys).drain().map(|(id, _)| id).collect();
            {
                let mut queue = lock(&self.0.queue);
                queue.closed = true;
                // Dropping the requests drops their reply senders: waiting callers get `Stopped`.
                queue.requests.clear();
            }
            for id in ids {
                let _ = os_unregister(id);
            }
        }
    }

    fn worker(shared: Arc<Shared>, ready: mpsc::Sender<u32>) {
        let _exit = ExitGuard(&shared);
        // SAFETY: `MSG` is plain data for which all-zero is valid.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        // SAFETY: `&mut msg` points at the live local `msg` initialized above, and the NULL window
        // plus 0/0 filter are permitted by PeekMessageW. PM_NOREMOVE only makes the system create
        // this thread's message queue, so `PostThreadMessageW` to the id published below cannot
        // fail for want of a queue.
        unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
        // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
        let thread_id = unsafe { GetCurrentThreadId() };
        if ready.send(thread_id).is_err() {
            return;
        }
        loop {
            // SAFETY: `&mut msg` points at the live local `msg` initialized above and is written
            // only during the call; the NULL window means no dispatching is needed.
            let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if ret <= 0 {
                // 0: WM_QUIT (shutdown); -1: the call failed and the queue is unusable.
                break;
            }
            match msg.message {
                WM_WAKE => drain(&shared),
                WM_HOTKEY => dispatch(&shared, msg.wParam as u32),
                _ => {}
            }
        }
    }

    pub(super) struct Manager {
        shared: Arc<Shared>,
        thread_id: u32,
        worker_thread: ThreadId,
        worker: Option<JoinHandle<()>>,
    }

    impl Manager {
        pub(super) fn new() -> Result<Self, HotkeyError> {
            let shared = Arc::new(Shared::default());
            let (ready_tx, ready_rx) = mpsc::channel();
            let worker_shared = Arc::clone(&shared);
            let handle = thread::Builder::new()
                .name("ferrokit-hotkey".to_owned())
                .spawn(move || worker(worker_shared, ready_tx))
                .map_err(|e| HotkeyError::Thread(e.to_string()))?;
            let Ok(thread_id) = ready_rx.recv() else {
                let _ = handle.join();
                return Err(HotkeyError::Thread(
                    "the worker thread ended during start-up".to_owned(),
                ));
            };
            Ok(Self {
                shared,
                thread_id,
                worker_thread: handle.thread().id(),
                worker: Some(handle),
            })
        }

        /// Runs `op` on the worker thread and waits for its result (in place when the caller is
        /// the worker thread, i.e. the handler: it could not wait for itself).
        fn call(&self, op: Op) -> Result<(), HotkeyError> {
            if thread::current().id() == self.worker_thread {
                return run_op(&self.shared, op);
            }
            let (reply, answer) = mpsc::channel();
            {
                // Queued and woken under one lock: the worker takes this lock (setting `closed`)
                // before it can finish, so while `closed` is unset `thread_id` still names our
                // worker and cannot have been reused by an unrelated thread, and the worker
                // cannot take the request before it is woken.
                let mut queue = lock(&self.shared.queue);
                if queue.closed {
                    return Err(HotkeyError::Stopped);
                }
                queue.requests.push_back(Request { op, reply });
                // SAFETY: by-value arguments only. `self.thread_id` names our worker: `closed` is
                // unset (checked above under the queue lock the worker must take before it
                // exits), so the id cannot have been reused, and its queue exists (created by
                // `PeekMessageW` in `worker` before the id was published).
                if unsafe { PostThreadMessageW(self.thread_id, WM_WAKE, 0, 0) } == 0 {
                    // SAFETY: `GetLastError` takes no arguments; called right after the failed post
                    // on the same thread, before anything else can overwrite the error slot.
                    let code = unsafe { GetLastError() };
                    // Nobody will wake the worker for this request, and it must not run later.
                    queue.requests.pop_back();
                    return Err(HotkeyError::Os { code });
                }
            }
            answer.recv().unwrap_or(Err(HotkeyError::Stopped))
        }

        pub(super) fn register(
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

        pub(super) fn unregister(&self, id: u32) -> Result<(), HotkeyError> {
            self.call(Op::Unregister { id })
        }

        pub(super) fn is_registered(&self, id: u32) -> bool {
            lock(&self.shared.hotkeys).contains_key(&id)
        }

        pub(super) fn registered_ids(&self) -> Vec<u32> {
            let mut ids: Vec<u32> = lock(&self.shared.hotkeys).keys().copied().collect();
            ids.sort_unstable();
            ids
        }

        pub(super) fn set_handler(&self, handler: Option<Handler>) {
            *lock(&self.shared.handler) = handler;
        }

        /// Id of the worker thread (tests post `WM_HOTKEY` to it in place of a key press).
        #[cfg(test)]
        pub(super) fn worker_thread_id(&self) -> u32 {
            self.thread_id
        }
    }

    impl Drop for Manager {
        fn drop(&mut self) {
            let Some(worker) = self.worker.take() else {
                return;
            };
            // WM_QUIT ends the worker's loop; it releases the hotkeys on its way out. Posting
            // happens under the queue lock: the worker takes that lock (setting `closed`) before
            // it can finish, so while `closed` is unset the thread id still names our worker and
            // cannot have been reused by an unrelated thread. Once `closed` is set the worker is
            // already leaving and needs no message. A post fails only transiently (full queue).
            loop {
                let queue = lock(&self.shared.queue);
                // SAFETY: by-value arguments only. `queue.closed` is unset in this branch (`||`
                // short-circuits) and the queue lock is held, so the worker is still alive and
                // `thread_id` has not been reused (see the comment above the loop).
                if queue.closed || unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) } != 0
                {
                    break;
                }
                drop(queue);
                thread::yield_now();
            }
            // The handler may be what drops the last reference: a thread cannot join itself, and
            // the worker ends right after the handler returns anyway.
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc::RecvTimeoutError;
        use std::sync::OnceLock;
        use std::time::Duration;

        const VK_F13: u32 = 0x7C;
        const PRESS_TIMEOUT: Duration = Duration::from_secs(10);
        // Not the modifiers of tests/test_global_hotkey.rs: test binaries may run side by side.
        const MODS: HotkeyModifiers = HotkeyModifiers::CONTROL
            .union(HotkeyModifiers::SHIFT)
            .union(HotkeyModifiers::WIN)
            .union(HotkeyModifiers::NO_REPEAT);

        /// A hotkey press, without a key press: what the system posts to the thread.
        fn press(manager: &Manager, id: u32) {
            assert_ne!(
                // SAFETY: by-value arguments only; `worker_thread_id()` is the id of the live worker
                // of `manager`, which the caller keeps alive; a post to a dead thread would only
                // fail and trip the assertion.
                unsafe {
                    PostThreadMessageW(manager.worker_thread_id(), WM_HOTKEY, id as usize, 0)
                },
                0
            );
        }

        #[test]
        fn press_reaches_the_handler_on_the_worker_thread() {
            let m = GlobalHotkeyManager::new().unwrap();
            m.register(1, MODS, VK_F13).unwrap();
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            m.set_handler(move |id| {
                let name = thread::current().name().map(str::to_owned);
                tx.lock().unwrap().send((id, name)).unwrap();
            });
            press(&m.imp, 1);
            let (id, name) = rx.recv_timeout(PRESS_TIMEOUT).unwrap();
            assert_eq!(id, 1);
            assert_eq!(name.as_deref(), Some("ferrokit-hotkey"));
            assert_ne!(thread::current().name(), Some("ferrokit-hotkey"));
        }

        #[test]
        fn a_press_of_an_unregistered_id_is_dropped() {
            let m = GlobalHotkeyManager::new().unwrap();
            m.register(2, MODS, VK_F13 + 1).unwrap();
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            m.set_handler(move |id| tx.lock().unwrap().send(id).unwrap());
            m.register(7, MODS, VK_F13 + 6).unwrap();
            press(&m.imp, 99);
            press(&m.imp, 2);
            // Presses are handled in order, so seeing 2 first means 99 was skipped.
            assert_eq!(rx.recv_timeout(PRESS_TIMEOUT).unwrap(), 2);
            m.unregister(2).unwrap();
            press(&m.imp, 2);
            press(&m.imp, 7);
            // Seeing 7 next means the press of the unregistered 2 before it was skipped.
            assert_eq!(rx.recv_timeout(PRESS_TIMEOUT).unwrap(), 7);
            assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Empty));
        }

        #[test]
        fn the_handler_may_unregister_its_own_hotkey() {
            let m = Arc::new(GlobalHotkeyManager::new().unwrap());
            m.register(3, MODS, VK_F13 + 2).unwrap();
            let cell: Arc<OnceLock<std::sync::Weak<GlobalHotkeyManager>>> = Arc::default();
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let handler_cell = Arc::clone(&cell);
            m.set_handler(move |id| {
                let m = handler_cell.get().unwrap().upgrade().unwrap();
                let result = m.unregister(id);
                tx.lock().unwrap().send(result).unwrap();
            });
            cell.set(Arc::downgrade(&m)).unwrap();
            press(&m.imp, 3);
            assert_eq!(rx.recv_timeout(PRESS_TIMEOUT).unwrap(), Ok(()));
            assert!(!m.is_registered(3));
        }

        #[test]
        fn dropping_the_last_reference_in_the_handler_ends_the_worker() {
            let m = Arc::new(GlobalHotkeyManager::new().unwrap());
            m.register(4, MODS, VK_F13 + 3).unwrap();
            let thread_id = m.imp.worker_thread_id();
            let held = Mutex::new(Some(Arc::clone(&m)));
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            m.set_handler(move |_| {
                // The test's reference is gone, so this is the last one: the manager is dropped on
                // its own worker thread, which must not wait for itself.
                drop(held.lock().unwrap().take());
                tx.lock().unwrap().send(()).unwrap();
            });
            drop(m);
            // SAFETY: by-value arguments only; `thread_id` is the live worker of `m`'s manager,
            // kept alive by `held` (moved into the handler) until the handler runs. A failing post
            // would only trip the assertion.
            assert_ne!(unsafe { PostThreadMessageW(thread_id, WM_HOTKEY, 4, 0) }, 0);
            rx.recv_timeout(PRESS_TIMEOUT).unwrap();
            // The handler (and the sender in it) is freed when the worker thread, the last owner
            // of the shared state, ends.
            assert_eq!(
                rx.recv_timeout(PRESS_TIMEOUT),
                Err(RecvTimeoutError::Disconnected)
            );
        }

        #[test]
        fn a_panicking_handler_stops_the_manager() {
            let m = GlobalHotkeyManager::new().unwrap();
            m.register(5, MODS, VK_F13 + 4).unwrap();
            m.set_handler(|_| panic!("handler panic (expected by this test)"));
            press(&m.imp, 5);
            // The system does not order a hotkey message against posted requests, so a request may
            // still be answered before the worker reaches the press: ask until it is dead.
            let deadline = std::time::Instant::now() + PRESS_TIMEOUT;
            loop {
                match m.register(6, MODS, VK_F13 + 5) {
                    Err(HotkeyError::Stopped) => break,
                    // The worker may die between the two calls; the loop then ends on `Stopped`.
                    Ok(()) => drop(m.unregister(6)),
                    Err(e) => panic!("{e:?}"),
                }
                assert!(std::time::Instant::now() < deadline, "worker still running");
                thread::yield_now();
            }
            assert_eq!(m.unregister(5), Err(HotkeyError::Stopped));
            assert!(m.registered_ids().is_empty());
        }
    }
}
