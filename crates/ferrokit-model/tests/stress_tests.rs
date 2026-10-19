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
}
