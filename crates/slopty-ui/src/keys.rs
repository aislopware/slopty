//! GPUI keystrokes → protocol key events. The host's engine does the actual encoding (legacy,
//! kitty, application modes), so all we do here is name the physical key and pass the text the
//! layout produced.

use gpui::{Keystroke, Modifiers};
use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};

/// Map GPUI modifiers.
#[must_use]
pub fn mods(m: Modifiers) -> Mods {
    let mut out = Mods::empty();
    out.set(Mods::SHIFT, m.shift);
    out.set(Mods::ALT, m.alt);
    out.set(Mods::CTRL, m.control);
    out.set(Mods::SUPER, m.platform);
    out
}

/// Map GPUI's key name (lowercase, unshifted) to a physical key.
#[must_use]
pub fn key_code(key: &str) -> KeyCode {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return single_char(c);
    }
    match key {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Space,
        "backspace" => KeyCode::Backspace,
        "escape" => KeyCode::Escape,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "left" => KeyCode::ArrowLeft,
        "right" => KeyCode::ArrowRight,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "capslock" => KeyCode::CapsLock,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        "f13" => KeyCode::F13,
        "f14" => KeyCode::F14,
        "f15" => KeyCode::F15,
        "f16" => KeyCode::F16,
        "f17" => KeyCode::F17,
        "f18" => KeyCode::F18,
        "f19" => KeyCode::F19,
        "f20" => KeyCode::F20,
        _ => KeyCode::Unidentified,
    }
}

const fn single_char(c: char) -> KeyCode {
    match c.to_ascii_lowercase() {
        'a' => KeyCode::A,
        'b' => KeyCode::B,
        'c' => KeyCode::C,
        'd' => KeyCode::D,
        'e' => KeyCode::E,
        'f' => KeyCode::F,
        'g' => KeyCode::G,
        'h' => KeyCode::H,
        'i' => KeyCode::I,
        'j' => KeyCode::J,
        'k' => KeyCode::K,
        'l' => KeyCode::L,
        'm' => KeyCode::M,
        'n' => KeyCode::N,
        'o' => KeyCode::O,
        'p' => KeyCode::P,
        'q' => KeyCode::Q,
        'r' => KeyCode::R,
        's' => KeyCode::S,
        't' => KeyCode::T,
        'u' => KeyCode::U,
        'v' => KeyCode::V,
        'w' => KeyCode::W,
        'x' => KeyCode::X,
        'y' => KeyCode::Y,
        'z' => KeyCode::Z,
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        '9' => KeyCode::Digit9,
        '-' => KeyCode::Minus,
        '=' => KeyCode::Equal,
        '[' => KeyCode::BracketLeft,
        ']' => KeyCode::BracketRight,
        '\\' => KeyCode::Backslash,
        ';' => KeyCode::Semicolon,
        '\'' => KeyCode::Quote,
        ',' => KeyCode::Comma,
        '.' => KeyCode::Period,
        '/' => KeyCode::Slash,
        '`' => KeyCode::Backquote,
        ' ' => KeyCode::Space,
        _ => KeyCode::Unidentified,
    }
}

/// Build the protocol event for a key press.
///
/// `alt_is_alt` says the ⌥ held is a modifier (the client's `option_as_alt` setting, resolved
/// for the side): the text is then the key without ⌥ (ghostty's own view retranslates the
/// key that way), so the host's encoder prefixes an escape instead of typing the symbol.
#[must_use]
pub fn key_event(seq: u64, keystroke: &Keystroke, repeat: bool, alt_is_alt: bool) -> KeyEvent {
    let all = mods(keystroke.modifiers);
    let unshifted = {
        let mut chars = keystroke.key.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    };
    let option_as_alt = alt_is_alt && all.contains(Mods::ALT);
    // Text the layout produced with Shift/Option already applied. Ctrl and Cmd never produce
    // text on macOS, so anything present had Shift (and maybe Alt) consumed.
    let text = if option_as_alt {
        // Without ⌥: the key itself, shifted when it is a letter; anything else leaves the
        // encoder its unshifted codepoint.
        unshifted.filter(char::is_ascii_alphabetic).map(|c| {
            if keystroke.modifiers.shift {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }
        })
    } else {
        keystroke.key_char.clone().filter(|t| !t.is_empty())
    };
    let consumed = match (&text, option_as_alt) {
        (Some(_), true) => all & Mods::SHIFT,
        (Some(_), false) => all & (Mods::SHIFT | Mods::ALT),
        (None, _) => Mods::empty(),
    };
    KeyEvent {
        seq,
        action: if repeat { KeyAction::Repeat } else { KeyAction::Press },
        code: key_code(&keystroke.key),
        mods: all,
        consumed_mods: consumed,
        text,
        unshifted,
        composing: false,
        option_as_alt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With ⌥ as Alt the event carries the key without ⌥ and the flag, so the host prefixes
    /// an escape; otherwise the layout's symbol goes as typed, ⌥ consumed.
    #[test]
    fn option_as_alt_rides_on_the_event() {
        let alt_b = Keystroke {
            modifiers: Modifiers { alt: true, ..Modifiers::default() },
            key: "b".to_owned(),
            key_char: Some("∫".to_owned()),
        };
        let typed = key_event(1, &alt_b, false, false);
        assert_eq!((typed.mods, typed.consumed_mods), (Mods::ALT, Mods::ALT));
        assert_eq!((typed.text.as_deref(), typed.option_as_alt), (Some("∫"), false));
        let alt = key_event(2, &alt_b, false, true);
        assert_eq!((alt.mods, alt.consumed_mods), (Mods::ALT, Mods::empty()));
        assert_eq!(
            (alt.text.as_deref(), alt.unshifted, alt.option_as_alt),
            (Some("b"), Some('b'), true)
        );
        let shifted = Keystroke {
            modifiers: Modifiers { alt: true, shift: true, ..Modifiers::default() },
            key_char: Some("ı".to_owned()),
            ..alt_b.clone()
        };
        let alt = key_event(3, &shifted, false, true);
        assert_eq!((alt.text.as_deref(), alt.consumed_mods), (Some("B"), Mods::SHIFT));
        // A symbol key has no text without ⌥; the host prefixes its unshifted codepoint.
        let alt_1 = Keystroke { key: "1".to_owned(), key_char: Some("¡".to_owned()), ..alt_b };
        let alt = key_event(4, &alt_1, false, true);
        assert_eq!((alt.text, alt.unshifted, alt.consumed_mods), (None, Some('1'), Mods::empty()));
        // The flag means nothing without ⌥ down.
        let plain =
            Keystroke { modifiers: Modifiers::default(), key_char: Some("b".to_owned()), ..alt_b };
        assert!(!key_event(5, &plain, false, true).option_as_alt);
    }

    #[test]
    fn names_map_to_codes() {
        assert_eq!(key_code("a"), KeyCode::A);
        assert_eq!(key_code("7"), KeyCode::Digit7);
        assert_eq!(key_code("enter"), KeyCode::Enter);
        assert_eq!(key_code("pageup"), KeyCode::PageUp);
        assert_eq!(key_code("f12"), KeyCode::F12);
        assert_eq!(key_code("/"), KeyCode::Slash);
        assert_eq!(key_code("wat"), KeyCode::Unidentified);
    }
}
