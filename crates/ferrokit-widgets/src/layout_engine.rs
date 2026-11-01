// SPDX-License-Identifier: LGPL-3.0-only
//! Port of `qtbase/src/widgets/kernel/qlayoutengine.cpp` (`qGeomCalc`, `qMaxExpCalc`,
//! `qSmartMinSize`, `qSmartMaxSize`) and of the sizing rules of `QWidgetItem` /
//! `QSpacerItem` in `qlayoutitem.cpp`, as of Qt 6.11.2.
//!
//! Layouts (`BoxLayout`, `GridLayout`) feed [`LayoutStruct`] chains to [`q_geom_calc`], exactly as
//! `QBoxLayout` and `QGridLayout` do, so the spare pixels, the spacing around empty items and the
//! rounding all come out the same.

use crate::layout::Alignment;
use crate::size_policy::Policy;
use crate::widget::Widget;
use ferrokit_gui::application::GuiApplication;
use ferrokit_gui::geometry::primitives::{Rect, Size};

/// `QLAYOUTSIZE_MAX`.
pub const LAYOUT_SIZE_MAX: i32 = i32::MAX / 256 / 16;
/// `QWIDGETSIZE_MAX`.
pub const WIDGET_SIZE_MAX: i32 = (1 << 24) - 1;
/// `qSmartSpacing` of a widget's layout: the style's layout spacing (`PM_LayoutHorizontalSpacing`,
/// qcommonstyle.cpp:4746-4748, 6 at 96 dpi).
pub const WIDGET_LAYOUT_SPACING: i32 = 6;
/// `PM_LayoutLeftMargin`/`TopMargin`/`RightMargin`/`BottomMargin` for a window: 11 (qcommonstyle.cpp:4732-4750).
pub const WINDOW_LAYOUT_MARGIN: i32 = 11;
/// The same pixel metrics for a child widget: 9 (qcommonstyle.cpp:4732-4750).
pub const CHILD_LAYOUT_MARGIN: i32 = 9;

/// `QLayoutPrivate::getMargin` (qlayout.cpp:117-129) for an owner without an explicit margin: the
/// owner style's margin, 11 for a window (no parent) and 9 for a child. These are unscaled: Qt's
/// `qstylehelper::dpiScaled` multiplies by `dpi / 96` (qstylehelper.cpp:61-96), and the logical DPI
/// is 96 on the Windows setup probed (PySide6 6.11.2, logicalDotsPerInch 96.0), so the scale is 1.
pub fn owner_layout_margin(has_parent: bool) -> i32 {
    if has_parent {
        CHILD_LAYOUT_MARGIN
    } else {
        WINDOW_LAYOUT_MARGIN
    }
}

/// `QLayoutStruct`: one row, column or box of a layout chain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutStruct {
    pub stretch: i32,
    pub size_hint: i32,
    pub maximum_size: i32,
    pub minimum_size: i32,
    pub spacing: i32,
    pub expansive: bool,
    pub empty: bool,
    done: bool,
    pub pos: i32,
    pub size: i32,
}

impl LayoutStruct {
    /// `QLayoutStruct::init`.
    pub fn init(&mut self, stretch: i32, min_size: i32) {
        self.stretch = stretch;
        self.minimum_size = min_size;
        self.size_hint = min_size;
        self.maximum_size = LAYOUT_SIZE_MAX;
        self.expansive = false;
        self.empty = true;
        self.spacing = 0;
    }

    fn smart_size_hint(&self) -> i32 {
        if self.stretch > 0 {
            self.minimum_size
        } else {
            self.size_hint
        }
    }

    fn effective_spacer(&self, uniform_spacer: i32) -> i32 {
        if uniform_spacer >= 0 {
            uniform_spacer
        } else {
            self.spacing
        }
    }
}

type Fixed64 = i64;

fn to_fixed(i: i32) -> Fixed64 {
    i as Fixed64 * 256
}

fn f_round(i: Fixed64) -> i32 {
    (if i % 256 < 128 { i / 256 } else { 1 + i / 256 }) as i32
}

