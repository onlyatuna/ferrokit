// SPDX-License-Identifier: LGPL-3.0-only
//! 1D/2D widget layout constraint solver and item sizing helpers.

use crate::layout::Alignment;
use crate::size_policy::Policy;
use crate::widget::Widget;
use ferrokit_gui::application::GuiApplication;
use ferrokit_gui::geometry::primitives::{Rect, Size};

pub const LAYOUT_SIZE_MAX: i32 = i32::MAX / 256 / 16;
pub const WIDGET_SIZE_MAX: i32 = (1 << 24) - 1;
pub const WIDGET_LAYOUT_SPACING: i32 = 6;
pub const WINDOW_LAYOUT_MARGIN: i32 = 11;
pub const CHILD_LAYOUT_MARGIN: i32 = 9;

pub fn owner_layout_margin(has_parent: bool) -> i32 {
    if has_parent {
        CHILD_LAYOUT_MARGIN
    } else {
        WINDOW_LAYOUT_MARGIN
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LayoutStruct {
    pub stretch: i32,
    pub size_hint: i32,
    pub maximum_size: i32,
    pub minimum_size: i32,
    pub spacing: i32,
    pub expansive: bool,
    pub empty: bool,
    pub pos: i32,
    pub size: i32,
}

impl LayoutStruct {
    pub fn init(&mut self, stretch: i32, min_size: i32) {
        self.stretch = stretch;
        self.minimum_size = min_size;
        self.size_hint = min_size;
        self.maximum_size = LAYOUT_SIZE_MAX;
        self.expansive = false;
        self.empty = true;
        self.spacing = 0;
    }

    fn preferred(&self) -> i32 {
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

pub fn q_geom_calc(
    chain: &mut [LayoutStruct],
    start: usize,
    count: usize,
    pos: i32,
    space: i32,
    spacer: i32,
) {
    if count == 0 || start >= chain.len() {
        return;
    }
    let slice = &mut chain[start..start + count];

    let last_non_empty = slice.iter().rposition(|item| !item.empty);
    let mut spacer_count: i64 = 0;
    let mut sum_spacing: i64 = 0;
    let mut c_min: i64 = 0;
    let mut c_hint: i64 = 0;
    let mut sum_stretch: i64 = 0;
    let mut expanding_count: usize = 0;
    let mut all_empty_nonstretch = true;

    for (i, item) in slice.iter().enumerate() {
        c_min += item.minimum_size as i64;
        c_hint += item.preferred() as i64;
        sum_stretch += item.stretch as i64;
        if item.expansive {
            expanding_count += 1;
        }
        if !(item.empty && !item.expansive && item.stretch <= 0) {
            all_empty_nonstretch = false;
        }
        if !item.empty && Some(i) != last_non_empty {
            spacer_count += 1;
            sum_spacing += item.effective_spacer(spacer) as i64;
        }
    }

    let space_i64 = space as i64;
    let mut effective_spacer_arg = spacer;
    let mut extra: i32 = 0;

    if space_i64 < c_min + sum_spacing {
        // Regime A: below minimum size
        if spacer >= 0 {
            let min_total = c_min + sum_spacing;
            let scaled = if min_total > 0 {
                ((spacer as i64 * space_i64) / min_total) as i32
            } else {
                0
            };
            effective_spacer_arg = scaled;
            sum_spacing = scaled as i64 * spacer_count;
        }

        let available = space_i64 - sum_spacing;
        let (current, excess, items_at_or_above) = if available <= 0 {
            (0i32, -available, count as i64 + 1)
        } else {
            let mut sorted_mins: Vec<i32> = slice.iter().map(|item| item.minimum_size).collect();
            sorted_mins.sort_unstable();

            let mut cur = sorted_mins[0];
            let mut used = 0i64;
            let mut prefix_sum = 0i64;
            let mut idx = 0usize;
            while idx < count && used < available {
                cur = sorted_mins[idx];
                used = prefix_sum + cur as i64 * (count - idx) as i64;
                prefix_sum += cur as i64;
                idx += 1;
            }
            let items = (count - (idx - 1)) as i64;
            (cur, used - available, items)
        };

        let base_cut = if items_at_or_above > 0 {
            excess / items_at_or_above
        } else {
            0
        };
        let rem = if items_at_or_above > 0 {
            excess % items_at_or_above
        } else {
            0
        };
        let mut rem_acc = 0i64;

        for item in slice.iter_mut() {
            rem_acc += rem;
            let extra_cut = if rem_acc >= items_at_or_above {
                rem_acc -= items_at_or_above;
                1
            } else {
                0
            };
            let cap = (current as i64 - base_cut - extra_cut) as i32;
            item.size = item.minimum_size.min(cap);
        }
    } else if space_i64 < c_hint + sum_spacing {
        // Regime B: between minimum size and preferred size
        let available = space_i64 - sum_spacing;
        let mut pinned = vec![false; count];
        for (i, item) in slice.iter_mut().enumerate() {
            let pref = item.preferred();
            item.size = pref;
            if item.minimum_size >= pref {
                pinned[i] = true;
            }
        }

        loop {
            let mut unpinned_count: i64 = 0;
            let mut total_current: i64 = 0;
            for (i, item) in slice.iter().enumerate() {
                if pinned[i] {
                    total_current += item.size as i64;
                } else {
                    total_current += item.preferred() as i64;
                    unpinned_count += 1;
                }
            }
            let overdraft = total_current - available;
            if unpinned_count == 0 || overdraft <= 0 {
                break;
            }

            const SUBPIXEL: i64 = 256;
            let fp_step = (overdraft * SUBPIXEL) / unpinned_count;
            let mut fp_acc: i64 = 0;
            let mut any_newly_pinned = false;
            let mut provisional = vec![0i32; count];

            for (i, item) in slice.iter().enumerate() {
                if pinned[i] {
                    continue;
                }
                fp_acc += fp_step;
                let cut = if fp_acc >= 0 {
                    (fp_acc + SUBPIXEL / 2) / SUBPIXEL
                } else {
                    -(((-fp_acc) + SUBPIXEL / 2 - 1) / SUBPIXEL)
                };
                fp_acc -= cut * SUBPIXEL;
                let new_size = (item.preferred() as i64 - cut) as i32;
                if new_size < item.minimum_size {
                    pinned[i] = true;
                    any_newly_pinned = true;
                    break;
                } else {
                    provisional[i] = new_size;
                }
            }

            if any_newly_pinned {
                for (i, item) in slice.iter_mut().enumerate() {
                    if pinned[i]
                        && item.size > item.minimum_size
                        && item.minimum_size < item.preferred()
                    {
                        item.size = item.minimum_size;
                    }
                }
            } else {
                for (i, item) in slice.iter_mut().enumerate() {
                    if !pinned[i] {
                        item.size = provisional[i];
                    }
                }
                break;
            }
        }
    } else {
        // Regime C: at or above preferred size
        let available = space_i64 - sum_spacing;
        let mut pinned = vec![false; count];
        for (i, item) in slice.iter_mut().enumerate() {
            let pref = item.preferred();
            let should_pin = item.maximum_size <= pref
                || (!all_empty_nonstretch && item.empty && !item.expansive && item.stretch == 0);
            if should_pin {
                pinned[i] = true;
                item.size = pref;
            }
        }

        let mut space_left = available;
        for (i, item) in slice.iter().enumerate() {
            if pinned[i] {
                space_left -= item.size as i64;
            }
        }

        loop {
            let active_count = pinned.iter().filter(|&&p| !p).count();
            if active_count == 0 {
                let extraspace = space_left;
                if extraspace > 0 {
                    extra = (extraspace / (spacer_count + 2)) as i32;
                }
                break;
            }

            let active_stretch: i64 = slice
                .iter()
                .enumerate()
                .filter(|&(i, _)| !pinned[i])
                .map(|(_, item)| item.stretch.max(0) as i64)
                .sum();
            let active_expanding = slice
                .iter()
                .enumerate()
                .filter(|&(i, item)| !pinned[i] && item.expansive)
                .count();

            let use_stretch = sum_stretch > 0 && active_stretch > 0;
            let use_expanding = !use_stretch && expanding_count > 0 && active_expanding > 0;

            let weights: Vec<i64> = slice
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    if pinned[i] {
                        0
                    } else if use_stretch {
                        item.stretch.max(0) as i64
                    } else if use_expanding {
                        if item.expansive { 1 } else { 0 }
                    } else {
                        1
                    }
                })
                .collect();

            let total_weight: i64 = weights.iter().sum();
            const SUBPIXEL: i64 = 256;
            let fp_space = space_left * SUBPIXEL;
            let mut fp_acc: i64 = 0;
            let mut provisional = vec![0i32; count];
            let mut deficit: i64 = 0;
            let mut surplus: i64 = 0;

            for (i, item) in slice.iter().enumerate() {
                if pinned[i] {
                    continue;
                }
                fp_acc += (fp_space * weights[i]) / total_weight;
                let alloc = (if fp_acc >= 0 {
                    (fp_acc + SUBPIXEL / 2) / SUBPIXEL
                } else {
                    -(((-fp_acc) + SUBPIXEL / 2 - 1) / SUBPIXEL)
                }) as i32;
                fp_acc -= alloc as i64 * SUBPIXEL;
                provisional[i] = alloc;

                let pref = item.preferred();
                if alloc < pref {
                    deficit += (pref - alloc) as i64;
                }
                if alloc > item.maximum_size {
                    surplus += (alloc - item.maximum_size) as i64;
                }
            }

            if deficit == surplus {
                for (i, item) in slice.iter_mut().enumerate() {
                    if !pinned[i] {
                        item.size = provisional[i];
                    }
                }
                break;
            }

            let pin_under = deficit > 0 && surplus <= deficit;
            let pin_over = surplus > 0 && surplus >= deficit;

            for (i, item) in slice.iter_mut().enumerate() {
                if pinned[i] {
                    continue;
                }
                let pref = item.preferred();
                if pin_under && provisional[i] < pref {
                    pinned[i] = true;
                    item.size = pref;
                    space_left -= pref as i64;
                } else if pin_over && provisional[i] > item.maximum_size {
                    pinned[i] = true;
                    item.size = item.maximum_size;
                    space_left -= item.maximum_size as i64;
                }
            }
        }
    }

    let mut p = pos + extra;
    for item in slice.iter_mut() {
        item.pos = p;
        p += item.size;
        if !item.empty {
            p += item.effective_spacer(effective_spacer_arg) + extra;
        }
    }
}

pub fn effective_minimum_size(widget: &dyn Widget) -> Size {
    widget.minimum_size()
}

pub fn effective_maximum_size(widget: &dyn Widget) -> Size {
    widget.maximum_size()
}

pub fn smart_min_size(widget: &dyn Widget) -> Size {
    let policy = widget.size_policy();
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);
    let max = effective_maximum_size(widget);

    let axis = |p: Policy, h: i32, mh: i32, mn: i32, mx: i32| -> i32 {
        let mut s = match p {
            Policy::Ignored => 0,
            _ if p.can_shrink() => mh,
            _ => h.max(mh),
        };
        s = s.min(mx);
        if mn > 0 {
            s = mn;
        }
        s.max(0)
    };

    Size::new(
        axis(
            policy.horizontal,
            hint.width,
            min_hint.width,
            min.width,
            max.width,
        ),
        axis(
            policy.vertical,
            hint.height,
            min_hint.height,
            min.height,
            max.height,
        ),
    )
}

pub fn smart_max_size(widget: &dyn Widget, align: Alignment) -> Size {
    if align.horizontal() && align.vertical() {
        return Size::new(LAYOUT_SIZE_MAX, LAYOUT_SIZE_MAX);
    }

    let mut max = effective_maximum_size(widget);
    let policy = widget.size_policy();
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);

    if align.horizontal() {
        max.width = LAYOUT_SIZE_MAX;
    } else if max.width == WIDGET_SIZE_MAX && !policy.horizontal.can_grow() {
        max.width = hint.width.max(min_hint.width).max(min.width);
    }

    if align.vertical() {
        max.height = LAYOUT_SIZE_MAX;
    } else if max.height == WIDGET_SIZE_MAX && !policy.vertical.can_grow() {
        max.height = hint.height.max(min_hint.height).max(min.height);
    }

    max
}

