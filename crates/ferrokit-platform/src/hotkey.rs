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

/// Global hotkeys on macOS through Carbon's `RegisterEventHotKey`, which (unlike an event tap)
/// needs no Accessibility permission. Everything the manager asks of the OS goes through
/// [`HotkeyBackend`], so the key and modifier tables, the id bookkeeping and the delivery path
/// run against [`MockHotkeyBackend`] on any OS; [`CocoaHotkeyManager::new`] picks the Carbon
/// backend on macOS and an always-failing one elsewhere.
pub mod cocoa_hotkey {
    use super::*;
    use ferrokit_core::event::{Event, EventKind};
    use ferrokit_core::event_loop::post_event_to_thread;
    use ferrokit_core::object::{ObjectId, ThreadId};
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::{LazyLock, Mutex, MutexGuard};

    // Carbon modifier masks (`Events.h`: cmdKey, shiftKey, optionKey, controlKey).
    pub const CARBON_CMD_KEY: u32 = 1 << 8;
    pub const CARBON_SHIFT_KEY: u32 = 1 << 9;
    pub const CARBON_OPTION_KEY: u32 = 1 << 11;
    pub const CARBON_CONTROL_KEY: u32 = 1 << 12;

    /// Carbon modifier mask for `modifiers`. The flags name physical keys, as on the other
    /// back ends: `ALT` is Option, `CONTROL` is Control, `SHIFT` is Shift and `WIN` (the
    /// Super/Windows key) is Command. `NO_REPEAT` has no Carbon counterpart: a hotkey fires once
    /// per press, never on key repeat, so it is always in effect.
    pub fn carbon_modifiers(modifiers: HotkeyModifiers) -> u32 {
        let mut mask = 0;
        if modifiers.contains(HotkeyModifiers::ALT) {
            mask |= CARBON_OPTION_KEY;
        }
        if modifiers.contains(HotkeyModifiers::CONTROL) {
            mask |= CARBON_CONTROL_KEY;
        }
        if modifiers.contains(HotkeyModifiers::SHIFT) {
            mask |= CARBON_SHIFT_KEY;
        }
        if modifiers.contains(HotkeyModifiers::WIN) {
            mask |= CARBON_CMD_KEY;
        }
        mask
    }

