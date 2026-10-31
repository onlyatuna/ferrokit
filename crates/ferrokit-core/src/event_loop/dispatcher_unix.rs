// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::event::{NativeEventFilter, NativeEventFilterChain, NativeMessage};
use crate::event_loop::dispatcher::{DispatchResult, EventDispatcher, EventDispatcherHandle};
use crate::event_loop::socket_notifier::{
    SocketDescriptor, SocketEvent, SocketNotifier, SocketNotifierError,
};
use crate::timer::{TimerEntry, TimerId, TimerRegistry};

pub const EPOLLIN: u32 = 0x001;
pub const EPOLLPRI: u32 = 0x002;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;

#[derive(Debug)]
pub struct EventFd {
    counter: AtomicU64,
}

impl EventFd {
    pub fn new(init: u64) -> Self {
        Self {
            counter: AtomicU64::new(init),
        }
    }

    pub fn write(&self, val: u64) {
        self.counter.fetch_add(val, Ordering::SeqCst);
    }

    pub fn read(&self) -> u64 {
        self.counter.swap(0, Ordering::SeqCst)
    }

    pub fn is_readable(&self) -> bool {
        self.counter.load(Ordering::SeqCst) > 0
    }
}

/// Simulated Linux `timerfd` structure
#[derive(Debug)]
pub struct TimerFd {
    pub id: TimerId,
    pub interval: Duration,
    pub single_shot: bool,
    pub next_fire: Mutex<Option<Instant>>,
    pub expirations: AtomicU64,
}

impl TimerFd {
    pub fn new(id: TimerId, interval: Duration, single_shot: bool) -> Self {
        let next_fire = Instant::now() + interval;
        Self {
            id,
            interval,
            single_shot,
            next_fire: Mutex::new(Some(next_fire)),
            expirations: AtomicU64::new(0),
        }
    }

    pub fn check_and_fire(&self, now: Instant) -> bool {
        let mut next = self.next_fire.lock().unwrap();
        if let Some(target) = *next {
            if now >= target {
                self.expirations.fetch_add(1, Ordering::SeqCst);
                if self.single_shot {
                    *next = None;
                } else {
                    *next = Some(now + self.interval);
                }
                return true;
            }
        }
        false
    }

    pub fn read_expirations(&self) -> u64 {
        self.expirations.swap(0, Ordering::SeqCst)
    }
}

/// The kernel side of the Linux reactor: one `epoll` set that holds every descriptor of an
/// enabled socket notifier and an `eventfd` that wakes a blocked `epoll_wait` (what Qt's
/// `QEventDispatcherUNIX` does with its thread-wakeup eventfd and `select`).
#[cfg(target_os = "linux")]
mod linux_epoll {
    use super::SocketDescriptor;
    use std::os::raw::c_int;

    /// `data` of the wake-up eventfd in the epoll set. A descriptor is never this large.
    const WAKE_TOKEN: u64 = u64::MAX;

    pub struct Poller {
        epoll_fd: c_int,
        wake_fd: c_int,
    }

