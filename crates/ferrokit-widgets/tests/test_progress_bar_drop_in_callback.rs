// SPDX-License-Identifier: LGPL-3.0-only
//! A `ProgressBar` dropped while one of its own callbacks runs.
//!
//! `impl Drop for ProgressBar` calls `unregister_qobject`, which needs the bar's memory to stay
//! valid until every callback of the bar that is running has returned. Who drops a registered bar
//! decides whether that holds:
//! - an [`OwnedObject`] (`register_owned_qobject`, `add_owned_child`) unregisters the bar itself
//!   when it is dropped and parks the allocation while a callback runs, so the bar's own `Drop`
//!   runs after that callback and finds nothing registered. The first two tests drop the owner
//!   from a slot of the bar (inside `set_value`, which goes on to use `self` afterwards) and drop
//!   the parent of an owned bar from such a slot; under Miri a freed-too-early bar is reported.
//! - any other owner registered the bar with `register_qobject_ptr`, whose contract says the owner
//!   never drops the object during a callback of it. The last test, a control, breaks that on
//!   purpose: under Miri it must be reported as a use after free, so `Drop` cannot be what makes
//!   it safe.
//!
//! The slots below are plain closures, so the owners they drop live in thread-locals.

use std::cell::Cell;
use std::ptr::NonNull;

use ferrokit_core::object::{
    add_owned_child, query_object_thread, register_owned_qobject, register_qobject_ptr,
    with_object_mut, ObjectData, ObjectId, OwnedObject, QObject,
};
use ferrokit_widgets::progress_bar::ProgressBar;
use ferrokit_widgets::widget::Widget;

thread_local! {
    static BAR_OWNER: Cell<Option<OwnedObject<ProgressBar>>> = const { Cell::new(None) };
    static PARENT_OWNER: Cell<Option<OwnedObject<Parent>>> = const { Cell::new(None) };
    static RAW_BAR: Cell<Option<NonNull<ProgressBar>>> = const { Cell::new(None) };
    /// What the slot saw right after it dropped the owner: `(self unregistered, other unregistered)`.
    static SEEN: Cell<Option<(bool, bool)>> = const { Cell::new(None) };
}

struct Parent {
    data: ObjectData,
}

impl Parent {
    fn new() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
        }
    }
}

impl QObject for Parent {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn unregistered(id: ObjectId) -> bool {
    query_object_thread(id).is_none()
}

/// Runs `ProgressBar::set_value` as a callback of the bar with `id`: the registry holds the bar's
/// borrow flag while it runs, as it does for an event or a queued signal.
fn set_value_in_callback(id: ObjectId, value: i32) {
    with_object_mut(id, |o| {
        o.as_qobject_any_mut()
            .and_then(|any| any.downcast_mut::<ProgressBar>())
            .expect("a ProgressBar")
            .set_value(value);
    })
    .expect("the bar is registered and not borrowed");
}

#[test]
fn an_owned_bar_dropped_by_its_own_slot_is_freed_after_the_callback() {
    // SAFETY: the owner stays on this thread; dropping it inside a callback of the bar parks the
    // allocation until that callback returns (`register_owned_qobject`).
    let owner = unsafe { register_owned_qobject(ProgressBar::new()) };
    let id = OwnedObject::object_id(&owner);
    owner
        .with(|bar| {
            bar.value_changed.connect_direct_object(id, move |_| {
                drop(BAR_OWNER.take());
                SEEN.set(Some((unregistered(id), false)));
            })
        })
        .expect("the bar is not borrowed");
    BAR_OWNER.set(Some(owner));

    set_value_in_callback(id, 5);

    assert_eq!(
        SEEN.take(),
        Some((true, false)),
        "the slot ran, and the bar was already unregistered inside it"
    );
    assert!(unregistered(id));
    assert!(BAR_OWNER.take().is_none());
}

#[test]
fn the_parent_of_an_owned_bar_dropped_by_a_slot_of_the_bar_frees_it_after_the_callback() {
    // SAFETY: the owner stays on this thread; its drop parks the allocation of any child whose
    // callback runs.
    let parent = unsafe { register_owned_qobject(Parent::new()) };
    let parent_id = OwnedObject::object_id(&parent);

    let bar = ProgressBar::new();
    let bar_id = bar.id();
    bar.value_changed.connect_direct_object(bar_id, move |_| {
        drop(PARENT_OWNER.take());
        SEEN.set(Some((unregistered(bar_id), unregistered(parent_id))));
    });
    // SAFETY: `parent` owns the bar on this thread, and the bar is used only through the registry.
    unsafe { add_owned_child(parent_id, Box::new(bar)) }
        .unwrap_or_else(|(err, _)| panic!("{err:?}"));
    PARENT_OWNER.set(Some(parent));

    set_value_in_callback(bar_id, 5);

    assert_eq!(
        SEEN.take(),
        Some((true, true)),
        "the slot ran, and the parent and the bar were unregistered inside it"
    );
    assert!(unregistered(bar_id) && unregistered(parent_id));
    assert!(PARENT_OWNER.take().is_none());
}

/// Expected: undefined behavior under Miri, in both aliasing models. A bar registered with
/// `register_qobject_ptr` and freed by its own slot is a use after free once `set_value` goes on
/// with `self`; the owner broke the registration contract, and `ProgressBar::drop` cannot help.
#[test]
#[cfg_attr(
    not(miri),
    ignore = "Miri control: expected to be reported as undefined behavior under Miri"
)]
fn control_a_pointer_registered_bar_dropped_by_its_own_slot() {
    let raw = Box::into_non_null(Box::new(ProgressBar::new()));
    // SAFETY: `raw` is a live, unmoved allocation made just above.
    let id = unsafe { raw.as_ref() }.id();
    // SAFETY: this breaks the contract on purpose; see the documentation of this test.
    unsafe { register_qobject_ptr(raw) };
    // SAFETY: no callback of the bar runs.
    unsafe { raw.as_ref() }
        .value_changed
        .connect_direct_object(id, |_| {
            let raw = RAW_BAR.take().expect("set before the callback");
            // SAFETY: none; this is the violation the control shows.
            drop(unsafe { Box::from_raw(raw.as_ptr()) });
        });
    RAW_BAR.set(Some(raw));

    set_value_in_callback(id, 5);
}
