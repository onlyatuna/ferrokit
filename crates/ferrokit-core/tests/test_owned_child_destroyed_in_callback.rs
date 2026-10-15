// SPDX-License-Identifier: LGPL-3.0-only
//! A parent dropped from inside the callback of one of its owned children.
//!
//! Qt destroys every child with its parent at once, even a child whose slot is running; the rest
//! of that slot must then not touch the object (undefined behavior in C++, an error in PySide6).
//! Here the callback runs on `&mut self`, so the child's memory has to outlive it. The child is
//! destroyed at once as far as anything else can tell: it is unregistered, no longer alive, and its
//! signals are disconnected. Its memory, and so its `Drop`, goes when the callback returns.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    add_owned_child, dispatch_to_object, register_qobject, set_parent, with_object_mut, ObjectData,
    ObjectId, QObject, ThreadId,
};
use ferrokit_core::signal::ConnectionType;

type Log = Rc<RefCell<Vec<String>>>;

struct Parent {
    data: ObjectData,
    log: Option<Log>,
}

impl Parent {
    fn registered() -> Box<Self> {
        Self::build(None)
    }

    /// A parent that logs when it is freed.
    fn logging(log: &Log) -> Box<Self> {
        Self::build(Some(Rc::clone(log)))
    }

    fn build(log: Option<Log>) -> Box<Self> {
        let mut p = Box::new(Self {
            data: ObjectData::new(ObjectId::next()),
            log,
        });
        // SAFETY: the Box stays at this address on this thread until it is dropped, which
        // unregisters it.
        unsafe { register_qobject(&mut *p) };
        p
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        if let Some(log) = &self.log {
            log.borrow_mut().push("parent freed".into());
        }
    }
}

impl QObject for Parent {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
}

type UserHook = Box<dyn FnOnce(&mut Child)>;

/// An owned child. It logs when it is freed, runs `on_drop` then, and runs `on_user` when it gets a
/// user event.
struct Child {
    data: ObjectData,
    name: &'static str,
    log: Log,
    payload: Vec<u32>,
    on_user: Option<UserHook>,
    on_drop: Option<Box<dyn FnOnce()>>,
}

impl Child {
    fn new(name: &'static str, log: &Log) -> Box<Self> {
        Box::new(Self {
            data: ObjectData::new(ObjectId::next()),
            name,
            log: Rc::clone(log),
            payload: vec![1, 2, 3],
            on_user: None,
            on_drop: None,
        })
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.log.borrow_mut().push(format!("{} freed", self.name));
        if let Some(hook) = self.on_drop.take() {
            hook();
        }
    }
}

impl QObject for Child {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::User(_)) {
            if let Some(hook) = self.on_user.take() {
                hook(self);
            }
            return true;
        }
        false
    }
}

fn attach(parent_id: ObjectId, child: Box<Child>) -> ObjectId {
    // SAFETY: the parent owns the Box from here on and is dropped on this thread.
    unsafe { add_owned_child(parent_id, child) }
        .unwrap_or_else(|(err, _)| panic!("attach refused: {err:?}"))
}

fn user_event() -> Event {
    Event::new(EventKind::User(Box::new(())))
}

fn has(log: &Log, entry: &str) -> bool {
    log.borrow().iter().any(|line| line == entry)
}

/// Makes `child` drop `parent` when it gets a user event, and report what it can still see.
fn drop_the_parent_from(child: &mut Child, parent: &Rc<RefCell<Option<Box<Parent>>>>, log: &Log) {
    let parent = Rc::clone(parent);
    let log = Rc::clone(log);
    child.on_user = Some(Box::new(move |me: &mut Child| {
        me.log
            .borrow_mut()
            .push("callback: dropping the parent".into());
        drop(parent.borrow_mut().take());
        let sibling_freed = has(&log, "sibling freed");
        let mid_freed = has(&log, "mid freed");
        let child_freed = has(&log, "child freed");
        me.log.borrow_mut().push(format!(
            "callback: parent dropped; alive={} parent={:?} payload={} sibling_freed={sibling_freed} mid_freed={mid_freed} child_freed={child_freed}",
            me.data.is_alive(),
            me.data.parent(),
            me.payload.iter().sum::<u32>(),
        ));
    }));
}

