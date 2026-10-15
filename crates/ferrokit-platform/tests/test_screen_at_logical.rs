// SPDX-License-Identifier: LGPL-3.0-only
//! Parity test for `QGuiApplication::screenAt` (`qguiapplication.cpp:1147-1164`).
//! Qt's `screenAt` queries screens in logical coordinates (`sibling->geometry().contains(point)`),
//! not raw native pixel coordinates through OS APIs like `MonitorFromPoint`.

use ferrokit_gui::geometry::primitives::Point;
use ferrokit_platform::integration::platform;

#[test]
fn screen_at_operates_in_logical_coordinates() {
    let p = platform();
    let screens = p.screens();
    if screens.is_empty() {
        return;
    }
    let s0 = &screens[0];
    let geom = s0.geometry();

    // Center point in logical coordinates must hit the screen
    let center = Point::new(geom.x + geom.width / 2, geom.y + geom.height / 2);
    let hit = p.screen_at(center);
    assert!(
        hit.is_some(),
        "center of logical geometry must hit a screen"
    );
    assert_eq!(hit.unwrap().geometry(), geom);

    // Points outside the bounding box of all screens' logical geometries must NOT hit any screen
    let rightmost = screens
        .iter()
        .map(|s| s.geometry().x + s.geometry().width)
        .max()
        .unwrap_or(0);
    let bottommost = screens
        .iter()
        .map(|s| s.geometry().y + s.geometry().height)
        .max()
        .unwrap_or(0);

    let outside_right = Point::new(rightmost + 100, geom.y + geom.height / 2);
    assert!(
        p.screen_at(outside_right).is_none(),
        "point beyond right of all logical screens must return None"
    );

    let outside_bottom = Point::new(geom.x + geom.width / 2, bottommost + 100);
    assert!(
        p.screen_at(outside_bottom).is_none(),
        "point beyond bottom of all logical screens must return None"
    );
}
