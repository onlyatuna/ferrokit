// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, LockResult, Mutex as StdMutex, RwLock as StdRwLock, TryLockError};
use std::thread::{current as current_thread, ThreadId as StdThreadId};
use std::time::{Duration, Instant};

// =============================================================================
// 1. Mutex<T> with Timed Locking (Qt QMutex equivalent)
// =============================================================================

/// An enhanced mutual exclusion primitive supporting timed and bounded locking,
/// modeled after Qt's `QMutex`.
pub struct Mutex<T: ?Sized> {
    cond: Condvar,
    inner: StdMutex<bool>, // true = locked, false = unlocked
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for Mutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state.
    pub const fn new(val: T) -> Self {
        Self {
            cond: Condvar::new(),
            inner: StdMutex::new(false),
            data: UnsafeCell::new(val),
        }
    }

    /// Consumes the mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        Ok(self.data.into_inner())
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires the mutex, blocking the current thread until it is available.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        let mut locked = self.inner.lock().unwrap();
        while *locked {
            locked = self.cond.wait(locked).unwrap();
        }
        *locked = true;
        Ok(MutexGuard { mutex: self })
    }

    /// Attempts to acquire the mutex without blocking.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let mut locked = self.inner.try_lock().ok()?;
        if !*locked {
            *locked = true;
            Some(MutexGuard { mutex: self })
        } else {
            None
        }
    }

    /// Attempts to acquire the mutex, waiting at most `timeout`.
    pub fn try_lock_for(&self, timeout: Duration) -> Option<MutexGuard<'_, T>> {
        self.try_lock_until(Instant::now() + timeout)
    }

    /// Attempts to acquire the mutex, waiting until `deadline`.
    pub fn try_lock_until(&self, deadline: Instant) -> Option<MutexGuard<'_, T>> {
        let mut locked = self.inner.lock().unwrap();
        while *locked {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let remaining = deadline - now;
            let (next_locked, timeout_res) = self.cond.wait_timeout(locked, remaining).unwrap();
            locked = next_locked;
            if timeout_res.timed_out() && *locked {
                return None;
            }
        }
        *locked = true;
        Some(MutexGuard { mutex: self })
    }

    /// Returns a mutable reference to the underlying data.
    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        Ok(self.data.get_mut())
    }
}

/// An RAII guard returned by [`Mutex::lock`], unlocking on drop.
pub struct MutexGuard<'a, T: ?Sized> {
    mutex: &'a Mutex<T>,
}

unsafe impl<T: ?Sized + Sync> Sync for MutexGuard<'_, T> {}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        let mut locked = self.mutex.inner.lock().unwrap();
        *locked = false;
        self.mutex.cond.notify_one();
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("Mutex");
        if let Some(guard) = self.try_lock() {
            d.field("data", &&*guard);
        } else {
            d.field("data", &format_args!("<locked>"));
        }
        d.finish()
    }
}

// =============================================================================
// 2. RecursiveMutex<T> (Qt QRecursiveMutex equivalent)
// =============================================================================

struct RecursiveState {
    owner: Option<StdThreadId>,
    count: usize,
}

/// A re-entrant mutual exclusion primitive allowing the same thread to acquire
/// the lock multiple times, modeled after Qt's `QRecursiveMutex`.
pub struct RecursiveMutex<T: ?Sized> {
    state: StdMutex<RecursiveState>,
    cond: Condvar,
    data: UnsafeCell<T>,
}

unsafe impl<T: ?Sized + Send> Send for RecursiveMutex<T> {}
unsafe impl<T: ?Sized + Send> Sync for RecursiveMutex<T> {}

impl<T> RecursiveMutex<T> {
    /// Creates a new recursive mutex.
    pub const fn new(val: T) -> Self {
        Self {
            state: StdMutex::new(RecursiveState {
                owner: None,
                count: 0,
            }),
            cond: Condvar::new(),
            data: UnsafeCell::new(val),
        }
    }

    /// Consumes the mutex, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        Ok(self.data.into_inner())
    }
}

impl<T: ?Sized> RecursiveMutex<T> {
    /// Acquires the recursive mutex. Re-entrant calls on the same thread succeed immediately.
    pub fn lock(&self) -> RecursiveMutexGuard<'_, T> {
        let current_id = current_thread().id();
        let mut state = self.state.lock().unwrap();