#[test]
fn an_owned_child_is_freed_when_its_callback_returns_not_when_its_parent_drops() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    drop_the_parent_from(&mut child, &parent, &log);
    attach(parent_id, child);

    let mut event = user_event();
    dispatch_to_object(child_id, &mut event);

    assert_eq!(
        *log.borrow(),
        vec![
            "callback: dropping the parent".to_string(),
            "callback: parent dropped; alive=false parent=None payload=6 sibling_freed=false mid_freed=false child_freed=false".to_string(),
            "child freed".to_string(),
        ]
    );
    assert!(
        with_object_mut(child_id, |_| ()).is_none(),
        "the child must not stay registered"
    );
}

#[test]
fn only_the_child_whose_callback_is_running_waits_for_it() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    attach(parent_id, Child::new("sibling", &log));
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    drop_the_parent_from(&mut child, &parent, &log);
    attach(parent_id, child);

    dispatch_to_object(child_id, &mut user_event());

    assert_eq!(
        *log.borrow(),
        vec![
            "callback: dropping the parent".to_string(),
            "sibling freed".to_string(),
            "callback: parent dropped; alive=false parent=None payload=6 sibling_freed=true mid_freed=false child_freed=false".to_string(),
            "child freed".to_string(),
        ]
    );
}

#[test]
fn a_grandchild_whose_callback_is_running_keeps_alive_only_itself() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let mid = Child::new("mid", &log);
    let mid_id = attach(parent_id, mid);
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    drop_the_parent_from(&mut child, &parent, &log);
    attach(mid_id, child);

    dispatch_to_object(child_id, &mut user_event());

    assert_eq!(
        *log.borrow(),
        vec![
            "callback: dropping the parent".to_string(),
            "mid freed".to_string(),
            "callback: parent dropped; alive=false parent=None payload=6 sibling_freed=false mid_freed=true child_freed=false".to_string(),
            "child freed".to_string(),
        ]
    );
}

#[test]
fn a_child_that_is_not_running_is_freed_with_its_parent() {
    let log = Log::default();
    let parent = Parent::registered();
    let parent_id = parent.data.id;
    attach(parent_id, Child::new("child", &log));

    drop(parent);

    assert_eq!(*log.borrow(), vec!["child freed".to_string()]);
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

#[test]
fn no_slot_of_a_destroyed_child_runs_once_the_slot_that_destroyed_it_has_returned() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let observer = Parent::registered();
    let slot_calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    {
        let first = ThreadBound(Rc::clone(&parent));
        let calls = Arc::clone(&slot_calls);
        child.data.object_name_changed.connect(move |_| {
            calls.lock().unwrap().push("first");
            drop(first.get().borrow_mut().take());
        });
        let calls = Arc::clone(&slot_calls);
        child.data.object_name_changed.connect_object(
            observer.data.id,
            ThreadId::current(),
            ConnectionType::Direct,
            move |_| calls.lock().unwrap().push("second"),
        );
        let calls = Arc::clone(&slot_calls);
        child
            .data
            .object_name_changed
            .connect(move |_| calls.lock().unwrap().push("third"));
    }
    child.on_user = Some(Box::new(|me: &mut Child| {
        me.data.set_object_name("renamed")
    }));
    attach(parent_id, child);

    dispatch_to_object(child_id, &mut user_event());

    assert_eq!(
        *slot_calls.lock().unwrap(),
        vec!["first"],
        "a slot connected after the one that destroyed the sender must not run, whether it is a \
         plain slot or a slot of another object"
    );
    assert_eq!(*log.borrow(), vec!["child freed".to_string()]);
}

#[test]
fn the_parent_the_child_and_its_connections_are_cleaned_up_in_one_order() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::logging(&log);
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let observer = Parent::registered();
    let slot_calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    {
        let calls = Arc::clone(&slot_calls);
        child
            .data
            .object_name_changed
            .connect(move |_| calls.lock().unwrap().push("plain"));
        let calls = Arc::clone(&slot_calls);
        child.data.object_name_changed.connect_object(
            observer.data.id,
            ThreadId::current(),
            ConnectionType::Direct,
            move |_| calls.lock().unwrap().push("object"),
        );
    }
    {
        let parent = Rc::clone(&parent);
        child.on_user = Some(Box::new(move |me: &mut Child| {
            me.log
                .borrow_mut()
                .push("callback: dropping the parent".into());
            drop(parent.borrow_mut().take());
            me.log.borrow_mut().push(format!(
                "callback: parent dropped; alive={} parent={:?}",
                me.data.is_alive(),
                me.data.parent()
            ));
            // The child is destroyed as far as its signals go: this emission reaches no slot.
            me.data.set_object_name("renamed");
        }));
    }
    attach(parent_id, child);

    dispatch_to_object(child_id, &mut user_event());

    assert_eq!(
        *log.borrow(),
        vec![
            "callback: dropping the parent".to_string(),
            "parent freed".to_string(),
            "callback: parent dropped; alive=false parent=None".to_string(),
            "child freed".to_string(),
        ]
    );
    assert!(
        slot_calls.lock().unwrap().is_empty(),
        "a slot ran for a signal emitted by a destroyed child: {:?}",
        slot_calls.lock().unwrap()
    );
}

