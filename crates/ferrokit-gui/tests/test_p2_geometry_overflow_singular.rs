// SPDX-License-Identifier: LGPL-3.0-only
//! P2 geometry regressions (issue #67): i32 edge overflow, Region scaling/overflow,
//! and scale-relative singularity detection in `Transform2D`, `Transform`, `Matrix4x4`.

use ferrokit_gui::geometry::*;

// --- Rect / Region overflow -------------------------------------------------

#[test]
fn rect_edges_saturate_instead_of_overflowing() {
    // Qt's QRect also has no defined behaviour past INT_MAX; here edges saturate and never panic.
    let r = Rect::new(i32::MAX - 1, i32::MAX - 1, i32::MAX, i32::MAX);
    assert_eq!(r.right(), i32::MAX);
    assert_eq!(r.bottom(), i32::MAX);
    let _ = r.top_right();
    let _ = r.bottom_right();
    let _ = r.center();
    let _ = r.contains(Point::new(i32::MAX - 1, i32::MAX - 1));
    let _ = r.intersected(&Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    let _ = r.united(&Rect::new(i32::MIN, i32::MIN, 10, 10));
    let big = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    let u = big.united(&r);
    assert!(u.width > 0 && u.height > 0);
}

#[test]
fn region_survives_huge_rects() {
    let huge = Rect::new(i32::MAX - 1, i32::MAX - 1, i32::MAX, i32::MAX);
    let a = Region::from_rect(huge);
    assert!(a.contains_point(Point::new(i32::MAX - 1, i32::MAX - 1)));
    assert!(!a.contains_point(Point::new(0, 0)));
    let b = Region::from_rect(Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    let u = &a | &b;
    let _ = u.bounding_rect();
    let i = &a & &b;
    assert!(i.is_empty());
    let d = &u - &a;
    assert!(d.contains_point(Point::new(0, 0)) || !d.is_empty());
    let x = &a ^ &b;
    assert!(!x.is_empty());
    let mut t = a.clone();
    t.translate(i32::MAX, i32::MAX);
    t.translate(i32::MIN, i32::MIN);
    let _ = t.bounding_rect();
    let _ = Region::from_rects(&[huge, Rect::new(0, 0, i32::MAX, i32::MAX)]).bounding_rect();
    assert!(Region::from_rect(huge).intersects(&a));
}

// --- Region correctness vs. a pixel bitmap ------------------------------------

const N: i32 = 24;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, m: i32) -> i32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as i32).rem_euclid(m)
    }
}

fn random_rect(g: &mut Lcg) -> Rect {
    Rect::new(g.next(N) - 2, g.next(N) - 2, g.next(12) - 1, g.next(12) - 1)
}

fn bitmap(rects: &[Rect]) -> Vec<bool> {
    let mut v = vec![false; (N * N) as usize];
    for r in rects {
        for y in r.y.max(0)..r.bottom().min(N) {
            for x in r.x.max(0)..r.right().min(N) {
                if r.width > 0 && r.height > 0 {
                    v[(y * N + x) as usize] = true;
                }
            }
        }
    }
    v
}

fn region_bitmap(r: &Region) -> Vec<bool> {
    let mut v = vec![false; (N * N) as usize];
    for y in 0..N {
        for x in 0..N {
            v[(y * N + x) as usize] = r.contains_point(Point::new(x, y));
        }
    }
    v
}

fn assert_disjoint_and_inside(r: &Region) {
    let rs = r.rects();
    for (i, a) in rs.iter().enumerate() {
        assert!(a.width > 0 && a.height > 0);
        for b in &rs[i + 1..] {
            assert!(!a.intersects(b), "overlap {a:?} {b:?}");
        }
    }
}

#[test]
fn region_ops_match_bitmap_reference() {
    let mut g = Lcg(0x1234_5678);
    for _ in 0..300 {
        let ra: Vec<Rect> = (0..(g.next(6) + 1)).map(|_| random_rect(&mut g)).collect();
        let rb: Vec<Rect> = (0..(g.next(6) + 1)).map(|_| random_rect(&mut g)).collect();
        let a = Region::from_rects(&ra);
        let b = Region::from_rects(&rb);
        let (ba, bb) = (bitmap(&ra), bitmap(&rb));
        assert_eq!(region_bitmap(&a), ba);
        assert_disjoint_and_inside(&a);

        let cases: [(&str, Region, Box<dyn Fn(bool, bool) -> bool>); 4] = [
            ("or", &a | &b, Box::new(|p, q| p || q)),
            ("and", &a & &b, Box::new(|p, q| p && q)),
            ("sub", &a - &b, Box::new(|p, q| p && !q)),
            ("xor", &a ^ &b, Box::new(|p, q| p != q)),
        ];
        for (name, got, f) in cases {
            let want: Vec<bool> = ba.iter().zip(&bb).map(|(&p, &q)| f(p, q)).collect();
            assert_eq!(region_bitmap(&got), want, "{name}");
            assert_disjoint_and_inside(&got);
        }
        let any = ba.iter().zip(&bb).any(|(&p, &q)| p && q);
        let grid = Rect::new(0, 0, N, N);
        assert_eq!(a.intersected_rect(&grid).intersects(&b.intersected_rect(&grid)), any);
    }
}

