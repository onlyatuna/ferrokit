// SPDX-License-Identifier: LGPL-3.0-only
//! The `PlatformWindow` methods of `CocoaNativeWindow` that talk to AppKit beyond a frame and a
//! title: window flags, mouse grab, mask, transient parent, partial presents, target position
//! and backdrop suspension. Run against the mock runtime, whose call log
//! (`MockObjcRuntime::calls_for`) shows the exact messages each one sent.
#![cfg(not(target_os = "macos"))]

use ferrokit_core::object::ThreadContext;
use ferrokit_gui::geometry::Region;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::paint::Pixmap;
use ferrokit_platform::backdrop::BackdropType;
use ferrokit_platform::objc_runtime::appkit::MockValue;
use ferrokit_platform::window_cocoa::cocoa_style_mask;
use ferrokit_platform::window_cocoa_input::{NsEventKind, handle_ns_event};
use ferrokit_platform::{
    CGPoint, CGRect, ClosureWindowEventHandler, CocoaNativeWindow, Id, MockObjcRuntime,
    NS_FLOATING_WINDOW_LEVEL, NS_MODAL_PANEL_WINDOW_LEVEL, NS_NORMAL_WINDOW_LEVEL,
    NS_SCREEN_SAVER_WINDOW_LEVEL, NS_WINDOW_STYLE_MASK_BORDERLESS, NS_WINDOW_STYLE_MASK_CLOSABLE,
    NS_WINDOW_STYLE_MASK_MINIATURIZABLE, NS_WINDOW_STYLE_MASK_RESIZABLE,
    NS_WINDOW_STYLE_MASK_TITLED, PlatformSurface, PlatformWindow, WindowFlags,
};

fn window(flags: WindowFlags) -> CocoaNativeWindow {
    ThreadContext::init_current(true, None);
    CocoaNativeWindow::new("methods", Rect::new(100, 100, 600, 400), flags).expect("window")
}

fn selectors(mock: &MockObjcRuntime, receiver: Id) -> Vec<String> {
    mock.calls_for(receiver)
        .into_iter()
        .map(|c| c.selector)
        .collect()
}

const TITLED: usize = NS_WINDOW_STYLE_MASK_TITLED
    | NS_WINDOW_STYLE_MASK_CLOSABLE
    | NS_WINDOW_STYLE_MASK_MINIATURIZABLE
    | NS_WINDOW_STYLE_MASK_RESIZABLE;

#[test]
fn window_flags_change_style_level_tool_behaviour_and_click_through() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();
    let data = || mock.get_object_data(ns_window).unwrap();
    assert_eq!(win.flags(), WindowFlags::NORMAL);
    assert_eq!(data().style_mask, TITLED);

    // Frameless: borderless style, nothing else changes.
    mock.clear_calls();
    win.set_window_flag(WindowFlags::FRAMELESS, true);
    assert!(win.flags().contains(WindowFlags::FRAMELESS));
    assert_eq!(data().style_mask, NS_WINDOW_STYLE_MASK_BORDERLESS);
    assert_eq!(selectors(mock, ns_window), ["setStyleMask:"]);
    win.set_window_flag(WindowFlags::FRAMELESS, false);
    assert_eq!(data().style_mask, TITLED);

    // A custom frame draws its own title bar but still resizes.
    win.set_window_flag(WindowFlags::FRAMELESS | WindowFlags::CUSTOM_FRAMELESS, true);
    assert_eq!(
        data().style_mask,
        NS_WINDOW_STYLE_MASK_BORDERLESS | NS_WINDOW_STYLE_MASK_RESIZABLE
    );
    win.set_window_flag(
        WindowFlags::FRAMELESS | WindowFlags::CUSTOM_FRAMELESS,
        false,
    );

    // Stacking: Tool floats, StaysOnTop is above it, ToolTip above both (Qt windowLevel).
    win.set_window_flag(WindowFlags::TOOL, true);
    assert_eq!(data().level, NS_FLOATING_WINDOW_LEVEL);
    win.set_window_flag(WindowFlags::STAYS_ON_TOP, true);
    assert_eq!(data().level, NS_MODAL_PANEL_WINDOW_LEVEL);
    win.set_window_flag(WindowFlags::TOOLTIP, true);
    assert_eq!(data().level, NS_SCREEN_SAVER_WINDOW_LEVEL);
    assert_eq!(
        data().style_mask,
        NS_WINDOW_STYLE_MASK_BORDERLESS,
        "a tooltip has no frame"
    );
    win.set_window_flag(WindowFlags::TOOLTIP, false);
    assert_eq!(data().level, NS_MODAL_PANEL_WINDOW_LEVEL);
    win.set_window_flag(WindowFlags::STAYS_ON_TOP, false);
    assert_eq!(data().level, NS_FLOATING_WINDOW_LEVEL);
    win.set_window_flag(WindowFlags::TOOL, false);
    assert_eq!(data().level, NS_NORMAL_WINDOW_LEVEL);

    // `Qt::Tool` hides the window while the application is inactive.
    let hides = |on: bool| {
        mock.calls_for(ns_window)
            .into_iter()
            .filter(|c| c.selector == "setHidesOnDeactivate:")
            .map(|c| c.args[0] == MockValue::Bool(on))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        hides(true),
        [true, false],
        "on with Tool, off when Tool went away"
    );

    win.set_window_flag(WindowFlags::CLICK_THROUGH, true);
    assert!(data().ignores_mouse_events);
    win.set_window_flag(WindowFlags::CLICK_THROUGH, false);
    assert!(!data().ignores_mouse_events);

    // Flags with no AppKit counterpart are remembered; setting what is already set does nothing.
    win.set_window_flag(WindowFlags::LAYERED, true);
    assert!(win.flags().contains(WindowFlags::LAYERED));
    mock.clear_calls();
    win.set_window_flag(WindowFlags::LAYERED, true);
    win.set_window_flag(WindowFlags::TOOL, false);
    assert!(mock.calls_for(ns_window).is_empty());

    // `set_stays_on_top` and the flag setter agree.
    win.set_stays_on_top(true);
    assert!(win.flags().contains(WindowFlags::STAYS_ON_TOP));
    win.set_window_flag(WindowFlags::STAYS_ON_TOP, false);
    assert_eq!(data().level, NS_NORMAL_WINDOW_LEVEL);
}