        loop {
            match state.owner {
                None => {
                    state.owner = Some(current_id);
                    state.count = 1;
                    return RecursiveMutexGuard { mutex: self };
                }
                Some(owner) if owner == current_id => {
                    state.count += 1;
                    return RecursiveMutexGuard { mutex: self };
                }
                Some(_) => {
                    state = self.cond.wait(state).unwrap();
                }
            }
        }
    }

    /// Attempts to acquire the recursive mutex without blocking.
    pub fn try_lock(&self) -> Option<RecursiveMutexGuard<'_, T>> {
        let current_id = current_thread().id();
        let mut state = self.state.try_lock().ok()?;

        match state.owner {
            None => {
                state.owner = Some(current_id);
                state.count = 1;
                Some(RecursiveMutexGuard { mutex: self })
            }
            Some(owner) if owner == current_id => {
                state.count += 1;
                Some(RecursiveMutexGuard { mutex: self })
            }
            Some(_) => None,
        }
    }

    /// Attempts to acquire the recursive mutex, waiting at most `timeout`.
    pub fn try_lock_for(&self, timeout: Duration) -> Option<RecursiveMutexGuard<'_, T>> {
        self.try_lock_until(Instant::now() + timeout)
    }

    /// Attempts to acquire the recursive mutex, waiting until `deadline`.
    pub fn try_lock_until(&self, deadline: Instant) -> Option<RecursiveMutexGuard<'_, T>> {
        let current_id = current_thread().id();
        let mut state = self.state.lock().unwrap();

        loop {
            match state.owner {
                None => {
                    state.owner = Some(current_id);
                    state.count = 1;
                    return Some(RecursiveMutexGuard { mutex: self });
                }
                Some(owner) if owner == current_id => {
                    state.count += 1;
                    return Some(RecursiveMutexGuard { mutex: self });
                }
                Some(_) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return None;
                    }
                    let remaining = deadline - now;
                    let (next_state, timeout_res) =
                        self.cond.wait_timeout(state, remaining).unwrap();
                    state = next_state;
                    if timeout_res.timed_out()
                        && state.owner != Some(current_id)
                        && state.owner.is_some()
                    {
                        return None;
                    }
                }
            }
        }
    }
}

/// An RAII guard returned by [`RecursiveMutex::lock`].
pub struct RecursiveMutexGuard<'a, T: ?Sized> {
    mutex: &'a RecursiveMutex<T>,
}

unsafe impl<T: ?Sized + Sync> Sync for RecursiveMutexGuard<'_, T> {}

impl<T: ?Sized> Deref for RecursiveMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T: ?Sized> DerefMut for RecursiveMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T: ?Sized> Drop for RecursiveMutexGuard<'_, T> {
    fn drop(&mut self) {
        let mut state = self.mutex.state.lock().unwrap();
        debug_assert!(state.count > 0);
        state.count -= 1;
        if state.count == 0 {
            state.owner = None;
            self.mutex.cond.notify_one();
        }
    }
}

// =============================================================================
// 3. RwLock<T> (Qt QReadWriteLock equivalent)
// =============================================================================

/// A read-write lock supporting multiple readers or a single writer with timed operations,
/// modeled after Qt's `QReadWriteLock`.
pub struct RwLock<T: ?Sized> {
    inner: StdRwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new read-write lock.
    pub const fn new(val: T) -> Self {
        Self {
            inner: StdRwLock::new(val),
        }
    }

    /// Consumes the RwLock, returning the underlying data.
    pub fn into_inner(self) -> LockResult<T> {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Locks with shared read access.
    pub fn read(&self) -> LockResult<std::sync::RwLockReadGuard<'_, T>> {
        self.inner.read()
    }

    /// Attempts to lock with shared read access.
    pub fn try_read(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, T>, TryLockError<std::sync::RwLockReadGuard<'_, T>>>
    {
        self.inner.try_read()
    }

    /// Locks with exclusive write access.
    pub fn write(&self) -> LockResult<std::sync::RwLockWriteGuard<'_, T>> {
        self.inner.write()
    }

