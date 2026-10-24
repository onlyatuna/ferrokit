// SPDX-License-Identifier: LGPL-3.0-only
//! `QScrollArea` follows its content widget's resizes (`QScrollArea::eventFilter` on `Resize`,
//! installed by `setWidget`): for a content that overrides `set_geometry`, while the area is
//! borrowed, and before the content has a parent link.

use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::command::WidgetCommandQueue;
use ferrokit_widgets::scroll::ScrollArea;
use ferrokit_widgets::widget::{Widget, WidgetBase, WidgetRef};
use std::cell::RefCell;
use std::rc::Rc;

/// Content that keeps its own size in `base.geometry` and reports the change itself.
struct SelfSized {
    base: WidgetBase,
}

impl QObject for SelfSized {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
}

impl Widget for SelfSized {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        let old = self.base.geometry.replace(rect);
        if (old.width, old.height) != (rect.width, rect.height) {
            self.base.notify_parent_resized();
        }
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }
    fn update(&self) {}
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn layout(&self) -> Option<&dyn ferrokit_widgets::layout::Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn ferrokit_widgets::layout::Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn ferrokit_widgets::layout::Layout>) {}
    fn parent_widget(&self) -> Option<ferrokit_widgets::WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<ferrokit_widgets::WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }
    fn add_child(&mut self, _child: WidgetRef) {}
    fn remove_child(&mut self, _child_id: ObjectId) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

fn content(w: i32, h: i32) -> WidgetRef {
    Rc::new(RefCell::new(Box::new(SelfSized {
        base: WidgetBase::with_geometry(Rect::new(0, 0, w, h)),
    })))
}

fn area_with(content: &WidgetRef, link: bool) -> WidgetRef {
    let mut a = ScrollArea::new();
    a.set_geometry(Rect::new(0, 0, 110, 100));
    a.set_widget(Rc::clone(content));
    let a: WidgetRef = Rc::new(RefCell::new(Box::new(a)));
    if link {
        content.borrow().set_parent_widget(Some(Rc::downgrade(&a)));
    }
    a
}

fn max_v(a: &WidgetRef) -> (bool, i32) {
    let b = a.borrow();
    let s = b.as_any().downcast_ref::<ScrollArea>().unwrap();
    (s.vertical_scroll_bar().is_visible(), s.vertical_scroll_bar().maximum())
}

#[test]
fn a_content_overriding_set_geometry_updates_the_range() {
    let c = content(100, 100);
    let a = area_with(&c, true);
    c.borrow().set_geometry(Rect::new(0, 0, 100, 500));
    assert_eq!(max_v(&a), (true, 400));
}

#[test]
fn a_resize_while_the_area_is_borrowed_is_applied_at_the_next_flush() {
    let c = content(100, 100);
    let a = area_with(&c, true);
    {
        let _busy = a.borrow_mut();
        c.borrow().set_geometry(Rect::new(0, 0, 100, 500));
    }
    WidgetCommandQueue::flush();
    assert_eq!(max_v(&a), (true, 400));
}

#[test]
fn a_content_resized_before_it_has_a_parent_link_is_picked_up() {
    let c = content(100, 100);
    let a = area_with(&c, false);
    c.borrow().set_geometry(Rect::new(0, 0, 100, 500));
    assert_eq!(max_v(&a), (true, 400), "no adopt_tree yet: the subscription from set_widget works");
}
