// SPDX-License-Identifier: LGPL-3.0-only
//! Integration tests for Complete Geometry, Region Algebra, and 3D Math subsystem.

use ferrokit_gui::geometry::*;

#[test]
fn test_line_and_line_f() {
    let l = Line::from_coords(10, 20, 40, 60);
    assert_eq!(l.dx(), 30);
    assert_eq!(l.dy(), 40);
    assert!(!l.is_null());

    let lf = l.to_line_f();
    assert_eq!(lf.length(), 50.0);

    // Angle test: (0,0) -> (10, 0) should be 0 deg
    let horiz = LineF::from_coords(0.0, 0.0, 10.0, 0.0);
    assert!((horiz.angle() - 0.0).abs() < 1e-4);

    // (0,0) -> (0, -10) should be 90 deg (Qt y goes down)
    let up = LineF::from_coords(0.0, 0.0, 0.0, -10.0);
    assert!((up.angle() - 90.0).abs() < 1e-4);

    // Intersections
    let l1 = LineF::from_coords(0.0, 0.0, 10.0, 10.0);
    let l2 = LineF::from_coords(0.0, 10.0, 10.0, 0.0);
    let (intersect_type, pt) = l1.intersects(&l2);
    assert_eq!(intersect_type, IntersectionType::BoundedIntersection);
    assert_eq!(pt, Some(PointF::new(5.0, 5.0)));

    // Point at
    assert_eq!(l1.point_at(0.5), PointF::new(5.0, 5.0));
    assert_eq!(l1.center(), PointF::new(5.0, 5.0));
}

#[test]
fn test_polygon_containment() {
    // Triangle: (0,0), (100, 0), (50, 100)
    let poly = PolygonF::from_points(vec![
        PointF::new(0.0, 0.0),
        PointF::new(100.0, 0.0),
        PointF::new(50.0, 100.0),
    ]);

    assert_eq!(poly.bounding_rect(), RectF::new(0.0, 0.0, 100.0, 100.0));

    // Point inside
    assert!(poly.contains_point(PointF::new(50.0, 30.0), FillRule::OddEven));
    assert!(poly.contains_point(PointF::new(50.0, 30.0), FillRule::Winding));

    // Point outside
    assert!(!poly.contains_point(PointF::new(10.0, 90.0), FillRule::OddEven));
    assert!(!poly.contains_point(PointF::new(200.0, 50.0), FillRule::Winding));
}

#[test]
fn test_region_algebra() {
    // R1: [0, 0, 100, 100]
    let r1 = Region::from_rect(Rect::new(0, 0, 100, 100));
    // R2: [50, 50, 100, 100]
    let r2 = Region::from_rect(Rect::new(50, 50, 100, 100));

    assert_eq!(r1.rect_count(), 1);
    assert_eq!(r1.bounding_rect(), Rect::new(0, 0, 100, 100));

    // Contains
    assert!(r1.contains_point(Point::new(50, 50)));
    assert!(!r1.contains_point(Point::new(105, 50)));
    assert!(r1.contains_rect(&Rect::new(10, 10, 20, 20)));
    assert!(!r1.contains_rect(&Rect::new(80, 80, 50, 50)));

    // Intersection: [50, 50, 50, 50]
    let inter = &r1 & &r2;
    assert_eq!(inter.bounding_rect(), Rect::new(50, 50, 50, 50));
    assert_eq!(inter.rect_count(), 1);
    assert!(inter.contains_point(Point::new(75, 75)));
    assert!(!inter.contains_point(Point::new(25, 25)));

    // Subtraction: R1 - R2
    let sub = &r1 - &r2;
    assert_eq!(sub.bounding_rect(), Rect::new(0, 0, 100, 100));
    assert!(!sub.contains_point(Point::new(75, 75))); // subtracted overlap
    assert!(sub.contains_point(Point::new(25, 25))); // kept non-overlap

    // Verify all remainder rects in sub are mutually disjoint
    for i in 0..sub.rect_count() {
        for j in (i + 1)..sub.rect_count() {
            assert!(
                !sub.rects()[i].intersects(&sub.rects()[j]),
                "Region rects must be disjoint"
            );
        }
    }

    // Union: R1 | R2
    let un = &r1 | &r2;
    assert_eq!(un.bounding_rect(), Rect::new(0, 0, 150, 150));
    assert!(un.contains_point(Point::new(25, 25)));
    assert!(un.contains_point(Point::new(75, 75)));
    assert!(un.contains_point(Point::new(125, 125)));
    assert!(!un.contains_point(Point::new(125, 25)));

    // Xor: (R1 | R2) - (R1 & R2)
    let xor = &r1 ^ &r2;
    assert!(xor.contains_point(Point::new(25, 25)));
    assert!(xor.contains_point(Point::new(125, 125)));
    assert!(!xor.contains_point(Point::new(75, 75))); // overlap removed in XOR
}

