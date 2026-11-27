// SPDX-License-Identifier: MIT OR Apache-2.0
//! Standalone concurrency models: hierarchy transactions (`model`) and thread-owned signal
//! slots (`local_slots`).

pub mod sync;
pub mod kani_proofs;
pub mod local_slots;
pub mod model;

pub use model::*;
