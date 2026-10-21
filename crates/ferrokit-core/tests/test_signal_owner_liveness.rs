// SPDX-License-Identifier: LGPL-3.0-only
//! A signal bound to an owner (`Signal::with_owner`) checks its owner's liveness token before it
//! calls each slot. The first two sections below pin what follows from that, one for each way an
//! owner stops being alive; the third is the same check seen from other threads.
//!
//! 1. The owner is dropped before the emission starts. The emission calls no slot, however the
//!    slot was connected. Qt cannot emit a destroyed object's signal at all; PySide6 6.11.2
//!    refuses (`RuntimeError: Signal source has been deleted`) and calls no slot. A queued call
//!    that was posted while the owner was alive is still delivered, in Qt (measured with PySide6
//!    6.11.2) and here.
//! 2. The owner is still there, but `unregister_qobject` was called for it. Qt has no registry and
//!    no such step, so there is nothing to compare with: this is ferrokit's own rule, that
//!    unregistering retires an object (`is_alive` is false, `QPointer`s go null, timers stop,
//!    connections to it are cut) and that its signals stay silent from then on.
//!
//! Every test uses a concrete type, so that Miri checks the owner's use too.

use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    register_owned_qobject, register_qobject_ptr, unregister_qobject, ObjectData, OwnedObject,
    QObject, ThreadId,
};
use ferrokit_core::signal::{ConnectionType, Signal, SlotId};

/// An object whose signal is bound to its own `ObjectData`.
struct Owner {
    data: ObjectData,
    changed: Signal<i32>,
}

impl Owner {
    fn new() -> Self {
        let data = ObjectData::with_auto_id();
        let changed = Signal::with_owner(&data);
        Self { data, changed }
    }

