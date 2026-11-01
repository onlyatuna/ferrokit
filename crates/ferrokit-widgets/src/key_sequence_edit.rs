// SPDX-License-Identifier: LGPL-3.0-only
//! Keyboard shortcut model (`QKeySequence`, `QKeyCombination`) and the recording
//! editor widget (`QKeySequenceEdit`).

use std::fmt;
use std::str::FromStr;

use ferrokit_core::event::{Event, EventKind, FocusReason};
use ferrokit_core::object::{ObjectData, ObjectId, QObject};
use ferrokit_core::signal::Signal;
use ferrokit_core::timer::{TimerId, TimerType, kill_object_timer};
use ferrokit_gui::geometry::primitives::{Point, PointF, Rect, RectF, Size};
use ferrokit_gui::paint::palette::{ColorGroup, ColorRole, Palette};
use ferrokit_gui::paint::{Brush, Painter, Pen, RenderHint};
use ferrokit_gui::text::{Font, FontMetrics};
use ferrokit_platform::hotkey::HotkeyModifiers;

use crate::focus::FocusPolicy;
use crate::layout::Layout;
use crate::size_policy::{Policy, QSizePolicy};
use crate::widget::{Widget, WidgetBase, WidgetRef, WidgetWeak};

/// Qt modifier masks as stored inside a combined key code (`Qt::KeyboardModifier`).
pub mod key_modifier {
    pub const NONE: u32 = 0;
    pub const SHIFT: u32 = 0x0200_0000;
    pub const CTRL: u32 = 0x0400_0000;
    pub const ALT: u32 = 0x0800_0000;
    pub const META: u32 = 0x1000_0000;
    pub const KEYPAD: u32 = 0x2000_0000;
    /// All modifier bits (`Qt::KeyboardModifierMask`).
    pub const MASK: u32 = 0xFE00_0000;
}

/// `Qt::Key` codes for non-printable keys. Printable keys use their upper-case
/// Unicode code point (e.g. `'A' as u32`).
pub mod qt_key {
    pub const SPACE: u32 = 0x20;
    pub const ESCAPE: u32 = 0x0100_0000;
    pub const TAB: u32 = 0x0100_0001;
    pub const BACKTAB: u32 = 0x0100_0002;
    pub const BACKSPACE: u32 = 0x0100_0003;
    pub const RETURN: u32 = 0x0100_0004;
    pub const ENTER: u32 = 0x0100_0005;
    pub const INSERT: u32 = 0x0100_0006;
    pub const DELETE: u32 = 0x0100_0007;
    pub const PAUSE: u32 = 0x0100_0008;
    pub const PRINT: u32 = 0x0100_0009;
    pub const SYS_REQ: u32 = 0x0100_000a;
    pub const CLEAR: u32 = 0x0100_000b;
    pub const HOME: u32 = 0x0100_0010;
    pub const END: u32 = 0x0100_0011;
    pub const LEFT: u32 = 0x0100_0012;
    pub const UP: u32 = 0x0100_0013;
    pub const RIGHT: u32 = 0x0100_0014;
    pub const DOWN: u32 = 0x0100_0015;
    pub const PAGE_UP: u32 = 0x0100_0016;
    pub const PAGE_DOWN: u32 = 0x0100_0017;
    pub const SHIFT: u32 = 0x0100_0020;
    pub const CONTROL: u32 = 0x0100_0021;
    pub const META: u32 = 0x0100_0022;
    pub const ALT: u32 = 0x0100_0023;
    pub const CAPS_LOCK: u32 = 0x0100_0024;
    pub const NUM_LOCK: u32 = 0x0100_0025;
    pub const SCROLL_LOCK: u32 = 0x0100_0026;
    pub const F1: u32 = 0x0100_0030;
    pub const F35: u32 = 0x0100_0052;
    pub const MENU: u32 = 0x0100_0055;
    pub const HELP: u32 = 0x0100_0058;
    pub const BACK: u32 = 0x0100_0061;
    pub const FORWARD: u32 = 0x0100_0062;
    pub const STOP: u32 = 0x0100_0063;
    pub const REFRESH: u32 = 0x0100_0064;
    pub const VOLUME_DOWN: u32 = 0x0100_0070;
    pub const VOLUME_MUTE: u32 = 0x0100_0071;
    pub const VOLUME_UP: u32 = 0x0100_0072;
    pub const MEDIA_PLAY: u32 = 0x0100_0080;
    pub const MEDIA_STOP: u32 = 0x0100_0081;
    pub const MEDIA_PREVIOUS: u32 = 0x0100_0082;
    pub const MEDIA_NEXT: u32 = 0x0100_0083;
    pub const MEDIA_RECORD: u32 = 0x0100_0084;
    pub const MEDIA_PAUSE: u32 = 0x0100_0085;
    pub const MEDIA_TOGGLE_PLAY_PAUSE: u32 = 0x0100_0086;
    pub const HOME_PAGE: u32 = 0x0100_0090;
    pub const FAVORITES: u32 = 0x0100_0091;
    pub const SEARCH: u32 = 0x0100_0092;
    pub const STANDBY: u32 = 0x0100_0093;
    pub const OPEN_URL: u32 = 0x0100_0094;
    pub const LAUNCH_MAIL: u32 = 0x0100_00a0;
    pub const LAUNCH_MEDIA: u32 = 0x0100_00a1;
    pub const LAUNCH0: u32 = 0x0100_00a2;
    pub const LAUNCH1: u32 = 0x0100_00a3;
    pub const POWER_OFF: u32 = 0x0100_00b7;
    pub const WAKE_UP: u32 = 0x0100_00b8;
    pub const EJECT: u32 = 0x0100_00b9;
    pub const CALCULATOR: u32 = 0x0100_00cb;
    pub const CLOSE: u32 = 0x0100_00ce;
    pub const COPY: u32 = 0x0100_00cf;
    pub const CUT: u32 = 0x0100_00d0;
    pub const PASTE: u32 = 0x0100_00e2;
    pub const RELOAD: u32 = 0x0100_00e6;
    pub const SAVE: u32 = 0x0100_00ea;
    pub const ZOOM_IN: u32 = 0x0100_00f6;
    pub const ZOOM_OUT: u32 = 0x0100_00f7;
    pub const NEW: u32 = 0x0100_0120;
    pub const OPEN: u32 = 0x0100_0121;
    pub const FIND: u32 = 0x0100_0122;
    pub const UNDO: u32 = 0x0100_0123;
    pub const REDO: u32 = 0x0100_0124;
    pub const SELECT: u32 = 0x0101_0000;
    pub const YES: u32 = 0x0101_0001;
    pub const NO: u32 = 0x0101_0002;
    pub const CANCEL: u32 = 0x0102_0001;
    pub const PRINTER: u32 = 0x0102_0002;
    pub const EXECUTE: u32 = 0x0102_0003;
    pub const SLEEP: u32 = 0x0102_0004;
    pub const PLAY: u32 = 0x0102_0005;
    pub const ZOOM: u32 = 0x0102_0006;
    pub const EXIT: u32 = 0x0102_000a;
    /// Unrecognized key (`Qt::Key_unknown`).
    pub const UNKNOWN: u32 = 0x01ff_ffff;
}

