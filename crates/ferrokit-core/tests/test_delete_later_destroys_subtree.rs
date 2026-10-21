// SPDX-License-Identifier: LGPL-3.0-only
//! `deleteLater` destroys the object and everything below it when the `DeferredDelete` is
//! delivered (Qt `QObject::event` -> `qDeleteInEventHandler`, qobject.cpp).
//!
//! An object a parent owns is dropped through its owner: it leaves the parent's child list, the
//! parent gets `ChildRemoved`, and the memory goes. An object owned elsewhere is unregistered
//! together with its subtree, and its parent's list forgets it.

use parking_lot::Mutex;
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::event_loop::EventLoop;
use ferrokit_core::object::{
    add_owned_child, register_owned_qobject, set_parent_by_id, with_object, with_object_mut,
    ObjectData, ObjectDataMut, ObjectId, OwnedObject, QObject,
};

type Log = Arc<Mutex<Vec<String>>>;

fn new_log() -> Log {
    Arc::new(Mutex::new(Vec::new()))
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().clone()
}

struct Probe {
    data: ObjectData,
    name: &'static str,
    log: Log,
}

impl Probe {
    fn new(name: &'static str, log: &Log) -> Self {
        Self {
            data: ObjectData::new(ObjectId::next()),
            name,
            log: Arc::clone(log),
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.log.lock().push(format!("{} dropped", self.name));
    }
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ObjectDataMut<'_> {
        ObjectDataMut::new(&mut self.data)
    }
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn event(&mut self, event: &mut Event) -> bool {
        if let EventKind::ChildRemoved { child_id } = event.kind {
            self.log
                .lock()
                .push(format!("{} saw removed {}", self.name, child_id.0));
        }
        false
    }
}

fn owned(name: &'static str, log: &Log) -> OwnedObject<Probe> {
    // SAFETY: the owner stays on this thread and is dropped when no callback of the object runs.
    unsafe { register_owned_qobject(Probe::new(name, log)) }
}

fn owned_child(parent: ObjectId, name: &'static str, log: &Log) -> ObjectId {
    // SAFETY: the child stays where it is allocated; nothing reaches it during a callback.
    unsafe { add_owned_child(parent, Box::new(Probe::new(name, log))) }
        .unwrap_or_else(|(error, _)| panic!("add_owned_child: {error:?}"))
}

fn children_of(parent: &OwnedObject<Probe>) -> Vec<ObjectId> {
    parent
        .with(|p| p.object_data().children().to_vec())
        .expect("parent reachable")
}

fn delete_later(id: ObjectId) {
    with_object(id, |o| o.delete_later()).expect("registered");
}

#[test]
fn deferred_delete_of_an_owned_child_frees_it_and_unlists_it_from_the_parent() {
    let log = new_log();
    let el = EventLoop::new();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let child_id = owned_child(parent_id, "child", &log);
    let grand_id = owned_child(child_id, "grand", &log);
    assert_eq!(children_of(&parent), vec![child_id]);

    delete_later(child_id);
    assert_eq!(el.flush_deferred_deletes(), 1);

    let seen = entries(&log);
    assert!(seen.contains(&"child dropped".to_string()), "child freed: {seen:?}");
    assert!(seen.contains(&"grand dropped".to_string()), "subtree freed: {seen:?}");
    assert!(
        seen.contains(&format!("parent saw removed {}", child_id.0)),
        "parent told: {seen:?}"
    );
    assert!(children_of(&parent).is_empty(), "parent's list forgot the child");
    assert!(with_object(child_id, |_| ()).is_none());
    assert!(with_object(grand_id, |_| ()).is_none());
    drop(parent);
    assert_eq!(
        entries(&log).iter().filter(|e| e.ends_with("dropped")).count(),
        3,
        "nothing is freed twice: {:?}",
        entries(&log)
    );
}