    impl Poller {
        pub fn new() -> Option<Self> {
            // SAFETY: plain syscalls; every descriptor created here is closed on failure or in
            // `Drop`.
            unsafe {
                let epoll_fd = libc::epoll_create1(libc::EPOLL_CLOEXEC);
                if epoll_fd < 0 {
                    return None;
                }
                let wake_fd = libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK);
                if wake_fd < 0 {
                    libc::close(epoll_fd);
                    return None;
                }
                let mut ev = libc::epoll_event {
                    events: libc::EPOLLIN as u32,
                    u64: WAKE_TOKEN,
                };
                if libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_ADD, wake_fd, &mut ev) < 0 {
                    libc::close(wake_fd);
                    libc::close(epoll_fd);
                    return None;
                }
                Some(Self { epoll_fd, wake_fd })
            }
        }

        /// Makes a blocked or later `wait` return.
        pub fn wake(&self) {
            let one: u64 = 1;
            // SAFETY: writes the 8 bytes of a live local to the eventfd. A full counter
            // (`EAGAIN`) already means "readable", so the result is not needed.
            unsafe {
                libc::write(self.wake_fd, (&one as *const u64).cast(), 8);
            }
        }

        fn drain_wake(&self) {
            let mut value: u64 = 0;
            // SAFETY: reads 8 bytes into a live local; the descriptor is non-blocking.
            unsafe {
                libc::read(self.wake_fd, (&mut value as *mut u64).cast(), 8);
            }
        }

        /// Sets the events reported for `fd`: none removes it from the set.
        pub fn set_interest(&self, fd: SocketDescriptor, events: u32) {
            // SAFETY: `epoll_ctl` only reads `ev`; a descriptor that is invalid or already
            // closed makes it fail, which leaves the set as it was.
            unsafe {
                if events == 0 {
                    libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut());
                    return;
                }
                let mut ev = libc::epoll_event {
                    events,
                    u64: fd as u64,
                };
                if libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_MOD, fd, &mut ev) < 0
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT)
                {
                    libc::epoll_ctl(self.epoll_fd, libc::EPOLL_CTL_ADD, fd, &mut ev);
                }
            }
        }

        /// Blocks for at most `timeout_ms` (0 polls, negative waits forever) until a registered
        /// descriptor is ready or `wake` is called. Returns whether it was woken and the ready
        /// `(descriptor, events)` pairs.
        pub fn wait(&self, timeout_ms: c_int) -> (bool, Vec<(SocketDescriptor, u32)>) {
            let mut events = [libc::epoll_event { events: 0, u64: 0 }; 64];
            // SAFETY: `events` has room for the 64 entries passed as the maximum.
            let count = unsafe {
                libc::epoll_wait(
                    self.epoll_fd,
                    events.as_mut_ptr(),
                    events.len() as c_int,
                    timeout_ms,
                )
            };
            let mut woken = false;
            let mut ready = Vec::new();
            for event in events.iter().take(count.max(0) as usize) {
                // Copied out: the struct is packed on x86-64.
                let (flags, token) = (event.events, event.u64);
                if token == WAKE_TOKEN {
                    woken = true;
                } else {
                    ready.push((token as SocketDescriptor, flags));
                }
            }
            if woken {
                self.drain_wake();
            }
            (woken, ready)
        }
    }

    impl Drop for Poller {
        fn drop(&mut self) {
            // SAFETY: both descriptors are owned by this value and closed once.
            unsafe {
                libc::close(self.wake_fd);
                libc::close(self.epoll_fd);
            }
        }
    }
}

pub struct EpollReactor {
    pub event_fd: Arc<EventFd>,
    pub timer_fds: Mutex<HashMap<TimerId, Arc<TimerFd>>>,
    pub socket_notifiers: Mutex<HashMap<(SocketDescriptor, SocketEvent), Arc<SocketNotifier>>>,
    pub pending_socket_events: Mutex<Vec<(SocketDescriptor, SocketEvent)>>,
    cond: Condvar,
    lock: Mutex<bool>,
    #[cfg(target_os = "linux")]
    poller: Option<linux_epoll::Poller>,
}

impl Default for EpollReactor {
    fn default() -> Self {
        Self::new()
    }
}

impl EpollReactor {
    pub fn new() -> Self {
        Self {
            event_fd: Arc::new(EventFd::new(0)),
            timer_fds: Mutex::new(HashMap::new()),
            socket_notifiers: Mutex::new(HashMap::new()),
            pending_socket_events: Mutex::new(Vec::new()),
            cond: Condvar::new(),
            lock: Mutex::new(false),
            #[cfg(target_os = "linux")]
            poller: linux_epoll::Poller::new(),
        }
    }

    /// Ends a sleep in `epoll_wait`, whichever way it sleeps.
    fn notify_sleeper(&self) {
        #[cfg(target_os = "linux")]
        if let Some(poller) = &self.poller {
            poller.wake();
        }
        let _guard = self.lock.lock().unwrap();
        self.cond.notify_all();
    }

    pub fn wake_up(&self) {
        self.event_fd.write(1);
        self.notify_sleeper();
    }

    pub fn register_timer(&self, id: TimerId, interval: Duration, single_shot: bool) {
        let tfd = Arc::new(TimerFd::new(id, interval, single_shot));
        self.timer_fds.lock().unwrap().insert(id, tfd);
        self.notify_sleeper();
    }

    pub fn unregister_timer(&self, id: TimerId) {
        self.timer_fds.lock().unwrap().remove(&id);
    }

