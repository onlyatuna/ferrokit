// SPDX-License-Identifier: LGPL-3.0-only
//! A change to a widget's style sheet, font, text metrics, size policy or style
//! properties must reach the repaint and layout machinery without any manual `update_layout()`
//! or `render_and_present()`.
//!
//! Qt (`QWidget::event`, qwidget.cpp:9502-9510): `FontChange` / `StyleChange` run
//! a repaint, a geometry update and a layout invalidation. `updateGeometry` (qwidget.cpp:10571-10587)
//! invalidates the parent layout, which posts a `LayoutRequest`. `QWidget::setSizePolicy` calls
//! `updateGeometry`. Unpolishing then polishing the style (what a PySide application does after
//! `setProperty`) ends in a `StyleChange`; `setProperty` alone restyles nothing in Qt, because
//! `QWidget` and `QApplication` ignore `DynamicPropertyChange` (qwidget.cpp:9449,
//! qapplication.cpp:2594).

use ferrokit_widgets::button::Button;
use ferrokit_widgets::frame::Frame;
use ferrokit_widgets::key_sequence_edit::KeySequenceEdit;
use ferrokit_widgets::menu::Menu;
use ferrokit_widgets::progress_bar::ProgressBar;
use ferrokit_widgets::scroll::{ScrollArea, ScrollBar};
use ferrokit_widgets::stacked::StackedWidget;
use ferrokit_widgets::widget::EmptyWidget;
use ferrokit_widgets::{Label, Policy, QSizePolicy, Widget};

fn every_widget_type() -> Vec<(&'static str, Box<dyn Widget>)> {
    vec![
        ("Button", Box::new(Button::new("b"))),
        ("EmptyWidget", Box::new(EmptyWidget::new())),
        ("Frame", Box::new(Frame::new())),
        ("KeySequenceEdit", Box::new(KeySequenceEdit::new())),
        ("Label", Box::new(Label::new("l"))),
        ("Menu", Box::new(Menu::new("m"))),
        ("ProgressBar", Box::new(ProgressBar::new())),
        ("ScrollArea", Box::new(ScrollArea::new())),
        (
            "ScrollBar",
            Box::new(ScrollBar::new(
                ferrokit_widgets::scroll::Orientation::Vertical,
            )),
        ),
        ("StackedWidget", Box::new(StackedWidget::new())),
    ]
}

#[test]
fn set_size_policy_on_every_widget_type_is_not_silently_dropped() {
    let policy = QSizePolicy::new(Policy::Fixed, Policy::Expanding);
    for (name, widget) in every_widget_type() {
        widget.set_size_policy(policy);
        assert_eq!(
            widget.size_policy(),
            policy,
            "{name} dropped set_size_policy"
        );
    }
}

#[test]
fn set_style_sheet_on_every_widget_type_is_not_silently_dropped() {
    for (name, widget) in every_widget_type() {
        widget.set_style_sheet("color: red;");
        assert!(
            widget.style_sheet().is_some(),
            "{name} dropped set_style_sheet"
        );
    }
}

#[test]
fn set_property_on_every_widget_type_can_be_read_back() {
    for (name, widget) in every_widget_type() {
        widget.set_property("state", "warn");
        assert_eq!(
            Widget::property(&*widget, "state").as_deref(),
            Some("warn"),
            "{name} dropped set_property"
        );
    }
}

#[test]
fn changing_the_size_policy_requests_a_parent_layout_and_an_unchanged_one_does_not() {
    use ferrokit_widgets::command::WidgetCommandQueue;
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    let label = Label::new("child");
    label.set_parent_widget(Some(Rc::downgrade(&parent)));

    WidgetCommandQueue::clear();
    label.set_size_policy(QSizePolicy::default());
    assert_eq!(
        WidgetCommandQueue::pending_count(),
        0,
        "QWidget::setSizePolicy returns early when nothing changed"
    );

    label.set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Fixed));
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "updateGeometry must ask the parent layout to run"
    );
    WidgetCommandQueue::clear();
}

