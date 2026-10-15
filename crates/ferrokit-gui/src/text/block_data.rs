// SPDX-License-Identifier: LGPL-3.0-only
//! Custom user data attached to a document block (`QTextBlockUserData` equivalent).

use std::fmt::Debug;

/// Trait for arbitrary user-defined metadata attached to a `TextBlock`.
pub trait TextBlockUserData: Debug + Send + Sync + 'static {}

/// Canonical Qt alias.
pub type QTextBlockUserData = dyn TextBlockUserData;