/// `qMaxExpCalc`: folds one box into the maximum size / expanding / empty state of a row or
/// column. Non-empty boxes win over empty ones.
pub fn q_max_exp_calc(
    max: &mut i32,
    exp: &mut bool,
    empty: &mut bool,
    box_max: i32,
    box_exp: bool,
    box_empty: bool,
) {
    if *exp {
        if box_exp {
            *max = (*max).max(box_max);
        }
    } else if box_exp || (*empty && (!box_empty || *max == 0)) {
        *max = box_max;
    } else if *empty == box_empty {
        *max = (*max).min(box_max);
    }
    *exp = *exp || box_exp;
    *empty = *empty && box_empty;
}

/// `qGeomCalc`: portions out `space` pixels starting at `pos` among the `count` entries of `chain`
/// that begin at `start`. `spacer` is the uniform spacing, or -1 to use each entry's own.
pub fn q_geom_calc(
    chain: &mut [LayoutStruct],
    start: usize,
    count: usize,
    pos: i32,
    space: i32,
    spacer: i32,
) {
    let mut spacer = spacer;
    let mut c_hint = 0i32;
    let mut c_min = 0i32;
    let mut sum_stretch = 0i32;
    let mut sum_spacing = 0i32;
    let mut expanding_count = 0i32;

    let mut all_empty_nonstretch = true;
    let mut pending_spacing = -1i32;
    let mut spacer_count = 0i32;

    for data in &mut chain[start..start + count] {
        data.done = false;
        c_hint += data.smart_size_hint();
        c_min += data.minimum_size;
        sum_stretch += data.stretch;
        if !data.empty {
            // Using pending_spacing, the spacing of the last (non-empty) item is ignored.
            if pending_spacing >= 0 {
                sum_spacing += pending_spacing;
                spacer_count += 1;
            }
            pending_spacing = data.effective_spacer(spacer);
        }
        if data.expansive {
            expanding_count += 1;
        }
        all_empty_nonstretch =
            all_empty_nonstretch && data.empty && !data.expansive && data.stretch <= 0;
    }

    let mut extraspace = 0i32;

    if space < c_min + sum_spacing {
        // Not even the minimum sizes fit: shave space off the largest items first.
        let min_size = c_min + sum_spacing;

        // Shrink the spacers proportionally.
        if spacer >= 0 {
            spacer = if min_size > 0 {
                spacer * space / min_size
            } else {
                0
            };
            sum_spacing = spacer * spacer_count;
        }

        let mut minimum_sizes: Vec<i32> = (start..start + count)
            .map(|i| chain[i].minimum_size)
            .collect();
        minimum_sizes.sort_unstable();

        let space_left = space - sum_spacing;

        let mut sum = 0i32;
        let mut idx = 0i32;
        let mut space_used = 0i32;
        let mut current = 0i32;
        while (idx as usize) < count && space_used < space_left {
            current = minimum_sizes[idx as usize];
            space_used = sum + current * (count as i32 - idx);
            sum += current;
            idx += 1;
        }
        idx -= 1;
        let deficit = space_used - space_left;

        let items = count as i32 - idx;
        // Capping every item at `current` would still use `deficit` pixels too many, so that excess
        // is spread over the items: each cap drops by deficit/items, and `rest` accumulates the
        // remainder of the integer division.
        let deficit_per_item = deficit / items;
        let remainder = deficit % items;
        let maxval = current - deficit_per_item;

        let mut rest = 0i32;
        for data in &mut chain[start..start + count] {
            let mut maxv = maxval;
            rest += remainder;
            if rest >= items {
                maxv -= 1;
                rest -= items;
            }
            data.size = data.minimum_size.min(maxv);
            data.done = true;
        }
    } else if space < c_hint + sum_spacing {
        // Between the minimum and the smart size hint: every item gives up an equal share.
        let mut n = count as i32;
        let mut space_left = space - sum_spacing;
        let mut overdraft = c_hint - space_left;

        // First give to the fixed ones.
        for data in &mut chain[start..start + count] {
            if !data.done && data.minimum_size >= data.smart_size_hint() {
                data.size = data.smart_size_hint();
                data.done = true;
                space_left -= data.smart_size_hint();
                n -= 1;
            }
        }
        let mut finished = n == 0;
        while !finished {
            finished = true;
            let fp_over = to_fixed(overdraft);
            let mut fp_w: Fixed64 = 0;

            for data in &mut chain[start..start + count] {
                if data.done {
                    continue;
                }
                fp_w += fp_over / n as Fixed64;
                let w = f_round(fp_w);
                data.size = data.smart_size_hint() - w;
                fp_w -= to_fixed(w);
                if data.size < data.minimum_size {
                    data.done = true;
                    data.size = data.minimum_size;
                    finished = false;
                    overdraft -= data.smart_size_hint() - data.minimum_size;
                    n -= 1;
                    break;
                }
            }
        }
        let _ = space_left;
    } else {
        // Extra space.
        let mut n = count as i32;
        let mut space_left = space - sum_spacing;
        // Settle the items that cannot or should not grow: those capped at their size hint, and
        // empty ones without stretch.
        for data in &mut chain[start..start + count] {
            if !data.done
                && (data.maximum_size <= data.smart_size_hint()
                    || (!all_empty_nonstretch
                        && data.empty
                        && !data.expansive
                        && data.stretch == 0))
            {
                data.size = data.smart_size_hint();
                data.done = true;
                space_left -= data.size;
                sum_stretch -= data.stretch;
                if data.expansive {
                    expanding_count -= 1;
                }
                n -= 1;
            }
        }
        extraspace = space_left;

        // Try a proportional split and measure the error: `deficit` is how far items fall short of
        // their size hint, `surplus` how far they overshoot their maximum. The larger error is
        // corrected (both when equal) and the split is retried for the remaining items until the
        // two balance.
        let mut surplus;
        let mut deficit;
        loop {
            surplus = 0;
            deficit = 0;
            let fp_space = to_fixed(space_left);
            let mut fp_w: Fixed64 = 0;
            for data in &mut chain[start..start + count] {
                if data.done {
                    continue;
                }
                extraspace = 0;
                if sum_stretch > 0 {
                    fp_w += (fp_space * data.stretch as Fixed64) / sum_stretch as Fixed64;
                } else if expanding_count > 0 {
                    fp_w += (fp_space * if data.expansive { 1 } else { 0 })
                        / expanding_count as Fixed64;
                } else {
                    fp_w += fp_space / n as Fixed64;
                }
                let w = f_round(fp_w);
                data.size = w;
                fp_w -= to_fixed(w);
                if w < data.smart_size_hint() {
                    deficit += data.smart_size_hint() - w;
                } else if w > data.maximum_size {
                    surplus += w - data.maximum_size;
                }
            }
            if deficit > 0 && surplus <= deficit {
                // Items that came out below their size hint are pinned at the hint.
                for data in &mut chain[start..start + count] {
                    if !data.done && data.size < data.smart_size_hint() {
                        data.size = data.smart_size_hint();
                        data.done = true;
                        space_left -= data.smart_size_hint();
                        sum_stretch -= data.stretch;
                        if data.expansive {
                            expanding_count -= 1;
                        }
                        n -= 1;
                    }
                }
            }
            if surplus > 0 && surplus >= deficit {
                // Items that came out above their maximum are pinned at the maximum.
                for data in &mut chain[start..start + count] {
                    if !data.done && data.size > data.maximum_size {
                        data.size = data.maximum_size;
                        data.done = true;
                        space_left -= data.maximum_size;
                        sum_stretch -= data.stretch;
                        if data.expansive {
                            expanding_count -= 1;
                        }
                        n -= 1;
                    }
                }
            }
            if !(n > 0 && surplus != deficit) {
                break;
            }
        }
        if n == 0 {
            extraspace = space_left;
        }
    }

    // Last resort: spread the leftover space evenly over the spacers, including the two at the
    // ends of the chain.
    let extra = extraspace / (spacer_count + 2);
    let mut p = pos + extra;
    for data in &mut chain[start..start + count] {
        data.pos = p;
        p += data.size;
        if !data.empty {
            p += data.effective_spacer(spacer) + extra;
        }
    }
}

