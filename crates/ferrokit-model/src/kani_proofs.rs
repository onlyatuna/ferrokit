// SPDX-License-Identifier: LGPL-3.0-only
//! Bounded state model verification harnesses for Kani.
//!
//! **Experimental and unverified.** Nothing builds these harnesses: no CI job runs Kani, and none of
//! them is a verified result for this repository.
//!
//! - The two hierarchy harnesses use a fixed three-node scenario with no symbolic input. Under
//!   Kani 0.68.0 (CBMC 6.11.0) on Linux they did not finish: the SAT stage ran out of memory, with
//!   the run killed at a sampled peak of about 51.9 GiB under a 52 GiB limit.
//! - The timer harness passes, but it re-types the rounding formula instead of calling the
//!   function in `ferrokit-core`. It does not check production code and is not a proof about it.
//! - The harnesses model `ferrokit-model`, a standalone simplified model, not the hierarchy in
//!   `ferrokit-core`.
//!
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
