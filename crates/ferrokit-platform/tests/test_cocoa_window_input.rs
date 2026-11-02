// SPDX-License-Identifier: LGPL-3.0-only
//! Native input of `CocoaNativeWindow`: what AppKit's `QNSView` methods do with an `NSEvent`,
//! run against the mock runtime with mock events (`MockObjcRuntime::object_with`). The mock
//! models the content view as a flipped view at the window origin, so a position given in
//! Cocoa's bottom-left window coordinates must come out top-left, as Qt reports it. On macOS the
//! same methods are checked by the real view class (examples/appkit_main_thread.rs).
#![cfg(not(target_os = "macos"))]

use std::cell::RefCell;
use std::rc::Rc;

use ferrokit_core::event::MouseButtons;
use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_platform::objc_runtime::appkit::{MockValue, NS_NOT_FOUND, NSRange};
use ferrokit_platform::window_cocoa_input::{
    self as input, IME_LINE_HEIGHT, NsEventKind, cursor_update, qt_key_for_event,
    qt_modifiers_from_ns_flags, reset_cursor_rects, text_input_first_rect,
    text_input_has_marked_text, text_input_insert_str, text_input_marked_range,
    text_input_set_marked_str, text_input_unmark, update_tracking_area,
};
use ferrokit_platform::{
    CGPoint, ClosureWindowEventHandler, CocoaNativeWindow, CursorShape, Id, KeyboardModifiers,
    MockObjcRuntime, MouseButton, PlatformWindow, WindowFlags, WindowSystemEvent,
};

const SHIFT: i64 = 1 << 17;
const CONTROL: i64 = 1 << 18;
const OPTION: i64 = 1 << 19;
const COMMAND: i64 = 1 << 20;

type Log = Rc<RefCell<Vec<WindowSystemEvent>>>;

/// A 600x400 window at (100, 100) whose handler records every event.
fn window() -> (CocoaNativeWindow, Log) {
    ThreadContext::init_current(true, None);
    let mut win =
        CocoaNativeWindow::new("input", Rect::new(100, 100, 600, 400), WindowFlags::NORMAL)
            .expect("window");
    let log: Log = Rc::default();
    let sink = Rc::clone(&log);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        sink.borrow_mut().push(event)
    })));
    (win, log)
}

fn ns_event(properties: &[(&str, MockValue)]) -> Id {
    MockObjcRuntime::instance().object_with("NSEvent", properties)
}

/// [`input::handle_ns_event`] on the mock runtime.
fn handle_ns_event(view: Id, kind: NsEventKind, event: Id) -> bool {
    // SAFETY: this file only builds for the mock runtime (`cfg(not(target_os = "macos"))`),
    // which never dereferences an `Id`, so any `event` meets the contract.
    unsafe { input::handle_ns_event(view, kind, event) }
}

/// [`input::text_input_insert`] on the mock runtime.
fn text_input_insert(view: Id, text: Id) {
    // SAFETY: as in `handle_ns_event` above: the mock runtime never dereferences `text`.
    unsafe { input::text_input_insert(view, text) }
}

/// A mouse event at `(x, y)` in window coordinates (origin bottom left).
fn mouse_event(button: i64, x: f64, y: f64, flags: i64) -> Id {
    ns_event(&[
        ("locationInWindow", MockValue::Point(CGPoint::new(x, y))),
        ("buttonNumber", MockValue::Int(button)),
        ("modifierFlags", MockValue::Int(flags)),
    ])
}

/// The view position (50, 80) from the top-left corner: (50, 400 - 80) from the bottom left.
fn at_50_80(button: i64, flags: i64) -> Id {
    mouse_event(button, 50.0, 320.0, flags)
}

fn key_event(code: i64, characters: &str, flags: i64, repeat: bool) -> Id {
    ns_event(&[
        ("keyCode", MockValue::Int(code)),
        (
            "charactersIgnoringModifiers",
            MockValue::Str(characters.into()),
        ),
        ("modifierFlags", MockValue::Int(flags)),
        ("isARepeat", MockValue::Bool(repeat)),
    ])
}

fn only(log: &Log) -> WindowSystemEvent {
    let mut events = log.borrow_mut();
    assert_eq!(events.len(), 1, "{events:?}");
    events.pop().unwrap()
}

fn take(log: &Log) -> Vec<WindowSystemEvent> {
    std::mem::take(&mut *log.borrow_mut())
}

