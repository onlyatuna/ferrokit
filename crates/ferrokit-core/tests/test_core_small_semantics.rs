// SPDX-License-Identifier: LGPL-3.0-only
//! Small core semantics: foreign-thread `start_timer`, ChildRemoved filtering on drop, and panic
//! containment while a dropped child notifies its parent.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::{
    clear_application_event_filters, install_application_event_filter, EventLoop,
};
use ferrokit_core::object::{
    add_owned_child, move_to_thread_by_id, query_object_thread, register_qobject_ptr,
    unregister_qobject, ObjectData, ObjectDataMut, ObjectId, QObject, ThreadId,
};
use ferrokit_core::timer::TimerId;

struct Node {
    data: ObjectData,
    log: Arc<Mutex<Vec<String>>>,
    tag: &'static str,
    panic_on_child_removed: bool,
}

impl Node {
    fn new(tag: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Box<Self> {
        Box::new(Self {
            data: ObjectData::new(ObjectId::next()),
            log: Arc::clone(log),
            tag,
            panic_on_child_removed: false,
        })
    }
    fn register(self: Box<Self>) -> *mut Node {
        let ptr = Box::into_raw(self);
        // SAFETY: the allocation stays put until the test frees it.
        unsafe { register_qobject_ptr(NonNull::new_unchecked(ptr as *mut dyn QObject)) };
        ptr
    }
}

impl QObject for Node {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ChildRemoved { .. }) {
            if self.panic_on_child_removed {
                panic!("child-removed handler panic");
            }
            self.log.lock().unwrap().push(format!("{}:event", self.tag));
        }
        false
    }
    fn event_filter(&mut self, _watched: ObjectId, event: &mut Event) -> bool {
        if matches!(event.kind, EventKind::ChildRemoved { .. }) {
            if self.panic_on_child_removed {
                panic!("child-removed filter panic");
            }
            self.log.lock().unwrap().push(format!("{}:filter", self.tag));
        }
        false
    }
}

#[test]
fn start_timer_from_foreign_thread_is_rejected() {
    let _loop = EventLoop::new();
    let log = Arc::default();
    let node = Node::new("n", &log).register();
    let other = std::thread::spawn(ThreadId::current).join().unwrap();
    let here = ThreadId::current();
    // SAFETY: only this thread touches the node.
    unsafe {
        move_to_thread_by_id((*node).data.id(), other, here).expect("bookkeeping move");
        assert_ne!((*node).data.thread_id(), ThreadId::current());
        let id = (*node).start_timer(10, ferrokit_core::timer::TimerType::Coarse);
        assert_eq!(id, TimerId::INVALID);
        assert!((*node).data.timers().is_empty());
        let id = (*node).data.id();
        drop(Box::from_raw(node));
        unregister_qobject(id);
    }
}

fn drop_child_with_filters(app_filter_too: bool) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    clear_application_event_filters();
    let parent = Node::new("parent", &log).register();
    let app = Node::new("app", &log).register();
    let pfilter = Node::new("pfilter", &log).register();
    let child = Node::new("child", &log).register();
    // SAFETY: single thread, no callbacks active while these run.
    unsafe {
        let pid = (*parent).data.id();
        if app_filter_too {
            install_application_event_filter((*app).data.id());
        }
        (*parent).data.install_event_filter((*pfilter).data.id());
        (*child).data.set_parent(Some(pid)).unwrap();
        drop(Box::from_raw(child));
    }
    clear_application_event_filters();
    let seen = log.lock().unwrap().clone();
    assert!(seen.contains(&"pfilter:filter".to_string()), "{seen:?}");
    assert!(seen.contains(&"parent:event".to_string()), "{seen:?}");
    if app_filter_too {
        assert!(seen.contains(&"app:filter".to_string()), "{seen:?}");
    }
    unsafe {
        for p in [parent, app, pfilter] {
            let id = (*p).data.id();
            drop(Box::from_raw(p));
            unregister_qobject(id);
        }
    }
}

#[test]
fn dropped_child_sends_child_removed_through_app_and_object_filters() {
    drop_child_with_filters(true);
}

fn panicking_drop(filter_panics: bool) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    clear_application_event_filters();
    let parent = Node::new("parent", &log).register();
    let pfilter = Node::new("pfilter", &log).register();
    let child = Node::new("child", &log).register();
    let grand = Node::new("grand", &log);
    let grand_alive: Arc<AtomicBool> = grand.data.liveness();
    // SAFETY: single thread, no callbacks active while these run.
    let (child_id, grand_id) = unsafe {
        let pid = (*parent).data.id();
        if filter_panics {
            (*pfilter).panic_on_child_removed = true;
            (*parent).data.install_event_filter((*pfilter).data.id());
        } else {
            (*parent).panic_on_child_removed = true;
        }
        (*child).data.set_parent(Some(pid)).unwrap();
        let cid = (*child).data.id();
        let gid = add_owned_child(cid, grand).unwrap_or_else(|(e, _)| panic!("{e:?}"));
        (cid, gid)
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(child));
    }));
    assert!(outcome.is_err(), "the handler panic is resumed after teardown");
    assert!(!grand_alive.load(Ordering::SeqCst), "children deleted");
    assert!(query_object_thread(child_id).is_none(), "child unregistered");
    assert!(query_object_thread(grand_id).is_none(), "grandchild unregistered");
    unsafe {
        for p in [parent, pfilter] {
            let id = (*p).data.id();
            drop(Box::from_raw(p));
            unregister_qobject(id);
        }
    }
}

#[test]
fn panicking_event_during_child_removed_still_tears_down() {
    panicking_drop(false);
}

#[test]
fn panicking_filter_during_child_removed_still_tears_down() {
    panicking_drop(true);
}