/// `QWidget::updateGeometry` on a font, text or alignment change invalidates the parent
/// layout (`FontChange` runs `updateGeometry`, qwidget.cpp:9502; `QLabel::setAlignment` and
/// `QPushButton::setText` both call `updateGeometry`). Each setter must queue a parent layout request.
#[test]
fn label_and_button_text_font_and_alignment_setters_request_a_parent_layout() {
    use ferrokit_gui::text::font::Font;
    use ferrokit_widgets::command::WidgetCommandQueue;
    use ferrokit_widgets::Alignment;
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    let mut label = Label::new("child");
    label.set_parent_widget(Some(Rc::downgrade(&parent)));
    let mut button = Button::new("go");
    button.set_parent_widget(Some(Rc::downgrade(&parent)));

    WidgetCommandQueue::clear();
    label.set_font(Font::new("Segoe UI", 30.0));
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Label::set_font did not request a layout"
    );
    WidgetCommandQueue::clear();
    label.set_text("a longer caption");
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Label::set_text did not request a layout"
    );
    WidgetCommandQueue::clear();
    label.set_alignment(Alignment::CENTER);
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Label::set_alignment did not request a layout"
    );
    WidgetCommandQueue::clear();
    button.set_font(Font::new("Segoe UI", 30.0));
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Button::set_font did not request a layout"
    );
    WidgetCommandQueue::clear();
    button.set_text("a considerably longer button caption");
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Button::set_text did not request a layout"
    );
    WidgetCommandQueue::clear();
}

/// `QLabel::setText` and `QPushButton::setText` return early when the text is unchanged
/// (qlabel.cpp:244-245, qabstractbutton.cpp:496-497): no `updateGeometry`, no repaint.
#[test]
fn label_and_button_set_text_with_unchanged_text_requests_nothing() {
    use ferrokit_widgets::command::WidgetCommandQueue;
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    let mut label = Label::new("12天 3時 4分");
    label.set_parent_widget(Some(Rc::downgrade(&parent)));
    let mut button = Button::new("go");
    button.set_parent_widget(Some(Rc::downgrade(&parent)));

    label.clear_dirty();
    button.clear_dirty();
    WidgetCommandQueue::clear();
    label.set_text("12天 3時 4分");
    assert_eq!(
        WidgetCommandQueue::pending_count(),
        0,
        "Label::set_text with unchanged text requested a layout"
    );
    assert!(
        label.dirty_rect().is_none(),
        "Label::set_text with unchanged text repainted"
    );
    button.set_text("go");
    assert_eq!(
        WidgetCommandQueue::pending_count(),
        0,
        "Button::set_text with unchanged text requested a layout"
    );
    assert!(
        button.dirty_rect().is_none(),
        "Button::set_text with unchanged text repainted"
    );

    label.set_text("13天 3時 4分");
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Label::set_text with new text did not request a layout"
    );
    assert!(
        label.dirty_rect().is_some(),
        "Label::set_text with new text did not repaint"
    );
    WidgetCommandQueue::clear();
    button.set_text("stop");
    assert!(
        WidgetCommandQueue::pending_count() > 0,
        "Button::set_text with new text did not request a layout"
    );
    assert!(
        button.dirty_rect().is_some(),
        "Button::set_text with new text did not repaint"
    );
    WidgetCommandQueue::clear();
}

#[test]
fn set_visible_on_every_widget_type_requests_a_parent_layout_only_when_it_changes() {
    use ferrokit_widgets::command::WidgetCommandQueue;
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    for (name, widget) in every_widget_type() {
        if name == "Menu" {
            continue; // a popup window: it has no parent layout, and starts hidden
        }
        widget.set_parent_widget(Some(Rc::downgrade(&parent)));
        WidgetCommandQueue::clear();
        widget.set_visible(true);
        assert_eq!(
            WidgetCommandQueue::pending_count(),
            0,
            "{name}: setVisible(true) on a visible widget"
        );
        widget.set_visible(false);
        assert!(
            WidgetCommandQueue::pending_count() > 0,
            "{name}: hide did not request a layout"
        );
        WidgetCommandQueue::clear();
        widget.set_visible(true);
        assert!(
            WidgetCommandQueue::pending_count() > 0,
            "{name}: show did not request a layout"
        );
        WidgetCommandQueue::clear();
    }
}