/// Key-name table used for parsing and formatting (portable text of `qkeysequence.cpp`).
/// The first entry for a key is its canonical name; later entries are accepted aliases.
const KEY_NAMES: &[(u32, &str)] = &[
    (qt_key::SPACE, "Space"),
    (qt_key::ESCAPE, "Esc"),
    (qt_key::TAB, "Tab"),
    (qt_key::BACKTAB, "Backtab"),
    (qt_key::BACKSPACE, "Backspace"),
    (qt_key::RETURN, "Return"),
    (qt_key::ENTER, "Enter"),
    (qt_key::INSERT, "Ins"),
    (qt_key::DELETE, "Del"),
    (qt_key::PAUSE, "Pause"),
    (qt_key::PRINT, "Print"),
    (qt_key::SYS_REQ, "SysReq"),
    (qt_key::HOME, "Home"),
    (qt_key::END, "End"),
    (qt_key::LEFT, "Left"),
    (qt_key::UP, "Up"),
    (qt_key::RIGHT, "Right"),
    (qt_key::DOWN, "Down"),
    (qt_key::PAGE_UP, "PgUp"),
    (qt_key::PAGE_DOWN, "PgDown"),
    (qt_key::CAPS_LOCK, "CapsLock"),
    (qt_key::NUM_LOCK, "NumLock"),
    (qt_key::SCROLL_LOCK, "ScrollLock"),
    (qt_key::MENU, "Menu"),
    (qt_key::HELP, "Help"),
    (qt_key::BACK, "Back"),
    (qt_key::FORWARD, "Forward"),
    (qt_key::STOP, "Stop"),
    (qt_key::REFRESH, "Refresh"),
    (qt_key::VOLUME_DOWN, "Volume Down"),
    (qt_key::VOLUME_MUTE, "Volume Mute"),
    (qt_key::VOLUME_UP, "Volume Up"),
    (qt_key::MEDIA_PLAY, "Media Play"),
    (qt_key::MEDIA_STOP, "Media Stop"),
    (qt_key::MEDIA_PREVIOUS, "Media Previous"),
    (qt_key::MEDIA_NEXT, "Media Next"),
    (qt_key::MEDIA_RECORD, "Media Record"),
    (qt_key::MEDIA_PAUSE, "Media Pause"),
    (qt_key::MEDIA_TOGGLE_PLAY_PAUSE, "Toggle Media Play/Pause"),
    (qt_key::HOME_PAGE, "Home Page"),
    (qt_key::FAVORITES, "Favorites"),
    (qt_key::SEARCH, "Search"),
    (qt_key::STANDBY, "Standby"),
    (qt_key::OPEN_URL, "Open URL"),
    (qt_key::LAUNCH_MAIL, "Launch Mail"),
    (qt_key::LAUNCH_MEDIA, "Launch Media"),
    (qt_key::LAUNCH0, "Launch (0)"),
    (qt_key::LAUNCH1, "Launch (1)"),
    (qt_key::POWER_OFF, "Power Off"),
    (qt_key::WAKE_UP, "Wake Up"),
    (qt_key::EJECT, "Eject"),
    (qt_key::CALCULATOR, "Calculator"),
    (qt_key::CLEAR, "Clear"),
    (qt_key::CLOSE, "Close"),
    (qt_key::COPY, "Copy"),
    (qt_key::CUT, "Cut"),
    (qt_key::PASTE, "Paste"),
    (qt_key::RELOAD, "Reload"),
    (qt_key::SAVE, "Save"),
    (qt_key::ZOOM_IN, "Zoom In"),
    (qt_key::ZOOM_OUT, "Zoom Out"),
    (qt_key::NEW, "New"),
    (qt_key::OPEN, "Open"),
    (qt_key::FIND, "Find"),
    (qt_key::UNDO, "Undo"),
    (qt_key::REDO, "Redo"),
    (qt_key::PRINT, "Print Screen"),
    (qt_key::PAGE_UP, "Page Up"),
    (qt_key::PAGE_DOWN, "Page Down"),
    (qt_key::CAPS_LOCK, "Caps Lock"),
    (qt_key::NUM_LOCK, "Num Lock"),
    (qt_key::NUM_LOCK, "Number Lock"),
    (qt_key::SCROLL_LOCK, "Scroll Lock"),
    (qt_key::INSERT, "Insert"),
    (qt_key::DELETE, "Delete"),
    (qt_key::ESCAPE, "Escape"),
    (qt_key::SYS_REQ, "System Request"),
    (qt_key::SELECT, "Select"),
    (qt_key::YES, "Yes"),
    (qt_key::NO, "No"),
    (qt_key::CANCEL, "Cancel"),
    (qt_key::PRINTER, "Printer"),
    (qt_key::EXECUTE, "Execute"),
    (qt_key::SLEEP, "Sleep"),
    (qt_key::PLAY, "Play"),
    (qt_key::ZOOM, "Zoom"),
    (qt_key::EXIT, "Exit"),
    (qt_key::SHIFT, "Shift"),
    (qt_key::CONTROL, "Control"),
    (qt_key::ALT, "Alt"),
    (qt_key::META, "Meta"),
];