#[test]
fn region_coalesces_a_large_grid_to_one_rect() {
    // 60x60 unit cells united one by one: must collapse to a single rect.
    let cells: Vec<Rect> = (0..60).flat_map(|y| (0..60).map(move |x| Rect::new(x, y, 1, 1))).collect();
    let r = Region::from_rects(&cells);
    assert_eq!(r.rects(), &[Rect::new(0, 0, 60, 60)]);
    // Removing the whole thing in strips leaves nothing.
    let mut shrinking = r.clone();
    for y in 0..60 {
        shrinking = shrinking.subtracted_rect(&Rect::new(0, y, 60, 1));
    }
    assert!(shrinking.is_empty());
}

// --- Singularity thresholds ---------------------------------------------------

#[test]
fn transform2d_tiny_scale_is_invertible() {
    let t = Transform2D::from_scale(1e-4, 2e-4);
    let inv = t.inverted().expect("tiny but invertible");
    let p = inv.map_point(PointF::new(1e-4, 2e-4));
    assert!((p.x - 1.0).abs() < 1e-3 && (p.y - 1.0).abs() < 1e-3, "{p:?}");
    let tiny = Transform2D::from_scale(1e-9, 1e-9);
    assert!(tiny.inverted().is_some());
}

#[test]
fn transform2d_singular_still_rejected() {
    assert!(Transform2D::from_scale(0.0, 1.0).inverted().is_none());
    assert!(Transform2D::from_row_matrix(1.0, 2.0, 2.0, 4.0, 0.0, 0.0).inverted().is_none());
    assert!(Transform2D::from_row_matrix(1e-5, 2e-5, 2e-5, 4e-5, 0.0, 0.0).inverted().is_none());
    assert!(Transform2D::from_scale(f32::NAN, 1.0).inverted().is_none());
}

#[test]
fn transform_tiny_scale_is_invertible() {
    let mut t = Transform::identity();
    t.scale(1e-5, 1e-5);
    let inv = t.inverted().expect("tiny but invertible");
    let p = inv.map_point(PointF::new(1e-5, 1e-5));
    assert!((p.x - 1.0).abs() < 1e-3 && (p.y - 1.0).abs() < 1e-3, "{p:?}");
}

#[test]
fn transform_singular_still_rejected() {
    let mut t = Transform::identity();
    t.scale(0.0, 1.0);
    assert!(t.inverted().is_none());
    let rank1 = Transform::identity();
    let mut rank1 = Transform { m11: 1.0, m12: 2.0, m21: 2.0, m22: 4.0, ..rank1 };
    assert!(rank1.inverted().is_none());
    rank1.m11 = 1e-5;
    rank1.m12 = 2e-5;
    rank1.m21 = 2e-5;
    rank1.m22 = 4e-5;
    assert!(rank1.inverted().is_none());
}

#[test]
fn square_to_quad_tiny_quad_is_accepted() {
    let s = 1e-4_f32;
    let q = [
        PointF::new(0.0, 0.0),
        PointF::new(s, 0.0),
        PointF::new(s, s * 0.5),
        PointF::new(0.0, s),
    ];
    assert!(Transform::square_to_quad(q).is_some());
}

#[test]
fn matrix4x4_tiny_scale_is_invertible() {
    let mut m = Matrix4x4::identity();
    m.scale(1e-10, 1e-10, 1e-10);
    let inv = m.inverted().expect("tiny but invertible");
    assert!((inv.m[0][0] - 1e10).abs() / 1e10 < 1e-4);
    assert!((inv.m[2][2] - 1e10).abs() / 1e10 < 1e-4);
}

#[test]
fn matrix4x4_mixed_magnitude_diagonal_is_invertible() {
    let mut m = Matrix4x4::identity();
    m.scale(1e6, 1e-6, 1.0);
    let inv = m.inverted().expect("invertible");
    assert!((inv.m[1][1] - 1e6).abs() / 1e6 < 1e-4);
}

#[test]
fn matrix4x4_singular_still_rejected() {
    assert!(Matrix4x4::from_array([
        1.0, 2.0, 3.0, 4.0, //
        5.0, 6.0, 7.0, 8.0, //
        9.0, 10.0, 11.0, 12.0, //
        13.0, 14.0, 15.0, 16.0,
    ])
    .inverted()
    .is_none());
    let mut z = Matrix4x4::identity();
    z.scale(1.0, 0.0, 1.0);
    assert!(z.inverted().is_none());
    let mut tiny_singular = Matrix4x4::identity();
    tiny_singular.scale(1e-8, 0.0, 1e-8);
    assert!(tiny_singular.inverted().is_none());
}
