// SPDX-License-Identifier: LGPL-3.0-only
use crate::signal::Signal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub type SocketDescriptor = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketEvent {
    Read,
    Exception,
}

/// Called after a notifier is enabled or disabled, so the dispatcher that polls its descriptor
/// can stop or resume polling it (`QSocketNotifier::setEnabled` → `registerSocketNotifier` /
/// `unregisterSocketNotifier`).
pub(crate) type EnableHook = Arc<dyn Fn() + Send + Sync>;

/// Why [`EventDispatcher::register_socket_notifier`](crate::event_loop::EventDispatcher::register_socket_notifier)
/// refused a notifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketNotifierError {
    /// The dispatcher never polls descriptors, so the notifier would never be activated by its
    /// descriptor: only the Linux event dispatcher does (with `epoll`). `dispatcher` is the
    /// dispatcher's [`class_name`](crate::event_loop::EventDispatcher::class_name).
    Unsupported { dispatcher: &'static str },
}

impl std::fmt::Display for SocketNotifierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported { dispatcher } => write!(
                f,
                "socket notifiers are not supported: the event dispatcher `{dispatcher}` does \
                 not poll descriptors"
            ),
        }
    }
}

impl std::error::Error for SocketNotifierError {}

/// Watches a descriptor for readiness (`QSocketNotifier`).
///
/// Platform support: only the Linux event dispatcher polls the descriptor (with `epoll`) and
/// emits `activated` when it is ready. On Windows, macOS and other Unix systems
/// [`EventDispatcher::register_socket_notifier`](crate::event_loop::EventDispatcher::register_socket_notifier)
/// fails with [`SocketNotifierError::Unsupported`] instead of registering a notifier that would
/// never be activated by the descriptor itself.
pub struct SocketNotifier {
    descriptor: SocketDescriptor,
    event_type: SocketEvent,
    enabled: AtomicBool,
    enable_hook: Mutex<Option<EnableHook>>,
    activated: Signal<SocketDescriptor>,
}

impl SocketNotifier {
    pub fn new(descriptor: SocketDescriptor, event_type: SocketEvent) -> Self {
        Self {
            descriptor,
            event_type,
            enabled: AtomicBool::new(true),
            enable_hook: Mutex::new(None),
            activated: Signal::new(),
        }
    }

    #[inline]
    pub fn descriptor(&self) -> SocketDescriptor {
        self.descriptor
    }

    #[inline]
    pub fn event_type(&self) -> SocketEvent {
        self.event_type
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        // Cloned out so the hook runs without the lock: it takes the dispatcher's locks.
        let hook = self
            .enable_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Sets the hook the dispatcher that polls this notifier uses to follow `set_enabled`.
    pub(crate) fn set_enable_hook(&self, hook: Option<EnableHook>) {
        *self
            .enable_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = hook;
    }

    #[inline]
    pub fn activated(&self) -> &Signal<SocketDescriptor> {
        &self.activated
    }

    pub fn notify(&self) {
        if self.is_enabled() {
            self.activated.emit(&self.descriptor);
        }
    }
}
