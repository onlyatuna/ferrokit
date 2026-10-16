// SPDX-License-Identifier: LGPL-3.0-only
//! Signal emitter binding: block state and sender tracking.
//!
//! Qt reference (`qtbase/src/corelib/kernel/qobject.cpp`):
//! - `QObject::doActivate` reads the emitter's own `blockSig` flag (qobject.cpp:4273) and does not
//!   consult the global object registry, so an unregistered emitter can still block its signals.
//! - `QObject::blockSignals` only toggles that per-object flag.
//! - A signal emitted with no sender leaves `QObject::sender()` as `nullptr` for the slot, even when
//!   the slot is entered from an outer emission whose sender is set. Qt has no unbound signals;
//!   so this behaviour is ferrokit's own requirement.

use ferrokit_core::object::{
    register_qobject_ptr, unregister_qobject, ObjectData, ObjectId, QObject, SignalBlocker,
};
use ferrokit_core::signal::{sender, Signal};
use ferrokit_core::variant::Variant;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

/// A QObject owner whose signal is bound to its own `ObjectData`.
struct Owner {
    id: ObjectId,
    data: ObjectData,
    clicked: Signal<()>,
}

impl Owner {
    fn new(name: &str) -> Self {
        let id = ObjectId::next();
        let mut data = ObjectData::new(id);
        data.set_object_name(name);
        let clicked = Signal::with_owner(&data);
        Self { id, data, clicked }
    }
}

impl QObject for Owner {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

#[test]
fn unregistered_owner_block_suppresses_emit() {
    // An emitter that was never registered must still honour block_signals(true).
    let mut owner = Owner::new("never-registered");
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    owner.clicked.connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    owner.block_signals(true);
    owner.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "unregistered blocked emitter still emitted"
    );

    owner.block_signals(false);
    owner.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "unblocked emitter did not emit"
    );
}

#[test]
fn block_survives_unregister() {
    // (unregister half): unregistering must not clear the block flag of a live owner.
    let mut owner = Owner::new("unregistered-while-blocked");
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    owner.clicked.connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    owner.block_signals(true);
    let id = owner.id;
    // SAFETY: `owner` is a local that is not moved between register and unregister.
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(&raw mut owner));
        unregister_qobject(id);
    }
    owner.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "block lost after unregister"
    );
}

#[test]
fn signal_blocker_suppresses_unregistered_owner() {
    // RAII path (QSignalBlocker) on an unregistered owner.
    let mut owner = Owner::new("raii-unregistered");
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    owner.clicked.connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    {
        let _blocker = SignalBlocker::new(&mut owner);
        owner.clicked.emit(&());
    }
    owner.clicked.emit(&());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "SignalBlocker did not suppress unregistered owner emit"
    );
}

#[test]
fn unbound_signal_inside_bound_slot_has_no_sender() {
    // A signal with no emitter, emitted from inside a slot of a bound emitter, sees sender None.
    let outer = Owner::new("outer");
    let inner: Arc<Signal<()>> = Arc::new(Signal::new());
    let seen: Arc<Mutex<Vec<Option<ObjectId>>>> = Arc::new(Mutex::new(Vec::new()));

    let inner_c = Arc::clone(&inner);
    outer.clicked.connect(move |_| {
        inner_c.emit(&());
    });
    let s = Arc::clone(&seen);
    inner.connect(move |_| s.lock().unwrap().push(sender()));

    outer.clicked.emit(&());
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[None],
        "unbound inner signal saw outer sender"
    );
}

#[derive(ferrokit_core::QObject)]
#[qobject(class_name = "NotifyProbe")]
struct NotifyProbe {
    data: ObjectData,
    #[property(notify = "value_changed")]
    value: i64,
    #[signal]
    value_changed: Signal<i64>,
}

impl NotifyProbe {
    fn new() -> Self {
        Self {
            data: ObjectData::with_auto_id(),
            value: 0,
            value_changed: Signal::new(),
        }
    }
}

#[test]
fn derived_property_write_emits_notify_once_per_change() {
    // A notify property emits its notify signal when the value changes, and only then.
    let mut probe = NotifyProbe::new();
    let seen: Arc<Mutex<Vec<i64>>> = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    probe
        .value_changed
        .connect(move |v| s.lock().unwrap().push(*v));

    assert!(probe.set_property("value", Variant::I64(5)));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[5],
        "notify not emitted on change"
    );

    assert!(probe.set_property("value", Variant::I64(5)));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[5],
        "notify emitted for unchanged value"
    );

    assert!(probe.set_property("value", Variant::I64(7)));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[5, 7],
        "notify missing for second change"
    );
}

#[test]
fn derived_property_notify_honours_block_signals() {
    // Notify emission of a derived property is suppressed while the owner blocks signals.
    let mut probe = NotifyProbe::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    probe.value_changed.connect(move |_| {
        h.fetch_add(1, Ordering::SeqCst);
    });

    probe.block_signals(true);
    assert!(probe.set_property("value", Variant::I64(9)));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "derived notify emitted while blocked"
    );

    probe.block_signals(false);
    assert!(probe.set_property("value", Variant::I64(10)));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "derived notify missing after unblock"
    );
}
