//! W3C key codes → macOS virtual key codes (`kVK_*`, ANSI layout positions).
//!
//! Virtual key codes name *positions*, so a `KeyCode::A` press lands as whatever the host's
//! keyboard layout puts there; the client also sends the text it produced, which the host
//! attaches with `CGEventKeyboardSetUnicodeString` so layouts do not have to agree.

use objc2_core_graphics::CGKeyCode;
use slopty_proto::input::KeyCode;

/// Virtual key code for `code`, or `None` when macOS has no keyboard event for it (media and
/// browser keys are system-defined events, not key presses).
#[must_use]
pub const fn virtual_key(code: KeyCode) -> Option<CGKeyCode> {
    use KeyCode as K;
    let vk: u16 = match code {
        K::A => 0x00,
        K::S => 0x01,
        K::D => 0x02,
        K::F => 0x03,
        K::H => 0x04,
        K::G => 0x05,
        K::Z => 0x06,
        K::X => 0x07,
        K::C => 0x08,
        K::V => 0x09,
        K::IntlBackslash => 0x0A,
        K::B => 0x0B,
        K::Q => 0x0C,
        K::W => 0x0D,
        K::E => 0x0E,
        K::R => 0x0F,
        K::Y => 0x10,
        K::T => 0x11,
        K::Digit1 => 0x12,
        K::Digit2 => 0x13,
        K::Digit3 => 0x14,
        K::Digit4 => 0x15,
        K::Digit6 => 0x16,
        K::Digit5 => 0x17,
        K::Equal => 0x18,
        K::Digit9 => 0x19,
        K::Digit7 => 0x1A,
        K::Minus => 0x1B,
        K::Digit8 => 0x1C,
        K::Digit0 => 0x1D,
        K::BracketRight => 0x1E,
        K::O => 0x1F,
        K::U => 0x20,
        K::BracketLeft => 0x21,
        K::I => 0x22,
        K::P => 0x23,
        K::Enter => 0x24,
        K::L => 0x25,
        K::J => 0x26,
        K::Quote => 0x27,
        K::K => 0x28,
        K::Semicolon => 0x29,
        K::Backslash => 0x2A,
        K::Comma => 0x2B,
        K::Slash => 0x2C,
        K::N => 0x2D,
        K::M => 0x2E,
        K::Period => 0x2F,
        K::Tab => 0x30,
        K::Space => 0x31,
        K::Backquote => 0x32,
        K::Backspace => 0x33,
        K::Escape => 0x35,
        K::MetaRight => 0x36,
        K::MetaLeft => 0x37,
        K::ShiftLeft => 0x38,
        K::CapsLock => 0x39,
        K::AltLeft => 0x3A,
        K::ControlLeft => 0x3B,
        K::ShiftRight => 0x3C,
        K::AltRight => 0x3D,
        K::ControlRight => 0x3E,
        K::Fn => 0x3F,
        K::F17 => 0x40,
        K::NumpadDecimal => 0x41,
        K::NumpadMultiply => 0x43,
        K::NumpadAdd => 0x45,
        K::NumpadClear | K::NumLock => 0x47,
        K::AudioVolumeUp => 0x48,
        K::AudioVolumeDown => 0x49,
        K::AudioVolumeMute => 0x4A,
        K::NumpadDivide => 0x4B,
        K::NumpadEnter => 0x4C,
        K::NumpadSubtract => 0x4E,
        K::F18 => 0x4F,
        K::F19 => 0x50,
        K::NumpadEqual => 0x51,
        K::Numpad0 => 0x52,
        K::Numpad1 => 0x53,
        K::Numpad2 => 0x54,
        K::Numpad3 => 0x55,
        K::Numpad4 => 0x56,
        K::Numpad5 => 0x57,
        K::Numpad6 => 0x58,
        K::Numpad7 => 0x59,
        K::F20 => 0x5A,
        K::Numpad8 => 0x5B,
        K::Numpad9 => 0x5C,
        K::IntlYen => 0x5D,
        K::IntlRo => 0x5E,
        K::NumpadComma => 0x5F,
        K::F5 => 0x60,
        K::F6 => 0x61,
        K::F7 => 0x62,
        K::F3 => 0x63,
        K::F8 => 0x64,
        K::F9 => 0x65,
        K::Convert => 0x66,
        K::F11 => 0x67,
        K::KanaMode => 0x68,
        K::F13 | K::PrintScreen => 0x69,
        K::F16 => 0x6A,
        K::F14 | K::ScrollLock => 0x6B,
        K::F10 => 0x6D,
        K::ContextMenu => 0x6E,
        K::F12 => 0x6F,
        K::F15 | K::Pause => 0x71,
        K::Help | K::Insert => 0x72,
        K::Home => 0x73,
        K::PageUp => 0x74,
        K::Delete => 0x75,
        K::F4 => 0x76,
        K::End => 0x77,
        K::F2 => 0x78,
        K::PageDown => 0x79,
        K::F1 => 0x7A,
        K::ArrowLeft => 0x7B,
        K::ArrowRight => 0x7C,
        K::ArrowDown => 0x7D,
        K::ArrowUp => 0x7E,
        _ => return None,
    };
    Some(vk)
}

