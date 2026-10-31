// SPDX-License-Identifier: LGPL-3.0-only
//! `QLabel` text wider than its rect, against Qt 6 (`qpainter.cpp:7415-7470`): the aligned
//! offset is not clamped, so centred/right text overflows symmetrically (`xoff` at 7426-7429),
//! and the overflow is clipped to the label rect (`setClipRect` at 7442-7446).
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::{PaintDevice, Painter, Pixmap};
use ferrokit_gui::text::font::Font;
use ferrokit_gui::text::font_metrics::FontMetrics;
use ferrokit_widgets::{Alignment, Label, Widget};

const H: i32 = 24;
const TEXT: &str = "2 % · MONTHLY";

fn font() -> Font {
    Font::new("Segoe UI", 12.0)
}

fn exact_advance(text: &str) -> f32 {
    let f = font();
    FontMetrics::from_font(&f).horizontal_advance_exact(text, &f)
}

fn make_label(text: &str, width: i32, align: Alignment) -> Label {
    let mut l = Label::new(text);
    l.set_font(font());
    l.set_alignment(align);
    l.set_geometry(Rect::new(0, 0, width, H));
    l
}

fn paint_into(w: i32, h: i32, paint: impl FnOnce(&mut Painter<'_>)) -> Pixmap {
    let mut pm = Pixmap::with_dpr(w as u32, h as u32, 1.0).unwrap();
    {
        let mut p = Painter::begin(&mut pm);
        paint(&mut p);
    }
    pm
}

/// First column where two same-sized pixmaps differ, for a readable failure message.
fn first_diff_column(a: &Pixmap, b: &Pixmap) -> Option<usize> {
    let w = a.width() as usize;
    a.data()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data().as_chunks::<4>().0)
        .enumerate()
        .find(|(_, (p, q))| p != q)
        .map(|(i, _)| i % w)
}

/// Overflowing centred / right text lands where Qt puts it: the unclamped offset from the
/// label's left edge, `(width - advance) / 2` or `width - advance`.
fn assert_overflow_offset(align: Alignment, x0_of: impl Fn(f32, f32) -> f32) {
    let exact = exact_advance(TEXT);
    let w = exact.floor() as i32 - 10;
    let x0 = x0_of(w as f32, exact);

    let mut overflow = make_label(TEXT, w, align);
    let got = paint_into(w, H, |p| overflow.paint_event(p));

    // Reference: the same text left-aligned at the offset Qt computes, unclipped.
    let mut reference = make_label(
        TEXT,
        exact.ceil() as i32 + 4,
        Alignment::LEFT | Alignment::V_CENTER,
    );
    let want = paint_into(w, H, |p| {
        p.translate(x0, 0.0);
        reference.paint_event(p);
    });

    assert!(
        got.data() == want.data(),
        "{align:?} overflow must start at x={x0} (qpainter.cpp:7426-7429); first differing column {:?}",
        first_diff_column(&got, &want)
    );
}

#[test]
fn centred_overflow_text_shifts_left_by_half_the_overflow() {
    assert_overflow_offset(Alignment::CENTER, |w, a| (w - a) / 2.0);
}

#[test]
fn right_overflow_text_is_not_clamped_to_the_left_edge() {
    assert_overflow_offset(Alignment::RIGHT | Alignment::V_CENTER, |w, a| w - a);
}

#[test]
fn overflowing_label_text_is_clipped_to_the_label_rect_on_both_sides() {
    let exact = exact_advance(TEXT);
    let w = exact.floor() as i32 - 10;
    let pad = 20;
    let mut l = make_label(TEXT, w, Alignment::CENTER);
    let pm = paint_into(w + 2 * pad, H, |p| {
        p.translate(pad as f32, 0.0);
        l.paint_event(p);
    });
    let pw = pm.width() as i32;
    let data = pm.data();
    for y in 0..H {
        for x in 0..pw {
            if x >= pad && x < pad + w {
                continue;
            }
            let alpha = data[((y * pw + x) * 4 + 3) as usize];
            assert_eq!(
                alpha, 0,
                "overflow text drawn outside the label rect at x={x} y={y} (qpainter.cpp:7442-7446)"
            );
        }
    }
}
