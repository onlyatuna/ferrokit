// SPDX-License-Identifier: LGPL-3.0-only
//! Shared probe for the layout port: parses one layout line, lays it out and reports the
//! rectangle of every widget item. Used by `examples/layout_probe.rs` (stdin to stdout, compared
//! with PySide6 by an external script) and by the in-process
//! oracle replay in `tests/layout_qt_oracle.rs`.
//!
//! Line format: `head | items`.
//! - `head` = `kind spacing margin width height [opt ...]`; `kind` is `H`, `V`, `G<columns>` or `K`
//!   (`QStackedLayout`, widget items only). Options: `m=L,T,R,B` (margins), `hs=N` / `vs=N` (grid
//!   horizontal / vertical spacing), `rmin=ROW:H` (row minimum height) and `cmin=COL:W` (column minimum
//!   width); `rmin` and `cmin` may repeat. `um` leaves the margins unset (the owner's default applies)
//!   and `us` leaves the spacing unset; the positional margin or spacing is then ignored. `own=W` makes
//!   the container a window (no parent widget), `own=C` (the default) a child of a parent widget.
//!   `at=X,Y` is the container's offset in its parent. A nested layout writes `width height` as `0 0`;
//!   they are ignored.
//! - `items` are separated by `;` at brace depth 0. An item is one of:
//!   - a widget: `hint_w,hint_h,min_w,min_h,max_w,max_h,policy_h,policy_v,stretch,hidden,row,
//!     column,row_span,column_span,row_stretch,alignment[,minimum_hint_w,minimum_hint_h]`.
//!     `alignment` is Qt's numeric `Qt::Alignment`; the minimum size hint defaults to `0,0`.
//!     `stretch` is the column stretch of the item's column when it is in row 0 of a grid.
//!   - a spacer (box only): `S<stretch>` is `addStretch(stretch)`.
//!   - a nested box (box only): `{head | items}:<stretch>` is `addLayout(child, stretch)`.
//!
//! The output lists the rectangles of all widget items depth-first in item order, hidden ones
//! included.

#![allow(dead_code)]

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Margins, Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

/// `x, y, width, height` of one widget item.
pub type Geom = (i32, i32, i32, i32);

struct Probe {
    base: WidgetBase,
    hint: Size,
    min: Size,
    max: Size,
    minimum_hint: Size,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

impl Widget for Probe {
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
    fn size_hint(&self) -> Size {
        self.hint
    }
    fn minimum_size(&self) -> Size {
        self.min
    }
    fn minimum_size_hint(&self) -> Size {
        self.minimum_hint
    }
    fn maximum_size(&self) -> Size {
        self.max
    }
    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }
    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }
    fn is_enabled(&self) -> bool {
        true
    }
    fn update(&self) {}
    fn dirty_rect(&self) -> Option<Rect> {
        None
    }
    fn clear_dirty(&self) {}
    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }
    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }
    fn window_id(&self) -> Option<ObjectId> {
        None
    }
    fn set_window_id(&self, _window_id: Option<ObjectId>) {}
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

fn policy(name: &str) -> Policy {
    match name {
        "Fixed" => Policy::Fixed,
        "Minimum" => Policy::Minimum,
        "Maximum" => Policy::Maximum,
        "Preferred" => Policy::Preferred,
        "Expanding" => Policy::Expanding,
        "MinimumExpanding" => Policy::MinimumExpanding,
        "Ignored" => Policy::Ignored,
        other => panic!("unknown policy {other}"),
    }
}

/// Parsed `head` of a layout.
struct Head {
    kind: char,
    /// `None` when `us` leaves the spacing unset.
    spacing: Option<i32>,
    /// `left, top, right, bottom`; `None` when `um` leaves the margins unset.
    margins: Option<[i32; 4]>,
    width: i32,
    height: i32,
    h_spacing: Option<i32>,
    v_spacing: Option<i32>,
    row_min: Vec<(usize, i32)>,
    col_min: Vec<(usize, i32)>,
    /// `own=W`: the container is a window, with no parent widget.
    window: bool,
    /// `at=X,Y`: the container's offset in its parent.
    offset: (i32, i32),
}