#[test]
fn window_created_with_flags_gets_the_same_style_and_tool_behaviour() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::FRAMELESS | WindowFlags::TOOL);
    let data = mock.get_object_data(win.ns_window()).unwrap();
    assert_eq!(data.style_mask, cocoa_style_mask(win.flags()));
    assert_eq!(data.style_mask, NS_WINDOW_STYLE_MASK_BORDERLESS);
    assert_eq!(data.level, NS_FLOATING_WINDOW_LEVEL);
    assert!(
        mock.calls_for(win.ns_window())
            .iter()
            .any(|c| c.selector == "setHidesOnDeactivate:" && c.args[0] == MockValue::Bool(true))
    );
}

#[test]
fn the_content_view_becomes_first_responder_and_the_window_accepts_moved_events() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::NORMAL);
    let calls = mock.calls_for(win.ns_window());
    assert!(
        calls
            .iter()
            .any(|c| c.selector == "makeFirstResponder:"
                && c.args[0] == MockValue::Id(win.ns_view()))
    );
    assert!(
        calls
            .iter()
            .any(|c| c.selector == "setAcceptsMouseMovedEvents:"
                && c.args[0] == MockValue::Bool(true))
    );
}

#[test]
fn explicit_mouse_grab_needs_a_visible_window_and_a_pressed_button_is_an_implicit_one() {
    let mut win = window(WindowFlags::NORMAL);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(|_| {})));
    assert!(!win.has_mouse_grab());
    assert!(
        !win.set_mouse_grab_enabled(true),
        "a hidden window cannot grab"
    );
    assert!(!win.has_mouse_grab());

    win.show();
    assert!(win.set_mouse_grab_enabled(true));
    assert!(win.has_mouse_grab());
    assert!(
        !win.set_mouse_grab_enabled(false),
        "releasing reports no grab"
    );
    assert!(!win.has_mouse_grab());

    // AppKit routes a drag to the view that took the press: that is a grab until the release.
    let press = |button: i64| {
        MockObjcRuntime::instance().object_with(
            "NSEvent",
            &[
                (
                    "locationInWindow",
                    MockValue::Point(CGPoint::new(5.0, 395.0)),
                ),
                ("buttonNumber", MockValue::Int(button)),
                ("modifierFlags", MockValue::Int(0)),
            ],
        )
    };
    // SAFETY: this file only builds for the mock runtime (`cfg(not(target_os = "macos"))`), which
    // never dereferences an `Id`; the events are mock mouse events besides.
    unsafe { handle_ns_event(win.ns_view(), NsEventKind::MouseDown, press(0)) };
    assert!(win.has_mouse_grab());
    // SAFETY: as for the press above.
    unsafe { handle_ns_event(win.ns_view(), NsEventKind::MouseUp, press(0)) };
    assert!(!win.has_mouse_grab());
}

fn region_calls(mock: &MockObjcRuntime, layer: Id) -> (Id, Vec<MockValue>) {
    let set_mask = mock
        .calls_for(layer)
        .into_iter()
        .rev()
        .find(|c| c.selector == "setMask:")
        .expect("setMask:");
    let MockValue::Id(shape) = set_mask.args[0] else {
        panic!("object argument");
    };
    let path = mock
        .calls_for(shape)
        .into_iter()
        .find(|c| c.selector == "setPath:")
        .map(|c| c.args)
        .unwrap_or_default();
    (shape, path)
}

