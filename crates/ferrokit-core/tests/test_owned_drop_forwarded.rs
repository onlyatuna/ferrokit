// SPDX-License-Identifier: LGPL-3.0-only
//! An `OwnedObject` dropped on a thread other than its registration thread forwards the
//! destruction to the registration thread (like `deleteLater`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    move_to_thread_by_id, register_owned_qobject, with_object, ObjectData, ObjectDataMut, ObjectId,
    OwnedObject, QObject, ThreadId,
};

const WAIT: Duration = Duration::from_secs(10);

struct Probe {
    data: ObjectData,
    dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        *self.dropped_on.lock() = Some(std::thread::current().id());
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

fn probe(dropped_on: &Arc<Mutex<Option<std::thread::ThreadId>>>) -> Probe {
    Probe {
        data: ObjectData::new(ObjectId::next()),
        dropped_on: Arc::clone(dropped_on),
    }
}

/// The object survives the owner's drop until the registration thread runs its queue, and is then
/// destroyed on that thread.
#[test]
fn the_destruction_runs_on_the_registration_thread_when_its_loop_turns() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let dropped_on = Arc::new(Mutex::new(None));
    // SAFETY: the owner is dropped by this thread only, and no callback of the object runs here.
    let owner = unsafe { register_owned_qobject(probe(&dropped_on)) };
    let id = OwnedObject::object_id(&owner);

    let (tid_tx, tid_rx) = channel();
    let (moved_tx, moved_rx) = channel::<()>();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let (seen_tx, seen_rx) = channel::<(bool, std::thread::ThreadId)>();
    let worker = std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        moved_rx.recv_timeout(WAIT).unwrap();
        // Before the owner's drop is delivered the object is alive.
        let alive_before = with_object(id, |_| ()).is_some();
        dropped_rx.recv_timeout(WAIT).unwrap();
        for _ in 0..20 {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = alive_before;
        seen_tx
            .send((with_object(id, |_| ()).is_none(), std::thread::current().id()))
            .unwrap();
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    moved_tx.send(()).unwrap();

    drop(owner);
    assert!(
        dropped_on.lock().is_none(),
        "the owner's thread must not destroy an object that lives on another thread"
    );
    dropped_tx.send(()).unwrap();
    let (gone, worker_thread) = seen_rx.recv_timeout(WAIT).unwrap();
    worker.join().unwrap();
    assert!(gone, "the object is unregistered once the forwarded call ran");
    assert_eq!(*dropped_on.lock(), Some(worker_thread));
}

/// The registration thread never turns its loop: the forwarded call is discarded with the thread's
/// event queue, and the object is destroyed then instead of leaking.
#[test]
fn the_object_is_destroyed_when_the_registration_thread_ends_without_running_its_queue() {
    let here = ThreadId::current();
    let _el = EventLoop::new();
    let dropped_on = Arc::new(Mutex::new(None));
    let ran = Arc::new(AtomicBool::new(false));
    // SAFETY: as above.
    let owner = unsafe { register_owned_qobject(probe(&dropped_on)) };
    let id = OwnedObject::object_id(&owner);

    let (tid_tx, tid_rx) = channel();
    let (dropped_tx, dropped_rx) = channel::<()>();
    let ran2 = Arc::clone(&ran);
    let worker = std::thread::spawn(move || {
        let _el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        dropped_rx.recv_timeout(WAIT).unwrap();
        ran2.store(true, Ordering::SeqCst);
    });
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(id, there, here), Ok(()));
    drop(owner);
    assert!(dropped_on.lock().is_none());
    dropped_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(ran.load(Ordering::SeqCst));
    assert!(
        dropped_on.lock().is_some(),
        "the object was leaked when its thread ended"
    );
}