/// One widget item with its layout placement.
struct WidgetSpec {
    hint: Size,
    min: Size,
    max: Size,
    policy: (Policy, Policy),
    minimum_hint: Size,
    stretch: u32,
    hidden: bool,
    row: usize,
    column: usize,
    row_span: usize,
    col_span: usize,
    row_stretch: u32,
    alignment: Alignment,
}

enum Item {
    Widget(WidgetSpec),
    Spacer(u32),
    Nested(LayoutSpec, u32),
}

struct LayoutSpec {
    head: Head,
    items: Vec<Item>,
}

/// Splits `s` at `sep` characters that are outside `{ }`.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn next_int(tokens: &mut std::str::SplitWhitespace) -> i32 {
    tokens.next().unwrap().parse().unwrap()
}

fn parse_head(head: &str) -> Head {
    let mut tokens = head.split_whitespace();
    let kind = tokens.next().unwrap().chars().next().unwrap();
    let spacing = next_int(&mut tokens);
    let margin = next_int(&mut tokens);
    let width = next_int(&mut tokens);
    let height = next_int(&mut tokens);
    let mut parsed = Head {
        kind,
        spacing: Some(spacing),
        margins: Some([margin; 4]),
        width,
        height,
        h_spacing: None,
        v_spacing: None,
        row_min: Vec::new(),
        col_min: Vec::new(),
        window: false,
        offset: (0, 0),
    };
    for opt in tokens {
        match opt {
            "um" => parsed.margins = None,
            "us" => parsed.spacing = None,
            _ => {
                let (key, value) = opt.split_once('=').expect("option key=value");
                match key {
                    "m" => {
                        let v: Vec<i32> = value.split(',').map(|x| x.parse().unwrap()).collect();
                        parsed.margins = Some([v[0], v[1], v[2], v[3]]);
                    }
                    "hs" => parsed.h_spacing = Some(value.parse().unwrap()),
                    "vs" => parsed.v_spacing = Some(value.parse().unwrap()),
                    "rmin" => {
                        let (r, h) = value.split_once(':').expect("rmin=ROW:H");
                        parsed
                            .row_min
                            .push((r.parse().unwrap(), h.parse().unwrap()));
                    }
                    "cmin" => {
                        let (c, w) = value.split_once(':').expect("cmin=COL:W");
                        parsed
                            .col_min
                            .push((c.parse().unwrap(), w.parse().unwrap()));
                    }
                    "own" => {
                        parsed.window = match value {
                            "W" => true,
                            "C" => false,
                            other => panic!("unknown owner {other}"),
                        }
                    }
                    "at" => {
                        let (x, y) = value.split_once(',').expect("at=X,Y");
                        parsed.offset = (x.parse().unwrap(), y.parse().unwrap());
                    }
                    other => panic!("unknown layout option {other}"),
                }
            }
        }
    }
    parsed
}

fn parse_layout(line: &str) -> LayoutSpec {
    let (head, items) = line.split_once('|').expect("head | items");
    let items = split_top(items, ';')
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_item)
        .collect();
    LayoutSpec {
        head: parse_head(head),
        items,
    }
}

/// Index of the `}` matching an already-consumed `{` (`s` starts just after that `{`).
fn matching_brace(s: &str) -> usize {
    let mut depth = 1i32;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    panic!("unclosed nested layout");
}

