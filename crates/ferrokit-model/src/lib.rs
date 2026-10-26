// SPDX-License-Identifier: LGPL-3.0-only
//! Standalone concurrency models: hierarchy transactions (`model`) and thread-owned signal
//! slots (`local_slots`).

pub mod sync;
pub mod kani_proofs;
pub mod local_slots;
pub mod model;

pub use model::*;
