// SPDX-License-Identifier: LGPL-3.0-only
//! A parent never reads an owned child whose callback is running.
//!
//! The callback runs on `&mut self`. Reading the child from anywhere else meanwhile, even only its
//! id through `object_data()`, makes a `&` that aliases that `&mut`: undefined behavior, which Miri
//! reports. The parent keeps each owned child's id next to its pointer, so dropping the parent,
//! moving a sibling or unlinking a sibling from inside the callback does not read the child.
//!
//! On a normal build the child counts the calls to its `object_data` and `object_data_mut` made
//! while its callback runs; the callback itself uses its fields directly, so only the framework
//! makes such calls.

use std::cell::Cell;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, add_owned_child, dispatch_to_object,
    register_owned_qobject, reparent_owned_object, set_parent_by_id, with_object,
};

struct Node {
    data: ObjectData,
}

impl Node {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
        }
    }

    fn registered() -> OwnedObject<Self> {
        // SAFETY: the returned owner is used only through `with`/`OwnedObject::object_id` (which
        // refuse or ignore callbacks), and is dropped by the test on this thread. A drop inside a
        // child's callback (first test) is the documented exception: `OwnedObject::drop`
        // unregisters at once and frees after the callback returns.
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
}

type Hook = Box<dyn FnOnce(&mut Probe)>;

/// An owned child that runs `hook` in its callback and counts the reads made meanwhile.
struct Probe {
    data: ObjectData,
    running: Cell<bool>,
    reads_while_running: Rc<Cell<u32>>,
    hook: Option<Hook>,
}

impl Probe {
    fn noted(&self) {
        if self.running.get() {
            self.reads_while_running
                .set(self.reads_while_running.get() + 1);
        }
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        self.noted();
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        self.noted();
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if !matches!(event.kind, EventKind::UpdateRequest) {
            return false;
        }
        if let Some(hook) = self.hook.take() {
            self.running.set(true);
            hook(self);
            self.running.set(false);
        }
        true
    }
}

/// Attaches a probe running `hook` to `parent`; returns its id and its read counter.
fn attach_probe(parent: ObjectId, hook: Hook) -> (ObjectId, Rc<Cell<u32>>) {
    let reads = Rc::new(Cell::new(0));
    let probe = Box::new(Probe {
        data: ObjectData::new(ObjectId::next()),
        running: Cell::new(false),
        reads_while_running: Rc::clone(&reads),
        hook: Some(hook),
    });
    // SAFETY: `probe` is a fresh `Box` created here on the parent's thread and moved into the
    // call; the test keeps no reference to it (only the returned id and the `Rc` counter), so
    // nothing aliases it when its callback runs. The parent owns it from now on.
    let id = unsafe { add_owned_child(parent, probe) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));
    (id, reads)
}

fn attach_node(parent: ObjectId) -> ObjectId {
    // SAFETY: as in `attach_probe`: a fresh `Box<Node>` created on this thread, moved into the
    // call, never referenced by the test afterwards.
    unsafe { add_owned_child(parent, Box::new(Node::new())) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"))
}

fn run_callback(id: ObjectId) {
    assert!(dispatch_to_object(
        id,
        &mut Event::new(EventKind::UpdateRequest)
    ));
}

fn gone(id: ObjectId) -> bool {
    with_object(id, |_| ()).is_none()
}

#[test]
fn dropping_the_parent_from_the_childs_callback_does_not_read_the_child() {
    let slot: Rc<Cell<Option<OwnedObject<Node>>>> = Rc::new(Cell::new(None));
    let parent = Node::registered();
    let parent_id = OwnedObject::object_id(&parent);
    slot.set(Some(parent));
    let hook_slot = Rc::clone(&slot);
    let (probe_id, reads) = attach_probe(parent_id, Box::new(move |_| drop(hook_slot.take())));
    let sibling_id = attach_node(parent_id);

    run_callback(probe_id);

    assert_eq!(
        reads.get(),
        0,
        "the running child was read while its parent was dropped"
    );
    assert!(gone(parent_id) && gone(probe_id) && gone(sibling_id));
}

#[test]
fn moving_a_sibling_from_the_childs_callback_does_not_read_the_child() {
    let parent = Node::registered();
    let other = Node::registered();
    let (parent_id, other_id) = (
        OwnedObject::object_id(&parent),
        OwnedObject::object_id(&other),
    );
    let sibling = Rc::new(Cell::new(ObjectId(0)));
    let target = Rc::clone(&sibling);
    // Attached first, so a search of the parent's owned children meets it before the sibling.
    let (probe_id, reads) = attach_probe(
        parent_id,
        Box::new(move |_| {
            let moved = reparent_owned_object(target.get(), Some(other_id));
            assert!(
                matches!(moved, Ok(None)),
                "the sibling moves to the other parent"
            );
        }),
    );
    sibling.set(attach_node(parent_id));

    run_callback(probe_id);

    assert_eq!(
        reads.get(),
        0,
        "the running child was read while a sibling moved"
    );
    assert_eq!(parent.with(|p| p.data.owned_children().len()).unwrap(), 1);
    assert_eq!(
        other
            .with(|o| OwnedObject::object_id(&o.data.owned_children()[0]))
            .unwrap(),
        sibling.get()
    );
}

#[test]
fn reparenting_a_sibling_to_its_own_parent_from_the_childs_callback_does_not_read_the_child() {
    let parent = Node::registered();
    let parent_id = OwnedObject::object_id(&parent);
    let sibling = Rc::new(Cell::new(ObjectId(0)));
    let target = Rc::clone(&sibling);
    let (probe_id, reads) = attach_probe(
        parent_id,
        Box::new(move |_| {
            let kept = reparent_owned_object(target.get(), Some(parent_id));
            assert!(
                matches!(kept, Ok(None)),
                "nothing to do: it is owned there already"
            );
        }),
    );
    sibling.set(attach_node(parent_id));

    run_callback(probe_id);

    assert_eq!(
        reads.get(),
        0,
        "the running child was read while its parent was searched"
    );
    assert_eq!(parent.with(|p| p.data.owned_children().len()).unwrap(), 2);
}

#[test]
fn dropping_a_non_owned_sibling_from_the_childs_callback_does_not_read_the_child() {
    let parent = Node::registered();
    let parent_id = OwnedObject::object_id(&parent);
    let loose = Node::registered();
    let loose_id = OwnedObject::object_id(&loose);
    set_parent_by_id(loose_id, Some(parent_id)).unwrap();
    let slot = Rc::new(Cell::new(Some(loose)));
    let hook_slot = Rc::clone(&slot);
    let (probe_id, reads) = attach_probe(parent_id, Box::new(move |_| drop(hook_slot.take())));

    run_callback(probe_id);

    assert_eq!(
        reads.get(),
        0,
        "the running child was read while a sibling unlinked itself"
    );
    assert!(gone(loose_id));
    parent
        .with(|p| {
            assert!(!p.data.children().contains(&loose_id));
            assert!(p.data.children().contains(&probe_id));
        })
        .unwrap();
}