    /// Attempts to lock with exclusive write access.
    pub fn try_write(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, T>, TryLockError<std::sync::RwLockWriteGuard<'_, T>>>
    {
        self.inner.try_write()
    }

    /// Attempts to lock for read access within a timeout.
    pub fn try_read_for(&self, timeout: Duration) -> Option<std::sync::RwLockReadGuard<'_, T>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(guard) = self.try_read() {
                return Some(guard);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::yield_now();
        }
    }

    /// Attempts to lock for write access within a timeout.
    pub fn try_write_for(&self, timeout: Duration) -> Option<std::sync::RwLockWriteGuard<'_, T>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(guard) = self.try_write() {
                return Some(guard);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::yield_now();
        }
    }
}

// =============================================================================
// 4. Semaphore (Qt QSemaphore equivalent)
// =============================================================================

/// A counting semaphore providing access control for shared resources,
/// modeled after Qt's `QSemaphore`.
pub struct Semaphore {
    permits: StdMutex<usize>,
    cond: Condvar,
}

impl Semaphore {
    /// Creates a new semaphore with `initial_permits`.
    pub const fn new(initial_permits: usize) -> Self {
        Self {
            permits: StdMutex::new(initial_permits),
            cond: Condvar::new(),
        }
    }

    /// Acquires `n` permits, blocking until available.
    pub fn acquire(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut available = self.permits.lock().unwrap();
        while *available < n {
            available = self.cond.wait(available).unwrap();
        }
        *available -= n;
    }

    /// Attempts to acquire `n` permits without blocking.
    pub fn try_acquire(&self, n: usize) -> bool {
        if n == 0 {
            return true;
        }
        let mut available = self.permits.lock().unwrap();
        if *available >= n {
            *available -= n;
            true
        } else {
            false
        }
    }

    /// Attempts to acquire `n` permits within `timeout`.
    pub fn try_acquire_for(&self, n: usize, timeout: Duration) -> bool {
        if n == 0 {
            return true;
        }
        let deadline = Instant::now() + timeout;
        let mut available = self.permits.lock().unwrap();

        while *available < n {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            let (next_available, timeout_res) =
                self.cond.wait_timeout(available, remaining).unwrap();
            available = next_available;
            if timeout_res.timed_out() && *available < n {
                return false;
            }
        }
        *available -= n;
        true
    }

    /// Releases `n` permits back to the semaphore.
    pub fn release(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut available = self.permits.lock().unwrap();
        *available += n;
        self.cond.notify_all();
    }

    /// Returns the number of currently available permits.
    pub fn available(&self) -> usize {
        *self.permits.lock().unwrap()
    }
}

// =============================================================================
// 5. WaitCondition (Qt QWaitCondition equivalent)
// =============================================================================

/// A condition variable used to synchronize threads, modeled after Qt's `QWaitCondition`.
pub struct WaitCondition {
    gate: StdMutex<()>,
    cond: Condvar,
}

impl Default for WaitCondition {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitCondition {
    /// Creates a new wait condition.
    pub const fn new() -> Self {
        Self {
            gate: StdMutex::new(()),
            cond: Condvar::new(),
        }
    }

    /// Atomically releases `guard`, waits until signaled, and re-acquires the lock upon waking.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        let mutex = guard.mutex;
        let gate_lock = self.gate.lock().unwrap();

        // Release mutex
        {
            let mut locked = mutex.inner.lock().unwrap();
            *locked = false;
            mutex.cond.notify_one();
        }
        drop(guard);

        // Sleep on condition variable
        let _gate = self.cond.wait(gate_lock).unwrap();

        // Re-acquire mutex
        mutex.lock().unwrap()
    }

    /// Waits on the condition for at most `timeout`. Returns the re-acquired guard and whether signaled.
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        timeout: Duration,
    ) -> (Option<MutexGuard<'a, T>>, bool) {
        let mutex = guard.mutex;
        let gate_lock = self.gate.lock().unwrap();

        // Release mutex
        {
            let mut locked = mutex.inner.lock().unwrap();
            *locked = false;
            mutex.cond.notify_one();
        }
        drop(guard);

        let (_gate_lock, timeout_res) = self.cond.wait_timeout(gate_lock, timeout).unwrap();
        let signaled = !timeout_res.timed_out();
        let acquired = mutex.try_lock_for(timeout);
        (acquired, signaled)
    }

    /// Wakes one waiting thread.
    pub fn wake_one(&self) {
        let _gate = self.gate.lock().unwrap();
        self.cond.notify_one();
    }

    /// Wakes all waiting threads.
    pub fn wake_all(&self) {
        let _gate = self.gate.lock().unwrap();
        self.cond.notify_all();
    }
}