#[test]
fn mask_clips_the_content_layer_to_the_region_and_makes_the_window_see_through() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::FRAMELESS);
    let ns_window = win.ns_window();
    // The mock's `layer` is the view itself.
    let layer = win.ns_view();
    mock.set_property(ns_window, "isOpaque", MockValue::Bool(true));

    let region = Region::from_rects(&[Rect::new(10, 20, 100, 50), Rect::new(200, 0, 30, 30)]);
    win.set_mask(&region);
    let (shape, path) = region_calls(mock, layer);
    assert_eq!(
        mock.get_object_data(shape).map(|d| d.class_name),
        None,
        "the +1 was released"
    );
    // The layer is not geometry-flipped: y counts from the bottom of the 400 high layer. A
    // `Region` is stored as bands of rectangles; the mask has every one of them.
    assert!(region.rect_count() >= 2);
    let expected: Vec<MockValue> = region
        .rects()
        .iter()
        .map(|r| {
            MockValue::Rect(CGRect::new(
                r.x as f64,
                (400 - (r.y + r.height)) as f64,
                r.width as f64,
                r.height as f64,
            ))
        })
        .collect();
    assert_eq!(path, expected);
    assert!(path.contains(&MockValue::Rect(CGRect::new(10.0, 370.0, 100.0, 10.0))));
    let window_calls = mock.calls_for(ns_window);
    assert!(
        window_calls
            .iter()
            .any(|c| c.selector == "setOpaque:" && c.args[0] == MockValue::Bool(false))
    );
    assert!(
        window_calls
            .iter()
            .any(|c| c.selector == "setBackgroundColor:")
    );
    assert!(
        window_calls
            .iter()
            .any(|c| c.selector == "invalidateShadow")
    );

    // A geometry-flipped layer takes top-left coordinates as they are.
    mock.set_property(layer, "isGeometryFlipped", MockValue::Bool(true));
    win.set_mask(&Region::from_rect(Rect::new(1, 2, 3, 4)));
    let (_, path) = region_calls(mock, layer);
    assert_eq!(path, [MockValue::Rect(CGRect::new(1.0, 2.0, 3.0, 4.0))]);

    // An empty region removes the mask and gives the opaque window back.
    mock.clear_calls();
    win.set_mask(&Region::from_rects(&[]));
    let removal = mock
        .calls_for(layer)
        .into_iter()
        .find(|c| c.selector == "setMask:")
        .expect("setMask:");
    assert_eq!(removal.args[0], MockValue::Id(Id::NIL));
    let window_calls = mock.calls_for(ns_window);
    assert!(
        window_calls
            .iter()
            .any(|c| c.selector == "setOpaque:" && c.args[0] == MockValue::Bool(true))
    );
}

#[test]
fn removing_a_mask_leaves_a_window_that_was_transparent_anyway_alone() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::FRAMELESS);
    let ns_window = win.ns_window();
    // Already non-opaque (`isOpaque` unset in the mock is NO): the window is not touched.
    win.set_mask(&Region::from_rect(Rect::new(0, 0, 10, 10)));
    win.set_mask(&Region::from_rects(&[]));
    assert!(!selectors(mock, ns_window).contains(&"setOpaque:".to_string()));
}

#[test]
fn transient_parent_attaches_the_window_as_a_child_window() {
    let mock = MockObjcRuntime::instance();
    let mut parent = window(WindowFlags::NORMAL);
    let other = window(WindowFlags::NORMAL);
    let mut child = window(WindowFlags::TOOL);
    let (p, o, c) = (parent.ns_window(), other.ns_window(), child.ns_window());
    assert_eq!(child.transient_parent(), None);

    child.set_transient_parent(Some(parent.native_handle()));
    assert_eq!(child.transient_parent(), Some(parent.native_handle()));
    let add = mock
        .calls_for(p)
        .into_iter()
        .find(|c| c.selector == "addChildWindow:ordered:")
        .expect("addChildWindow:ordered:");
    assert_eq!(
        add.args,
        [MockValue::Id(c), MockValue::Int(1)],
        "NSWindowAbove"
    );

    // Setting the same parent again sends nothing; another parent replaces the first.
    mock.clear_calls();
    child.set_transient_parent(Some(parent.native_handle()));
    assert!(mock.calls().is_empty());
    child.set_transient_parent(Some(other.native_handle()));
    assert_eq!(child.transient_parent(), Some(other.native_handle()));
    assert_eq!(selectors(mock, p), ["removeChildWindow:"]);
    assert_eq!(selectors(mock, o), ["addChildWindow:ordered:"]);

    // A window is not its own parent, and a null handle detaches.
    child.set_transient_parent(Some(child.native_handle()));
    assert_eq!(child.transient_parent(), None);
    child.set_transient_parent(Some(other.native_handle()));
    child.set_transient_parent(Some(0));
    assert_eq!(child.transient_parent(), None);

    // A child that is dropped while attached detaches first; its parent is left intact.
    child.set_transient_parent(Some(parent.native_handle()));
    mock.clear_calls();
    drop(child);
    assert_eq!(selectors(mock, p), ["removeChildWindow:"]);
    assert!(mock.get_object_data(c).is_none());
    assert!(mock.get_object_data(p).is_some());
    assert!(!mock.was_over_released(p));
    parent.set_transient_parent(None);
}