/// `QWidgetPrivate::update` returns for a widget that is not visible, which includes one whose
/// parent is hidden (qwidget.cpp:11367-11368): nothing is marked for repaint. Every widget type
/// is checked under a hidden parent, and once the parent is shown again `update` repaints.
#[test]
fn update_under_a_hidden_parent_marks_nothing_until_the_parent_is_shown() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    parent.borrow().set_visible(false);
    for (name, widget) in every_widget_type() {
        if name == "Menu" {
            continue; // a popup window with its own `update`
        }
        widget.set_geometry(ferrokit_gui::geometry::primitives::Rect::new(0, 0, 20, 10));
        widget.set_parent_widget(Some(Rc::downgrade(&parent)));
        widget.clear_dirty();
        widget.update();
        assert_eq!(
            widget.dirty_rect(),
            None,
            "{name}: update() under a hidden parent"
        );
    }
    parent.borrow().set_visible(true);
    let child = Label::new("c");
    child.set_geometry(ferrokit_gui::geometry::primitives::Rect::new(0, 0, 20, 10));
    child.set_parent_widget(Some(Rc::downgrade(&parent)));
    child.clear_dirty();
    child.update();
    assert!(
        child.dirty_rect().is_some(),
        "update() under a shown parent must repaint"
    );
}

/// `hide_sys` invalidates the area the widget leaves before it stops counting as visible
/// (qwidget.cpp:8312-8319), so its parent repaints there.
#[test]
fn hiding_a_widget_repaints_the_area_it_leaves() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let parent: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Frame::new())));
    parent
        .borrow()
        .set_geometry(ferrokit_gui::geometry::primitives::Rect::new(0, 0, 100, 50));
    let child: ferrokit_widgets::WidgetRef = Rc::new(RefCell::new(Box::new(Label::new("c"))));
    child
        .borrow()
        .set_geometry(ferrokit_gui::geometry::primitives::Rect::new(10, 5, 20, 10));
    child
        .borrow()
        .set_parent_widget(Some(Rc::downgrade(&parent)));
    parent.borrow_mut().add_child(Rc::clone(&child));
    parent.borrow().clear_dirty();
    child.borrow().clear_dirty();

    child.borrow().set_visible(false);
    let dirty = ferrokit_widgets::window::collect_dirty_region(
        &parent,
        ferrokit_gui::geometry::primitives::Point::new(0, 0),
    );
    assert_eq!(
        dirty,
        Some(ferrokit_gui::geometry::primitives::Rect::new(10, 5, 20, 10)),
        "the hidden child's area must be repainted"
    );
}

#[cfg(windows)]
mod pumped {
    //! Real `Window`, real event loop, one pump per observation.
    use std::cell::RefCell;
    use std::rc::Rc;

    use ferrokit_core::event_loop::EventLoop;
    use ferrokit_gui::geometry::primitives::Rect;
    use ferrokit_gui::text::font::Font;
    use ferrokit_platform::WindowFlags;
    use ferrokit_widgets::button::Button;
    use ferrokit_widgets::layout::{BoxLayout, Layout};
    use ferrokit_widgets::widget::{EmptyWidget, WidgetRef};
    use ferrokit_widgets::window::Window;
    use ferrokit_widgets::{Label, Policy, QSizePolicy, Widget};

    struct Fixture {
        el: EventLoop,
        win: Box<Window>,
        first: WidgetRef,
        second: WidgetRef,
        spacing: i32,
    }

    const WINDOW_WIDTH: i32 = 400;

    fn fixture(first: Box<dyn Widget>, second: Box<dyn Widget>) -> Fixture {
        let el = EventLoop::new();
        let mut win = Box::new(
            Window::new(
                "Widget Invalidation",
                Rect::new(0, 0, WINDOW_WIDTH, 120),
                WindowFlags::empty(),
            )
            .expect("create window"),
        );

        let first: WidgetRef = Rc::new(RefCell::new(first));
        let second: WidgetRef = Rc::new(RefCell::new(second));
        let mut layout = BoxLayout::horizontal();
        layout.set_spacing(6);
        layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
        layout.add_widget(Rc::clone(&first));
        layout.add_widget(Rc::clone(&second));
        let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
        container.borrow_mut().set_layout(Box::new(layout));
        win.set_root_widget(container);

        let mut fx = Fixture {
            el,
            win,
            first,
            second,
            spacing: 6,
        };
        // Settle the initial layout and paint.
        fx.win.render_and_present();
        pump_idle(&mut fx);
        fx
    }

    /// One posted-event pump per turn until nothing changes (bounded).
    fn pump_idle(fx: &mut Fixture) -> u32 {
        for turn in 1..=16 {
            let before = fx.win.render_stats();
            fx.el.process_events(false);
            if fx.win.render_stats() == before {
                return turn;
            }
        }
        panic!("render ping-pong: still rendering after 16 turns");
    }

