// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_platform::{Menu, WindowFlags};
use ferrokit_widgets::{BoxLayout, EmptyWidget, Layout, StackedWidget, Widget, WidgetRef, Window};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `Menu::popup` needs a shared menu (`aboutToShow` runs with it free); these tests drive an
/// owned one, so hand it over for the popup and take it back.
fn popped(
    menu: ferrokit_widgets::Menu,
    pos: ferrokit_gui::geometry::primitives::Point,
) -> ferrokit_widgets::Menu {
    let shared = std::rc::Rc::new(std::cell::RefCell::new(menu));
    ferrokit_widgets::Menu::popup(&shared, pos);
    match std::rc::Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        Err(_) => unreachable!("the popup keeps no handle to its menu"),
    }
}

#[test]
fn test_stacked_widget_page_switching() {
    let stack: WidgetRef = Rc::new(RefCell::new(Box::new(StackedWidget::new())));
    let page0: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let page1: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let read = |f: &dyn Fn(&StackedWidget) -> usize| {
        f(stack
            .borrow()
            .as_any()
            .downcast_ref::<StackedWidget>()
            .unwrap())
    };

    let idx0 = StackedWidget::add_widget(&stack, page0.clone());
    let idx1 = StackedWidget::add_widget(&stack, page1.clone());
    assert_eq!(idx0, 0);
    assert_eq!(idx1, 1);
    assert_eq!(read(&|s| s.count()), 2);
    assert_eq!(read(&|s| s.current_index()), 0);

    let changed = Arc::new(AtomicUsize::new(999));
    let changed_clone = Arc::clone(&changed);
    stack
        .borrow()
        .as_any()
        .downcast_ref::<StackedWidget>()
        .unwrap()
        .current_changed
        .connect(move |idx: &i32| {
            changed_clone.store(*idx as usize, Ordering::SeqCst);
        });

    StackedWidget::set_current_index(&stack, 1);
    assert_eq!(read(&|s| s.current_index()), 1);
    assert_eq!(changed.load(Ordering::SeqCst), 1);
}

#[test]
fn test_box_layout_add_stretch() {
    let mut layout = BoxLayout::horizontal();
    let item1: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let item2: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));

    layout.add_widget(item1.clone());
    layout.add_stretch(1);
    layout.add_widget(item2.clone());

    layout.set_geometry(Rect::new(0, 0, 300, 50));
    assert_eq!(item1.borrow().geometry().x, 0);
    assert!(item2.borrow().geometry().x > 100);
}

#[test]
fn test_tray_menu_submenus() {
    let mut root_menu = Menu::new();
    root_menu.add_action(1, "Open");

    let mut style_submenu = Menu::new();
    style_submenu.add_checkable(10, "Cards", true);
    style_submenu.add_checkable(11, "Table", false);

    root_menu.add_submenu("UI Style", style_submenu);
    root_menu.add_separator();
    root_menu.add_action(99, "Exit");

    assert_eq!(root_menu.len(), 4);
    assert!(root_menu.items[1].submenu.is_some());
    assert_eq!(root_menu.items[1].submenu.as_ref().unwrap().len(), 2);
}

