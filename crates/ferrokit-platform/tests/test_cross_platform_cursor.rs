// SPDX-License-Identifier: LGPL-3.0-only
//! Cross-platform cursor API, macOS `CocoaCursor` 22-shape & override support, and Cocoa window
//! polish (`set_click_through` flags, `calc_frameless_edge` live window bounds).

use std::sync::{Arc, Mutex, MutexGuard};

use ferrokit_core::signal::Signal;
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::image::Icon;
use ferrokit_platform::clipboard::PlatformClipboard;
use ferrokit_platform::cursor::{
    CursorShape, GenericCursor, PlatformCursor, UnixCursor,
    set_cursor_shape as mod_set_cursor_shape,
};
use ferrokit_platform::hotkey::PlatformHotkeyManager;
use ferrokit_platform::platform_tray::PlatformTrayIcon;
use ferrokit_platform::platform_window::PlatformWindow;
use ferrokit_platform::screen::PlatformScreen;
use ferrokit_platform::theme::PlatformTheme;
use ferrokit_platform::{
    GenericPlatformIntegration, PlatformIntegration, WindowFlags, platform, set_cursor_shape,
    set_platform_integration,
};

static CURSOR_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    CURSOR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

struct RecordingCursor {
    recorded: Arc<Mutex<Vec<CursorShape>>>,
}

impl PlatformCursor for RecordingCursor {
    fn change_cursor(&mut self, shape: CursorShape) {
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(shape);
    }
}

struct RecordingPlatform {
    inner: GenericPlatformIntegration,
    recorded: Arc<Mutex<Vec<CursorShape>>>,
}

impl PlatformIntegration for RecordingPlatform {
    fn create_window(
        &self,
        title: &str,
        rect: Rect,
        flags: WindowFlags,
    ) -> Result<Box<dyn PlatformWindow>, &'static str> {
        self.inner.create_window(title, rect, flags)
    }

    fn create_tray_icon(
        &self,
        tooltip: &str,
        icon: &Icon,
    ) -> Result<Box<dyn PlatformTrayIcon>, &'static str> {
        self.inner.create_tray_icon(tooltip, icon)
    }

    fn primary_screen(&self) -> Box<dyn PlatformScreen> {
        self.inner.primary_screen()
    }

    fn screens(&self) -> Vec<Box<dyn PlatformScreen>> {
        self.inner.screens()
    }

    fn screen_at(&self, pos: Point) -> Option<Box<dyn PlatformScreen>> {
        self.inner.screen_at(pos)
    }

    fn screen_changed(&self) -> &Signal<()> {
        self.inner.screen_changed()
    }

    fn theme(&self) -> Arc<dyn PlatformTheme> {
        self.inner.theme()
    }

    fn clipboard(&self) -> Box<dyn PlatformClipboard> {
        self.inner.clipboard()
    }

    fn create_hotkey_manager(&self) -> Result<Box<dyn PlatformHotkeyManager>, &'static str> {
        self.inner.create_hotkey_manager()
    }

    fn cursor(&self) -> Box<dyn PlatformCursor> {
        Box::new(RecordingCursor {
            recorded: Arc::clone(&self.recorded),
        })
    }
}

struct PlatformRestore(Arc<dyn PlatformIntegration>);

impl Drop for PlatformRestore {
    fn drop(&mut self) {
        set_platform_integration(Arc::clone(&self.0));
    }
}

#[test]
fn platform_set_cursor_shape_delegates_to_active_platform_cursor() {
    let _g = lock();
    let _restore = PlatformRestore(platform());
    let recorded = Arc::new(Mutex::new(Vec::new()));
    set_platform_integration(Arc::new(RecordingPlatform {
        inner: GenericPlatformIntegration::default(),
        recorded: Arc::clone(&recorded),
    }));

    set_cursor_shape(CursorShape::SizeFDiag);
    mod_set_cursor_shape(CursorShape::SizeBDiag);
    set_cursor_shape(CursorShape::Arrow);

    let shapes = recorded.lock().unwrap().clone();
    assert_eq!(
        shapes,
        vec![
            CursorShape::SizeFDiag,
            CursorShape::SizeBDiag,
            CursorShape::Arrow,
        ]
    );
}