/// Modifier prefixes recognized when parsing (lower-case, `decodeString`).
const MODIFIER_PREFIXES: &[(u32, &str)] = &[
    (key_modifier::CTRL, "ctrl+"),
    (key_modifier::SHIFT, "shift+"),
    (key_modifier::ALT, "alt+"),
    (key_modifier::META, "meta+"),
    (key_modifier::KEYPAD, "num+"),
];

/// A key plus modifiers (`QKeyCombination`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyCombination {
    /// `Qt::Key` code.
    pub key: u32,
    /// `key_modifier` mask bits.
    pub modifiers: u32,
}

/// Canonical Qt alias.
pub type QKeyCombination = KeyCombination;

impl KeyCombination {
    pub const fn new(key: u32, modifiers: u32) -> Self {
        Self {
            key,
            modifiers: modifiers & key_modifier::MASK,
        }
    }

    /// Splits a combined `key | modifiers` code (`QKeyCombination::fromCombined`).
    pub const fn from_combined(combined: u32) -> Self {
        Self {
            key: combined & !key_modifier::MASK,
            modifiers: combined & key_modifier::MASK,
        }
    }

    /// `key | modifiers` (`QKeyCombination::toCombined`).
    pub const fn to_combined(self) -> u32 {
        self.key | self.modifiers
    }

    /// Builds a combination from a raw key event, normalizing Win32 virtual-key
    /// codes and platform modifier bits to Qt values.
    pub fn from_key_event(key: u32, modifiers: u32) -> Self {
        let (qt, keypad) = normalize_key(key, modifiers);
        let mut mods = normalize_modifiers(modifiers);
        if keypad {
            mods |= key_modifier::KEYPAD;
        }
        Self::new(qt, mods)
    }

    /// Portable text for this combination, e.g. `"Ctrl+Shift+S"` (`encodeString`).
    pub fn to_portable_string(self) -> String {
        if self.key == qt_key::UNKNOWN || self.to_combined() == 0 {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::with_capacity(5);
        for (mask, name) in [
            (key_modifier::META, "Meta"),
            (key_modifier::CTRL, "Ctrl"),
            (key_modifier::ALT, "Alt"),
            (key_modifier::SHIFT, "Shift"),
            (key_modifier::KEYPAD, "Num"),
        ] {
            if self.modifiers & mask != 0 {
                parts.push(name.to_string());
            }
        }
        parts.push(key_name(self.key));
        parts.join("+")
    }
}

/// Win32 virtual-key code of a `Qt::Key` (`keypad`: the key is on the numeric keypad,
/// `Qt::KeypadModifier`), or `None` for keys with no virtual key a global hotkey can use: the
/// modifier keys themselves, `Key_unknown`, and keys outside the Windows keyboard (media keys
/// without a `VK_*` code, `Key_Select` ...). This is the inverse of [`normalize_key`].
fn key_to_virtual_key(key: u32, keypad: bool) -> Option<u32> {
    const KEYPAD_PUNCTUATION: &[(char, u32)] = &[
        ('*', 0x6A),
        ('+', 0x6B),
        (',', 0x6C),
        ('-', 0x6D),
        ('.', 0x6E),
        ('/', 0x6F),
    ];
    const OEM_PUNCTUATION: &[(char, u32)] = &[
        (';', 0xBA),
        ('=', 0xBB),
        (',', 0xBC),
        ('-', 0xBD),
        ('.', 0xBE),
        ('/', 0xBF),
        ('`', 0xC0),
        ('[', 0xDB),
        ('\\', 0xDC),
        (']', 0xDD),
        ('\'', 0xDE),
    ];
    if key < qt_key::ESCAPE {
        let c = char::from_u32(key)?;
        return match c {
            ' ' => Some(0x20),
            '0'..='9' if keypad => Some(0x60 + (c as u32 - '0' as u32)),
            '0'..='9' | 'A'..='Z' => Some(c as u32),
            _ => {
                let table = if keypad {
                    KEYPAD_PUNCTUATION
                } else {
                    OEM_PUNCTUATION
                };
                table.iter().find(|(ch, _)| *ch == c).map(|(_, vk)| *vk)
            }
        };
    }
    if (qt_key::F1..=qt_key::F1 + 23).contains(&key) {
        return Some(0x70 + (key - qt_key::F1));
    }
    Some(match key {
        qt_key::BACKSPACE => 0x08,
        qt_key::TAB => 0x09,
        qt_key::CLEAR => 0x0C,
        qt_key::RETURN | qt_key::ENTER => 0x0D,
        qt_key::PAUSE => 0x13,
        qt_key::CAPS_LOCK => 0x14,
        qt_key::ESCAPE => 0x1B,
        qt_key::PAGE_UP => 0x21,
        qt_key::PAGE_DOWN => 0x22,
        qt_key::END => 0x23,
        qt_key::HOME => 0x24,
        qt_key::LEFT => 0x25,
        qt_key::UP => 0x26,
        qt_key::RIGHT => 0x27,
        qt_key::DOWN => 0x28,
        qt_key::PRINT => 0x2C,
        qt_key::INSERT => 0x2D,
        qt_key::DELETE => 0x2E,
        qt_key::HELP => 0x2F,
        qt_key::MENU => 0x5D,
        qt_key::NUM_LOCK => 0x90,
        qt_key::SCROLL_LOCK => 0x91,
        qt_key::BACK => 0xA6,
        qt_key::FORWARD => 0xA7,
        qt_key::REFRESH => 0xA8,
        qt_key::STOP => 0xA9,
        qt_key::SEARCH => 0xAA,
        qt_key::FAVORITES => 0xAB,
        qt_key::HOME_PAGE => 0xAC,
        qt_key::VOLUME_MUTE => 0xAD,
        qt_key::VOLUME_DOWN => 0xAE,
        qt_key::VOLUME_UP => 0xAF,
        qt_key::MEDIA_NEXT => 0xB0,
        qt_key::MEDIA_PREVIOUS => 0xB1,
        qt_key::MEDIA_STOP => 0xB2,
        qt_key::MEDIA_TOGGLE_PLAY_PAUSE => 0xB3,
        qt_key::LAUNCH_MAIL => 0xB4,
        qt_key::LAUNCH_MEDIA => 0xB5,
        qt_key::LAUNCH0 => 0xB6,
        qt_key::LAUNCH1 => 0xB7,
        _ => return None,
    })
}

impl KeyCombination {
    /// The modifier set and Win32 virtual-key code to hand to a global hotkey registration
    /// (`GlobalHotkeyManager::register`, `PlatformHotkeyManager::register_hotkey`), or `None` if
    /// this combination cannot be one: no key, a modifier key on its own, or a key with no
    /// virtual-key code (see [`qt_key`]). `Qt::MetaModifier` is the Windows key; `Qt::KeypadModifier`
    /// selects the numeric-keypad virtual key instead of being a modifier. The result never
    /// contains `HotkeyModifiers::NO_REPEAT`; add it for one event per press.
    pub fn to_hotkey(self) -> Option<(HotkeyModifiers, u32)> {
        let vk = key_to_virtual_key(self.key, self.modifiers & key_modifier::KEYPAD != 0)?;
        let mut modifiers = HotkeyModifiers::empty();
        for (mask, flag) in [
            (key_modifier::SHIFT, HotkeyModifiers::SHIFT),
            (key_modifier::CTRL, HotkeyModifiers::CONTROL),
            (key_modifier::ALT, HotkeyModifiers::ALT),
            (key_modifier::META, HotkeyModifiers::WIN),
        ] {
            if self.modifiers & mask != 0 {
                modifiers |= flag;
            }
        }
        Some((modifiers, vk))
    }