#[test]
fn test_vector_math() {
    let v2 = Vector2D::new(3.0, 4.0);
    assert_eq!(v2.length(), 5.0);
    assert_eq!(v2.normalized(), Vector2D::new(0.6, 0.8));
    assert_eq!(Vector2D::dot_product(v2, Vector2D::new(2.0, 1.0)), 10.0);

    let v3a = Vector3D::new(1.0, 0.0, 0.0);
    let v3b = Vector3D::new(0.0, 1.0, 0.0);
    let cross = Vector3D::cross_product(v3a, v3b);
    assert_eq!(cross, Vector3D::new(0.0, 0.0, 1.0));

    let v4 = Vector4D::new(2.0, 4.0, 6.0, 2.0);
    let affine = v4.to_vector3d_affine();
    assert_eq!(affine, Vector3D::new(1.0, 2.0, 3.0));
}

#[test]
fn test_matrix4x4() {
    let mut m = Matrix4x4::identity();
    m.translate(10.0, 20.0, 30.0);

    let p = m.map_vector3d(Vector3D::new(0.0, 0.0, 0.0));
    assert_eq!(p, Vector3D::new(10.0, 20.0, 30.0));

    // Inversion
    let inv = m.inverted().expect("invertible");
    let back = inv.map_vector3d(p);
    assert!((back.x).abs() < 1e-5);
    assert!((back.y).abs() < 1e-5);
    assert!((back.z).abs() < 1e-5);

    // Scale
    let mut m_scale = Matrix4x4::identity();
    m_scale.scale_uniform(2.0);
    assert_eq!(
        m_scale.map_vector3d(Vector3D::new(1.0, 2.0, 3.0)),
        Vector3D::new(2.0, 4.0, 6.0)
    );
}

#[test]
fn test_quaternion() {
    let q_id = Quaternion::identity();
    assert_eq!(q_id.length(), 1.0);

    // Rotate 90 degrees around Z axis
    let q_rot_z = Quaternion::from_axis_and_angle(Vector3D::new(0.0, 0.0, 1.0), 90.0);
    let rotated = q_rot_z.rotated_vector(Vector3D::new(1.0, 0.0, 0.0));
    assert!((rotated.x).abs() < 1e-5);
    assert!((rotated.y - 1.0).abs() < 1e-5);
    assert!((rotated.z).abs() < 1e-5);

    // Slerp halfway
    let slerped = Quaternion::slerp(Quaternion::identity(), q_rot_z, 0.5);
    let half_rot = slerped.rotated_vector(Vector3D::new(1.0, 0.0, 0.0));
    let expected_cos45 = 45.0f32.to_radians().cos();
    assert!((half_rot.x - expected_cos45).abs() < 1e-4);
    assert!((half_rot.y - expected_cos45).abs() < 1e-4);
}

#[test]
fn test_projective_transform() {
    // Identity
    let mut t = Transform::identity();
    assert!(t.is_identity());
    assert!(t.is_affine());

    // Affine translation
    t.translate(10.0, 20.0);
    assert_eq!(t.map_point(PointF::new(5.0, 5.0)), PointF::new(15.0, 25.0));

    // Inversion
    let inv = t.inverted().expect("invertible");
    assert_eq!(
        inv.map_point(PointF::new(15.0, 25.0)),
        PointF::new(5.0, 5.0)
    );

    // Perspective transformation: squareToQuad
    let quad = [
        PointF::new(0.0, 0.0),
        PointF::new(100.0, 10.0),
        PointF::new(90.0, 90.0),
        PointF::new(10.0, 100.0),
    ];
    let proj = Transform::square_to_quad(quad).expect("valid quad");
    let mapped_origin = proj.map_point(PointF::new(0.0, 0.0));
    assert_eq!(mapped_origin, PointF::new(0.0, 0.0));
}

#[test]
fn test_qt_canonical_geometry_aliases() {
    let _l: QLine = Line::new(Point::new(0, 0), Point::new(1, 1));
    let _lf: QLineF = LineF::new(PointF::new(0.0, 0.0), PointF::new(1.0, 1.0));
    let _poly: QPolygon = Polygon::new();
    let _poly_f: QPolygonF = PolygonF::new();
    let _rgn: QRegion = Region::new();
    let _v2: QVector2D = Vector2D::new(1.0, 2.0);
    let _v3: QVector3D = Vector3D::new(1.0, 2.0, 3.0);
    let _v4: QVector4D = Vector4D::new(1.0, 2.0, 3.0, 4.0);
    let _m4: QMatrix4x4 = Matrix4x4::identity();
    let _quat: QQuaternion = Quaternion::identity();
    let _tr: QTransform = Transform::identity();
}
