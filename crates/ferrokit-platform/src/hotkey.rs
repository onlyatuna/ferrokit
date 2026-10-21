// SPDX-License-Identifier: LGPL-3.0-only
use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct HotkeyModifiers: u32 {
        const ALT = 0x0001;
        const CONTROL = 0x0002;
        const SHIFT = 0x0004;
        const WIN = 0x0008;
        const NO_REPEAT = 0x4000;
    }
}

pub trait PlatformHotkeyManager {
    fn register_hotkey(
        &mut self,
        id: u32,
        modifiers: HotkeyModifiers,
        vk: u32,
    ) -> Result<(), &'static str>;

    fn unregister_hotkey(&mut self, id: u32) -> Result<(), &'static str>;
}

#[cfg(windows)]
pub mod win32_hotkey {
    use super::*;
    use std::collections::HashSet;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
    };

    pub struct Win32HotkeyManager {
        hwnd: HWND,
        registered_ids: HashSet<u32>,
    }

    // Deliberately neither `Send` nor `Sync` (raw `HWND`): `RegisterHotKey`/`UnregisterHotKey`
    // bind a hotkey to the thread that owns `hwnd`, so the manager stays on that thread.

    impl Win32HotkeyManager {
        pub fn new(hwnd: HWND) -> Self {
            Self {
                hwnd,
                registered_ids: HashSet::new(),
            }
        }

        pub fn registered_ids(&self) -> &HashSet<u32> {
            &self.registered_ids
        }
    }

    impl PlatformHotkeyManager for Win32HotkeyManager {
        fn register_hotkey(
            &mut self,
            id: u32,
            modifiers: HotkeyModifiers,
            vk: u32,
        ) -> Result<(), &'static str> {
            let win_mod: HOT_KEY_MODIFIERS = modifiers.bits();
            let success = unsafe { RegisterHotKey(self.hwnd, id as i32, win_mod, vk) };
            if success != 0 {
                self.registered_ids.insert(id);
                Ok(())
            } else {
                Err("RegisterHotKey failed")
            }
        }

        fn unregister_hotkey(&mut self, id: u32) -> Result<(), &'static str> {
            let success = unsafe { UnregisterHotKey(self.hwnd, id as i32) };
            self.registered_ids.remove(&id);
            if success != 0 {
                Ok(())
            } else {
                Err("UnregisterHotKey failed")
            }
        }
    }

    impl Drop for Win32HotkeyManager {
        fn drop(&mut self) {
            let ids: Vec<u32> = self.registered_ids.iter().copied().collect();
            for id in ids {
                let _ = self.unregister_hotkey(id);
            }
        }
    }
}

#[cfg(windows)]
pub use win32_hotkey::Win32HotkeyManager;

#[derive(Debug, Default)]
pub struct GenericHotkeyManager {
    registered_ids: std::collections::HashSet<u32>,
}

impl GenericHotkeyManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn registered_ids(&self) -> &std::collections::HashSet<u32> {
        &self.registered_ids
    }
}

impl PlatformHotkeyManager for GenericHotkeyManager {
    fn register_hotkey(
        &mut self,
        _id: u32,
        _modifiers: HotkeyModifiers,
        _vk: u32,
    ) -> Result<(), &'static str> {
        Err("GenericHotkeyManager does not support global hotkeys")
    }

    fn unregister_hotkey(&mut self, _id: u32) -> Result<(), &'static str> {
        Err("GenericHotkeyManager does not support global hotkeys")
    }
}

pub mod cocoa_hotkey {
    use super::*;
    use std::collections::HashSet;

    #[derive(Debug, Default)]
    pub struct CocoaHotkeyManager {
        registered_ids: HashSet<u32>,
    }

    impl CocoaHotkeyManager {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn registered_ids(&self) -> &HashSet<u32> {
            &self.registered_ids
        }

