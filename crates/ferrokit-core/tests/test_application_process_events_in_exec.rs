// SPDX-License-Identifier: LGPL-3.0-only
//! `QCoreApplication::processEvents` called from a handler that runs inside `exec` must pump
//! the running loop: events posted before the call are delivered before it returns.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferrokit_core::application::CoreApplication;
use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::post_event_to_thread;
use ferrokit_core::object::{
    register_owned_qobject, unregister_qobject, ObjectData, ObjectId, OwnedObject, QObject,
    ThreadId,
};

type Log = Arc<Mutex<Vec<&'static str>>>;

/// Aborts the test process when the body hangs, so a regression fails instead of hanging CI.
struct Watchdog {
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl Watchdog {
    fn arm(limit: Duration) -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let done = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&done);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + limit;
            while std::time::Instant::now() < deadline {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // Written to the raw stderr so libtest output capture cannot swallow it on exit.
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("watchdog: test body exceeded {limit:?}\n").as_bytes(),
            );
            std::process::exit(101);
        });
        Self { done }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Receives the posted mouse press inside `exec`, posts to `Sink`, and pumps the loop.
struct Trigger {
    data: ObjectData,
    sink: ObjectId,
    log: Log,
}

impl QObject for Trigger {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        if !matches!(event.kind, EventKind::MouseButtonPress { .. }) {
            return false;
        }
        self.log.lock().unwrap().push("trigger");
        post_event_to_thread(
            ThreadId::current(),
            self.sink,
            Event::new(EventKind::UpdateRequest),
        );
        let delivered = CoreApplication::process_events(false);
        self.log.lock().unwrap().push(if delivered {
            "processed"
        } else {
            "not-processed"
        });
        CoreApplication::quit();
        true
    }
}

/// Records the event posted by `Trigger`.
struct Sink {
    data: ObjectData,
    log: Log,
}

impl QObject for Sink {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            self.log.lock().unwrap().push("nested");
            true
        } else {
            false
        }
    }
}

#[test]
fn process_events_inside_exec_delivers_posted_events_before_returning() {
    let _watchdog = Watchdog::arm(Duration::from_secs(10));
    let mut app = CoreApplication::new(vec!["test".to_string()]);
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    // SAFETY: both owners stay on this thread; nothing touches either object while a handler runs,
    // and they are unregistered after exec returns.
    let sink = unsafe {
        register_owned_qobject(Sink {
            data: ObjectData::new(ObjectId::next()),
            log: Arc::clone(&log),
        })
    };
    let sink_id = OwnedObject::object_id(&sink);
    // SAFETY: as above.
    let trigger = unsafe {
        register_owned_qobject(Trigger {
            data: ObjectData::new(ObjectId::next()),
            sink: sink_id,
            log: Arc::clone(&log),
        })
    };
    let trigger_id = OwnedObject::object_id(&trigger);
    post_event_to_thread(
        ThreadId::current(),
        trigger_id,
        Event::new_spontaneous(EventKind::MouseButtonPress {
            x: 0,
            y: 0,
            button: 1,
        }),
    );

    app.exec();

    // SAFETY: exec has returned, so no handler can reach either object any more.
    unsafe {
        unregister_qobject(trigger_id);
        unregister_qobject(sink_id);
    }
    assert_eq!(
        *log.lock().unwrap(),
        vec!["trigger", "nested", "processed"],
        "process_events inside exec must pump the running loop"
    );
}
