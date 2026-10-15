// SPDX-License-Identifier: LGPL-3.0-only
//! An object tree lives in one thread: a parent and its children share a thread affinity.
//!
//! Qt refuses a parent that belongs to another thread. It has already removed the child from its
//! old parent when it finds out, warns, and leaves the child without a parent
//! (qobject.cpp:2309-2345). `set_parent` does the same and reports `Err(DifferentThread)`. An owned
//! child's `Box` would be lost by a silent detach, so `reparent_owned` and `add_owned_child` refuse
//! without changing anything.

use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, move_to_thread, register_qobject, reparent_owned, set_parent, ObjectData,
    ObjectId, QObject, ReparentError, ThreadId,
};

struct Node {
    data: ObjectData,
    seen: Vec<String>,
}

impl Node {
    fn registered() -> Box<Self> {
        let mut n = Box::new(Self {
            data: ObjectData::new(ObjectId::next()),
            seen: Vec::new(),
        });
        // SAFETY: the Box is never moved out of and stays on this thread until it is dropped,
        // which unregisters it.
        unsafe { register_qobject(&mut *n) };
        n
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::ChildAdded { child_id } => self.seen.push(format!("added {}", child_id.0)),
            EventKind::ChildRemoved { child_id } => {
                self.seen.push(format!("removed {}", child_id.0))
            }
            _ => {}
        }
        false
    }
}

/// What a `Node` on another thread ended up with.
struct RemoteReport {
    children: Vec<ObjectId>,
    seen: Vec<String>,
}

/// A registered `Node` that lives on its own thread until `finish`.
struct Remote {
    id: ObjectId,
    release: Sender<()>,
    thread: JoinHandle<RemoteReport>,
}

impl Remote {
    fn start() -> Self {
        let (id_tx, id_rx) = channel();
        let (release, released) = channel::<()>();
        let thread = std::thread::spawn(move || {
            let node = Node::registered();
            id_tx.send(node.data.id).unwrap();
            // Ends when `finish` sends, or when the test panics and drops the sender.
            let _ = released.recv();
            RemoteReport {
                children: node.data.children.clone(),
                seen: node.seen.clone(),
            }
        });
        let id = id_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the remote node is registered");
        Self {
            id,
            release,
            thread,
        }
    }

    fn finish(self) -> RemoteReport {
        self.release.send(()).unwrap();
        self.thread.join().unwrap()
    }
}

#[test]
fn a_parent_in_another_thread_is_refused_and_the_child_loses_its_old_parent() {
    let old = Node::registered();
    let old_id = old.data.id;
    let mut child = Node::registered();
    let child_id = child.data.id;
    set_parent(&mut child.data, Some(old_id)).unwrap();
    let remote = Remote::start();

    let result = set_parent(&mut child.data, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(child.data.parent(), None);
    assert!(
        old.data.children.is_empty(),
        "the old parent no longer lists the child"
    );
    assert_eq!(
        old.seen,
        vec![
            format!("added {}", child_id.0),
            format!("removed {}", child_id.0)
        ]
    );
    let report = remote.finish();
    assert!(report.children.is_empty(), "the new parent gained no child");
    assert!(report.seen.is_empty(), "the new parent heard nothing");
}

#[test]
fn a_parent_in_another_thread_is_refused_for_a_child_that_had_no_parent() {
    let mut child = Node::registered();
    let remote = Remote::start();

    let result = set_parent(&mut child.data, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(child.data.parent(), None);
    assert!(remote.finish().children.is_empty());
}

#[test]
fn a_refused_parent_leaves_the_child_free_to_take_a_parent_in_its_own_thread() {
    let own = Node::registered();
    let own_id = own.data.id;
    let mut child = Node::registered();
    let remote = Remote::start();
    assert_eq!(
        set_parent(&mut child.data, Some(remote.id)),
        Err(ReparentError::DifferentThread)
    );

    set_parent(&mut child.data, Some(own_id)).unwrap();

    assert_eq!(child.data.parent(), Some(own_id));
    remote.finish();
}

#[test]
fn the_check_compares_the_current_affinities_not_the_creating_threads() {
    let mut parent = Node::registered();
    let parent_id = parent.data.id;
    let mut child = Node::registered();
    let here = ThreadId::current();
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();

    move_to_thread(&mut parent.data, elsewhere, here).unwrap();
    assert_eq!(
        set_parent(&mut child.data, Some(parent_id)),
        Err(ReparentError::DifferentThread),
        "the parent moved away from the thread it was created in"
    );

    move_to_thread(&mut child.data, elsewhere, here).unwrap();
    assert_eq!(set_parent(&mut child.data, Some(parent_id)), Ok(()));
}

#[test]
fn an_owned_child_is_not_moved_to_a_parent_in_another_thread() {
    let owner = Node::registered();
    let owner_id = owner.data.id;
    // SAFETY: the Box is handed to the owner, which keeps it at this address until it is dropped.
    let child_id = unsafe { add_owned_child(owner_id, Node::registered()) }
        .map_err(|(error, _)| error)
        .expect("same thread");
    let remote = Remote::start();

    let result = reparent_owned(child_id, Some(remote.id));

    assert_eq!(result.err(), Some(ReparentError::DifferentThread));
    assert_eq!(owner.data.children, vec![child_id]);
    assert_eq!(
        owner.data.owned_children.len(),
        1,
        "the owner still holds the Box"
    );
    assert!(remote.finish().children.is_empty());
}

#[test]
fn an_owned_child_built_for_another_thread_is_handed_back() {
    let owner = Node::registered();
    let owner_id = owner.data.id;
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();
    let child = Box::new(Node {
        data: ObjectData::with_thread(ObjectId::next(), elsewhere),
        seen: Vec::new(),
    });
    let child_id = child.data.id;

    // SAFETY: the Box comes back with the error, so nothing stays attached to the owner.
    let result = unsafe { add_owned_child(owner_id, child) };

    match result {
        Err((error, handed_back)) => {
            assert_eq!(error, ReparentError::DifferentThread);
            assert_eq!(handed_back.data.id, child_id);
        }
        Ok(_) => panic!("an owned child with another affinity was attached"),
    }
    assert!(owner.data.children.is_empty());
}

const CASE_ENV: &str = "FERROKIT_TEST_CASE";

/// The refusal of `the_refusal_prints_qts_warning`, run in a child process so its stderr can be
/// read. Does nothing in a normal run.
#[test]
fn the_refusal_case_for_the_warning_test() {
    if std::env::var(CASE_ENV).as_deref() != Ok("warning") {
        return;
    }
    let mut child = Node::registered();
    let remote = Remote::start();
    assert_eq!(
        set_parent(&mut child.data, Some(remote.id)),
        Err(ReparentError::DifferentThread)
    );
    remote.finish();
}

#[test]
fn the_refusal_prints_qts_warning() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_refusal_case_for_the_warning_test",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CASE_ENV, "warning")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the case failed: {stderr}");
    assert!(
        stderr
            .contains("QObject::setParent: Cannot set parent, new parent is in a different thread"),
        "stderr was: {stderr}"
    );
}
