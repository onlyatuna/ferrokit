// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[test]
fn the_receiver_index_follows_register_replace_and_unregister() {
    let (a, b) = (ObjectId::next(), ObjectId::next());
    let mut reg = TimerRegistry::new();
    let a1 = reg.register(a, 10, TimerType::Coarse, false);
    let a2 = reg.register(a, 20, TimerType::Coarse, false);
    let b1 = reg.register(b, 30, TimerType::Coarse, false);
    let mut of_a = reg.ids_for_receiver(a).to_vec();
    of_a.sort_by_key(|id| id.0);
    assert_eq!(of_a, vec![a1, a2]);
    assert_eq!(reg.ids_for_receiver(b), &[b1]);

    // A live id is never replaced: the registration is refused and the timer stays with `a`.
    assert!(!reg.register_with_id(a1, b, 10, TimerType::Coarse, false));
    let mut of_a = reg.ids_for_receiver(a).to_vec();
    of_a.sort_by_key(|id| id.0);
    assert_eq!(of_a, vec![a1, a2]);
    assert_eq!(reg.ids_for_receiver(b), &[b1]);
    assert_eq!(reg.len(), 3);
    assert!(reg.unregister(a1).is_some());
    // A freed id registers again under a new receiver, as a moved timer does.
    assert!(reg.register_with_id(a1, b, 10, TimerType::Coarse, false));
    assert_eq!(reg.ids_for_receiver(a), &[a2]);
    let mut of_b = reg.ids_for_receiver(b).to_vec();
    of_b.sort_by_key(|id| id.0);
    assert_eq!(of_b, vec![a1, b1]);

    assert!(reg.unregister(a2).is_some());
    assert!(reg.ids_for_receiver(a).is_empty());
    assert!(reg.unregister(a2).is_none());
    assert_eq!(reg.len(), 2);
}

/// A moved timer whose id is already live on the target thread is re-registered under a
/// fresh id, not dropped.
#[test]
fn adopting_a_timer_with_a_live_id_gives_it_a_fresh_id() {
    let registry = Arc::new(Mutex::new(TimerRegistry::new()));
    #[cfg(windows)]
    register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
    #[cfg(not(windows))]
    register_thread_timer_context(Arc::clone(&registry));
    let held = Timer::new();
    let mover = Timer::new();
    let moved_id = mover.start();
    // The timer leaves its registry, as `detach_timers_for_objects` does on a thread move.
    let entry = registry.lock().unwrap().unregister(moved_id).unwrap();
    // The target thread already has a live timer of another object under that id.
    assert!(registry.lock().unwrap().register_with_id(
        moved_id,
        held.object_id(),
        50,
        TimerType::Coarse,
        false
    ));
    adopt_entries(vec![MovedTimer {
        entry,
        aliases: Vec::new(),
    }]);
    let fresh = {
        let reg = registry.lock().unwrap();
        assert_eq!(reg.len(), 2, "the moved timer was dropped");
        assert_eq!(reg.get(moved_id).unwrap().receiver, held.object_id());
        let ids = reg.ids_for_receiver(mover.object_id());
        assert_eq!(ids.len(), 1);
        assert_ne!(ids[0], moved_id);
        ids[0]
    };
    // The owner follows the re-id: it reports the new id, and stop() kills only its own timer.
    assert_eq!(mover.timer_id(), fresh);
    mover.stop();
    let reg = registry.lock().unwrap();
    assert!(reg.get(fresh).is_none(), "the moved timer was not stopped");
    assert_eq!(
        reg.get(moved_id).map(|e| e.receiver),
        Some(held.object_id()),
        "an unrelated timer was killed"
    );
    drop(reg);
    drop(held);
    drop(mover);
    unregister_thread_timer_context();
}

/// An object that starts its timers through `QObject::start_timer`.
struct TimerOwner {
    data: ObjectData,
}

impl QObject for TimerOwner {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> crate::object::ObjectDataMut<'_> {
        crate::object::ObjectDataMut::new(&mut self.data)
    }
}

fn owner_timers(owner: &OwnedObject<TimerOwner>) -> Vec<TimerId> {
    owner.with(|o| o.data.timers.clone()).unwrap()
}

/// Moves the object timer `started` of `receiver` away and back while `held`'s timer occupies
/// its id, as after the id counter wrapped. Returns the id it was given on adoption.
fn reidentify_object_timer(
    registry: &Mutex<TimerRegistry>,
    receiver: ObjectId,
    started: TimerId,
    held: &Timer,
) -> TimerId {
    let moved = detach_registered_timers(&[receiver]);
    assert_eq!(moved.len(), 1);
    assert!(registry.lock().unwrap().register_with_id(
        started,
        held.object_id(),
        50,
        TimerType::Coarse,
        false
    ));
    adopt_entries(moved);
    let reg = registry.lock().unwrap();
    let ids = reg.ids_for_receiver(receiver);
    assert_eq!(ids.len(), 1);
    assert_ne!(ids[0], started);
    ids[0]
}