/// `QSizePolicy::ExpandFlag` of one axis.
fn expand_flag(policy: Policy) -> bool {
    matches!(policy, Policy::Expanding | Policy::MinimumExpanding)
}

/// Effective minimum size for layout (`QWidget::minimumSize`, combining explicit and styled limits).
pub fn effective_minimum_size(widget: &dyn Widget) -> Size {
    widget.minimum_size()
}

/// Effective maximum size for layout (`QWidget::maximumSize`, combining explicit and styled limits).
pub fn effective_maximum_size(widget: &dyn Widget) -> Size {
    widget.maximum_size()
}

/// `qSmartMinSize(sizeHint, minSizeHint, minSize, maxSize, sizePolicy)` for `widget`.
pub fn smart_min_size(widget: &dyn Widget) -> Size {
    fn axis(policy: Policy, hint: i32, min_hint: i32) -> i32 {
        match policy {
            Policy::Ignored => 0,
            p if p.can_shrink() => min_hint,
            _ => hint.max(min_hint),
        }
    }
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);
    let max = effective_maximum_size(widget);
    let policy = widget.size_policy();

    let mut w = axis(policy.horizontal, hint.width, min_hint.width).min(max.width);
    let mut h = axis(policy.vertical, hint.height, min_hint.height).min(max.height);
    if min.width > 0 {
        w = min.width;
    }
    if min.height > 0 {
        h = min.height;
    }
    Size::new(w.max(0), h.max(0))
}

