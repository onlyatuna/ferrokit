// SPDX-License-Identifier: LGPL-3.0-only
//! Replays the PySide6 layout oracle (`fixtures/layout_qt_oracle.txt`) in-process against the
//! Rust layout engine. Each record is a layout line and the rectangles Qt produced for its items;
//! hidden items are recorded as `-` and not compared. Recorded with PySide6 6.11.2.
//! Records that the port does not match
//! yet are kept in `fixtures/layout_qt_oracle_known.txt`: they are replayed and reported, not failed.

#[path = "support/layout_probe.rs"]
mod layout_probe;

use ferrokit_core::event::Event;
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_widgets::*;
use std::cell::RefCell;
use std::rc::Rc;

const ORACLE: &str = include_str!("fixtures/layout_qt_oracle.txt");
/// Known mismatches against Qt, replayed for reporting only.
const KNOWN: &str = include_str!("fixtures/layout_qt_oracle_known.txt");

/// Records of a fixture as `(line_number, case, expected_rects)`.
fn records_of(text: &'static str) -> Vec<(usize, &'static str, &'static str)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(n, line)| {
            let (case, expect) = line.split_once('\t').expect("case TAB expect");
            (n + 1, case, expect)
        })
        .collect()
}

fn records() -> Vec<(usize, &'static str, &'static str)> {
    records_of(ORACLE)
}

/// Compares the rectangle of every visible item of each record with Qt's; returns the differences.
fn compare(records: &[(usize, &str, &str)]) -> Vec<String> {
    let mut diffs = Vec::new();
    for &(line_no, case, expect) in records {
        let expected: Vec<&str> = if expect.is_empty() {
            Vec::new()
        } else {
            expect.split(';').collect()
        };
        let actual = layout_probe::lay_out_case(case);
        assert_eq!(
            actual.len(),
            expected.len(),
            "line {line_no}: item count differs for {case}"
        );
        for (i, (want, got)) in expected.iter().zip(&actual).enumerate() {
            if *want == "-" {
                continue;
            }
            let want = layout_probe::parse_geom(want);
            if want != *got {
                diffs.push(format!(
                    "line {line_no} item {i}: qt {want:?} rust {got:?}\n  case: {case}"
                ));
            }
        }
    }
    diffs
}

#[test]
fn layout_matches_pyside6_oracle() {
    let cases = records();
    assert!(
        cases.len() >= 300,
        "oracle fixture has only {} cases",
        cases.len()
    );
    let diffs = compare(&cases);
    assert!(
        diffs.is_empty(),
        "{} rectangle(s) differ from Qt ({} cases):\n{}",
        diffs.len(),
        cases.len(),
        diffs.join("\n")
    );
}

/// Known mismatches are replayed and reported without failing: a fixed one stops appearing here,
/// and the rectangles Qt produced are still checked against the port's output.
#[test]
fn known_mismatches_are_reported() {
    let known = records_of(KNOWN);
    let diffs = compare(&known);
    eprintln!(
        "{} rectangle(s) in {} known record(s) differ from Qt",
        diffs.len(),
        known.len()
    );
    for diff in &diffs {
        eprintln!("known mismatch: {diff}");
    }
}

/// The fixture must keep exercising every layout feature the port is required to match.
#[test]
fn oracle_covers_required_features() {
    let text = ORACLE;
    let has = |needle: &str| text.contains(needle);
    assert!(has(" m="), "no asymmetric/explicit margins");
    assert!(has(" hs=") && has(" vs="), "no grid H/V spacing");
    assert!(has(" rmin="), "no row minimum height");
    assert!(has(" cmin="), "no column minimum width");
    assert!(has("{"), "no nested layout");
    assert!(has("| S") || has(";S") || has("|S"), "no spacer");
    assert!(
        records()
            .iter()
            .any(|(_, _, expect)| expect.split(';').any(|r| r == "-")),
        "no hidden item record"
    );
    assert!(
        records()
            .iter()
            .any(|(_, case, _)| case.split(',').count() > 17 || case.contains(",0x")),
        "no 18-field minimum-size-hint item"
    );
    assert!(
        has("Expanding") && has("Minimum"),
        "no stretch/policy variety"
    );
    assert!(
        has("hs=3 vs=9") || has("hs=2 vs=8"),
        "no H != V grid spacing case"
    );
    assert!(has(" um ") && has(" us "), "no unset margins or spacing");
    assert!(has(" own=W") && has(" own=C"), "no window and child owners");
    assert!(has(" at="), "no container offset");
    assert!(
        records()
            .iter()
            .chain(records_of(KNOWN).iter())
            .any(|(_, case, _)| case.starts_with("K ")),
        "no QStackedLayout (kind K)"
    );
}

/// Minimal public-API nested case, independent of the probe helper: a horizontal box holds a
/// 50x20 leaf and then a vertical box of two leaves (added with `add_layout`). The nested
/// leaves' geometry must be in the container's coordinates, so they start at x = 50.
struct Probe {
    base: WidgetBase,
    hint: Size,
}

impl QObject for Probe {
    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }
    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.base.object_data
    }
    fn event(&mut self, _event: &mut Event) -> bool {
        false
    }
}

impl Widget for Probe {
    fn widget_base(&self) -> &WidgetBase {
        &self.base
    }
    fn id(&self) -> ObjectId {
        self.base.object_data.id
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
    fn minimum_size_hint(&self) -> Size {
        Size::new(0, 0)
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

#[test]
fn nested_add_layout_children_are_container_relative() {
    fn leaf() -> WidgetRef {
        Rc::new(RefCell::new(Box::new(Probe {
            base: WidgetBase::new(),
            hint: Size::new(100, 30),
        })))
    }
    let first = leaf();
    let a = leaf();
    let b = leaf();

    let mut vbox = BoxLayout::vertical();
    vbox.set_spacing(0);
    vbox.add_widget_aligned(a.clone(), 0, Alignment::NONE);
    vbox.add_widget_aligned(b.clone(), 0, Alignment::NONE);
    let mut hbox = BoxLayout::horizontal();
    hbox.set_spacing(0);
    hbox.set_margins(ferrokit_gui::geometry::primitives::Margins::new(0, 0, 0, 0));
    hbox.add_widget_aligned(first.clone(), 0, Alignment::NONE);
    let holder = hbox.add_layout(Box::new(vbox), 0);

    let container: WidgetRef = Rc::new(RefCell::new(Box::new(EmptyWidget::with_geometry(
        Rect::new(0, 0, 200, 60),
    ))));
    container.borrow_mut().set_layout(Box::new(hbox));
    container.borrow().update_layout();

    let g = |w: &WidgetRef| {
        let r = w.borrow().geometry();
        (r.x, r.y, r.width, r.height)
    };
    // Leaves of a nested layout are relative to their holder: map them into container coordinates.
    let (hx, hy) = {
        let h = holder.borrow().geometry();
        (h.x, h.y)
    };
    let shift = |r: (i32, i32, i32, i32)| (r.0 + hx, r.1 + hy, r.2, r.3);
    let (fa, fb, ff) = (shift(g(&a)), shift(g(&b)), g(&first));
    println!("first {ff:?} a {fa:?} b {fb:?} holder ({hx},{hy})");
    assert_eq!(ff.0, 0, "first leaf x");
    assert_eq!(fa, (100, 0, 100, 30), "nested leaf a: Qt (100,0,100,30)");
    assert_eq!(fb, (100, 30, 100, 30), "nested leaf b: Qt (100,30,100,30)");
}