    /// The combination a global hotkey of `modifiers` + virtual key `vk` stands for (the inverse
    /// of [`to_hotkey`](Self::to_hotkey); `HotkeyModifiers::NO_REPEAT` is not a modifier and is
    /// ignored). `None` for a virtual key that is not one of the keys `to_hotkey` produces.
    pub fn from_hotkey(modifiers: HotkeyModifiers, vk: u32) -> Option<Self> {
        // Virtual keys that `normalize_key` reads as the ASCII punctuation sharing their value.
        let (key, keypad) = match vk {
            0x2C => (qt_key::PRINT, false),
            0x2F => (qt_key::HELP, false),
            _ => normalize_key(vk, 0),
        };
        if key_to_virtual_key(key, keypad) != Some(vk) {
            return None;
        }
        let mut mask = if keypad { key_modifier::KEYPAD } else { 0 };
        for (flag, qt_mask) in [
            (HotkeyModifiers::SHIFT, key_modifier::SHIFT),
            (HotkeyModifiers::CONTROL, key_modifier::CTRL),
            (HotkeyModifiers::ALT, key_modifier::ALT),
            (HotkeyModifiers::WIN, key_modifier::META),
        ] {
            if modifiers.contains(flag) {
                mask |= qt_mask;
            }
        }
        Some(Self::new(key, mask))
    }
}

/// Maps a key to its display name (`QKeySequencePrivate::keyName`, portable text).
pub fn key_name(key: u32) -> String {
    if key != 0 && key < qt_key::ESCAPE && key != qt_key::SPACE {
        return char::from_u32(key)
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
    }
    if (qt_key::F1..=qt_key::F35).contains(&key) {
        return format!("F{}", key - qt_key::F1 + 1);
    }
    if key == 0 {
        return String::new();
    }
    KEY_NAMES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, name)| name.to_string())
        .or_else(|| char::from_u32(key).map(|c| c.to_uppercase().collect()))
        .unwrap_or_default()
}

/// Converts platform modifier bits (`KeyboardModifiers`: 1 Shift, 2 Ctrl, 4 Alt,
/// 8 Meta, 16 Keypad) or Qt masks into Qt masks.
pub fn normalize_modifiers(modifiers: u32) -> u32 {
    let mut out = modifiers & key_modifier::MASK;
    for (platform_bit, qt_mask) in [
        (1, key_modifier::SHIFT),
        (2, key_modifier::CTRL),
        (4, key_modifier::ALT),
        (8, key_modifier::META),
        (16, key_modifier::KEYPAD),
    ] {
        if modifiers & platform_bit != 0 {
            out |= qt_mask;
        }
    }
    out
}

/// Normalizes a key event code to a `Qt::Key`; returns `(key, is_keypad)`.
///
/// Codes `>= 0x0100_0000` are already Qt keys. Below that, Win32 virtual-key codes
/// without a printable meaning are translated, following the crate convention:
/// `0x21..=0x28` and `0x2E` are navigation keys, `0x60..=0x6F` the numeric keypad,
/// `0x70..=0x87` function keys and `0xBA..=0xDE` the US-layout OEM punctuation keys.
/// Other printable ASCII codes keep their Qt meaning (upper-cased letters).
pub fn normalize_key(key: u32, modifiers: u32) -> (u32, bool) {
    if key >= 0x0100_0000 {
        return (key, false);
    }
    let meta_held = normalize_modifiers(modifiers) & key_modifier::META != 0;
    let mapped = match key {
        0x08 => qt_key::BACKSPACE,
        0x09 => qt_key::TAB,
        0x0C => qt_key::CLEAR,
        0x0D => qt_key::RETURN,
        0x10 | 0xA0 | 0xA1 => qt_key::SHIFT,
        0x11 | 0xA2 | 0xA3 => qt_key::CONTROL,
        0x12 | 0xA4 | 0xA5 => qt_key::ALT,
        0x13 => qt_key::PAUSE,
        0x14 => qt_key::CAPS_LOCK,
        0x1B => qt_key::ESCAPE,
        0x21 => qt_key::PAGE_UP,
        0x22 => qt_key::PAGE_DOWN,
        0x23 => qt_key::END,
        0x24 => qt_key::HOME,
        0x25 => qt_key::LEFT,
        0x26 => qt_key::UP,
        0x27 => qt_key::RIGHT,
        0x28 => qt_key::DOWN,
        0x2D => qt_key::INSERT,
        0x2E => qt_key::DELETE,
        0x5D => qt_key::MENU,
        // VK_LWIN / VK_RWIN arrive with the Meta modifier held.
        0x5B | 0x5C if meta_held => qt_key::META,
        0x60..=0x69 => return (key - 0x60 + '0' as u32, true),
        0x6A => return ('*' as u32, true),
        0x6B => return ('+' as u32, true),
        0x6C => return (',' as u32, true),
        0x6D => return ('-' as u32, true),
        0x6E => return ('.' as u32, true),
        0x6F => return ('/' as u32, true),
        0x70..=0x87 => qt_key::F1 + (key - 0x70),
        0x90 => qt_key::NUM_LOCK,
        0x91 => qt_key::SCROLL_LOCK,
        0xA6 => qt_key::BACK,
        0xA7 => qt_key::FORWARD,
        0xA8 => qt_key::REFRESH,
        0xA9 => qt_key::STOP,
        0xAA => qt_key::SEARCH,
        0xAB => qt_key::FAVORITES,
        0xAC => qt_key::HOME_PAGE,
        0xAD => qt_key::VOLUME_MUTE,
        0xAE => qt_key::VOLUME_DOWN,
        0xAF => qt_key::VOLUME_UP,
        0xB0 => qt_key::MEDIA_NEXT,
        0xB1 => qt_key::MEDIA_PREVIOUS,
        0xB2 => qt_key::MEDIA_STOP,
        0xB3 => qt_key::MEDIA_TOGGLE_PLAY_PAUSE,
        0xB4 => qt_key::LAUNCH_MAIL,
        0xB5 => qt_key::LAUNCH_MEDIA,
        0xB6 => qt_key::LAUNCH0,
        0xB7 => qt_key::LAUNCH1,
        0xBA => ';' as u32,
        0xBB => '=' as u32,
        0xBC => ',' as u32,
        0xBD => '-' as u32,
        0xBE => '.' as u32,
        0xBF => '/' as u32,
        0xC0 => '`' as u32,
        0xDB => '[' as u32,
        0xDC => '\\' as u32,
        0xDD => ']' as u32,
        0xDE => '\'' as u32,
        other => char::from_u32(other)
            .map(|c| c.to_uppercase().next().unwrap_or(c) as u32)
            .unwrap_or(qt_key::UNKNOWN),
    };
    (mapped, false)
}

