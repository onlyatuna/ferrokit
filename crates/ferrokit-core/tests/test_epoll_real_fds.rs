// SPDX-License-Identifier: LGPL-3.0-only
//! The Linux dispatcher blocks in `epoll_wait` on the descriptors of enabled socket notifiers and
//! wakes for the one that becomes ready. Real descriptors (a pipe) stand in for sockets.
#![cfg(target_os = "linux")]

use ferrokit_core::event_loop::{
    DispatchResult, EventDispatcher, EventDispatcherHandle, SocketEvent, SocketNotifier,
    UnixEventDispatcher,
};
use std::os::raw::c_int;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// A non-blocking pipe, closed on drop.
struct Pipe {
    read: c_int,
    write: c_int,
}

impl Pipe {
    fn new() -> Self {
        let mut fds = [0 as c_int; 2];
        // SAFETY: `fds` is a local `[c_int; 2]`, exactly the two-element array `pipe2` writes
        // through the pointer; `fds` is not read before `pipe2` returns.
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) };
        assert_eq!(rc, 0, "pipe2 failed");
        Self {
            read: fds[0],
            write: fds[1],
        }
    }

    fn put(&self) {
        write_byte(self.write);
    }
}

fn write_byte(fd: c_int) {
    let byte = 1u8;
    // SAFETY: `byte` is a live local, so the pointer is valid for the 1 byte read; `fd` is the
    // write end of a `Pipe` that outlives every caller (the writer thread is joined before the
    // `Pipe` drops). A full non-blocking pipe makes `write` fail with `EAGAIN`, which the result
    // ignoring tolerates.
    unsafe {
        libc::write(fd, (&byte as *const u8).cast(), 1);
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: `read` and `write` are the descriptors `pipe2` returned in `Pipe::new`; this
        // value is their only owner (the writer thread of the test only holds a copy of the number
        // and is joined before the `Pipe` is dropped), and `drop` runs once, so each is closed once.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

fn counting_notifier(fd: c_int, kind: SocketEvent) -> (Arc<SocketNotifier>, Arc<AtomicUsize>) {
    let notifier = Arc::new(SocketNotifier::new(fd, kind));
    let count = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&count);
    notifier.activated().connect(move |_| {
        sink.fetch_add(1, Ordering::SeqCst);
    });
    (notifier, count)
}

/// A descriptor that becomes readable while the loop sleeps ends the sleep. The writer keeps
/// writing until the wait returns, so the data arrives while the loop is blocked, not before.
#[test]
fn readable_descriptor_wakes_a_blocked_dispatcher() {
    let pipe = Pipe::new();
    let dispatcher = UnixEventDispatcher::new();
    let (notifier, count) = counting_notifier(pipe.read, SocketEvent::Read);
    dispatcher
        .register_socket_notifier(&notifier)
        .expect("Linux polls descriptors");

    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let write_fd = pipe.write;
    let writer = std::thread::spawn(move || {
        while !writer_stop.load(Ordering::SeqCst) {
            write_byte(write_fd);
            std::thread::park_timeout(Duration::from_millis(5));
        }
    });

    // Neither a timer nor a wake-up is due for a long time: only the descriptor can end this.
    let result = dispatcher.process_events(true, Some(Duration::from_secs(10)));
    stop.store(true, Ordering::SeqCst);
    writer.thread().unpark();
    writer.join().unwrap();

    assert_eq!(result, DispatchResult::Normal);
    assert!(count.load(Ordering::SeqCst) >= 1);
    dispatcher.unregister_socket_notifier(&notifier);
}

/// `process_events(false)` polls the descriptors too.
#[test]
fn a_non_waiting_pass_reports_ready_descriptors() {
    let pipe = Pipe::new();
    let dispatcher = UnixEventDispatcher::new();
    let (notifier, count) = counting_notifier(pipe.read, SocketEvent::Read);
    dispatcher
        .register_socket_notifier(&notifier)
        .expect("Linux polls descriptors");
    pipe.put();

    assert_eq!(
        dispatcher.process_events(false, None),
        DispatchResult::Normal
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    dispatcher.unregister_socket_notifier(&notifier);
}

/// A disabled notifier on a readable descriptor must not end every wait at once (the descriptor
/// stays readable, so a level-triggered poll would spin the loop).
#[test]
fn a_disabled_notifier_leaves_the_poll_set() {
    let pipe = Pipe::new();
    let dispatcher = UnixEventDispatcher::new();
    let (notifier, count) = counting_notifier(pipe.read, SocketEvent::Read);
    dispatcher
        .register_socket_notifier(&notifier)
        .expect("Linux polls descriptors");
    pipe.put();
    notifier.set_enabled(false);

    for _ in 0..2 {
        let result = dispatcher.process_events(true, Some(Duration::from_millis(20)));
        assert_eq!(result, DispatchResult::Timeout);
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // Enabling it again resumes polling the still readable descriptor.
    notifier.set_enabled(true);
    assert_eq!(
        dispatcher.process_events(true, Some(Duration::from_secs(60))),
        DispatchResult::Normal
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
    dispatcher.unregister_socket_notifier(&notifier);
}

/// A Read and an Exception notifier of one descriptor share the one epoll registration: adding
/// the second must not be lost, and removing it must not silence the first.
#[test]
fn read_and_exception_notifiers_of_one_descriptor_are_independent() {
    let pipe = Pipe::new();
    let dispatcher = UnixEventDispatcher::new();
    let (read, read_count) = counting_notifier(pipe.read, SocketEvent::Read);
    let (exception, _) = counting_notifier(pipe.read, SocketEvent::Exception);
    dispatcher
        .register_socket_notifier(&read)
        .expect("Linux polls descriptors");
    dispatcher
        .register_socket_notifier(&exception)
        .expect("Linux polls descriptors");
    dispatcher.unregister_socket_notifier(&exception);

    pipe.put();
    assert_eq!(
        dispatcher.process_events(false, None),
        DispatchResult::Normal
    );
    assert_eq!(read_count.load(Ordering::SeqCst), 1);

    dispatcher.unregister_socket_notifier(&read);
}

/// A wake-up from another thread still ends a blocked wait, now through the eventfd.
#[test]
fn wake_up_ends_a_blocked_wait() {
    let dispatcher = UnixEventDispatcher::new();
    let handle = dispatcher.clone_handle();
    let waker = std::thread::spawn(move || handle.wake_up());
    let result = dispatcher.process_events(true, Some(Duration::from_secs(60)));
    waker.join().unwrap();
    assert_eq!(result, DispatchResult::Awoken);
}