#[test]
fn deferred_delete_of_an_owned_child_is_not_found_by_the_parent_afterwards() {
    let log = new_log();
    let el = EventLoop::new();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let child_id = owned_child(parent_id, "child", &log);

    delete_later(child_id);
    el.flush_deferred_deletes();

    let found = parent
        .with(|p| p.find_child_id("", std::any::TypeId::of::<Probe>()))
        .unwrap();
    assert_eq!(found, None);
}

#[test]
fn deferred_delete_of_an_unowned_object_unregisters_its_subtree() {
    let log = new_log();
    let el = EventLoop::new();
    let top = owned("top", &log);
    let top_id = OwnedObject::object_id(&top);
    let kid = owned("kid", &log);
    let kid_id = OwnedObject::object_id(&kid);
    set_parent_by_id(kid_id, Some(top_id)).unwrap();
    let grandkid = owned("grandkid", &log);
    let grandkid_id = OwnedObject::object_id(&grandkid);
    set_parent_by_id(grandkid_id, Some(kid_id)).unwrap();

    delete_later(top_id);
    assert_eq!(el.flush_deferred_deletes(), 1);

    assert!(with_object(top_id, |_| ()).is_none());
    assert!(with_object(kid_id, |_| ()).is_none(), "child of a deleted object is gone");
    assert!(with_object(grandkid_id, |_| ()).is_none(), "and so is its child");
}

#[test]
fn deferred_delete_of_a_non_owned_child_unlists_it_from_the_parent() {
    let log = new_log();
    let el = EventLoop::new();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let kid = owned("kid", &log);
    let kid_id = OwnedObject::object_id(&kid);
    set_parent_by_id(kid_id, Some(parent_id)).unwrap();
    assert_eq!(children_of(&parent), vec![kid_id]);

    delete_later(kid_id);
    assert_eq!(el.flush_deferred_deletes(), 1);

    assert!(children_of(&parent).is_empty(), "parent forgot the deleted child");
}

#[test]
fn deferred_delete_with_the_parent_borrowed_finishes_when_the_parent_is_released() {
    let log = new_log();
    let el = EventLoop::new();
    let mut parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let child_id = owned_child(parent_id, "child", &log);
    delete_later(child_id);

    parent
        .with_mut(|_| {
            assert_eq!(el.flush_deferred_deletes(), 1);
            assert!(
                with_object_mut(child_id, |_| ()).is_none(),
                "the deleted child is out of reach at once"
            );
        })
        .expect("parent reachable");

    assert!(children_of(&parent).is_empty());
    assert!(entries(&log).contains(&"child dropped".to_string()), "{:?}", entries(&log));
}

// ---- stale registry lists must not orphan a child that belongs to another parent ----

use ferrokit_core::object::{query_object_parent, reparent};

/// Builds `stale`, whose registry list names `kid` while `kid`'s own record names `real`: the list
/// was copied when `kid` was not registered yet, and `kid` was registered under another parent.
fn stale_list_setup(log: &Log) -> (OwnedObject<Probe>, OwnedObject<Probe>, OwnedObject<Probe>) {
    let mut stale = Probe::new("stale", log);
    let mut kid = Probe::new("kid", log);
    reparent(&mut kid.data, None, Some(&mut stale.data));
    let stale = unsafe { register_owned_qobject(stale) };
    reparent(&mut kid.data, None, None);
    let kid = unsafe { register_owned_qobject(kid) };
    let real = owned("real", log);
    set_parent_by_id(OwnedObject::object_id(&kid), Some(OwnedObject::object_id(&real))).unwrap();
    (stale, kid, real)
}

#[test]
fn unregistering_a_stale_parent_does_not_orphan_a_child_of_another_parent() {
    let log = new_log();
    let (stale, kid, real) = stale_list_setup(&log);
    let kid_id = OwnedObject::object_id(&kid);
    let real_id = OwnedObject::object_id(&real);
    assert_eq!(query_object_parent(kid_id), Some(Some(real_id)));

    drop(stale);

    assert_eq!(
        query_object_parent(kid_id),
        Some(Some(real_id)),
        "the child still belongs to its real parent"
    );
}

