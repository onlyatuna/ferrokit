// SPDX-License-Identifier: LGPL-3.0-only
//! Native size limits from logical ones, independent of the platform: `toNativeSizeConstrained`
//! (qwindowswindow.cpp:1055-1067) and the way `QWindowsGeometryHint::applyToMinMaxInfo`
//! (:1238-1262) chooses which `MINMAXINFO` fields to write. Pure functions: this is not a test of
//! any window system.

use ferrokit_platform::size_limits::{
    SizeLimits, TrackLimits, WINDOW_SIZE_MAX, clamp_size_limit, scale_size_limit,
};

const NONE: Option<i32> = None;

fn limits(min: (i32, i32), max: (i32, i32)) -> SizeLimits {
    SizeLimits { min, max }
}

fn min_only(min: (i32, i32)) -> SizeLimits {
    limits(min, (WINDOW_SIZE_MAX, WINDOW_SIZE_MAX))
}

fn max_only(max: (i32, i32)) -> SizeLimits {
    limits((0, 0), max)
}

// ---- Minimum -------------------------------------------------------------------------------

#[test]
fn a_bound_scales_by_the_ratio_and_rounds_half_away_from_zero() {
    // Windows' own scale steps (96/120/144/168/192/240 dpi) and the values where rounding matters.
    for (dip, dpr, native) in [
        (300, 1.0, 300),
        (300, 1.25, 375),
        (300, 1.5, 450),
        (300, 1.75, 525),
        (300, 2.0, 600),
        (300, 2.5, 750),
        (15, 1.25, 19), // 18.75
        (2, 1.25, 3),   // 2.5 rounds up
        (1, 1.25, 1),   // 1.25
        (3, 1.5, 5),    // 4.5 rounds up
        (7, 1.75, 12),  // 12.25
        (1, 2.5, 3),    // 2.5 rounds up
    ] {
        assert_eq!(scale_size_limit(dip, dpr), native, "{dip} at {dpr}");
    }
}

#[test]
fn no_minimum_and_no_maximum_keep_their_meaning_at_every_ratio() {
    for dpr in [1.0, 1.25, 1.5, 2.0, 2.5] {
        assert_eq!(scale_size_limit(0, dpr), 0, "0 means no minimum at {dpr}");
        assert_eq!(
            scale_size_limit(WINDOW_SIZE_MAX, dpr),
            WINDOW_SIZE_MAX,
            "WINDOW_SIZE_MAX means no maximum at {dpr}"
        );
    }
}

#[test]
fn the_largest_real_bound_is_still_scaled() {
    // One below "no maximum" is a bound like any other (the comparison is `< QWINDOWSIZE_MAX`).
    assert_eq!(scale_size_limit(WINDOW_SIZE_MAX - 1, 1.5), 25_165_821);
}

#[test]
fn a_ratio_that_is_not_positive_leaves_the_value_alone() {
    assert_eq!(scale_size_limit(300, 0.0), 300);
    assert_eq!(scale_size_limit(300, -1.5), 300);
}

#[test]
fn an_axis_without_a_minimum_is_not_reported() {
    // `None` keeps the system's value in `MINMAXINFO` (Qt only overrides an axis whose minimum is positive).
    assert_eq!(min_only((300, 0)).native_track(1.25).min, (Some(375), NONE));
    assert_eq!(min_only((0, 200)).native_track(1.5).min, (NONE, Some(300)));
    assert_eq!(min_only((0, 0)).native_track(2.0).min, (NONE, NONE));
    assert_eq!(
        min_only((300, 200)).native_track(1.0).min,
        (Some(300), Some(200))
    );
}

// ---- Maximum -------------------------------------------------------------------------------

#[test]
fn a_maximum_scales_like_a_minimum() {
    let t = max_only((400, 300)).native_track(1.25);
    assert_eq!(t.max, (Some(500), Some(375)));
    assert_eq!(t.min, (NONE, NONE));
}

#[test]
fn no_maximum_is_not_reported_at_any_ratio() {
    for dpr in [1.0, 1.25, 1.5, 2.5] {
        assert_eq!(
            SizeLimits::default().native_track(dpr),
            TrackLimits {
                min: (NONE, NONE),
                max: (NONE, NONE)
            }
        );
    }
}

#[test]
fn a_maximum_on_one_axis_leaves_the_other_axis_alone() {
    let t = max_only((400, WINDOW_SIZE_MAX)).native_track(1.5);
    assert_eq!(t.max, (Some(600), NONE));
    let t = max_only((WINDOW_SIZE_MAX, 300)).native_track(1.5);
    assert_eq!(t.max, (NONE, Some(450)));
}