        /// Map virtual key code to macOS Carbon virtual key code if applicable.
        pub fn map_to_mac_keycode(vk: u32) -> u32 {
            // ASCII 'A'..'Z' -> macOS Carbon keycodes:
            // A=0, S=1, D=2, F=3, H=4, G=5, Z=6, X=7, C=8, V=9, B=11, Q=12, W=13, E=14, R=15, Y=16, T=17
            match vk as u8 as char {
                'A' | 'a' => 0,
                'S' | 's' => 1,
                'D' | 'd' => 2,
                'F' | 'f' => 3,
                'H' | 'h' => 4,
                'G' | 'g' => 5,
                'Z' | 'z' => 6,
                'X' | 'x' => 7,
                'C' | 'c' => 8,
                'V' | 'v' => 9,
                'B' | 'b' => 11,
                'Q' | 'q' => 12,
                'W' | 'w' => 13,
                'E' | 'e' => 14,
                'R' | 'r' => 15,
                'Y' | 'y' => 16,
                'T' | 't' => 17,
                _ => vk,
            }
        }
    }

    impl PlatformHotkeyManager for CocoaHotkeyManager {
        fn register_hotkey(
            &mut self,
            _id: u32,
            _modifiers: HotkeyModifiers,
            _vk: u32,
        ) -> Result<(), &'static str> {
            Err("CocoaHotkeyManager: global hotkeys not implemented")
        }

        fn unregister_hotkey(&mut self, _id: u32) -> Result<(), &'static str> {
            Err("CocoaHotkeyManager: global hotkeys not implemented")
        }
    }

    impl Drop for CocoaHotkeyManager {
        fn drop(&mut self) {
            let ids: Vec<u32> = self.registered_ids.iter().copied().collect();
            for id in ids {
                let _ = self.unregister_hotkey(id);
            }
        }
    }
}

pub use cocoa_hotkey::CocoaHotkeyManager;

pub mod unix_hotkey {
    use super::*;
    use std::collections::HashSet;

    #[derive(Debug, Default)]
    pub struct UnixHotkeyManager {
        registered_ids: HashSet<u32>,
    }

    impl UnixHotkeyManager {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn registered_ids(&self) -> &HashSet<u32> {
            &self.registered_ids
        }

        /// Map HotkeyModifiers to X11 modifier mask
        pub fn x11_modifiers(modifiers: HotkeyModifiers) -> u32 {
            let mut mask = 0;
            if modifiers.contains(HotkeyModifiers::SHIFT) {
                mask |= 1 << 0; // ShiftMask
            }
            if modifiers.contains(HotkeyModifiers::CONTROL) {
                mask |= 1 << 2; // ControlMask
            }
            if modifiers.contains(HotkeyModifiers::ALT) {
                mask |= 1 << 3; // Mod1Mask (Alt)
            }
            if modifiers.contains(HotkeyModifiers::WIN) {
                mask |= 1 << 6; // Mod4Mask (Super/Meta)
            }
            mask
        }
    }
    impl PlatformHotkeyManager for UnixHotkeyManager {
        fn register_hotkey(
            &mut self,
            _id: u32,
            _modifiers: HotkeyModifiers,
            _vk: u32,
        ) -> Result<(), &'static str> {
            Err("UnixHotkeyManager: global hotkeys not implemented")
        }

        fn unregister_hotkey(&mut self, _id: u32) -> Result<(), &'static str> {
            Err("UnixHotkeyManager: global hotkeys not implemented")
        }
    }

    impl Drop for UnixHotkeyManager {
        fn drop(&mut self) {
            let ids: Vec<u32> = self.registered_ids.iter().copied().collect();
            for id in ids {
                let _ = self.unregister_hotkey(id);
            }
        }
    }
}

pub use unix_hotkey::UnixHotkeyManager;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cocoa_hotkey_manager() {
        let mut mgr = CocoaHotkeyManager::new();
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(b'C' as u32), 8);
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(b'A' as u32), 0);

        assert!(mgr.register_hotkey(9527, HotkeyModifiers::ALT, 8).is_err());
        assert!(!mgr.registered_ids().contains(&9527));
        assert!(mgr.unregister_hotkey(9527).is_err());
    }

    #[test]
    fn test_unix_hotkey_manager() {
        let mut mgr = UnixHotkeyManager::new();
        let mask = UnixHotkeyManager::x11_modifiers(HotkeyModifiers::ALT | HotkeyModifiers::SHIFT);
        assert_eq!(mask, (1 << 3) | (1 << 0));

        assert!(mgr
            .register_hotkey(9528, HotkeyModifiers::ALT | HotkeyModifiers::SHIFT, 67)
            .is_err());
        assert!(!mgr.registered_ids().contains(&9528));
        assert!(mgr.unregister_hotkey(9528).is_err());
    }
}
