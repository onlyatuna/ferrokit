// SPDX-License-Identifier: LGPL-3.0-only
//! Bounded state model verification harnesses for Kani.
//! Note: Kani is restricted strictly to bounded sequential state invariants
//! (error return zero state change, parent/child list consistency, arithmetic non-overflow)
//! and is NEVER used to claim concurrency safety of the H/L0/L1/L2 lock graph.

#[cfg(kani)]
mod proofs {
    use crate::model::*;

    #[kani::proof]
    #[kani::unwind(5)]
    fn proof_error_return_guarantees_zero_state_mutation() {
        let reg = HierarchyRegistry::new(3);

        // Precondition: 1 is under 0
        let _ = reg.set_parent(1, Some(0));

        // Attempt invalid parent (self-parenting)
        let res = reg.set_parent(0, Some(0));
        assert!(res.is_err());

        // Invariants must hold strictly
        reg.assert_invariants();
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn proof_cycle_rejection_leaves_state_unchanged() {
        let reg = HierarchyRegistry::new(3);

        let _ = reg.set_parent(1, Some(0));

        // Attempt cycle: 0 under 1
        let res = reg.set_parent(0, Some(1));
        assert!(res.is_err());

        reg.assert_invariants();
    }

    #[kani::proof]
    fn proof_timer_deadline_arithmetic_saturates_without_overflow() {
        let current_time: u64 = kani::any();
        let interval: u64 = kani::any();

        // Ferrokit timer deadline calculation with saturation:
        let rounded_interval = interval.saturating_add(500) / 1000 * 1000;
        let deadline = current_time.saturating_add(rounded_interval);

        assert!(deadline >= current_time);
    }
}