#[test]
fn a_maximum_below_the_minimum_is_raised_to_it_per_axis() {
    // qwindowswindow.cpp:1223-1224: `qMax(maximumSize, minimumSize)`, in native pixels.
    let t = limits((300, 200), (200, 500)).native_track(1.0);
    assert_eq!(t.min, (Some(300), Some(200)));
    assert_eq!(
        t.max,
        (Some(300), Some(500)),
        "width raised to the minimum, height kept"
    );
    // The comparison is after scaling, so the order of the two does not depend on the ratio.
    let t = limits((300, 200), (200, 100)).native_track(1.25);
    assert_eq!(t.max, (Some(375), Some(250)));
}

#[test]
fn a_maximum_that_scales_past_no_maximum_is_not_reported() {
    // Qt compares after scaling (`maximumWidth < QWINDOWSIZE_MAX`): 16,000,000 x 1.25 is above it.
    let t = max_only((16_000_000, 300)).native_track(1.25);
    assert_eq!(t.max, (NONE, Some(375)));
}

#[test]
fn a_maximum_of_zero_is_reported_like_qt_does() {
    // `toNativeSizeConstrained` leaves 0 alone, and 0 is below QWINDOWSIZE_MAX, so Qt writes it.
    assert_eq!(
        max_only((0, 300)).native_track(1.5).max,
        (Some(0), Some(450))
    );
}

// ---- Fixed size ----------------------------------------------------------------------------

#[test]
fn a_fixed_size_is_the_same_native_size_as_minimum_and_maximum() {
    let t = limits((400, 300), (400, 300)).native_track(1.5);
    assert_eq!(t.min, (Some(600), Some(450)));
    assert_eq!(t.max, (Some(600), Some(450)));
}

#[test]
fn fixed_axes_need_a_minimum_that_is_not_empty_and_equal_to_the_maximum() {
    // `QWindowsWindow::handleNonClientHitTest` (qwindowswindow.cpp:3500-3507).
    assert_eq!(limits((400, 300), (400, 300)).fixed_axes(), (true, true));
    assert_eq!(limits((400, 300), (400, 900)).fixed_axes(), (true, false));
    assert_eq!(limits((400, 300), (900, 300)).fixed_axes(), (false, true));
    assert_eq!(limits((400, 300), (900, 900)).fixed_axes(), (false, false));
    // An empty minimum on either axis: nothing counts as fixed, even where the two are equal.
    assert_eq!(limits((400, 0), (400, 0)).fixed_axes(), (false, false));
    assert_eq!(limits((0, 300), (0, 300)).fixed_axes(), (false, false));
    assert_eq!(SizeLimits::default().fixed_axes(), (false, false));
}

// ---- Stored form ---------------------------------------------------------------------------

#[test]
fn a_stored_limit_is_kept_between_zero_and_no_maximum() {
    assert_eq!(clamp_size_limit(-5, 1 << 30), (0, WINDOW_SIZE_MAX));
    assert_eq!(clamp_size_limit(300, 200), (300, 200));
    assert_eq!(clamp_size_limit(WINDOW_SIZE_MAX, 0), (WINDOW_SIZE_MAX, 0));
}

// ---- Bounding a requested size ---------------------------------------------------------------

#[test]
fn a_requested_size_is_bounded_per_axis_and_the_minimum_wins() {
    // `QWidgetPrivate::setGeometry_sys` (qwidget.cpp:7299-7305): `qMin` with the maximum, then
    // `qMax` with the minimum.
    let l = limits((100, 80), (300, 200));
    assert_eq!(l.bound_size(50, 30), (100, 80));
    assert_eq!(l.bound_size(500, 900), (300, 200));
    assert_eq!(l.bound_size(220, 120), (220, 120));
    assert_eq!(l.bound_size(50, 900), (100, 200));
    // Maximum below minimum: the minimum, on that axis only.
    let l = limits((200, 100), (150, 300));
    assert_eq!(l.bound_size(50, 500), (200, 300));
    assert_eq!(l.bound_size(500, 50), (200, 100));
}

#[test]
fn default_limits_leave_every_size_alone() {
    for (w, h) in [(0, 0), (7, 3), (5000, 4000), (-4, 9)] {
        assert_eq!(SizeLimits::default().bound_size(w, h), (w, h));
    }
}
