// SPDX-License-Identifier: LGPL-3.0-only
//! Frame size_hint and minimum_size_hint match Qt.
//!
//! Qt reference: `QFrame::sizeHint()` (`qframe.cpp:465-479`):
//! - `HLine`: `QSize(-1, 3)`
//! - `VLine`: `QSize(3, -1)`
//! - other: `QWidget::sizeHint()` (`qwidget.cpp:8884`), returning layout totalSizeHint or `QSize(-1, -1)`.
//!   `minimumSizeHint()` (`qwidget.cpp:8911`): returns layout totalMinimumSize or `QSize(-1, -1)`.

use ferrokit_gui::geometry::primitives::Size;
use ferrokit_widgets::frame::{Frame, FrameShape};
use ferrokit_widgets::widget::Widget;

#[test]
fn frame_without_layout_size_hint_matches_qt() {
    let frame = Frame::new();
    assert_eq!(
        frame.size_hint(),
        Size::new(-1, -1),
        "Frame without layout sizeHint must be (-1, -1)"
    );
}

#[test]
fn frame_without_layout_minimum_size_hint_matches_qt() {
    let frame = Frame::new();
    assert_eq!(
        frame.minimum_size_hint(),
        Size::new(-1, -1),
        "Frame without layout minimumSizeHint must be (-1, -1)"
    );
}

#[test]
fn hline_frame_size_hint_matches_qt() {
    let mut frame = Frame::new();
    frame.set_frame_shape(FrameShape::HLine);
    assert_eq!(
        frame.size_hint(),
        Size::new(-1, 3),
        "HLine sizeHint must be (-1, 3) (qframe.cpp:473)"
    );
    assert_eq!(
        frame.minimum_size_hint(),
        Size::new(-1, -1),
        "HLine minimumSizeHint must be (-1, -1) (qwidget.cpp:8911)"
    );
}

#[test]
fn vline_frame_size_hint_matches_qt() {
    let mut frame = Frame::new();
    frame.set_frame_shape(FrameShape::VLine);
    assert_eq!(
        frame.size_hint(),
        Size::new(3, -1),
        "VLine sizeHint must be (3, -1) (qframe.cpp:475)"
    );
    assert_eq!(
        frame.minimum_size_hint(),
        Size::new(-1, -1),
        "VLine minimumSizeHint must be (-1, -1) (qwidget.cpp:8911)"
    );
}