    fn label_pair() -> Fixture {
        let mut a = Label::new("Alpha");
        a.set_font(Font::new("Segoe UI", 12.0));
        let mut b = Label::new("Beta");
        b.set_font(Font::new("Segoe UI", 12.0));
        fixture(Box::new(a), Box::new(b))
    }

    /// With both labels `Preferred`, the second sits right after the first's allotted width.
    fn left_of_second(fx: &Fixture) -> i32 {
        fx.second.borrow().geometry().x
    }

    #[test]
    fn style_sheet_font_size_change_relayouts_parent_after_one_pump() {
        // A Button: its size policy already works, so this isolates the style-sheet path.
        let mut fx = fixture(Box::new(Button::new("Alpha")), Box::new(Label::new("Beta")));
        let renders_before = fx.win.render_stats().render_count;

        // Fixed so the first label's width follows its size hint exactly.
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        pump_idle(&mut fx);
        let hint_before = fx.first.borrow().size_hint().width;
        fx.first.borrow().set_style_sheet("font-size: 28px;");
        let turns = pump_idle(&mut fx);

        let hint = fx.first.borrow().size_hint();
        assert!(
            hint.width > hint_before,
            "a 28px font must be wider than the 12pt label was"
        );
        assert_eq!(
            fx.first.borrow().geometry().width,
            hint.width,
            "the label did not take its new size hint (no relayout after {turns} pump turns)"
        );
        assert_eq!(
            left_of_second(&fx),
            hint.width + fx.spacing,
            "the sibling did not move"
        );
        assert!(
            fx.win.render_stats().render_count > renders_before,
            "a style change must schedule a repaint"
        );
    }

