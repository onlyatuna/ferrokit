// SPDX-License-Identifier: LGPL-3.0-only
//! A slot destroys the object whose signal is being emitted.
//!
//! Qt stops such an emission: `~QObject` disconnects every receiver and zeroes the connection
//! counter before it deletes the children (qobject.cpp:1086-1178), and `doActivate` leaves its
//! loop once the counter is 0 (qobject.cpp:4431). The slots after the one that destroyed the
//! sender are not called, whichever way they were connected. PySide6 behaves the same.
//!
//! A bound signal here carries the liveness token of its owner and checks it before it calls each
//! slot. The tests emit through a clone of the signal, which is what keeps the signal usable after
//! its owner is freed.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::object::ThreadId;
use ferrokit_core::object::{
    add_owned_child, register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject,
};
use ferrokit_core::signal::{ConnectionType, Signal};

type Calls = Arc<Mutex<Vec<&'static str>>>;

fn calls() -> Calls {
    Calls::default()
}

fn seen(calls: &Calls) -> Vec<&'static str> {
    calls.lock().unwrap().clone()
}

/// Records that the slot `name` ran.
fn record(calls: &Calls, name: &'static str) -> impl Fn(&String) + Send + Sync + 'static {
    let calls = Arc::clone(calls);
    move |_| calls.lock().unwrap().push(name)
}

/// A handle that lets a closure that must be `Send + Sync` hold a thread-bound value. Every use
/// in this file is on one thread.
struct ThreadBound<T>(T);
unsafe impl<T> Send for ThreadBound<T> {}
unsafe impl<T> Sync for ThreadBound<T> {}
impl<T> ThreadBound<T> {
    fn get(&self) -> &T {
        &self.0
    }
}

struct Node {
    data: ObjectData,
    freed: Arc<AtomicBool>,
    on_drop: Option<Box<dyn FnOnce()>>,
}

impl Node {
    fn new() -> Box<Self> {
        Box::new(Self::unboxed())
    }

    fn unboxed() -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            freed: Arc::new(AtomicBool::new(false)),
            on_drop: None,
        }
    }

    fn registered() -> OwnedObject<Self> {
        // SAFETY: the owner stays on this thread and is dropped, which unregisters the object,
        // when no callback of it runs.
        unsafe { register_owned_qobject(Self::unboxed()) }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.freed.store(true, Ordering::SeqCst);
        if let Some(hook) = self.on_drop.take() {
            hook();
        }
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
}

/// A sender: an owned child of a parent, with a handle to its own signal. Dropping the parent
/// destroys the sender.
struct Sender {
    owner: ThreadBound<Rc<RefCell<Option<OwnedObject<Node>>>>>,
    signal: Signal<String>,
    freed: Arc<AtomicBool>,
}

impl Sender {
    fn new() -> Self {
        let parent = Node::registered();
        let parent_id = parent.data.id();
        let child = Node::new();
        let signal = child.data.object_name_changed().clone();
        let freed = Arc::clone(&child.freed);
        // SAFETY: the parent owns the Box from here on and is dropped on this thread.
        unsafe { add_owned_child(parent_id, child) }
            .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));
        Self {
            owner: ThreadBound(Rc::new(RefCell::new(Some(parent)))),
            signal,
            freed,
        }
    }

    fn emit(&self) {
        self.signal.emit(&String::new());
    }

    fn is_freed(&self) -> bool {
        self.freed.load(Ordering::SeqCst)
    }

    /// A slot that records `name` and then destroys this sender.
    fn destroyer(
        &self,
        calls: &Calls,
        name: &'static str,
    ) -> impl Fn(&String) + Send + Sync + 'static {
        let owner = ThreadBound(Rc::clone(self.owner.get()));
        let calls = Arc::clone(calls);
        move |_| {
            calls.lock().unwrap().push(name);
            drop(owner.get().borrow_mut().take());
        }
    }
}

/// A slot that records `name` and then emits `other`.
fn emit_then(
    calls: &Calls,
    name: &'static str,
    other: &Signal<String>,
) -> impl Fn(&String) + Send + Sync + 'static {
    let calls = Arc::clone(calls);
    let other = other.clone();
    move |_| {
        calls.lock().unwrap().push(name);
        other.emit(&String::new());
    }
}

#[test]
fn slots_after_the_one_that_destroyed_the_sender_are_not_called() {
    let calls = calls();
    let sender = Sender::new();
    let observer = Node::registered();
    sender.signal.connect(sender.destroyer(&calls, "destroy"));
    sender.signal.connect(record(&calls, "plain after"));
    sender.signal.connect_object(
        observer.data.id(),
        ThreadId::current(),
        ConnectionType::Direct,
        record(&calls, "object after"),
    );
    sender.signal.connect_object(
        observer.data.id(),
        ThreadId::current(),
        ConnectionType::Auto,
        record(&calls, "auto after"),
    );

    sender.emit();

    assert_eq!(seen(&calls), vec!["destroy"]);
    assert!(sender.is_freed());
}