/// Parses one combination such as `"Ctrl+Shift+S"` (`QKeySequencePrivate::decodeString`,
/// portable text). Unrecognized text yields `qt_key::UNKNOWN`.
pub fn decode_combination(text: &str) -> KeyCombination {
    let unknown = KeyCombination::new(qt_key::UNKNOWN, 0);
    let accel: Vec<char> = text.to_lowercase().chars().collect();
    if accel.is_empty() {
        return unknown;
    }
    let mut modifiers = 0u32;
    let mut single_plus: Option<usize> = None;
    let mut last = 0usize;
    let mut search_from = 1usize;
    while let Some(offset) = accel
        .get(search_from..)
        .and_then(|rest| rest.iter().position(|&c| c == '+'))
    {
        let i = search_from + offset;
        let mut start = last;
        while i + 1 - start > 1 && accel[start] == ' ' {
            start += 1;
        }
        let sub: String = accel[start..=i].iter().collect();
        if sub.chars().count() == 1 {
            // A lone '+' is only allowed once, as the key itself.
            if single_plus.is_some() {
                return unknown;
            }
            single_plus = Some(start);
        } else {
            let compact: String = sub.chars().filter(|&c| c != ' ').collect();
            match MODIFIER_PREFIXES
                .iter()
                .find(|(_, name)| *name == sub || *name == compact)
            {
                Some((mask, _)) => modifiers |= mask,
                None => return unknown,
            }
        }
        last = i + 1;
        search_from = i + 1;
    }

    let search_end = match single_plus {
        Some(sp) if sp > 0 => sp - 1,
        _ => accel.len() - 1,
    };
    let key_start = match accel[..=search_end].iter().rposition(|&c| c == '+') {
        Some(p) if p > 0 => p + 1,
        _ => 0,
    };
    let mut key_chars = &accel[key_start..];
    while key_chars.len() > 1 && key_chars[0] == ' ' {
        key_chars = &key_chars[1..];
    }
    while key_chars.len() > 1 && key_chars[key_chars.len() - 1] == ' ' {
        key_chars = &key_chars[..key_chars.len() - 1];
    }
    if key_chars.is_empty() {
        return unknown;
    }
    let key_text: String = key_chars.iter().collect();
    let key = if key_chars.len() == 1 {
        key_chars[0].to_uppercase().next().unwrap_or(key_chars[0]) as u32
    } else if let Some(n) = key_text
        .strip_prefix('f')
        .and_then(|rest| rest.parse::<u32>().ok())
        .filter(|n| (1..=35).contains(n))
    {
        qt_key::F1 + n - 1
    } else {
        match KEY_NAMES
            .iter()
            .find(|(_, name)| name.to_lowercase() == key_text)
        {
            Some((key, _)) => *key,
            None => return unknown,
        }
    };
    KeyCombination::new(key, modifiers)
}

/// Result of comparing key sequences (`QKeySequence::SequenceMatch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SequenceMatch {
    NoMatch,
    PartialMatch,
    ExactMatch,
}

/// Up to four key combinations pressed in succession (`QKeySequence`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct KeySequence {
    keys: Vec<KeyCombination>,
}

/// Canonical Qt alias.
pub type QKeySequence = KeySequence;

impl KeySequence {
    /// Maximum number of chords in a sequence (`QKeySequencePrivate::MaxKeyCount`).
    pub const MAX_KEY_COUNT: usize = 4;

    /// The empty sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a sequence from up to four combinations; extra entries are ignored.
    pub fn from_keys(keys: &[KeyCombination]) -> Self {
        Self {
            keys: keys
                .iter()
                .copied()
                .filter(|k| k.to_combined() != 0)
                .take(Self::MAX_KEY_COUNT)
                .collect(),
        }
    }