fn press_button(event: &WindowSystemEvent) -> MouseButton {
    match event {
        WindowSystemEvent::MousePress { button, .. } => *button,
        other => panic!("not a press: {other:?}"),
    }
}

fn mods(shift: bool, control: bool, alt: bool, meta: bool) -> KeyboardModifiers {
    KeyboardModifiers {
        shift,
        control,
        alt,
        meta,
    }
}

#[test]
fn mouse_press_and_release_report_top_left_logical_positions_and_every_button() {
    let (win, log) = window();
    let view = win.ns_view();
    // The method names the left and right buttons (`buttonNumber` is not consulted, so it is 0
    // here, as it can be on a synthetic event); `buttonNumber` names the other buttons.
    use NsEventKind::*;
    let buttons = [
        (MouseDown, MouseUp, 0, MouseButton::Left),
        (RightMouseDown, RightMouseUp, 0, MouseButton::Right),
        (OtherMouseDown, OtherMouseUp, 2, MouseButton::Middle),
        (OtherMouseDown, OtherMouseUp, 3, MouseButton::Other(3)),
        (OtherMouseDown, OtherMouseUp, 4, MouseButton::Other(4)),
    ];
    for (down, up, number, expected) in buttons {
        assert!(handle_ns_event(view, down, at_50_80(number, 0)));
        let press = only(&log);
        assert_eq!(press_button(&press), expected);
        let WindowSystemEvent::MousePress {
            pos,
            global_pos,
            modifiers,
            ..
        } = press
        else {
            unreachable!()
        };
        assert_eq!(
            pos,
            Point::new(50, 80),
            "y is flipped to the top-left origin"
        );
        assert_eq!(
            global_pos,
            Point::new(150, 180),
            "window origin (100, 100) plus the position"
        );
        assert_eq!(modifiers, KeyboardModifiers::default());

        assert!(handle_ns_event(view, up, at_50_80(number, 0)));
        assert_eq!(
            only(&log),
            WindowSystemEvent::MouseRelease {
                pos: Point::new(50, 80),
                global_pos: Point::new(150, 180),
                button: expected,
                modifiers: KeyboardModifiers::default(),
            }
        );
    }
}

#[test]
fn positions_past_the_top_left_edge_are_negative_not_clamped_to_zero() {
    let (win, log) = window();
    // 0.5 left of the view and 10 above it: `floor`, like Qt's QPoint of a QPointF.
    handle_ns_event(
        win.ns_view(),
        NsEventKind::MouseMoved,
        mouse_event(0, -0.5, 410.0, 0),
    );
    let WindowSystemEvent::MouseMove {
        pos, global_pos, ..
    } = only(&log)
    else {
        panic!("not a move");
    };
    assert_eq!(pos, Point::new(-1, -10));
    assert_eq!(global_pos, Point::new(99, 90));
}

#[test]
fn cocoa_modifiers_map_to_qt_modifiers_with_command_as_control() {
    assert_eq!(qt_modifiers_from_ns_flags(0), 0);
    assert_eq!(qt_modifiers_from_ns_flags(SHIFT as usize), 0x0200_0000);
    assert_eq!(qt_modifiers_from_ns_flags(COMMAND as usize), 0x0400_0000);
    assert_eq!(qt_modifiers_from_ns_flags(OPTION as usize), 0x0800_0000);
    assert_eq!(qt_modifiers_from_ns_flags(CONTROL as usize), 0x1000_0000);

    let (win, log) = window();
    let view = win.ns_view();
    for (flags, expected) in [
        (SHIFT, mods(true, false, false, false)),
        (COMMAND, mods(false, true, false, false)),
        (OPTION, mods(false, false, true, false)),
        (SHIFT | COMMAND | OPTION, mods(true, true, true, false)),
    ] {
        handle_ns_event(view, NsEventKind::MouseDown, at_50_80(0, flags));
        let WindowSystemEvent::MousePress {
            modifiers, button, ..
        } = only(&log)
        else {
            panic!("not a press");
        };
        assert_eq!(modifiers, expected, "flags {flags:#x}");
        assert_eq!(button, MouseButton::Left);
        handle_ns_event(view, NsEventKind::MouseUp, at_50_80(0, 0));
        take(&log);
    }
}