    #[test]
    fn label_set_size_policy_changes_layout_result() {
        let mut fx = label_pair();
        let first_hint = fx.first.borrow().size_hint().width;

        // `Fixed` keeps the first label at its hint; the second takes everything that is left.
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        pump_idle(&mut fx);
        assert_eq!(fx.first.borrow().geometry().width, first_hint);
        assert_eq!(
            fx.second.borrow().geometry().width,
            WINDOW_WIDTH - first_hint - fx.spacing
        );

        // `Expanding` hands the first label the free space (equal stretch, 0): both expand.
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Expanding, Policy::Preferred));
        fx.second
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        pump_idle(&mut fx);
        let second_hint = fx.second.borrow().size_hint().width;
        assert_eq!(fx.second.borrow().geometry().width, second_hint);
        assert_eq!(
            fx.first.borrow().geometry().width,
            WINDOW_WIDTH - second_hint - fx.spacing
        );
    }

    #[test]
    fn label_set_font_relayouts_parent_after_one_pump() {
        let mut fx = label_pair();
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        pump_idle(&mut fx);
        let before = fx.first.borrow().geometry().width;

        {
            let mut w = fx.first.borrow_mut();
            let label = w.as_any_mut().downcast_mut::<Label>().unwrap();
            label.set_font(Font::new("Segoe UI", 30.0));
        }
        pump_idle(&mut fx);

        let hint = fx.first.borrow().size_hint().width;
        assert!(hint > before);
        assert_eq!(
            fx.first.borrow().geometry().width,
            hint,
            "label kept its old width"
        );
        assert_eq!(left_of_second(&fx), hint + fx.spacing);
    }

    #[test]
    fn button_set_text_relayouts_parent_after_one_pump() {
        let mut fx = fixture(Box::new(Button::new("Go")), Box::new(Label::new("tail")));
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        pump_idle(&mut fx);
        let before = fx.first.borrow().geometry().width;

        {
            let mut w = fx.first.borrow_mut();
            let button = w.as_any_mut().downcast_mut::<Button>().unwrap();
            button.set_text("A considerably longer button caption");
        }
        pump_idle(&mut fx);

        let hint = fx.first.borrow().size_hint().width;
        assert!(hint > before);
        assert_eq!(
            fx.first.borrow().geometry().width,
            hint,
            "button kept its old width"
        );
        assert_eq!(left_of_second(&fx), hint + fx.spacing);
    }

    /// A size-hint change two levels down reaches the root layout. Qt's
    /// `QLayout::activate` ends with `mw->updateGeometry()` (qlayout.cpp:1131), so after the
    /// wrapper's layout runs, the wrapper's own parent layout is invalidated in turn.
    #[test]
    fn a_label_text_change_inside_a_fixed_wrapper_resizes_the_wrapper_and_moves_the_sibling() {
        let mut inner = Label::new("Alpha");
        inner.set_font(Font::new("Segoe UI", 12.0));
        let inner: WidgetRef = Rc::new(RefCell::new(Box::new(inner)));
        let mut wrapper_layout = BoxLayout::horizontal();
        wrapper_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
        wrapper_layout.add_widget(Rc::clone(&inner));
        let mut wrapper = EmptyWidget::new();
        wrapper.set_layout(Box::new(wrapper_layout));
        wrapper.set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        let mut fx = fixture(Box::new(wrapper), Box::new(Label::new("Beta")));
        let before = fx.first.borrow().geometry().width;
        assert_eq!(before, fx.first.borrow().size_hint().width);

        {
            let mut w = inner.borrow_mut();
            let label = w.as_any_mut().downcast_mut::<Label>().unwrap();
            label.set_text("A considerably longer caption than before");
        }
        pump_idle(&mut fx);

        let hint = fx.first.borrow().size_hint().width;
        assert!(hint > before);
        assert_eq!(
            fx.first.borrow().geometry().width,
            hint,
            "the wrapper kept its old width"
        );
        assert_eq!(
            left_of_second(&fx),
            hint + fx.spacing,
            "the sibling did not move"
        );
    }

    /// `QWidget::setVisible` on a child runs `QLayout::invalidate` on the parent
    /// (qwidget.cpp:8465-8468), and a hidden `QWidgetItem` is empty (qlayoutitem.cpp:691-693).
    #[test]
    fn hide_and_show_relayout_the_parent_after_one_pump_without_a_manual_call() {
        let mut fx = label_pair();
        let shown_x = left_of_second(&fx);
        assert!(shown_x > 0);

        fx.first.borrow().set_visible(false);
        pump_idle(&mut fx);
        assert_eq!(
            left_of_second(&fx),
            0,
            "the sibling did not move into the freed space"
        );

        fx.first.borrow().set_visible(true);
        pump_idle(&mut fx);
        assert_eq!(
            left_of_second(&fx),
            shown_x,
            "showing did not restore the layout"
        );
    }

    #[test]
    fn repolish_after_set_property_restyles_and_relayouts_after_one_pump() {
        let mut fx = label_pair();
        fx.first
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Fixed, Policy::Preferred));
        fx.first.borrow().set_style_sheet(
            "QLabel { font-size: 12px; } QLabel[state=\"big\"] { font-size: 30px; }",
        );
        pump_idle(&mut fx);
        let before = fx.first.borrow().geometry().width;

        fx.first.borrow().set_property("state", "big");
        fx.first.borrow().repolish();
        pump_idle(&mut fx);

        let hint = fx.first.borrow().size_hint().width;
        assert!(hint > before, "the property rule must enlarge the label");
        assert_eq!(fx.first.borrow().geometry().width, hint);
        assert_eq!(left_of_second(&fx), hint + fx.spacing);
    }

    /// Qt `QLayout::widgetEvent` (qlayout.cpp:520-535): a Resize re-lays out the widget's own
    /// layout. `inner` owns a layout and is resized by a layout-less parent (`first`, a plain
    /// `EmptyWidget` with no layout), so nothing else queues `inner`'s layout pass.
    #[test]
    fn a_layout_owning_widget_resized_by_a_layout_less_parent_relayouts_its_children() {
        let mut fx = fixture(Box::new(EmptyWidget::new()), Box::new(Label::new("Beta")));

        let alpha: WidgetRef = Rc::new(RefCell::new(Box::new(Label::new("Alpha"))));
        alpha
            .borrow()
            .set_size_policy(QSizePolicy::new(Policy::Expanding, Policy::Preferred));
        let mut inner_layout = BoxLayout::horizontal();
        inner_layout.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
        inner_layout.add_widget(Rc::clone(&alpha));
        let mut inner_widget = EmptyWidget::new();
        inner_widget.set_layout(Box::new(inner_layout));
        let inner: WidgetRef = Rc::new(RefCell::new(Box::new(inner_widget)));

        fx.first.borrow_mut().add_child(Rc::clone(&inner));
        pump_idle(&mut fx);

        inner.borrow().set_geometry(Rect::new(0, 0, 300, 40));
        pump_idle(&mut fx);

        assert_eq!(
            alpha.borrow().geometry().width,
            300,
            "the child layout was not re-run"
        );
    }
}
