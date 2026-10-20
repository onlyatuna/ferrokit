// SPDX-License-Identifier: LGPL-3.0-only
//! Regression tests for parked-ancestor descendant lifetime and `find_child` borrow checks.
//!
//! 1. An object `leaf` runs a callback that dispatches to its parent `mid`; `mid`'s callback drops
//!    `mid`'s own parent `root`. `mid` is inside a callback, so `root` parks it and unregisters
//!    everything below it, `leaf` included, while `leaf`'s callback is still on the stack below
//!    `mid`'s. When `mid`'s callback returns and `mid` is freed, `leaf` must remain parked until
//!    `leaf`'s own callback returns, because `OwnedObject::is_borrowed` tracks the callback borrow
//!    independently of registry membership.
//! 2. While `leaf`'s callback holds `&mut Leaf` on the stack and dispatches to `mid`, `mid` calling
//!    `find_child` / `find_children` / `find_child_id` must not dereference `leaf` through
//!    `owned_children`.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, dispatch_to_object, register_owned_qobject, ObjectData, ObjectId, OwnedObject,
    QObject, QObjectExt,
};

struct Root {
    data: ObjectData,
}

impl QObject for Root {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

/// Drops `root` from inside its own callback.
struct Mid {
    data: ObjectData,
    root: Rc<Cell<Option<OwnedObject<Root>>>>,
}

impl QObject for Mid {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            drop(self.root.take());
        }
        true
    }
}

/// Dispatches to `mid`, then writes to `self` and reports whether it was freed meanwhile.
struct Leaf {
    data: ObjectData,
    mid: ObjectId,
    survived_after_nested_dispatch: bool,
    freed: Rc<Cell<bool>>,
    freed_during_its_callback: Rc<Cell<Option<bool>>>,
}

impl Drop for Leaf {
    fn drop(&mut self) {
        self.freed.set(true);
    }
}

impl QObject for Leaf {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        let mid = self.mid;
        let freed = Rc::clone(&self.freed);
        let report = Rc::clone(&self.freed_during_its_callback);
        dispatch_to_object(mid, &mut Event::new(EventKind::UpdateRequest));
        self.survived_after_nested_dispatch = true;
        report.set(Some(freed.get()));
        true
    }
}

#[test]
fn a_descendant_inside_its_callback_is_not_freed_when_its_ancestor_is_dropped_below_it() {
    let root_cell = Rc::new(Cell::new(None));
    let freed = Rc::new(Cell::new(false));
    let report = Rc::new(Cell::new(None));

    // SAFETY: the owner stays on this thread and is dropped when no callback of it runs.
    let root = unsafe {
        register_owned_qobject(Root {
            data: ObjectData::new(ObjectId::next()),
        })
    };
    let root_id = OwnedObject::object_id(&root);
    root_cell.set(Some(root));

    let mid = Box::new(Mid {
        data: ObjectData::new(ObjectId::next()),
        root: Rc::clone(&root_cell),
    });
    let mid_id = mid.data.id();
    // SAFETY: the parent owns the child from here on and is dropped on this thread.
    unsafe { add_owned_child(root_id, mid) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));

    let leaf = Box::new(Leaf {
        data: ObjectData::new(ObjectId::next()),
        mid: mid_id,
        survived_after_nested_dispatch: false,
        freed: Rc::clone(&freed),
        freed_during_its_callback: Rc::clone(&report),
    });
    let leaf_id = leaf.data.id();
    // SAFETY: as above.
    unsafe { add_owned_child(mid_id, leaf) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));

    dispatch_to_object(leaf_id, &mut Event::new(EventKind::UpdateRequest));

    assert_eq!(
        report.get(),
        Some(false),
        "the leaf was freed while its own callback was still running"
    );
    assert!(
        freed.get(),
        "the leaf is freed once its callback has returned"
    );
}

struct SearchingParent {
    data: ObjectData,
    found_while_child_running: Rc<Cell<Option<bool>>>,
}

impl QObject for SearchingParent {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            let found_ref = self
                .with_child::<RunningChild, _>("running", |_| ())
                .is_some();
            let found_id = self
                .find_child_id("running", std::any::TypeId::of::<RunningChild>())
                .is_some();
            let mut count = 0;
            self.for_each_child::<RunningChild>(Some("running"), |_| count += 1);
            let found_vec = count != 0;
            let found_mut = self
                .with_child_mut::<RunningChild, _>("running", |_| ())
                .is_some();
            self.found_while_child_running
                .set(Some(found_ref || found_id || found_vec || found_mut));
        }
        true
    }
}

struct RunningChild {
    data: ObjectData,
    parent_id: ObjectId,
    steps: usize,
}

impl QObject for RunningChild {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        self.steps += 1;
        dispatch_to_object(self.parent_id, &mut Event::new(EventKind::UpdateRequest));
        self.steps += 1;
        true
    }
}

#[test]
fn find_child_on_parent_skips_owned_child_whose_callback_is_running() {
    let found = Rc::new(Cell::new(None));
    // SAFETY: the owner stays on this thread and is dropped after callbacks finish.
    let parent = unsafe {
        register_owned_qobject(SearchingParent {
            data: ObjectData::new(ObjectId::next()),
            found_while_child_running: Rc::clone(&found),
        })
    };
    let parent_id = OwnedObject::object_id(&parent);

    let mut child_data = ObjectData::new(ObjectId::next());
    child_data.set_object_name("running");
    let child = Box::new(RunningChild {
        data: child_data,
        parent_id,
        steps: 0,
    });
    let child_id = child.data.id();
    // SAFETY: parent owns the child on this thread.
    unsafe { add_owned_child(parent_id, child) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));

    assert!(
        parent
            .with(|p| p.with_child::<RunningChild, _>("running", |_| ()).is_some())
            .unwrap(),
        "between callbacks find_child locates the owned child"
    );

    dispatch_to_object(child_id, &mut Event::new(EventKind::UpdateRequest));
    assert_eq!(
        found.get(),
        Some(false),
        "while the child's callback holds &mut self, find_child must skip it instead of aliasing"
    );
    assert_eq!(
        parent
            .with(|p| p.with_child::<RunningChild, _>("running", |c| c.steps))
            .unwrap(),
        Some(2),
        "after the callback returns, find_child locates the child again"
    );
}