/// A re-identified `QObject::start_timer` timer leaves no stale id in its object's list, the
/// id its owner holds still kills it, and no redirect outlives the timer.
#[test]
fn a_reidentified_object_timer_leaves_no_stale_id_or_redirect() {
    let registry = Arc::new(Mutex::new(TimerRegistry::new()));
    #[cfg(windows)]
    register_thread_timer_context(Arc::clone(&registry), std::ptr::null_mut());
    #[cfg(not(windows))]
    register_thread_timer_context(Arc::clone(&registry));
    let held = Timer::new();
    let data = ObjectData::new(ObjectId::next());
    let receiver = data.id();
    // SAFETY: `register_owned_qobject` boxes the `TimerOwner`, so it stays at one address
    // until `owner` is dropped (which unregisters it). The test reaches it only through
    // `owner.with_mut`/`with`, which refuse while a callback of the object runs, never
    // through a reference held across a call. The object's `ObjectData` does not opt in to
    // thread moves, so every callback runs on this test thread, the registering one.
    let mut owner = unsafe { crate::object::register_owned_qobject(TimerOwner { data }) };

    // Killed through the id `start_timer` returned, which the re-id made stale.
    let started = owner
        .with_mut(|o| o.start_timer(50, TimerType::Coarse))
        .unwrap();
    assert!(started.is_valid());
    let fresh = reidentify_object_timer(&registry, receiver, started, &held);
    assert_eq!(
        owner_timers(&owner),
        vec![fresh],
        "stale id left in the object's list"
    );
    assert_eq!(reidentified_as(receiver, started), Some(fresh));
    assert!(owner.with_mut(|o| o.kill_timer(started)).unwrap());
    assert!(owner_timers(&owner).is_empty());
    assert_eq!(
        reidentified_as(receiver, started),
        None,
        "redirect outlived the timer"
    );
    {
        let reg = registry.lock().unwrap();
        assert!(reg.get(fresh).is_none());
        assert_eq!(reg.len(), 1, "an unrelated timer was killed");
    }
    assert!(
        !owner.with_mut(|o| o.kill_timer(started)).unwrap(),
        "killed twice"
    );

    // Killed through the new id: the redirect goes with the timer.
    let started = owner
        .with_mut(|o| o.start_timer(50, TimerType::Coarse))
        .unwrap();
    let fresh = reidentify_object_timer(&registry, receiver, started, &held);
    assert!(owner.with_mut(|o| o.kill_timer(fresh)).unwrap());
    assert_eq!(
        reidentified_as(receiver, started),
        None,
        "redirect outlived the timer"
    );
    assert!(owner_timers(&owner).is_empty());

    // The redirect follows the timer through a second move and is dropped with the timer
    // when the object's timers are removed (as on its destruction).
    let started = owner
        .with_mut(|o| o.start_timer(50, TimerType::Coarse))
        .unwrap();
    let fresh = reidentify_object_timer(&registry, receiver, started, &held);
    let moved = detach_registered_timers(&[receiver]);
    assert_eq!(
        reidentified_as(receiver, started),
        None,
        "redirect stayed on the old thread"
    );
    adopt_entries(moved);
    assert_eq!(owner_timers(&owner), vec![fresh]);
    assert_eq!(reidentified_as(receiver, started), Some(fresh));
    assert_eq!(stop_timers_for_object(receiver), 1);
    assert_eq!(
        reidentified_as(receiver, started),
        None,
        "redirect outlived the timer"
    );
    // The object's list is dropped with the object; here it is emptied to go on.
    assert!(
        !owner.with_mut(|o| o.kill_timer(fresh)).unwrap(),
        "already unregistered"
    );

    // A thread without a timer context schedules nothing, so the object keeps no id.
    let started = owner
        .with_mut(|o| o.start_timer(50, TimerType::Coarse))
        .unwrap();
    let moved = detach_registered_timers(&[receiver]);
    unregister_thread_timer_context();
    adopt_entries(moved);
    assert!(
        owner_timers(&owner).is_empty(),
        "{started} left in the list"
    );
    assert!(
        registry
            .lock()
            .unwrap()
            .ids_for_receiver(receiver)
            .is_empty()
    );
    drop(held);
    drop(owner);
}

#[test]
fn context_shots_are_found_by_context_and_forgotten_once_fired() {
    let mut shots = ContextShots::default();
    let (c1, c2) = (ObjectId::next(), ObjectId::next());
    let (r1, r2, r3) = (ObjectId::next(), ObjectId::next(), ObjectId::next());
    shots.insert(r1, c1);
    shots.insert(r2, c1);
    shots.insert(r3, c2);
    shots.remove(r2);
    assert_eq!(shots.take_for(&[c1]), vec![(r1, c1)]);
    assert!(shots.take_for(&[c1]).is_empty());
    assert_eq!(shots.take_for(&[c1, c2]), vec![(r3, c2)]);
    assert!(shots.context_of.is_empty() && shots.receivers_of.is_empty());
}
