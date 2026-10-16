// SPDX-License-Identifier: LGPL-3.0-only
//! Reproduction, not a regression test: `cargo test -p ferrokit-core --test
//! test_parked_ancestor_frees_running_descendant -- --ignored`.
//!
//! An object `leaf` runs a callback that dispatches to its parent `mid`; `mid`'s callback drops
//! `mid`'s own parent `root`. `mid` is inside a callback, so `root` parks it and unregisters
//! everything below it, `leaf` included, but `leaf`'s callback is still on the stack, below
//! `mid`'s. When `mid`'s callback returns, `mid` is freed, and it decides about `leaf` with
//! `registered_and_borrowed`, which is false for an unregistered object even while its callback
//! runs. `leaf` is then freed under its own callback.
//!
//! The test fails while that is so. The existing test for a nested callback covers the opposite
//! nesting, where the descendant is the innermost callback.

use std::cell::Cell;
use std::rc::Rc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, dispatch_to_object, register_owned_qobject, ObjectData, ObjectId,
    OwnedObject, QObject,
};

struct Root {
    data: ObjectData,
}

impl QObject for Root {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::UpdateRequest) {
            drop(self.root.take());
        }
        true
    }
}

/// Dispatches to `mid`, then reports whether it was freed meanwhile. After the dispatch it uses
/// only values it copied out before, never `self`.
struct Leaf {
    data: ObjectData,
    mid: ObjectId,
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
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        let mid = self.mid;
        let freed = Rc::clone(&self.freed);
        let report = Rc::clone(&self.freed_during_its_callback);
        dispatch_to_object(mid, &mut Event::new(EventKind::UpdateRequest));
        report.set(Some(freed.get()));
        true
    }
}

#[test]
#[ignore = "reproduction of an open defect: the descendant is freed under its own callback"]
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
    let root_id = root.data.id;
    root_cell.set(Some(root));

    let mid = Box::new(Mid {
        data: ObjectData::new(ObjectId::next()),
        root: Rc::clone(&root_cell),
    });
    let mid_id = mid.data.id;
    // SAFETY: the parent owns the child from here on and is dropped on this thread.
    unsafe { add_owned_child(root_id, mid) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));

    let leaf = Box::new(Leaf {
        data: ObjectData::new(ObjectId::next()),
        mid: mid_id,
        freed: Rc::clone(&freed),
        freed_during_its_callback: Rc::clone(&report),
    });
    let leaf_id = leaf.data.id;
    // SAFETY: as above.
    unsafe { add_owned_child(mid_id, leaf) }.unwrap_or_else(|(e, _)| panic!("{e:?}"));

    dispatch_to_object(leaf_id, &mut Event::new(EventKind::UpdateRequest));

    assert_eq!(
        report.get(),
        Some(false),
        "the leaf was freed while its own callback was still running"
    );
    assert!(freed.get(), "the leaf is freed once its callback has returned");
}
