// SPDX-License-Identifier: LGPL-3.0-only
//! Unparenting an owned child must never destroy it.
//!
//! Qt's `setParent_helper` (qobject.cpp:2287-2345) only unlinks the child and sends
//! `ChildRemoved`; object lifetime stays with the caller.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::notify_helper;
use ferrokit_core::object::{
    add_owned_child, register_owned_qobject, reparent_owned_object, set_parent_by_id,
    unregister_qobject, with_object, with_object_mut, ObjectData, ObjectId, OwnedObject, QObject,
    ReparentError,
};

type Hook = Arc<Mutex<Option<Box<dyn FnMut(&mut Event) + Send>>>>;

/// Records every event it receives and optionally runs a hook inside `event()`.
struct Node {
    data: ObjectData,
    seen: Arc<Mutex<Vec<String>>>,
    hook: Hook,
}

impl Node {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            seen: Arc::new(Mutex::new(Vec::new())),
            hook: Arc::new(Mutex::new(None)),
        }
    }
    fn boxed() -> Box<Self> {
        Box::new(Self::new())
    }
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread and is dropped, which unregisters the object,
        // when no callback of it runs.
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
            EventKind::ChildAdded { child_id } => self
                .seen
                .lock()
                .unwrap()
                .push(format!("added {}", child_id.0)),
            EventKind::ChildRemoved { child_id } => self
                .seen
                .lock()
                .unwrap()
                .push(format!("removed {}", child_id.0)),
            _ => {}
        }
        let hook = Arc::clone(&self.hook);
        let mut slot = hook.lock().unwrap();
        if let Some(f) = slot.as_mut() {
            f(event);
        }
        false
    }
}

/// Event filter that records what it was asked to filter.
struct Spy {
    data: ObjectData,
    log: Arc<Mutex<Vec<String>>>,
}

impl QObject for Spy {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event_filter(&mut self, watched: ObjectId, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::ChildAdded { child_id } => self
                .log
                .lock()
                .unwrap()
                .push(format!("added {} on {}", child_id.0, watched.0)),
            EventKind::ChildRemoved { child_id } => self
                .log
                .lock()
                .unwrap()
                .push(format!("removed {} on {}", child_id.0, watched.0)),
            _ => {}
        }
        false
    }
}

fn adopt(parent_id: ObjectId) -> (ObjectId, Arc<std::sync::atomic::AtomicBool>) {
    let child = Node::boxed();
    let alive = child.data.liveness();
    // SAFETY: the child's Box is moved into `parent` and only moved again by reparent_owned.
    let id = unsafe { add_owned_child(parent_id, child) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));
    (id, alive)
}

fn finish(node: OwnedObject<Node>) {
    let id = OwnedObject::object_id(&node);
    drop(node);
    // SAFETY: dropped on its registration thread, no callback active.
    unsafe { unregister_qobject(id) };
}

/// Reduces the result to its error so tests can compare it (`dyn QObject` has no `Debug`).
fn refused(
    r: Result<Option<OwnedObject<dyn QObject>>, ReparentError>,
) -> Result<(), ReparentError> {
    r.map(|_| ())
}

fn parent_of(id: ObjectId) -> Option<Option<ObjectId>> {
    with_object(id, |o| o.object_data().parent())
}

/// Reads of an owner's `Node` that are not inside a callback of it.
trait Probe {
    fn id(&self) -> ObjectId;
    fn children(&self) -> Vec<ObjectId>;
    fn owned_len(&self) -> usize;
    fn seen(&self) -> Arc<Mutex<Vec<String>>>;
    fn hook(&self) -> Hook;
}

impl Probe for OwnedObject<Node> {
    fn id(&self) -> ObjectId {
        OwnedObject::object_id(self)
    }
    fn children(&self) -> Vec<ObjectId> {
        self.with(|n| n.data.children().to_vec()).unwrap()
    }
    fn owned_len(&self) -> usize {
        self.with(|n| n.data.owned_children().len()).unwrap()
    }
    fn seen(&self) -> Arc<Mutex<Vec<String>>> {
        self.with(|n| Arc::clone(&n.seen)).unwrap()
    }
    fn hook(&self) -> Hook {
        self.with(|n| Arc::clone(&n.hook)).unwrap()
    }
}

#[test]
fn set_parent_none_on_owned_child_keeps_child_alive() {
    let parent = Node::registered();
    let (child_id, child_alive) = adopt(parent.id());

    let result = with_object_mut(child_id, |c| c.object_data_mut().set_parent(None)).unwrap();

    assert_eq!(result, Err(ReparentError::OwnedByParent));
    assert!(
        child_alive.load(Ordering::SeqCst),
        "set_parent(None) destroyed an owned child"
    );
    assert_eq!(parent.owned_len(), 1, "parent must still own the child");
    assert_eq!(
        parent.children(),
        vec![child_id],
        "child must still be listed"
    );

    finish(parent);
    assert!(
        !child_alive.load(Ordering::SeqCst),
        "dropping the parent destroys its owned child"
    );
}

