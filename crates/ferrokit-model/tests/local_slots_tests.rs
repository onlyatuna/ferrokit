// SPDX-License-Identifier: LGPL-3.0-only
//! Deterministic std tests of the local-slot model: one scripted schedule per protocol step,
//! the shared scenarios on real threads, and the negative (mutant) checks.
//!
//! The Loom and Shuttle suites explore the interleavings; these pin down what each step does.

#![cfg(not(any(feature = "loom", feature = "shuttle")))]

use ferrokit_model::local_slots::scenarios;
use ferrokit_model::local_slots::*;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

fn world() -> Arc<World> {
    World::new(LocalMutants::default())
}

#[test]
fn owner_calls_and_a_foreign_call_is_skipped() {
    let w = world();
    let (to_main, from_owner) = mpsc::channel();
    let (to_owner, from_main) = mpsc::channel::<()>();
    let w2 = Arc::clone(&w);
    let owner = thread::spawn(move || {
        let h = install(&w2, SlotSpec::default());
        to_main.send(Arc::clone(&h)).unwrap();
        from_main.recv().unwrap();
        assert!(h.call());
    });
    let h = from_owner.recv().unwrap();
    assert!(!h.call(), "main is not the owner");
    assert_eq!(w.stats().calls, 0);
    assert_eq!(w.stats().foreign_skips, 1);
    to_owner.send(()).unwrap();
    owner.join().unwrap();
    w.wait_exited(1);
    assert_eq!(w.stats().calls, 1);
    drop(h);
    w.verify();
}

#[test]
fn a_foreign_drop_waits_for_the_owner_drain() {
    let w = world();
    let (to_main, from_owner) = mpsc::channel();
    let (to_owner, from_main) = mpsc::channel::<()>();
    let w2 = Arc::clone(&w);
    let owner = thread::spawn(move || {
        let h = install(&w2, SlotSpec::default());
        let (key, me) = (h.key(), h.owner());
        to_main.send(h).unwrap(); // the owner no longer holds a handle
        from_main.recv().unwrap(); // main has dropped the last one
        assert_eq!(
            w2.drop_count(key),
            0,
            "still installed until the owner drains"
        );
        assert_eq!(drain_retired(&w2), 1);
        assert_eq!(w2.drop_count(key), 1);
        assert_eq!(w2.drop_sites(key), vec![Some(me)]);
    });
    let h = from_owner.recv().unwrap();
    drop(h);
    assert_eq!(w.stats().posts_delivered, 1);
    to_owner.send(()).unwrap();
    owner.join().unwrap();
    w.wait_exited(1);
    assert_eq!(w.stats().posts_to_ended, 0);
    w.verify();
}

#[test]
fn a_burst_of_foreign_drops_coalesces_into_one_wake_post() {
    let w = world();
    let (to_main, from_owner) = mpsc::channel();
    let (to_owner, from_main) = mpsc::channel::<()>();
    let w2 = Arc::clone(&w);
    let owner = thread::spawn(move || {
        let hs: Vec<_> = (0..8).map(|_| install(&w2, SlotSpec::default())).collect();
        let keys: Vec<u64> = hs.iter().map(|h| h.key()).collect();
        to_main.send(hs).unwrap();
        from_main.recv().unwrap();
        assert!(keys.iter().all(|&k| w2.drop_count(k) == 0));
        assert_eq!(drain_retired(&w2), 8, "all 8 keys were buffered together");
        assert!(keys.iter().all(|&k| w2.drop_count(k) == 1));
    });
    let hs = from_owner.recv().unwrap();
    drop(hs);
    assert_eq!(w.stats().posts_delivered, 1, "8 drops, one wake post");
    to_owner.send(()).unwrap();
    owner.join().unwrap();
    w.wait_exited(1);
    w.verify();
}

