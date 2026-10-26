// SPDX-License-Identifier: LGPL-3.0-only
use crate::event::{NativeEventFilter, NativeEventFilterChain, NativeMessage};
use crate::timer::{TimerEntry, TimerRegistry};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchResult {
    Quit(i32),
    Awoken,
    Timeout,
    Normal,
}

impl DispatchResult {
    pub fn is_quit(&self) -> bool {
        matches!(self, DispatchResult::Quit(_))
    }

    pub fn is_awoken(&self) -> bool {
        matches!(self, DispatchResult::Awoken)
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self, DispatchResult::Timeout)
    }
}

pub trait EventDispatcherHandle: Send + Sync {
    fn wake_up(&self);
}

/// The thread-owned half of an event loop: implementations are not required to be `Send` or
/// `Sync` (the Win32 one owns a window that only its creating thread may destroy). Other threads
/// wake the loop through an [`EventDispatcherHandle`] from [`clone_handle`](Self::clone_handle).
pub trait EventDispatcher {
    fn wake_up(&self);
    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle>;
    fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>);
    fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool;
    fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult;
    fn register_timer(&self, entry: &TimerEntry);
    fn unregister_timer(&self, entry: &TimerEntry);
    /// Delivers due timers. Implementations lock `registry` only around bookkeeping, never while
    /// the timer event is sent, so a slot may stop or restart timers.
    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>);
    /// Starts polling the notifier's descriptor so it fires `activated` when the descriptor is
    /// ready (`QAbstractEventDispatcher::registerSocketNotifier`).
    ///
    /// Real descriptor polling exists on Linux only (an `epoll` set in the Unix dispatcher). A
    /// dispatcher that cannot poll descriptors, which is every other platform (Windows, macOS and
    /// the other Unix systems) and the default implementation, registers nothing and fails with
    /// [`SocketNotifierError::Unsupported`](crate::event_loop::SocketNotifierError::Unsupported),
    /// so a caller does not wait for an activation that never comes.
    fn register_socket_notifier(
        &self,
        _notifier: &Arc<crate::event_loop::SocketNotifier>,
    ) -> Result<(), crate::event_loop::SocketNotifierError> {
        Err(crate::event_loop::SocketNotifierError::Unsupported {
            dispatcher: self.class_name(),
        })
    }
    /// Stops polling the notifier's descriptor; a no-op for a notifier that
    /// [`register_socket_notifier`](Self::register_socket_notifier) did not register.
    fn unregister_socket_notifier(&self, _notifier: &Arc<crate::event_loop::SocketNotifier>) {}
    /// The concrete dispatcher's type name, like Qt's
    /// the class name of the thread's `QAbstractEventDispatcher` instance.
    fn class_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Fallback dispatcher: the loop thread parks, and [`wake_up`](EventDispatcher::wake_up) sets a flag
/// and unparks it. Timers live in the loop's registry and are delivered by `send_timer_events`.
#[derive(Debug)]
pub struct GenericEventDispatcher {
    wake_up_flag: Arc<AtomicBool>,
    /// The thread that runs `process_events`: the one that created the dispatcher.
    thread: std::thread::Thread,
    native_filters: NativeEventFilterChain,
}