    /// Carbon virtual key code (`kVK_ANSI_*`, `kVK_F*`, ... of `Events.h`, ANSI layout) of the
    /// Windows-style virtual key `vk` that [`PlatformHotkeyManager::register_hotkey`] takes:
    /// `'A'..='Z'`, `'0'..='9'`, `VK_F1..VK_F20`, the arrows, editing and numeric keypad keys
    /// and the `VK_OEM_*` punctuation keys. `None` if the key has no Mac counterpart.
    pub fn carbon_keycode(vk: u32) -> Option<u32> {
        Some(match vk {
            0x41 => 0,   // A
            0x53 => 1,   // S
            0x44 => 2,   // D
            0x46 => 3,   // F
            0x48 => 4,   // H
            0x47 => 5,   // G
            0x5A => 6,   // Z
            0x58 => 7,   // X
            0x43 => 8,   // C
            0x56 => 9,   // V
            0x42 => 11,  // B
            0x51 => 12,  // Q
            0x57 => 13,  // W
            0x45 => 14,  // E
            0x52 => 15,  // R
            0x59 => 16,  // Y
            0x54 => 17,  // T
            0x4F => 31,  // O
            0x55 => 32,  // U
            0x49 => 34,  // I
            0x50 => 35,  // P
            0x4C => 37,  // L
            0x4A => 38,  // J
            0x4B => 40,  // K
            0x4E => 45,  // N
            0x4D => 46,  // M
            0x31 => 18,  // 1
            0x32 => 19,  // 2
            0x33 => 20,  // 3
            0x34 => 21,  // 4
            0x35 => 23,  // 5
            0x36 => 22,  // 6
            0x37 => 26,  // 7
            0x38 => 28,  // 8
            0x39 => 25,  // 9
            0x30 => 29,  // 0
            0x0D => 36,  // VK_RETURN
            0x09 => 48,  // VK_TAB
            0x20 => 49,  // VK_SPACE
            0x08 => 51,  // VK_BACK (Delete)
            0x1B => 53,  // VK_ESCAPE
            0x2E => 117, // VK_DELETE (forward delete)
            0x2D => 114, // VK_INSERT (Help)
            0x24 => 115, // VK_HOME
            0x23 => 119, // VK_END
            0x21 => 116, // VK_PRIOR (Page Up)
            0x22 => 121, // VK_NEXT (Page Down)
            0x25 => 123, // VK_LEFT
            0x27 => 124, // VK_RIGHT
            0x28 => 125, // VK_DOWN
            0x26 => 126, // VK_UP
            0x70 => 122, // VK_F1
            0x71 => 120, // F2
            0x72 => 99,  // F3
            0x73 => 118, // F4
            0x74 => 96,  // F5
            0x75 => 97,  // F6
            0x76 => 98,  // F7
            0x77 => 100, // F8
            0x78 => 101, // F9
            0x79 => 109, // F10
            0x7A => 103, // F11
            0x7B => 111, // F12
            0x7C => 105, // F13
            0x7D => 107, // F14
            0x7E => 113, // F15
            0x7F => 106, // F16
            0x80 => 64,  // F17
            0x81 => 79,  // F18
            0x82 => 80,  // F19
            0x83 => 90,  // F20
            0x60 => 82,  // VK_NUMPAD0
            0x61 => 83,  // NUMPAD1
            0x62 => 84,  // NUMPAD2
            0x63 => 85,  // NUMPAD3
            0x64 => 86,  // NUMPAD4
            0x65 => 87,  // NUMPAD5
            0x66 => 88,  // NUMPAD6
            0x67 => 89,  // NUMPAD7
            0x68 => 91,  // NUMPAD8
            0x69 => 92,  // NUMPAD9
            0x6A => 67,  // VK_MULTIPLY
            0x6B => 69,  // VK_ADD
            0x6D => 78,  // VK_SUBTRACT
            0x6E => 65,  // VK_DECIMAL
            0x6F => 75,  // VK_DIVIDE
            0xBA => 41,  // VK_OEM_1 ;
            0xBB => 24,  // VK_OEM_PLUS =
            0xBC => 43,  // VK_OEM_COMMA ,
            0xBD => 27,  // VK_OEM_MINUS -
            0xBE => 47,  // VK_OEM_PERIOD .
            0xBF => 44,  // VK_OEM_2 /
            0xC0 => 50,  // VK_OEM_3 `
            0xDB => 33,  // VK_OEM_4 [
            0xDC => 42,  // VK_OEM_5 \
            0xDD => 30,  // VK_OEM_6 ]
            0xDE => 39,  // VK_OEM_7 '
            _ => return None,
        })
    }

    /// What the manager needs from the OS. One implementation talks to Carbon, the other two
    /// ([`MockHotkeyBackend`], the unavailable backend) exist so the manager runs everywhere.
    ///
    /// A backend does not call the manager: the OS callback (or the mock's
    /// [`press`](MockHotkeyBackend::press)) hands the `slot` the hotkey was registered with to the
    /// manager's routing, which delivers on the owner thread.
    pub trait HotkeyBackend {
        /// Registers the key combination for `slot` (a process-unique id the backend reports
        /// presses with). Returns an opaque handle for [`unregister`](Self::unregister).
        /// Fails if the combination is held by the system or another application.
        fn register(
            &mut self,
            slot: u32,
            keycode: u32,
            modifiers: u32,
        ) -> Result<usize, &'static str>;

        fn unregister(&mut self, handle: usize) -> Result<(), &'static str>;
    }

    /// Where a registered slot's presses go: the manager `instance`, the application's hotkey
    /// `id`, and the thread that owns the manager.
    #[derive(Clone, Copy)]
    struct Route {
        instance: u64,
        id: u32,
        owner: ThreadId,
    }

    static ROUTES: LazyLock<Mutex<HashMap<u32, Route>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    static NEXT_SLOT: AtomicU32 = AtomicU32::new(1);
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

    thread_local! {
        /// Handlers of the managers owned by this thread, by instance. The handler is not
        /// `Send`: it is only ever reached from the owner thread's event loop.
        static HANDLERS: RefCell<HashMap<u64, Rc<dyn Fn(u32)>>> = RefCell::new(HashMap::new());
    }