/// `qSmartMaxSize(sizeHint, minSize, maxSize, sizePolicy, align)` for `widget`: an aligned axis
/// may grow without limit, because the item is then placed inside the cell instead of filling it.
pub fn smart_max_size(widget: &dyn Widget, align: Alignment) -> Size {
    if align.horizontal() && align.vertical() {
        return Size::new(LAYOUT_SIZE_MAX, LAYOUT_SIZE_MAX);
    }
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);
    let policy = widget.size_policy();
    let mut s = effective_maximum_size(widget);
    let hint = Size::new(
        hint.width.max(min_hint.width).max(min.width),
        hint.height.max(min_hint.height).max(min.height),
    );
    if s.width == WIDGET_SIZE_MAX && !align.horizontal() && !policy.horizontal.can_grow() {
        s.width = hint.width;
    }
    if s.height == WIDGET_SIZE_MAX && !align.vertical() && !policy.vertical.can_grow() {
        s.height = hint.height;
    }
    if align.horizontal() {
        s.width = LAYOUT_SIZE_MAX;
    }
    if align.vertical() {
        s.height = LAYOUT_SIZE_MAX;
    }
    s
}

/// `QWidgetItem::isEmpty` (qlayoutitem.cpp:691-693): a hidden widget takes no part in the layout,
/// unless its size policy retains its size when hidden.
pub fn item_is_empty(widget: &dyn Widget) -> bool {
    !widget.is_visible() && !widget.size_policy().retain_size_when_hidden
}

/// `QStyle::visualAlignment(widget->layoutDirection(), align)` (qguiapplication_p.h:176-186) in
/// the application's layout direction, which every widget has here.
pub fn visual_alignment(align: Alignment) -> Alignment {
    align.visual(GuiApplication::layout_direction())
}

/// `QLayout::alignmentRect` (qlayout.cpp:1410-1455): the rectangle an aligned layout lays its
/// content in. Without an alignment flag on an axis, or on an expanding axis, the size is the
/// rectangle bounded by the maximum; on a fixed vertical axis a height-for-width layout takes the
/// height it wants at the width it gets. The alignment then places that size inside `rect`.
pub fn alignment_rect(
    rect: Rect,
    align: Alignment,
    hint: Size,
    max: Size,
    expanding: (bool, bool),
    height_for_width: Option<&dyn Fn(i32) -> i32>,
) -> Rect {
    let (expand_h, expand_v) = expanding;
    let mut s = hint;
    if expand_h || !align.horizontal() {
        s.width = rect.width.min(max.width);
    }
    if expand_v || !align.vertical() {
        s.height = rect.height.min(max.height);
    } else if let Some(height_for_width) = height_for_width {
        let h = height_for_width(s.width);
        if h < s.height {
            s.height = h.min(max.height);
        }
    }
    s.width = s.width.min(rect.width);
    s.height = s.height.min(rect.height);
    let mut x = rect.x;
    let mut y = rect.y;
    let visual = visual_alignment(align);
    if visual.contains(Alignment::RIGHT) {
        x += rect.width - s.width;
    } else if !visual.contains(Alignment::LEFT) {
        x += (rect.width - s.width) / 2;
    }
    if align.contains(Alignment::BOTTOM) {
        y += rect.height - s.height;
    } else if !align.contains(Alignment::TOP) {
        y += (rect.height - s.height) / 2;
    }
    Rect::new(x, y, s.width, s.height)
}