pub fn item_is_empty(widget: &dyn Widget) -> bool {
    !widget.is_visible() && !widget.size_policy().retain_size_when_hidden
}

pub fn visual_alignment(align: Alignment) -> Alignment {
    align.visual(GuiApplication::layout_direction())
}

pub fn alignment_rect(
    rect: Rect,
    align: Alignment,
    hint: Size,
    max: Size,
    expanding: (bool, bool),
    height_for_width: Option<&dyn Fn(i32) -> i32>,
) -> Rect {
    let mut s = Size::new(rect.width.min(max.width), rect.height.min(max.height));

    if align.horizontal() && !expanding.0 {
        s.width = hint.width.min(s.width).min(max.width);
    }
    if align.vertical() && !expanding.1 {
        if let Some(hfw) = height_for_width {
            s.height = hfw(s.width).min(s.height).min(max.height);
        } else {
            s.height = hint.height.min(s.height).min(max.height);
        }
    }

    let a = visual_alignment(align);
    let x = if a.contains(Alignment::RIGHT) {
        rect.x + rect.width - s.width
    } else if a.contains(Alignment::LEFT) {
        rect.x
    } else {
        rect.x + (rect.width - s.width) / 2
    };

    let y = if a.contains(Alignment::BOTTOM) {
        rect.y + rect.height - s.height
    } else if a.contains(Alignment::TOP) {
        rect.y
    } else {
        rect.y + (rect.height - s.height) / 2
    };

    Rect::new(x, y, s.width, s.height)
}