#[test]
fn every_slot_runs_when_no_slot_destroys_the_sender() {
    let calls = calls();
    let sender = Sender::new();
    let observer = Node::registered();
    sender.signal.connect(record(&calls, "plain"));
    sender.signal.connect_object(
        observer.data.id(),
        ThreadId::current(),
        ConnectionType::Direct,
        record(&calls, "object"),
    );
    sender.signal.connect(record(&calls, "plain again"));

    sender.emit();
    sender.emit();

    assert_eq!(
        seen(&calls),
        vec![
            "plain",
            "object",
            "plain again",
            "plain",
            "object",
            "plain again"
        ]
    );
    assert!(!sender.is_freed());
}

#[test]
fn destroying_another_object_does_not_stop_the_emission() {
    let calls = calls();
    let sender = Sender::new();
    let other = Sender::new();
    sender
        .signal
        .connect(other.destroyer(&calls, "destroy other"));
    sender.signal.connect(record(&calls, "after"));

    sender.emit();

    assert_eq!(seen(&calls), vec!["destroy other", "after"]);
    assert!(other.is_freed());
    assert!(!sender.is_freed());
}

#[test]
fn a_nested_emission_that_destroys_the_outer_sender_ends_the_outer_emission() {
    let calls = calls();
    let outer = Sender::new();
    let inner = Sender::new();
    outer
        .signal
        .connect(emit_then(&calls, "outer first", &inner.signal));
    outer.signal.connect(record(&calls, "outer second"));
    inner
        .signal
        .connect(outer.destroyer(&calls, "inner destroys outer"));
    inner.signal.connect(record(&calls, "inner second"));

    outer.emit();

    assert_eq!(
        seen(&calls),
        vec!["outer first", "inner destroys outer", "inner second"],
        "the inner sender is alive, so its emission goes on; the outer one is over"
    );
    assert!(outer.is_freed());
    assert!(!inner.is_freed());
}

#[test]
fn a_nested_emission_that_destroys_the_inner_sender_ends_only_the_inner_emission() {
    let calls = calls();
    let outer = Sender::new();
    let inner = Sender::new();
    outer
        .signal
        .connect(emit_then(&calls, "outer first", &inner.signal));
    outer.signal.connect(record(&calls, "outer second"));
    inner
        .signal
        .connect(inner.destroyer(&calls, "inner destroys itself"));
    inner.signal.connect(record(&calls, "inner second"));

    outer.emit();

    assert_eq!(
        seen(&calls),
        vec!["outer first", "inner destroys itself", "outer second"]
    );
    assert!(inner.is_freed());
    assert!(!outer.is_freed());
}

#[test]
fn a_slot_that_emits_its_own_signal_again_and_a_later_slot_that_destroys_the_sender_end_both_emissions(
) {
    let calls = calls();
    let sender = Sender::new();
    let again = Arc::new(AtomicBool::new(false));
    {
        let calls = Arc::clone(&calls);
        let signal = sender.signal.clone();
        sender.signal.connect(move |_| {
            calls.lock().unwrap().push("first");
            if !again.swap(true, Ordering::SeqCst) {
                signal.emit(&String::new());
            }
        });
    }
    sender.signal.connect(sender.destroyer(&calls, "destroy"));
    sender.signal.connect(record(&calls, "last"));

    sender.emit();

    assert_eq!(seen(&calls), vec!["first", "first", "destroy"]);
    // The first slot holds a handle to the signal it is connected to; that reference cycle is the
    // test's own, so it is cut here.
    sender.signal.disconnect_all();
}

#[test]
fn a_signal_handle_does_not_keep_its_owner_alive() {
    let sender = Sender::new();
    let handle = sender.signal.clone();

    drop(sender.owner.get().borrow_mut().take());

    assert!(
        sender.is_freed(),
        "the owner is freed although a handle to its signal exists"
    );
    drop(handle);
}

#[test]
fn a_signal_emitted_while_its_owner_is_being_destroyed_reaches_no_slot() {
    // `~QObject` has cut every connection before it deletes the children, so a child that
    // emits its parent's signal from its own destructor reaches nothing (qobject.cpp:1086-1178).
    let calls = calls();
    let parent = Node::registered();
    let parent_id = parent.data.id();
    let signal = parent.data.object_name_changed().clone();
    signal.connect(record(&calls, "slot of the parent's signal"));
    let mut child = Node::new();
    let emitter = signal.clone();
    child.on_drop = Some(Box::new(move || emitter.emit(&String::new())));
    // SAFETY: the parent owns the Box from here on and is dropped on this thread.
    unsafe { add_owned_child(parent_id, child) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"));

    drop(parent);

    assert_eq!(seen(&calls), Vec::<&str>::new());
}