#[test]
fn control_click_is_a_right_click_and_its_release_matches() {
    let (win, log) = window();
    let view = win.ns_view();
    handle_ns_event(view, NsEventKind::MouseDown, at_50_80(0, CONTROL));
    let WindowSystemEvent::MousePress {
        button, modifiers, ..
    } = only(&log)
    else {
        panic!("not a press");
    };
    assert_eq!(button, MouseButton::Right);
    assert_eq!(
        modifiers,
        mods(false, false, false, true),
        "Control stays reported (as Meta)"
    );

    // Control was let go before the button: the release is still the right button's.
    handle_ns_event(view, NsEventKind::MouseUp, at_50_80(0, 0));
    let WindowSystemEvent::MouseRelease { button, .. } = only(&log) else {
        panic!("not a release");
    };
    assert_eq!(button, MouseButton::Right);

    // The next plain click is a left click again.
    handle_ns_event(view, NsEventKind::MouseDown, at_50_80(0, 0));
    assert_eq!(press_button(&only(&log)), MouseButton::Left);
}

#[test]
fn moves_report_the_buttons_held_and_enter_and_exit_become_move_and_leave() {
    let (win, log) = window();
    let view = win.ns_view();
    let buttons_of = |event: &WindowSystemEvent| match event {
        WindowSystemEvent::MouseMove { buttons, .. } => *buttons,
        other => panic!("not a move: {other:?}"),
    };

    handle_ns_event(view, NsEventKind::MouseEntered, at_50_80(0, 0));
    let entered = only(&log);
    assert!(buttons_of(&entered).is_empty());
    let WindowSystemEvent::MouseMove { pos, .. } = entered else {
        unreachable!()
    };
    assert_eq!(
        pos,
        Point::new(50, 80),
        "an enter is a move to where the pointer came in"
    );

    handle_ns_event(view, NsEventKind::MouseDown, at_50_80(0, 0));
    handle_ns_event(view, NsEventKind::RightMouseDown, at_50_80(1, 0));
    take(&log);
    handle_ns_event(view, NsEventKind::RightMouseDragged, at_50_80(1, 0));
    let held = buttons_of(&only(&log));
    assert!(held.contains(MouseButtons::LEFT) && held.contains(MouseButtons::RIGHT));

    handle_ns_event(view, NsEventKind::MouseUp, at_50_80(0, 0));
    take(&log);
    handle_ns_event(view, NsEventKind::MouseMoved, at_50_80(0, 0));
    let held = buttons_of(&only(&log));
    assert!(!held.contains(MouseButtons::LEFT) && held.contains(MouseButtons::RIGHT));

    handle_ns_event(view, NsEventKind::MouseExited, at_50_80(0, 0));
    assert_eq!(only(&log), WindowSystemEvent::MouseLeave);
}

#[test]
fn scroll_wheel_deltas_use_qt_angle_units() {
    let (win, log) = window();
    let view = win.ns_view();
    let wheel = |properties: &[(&str, MockValue)], flags: i64| {
        let mut all = vec![
            (
                "locationInWindow",
                MockValue::Point(CGPoint::new(50.0, 320.0)),
            ),
            ("modifierFlags", MockValue::Int(flags)),
        ];
        all.extend_from_slice(properties);
        ns_event(&all)
    };

    // A trackpad: pixels, a quarter degree each (2 units of 1/8 degree).
    handle_ns_event(
        view,
        NsEventKind::ScrollWheel,
        wheel(
            &[
                ("hasPreciseScrollingDeltas", MockValue::Bool(true)),
                ("scrollingDeltaX", MockValue::Float(3.0)),
                ("scrollingDeltaY", MockValue::Float(-10.0)),
            ],
            COMMAND,
        ),
    );
    let WindowSystemEvent::Wheel {
        pos,
        global_pos,
        delta,
        modifiers,
    } = only(&log)
    else {
        panic!("not a wheel event");
    };
    assert_eq!((delta.x, delta.y), (6, -20));
    assert_eq!(pos, Point::new(50, 80));
    assert_eq!(global_pos, Point::new(150, 180));
    assert_eq!(modifiers, mods(false, true, false, false));

    // A wheel: the accelerated delta is reduced to at most one notch (120) per event.
    for (delta_x, delta_y, expected) in [
        (0.0, 0.01, (0, 100)),
        (0.0, 3.0, (0, 120)),
        (-0.5, -3.0, (-120, -120)),
        (0.0, 0.0, (0, 0)),
    ] {
        handle_ns_event(
            view,
            NsEventKind::ScrollWheel,
            wheel(
                &[
                    ("deltaX", MockValue::Float(delta_x)),
                    ("deltaY", MockValue::Float(delta_y)),
                ],
                0,
            ),
        );
        let WindowSystemEvent::Wheel { delta, .. } = only(&log) else {
            panic!("not a wheel event");
        };
        assert_eq!(
            (delta.x, delta.y),
            expected,
            "deltas ({delta_x}, {delta_y})"
        );
    }
}