/// `QWidgetItem::sizeHint`.
pub fn item_size_hint(widget: &dyn Widget) -> Size {
    if item_is_empty(widget) {
        return Size::new(0, 0);
    }
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);
    let max = effective_maximum_size(widget);
    let policy = widget.size_policy();
    let mut w = hint.width.max(min_hint.width).min(max.width).max(min.width);
    let mut h = hint
        .height
        .max(min_hint.height)
        .min(max.height)
        .max(min.height);
    if policy.horizontal == Policy::Ignored {
        w = 0;
    }
    if policy.vertical == Policy::Ignored {
        h = 0;
    }
    Size::new(w, h)
}

/// `QWidgetItem::minimumSize`.
pub fn item_minimum_size(widget: &dyn Widget) -> Size {
    if item_is_empty(widget) {
        Size::new(0, 0)
    } else {
        smart_min_size(widget)
    }
}

/// `QWidgetItem::maximumSize`.
pub fn item_maximum_size(widget: &dyn Widget, align: Alignment) -> Size {
    if item_is_empty(widget) {
        Size::new(0, 0)
    } else {
        smart_max_size(widget, align)
    }
}

/// `QWidgetItem::hasHeightForWidth`: an empty item has none.
pub fn item_has_height_for_width(widget: &dyn Widget) -> bool {
    !item_is_empty(widget) && widget.has_height_for_width()
}

/// `QWidgetItem::heightForWidth` (qlayoutitem.cpp:508-535): the widget's height for `width`, kept
/// within its effective minimum and maximum height; -1 for an empty item.
pub fn item_height_for_width(widget: &dyn Widget, width: i32) -> i32 {
    if item_is_empty(widget) {
        return -1;
    }
    let min = effective_minimum_size(widget).height;
    let max = effective_maximum_size(widget).height;
    let h = widget.height_for_width(width);
    if h > max {
        max
    } else if h < min {
        min
    } else {
        h
    }
}

/// `QWidgetItem::expandingDirections` as (horizontal, vertical): the size policy's, plus those of
/// the widget's own layout when the policy lets it grow, minus the axes the item is aligned in.
pub fn item_expanding(widget: &dyn Widget, align: Alignment) -> (bool, bool) {
    if item_is_empty(widget) {
        return (false, false);
    }
    let policy = widget.size_policy();
    let mut horizontal = expand_flag(policy.horizontal);
    let mut vertical = expand_flag(policy.vertical);
    if let Some(layout) = widget.layout_ref_mut() {
        let (layout_h, layout_v) = layout.expanding_directions();
        if policy.horizontal.can_grow() && layout_h {
            horizontal = true;
        }
        if policy.vertical.can_grow() && layout_v {
            vertical = true;
        }
    }
    (
        horizontal && !align.horizontal(),
        vertical && !align.vertical(),
    )
}

/// `QWidgetItem::setGeometry` (qlayoutitem.cpp:408-474) for a left-to-right layout.
///
/// The widget gets the rectangle bounded by the item's maximum size; on an aligned axis it is
/// also cut down to the item's size hint and placed inside the rectangle. A missing horizontal
/// alignment means `AlignLeft` (`QStyle::visualAlignment`), a missing vertical one centres. A
/// hidden widget keeps no geometry. A vertically aligned widget is cut to its height for its width
/// when it has `heightForWidth`, else to its size hint height.
pub fn item_set_geometry(widget: &dyn Widget, rect: Rect, align: Alignment) {
    item_set_geometry_with(widget, rect, align, ItemLimits::of(widget, align));
}

