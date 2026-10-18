// SPDX-License-Identifier: LGPL-3.0-only
//! A user `EventCompressor` that panics while a post is under the queue and registry locks must
//! not stop later object destruction or event loop teardown on that thread.
//!
//! The panic poisons process-global locks if they are not recovered, so the scenario runs in a
//! child process (`the_compressor_panic_case`) that the driving test starts, kills after a hard
//! timeout, and judges by its exit status.

use ferrokit_core::event::{Event, EventCompressor, EventKind};
use ferrokit_core::event_loop::{get_thread_event_sender, post_event_to_thread, EventLoop};
use ferrokit_core::object::{
    register_owned_qobject, with_object, ObjectData, ObjectId, QObject, ThreadId,
};
use ferrokit_core::timer::Timer;
use std::io::Read;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CASE_ENV: &str = "FERROKIT_TEST_CASE";
const CASE_TIMEOUT: Duration = Duration::from_secs(60);

struct PanickingCompressor;

impl EventCompressor for PanickingCompressor {
    fn try_compress(&self, _existing: &mut Event, _incoming: &Event, _receiver: ObjectId) -> bool {
        panic!("the test compressor panics");
    }
}

struct Node {
    data: ObjectData,
    dropped: Arc<AtomicUsize>,
}

impl Drop for Node {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn update_request() -> Event {
    Event::new(EventKind::UpdateRequest)
}

/// The scenario, run in a child process. Does nothing in a normal run.
#[test]
fn the_compressor_panic_case() {
    if std::env::var(CASE_ENV).as_deref() != Ok("compressor") {
        return;
    }
    let mut el = EventLoop::new();
    el.set_compressor(Arc::new(PanickingCompressor));
    let thread = ThreadId::current();

    let dropped = Arc::new(AtomicUsize::new(0));
    // SAFETY: the owner stays on this thread and is dropped while no callback of it runs.
    let owned = unsafe {
        register_owned_qobject(Node {
            data: ObjectData::new(ObjectId::next()),
            dropped: Arc::clone(&dropped),
        })
    };
    let id = owned.object_data().id();

    // The first post finds nothing to merge with; every later post to the same receiver reaches
    // the compressor, which panics under the queue lock (and the registry lock when posting by
    // thread).
    post_event_to_thread(thread, id, update_request());
    let by_thread = catch_unwind(|| post_event_to_thread(thread, id, update_request()));
    assert!(
        by_thread.is_err(),
        "posting by thread did not reach the compressor"
    );
    let by_loop = catch_unwind(AssertUnwindSafe(|| el.post_event(id, update_request())));
    assert!(
        by_loop.is_err(),
        "posting through the loop did not reach the compressor"
    );
    let by_handle = catch_unwind(AssertUnwindSafe(|| {
        el.handle().post_event(id, update_request())
    }));
    assert!(
        by_handle.is_err(),
        "posting through a handle did not reach the compressor"
    );

    // Starting a zero-interval timer posts a `ZeroTimer` to its own object while the timer
    // registry lock is held, so the same panic unwinds through that lock too.
    let timer = Timer::new();
    let timer_id = timer.object_id();
    post_event_to_thread(thread, timer_id, update_request());
    let started = catch_unwind(AssertUnwindSafe(|| timer.start_with_interval(0)));
    assert!(
        started.is_err(),
        "starting the zero timer did not reach the compressor"
    );

    // The registries still work: a post to a receiver with nothing queued goes through.
    let bystander = ObjectId::next();
    post_event_to_thread(thread, bystander, update_request());

    // Destroying the objects completes, frees them, and removes what they left behind.
    let destroyed = catch_unwind(AssertUnwindSafe(|| drop(owned)));
    assert!(
        destroyed.is_ok(),
        "destroying an object panicked after the compressor panic"
    );
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "the object was leaked");
    assert!(with_object(id, |_| ()).is_none());

    let destroyed = catch_unwind(AssertUnwindSafe(|| drop(timer)));
    assert!(
        destroyed.is_ok(),
        "destroying the timer panicked after the compressor panic"
    );
    assert!(with_object(timer_id, |_| ()).is_none());

    match el.timer_registry().lock() {
        Ok(registry) => assert_eq!(registry.len(), 0, "a timer registration was left behind"),
        Err(_) => panic!("the timer registry lock is still poisoned"),
    }
    match el.queue().lock() {
        Ok(queue) => {
            assert_eq!(
                queue.len(),
                1,
                "events of destroyed objects were not purged"
            );
            assert_eq!(queue.events()[0].receiver, bystander);
        }
        Err(_) => panic!("the event queue lock is still poisoned"),
    }

    // Tearing the loop down completes and unregisters its handle.
    let torn_down = catch_unwind(AssertUnwindSafe(|| drop(el)));
    assert!(torn_down.is_ok(), "dropping the event loop panicked");
    assert!(get_thread_event_sender(thread).is_none());
}

/// Runs `case` in a child process with `CASE_ENV` set to `case_value`, killing it after
/// `CASE_TIMEOUT`.
fn run_case(case: &str, case_value: &str) -> (std::process::ExitStatus, String, String) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env(CASE_ENV, case_value)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > CASE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let out = stdout_reader.join().unwrap();
            let err = stderr_reader.join().unwrap();
            panic!("{case} did not finish within {CASE_TIMEOUT:?}\nstdout: {out}\nstderr: {err}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (
        status,
        stdout_reader.join().unwrap(),
        stderr_reader.join().unwrap(),
    )
}

#[cfg_attr(miri, ignore)]
#[test]
fn a_panicking_compressor_does_not_stop_destruction_or_loop_teardown() {
    let case = "the_compressor_panic_case";
    let (status, stdout, stderr) = run_case(case, "compressor");
    assert!(
        status.success(),
        "the case failed: {status}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&format!("{case} ... ok")),
        "the case did not run\nstdout: {stdout}\nstderr: {stderr}"
    );
}
