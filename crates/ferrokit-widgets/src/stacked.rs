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
        let changes = self.insert_state(index, widget);
        apply_visibility(changes);
        index
    }

    /// Inserts a widget at the specified index in the stack (`QStackedLayout::insertWidget`,
    /// qstackedlayout.cpp:230-260): a page inserted at or before the current one keeps the current
    /// page current, and a page that does not become current is hidden.
    pub fn insert_widget(&mut self, index: usize, widget: WidgetRef) {
        let changes = self.insert_state(index, widget);
        apply_visibility(changes);
    }

    /// Removes the widget at index from the stack (`QStackedLayout::takeAt`,
    /// qstackedlayout.cpp:240-262): removing the current page shows its neighbour, removing an
    /// earlier one keeps the current page current, and the removed page is hidden last.
    pub fn remove_widget(&mut self, index: usize) -> Option<WidgetRef> {
        let (removed, changes) = self.remove_state(index);
        apply_visibility(changes);
        removed
    }

    /// `insert_widget` without the visibility changes, which the caller applies with
    /// `apply_visibility` once no borrow of the layout is held (they send `Show`/`Hide`).
    pub(crate) fn insert_state(&mut self, index: usize, widget: WidgetRef) -> Vec<(WidgetRef, bool)> {
        let clamped = index.min(self.widgets.len());
        let first = self.widgets.is_empty();
        self.widgets.insert(clamped, widget.clone());
        self.update_layout();
        if first {
            self.current_index = clamped;
            vec![(widget, true)]
        } else {
            if clamped <= self.current_index {
                self.current_index += 1;
            }
            vec![(widget, false)]
        }
    }

    /// `remove_widget` without the visibility changes (see `insert_state`), in Qt's order: the new
    /// current page is shown before the removed one is hidden.
    pub(crate) fn remove_state(&mut self, index: usize) -> (Option<WidgetRef>, Vec<(WidgetRef, bool)>) {
        if index >= self.widgets.len() {
            return (None, Vec::new());
        }
        let removed = self.widgets.remove(index);
        let mut changes = Vec::new();
        if index == self.current_index {
            if self.widgets.is_empty() {
                self.current_index = 0;
            } else {
                self.current_index = index.min(self.widgets.len() - 1);
                changes.push((self.widgets[self.current_index].clone(), true));
            }
        } else if index < self.current_index {
            self.current_index -= 1;
        }
        changes.push((removed.clone(), false));
        self.update_layout();
        (Some(removed), changes)
    }

    /// `set_current_index` without the page switch (see `insert_state`): the old and the new
    /// page, for `switch_page`. `None` when `index` is out of range or already current
    /// (qstackedlayout.cpp:295-297).
    pub(crate) fn set_current_state(&mut self, index: usize) -> Option<(Option<WidgetRef>, WidgetRef)> {
        if index >= self.widgets.len() || index == self.current_index {
            return None;
        }
        let previous = self.widgets.get(self.current_index).cloned();
        self.current_index = index;
        self.update_layout();
        Some((previous, self.widgets[index].clone()))
    }

    /// The page to place and its rectangle (`QStackedLayout::setGeometry`), clearing the dirty flag.
    /// The caller sets the geometry once no borrow of the layout is held.
    pub(crate) fn take_placement(&mut self) -> Option<(WidgetRef, Rect)> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let widget = self.widgets.get(self.current_index)?.clone();
        let (min, max) = {
            let w = widget.borrow();
            (w.minimum_size(), w.maximum_size())
        };
        let width = self.geometry.width.min(max.width).max(min.width);
        let height = self.geometry.height.min(max.height).max(min.height);
        Some((widget, Rect::new(self.geometry.x, self.geometry.y, width, height)))
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
        if let Some((prev, next)) = self.set_current_state(index) {
            switch_page(prev.as_ref(), &next);
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
        if let Some((widget, rect)) = self.take_placement() {
            place_page(&widget, rect);
        }
    }
    fn widgets(&self) -> Vec<WidgetRef> {
        self.widgets.clone()
    }
}

/// Shows and hides the pages a layout change decided on, in order.
pub(crate) fn apply_visibility(changes: Vec<(WidgetRef, bool)>) {
    for (widget, visible) in changes {
        crate::widget::set_widget_visible(&widget, visible);
    }
}

/// The page switch of `QStackedLayout::setCurrentIndex` (qstackedlayout.cpp:302-336): when the
/// focus is on `prev` it is cleared before `prev` is hidden, and once `next` is shown it moves
/// into `next`, so it never leaves the stack.
pub(crate) fn switch_page(prev: Option<&WidgetRef>, next: &WidgetRef) {
    let focus = prev.and_then(crate::focus::take_page_focus);
    if let Some(prev) = prev {
        crate::widget::set_widget_visible(prev, false);
    }
    crate::widget::set_widget_visible(next, true);
    if let Some(focus) = focus {
        crate::focus::give_page_focus(focus, next);
    }
}