#[test]
fn key_codes_become_qt_keys() {
    // Printable keys follow the layout through `charactersIgnoringModifiers`, upper-cased.
    assert_eq!(qt_key_for_event(0x00, Some("a"), false), 'A' as u32);
    assert_eq!(qt_key_for_event(0x12, Some("1"), false), '1' as u32);
    assert_eq!(qt_key_for_event(0x31, Some(" "), false), 0x20);
    assert_eq!(qt_key_for_event(0x1B, Some("-"), false), '-' as u32);
    assert_eq!(qt_key_for_event(0x0E, Some("é"), false), 'É' as u32);
    assert_eq!(
        qt_key_for_event(0x00, Some("ß"), false),
        'ß' as u32,
        "no multi-scalar upper case"
    );
    // Keys without text are decided by their code.
    assert_eq!(qt_key_for_event(0x24, Some("\r"), false), 0x0100_0004);
    assert_eq!(qt_key_for_event(0x4C, Some("\u{3}"), false), 0x0100_0005);
    assert_eq!(qt_key_for_event(0x30, Some("\t"), false), 0x0100_0001);
    assert_eq!(
        qt_key_for_event(0x30, Some("\t"), true),
        0x0100_0002,
        "Shift+Tab is Backtab"
    );
    assert_eq!(qt_key_for_event(0x33, Some("\u{7f}"), false), 0x0100_0003);
    assert_eq!(qt_key_for_event(0x75, Some("\u{F728}"), false), 0x0100_0007);
    assert_eq!(qt_key_for_event(0x35, Some("\u{1b}"), false), 0x0100_0000);
    assert_eq!(qt_key_for_event(0x7B, Some("\u{F702}"), false), 0x0100_0012);
    assert_eq!(qt_key_for_event(0x7E, Some("\u{F700}"), false), 0x0100_0013);
    assert_eq!(qt_key_for_event(0x60, None, false), 0x0100_0030 + 4, "F5");
    assert_eq!(qt_key_for_event(0x5A, None, false), 0x0100_0030 + 19, "F20");
    // A key the layout leaves without a character is unknown.
    assert_eq!(qt_key_for_event(0x7F, None, false), 0x01ff_ffff);
    assert_eq!(qt_key_for_event(0x7F, Some("\u{F8FF}"), false), 0x01ff_ffff);
}

#[test]
fn key_down_goes_through_the_input_method_unless_it_is_a_shortcut() {
    let (win, log) = window();
    let view = win.ns_view();
    let mock = MockObjcRuntime::instance();
    let interpreted = |before: usize| {
        mock.calls_for(view)
            .iter()
            .filter(|c| c.selector == "interpretKeyEvents:")
            .count()
            - before
    };

    // A letter: the input method sees `[event]` first, then the window gets the key press.
    let a = key_event(0x00, "a", 0, false);
    assert!(handle_ns_event(view, NsEventKind::KeyDown, a));
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyPress {
            key: 'A' as u32,
            modifiers: KeyboardModifiers::default(),
            is_repeat: false,
        }
    );
    let call = mock
        .calls_for(view)
        .into_iter()
        .rev()
        .find(|c| c.selector == "interpretKeyEvents:")
        .expect("interpretKeyEvents: sent to the view");
    let MockValue::Id(array) = call.args[0] else {
        panic!("argument is an object");
    };
    assert_eq!(mock.get_object_data(array).unwrap().children, vec![a]);

    // Command+C is a shortcut, not text: no input method, the key press carries the chord.
    let before = mock
        .calls_for(view)
        .iter()
        .filter(|c| c.selector == "interpretKeyEvents:")
        .count();
    handle_ns_event(
        view,
        NsEventKind::KeyDown,
        key_event(0x08, "c", COMMAND, false),
    );
    assert_eq!(interpreted(before), 0);
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyPress {
            key: 'C' as u32,
            modifiers: mods(false, true, false, false),
            is_repeat: false,
        }
    );

    // Auto-repeat and release.
    handle_ns_event(
        view,
        NsEventKind::KeyDown,
        key_event(0x7B, "\u{F702}", SHIFT, true),
    );
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyPress {
            key: 0x0100_0012,
            modifiers: mods(true, false, false, false),
            is_repeat: true,
        }
    );
    handle_ns_event(
        view,
        NsEventKind::KeyUp,
        key_event(0x7B, "\u{F702}", SHIFT, false),
    );
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyRelease {
            key: 0x0100_0012,
            modifiers: mods(true, false, false, false),
        }
    );
}

