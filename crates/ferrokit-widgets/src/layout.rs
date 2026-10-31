// SPDX-License-Identifier: LGPL-3.0-only
use crate::layout_engine::{
    alignment_rect, distribute_multi_box, find_size, init_empty_multi_box, item_expanding,
    item_has_height_for_width, item_height_for_width, item_is_empty, item_minimum_size,
    item_set_geometry_with, item_size_hint, q_geom_calc, q_max_exp_calc, setup_spacings,
    ItemLimits, LayoutStruct, LAYOUT_SIZE_MAX,
};
use crate::size_policy::Policy;
use crate::widget::{EmptyWidget, Widget, WidgetRef};
use ferrokit_gui::application::GuiApplication;
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use std::cell::{Cell, RefCell};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    TopToBottom,
    LeftToRight,
}

/// `Qt::Alignment` of a layout item (`QLayoutItem::setAlignment`): where the item sits in the
/// space the layout gives it. With no flag on an axis the item fills that axis (up to its maximum
/// size); with a flag it shrinks to its size hint and is placed by the flag. The same type aligns
/// a layout, a `Label`'s text and a text block, as `Qt::Alignment` does.
pub use ferrokit_gui::Alignment;

/// `QLayout::SizeConstraint` (qlayout.h): how `QLayout::activate` constrains the owner widget of a
/// top-level layout (qlayout.cpp:1015-1095). Nested layouts ignore it.
#[allow(clippy::enum_variant_names)] // Qt's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeConstraint {
    /// A window's minimum becomes the layout's total minimum size; a child widget's becomes 0. An
    /// axis with an explicit minimum (`set_minimum_size`) keeps it.
    #[default]
    SetDefaultConstraint,
    /// The owner is not constrained.
    SetNoConstraint,
    /// The owner's minimum becomes the layout's total minimum size.
    SetMinimumSize,
    /// The owner's minimum and maximum become the layout's total size hint.
    SetFixedSize,
    /// The owner's maximum becomes the layout's total maximum size.
    SetMaximumSize,
    /// The owner's minimum and maximum become the layout's total minimum and maximum sizes.
    SetMinAndMaxSize,
}

/// `QSpacerItem`: a blank box with a size hint and a size policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpacerItem {
    pub width: i32,
    pub height: i32,
    pub horizontal: Policy,
    pub vertical: Policy,
}

impl SpacerItem {
    pub const fn new(width: i32, height: i32, horizontal: Policy, vertical: Policy) -> Self {
        Self {
            width,
            height,
            horizontal,
            vertical,
        }
    }

    /// `QSpacerItem::sizeHint`: the given size, whatever the policy (qlayoutitem.cpp:651-653).
    pub fn size_hint(&self) -> Size {
        Size::new(self.width, self.height)
    }

    /// `QSpacerItem::minimumSize`: 0 where the policy may shrink (qlayoutitem.cpp:607-610).
    pub fn minimum_size(&self) -> Size {
        let min = |len: i32, p: Policy| if p.can_shrink() { 0 } else { len };
        Size::new(
            min(self.width, self.horizontal),
            min(self.height, self.vertical),
        )
    }

    /// `QSpacerItem::maximumSize`: unbounded where the policy may grow (qlayoutitem.cpp:628-631).
    pub fn maximum_size(&self) -> Size {
        let max = |len: i32, p: Policy| if p.can_grow() { LAYOUT_SIZE_MAX } else { len };
        Size::new(
            max(self.width, self.horizontal),
            max(self.height, self.vertical),
        )
    }

    /// `QSpacerItem::expandingDirections`: the policies carrying `ExpandFlag`
    /// (qlayoutitem.cpp:570-572; `Ignored` has none, qsizepolicy.h).
    pub fn expanding_directions(&self) -> (bool, bool) {
        let exp = |p: Policy| matches!(p, Policy::Expanding | Policy::MinimumExpanding);
        (exp(self.horizontal), exp(self.vertical))
    }
}

pub struct LayoutItem {
    pub widget: WidgetRef,
    pub stretch: u32,
    /// `QLayoutItem::alignment`.
    pub alignment: Alignment,
    /// A `QSpacerItem`: it has no content of its own, so it takes no spacing. `widget` is then a
    /// placeholder that only records the spacer's rectangle.
    pub spacer: Option<SpacerItem>,
    /// The item holds a child layout (`QLayout` as a `QLayoutItem`) rather than a widget: its
    /// limits come from that layout, not from a widget's own size policy.
    pub nested: bool,
}

pub trait Layout: 'static {
    fn geometry(&self) -> Rect;

    fn set_geometry(&mut self, rect: Rect);

    fn add_widget(&mut self, widget: WidgetRef);

    /// `QLayout::setAlignment(QWidget*, Qt::Alignment)`: sets the alignment of the item that holds
    /// `widget` (a direct child item only) and re-lays the layout out. Returns false when the
    /// layout has no such item.
    fn set_alignment(&mut self, widget: &WidgetRef, alignment: Alignment) -> bool;

    fn widgets(&self) -> Vec<WidgetRef> {
        Vec::new()
    }

    /// The concrete layout, to reach what only it has (`QBoxLayout::insertWidget`, ...) once it
    /// sits behind `Box<dyn Layout>`; see [`dyn Widget::with_layout_as`](crate::widget::Widget).
    fn as_any(&self) -> &dyn std::any::Any;

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;

    /// `QLayout::count`: the number of items (widgets, spacers and child layouts).
    fn count(&self) -> usize {
        self.widgets().len()
    }

    /// `QLayout::itemAt`: the widget of item `index`. A spacer or child layout item gives its
    /// placeholder widget; `None` past the end.
    fn item_at(&self, index: usize) -> Option<WidgetRef> {
        self.widgets().into_iter().nth(index)
    }

    /// `QLayout::takeAt`: removes item `index` from the layout and returns its widget (the
    /// widget is neither hidden nor destroyed). `None` past the end, or when the layout cannot
    /// remove items.
    fn take_at(&mut self, _index: usize) -> Option<WidgetRef> {
        None
    }

    fn set_margins(&mut self, margins: Margins);

    fn margins(&self) -> Margins;

    /// The margin an owner gives a layout with no explicit margin (`QLayoutPrivate::getMargin`,
    /// qlayout.cpp:117-129): 11 for a window, 9 for a child widget, 0 for a nested layout. Ignored
    /// once `set_margins` has set an explicit margin. Layouts without margins ignore it.
    fn set_default_margin(&mut self, _margin: i32) {}
    /// Marks this layout as nested in another layout: it has no owner widget, so its margin is 0
    /// (`QLayoutPrivate::getMargin`, qlayout.cpp:117-129) and later owner defaults are ignored.
    /// An explicit margin still wins.
    fn set_nested(&mut self) {}

    /// `!QLayoutPrivate::topLevel`: `set_nested` ran, so this layout is an item of another layout
    /// and its holder is not an owner its size constraint applies to (qlayout.cpp:985-986).
    fn is_nested(&self) -> bool;

    fn set_spacing(&mut self, spacing: i32);

    fn spacing(&self) -> i32;

    /// `QLayout::alignment()` (the `QLayoutItem` alignment of a layout): NONE unless the layout
    /// keeps one (`qlayout.cpp:213-227`, `qlayoutitem.cpp:151-153`).
    fn alignment(&self) -> Alignment {
        Alignment::NONE
    }

    /// Stores the layout's own alignment and invalidates it (`QLayout::setAlignment(QLayout*)`,
    /// qlayout.cpp:213-227). Layouts without an alignment ignore it.
    fn set_layout_alignment(&mut self, _alignment: Alignment) {}

    /// The value `qSmartSpacing` gives a layout whose own spacing is unset (qlayoutengine.cpp:390-401):
    /// the style's layout spacing for a widget parent, the parent layout's `spacing()` for a nested
    /// layout, -1 when parentless. Layouts without spacing ignore it.
    fn set_smart_spacing(&mut self, _spacing: i32) {}

    fn size_hint(&self) -> Size;

    /// The smallest size the layout can be squeezed to (`QLayout::minimumSize`).
    fn minimum_size(&self) -> Size;

    /// `QLayout::maximumSize` (qlayout.cpp:912-914): unbounded unless the layout overrides it.
    fn maximum_size(&self) -> Size {
        Size::new(LAYOUT_SIZE_MAX, LAYOUT_SIZE_MAX)
    }

    /// The directions the layout wants to grow in, as (horizontal, vertical)
    /// (`QLayout::expandingDirections`).
    fn expanding_directions(&self) -> (bool, bool);

    /// Whether the layout's height depends on its width (`QLayout::hasHeightForWidth`).
    fn has_height_for_width(&self) -> bool {
        false
    }

    /// `QLayout::isEmpty` (qlayout.cpp:424-435): every item is empty. Only box and grid layouts hold
    /// items here; other layouts report `false`.
    fn is_empty(&self) -> bool {
        false
    }

    /// The height the layout wants at `width` (`QLayout::heightForWidth`). `-1` when it has no
    /// height-for-width (`qlayout.cpp`: the default returns -1).
    fn height_for_width(&self, _width: i32) -> i32 {
        -1
    }
    /// The minimum height the layout needs for the given width (`QLayout::minimumHeightForWidth`,
    /// `qlayoutitem.cpp:355-358`). The default implementation returns `height_for_width(width)`.
    fn minimum_height_for_width(&self, width: i32) -> i32 {
        self.height_for_width(width)
    }

    /// Marks this layout as dirty/invalid, requiring recalculation on next activation.
    fn invalidate(&mut self);

    /// Checks if this layout is currently dirty.
    fn is_dirty(&self) -> bool {
        true
    }

    /// Recalculates and positions child items if the layout is dirty.
    fn activate(&mut self);

    /// Convenience and compatibility method: invalidates and activates this layout.
    fn update_layout(&mut self) {
        self.invalidate();
        self.activate();
    }

    /// `QLayout::sizeConstraint` (qlayout.cpp:1345-1349).
    fn size_constraint(&self) -> SizeConstraint;

    /// `QLayout::setSizeConstraint` (qlayout.cpp:1323-1343): both axes; a change invalidates the
    /// layout. It takes effect when the owner activates this layout as its top-level layout.
    fn set_size_constraint(&mut self, constraint: SizeConstraint);
}