impl Default for GenericEventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl GenericEventDispatcher {
    pub fn new() -> Self {
        Self {
            wake_up_flag: Arc::new(AtomicBool::new(false)),
            thread: std::thread::current(),
            native_filters: NativeEventFilterChain::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenericEventDispatcherHandle {
    wake_up_flag: Arc<AtomicBool>,
    thread: std::thread::Thread,
}

impl EventDispatcherHandle for GenericEventDispatcherHandle {
    fn wake_up(&self) {
        self.wake_up_flag.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

impl EventDispatcher for GenericEventDispatcher {
    fn wake_up(&self) {
        self.wake_up_flag.store(true, Ordering::Release);
        self.thread.unpark();
    }

    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle> {
        Arc::new(GenericEventDispatcherHandle {
            wake_up_flag: Arc::clone(&self.wake_up_flag),
            thread: self.thread.clone(),
        })
    }

    fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>) {
        self.native_filters.install(filter);
    }

    fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool {
        self.native_filters.filter_native(event_type, msg, result)
    }

    fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult {
        if self.wake_up_flag.swap(false, Ordering::AcqRel) {
            return DispatchResult::Awoken;
        }

        if can_wait {
            if let Some(timeout) = next_timer_timeout {
                if timeout.is_zero() {
                    return DispatchResult::Timeout;
                }
                std::thread::park_timeout(timeout);
            } else {
                std::thread::park();
            }

            if self.wake_up_flag.swap(false, Ordering::AcqRel) {
                DispatchResult::Awoken
            } else {
                DispatchResult::Timeout
            }
        } else {
            DispatchResult::Normal
        }
    }

    fn register_timer(&self, _entry: &TimerEntry) {}

    fn unregister_timer(&self, _entry: &TimerEntry) {}

    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        crate::timer::deliver_due_timers(registry, crate::timer::current_time_ms(), []);
    }
}

/// Which creation path an event loop comes from, which decides its dispatcher, as in Qt:
/// `Core` is `QThreadPrivate::createEventDispatcher` (worker threads and `QCoreApplication`,
/// qthread_unix.cpp:316-325, qcoreapplication.cpp:518-523); `Gui` is the platform plugin's
/// dispatcher that `QGuiApplication` asks for (qguiapplication.cpp:1629-1643). The thread does
/// not matter. Only macOS has a separate GUI dispatcher in ferrokit (Cocoa); elsewhere both kinds get
/// the platform's one dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatcherKind {
    Core,
    Gui,
}

#[cfg(windows)]
pub type DefaultEventDispatcher = super::dispatcher_win::Win32EventDispatcher;

#[cfg(target_os = "linux")]
pub type DefaultEventDispatcher = super::dispatcher_unix::UnixEventDispatcher;

/// macOS: `Core` loops use the UNIX dispatcher (Qt's `QEventDispatcherUNIX`), `Gui` loops the
/// Cocoa one (`QCocoaEventDispatcher`, qcocoaintegration.mm:353-356).
#[cfg(target_os = "macos")]
pub type DefaultEventDispatcher = DarwinEventDispatcher;

#[cfg(target_os = "macos")]
pub enum DarwinEventDispatcher {
    Cocoa(super::dispatcher_cocoa::CocoaEventDispatcher),
    Unix(super::dispatcher_unix::UnixEventDispatcher),
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
pub enum DarwinEventDispatcherHandle {
    Cocoa(super::dispatcher_cocoa::CocoaEventDispatcherHandle),
    Unix(super::dispatcher_unix::UnixEventDispatcherHandle),
}

#[cfg(target_os = "macos")]
impl EventDispatcherHandle for DarwinEventDispatcherHandle {
    fn wake_up(&self) {
        match self {
            Self::Cocoa(h) => h.wake_up(),
            Self::Unix(h) => h.wake_up(),
        }
    }
}

#[cfg(target_os = "macos")]
impl DarwinEventDispatcher {
    pub fn new(kind: DispatcherKind) -> Self {
        match kind {
            DispatcherKind::Gui => {
                Self::Cocoa(super::dispatcher_cocoa::CocoaEventDispatcher::new())
            }
            DispatcherKind::Core => Self::Unix(super::dispatcher_unix::UnixEventDispatcher::new()),
        }
    }

    pub fn clone_handle(&self) -> DarwinEventDispatcherHandle {
        match self {
            Self::Cocoa(d) => DarwinEventDispatcherHandle::Cocoa(d.clone_handle()),
            Self::Unix(d) => DarwinEventDispatcherHandle::Unix(d.clone_handle()),
        }
    }

    fn inner(&self) -> &dyn EventDispatcher {
        match self {
            Self::Cocoa(d) => d,
            Self::Unix(d) => d,
        }
    }
}

#[cfg(target_os = "macos")]
impl EventDispatcher for DarwinEventDispatcher {
    fn wake_up(&self) {
        self.inner().wake_up();
    }
    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle> {
        Arc::new(DarwinEventDispatcher::clone_handle(self))
    }
    fn install_native_event_filter(&self, filter: Box<dyn NativeEventFilter>) {
        self.inner().install_native_event_filter(filter);
    }
    fn filter_native_event(
        &self,
        event_type: &str,
        msg: &NativeMessage,
        result: &mut isize,
    ) -> bool {
        self.inner().filter_native_event(event_type, msg, result)
    }
    fn process_events(
        &self,
        can_wait: bool,
        next_timer_timeout: Option<Duration>,
    ) -> DispatchResult {
        self.inner().process_events(can_wait, next_timer_timeout)
    }
    fn register_timer(&self, entry: &TimerEntry) {
        self.inner().register_timer(entry);
    }
    fn unregister_timer(&self, entry: &TimerEntry) {
        self.inner().unregister_timer(entry);
    }
    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        self.inner().send_timer_events(registry);
    }
    fn register_socket_notifier(
        &self,
        notifier: &Arc<crate::event_loop::SocketNotifier>,
    ) -> Result<(), crate::event_loop::SocketNotifierError> {
        self.inner().register_socket_notifier(notifier)
    }
    fn unregister_socket_notifier(&self, notifier: &Arc<crate::event_loop::SocketNotifier>) {
        self.inner().unregister_socket_notifier(notifier);
    }
    fn class_name(&self) -> &'static str {
        self.inner().class_name()
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub type DefaultEventDispatcher = GenericEventDispatcher;

pub fn create_dispatcher(kind: DispatcherKind) -> DefaultEventDispatcher {
    #[cfg(target_os = "macos")]
    return DarwinEventDispatcher::new(kind);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        DefaultEventDispatcher::new()
    }
}

/// Qt picks the dispatcher by how the loop is created, not by the calling thread. Darwin
/// `QThreadPrivate::createEventDispatcher` (qthread_unix.cpp:316-325) returns `QEventDispatcherUNIX`
/// for every thread with no main-thread check, and `QCoreApplication` uses that path too
/// (qcoreapplication.cpp:518-523); only `QGuiApplication` asks the platform plugin
/// (qguiapplication.cpp:1629-1643), which on macOS creates `QCocoaEventDispatcher`.
#[cfg(all(test, target_os = "macos"))]
mod darwin_tests {
    use super::{DarwinEventDispatcher, DispatcherKind};
    use crate::event_loop::EventLoop;
    use crate::object::ThreadContext;

    #[test]
    fn a_core_loop_on_the_main_thread_uses_the_unix_dispatcher() {
        ThreadContext::init_current(true, None);
        let el = EventLoop::new();
        let unix = matches!(el.dispatcher(), DarwinEventDispatcher::Unix(_));
        drop(el);
        ThreadContext::clear_current();
        assert!(
            unix,
            "a core event loop must use the UNIX dispatcher even on the main thread"
        );
    }

    #[test]
    fn a_worker_loop_uses_the_unix_dispatcher() {
        let unix = std::thread::spawn(|| {
            let el = EventLoop::new();
            matches!(el.dispatcher(), DarwinEventDispatcher::Unix(_))
        })
        .join()
        .unwrap();
        assert!(unix, "a worker event loop must use the UNIX dispatcher");
    }

    #[test]
    fn a_gui_loop_uses_the_cocoa_dispatcher() {
        let el = EventLoop::with_dispatcher_kind(DispatcherKind::Gui);
        assert!(matches!(el.dispatcher(), DarwinEventDispatcher::Cocoa(_)));
    }
}

#[cfg(test)]
mod socket_notifier_tests {
    use super::{EventDispatcher, GenericEventDispatcher};
    use crate::event_loop::{SocketEvent, SocketNotifier, SocketNotifierError};
    use std::sync::Arc;

    /// A dispatcher that does not poll descriptors refuses a notifier, and unregistering the
    /// refused one is harmless.
    #[test]
    fn test_a_dispatcher_without_descriptor_polling_refuses_a_socket_notifier() {
        let dispatcher = GenericEventDispatcher::new();
        let notifier = Arc::new(SocketNotifier::new(7, SocketEvent::Read));

        assert_eq!(
            dispatcher.register_socket_notifier(&notifier),
            Err(SocketNotifierError::Unsupported {
                dispatcher: dispatcher.class_name()
            })
        );
        dispatcher.unregister_socket_notifier(&notifier);
    }

    #[test]
    fn test_the_unsupported_error_names_the_dispatcher() {
        let error = SocketNotifierError::Unsupported {
            dispatcher: "SomeDispatcher",
        };
        assert!(error.to_string().contains("SomeDispatcher"));
    }
}