    /// An owner that the registry knows, held by an owner of its own.
    fn registered() -> OwnedObject<Self> {
        // SAFETY: the `OwnedObject` stays on this thread; no callback of the owner runs.
        unsafe { register_owned_qobject(Self::new()) }
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

/// An object that only receives.
struct Receiver {
    data: ObjectData,
}

impl QObject for Receiver {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
}

fn receiver() -> OwnedObject<Receiver> {
    // SAFETY: the `OwnedObject` stays on this thread; no callback of the receiver runs.
    unsafe {
        register_owned_qobject(Receiver {
            data: ObjectData::with_auto_id(),
        })
    }
}

/// A slot and the number of times it ran.
fn counter() -> (Arc<AtomicUsize>, impl Fn(&i32) + Send + Sync + 'static) {
    let n = Arc::new(AtomicUsize::new(0));
    let slot = {
        let n = Arc::clone(&n);
        move |_: &i32| {
            n.fetch_add(1, Ordering::SeqCst);
        }
    };
    (n, slot)
}

fn runs(counters: &[(&'static str, Arc<AtomicUsize>)]) -> Vec<(&'static str, usize)> {
    counters
        .iter()
        .map(|(name, n)| (*name, n.load(Ordering::SeqCst)))
        .collect()
}

static UNIQUE_RUNS: AtomicUsize = AtomicUsize::new(0);

fn unique_slot(_: &i32) {
    UNIQUE_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// Connects one slot of every kind to `signal`, all bound to `receiver`'s id where the kind takes
/// a receiver. Returns what each of them counts. `UNIQUE_RUNS` is the one slot with no counter of
/// its own: only the test that calls this function reads it.
fn connect_every_kind(
    signal: &Signal<i32>,
    rcv: &Receiver,
) -> (
    Vec<(&'static str, Arc<AtomicUsize>)>,
    ferrokit_core::signal::ScopedConnection,
) {
    let here = ThreadId::current();
    let id = rcv.data.id();
    let mut counters = Vec::new();

    let (n, slot) = counter();
    signal.connect(slot);
    counters.push(("connect", n));

    let (n, slot) = counter();
    signal.connect_direct_object(id, slot);
    counters.push(("connect_direct_object", n));

    for (name, kind) in [
        ("connect_object Direct", ConnectionType::Direct),
        ("connect_object Auto", ConnectionType::Auto),
        ("connect_object Queued", ConnectionType::Queued),
    ] {
        let (n, slot) = counter();
        signal.connect_object(id, here, kind, slot);
        counters.push((name, n));
    }

    let (n, slot) = counter();
    signal.connect_single_shot_to(rcv, slot);
    counters.push(("connect_single_shot_to", n));

    signal
        .connect_unique_with_key(rcv, SlotId::next(), unique_slot)
        .expect("the unique connection is made");

    let (n, slot) = counter();
    let scoped = signal.connect_scoped(slot);
    counters.push(("connect_scoped", n));

    (counters, scoped)
}

// ---------------------------------------------------------------------------------------------
// 1. The owner is dropped before the emission starts
// ---------------------------------------------------------------------------------------------

#[test]
fn a_live_owner_reaches_a_slot_of_every_kind() {
    // The control for the next test: the same connections do run while the owner is alive.
    UNIQUE_RUNS.store(0, Ordering::SeqCst);
    let el = EventLoop::new();
    let rcv = receiver();
    let owner = Owner::registered();
    let changed = owner.with(|o| o.changed.clone()).unwrap();
    let (counters, _scoped) = rcv.with(|r| connect_every_kind(&changed, r)).unwrap();

    changed.emit(&1);
    el.process_events(false);

    for (name, n) in runs(&counters) {
        assert_eq!(n, 1, "{name} ran {n} times for a live owner");
    }
    assert_eq!(UNIQUE_RUNS.load(Ordering::SeqCst), 1, "unique slot");
}

#[test]
fn an_emission_on_a_dropped_owner_calls_no_slot_however_it_was_connected() {
    UNIQUE_RUNS.store(0, Ordering::SeqCst);
    let el = EventLoop::new();
    let rcv = receiver();
    let owner = Owner::registered();
    let handle = owner.with(|o| o.changed.clone()).unwrap();
    let (counters, _scoped) = rcv.with(|r| connect_every_kind(&handle, r)).unwrap();

    drop(owner);
    handle.emit(&1);
    el.process_events(false); // a queued call posted by the emission would run here

    for (name, n) in runs(&counters) {
        assert_eq!(n, 0, "{name} ran {n} times for a dropped owner");
    }
    assert_eq!(UNIQUE_RUNS.load(Ordering::SeqCst), 0, "unique slot");
}

#[test]
fn an_emission_on_a_dropped_owner_that_was_never_registered_calls_no_slot() {
    let owner = Owner::new();
    let handle = owner.changed.clone();
    let (n, slot) = counter();
    handle.connect(slot);

    drop(owner);
    handle.emit(&1);

    assert_eq!(n.load(Ordering::SeqCst), 0);
}

#[test]
fn a_signal_without_an_owner_runs_whatever_happens_to_other_objects() {
    let signal: Signal<i32> = Signal::new();
    let (n, slot) = counter();
    signal.connect(slot);
    drop(Owner::registered());

    signal.emit(&1);

    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn the_signal_of_a_live_owner_is_not_affected_by_the_drop_of_another_owner() {
    let first = Owner::registered();
    let second = Owner::registered();
    let (n, slot) = counter();
    second.with(|s| s.changed.connect(slot)).unwrap();

    drop(first);
    second.with(|s| s.changed.emit(&1)).unwrap();

    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn a_queued_call_posted_while_the_owner_was_alive_is_still_delivered_after_it_is_dropped() {
    // Qt: a posted `QMetaCallEvent` is delivered although the sender has been deleted since
    // (PySide6 6.11.2: the slot runs once).
    let el = EventLoop::new();
    let rcv = receiver();
    let owner = Owner::registered();
    let (n, slot) = counter();
    let changed = owner.with(|o| o.changed.clone()).unwrap();
    changed.connect_object(
        OwnedObject::object_id(&rcv),
        ThreadId::current(),
        ConnectionType::Queued,
        slot,
    );

    changed.emit(&1);
    assert_eq!(n.load(Ordering::SeqCst), 0, "queued: not yet");
    drop(owner);
    el.process_events(false);

    assert_eq!(n.load(Ordering::SeqCst), 1);
}

/// A slot that is meant to run in the middle of an emission and drop the owner it belongs to. The
/// closure must be `Send + Sync` and holds a thread-bound value; every use is on one thread.
struct Dropper(Rc<Cell<Option<OwnedObject<Owner>>>>);
unsafe impl Send for Dropper {}
unsafe impl Sync for Dropper {}

impl Dropper {
    fn drop_the_owner(&self) {
        drop(self.0.take());
    }
}

#[test]
fn an_emission_started_inside_a_slot_on_a_dropped_owner_ends_there_and_the_outer_one_goes_on() {
    let outer = Owner::registered();
    let gone = Owner::registered();
    let gone_handle = gone.with(|g| g.changed.clone()).unwrap();
    let (gone_runs, slot) = counter();
    gone_handle.connect(slot);
    drop(gone);

    let (second_runs, second) = counter();
    {
        let gone_handle = gone_handle.clone();
        outer.with(|o| o.changed.connect(move |_| gone_handle.emit(&2))).unwrap();
    }
    outer.with(|o| o.changed.connect(second)).unwrap();

    outer.with(|o| o.changed.emit(&1)).unwrap();

    assert_eq!(
        gone_runs.load(Ordering::SeqCst),
        0,
        "the dropped owner's slot"
    );
    assert_eq!(
        second_runs.load(Ordering::SeqCst),
        1,
        "the outer emission goes on after the nested one ended"
    );
}

#[test]
fn a_slot_that_drops_the_owner_and_emits_again_reaches_nothing() {
    let owner = Owner::registered();
    let handle = owner.with(|o| o.changed.clone()).unwrap();
    let cell = Dropper(Rc::new(Cell::new(Some(owner))));
    let first_runs = Arc::new(AtomicUsize::new(0));
    let (later_runs, later) = counter();
    {
        let handle = handle.clone();
        let first_runs = Arc::clone(&first_runs);
        handle.clone().connect(move |_| {
            // Re-emit only on the first run, so that a regression fails the assertions below
            // instead of recursing until the stack overflows.
            if first_runs.fetch_add(1, Ordering::SeqCst) == 0 {
                cell.drop_the_owner();
                handle.emit(&2); // the owner is gone: nothing runs, this slot included
            }
        });
    }
    handle.connect(later);

    handle.emit(&1);

    assert_eq!(
        first_runs.load(Ordering::SeqCst),
        1,
        "the slot that dropped the owner"
    );
    assert_eq!(later_runs.load(Ordering::SeqCst), 0, "the slot after it");

    // The first slot holds a handle to the signal it is connected to. Without this the signal
    // and the slot keep each other alive, and Miri reports the leak when the process ends.
    handle.disconnect_all();
}

#[test]
fn an_emission_from_another_thread_after_the_owner_was_dropped_calls_no_slot() {
    let owner = Owner::registered();
    let handle = owner.with(|o| o.changed.clone()).unwrap();
    let (n, slot) = counter();
    handle.connect(slot);

    drop(owner);
    let worker = {
        let handle = handle.clone();
        std::thread::spawn(move || handle.emit(&1))
    };
    worker.join().expect("the emitting thread does not panic");

    assert_eq!(n.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------------------------
// 2. The owner exists, but it was unregistered
// ---------------------------------------------------------------------------------------------

/// Registers a local `Owner` by a pointer taken with `&raw mut`, so that the test may keep using
/// the variable between callbacks.
macro_rules! register_in_place {
    ($owner:ident) => {
        // SAFETY: the variable stays where it is until the test ends, no callback of the object
        // runs, and the pointer is taken without making a reference.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(&raw mut $owner)) }
    };
}

#[test]
fn unregistering_an_owner_that_still_exists_silences_its_signal() {
    let mut owner = Owner::new();
    let id = owner.data.id();
    register_in_place!(owner);
    let (n, slot) = counter();
    owner.changed.connect(slot);
    owner.data.set_object_name("still here");

    owner.changed.emit(&1);
    assert_eq!(n.load(Ordering::SeqCst), 1, "registered: the slot runs");

    // SAFETY: no callback of the owner runs.
    unsafe { unregister_qobject(id) };
    owner.changed.emit(&2);

    assert_eq!(
        n.load(Ordering::SeqCst),
        1,
        "unregistered: the slot does not run"
    );
    assert!(
        !owner.data.is_alive(),
        "an unregistered object is not alive"
    );
    assert_eq!(
        owner.data.object_name(),
        Some("still here"),
        "the object itself is intact"
    );
}

#[test]
fn an_owner_that_was_never_registered_emits_and_unregistering_an_unknown_id_changes_nothing() {
    let owner = Owner::new();
    let (n, slot) = counter();
    owner.changed.connect(slot);

    // SAFETY: no object has this id, so there is no callback to overlap.
    unsafe { unregister_qobject(owner.data.id()) };
    owner.changed.emit(&1);

    assert_eq!(n.load(Ordering::SeqCst), 1);
    assert!(owner.data.is_alive());
}

#[test]
fn an_unregistered_object_stays_retired_when_it_is_registered_again() {
    // The liveness token only ever goes from alive to gone, as a `QPointer` only ever goes null:
    // registering the same object again does not bring it back.
    let mut owner = Owner::new();
    let id = owner.data.id();
    register_in_place!(owner);
    let (n, slot) = counter();
    owner.changed.connect(slot);
    // SAFETY: no callback of the owner runs.
    unsafe { unregister_qobject(id) };

    register_in_place!(owner);
    owner.changed.emit(&1);

    assert_eq!(n.load(Ordering::SeqCst), 0);
    assert!(!owner.data.is_alive());
    // SAFETY: no callback of the owner runs.
    unsafe { unregister_qobject(id) };
}

#[test]
fn a_slot_that_unregisters_its_owner_ends_the_emission() {
    let mut owner = Owner::new();
    let id = owner.data.id();
    register_in_place!(owner);
    let (first_runs, first_counter) = counter();
    let (later_runs, later) = counter();
    owner.changed.connect(move |value| {
        first_counter(value);
        // SAFETY: the slot is not a callback of the owner, so no callback of it runs.
        unsafe { unregister_qobject(id) };
    });
    owner.changed.connect(later);

    owner.changed.emit(&1);
    owner.changed.emit(&2);

    assert_eq!(
        first_runs.load(Ordering::SeqCst),
        1,
        "the slot that unregistered the owner"
    );
    assert_eq!(
        later_runs.load(Ordering::SeqCst),
        0,
        "the slot after it, in both emissions"
    );
}

#[test]
fn an_emission_started_inside_a_slot_on_an_unregistered_owner_ends_there_and_the_outer_one_goes_on()
{
    let outer = Owner::registered();
    let mut retired = Owner::new();
    let retired_id = retired.data.id();
    register_in_place!(retired);
    let (retired_runs, slot) = counter();
    retired.changed.connect(slot);
    // SAFETY: no callback of the owner runs.
    unsafe { unregister_qobject(retired_id) };

    let (second_runs, second) = counter();
    {
        let handle = retired.changed.clone();
        outer.with(|o| o.changed.connect(move |_| handle.emit(&2))).unwrap();
    }
    outer.with(|o| o.changed.connect(second)).unwrap();

    outer.with(|o| o.changed.emit(&1)).unwrap();

    assert_eq!(
        retired_runs.load(Ordering::SeqCst),
        0,
        "the unregistered owner's slot"
    );
    assert_eq!(
        second_runs.load(Ordering::SeqCst),
        1,
        "the outer emission goes on"
    );
}

#[test]
fn an_emission_from_another_thread_after_the_unregistration_calls_no_slot() {
    let mut owner = Owner::new();
    let id = owner.data.id();
    register_in_place!(owner);
    let (n, slot) = counter();
    owner.changed.connect(slot);
    // SAFETY: no callback of the owner runs.
    unsafe { unregister_qobject(id) };

    let handle = owner.changed.clone();
    std::thread::spawn(move || handle.emit(&1))
        .join()
        .expect("the emitting thread does not panic");

    assert_eq!(n.load(Ordering::SeqCst), 0);
}