#[test]
fn thread_exit_drops_the_closure_once_on_the_owner() {
    let w = world();
    let w2 = Arc::clone(&w);
    let h = thread::spawn(move || install(&w2, SlotSpec::default()))
        .join()
        .unwrap();
    w.wait_exited(1);
    assert_eq!(w.drop_count(h.key()), 1);
    assert_eq!(w.drop_sites(h.key()), vec![Some(h.owner())]);
    assert_eq!(w.stats().threads_exited, 1);
    // The calling side outlives the owner: a call finds no slot, a drop is a post to an ended
    // thread, and neither panics.
    assert!(!h.call());
    drop(h);
    assert_eq!(w.stats().retires_skipped_ended, 1);
    assert_eq!(w.stats().posts_to_ended, 0);
    w.verify();
}

#[test]
fn an_owner_drop_retires_at_once_without_a_post() {
    let w = world();
    let w2 = Arc::clone(&w);
    thread::spawn(move || {
        let h = install(&w2, SlotSpec::default());
        let key = h.key();
        drop(h);
        assert_eq!(w2.drop_count(key), 1);
        assert_eq!(w2.stats().posts_delivered, 0);
    })
    .join()
    .unwrap();
    w.wait_exited(1);
    w.verify();
}

#[test]
fn the_owner_compare_is_only_a_diagnostic() {
    let w = World::new(LocalMutants {
        skip_owner_check: true,
        ..LocalMutants::default()
    });
    let w2 = Arc::clone(&w);
    let (to_main, from_owner) = mpsc::channel();
    let (to_owner, from_main) = mpsc::channel::<()>();
    let owner = thread::spawn(move || {
        let h = install(&w2, SlotSpec::default());
        to_main.send(Arc::clone(&h)).unwrap();
        from_main.recv().unwrap();
        drop(h);
    });
    let h = from_owner.recv().unwrap();
    assert!(
        !h.call(),
        "the key is looked up in main's own, empty, table"
    );
    assert_eq!(w.stats().calls, 0);
    to_owner.send(()).unwrap();
    owner.join().unwrap();
    w.wait_exited(1);
    drop(h);
    w.verify();
}

#[test]
fn nested_retire_and_release_run_on_real_threads() {
    for _ in 0..200 {
        scenarios::foreign_drop_then_owner_drain(LocalMutants::default());
        scenarios::foreign_drop_after_owner_exit(LocalMutants::default());
        scenarios::nested_retire(LocalMutants::default());
        scenarios::call_releases_capture(LocalMutants::default());
        scenarios::two_owners_exit_then_foreign_use(LocalMutants::default());
    }
}

#[test]
fn a_nested_retire_drops_both_closures_exactly_once() {
    let w = world();
    let w2 = Arc::clone(&w);
    thread::spawn(move || {
        let hb = install(&w2, SlotSpec::default());
        let kb = hb.key();
        let ha = install(
            &w2,
            SlotSpec {
                capture: Some(hb),
                release_on_call: false,
            },
        );
        let ka = ha.key();
        drop(ha); // retire A; A's destructor drops B's handle, which retires B re-entrantly
        assert_eq!(w2.drop_count(ka), 1);
        assert_eq!(w2.drop_count(kb), 1);
    })
    .join()
    .unwrap();
    w.wait_exited(1);
    w.verify();
}

#[test]
fn a_slot_that_releases_its_capture_retires_it_mid_call() {
    let w = world();
    let w2 = Arc::clone(&w);
    thread::spawn(move || {
        let hb = install(&w2, SlotSpec::default());
        let kb = hb.key();
        let ha = install(
            &w2,
            SlotSpec {
                capture: Some(hb),
                release_on_call: true,
            },
        );
        assert_eq!(w2.drop_count(kb), 0);
        assert!(ha.call());
        assert_eq!(w2.drop_count(kb), 1, "released by the call itself");
    })
    .join()
    .unwrap();
    w.wait_exited(1);
    w.verify();
}

#[test]
fn many_owners_and_foreign_droppers() {
    // Several owners, handles dropped on whatever thread ends up with them.
    let w = world();
    let mut owners = Vec::new();
    for _ in 0..4 {
        let w2 = Arc::clone(&w);
        owners.push(thread::spawn(move || {
            let hs: Vec<_> = (0..8).map(|_| install(&w2, SlotSpec::default())).collect();
            for h in &hs {
                assert!(h.call());
            }
            hs
        }));
    }
    let handles: Vec<_> = owners.into_iter().flat_map(|o| o.join().unwrap()).collect();
    w.wait_exited(4);
    let s = w.stats();
    assert_eq!((s.installed, s.drops, s.threads_exited), (32, 32, 4));
    drop(handles);
    assert_eq!(w.stats().retires_skipped_ended, 32);
    assert_eq!(w.stats().posts_to_ended, 0);
    w.verify();
}