/// The size limits `item_set_geometry` applies: the item's `qSmartMaxSize`
/// (`QWidgetItem::maximumSize`), the widget's own minimum and maximum size, which
/// `QWidget::setGeometry` bounds the result by (qwidget.cpp:7286, 7300-7305), and, for an aligned
/// item, the size it is cut to on an aligned axis (`QWidgetItem::setGeometry`,
/// qlayoutitem.cpp:432-447).
///
/// They depend only on the widget's size metrics, so a layout computes them with the rest of its
/// metric cache and reuses them for every geometry pass until it is invalidated, as
/// `QWidgetItemV2` caches the item's size hint (qlayoutitem.cpp:778-789).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLimits {
    pub item_max: Size,
    pub widget_min: Size,
    pub widget_max: Size,
    /// The preferred size an aligned axis is cut to; unused (zero) without alignment.
    pub aligned_pref: Size,
}

impl ItemLimits {
    pub fn of(widget: &dyn Widget, align: Alignment) -> Self {
        let aligned_pref = if align.horizontal() || align.vertical() {
            let policy = widget.size_policy();
            let mut pref = item_size_hint(widget);
            if policy.horizontal == Policy::Ignored {
                pref.width = widget
                    .size_hint()
                    .width
                    .max(effective_minimum_size(widget).width);
            }
            if policy.vertical == Policy::Ignored {
                pref.height = widget
                    .size_hint()
                    .height
                    .max(effective_minimum_size(widget).height);
            }
            pref
        } else {
            Size::new(0, 0)
        };
        Self {
            item_max: item_maximum_size(widget, align),
            widget_min: effective_minimum_size(widget),
            widget_max: effective_maximum_size(widget),
            aligned_pref,
        }
    }
}

/// `item_set_geometry` with limits computed earlier (`ItemLimits::of`).
pub fn item_set_geometry_with(
    widget: &dyn Widget,
    rect: Rect,
    align: Alignment,
    limits: ItemLimits,
) {
    if item_is_empty(widget) {
        // Qt `QWidgetItem::setGeometry` (qlayoutitem.cpp:410) returns early for an empty item.
        // Empty / hidden non-retained items keep their geometry rather than being set to (0,0,0,0).
        return;
    }
    let max = limits.item_max;
    let mut width = rect.width.min(max.width);
    let mut height = rect.height.min(max.height);
    if align.horizontal() {
        width = width.min(limits.aligned_pref.width);
    }
    if align.vertical() {
        height = if item_has_height_for_width(widget) {
            height.min(item_height_for_width(widget, width))
        } else {
            height.min(limits.aligned_pref.height)
        };
    }
    let mut x = rect.x;
    let mut y = rect.y;
    // `QWidgetItem::setGeometry` (qlayoutitem.cpp:451-455) decides on the visual alignment.
    let visual = visual_alignment(align);
    let left = !visual.contains(Alignment::RIGHT) && visual.contains(Alignment::LEFT);
    if visual.contains(Alignment::RIGHT) {
        x += rect.width - width;
    } else if !left {
        x += (rect.width - width) / 2;
    }
    if align.contains(Alignment::BOTTOM) {
        y += rect.height - height;
    } else if !align.contains(Alignment::TOP) {
        y += (rect.height - height) / 2;
    }
    // Do not move outside of the parent.
    if x < 0 {
        width += x;
        x = 0;
    }
    if y < 0 {
        height += y;
        y = 0;
    }
    // `QWidget::setGeometry` keeps the size within the widget's own minimum and maximum size.
    let width = width
        .min(limits.widget_max.width)
        .max(limits.widget_min.width);
    let height = height
        .min(limits.widget_max.height)
        .max(limits.widget_min.height);
    widget.set_geometry(Rect::new(x, y, width, height));
}

/// `QGridLayoutPrivate::findSize`: the sum of one field of every row and of every column, each
/// with the spacing that follows it.
pub fn find_size(
    rows: &[LayoutStruct],
    cols: &[LayoutStruct],
    field: impl Fn(&LayoutStruct) -> i32,
) -> Size {
    let h: i64 = rows
        .iter()
        .map(|r| field(r) as i64 + r.spacing as i64)
        .sum();
    let w: i64 = cols
        .iter()
        .map(|c| field(c) as i64 + c.spacing as i64)
        .sum();
    Size::new(
        w.min(LAYOUT_SIZE_MAX as i64) as i32,
        h.min(LAYOUT_SIZE_MAX as i64) as i32,
    )
}