#[test]
fn test_window_opacity_and_minimum_size() {
    let mut win = Window::new(
        "Opacity Test Window",
        Rect::new(100, 100, 200, 200),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("Window creation failed");

    win.set_opacity(0.85);
    assert!((win.opacity() - 0.85).abs() < 1e-4);

    win.set_minimum_size(150, 120);
    assert_eq!(win.minimum_size(), (150, 120));
}

#[test]
fn test_menu_high_dpi_hit_testing() {
    use ferrokit_gui::geometry::primitives::Point;
    use ferrokit_platform::high_dpi::from_native_point;
    use ferrokit_widgets::action::Action;
    use ferrokit_widgets::menu::Menu;

    let mut menu = Menu::new("TestMenu");
    let act_refresh = Action::new_ref("Refresh All");
    let act_cards = Action::new_ref("Cards");
    let act_table = Action::new_ref("Table");
    let act_exit = Action::new_ref("Exit");

    menu.add_action(act_refresh.clone());
    menu.add_action(act_cards.clone());
    menu.add_action(act_table.clone());
    menu.add_separator();
    menu.add_action(act_exit.clone());

    let menu = popped(menu, Point::new(0, 0));

    let exit_geo = menu
        .action_geometry(&act_exit)
        .expect("Exit action geometry exists");
    let target_logical_pt = Point::new(exit_geo.center().x, exit_geo.center().y);

    // Test across typical Windows DPI scaling factors: 1.0 (100%), 1.25 (125%), 1.5 (150%), 2.0 (200%)
    for dpr in [1.0f32, 1.25, 1.5, 2.0] {
        let physical_x = (target_logical_pt.x as f32 * dpr).round() as i32;
        let physical_y = (target_logical_pt.y as f32 * dpr).round() as i32;
        let physical_pt = Point::new(physical_x, physical_y);

        // Convert physical point using high_dpi helper
        let resolved_logical = from_native_point(physical_pt, dpr);

        let hit_action = menu.action_at(resolved_logical);
        assert!(hit_action.is_some(), "Must hit an action at DPR {dpr}");
        assert_eq!(
            hit_action.unwrap().borrow().text(),
            "Exit",
            "Must hit Exit action at DPR {dpr}"
        );
    }
}
#[test]
fn test_window_present_custom_physical_backing_store_parity() {
    let mut win = Window::new(
        "Present Custom Test",
        Rect::new(50, 50, 200, 150),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("Window creation failed");

    let mut painted = false;
    win.present_custom(|_painter| {
        painted = true;
    });
    assert!(painted, "present_custom closure must be invoked");
    assert!(win.backing_store().physical_width() >= 200);
    assert!(win.backing_store().physical_height() >= 150);
}
#[test]
fn test_submenu_hover_and_signature_propagation() {
    use ferrokit_gui::geometry::primitives::Point;
    use ferrokit_widgets::action::Action;
    use ferrokit_widgets::menu::Menu;

    let mut root = Menu::new("RootMenu");
    let sub_ref = root.add_menu("Layout");
    let act_triple = Action::new_ref("Horizontal Triple");
    let act_stack = Action::new_ref("Vertical Stack");
    sub_ref.borrow_mut().add_action(act_triple.clone());
    sub_ref.borrow_mut().add_action(act_stack.clone());

    let act_layout = root.actions()[0].clone();
    let mut root = popped(root, Point::new(0, 0));

    // Move to root item 0 (Layout) -> opens submenu
    let layout_geo = root
        .action_geometry(&act_layout)
        .expect("Layout action geometry");
    root.handle_mouse_move_at(layout_geo.center());

    assert!(root.active_action().is_some(), "Root item should be active");
    assert!(root.open_submenu().is_some(), "Submenu should be open");

    let sub_rc = root.open_submenu().unwrap();
    assert!(
        sub_rc.borrow().active_action().is_none(),
        "Initially submenu has no active item"
    );

    let sig_before = root.hover_signature();

    // Calculate coordinates inside the submenu item
    let sub_origin = sub_rc.borrow().geometry();
    let triple_local = sub_rc
        .borrow()
        .action_geometry(&act_triple)
        .expect("Triple action geo");
    let root_coords_for_triple = Point::new(
        sub_origin.x + triple_local.center().x,
        sub_origin.y + triple_local.center().y,
    );

    // Move mouse into submenu item 0
    root.handle_mouse_move_at(root_coords_for_triple);

    assert_eq!(
        sub_rc
            .borrow()
            .active_action()
            .map(|a| a.borrow().text().to_string()),
        Some("Horizontal Triple".to_string()),
        "Submenu item 0 should be active after hover"
    );

    let sig_after = root.hover_signature();
    assert_ne!(
        sig_before, sig_after,
        "Hover signature must change to trigger window repaint"
    );

    // Move mouse back to root item -> clears submenu active item
    root.handle_mouse_move_at(layout_geo.center());
    assert!(
        sub_rc.borrow().active_action().is_none(),
        "Submenu active item must be cleared when mouse returns to parent menu"
    );
    assert_ne!(
        sig_after,
        root.hover_signature(),
        "Hover signature must change when returning to parent menu"
    );
}

/// Root menu shaped like a context menu around "Opacity": a submenu item taller than the
/// plain items beneath it ("Interval", "Autostart"), which lie inside the submenu's vertical span.
fn opacity_menu() -> (
    ferrokit_widgets::menu::Menu,
    ferrokit_widgets::action::ActionRef,
    ferrokit_widgets::action::ActionRef,
) {
    use ferrokit_gui::geometry::primitives::Point;
    use ferrokit_widgets::action::Action;
    use ferrokit_widgets::menu::Menu;

    let mut root = Menu::new("Root");
    let opacity = root.add_menu("Opacity");
    for pct in ["100%", "90%", "80%", "70%", "50%", "30%"] {
        opacity.borrow_mut().add_action(Action::new_ref(pct));
    }
    let interval = Action::new_ref("Interval");
    let autostart = Action::new_ref("Autostart");
    root.add_action(interval.clone());
    root.add_action(autostart.clone());
    let root = popped(root, Point::new(0, 0));
    (root, interval, autostart)
}

fn active_text(menu: &ferrokit_widgets::menu::Menu) -> Option<String> {
    menu.active_action().map(|a| a.borrow().text().to_string())
}

#[test]
fn test_hover_moving_straight_down_past_open_submenu_selects_rows_below() {
    let (mut root, interval, autostart) = opacity_menu();
    let opacity = root.actions()[0].clone();
    let opacity_c = root.action_geometry(&opacity).unwrap().center();
    root.handle_mouse_move_at(opacity_c);
    assert!(
        root.open_submenu().is_some(),
        "hovering Opacity opens its submenu"
    );

    // Rows below "Opacity" are inside the submenu's vertical extent; the pointer is not heading
    // for the submenu, so they must take the hover.
    let interval_c = root.action_geometry(&interval).unwrap().center();
    root.handle_mouse_move_at(ferrokit_gui::geometry::primitives::Point::new(
        opacity_c.x,
        interval_c.y,
    ));
    assert_eq!(active_text(&root).as_deref(), Some("Interval"));
    assert!(
        root.open_submenu().is_none(),
        "moving to a plain item closes the submenu"
    );

    let autostart_c = root.action_geometry(&autostart).unwrap().center();
    root.handle_mouse_move_at(ferrokit_gui::geometry::primitives::Point::new(
        opacity_c.x,
        autostart_c.y,
    ));
    assert_eq!(active_text(&root).as_deref(), Some("Autostart"));
}

#[test]
fn test_hover_diagonal_towards_open_submenu_keeps_item_until_it_is_reached() {
    use ferrokit_gui::geometry::primitives::Point;
    let (mut root, _interval, _autostart) = opacity_menu();
    let opacity = root.actions()[0].clone();
    let start = root.action_geometry(&opacity).unwrap().center();
    root.handle_mouse_move_at(start);
    let sub = root.open_submenu().expect("submenu open");
    let sg = sub.borrow().geometry();
    let last = sub.borrow().actions().last().cloned().unwrap();
    let lg = sub.borrow().action_geometry(&last).unwrap();
    let target = Point::new(sg.x + lg.center().x, sg.y + lg.center().y);

    // Straight line from the item to the submenu's last row, crossing the rows below "Opacity".
    let n = 24;
    for step in 1..=n {
        let p = Point::new(
            start.x + (target.x - start.x) * step / n,
            start.y + (target.y - start.y) * step / n,
        );
        root.handle_mouse_move_at(p);
        if p.x < sg.x {
            assert_eq!(
                active_text(&root).as_deref(),
                Some("Opacity"),
                "diagonal travel at {p:?} must not hop to the rows it crosses"
            );
            assert!(
                root.open_submenu().is_some(),
                "submenu must stay open at {p:?}"
            );
        }
    }
    assert_eq!(
        sub.borrow()
            .active_action()
            .map(|a| a.borrow().text().to_string())
            .as_deref(),
        Some("30%"),
        "pointer ends on the submenu's last row"
    );
}
#[test]
fn test_window_system_resize_event_and_backing_store_update() {
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::object::QObject;
    use ferrokit_gui::geometry::primitives::{Rect, Size};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};

    let mut win = Window::new(
        "Resize Test",
        Rect::new(0, 0, 400, 300),
        WindowFlags::empty(),
    )
    .expect("create window");

    assert_eq!(win.geometry().width, 400);
    assert_eq!(win.geometry().height, 300);
    let initial_store_w = win.backing_store().physical_width();
    let initial_store_h = win.backing_store().physical_height();

    let resized_w = Arc::new(AtomicI32::new(0));
    let resized_h = Arc::new(AtomicI32::new(0));
    let rw = Arc::clone(&resized_w);
    let rh = Arc::clone(&resized_h);

    win.set_resize_handler(move |size: Size| {
        rw.store(size.width, Ordering::SeqCst);
        rh.store(size.height, Ordering::SeqCst);
    });

    // Simulate system resize event (e.g. from WM_SIZE)
    let mut resize_event = Event::new_spontaneous(EventKind::Resize {
        width: 600,
        height: 500,
        old_width: 400,
        old_height: 300,
    });

    assert!(
        win.event(&mut resize_event),
        "Window must handle EventKind::Resize"
    );

    // Verify window geometry is updated
    assert_eq!(win.geometry().width, 600);
    assert_eq!(win.geometry().height, 500);

    // Verify resize callback was triggered
    assert_eq!(resized_w.load(Ordering::SeqCst), 600);
    assert_eq!(resized_h.load(Ordering::SeqCst), 500);

    // Verify backing store was resized to the new dimensions
    assert!(win.backing_store().physical_width() > initial_store_w);
    assert!(win.backing_store().physical_height() > initial_store_h);
}
#[test]
fn test_window_system_event_handler_resize() {
    use ferrokit_gui::geometry::primitives::Size;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};

    let mut win = Window::new(
        "Handler Resize Test",
        Rect::new(0, 0, 300, 200),
        WindowFlags::empty(),
    )
    .expect("create window");

    let initial_store_w = win.backing_store().physical_width();
    let initial_store_h = win.backing_store().physical_height();

    let resized_w = Arc::new(AtomicI32::new(0));
    let resized_h = Arc::new(AtomicI32::new(0));
    let rw = Arc::clone(&resized_w);
    let rh = Arc::clone(&resized_h);

    win.set_resize_handler(move |size: Size| {
        rw.store(size.width, Ordering::SeqCst);
        rh.store(size.height, Ordering::SeqCst);
    });

    // Programmatic set_geometry triggers internal resize without deadlock
    win.set_geometry(Rect::new(0, 0, 550, 420));
    assert_eq!(win.geometry().width, 550);
    assert_eq!(win.geometry().height, 420);
    assert!(win.backing_store().physical_width() > initial_store_w);
    assert!(win.backing_store().physical_height() > initial_store_h);
}