pub fn item_size_hint(widget: &dyn Widget) -> Size {
    if item_is_empty(widget) {
        return Size::new(0, 0);
    }
    let policy = widget.size_policy();
    let hint = widget.size_hint();
    let min_hint = widget.minimum_size_hint();
    let min = effective_minimum_size(widget);
    let max = effective_maximum_size(widget);

    let axis = |p: Policy, h: i32, mh: i32, mn: i32, mx: i32| -> i32 {
        if p == Policy::Ignored {
            0
        } else {
            h.max(mh).min(mx).max(mn)
        }
    };

    Size::new(
        axis(
            policy.horizontal,
            hint.width,
            min_hint.width,
            min.width,
            max.width,
        ),
        axis(
            policy.vertical,
            hint.height,
            min_hint.height,
            min.height,
            max.height,
        ),
    )
}

pub fn item_minimum_size(widget: &dyn Widget) -> Size {
    if item_is_empty(widget) {
        Size::new(0, 0)
    } else {
        smart_min_size(widget)
    }
}

pub fn item_maximum_size(widget: &dyn Widget, align: Alignment) -> Size {
    if item_is_empty(widget) {
        Size::new(0, 0)
    } else {
        smart_max_size(widget, align)
    }
}

pub fn item_has_height_for_width(widget: &dyn Widget) -> bool {
    !item_is_empty(widget) && widget.has_height_for_width()
}