fn parse_item(item: &str) -> Item {
    if let Some(inner) = item.strip_prefix('{') {
        let close = matching_brace(inner);
        let stretch = inner[close + 1..]
            .strip_prefix(':')
            .expect("nested layout `}:stretch`");
        return Item::Nested(parse_layout(&inner[..close]), stretch.parse().unwrap());
    }
    if let Some(stretch) = item.strip_prefix('S') {
        return Item::Spacer(stretch.parse().unwrap());
    }
    let f: Vec<&str> = item.split(',').collect();
    let n = |i: usize| f[i].parse::<i32>().unwrap();
    let minimum_hint = if f.len() > 16 {
        Size::new(n(16), n(17))
    } else {
        Size::new(0, 0)
    };
    Item::Widget(WidgetSpec {
        hint: Size::new(n(0), n(1)),
        min: Size::new(n(2), n(3)),
        max: Size::new(n(4), n(5)),
        policy: (policy(f[6]), policy(f[7])),
        minimum_hint,
        stretch: n(8) as u32,
        hidden: f[9] == "1",
        row: n(10) as usize,
        column: n(11) as usize,
        row_span: n(12) as usize,
        col_span: n(13) as usize,
        row_stretch: n(14) as u32,
        alignment: Alignment::from_bits(n(15) as u32),
    })
}

fn margins(m: [i32; 4]) -> Margins {
    Margins {
        left: m[0],
        top: m[1],
        right: m[2],
        bottom: m[3],
    }
}

/// A widget item and the nested layouts that enclose it (outermost first, as indexes into the
/// holders of `build_box`).
struct Leaf {
    widget: WidgetRef,
    holders: Vec<usize>,
}

/// Creates the widget for a spec and records it in `out` (depth-first order).
fn make_widget(spec: &WidgetSpec, holders: &[usize], out: &mut Vec<Leaf>) -> WidgetRef {
    let probe = Probe {
        base: WidgetBase::new(),
        hint: spec.hint,
        min: spec.min,
        max: spec.max,
        minimum_hint: spec.minimum_hint,
    };
    probe
        .base
        .set_size_policy(QSizePolicy::new(spec.policy.0, spec.policy.1));
    probe.base.set_visible(!spec.hidden);
    // The Qt side creates a 100x30 widget and then calls `setMinimumSize` and `setMaximumSize`, which
    // grow and shrink it to the bounds (qwidget.cpp:4013-4020, 4073-4080). A page that no layout
    // places (a non-current `QStackedLayout` page) keeps that geometry.
    let width = 100.max(spec.min.width).min(spec.max.width);
    let height = 30.max(spec.min.height).min(spec.max.height);
    probe.base.set_geometry(Rect::new(0, 0, width, height));
    let widget: WidgetRef = Rc::new(RefCell::new(Box::new(probe)));
    out.push(Leaf {
        widget: widget.clone(),
        holders: holders.to_vec(),
    });
    widget
}

/// `path` holds the indexes of the enclosing nested layouts; `holders[i]` is the holder widget
/// `add_layout` returned for nested layout `i`, filled in once that layout is added.
fn build_box(
    layout: &LayoutSpec,
    path: &mut Vec<usize>,
    holders: &mut Vec<Option<WidgetRef>>,
    out: &mut Vec<Leaf>,
) -> BoxLayout {
    let head = &layout.head;
    let mut boxed = if head.kind == 'V' {
        BoxLayout::vertical()
    } else {
        BoxLayout::horizontal()
    };
    if let Some(m) = head.margins {
        boxed.set_margins(margins(m));
    }
    if let Some(s) = head.spacing {
        boxed.set_spacing(s);
    }
    for item in &layout.items {
        match item {
            Item::Widget(spec) => {
                let widget = make_widget(spec, path, out);
                boxed.add_widget_aligned(widget, spec.stretch, spec.alignment);
            }
            Item::Spacer(stretch) => boxed.add_stretch(*stretch),
            Item::Nested(child, stretch) => {
                let id = holders.len();
                holders.push(None);
                path.push(id);
                let child = build_box(child, path, holders, out);
                path.pop();
                holders[id] = Some(boxed.add_layout(Box::new(child), *stretch));
            }
        }
    }
    boxed
}

