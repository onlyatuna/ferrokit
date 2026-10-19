// SPDX-License-Identifier: LGPL-3.0-only
use crate::layout::{Layout, SizeConstraint};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};
use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_gui::paint::Painter;

/// Layout that displays one child widget at a time from a stack (`QStackedLayout`).
pub struct StackedLayout {
    geometry: Rect,
    margins: Margins,
    widgets: Vec<WidgetRef>,
    current_index: usize,
    dirty: bool,
    /// `QLayoutPrivate::horizontalConstraint` / `verticalConstraint` (both axes alike).
    size_constraint: SizeConstraint,
    /// An item of another layout (`set_nested`).
    nested: bool,
    /// `QLayoutPrivate::insideSpacing`: the explicit spacing, -1 when unset (qlayout.cpp:246-262).
    spacing: i32,
}

impl StackedLayout {
    /// Creates a new empty stacked layout.
    pub fn new() -> Self {
        Self {
            geometry: Rect::new(0, 0, 0, 0),
            margins: Margins::new(0, 0, 0, 0),
            widgets: Vec::new(),
            current_index: 0,
            dirty: true,
            size_constraint: SizeConstraint::SetDefaultConstraint,
            nested: false,
            spacing: -1,
        }
    }

    /// Appends a widget to the stack, returning its page index.
    pub fn add_widget(&mut self, widget: WidgetRef) -> usize {
        let index = self.widgets.len();
        self.widgets.push(widget);
        self.update_layout();
        index
    }

    /// Inserts a widget at the specified index in the stack.
    pub fn insert_widget(&mut self, index: usize, widget: WidgetRef) {
        let clamped = index.min(self.widgets.len());
        self.widgets.insert(clamped, widget);
        self.update_layout();
    }

    /// Removes the widget at index from the stack.
    pub fn remove_widget(&mut self, index: usize) -> Option<WidgetRef> {
        if index < self.widgets.len() {
            let removed = self.widgets.remove(index);
            if self.current_index >= self.widgets.len() && !self.widgets.is_empty() {
                self.current_index = self.widgets.len() - 1;
            }
            self.update_layout();
            Some(removed)
        } else {
            None
        }
    }

    /// Returns the number of widgets in the stack.
    pub fn count(&self) -> usize {
        self.widgets.len()
    }

    /// Returns the index of the currently visible widget.
    pub fn current_index(&self) -> usize {
        self.current_index
    }

    /// Switches the active visible widget to the specified index.
    pub fn set_current_index(&mut self, index: usize) {
        if index < self.widgets.len() {
            self.current_index = index;
            self.update_layout();
        }
    }

    /// Returns a reference to the currently visible widget, if any.
    pub fn current_widget(&self) -> Option<WidgetRef> {
        self.widgets.get(self.current_index).cloned()
    }

    /// `QLayout::setSpacing` (qlayout.cpp:257-262).
    pub fn set_spacing(&mut self, spacing: i32) {
        self.spacing = spacing;
        self.update_layout();
    }

    /// `QLayout::spacing` (qlayout.cpp:246-255).
    pub fn spacing(&self) -> i32 {
        self.spacing
    }
}