#[test]
fn transient_parent_ignores_foreign_handles_and_ends_with_the_parent() {
    let mock = MockObjcRuntime::instance();
    let parent = window(WindowFlags::NORMAL);
    let mut child = window(WindowFlags::TOOL);
    // A handle that is not a live `CocoaNativeWindow` is never sent a message: it is ignored,
    // which neither attaches nor detaches the window.
    let foreign = mock.allocate_object("NSWindow");
    child.set_transient_parent(Some(foreign.0 as isize));
    assert_eq!(child.transient_parent(), None);
    assert!(selectors(mock, foreign).is_empty());
    child.set_transient_parent(Some(parent.native_handle()));
    child.set_transient_parent(Some(foreign.0 as isize));
    assert_eq!(child.transient_parent(), Some(parent.native_handle()));
    assert!(selectors(mock, foreign).is_empty());

    // A parent that is dropped detaches its children first, so the handle never outlives it,
    // and is refused like any other handle from then on.
    let parent_handle = parent.native_handle();
    drop(parent);
    assert_eq!(child.transient_parent(), None);
    child.set_transient_parent(Some(parent_handle));
    assert_eq!(child.transient_parent(), None);
}

#[test]
fn a_transient_parent_that_would_make_a_cycle_is_refused() {
    let mock = MockObjcRuntime::instance();
    let mut a = window(WindowFlags::NORMAL);
    let mut b = window(WindowFlags::TOOL);
    let mut c = window(WindowFlags::TOOL);
    b.set_transient_parent(Some(a.native_handle()));
    c.set_transient_parent(Some(b.native_handle()));

    // `a` above its own grandchild and `b` above its child would be cycles: no window is
    // attached to or detached from anything.
    mock.clear_calls();
    a.set_transient_parent(Some(c.native_handle()));
    b.set_transient_parent(Some(c.native_handle()));
    for w in [a.ns_window(), b.ns_window(), c.ns_window()] {
        assert!(
            !selectors(mock, w)
                .iter()
                .any(|s| s == "addChildWindow:ordered:" || s == "removeChildWindow:")
        );
    }
    assert_eq!(a.transient_parent(), None);
    assert_eq!(b.transient_parent(), Some(a.native_handle()));

    // A window outside the chain is still accepted.
    let d = window(WindowFlags::NORMAL);
    a.set_transient_parent(Some(d.native_handle()));
    assert_eq!(a.transient_parent(), Some(d.native_handle()));
}

fn solid(width: u32, height: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height).expect("pixmap");
    pixmap.fill(ferrokit_gui::tiny_skia::Color::from_rgba8(
        200, 100, 50, 255,
    ));
    pixmap
}

fn lit(win: &CocoaNativeWindow, x: usize, y: usize) -> bool {
    let surface = win.surface();
    let offset = (y * surface.width() as usize + x) * 4;
    surface.pixel_buffer()[offset..offset + 4] != [0, 0, 0, 0]
}

#[test]
fn partial_presents_copy_only_the_dirty_pixels() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let layer = win.ns_view();
    let mut pixmap = solid(600, 400);
    assert_eq!(win.surface().width(), 600);

    // `present_dirty`: only the rectangle reaches the frame.
    win.present_dirty(&mut pixmap, 1.0, Rect::new(10, 10, 20, 5))
        .expect("present_dirty");
    assert!(lit(&win, 10, 10) && lit(&win, 29, 14));
    assert!(!lit(&win, 9, 10) && !lit(&win, 30, 10) && !lit(&win, 10, 15) && !lit(&win, 300, 300));
    assert_ne!(
        mock.get_object_data(layer).unwrap().contents,
        Id::NIL,
        "the layer got its contents"
    );

    // `present_region`: every rectangle of the region, and the pixmap is only read.
    let region = Region::from_rects(&[Rect::new(100, 100, 4, 4), Rect::new(300, 200, 2, 2)]);
    win.present_region(&pixmap, &region)
        .expect("present_region");
    assert!(lit(&win, 100, 100) && lit(&win, 103, 103) && lit(&win, 301, 201));
    assert!(!lit(&win, 104, 100) && !lit(&win, 299, 200) && !lit(&win, 302, 201));

    // A region outside the frame shows nothing and does not fail.
    win.present_region(&pixmap, &Region::from_rect(Rect::new(5000, 5000, 10, 10)))
        .expect("outside");

    // A pixmap of another size replaces the whole frame and resizes the surface.
    let mut bigger = solid(640, 480);
    win.present_dirty(&mut bigger, 1.0, Rect::new(0, 0, 1, 1))
        .expect("resized present_dirty");
    assert_eq!((win.surface().width(), win.surface().height()), (640, 480));
    assert!(lit(&win, 639, 479) && lit(&win, 320, 240));
    let smaller = solid(320, 200);
    win.present_region(&smaller, &Region::from_rect(Rect::new(0, 0, 1, 1)))
        .expect("resized present_region");
    assert_eq!((win.surface().width(), win.surface().height()), (320, 200));
    assert!(lit(&win, 319, 199));
}

