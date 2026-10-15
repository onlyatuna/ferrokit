// SPDX-License-Identifier: LGPL-3.0-only
use crate::signal::Signal;
use std::sync::atomic::{AtomicBool, Ordering};

pub type SocketDescriptor = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketEvent {
    Read,
    Exception,
}

pub struct SocketNotifier {
    descriptor: SocketDescriptor,
    event_type: SocketEvent,
    enabled: AtomicBool,
    activated: Signal<SocketDescriptor>,
}

impl SocketNotifier {
    pub fn new(descriptor: SocketDescriptor, event_type: SocketEvent) -> Self {
        Self {
            descriptor,
            event_type,
            enabled: AtomicBool::new(true),
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

    #[inline]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
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
