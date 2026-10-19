// SPDX-License-Identifier: LGPL-3.0-only
//! Pluggable synchronization primitives for Loom, Shuttle, and standard execution.

#[cfg(feature = "loom")]
pub mod sys {
    pub use loom::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use loom::thread;
}

#[cfg(feature = "shuttle")]
pub mod sys {
    pub use shuttle::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use shuttle::thread;
}

#[cfg(not(any(feature = "loom", feature = "shuttle")))]
pub mod sys {
    pub use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use std::thread;
}
