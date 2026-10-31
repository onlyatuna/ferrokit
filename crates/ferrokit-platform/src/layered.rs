// SPDX-License-Identifier: LGPL-3.0-only
pub use crate::surface::macos::CocoaLayerSurface;
pub use crate::surface::wayland::WaylandShmSurface;
#[cfg(windows)]
pub use crate::surface::win32::Win32LayeredSurface as LayeredSurface;
#[cfg(windows)]
pub use crate::surface::win32::Win32LayeredSurface;
pub use crate::surface::x11::X11ShmSurface;
pub use crate::surface::PlatformSurface;

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::window::{NativeWindow, WindowFlags};
    use ferrokit_gui::geometry::Rect;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;

    #[test]
    fn test_layered_surface_create_and_buffer() {
        let window = NativeWindow::new(
            "Layered Test",
            Rect::new(100, 100, 200, 150),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("failed to create native window");

        let mut surface =
            LayeredSurface::new(window.hwnd(), 200, 150).expect("failed to create layered surface");
        assert_eq!(surface.width(), 200);
        assert_eq!(surface.height(), 150);
        assert!(!surface.hdc().is_null());
        assert!(!surface.bits().is_null());

        let len = (200 * 150 * 4) as usize;
        assert_eq!(surface.buffer().len(), len);

        // Test buffer mutation
        surface.buffer_mut()[0] = 255;
        assert_eq!(surface.buffer()[0], 255);
    }

    #[test]
    fn test_layered_surface_resize() {
        let window = NativeWindow::new(
            "Resize Test",
            Rect::new(100, 100, 100, 100),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("failed to create native window");

        let mut surface =
            LayeredSurface::new(window.hwnd(), 100, 100).expect("failed to create layered surface");
        assert_eq!(surface.width(), 100);
        assert_eq!(surface.height(), 100);

        surface.resize(300, 200).expect("failed to resize surface");
        assert_eq!(surface.width(), 300);
        assert_eq!(surface.height(), 200);
        assert_eq!(surface.buffer().len(), 300 * 200 * 4);
    }

    #[test]
    fn test_layered_surface_present() {
        let window = NativeWindow::new(
            "Present Test",
            Rect::new(50, 50, 64, 64),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("failed to create native window");

        let mut surface =
            LayeredSurface::new(window.hwnd(), 64, 64).expect("failed to create layered surface");

        let mut pixmap = Pixmap::new(64, 64).expect("failed to create pixmap");
        // Fill semi-transparent red RGBA: [200, 0, 0, 128]
        pixmap.fill(Color::from_rgba8(200, 0, 0, 128));

        let res = surface.present(&mut pixmap, 1.0);
        assert!(
            res.is_ok(),
            "present to UpdateLayeredWindow should succeed: {:?}",
            res.err()
        );

        // 1. Verify surface buffer contains Windows GDI premultiplied BGRA: [0, 0, 100, 128]
        let buf = surface.buffer();
        assert_eq!(buf[0], 0); // B
        assert_eq!(buf[1], 0); // G
        assert_eq!(buf[2], 100); // R (premultiplied)
        assert_eq!(buf[3], 128); // A

        // 2. Verify pixmap internal data is restored to RGBA [100, 0, 0, 128]
        let pm_data = pixmap.data();
        assert_eq!(pm_data[0], 100); // R
        assert_eq!(pm_data[1], 0); // G
        assert_eq!(pm_data[2], 0); // B
        assert_eq!(pm_data[3], 128); // A
    }

    #[test]
    fn test_layered_surface_present_dirty() {
        let window = NativeWindow::new(
            "Present Dirty Test",
            Rect::new(50, 50, 64, 64),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("failed to create native window");

        let mut surface =
            LayeredSurface::new(window.hwnd(), 64, 64).expect("failed to create layered surface");
        let mut pixmap = Pixmap::new(64, 64).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(0, 120, 200, 255));

        // Update dirty rect of 16x16 only
        let dirty = Rect::new(16, 16, 16, 16);
        let res = surface.present_dirty(&mut pixmap, 0.9, dirty);
        assert!(res.is_ok(), "present_dirty should succeed: {:?}", res.err());

        // Empty dirty rect should return Ok early
        let empty_dirty = Rect::new(0, 0, 0, 0);
        let empty_res = surface.present_dirty(&mut pixmap, 1.0, empty_dirty);
        assert!(empty_res.is_ok());
    }

    #[test]
    fn test_exact_flags_present() {
        let flags = WindowFlags::FRAMELESS
            | WindowFlags::CUSTOM_FRAMELESS
            | WindowFlags::LAYERED
            | WindowFlags::TOOL
            | WindowFlags::STAYS_ON_TOP;
        let window = NativeWindow::new("Flags Test", Rect::new(100, 100, 450, 350), flags)
            .expect("failed to create native window");
        window.show();

        let mut surface =
            LayeredSurface::new(window.hwnd(), 450, 350).expect("failed to create layered surface");

        let mut pixmap = Pixmap::with_dpr(563, 438, 1.25).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(200, 0, 0, 128));

        let res = surface.present(&mut pixmap, 0.88);
        assert!(res.is_ok(), "exact flags present failed: {:?}", res.err());
    }

    #[test]
    fn test_cross_platform_surface_polymorphism() {
        // Verify X11, Wayland, and macOS backends implement PlatformSurface
        let mut x11: Box<dyn PlatformSurface> = Box::new(X11ShmSurface::new(1001, 80, 60).unwrap());
        assert_eq!(x11.width(), 80);
        assert_eq!(x11.height(), 60);
        x11.resize(100, 80).unwrap();
        assert_eq!(x11.width(), 100);

        let mut wayland: Box<dyn PlatformSurface> =
            Box::new(WaylandShmSurface::new(2001, 80, 60).unwrap());
        assert_eq!(wayland.width(), 80);
        wayland.resize(120, 90).unwrap();
        assert_eq!(wayland.width(), 120);

        let mut macos: Box<dyn PlatformSurface> =
            Box::new(CocoaLayerSurface::new(3001, 80, 60).unwrap());
        assert_eq!(macos.width(), 80);
        macos.resize(140, 100).unwrap();
        assert_eq!(macos.width(), 140);

        // Verify polymorphic present and present_dirty calls
        let mut pm = Pixmap::new(100, 80).unwrap();
        pm.fill(Color::from_rgba8(50, 150, 250, 255));
        assert!(x11.present(&mut pm, 1.0).is_ok());
        assert!(x11
            .present_dirty(&mut pm, 1.0, Rect::new(10, 10, 20, 20))
            .is_ok());
    }

    #[test]
    #[cfg(windows)]
    fn test_native_window_persistent_surface_reuse() {
        use crate::platform_window::PlatformWindow;

        let mut window = NativeWindow::new(
            "Persistent Surface Test",
            Rect::new(20, 20, 120, 90),
            WindowFlags::FRAMELESS | WindowFlags::LAYERED,
        )
        .expect("failed to create window");

        let mut pixmap = Pixmap::new(120, 90).expect("failed to create pixmap");
        pixmap.fill(Color::from_rgba8(20, 40, 60, 255));

        // Multi-frame present_dirty calls reusing the cached surface
        for i in 0..10 {
            let dirty = Rect::new(i * 5, i * 5, 30, 30);
            let res = window.present_dirty(&mut pixmap, 0.95, dirty);
            assert!(
                res.is_ok(),
                "Frame {} present_dirty failed: {:?}",
                i,
                res.err()
            );
        }

        // Test geometry change triggers surface resize
        window.set_geometry(Rect::new(20, 20, 160, 120));
        let mut resized_pixmap = Pixmap::new(160, 120).expect("resized pixmap");
        resized_pixmap.fill(Color::from_rgba8(100, 150, 200, 255));
        let res = window.present_dirty(&mut resized_pixmap, 1.0, Rect::new(0, 0, 160, 120));
        assert!(
            res.is_ok(),
            "Resized window present_dirty failed: {:?}",
            res.err()
        );
    }
}
