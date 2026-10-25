// SPDX-License-Identifier: LGPL-3.0-only
//! The static functions of `QCursor`: the pointer position.
//!
//! A widget's cursor shape is `Widget::set_cursor`; the application-wide shape is
//! `Application::set_override_cursor`.

use ferrokit_gui::geometry::primitives::Point;

pub use ferrokit_platform::cursor::CursorShape;

/// `QCursor`'s static members. A shape is a [`CursorShape`]; there is no bitmap cursor.
pub struct Cursor;

impl Cursor {
    /// `QCursor::pos()` (qcursor.cpp:157-191): the pointer position in global (logical) desktop
    /// coordinates, queried from the window system (`GetCursorPos` on Windows) and mapped
    /// through the screen it is on. A backend that cannot query it reports the position of the
    /// last mouse event (`QGuiApplicationPrivate::lastCursorPosition`), so mouse events a test
    /// feeds in are reflected; Qt documents the same fallback.
    pub fn pos() -> Point {
        match ferrokit_platform::platform().cursor().pos() {
            Some(native) => ferrokit_platform::screen::native_cursor_to_logical(native),
            None => ferrokit_platform::window::last_cursor_position(),
        }
    }

    /// `QCursor::setPos` (qcursor.cpp:214-243): moves the pointer to a global (logical) desktop
    /// position (`SetCursorPos` on Windows). The window system answers with mouse events. A
    /// backend that cannot move the pointer does nothing, as Qt documents for platforms
    /// without cursors; a position the pointer is at already is not set again (qcursor.cpp:219-225).
    pub fn set_pos(pos: Point) {
        let native = ferrokit_platform::screen::logical_point_to_native(pos);
        let mut cursor = ferrokit_platform::platform().cursor();
        if cursor.pos() != Some(native) {
            cursor.set_pos(native);
        }
    }
}