#[test]
fn modifier_keys_report_press_and_release_from_flags_changed() {
    let (win, log) = window();
    let view = win.ns_view();
    let flags_changed = |code: i64, flags: i64| {
        ns_event(&[
            ("keyCode", MockValue::Int(code)),
            ("modifierFlags", MockValue::Int(flags)),
        ])
    };
    for (code, flag, key, held) in [
        (0x38, SHIFT, 0x0100_0020, mods(true, false, false, false)),
        (0x37, COMMAND, 0x0100_0021, mods(false, true, false, false)),
        (0x3A, OPTION, 0x0100_0023, mods(false, false, true, false)),
        (0x3B, CONTROL, 0x0100_0022, mods(false, false, false, true)),
    ] {
        assert!(handle_ns_event(
            view,
            NsEventKind::FlagsChanged,
            flags_changed(code, flag)
        ));
        assert_eq!(
            only(&log),
            WindowSystemEvent::KeyPress {
                key,
                modifiers: held,
                is_repeat: false
            }
        );
        assert!(handle_ns_event(
            view,
            NsEventKind::FlagsChanged,
            flags_changed(code, 0)
        ));
        assert_eq!(
            only(&log),
            WindowSystemEvent::KeyRelease {
                key,
                modifiers: KeyboardModifiers::default()
            }
        );
    }
    // The Fn key is not a Qt modifier.
    assert!(!handle_ns_event(
        view,
        NsEventKind::FlagsChanged,
        flags_changed(0x3F, 1 << 23)
    ));
    assert!(take(&log).is_empty());
}
#[test]
fn side_specific_modifiers_distinguish_left_and_right_releases() {
    let (win, log) = window();
    let view = win.ns_view();
    let flags_changed = |code: i64, flags: i64| {
        ns_event(&[
            ("keyCode", MockValue::Int(code)),
            ("modifierFlags", MockValue::Int(flags)),
        ])
    };

    // Press Left Shift (0x38) with general Shift (1 << 17) and NX_DEVICELSHIFTKEYMASK (0x02)
    assert!(handle_ns_event(
        view,
        NsEventKind::FlagsChanged,
        flags_changed(0x38, (1 << 17) | 0x02)
    ));
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyPress {
            key: 0x0100_0020,
            modifiers: mods(true, false, false, false),
            is_repeat: false,
        }
    );

    // Now press Right Shift (0x3C) too: both general Shift and both device masks (0x02 | 0x04)
    assert!(handle_ns_event(
        view,
        NsEventKind::FlagsChanged,
        flags_changed(0x3C, (1 << 17) | 0x02 | 0x04)
    ));
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyPress {
            key: 0x0100_0020,
            modifiers: mods(true, false, false, false),
            is_repeat: false,
        }
    );

    // Release Left Shift (0x38), while Right Shift remains held:
    // General Shift is STILL set (1 << 17), but NX_DEVICELSHIFTKEYMASK (0x02) is cleared,
    // leaving only NX_DEVICERSHIFTKEYMASK (0x04).
    // Before this fix, general Shift != 0 would cause Left Shift to be reported as KeyPress again!
    // With side mask checking, it is correctly reported as KeyRelease.
    assert!(handle_ns_event(
        view,
        NsEventKind::FlagsChanged,
        flags_changed(0x38, (1 << 17) | 0x04)
    ));
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyRelease {
            key: 0x0100_0020,
            modifiers: mods(true, false, false, false),
        }
    );

    // Finally release Right Shift (0x3C): general Shift and device mask are both 0.
    assert!(handle_ns_event(
        view,
        NsEventKind::FlagsChanged,
        flags_changed(0x3C, 0)
    ));
    assert_eq!(
        only(&log),
        WindowSystemEvent::KeyRelease {
            key: 0x0100_0020,
            modifiers: KeyboardModifiers::default(),
        }
    );
}

fn commit(text: &str) -> WindowSystemEvent {
    WindowSystemEvent::InputMethod {
        commit_string: text.into(),
        preedit_string: String::new(),
        cursor_position: 0,
    }
}

