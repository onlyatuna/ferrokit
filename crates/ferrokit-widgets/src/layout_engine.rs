// SPDX-License-Identifier: LGPL-3.0-only
//! Port of `reference (`q_geom_calc`, `q_max_exp_calc`,
//! `smart_min_size`, `smart_max_size`) and of the sizing rules of a widget layout item /
//! a spacer layout item in, as of Qt 6.11.2.
//!
//! Layouts (`BoxLayout`, `GridLayout`) feed [`LayoutStruct`] chains to [`q_geom_calc`], exactly as
//! `BoxLayout` and `GridLayout` do, so the spare pixels, the spacing around empty items and the
//! rounding all come out the same.

use crate::layout::Alignment;
use crate::size_policy::Policy;
use crate::widget::Widget;
use ferrokit_gui::application::GuiApplication;
use ferrokit_gui::geometry::primitives::{Rect, Size};

/// `LAYOUT_SIZE_MAX`.
pub const LAYOUT_SIZE_MAX: i32 = i32::MAX / 256 / 16;
/// `WIDGET_SIZE_MAX`.
pub const WIDGET_SIZE_MAX: i32 = (1 << 24) - 1;
/// layout default spacing of a widget's layout: the style's layout spacing (horizontal layout spacing metric,
/// 6 at 96 dpi).
pub const WIDGET_LAYOUT_SPACING: i32 = 6;
/// layout margin metrics for a window: 11.
pub const WINDOW_LAYOUT_MARGIN: i32 = 11;
/// The same pixel metrics for a child widget: 9.
pub const CHILD_LAYOUT_MARGIN: i32 = 9;

/// default owner layout margin lookup for an owner without an explicit margin: the
/// owner style's margin, 11 for a window (no parent) and 9 for a child. These are unscaled: Qt's
/// DPI scaling multiplies by `dpi / 96`, and the logical DPI
/// is 96 on the Windows setup probed (PySide6 6.11.2, logicalDotsPerInch 96.0), so the scale is 1.
pub fn owner_layout_margin(has_parent: bool) -> i32 {
    if has_parent {
        CHILD_LAYOUT_MARGIN
    } else {
        WINDOW_LAYOUT_MARGIN
    }
}

/// a layout chain entry: one row, column or box of a layout chain.
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
    /// chain item initialization.
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

struct ChainSummary {
    total_min: i32,
    total_preferred: i32,
    total_stretch: i32,
    total_spacing: i32,
    expanding_count: i32,
    spacer_count: i32,
    all_empty_nonstretch: bool,
}

fn summarize_chain(chain: &mut [LayoutStruct], uniform_spacer: i32) -> ChainSummary {
    let mut summary = ChainSummary {
        total_min: 0,
        total_preferred: 0,
        total_stretch: 0,
        total_spacing: 0,
        expanding_count: 0,
        spacer_count: 0,
        all_empty_nonstretch: true,
    };
    let mut pending_spacing = -1i32;
    for item in chain.iter_mut() {
        item.done = false;
        summary.total_preferred += item.smart_size_hint();
        summary.total_min += item.minimum_size;
        summary.total_stretch += item.stretch;
        if !item.empty {
            if pending_spacing >= 0 {
                summary.total_spacing += pending_spacing;
                summary.spacer_count += 1;
            }
            pending_spacing = item.effective_spacer(uniform_spacer);
        }
        if item.expansive {
            summary.expanding_count += 1;
        }
        summary.all_empty_nonstretch =
            summary.all_empty_nonstretch && item.empty && !item.expansive && item.stretch <= 0;
    }
    summary
}

/// Folds one box into the maximum size, expanding flag, and emptiness of a row or column.
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

fn distribute_underflow(
    chain: &mut [LayoutStruct],
    space: i32,
    spacer: &mut i32,
    summary: &mut ChainSummary,
) {
    let min_total = summary.total_min + summary.total_spacing;
    if *spacer >= 0 {
        *spacer = if min_total > 0 {
            *spacer * space / min_total
        } else {
            0
        };
        summary.total_spacing = *spacer * summary.spacer_count;
    }

    let mut mins: Vec<i32> = chain.iter().map(|item| item.minimum_size).collect();
    mins.sort_unstable();

    let count = chain.len() as i32;
    let available = space - summary.total_spacing;
    let mut prefix_sum = 0i32;
    let mut idx = 0i32;
    let mut used = 0i32;
    let mut cap = 0i32;
    while (idx as usize) < chain.len() && used < available {
        cap = mins[idx as usize];
        used = prefix_sum + cap * (count - idx);
        prefix_sum += cap;
        idx += 1;
    }
    idx -= 1;

    let items = count - idx;
    let excess = used - available;
    let base_cap = cap - excess / items;
    let extra_cut = excess % items;

    let mut acc = 0i32;
    for item in chain.iter_mut() {
        let mut limit = base_cap;
        acc += extra_cut;
        if acc >= items {
            limit -= 1;
            acc -= items;
        }
        item.size = item.minimum_size.min(limit);
        item.done = true;
    }
}