// Negative checks: each mutant must be caught.

#[test]
#[should_panic(expected = "installed twice")]
fn mutant_reused_keys_are_caught() {
    let w = World::new(LocalMutants {
        reuse_keys: true,
        ..LocalMutants::default()
    });
    let a = install(&w, SlotSpec::default());
    let b = install(&w, SlotSpec::default());
    drop((a, b));
    w.verify();
}

#[test]
#[should_panic(expected = "installed twice")]
fn mutant_reused_keys_across_threads_are_caught_by_verify() {
    // Separate tables do not collide, so only the audit can see it.
    let w = World::new(LocalMutants {
        reuse_keys: true,
        ..LocalMutants::default()
    });
    for _ in 0..2 {
        let w2 = Arc::clone(&w);
        thread::spawn(move || install(&w2, SlotSpec::default()))
            .join()
            .unwrap();
    }
    w.verify();
}

#[test]
#[should_panic(expected = "re-entered")]
fn mutant_a_borrow_held_across_the_destructor_is_caught() {
    let w = World::new(LocalMutants {
        hold_borrow_on_retire: true,
        ..LocalMutants::default()
    });
    let w2 = Arc::clone(&w);
    thread::spawn(move || {
        let hb = install(&w2, SlotSpec::default());
        let ha = install(
            &w2,
            SlotSpec {
                capture: Some(hb),
                release_on_call: false,
            },
        );
        drop(ha); // A's destructor drops B's handle: re-enters the borrowed table
    })
    .join()
    .unwrap();
    w.verify();
}

#[test]
#[should_panic(expected = "ended owner")]
fn mutant_a_post_to_an_ended_owner_treated_as_an_error_is_caught() {
    let w = World::new(LocalMutants {
        error_on_ended_post: true,
        ..LocalMutants::default()
    });
    let w2 = Arc::clone(&w);
    let h = thread::spawn(move || install(&w2, SlotSpec::default()))
        .join()
        .unwrap();
    drop(h);
    w.verify();
}

#[test]
fn reverse_teardown_order_still_satisfies_invariants() {
    scenarios::foreign_drop_after_owner_exit(LocalMutants {
        end_owner_after_slots: true,
        ..LocalMutants::default()
    });
}

#[test]
#[should_panic(expected = "was not retired by drain_retired")]
fn mutant_dropped_retire_posts_are_caught_before_thread_exit() {
    scenarios::foreign_drop_then_owner_drain(LocalMutants {
        drop_retire_posts: true,
        ..LocalMutants::default()
    });
}

#[test]
#[should_panic(expected = "dropped on thread")]
fn mutant_foreign_inline_drop_is_caught_by_e2() {
    scenarios::foreign_drop_then_owner_drain(LocalMutants {
        foreign_drop_inline: true,
        ..LocalMutants::default()
    });
}

#[test]
#[should_panic(expected = "called on thread")]
fn mutant_foreign_inline_call_is_caught_by_e3() {
    let w = World::new(LocalMutants {
        foreign_call_inline: true,
        ..LocalMutants::default()
    });
    let w2 = Arc::clone(&w);
    let h = thread::spawn(move || install(&w2, SlotSpec::default()))
        .join()
        .unwrap();
    let _ = h.call();
    drop(h);
    w.verify();
}

#[test]
#[should_panic(expected = "retire keys leaked")]
fn mutant_a_retire_for_an_exited_owner_is_leaked_and_caught_by_e6() {
    let w = World::new(LocalMutants {
        leak_retire_after_owner_exit: true,
        ..LocalMutants::default()
    });
    let w2 = Arc::clone(&w);
    let h = thread::spawn(move || install(&w2, SlotSpec::default()))
        .join()
        .unwrap();
    w.wait_exited(1);
    drop(h); // without the ended check this buffers a key nobody will ever drain
    w.verify();
}