fn preedit(text: &str, cursor: i32) -> WindowSystemEvent {
    WindowSystemEvent::InputMethod {
        commit_string: String::new(),
        preedit_string: text.into(),
        cursor_position: cursor,
    }
}

#[test]
fn input_method_text_commits_what_a_key_press_cannot_carry() {
    let (win, log) = window();
    let view = win.ns_view();

    // Plain ASCII typed with no composition is already in the key press.
    for text in ["a", "Z", "5", " ", "!"] {
        text_input_insert_str(view, text);
    }
    assert!(take(&log).is_empty());

    // Other scripts, dead-key results and emoji are not.
    for text in ["é", "日", "😀", "ab"] {
        text_input_insert_str(view, text);
        assert_eq!(only(&log), commit(text));
    }

    // The argument can be an NSString (mock: its `string`).
    let ns_string = MockObjcRuntime::instance()
        .object_with("NSString", &[("string", MockValue::Str("ü".into()))]);
    text_input_insert(view, ns_string);
    assert_eq!(only(&log), commit("ü"));
    text_input_insert(view, Id::NIL);
    assert!(take(&log).is_empty());
}

#[test]
fn composition_is_marked_text_that_unmark_or_insert_commits() {
    let (win, log) = window();
    let view = win.ns_view();
    assert!(!text_input_has_marked_text(view));
    assert_eq!(text_input_marked_range(view), NSRange::NOT_FOUND);

    // Clearing a composition that does not exist says nothing.
    text_input_set_marked_str(view, "", NSRange::NOT_FOUND);
    assert!(take(&log).is_empty());

    text_input_set_marked_str(
        view,
        "に",
        NSRange {
            location: 1,
            length: 0,
        },
    );
    assert_eq!(only(&log), preedit("に", 1));
    assert!(text_input_has_marked_text(view));
    assert_eq!(
        text_input_marked_range(view),
        NSRange {
            location: 0,
            length: 1
        }
    );

    // No selection reported: the caret is at the end, counted in UTF-16 units.
    text_input_set_marked_str(view, "x😀", NSRange::NOT_FOUND);
    assert_eq!(only(&log), preedit("x😀", 3));
    assert_eq!(text_input_marked_range(view).length, 3);
    assert_eq!(NS_NOT_FOUND, NSRange::NOT_FOUND.location);

    // While composing the input method owns the keys: no key press reaches the window.
    handle_ns_event(view, NsEventKind::KeyDown, key_event(0x00, "a", 0, false));
    handle_ns_event(view, NsEventKind::KeyUp, key_event(0x00, "a", 0, false));
    assert!(take(&log).is_empty());

    // Committing ends the composition, plain ASCII included.
    text_input_insert_str(view, "a");
    assert_eq!(only(&log), commit("a"));
    assert!(!text_input_has_marked_text(view));

    // `unmarkText` accepts the composition as typed.
    text_input_set_marked_str(
        view,
        "かな",
        NSRange {
            location: 2,
            length: 0,
        },
    );
    take(&log);
    text_input_unmark(view);
    assert_eq!(only(&log), commit("かな"));
    text_input_unmark(view);
    assert!(take(&log).is_empty());

    // Cancelling the composition clears the preedit.
    text_input_set_marked_str(
        view,
        "x",
        NSRange {
            location: 1,
            length: 0,
        },
    );
    take(&log);
    text_input_set_marked_str(view, "", NSRange::NOT_FOUND);
    assert_eq!(only(&log), preedit("", 0));
    assert!(!text_input_has_marked_text(view));
}

#[test]
fn candidate_window_sits_at_the_bottom_edge_of_the_caret_line() {
    let (mut win, _log) = window();
    let view = win.ns_view();
    let line = IME_LINE_HEIGHT as f64;
    // The caret's top-left (0, 0) is (0, 400) from the window's bottom left; the line's bottom
    // edge is `IME_LINE_HEIGHT` lower, on a window whose frame starts at (100, 580) from the
    // screen's bottom left. AppKit puts the candidates below the rect's origin.
    let rect = text_input_first_rect(view);
    assert_eq!((rect.origin.x, rect.origin.y), (100.0, 980.0 - line));
    assert_eq!((rect.size.width, rect.size.height), (1.0, line));

    win.set_ime_focus(Point::new(30, 40));
    let rect = text_input_first_rect(view);
    assert_eq!((rect.origin.x, rect.origin.y), (130.0, 940.0 - line));
    assert_eq!((rect.size.width, rect.size.height), (1.0, line));
    // The typed line (from the caret's top, y = 940) is inside the rect, not below it.
    assert_eq!(rect.origin.y + rect.size.height, 940.0);
}

