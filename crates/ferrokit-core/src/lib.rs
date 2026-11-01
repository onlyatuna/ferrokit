// SPDX-License-Identifier: LGPL-3.0-only
//! ferrokit-core: Core primitives, event system, and object model.

pub mod animation;
pub mod event;
pub mod event_loop;
pub mod fs;
pub mod io;
pub mod json;
pub mod meta;
pub mod object;
mod poison;
pub mod property;
pub mod serialize;
pub mod settings;
pub mod signal;
pub mod thread;
pub mod timer;
pub mod types;
pub mod variant;

pub use animation::*;
pub use event::*;
pub use event_loop::*;
pub use fs::*;
pub use io::*;
pub use json::*;
pub use meta::*;
pub use object::*;
pub use property::*;
pub use serialize::*;
pub use settings::*;
pub use signal::*;
pub use thread::*;
pub use timer::*;
pub use types::*;
pub use variant::*;
pub mod application;
pub use application::*;

#[cfg(feature = "derive")]
pub use ferrokit_derive::{QObject, slot, slots};