fn build_grid(layout: &LayoutSpec, out: &mut Vec<Leaf>) -> GridLayout {
    let head = &layout.head;
    let mut grid = GridLayout::new();
    if let Some(m) = head.margins {
        grid.set_margins(margins(m));
    }
    if let Some(h) = head.h_spacing.or(head.spacing) {
        grid.set_horizontal_spacing(h);
    }
    if let Some(v) = head.v_spacing.or(head.spacing) {
        grid.set_vertical_spacing(v);
    }
    for &(row, h) in &head.row_min {
        grid.set_row_minimum_height(row, h);
    }
    for &(col, w) in &head.col_min {
        grid.set_column_minimum_width(col, w);
    }
    for item in &layout.items {
        let Item::Widget(spec) = item else {
            panic!("spacers and nested layouts are box-only");
        };
        let widget = make_widget(spec, &[], out);
        grid.add_widget_aligned(
            widget,
            spec.row,
            spec.column,
            spec.row_span,
            spec.col_span,
            spec.alignment,
        );
        if spec.row == 0 && spec.stretch > 0 {
            grid.set_column_stretch(spec.column, spec.stretch);
        }
        if spec.row_stretch > 0 {
            grid.set_row_stretch(spec.row, spec.row_stretch);
        }
    }
    grid
}

/// `QStackedLayout`: widget items only, one page each; the probe does not change visibility.
fn build_stack(layout: &LayoutSpec, out: &mut Vec<Leaf>) -> StackedLayout {
    let head = &layout.head;
    let mut stack = StackedLayout::new();
    if let Some(m) = head.margins {
        stack.set_margins(margins(m));
    }
    for item in &layout.items {
        let Item::Widget(spec) = item else {
            panic!("spacers and nested layouts are box-only");
        };
        let widget = make_widget(spec, &[], out);
        stack.add_widget(widget);
    }
    stack
}

/// Lays out one line (see the module docs) and returns every widget item's rectangle in
/// container coordinates: a nested item's own rectangle is holder-relative, so it is offset by
/// the slots of every enclosing holder (what is visible on screen, as Qt's `geometry()` reports).
pub fn lay_out_case(line: &str) -> Vec<Geom> {
    let top = parse_layout(line);
    let head = &top.head;
    let (width, height) = (head.width, head.height);
    let (offset_x, offset_y) = head.offset;
    let mut leaves = Vec::new();
    let mut holders = Vec::new();
    let layout: Box<dyn ferrokit_widgets::Layout> = match head.kind {
        'G' => Box::new(build_grid(&top, &mut leaves)),
        'K' => Box::new(build_stack(&top, &mut leaves)),
        _ => Box::new(build_box(&top, &mut Vec::new(), &mut holders, &mut leaves)),
    };
    // A window has no parent widget; a child container gets one, kept alive until the end.
    let parent: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::new())));
    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(offset_x, offset_y, width, height),
    ))));
    if !head.window {
        container
            .borrow()
            .set_parent_widget(Some(Rc::downgrade(&parent)));
    }
    container.borrow_mut().set_layout(layout);
    container
        .borrow_mut()
        .set_geometry(Rect::new(offset_x, offset_y, width, height));
    container.borrow().update_layout();
    let holder_origin = |i: usize| {
        let g = holders[i].as_ref().expect("holder").borrow().geometry();
        (g.x, g.y)
    };
    leaves
        .iter()
        .map(|leaf| {
            let g = leaf.widget.borrow().geometry();
            let (mut x, mut y) = (g.x, g.y);
            for &i in &leaf.holders {
                let (hx, hy) = holder_origin(i);
                x += hx;
                y += hy;
            }
            (x, y, g.width, g.height)
        })
        .collect()
}

/// Parses `x,y,w,h` (the oracle's rectangle format).
pub fn parse_geom(s: &str) -> Geom {
    let v: Vec<i32> = s.split(',').map(|x| x.parse().unwrap()).collect();
    (v[0], v[1], v[2], v[3])
}

/// Formats rectangles as `x,y,w,h` joined by `;` (the probe's stdout format).
pub fn format_geoms(geoms: &[Geom]) -> String {
    geoms
        .iter()
        .map(|g| format!("{},{},{},{}", g.0, g.1, g.2, g.3))
        .collect::<Vec<_>>()
        .join(";")
}
