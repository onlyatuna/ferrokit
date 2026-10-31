// SPDX-License-Identifier: LGPL-3.0-only
//! Signals and slots (`QObject::connect`).
//!
//! [`Signal`] is a `Send + Sync + Clone` handle, so [`Signal::connect`] and the other `connect*`
//! methods take `Send + Sync` slots. A slot that has to capture state of one thread (an `Rc`, a
//! `RefCell`) is connected with [`Signal::connect_local`], [`Signal::connect_local_scoped`] or
//! [`Signal::connect_local_single_shot`]: the closure stays in a table of the connecting thread
//! and runs only when the signal is emitted from that thread. An emission from another thread
//! skips it, where Qt would run it in the emitting thread. The `connect_local` documentation lists
//! this and the other differences from Qt (when the closure is dropped, thread exit).
//! [`Signal::connect_local_to`] and its variants connect against a context object (Qt's
//! `connect(sender, signal, context, functor, type)`) and also work across threads when
//! `T: Clone + Send`.
pub(crate) mod local;
#[allow(clippy::module_inception)]
pub mod signal;
pub use crate::object::SignalBlocker;
pub use local::{has_pending_local_retires, LocalConnectError};
pub use signal::*;
