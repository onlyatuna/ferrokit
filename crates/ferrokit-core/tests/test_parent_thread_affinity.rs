// SPDX-License-Identifier: LGPL-3.0-only
//! An object tree lives in one thread: a parent and its children share a thread affinity.
//!
//! Qt refuses a parent that belongs to another thread. It has already removed the child from its
//! old parent when it finds out, warns, and leaves the child without a parent
//! (qobject.cpp:2309-2345; line numbers differ between Qt versions, these were checked against Qt
//! 6.11.2's `qobject.cpp`). `set_parent` does the same, prints Qt's warning and reports
//! `Err(DifferentThread)`. An owned child's `Box` would be lost by a detach, so `reparent_owned`
//! and `add_owned_child` refuse without changing anything and without printing the warning: the
//! owner keeps the child it had, and a refused new child is handed back.

use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use ferrokit_core::event::{Event, EventKind};
#[allow(deprecated)] // the test exercises the deprecated `move_to_thread`
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, ReparentError, ThreadId, add_owned_child,
    move_to_thread, move_to_thread_by_id, query_object_parent, register_owned_qobject,
    reparent_owned_object, set_parent, set_parent_by_id,
};

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Node` holds an `ObjectData` and a `Vec<String>`, both `Send`; no `Rc`, no `Cell`
    // shared with a handle left behind, no thread-local state.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

struct Node {
    data: ObjectData,
    seen: Vec<String>,
}

impl Node {
    fn new() -> Self {
        Self {
            data: movable_data(ObjectId::next()),
            seen: Vec::new(),
        }
    }

    fn registered() -> OwnedObject<Self> {
        // SAFETY: the returned owner is used only through `with`/`with_mut` and dropped by the
        // test on this thread (or, for `Remote`, on the thread that made it); no callback of the
        // node runs concurrently with those uses, and the tests never touch the node inside one.
        unsafe { register_owned_qobject(Self::new()) }
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
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
            id_tx.send(node.with(|n| n.data.id()).unwrap()).unwrap();
            // Ends when `finish` sends, or when the test panics and drops the sender.
            let _ = released.recv();
            node.with(|n| RemoteReport {
                children: n.data.children().to_vec(),
                seen: n.seen.clone(),
            })
            .unwrap()
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
    let old_id = old.with(|o| o.data.id()).unwrap();
    // `set_parent` takes the child's `&mut ObjectData`, which an owner only lends through
    // `with_mut` (where the object counts as borrowed), so the child is not owned here.
    let mut child = Node::new();
    let child_id = child.data.id();
    set_parent(&mut child.data, Some(old_id)).unwrap();
    let remote = Remote::start();

    let result = set_parent(&mut child.data, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(child.data.parent(), None);
    assert!(
        old.with(|o| o.data.children().is_empty()).unwrap(),
        "the old parent no longer lists the child"
    );
    assert_eq!(
        old.with(|o| o.seen.clone()).unwrap(),
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
    // Not owned: see the first test.
    let mut child = Node::new();
    let remote = Remote::start();

    let result = set_parent(&mut child.data, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(child.data.parent(), None);
    assert!(remote.finish().children.is_empty());
}
#[test]
fn set_parent_by_id_refuses_parent_in_another_thread_and_detaches_child_from_old_parent() {
    let old = Node::registered();
    let old_id = old.with(|o| o.data.id()).unwrap();
    let child = Node::registered();
    let child_id = child.with(|c| c.data.id()).unwrap();
    set_parent_by_id(child_id, Some(old_id)).unwrap();
    let remote = Remote::start();

    let result = set_parent_by_id(child_id, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(query_object_parent(child_id), Some(None));
    assert_eq!(child.with(|c| c.data.parent()).unwrap(), None);
    assert!(
        old.with(|o| o.data.children().is_empty()).unwrap(),
        "the old parent no longer lists the child"
    );
    assert_eq!(
        old.with(|o| o.seen.clone()).unwrap(),
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
fn set_parent_by_id_refuses_parent_in_another_thread_for_parentless_child() {
    let child = Node::registered();
    let child_id = child.with(|c| c.data.id()).unwrap();
    let remote = Remote::start();

    let result = set_parent_by_id(child_id, Some(remote.id));

    assert_eq!(result, Err(ReparentError::DifferentThread));
    assert_eq!(query_object_parent(child_id), Some(None));
    assert_eq!(child.with(|c| c.data.parent()).unwrap(), None);
    assert!(remote.finish().children.is_empty());
}

#[test]
fn a_refused_parent_leaves_the_child_free_to_take_a_parent_in_its_own_thread() {
    let own = Node::registered();
    let own_id = own.with(|o| o.data.id()).unwrap();
    let mut child = Node::new();
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
    let parent = Node::registered();
    let parent_id = parent.with(|p| p.data.id()).unwrap();
    // Not owned: `set_parent` takes the child's `&mut ObjectData` (see the first test).
    let mut child = Node::new();
    let here = ThreadId::current();
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();

    move_to_thread_by_id(parent_id, elsewhere, here).unwrap();
    assert_eq!(
        set_parent(&mut child.data, Some(parent_id)),
        Err(ReparentError::DifferentThread),
        "the parent moved away from the thread it was created in"
    );

    #[allow(deprecated)] // exercises the deprecated `move_to_thread` on purpose
    move_to_thread(&mut child.data, elsewhere, here).unwrap();
    assert_eq!(set_parent(&mut child.data, Some(parent_id)), Ok(()));
}

#[test]
fn an_owned_child_is_not_moved_to_a_parent_in_another_thread() {
    let owner = Node::registered();
    let owner_id = owner.with(|o| o.data.id()).unwrap();
    // SAFETY: the `Box::new(Node::new())` is a fresh allocation moved into the call, on the
    // parent's thread (`expect("same thread")` checks it was accepted); no reference to it is held
    // and the parent owns it afterwards.
    let child_id = unsafe { add_owned_child(owner_id, Box::new(Node::new())) }
        .map_err(|(error, _)| error)
        .expect("same thread");
    let remote = Remote::start();

    let result = reparent_owned_object(child_id, Some(remote.id));

    assert_eq!(result.err(), Some(ReparentError::DifferentThread));
    assert_eq!(
        owner.with(|o| o.data.children().to_vec()).unwrap(),
        vec![child_id]
    );
    owner
        .with(|o| {
            assert_eq!(
                o.data.owned_children().len(),
                1,
                "the owner still holds the Box"
            );
            o.data.owned_children()[0]
                .with(|c| {
                    let child = c.object_data();
                    assert_eq!(
                        child.parent(),
                        Some(owner_id),
                        "the child keeps its old parent"
                    );
                    assert!(child.is_owned_by_parent());
                })
                .unwrap();
        })
        .unwrap();
    assert_eq!(
        owner.with(|o| o.seen.clone()).unwrap(),
        vec![format!("added {}", child_id.0)],
        "the old parent is told nothing more"
    );
    let report = remote.finish();
    assert!(report.children.is_empty(), "the new parent gained no child");
    assert!(report.seen.is_empty(), "the new parent heard nothing");
}

#[test]
fn a_refused_owned_child_can_still_be_moved_to_a_parent_in_its_own_thread() {
    let owner = Node::registered();
    let owner_id = owner.with(|o| o.data.id()).unwrap();
    let other = Node::registered();
    let other_id = other.with(|o| o.data.id()).unwrap();
    // SAFETY: the `Box::new(Node::new())` is a fresh allocation moved into the call, on the
    // parent's thread (`expect("same thread")` checks it was accepted); no reference to it is held
    // and the parent owns it afterwards.
    let child_id = unsafe { add_owned_child(owner_id, Box::new(Node::new())) }
        .map_err(|(error, _)| error)
        .expect("same thread");
    let remote = Remote::start();
    assert_eq!(
        reparent_owned_object(child_id, Some(remote.id)).err(),
        Some(ReparentError::DifferentThread)
    );

    let moved = reparent_owned_object(child_id, Some(other_id));

    assert!(
        matches!(moved, Ok(None)),
        "the move was refused: {:?}",
        moved.err()
    );
    assert!(owner.with(|o| o.data.children().is_empty()).unwrap());
    assert_eq!(
        other.with(|o| o.data.children().to_vec()).unwrap(),
        vec![child_id]
    );
    remote.finish();
}

#[test]
fn an_owned_child_built_for_another_thread_is_handed_back() {
    let owner = Node::registered();
    let owner_id = owner.with(|o| o.data.id()).unwrap();
    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();
    let child = Box::new(Node {
        data: ObjectData::with_thread(ObjectId::next(), elsewhere),
        seen: Vec::new(),
    });
    let child_id = child.data.id();

    // SAFETY: `child` is a fresh `Box` whose `ObjectData` is bound to another thread's affinity;
    // `add_owned_child` checks the affinity first and returns the `Box` in the error without
    // registering it (asserted below: `DifferentThread`, no parent link), so no pointer to it is
    // kept and no reference was held across the call (only `child_id` was read before).
    let result = unsafe { add_owned_child(owner_id, child) };

    match result {
        Err((error, handed_back)) => {
            assert_eq!(error, ReparentError::DifferentThread);
            assert_eq!(handed_back.data.id(), child_id);
            assert_eq!(handed_back.data.parent(), None, "no parent link was made");
            assert!(!handed_back.data.is_owned_by_parent());
        }
        Ok(_) => panic!("an owned child with another affinity was attached"),
    }
    assert!(owner.with(|o| o.data.children().is_empty()).unwrap());
    assert!(owner.with(|o| o.data.owned_children().is_empty()).unwrap());
    assert!(
        owner.with(|o| o.seen.is_empty()).unwrap(),
        "the owner was told of no child"
    );
}

const CASE_ENV: &str = "FERROKIT_TEST_CASE";

/// The refusal of `the_refusal_prints_qts_warning`, run in a child process so its stderr can be
/// read. Does nothing in a normal run.
#[test]
fn the_refusal_case_for_the_warning_test() {
    if std::env::var(CASE_ENV).as_deref() != Ok("warning") {
        return;
    }
    let mut child = Node::new();
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

/// The two refusals of `the_owned_refusals_print_no_warning`, run in a child process so its stderr
/// can be read. Does nothing in a normal run.
#[test]
fn the_owned_refusal_case_for_the_silence_test() {
    if std::env::var(CASE_ENV).as_deref() != Ok("silence") {
        return;
    }
    let owner = Node::registered();
    let owner_id = owner.with(|o| o.data.id()).unwrap();
    // SAFETY: the `Box::new(Node::new())` is a fresh allocation moved into the call, on the
    // parent's thread (`expect("same thread")` checks it was accepted); no reference to it is held
    // and the parent owns it afterwards.
    let child_id = unsafe { add_owned_child(owner_id, Box::new(Node::new())) }
        .map_err(|(error, _)| error)
        .expect("same thread");
    let remote = Remote::start();
    assert_eq!(
        reparent_owned_object(child_id, Some(remote.id)).err(),
        Some(ReparentError::DifferentThread)
    );
    remote.finish();

    let elsewhere = std::thread::spawn(ThreadId::current).join().unwrap();
    let foreign = Box::new(Node {
        data: ObjectData::with_thread(ObjectId::next(), elsewhere),
        seen: Vec::new(),
    });
    // SAFETY: `foreign` is a fresh `Box` made here; `add_owned_child` checks the child's thread
    // affinity against the parent's first and, on `DifferentThread`, returns the `Box` in the
    // error without registering it (asserted on the next line), so no pointer to it is kept.
    let refused = unsafe { add_owned_child(owner_id, foreign) };
    assert!(matches!(refused, Err((ReparentError::DifferentThread, _))));
}

#[test]
fn the_owned_refusals_print_no_warning() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_owned_refusal_case_for_the_silence_test",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CASE_ENV, "silence")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "the case failed: {stderr}");
    assert!(
        stdout.contains("the_owned_refusal_case_for_the_silence_test ... ok"),
        "the case did not run: {stdout}"
    );
    assert!(
        !stderr.to_lowercase().contains("different thread"),
        "stderr was: {stderr}"
    );
}