#[test]
fn present_region_moves_a_layered_window_to_its_target_position_once() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::FRAMELESS | WindowFlags::LAYERED);
    let ns_window = win.ns_window();
    let pixmap = solid(150, 90);
    let region = Region::from_rect(Rect::new(0, 0, 150, 90));
    let frame = || mock.get_object_data(ns_window).unwrap().frame;

    // Without a target the frame is left alone.
    win.present_region(&pixmap, &region).unwrap();
    assert_eq!(win.geometry(), Rect::new(100, 100, 600, 400));

    // With one, the window takes the position and the logical size of the picture, in native
    // (bottom-left) coordinates: y = 1080 - (30 + 90).
    win.set_target_pos(Some(Point::new(40, 30)));
    win.present_region(&pixmap, &region).unwrap();
    assert_eq!(win.geometry(), Rect::new(40, 30, 150, 90));
    assert_eq!(
        (
            frame().origin.x,
            frame().origin.y,
            frame().size.width,
            frame().size.height
        ),
        (40.0, 960.0, 150.0, 90.0)
    );

    // The position is consumed by one present.
    mock.clear_calls();
    win.present_region(&pixmap, &region).unwrap();
    assert_eq!(win.geometry(), Rect::new(40, 30, 150, 90));
    win.set_target_pos(Some(Point::new(1, 2)));
    win.set_target_pos(None);
    win.present_region(&pixmap, &region).unwrap();
    assert_eq!(win.geometry(), Rect::new(40, 30, 150, 90));
}

#[test]
fn suspending_acrylic_makes_the_effect_view_inactive_and_resuming_makes_it_active_again() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    // The effect view is added as a subview of the window's content view.
    let effect_view = |win: &CocoaNativeWindow| {
        *mock
            .get_object_data(win.ns_view())
            .unwrap()
            .children
            .last()
            .unwrap()
    };
    let state_of = |win: &CocoaNativeWindow| mock.get_object_data(effect_view(win)).unwrap().state;

    // Nothing requested: nothing to suspend or resume.
    assert!(!win.suspend_backdrop());
    assert!(!win.resume_backdrop());

    assert!(win.set_backdrop(BackdropType::Acrylic, true));
    assert_eq!(state_of(&win), 1, "NSVisualEffectStateActive");
    assert!(win.suspend_backdrop());
    assert_eq!(
        state_of(&win),
        2,
        "inactive: a flat fill of the material, no blur"
    );
    assert!(!win.suspend_backdrop(), "already suspended");
    assert_eq!(
        mock.get_object_data(win.ns_view()).unwrap().children.len(),
        1,
        "effect view is child of content view"
    );

    // Asking for Acrylic again while suspended keeps it suspended.
    assert!(win.set_backdrop(BackdropType::Acrylic, false));
    assert_eq!(state_of(&win), 2);

    assert!(win.resume_backdrop());
    assert_eq!(state_of(&win), 1);
    assert!(!win.resume_backdrop(), "already resumed");

    // Other backdrops are never suspended.
    assert!(win.set_backdrop(BackdropType::Mica, true));
    assert!(!win.suspend_backdrop());
    assert_eq!(state_of(&win), 1);
}