/// Keys that only change modifier state; they post as `FlagsChanged`, not `KeyDown`.
#[must_use]
pub const fn is_modifier(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::MetaLeft
            | KeyCode::MetaRight
            | KeyCode::CapsLock
            | KeyCode::Fn
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything macOS cannot express as a keyboard event, spelled out so a new `KeyCode`
    /// variant fails this test until someone decides where it goes.
    const UNMAPPED: &[KeyCode] = &[
        KeyCode::Unidentified,
        KeyCode::NonConvert,
        KeyCode::NumpadBackspace,
        KeyCode::NumpadClearEntry,
        KeyCode::NumpadMemoryAdd,
        KeyCode::NumpadMemoryClear,
        KeyCode::NumpadMemoryRecall,
        KeyCode::NumpadMemoryStore,
        KeyCode::NumpadMemorySubtract,
        KeyCode::NumpadParenLeft,
        KeyCode::NumpadParenRight,
        KeyCode::NumpadSeparator,
        KeyCode::NumpadUp,
        KeyCode::NumpadDown,
        KeyCode::NumpadRight,
        KeyCode::NumpadLeft,
        KeyCode::NumpadBegin,
        KeyCode::NumpadHome,
        KeyCode::NumpadEnd,
        KeyCode::NumpadInsert,
        KeyCode::NumpadDelete,
        KeyCode::NumpadPageUp,
        KeyCode::NumpadPageDown,
        KeyCode::F21,
        KeyCode::F22,
        KeyCode::F23,
        KeyCode::F24,
        KeyCode::F25,
        KeyCode::FnLock,
        KeyCode::BrowserBack,
        KeyCode::BrowserFavorites,
        KeyCode::BrowserForward,
        KeyCode::BrowserHome,
        KeyCode::BrowserRefresh,
        KeyCode::BrowserSearch,
        KeyCode::BrowserStop,
        KeyCode::Eject,
        KeyCode::LaunchApp1,
        KeyCode::LaunchApp2,
        KeyCode::LaunchMail,
        KeyCode::MediaPlayPause,
        KeyCode::MediaSelect,
        KeyCode::MediaStop,
        KeyCode::MediaTrackNext,
        KeyCode::MediaTrackPrevious,
        KeyCode::Power,
        KeyCode::Sleep,
        KeyCode::WakeUp,
        KeyCode::Copy,
        KeyCode::Cut,
        KeyCode::Paste,
    ];

    #[test]
    fn every_key_is_mapped_or_listed() {
        for &code in KeyCode::ALL {
            let mapped = virtual_key(code).is_some();
            let listed = UNMAPPED.contains(&code);
            assert!(mapped != listed, "{code:?}: mapped={mapped} listed={listed}");
        }
    }

    #[test]
    fn distinct_keys_get_distinct_codes() {
        // Only the documented aliases may share a position.
        let aliases = [
            (KeyCode::NumpadClear, KeyCode::NumLock),
            (KeyCode::F13, KeyCode::PrintScreen),
            (KeyCode::F14, KeyCode::ScrollLock),
            (KeyCode::F15, KeyCode::Pause),
            (KeyCode::Help, KeyCode::Insert),
        ];
        let mut seen = std::collections::HashMap::new();
        for &code in KeyCode::ALL {
            let Some(vk) = virtual_key(code) else { continue };
            let Some(prev) = seen.insert(vk, code) else { continue };
            let allowed = aliases.contains(&(prev, code)) || aliases.contains(&(code, prev));
            assert!(allowed, "{prev:?} and {code:?} share {vk:#x}");
        }
    }

    #[test]
    fn modifiers_have_codes() {
        for &code in KeyCode::ALL {
            if is_modifier(code) {
                assert!(virtual_key(code).is_some(), "{code:?}");
            }
        }
    }
}