#[test]
fn releasing_a_borrowed_child_of_another_parent_leaves_it_alone() {
    let log = new_log();
    let (stale, mut kid, real) = stale_list_setup(&log);
    let kid_id = OwnedObject::object_id(&kid);
    let real_id = OwnedObject::object_id(&real);

    // `stale` lists `kid` in its own local list too (a stale entry), and a callback of `kid` runs
    // while `stale` is dropped.
    kid.with_mut(|_| {
        drop(stale);
        assert!(
            with_object(kid_id, |_| ()).is_none(),
            "borrowed here, so unreachable either way"
        );
    })
    .unwrap();
    assert_eq!(query_object_parent(kid_id), Some(Some(real_id)));
    assert!(OwnedObject::with(&kid, |_| ()).is_some(), "kid is still registered");
}

// ---- findChild covers every child, owned or not (Qt qt_qFindChild_helper) ----

#[test]
fn find_child_sees_children_attached_without_ownership() {
    let log = new_log();
    let parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let mut kid = owned("kid", &log);
    let kid_id = OwnedObject::object_id(&kid);
    kid.with_mut(|k| k.set_object_name("kid")).unwrap();
    set_parent_by_id(kid_id, Some(parent_id)).unwrap();
    let probe = std::any::TypeId::of::<Probe>();

    assert_eq!(parent.with(|p| p.find_child_id("kid", probe)).unwrap(), Some(kid_id));
    let mut seen = 0;
    assert!(parent
        .with(|p| p.find_child_any("kid", probe, &mut |_| seen += 1))
        .unwrap());
    assert_eq!(seen, 1);
    let mut all = 0;
    parent.with(|p| p.find_children_any(Some("kid"), probe, &mut |_| all += 1)).unwrap();
    assert_eq!(all, 1);
}

// ---- `destroyed` of the deleted object goes out before its descendants' ----

fn log_destroyed(id: ObjectId, label: &'static str, log: &Log) {
    let log = Arc::clone(log);
    with_object(id, |o| {
        o.destroyed().connect(move |_: &ObjectId| log.lock().push(format!("destroyed {label}")));
    })
    .expect("registered");
}

#[test]
fn deferred_delete_of_an_unowned_object_emits_destroyed_parent_first() {
    let log = new_log();
    let el = EventLoop::new();
    let top = owned("top", &log);
    let top_id = OwnedObject::object_id(&top);
    let kid = owned("kid", &log);
    let kid_id = OwnedObject::object_id(&kid);
    set_parent_by_id(kid_id, Some(top_id)).unwrap();
    log_destroyed(top_id, "top", &log);
    log_destroyed(kid_id, "kid", &log);

    delete_later(top_id);
    el.flush_deferred_deletes();

    let seen: Vec<_> = entries(&log).into_iter().filter(|e| e.starts_with("destroyed")).collect();
    assert_eq!(seen, vec!["destroyed top", "destroyed kid"]);
}

#[test]
fn deferred_delete_with_the_parent_borrowed_emits_destroyed_parent_first() {
    let log = new_log();
    let el = EventLoop::new();
    let mut parent = owned("parent", &log);
    let parent_id = OwnedObject::object_id(&parent);
    let child_id = owned_child(parent_id, "child", &log);
    let grand_id = owned_child(child_id, "grand", &log);
    log_destroyed(child_id, "child", &log);
    log_destroyed(grand_id, "grand", &log);
    delete_later(child_id);

    parent.with_mut(|_| el.flush_deferred_deletes()).unwrap();

    let seen: Vec<_> = entries(&log).into_iter().filter(|e| e.starts_with("destroyed")).collect();
    assert_eq!(seen, vec!["destroyed child", "destroyed grand"]);
}