/// Linear box layout arranging items horizontally or vertically (`QBoxLayout`).
pub struct BoxLayout {
    direction: Direction,
    geometry: Rect,
    margins: Margins,
    /// True once `set_margins` ran; an owner's default margin no longer applies (qlayout.cpp:117-129).
    margins_explicit: bool,
    /// `QBoxLayoutPrivate::spacing`: the explicit spacing, -1 when unset (qboxlayout.cpp:580-585).
    spacing: i32,
    /// The `qSmartSpacing` value used while the explicit spacing is unset (qlayoutengine.cpp:390-401).
    smart_spacing: i32,
    /// `QLayoutItem::align` of this layout (`QLayout::setAlignment`, qlayout.cpp:213-227).
    alignment: Alignment,
    items: Vec<LayoutItem>,
    metric_dirty: Cell<bool>,
    needs_activation: Cell<bool>,
    geom_cache: RefCell<Option<BoxGeom>>,
    /// `QLayoutPrivate::horizontalConstraint` / `verticalConstraint` (both axes alike).
    size_constraint: SizeConstraint,
    /// An item of another layout (`set_nested`).
    nested: bool,
}

impl BoxLayout {
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            geometry: Rect::new(0, 0, 0, 0),
            margins: Margins::new(0, 0, 0, 0),
            margins_explicit: false,
            spacing: -1,
            smart_spacing: -1,
            alignment: Alignment::NONE,
            items: Vec::new(),
            metric_dirty: Cell::new(true),
            needs_activation: Cell::new(true),
            geom_cache: RefCell::new(None),
            size_constraint: SizeConstraint::SetDefaultConstraint,
            nested: false,
        }
    }

    pub fn horizontal() -> Self {
        Self::new(Direction::LeftToRight)
    }

    pub fn vertical() -> Self {
        Self::new(Direction::TopToBottom)
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    pub fn set_direction(&mut self, direction: Direction) {
        self.direction = direction;
        self.update_layout();
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    /// `QBoxLayoutPrivate::validateIndex`: an index past the end appends.
    fn insert_item(&mut self, index: usize, item: LayoutItem) {
        let clamped = index.min(self.items.len());
        self.items.insert(clamped, item);
        self.update_layout();
    }

    pub fn insert_widget(&mut self, index: usize, widget: WidgetRef, stretch: u32) {
        self.insert_item(
            index,
            LayoutItem {
                widget,
                stretch,
                alignment: Alignment::NONE,
                spacer: None,
                nested: false,
            },
        );
    }

    /// `QBoxLayout::addWidget(widget, stretch, alignment)`.
    pub fn add_widget_aligned(&mut self, widget: WidgetRef, stretch: u32, alignment: Alignment) {
        self.insert_item(
            usize::MAX,
            LayoutItem {
                widget,
                stretch,
                alignment,
                spacer: None,
                nested: false,
            },
        );
    }

    pub fn remove_widget(&mut self, index: usize) -> Option<WidgetRef> {
        if index < self.items.len() {
            let item = self.items.remove(index);
            self.update_layout();
            Some(item.widget)
        } else {
            None
        }
    }

    /// `QBoxLayout::addStretch`: an empty, expanding spacer that takes no spacing.
    pub fn add_stretch(&mut self, stretch: u32) {
        self.insert_stretch(usize::MAX, stretch);
    }

    /// `QBoxLayout::addWidget(widget, stretch)`: alignment NONE.
    pub fn add_widget_with_stretch(&mut self, widget: WidgetRef, stretch: u32) {
        self.add_widget_aligned(widget, stretch, Alignment::NONE);
    }

    /// `QBoxLayout::addLayout(layout, stretch)`: the child layout is one item of this layout,
    /// sized by the child's own `maximumSize`/`minimumSize`/`sizeHint`. The holder widget only
    /// carries the child; it has no geometry or size policy of its own. Returns the holder item,
    /// which `Layout::set_alignment` takes to align the child layout.
    pub fn add_layout(&mut self, layout: Box<dyn Layout>, stretch: u32) -> WidgetRef {
        let mut holder = EmptyWidget::with_geometry(Rect::new(0, 0, 0, 0));
        holder.set_layout(layout);
        // `qSmartSpacing` of a nested layout is its parent layout's `spacing()` (qlayoutengine.cpp:390-401).
        let spacing = self.spacing_value();
        if let Some(mut child) = holder.layout_ref_mut() {
            // A nested layout has no owner widget: its margin is 0, not the owner default
            // (`QLayoutPrivate::getMargin`, qlayout.cpp:117-129).
            // Owner defaults no longer reach a nested layout (`set_nested`).
            child.set_nested();
            child.set_smart_spacing(spacing);
        }
        let widget: WidgetRef = std::rc::Rc::new(std::cell::RefCell::new(Box::new(holder)));
        self.insert_item(
            usize::MAX,
            LayoutItem {
                widget: widget.clone(),
                stretch,
                alignment: Alignment::NONE,
                spacer: None,
                nested: true,
            },
        );
        widget
    }

    /// `QBoxLayout::spacing()` (qboxlayout.cpp:562-571): the explicit spacing, or the smart spacing
    /// while the explicit one is unset (-1).
    fn spacing_value(&self) -> i32 {
        if self.spacing >= 0 {
            self.spacing
        } else {
            self.smart_spacing
        }
    }

    /// The gap `setupGeom` puts between two non-empty items (qboxlayout.cpp:243-296): the spacing,
    /// or none where no spacing is known (a parentless layout).
    fn gap(&self) -> i32 {
        self.spacing_value().max(0)
    }

    /// Hands this layout's spacing to every nested child layout (`qSmartSpacing`, qlayoutengine.cpp:390-401).
    fn propagate_smart_spacing(&self) {
        let spacing = self.spacing_value();
        for item in self.items.iter().filter(|item| item.nested) {
            let holder = item.widget.borrow();
            let child = holder.layout_ref_mut();
            if let Some(mut child) = child {
                child.set_smart_spacing(spacing);
            }
        }
    }

    /// The rectangle the content is laid out in: `QBoxLayout::setGeometry` (qboxlayout.cpp:743)
    /// places an aligned layout inside `QLayout::alignmentRect` of its geometry.
    fn placed_rect(&self) -> Rect {
        if self.alignment == Alignment::NONE {
            return self.geometry;
        }
        let (hint, max, expanding, has_hfw) = {
            self.ensure_geom();
            let cache = self.geom_cache.borrow();
            let geom = cache.as_ref().unwrap();
            (geom.hint, geom.max, geom.expanding, geom.has_hfw)
        };
        let hfw = |width: i32| self.height_for_width(width);
        alignment_rect(
            self.geometry,
            self.alignment,
            hint,
            max,
            expanding,
            if has_hfw { Some(&hfw) } else { None },
        )
    }

    /// `QBoxLayout::insertSpacerItem`: the spacer goes in with stretch 0 (qboxlayout.cpp:894-902).
    pub fn insert_spacer_item(&mut self, index: usize, spacer: SpacerItem) {
        self.insert_spacer(index, spacer, 0);
    }

    /// `QBoxLayout::addSpacerItem`.
    pub fn add_spacer_item(&mut self, spacer: SpacerItem) {
        self.insert_spacer_item(usize::MAX, spacer);
    }

    /// `QBoxLayout::insertSpacing`: a `Fixed` spacer of `size` along the layout, `Minimum` across
    /// it (qboxlayout.cpp:844-858).
    pub fn insert_spacing(&mut self, index: usize, size: i32) {
        let spacer = if self.direction == Direction::LeftToRight {
            SpacerItem::new(size, 0, Policy::Fixed, Policy::Minimum)
        } else {
            SpacerItem::new(0, size, Policy::Minimum, Policy::Fixed)
        };
        self.insert_spacer(index, spacer, 0);
    }

    /// `QBoxLayout::addSpacing`.
    pub fn add_spacing(&mut self, size: i32) {
        self.insert_spacing(usize::MAX, size);
    }

    /// `QBoxLayout::insertStretch`: a 0x0 spacer, `Expanding` along the layout and `Minimum`
    /// across it, that keeps `stretch` as given (qboxlayout.cpp:867-881).
    pub fn insert_stretch(&mut self, index: usize, stretch: u32) {
        let spacer = if self.direction == Direction::LeftToRight {
            SpacerItem::new(0, 0, Policy::Expanding, Policy::Minimum)
        } else {
            SpacerItem::new(0, 0, Policy::Minimum, Policy::Expanding)
        };
        self.insert_spacer(index, spacer, stretch);
    }

    fn insert_spacer(&mut self, index: usize, spacer: SpacerItem, stretch: u32) {
        let widget = crate::widget::EmptyWidget::with_geometry(Rect::new(0, 0, 0, 0));
        let widget: WidgetRef = std::rc::Rc::new(std::cell::RefCell::new(Box::new(widget)));
        self.insert_item(
            index,
            LayoutItem {
                widget,
                stretch,
                alignment: Alignment::NONE,
                spacer: Some(spacer),
                nested: false,
            },
        );
    }

    /// `QBoxLayout::setStretchFactor(QWidget*, int)`: sets the stretch of the item holding
    /// `widget` (a direct item only); false when there is none (qboxlayout.cpp:1069-1083).
    pub fn set_stretch_factor(&mut self, widget: &WidgetRef, stretch: u32) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| item.spacer.is_none() && std::rc::Rc::ptr_eq(&item.widget, widget))
        else {
            return false;
        };
        item.stretch = stretch;
        self.update_layout();
        true
    }

    /// `QBoxLayout::setStretch`: out-of-range indexes are ignored (qboxlayout.cpp:1114-1124).
    pub fn set_stretch(&mut self, index: usize, stretch: u32) {
        if let Some(item) = self.items.get_mut(index) {
            if item.stretch != stretch {
                item.stretch = stretch;
                self.update_layout();
            }
        }
    }

    /// `QBoxLayout::stretch`: the stored factor, not the size policy's; `None` out of range
    /// (Qt returns -1, qboxlayout.cpp:1132-1138).
    pub fn stretch(&self, index: usize) -> Option<u32> {
        self.items.get(index).map(|item| item.stretch)
    }
}