#[test]
fn a_callback_nested_in_another_one_leaves_nothing_parked_and_frees_the_inner_child_last() {
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let mut inner = Child::new("inner", &log);
    let inner_id = inner.data.id;
    drop_the_parent_from(&mut inner, &parent, &log);
    let mut mid = Child::new("mid", &log);
    mid.on_user = Some(Box::new(move |me: &mut Child| {
        me.log
            .borrow_mut()
            .push("mid callback: running the inner callback".into());
        dispatch_to_object(inner_id, &mut user_event());
        me.log.borrow_mut().push("mid callback: returning".into());
    }));
    let mid_id = attach(parent_id, mid);
    attach(mid_id, inner);

    dispatch_to_object(mid_id, &mut user_event());

    assert_eq!(
        *log.borrow(),
        vec![
            "mid callback: running the inner callback".to_string(),
            "callback: dropping the parent".to_string(),
            "callback: parent dropped; alive=false parent=None payload=6 sibling_freed=false mid_freed=false child_freed=false".to_string(),
            "mid callback: returning".to_string(),
            "mid freed".to_string(),
            "inner freed".to_string(),
        ]
    );
}

#[test]
fn freeing_a_parked_child_can_run_a_callback_that_parks_and_frees_another() {
    let log = Log::default();
    let second_parent_id;
    let second_parent = {
        let p = Parent::registered();
        second_parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let mut second = Child::new("second child", &log);
    let second_id = second.data.id;
    drop_the_parent_from(&mut second, &second_parent, &log);
    attach(second_parent_id, second);

    let first_parent_id;
    let first_parent = {
        let p = Parent::registered();
        first_parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let mut first = Child::new("first child", &log);
    let first_id = first.data.id;
    drop_the_parent_from(&mut first, &first_parent, &log);
    // The first child's `Drop` runs while its guard is freeing it. It dispatches to the second
    // child, which drops its own parent and is parked and freed by a guard nested in that drop.
    first.on_drop = Some(Box::new(move || {
        dispatch_to_object(second_id, &mut user_event());
    }));
    attach(first_parent_id, first);

    dispatch_to_object(first_id, &mut user_event());

    let same_report = "callback: parent dropped; alive=false parent=None payload=6 sibling_freed=false mid_freed=false child_freed=false";
    assert_eq!(
        *log.borrow(),
        vec![
            "callback: dropping the parent".to_string(),
            same_report.to_string(),
            "first child freed".to_string(),
            "callback: dropping the parent".to_string(),
            same_report.to_string(),
            "second child freed".to_string(),
        ]
    );
}

#[test]
fn a_child_attached_inside_a_callback_that_destroys_its_parent_is_not_listed_in_a_dead_object() {
    // The attach is queued on the running child, which is destroyed before the callback returns.
    // The queued change must be dropped, not written to the object that is waiting to be freed.
    let log = Log::default();
    let parent_id;
    let parent = {
        let p = Parent::registered();
        parent_id = p.data.id;
        Rc::new(RefCell::new(Some(p)))
    };
    let attached = Rc::new(RefCell::new(Parent::registered()));
    let mut child = Child::new("child", &log);
    let child_id = child.data.id;
    {
        let parent = Rc::clone(&parent);
        let attached = Rc::clone(&attached);
        child.on_user = Some(Box::new(move |me: &mut Child| {
            set_parent(&mut attached.borrow_mut().data, Some(me.data.id)).unwrap();
            drop(parent.borrow_mut().take());
        }));
    }
    attach(parent_id, child);

    dispatch_to_object(child_id, &mut user_event());

    assert_eq!(*log.borrow(), vec!["child freed".to_string()]);
    assert_eq!(
        attached.borrow().data.parent(),
        None,
        "the attached object still has the destroyed child as its parent"
    );
}