/// `initEmptyMultiBox`.
pub fn init_empty_multi_box(chain: &mut [LayoutStruct], start: usize, end: usize) {
    for data in &mut chain[start..=end] {
        if data.empty && data.maximum_size == 0 {
            // truly empty box
            data.maximum_size = WIDGET_SIZE_MAX;
        }
        data.empty = false;
    }
}

/// `distributeMultiBox`.
pub fn distribute_multi_box(
    chain: &mut [LayoutStruct],
    start: usize,
    end: usize,
    min_size: i32,
    size_hint: i32,
    stretch_array: &[u32],
    stretch: i32,
) {
    let mut w = 0i32;
    let mut wh = 0i32;
    let mut max = 0i32;

    for (i, data) in chain.iter_mut().enumerate().take(end + 1).skip(start) {
        w += data.minimum_size;
        wh += data.size_hint;
        max = max.saturating_add(data.maximum_size);
        if stretch_array.get(i).copied().unwrap_or(0) == 0 {
            data.stretch = data.stretch.max(stretch);
        }
        if i != end {
            let spacing = data.spacing;
            w += spacing;
            wh += spacing;
            max = max.saturating_add(spacing);
        }
    }

    let count = end - start + 1;
    if max < min_size {
        // At least one maximum size must grow; `qGeomCalc` puts the extra space in between the
        // items, which is recovered here and given to the items themselves.
        q_geom_calc(chain, start, count, 0, min_size, -1);
        let mut pos = 0;
        for i in start..=end {
            let next_pos = if i == end { min_size } else { chain[i + 1].pos };
            let data = &mut chain[i];
            let mut real_size = next_pos - pos;
            if i != end {
                real_size -= data.spacing;
            }
            if data.minimum_size < real_size {
                data.minimum_size = real_size;
            }
            if data.maximum_size < data.minimum_size {
                data.maximum_size = data.minimum_size;
            }
            pos = next_pos;
        }
    } else if w < min_size {
        q_geom_calc(chain, start, count, 0, min_size, -1);
        for data in &mut chain[start..=end] {
            if data.minimum_size < data.size {
                data.minimum_size = data.size;
            }
        }
    }

    if wh < size_hint {
        q_geom_calc(chain, start, count, 0, size_hint, -1);
        for data in &mut chain[start..=end] {
            if data.size_hint < data.size {
                data.size_hint = data.size;
            }
        }
    }
}

/// `QGridLayoutPrivate::setupSpacings` with a fixed spacing: the spacing after a non-empty row
/// (or column, when `horizontal`) up to the next non-empty one that is not the same item.
/// `grid` holds the item covering each of the `cc` columns of every row.
pub fn setup_spacings(
    chain: &mut [LayoutStruct],
    grid: &[Option<usize>],
    cc: usize,
    fixed_spacing: i32,
    horizontal: bool,
    empty_of: &dyn Fn(usize) -> bool,
) {
    let num_rows = chain.len();
    let num_columns = if horizontal {
        grid.len() / cc.max(1)
    } else {
        cc
    };
    for c in 0..num_columns {
        let mut previous_box: Option<usize> = None;
        let mut previous_row: Option<usize> = None;
        for r in 0..num_rows {
            if chain[r].empty {
                continue;
            }
            let cell = if horizontal { c * cc + r } else { r * cc + c };
            let current = grid[cell];
            if let Some(prev_row) = previous_row
                && (current.is_none() || previous_box != current)
            {
                let mut spacing = fixed_spacing;
                if !horizontal
                    && let Some(sibling) = current
                    && empty_of(sibling)
                {
                    spacing = 0;
                }
                if spacing > chain[prev_row].spacing {
                    chain[prev_row].spacing = spacing;
                }
            }
            previous_box = current;
            previous_row = Some(r);
        }
    }
}
