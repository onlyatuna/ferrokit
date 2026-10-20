// SPDX-License-Identifier: LGPL-3.0-only
//! The registry keeps an index from a parent to the registered objects whose record links to it,
//! and a thread move reads the subtree from that index instead of walking every registered object.
//!
//! The index is checked against the relation a full walk derives (`verify_parent_link_index`)
//! after each step of sequences of registration, reparenting, deferred changes, moves, drops and
//! forwarded drops. A test that asserts only the answer of a move would pass with a wrong index
//! that happens to give the same subtree.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    add_owned_child, child_index_entries_read, move_to_thread_by_id, query_object_parent,
    query_object_thread, register_owned_qobject, reparent_owned_object, set_parent_by_id,
    verify_parent_link_index, with_object_mut, ObjectData, ObjectDataMut, ObjectId, OwnedObject,
    QObject, ThreadId,
};

const WAIT: Duration = Duration::from_secs(10);

/// The registry is process-wide and the check is exact only while nothing else changes a link.
static SERIAL: Mutex<()> = Mutex::new(());

struct Node {
    data: ObjectData,
}

impl Node {
    fn with_id(id: ObjectId) -> Self {
        Self { data: ObjectData::new(id) }
    }

    fn new() -> Self {
        Self::with_id(ObjectId::next())
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
}

fn registered() -> OwnedObject<Node> {
    // SAFETY: the owner stays on its registration thread until a move, and is dropped when no
    // callback of the object runs.
    unsafe { register_owned_qobject(Node::new()) }
}

fn id_of(o: &OwnedObject<Node>) -> ObjectId {
    OwnedObject::object_id(o)
}

#[track_caller]
fn check() {
    if let Err(difference) = verify_parent_link_index() {
        panic!("the index differs from the registry: {difference}");
    }
}

#[test]
fn the_index_follows_registration_reparenting_and_unregistration() {
    let _serial = SERIAL.lock();
    let _el = EventLoop::new();
    check();

    let a = registered();
    let b = registered();
    let c = registered();
    let d = registered();
    let (a_id, b_id, c_id, d_id) = (id_of(&a), id_of(&b), id_of(&c), id_of(&d));
    check();

    set_parent_by_id(b_id, Some(a_id)).unwrap();
    set_parent_by_id(c_id, Some(a_id)).unwrap();
    set_parent_by_id(d_id, Some(b_id)).unwrap();
    check();

    // To another parent, to the same parent again, and to none.
    set_parent_by_id(c_id, Some(b_id)).unwrap();
    check();
    set_parent_by_id(c_id, Some(b_id)).unwrap();
    check();
    set_parent_by_id(d_id, None).unwrap();
    check();
    set_parent_by_id(d_id, Some(c_id)).unwrap();
    check();

    // A link that would close a cycle changes nothing.
    assert!(set_parent_by_id(a_id, Some(d_id)).is_err());
    check();

    // The middle of the chain goes away. A parent's drop lets go of the children it lists, which
    // unregisters them (see `release_child`), and each one clears the links of its own children.
    drop(b);
    check();
    assert_eq!(query_object_parent(b_id), None);
    assert_eq!(query_object_parent(c_id), None);
    assert_eq!(query_object_parent(d_id), Some(None));

    drop(c);
    check();
    drop(d);
    check();
    drop(a);
    check();
}

#[test]
fn a_child_linked_before_registration_and_before_its_parent_is_found_through_the_index() {
    let _serial = SERIAL.lock();
    let here = ThreadId::current();
    let _el = EventLoop::new();

    let parent_id = ObjectId::next();
    let mut child = Node::new();
    // Neither is registered: the link is accepted and only the child's own data has it.
    child.data.set_parent(Some(parent_id)).unwrap();
    // SAFETY: as in `registered`.
    let child = unsafe { register_owned_qobject(child) };
    let child_id = id_of(&child);
    check();
    assert_eq!(query_object_parent(child_id), Some(Some(parent_id)));

    // The parent is registered afterwards, under the id the child names.
    // SAFETY: as in `registered`.
    let parent = unsafe { register_owned_qobject(Node::with_id(parent_id)) };
    check();

    // A move of the parent takes the child with it, which only the index can tell it: the
    // parent's own lists do not name the child.
    let (tid_tx, tid_rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = spawn_pumping_worker(tid_tx, &stop);
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(query_object_thread(child_id), Some(here));
    assert_eq!(move_to_thread_by_id(parent_id, there, here), Ok(()));
    check();
    assert_eq!(query_object_thread(parent_id), Some(there));
    assert_eq!(query_object_thread(child_id), Some(there), "the child was left behind");

    // The owners are dropped away from the registration thread: the worker destroys the objects.
    drop(parent);
    wait_until_gone(parent_id);
    check();
    drop(child);
    wait_until_gone(child_id);
    check();
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
}

#[test]
fn owned_children_moved_between_parents_are_indexed_under_the_new_parent() {
    let _serial = SERIAL.lock();
    let _el = EventLoop::new();

    let first = registered();
    let second = registered();
    let (first_id, second_id) = (id_of(&first), id_of(&second));
    // SAFETY: the child is not used through the `Box` afterwards, and stays on this thread.
    let child_id = unsafe { add_owned_child(first_id, Box::new(Node::new())) }.ok().unwrap();
    check();
    assert_eq!(query_object_parent(child_id), Some(Some(first_id)));

    assert!(reparent_owned_object(child_id, Some(second_id)).unwrap().is_none());
    check();
    assert_eq!(query_object_parent(child_id), Some(Some(second_id)));

    // Released from every parent: the caller gets the owner, and the child stays registered.
    let released = reparent_owned_object(child_id, None).unwrap().expect("an owner");
    check();
    assert_eq!(query_object_parent(child_id), Some(None));

    drop(released);
    check();
    assert_eq!(query_object_parent(child_id), None);
    drop(first);
    drop(second);
    check();
}

#[test]
fn changes_made_while_a_parent_is_borrowed_reach_the_index_at_once() {
    let _serial = SERIAL.lock();
    let _el = EventLoop::new();

    let old = registered();
    let new = registered();
    let (old_id, new_id) = (id_of(&old), id_of(&new));
    let child = registered();
    let child_id = id_of(&child);
    set_parent_by_id(child_id, Some(old_id)).unwrap();
    check();

    // The parents' own lists wait for the callbacks to return (deferred changes); the registry's
    // link, and with it the index, is changed by the commit and does not wait.
    with_object_mut(old_id, |_| {
        with_object_mut(new_id, |_| {
            set_parent_by_id(child_id, Some(new_id)).unwrap();
            check();
            assert_eq!(query_object_parent(child_id), Some(Some(new_id)));
            set_parent_by_id(child_id, Some(old_id)).unwrap();
            check();
            set_parent_by_id(child_id, Some(new_id)).unwrap();
            check();
        })
        .expect("the new parent is not borrowed");
    })
    .expect("the old parent is not borrowed");
    check();

    // A child dropped while its parent is borrowed.
    let other = registered();
    let other_id = id_of(&other);
    with_object_mut(new_id, |_| {
        set_parent_by_id(other_id, Some(new_id)).unwrap();
        check();
        drop(other);
        check();
    })
    .expect("the parent is not borrowed");
    check();

    // A parent dropped inside its own callback is unregistered at once, and its child's link goes
    // with it.
    let parent_id = {
        let parent = registered();
        let parent_id = id_of(&parent);
        set_parent_by_id(child_id, Some(parent_id)).unwrap();
        check();
        with_object_mut(parent_id, |_| {
            drop(parent);
            check();
        })
        .expect("the parent is registered and not borrowed");
        parent_id
    };
    check();
    assert_eq!(query_object_parent(parent_id), None);
    drop(child);
    drop(old);
    drop(new);
    check();
}

/// Moves a subtree to a worker, drops its owners on this thread, and lets the worker run the
/// forwarded destruction. The index is checked at each quiet point.
#[test]
fn a_subtree_moved_to_another_thread_and_dropped_here_leaves_the_index_clean() {
    let _serial = SERIAL.lock();
    let here = ThreadId::current();
    let _el = EventLoop::new();

    let root = registered();
    let mid = registered();
    let leaf = registered();
    let (root_id, mid_id, leaf_id) = (id_of(&root), id_of(&mid), id_of(&leaf));
    set_parent_by_id(mid_id, Some(root_id)).unwrap();
    set_parent_by_id(leaf_id, Some(mid_id)).unwrap();
    check();

    let (tid_tx, tid_rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = spawn_pumping_worker(tid_tx, &stop);
    let there = tid_rx.recv_timeout(WAIT).unwrap();
    assert_eq!(move_to_thread_by_id(root_id, there, here), Ok(()));
    check();
    for id in [root_id, mid_id, leaf_id] {
        assert_eq!(query_object_thread(id), Some(there));
    }

    // Owners dropped away from the registration thread forward the destruction to the worker.
    drop(mid);
    wait_until_gone(mid_id);
    // The destroyed parent let go of the child it lists, which unregistered it.
    wait_until_gone(leaf_id);
    check();
    drop(root);
    wait_until_gone(root_id);
    check();
    drop(leaf);
    check();

    stop.store(true, Ordering::Release);
    worker.join().unwrap();
    check();
}

/// A thread with an event loop that runs its queue until `stop` is set, and reports its id.
fn spawn_pumping_worker(
    tid_tx: std::sync::mpsc::Sender<ThreadId>,
    stop: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let stop = Arc::clone(stop);
    std::thread::spawn(move || {
        let el = EventLoop::new();
        tid_tx.send(ThreadId::current()).unwrap();
        while !stop.load(Ordering::Acquire) {
            el.process_events(false);
            std::thread::sleep(Duration::from_millis(1));
        }
        el.process_events(false);
    })
}

fn wait_until_gone(id: ObjectId) {
    let start = Instant::now();
    while query_object_parent(id).is_some() {
        assert!(start.elapsed() < WAIT, "{id:?} was not destroyed by the worker");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Moves small subtrees while many unrelated objects are registered. What a move inspects is
/// counted: the index entries it reads (`child_index_entries_read`, per calling thread). The count
/// for the same moves must be the same with and without the unrelated objects, and it must be
/// bounded by the size of the subtree, so a walk over the registry would fail it. The time is only
/// printed (`--nocapture`).
#[test]
fn moving_a_small_subtree_does_not_depend_on_how_many_objects_are_registered() {
    let _serial = SERIAL.lock();
    let here = ThreadId::current();
    let _el = EventLoop::new();

    let (unrelated_count, moves) = if cfg!(miri) { (40, 4) } else { (20_000, 2_000) };
    const SUBTREE: usize = 4;

    let (tid_tx, tid_rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker = spawn_pumping_worker(tid_tx, &stop);
    let there = tid_rx.recv_timeout(WAIT).unwrap();

    // Returns the index entries the moves read.
    let run = |label: &str| -> usize {
        let mut owners = Vec::with_capacity(moves * SUBTREE);
        let mut ids = Vec::with_capacity(moves * SUBTREE);
        let mut entries_read = 0;
        let start = Instant::now();
        for _ in 0..moves {
            let root = registered();
            let (child_a, child_b) = (registered(), registered());
            let grandchild = registered();
            let root_id = id_of(&root);
            set_parent_by_id(id_of(&child_a), Some(root_id)).unwrap();
            set_parent_by_id(id_of(&child_b), Some(root_id)).unwrap();
            set_parent_by_id(id_of(&grandchild), Some(id_of(&child_a))).unwrap();
            let before = child_index_entries_read();
            assert_eq!(move_to_thread_by_id(root_id, there, here), Ok(()));
            entries_read += child_index_entries_read() - before;
            ids.extend([root_id, id_of(&child_a), id_of(&child_b), id_of(&grandchild)]);
            owners.extend([root, child_a, child_b, grandchild]);
        }
        let elapsed = start.elapsed();
        eprintln!(
            "{label}: {moves} moves of a {SUBTREE}-object subtree in {elapsed:?} ({:?} per move), \
             {entries_read} index entries read",
            elapsed / moves as u32
        );
        check();
        drop(owners);
        for id in ids {
            wait_until_gone(id);
        }
        check();
        entries_read
    };

    let alone = run("no unrelated objects");
    let unrelated: Vec<_> = (0..unrelated_count).map(|_| registered()).collect();
    check();
    let crowded = run(&format!("{unrelated_count} unrelated objects"));
    drop(unrelated);
    check();

    stop.store(true, Ordering::Release);
    worker.join().unwrap();

    assert!(alone > 0, "the counter saw no read: the move no longer goes through the index");
    assert_eq!(
        alone, crowded,
        "the unrelated registered objects changed what the moves inspected"
    );
    assert!(
        alone <= moves * SUBTREE,
        "a move read {alone} entries for {moves} subtrees of {SUBTREE} objects"
    );
}