    /// The epoll events `fd` needs for its enabled notifiers. A Read and an Exception notifier
    /// of one descriptor share one epoll registration, so the mask is the union of the two.
    #[cfg(target_os = "linux")]
    fn interest_of(
        notifiers: &HashMap<(SocketDescriptor, SocketEvent), Arc<SocketNotifier>>,
        fd: SocketDescriptor,
    ) -> u32 {
        let enabled = |kind| {
            notifiers
                .get(&(fd, kind))
                .is_some_and(|notifier| notifier.is_enabled())
        };
        let mut events = 0;
        if enabled(SocketEvent::Read) {
            events |= (libc::EPOLLIN | libc::EPOLLERR | libc::EPOLLHUP | libc::EPOLLRDHUP) as u32;
        }
        if enabled(SocketEvent::Exception) {
            events |= (libc::EPOLLPRI | libc::EPOLLERR | libc::EPOLLHUP) as u32;
        }
        events
    }

    /// Brings the epoll registration of `fd` in line with its notifiers: a descriptor whose
    /// notifiers are all disabled or removed leaves the set, so a readable one cannot spin the
    /// loop. The map lock is held across the syscall so two updates of one descriptor cannot
    /// interleave.
    fn sync_fd(&self, fd: SocketDescriptor) {
        #[cfg(target_os = "linux")]
        if let Some(poller) = &self.poller {
            let notifiers = self.socket_notifiers.lock().unwrap();
            poller.set_interest(fd, Self::interest_of(&notifiers, fd));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = fd;
    }

    /// A notifier of `fd` was enabled or disabled. A thread blocked in `epoll_wait` sees the new
    /// interest set at once, so nothing needs waking.
    fn notifier_enabled_changed(&self, fd: SocketDescriptor) {
        self.sync_fd(fd);
    }

    pub fn register_socket_notifier(&self, notifier: &Arc<SocketNotifier>) {
        let key = (notifier.descriptor(), notifier.event_type());
        self.socket_notifiers
            .lock()
            .unwrap()
            .insert(key, Arc::clone(notifier));
        self.sync_fd(key.0);
    }

    /// Removes `notifier` only if it is the one registered for its `(fd, kind)`: a stale
    /// notifier of a closed descriptor must not remove a live one registered for the reused fd.
    pub fn unregister_socket_notifier(&self, notifier: &Arc<SocketNotifier>) {
        let key = (notifier.descriptor(), notifier.event_type());
        {
            let mut notifiers = self.socket_notifiers.lock().unwrap();
            if !notifiers
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, notifier))
            {
                return;
            }
            notifiers.remove(&key);
        }
        self.sync_fd(key.0);
    }

    /// Whether this reactor polls real descriptors: only the Linux one does, and only when its
    /// `epoll` set could be created. Elsewhere only events simulated with
    /// [`trigger_socket_event`](Self::trigger_socket_event) reach a registered notifier.
    pub fn polls_descriptors(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.poller.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    pub fn trigger_socket_event(&self, fd: SocketDescriptor, event: SocketEvent) {
        self.pending_socket_events.lock().unwrap().push((fd, event));
        self.notify_sleeper();
    }

    /// Waits until a descriptor is ready, `wake_up` is called, a timer is due or `timeout`
    /// passes (`None` waits for a timer or a wake-up). Returns whether the loop was woken, the
    /// due timers and the ready `(descriptor, kind)` pairs.
    pub fn epoll_wait(
        &self,
        timeout: Option<Duration>,
    ) -> (bool, Vec<TimerId>, Vec<(SocketDescriptor, SocketEvent)>) {
        #[cfg(target_os = "linux")]
        if let Some(poller) = &self.poller {
            return self.wait_in_kernel(poller, timeout);
        }
        self.wait_on_condvar(timeout)
    }

    /// How long a wait may last: `timeout` and the next simulated timer, whichever is earlier.
    fn sleep_duration(&self, timeout: Option<Duration>, now: Instant) -> Duration {
        match (timeout, self.next_timer_delay(now)) {
            (Some(t), Some(td)) => t.min(td),
            (Some(t), None) => t,
            (None, Some(td)) => td,
            (None, None) => Duration::from_secs(3600),
        }
    }

    #[cfg(target_os = "linux")]
    fn wait_in_kernel(
        &self,
        poller: &linux_epoll::Poller,
        timeout: Option<Duration>,
    ) -> (bool, Vec<TimerId>, Vec<(SocketDescriptor, SocketEvent)>) {
        let start = Instant::now();
        let mut pending_sockets = std::mem::take(&mut *self.pending_socket_events.lock().unwrap());
        let mut awoken = self.event_fd.is_readable();
        if awoken {
            self.event_fd.read();
        }
        let mut expired = self.collect_expired_timers(start);

        // Nothing to wait for when something is ready already: the poll then only collects the
        // descriptors that are ready as well.
        let wait = if awoken || !pending_sockets.is_empty() || !expired.is_empty() {
            Duration::ZERO
        } else {
            self.sleep_duration(timeout, start)
        };
        let millis = wait.as_millis() + u128::from(wait.subsec_nanos() % 1_000_000 != 0);
        let (_woken, ready) = poller.wait(millis.min(libc::c_int::MAX as u128) as libc::c_int);

        if self.event_fd.is_readable() {
            self.event_fd.read();
            awoken = true;
        }
        if !wait.is_zero() {
            for id in self.collect_expired_timers(Instant::now()) {
                if !expired.contains(&id) {
                    expired.push(id);
                }
            }
            pending_sockets.extend(std::mem::take(
                &mut *self.pending_socket_events.lock().unwrap(),
            ));
        }
        for (fd, flags) in ready {
            let failed = flags & (libc::EPOLLERR | libc::EPOLLHUP | libc::EPOLLRDHUP) as u32 != 0;
            let readable = flags & (libc::EPOLLIN | libc::EPOLLPRI) as u32 != 0;
            if failed {
                pending_sockets.push((fd, SocketEvent::Exception));
            }
            if readable || failed {
                pending_sockets.push((fd, SocketEvent::Read));
            }
        }
        (awoken, expired, pending_sockets)
    }

    /// The wait of a reactor without a kernel poller: only simulated events, a condition variable.
    fn wait_on_condvar(
        &self,
        timeout: Option<Duration>,
    ) -> (bool, Vec<TimerId>, Vec<(SocketDescriptor, SocketEvent)>) {
        let start = Instant::now();
        let pending_sockets = std::mem::take(&mut *self.pending_socket_events.lock().unwrap());

        // Check if eventfd is readable
        if self.event_fd.is_readable() {
            self.event_fd.read();
            return (true, self.collect_expired_timers(start), pending_sockets);
        }

        // Return immediately if there are pending socket events
        if !pending_sockets.is_empty() {
            return (false, self.collect_expired_timers(start), pending_sockets);
        }

        // Check if any timers expired
        let expired = self.collect_expired_timers(start);
        if !expired.is_empty() {
            return (false, expired, Vec::new());
        }
        let sleep_duration = self.sleep_duration(timeout, start);
        if sleep_duration.is_zero() {
            return (false, Vec::new(), Vec::new());
        }

        // Wait on condition variable or timeout
        let guard = self.lock.lock().unwrap();
        if self.event_fd.is_readable() {
            self.event_fd.read();
            let now = Instant::now();
            let expired = self.collect_expired_timers(now);
            return (true, expired, pending_sockets);
        }

        let (guard, _timeout_res) = self.cond.wait_timeout(guard, sleep_duration).unwrap();
        let was_awoken = if self.event_fd.is_readable() {
            self.event_fd.read();
            true
        } else {
            false
        };
        drop(guard);

        let now = Instant::now();
        let expired = self.collect_expired_timers(now);
        let pending_sockets = std::mem::take(&mut *self.pending_socket_events.lock().unwrap());
        (was_awoken, expired, pending_sockets)
    }

    fn next_timer_delay(&self, now: Instant) -> Option<Duration> {
        let tfds = self.timer_fds.lock().unwrap();
        let mut min_delay: Option<Duration> = None;
        for tfd in tfds.values() {
            if let Some(target) = *tfd.next_fire.lock().unwrap() {
                let delay = if target > now {
                    target - now
                } else {
                    Duration::from_millis(0)
                };
                min_delay = Some(min_delay.map_or(delay, |m| m.min(delay)));
            }
        }
        min_delay
    }

    fn collect_expired_timers(&self, now: Instant) -> Vec<TimerId> {
        let tfds = self.timer_fds.lock().unwrap();
        let mut expired = Vec::new();
        for (id, tfd) in tfds.iter() {
            if tfd.check_and_fire(now) {
                expired.push(*id);
            }
        }
        expired
    }
}

