// SPDX-License-Identifier: LGPL-3.0-only
//! 3D Mathematics and Transformations (`QVector2D`, `QVector3D`, `QVector4D`, `QMatrix4x4`, `QQuaternion`).
//!
//! Provides vector math, 4x4 matrix transformations, projections (perspective and orthographic),
//! camera view matrices (look-at), and quaternions with SLERP for 3D rotations.

pub mod matrix4x4;
pub mod quaternion;
pub mod vector2d;
pub mod vector3d;
pub mod vector4d;

pub use matrix4x4::*;
pub use quaternion::*;
pub use vector2d::*;
pub use vector3d::*;
pub use vector4d::*;

// --- Qt Canonical Aliases ---
pub type QVector2D = Vector2D;
pub type QVector3D = Vector3D;
pub type QVector4D = Vector4D;
pub type QMatrix4x4 = Matrix4x4;
pub type QQuaternion = Quaternion;