#[cfg(windows)]
#[test]
fn test_geometry_change_event_delivery() {
    use ferrokit_gui::geometry::primitives::Size;
    use ferrokit_platform::handle_geometry_change;
    use ferrokit_platform::window_system_interface::Delivery;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};

    let mut win = Window::new(
        "Geometry Change Test",
        Rect::new(0, 0, 320, 240),
        WindowFlags::empty(),
    )
    .expect("create window");

    let initial_store_w = win.backing_store().physical_width();
    let initial_store_h = win.backing_store().physical_height();

    let resized_w = Arc::new(AtomicI32::new(0));
    let resized_h = Arc::new(AtomicI32::new(0));
    let rw = Arc::clone(&resized_w);
    let rh = Arc::clone(&resized_h);

    win.set_resize_handler(move |size: Size| {
        rw.store(size.width, Ordering::SeqCst);
        rh.store(size.height, Ordering::SeqCst);
    });

    let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;
    handle_geometry_change(Delivery::Default, hwnd, Rect::new(10, 20, 480, 360));

    assert_eq!(win.geometry().x, 10);
    assert_eq!(win.geometry().y, 20);
    assert_eq!(win.geometry().width, 480);
    assert_eq!(win.geometry().height, 360);
    assert_eq!(resized_w.load(Ordering::SeqCst), 480);
    assert_eq!(resized_h.load(Ordering::SeqCst), 360);
    assert!(win.backing_store().physical_width() > initial_store_w);
    assert!(win.backing_store().physical_height() > initial_store_h);
}

