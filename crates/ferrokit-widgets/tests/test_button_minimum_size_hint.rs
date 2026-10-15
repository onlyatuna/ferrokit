// SPDX-License-Identifier: LGPL-3.0-only
//! Button::minimum_size_hint matches size_hint.
//!
//! Qt reference: `QPushButton::minimumSizeHint` returns `sizeHint()` (`qpushbutton.cpp:398-401`).
//! PySide6: `QPushButton("Test").minimumSizeHint() == QPushButton("Test").sizeHint()`.

use ferrokit_widgets::button::Button;
use ferrokit_widgets::widget::Widget;

#[test]
fn button_minimum_size_hint_matches_size_hint() {
    let btn = Button::new("Test");
    let hint = btn.size_hint();
    let min_hint = btn.minimum_size_hint();
    assert_eq!(
        min_hint, hint,
        "Button::minimum_size_hint must equal size_hint (qpushbutton.cpp:398-401)"
    );
}