#[test]
fn reparent_owned_to_none_returns_the_box_and_child_survives() {
    let parent = Node::registered();
    let (child_id, child_alive) = adopt(parent.id());

    let released = reparent_owned_object(child_id, None)
        .unwrap()
        .expect("ownership returns to the caller");

    assert!(child_alive.load(Ordering::SeqCst));
    assert_eq!(released.with(|r| r.object_data().parent()).unwrap(), None);
    assert!(!released
        .with(|r| r.object_data().is_owned_by_parent())
        .unwrap());
    assert!(parent.children().is_empty());
    assert!(parent.owned_len() == 0);
    assert_eq!(
        parent_of(child_id),
        Some(None),
        "registry record must be unparented too"
    );

    // Parent going away no longer touches the released child.
    finish(parent);
    assert!(
        child_alive.load(Ordering::SeqCst),
        "released child must outlive its former parent"
    );
    drop(released);
    assert!(
        !child_alive.load(Ordering::SeqCst),
        "caller owns it and may drop it"
    );
}

#[test]
fn reparent_owned_moves_ownership_to_a_new_parent() {
    let a = Node::registered();
    let b = Node::registered();
    let (child_id, alive) = adopt(a.id());
    assert_eq!(a.children(), &[child_id]);
    assert!(b.children().is_empty());

    assert!(reparent_owned_object(child_id, Some(b.id()))
        .unwrap()
        .is_none());

    assert!(a.children().is_empty() && a.owned_len() == 0);
    assert_eq!(b.children(), &[child_id]);
    assert_eq!(b.owned_len(), 1);
    assert_eq!(parent_of(child_id), Some(Some(b.id())));

    finish(a);
    assert!(alive.load(Ordering::SeqCst));
    finish(b);
    assert!(
        !alive.load(Ordering::SeqCst),
        "the new parent owns and destroys it"
    );
}

#[test]
fn reparent_owned_notifies_both_parents_through_event_filters() {
    let mut a = Node::registered();
    let mut b = Node::registered();
    let log = Arc::new(Mutex::new(Vec::new()));
    // SAFETY: the owner stays on this thread and is dropped when no callback of it runs.
    let spy = unsafe {
        register_owned_qobject(Spy {
            data: ObjectData::new(ObjectId::next()),
            log: Arc::clone(&log),
        })
    };
    let spy_id = OwnedObject::object_id(&spy);
    a.with_mut(|n| n.data.install_event_filter(spy_id)).unwrap();
    b.with_mut(|n| n.data.install_event_filter(spy_id)).unwrap();
    let (child_id, _alive) = adopt(a.id());

    reparent_owned_object(child_id, Some(b.id())).unwrap();

    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!("added {} on {}", child_id.0, a.id().0),
            format!("removed {} on {}", child_id.0, a.id().0),
            format!("added {} on {}", child_id.0, b.id().0),
        ],
        "filters must see ChildAdded on attach, then ChildRemoved and ChildAdded on reparent"
    );
    assert_eq!(
        *a.seen().lock().unwrap(),
        vec![
            format!("added {}", child_id.0),
            format!("removed {}", child_id.0)
        ]
    );
    assert_eq!(
        *b.seen().lock().unwrap(),
        vec![format!("added {}", child_id.0)]
    );

    finish(a);
    finish(b);
    let spy_id = OwnedObject::object_id(&spy);
    drop(spy);
    // SAFETY: dropped on its registration thread.
    unsafe { unregister_qobject(spy_id) };
}

#[test]
fn reparent_owned_rejects_self_and_descendant_parents_without_changes() {
    let top = Node::registered();
    let (mid_id, _) = adopt(top.id());
    // Give `mid` a child of its own, reachable through the registry.
    let grand_id = attach_grandchild(mid_id);

    assert_eq!(
        refused(reparent_owned_object(mid_id, Some(mid_id))),
        Err(ReparentError::InvalidParent)
    );
    assert_eq!(
        refused(reparent_owned_object(mid_id, Some(grand_id))),
        Err(ReparentError::InvalidParent)
    );
    assert_eq!(
        refused(reparent_owned_object(mid_id, Some(ObjectId(u64::MAX)))),
        Err(ReparentError::InvalidParent)
    );

    assert_eq!(top.owned_len(), 1);
    assert_eq!(parent_of(mid_id), Some(Some(top.id())));
    finish(top);
}

