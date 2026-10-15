// SPDX-License-Identifier: LGPL-3.0-only
pub mod dispatcher;
pub mod socket_notifier;
pub use socket_notifier::*;

pub use dispatcher::*;

pub mod r#loop;
pub use r#loop::*;

#[cfg(windows)]
pub mod dispatcher_win;

#[cfg(windows)]
pub use dispatcher_win::*;
pub mod dispatcher_unix;
pub use dispatcher_unix::{EpollReactor, UnixEventDispatcher, UnixEventDispatcherHandle};

#[cfg(target_os = "macos")]
pub mod dispatcher_cocoa;
#[cfg(target_os = "macos")]
pub use dispatcher_cocoa::{CocoaEventDispatcher, CocoaEventDispatcherHandle};