/// Linux/Unix cross-thread wakeup handle
#[derive(Clone)]
pub struct UnixEventDispatcherHandle {
    reactor: Arc<EpollReactor>,
}

impl EventDispatcherHandle for UnixEventDispatcherHandle {
    fn wake_up(&self) {
        self.reactor.wake_up();
    }
}

pub struct UnixEventDispatcher {
    reactor: Arc<EpollReactor>,
    pending_timers: Mutex<Vec<TimerId>>,
    native_filters: NativeEventFilterChain,
}

impl Default for UnixEventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl UnixEventDispatcher {
    pub fn new() -> Self {
        Self {
            reactor: Arc::new(EpollReactor::new()),
            pending_timers: Mutex::new(Vec::new()),
            native_filters: NativeEventFilterChain::new(),
        }
    }

    pub fn clone_handle(&self) -> UnixEventDispatcherHandle {
        UnixEventDispatcherHandle {
            reactor: Arc::clone(&self.reactor),
        }
    }
}

impl EventDispatcher for UnixEventDispatcher {
    fn wake_up(&self) {
        self.reactor.wake_up();
    }

    fn clone_handle(&self) -> Arc<dyn EventDispatcherHandle> {
        Arc::new(self.clone_handle())
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
        let timeout = if can_wait {
            next_timer_timeout
        } else {
            Some(Duration::from_millis(0))
        };

        let (awoken, expired_timers, triggered_sockets) = self.reactor.epoll_wait(timeout);

        if !triggered_sockets.is_empty() {
            // Collect under the lock, then notify without it: a notifier may run a nested event
            // loop that registers or unregisters notifiers, which would deadlock on this lock.
            let notifiers: Vec<Arc<SocketNotifier>> = {
                let map = self.reactor.socket_notifiers.lock().unwrap();
                triggered_sockets
                    .iter()
                    .filter_map(|key| map.get(key).cloned())
                    .collect()
            };
            for notifier in notifiers {
                notifier.notify();
            }
        }

        // Due simulated timers are kept even when the loop was also woken: they have fired once
        // and would be lost otherwise.
        let timers_due = !expired_timers.is_empty();
        if timers_due {
            self.pending_timers.lock().unwrap().extend(expired_timers);
        }
        if awoken {
            return DispatchResult::Awoken;
        }
        if timers_due {
            return DispatchResult::Normal;
        }
        if !triggered_sockets.is_empty() {
            return DispatchResult::Normal;
        }
        if !can_wait || next_timer_timeout.is_some() {
            DispatchResult::Timeout
        } else {
            DispatchResult::Normal
        }
    }