#[test]
fn raise_orders_window_front_without_activating_or_making_key() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    // Invisible window: raise is a no-op (AppKit orderFront: would show it).
    mock.clear_calls();
    win.raise();
    assert!(!win.is_active());
    assert!(
        !selectors(mock, ns_window).contains(&"orderFront:".to_string()),
        "invisible window is not raised"
    );

    // Visible window: ordered front without taking key.
    win.show();
    mock.update_object(ns_window, |d| d.is_key = false);
    mock.clear_calls();
    win.raise();
    assert!(
        selectors(mock, ns_window).contains(&"orderFront:".to_string()),
        "raise orders the visible window front in the local stacking order"
    );
    assert!(
        !selectors(mock, ns_window).contains(&"makeKeyAndOrderFront:".to_string()),
        "raise must not activate or make key"
    );
    assert!(!win.is_active(), "raise leaves the window non-key");

    // When miniaturized or hidden natively (isVisible == false), raise is a no-op.
    mock.update_object(ns_window, |d| d.is_visible = false);
    mock.clear_calls();
    win.raise();
    assert!(
        !selectors(mock, ns_window).contains(&"orderFront:".to_string()),
        "window with native isVisible == false is not raised"
    );
}
#[test]
fn request_activate_window_activates_app_and_orders_front_unless_hidden_or_tooltip() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    // Invisible window: request_activate_window is a no-op.
    mock.clear_calls();
    win.request_activate_window();
    assert!(!win.is_active());
    assert!(
        !selectors(mock, ns_window).contains(&"makeKeyWindow".to_string()),
        "invisible window cannot be activated"
    );

    // Tooltip window: never takes activation even when shown.
    let tooltip = window(WindowFlags::TOOLTIP);
    tooltip.show();
    mock.clear_calls();
    tooltip.request_activate_window();
    assert!(!tooltip.is_active());
    assert!(
        !selectors(mock, tooltip.ns_window()).contains(&"makeKeyWindow".to_string()),
        "tooltip never takes activation"
    );
    // Click-through window: never takes activation even when shown.
    let click_through = window(WindowFlags::CLICK_THROUGH);
    click_through.show();
    mock.clear_calls();
    click_through.request_activate_window();
    assert!(!click_through.is_active());
    assert!(
        !selectors(mock, click_through.ns_window()).contains(&"makeKeyWindow".to_string()),
        "click-through window never takes activation"
    );
    // Visible normal window: activates the application and makes key.
    win.show();
    mock.clear_calls();
    win.request_activate_window();
    assert!(win.is_active());
    let selectors_after = selectors(mock, ns_window);
    assert!(
        selectors_after.contains(&"makeKeyWindow".to_string()),
        "makes the window key"
    );
    assert!(
        selectors_after.contains(&"makeFirstResponder:".to_string()),
        "makes the view the first responder"
    );
    // SAFETY: mock runtime only: sharedApplication takes no arguments, matching send_class_0.
    let nsapp = unsafe {
        ferrokit_platform::ObjcMsg::send_class_0(
            ferrokit_platform::Class::get("NSApplication").unwrap(),
            ferrokit_platform::Sel::register("sharedApplication"),
        )
    };
    assert!(
        selectors(mock, nsapp).contains(&"activateIgnoringOtherApps:".to_string()),
        "activates the shared NSApplication"
    );
}

#[test]
fn device_pixel_ratio_follows_window_backing_scale_factor_with_screen_fallback() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    // Default window scale factor in mock is 1.0.
    assert_eq!(win.device_pixel_ratio(), 1.0);

    // Window on a 2.0x Retina display.
    mock.update_object(ns_window, |d| d.scale_factor = 2.0);
    assert_eq!(win.device_pixel_ratio(), 2.0);

    // Zero or invalid scale factor falls back to primary screen (2.0 in mock).
    mock.update_object(ns_window, |d| d.scale_factor = 0.0);
    assert_eq!(
        win.device_pixel_ratio(),
        ferrokit_platform::platform()
            .primary_screen()
            .device_pixel_ratio()
    );
}

#[test]
fn flat_tint_backdrop_applies_inactive_effect_view() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let effect_view = |win: &CocoaNativeWindow| {
        *mock
            .get_object_data(win.ns_view())
            .unwrap()
            .children
            .last()
            .unwrap()
    };

    assert!(win.set_backdrop(BackdropType::FlatTint, true));
    let data = mock.get_object_data(effect_view(&win)).unwrap();
    assert_eq!(
        data.state, 2,
        "NSVisualEffectStateInactive (flat tint without blur)"
    );
    assert_eq!(data.material, 6, "Popover material");
    let effect = effect_view(&win);
    let effect_calls: Vec<_> = mock
        .calls_for(effect)
        .into_iter()
        .filter(|c| c.selector == "setAutoresizingMask:")
        .collect();
    assert_eq!(
        effect_calls.len(),
        1,
        "setAutoresizingMask: called on effect view"
    );
    assert_eq!(
        effect_calls[0].args,
        vec![MockValue::Int(0x12)],
        "NSViewWidthSizable | NSViewHeightSizable"
    );

    let z_calls: Vec<_> = mock
        .calls_for(effect)
        .into_iter()
        .filter(|c| c.selector == "setZPosition:")
        .collect();
    assert_eq!(z_calls.len(), 1);
    assert_eq!(
        z_calls[0].args,
        vec![MockValue::Float(f64::from(-f32::MAX))]
    );

    // Window is made non-opaque with clearColor for behind-window effect.
    let opaque_calls: Vec<_> = mock
        .calls_for(win.ns_window())
        .into_iter()
        .filter(|c| c.selector == "setOpaque:")
        .collect();
    assert_eq!(opaque_calls.len(), 1);
    assert_eq!(opaque_calls[0].args, vec![MockValue::Bool(false)]);
    let bg_calls: Vec<_> = mock
        .calls_for(win.ns_window())
        .into_iter()
        .filter(|c| c.selector == "setBackgroundColor:")
        .collect();
    assert_eq!(bg_calls.len(), 1);
    let MockValue::Id(clear_id) = bg_calls[0].args[0] else {
        panic!("expected Id argument for setBackgroundColor:");
    };
    assert_eq!(mock.get_object_data(clear_id).unwrap().title, "clearColor");

    // When reset to None, opacity and windowBackgroundColor are restored.
    assert!(win.set_backdrop(BackdropType::None, false));
    let opaque_calls: Vec<_> = mock
        .calls_for(win.ns_window())
        .into_iter()
        .filter(|c| c.selector == "setOpaque:")
        .collect();
    assert_eq!(opaque_calls.len(), 2);
    assert_eq!(opaque_calls[1].args, vec![MockValue::Bool(true)]);
    let bg_calls: Vec<_> = mock
        .calls_for(win.ns_window())
        .into_iter()
        .filter(|c| c.selector == "setBackgroundColor:")
        .collect();
    assert_eq!(bg_calls.len(), 2);
    let MockValue::Id(win_bg_id) = bg_calls[1].args[0] else {
        panic!("expected Id argument for setBackgroundColor:");
    };
    assert_eq!(
        mock.get_object_data(win_bg_id).unwrap().title,
        "windowBackgroundColor"
    );
}