#[test]
fn a_key_release_follows_its_press_into_the_input_method() {
    let (win, log) = window();
    let view = win.ns_view();
    let down = |code, text: &str| {
        handle_ns_event(view, NsEventKind::KeyDown, key_event(code, text, 0, false))
    };
    let up = |code, text: &str| {
        handle_ns_event(view, NsEventKind::KeyUp, key_event(code, text, 0, false))
    };

    // A press the input method consumed (composition) and its release: neither is seen.
    text_input_set_marked_str(
        view,
        "に",
        NSRange {
            location: 1,
            length: 0,
        },
    );
    take(&log);
    down(0x00, "a");
    // The composition ends first (committed text), the release comes after.
    text_input_insert_str(view, "に");
    take(&log);
    up(0x00, "a");
    assert!(take(&log).is_empty(), "no KeyRelease without a KeyPress");

    // The suppression was one release; the next press and release are delivered.
    down(0x00, "a");
    up(0x00, "a");
    let events = take(&log);
    assert!(
        matches!(events[0], WindowSystemEvent::KeyPress { .. }),
        "{events:?}"
    );
    assert!(
        matches!(events[1], WindowSystemEvent::KeyRelease { .. }),
        "{events:?}"
    );
    assert_eq!(events.len(), 2);

    // Another key pressed before the composition and released during it is still released.
    down(0x0B, "b");
    text_input_set_marked_str(
        view,
        "x",
        NSRange {
            location: 1,
            length: 0,
        },
    );
    take(&log);
    up(0x0B, "b");
    assert!(matches!(only(&log), WindowSystemEvent::KeyRelease { .. }));
}

#[test]
fn tracking_area_is_installed_replaced_and_released() {
    let (win, _log) = window();
    let view = win.ns_view();
    let mock = MockObjcRuntime::instance();
    let areas = |view| -> Vec<Id> {
        mock.calls_for(view)
            .into_iter()
            .filter(|c| c.selector == "addTrackingArea:")
            .map(|c| match c.args[0] {
                MockValue::Id(id) => id,
                _ => panic!("object argument"),
            })
            .collect()
    };

    let first = *areas(view)
        .last()
        .expect("the window installs a tracking area");
    let init = mock
        .calls_for(first)
        .into_iter()
        .find(|c| c.selector == "initWithRect:options:owner:userInfo:")
        .expect("initWithRect:options:owner:userInfo:");
    // Entered/exited, moved, cursor update, active in the active app, in the visible rect.
    assert_eq!(
        init.args[1],
        MockValue::Int(0x01 | 0x02 | 0x04 | 0x40 | 0x200)
    );
    assert_eq!(init.args[2], MockValue::Id(view), "the view is the owner");
    assert_eq!(mock.get_object_data(first).unwrap().retain_count, 1);

    // `updateTrackingAreas` swaps it: the old one is removed from the view and freed.
    update_tracking_area(view);
    let second = *areas(view).last().unwrap();
    assert_ne!(first, second);
    assert!(
        mock.get_object_data(first).is_none(),
        "the replaced area leaked"
    );
    assert!(
        mock.calls_for(view)
            .iter()
            .any(|c| c.selector == "removeTrackingArea:" && c.args[0] == MockValue::Id(first))
    );

    drop(win);
    assert!(
        mock.get_object_data(second).is_none(),
        "the last area leaked"
    );
    assert!(!mock.was_over_released(first) && !mock.was_over_released(second));
}

