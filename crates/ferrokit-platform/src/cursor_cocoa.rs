// SPDX-License-Identifier: LGPL-3.0-only
//! Pointer position on macOS: `QCocoaCursor::pos` / `setPos` over Quartz Event Services.
//!
//! Quartz global display coordinates already have their origin at the top-left of the primary
//! display and are measured in points, which is the "native desktop" space `PlatformCursor`
//! uses, so no flip against the screen height is needed (unlike `[NSEvent mouseLocation]`).

use ferrokit_gui::geometry::primitives::Point;

/// The two Quartz calls behind the cursor position.
pub trait CursorServices {
    /// `CGEventGetLocation` of a fresh event: the pointer in top-left global points, `None` when
    /// the event cannot be created.
    fn location(&self) -> Option<(f64, f64)>;
    /// `CGWarpMouseCursorPosition` followed by `CGAssociateMouseAndMouseCursorPosition(true)`.
    fn warp(&self, x: f64, y: f64);
}

/// The pointer position as a [`Point`] (rounded to whole points, like `QPointF::toPoint`).
pub fn cursor_pos(services: &dyn CursorServices) -> Option<Point> {
    let (x, y) = services.location()?;
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    Some(Point::new(x.round() as i32, y.round() as i32))
}

/// Moves the pointer to `pos` (`QCocoaCursor::setPos`).
pub fn set_cursor_pos(services: &dyn CursorServices, pos: Point) {
    services.warp(f64::from(pos.x), f64::from(pos.y));
}

/// The real Quartz services on macOS; elsewhere there is no pointer to query or move.
pub struct NativeCursorServices;

#[cfg(target_os = "macos")]
mod native {
    use super::{CursorServices, NativeCursorServices};
    use crate::objc_runtime::CGPoint;
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventCreate(source: *const c_void) -> *mut c_void;
        fn CGEventGetLocation(event: *const c_void) -> CGPoint;
        fn CGWarpMouseCursorPosition(new_cursor_position: CGPoint) -> i32;
        fn CGAssociateMouseAndMouseCursorPosition(connected: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(object: *const c_void);
    }

    impl CursorServices for NativeCursorServices {
        fn location(&self) -> Option<(f64, f64)> {
            // SAFETY: `CGEventCreate(NULL)` returns an owned (+1) event or NULL, and NULL is
            // checked before use; the location is read while the event is alive and the event is
            // released exactly once. These Quartz functions take no Rust pointers (the only
            // pointer is NULL by design) and are not main-thread-bound per the Quartz Event
            // Services documentation [inferred].
            unsafe {
                let event = CGEventCreate(std::ptr::null());
                if event.is_null() {
                    return None;
                }
                let point = CGEventGetLocation(event);
                CFRelease(event);
                Some((point.x, point.y))
            }
        }

        fn warp(&self, x: f64, y: f64) {
            // SAFETY: both calls take plain values (a by-value `CGPoint`, a `boolean_t`), no
            // pointers. The returned `CGError` is ignored on purpose: a failed warp leaves the
            // pointer where it was, which `QCursor::setPos` allows.
            unsafe {
                CGWarpMouseCursorPosition(CGPoint::new(x, y));
                // A warp detaches the pointer from the mouse until the next association.
                CGAssociateMouseAndMouseCursorPosition(1);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl CursorServices for NativeCursorServices {
    fn location(&self) -> Option<(f64, f64)> {
        None
    }

    fn warp(&self, _x: f64, _y: f64) {}
}
