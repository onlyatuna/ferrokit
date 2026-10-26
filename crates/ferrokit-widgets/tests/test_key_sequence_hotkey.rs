// SPDX-License-Identifier: LGPL-3.0-only
//! `QKeySequence` text and the modifier set + Win32 virtual key a global hotkey is registered
//! with: the combinations the HUD offers ("Alt+C", "Ctrl+Shift+H", "F10", "Ctrl+Alt+Shift+F23")
//! and the round trip for every key `to_hotkey` accepts.

use ferrokit_platform::hotkey::HotkeyModifiers;
use ferrokit_widgets::key_sequence_edit::{key_modifier, qt_key, KeyCombination, KeySequence};

const VK_SPACE: u32 = 0x20;
const VK_F10: u32 = 0x79;
const VK_F24: u32 = 0x87;

fn hotkey(text: &str) -> Option<(HotkeyModifiers, u32)> {
    KeySequence::from_portable_string(text).to_hotkey()
}

#[test]
fn the_hud_combinations_map_to_modifiers_and_virtual_keys() {
    assert_eq!(hotkey("Alt+C"), Some((HotkeyModifiers::ALT, b'C' as u32)));
    assert_eq!(
        hotkey("Ctrl+Shift+H"),
        Some((
            HotkeyModifiers::CONTROL | HotkeyModifiers::SHIFT,
            b'H' as u32
        ))
    );
    assert_eq!(hotkey("F10"), Some((HotkeyModifiers::empty(), VK_F10)));
    assert_eq!(
        hotkey("Ctrl+Alt+Shift+F24"),
        Some((
            HotkeyModifiers::CONTROL | HotkeyModifiers::ALT | HotkeyModifiers::SHIFT,
            VK_F24
        ))
    );
    assert_eq!(hotkey("Meta+Space"), Some((HotkeyModifiers::WIN, VK_SPACE)));
    // Lower-case names parse the way Qt parses them.
    assert_eq!(hotkey("alt+c"), Some((HotkeyModifiers::ALT, b'C' as u32)));
}

#[test]
fn the_text_of_a_hotkey_is_the_qt_portable_text() {
    let format = |mods, vk| {
        KeySequence::from_hotkey(mods, vk)
            .unwrap()
            .to_portable_string()
    };
    assert_eq!(format(HotkeyModifiers::ALT, b'C' as u32), "Alt+C");
    assert_eq!(
        format(HotkeyModifiers::ALT | HotkeyModifiers::SHIFT, b'C' as u32),
        "Alt+Shift+C"
    );
    // Qt writes Meta, Ctrl, Alt, Shift in that order.
    assert_eq!(
        format(
            HotkeyModifiers::CONTROL | HotkeyModifiers::SHIFT | HotkeyModifiers::ALT,
            0x70 + 11
        ),
        "Ctrl+Alt+Shift+F12"
    );
    assert_eq!(format(HotkeyModifiers::empty(), VK_SPACE), "Space");
    assert_eq!(
        format(HotkeyModifiers::WIN | HotkeyModifiers::NO_REPEAT, 0x1B),
        "Meta+Esc",
        "NO_REPEAT is a delivery flag, not a modifier"
    );
}

#[test]
fn text_that_cannot_be_a_hotkey_gives_none() {
    assert_eq!(hotkey(""), None);
    assert_eq!(hotkey("InvalidKeyString"), None);
    // Two chords: a hotkey is one press.
    assert_eq!(hotkey("Ctrl+X, Ctrl+C"), None);
    // A modifier on its own.
    assert_eq!(hotkey("Shift"), None);
    assert_eq!(hotkey("Ctrl+Alt"), None);
    // Past F24 there is no virtual key.
    assert_eq!(hotkey("F25"), None);
    assert_eq!(KeySequence::new().to_hotkey(), None);
    assert_eq!(KeyCombination::new(qt_key::UNKNOWN, 0).to_hotkey(), None);
    assert_eq!(KeyCombination::default().to_hotkey(), None);
}

#[test]
fn the_keypad_modifier_selects_the_keypad_virtual_key() {
    assert_eq!(
        KeyCombination::new('5' as u32, key_modifier::KEYPAD | key_modifier::CTRL).to_hotkey(),
        Some((HotkeyModifiers::CONTROL, 0x65))
    );
    assert_eq!(hotkey("Num+5"), Some((HotkeyModifiers::empty(), 0x65)));
    assert_eq!(
        KeyCombination::new('5' as u32, 0).to_hotkey(),
        Some((HotkeyModifiers::empty(), b'5' as u32))
    );
    let back = KeyCombination::from_hotkey(HotkeyModifiers::CONTROL, 0x65).unwrap();
    assert_eq!(back.key, '5' as u32);
    assert_eq!(back.modifiers, key_modifier::KEYPAD | key_modifier::CTRL);
}

#[test]
fn every_virtual_key_that_converts_converts_back() {
    let mut converted = 0;
    for vk in 0..=0xFFu32 {
        let Some(back) = KeyCombination::from_hotkey(HotkeyModifiers::empty(), vk) else {
            continue;
        };
        converted += 1;
        let (mods, again) = back
            .to_hotkey()
            .expect("a combination from a hotkey converts");
        assert_eq!((mods, again), (HotkeyModifiers::empty(), vk), "vk {vk:#x}");
    }
    // Letters, digits, F1..F24, navigation, OEM punctuation, keypad ...: plenty, and no more than
    // there are virtual keys.
    assert!(converted > 100, "{converted}");

    for vk in [
        b'A' as u32,
        b'Z' as u32,
        b'0' as u32,
        b'9' as u32,
        VK_SPACE,
        0x08,
        0x09,
        0x0D,
        0x1B,
        0x24,
        0x2C,
        0x2D,
        0x2E,
        0x70,
        VK_F24,
        0xBA,
        0xDE,
        0x6A,
    ] {
        assert!(
            KeyCombination::from_hotkey(HotkeyModifiers::empty(), vk).is_some(),
            "vk {vk:#x}"
        );
    }
}

#[test]
fn virtual_keys_without_a_qt_key_give_none() {
    // Modifier keys, a gap in the table, and codes beyond the keyboard.
    for vk in [0x10u32, 0x11, 0x12, 0xA0, 0xA5, 0x5B, 0x88, 0xFF, 0x100, 0] {
        assert_eq!(
            KeyCombination::from_hotkey(HotkeyModifiers::ALT, vk),
            None,
            "vk {vk:#x}"
        );
    }
}

#[test]
fn every_modifier_combination_round_trips() {
    let all = HotkeyModifiers::ALT
        | HotkeyModifiers::CONTROL
        | HotkeyModifiers::SHIFT
        | HotkeyModifiers::WIN;
    for bits in 0..=all.bits() {
        let Some(mods) = HotkeyModifiers::from_bits(bits).filter(|m| all.contains(*m)) else {
            continue;
        };
        let seq = KeySequence::from_hotkey(mods, b'C' as u32).unwrap();
        assert_eq!(seq.to_hotkey(), Some((mods, b'C' as u32)));
        // Through the text as well.
        assert_eq!(
            KeySequence::from_portable_string(&seq.to_portable_string()),
            seq
        );
    }
}
