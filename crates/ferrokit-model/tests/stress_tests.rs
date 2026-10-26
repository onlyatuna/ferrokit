// SPDX-License-Identifier: LGPL-3.0-only
//! Native concurrency stress test for ThreadSanitizer.

#[cfg(not(any(feature = "loom", feature = "shuttle")))]
mod native_stress {
    use ferrokit_model::model::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_native_concurrent_stress_tsan() {
        let reg = Arc::new(HierarchyRegistry::new(6));

        // Initial setup
        reg.set_parent(1, Some(0)).unwrap();
        reg.set_parent(2, Some(0)).unwrap();
        reg.set_parent(3, Some(1)).unwrap();
        reg.set_parent(4, Some(2)).unwrap();
        reg.assert_invariants();

        let mut handles = Vec::new();

        // Thread 1: alternates reparenting between 0 and 2
        let r1 = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let _ = r1.set_parent(3, Some(0));
                let _ = r1.set_parent(3, Some(2));
            }
        }));

        // Thread 2: alternates swapping 1 and 2
        let r2 = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let _ = r2.swap_hierarchy(1, 2);
            }
        }));

        // Thread 3: moves 4 between 1, 2, and None
        let r3 = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let _ = r3.set_parent(4, Some(1));
                let _ = r3.set_parent(4, None);
            }
        }));

        // Thread 4: moves 5 under 0, 1, 2
        let r4 = Arc::clone(&reg);
        handles.push(thread::spawn(move || {
            for _ in 0..200 {
                let _ = r4.set_parent(5, Some(0));
                let _ = r4.set_parent(5, Some(1));
            }
        }));

        for h in handles {
            h.join().unwrap();
        }

        reg.assert_invariants();
    }

    /// Thread-owned slots (`src/local_slots.rs`) on real threads, for ThreadSanitizer: owners
    /// install slots with nested captures, publish handles on a shared board, call and drop each
    /// other's handles, drain their queues and exit while foreign drops are still in flight.
    #[test]
    fn test_native_local_slots_stress_tsan() {
        use ferrokit_model::local_slots::{drain_retired, install, scenarios, Handle};
        use ferrokit_model::local_slots::{LocalMutants, SlotSpec, World};
        use std::sync::Mutex;

        for _ in 0..50 {
            scenarios::foreign_drop_then_owner_drain(LocalMutants::default());
            scenarios::foreign_drop_after_owner_exit(LocalMutants::default());
            scenarios::nested_retire(LocalMutants::default());
            scenarios::call_releases_capture(LocalMutants::default());
            scenarios::two_owners_exit_then_foreign_use(LocalMutants::default());
        }

        const OWNERS: usize = 4;
        const ROUNDS: usize = 300;
        let world = World::new(LocalMutants::default());
        let board: Arc<Mutex<Vec<Arc<Handle>>>> = Arc::new(Mutex::new(Vec::new()));

        let mut threads = Vec::new();
        for seed in 0..OWNERS {
            let world = Arc::clone(&world);
            let board = Arc::clone(&board);
            threads.push(thread::spawn(move || {
                let mut rng = (seed as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                let mut next = move || {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    rng
                };
                let mut mine: Vec<Arc<Handle>> = Vec::new();
                let first = install(&world, SlotSpec::default());
                let mut me: Option<u64> = Some(first.owner());
                board.lock().unwrap().push(Arc::clone(&first));
                mine.push(first);
                for _ in 0..ROUNDS {
                    match next() % 4 {
                        0 => {
                            // Install, sometimes capturing another of my handles.
                            let capture = if next() % 2 == 0 { mine.pop() } else { None };
                            let h = install(
                                &world,
                                SlotSpec {
                                    release_on_call: next() % 2 == 0,
                                    capture,
                                },
                            );
                            me = Some(h.owner());
                            board.lock().unwrap().push(Arc::clone(&h));
                            mine.push(h);
                        }
                        1 => {
                            // Call one of mine (runs), or a foreign one from the board (skipped).
                            if let Some(h) = mine.last() {
                                assert!(h.call(), "a live handle of mine must run its slot");
                            }
                            let other = board.lock().unwrap().pop();
                            if let Some(h) = other {
                                if me != Some(h.owner()) {
                                    assert!(!h.call(), "a slot must not run off its owner");
                                }
                                drop(h); // foreign or own: retire or post
                            }
                        }
                        2 => {
                            drain_retired(&world);
                        }
                        _ => {
                            if !mine.is_empty() {
                                let i = (next() as usize) % mine.len();
                                drop(mine.swap_remove(i));
                            }
                        }
                    }
                }
                // Exit with slots and handles in flight: handles of mine still on the board are
                // dropped by other threads or by the joiner, posting to an ended owner.
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        world.wait_exited(OWNERS);
        board.lock().unwrap().clear();
        world.verify();
    }
}