#[test]
fn generic_and_unix_cursors_track_all_shapes() {
    let mut generic = GenericCursor::new();
    let mut unix = UnixCursor::new();
    for shape in CursorShape::ALL {
        generic.change_cursor(shape);
        unix.change_cursor(shape);
        assert_eq!(generic.current_shape(), shape);
        assert_eq!(unix.current_shape(), shape);
        assert!(!UnixCursor::cursor_name(shape).is_empty());
    }
}

#[cfg(not(target_os = "macos"))]
mod cocoa_mock {
    use super::lock;
    use ferrokit_core::object::ThreadContext;
    use ferrokit_gui::geometry::primitives::{Point, Rect};
    use ferrokit_platform::WindowFlags;
    use ferrokit_platform::cursor::{CocoaCursor, CursorShape, PlatformCursor};
    use ferrokit_platform::objc_runtime::{Class, Id, MockObjcRuntime, ObjcMsg, Sel};
    use ferrokit_platform::platform_window::{PlatformWindow, WindowEdges};
    use ferrokit_platform::window::calc_frameless_edge;
    use ferrokit_platform::window_cocoa::CocoaNativeWindow;
    use ferrokit_platform::window_cocoa_input::{cursor_update, reset_cursor_rects};

    struct OverrideReset;

    impl Drop for OverrideReset {
        fn drop(&mut self) {
            CocoaCursor::clear_override();
        }
    }

    fn current_ns_cursor() -> Id {
        let cls = Class::get("NSCursor").unwrap();
        // SAFETY: mock runtime `NSCursor +currentCursor` takes no arguments and returns an Id.
        unsafe { ObjcMsg::send_class_0(cls, Sel::register("currentCursor")) }
    }

    #[test]
    fn cocoa_cursor_sets_all_22_shapes_and_distinguishes_custom_bitmaps() {
        let _g = lock();
        let _reset = OverrideReset;
        let mut cursor = CocoaCursor::new();

        for shape in CursorShape::ALL {
            cursor.change_cursor(shape);
            assert_eq!(cursor.current_shape(), shape);
            assert_eq!(CocoaCursor::current_global_shape(), shape);
            let wanted = CocoaCursor::native_cursor(shape);
            assert!(!wanted.is_nil(), "{shape:?}");
            assert_eq!(current_ns_cursor(), wanted, "{shape:?}");
        }

        let arrow = CocoaCursor::native_cursor(CursorShape::Arrow);
        for custom_shape in [
            CursorShape::SizeFDiag,
            CursorShape::SizeBDiag,
            CursorShape::SizeAll,
            CursorShape::UpArrow,
            CursorShape::Wait,
            CursorShape::Busy,
            CursorShape::WhatsThis,
            CursorShape::Blank,
        ] {
            let custom = CocoaCursor::native_cursor(custom_shape);
            assert_ne!(
                custom, arrow,
                "{custom_shape:?} must resolve to a dedicated custom NSCursor, not arrowCursor"
            );
        }
    }

    #[test]
    fn cocoa_override_cursor_beats_window_cursor_and_restores_cleanly() {
        let _g = lock();
        let _reset = OverrideReset;
        ThreadContext::init_current(true, None);

        let mut cursor = CocoaCursor::new();
        cursor.change_cursor(CursorShape::PointingHand);
        let before = CocoaCursor::native_cursor(CursorShape::PointingHand);
        assert_eq!(current_ns_cursor(), before);

        let mut win_with_cursor =
            CocoaNativeWindow::new("c1", Rect::new(20, 20, 400, 300), WindowFlags::NORMAL)
                .expect("cocoa window");
        win_with_cursor.set_cursor(CursorShape::IBeam);
        let mut win_without_cursor =
            CocoaNativeWindow::new("c2", Rect::new(40, 40, 400, 300), WindowFlags::NORMAL)
                .expect("cocoa window");
        win_without_cursor.unset_cursor();

        assert!(cursor.set_override_cursor(CursorShape::SizeFDiag));
        assert_eq!(CocoaCursor::override_shape(), Some(CursorShape::SizeFDiag));
        let fdiag = CocoaCursor::native_cursor(CursorShape::SizeFDiag);
        assert_eq!(current_ns_cursor(), fdiag);

        // Regular change_cursor calls must not clobber the active override.
        cursor.change_cursor(CursorShape::Cross);
        assert_eq!(current_ns_cursor(), fdiag);

        let mock = MockObjcRuntime::instance();
        mock.clear_calls();
        assert!(reset_cursor_rects(win_with_cursor.ns_view()));
        assert!(mock.calls_for(win_with_cursor.ns_view()).iter().any(|c| {
            c.selector == "addCursorRect:cursor:"
                && c.args.last()
                    == Some(&ferrokit_platform::objc_runtime::appkit::MockValue::Id(
                        fdiag,
                    ))
        }));

        mock.clear_calls();
        assert!(cursor_update(win_without_cursor.ns_view()));
        assert!(mock.calls_for(fdiag).iter().any(|c| c.selector == "set"));

        // Changing the override replaces the cursor on screen without losing the pre-override
        // remembered cursor.
        assert!(cursor.set_override_cursor(CursorShape::Blank));
        let blank = CocoaCursor::native_cursor(CursorShape::Blank);
        assert_eq!(current_ns_cursor(), blank);

        cursor.clear_override_cursor();
        assert_eq!(CocoaCursor::override_shape(), None);
        assert_eq!(current_ns_cursor(), before);

        // Per-window cursors resume cleanly once the override is cleared.
        let ibeam = CocoaCursor::native_cursor(CursorShape::IBeam);
        mock.clear_calls();
        assert!(cursor_update(win_with_cursor.ns_view()));
        assert!(mock.calls_for(ibeam).iter().any(|c| c.selector == "set"));
        assert!(!cursor_update(win_without_cursor.ns_view()));
    }

