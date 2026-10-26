// SPDX-License-Identifier: LGPL-3.0-only
//! Pluggable synchronization primitives for Loom, Shuttle, and standard execution.
//!
//! `thread_local!` is re-exported too: both Loom (`loom::thread_local!`, `LocalKey::try_with`,
//! locals dropped when a model thread ends) and Shuttle (`shuttle::thread_local!`, same API,
//! destructors run on the thread before it counts as finished) ship a mock of it. The mocks
//! differ from `std` in two ways the local-slot model has to live with: the initializer is a plain
//! expression (no `const { .. }` form), and Loom drops *all* of a thread's locals at once, so
//! during a destructor every other mock local of that thread already reads as destroyed. Loom's
//! `JoinHandle::join` also returns *before* the joined thread's mock locals are dropped (std and
//! Shuttle drop them first), so a model that must see the end of a thread-local teardown cannot
//! rely on `join`; `local_slots` signals it with a `Condvar` instead.

#[cfg(feature = "loom")]
pub mod sys {
    pub use loom::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use loom::thread;
    pub use loom::thread_local;

    pub mod atomic {
        pub use loom::sync::atomic::{AtomicU64, Ordering};
    }
}

#[cfg(feature = "shuttle")]
pub mod sys {
    pub use shuttle::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use shuttle::thread;
    pub use shuttle::thread_local;

    pub mod atomic {
        pub use shuttle::sync::atomic::{AtomicU64, Ordering};
    }
}

#[cfg(not(any(feature = "loom", feature = "shuttle")))]
pub mod sys {
    pub use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
    pub use std::thread;
    pub use std::thread_local;

    pub mod atomic {
        pub use std::sync::atomic::{AtomicU64, Ordering};
    }
}