/// Attaches a grandchild to `mid_id` from outside `mid`'s borrow. A borrowed parent refuses the
/// attach (`Busy`), so this must not run inside `with_object_mut(mid_id, …)`.
fn attach_grandchild(mid_id: ObjectId) -> ObjectId {
    let grand = Node::boxed();
    // SAFETY: the Box moves into `mid` and stays there until `top` is finished.
    unsafe { add_owned_child(mid_id, grand) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"))
}

#[test]
fn reparent_owned_rejects_children_that_are_not_owned() {
    let parent = Node::registered();
    let loose = Node::registered();
    let loose_id = loose.id();
    // Registered metadata link only: the parent does not hold the Box.
    set_parent_by_id(loose_id, Some(parent.id())).unwrap();

    assert_eq!(
        refused(reparent_owned_object(loose_id, None)),
        Err(ReparentError::NotOwned)
    );
    assert_eq!(
        refused(reparent_owned_object(ObjectId(u64::MAX), None)),
        Err(ReparentError::UnknownObject)
    );
    let root = Node::registered();
    let root_id = root.id();
    assert_eq!(
        refused(reparent_owned_object(root_id, Some(parent.id()))),
        Err(ReparentError::NotOwned)
    );

    // set_parent still works for non-owned children.
    set_parent_by_id(loose_id, None).unwrap();
    assert!(parent.children().is_empty());

    finish(root);
    finish(loose);
    finish(parent);
}

#[test]
fn reparent_owned_fails_visibly_when_a_party_is_borrowed_and_changes_nothing() {
    let a = Node::registered();
    let b = Node::registered();
    let (child_id, alive) = adopt(a.id());
    let b_id = b.id();

    // 1) Destination parent is mid-callback.
    let outcome = Arc::new(Mutex::new(Vec::new()));
    {
        let outcome = Arc::clone(&outcome);
        *b.hook().lock().unwrap() = Some(Box::new(move |ev| {
            if matches!(ev.kind, EventKind::UpdateRequest) {
                outcome
                    .lock()
                    .unwrap()
                    .push(reparent_owned_object(child_id, Some(b_id)).map(|_| ()));
            }
        }));
    }
    notify_helper(b_id, &mut Event::new(EventKind::UpdateRequest));
    *b.hook().lock().unwrap() = None;
    assert_eq!(*outcome.lock().unwrap(), vec![Err(ReparentError::Busy)]);

    // 2) The child itself is mid-callback: releasing its Box would let the caller drop it.
    let outcome2 = Arc::new(Mutex::new(Vec::new()));
    {
        let outcome2 = Arc::clone(&outcome2);
        ferrokit_core::object::with_object_mut(child_id, |_| {
            outcome2
                .lock()
                .unwrap()
                .push(reparent_owned_object(child_id, None).map(|_| ()));
        });
    }
    assert_eq!(*outcome2.lock().unwrap(), vec![Err(ReparentError::Busy)]);

    assert_eq!(
        a.owned_len(),
        1,
        "failed calls must leave ownership untouched"
    );
    assert_eq!(a.children(), &[child_id]);
    assert!(b.children().is_empty());
    assert_eq!(parent_of(child_id), Some(Some(a.id())));
    assert!(alive.load(Ordering::SeqCst));

    finish(a);
    finish(b);
}

// Attaching an owned child must notify the parent with `ChildAdded`
// (Qt `setParent_helper`, qobject.cpp:2346-2352).
#[test]
fn add_owned_child_sends_child_added() {
    let parent = Node::registered();
    let parent_id = parent.id();
    let child = Node::boxed();
    let child_id = child.data.id();
    // SAFETY: the Box is owned by `parent` for the rest of the test.
    let attached = unsafe { add_owned_child(parent_id, child) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));
    assert_eq!(attached, child_id);
    assert_eq!(
        *parent.seen().lock().unwrap(),
        vec![format!("added {}", child_id.0)]
    );
    finish(parent);
}

#[test]
fn add_owned_child_fails_visibly_when_parent_is_borrowed() {
    let parent = Node::registered();
    let parent_id = parent.id();
    let outcome = ferrokit_core::object::with_object_mut(parent_id, |_| {
        // SAFETY: the Box is returned in the error and never stored.
        unsafe { add_owned_child(parent_id, Node::boxed()) }.map(|_| ())
    })
    .expect("parent is registered");
    match outcome {
        Err((err, _returned)) => assert_eq!(err, ReparentError::Busy),
        Ok(()) => panic!("attach into a borrowed parent must not succeed"),
    }
    assert!(parent.owned_len() == 0);
    finish(parent);
}
