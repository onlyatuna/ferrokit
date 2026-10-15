// SPDX-License-Identifier: LGPL-3.0-only
//! Cross-platform I/O subsystem and abstractions (`Qt corelib/io` equivalent).
//!
//! Provides file and stream devices, in-memory buffers, directory management,
//! path manipulation, virtual embedded resources, standard system paths,
//! external process execution, temporary files/directories, advisory lock files,
//! and storage volume inspection.

pub mod buffer;
pub mod device;
pub mod dir;
pub mod file;
pub mod lock_file;
pub mod path;
pub mod process;
pub mod resource;
pub mod standard_paths;
pub mod storage_info;
pub mod temp;

pub use buffer::*;
pub use device::*;
pub use dir::*;
pub use file::*;
pub use lock_file::*;
pub use path::*;
pub use process::*;
pub use resource::*;
pub use standard_paths::*;
pub use storage_info::*;
pub use temp::*;