#[test]
fn window_cursor_is_kept_by_the_view_and_shown_through_a_cursor_rect() {
    let (mut win, _log) = window();
    let view = win.ns_view();
    let ns_window = win.ns_window();
    let mock = MockObjcRuntime::instance();

    // No cursor of its own: AppKit's default handling runs.
    assert!(!reset_cursor_rects(view));
    assert!(!cursor_update(view));

    win.set_cursor(CursorShape::IBeam);
    let ibeam = mock
        .calls_for(ns_window)
        .into_iter()
        .rev()
        .find(|c| c.selector == "invalidateCursorRectsForView:")
        .expect("AppKit is told to rebuild the cursor rects");
    assert_eq!(ibeam.args[0], MockValue::Id(view));

    assert!(reset_cursor_rects(view));
    let calls = mock.calls_for(view);
    let rect_call = calls
        .iter()
        .rev()
        .find(|c| c.selector == "addCursorRect:cursor:")
        .expect("cursor rect");
    let MockValue::Rect(rect) = rect_call.args[0] else {
        panic!("rect argument");
    };
    assert_eq!(
        (rect.size.width, rect.size.height),
        (600.0, 400.0),
        "the whole visible rect"
    );
    let MockValue::Id(cursor) = rect_call.args[1] else {
        panic!("cursor argument");
    };
    assert_eq!(mock.get_object_data(cursor).unwrap().class_name, "NSCursor");
    let retained = mock.get_object_data(cursor).unwrap().retain_count;
    assert!(calls.iter().any(|c| c.selector == "discardCursorRects"));

    assert!(cursor_update(view));
    assert!(mock.calls_for(cursor).iter().any(|c| c.selector == "set"));

    // The same shape again is the same shared cursor, still held once by the view.
    win.set_cursor(CursorShape::IBeam);
    assert_eq!(mock.get_object_data(cursor).unwrap().retain_count, retained);

    // Another shape releases the previous cursor.
    win.set_cursor(CursorShape::PointingHand);
    assert_eq!(
        mock.get_object_data(cursor).unwrap().retain_count,
        retained - 1
    );

    // `unset_cursor` leaves the window without a cursor and the hand released.
    win.unset_cursor();
    assert!(!reset_cursor_rects(view));
    drop(win);
    assert!(!mock.was_over_released(cursor));
}

#[test]
fn events_raised_inside_the_handler_are_delivered_after_it_returns() {
    ThreadContext::init_current(true, None);
    let mut win = CocoaNativeWindow::new("nested", Rect::new(0, 0, 600, 400), WindowFlags::NORMAL)
        .expect("window");
    let view = win.ns_view();
    let trace: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = Rc::clone(&trace);
    let depth = Rc::new(RefCell::new(0));
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        *depth.borrow_mut() += 1;
        assert_eq!(*depth.borrow(), 1, "the handler is never re-entered");
        let name = match event {
            WindowSystemEvent::MouseMove { .. } => "move",
            WindowSystemEvent::MouseLeave => "leave",
            _ => "other",
        };
        sink.borrow_mut().push(format!("begin {name}"));
        if matches!(event, WindowSystemEvent::MouseMove { .. }) {
            // A handler that runs a nested event loop, creates a window, ...: the event waits.
            handle_ns_event(view, NsEventKind::MouseExited, ns_event(&[]));
        }
        sink.borrow_mut().push("end".to_string());
        *depth.borrow_mut() -= 1;
    })));
    handle_ns_event(view, NsEventKind::MouseMoved, mouse_event(0, 1.0, 399.0, 0));
    assert_eq!(*trace.borrow(), ["begin move", "end", "begin leave", "end"]);
}

#[test]
fn a_dropped_window_stops_receiving_and_frees_its_handler() {
    let (win, log) = window();
    let view = win.ns_view();
    assert_eq!(Rc::strong_count(&log), 2, "the handler holds the log");
    drop(win);
    assert_eq!(
        Rc::strong_count(&log),
        1,
        "the handler was dropped with the window"
    );
    assert!(!handle_ns_event(
        view,
        NsEventKind::MouseDown,
        at_50_80(0, 0)
    ));
    assert!(log.borrow().is_empty());
}

#[test]
fn events_without_a_handler_are_ignored() {
    ThreadContext::init_current(true, None);
    let win = CocoaNativeWindow::new("quiet", Rect::new(0, 0, 600, 400), WindowFlags::NORMAL)
        .expect("window");
    assert!(!handle_ns_event(
        win.ns_view(),
        NsEventKind::MouseDown,
        at_50_80(0, 0)
    ));
}

#[test]
fn native_input_is_main_thread_only() {
    let (win, _log) = window();
    let view = win.ns_view().0 as usize;
    let event = at_50_80(0, 0).0 as usize;
    let result = std::thread::spawn(move || {
        handle_ns_event(
            Id(view as *mut std::ffi::c_void),
            NsEventKind::MouseDown,
            Id(event as *mut std::ffi::c_void),
        )
    })
    .join();
    assert!(
        result.is_err(),
        "AppKit callbacks off the main thread are a bug"
    );
}