fn distribute_compression(chain: &mut [LayoutStruct], space: i32, total_spacing: i32) {
    let mut active = chain.len() as i64;
    let mut available = space - total_spacing;
    let mut overdraft = 0i64;

    for item in chain.iter_mut() {
        let pref = item.smart_size_hint();
        if item.minimum_size >= pref {
            item.size = pref;
            item.done = true;
            available -= pref;
            active -= 1;
        } else {
            overdraft += pref as i64;
        }
    }
    overdraft -= available as i64;

    loop {
        if active <= 0 {
            break;
        }
        let mut converged = true;
        let mut acc_num: i64 = 0;
        for item in chain.iter_mut() {
            if item.done {
                continue;
            }
            acc_num += overdraft;
            let cut = ((acc_num + active / 2) / active) as i32;
            acc_num -= cut as i64 * active;
            let pref = item.smart_size_hint();
            item.size = pref - cut;
            if item.size < item.minimum_size {
                item.done = true;
                item.size = item.minimum_size;
                converged = false;
                overdraft -= (pref - item.minimum_size) as i64;
                active -= 1;
                break;
            }
        }
        if converged {
            break;
        }
    }
}

fn distribute_expansion(chain: &mut [LayoutStruct], space: i32, summary: &ChainSummary) -> i32 {
    let mut active = chain.len() as i32;
    let mut available = space - summary.total_spacing;
    let mut sum_stretch = summary.total_stretch;
    let mut expanding_count = summary.expanding_count;

    for item in chain.iter_mut() {
        let pref = item.smart_size_hint();
        let non_growing_empty =
            !summary.all_empty_nonstretch && item.empty && !item.expansive && item.stretch == 0;
        if item.maximum_size <= pref || non_growing_empty {
            item.size = pref;
            item.done = true;
            available -= pref;
            sum_stretch -= item.stretch;
            if item.expansive {
                expanding_count -= 1;
            }
            active -= 1;
        }
    }

    let mut leftover = available;
    loop {
        let mut surplus = 0i32;
        let mut deficit = 0i32;
        let mut acc_num: i64 = 0;
        let denom: i64 = if sum_stretch > 0 {
            sum_stretch as i64
        } else if expanding_count > 0 {
            expanding_count as i64
        } else {
            active.max(1) as i64
        };

        for item in chain.iter_mut() {
            if item.done {
                continue;
            }
            leftover = 0;
            let weight: i64 = if sum_stretch > 0 {
                item.stretch as i64
            } else if expanding_count > 0 {
                if item.expansive { 1 } else { 0 }
            } else {
                1
            };
            acc_num += available as i64 * weight;
            let share = ((acc_num + denom / 2) / denom) as i32;
            acc_num -= share as i64 * denom;
            item.size = share;
            if share < item.smart_size_hint() {
                deficit += item.smart_size_hint() - share;
            } else if share > item.maximum_size {
                surplus += share - item.maximum_size;
            }
        }

        if deficit > 0 && surplus <= deficit {
            for item in chain.iter_mut() {
                if !item.done && item.size < item.smart_size_hint() {
                    item.size = item.smart_size_hint();
                    item.done = true;
                    available -= item.size;
                    sum_stretch -= item.stretch;
                    if item.expansive {
                        expanding_count -= 1;
                    }
                    active -= 1;
                }
            }
        }
        if surplus > 0 && surplus >= deficit {
            for item in chain.iter_mut() {
                if !item.done && item.size > item.maximum_size {
                    item.size = item.maximum_size;
                    item.done = true;
                    available -= item.size;
                    sum_stretch -= item.stretch;
                    if item.expansive {
                        expanding_count -= 1;
                    }
                    active -= 1;
                }
            }
        }
        if active <= 0 || surplus == deficit {
            break;
        }
    }

    if active == 0 { available } else { leftover }
}