#[test]
fn window_delegate_notifications_deliver_events_and_are_cleaned_up_on_drop() {
    use ferrokit_platform::WindowSystemEvent;
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();
    let ns_view = win.ns_view();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = std::rc::Rc::clone(&events);
    win.set_event_handler(Box::new(ClosureWindowEventHandler::new(move |event| {
        sink.borrow_mut().push(event);
    })));

    // Verify 6 notification observers were registered on NSNotificationCenter for this window.
    // SAFETY: defaultCenter is a no-argument class method returning an object.
    let center = unsafe {
        ferrokit_platform::ObjcMsg::send_class_0(
            ferrokit_platform::Class::get("NSNotificationCenter").unwrap(),
            ferrokit_platform::Sel::register("defaultCenter"),
        )
    };
    let center_calls: Vec<_> = mock
        .calls_for(center)
        .into_iter()
        .filter(|c| c.selector == "addObserver:selector:name:object:")
        .filter(|c| c.args.last() == Some(&MockValue::Id(ns_window)))
        .collect();
    assert_eq!(
        center_calls.len(),
        6,
        "all 6 delegate notifications registered"
    );

    // 1. FocusIn
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidBecomeKeyNotification",
    );
    assert_eq!(events.borrow_mut().pop(), Some(WindowSystemEvent::FocusIn));

    // 2. FocusOut
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidResignKeyNotification",
    );
    assert_eq!(events.borrow_mut().pop(), Some(WindowSystemEvent::FocusOut));

    // 3. CloseRequest via synchronous windowShouldClose:
    assert!(
        !ferrokit_platform::window_cocoa_input::window_should_close(ns_window),
        "windowShouldClose: returns false when handled so AppKit does not close behind our back"
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::CloseRequest)
    );

    // 4. Resize: verifies that NSWindowDidResizeNotification delivers both Resize AND GeometryChange
    // using the content rect (frame height 528 - 28pt titlebar = 500 content height).
    // Primary screen height is 1080: y = 1080 - 380 - 500 = 200.
    mock.update_object(ns_window, |d| {
        d.frame = CGRect::new(100.0, 380.0, 800.0, 528.0);
    });
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidResizeNotification",
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::Resize {
            size: ferrokit_gui::geometry::primitives::Size::new(800, 500)
        })
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::GeometryChange {
            geometry: Rect::new(100, 200, 800, 500)
        })
    );
    // PlatformWindow::geometry() dynamically reflects live content rect
    assert_eq!(
        PlatformWindow::geometry(&win),
        Rect::new(100, 200, 800, 500)
    );

    // 5. GeometryChange (flipped from Cocoa bottom-left to Qt top-left, testing QRectF::toRect rounding)
    // Secondary monitor with negative x: (-50.4).round() as i32 = -50; (-50.6).round() as i32 = -51.
    mock.update_object(ns_window, |d| {
        d.frame = CGRect::new(-50.6, 380.0, 800.0, 528.0);
    });
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidMoveNotification",
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::GeometryChange {
            geometry: Rect::new(-51, 200, 800, 500)
        })
    );
    assert_eq!(
        PlatformWindow::geometry(&win),
        Rect::new(-51, 200, 800, 500)
    );

    // 6. Screen change notification delivers DPI and geometry updates
    mock.update_object(ns_window, |d| {
        d.frame = CGRect::new(200.0, 380.0, 800.0, 528.0);
        d.scale_factor = 2.0;
    });
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidChangeScreenNotification",
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::DpiChanged {
            dpi_x: 192,
            dpi_y: 192
        })
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::GeometryChange {
            geometry: Rect::new(200, 200, 800, 500)
        })
    );
    // 7. DpiChanged
    mock.update_object(ns_window, |d| d.scale_factor = 2.0);
    ferrokit_platform::window_cocoa_input::dispatch_notification(
        ns_view,
        ns_window,
        "NSWindowDidChangeBackingPropertiesNotification",
    );
    assert_eq!(
        events.borrow_mut().pop(),
        Some(WindowSystemEvent::DpiChanged {
            dpi_x: 192,
            dpi_y: 192
        })
    );

    // When dropped, all 6 observers are removed from the notification center.
    mock.clear_calls();
    drop(win);
    let remove_calls: Vec<_> = mock
        .calls_for(center)
        .into_iter()
        .filter(|c| c.selector == "removeObserver:")
        .collect();
    assert_eq!(remove_calls.len(), 6, "all 6 observers removed on drop");
}
#[test]
fn request_activate_window_checks_native_is_visible() {
    let mock = MockObjcRuntime::instance();
    let win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    win.show();
    mock.update_object(ns_window, |d| d.is_visible = false);
    mock.clear_calls();

    win.request_activate_window();
    assert!(
        !selectors(mock, ns_window).contains(&"makeKeyWindow".to_string()),
        "window with native isVisible == false must not be activated"
    );
}