impl Default for StackedLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout for StackedLayout {
    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        self.geometry = rect;
        self.dirty = true;
        self.activate();
    }

    fn add_widget(&mut self, widget: WidgetRef) {
        self.add_widget(widget);
    }

    /// `QLayout::setAlignment`: finds the page, then does nothing visible, because
    /// `QStackedLayout::setGeometry` gives the current page the whole rectangle with
    /// `widget->setGeometry(rect)` and never consults the item's alignment
    /// (qstackedlayout.cpp:453-467).
    fn set_alignment(&mut self, widget: &WidgetRef, _alignment: crate::layout::Alignment) -> bool {
        self.widgets.iter().any(|page| std::rc::Rc::ptr_eq(page, widget))
    }

    fn set_margins(&mut self, margins: Margins) {
        self.margins = margins;
        self.update_layout();
    }

    fn margins(&self) -> Margins {
        self.margins
    }

    fn set_nested(&mut self) {
        self.nested = true;
    }

    fn is_nested(&self) -> bool {
        self.nested
    }

    fn set_spacing(&mut self, spacing: i32) {
        self.set_spacing(spacing);
    }

    fn spacing(&self) -> i32 {
        self.spacing()
    }

    /// `QStackedLayout::sizeHint` (qstackedlayout.cpp:417-433): the component-wise maximum over
    /// every page, current or not; an `Ignored` axis counts as 0. The contents margins are not
    /// added.
    fn size_hint(&self) -> Size {
        let mut page = Size::new(0, 0);
        for widget in &self.widgets {
            let w = widget.borrow();
            let hint = w.size_hint();
            let policy = w.size_policy();
            let width = if policy.horizontal == crate::size_policy::Policy::Ignored { 0 } else { hint.width };
            let height = if policy.vertical == crate::size_policy::Policy::Ignored { 0 } else { hint.height };
            page = Size::new(page.width.max(width), page.height.max(height));
        }
        page
    }

    /// `QStackedLayout::minimumSize` (qstackedlayout.cpp:438-448): the maximum of `qSmartMinSize`
    /// over every page. The contents margins are not added.
    fn minimum_size(&self) -> Size {
        let mut page = Size::new(0, 0);
        for widget in &self.widgets {
            let min = crate::layout_engine::smart_min_size(&**widget.borrow());
            page = Size::new(page.width.max(min.width), page.height.max(min.height));
        }
        page
    }

    fn expanding_directions(&self) -> (bool, bool) {
        self.current_widget()
            .map(|curr| crate::layout_engine::item_expanding(&**curr.borrow(), crate::layout::Alignment::NONE))
            .unwrap_or((false, false))
    }

    fn invalidate(&mut self) {
        self.dirty = true;
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn size_constraint(&self) -> SizeConstraint {
        self.size_constraint
    }

    fn set_size_constraint(&mut self, constraint: SizeConstraint) {
        if self.size_constraint != constraint {
            self.size_constraint = constraint;
            self.invalidate();
        }
    }

    /// `QStackedLayout::setGeometry` (qstackedlayout.cpp:453-468), StackOne mode: only the current
    /// page is placed, with `widget->setGeometry(rect)` on the whole rectangle. Contents margins are
    /// not applied, other pages keep their geometry, and `QWidget::setGeometry` clamps the size to
    /// the page's own minimum and maximum size.
    fn activate(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;

        for (idx, widget) in self.widgets.iter().enumerate() {
            let current = idx == self.current_index;
            if current {
                let old_size = {
                    let w = widget.borrow();
                    let g = w.geometry();
                    Size::new(g.width, g.height)
                };
                let w = widget.borrow();
                let (min, max) = (w.minimum_size(), w.maximum_size());
                let width = self.geometry.width.min(max.width).max(min.width);
                let height = self.geometry.height.min(max.height).max(min.height);
                w.set_geometry(Rect::new(self.geometry.x, self.geometry.y, width, height));
                drop(w);
                crate::widget::set_widget_visible(widget, true);
                crate::layout::relayout_child_after_resize(widget, old_size);
            } else {
                crate::widget::set_widget_visible(widget, false);
            }
        }
    }
    fn widgets(&self) -> Vec<WidgetRef> {
        self.widgets.clone()
    }
}

/// A container widget that stacks child widgets, displaying one at a time (`QStackedWidget`).
pub struct StackedWidget {
    base: WidgetBase,
    layout: std::cell::RefCell<StackedLayout>,
    pub current_changed: Signal<usize>,
}

impl StackedWidget {
    /// Creates a new stacked widget container.
    pub fn new() -> Self {
        let base = WidgetBase::new();
        base.set_size_policy(crate::size_policy::QSizePolicy::new(
            crate::size_policy::Policy::Expanding,
            crate::size_policy::Policy::Expanding,
        ));
        let current_changed = Signal::with_owner(&base.object_data);
        Self {
            base,
            layout: std::cell::RefCell::new(StackedLayout::new()),
            current_changed,
        }
    }

    /// Appends a widget to the stack and returns its index.
    pub fn add_widget(&mut self, widget: WidgetRef) -> usize {
        self.base.add_child(widget.clone());
        let idx = self.layout.get_mut().add_widget(widget);
        self.update();
        idx
    }

    /// Returns the index of the currently active page.
    pub fn current_index(&self) -> usize {
        self.layout.borrow().current_index()
    }

    /// Sets the active visible page index, emitting `current_changed`.
    pub fn set_current_index(&mut self, index: usize) {
        let changed = {
            let mut layout = self.layout.borrow_mut();
            if index != layout.current_index() && index < layout.count() {
                layout.set_current_index(index);
                true
            } else {
                false
            }
        };
        if changed {
            self.current_changed.emit(&index);
            self.update();
        }
    }

    /// Returns the currently active widget page.
    pub fn current_widget(&self) -> Option<WidgetRef> {
        self.layout.borrow().current_widget()
    }

    /// Returns the number of widgets in the stack.
    pub fn count(&self) -> usize {
        self.layout.borrow().count()
    }
}

impl Default for StackedWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl QObject for StackedWidget {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Widget for StackedWidget {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }

    fn geometry(&self) -> Rect {
        self.base.geometry()
    }

    fn set_geometry(&self, rect: Rect) {
        if self.base.geometry() != rect {
            self.base.set_geometry(rect);
            self.layout.borrow_mut().set_geometry(Rect::new(0, 0, rect.width, rect.height));
            self.update();
        }
    }

    fn size_hint(&self) -> Size {
        self.layout.borrow().size_hint()
    }

    fn minimum_size_hint(&self) -> Size {
        self.layout.borrow().minimum_size()
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

    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }

    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }

    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }

    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }

    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
        for child in self.children() {
            child.borrow().set_window_id(window_id);
        }
    }

    fn children(&self) -> Vec<WidgetRef> {
        self.base.children()
    }

    fn add_child(&mut self, child: WidgetRef) {
        self.base.add_child(child);
    }

    fn remove_child(&mut self, child_id: ObjectId) {
        self.base.remove_child(child_id);
    }

    fn paint_event(&mut self, _painter: &mut Painter) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