#[cfg(windows)]
#[test]
fn test_lazy_backing_store_resize_observable_ordering() {
    use ferrokit_gui::geometry::primitives::{Rect, Size};
    use ferrokit_platform::handle_geometry_change;
    use ferrokit_platform::window_system_interface::Delivery;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let mut win = Window::new(
        "Lazy Resize Ordering Test",
        Rect::new(0, 0, 300, 200),
        WindowFlags::empty(),
    )
    .expect("create window");

    let initial_store_w = win.backing_store().physical_width();
    let initial_store_h = win.backing_store().physical_height();

    let bs_arc = win.backing_store_handle();
    let store_w_during_callback = Arc::new(AtomicU32::new(0));
    let store_h_during_callback = Arc::new(AtomicU32::new(0));
    let sw_cb = Arc::clone(&store_w_during_callback);
    let sh_cb = Arc::clone(&store_h_during_callback);

    win.set_resize_handler(move |_size: Size| {
        // Under Qt observable ordering & Lazy Resize:
        // When resize callback runs, the backing store has NOT been reallocated yet!
        // Callbacks observe and adjust state before the buffer is allocated.
        let bs = bs_arc.borrow();
        sw_cb.store(bs.physical_width(), Ordering::SeqCst);
        sh_cb.store(bs.physical_height(), Ordering::SeqCst);
    });

    let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;
    // Dispatch geometry change (600x450)
    handle_geometry_change(Delivery::Default, hwnd, Rect::new(0, 0, 600, 450));

    // 1. In callback: backing store was STILL at initial size (300x200 scaled)!
    assert_eq!(
        store_w_during_callback.load(Ordering::SeqCst),
        initial_store_w,
        "Backing store must NOT be reallocated before resize callback runs!"
    );
    assert_eq!(
        store_h_during_callback.load(Ordering::SeqCst),
        initial_store_h,
        "Backing store must NOT be reallocated before resize callback runs!"
    );

    // 2. After event handling / paintAndFlush: backing store was lazily resized to the new size!
    assert!(
        win.backing_store().physical_width() > initial_store_w,
        "Backing store must be resized after paintAndFlush completes"
    );
    assert!(
        win.backing_store().physical_height() > initial_store_h,
        "Backing store must be resized after paintAndFlush completes"
    );
}

