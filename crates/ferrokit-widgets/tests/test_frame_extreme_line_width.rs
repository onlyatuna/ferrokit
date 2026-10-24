// SPDX-License-Identifier: LGPL-3.0-only
//! An extreme `lineWidth`/`midLineWidth` must not overflow the frame-width arithmetic or make the
//! bevel painter loop once per pixel of the width (Qt's own `int` math is undefined there).

use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_gui::paint::pixmap::Pixmap;
use ferrokit_gui::paint::Painter;
use ferrokit_widgets::frame::{Frame, FrameShadow, FrameShape, FrameStyle};
use ferrokit_gui::paint::palette::Palette;
use ferrokit_widgets::widget::Widget;

#[test]
fn frame_width_and_contents_rect_saturate() {
    let mut frame = Frame::with_style(FrameShape::Box, FrameShadow::Raised);
    frame.set_line_width(i32::MAX);
    frame.set_mid_line_width(i32::MAX);
    assert_eq!(frame.frame_width(), i32::MAX);
    let contents = frame.contents_rect();
    assert_eq!((contents.width, contents.height), (0, 0));
    assert_eq!(frame.size_hint(), Size::new(-1, -1));
}

#[test]
fn painting_an_extreme_line_width_terminates() {
    let mut pixmap = Pixmap::new(20, 20).unwrap();
    let palette = Palette::light();
    for (shape, shadow) in [
        (FrameShape::Box, FrameShadow::Plain),
        (FrameShape::Box, FrameShadow::Raised),
        (FrameShape::Panel, FrameShadow::Sunken),
        (FrameShape::HLine, FrameShadow::Raised),
        (FrameShape::VLine, FrameShadow::Plain),
    ] {
        let mut style = FrameStyle::new(shape, shadow);
        style.line_width = i32::MAX;
        style.mid_line_width = i32::MAX;
        let mut painter = Painter::begin(&mut pixmap);
        style.paint(&mut painter, Rect::new(0, 0, 20, 20), &palette, true);
    }
}