    #[test]
    fn cocoa_window_set_cursor_uses_custom_cursors_for_diagonal_and_blank() {
        let _g = lock();
        let _reset = OverrideReset;
        ThreadContext::init_current(true, None);
        let mut win =
            CocoaNativeWindow::new("cursor", Rect::new(0, 0, 320, 240), WindowFlags::NORMAL)
                .expect("cocoa window");

        let mock = MockObjcRuntime::instance();
        for shape in [
            CursorShape::SizeFDiag,
            CursorShape::SizeBDiag,
            CursorShape::SizeAll,
            CursorShape::Blank,
        ] {
            let expected = CocoaCursor::native_cursor(shape);
            win.set_cursor(shape);
            mock.clear_calls();
            assert!(cursor_update(win.ns_view()), "{shape:?}");
            assert!(
                mock.calls_for(expected).iter().any(|c| c.selector == "set"),
                "cursorUpdate: must set custom NSCursor for {shape:?}"
            );
        }
    }

    #[test]
    fn cocoa_set_click_through_updates_window_flags_and_native_property() {
        let _g = lock();
        ThreadContext::init_current(true, None);
        let mut win =
            CocoaNativeWindow::new("click", Rect::new(0, 0, 320, 240), WindowFlags::NORMAL)
                .expect("cocoa window");
        assert!(!win.flags().contains(WindowFlags::CLICK_THROUGH));

        win.set_click_through(true);
        assert!(win.flags().contains(WindowFlags::CLICK_THROUGH));
        assert!(
            MockObjcRuntime::instance()
                .get_object_data(win.ns_window())
                .unwrap()
                .ignores_mouse_events
        );

        win.set_click_through(false);
        assert!(!win.flags().contains(WindowFlags::CLICK_THROUGH));
        assert!(
            !MockObjcRuntime::instance()
                .get_object_data(win.ns_window())
                .unwrap()
                .ignores_mouse_events
        );
    }

    #[test]
    fn calc_frameless_edge_queries_live_cocoa_window_dimensions() {
        let _g = lock();
        ThreadContext::init_current(true, None);
        let win = CocoaNativeWindow::new(
            "frameless",
            Rect::new(50, 50, 800, 500),
            WindowFlags::FRAMELESS,
        )
        .expect("cocoa window");
        let handle = win.native_handle();
        assert_ne!(handle, 0);

        // Coordinates near bottom-right of 800x500 would miss the old hardcoded 600x400 fallback.
        assert_eq!(
            calc_frameless_edge(handle, Point::new(795, 495), false),
            WindowEdges::RIGHT | WindowEdges::BOTTOM
        );
        assert_eq!(
            calc_frameless_edge(handle, Point::new(795, 250), false),
            WindowEdges::RIGHT
        );
        assert_eq!(
            calc_frameless_edge(handle, Point::new(400, 250), false),
            WindowEdges::empty()
        );
        assert_eq!(
            calc_frameless_edge(handle, Point::new(2, 2), true),
            WindowEdges::empty()
        );
    }
}