    fn routes() -> MutexGuard<'static, HashMap<u32, Route>> {
        // The map is only mutated by single inserts/removes, so it is valid after a panic.
        ROUTES.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called by the OS callback for a press of `slot`: queues the delivery on the owner
    /// thread's event loop (so the handler never runs inside the Carbon callback and may
    /// register or unregister hotkeys). Returns whether the slot is known.
    fn route_press(slot: u32) -> bool {
        let Some(route) = routes().get(&slot).copied() else {
            return false;
        };
        post_event_to_thread(
            route.owner,
            ObjectId(0),
            Event::new(EventKind::MetaCallNoTarget(Box::new(move || deliver(slot)))),
        );
        true
    }

    /// Runs on the owner thread. A press queued before its hotkey was unregistered is dropped.
    fn deliver(slot: u32) {
        let Some(route) = routes().get(&slot).copied() else {
            return;
        };
        let handler = HANDLERS.with(|h| h.borrow().get(&route.instance).cloned());
        if let Some(handler) = handler {
            handler(route.id);
        }
    }

    /// `Err` unless the thread asking is the main thread (`is_main`): Carbon hotkeys belong to
    /// the main thread's event target.
    pub fn require_main_thread(is_main: bool) -> Result<(), &'static str> {
        if is_main {
            Ok(())
        } else {
            Err("CocoaHotkeyManager: must be created on the main thread")
        }
    }

    #[cfg(not(target_os = "macos"))]
    struct UnavailableBackend;

    #[cfg(not(target_os = "macos"))]
    impl HotkeyBackend for UnavailableBackend {
        fn register(&mut self, _: u32, _: u32, _: u32) -> Result<usize, &'static str> {
            Err("CocoaHotkeyManager: global hotkeys require macOS")
        }

        fn unregister(&mut self, _: usize) -> Result<(), &'static str> {
            Err("CocoaHotkeyManager: global hotkeys require macOS")
        }
    }

    struct Registration {
        slot: u32,
        handle: usize,
    }

    /// Registers global hotkeys and reports their presses on the thread that created the
    /// manager (the main thread on macOS), through [`set_triggered_handler`](Self::set_triggered_handler).
    ///
    /// Neither `Send` nor `Sync`: Carbon hotkeys belong to the main thread's event target. Every
    /// hotkey still registered is unregistered on drop.
    ///
    /// Newer macOS versions can reject a combination whose only modifiers are Option or
    /// Option+Shift (it would type a character), so registering one may fail; add Command or
    /// Control.
    pub struct CocoaHotkeyManager {
        backend: Box<dyn HotkeyBackend>,
        instance: u64,
        owner: ThreadId,
        registered_ids: HashSet<u32>,
        registrations: HashMap<u32, Registration>,
    }

    impl std::fmt::Debug for CocoaHotkeyManager {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CocoaHotkeyManager")
                .field("registered_ids", &self.registered_ids)
                .finish_non_exhaustive()
        }
    }

    impl Default for CocoaHotkeyManager {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CocoaHotkeyManager {
        /// A manager on the real OS back end: Carbon on macOS (must be created on the main
        /// thread), otherwise one whose registrations all fail.
        pub fn new() -> Self {
            #[cfg(target_os = "macos")]
            let backend: Box<dyn HotkeyBackend> = Box::new(carbon::CarbonBackend::new());
            #[cfg(not(target_os = "macos"))]
            let backend: Box<dyn HotkeyBackend> = Box::new(UnavailableBackend);
            Self::with_backend(backend)
        }

        /// Like [`new`](Self::new), but fails instead of panicking when the Carbon back end
        /// cannot be used: on macOS, if the calling thread is not the main thread (no
        /// `CoreApplication` yet, or another thread). Off macOS it always succeeds.
        pub fn try_new() -> Result<Self, &'static str> {
            #[cfg(target_os = "macos")]
            require_main_thread(ferrokit_core::object::ThreadContext::is_main_thread())?;
            Ok(Self::new())
        }

        /// A manager on `backend`, owned by the calling thread.
        pub fn with_backend(backend: Box<dyn HotkeyBackend>) -> Self {
            Self {
                backend,
                instance: NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed),
                owner: ThreadId::current(),
                registered_ids: HashSet::new(),
                registrations: HashMap::new(),
            }
        }

        pub fn registered_ids(&self) -> &HashSet<u32> {
            &self.registered_ids
        }

        /// Sets the function called with the id of a hotkey when it is pressed. It runs from
        /// the owner thread's event loop, never from inside the OS callback; a press whose
        /// hotkey was unregistered in the meantime is dropped. Replaces any earlier handler.
        pub fn set_triggered_handler(&mut self, handler: impl Fn(u32) + 'static) {
            let handler: Rc<dyn Fn(u32)> = Rc::new(handler);
            HANDLERS.with(|h| h.borrow_mut().insert(self.instance, handler));
        }

        pub fn clear_triggered_handler(&mut self) {
            HANDLERS.with(|h| h.borrow_mut().remove(&self.instance));
        }

        /// Carbon virtual key code for the virtual key `vk`, or `vk` unchanged if the key has no
        /// Mac counterpart (see [`carbon_keycode`]).
        ///
        /// `vk` has Windows virtual-key semantics. Earlier versions also accepted lowercase ASCII
        /// letters (`'a'..='z'`); those values are now the numeric keypad and function keys
        /// (`0x61..=0x7A`), so they map to those keys, not to the letters. Pass uppercase
        /// `'A'..='Z'`.
        pub fn map_to_mac_keycode(vk: u32) -> u32 {
            carbon_keycode(vk).unwrap_or(vk)
        }
    }

    impl PlatformHotkeyManager for CocoaHotkeyManager {
        fn register_hotkey(
            &mut self,
            id: u32,
            modifiers: HotkeyModifiers,
            vk: u32,
        ) -> Result<(), &'static str> {
            if self.registered_ids.contains(&id) {
                return Err("CocoaHotkeyManager: hotkey id already registered");
            }
            let keycode = carbon_keycode(vk).ok_or("CocoaHotkeyManager: unsupported key")?;
            let slot = NEXT_SLOT.fetch_add(1, Ordering::Relaxed);
            // Routed before the OS can report a press, removed again if registering fails.
            routes().insert(
                slot,
                Route {
                    instance: self.instance,
                    id,
                    owner: self.owner,
                },
            );
            match self
                .backend
                .register(slot, keycode, carbon_modifiers(modifiers))
            {
                Ok(handle) => {
                    self.registered_ids.insert(id);
                    self.registrations.insert(id, Registration { slot, handle });
                    Ok(())
                }
                Err(e) => {
                    routes().remove(&slot);
                    Err(e)
                }
            }
        }

        fn unregister_hotkey(&mut self, id: u32) -> Result<(), &'static str> {
            self.registered_ids.remove(&id);
            let Some(reg) = self.registrations.remove(&id) else {
                return Err("CocoaHotkeyManager: hotkey id not registered");
            };
            // Unrouted first: a press already queued for this slot is dropped on delivery.
            routes().remove(&reg.slot);
            self.backend.unregister(reg.handle)
        }
    }

    impl Drop for CocoaHotkeyManager {
        fn drop(&mut self) {
            let ids: Vec<u32> = self.registered_ids.iter().copied().collect();
            for id in ids {
                let _ = self.unregister_hotkey(id);
            }
            // `try_with`: thread-locals may already be gone if the manager outlives its thread.
            let _ = HANDLERS.try_with(|h| h.borrow_mut().remove(&self.instance));
        }
    }

    /// One hotkey held by a [`MockHotkeyBackend`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MockHotkey {
        pub handle: usize,
        pub slot: u32,
        pub keycode: u32,
        pub modifiers: u32,
    }

    #[derive(Debug, Default)]
    struct MockState {
        hotkeys: Vec<MockHotkey>,
        /// Combinations held by "another application".
        occupied: Vec<(u32, u32)>,
        next_handle: usize,
    }

    /// An in-memory hotkey table modelled on Carbon's conflicts: a combination can be held once,
    /// by this table or by an [`occupy`](Self::occupy)ing "other application".
    /// `eventHotKeyExistsErr` is mainly what Carbon reports for a duplicate registration by the
    /// same application; whether it also reports a combination held by another application, or
    /// a system shortcut, depends on the macOS version. Clones share the table, so a test keeps
    /// one while the manager owns the other.
    #[derive(Debug, Clone, Default)]
    pub struct MockHotkeyBackend {
        state: Rc<RefCell<MockState>>,
    }

    impl MockHotkeyBackend {
        pub fn new() -> Self {
            Self::default()
        }

        /// Hotkeys currently registered, in registration order.
        pub fn hotkeys(&self) -> Vec<MockHotkey> {
            self.state.borrow().hotkeys.clone()
        }

        /// Makes `keycode` + `modifiers` (Carbon values) unavailable, like a hotkey of another app.
        pub fn occupy(&self, keycode: u32, modifiers: u32) {
            self.state.borrow_mut().occupied.push((keycode, modifiers));
        }

        /// The user presses `keycode` + `modifiers`. Returns whether a registered hotkey took it.
        pub fn press(&self, keycode: u32, modifiers: u32) -> bool {
            let slot = self
                .state
                .borrow()
                .hotkeys
                .iter()
                .find(|h| h.keycode == keycode && h.modifiers == modifiers)
                .map(|h| h.slot);
            slot.is_some_and(route_press)
        }
    }

    impl HotkeyBackend for MockHotkeyBackend {
        fn register(
            &mut self,
            slot: u32,
            keycode: u32,
            modifiers: u32,
        ) -> Result<usize, &'static str> {
            let mut state = self.state.borrow_mut();
            let key = (keycode, modifiers);
            if state.occupied.contains(&key)
                || state
                    .hotkeys
                    .iter()
                    .any(|h| (h.keycode, h.modifiers) == key)
            {
                return Err("RegisterEventHotKey failed: hotkey already registered");
            }
            state.next_handle += 1;
            let handle = state.next_handle;
            state.hotkeys.push(MockHotkey {
                handle,
                slot,
                keycode,
                modifiers,
            });
            Ok(handle)
        }

        fn unregister(&mut self, handle: usize) -> Result<(), &'static str> {
            let mut state = self.state.borrow_mut();
            match state.hotkeys.iter().position(|h| h.handle == handle) {
                Some(i) => {
                    state.hotkeys.remove(i);
                    Ok(())
                }
                None => Err("UnregisterEventHotKey failed"),
            }
        }
    }

    #[cfg(target_os = "macos")]
    mod carbon {
        use super::*;
        use ferrokit_core::object::ThreadContext;
        use std::ffi::c_void;

        type OSStatus = i32;
        type EventTargetRef = *mut c_void;
        type EventHandlerRef = *mut c_void;
        type EventHandlerCallRef = *mut c_void;
        type EventRef = *mut c_void;
        type EventHotKeyRef = *mut c_void;
        type EventHandlerProc =
            extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus;

        #[repr(C)]
        struct EventTypeSpec {
            event_class: u32,
            event_kind: u32,
        }

        /// `EventHotKeyID`, passed to `RegisterEventHotKey` by value.
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct EventHotKeyId {
            signature: u32,
            id: u32,
        }

        const NO_ERR: OSStatus = 0;
        const EVENT_NOT_HANDLED_ERR: OSStatus = -9874;
        const EVENT_HOT_KEY_EXISTS_ERR: OSStatus = -9878;
        const EVENT_HOT_KEY_INVALID_ERR: OSStatus = -9879;
        const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
        const EVENT_HOT_KEY_PRESSED: u32 = 5;
        const EVENT_PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
        const TYPE_EVENT_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
        /// Our `EventHotKeyID::signature` ('FKIT'): other hotkey events are not ours.
        const SIGNATURE: u32 = u32::from_be_bytes(*b"FKIT");

        #[link(name = "Carbon", kind = "framework")]
        extern "C" {
            fn GetApplicationEventTarget() -> EventTargetRef;
            fn InstallEventHandler(
                target: EventTargetRef,
                handler: EventHandlerProc,
                num_types: u32,
                list: *const EventTypeSpec,
                user_data: *mut c_void,
                out_ref: *mut EventHandlerRef,
            ) -> OSStatus;
            fn GetEventParameter(
                event: EventRef,
                name: u32,
                desired_type: u32,
                actual_type: *mut u32,
                buffer_size: usize,
                actual_size: *mut usize,
                out_data: *mut c_void,
            ) -> OSStatus;
            fn RegisterEventHotKey(
                key_code: u32,
                modifiers: u32,
                hot_key_id: EventHotKeyId,
                target: EventTargetRef,
                options: u32,
                out_ref: *mut EventHotKeyRef,
            ) -> OSStatus;
            fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OSStatus;
        }

        /// Carbon calls this on the main thread, from the application event target, for every
        /// hotkey press. It only looks the slot up and queues the delivery.
        extern "C" fn hot_key_pressed(
            _call: EventHandlerCallRef,
            event: EventRef,
            _user_data: *mut c_void,
        ) -> OSStatus {
            let mut id = EventHotKeyId {
                signature: 0,
                id: 0,
            };
            // SAFETY: `event` is the live event Carbon passed to this handler; `id` is a
            // writable `EventHotKeyID` of exactly the size given, and the other out parameters
            // are allowed to be null.
            let status = unsafe {
                GetEventParameter(
                    event,
                    EVENT_PARAM_DIRECT_OBJECT,
                    TYPE_EVENT_HOT_KEY_ID,
                    std::ptr::null_mut(),
                    std::mem::size_of::<EventHotKeyId>(),
                    std::ptr::null_mut(),
                    &mut id as *mut EventHotKeyId as *mut c_void,
                )
            };
            // A panic must not unwind into Carbon's C frames (undefined behaviour): treat it as
            // "not handled".
            let routed = std::panic::catch_unwind(|| {
                status == NO_ERR && id.signature == SIGNATURE && route_press(id.id)
            })
            .unwrap_or(false);
            if routed {
                NO_ERR
            } else {
                EVENT_NOT_HANDLED_ERR
            }
        }

        /// The one process-wide hotkey-pressed handler, installed on first use. It carries no
        /// per-manager state (presses are routed by slot), so it is never removed and cannot
        /// dangle.
        static HANDLER_INSTALLED: LazyLock<Result<(), &'static str>> = LazyLock::new(|| {
            let spec = EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOT_KEY_PRESSED,
            };
            let mut handler_ref: EventHandlerRef = std::ptr::null_mut();
            // SAFETY: `hot_key_pressed` has the `EventHandlerProcPtr` signature; `spec` is one
            // valid `EventTypeSpec` (count 1) that Carbon copies during the call; `user_data`
            // is unused (null); `handler_ref` is a writable out pointer.
            let status = unsafe {
                InstallEventHandler(
                    GetApplicationEventTarget(),
                    hot_key_pressed,
                    1,
                    &spec,
                    std::ptr::null_mut(),
                    &mut handler_ref,
                )
            };
            if status == NO_ERR {
                Ok(())
            } else {
                Err("InstallEventHandler failed")
            }
        });

        /// The Carbon back end. Hotkeys are delivered through the main thread's event target, so
        /// it is created on, and (being `!Send`, as a boxed trait object) stays on, the main
        /// thread.
        pub(super) struct CarbonBackend;

        impl CarbonBackend {
            pub(super) fn new() -> Self {
                ThreadContext::assert_main_thread("CocoaHotkeyManager::new");
                Self
            }
        }

        impl HotkeyBackend for CarbonBackend {
            fn register(
                &mut self,
                slot: u32,
                keycode: u32,
                modifiers: u32,
            ) -> Result<usize, &'static str> {
                (*HANDLER_INSTALLED)?;
                let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
                // SAFETY: plain Carbon call on the main thread; `hot_key` is a writable out
                // pointer and the application event target outlives the process.
                let status = unsafe {
                    RegisterEventHotKey(
                        keycode,
                        modifiers,
                        EventHotKeyId {
                            signature: SIGNATURE,
                            id: slot,
                        },
                        GetApplicationEventTarget(),
                        0,
                        &mut hot_key,
                    )
                };
                match status {
                    NO_ERR if !hot_key.is_null() => Ok(hot_key as usize),
                    EVENT_HOT_KEY_EXISTS_ERR => {
                        Err("RegisterEventHotKey failed: hotkey already registered")
                    }
                    EVENT_HOT_KEY_INVALID_ERR => Err("RegisterEventHotKey failed: invalid hotkey"),
                    _ => Err("RegisterEventHotKey failed"),
                }
            }

            fn unregister(&mut self, handle: usize) -> Result<(), &'static str> {
                // SAFETY: `handle` came from a successful `RegisterEventHotKey` and the manager
                // passes each handle here exactly once (it forgets the registration first).
                let status = unsafe { UnregisterEventHotKey(handle as EventHotKeyRef) };
                if status == NO_ERR {
                    Ok(())
                } else {
                    Err("UnregisterEventHotKey failed")
                }
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

    use crate::hotkey::cocoa_hotkey::{
        carbon_keycode, carbon_modifiers, MockHotkey, MockHotkeyBackend, CARBON_CMD_KEY,
        CARBON_CONTROL_KEY, CARBON_OPTION_KEY, CARBON_SHIFT_KEY,
    };

    fn manager() -> (CocoaHotkeyManager, MockHotkeyBackend) {
        let mock = MockHotkeyBackend::new();
        (
            CocoaHotkeyManager::with_backend(Box::new(mock.clone())),
            mock,
        )
    }

    #[test]
    fn test_cocoa_hotkey_manager() {
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(b'C' as u32), 8);
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(b'A' as u32), 0);
    }

    #[test]
    fn test_cocoa_hotkey_off_main_thread_is_an_error_not_a_panic() {
        use crate::hotkey::cocoa_hotkey::require_main_thread;
        assert_eq!(require_main_thread(true), Ok(()));
        let err = require_main_thread(false).unwrap_err();
        assert!(err.contains("main thread"), "{err}");
    }

    #[test]
    fn test_cocoa_hotkey_lowercase_ascii_is_a_windows_vk_not_a_letter() {
        // 'a' is VK_NUMPAD1 (0x61), not the letter A.
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(b'a' as u32), 83);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn test_cocoa_hotkey_manager_off_macos_fails() {
        let mut mgr = CocoaHotkeyManager::new();
        assert!(mgr.register_hotkey(9527, HotkeyModifiers::ALT, 8).is_err());
        assert!(!mgr.registered_ids().contains(&9527));
        assert!(mgr.unregister_hotkey(9527).is_err());
    }

    #[test]
    fn test_cocoa_hotkey_modifier_mapping() {
        assert_eq!(carbon_modifiers(HotkeyModifiers::empty()), 0);
        assert_eq!(carbon_modifiers(HotkeyModifiers::ALT), CARBON_OPTION_KEY);
        assert_eq!(
            carbon_modifiers(HotkeyModifiers::CONTROL),
            CARBON_CONTROL_KEY
        );
        assert_eq!(carbon_modifiers(HotkeyModifiers::SHIFT), CARBON_SHIFT_KEY);
        assert_eq!(carbon_modifiers(HotkeyModifiers::WIN), CARBON_CMD_KEY);
        assert_eq!(
            carbon_modifiers(HotkeyModifiers::all()),
            CARBON_OPTION_KEY | CARBON_CONTROL_KEY | CARBON_SHIFT_KEY | CARBON_CMD_KEY
        );
        // Values of cmdKey/shiftKey/optionKey/controlKey in Carbon's Events.h.
        assert_eq!(
            [
                CARBON_CMD_KEY,
                CARBON_SHIFT_KEY,
                CARBON_OPTION_KEY,
                CARBON_CONTROL_KEY
            ],
            [256, 512, 2048, 4096]
        );
        // NO_REPEAT is implicit on Carbon and adds nothing.
        assert_eq!(
            carbon_modifiers(HotkeyModifiers::NO_REPEAT | HotkeyModifiers::ALT),
            CARBON_OPTION_KEY
        );
    }

    #[test]
    fn test_cocoa_hotkey_keycode_table() {
        // kVK_ANSI_* / kVK_* values of Events.h.
        let cases: &[(u32, u32)] = &[
            (0x41, 0),   // A
            (0x5A, 6),   // Z
            (0x4D, 46),  // M
            (0x30, 29),  // 0
            (0x39, 25),  // 9
            (0x0D, 36),  // Return
            (0x20, 49),  // Space
            (0x1B, 53),  // Escape
            (0x08, 51),  // Delete
            (0x2E, 117), // ForwardDelete
            (0x25, 123), // Left
            (0x26, 126), // Up
            (0x70, 122), // F1
            (0x7B, 111), // F12
            (0x83, 90),  // F20
            (0x60, 82),  // Keypad0
            (0x69, 92),  // Keypad9
            (0xBA, 41),  // ;
            (0xC0, 50),  // `
        ];
        for &(vk, code) in cases {
            assert_eq!(carbon_keycode(vk), Some(code), "vk {vk:#x}");
        }
        for vk in [0x00, 0x07, 0x0E, 0x84, 0xFF, 0x1_0000] {
            assert_eq!(carbon_keycode(vk), None, "vk {vk:#x}");
        }
        // Every table entry is a distinct key: two virtual keys never share a Carbon code.
        let mut seen = std::collections::HashSet::new();
        for vk in 0..=0xFFu32 {
            if let Some(code) = carbon_keycode(vk) {
                assert!(
                    seen.insert(code),
                    "duplicate Carbon keycode {code} (vk {vk:#x})"
                );
            }
        }
        // Unmapped keys pass through the legacy helper unchanged.
        assert_eq!(CocoaHotkeyManager::map_to_mac_keycode(0xFF), 0xFF);
    }

    #[test]
    fn test_cocoa_hotkey_register_unregister_lifecycle() {
        let (mut mgr, mock) = manager();
        mgr.register_hotkey(7, HotkeyModifiers::WIN | HotkeyModifiers::SHIFT, 0x43)
            .expect("register");
        assert!(mgr.registered_ids().contains(&7));
        let held = mock.hotkeys();
        assert_eq!(held.len(), 1);
        assert_eq!(
            (held[0].keycode, held[0].modifiers),
            (8, CARBON_CMD_KEY | CARBON_SHIFT_KEY)
        );

        mgr.unregister_hotkey(7).expect("unregister");
        assert!(mgr.registered_ids().is_empty());
        assert!(mock.hotkeys().is_empty());
        // The combination is free again.
        mgr.register_hotkey(7, HotkeyModifiers::WIN | HotkeyModifiers::SHIFT, 0x43)
            .expect("re-register");
        assert_eq!(mock.hotkeys().len(), 1);
    }

    #[test]
    fn test_cocoa_hotkey_errors_leave_no_state() {
        let (mut mgr, mock) = manager();
        mgr.register_hotkey(1, HotkeyModifiers::ALT, 0x41).unwrap();

        // Same id again, and an unmapped key.
        assert!(mgr
            .register_hotkey(1, HotkeyModifiers::CONTROL, 0x42)
            .is_err());
        assert_eq!(
            mgr.register_hotkey(2, HotkeyModifiers::ALT, 0xFF),
            Err("CocoaHotkeyManager: unsupported key")
        );
        // Same combination under another id: a conflict, as with another application.
        assert!(mgr.register_hotkey(3, HotkeyModifiers::ALT, 0x41).is_err());
        mock.occupy(11, CARBON_CONTROL_KEY);
        assert!(mgr
            .register_hotkey(4, HotkeyModifiers::CONTROL, 0x42)
            .is_err());

        assert_eq!(mgr.registered_ids().len(), 1);
        assert_eq!(mock.hotkeys().len(), 1);
        // An id that failed to register is not registered.
        assert!(mgr.unregister_hotkey(3).is_err());
        assert!(mgr.unregister_hotkey(99).is_err());
        mgr.unregister_hotkey(1).unwrap();
        assert!(mgr.unregister_hotkey(1).is_err());
    }

    #[test]
    fn test_cocoa_hotkey_drop_unregisters_everything() {
        let (mut mgr, mock) = manager();
        mgr.register_hotkey(1, HotkeyModifiers::ALT, 0x41).unwrap();
        mgr.register_hotkey(2, HotkeyModifiers::ALT, 0x42).unwrap();
        mgr.register_hotkey(3, HotkeyModifiers::CONTROL, 0x70)
            .unwrap();
        assert_eq!(mock.hotkeys().len(), 3);
        drop(mgr);
        assert_eq!(mock.hotkeys(), Vec::<MockHotkey>::new());
    }

    #[test]
    fn test_cocoa_hotkey_managers_do_not_share_ids() {
        let (mut a, mock_a) = manager();
        let (mut b, mock_b) = manager();
        a.register_hotkey(1, HotkeyModifiers::ALT, 0x41).unwrap();
        b.register_hotkey(1, HotkeyModifiers::ALT, 0x41).unwrap();
        assert_ne!(mock_a.hotkeys()[0].slot, mock_b.hotkeys()[0].slot);
        drop(a);
        assert!(mock_a.hotkeys().is_empty());
        assert_eq!(mock_b.hotkeys().len(), 1);
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