#[test]
fn send_f64_return_on_nil_receiver_returns_zero() {
    // SAFETY: testing nil receiver handling for send_f64_return.
    let result = unsafe {
        ferrokit_platform::objc_runtime::ObjcMsg::send_f64_return(
            ferrokit_platform::objc_runtime::Id::NIL,
            ferrokit_platform::objc_runtime::Sel::register("backingScaleFactor"),
        )
    };
    assert_eq!(result, 0.0);
}

#[test]
fn window_flipped_geometry_rounds_like_qrectf_to_rect_and_strips_titlebar() {
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    // Titled window has a 28pt titlebar in mock.
    // Setting content geometry (100, 200, 300, 200) sets frame height to 228 and origin.y = 1080 - 200 - 200 = 680.
    win.set_geometry(Rect::new(100, 200, 300, 200));
    let frame = mock.get_object_data(ns_window).unwrap().frame;
    assert_eq!(frame, CGRect::new(100.0, 680.0, 300.0, 228.0));
    assert_eq!(win.geometry(), Rect::new(100, 200, 300, 200));

    // Fractional coordinates: origin.x = -50.4 -> rounds to -50.
    // Frame height 228 -> content height 200; origin.y = 100.6 -> top_left_y = 1080.0 - 100.6 - 200.0 = 779.4 -> rounds to 779.
    mock.update_object(ns_window, |d| {
        d.frame = CGRect::new(-50.4, 100.6, 300.0, 228.0);
    });
    assert_eq!(win.geometry(), Rect::new(-50, 779, 300, 200));
}

#[test]
fn backdrop_none_preserves_transparency_when_vibrancy_or_mask_is_active() {
    use ferrokit_platform::window_cocoa::CocoaVibrancyMaterial;
    let mock = MockObjcRuntime::instance();
    let mut win = window(WindowFlags::NORMAL);
    let ns_window = win.ns_window();

    // Calling BackdropType::None when no backdrop was ever active does not touch setOpaque:.
    mock.clear_calls();
    assert!(win.set_backdrop(BackdropType::None, false));
    assert!(
        !selectors(mock, ns_window).contains(&"setOpaque:".to_string()),
        "BackdropType::None without prior backdrop must not clobber window opacity"
    );

    // Enable vibrancy AND backdrop; removing backdrop must keep window non-opaque because vibrancy is active.
    win.set_vibrancy(Some(CocoaVibrancyMaterial::HudWindow));
    assert!(win.set_backdrop(BackdropType::Acrylic, true));
    mock.clear_calls();
    assert!(win.set_backdrop(BackdropType::None, false));
    assert!(
        !mock
            .calls_for(ns_window)
            .iter()
            .any(|c| c.selector == "setOpaque:" && c.args == vec![MockValue::Bool(true)]),
        "removing backdrop while vibrancy is active must not setOpaque: true"
    );

    // Once vibrancy is also removed, window opacity is restored to true.
    mock.clear_calls();
    win.set_vibrancy(None);
    assert!(
        mock.calls_for(ns_window)
            .iter()
            .any(|c| c.selector == "setOpaque:" && c.args == vec![MockValue::Bool(true)]),
        "removing last transparency source restores setOpaque: true"
    );
}