    /// Parses portable text like `"Ctrl+X, Ctrl+C"` (`QKeySequence::fromString`).
    pub fn from_portable_string(text: &str) -> Self {
        let mut keys = Vec::new();
        let mut rest: Vec<char> = text.chars().collect();
        while !rest.is_empty() && keys.len() < Self::MAX_KEY_COUNT {
            // Split at the chord separator ", " while allowing ',' as a key ("Ctrl+,").
            let mut split: Option<usize> = rest.iter().position(|&c| c == ',');
            let mut skip_space = 0;
            if let Some(mut p) = split {
                if p == rest.len() - 1 {
                    split = None;
                } else {
                    if rest[p + 1] == ',' {
                        p += 1;
                    }
                    if rest.get(p + 1) == Some(&' ') {
                        skip_space = 1;
                        p += 1;
                    }
                    split = Some(p);
                }
            }
            let (part, remainder): (String, Vec<char>) = match split {
                Some(p) => (
                    rest[..p - skip_space].iter().collect(),
                    rest[p + 1..].to_vec(),
                ),
                None => (rest.iter().collect(), Vec::new()),
            };
            keys.push(decode_combination(&part));
            rest = remainder;
        }
        Self { keys }
    }

    /// Portable text, chords joined by `", "` (`QKeySequence::toString(PortableText)`).
    pub fn to_portable_string(&self) -> String {
        self.keys
            .iter()
            .map(|k| k.to_portable_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Number of chords (`QKeySequence::count`).
    pub fn count(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Chord at `index` (`QKeySequence::operator[]`).
    pub fn get(&self, index: usize) -> Option<KeyCombination> {
        self.keys.get(index).copied()
    }

    pub fn keys(&self) -> &[KeyCombination] {
        &self.keys
    }

    /// Whether this sequence is a prefix of (`PartialMatch`) or equal to
    /// (`ExactMatch`) `sequence` (`QKeySequence::matches`).
    pub fn matches(&self, sequence: &KeySequence) -> SequenceMatch {
        if self.keys.len() > sequence.keys.len() {
            return SequenceMatch::NoMatch;
        }
        if self.keys.iter().zip(&sequence.keys).any(|(a, b)| a != b) {
            return SequenceMatch::NoMatch;
        }
        if self.keys.len() == sequence.keys.len() {
            SequenceMatch::ExactMatch
        } else {
            SequenceMatch::PartialMatch
        }
    }

    /// The modifier set and virtual-key code of a one-chord sequence such as `"Alt+C"`, for a
    /// global hotkey; `None` for an empty or multi-chord sequence, or if the chord cannot be a
    /// hotkey (see [`KeyCombination::to_hotkey`]).
    pub fn to_hotkey(&self) -> Option<(HotkeyModifiers, u32)> {
        match self.keys.as_slice() {
            [only] => only.to_hotkey(),
            _ => None,
        }
    }

    /// The one-chord sequence of a global hotkey (see [`KeyCombination::from_hotkey`]).
    pub fn from_hotkey(modifiers: HotkeyModifiers, vk: u32) -> Option<Self> {
        KeyCombination::from_hotkey(modifiers, vk).map(|key| Self { keys: vec![key] })
    }
}

impl fmt::Display for KeySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_portable_string())
    }
}

impl FromStr for KeySequence {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_portable_string(s))
    }
}

/// Delay after releasing the first key before recording finishes (Qt: 1 s).
const RELEASE_FINISH_MS: u64 = 1000;
/// Width of the clear button area at the right edge.
const CLEAR_BUTTON_WIDTH: i32 = 18;

/// Widget that records a key sequence from key presses (`QKeySequenceEdit`).
pub struct KeySequenceEdit {
    pub base: WidgetBase,
    key_sequence: KeySequence,
    /// Chords recorded in the current session.
    recorded: Vec<KeyCombination>,
    /// First key pressed in the current recording session (`prevKey`); `None` when idle.
    prev_key: Option<u32>,
    maximum_sequence_length: usize,
    finishing_key_combinations: Vec<KeyCombination>,
    clear_button_enabled: bool,
    release_timer: TimerId,
    text: String,
    placeholder: String,
    font: Font,
    palette: Palette,

    /// Emitted whenever the sequence changes (`QKeySequenceEdit::keySequenceChanged`).
    pub key_sequence_changed: Signal<KeySequence>,
    /// Emitted when recording finishes (`QKeySequenceEdit::editingFinished`).
    pub editing_finished: Signal<()>,
}

/// Canonical Qt alias.
pub type QKeySequenceEdit = KeySequenceEdit;

impl Default for KeySequenceEdit {
    fn default() -> Self {
        Self::new()
    }
}

impl KeySequenceEdit {
    pub fn new() -> Self {
        let base = WidgetBase::new();
        base.set_focus_policy(FocusPolicy::StrongFocus);
        base.set_size_policy(QSizePolicy::new(Policy::Expanding, Policy::Fixed));
        base.set_geometry(Rect::new(0, 0, 160, 28));
        let key_sequence_changed = Signal::with_owner(&base.object_data);
        let editing_finished = Signal::with_owner(&base.object_data);
        Self {
            base,
            key_sequence: KeySequence::new(),
            recorded: Vec::new(),
            prev_key: None,
            maximum_sequence_length: KeySequence::MAX_KEY_COUNT,
            finishing_key_combinations: vec![
                KeyCombination::new(qt_key::TAB, 0),
                KeyCombination::new(qt_key::BACKTAB, 0),
            ],
            clear_button_enabled: false,
            release_timer: TimerId::INVALID,
            text: String::new(),
            placeholder: "Press shortcut".to_string(),
            font: Font::new("Segoe UI", 13.0),
            palette: Palette::light(),
            key_sequence_changed,
            editing_finished,
        }
    }

    /// Creates an editor pre-filled with `sequence`.
    pub fn with_key_sequence(sequence: KeySequence) -> Self {
        let mut edit = Self::new();
        edit.set_key_sequence(sequence);
        edit
    }

    pub fn key_sequence(&self) -> &KeySequence {
        &self.key_sequence
    }

    /// Replaces the sequence, truncating it to the maximum length (`QKeySequenceEdit::setKeySequence`).
    pub fn set_key_sequence(&mut self, sequence: KeySequence) {
        self.reset_state();
        let truncated = KeySequence::from_keys(
            &sequence.keys()[..sequence.count().min(self.maximum_sequence_length)],
        );
        if truncated == self.key_sequence {
            return;
        }
        self.key_sequence = truncated;
        self.recorded = self.key_sequence.keys().to_vec();
        self.text = self.key_sequence.to_portable_string();
        self.update();
        self.key_sequence_changed.emit(&self.key_sequence);
    }