impl Layout for BoxLayout {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn count(&self) -> usize {
        self.items.len()
    }

    fn take_at(&mut self, index: usize) -> Option<WidgetRef> {
        self.remove_widget(index)
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        if self.geometry == rect && !self.needs_activation.get() {
            return;
        }
        self.geometry = rect;
        self.needs_activation.set(true);
        self.activate();
    }

    fn add_widget(&mut self, widget: WidgetRef) {
        self.add_widget_with_stretch(widget, 0);
    }

    fn set_alignment(&mut self, widget: &WidgetRef, alignment: Alignment) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| item.spacer.is_none() && std::rc::Rc::ptr_eq(&item.widget, widget))
        else {
            return false;
        };
        if item.nested {
            // `QLayout::setAlignment(QLayout*)` stores the alignment on the child layout (qlayout.cpp:213-227).
            let holder = item.widget.borrow();
            let child = holder.layout_ref_mut();
            if let Some(mut child) = child {
                child.set_layout_alignment(alignment);
            }
        } else {
            item.alignment = alignment;
        }
        self.update_layout();
        true
    }

    fn widgets(&self) -> Vec<WidgetRef> {
        self.items.iter().map(|it| it.widget.clone()).collect()
    }

    fn set_margins(&mut self, margins: Margins) {
        self.margins = margins;
        self.margins_explicit = true;
        self.update_layout();
    }

    fn set_default_margin(&mut self, margin: i32) {
        if self.margins_explicit {
            return;
        }
        let margins = Margins::new(margin, margin, margin, margin);
        if self.margins != margins {
            self.margins = margins;
            self.update_layout();
        }
    }

    fn set_nested(&mut self) {
        self.set_default_margin(0);
        self.margins_explicit = true;
        self.nested = true;
    }

    fn is_nested(&self) -> bool {
        self.nested
    }

    fn margins(&self) -> Margins {
        self.margins
    }

    fn set_spacing(&mut self, spacing: i32) {
        self.spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    fn spacing(&self) -> i32 {
        self.spacing_value()
    }

    fn alignment(&self) -> Alignment {
        self.alignment
    }

    fn set_layout_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
        self.invalidate();
    }

    fn set_smart_spacing(&mut self, spacing: i32) {
        self.smart_spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    fn size_hint(&self) -> Size {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().hint
    }

    fn minimum_size(&self) -> Size {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().min
    }

    /// `QBoxLayout::maximumSize` (qboxlayout.cpp:612-625): an aligned axis is unbounded.
    fn maximum_size(&self) -> Size {
        self.ensure_geom();
        let mut max = self.geom_cache.borrow().as_ref().unwrap().max;
        if self.alignment.horizontal() {
            max.width = LAYOUT_SIZE_MAX;
        }
        if self.alignment.vertical() {
            max.height = LAYOUT_SIZE_MAX;
        }
        max
    }

    fn expanding_directions(&self) -> (bool, bool) {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().expanding
    }

    /// `QBoxLayout::hasHeightForWidth` (qboxlayout.cpp:635-640).
    fn has_height_for_width(&self) -> bool {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().has_hfw
    }

    fn is_empty(&self) -> bool {
        self.items
            .iter()
            .all(|item| item.spacer.is_some() || entry_is_empty(&item.widget, item.nested))
    }

    /// `QBoxLayout::heightForWidth` (qboxlayout.cpp:641-652): the width less the margins, then the
    /// content height plus the margins.
    fn height_for_width(&self, width: i32) -> i32 {
        self.ensure_geom();
        let cache = self.geom_cache.borrow();
        let geom = cache.as_ref().unwrap();
        if !geom.has_hfw {
            return -1;
        }
        let m = self.margins;
        self.calc_hfw(geom, width - m.left - m.right) + m.top + m.bottom
    }

    fn invalidate(&mut self) {
        self.metric_dirty.set(true);
        self.needs_activation.set(true);
        *self.geom_cache.borrow_mut() = None;
    }

    fn is_dirty(&self) -> bool {
        self.needs_activation.get()
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

    /// `QBoxLayout::setGeometry`.
    fn activate(&mut self) {
        if !self.needs_activation.get() {
            return;
        }
        if items_busy(self.items.iter().map(|item| &item.widget)) {
            crate::layout_scheduler::LayoutScheduler::note_deferred();
            return;
        }
        self.needs_activation.set(false);
        if self.items.is_empty() {
            return;
        }

        self.ensure_geom();
        // `qGeomCalc` writes positions into the chain, so the activation works on a copy; the
        // cached chain stays the metric input for the next geometry pass.
        let mut chain = self.geom_cache.borrow().as_ref().unwrap().chain.clone();
        let horz = self.direction == Direction::LeftToRight;
        let rtl = horz && GuiApplication::is_right_to_left();
        let placed = self.placed_rect();
        let s = Rect::new(
            placed.x + self.margins.left,
            placed.y + self.margins.top,
            placed.width - self.margins.left - self.margins.right,
            placed.height - self.margins.top - self.margins.bottom,
        );
        let (pos, space) = if horz {
            (s.x, s.width)
        } else {
            (s.y, s.height)
        };
        let n = chain.len();
        // qboxlayout.cpp:752-766: a vertical box gives each height-for-width item the height it
        // wants at the width it gets, as both its size hint and its minimum.
        if !horz && self.geom_cache.borrow().as_ref().unwrap().has_hfw {
            for (i, item) in self.items.iter().enumerate() {
                if item.spacer.is_some() {
                    continue;
                }
                let w = item.widget.borrow();
                if !item_has_height_for_width(&**w) {
                    continue;
                }
                let min_w = item_minimum_size(&**w).width;
                let limits = self.geom_cache.borrow().as_ref().unwrap().limits[i];
                let width = s.width.min(limits.item_max.width).max(min_w);
                let height = item_height_for_width(&**w, width);
                chain[i].size_hint = height;
                chain[i].minimum_size = height;
            }
        }
        q_geom_calc(&mut chain, 0, n, pos, space, -1);

        for (i, (item, data)) in self.items.iter().zip(&chain).enumerate() {
            let rect = if horz {
                // `QBoxLayout::setGeometry` mirrors a horizontal box under RTL (qboxlayout.cpp:788-790).
                let pos = if rtl {
                    2 * s.x + s.width - data.pos - data.size
                } else {
                    data.pos
                };
                Rect::new(pos, s.y, data.size, s.height)
            } else {
                Rect::new(s.x, data.pos, s.width, data.size)
            };
            let old_size = {
                let w = item.widget.borrow();
                let g = w.geometry();
                Size::new(g.width, g.height)
            };
            if item.spacer.is_some() {
                // A `QSpacerItem` just remembers its rectangle.
                item.widget.borrow().set_geometry(rect);
                relayout_child_after_resize(&item.widget, old_size);
            } else if item.nested {
                // `QBoxLayout::setGeometry` hands a nested layout its whole slot and clamps nothing;
                // the child applies its own alignment (qboxlayout.cpp:743).
                item.widget.borrow().set_geometry(rect);
                set_nested_layout_geometry(&item.widget);
            } else {
                let limits = self.geom_cache.borrow().as_ref().unwrap().limits[i];
                item_set_geometry_with(&**item.widget.borrow(), rect, item.alignment, limits);
                relayout_child_after_resize(&item.widget, old_size);
            }
        }
    }
}

/// What `QBoxLayoutPrivate::setupGeom` computes.
#[derive(Clone)]
struct BoxGeom {
    chain: Vec<LayoutStruct>,
    /// Per item, what `item_set_geometry` bounds the item by (unused for spacers).
    limits: Vec<ItemLimits>,
    min: Size,
    max: Size,
    hint: Size,
    expanding: (bool, bool),
    /// Some item has height-for-width (`QBoxLayoutPrivate::hasHfw`).
    has_hfw: bool,
}

/// `QBoxLayoutItem::hfw` (qboxlayout.cpp:22-28): the item's height at `width` when it has
/// height-for-width, else its size-hint height.
fn box_item_hfw(item: &LayoutItem, width: i32) -> i32 {
    match item.spacer {
        Some(spacer) => spacer.size_hint().height,
        None => {
            let w = item.widget.borrow();
            if item_has_height_for_width(&**w) {
                item_height_for_width(&**w, width)
            } else {
                item_size_hint(&**w).height
            }
        }
    }
}

/// Gives a nested layout the rectangle its holder was placed at. The nested items are children of
/// the holder, so the child layout works in holder coordinates: the holder's size at origin (0,0),
/// the same rectangle `Widget::update_layout` gives it.
fn set_nested_layout_geometry(holder: &WidgetRef) {
    let w = holder.borrow();
    let rect = w.geometry();
    let child = w.layout_ref_mut();
    if let Some(mut child) = child {
        child.set_geometry(Rect::new(0, 0, rect.width, rect.height));
    }
}

/// `QLayoutItem::isEmpty` for a box or grid entry: a nested layout is empty when every item in it
/// is (qlayout.cpp:424-435); a widget when hidden (qlayoutitem.cpp:691-694).
fn entry_is_empty(widget: &WidgetRef, nested: bool) -> bool {
    if nested {
        widget
            .borrow()
            .layout_ref_mut()
            .is_none_or(|inner| inner.is_empty())
    } else {
        item_is_empty(&**widget.borrow())
    }
}

impl BoxLayout {
    fn ensure_geom(&self) {
        if !self.metric_dirty.get() && self.geom_cache.borrow().is_some() {
            return;
        }
        let geom = self.setup_geom();
        *self.geom_cache.borrow_mut() = Some(geom);
        self.metric_dirty.set(false);
    }

    /// `QBoxLayoutPrivate::calcHfw` (qboxlayout.cpp:366-395): the content height at `width`, margins
    /// excluded. A horizontal box lays its items out at `width` and takes the tallest; a vertical
    /// box stacks them with their spacing.
    fn calc_hfw(&self, geom: &BoxGeom, width: i32) -> i32 {
        if self.direction == Direction::LeftToRight {
            let mut chain = geom.chain.clone();
            let n = chain.len();
            q_geom_calc(&mut chain, 0, n, 0, width, -1);
            self.items
                .iter()
                .zip(&chain)
                .map(|(item, data)| box_item_hfw(item, data.size))
                .fold(0, i32::max)
        } else {
            self.items
                .iter()
                .zip(&geom.chain)
                .map(|(item, data)| box_item_hfw(item, width) + data.spacing)
                .sum()
        }
    }

    /// `QBoxLayoutPrivate::setupGeom`: the chain handed to `qGeomCalc` and the layout's sizes.
    fn setup_geom(&self) -> BoxGeom {
        let horz = self.direction == Direction::LeftToRight;
        // Along the layout the maximum sizes add up; across it they are folded by `qMaxExpCalc`.
        let mut max_main: i64 = 0;
        let mut max_cross: i32 = LAYOUT_SIZE_MAX;
        let (mut min_main, mut hint_main) = (0i32, 0i32);
        let (mut min_cross, mut hint_cross) = (0i32, 0i32);
        let (mut main_exp, mut cross_exp) = (false, false);

        let mut chain = vec![LayoutStruct::default(); self.items.len()];
        let mut limits = Vec::with_capacity(self.items.len());
        let mut previous_non_empty: Option<usize> = None;
        let mut has_hfw = false;

        for (i, item) in self.items.iter().enumerate() {
            // Everything below is (main axis, cross axis).
            let (max, min, hint, exp, empty, is_widget, policy_stretch, item_limits) =
                if let Some(spacer) = item.spacer {
                    // Everything a `QSpacerItem` reports, transposed for a vertical layout.
                    let swap = |s: Size| {
                        if horz {
                            (s.width, s.height)
                        } else {
                            (s.height, s.width)
                        }
                    };
                    let exp = spacer.expanding_directions();
                    (
                        swap(spacer.maximum_size()),
                        swap(spacer.minimum_size()),
                        swap(spacer.size_hint()),
                        if horz { exp } else { (exp.1, exp.0) },
                        true,
                        false,
                        0,
                        ItemLimits {
                            item_max: spacer.maximum_size(),
                            widget_min: Size::new(0, 0),
                            widget_max: spacer.maximum_size(),
                            aligned_pref: Size::new(0, 0),
                        },
                    )
                } else {
                    let w = item.widget.borrow();
                    let policy = w.size_policy();
                    let swap = |s: Size| {
                        if horz {
                            (s.width, s.height)
                        } else {
                            (s.height, s.width)
                        }
                    };
                    let exp = item_expanding(&**w, item.alignment);
                    let mut item_limits = ItemLimits::of(&**w, item.alignment);
                    if item.nested {
                        // `QLayoutItem::maximumSize` of a `QLayout` is that layout's own maximumSize.
                        if let Some(inner) = w.layout_ref_mut() {
                            item_limits.item_max = inner.maximum_size();
                        }
                    }
                    (
                        swap(item_limits.item_max),
                        swap(item_minimum_size(&**w)),
                        swap(item_size_hint(&**w)),
                        if horz { exp } else { (exp.1, exp.0) },
                        entry_is_empty(&item.widget, item.nested),
                        true,
                        (if horz {
                            policy.horizontal_stretch
                        } else {
                            policy.vertical_stretch
                        }) as i32,
                        item_limits,
                    )
                };

            let mut spacing = 0;
            if !empty {
                spacing = if previous_non_empty.is_some() {
                    self.gap()
                } else {
                    0
                };
                if let Some(previous) = previous_non_empty {
                    chain[previous].spacing = spacing;
                }
                previous_non_empty = Some(i);
            }

            let expand = exp.0 || item.stretch > 0;
            main_exp = main_exp || expand;
            max_main += (spacing + max.0) as i64;
            min_main += spacing + min.0;
            hint_main += spacing + hint.0;
            if !(empty && is_widget) {
                // hidden widgets are ignored
                let mut dummy = true;
                q_max_exp_calc(
                    &mut max_cross,
                    &mut cross_exp,
                    &mut dummy,
                    max.1,
                    exp.1,
                    empty,
                );
            }
            min_cross = min_cross.max(min.1);
            hint_cross = hint_cross.max(hint.1);

            chain[i].size_hint = hint.0;
            chain[i].maximum_size = max.0;
            chain[i].minimum_size = min.0;
            chain[i].expansive = expand;
            chain[i].stretch = if item.stretch > 0 {
                item.stretch as i32
            } else {
                policy_stretch
            };
            chain[i].empty = empty;
            limits.push(item_limits);
            chain[i].spacing = 0; // may be set non-zero by a later non-empty item
            if item.spacer.is_none() {
                has_hfw = has_hfw || item_has_height_for_width(&**item.widget.borrow());
            }
        }

        let extra = Size::new(
            self.margins.left + self.margins.right,
            self.margins.top + self.margins.bottom,
        );
        let (min_w, min_h) = if horz {
            (min_main, min_cross)
        } else {
            (min_cross, min_main)
        };
        let (hint_w, hint_h) = if horz {
            (hint_main, hint_cross)
        } else {
            (hint_cross, hint_main)
        };
        let (max_w, max_h) = if horz {
            (max_main, max_cross as i64)
        } else {
            (max_cross as i64, max_main)
        };
        // `maxSize = QSize(maxw, maxh).expandedTo(minSize)`; `sizeHint` is bounded by both.
        let bounded = |hint: i32, min: i32, max: i64| {
            hint.max(min)
                .min(max.max(min as i64).min(i32::MAX as i64) as i32)
        };
        // `maxSize = QSize(maxw, maxh).expandedTo(minSize)`, then the margins, bounded to
        // `QLAYOUTSIZE_MAX` (qboxlayout.cpp:347-357, 612-621).
        let max_bounded = |max: i64, min: i32, extra: i32| {
            (max.max(min as i64) + extra as i64).min(LAYOUT_SIZE_MAX as i64) as i32
        };
        BoxGeom {
            chain,
            limits,
            max: Size::new(
                max_bounded(max_w, min_w, extra.width),
                max_bounded(max_h, min_h, extra.height),
            ),
            min: Size::new(min_w + extra.width, min_h + extra.height),
            hint: Size::new(
                bounded(hint_w, min_w, max_w) + extra.width,
                bounded(hint_h, min_h, max_h) + extra.height,
            ),
            expanding: if horz {
                (main_exp, cross_exp)
            } else {
                (cross_exp, main_exp)
            },
            has_hfw,
        }
    }
}

/// Item inside a GridLayout spanning a grid area (`QGridLayout`).
pub struct GridItem {
    pub widget: WidgetRef,
    pub row: usize,
    pub column: usize,
    pub row_span: usize,
    pub col_span: usize,
    /// `QLayoutItem::alignment`.
    pub alignment: Alignment,
    /// The item is a nested layout (`QGridLayout::addLayout`): `widget` holds the child layout.
    pub nested: bool,
}

#[derive(Clone)]
struct GridGeom {
    rows: Vec<LayoutStruct>,
    cols: Vec<LayoutStruct>,
    limits: Vec<ItemLimits>,
    hint: Size,
    min: Size,
    expanding: (bool, bool),
    max: Size,
    /// Some item has height-for-width (`QGridLayoutPrivate::has_hfw`).
    has_hfw: bool,
}

/// Grid layout laying out widgets in a 2D grid of rows and columns (`QGridLayout`).
///
/// `QGridLayout` has no `addStretch`: the box-only API is not reachable from a grid, so a
/// stretch request cannot silently append a row (qgridlayout.cpp:1383-1397 takes no stretch).
///
/// ```compile_fail
/// use ferrokit_widgets::{GridLayout, Layout};
/// let mut grid = GridLayout::new();
/// grid.add_stretch(1);
/// ```
pub struct GridLayout {
    geometry: Rect,
    margins: Margins,
    /// True once `set_margins` ran; an owner's default margin no longer applies (qlayout.cpp:117-129).
    margins_explicit: bool,
    /// `QGridLayoutPrivate::horizontal_spacing`: the explicit spacing, -1 while unset.
    h_spacing: i32,
    /// `QGridLayoutPrivate::vertical_spacing`: the explicit spacing, -1 while unset.
    v_spacing: i32,
    /// Smart spacing from the parent layout or widget (`qSmartSpacing`, qlayoutengine.cpp:390-401).
    smart_spacing: i32,
    /// `QLayout::alignment`: set on this layout by its parent (`QLayoutItem::alignment`).
    alignment: Alignment,
    items: Vec<GridItem>,
    row_stretches: Vec<u32>,
    col_stretches: Vec<u32>,
    col_min_widths: Vec<i32>,
    row_min_heights: Vec<i32>,
    metric_dirty: Cell<bool>,
    needs_activation: Cell<bool>,
    geom_cache: RefCell<Option<GridGeom>>,
    /// `QLayoutPrivate::horizontalConstraint` / `verticalConstraint` (both axes alike).
    size_constraint: SizeConstraint,
    /// An item of another layout (`set_nested`).
    nested: bool,
}
impl GridLayout {
    pub fn new() -> Self {
        Self {
            geometry: Rect::new(0, 0, 0, 0),
            margins: Margins::new(0, 0, 0, 0),
            margins_explicit: false,
            h_spacing: -1,
            v_spacing: -1,
            smart_spacing: -1,
            alignment: Alignment::NONE,
            items: Vec::new(),
            row_stretches: Vec::new(),
            col_stretches: Vec::new(),
            col_min_widths: Vec::new(),
            row_min_heights: Vec::new(),
            metric_dirty: Cell::new(true),
            needs_activation: Cell::new(true),
            geom_cache: RefCell::new(None),
            size_constraint: SizeConstraint::SetDefaultConstraint,
            nested: false,
        }
    }
    pub fn add_widget(&mut self, widget: WidgetRef, row: usize, column: usize) {
        self.add_widget_with_span(widget, row, column, 1, 1);
    }

    pub fn add_widget_with_span(
        &mut self,
        widget: WidgetRef,
        row: usize,
        column: usize,
        row_span: usize,
        col_span: usize,
    ) {
        self.add_widget_aligned(widget, row, column, row_span, col_span, Alignment::NONE);
    }

    /// `QGridLayout::rowStretch` (qgridlayout.cpp:1506-1510). Rows never set read 0; Qt's
    /// `QVector::at` would assert past the end instead.
    pub fn row_stretch(&self, row: usize) -> u32 {
        self.row_stretches.get(row).copied().unwrap_or(0)
    }

    /// `QGridLayout::columnStretch` (qgridlayout.cpp:1517-1520).
    pub fn column_stretch(&self, col: usize) -> u32 {
        self.col_stretches.get(col).copied().unwrap_or(0)
    }

    /// `QGridLayout::rowMinimumHeight` (qgridlayout.cpp:1566-1569).
    pub fn row_minimum_height(&self, row: usize) -> i32 {
        self.row_min_heights.get(row).copied().unwrap_or(0)
    }

    /// `QGridLayout::columnMinimumWidth` (qgridlayout.cpp:1589-1593).
    pub fn column_minimum_width(&self, col: usize) -> i32 {
        self.col_min_widths.get(col).copied().unwrap_or(0)
    }

    /// `QGridLayout::horizontalSpacing` (qgridlayout.cpp:1093-1101): the explicit spacing, or the
    /// smart spacing while none is set.
    pub fn horizontal_spacing(&self) -> i32 {
        if self.h_spacing >= 0 {
            self.h_spacing
        } else {
            self.smart_spacing
        }
    }

    /// `QGridLayout::verticalSpacing` (qgridlayout.cpp:1121-1129).
    pub fn vertical_spacing(&self) -> i32 {
        if self.v_spacing >= 0 {
            self.v_spacing
        } else {
            self.smart_spacing
        }
    }

    /// `QGridLayout::addWidget(widget, row, column, rowSpan, columnSpan, alignment)`.
    pub fn add_widget_aligned(
        &mut self,
        widget: WidgetRef,
        row: usize,
        column: usize,
        row_span: usize,
        col_span: usize,
        alignment: Alignment,
    ) {
        self.items.push(GridItem {
            widget,
            row,
            column,
            row_span: row_span.max(1),
            col_span: col_span.max(1),
            alignment,
            nested: false,
        });
        self.update_layout();
    }

    /// `QGridLayout::addLayout(layout, row, column, rowSpan, columnSpan, alignment)`: the child
    /// layout takes the cell span as one item. `alignment` is set on the child layout, as
    /// `QGridLayout::addLayout` does (`QLayout::setAlignment`, qlayout.cpp:213-227). Returns the
    /// holder widget, which identifies the item for `set_alignment` and `remove_widget`.
    pub fn add_layout(
        &mut self,
        layout: Box<dyn Layout>,
        row: usize,
        column: usize,
        row_span: usize,
        col_span: usize,
        alignment: Alignment,
    ) -> WidgetRef {
        let mut holder = EmptyWidget::with_geometry(Rect::new(0, 0, 0, 0));
        holder.set_layout(layout);
        let spacing = self.spacing();
        if let Some(mut child) = holder.layout_ref_mut() {
            // A nested layout has no owner widget: its margin is 0, not the owner default
            // (`QLayoutPrivate::getMargin`, qlayout.cpp:117-129).
            // Owner defaults no longer reach a nested layout (`set_nested`).
            child.set_nested();
            child.set_smart_spacing(spacing);
            child.set_layout_alignment(alignment);
        }
        let widget: WidgetRef = std::rc::Rc::new(std::cell::RefCell::new(Box::new(holder)));
        self.items.push(GridItem {
            widget: widget.clone(),
            row,
            column,
            row_span: row_span.max(1),
            col_span: col_span.max(1),
            alignment: Alignment::NONE,
            nested: true,
        });
        self.update_layout();
        widget
    }

    /// `QLayout::removeWidget` (qlayout.cpp:1468-1485): every item holding `widget` leaves the
    /// layout. The widget itself is not destroyed. Returns whether an item was removed.
    pub fn remove_widget(&mut self, widget: &WidgetRef) -> bool {
        let before = self.items.len();
        self.items
            .retain(|item| !std::rc::Rc::ptr_eq(&item.widget, widget));
        let removed = self.items.len() != before;
        if removed {
            self.update_layout();
        }
        removed
    }

    pub fn set_row_stretch(&mut self, row: usize, stretch: u32) {
        if row >= self.row_stretches.len() {
            self.row_stretches.resize(row + 1, 0);
        }
        self.row_stretches[row] = stretch;
        self.update_layout();
    }

    pub fn set_column_stretch(&mut self, col: usize, stretch: u32) {
        if col >= self.col_stretches.len() {
            self.col_stretches.resize(col + 1, 0);
        }
        self.col_stretches[col] = stretch;
        self.update_layout();
    }

    /// `QGridLayout::setColumnMinimumWidth` equivalent: the column is laid out at least this
    /// wide even when no item in it asks for it.
    pub fn set_column_minimum_width(&mut self, col: usize, min_w: i32) {
        if col >= self.col_min_widths.len() {
            self.col_min_widths.resize(col + 1, 0);
        }
        self.col_min_widths[col] = min_w;
        self.update_layout();
    }

    /// `QGridLayout::setRowMinimumHeight` equivalent: the row is laid out at least this tall
    /// even when no item in it asks for it.
    pub fn set_row_minimum_height(&mut self, row: usize, min_h: i32) {
        if row >= self.row_min_heights.len() {
            self.row_min_heights.resize(row + 1, 0);
        }
        self.row_min_heights[row] = min_h;
        self.update_layout();
    }

    pub fn set_horizontal_spacing(&mut self, spacing: i32) {
        self.h_spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    pub fn set_vertical_spacing(&mut self, spacing: i32) {
        self.v_spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    /// `QGridLayoutPrivate::expand`: rows and columns grow with the items and with any stretch
    /// or minimum size set for them.
    pub fn row_count(&self) -> usize {
        self.items
            .iter()
            .map(|it| it.row + it.row_span)
            .chain([self.row_stretches.len(), self.row_min_heights.len()])
            .max()
            .unwrap_or(0)
    }

    pub fn column_count(&self) -> usize {
        self.items
            .iter()
            .map(|it| it.column + it.col_span)
            .chain([self.col_stretches.len(), self.col_min_widths.len()])
            .max()
            .unwrap_or(0)
    }
    /// Gap between columns: a layout with no spacing puts none in (`setupSpacings` takes the
    /// spacing, and a parentless grid has none, probed).
    fn gap_h(&self) -> i32 {
        self.horizontal_spacing().max(0)
    }

    fn gap_v(&self) -> i32 {
        self.vertical_spacing().max(0)
    }

    /// Hands the spacing to every nested child layout (`qSmartSpacing`, qlayoutengine.cpp:390-401).
    fn propagate_smart_spacing(&self) {
        let spacing = self.spacing();
        for item in self.items.iter().filter(|item| item.nested) {
            let holder = item.widget.borrow();
            let child = holder.layout_ref_mut();
            if let Some(mut child) = child {
                child.set_smart_spacing(spacing);
            }
        }
    }

    /// The rectangle the content is laid out in: `QGridLayout::setGeometry` places an aligned grid
    /// inside `QLayout::alignmentRect` of its geometry (qgridlayout.cpp:1324).
    fn placed_rect(&self) -> Rect {
        if self.alignment == Alignment::NONE {
            return self.geometry;
        }
        self.ensure_geom();
        let (hint, max, expanding, has_hfw) = {
            let cache = self.geom_cache.borrow();
            let geom = cache.as_ref().unwrap();
            (geom.hint, geom.max, geom.expanding, geom.has_hfw)
        };
        let hfw = |width: i32| self.height_for_width(width);
        alignment_rect(
            self.geometry,
            self.alignment,
            hint,
            max,
            expanding,
            if has_hfw { Some(&hfw) } else { None },
        )
    }

    /// Width of the columns an item spans, once `cols` holds their positions.
    fn cell_width(cols: &[LayoutStruct], item: &GridItem) -> i32 {
        let last = &cols[item.column + item.col_span - 1];
        last.pos + last.size - cols[item.column].pos
    }

    /// `QGridLayoutPrivate::setupHfwLayoutData` and `recalcHFW` (qgridlayout.cpp:334-356, 816-877):
    /// the row chain once `cols` holds the column widths. Rows start at their minimum; each item
    /// raises the rows it covers to its height at its cell width.
    fn hfw_rows(&self, base_rows: &[LayoutStruct], cols: &[LayoutStruct]) -> Vec<LayoutStruct> {
        let mut rows = base_rows.to_vec();
        for (i, row) in rows.iter_mut().enumerate() {
            let min = self.row_min_heights.get(i).copied().unwrap_or(0);
            row.size_hint = min;
            row.minimum_size = min;
        }
        for item in &self.items {
            let (r1, r2) = (item.row, item.row + item.row_span - 1);
            if r1 != r2 {
                init_empty_multi_box(&mut rows, r1, r2);
                continue;
            }
            let w = item.widget.borrow();
            if entry_is_empty(&item.widget, item.nested) {
                continue;
            }
            let row = &mut rows[r1];
            if item_has_height_for_width(&**w) {
                let h = item_height_for_width(&**w, Self::cell_width(cols, item));
                row.size_hint = row.size_hint.max(h);
                row.minimum_size = row.minimum_size.max(h);
            } else {
                row.size_hint = row.size_hint.max(item_size_hint(&**w).height);
                row.minimum_size = row.minimum_size.max(item_minimum_size(&**w).height);
            }
        }
        for item in &self.items {
            let (r1, r2) = (item.row, item.row + item.row_span - 1);
            let w = item.widget.borrow();
            if r1 == r2 || entry_is_empty(&item.widget, item.nested) {
                continue;
            }
            let (mut hint, mut min) = (item_size_hint(&**w).height, item_minimum_size(&**w).height);
            if item_has_height_for_width(&**w) {
                let h = item_height_for_width(&**w, Self::cell_width(cols, item));
                hint = hint.max(h);
                min = min.max(h);
            }
            let stretch = w.size_policy().vertical_stretch as i32;
            distribute_multi_box(&mut rows, r1, r2, min, hint, &self.row_stretches, stretch);
        }
        rows
    }

    fn ensure_geom(&self) {
        if !self.metric_dirty.get() && self.geom_cache.borrow().is_some() {
            return;
        }
        let (rows, cols, limits) = self.setup_layout_data();
        let hint_raw = find_size(&rows, &cols, |d| d.size_hint);
        let min_raw = find_size(&rows, &cols, |d| d.minimum_size);
        let max_raw = find_size(&rows, &cols, |d| d.maximum_size);
        let h_margin = self.margins.left + self.margins.right;
        let v_margin = self.margins.top + self.margins.bottom;
        let hint = Size::new(hint_raw.width + h_margin, hint_raw.height + v_margin);
        let min = Size::new(min_raw.width + h_margin, min_raw.height + v_margin);
        // `QGridLayout::maximumSize` (qgridlayout.cpp:1207-1221): bounded to QLAYOUTSIZE_MAX.
        let max = Size::new(
            (max_raw.width + h_margin).min(LAYOUT_SIZE_MAX),
            (max_raw.height + v_margin).min(LAYOUT_SIZE_MAX),
        );
        let expanding = (
            cols.iter().any(|c| c.expansive),
            rows.iter().any(|r| r.expansive),
        );
        let has_hfw = self
            .items
            .iter()
            .any(|item| item_has_height_for_width(&**item.widget.borrow()));
        let geom = GridGeom {
            rows,
            cols,
            limits,
            hint,
            min,
            max,
            expanding,
            has_hfw,
        };
        *self.geom_cache.borrow_mut() = Some(geom);
        self.metric_dirty.set(false);
    }
}

impl Default for GridLayout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout for GridLayout {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn take_at(&mut self, index: usize) -> Option<WidgetRef> {
        let widget = self.items.get(index)?.widget.clone();
        GridLayout::remove_widget(self, &widget);
        Some(widget)
    }

    fn geometry(&self) -> Rect {
        self.geometry
    }

    fn set_geometry(&mut self, rect: Rect) {
        if self.geometry == rect && !self.needs_activation.get() {
            return;
        }
        self.geometry = rect;
        self.needs_activation.set(true);
        self.activate();
    }
    fn add_widget(&mut self, widget: WidgetRef) {
        let r = self.row_count();
        self.add_widget(widget, r, 0);
    }

    fn set_alignment(&mut self, widget: &WidgetRef, alignment: Alignment) -> bool {
        let Some(item) = self
            .items
            .iter_mut()
            .find(|item| std::rc::Rc::ptr_eq(&item.widget, widget))
        else {
            return false;
        };
        if item.nested {
            // `QLayout::setAlignment(QLayout*)` stores the alignment on the child layout (qlayout.cpp:213-227).
            let holder = item.widget.borrow();
            let child = holder.layout_ref_mut();
            if let Some(mut child) = child {
                child.set_layout_alignment(alignment);
            }
        } else {
            item.alignment = alignment;
        }
        self.update_layout();
        true
    }

    fn widgets(&self) -> Vec<WidgetRef> {
        self.items.iter().map(|it| it.widget.clone()).collect()
    }
    fn set_margins(&mut self, margins: Margins) {
        self.margins = margins;
        self.margins_explicit = true;
        self.update_layout();
    }

    fn set_default_margin(&mut self, margin: i32) {
        if self.margins_explicit {
            return;
        }
        let margins = Margins::new(margin, margin, margin, margin);
        if self.margins != margins {
            self.margins = margins;
            self.update_layout();
        }
    }

    fn set_nested(&mut self) {
        self.set_default_margin(0);
        self.margins_explicit = true;
        self.nested = true;
    }

    fn is_nested(&self) -> bool {
        self.nested
    }

    fn margins(&self) -> Margins {
        self.margins
    }

    fn set_spacing(&mut self, spacing: i32) {
        self.h_spacing = spacing;
        self.v_spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    fn spacing(&self) -> i32 {
        // `QGridLayout::spacing` (qgridlayout.cpp:1150-1158): the common value, or -1 when the
        // horizontal and vertical spacings differ.
        let (h, v) = (self.horizontal_spacing(), self.vertical_spacing());
        if h == v {
            h
        } else {
            -1
        }
    }

    fn alignment(&self) -> Alignment {
        self.alignment
    }

    fn set_layout_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
        self.invalidate();
    }

    fn set_smart_spacing(&mut self, spacing: i32) {
        self.smart_spacing = spacing;
        self.propagate_smart_spacing();
        self.update_layout();
    }

    fn maximum_size(&self) -> Size {
        // `QGridLayout::maximumSize` (qgridlayout.cpp:1207-1221): an aligned axis is unbounded.
        self.ensure_geom();
        let mut max = self.geom_cache.borrow().as_ref().unwrap().max;
        if self.alignment.horizontal() {
            max.width = LAYOUT_SIZE_MAX;
        }
        if self.alignment.vertical() {
            max.height = LAYOUT_SIZE_MAX;
        }
        max
    }

    fn has_height_for_width(&self) -> bool {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().has_hfw
    }

    fn is_empty(&self) -> bool {
        self.items
            .iter()
            .all(|item| entry_is_empty(&item.widget, item.nested))
    }

    fn height_for_width(&self, width: i32) -> i32 {
        // `QGridLayoutPrivate::heightForWidth` (qgridlayout.cpp:358-372).
        self.ensure_geom();
        let cache = self.geom_cache.borrow();
        let geom = cache.as_ref().unwrap();
        if !geom.has_hfw {
            return -1;
        }
        let m = self.margins;
        let inner = width - m.left - m.right;
        let mut cols = geom.cols.clone();
        let cc = cols.len();
        q_geom_calc(&mut cols, 0, cc, 0, inner, -1);
        let rows = self.hfw_rows(&geom.rows, &cols);
        find_size(&rows, &cols, |d| d.size_hint).height + m.top + m.bottom
    }
    fn minimum_height_for_width(&self, width: i32) -> i32 {
        // `QGridLayoutPrivate::minimumHeightForWidth` (qgridlayout.cpp:374-382).
        self.ensure_geom();
        let cache = self.geom_cache.borrow();
        let geom = cache.as_ref().unwrap();
        if !geom.has_hfw {
            return -1;
        }
        let m = self.margins;
        let inner = width - m.left - m.right;
        let mut cols = geom.cols.clone();
        let cc = cols.len();
        q_geom_calc(&mut cols, 0, cc, 0, inner, -1);
        let rows = self.hfw_rows(&geom.rows, &cols);
        find_size(&rows, &cols, |d| d.minimum_size).height + m.top + m.bottom
    }

    fn size_hint(&self) -> Size {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().hint
    }

    fn minimum_size(&self) -> Size {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().min
    }

    fn expanding_directions(&self) -> (bool, bool) {
        self.ensure_geom();
        self.geom_cache.borrow().as_ref().unwrap().expanding
    }

    fn invalidate(&mut self) {
        self.metric_dirty.set(true);
        self.needs_activation.set(true);
        *self.geom_cache.borrow_mut() = None;
    }

    fn is_dirty(&self) -> bool {
        self.needs_activation.get()
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

    /// `QGridLayoutPrivate::distribute`.
    fn activate(&mut self) {
        if !self.needs_activation.get() {
            return;
        }
        if items_busy(self.items.iter().map(|item| &item.widget)) {
            crate::layout_scheduler::LayoutScheduler::note_deferred();
            return;
        }
        self.needs_activation.set(false);
        self.ensure_geom();
        let placed = self.placed_rect();
        // `qGeomCalc` writes positions into the chains, so the activation works on copies.
        let (base_rows, mut cols, has_hfw) = {
            let borrow = self.geom_cache.borrow();
            let g = borrow.as_ref().unwrap();
            (g.rows.clone(), g.cols.clone(), g.has_hfw)
        };
        let (rr, cc) = (base_rows.len(), cols.len());
        if rr == 0 || cc == 0 {
            return;
        }

        let x = placed.x + self.margins.left;
        let y = placed.y + self.margins.top;
        let width = placed.width - self.margins.left - self.margins.right;
        let height = placed.height - self.margins.top - self.margins.bottom;
        q_geom_calc(&mut cols, 0, cc, x, width, -1);
        // `QGridLayoutPrivate::distribute` lays the rows out from the height-for-width rows when
        // some item has height-for-width (qgridlayout.cpp:879-928).
        let mut rows = if has_hfw {
            self.hfw_rows(&base_rows, &cols)
        } else {
            base_rows
        };
        q_geom_calc(&mut rows, 0, rr, y, height, -1);
        let rtl = GuiApplication::is_right_to_left();

        for (i, item) in self.items.iter().enumerate() {
            let (r2, c2) = (
                item.row + item.row_span - 1,
                item.column + item.col_span - 1,
            );
            let left = cols[item.column].pos;
            let top = rows[item.row].pos;
            let w = cols[c2].pos + cols[c2].size - left;
            let h = rows[r2].pos + rows[r2].size - top;
            // `QGridLayoutPrivate::distribute` mirrors a cell under RTL (qgridlayout.cpp:921-922).
            let left = if rtl { 2 * x + width - left - w } else { left };

            let old_size = {
                let widget = item.widget.borrow();
                let g = widget.geometry();
                Size::new(g.width, g.height)
            };
            let rect = Rect::new(left, top, w, h);
            if item.nested {
                // `QGridBox::setGeometry` hands a nested layout its whole cell (qgridlayout.cpp:48);
                // the child applies its own alignment in its `setGeometry`.
                item.widget.borrow().set_geometry(rect);
                set_nested_layout_geometry(&item.widget);
            } else {
                let limits = self.geom_cache.borrow().as_ref().unwrap().limits[i];
                item_set_geometry_with(&**item.widget.borrow(), rect, item.alignment, limits);
                relayout_child_after_resize(&item.widget, old_size);
            }
        }
    }
}

/// Whether some item's widget is borrowed further up the call stack (a click handler that adds
/// a row to its own layout). Such a layout cannot place its items now; it stays dirty and
/// `LayoutScheduler::flush_deferred` runs it once the borrow ends.
fn items_busy<'a>(mut widgets: impl Iterator<Item = &'a WidgetRef>) -> bool {
    widgets.any(|widget| widget.try_borrow().is_err())
}

/// After a layout placed `child`: a child that owns a layout lays it out again when its own size
/// changed or the layout is still invalid. The size is the widget's, not the cell's: an aligned or
/// size-limited item can keep its size in a resized cell, and Qt resizes (`QEvent::Resize`) only
/// when the widget's size changed (qwidget.cpp:7317-7329). The child's metric cache stays: a new
/// size moves its items but changes none of their size hints (`QBoxLayout::setGeometry` only
/// re-runs `setupGeom` when the layout is dirty, qboxlayout.cpp:735-742). An invalid layout has
/// already dropped its metrics.
pub(crate) fn relayout_child_after_resize(child: &WidgetRef, old_size: Size) {
    let w = child.borrow();
    let g = w.geometry();
    let needs_pass = w
        .layout_ref_mut()
        .map(|layout| old_size != Size::new(g.width, g.height) || layout.is_dirty());
    drop(w);
    if needs_pass == Some(true) {
        crate::layout_scheduler::LayoutScheduler::request_layout(child);
    }
}

impl GridLayout {
    /// `QGridLayoutPrivate::setupLayoutData`: the row and column chains of the current items.
    fn setup_layout_data(&self) -> (Vec<LayoutStruct>, Vec<LayoutStruct>, Vec<ItemLimits>) {
        let rr = self.row_count();
        let cc = self.column_count();
        let stretch_of = |v: &[u32], i: usize| v.get(i).copied().unwrap_or(0) as i32;
        let min_of = |v: &[i32], i: usize| v.get(i).copied().unwrap_or(0);

        let mut rows = vec![LayoutStruct::default(); rr];
        let mut cols = vec![LayoutStruct::default(); cc];
        for (i, row) in rows.iter_mut().enumerate() {
            let (stretch, min) = (
                stretch_of(&self.row_stretches, i),
                min_of(&self.row_min_heights, i),
            );
            row.init(stretch, min);
            row.maximum_size = if stretch != 0 { LAYOUT_SIZE_MAX } else { min };
        }
        for (i, col) in cols.iter_mut().enumerate() {
            let (stretch, min) = (
                stretch_of(&self.col_stretches, i),
                min_of(&self.col_min_widths, i),
            );
            col.init(stretch, min);
            col.maximum_size = if stretch != 0 { LAYOUT_SIZE_MAX } else { min };
        }

        struct Boxed {
            min: Size,
            hint: Size,
            max: Size,
            expanding: (bool, bool),
            empty: bool,
            h_stretch: i32,
            v_stretch: i32,
            limits: ItemLimits,
        }
        let boxes: Vec<Boxed> = self
            .items
            .iter()
            .map(|item| {
                let w = item.widget.borrow();
                let policy = w.size_policy();
                let mut limits = ItemLimits::of(&**w, item.alignment);
                if item.nested {
                    // `QLayoutItem::maximumSize` of a `QLayout` is that layout's own maximumSize.
                    if let Some(inner) = w.layout_ref_mut() {
                        limits.item_max = inner.maximum_size();
                    }
                }
                Boxed {
                    min: item_minimum_size(&**w),
                    hint: item_size_hint(&**w),
                    max: limits.item_max,
                    expanding: item_expanding(&**w, item.alignment),
                    empty: entry_is_empty(&item.widget, item.nested),
                    h_stretch: policy.horizontal_stretch as i32,
                    v_stretch: policy.vertical_stretch as i32,
                    limits,
                }
            })
            .collect();

        // Which item covers which cell, to find the neighbours the spacing goes between.
        let mut grid: Vec<Option<usize>> = vec![None; rr * cc];
        let mut has_multi = false;
        for (i, item) in self.items.iter().enumerate() {
            let b = &boxes[i];
            let (to_row, to_col) = (
                item.row + item.row_span - 1,
                item.column + item.col_span - 1,
            );

            if item.row == to_row {
                if !b.empty {
                    let data = &mut rows[item.row];
                    if stretch_of(&self.row_stretches, item.row) == 0 {
                        data.stretch = data.stretch.max(b.v_stretch);
                    }
                    data.size_hint = data.size_hint.max(b.hint.height);
                    data.minimum_size = data.minimum_size.max(b.min.height);
                    q_max_exp_calc(
                        &mut data.maximum_size,
                        &mut data.expansive,
                        &mut data.empty,
                        b.max.height,
                        b.expanding.1,
                        b.empty,
                    );
                }
            } else {
                init_empty_multi_box(&mut rows, item.row, to_row);
                has_multi = true;
            }

            if item.column == to_col {
                if !b.empty {
                    let data = &mut cols[item.column];
                    if stretch_of(&self.col_stretches, item.column) == 0 {
                        data.stretch = data.stretch.max(b.h_stretch);
                    }
                    data.size_hint = data.size_hint.max(b.hint.width);
                    data.minimum_size = data.minimum_size.max(b.min.width);
                    q_max_exp_calc(
                        &mut data.maximum_size,
                        &mut data.expansive,
                        &mut data.empty,
                        b.max.width,
                        b.expanding.0,
                        b.empty,
                    );
                }
            } else {
                init_empty_multi_box(&mut cols, item.column, to_col);
                has_multi = true;
            }

            for r in item.row..=to_row {
                for c in item.column..=to_col {
                    grid[r * cc + c] = Some(i);
                }
            }
        }

        let empty_of = |i: usize| boxes[i].empty;
        setup_spacings(&mut cols, &grid, cc, self.gap_h(), true, &empty_of);
        setup_spacings(&mut rows, &grid, cc, self.gap_v(), false, &empty_of);

        // Multi-cell items go in after the single-cell ones for a better distribution.
        if has_multi {
            for (i, item) in self.items.iter().enumerate() {
                let b = &boxes[i];
                let (to_row, to_col) = (
                    item.row + item.row_span - 1,
                    item.column + item.col_span - 1,
                );
                if item.row != to_row {
                    distribute_multi_box(
                        &mut rows,
                        item.row,
                        to_row,
                        b.min.height,
                        b.hint.height,
                        &self.row_stretches,
                        b.v_stretch,
                    );
                }
                if item.column != to_col {
                    distribute_multi_box(
                        &mut cols,
                        item.column,
                        to_col,
                        b.min.width,
                        b.hint.width,
                        &self.col_stretches,
                        b.h_stretch,
                    );
                }
            }
        }

        for row in &mut rows {
            row.expansive = row.expansive || row.stretch > 0;
        }
        for col in &mut cols {
            col.expansive = col.expansive || col.stretch > 0;
        }
        let limits = boxes.iter().map(|b| b.limits).collect();
        (rows, cols, limits)
    }
}