/// `widget->setGeometry(rect)` on the current page, then the relayout its new size needs. It never
/// changes the page's visibility.
fn place_page(widget: &WidgetRef, rect: Rect) {
    let old_size = {
        let g = widget.borrow().geometry();
        Size::new(g.width, g.height)
    };
    widget.borrow().set_geometry(rect);
    crate::layout::relayout_child_after_resize(widget, old_size);
}

/// A container widget that stacks child widgets, displaying one at a time (`QStackedWidget`).
pub struct StackedWidget {
    base: WidgetBase,
    layout: std::cell::RefCell<StackedLayout>,
    /// `QStackedWidget::currentChanged(int)`: the new current index, -1 once the stack is empty.
    pub current_changed: Signal<i32>,
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

    /// Runs `f` on the stack with its borrow held only for the call.
    fn with<R>(stack: &WidgetRef, f: impl FnOnce(&mut StackedWidget) -> R) -> R {
        let mut guard = stack.borrow_mut();
        let stacked = guard
            .as_any_mut()
            .downcast_mut::<StackedWidget>()
            .expect("not a StackedWidget");
        f(stacked)
    }

    /// Appends a page to the stack and returns its index.
    pub fn add_widget(stack: &WidgetRef, widget: WidgetRef) -> usize {
        let index = Self::with(stack, |s| s.layout.borrow().count());
        Self::insert_widget(stack, index, widget)
    }

    /// Inserts a page at `index`; see [`StackedLayout::insert_widget`]. Show and hide events, and
    /// `current_changed`, are sent with the stack not borrowed. Returns the page's index.
    pub fn insert_widget(stack: &WidgetRef, index: usize, widget: WidgetRef) -> usize {
        let (at, first, changes, signal) = Self::with(stack, |s| {
            s.base.add_child(widget.clone());
            let mut layout = s.layout.borrow_mut();
            let at = index.min(layout.count());
            let first = layout.count() == 0;
            let changes = layout.insert_state(index, widget);
            (at, first, changes, s.current_changed.clone())
        });
        apply_visibility(changes);
        if first {
            signal.emit(&(at as i32));
        }
        stack.borrow().update();
        at
    }

    /// Removes the page at `index` (`QStackedWidget::removeWidget`): the neighbouring page becomes
    /// current when the current one was removed, and `current_changed` reports it (-1 when the
    /// stack is now empty) before the removed page is hidden. The page stays a child of the stack,
    /// as Qt keeps its parent.
    pub fn remove_widget(stack: &WidgetRef, index: usize) -> Option<WidgetRef> {
        let (removed, mut changes, current, signal) = Self::with(stack, |s| {
            let mut layout = s.layout.borrow_mut();
            let was_current = index == layout.current_index();
            let (removed, changes) = layout.remove_state(index);
            let current = was_current.then(|| match layout.count() {
                0 => -1,
                _ => layout.current_index() as i32,
            });
            (removed, changes, current, s.current_changed.clone())
        });
        let removed = removed?;
        let hide = changes.pop();
        apply_visibility(changes);
        if let Some(current) = current {
            signal.emit(&current);
        }
        apply_visibility(hide.into_iter().collect());
        stack.borrow().update();
        Some(removed)
    }

    /// Returns the index of the currently active page.
    pub fn current_index(&self) -> usize {
        self.layout.borrow().current_index()
    }

    /// Sets the active visible page index, emitting `current_changed`; see
    /// [`StackedLayout::set_current_index`]. The pages switch with the stack not borrowed.
    pub fn set_current_index(stack: &WidgetRef, index: usize) {
        let (pages, signal) = Self::with(stack, |s| {
            let pages = s.layout.borrow_mut().set_current_state(index);
            (pages, s.current_changed.clone())
        });
        if let Some((prev, next)) = pages {
            switch_page(prev.as_ref(), &next);
            signal.emit(&(index as i32));
            stack.borrow().update();
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
            // The layout's borrow is released before the page is placed: the page's `Resize` runs
            // filters and slots that may read the stack.
            let placement = {
                let mut layout = self.layout.borrow_mut();
                layout.geometry = Rect::new(0, 0, rect.width, rect.height);
                layout.dirty = true;
                layout.take_placement()
            };
            if let Some((page, page_rect)) = placement {
                place_page(&page, page_rect);
            }
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
