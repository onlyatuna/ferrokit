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

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `Node` holds an `ObjectData`, `Arc<Mutex<_>>`, a `&'static str` and a `bool`, all
    // `Send`; no `Rc`, `Cell`, raw pointer or thread-local state, so using and dropping it on
    // another thread after `move_to_thread_by_id` is sound.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

struct Node {
    data: ObjectData,
    log: Arc<Mutex<Vec<String>>>,
    tag: &'static str,
    panic_on_child_removed: bool,
}

impl Node {
    fn new(tag: &'static str, log: &Arc<Mutex<Vec<String>>>) -> Box<Self> {
        Box::new(Self {
            data: movable_data(ObjectId::next()),
            log: Arc::clone(log),
            tag,
            panic_on_child_removed: false,
        })
    }
    fn register(self: Box<Self>) -> *mut Node {
        let ptr = Box::into_raw(self);
        // SAFETY: `ptr` comes from `Box::into_raw`, so it is a live allocation that no `Box` or
        // reference covers until the caller frees it with `Box::from_raw` (every caller does, after
        // unregistering, on this thread). The pointer is `ptr` itself, not one made from a
        // reference, so rule 1 holds; the callers only reach the node through `ptr` (rule 7).
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
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:filter", self.tag));
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
    // SAFETY: `node` is the pointer from `Box::into_raw` in `register`, live and the only owner.
    // The test runs one thread; `move_to_thread_by_id` moved the registration to `other`, a thread
    // that has already exited, so no callback of `node` can run anywhere and the thread rule "use
    // directly only on the registration thread" is deliberately not met: it exists to keep direct
    // use from racing callbacks, and none can run. `Node` holds only `Arc<Mutex<_>>` and plain
    // fields, which is what the opt-in in `movable_data` promised. No timer was created (the start
    // is rejected, asserted via `timers().is_empty()`), so `unregister_qobject` has none to stop
    // from the wrong thread. `drop(Box::from_raw(node))` frees the memory once; `ObjectData::drop`
    // unregisters in place first, so the explicit `unregister_qobject(id)` after is a second call
    // for an id the registry no longer holds, and `id` was read before the drop.
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
    // SAFETY: `parent`, `app`, `pfilter` and `child` are raw pointers from `Box::into_raw` in
    // `register`, live and used only on this thread, the registration thread, with no callback
    // active between statements. They are the registered pointers, so the writes through them
    // (rule 7) keep the registry's copies valid. `install_event_filter` and `set_parent` run
    // callbacks only on the old and new parent and the filters, never on the object they edit (see
    // `set_parent_by_id`). `drop(Box::from_raw(child))` is `child`'s only free: its `Drop` unregisters
    // it in place, and the callbacks it triggers (`ChildRemoved` on `parent`, `pfilter`, `app`)
    // run on other objects, so nothing touches `child` during its own drop.
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
    // SAFETY: `parent`, `app` and `pfilter` are the raw pointers from `Box::into_raw` in
    // `register`, still the only owners, used on the registering thread with no callback active.
    // `drop(Box::from_raw(p))` runs `ObjectData::drop`, which unregisters the object in place
    // before the memory is freed (the `unregister_qobject` requirement on its memory); the explicit
    // `unregister_qobject(id)` after it is a second call for an id the registry no longer holds,
    // and it never touches memory. `id` is read before the drop. No timers exist [inferred].
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
    // SAFETY: the three pointers (`parent`, `pfilter`, `child`) come from `Box::into_raw` in
    // `register`, are live, unmoved, and used only on this thread, the registration thread. They
    // are the registered pointers, so these writes go through them (rule 7). `install_event_filter`
    // and `set_parent` run callbacks only on the old and new parent and the filters, never on the
    // object they edit (see `set_parent_by_id`), and no callback is active while these run.
    // `add_owned_child(cid, grand)`: `grand` is a fresh `Box` with no reference taken from it
    // (only an `Arc` liveness token), `cid` is registered on this thread, and `grand` is not
    // touched again.
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
    // SAFETY: `child` is the pointer from `Box::into_raw` in `register`, and nothing else frees or
    // uses it: this is its only `Box::from_raw`. The Box is derived from the same raw pointer that
    // `register_qobject_ptr` received, so it does not invalidate the registry's copy [inferred
    // from how Miri treats a Box rebuilt from the pointer it was split into]. The drop runs
    // `ObjectData::drop`, which unregisters in place; the callbacks it triggers (`ChildRemoved` on
    // `parent`, filters) run on other objects, so nothing touches `child` during its own drop.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        drop(Box::from_raw(child));
    }));
    assert!(
        outcome.is_err(),
        "the handler panic is resumed after teardown"
    );
    assert!(!grand_alive.load(Ordering::SeqCst), "children deleted");
    assert!(
        query_object_thread(child_id).is_none(),
        "child unregistered"
    );
    assert!(
        query_object_thread(grand_id).is_none(),
        "grandchild unregistered"
    );
    // SAFETY: `parent` and `pfilter` are the raw pointers from `Box::into_raw` in `register`, still
    // the only owners; the pointers are used on the registering thread with no callback active
    // (the handler panic was caught above and `child` is gone). `drop(Box::from_raw(p))` runs
    // `ObjectData::drop`, which unregisters the object in place before the memory is freed (the
    // `unregister_qobject` requirement on its memory); the explicit `unregister_qobject(id)` after
    // it is then a second call for an id the registry no longer holds, and it never touches memory.
    // `id` is read before the drop, so nothing reads freed memory. No timers exist [inferred].
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
