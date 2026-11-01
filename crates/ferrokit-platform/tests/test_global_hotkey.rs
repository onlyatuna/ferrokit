// SPDX-License-Identifier: LGPL-3.0-only
//! `GlobalHotkeyManager` (a hotkey without a window, typed registration errors, release and
//! shutdown) and the typed errors of `Win32HotkeyManager`. No key is pressed: only unusual
//! combinations are registered, and every one is released again.

use ferrokit_platform::{GlobalHotkeyManager, HotkeyError};

#[cfg(not(windows))]
#[test]
fn off_windows_there_is_no_global_hotkey_back_end() {
    assert_eq!(
        GlobalHotkeyManager::new().unwrap_err(),
        HotkeyError::Unsupported
    );
    assert!(!HotkeyError::Unsupported.to_string().is_empty());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use ferrokit_platform::{HotkeyModifiers, PlatformHotkeyManager, Win32HotkeyManager};

    // F13..F24, one pair of keys per test so tests running side by side never meet.
    const fn f(n: u32) -> u32 {
        0x70 + n - 1
    }
    const ERROR_HOTKEY_ALREADY_REGISTERED: u32 = 1409;
    const MODS: HotkeyModifiers = HotkeyModifiers::CONTROL
        .union(HotkeyModifiers::ALT)
        .union(HotkeyModifiers::SHIFT)
        .union(HotkeyModifiers::WIN)
        .union(HotkeyModifiers::NO_REPEAT);

    #[test]
    fn registers_without_a_window_then_unregisters_and_registers_again() {
        let m = GlobalHotkeyManager::new().unwrap();
        assert!(m.registered_ids().is_empty());
        m.register(10, MODS, f(13)).unwrap();
        assert!(m.is_registered(10));
        assert_eq!(m.registered_ids(), [10]);

        m.unregister(10).unwrap();
        assert!(!m.is_registered(10));
        assert!(m.registered_ids().is_empty());

        m.register(10, MODS, f(13)).unwrap();
        assert!(m.is_registered(10));
    }

    #[test]
    fn a_refused_registration_reports_the_os_error_code() {
        let m = GlobalHotkeyManager::new().unwrap();
        m.register(20, MODS, f(14)).unwrap();

        // The same combination under another id of the same manager: the system refuses it.
        let err = m.register(21, MODS, f(14)).unwrap_err();
        assert_eq!(
            err,
            HotkeyError::Os {
                code: ERROR_HOTKEY_ALREADY_REGISTERED
            }
        );
        assert_eq!(err.os_code(), Some(1409));
        assert!(err.is_already_registered());
        assert!(err.to_string().contains("1409"));
        assert!(
            !m.is_registered(21),
            "a failed registration changes nothing"
        );

        // Another manager (another thread) is refused as well: it is the system's table.
        let other = GlobalHotkeyManager::new().unwrap();
        assert!(
            other
                .register(20, MODS, f(14))
                .unwrap_err()
                .is_already_registered()
        );
        assert!(other.registered_ids().is_empty());

        // Once released the combination is free for the other manager.
        m.unregister(20).unwrap();
        other.register(20, MODS, f(14)).unwrap();
    }

    #[test]
    fn id_bookkeeping_errors_do_not_reach_the_os() {
        let m = GlobalHotkeyManager::new().unwrap();
        m.register(30, MODS, f(15)).unwrap();
        assert_eq!(m.register(30, MODS, f(16)), Err(HotkeyError::IdInUse(30)));
        assert_eq!(m.unregister(31), Err(HotkeyError::NotRegistered(31)));
        assert_eq!(m.registered_ids(), [30]);
        assert_eq!(HotkeyError::IdInUse(30).os_code(), None);
        assert!(!HotkeyError::IdInUse(30).is_already_registered());
    }

    #[test]
    fn replace_moves_an_id_to_a_new_combination_or_keeps_the_old_one() {
        let m = GlobalHotkeyManager::new().unwrap();
        let other = GlobalHotkeyManager::new().unwrap();
        m.replace(40, MODS, f(17)).unwrap(); // free id: plain registration
        m.replace(40, MODS, f(18)).unwrap();
        assert_eq!(m.registered_ids(), [40]);
        // F16 was released by the move, F17 is held.
        other.register(41, MODS, f(17)).unwrap();
        assert!(
            other
                .register(42, MODS, f(18))
                .unwrap_err()
                .is_already_registered()
        );

        // Moving onto a combination another registration holds fails and leaves F17 in place.
        assert!(
            m.replace(40, MODS, f(17))
                .unwrap_err()
                .is_already_registered()
        );
        assert!(m.is_registered(40));
        assert!(
            other
                .register(42, MODS, f(18))
                .unwrap_err()
                .is_already_registered()
        );
    }

    #[test]
    fn dropping_the_manager_releases_its_hotkeys_and_its_thread() {
        let m = GlobalHotkeyManager::new().unwrap();
        m.register(50, MODS, f(19)).unwrap();
        m.register(51, MODS, f(20)).unwrap();
        drop(m);
        // Both combinations are free again, so the worker unregistered them before it ended.
        let m = GlobalHotkeyManager::new().unwrap();
        m.register(50, MODS, f(19)).unwrap();
        m.register(51, MODS, f(20)).unwrap();
    }

    #[test]
    fn the_manager_can_be_shared_between_threads() {
        let m = std::sync::Arc::new(GlobalHotkeyManager::new().unwrap());
        let handles: Vec<_> = [(60, f(21)), (61, f(22)), (62, f(23))]
            .into_iter()
            .map(|(id, vk)| {
                let m = std::sync::Arc::clone(&m);
                std::thread::spawn(move || m.register(id, MODS, vk))
            })
            .collect();
        for h in handles {
            h.join().unwrap().unwrap();
        }
        assert_eq!(m.registered_ids(), [60, 61, 62]);
    }

    #[test]
    fn win32_hotkey_manager_reports_typed_errors_too() {
        let mut a = Win32HotkeyManager::new(std::ptr::null_mut());
        a.try_register_hotkey(70, MODS, f(24)).unwrap();
        assert_eq!(
            a.try_register_hotkey(70, MODS, f(24)),
            Err(HotkeyError::IdInUse(70))
        );
        // Same thread, same (NULL) window, another id: the system refuses the combination.
        let err = a.try_register_hotkey(71, MODS, f(24)).unwrap_err();
        assert!(err.is_already_registered(), "{err:?}");
        assert!(!a.registered_ids().contains(&71));
        // The trait still reports a message.
        assert_eq!(
            a.register_hotkey(71, MODS, f(24)),
            Err("RegisterHotKey failed")
        );

        a.try_unregister_hotkey(70).unwrap();
        assert_eq!(
            a.try_unregister_hotkey(70),
            Err(HotkeyError::NotRegistered(70))
        );
        assert_eq!(a.unregister_hotkey(70), Err("hotkey id not registered"));
    }
}