    /// Clears the sequence (`QKeySequenceEdit::clear`).
    pub fn clear(&mut self) {
        self.set_key_sequence(KeySequence::new());
    }

    /// Number of chords recorded before editing finishes automatically (1..=4).
    pub fn maximum_sequence_length(&self) -> usize {
        self.maximum_sequence_length
    }

    pub fn set_maximum_sequence_length(&mut self, length: usize) {
        self.maximum_sequence_length = length.clamp(1, KeySequence::MAX_KEY_COUNT);
        if self.key_sequence.count() > self.maximum_sequence_length {
            let truncated =
                KeySequence::from_keys(&self.key_sequence.keys()[..self.maximum_sequence_length]);
            self.set_key_sequence(truncated);
        }
    }

    /// Combinations that finish editing instead of being recorded (`finishingKeyCombinations`).
    /// The default is Tab and Backtab, which then keep their usual meaning of moving the focus.
    pub fn finishing_key_combinations(&self) -> &[KeyCombination] {
        &self.finishing_key_combinations
    }

    pub fn set_finishing_key_combinations(&mut self, combinations: Vec<KeyCombination>) {
        self.finishing_key_combinations = combinations;
    }

    pub fn is_clear_button_enabled(&self) -> bool {
        self.clear_button_enabled
    }

    pub fn set_clear_button_enabled(&mut self, enabled: bool) {
        self.clear_button_enabled = enabled;
        self.update();
    }

    /// Whether a recording session is in progress.
    pub fn is_recording(&self) -> bool {
        self.prev_key.is_some()
    }

    /// Text shown in the editor (e.g. `"Ctrl+A, ..."` while recording).
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn placeholder_text(&self) -> &str {
        &self.placeholder
    }

    pub fn set_placeholder_text(&mut self, text: impl Into<String>) {
        self.placeholder = text.into();
        self.update();
    }

    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.update();
    }

    /// Ends the recording session and emits the result (`QKeySequenceEditPrivate::finishEditing`).
    pub fn finish_editing(&mut self) {
        self.reset_state();
        self.key_sequence_changed.emit(&self.key_sequence);
        self.editing_finished.emit(&());
    }

    fn reset_state(&mut self) {
        self.stop_release_timer();
        self.prev_key = None;
        self.text = self.key_sequence.to_portable_string();
        self.update();
    }

    fn stop_release_timer(&mut self) {
        if self.release_timer.is_valid() {
            let id = self.release_timer;
            self.release_timer = TimerId::INVALID;
            self.kill_timer(id);
        }
    }

    fn clear_button_rect(&self) -> Rect {
        let g = self.base.geometry();
        Rect::new(
            g.width - CLEAR_BUTTON_WIDTH - 2,
            0,
            CLEAR_BUTTON_WIDTH,
            g.height,
        )
    }

    /// Whether `combo` finishes editing: it is listed, or it is Shift+Tab (or Backtab with only
    /// Shift) and the default Backtab entry is listed. Qt reports Shift+Tab as Backtab, so the
    /// default entry would otherwise never match.
    fn is_finishing(&self, combo: KeyCombination) -> bool {
        let shift_backtab = (combo.key == qt_key::TAB || combo.key == qt_key::BACKTAB)
            && combo.modifiers == key_modifier::SHIFT;
        self.finishing_key_combinations.contains(&combo)
            || (shift_backtab
                && self
                    .finishing_key_combinations
                    .contains(&KeyCombination::new(qt_key::BACKTAB, 0)))
    }

    fn record_key_press(&mut self, key: u32, modifiers: u32) {
        let combo = KeyCombination::from_key_event(key, modifiers);
        if self.is_finishing(combo) {
            self.finish_editing();
            return;
        }
        if self.prev_key.is_none() {
            self.clear();
            self.recorded.clear();
            self.prev_key = Some(combo.key);
        }
        if matches!(
            combo.key,
            qt_key::CONTROL | qt_key::SHIFT | qt_key::META | qt_key::ALT | qt_key::UNKNOWN
        ) {
            return;
        }
        if self.recorded.len() >= self.maximum_sequence_length {
            return;
        }
        // Keypad state is not part of recorded shortcuts (`translateModifiers`).
        self.recorded.push(KeyCombination::new(
            combo.key,
            combo.modifiers & !key_modifier::KEYPAD,
        ));
        self.key_sequence = KeySequence::from_keys(&self.recorded);
        self.text = self.key_sequence.to_portable_string();
        if self.recorded.len() < self.maximum_sequence_length {
            self.text.push_str(", ...");
        }
        self.update();
    }

    fn record_key_release(&mut self, key: u32, modifiers: u32) {
        let (released, _) = normalize_key(key, modifiers);
        if self.prev_key != Some(released) {
            return;
        }
        if self.recorded.len() < self.maximum_sequence_length {
            self.stop_release_timer();
            self.release_timer = self.start_timer(RELEASE_FINISH_MS, TimerType::Coarse);
        } else {
            self.finish_editing();
        }
    }
}

impl Drop for KeySequenceEdit {
    fn drop(&mut self) {
        if self.release_timer.is_valid() {
            // kill_object_timer only touches the thread timer context via `try_with`.
            kill_object_timer(self.release_timer);
        }
    }
}

impl QObject for KeySequenceEdit {
    crate::widget::widget_object_properties!();

    fn object_data(&self) -> &ObjectData {
        &self.base.object_data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.base.object_data)
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn timer_event(&mut self, timer_id: u64) {
        if self.release_timer.is_valid() && timer_id == self.release_timer.0 as u64 {
            self.finish_editing();
        }
    }

    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::KeyPress {
                key,
                modifiers,
                is_repeat,
            } => {
                self.key_press_event(*key, *modifiers, *is_repeat);
                true
            }
            EventKind::KeyRelease { key, modifiers } => {
                self.key_release_event(*key, *modifiers);
                true
            }
            EventKind::MouseButtonPress { x, y, button } => {
                self.mouse_press_event(Point::new(*x, *y), *button, 0);
                true
            }
            EventKind::FocusIn { reason } => {
                self.focus_in_event(*reason);
                true
            }
            EventKind::FocusOut { reason } => {
                self.focus_out_event(*reason);
                true
            }
            EventKind::Timer { timer_id } => {
                self.timer_event(*timer_id);
                true
            }
            _ => false,
        }
    }
}