pub fn item_height_for_width(widget: &dyn Widget, width: i32) -> i32 {
    if item_is_empty(widget) {
        return -1;
    }
    let mut h = widget.height_for_width(width);
    let min_h = effective_minimum_size(widget).height;
    let max_h = effective_maximum_size(widget).height;
    if h > max_h {
        h = max_h;
    }
    if h < min_h {
        h = min_h;
    }
    h
}

pub fn item_expanding(widget: &dyn Widget, align: Alignment) -> (bool, bool) {
    if item_is_empty(widget) {
        return (false, false);
    }
    let policy = widget.size_policy();
    let expand_flag = |p: Policy| matches!(p, Policy::Expanding | Policy::MinimumExpanding);
    let mut horiz = expand_flag(policy.horizontal);
    let mut vert = expand_flag(policy.vertical);

    if let Some(layout) = widget.layout_ref_mut() {
        let (lh, lv) = layout.expanding_directions();
        if policy.horizontal.can_grow() {
            horiz = horiz || lh;
        }
        if policy.vertical.can_grow() {
            vert = vert || lv;
        }
    }

    if align.horizontal() {
        horiz = false;
    }
    if align.vertical() {
        vert = false;
    }

    (horiz, vert)
}

pub fn item_set_geometry(widget: &dyn Widget, rect: Rect, align: Alignment) {
    if item_is_empty(widget) {
        return;
    }
    let limits = ItemLimits::of(widget, align);
    item_set_geometry_with(widget, rect, align, limits);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLimits {
    pub item_max: Size,
    pub widget_min: Size,
    pub widget_max: Size,
    pub aligned_pref: Size,
}

impl ItemLimits {
    pub fn of(widget: &dyn Widget, align: Alignment) -> Self {
        let policy = widget.size_policy();
        let hint = widget.size_hint();
        let widget_min = effective_minimum_size(widget);
        let item_hint = item_size_hint(widget);
        let aligned_pref = Size::new(
            if policy.horizontal == Policy::Ignored {
                hint.width.max(widget_min.width).max(0)
            } else {
                item_hint.width
            },
            if policy.vertical == Policy::Ignored {
                hint.height.max(widget_min.height).max(0)
            } else {
                item_hint.height
            },
        );
        Self {
            item_max: item_maximum_size(widget, align),
            widget_min,
            widget_max: effective_maximum_size(widget),
            aligned_pref,
        }
    }
}

pub fn item_set_geometry_with(
    widget: &dyn Widget,
    rect: Rect,
    align: Alignment,
    limits: ItemLimits,
) {
    if item_is_empty(widget) {
        return;
    }

    let bounded_w = rect.width.min(limits.item_max.width);
    let bounded_h = rect.height.min(limits.item_max.height);

    let mut w = if align.horizontal() {
        bounded_w.min(limits.aligned_pref.width)
    } else {
        bounded_w
    };

    let mut h = if align.vertical() {
        if item_has_height_for_width(widget) {
            let hfw = item_height_for_width(widget, w);
            if hfw >= 0 {
                bounded_h.min(hfw)
            } else {
                bounded_h.min(limits.aligned_pref.height)
            }
        } else {
            bounded_h.min(limits.aligned_pref.height)
        }
    } else {
        bounded_h
    };

    let a = visual_alignment(align);
    let mut x = if a.contains(Alignment::RIGHT) {
        rect.x + rect.width - w
    } else if !a.contains(Alignment::LEFT) {
        rect.x + (rect.width - w) / 2
    } else {
        rect.x
    };

    let mut y = if a.contains(Alignment::BOTTOM) {
        rect.y + rect.height - h
    } else if !a.contains(Alignment::TOP) {
        rect.y + (rect.height - h) / 2
    } else {
        rect.y
    };

    if x < 0 {
        w += x;
        x = 0;
    }
    if y < 0 {
        h += y;
        y = 0;
    }

    let clamp_axis = |val: i32, mn: i32, mx: i32| -> i32 {
        let mut v = val;
        if v > mx {
            v = mx;
        }
        if v < mn {
            v = mn;
        }
        v
    };

    w = clamp_axis(w, limits.widget_min.width, limits.widget_max.width);
    h = clamp_axis(h, limits.widget_min.height, limits.widget_max.height);

    widget.set_geometry(Rect::new(x, y, w, h));
}

pub fn find_size(
    rows: &[LayoutStruct],
    cols: &[LayoutStruct],
    field: impl Fn(&LayoutStruct) -> i32,
) -> Size {
    let sum_axis = |tracks: &[LayoutStruct]| -> i32 {
        let total: i64 = tracks
            .iter()
            .map(|t| field(t) as i64 + t.spacing as i64)
            .sum();
        total.min(LAYOUT_SIZE_MAX as i64) as i32
    };
    Size::new(sum_axis(cols), sum_axis(rows))
}

pub fn init_empty_multi_box(chain: &mut [LayoutStruct], start: usize, end: usize) {
    for item in &mut chain[start..=end] {
        if item.empty && item.maximum_size == 0 {
            item.maximum_size = WIDGET_SIZE_MAX;
        }
        item.empty = false;
    }
}

pub fn distribute_multi_box(
    chain: &mut [LayoutStruct],
    start: usize,
    end: usize,
    min_size: i32,
    size_hint: i32,
    stretch_array: &[u32],
    stretch: i32,
) {
    let count = end - start + 1;
    let mut min_total = 0i32;
    let mut pref_total = 0i32;
    let mut max_total = 0i32;
    let mut sum_spacing = 0i32;

    for (i, item) in chain[start..=end].iter_mut().enumerate() {
        let idx = start + i;
        if stretch_array.get(idx).copied().unwrap_or(0) == 0 {
            item.stretch = item.stretch.max(stretch);
        }
        min_total = min_total.saturating_add(item.minimum_size);
        pref_total = pref_total.saturating_add(item.size_hint);
        max_total = max_total.saturating_add(item.maximum_size);
        if idx < end {
            sum_spacing = sum_spacing.saturating_add(item.spacing);
        }
    }
    min_total = min_total.saturating_add(sum_spacing);
    pref_total = pref_total.saturating_add(sum_spacing);
    max_total = max_total.saturating_add(sum_spacing);

    if max_total < min_size {
        q_geom_calc(chain, start, count, 0, min_size, -1);
        for i in start..=end {
            let item_start = if i == start { 0 } else { chain[i].pos };
            let item_end = if i == end {
                min_size
            } else {
                chain[i + 1].pos - chain[i].spacing
            };
            let absorbed = (item_end - item_start).max(chain[i].size);
            chain[i].minimum_size = chain[i].minimum_size.max(absorbed);
            chain[i].maximum_size = chain[i].maximum_size.max(absorbed);
        }
    } else if min_total < min_size {
        q_geom_calc(chain, start, count, 0, min_size, -1);
        for item in &mut chain[start..=end] {
            item.minimum_size = item.minimum_size.max(item.size);
        }
    }

    if pref_total < size_hint {
        q_geom_calc(chain, start, count, 0, size_hint, -1);
        for item in &mut chain[start..=end] {
            item.size_hint = item.size_hint.max(item.size);
        }
    }
}

pub fn setup_spacings(
    chain: &mut [LayoutStruct],
    grid: &[Option<usize>],
    cc: usize,
    fixed_spacing: i32,
    horizontal: bool,
    empty_of: &dyn Fn(usize) -> bool,
) {
    if cc == 0 || grid.is_empty() {
        return;
    }
    let rr = chain.len();
    let outer = if horizontal { grid.len() / cc } else { cc };

    for o in 0..outer {
        let mut prev_track: Option<usize> = None;
        let mut prev_item: Option<usize> = None;
        for t in 0..rr {
            if chain[t].empty {
                continue;
            }
            let idx = if horizontal { o * cc + t } else { t * cc + o };
            let current = grid[idx];
            if let Some(pt) = prev_track
                && (current.is_none() || prev_item != current)
            {
                let sp = match current {
                    Some(s) if !horizontal && empty_of(s) => 0,
                    _ => fixed_spacing,
                };
                chain[pt].spacing = chain[pt].spacing.max(sp);
            }
            prev_track = Some(t);
            prev_item = current;
        }
    }
}