#[cfg(windows)]
#[test]
fn test_resize_event_observable_ordering_before_layout_activation() {
    use ferrokit_gui::geometry::primitives::{Rect, Size};
    use ferrokit_platform::handle_geometry_change;
    use ferrokit_platform::window_system_interface::Delivery;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, Ordering};

    let mut win = Window::new(
        "Layout Lifecycle Test",
        Rect::new(0, 0, 300, 200),
        WindowFlags::empty(),
    )
    .expect("create window");

    let counter = Arc::new(AtomicI32::new(0));
    let child_resize_order = Arc::new(AtomicI32::new(-1));
    let cb_order = Arc::new(AtomicI32::new(-1));

    let child = EmptyWidget::new();
    let counter_child = Arc::clone(&counter);
    let child_order_cb = Arc::clone(&child_resize_order);
    child.set_resize_handler(move |_new_size: Size, _old_size: Size| {
        let order = counter_child.fetch_add(1, Ordering::SeqCst);
        child_order_cb.store(order, Ordering::SeqCst);
    });

    let child_ref: WidgetRef = Rc::new(RefCell::new(Box::new(child)));
    let mut layout = BoxLayout::vertical();
    layout.add_widget(child_ref.clone());
    win.root_widget().borrow_mut().set_layout(Box::new(layout));

    // Force initial layout pass and reset sequence counter
    ferrokit_widgets::layout_scheduler::LayoutScheduler::invalidate(&win.root_widget());
    ferrokit_widgets::layout_scheduler::LayoutScheduler::activate_pending();
    counter.store(0, Ordering::SeqCst);
    child_resize_order.store(-1, Ordering::SeqCst);

    let counter_cb = Arc::clone(&counter);
    let cb_order_clone = Arc::clone(&cb_order);

    win.set_resize_handler(move |_size: Size| {
        // Under Qt lifecycle ordering:
        // ResizeEvent is dispatched to widgets and callbacks BEFORE layout activation!
        let order = counter_cb.fetch_add(1, Ordering::SeqCst);
        cb_order_clone.store(order, Ordering::SeqCst);
    });

    let hwnd = win.native_handle() as windows_sys::Win32::Foundation::HWND;
    // Resize window to 600x500
    handle_geometry_change(Delivery::Default, hwnd, Rect::new(0, 0, 600, 500));

    let win_cb = cb_order.load(Ordering::SeqCst);
    let child_res = child_resize_order.load(Ordering::SeqCst);

    // Assert: Window resize callback was called BEFORE child resize event!
    assert!(
        win_cb >= 0,
        "Window resize callback must have been executed"
    );
    assert!(
        child_res >= 0,
        "Child resize event must have been executed via layout activation"
    );
    assert!(
        win_cb < child_res,
        "Window resize callback (order {}) must execute BEFORE child resize event (order {})",
        win_cb,
        child_res
    );
}
