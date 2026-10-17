// SPDX-License-Identifier: LGPL-3.0-only
//! Poison-tolerant locking for the process-global and per-thread registries that object
//! destruction, event-loop teardown and thread-affinity bookkeeping must always be able to reach.
//!
//! Unregistering an object (`unregister_qobject`), dropping an `OwnedObject` and dropping an
//! `EventLoop` cannot return an error. If one of them panicked because an earlier panic had
//! poisoned a registry lock, the object would already be gone from the registry but never freed,
//! and every later destruction would panic the same way.
//!
//! Recovering with [`PoisonError::into_inner`](std::sync::PoisonError::into_inner) is only
//! correct when the guarded data is structurally valid after a panic with the guard held. These
//! helpers are therefore used only for locks whose data is a plain container (`HashMap`, `Vec`,
//! a queue of events) changed by single-step operations (insert, remove, retain, push, store),
//! or by steps that run no user code between the first and the last change. For such data the
//! poison flag carries no information, so it is cleared after recovery and later lockers, outside
//! the crate included, see a healthy lock.
//!
//! Do not use them for data with an invariant that spans several steps with user code in
//! between: there a poisoned lock must be reported, or the user code must run without the lock.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Locks `mutex`, recovering the data when a previous holder panicked.
pub(crate) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        mutex.clear_poison();
        poisoned.into_inner()
    })
}

/// Takes a read lock on `lock`, recovering the data when a previous writer panicked.
pub(crate) fn read_unpoisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| {
        lock.clear_poison();
        poisoned.into_inner()
    })
}

/// Takes a write lock on `lock`, recovering the data when a previous writer panicked.
pub(crate) fn write_unpoisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|poisoned| {
        lock.clear_poison();
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_mutex_poisoned_by_a_panicking_holder_is_recovered_and_healed() {
        let mutex = Arc::new(Mutex::new(vec![1, 2, 3]));
        let holder = Arc::clone(&mutex);
        let result = std::thread::spawn(move || {
            let mut data = holder.lock().unwrap();
            data.push(4);
            panic!("holder panics with the lock held");
        })
        .join();
        assert!(result.is_err());
        assert!(mutex.is_poisoned());

        assert_eq!(*lock_unpoisoned(&mutex), vec![1, 2, 3, 4]);
        assert!(!mutex.is_poisoned(), "recovery clears the poison flag");
        assert_eq!(mutex.lock().unwrap().len(), 4);
    }

    #[test]
    fn an_rwlock_poisoned_by_a_panicking_writer_is_recovered_for_read_and_write() {
        let lock = Arc::new(RwLock::new(10_u32));
        let holder = Arc::clone(&lock);
        let result = std::thread::spawn(move || {
            let mut data = holder.write().unwrap();
            *data = 11;
            panic!("writer panics with the lock held");
        })
        .join();
        assert!(result.is_err());
        assert!(lock.is_poisoned());

        assert_eq!(*read_unpoisoned(&lock), 11);
        assert!(!lock.is_poisoned());

        let holder = Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let _data = holder.write().unwrap();
            panic!("second poisoning");
        })
        .join();
        *write_unpoisoned(&lock) += 1;
        assert_eq!(*lock.read().unwrap(), 12);
    }
}
