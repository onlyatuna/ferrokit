// SPDX-License-Identifier: LGPL-3.0-only
//! Window size limits: the logical minimum and maximum a window carries and the native tracking
//! sizes derived from them (`QWindow::minimumSize`/`maximumSize`, `QWindowsGeometryHint`).
//!
//! Limits are stored in logical (device-independent) pixels and converted to native pixels each
//! time the window system asks, with the window's current ratio. A stored native value would go
//! stale when the window moves to a screen with another scale factor; Qt keeps the logical value
//! for the same reason (`QWindowsGeometryHint::frameSizeConstraints`, qwindowswindow.cpp:1216-1236).

/// `QWINDOWSIZE_MAX` (qplatformwindow.h:27): the value of an axis without a maximum. It equals
/// `QWIDGETSIZE_MAX`.
pub const WINDOW_SIZE_MAX: i32 = (1 << 24) - 1;

/// A logical limit as `QWindow::setMinimumSize`/`setMaximumSize` store it: each axis in
/// `0..=WINDOW_SIZE_MAX` (`QWindowPrivate::setMinOrMaxSize`, qwindow.cpp:633-634).
pub fn clamp_size_limit(w: i32, h: i32) -> (i32, i32) {
    (w.clamp(0, WINDOW_SIZE_MAX), h.clamp(0, WINDOW_SIZE_MAX))
}

/// One axis of a logical limit in native pixels: `toNativeSizeConstrained`
/// (qwindowswindow.cpp:1055-1067). Only a value that is a real bound is scaled, rounded half away
/// from zero (`qRound`): 0 (no minimum) and `WINDOW_SIZE_MAX` (no maximum) keep their meaning at
/// every ratio. A ratio that is not positive leaves the value alone.
pub fn scale_size_limit(dip: i32, dpr: f32) -> i32 {
    if dpr > 0.0 && dip > 0 && dip < WINDOW_SIZE_MAX {
        (f64::from(dip) * f64::from(dpr)).round() as i32
    } else {
        dip
    }
}

/// The minimum and maximum size of a window in logical pixels. The default is no limit: minimum 0
/// and maximum `WINDOW_SIZE_MAX` on both axes, which is also how a limit is removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SizeLimits {
    pub min: (i32, i32),
    pub max: (i32, i32),
}

impl Default for SizeLimits {
    fn default() -> Self {
        Self {
            min: (0, 0),
            max: (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX),
        }
    }
}

/// The `MINMAXINFO` tracking sizes of a window: per axis `None` leaves the system's value alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackLimits {
    pub min: (Option<i32>, Option<i32>),
    pub max: (Option<i32>, Option<i32>),
}

impl SizeLimits {
    /// What `QWindowsGeometryHint::applyToMinMaxInfo` writes for these limits at ratio `dpr`
    /// (qwindowswindow.cpp:1216-1262), per axis:
    ///
    /// - a minimum of 0 is not written;
    /// - a maximum is written only when it is below `WINDOW_SIZE_MAX` (in native pixels, after
    ///   scaling), and never below the native minimum: a maximum under the minimum is raised to it;
    /// - an axis is independent of the other.
    ///
    /// Qt adds the frame margins to both here; a ferrokit window has none (its client area is the whole
    /// window rectangle), so nothing is added.
    pub fn native_track(&self, dpr: f32) -> TrackLimits {
        let (min_w, min_h) = (
            scale_size_limit(self.min.0, dpr),
            scale_size_limit(self.min.1, dpr),
        );
        let (max_w, max_h) = (
            scale_size_limit(self.max.0, dpr).max(min_w),
            scale_size_limit(self.max.1, dpr).max(min_h),
        );
        let min = |v: i32| (v > 0).then_some(v);
        let max = |v: i32| (v < WINDOW_SIZE_MAX).then_some(v);
        TrackLimits {
            min: (min(min_w), min(min_h)),
            max: (max(max_w), max(max_h)),
        }
    }

    /// The axes on which the window cannot change size: minimum equals maximum. Like
    /// `QWindowsWindow::handleNonClientHitTest` (qwindowswindow.cpp:3500-3507), a minimum that is
    /// empty on either axis means no axis counts as fixed.
    pub fn fixed_axes(&self) -> (bool, bool) {
        if self.min.0 <= 0 || self.min.1 <= 0 {
            return (false, false);
        }
        (self.min.0 == self.max.0, self.min.1 == self.max.1)
    }

    /// A requested size brought inside the limits like `QWidgetPrivate::setGeometry_sys`
    /// (qwidget.cpp:7299-7305): per axis the maximum first, then the minimum, so **the minimum
    /// wins** where the two conflict. (`QWindowPrivate::setMinOrMaxSize`, qwindow.cpp:653, uses the
    /// opposite order, but a widget window does not go through it: `setConstraints_sys`,
    /// qwidget.cpp:3975-3995, copies the limits to the `QWindow` directly.) This agrees with
    /// `native_track`, which raises a maximum under the minimum. A minimum of 0 leaves the value
    /// alone, so a size that is not positive is not changed by default limits.
    pub fn bound_size(&self, w: i32, h: i32) -> (i32, i32) {
        let axis = |v: i32, min: i32, max: i32| {
            let v = v.min(max);
            if min > 0 {
                v.max(min)
            } else {
                v
            }
        };
        (
            axis(w, self.min.0, self.max.0),
            axis(h, self.min.1, self.max.1),
        )
    }
}
