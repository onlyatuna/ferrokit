// SPDX-License-Identifier: LGPL-3.0-only
//! Parity tests for `ToolTipChange` event.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::event::{Event, EventKind};
use ferrokit_core::object::{
    register_owned_qobject, register_qobject_ptr, unregister_qobject, ObjectId,
};
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_widgets::widget::{
    set_owned_widget_tool_tip, set_widget_ptr_tool_tip, set_widget_tool_tip, Widget, WidgetRef,
};
struct ToolTipProbe {
    base: ferrokit_widgets::widget::WidgetBase,
    change_seen: Arc<AtomicBool>,
}

impl ferrokit_core::object::QObject for ToolTipProbe {
    fn object_data(&self) -> &ferrokit_core::object::ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, ev: &mut Event) -> bool {
        if matches!(ev.kind, EventKind::ToolTipChange) {
            self.change_seen.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }
}

impl Widget for ToolTipProbe {
    fn widget_base(&self) -> &ferrokit_widgets::widget::WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }
    fn geometry(&self) -> Rect {
        self.base.geometry()
    }
    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
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
    fn update(&self) {
        self.base.update();
    }
    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }
    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }
    fn layout(&self) -> Option<&dyn ferrokit_widgets::layout::Layout> {
        None
    }
    fn layout_mut(&mut self) -> Option<&mut Box<dyn ferrokit_widgets::layout::Layout>> {
        None
    }
    fn set_layout(&mut self, _layout: Box<dyn ferrokit_widgets::layout::Layout>) {}
    fn parent_widget(&self) -> Option<ferrokit_widgets::widget::WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<ferrokit_widgets::widget::WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }
    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<ferrokit_widgets::widget::WidgetRef> {
        self.base.children()
    }
    fn add_child(&mut self, child: ferrokit_widgets::widget::WidgetRef) {
        self.base.children.borrow_mut().push(child);
    }
    fn remove_child(&mut self, child_id: ObjectId) {
        self.base
            .children
            .borrow_mut()
            .retain(|c| c.borrow().id() != child_id);
    }
    fn paint_event(&mut self, _painter: &mut ferrokit_gui::paint::Painter) {}
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[test]
fn set_tool_tip_delivers_tool_tip_change_event() {
    // 1. Pointer-registered stack widget using two-phase `set_widget_ptr_tool_tip`.
    let seen = Arc::new(AtomicBool::new(false));
    let mut probe = ToolTipProbe {
        base: ferrokit_widgets::widget::WidgetBase::new(),
        change_seen: Arc::clone(&seen),
    };
    let id = probe.id();
    let probe_ptr = &raw mut probe;
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(probe_ptr));
    }

    assert!(!seen.load(Ordering::SeqCst));
    assert!(unsafe { set_widget_ptr_tool_tip(probe_ptr, "Hello Tooltip") });
    assert!(
        seen.load(Ordering::SeqCst),
        "set_widget_ptr_tool_tip must send ToolTipChange event"
    );
    assert_eq!(probe.tool_tip(), "Hello Tooltip");

    unsafe {
        unregister_qobject(id);
    }

    // 2. Heap-owned registered widget using two-phase `set_owned_widget_tool_tip`.
    let seen_owned = Arc::new(AtomicBool::new(false));
    let owned = unsafe {
        register_owned_qobject(ToolTipProbe {
            base: ferrokit_widgets::widget::WidgetBase::new(),
            change_seen: Arc::clone(&seen_owned),
        })
    };
    assert!(set_owned_widget_tool_tip(&owned, "Owned Tooltip"));
    assert!(seen_owned.load(Ordering::SeqCst));
    assert_eq!(owned.with(|o| o.tool_tip().to_string()).as_deref(), Some("Owned Tooltip"));
    drop(owned);

    // 3. Direct `set_tool_tip` on an unregistered widget updates text without UB.
    let seen_unreg = Arc::new(AtomicBool::new(false));
    let unreg = ToolTipProbe {
        base: ferrokit_widgets::widget::WidgetBase::new(),
        change_seen: Arc::clone(&seen_unreg),
    };
    unreg.set_tool_tip("Direct Unregistered");
    assert_eq!(unreg.tool_tip(), "Direct Unregistered");
    assert!(!seen_unreg.load(Ordering::SeqCst));

    // 4. Direct `set_tool_tip` on a registered widget updates text without taking an overlapping
    //    mutable borrow through the registry, passing Stacked Borrows and Tree Borrows.
    let seen_reg = Arc::new(AtomicBool::new(false));
    let mut reg_probe = ToolTipProbe {
        base: ferrokit_widgets::widget::WidgetBase::new(),
        change_seen: Arc::clone(&seen_reg),
    };
    let reg_id = reg_probe.id();
    let reg_ptr = &raw mut reg_probe;
    unsafe {
        register_qobject_ptr(NonNull::new_unchecked(reg_ptr));
    }
    reg_probe.set_tool_tip("Direct Registered");
    assert_eq!(reg_probe.tool_tip(), "Direct Registered");
    assert!(
        !seen_reg.load(Ordering::SeqCst),
        "set_tool_tip(&self) avoids synchronous send_event to eliminate alias conflicts"
    );
    unsafe {
        unregister_qobject(reg_id);
    }

    // 5. `WidgetRef` using two-phase `set_widget_tool_tip` for event delivery.
    let seen_ref = Arc::new(AtomicBool::new(false));
    let widget_ref: WidgetRef = Rc::new(RefCell::new(Box::new(ToolTipProbe {
        base: ferrokit_widgets::widget::WidgetBase::new(),
        change_seen: Arc::clone(&seen_ref),
    })));
    set_widget_tool_tip(&widget_ref, "Ref Tooltip");
    assert!(seen_ref.load(Ordering::SeqCst));
    assert_eq!(widget_ref.borrow().tool_tip(), "Ref Tooltip");
}
