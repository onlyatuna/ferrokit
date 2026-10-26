// SPDX-License-Identifier: LGPL-3.0-only
//! Shuttle model tests: randomized and reproducible schedule testing.

#![cfg(feature = "shuttle")]

use ferrokit_model::model::*;
use ferrokit_model::sync::sys::*;

#[test]
fn test_shuttle_concurrent_set_parent_cycle_avoidance() {
    shuttle::check_random(|| {
        let reg = Arc::new(HierarchyRegistry::new(4));

        // Initial setup: 0 -> 1 -> 2
        reg.set_parent(1, Some(0)).unwrap();
        reg.set_parent(2, Some(1)).unwrap();
        reg.assert_invariants();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            // T1 moves root 0 under 3
            let _ = r1.set_parent(0, Some(3));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            // T2 moves 3 under 2 (potential cycle 0->3->2->1->0)
            let _ = r2.set_parent(3, Some(2));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        reg.assert_invariants();
    }, 100);
}

#[test]
fn test_shuttle_concurrent_swap_and_set_parent() {
    shuttle::check_random(|| {
        let reg = Arc::new(HierarchyRegistry::new(4));

        // Initial: 2 is under 0, 3 is under 1
        reg.set_parent(2, Some(0)).unwrap();
        reg.set_parent(3, Some(1)).unwrap();
        reg.assert_invariants();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.swap_hierarchy(0, 1);
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(2, Some(1));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        reg.assert_invariants();
    }, 100);
}

#[test]
fn test_shuttle_competing_reparent_three_threads() {
    shuttle::check_random(|| {
        let reg = Arc::new(HierarchyRegistry::new(4));

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.set_parent(0, Some(1));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(0, Some(2));
        });

        let r3 = Arc::clone(&reg);
        let t3 = thread::spawn(move || {
            let _ = r3.set_parent(0, Some(3));
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        reg.assert_invariants();
    }, 100);
}

#[test]
#[should_panic]
fn test_shuttle_negative_mutant_omit_h_fails() {
    shuttle::check_random(|| {
        let mut config = MutantConfig::default();
        config.omit_h = true;
        let reg = Arc::new(HierarchyRegistry::with_mutants(3, config));

        reg.set_parent(1, Some(0)).unwrap();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.set_parent(0, Some(2));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(2, Some(1));
        });

        let _ = t1.join();
        let _ = t2.join();

        reg.assert_invariants();
    }, 100);
}

#[test]
#[should_panic]
fn test_shuttle_negative_mutant_invert_l1_deadlocks() {
    shuttle::check_random(|| {
        let mut config = MutantConfig::default();
        config.invert_l1 = true;
        config.omit_h = true;
        let reg = Arc::new(HierarchyRegistry::with_mutants(4, config));

        reg.set_parent(2, Some(0)).unwrap();
        reg.set_parent(3, Some(1)).unwrap();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.set_parent(2, Some(1));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(3, Some(0));
        });

        let _ = t1.join();
        let _ = t2.join();

        reg.assert_invariants();
    }, 100);
}

#[test]
#[should_panic]
fn test_shuttle_negative_mutant_skip_revalidation_fails() {
    shuttle::check_random(|| {
        let mut config = MutantConfig::default();
        config.skip_revalidation = true;
        config.omit_h = true;
        let reg = Arc::new(HierarchyRegistry::with_mutants(4, config));

        reg.set_parent(2, Some(0)).unwrap();
        reg.set_parent(3, Some(1)).unwrap();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.swap_hierarchy(0, 1);
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(2, Some(1));
        });

        let _ = t1.join();
        let _ = t2.join();

        reg.assert_invariants();
    }, 100);
}

// ---- Thread-owned slot protocol (`src/local_slots.rs`) ----
// Shuttle model tests of the thread-owned slot protocol (see `src/local_slots.rs` for the scope
// of the model): randomized, reproducible schedules of the same scenarios Loom explores.
//

use ferrokit_model::local_slots::{scenarios, LocalMutants};

const ITERATIONS: usize = 500;

#[test]
fn test_shuttle_foreign_drop_then_owner_drain() {
    shuttle::check_random(
        || scenarios::foreign_drop_then_owner_drain(LocalMutants::default()),
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_foreign_drop_races_owner_exit() {
    shuttle::check_random(
        || scenarios::foreign_drop_after_owner_exit(LocalMutants::default()),
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_nested_retire_reenters_the_table() {
    shuttle::check_random(
        || scenarios::nested_retire(LocalMutants::default()),
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_call_releases_capture() {
    shuttle::check_random(
        || scenarios::call_releases_capture(LocalMutants::default()),
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_two_owners_exit_then_foreign_use() {
    shuttle::check_random(
        || scenarios::two_owners_exit_then_foreign_use(LocalMutants::default()),
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_owner_compare_is_only_a_diagnostic() {
    shuttle::check_random(
        || {
            scenarios::foreign_drop_then_owner_drain(LocalMutants {
                skip_owner_check: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "re-entered")]
fn test_shuttle_negative_mutant_borrow_held_during_retire_fails() {
    shuttle::check_random(
        || {
            scenarios::nested_retire(LocalMutants {
                hold_borrow_on_retire: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "installed twice")]
fn test_shuttle_negative_mutant_reused_keys_fail() {
    shuttle::check_random(
        || {
            scenarios::nested_retire(LocalMutants {
                reuse_keys: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "ended owner")]
fn test_shuttle_negative_mutant_error_on_ended_post_fails() {
    shuttle::check_random(
        || {
            scenarios::foreign_drop_after_owner_exit(LocalMutants {
                error_on_ended_post: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
fn test_shuttle_reverse_teardown_order() {
    shuttle::check_random(
        || {
            scenarios::foreign_drop_after_owner_exit(LocalMutants {
                close_queue_after_slots: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "was not retired by drain_retired")]
fn test_shuttle_negative_mutant_dropped_retire_posts_fail() {
    shuttle::check_random(
        || {
            scenarios::foreign_drop_then_owner_drain(LocalMutants {
                drop_retire_posts: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}

#[test]
#[should_panic(expected = "dropped on thread")]
fn test_shuttle_negative_mutant_foreign_inline_drop_fails() {
    shuttle::check_random(
        || {
            scenarios::foreign_drop_then_owner_drain(LocalMutants {
                foreign_drop_inline: true,
                ..LocalMutants::default()
            })
        },
        ITERATIONS,
    );
}
