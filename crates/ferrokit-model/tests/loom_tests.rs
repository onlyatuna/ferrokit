// SPDX-License-Identifier: LGPL-3.0-only
//! Loom model tests: exhaustive verification of 2-thread concurrent operations.

#![cfg(feature = "loom")]

use ferrokit_model::model::*;
use ferrokit_model::sync::sys::*;

#[test]
fn test_loom_concurrent_set_parent_cycle_avoidance() {
    loom::model(|| {
        // Objects: 0 (A), 1 (B), 2 (C)
        let reg = Arc::new(HierarchyRegistry::new(3));

        // Initial setup: A (0) is parent of B (1)
        reg.set_parent(1, Some(0)).unwrap();
        reg.assert_invariants();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            // T1 tries to put A under C
            let _ = r1.set_parent(0, Some(2));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            // T2 tries to put C under B
            let _ = r2.set_parent(2, Some(1));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Must never form a cycle, must have zero lost updates
        reg.assert_invariants();
    });
}

#[test]
fn test_loom_concurrent_competing_reparent() {
    loom::model(|| {
        // Objects: 0 (Child), 1 (P1), 2 (P2)
        let reg = Arc::new(HierarchyRegistry::new(3));

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.set_parent(0, Some(1));
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.set_parent(0, Some(2));
        });

        t1.join().unwrap();
        t2.join().unwrap();

        reg.assert_invariants();
    });
}

#[test]
fn test_loom_concurrent_swap_and_set_parent() {
    loom::model(|| {
        // Objects: 0 (P1), 1 (P2), 2 (C)
        let reg = Arc::new(HierarchyRegistry::new(3));
        // C starts under P1
        reg.set_parent(2, Some(0)).unwrap();
        reg.assert_invariants();

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.swap_hierarchy(0, 1);
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            // T2 concurrently detaches C or moves C to None
            let _ = r2.set_parent(2, None);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        reg.assert_invariants();
    });
}

#[test]
fn test_loom_concurrent_competing_swaps() {
    loom::model(|| {
        // Objects: 0 (A), 1 (B), 2 (C)
        let reg = Arc::new(HierarchyRegistry::new(3));

        let r1 = Arc::clone(&reg);
        let t1 = thread::spawn(move || {
            let _ = r1.swap_hierarchy(0, 1);
        });

        let r2 = Arc::clone(&reg);
        let t2 = thread::spawn(move || {
            let _ = r2.swap_hierarchy(1, 2);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        reg.assert_invariants();
    });
}

#[test]
#[should_panic]
fn test_loom_negative_mutant_omit_h_fails() {
    loom::model(|| {
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

        // Without H, TOCTOU interleaving allows A->C and C->B->A cycle formation!
        reg.assert_invariants();
    });
}

#[test]
#[should_panic]
fn test_loom_negative_mutant_skip_revalidation_fails() {
    loom::model(|| {
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
    });
}

// ---- Thread-owned slot protocol (`src/local_slots.rs`) ----
// Loom model tests of the thread-owned slot protocol (see `src/local_slots.rs` for the scope of
// the model): every interleaving of two active threads (an owner and a foreign thread, or two
// owners) is explored, within a preemption bound.

use ferrokit_model::local_slots::{scenarios, LocalMutants};

/// Preemption bound for these scenarios: every interleaving with at most three preemptions per
/// execution is explored (loom's usual setting for "exhaustive, bounded"). The scenarios have a
/// handful of modeled operations (key counter, queue lock, exit counter), so this covers every
/// ordering of post / drain / exit / foreign drop; interleavings that need four or more
/// preemptions are NOT covered.
fn bounded(f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(f);
}

#[test]
fn test_loom_foreign_drop_then_owner_drain() {
    bounded(|| scenarios::foreign_drop_then_owner_drain(LocalMutants::default()));
}

#[test]
fn test_loom_foreign_drop_races_owner_exit() {
    bounded(|| scenarios::foreign_drop_after_owner_exit(LocalMutants::default()));
}

#[test]
fn test_loom_nested_retire_reenters_the_table() {
    bounded(|| scenarios::nested_retire(LocalMutants::default()));
}

#[test]
fn test_loom_call_releases_capture() {
    bounded(|| scenarios::call_releases_capture(LocalMutants::default()));
}

#[test]
fn test_loom_two_owners_exit_then_foreign_use() {
    // Two owners installing and tearing down concurrently: unbounded is too large for CI.
    bounded(|| scenarios::two_owners_exit_then_foreign_use(LocalMutants::default()));
}

#[test]
fn test_loom_owner_compare_is_only_a_diagnostic() {
    bounded(|| {
        scenarios::foreign_drop_then_owner_drain(LocalMutants {
            skip_owner_check: true,
            ..LocalMutants::default()
        })
    });
}

#[test]
#[should_panic]
fn test_loom_negative_mutant_borrow_held_during_retire_fails() {
    bounded(|| {
        scenarios::nested_retire(LocalMutants {
            hold_borrow_on_retire: true,
            ..LocalMutants::default()
        })
    });
}

#[test]
#[should_panic]
fn test_loom_negative_mutant_reused_keys_fail() {
    bounded(|| {
        scenarios::nested_retire(LocalMutants {
            reuse_keys: true,
            ..LocalMutants::default()
        })
    });
}

#[test]
#[should_panic]
fn test_loom_negative_mutant_error_on_ended_post_fails() {
    bounded(|| {
        scenarios::foreign_drop_after_owner_exit(LocalMutants {
            error_on_ended_post: true,
            ..LocalMutants::default()
        })
    });
}