    fn register_timer(&self, entry: &TimerEntry) {
        self.reactor.register_timer(
            entry.id,
            Duration::from_millis(entry.interval_ms),
            entry.single_shot,
        );
    }

    fn unregister_timer(&self, entry: &TimerEntry) {
        self.reactor.unregister_timer(entry.id);
    }

    fn register_socket_notifier(
        &self,
        notifier: &Arc<SocketNotifier>,
    ) -> Result<(), SocketNotifierError> {
        if !self.reactor.polls_descriptors() {
            return Err(SocketNotifierError::Unsupported {
                dispatcher: self.class_name(),
            });
        }
        self.reactor.register_socket_notifier(notifier);
        // A notifier that is disabled later must leave the poll set at once, or a readable
        // descriptor would end every wait immediately.
        let reactor = Arc::downgrade(&self.reactor);
        let fd = notifier.descriptor();
        notifier.set_enable_hook(Some(Arc::new(move || {
            if let Some(reactor) = reactor.upgrade() {
                reactor.notifier_enabled_changed(fd);
            }
        })));
        Ok(())
    }

    fn unregister_socket_notifier(&self, notifier: &Arc<SocketNotifier>) {
        notifier.set_enable_hook(None);
        self.reactor.unregister_socket_notifier(notifier);
    }

    fn send_timer_events(&self, registry: &Mutex<TimerRegistry>) {
        let now_ms = crate::timer::current_time_ms();
        let pending = std::mem::take(&mut *self.pending_timers.lock().unwrap());
        for id in crate::timer::deliver_due_timers(registry, now_ms, pending) {
            self.reactor.unregister_timer(id);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unix_eventfd_write_read() {
        let efd = EventFd::new(0);
        assert!(!efd.is_readable());

        efd.write(1);
        assert!(efd.is_readable());

        efd.write(2);
        assert_eq!(efd.read(), 3);
        assert!(!efd.is_readable());
    }

    #[test]
    fn test_unix_dispatcher_cross_thread_wakeup() {
        let dispatcher = UnixEventDispatcher::new();
        let handle = dispatcher.clone_handle();

        let thread_handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            handle.wake_up();
        });

        let result = dispatcher.process_events(true, Some(Duration::from_millis(500)));
        assert_eq!(result, DispatchResult::Awoken);

        thread_handle.join().unwrap();
    }