fn assign_positions(
    chain: &mut [LayoutStruct],
    pos: i32,
    spacer: i32,
    spacer_count: i32,
    leftover: i32,
) {
    let extra_gap = leftover / (spacer_count + 2);
    let mut cursor = pos + extra_gap;
    for item in chain.iter_mut() {
        item.pos = cursor;
        cursor += item.size;
        if !item.empty {
            cursor += item.effective_spacer(spacer) + extra_gap;
        }
    }
}

/// Distributes `space` pixels starting at `pos` among `count` entries of `chain` beginning at `start`.
pub fn q_geom_calc(
    chain: &mut [LayoutStruct],
    start: usize,
    count: usize,
    pos: i32,
    space: i32,
    spacer: i32,
) {
    let slice = &mut chain[start..start + count];
    let mut spacer = spacer;
    let mut summary = summarize_chain(slice, spacer);

    let leftover = if space < summary.total_min + summary.total_spacing {
        distribute_underflow(slice, space, &mut spacer, &mut summary);
        0
    } else if space < summary.total_preferred + summary.total_spacing {
        distribute_compression(slice, space, summary.total_spacing);
        0
    } else {
        distribute_expansion(slice, space, &summary)
    };

    assign_positions(slice, pos, spacer, summary.spacer_count, leftover);
}

/// axis expansion flag of one axis.
fn expand_flag(policy: Policy) -> bool {
    matches!(policy, Policy::Expanding | Policy::MinimumExpanding)
}

/// Effective minimum size for layout (effective minimum size, combining explicit and styled limits).
pub fn effective_minimum_size(widget: &dyn Widget) -> Size {
    widget.minimum_size()
}

/// Effective maximum size for layout (effective maximum size, combining explicit and styled limits).
pub fn effective_maximum_size(widget: &dyn Widget) -> Size {
    widget.maximum_size()
}

/// `smart_min_size` for `widget`.
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

/// `smart_max_size` for `widget`: an aligned axis
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

/// widget item emptiness: a hidden widget takes no part in the layout,
/// unless its size policy retains its size when hidden.
pub fn item_is_empty(widget: &dyn Widget) -> bool {
    !widget.is_visible() && !widget.size_policy().retain_size_when_hidden
}

/// the style in
/// the application's layout direction, which every widget has here.
pub fn visual_alignment(align: Alignment) -> Alignment {
    align.visual(GuiApplication::layout_direction())
}

/// aligned layout rectangle placement: the rectangle an aligned layout lays its
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

/// widget item size hint.
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

/// `QWidgetItem::heightForWidth`: the widget's height for `width`, kept
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

/// `QWidgetItem::setGeometry` for a left-to-right layout.
///
/// The widget gets the rectangle bounded by the item's maximum size; on an aligned axis it is
/// also cut down to the item's size hint and placed inside the rectangle. A missing horizontal
/// alignment means `AlignLeft` (the style), a missing vertical one centres. A
/// hidden widget keeps no geometry. A vertically aligned widget is cut to its height for its width
/// when it has `heightForWidth`, else to its size hint height.
pub fn item_set_geometry(widget: &dyn Widget, rect: Rect, align: Alignment) {
    item_set_geometry_with(widget, rect, align, ItemLimits::of(widget, align));
}

/// The size limits `item_set_geometry` applies: the item's `smart_max_size`
/// (`QWidgetItem::maximumSize`), the widget's own minimum and maximum size, which
/// the widget bounds the result by (7300-7305), and, for an aligned
/// item, the size it is cut to on an aligned axis (`QWidgetItem::setGeometry`,
/// ).
///
/// They depend only on the widget's size metrics, so a layout computes them with the rest of its
/// metric cache and reuses them for every geometry pass until it is invalidated, as
/// `QWidgetItemV2` caches the item's size hint.
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
        // Qt `QWidgetItem::setGeometry` returns early for an empty item.
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
    // `QWidgetItem::setGeometry` decides on the visual alignment.
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
    // the widget keeps the size within the widget's own minimum and maximum size.
    let width = width
        .min(limits.widget_max.width)
        .max(limits.widget_min.width);
    let height = height
        .min(limits.widget_max.height)
        .max(limits.widget_min.height);
    widget.set_geometry(Rect::new(x, y, width, height));
}

/// grid layout internals: the sum of one field of every row and of every column, each
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
        // At least one maximum size must grow; `q_geom_calc` puts the extra space in between the
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

/// grid layout internals with a fixed spacing: the spacing after a non-empty row
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
