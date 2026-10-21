// SPDX-License-Identifier: LGPL-3.0-only
//! DirectComposition surface creation must not fail on interface lookups.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::surface::dcomp::DCompSurface;
use ferrokit_platform::surface::PlatformSurface;
use ferrokit_platform::window::{NativeWindow, WindowFlags};

/// A machine without a usable Direct3D 11 device may refuse to create the surface; a wrong IID
/// (`E_NOINTERFACE` from `DCompositionCreateDevice`, or from `GetBuffer` on present) is a bug
/// whatever the hardware.
#[test]
fn a_dcomp_surface_is_created_and_presents_without_interface_errors() {
    let window = NativeWindow::new(
        "DComp IID",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window");
    match DCompSurface::new(window.hwnd(), 120, 80) {
        Ok(mut surface) => {
            let mut pixmap = Pixmap::new(120, 80).expect("pixmap");
            pixmap.fill(Color::from_rgba8(20, 40, 80, 220));
            surface
                .present_dirty(&mut pixmap, 1.0, Rect::new(0, 0, 120, 80))
                .expect("a created DComp surface presents");
            // Partial presents follow the full first one (dirty rectangles are legal from then on).
            for dirty in [Rect::new(10, 10, 30, 20), Rect::new(50, 40, 20, 20)] {
                surface
                    .present_dirty(&mut pixmap, 1.0, dirty)
                    .expect("a partial present");
            }
            surface.resize(160, 100).expect("resize");
            let mut bigger = Pixmap::new(160, 100).expect("pixmap");
            bigger.fill(Color::from_rgba8(80, 40, 20, 220));
            surface
                .present_dirty(&mut bigger, 1.0, Rect::new(0, 0, 20, 20))
                .expect("a present after a resize");
        }
        Err(e) => {
            assert!(
                !e.contains("DCompositionCreateDevice") && !e.contains("IDXGIFactory2"),
                "DirectComposition setup failed on an interface lookup: {e}"
            );
            assert!(
                std::env::var_os("FERROKIT_ENABLE_DCOMP").is_none(),
                "DirectComposition is enabled by the environment but cannot be created: {e}"
            );
        }
    }
}
