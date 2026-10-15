// SPDX-License-Identifier: LGPL-3.0-only
//! `MenuStyle` follows `QStyleSheetStyle`'s menu box model (measured with PySide6 6.11.2 on the
//! a card-style sheet: rows 27 px, separators 9 px, first row at y = 5, a menu with a
//! checkable item 20 px wider than the same one without).

use ferrokit_gui::text::Font;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_widgets::menu::MenuStyle;
use ferrokit_widgets::{Action, Menu, Widget};

fn cards_style() -> MenuStyle {
    let c = Color::from_rgba8(22, 25, 32, 255);
    MenuStyle {
        font: Font::new("Segoe UI", 11.0),
        row_font: Font::new("Segoe UI", 12.0),
        background: c,
        border: c,
        text: c,
        disabled_text: c,
        hover_background: c,
        hover_text: c,
        separator: c,
        radius: 6.0,
        padding_v: 4,
        item_padding: [6, 24, 6, 20],
        separator_margin: [4, 8],
    }
}

fn menu_with(build: impl FnOnce(&mut Menu)) -> Menu {
    let mut menu = Menu::new("");
    menu.set_style(cards_style());
    build(&mut menu);
    menu
}

#[test]
fn height_is_frame_padding_rows_and_separators() {
    let one = menu_with(|m| {
        m.add_action(Action::new_ref("Refresh"));
    })
    .size_hint();
    let with_separator = menu_with(|m| {
        m.add_action(Action::new_ref("Refresh"));
        m.add_separator();
    })
    .size_hint();
    let two = menu_with(|m| {
        m.add_action(Action::new_ref("Refresh"));
        m.add_action(Action::new_ref("Exit"));
    })
    .size_hint();

    let row = two.height - one.height;
    // The row is the menu font's height plus the item's vertical padding.
    assert!(row > 12, "row {row} must include 6 + 6 padding");
    // 1 px frame + 4 px padding on each side, one row.
    assert_eq!(one.height, 2 * (1 + 4) + row);
    // 1 px line + 4 px margin above and below.
    assert_eq!(with_separator.height - one.height, 9);
}

#[test]
fn check_column_widens_the_menu_only_when_an_item_is_checkable() {
    let plain = menu_with(|m| {
        m.add_action(Action::new_ref("Always on Top"));
    })
    .size_hint();
    let checkable = menu_with(|m| {
        let a = Action::new_ref("Always on Top");
        a.borrow_mut().set_checkable(true);
        m.add_action(a);
    })
    .size_hint();
    // The base style's check indicator plus 4 px, measured with PySide6 6.11.2 per style.
    use ferrokit_platform::NativeStyle;
    let expected = match ferrokit_platform::platform().theme().native_style() {
        NativeStyle::Windows11 => 20,
        NativeStyle::WindowsVista => 17,
        NativeStyle::Fusion => 18,
        NativeStyle::Macintosh => 23,
    };
    assert_eq!(checkable.width - plain.width, expected);
}

#[test]
fn width_adds_frame_and_left_right_padding_to_the_widest_text() {
    let short = menu_with(|m| {
        m.add_action(Action::new_ref("MMMM"));
    })
    .size_hint();
    let long = menu_with(|m| {
        m.add_action(Action::new_ref("MMMM"));
        m.add_action(Action::new_ref("MMMMMMMM"));
    })
    .size_hint();
    assert!(long.width > short.width);
    // Width of "MMMM" in the menu font plus 2 (frame) + 20 + 24 (padding): nothing else.
    let text = ferrokit_gui::text::FontMetrics::from_font(&Font::new("Segoe UI", 12.0))
        .horizontal_advance_exact("MMMM", &Font::new("Segoe UI", 12.0))
        .ceil() as i32;
    assert_eq!(short.width, 2 + 20 + 24 + text);
}
