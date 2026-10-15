// SPDX-License-Identifier: LGPL-3.0-only
//! What a `QScrollArea` paints: its content widget once per paint, inside the viewport, with the
//! scroll bars on top.
//! Qt: `QScrollArea::setWidget` reparents the content to the viewport (qscrollarea.cpp:242-243);
//! the viewport is created first and the scroll bar containers after it, as siblings
//! (qabstractscrollarea.cpp:254, 258, 266), so the bars stack above it. A child paints once,
//! clipped to its parent (`QWidgetPrivate::clipRect`, qwidget.cpp:1904-1922).
#![cfg(windows)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ferrokit_gui::geometry::primitives::{Rect, RectF};
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::WindowFlags;
use ferrokit_widgets::scroll::ScrollArea;
use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
use ferrokit_widgets::window::Window;

const GREEN: (u8, u8, u8, u8) = (0, 255, 0, 255);

fn px(win: &Window, x: i32, y: i32) -> (u8, u8, u8, u8) {
    let bs = win.backing_store();
    let dpr = bs.device_pixel_ratio().round() as i32;
    let p = bs
        .pixmap()
        .pixel((x * dpr) as u32, (y * dpr) as u32)
        .expect("pixel inside backing store");
    (p.red(), p.green(), p.blue(), p.alpha())
}

/// A 100x100 scroll area at the window origin whose 100x400 content paints solid green and counts
/// its paints. The content overflows vertically, so a vertical bar shows at x = 90..100.
fn scroll_window() -> (Box<Window>, WidgetRef, Rc<Cell<u32>>) {
    let win = Box::new(
        Window::new(
            "Scroll Area Paint",
            Rect::new(0, 0, 300, 200),
            WindowFlags::empty(),
        )
        .expect("create window"),
    );
    let paints = Rc::new(Cell::new(0));
    let counter = Rc::clone(&paints);
    let mut content = EmptyWidget::with_geometry(Rect::new(0, 0, 100, 400));
    content.set_paint_handler(move |painter| {
        counter.set(counter.get() + 1);
        painter.fill_rect(
            RectF {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 400.0,
            },
            Color::from_rgba8(GREEN.0, GREEN.1, GREEN.2, GREEN.3),
        );
    });
    let content: WidgetRef = Rc::new(RefCell::new(Box::new(content)));
    let mut area = ScrollArea::new();
    area.set_widget(Rc::clone(&content));
    let area: WidgetRef = Rc::new(RefCell::new(Box::new(area)));
    win.root_widget().borrow_mut().add_child(Rc::clone(&area));
    area.borrow().set_geometry(Rect::new(0, 0, 100, 100));
    (win, area, paints)
}

#[test]
fn the_content_is_painted_once_per_render() {
    let (mut win, _area, paints) = scroll_window();
    win.render_and_present();
    assert_eq!(paints.get(), 1, "content painted once for one render");
}

#[test]
fn the_vertical_scroll_bar_is_not_covered_by_the_content() {
    let (mut win, _area, _paints) = scroll_window();
    win.render_and_present();
    assert_eq!(px(&win, 50, 50), GREEN, "content shows inside the viewport");
    assert_ne!(
        px(&win, 95, 50),
        GREEN,
        "the bar column at x = 95 shows the scroll bar, not the content"
    );
}
