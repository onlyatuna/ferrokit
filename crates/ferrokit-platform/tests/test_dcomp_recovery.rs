// SPDX-License-Identifier: LGPL-3.0-only
//! Tests DirectComposition device loss resilience, GDI fallback, and WindowSystem self-healing.
#![cfg(windows)]

use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
use ferrokit_platform::PlatformWindow;
use ferrokit_platform::surface::PlatformSurface;
use ferrokit_platform::surface::dcomp::DCompSurface;
use ferrokit_platform::surface::win32::Win32LayeredSurface;
use ferrokit_platform::window::{NativeWindow, WindowFlags};

#[test]
fn test_dcomp_surface_to_gdi_layered_transition_parity() {
    let window = NativeWindow::new(
        "DComp Transition Test",
        Rect::new(50, 50, 120, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    let hwnd = window.hwnd();

    // 1. First establish DComp surface if hardware/platform supports it
    if let Ok(mut dcomp) = DCompSurface::new(hwnd, 120, 80) {
        let mut pixmap = Pixmap::new(120, 80).expect("pixmap allocate");
        pixmap.fill(Color::from_rgba8(20, 40, 80, 220));
        let res = dcomp.present_dirty(&mut pixmap, 1.0, Rect::new(0, 0, 120, 80));
        assert!(res.is_ok(), "DComp initial present failed: {:?}", res);

        // 2. Simulate Device Lost / teardown by dropping DComp surface
        // DirectComposition target is released and unbound from hwnd during drop
        drop(dcomp);
    }

    // 3. Seamlessly create Win32LayeredSurface (GDI fallback) on the exact same HWND
    let mut gdi_surface = Win32LayeredSurface::new(hwnd, 120, 80)
        .expect("Win32LayeredSurface fallback creation failed");

    let mut pixmap_gdi = Pixmap::new(120, 80).expect("pixmap allocate");
    pixmap_gdi.fill(Color::from_rgba8(180, 50, 20, 255));
    let gdi_res = gdi_surface.present_dirty(&mut pixmap_gdi, 1.0, Rect::new(0, 0, 120, 80));
    assert!(
        gdi_res.is_ok(),
        "GDI layered surface presentation after DComp drop failed: {:?}",
        gdi_res
    );
}

#[test]
fn test_window_system_present_dirty_self_healing_recovery() {
    let mut window = NativeWindow::new(
        "Self Healing Test",
        Rect::new(100, 100, 150, 100),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    let mut pixmap = Pixmap::new(150, 100).expect("pixmap");
    pixmap.fill(Color::from_rgba8(10, 100, 200, 240));

    // Initial present through WindowSystem trait
    let res = window.present_dirty(&mut pixmap, 0.9, Rect::new(0, 0, 150, 100));
    assert!(res.is_ok(), "Initial present_dirty failed: {:?}", res);

    // Present second frame with dynamic dirty rectangle
    let dirty = Rect::new(10, 10, 50, 40);
    let res2 = window.present_dirty(&mut pixmap, 0.9, dirty);
    assert!(
        res2.is_ok(),
        "Second frame dirty present failed: {:?}",
        res2
    );

    // Resize window geometry and present updated frame
    window.set_geometry(Rect::new(100, 100, 200, 120));
    let mut pixmap_resized = Pixmap::new(200, 120).expect("pixmap resized");
    pixmap_resized.fill(Color::from_rgba8(10, 150, 220, 255));
    let res3 = window.present(&mut pixmap_resized, 1.0);
    assert!(res3.is_ok(), "Resized frame present failed: {:?}", res3);
}

#[test]
fn test_get_or_create_layered_surface_error_reset_resilience() {
    let mut window = NativeWindow::new(
        "Surface Resilience Test",
        Rect::new(50, 50, 100, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    // 1. Get or create valid surface
    let surf = window.get_or_create_layered_surface(100, 80);
    assert!(surf.is_ok(), "Valid surface creation failed");

    // 2. Trigger invalid dimension (0, 0) resize failure
    let invalid_res = window.get_or_create_layered_surface(0, 0);
    assert!(
        invalid_res.is_err(),
        "Invalid dimension resize must return Err"
    );

    // 3. Verify internal cache was cleared and recovers cleanly on next valid request
    let recovered_surf = window.get_or_create_layered_surface(100, 80);
    assert!(
        recovered_surf.is_ok(),
        "Surface must cleanly recover after error reset"
    );
    let s = recovered_surf.unwrap();
    assert_eq!(s.width(), 100);
    assert_eq!(s.height(), 80);
}

// Mutex to serialize tests that mutate process-global DComp override
static DCOMP_TEST_MUTEX: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

struct DCompOverrideGuard;

impl DCompOverrideGuard {
    fn acquire(enabled: Option<bool>) -> (parking_lot::MutexGuard<'static, ()>, Self) {
        let guard = DCOMP_TEST_MUTEX.lock();
        ferrokit_platform::set_dcomp_enabled(enabled);
        (guard, Self)
    }
}

impl Drop for DCompOverrideGuard {
    fn drop(&mut self) {
        ferrokit_platform::set_dcomp_enabled(None);
    }
}

#[test]
fn test_dcomp_policy_opt_in_and_runtime_downgrade() {
    use ferrokit_platform::is_dcomp_enabled;
    use ferrokit_platform::presenter::WindowsPresenter;

    let (_lock, _guard) = DCompOverrideGuard::acquire(None);

    // 1. By default, DComp is disabled (opt-in policy)
    if std::env::var("FERROKIT_ENABLE_DCOMP").is_err() {
        assert!(!is_dcomp_enabled(), "DComp must be disabled by default");
    }

    let mut window = NativeWindow::new(
        "DComp Policy Test",
        Rect::new(50, 50, 100, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    // Explicitly force disable DComp
    ferrokit_platform::set_dcomp_enabled(Some(false));
    assert!(!is_dcomp_enabled());

    let p = window.get_or_create_presenter(100, 80).expect("presenter");
    match p {
        WindowsPresenter::Layered(_) => {}
        _ => panic!("Expected Win32LayeredPresenter when DComp is disabled"),
    }

    // Explicitly test downgrade flag
    window.set_dcomp_downgraded(true);
    assert!(window.is_dcomp_downgraded());

    // Even if DComp is force-enabled, a downgraded window must still choose Layered
    ferrokit_platform::set_dcomp_enabled(Some(true));
    assert!(is_dcomp_enabled());

    // Recreate to test selection logic (`presenter_mut()` yields `Option<&mut _>`, so there is
    // no presenter to take out here)
    let p_downgraded = window.get_or_create_presenter(100, 80).expect("presenter");
    match p_downgraded {
        WindowsPresenter::Layered(_) => {}
        _ => panic!("Expected Win32LayeredPresenter when window is marked dcomp_downgraded"),
    }
}

#[test]
fn test_dcomp_runtime_present_failure_auto_downgrade_and_recovery_deterministic() {
    use ferrokit_gui::geometry::Region;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::platform_window::PlatformWindow;
    use ferrokit_platform::presenter::WindowsPresenter;
    use ferrokit_platform::surface::DCompSurface;

    let (_lock, _guard) = DCompOverrideGuard::acquire(Some(true));

    let mut window = NativeWindow::new(
        "DComp Deterministic Present Failure Downgrade Test",
        Rect::new(50, 50, 100, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    assert!(!window.is_dcomp_downgraded());

    // Inject mock DComp surface configured to fail on present
    let mut mock_dcomp = DCompSurface::new_mock_for_test(window.hwnd(), 100, 80);
    mock_dcomp.set_fail_present_for_test(true);
    window.set_presenter_for_test(Some(WindowsPresenter::DirectComposition(mock_dcomp)));

    let mut pixmap = Pixmap::new(100, 80).unwrap();
    pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
    let full = Region::from_coords(0, 0, 100, 80);

    // Deterministic verify: DComp fails -> automatically sets dcomp_downgraded = true ->
    // recreates presenter as Layered -> retry succeeds!
    let r = window.present_region(&pixmap, &full);
    assert!(
        r.is_ok(),
        "present_region must recover cleanly via Layered fallback: {:?}",
        r.err()
    );
    assert!(
        window.is_dcomp_downgraded(),
        "Window must be marked dcomp_downgraded after present failure"
    );

    // Verify active presenter is now Layered
    match window.presenter().unwrap() {
        WindowsPresenter::Layered(_) => {}
        _ => panic!("Expected Win32LayeredPresenter after auto downgrade"),
    }

    // Subsequent present remains on Layered
    let r2 = window.present_region(&pixmap, &full);
    assert!(r2.is_ok(), "Subsequent present on Layered must succeed");
}

#[test]
fn test_dcomp_resize_failure_auto_downgrades_flag_deterministic() {
    use ferrokit_platform::presenter::WindowsPresenter;
    use ferrokit_platform::surface::DCompSurface;

    let (_lock, _guard) = DCompOverrideGuard::acquire(Some(true));

    let mut window = NativeWindow::new(
        "DComp Deterministic Resize Failure Downgrade Test",
        Rect::new(50, 50, 100, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    assert!(!window.is_dcomp_downgraded());

    // Inject mock DComp surface configured to fail on resize
    let mut mock_dcomp = DCompSurface::new_mock_for_test(window.hwnd(), 100, 80);
    mock_dcomp.set_fail_resize_for_test(true);
    window.set_presenter_for_test(Some(WindowsPresenter::DirectComposition(mock_dcomp)));

    // get_or_create_presenter triggers p.resize() which fails -> must mark dcomp_downgraded = true
    let res = window.get_or_create_presenter(120, 90);
    assert!(res.is_err(), "Resize failure must return Err");
    assert!(
        window.is_dcomp_downgraded(),
        "dcomp_downgraded must be set to true on resize failure"
    );

    // Next call chooses Layered because dcomp_downgraded is true
    let p_next = window
        .get_or_create_presenter(120, 90)
        .expect("Layered fallback presenter");
    match p_next {
        WindowsPresenter::Layered(_) => {}
        _ => panic!("Expected Win32LayeredPresenter after downgrade"),
    }
}

/// Tests presenter downgrade integration when a real DirectComposition surface object is initialized,
/// coupled with a synthetic Present failure.
///
/// Note: If real DirectComposition creation fails (e.g. CI / VM without D3D11/DXGI hardware),
/// this test emits an explicit skip diagnostic and early-returns (standard Cargo test harness
/// records the process exit as passed; true deterministic downgrade verification is guaranteed by
/// `test_dcomp_*_deterministic`). Furthermore, this test verifies recovery from synthetic Present error,
/// and does NOT simulate actual DXGI `DEVICE_REMOVED` or GPU driver crash.
#[test]
fn test_real_dcomp_initialization_with_synthetic_fault_downgrade() {
    use ferrokit_gui::geometry::Region;
    use ferrokit_gui::paint::Pixmap;
    use ferrokit_gui::tiny_skia::Color;
    use ferrokit_platform::platform_window::PlatformWindow;
    use ferrokit_platform::presenter::WindowsPresenter;
    use ferrokit_platform::surface::DCompSurface;

    let (_lock, _guard) = DCompOverrideGuard::acquire(Some(true));

    let mut window = NativeWindow::new(
        "DComp Real Surface Synthetic Fault Test",
        Rect::new(50, 50, 100, 80),
        WindowFlags::FRAMELESS | WindowFlags::LAYERED,
    )
    .expect("native window create");

    let dcomp_res = DCompSurface::new(window.hwnd(), 100, 80);
    let mut dcomp = match dcomp_res {
        Ok(s) => s,
        Err(e) => {
            // Explicit skip diagnostic: informs runner that real hardware DComp surface could not be created
            eprintln!(
                "[DIAGNOSTIC-SKIP] Real DirectComposition surface creation unsupported in this environment ({e}). \
                 Test completed as explicit skip diagnostic; deterministic downgrade logic verified via test_dcomp_*_deterministic."
            );
            return;
        }
    };

    // When real DComp surface is initialized, test presenter downgrade integration upon synthetic fault
    dcomp.set_fail_present_for_test(true);
    window.set_presenter_for_test(Some(WindowsPresenter::DirectComposition(dcomp)));

    let mut pixmap = Pixmap::new(100, 80).unwrap();
    pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
    let full = Region::from_coords(0, 0, 100, 80);

    let r = window.present_region(&pixmap, &full);
    assert!(
        r.is_ok(),
        "present_region must recover cleanly via Layered fallback after real DComp initialization: {:?}",
        r.err()
    );
    assert!(
        window.is_dcomp_downgraded(),
        "Window must be marked dcomp_downgraded after present failure"
    );

    match window.presenter().unwrap() {
        WindowsPresenter::Layered(_) => {}
        _ => panic!("Expected Win32LayeredPresenter after auto downgrade"),
    }
}