    #[test]
    fn test_unix_dispatcher_timeout() {
        let dispatcher = UnixEventDispatcher::new();
        let start = Instant::now();
        let result = dispatcher.process_events(true, Some(Duration::from_millis(30)));
        assert_eq!(result, DispatchResult::Timeout);
        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[test]
    fn test_unix_dispatcher_timer_integration() {
        let dispatcher = UnixEventDispatcher::new();
        let mut entry = TimerEntry::new(
            TimerId(101),
            20,
            0,
            crate::timer::TimerType::Precise,
            crate::object::ObjectId::next(),
        );
        entry.single_shot = true;
        dispatcher.register_timer(&entry);
        std::thread::sleep(Duration::from_millis(30));

        let result = dispatcher.process_events(false, None);
        assert_eq!(result, DispatchResult::Normal);

        dispatcher.unregister_timer(&entry);
    }

    #[test]
    fn test_unix_dispatcher_socket_notifier_activation() {
        let dispatcher = UnixEventDispatcher::new();
        let fd = 42; // Simulated socket FD (e.g. X11 or D-Bus)
        let notifier = Arc::new(SocketNotifier::new(fd, SocketEvent::Read));

        let triggered_fd = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&triggered_fd);
        notifier.activated().connect(move |&sock| {
            *sink.lock().unwrap() = Some(sock);
        });

        // The reactor takes the simulated descriptor on every platform; the dispatcher refuses a
        // notifier where it does not poll descriptors.
        dispatcher.reactor.register_socket_notifier(&notifier);

        // Simulate incoming socket data (EPOLLIN)
        dispatcher
            .reactor
            .trigger_socket_event(fd, SocketEvent::Read);

        // Dispatch events
        let res = dispatcher.process_events(false, None);
        assert_eq!(res, DispatchResult::Normal);
        assert_eq!(*triggered_fd.lock().unwrap(), Some(fd));

        // Disabling notifier should prevent signals
        *triggered_fd.lock().unwrap() = None;
        notifier.set_enabled(false);
        dispatcher
            .reactor
            .trigger_socket_event(fd, SocketEvent::Read);
        dispatcher.process_events(false, None);
        assert_eq!(*triggered_fd.lock().unwrap(), None);

        dispatcher.unregister_socket_notifier(&notifier);
    }

    /// Unregistering a stale notifier whose fd was reused does not remove the live notifier
    /// registered for that fd (Qt `QEventDispatcherUNIX::unregisterSocketNotifier` matches the
    /// notifier itself).
    #[test]
    fn test_unregistering_a_stale_notifier_keeps_the_live_one_for_a_reused_fd() {
        let dispatcher = UnixEventDispatcher::new();
        let fd = 43;
        let stale = Arc::new(SocketNotifier::new(fd, SocketEvent::Read));
        let live = Arc::new(SocketNotifier::new(fd, SocketEvent::Read));
        let hits = Arc::new(Mutex::new(0));
        let sink = Arc::clone(&hits);
        live.activated()
            .connect(move |_| *sink.lock().unwrap() += 1);

        dispatcher.reactor.register_socket_notifier(&stale);
        dispatcher.reactor.register_socket_notifier(&live);
        dispatcher.unregister_socket_notifier(&stale);

        dispatcher
            .reactor
            .trigger_socket_event(fd, SocketEvent::Read);
        dispatcher.process_events(false, None);
        assert_eq!(*hits.lock().unwrap(), 1, "the live notifier was removed");
        dispatcher.unregister_socket_notifier(&live);
    }

    /// The dispatcher registers a notifier only where it polls descriptors, and fails loudly with
    /// `Unsupported` everywhere else (Windows, macOS, a Linux without `epoll`), instead of
    /// holding a notifier that its descriptor would never activate.
    #[test]
    fn test_registering_a_socket_notifier_fails_where_descriptors_are_not_polled() {
        let dispatcher = UnixEventDispatcher::new();
        let notifier = Arc::new(SocketNotifier::new(44, SocketEvent::Read));

        let registered = dispatcher.register_socket_notifier(&notifier);

        assert_eq!(registered.is_ok(), dispatcher.reactor.polls_descriptors());
        if let Err(error) = registered {
            assert_eq!(
                error,
                SocketNotifierError::Unsupported {
                    dispatcher: dispatcher.class_name()
                }
            );
            assert!(
                dispatcher
                    .reactor
                    .socket_notifiers
                    .lock()
                    .unwrap()
                    .is_empty(),
                "a refused notifier is not kept"
            );
        }
        #[cfg(not(target_os = "linux"))]
        assert!(registered.is_err());
        dispatcher.unregister_socket_notifier(&notifier);
    }
}