impl Widget for KeySequenceEdit {
    fn widget_base(&self) -> &crate::widget::WidgetBase {
        &self.base
    }

    fn id(&self) -> ObjectId {
        self.base.object_data.id()
    }

    fn geometry(&self) -> Rect {
        self.base.geometry()
    }

    fn set_geometry(&self, rect: Rect) {
        self.base.set_geometry(rect);
    }

    fn size_hint(&self) -> Size {
        let metrics = FontMetrics::from_font(&self.font);
        let sample = "Ctrl+Shift+Alt+F12, ...";
        let w = metrics.horizontal_advance(sample, &self.font).ceil() as i32 + 16;
        Size::new(w, (metrics.height.ceil() as i32 + 12).max(28))
    }

    fn is_visible(&self) -> bool {
        self.base.is_visible()
    }

    fn set_visible(&self, visible: bool) {
        self.base.set_visible(visible);
    }

    fn is_enabled(&self) -> bool {
        self.base.is_enabled()
    }

    fn update(&self) {
        self.base.update();
    }

    fn dirty_rect(&self) -> Option<Rect> {
        self.base.dirty_rect()
    }

    fn clear_dirty(&self) {
        self.base.clear_dirty();
    }

    fn layout(&self) -> Option<&dyn Layout> {
        None
    }

    fn layout_mut(&mut self) -> Option<&mut Box<dyn Layout>> {
        None
    }

    fn set_layout(&mut self, _layout: Box<dyn Layout>) {}

    fn parent_widget(&self) -> Option<WidgetWeak> {
        self.base.parent_widget()
    }

    fn set_parent_widget(&self, parent: Option<WidgetWeak>) {
        self.base.set_parent_widget(parent);
    }

    fn window_id(&self) -> Option<ObjectId> {
        self.base.window_id()
    }

    fn set_window_id(&self, window_id: Option<ObjectId>) {
        self.base.set_window_id(window_id);
    }
    fn children(&self) -> Vec<WidgetRef> {
        Vec::new()
    }

    fn add_child(&mut self, _child: WidgetRef) {}

    fn remove_child(&mut self, _child_id: ObjectId) {}

    fn focus_policy(&self) -> FocusPolicy {
        self.base.focus_policy()
    }

    fn set_focus_policy(&self, policy: FocusPolicy) {
        self.base.set_focus_policy(policy);
    }

    fn has_focus(&self) -> bool {
        self.base.has_focus()
    }

    fn set_has_focus(&self, focus: bool) {
        self.base.set_has_focus(focus);
        self.update();
    }

    fn focus_in_event(&mut self, _reason: FocusReason) {
        self.update();
    }

    /// Qt finishes editing on every focus loss except to a popup.
    fn focus_out_event(&mut self, reason: FocusReason) {
        if reason != FocusReason::Popup {
            self.finish_editing();
        }
    }

    fn mouse_press_event(&mut self, pos: Point, button: u32, _modifiers: u32) {
        if button == 1
            && self.clear_button_enabled
            && !self.key_sequence.is_empty()
            && self.clear_button_rect().contains(pos)
        {
            self.clear();
        }
    }

    /// `QKeySequenceEdit::event`: Tab and Backtab are recorded unless they are finishing keys,
    /// in which case they move the focus.
    fn wants_tab_key(&self, key: u32, modifiers: u32) -> bool {
        let combo = KeyCombination::from_key_event(key, modifiers);
        !self.is_finishing(combo)
    }

    fn key_press_event(&mut self, key: u32, modifiers: u32, _is_repeat: bool) {
        if self.base.is_enabled() {
            self.record_key_press(key, modifiers);
        }
    }

    fn key_release_event(&mut self, key: u32, modifiers: u32) {
        if self.base.is_enabled() {
            self.record_key_release(key, modifiers);
        }
    }

    fn paint_event(&mut self, painter: &mut Painter) {
        let g = self.base.geometry();
        let group = if self.base.is_enabled() {
            ColorGroup::Active
        } else {
            ColorGroup::Disabled
        };
        let rect = RectF::new(0.5, 0.5, g.width as f32 - 1.0, g.height as f32 - 1.0);
        painter.set_brush(Brush::Color(self.palette.color(group, ColorRole::Base)));
        let border = if self.base.has_focus() {
            self.palette.color(group, ColorRole::Highlight)
        } else {
            self.palette.color(group, ColorRole::Mid)
        };
        // Frame and clear cross are anti-aliased (`qfusionstyle.cpp:538-552`); the cross has no Qt vector counterpart (doubt).
        painter.set_render_hint(RenderHint::Antialiasing, true);
        painter.set_pen(Pen::new(
            border,
            if self.base.has_focus() { 1.5 } else { 1.0 },
        ));
        painter.draw_rounded_rect(rect, 3.0, 3.0);

        let metrics = FontMetrics::from_font(&self.font);
        let baseline = (g.height as f32 - metrics.height) / 2.0 + metrics.ascent;
        let (text, role) = if self.text.is_empty() {
            (self.placeholder.as_str(), ColorRole::PlaceholderText)
        } else {
            (self.text.as_str(), ColorRole::Text)
        };
        if !text.is_empty() {
            painter.set_pen(Pen::new(self.palette.color(group, role), 1.0));
            painter.draw_text(PointF::new(6.0, baseline), text, &self.font);
        }

        if self.clear_button_enabled && !self.key_sequence.is_empty() {
            let r = self.clear_button_rect();
            let cx = r.x as f32 + r.width as f32 / 2.0;
            let cy = r.y as f32 + r.height as f32 / 2.0;
            painter.set_pen(
                Pen::new(self.palette.color(group, ColorRole::Dark), 1.5)
                    .with_cap(ferrokit_gui::tiny_skia::LineCap::Round)
                    .with_join(ferrokit_gui::tiny_skia::LineJoin::Round),
            );
            painter.draw_line(
                PointF::new(cx - 4.0, cy - 4.0),
                PointF::new(cx + 4.0, cy + 4.0),
            );
            painter.draw_line(
                PointF::new(cx + 4.0, cy - 4.0),
                PointF::new(cx - 4.0, cy + 4.0),
            );
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
