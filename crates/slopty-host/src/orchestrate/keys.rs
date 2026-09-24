//! Key names as an orchestrating agent writes them (`ctrl+c`, `enter`, `shift+tab`).
//!
//! Each is turned into the key event a client would send, so the engine's encoder writes whatever
//! the program's modes call for (legacy bytes, kitty keyboard sequences, application cursor keys).

use slopty_proto::input::{KeyAction, KeyCode, KeyEvent, Mods};

/// What a bad name is told: every form that parses.
pub const FORMS: &str = "a key is `[mods+]key`: mods `ctrl`, `alt` (or `opt`), `shift`, `cmd`; \
     key a single character, a name (`enter`, `tab`, `space`, `escape`, `backspace`, `delete`, \
     `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `insert`, `f1`…`f25`) \
     or a W3C `KeyboardEvent.code` (`ArrowUp`, `Digit1`, `NumpadEnter`)";

macro_rules! code_names {
    ($($name:ident),* $(,)?) => {
        /// Every [`KeyCode`] under its W3C name.
        const CODES: &[(&str, KeyCode)] = &[$((stringify!($name), KeyCode::$name)),*];
    };
}
slopty_proto::for_each_key_code!(code_names);

/// Short names people type for keys whose W3C name is long.
const ALIASES: &[(&str, KeyCode)] = &[
    ("enter", KeyCode::Enter),
    ("return", KeyCode::Enter),
    ("ret", KeyCode::Enter),
    ("esc", KeyCode::Escape),
    ("bs", KeyCode::Backspace),
    ("del", KeyCode::Delete),
    ("ins", KeyCode::Insert),
    ("up", KeyCode::ArrowUp),
    ("down", KeyCode::ArrowDown),
    ("left", KeyCode::ArrowLeft),
    ("right", KeyCode::ArrowRight),
    ("pgup", KeyCode::PageUp),
    ("pgdn", KeyCode::PageDown),
];

/// Parse one `[mods+]key` into a key press. `seq` is the sequence number it carries.
///
/// # Errors
///
/// The name, when it is not one; the message lists the forms that are.
pub fn parse(name: &str, seq: u64) -> Result<KeyEvent, String> {
    let bad = || format!("unknown key {name:?}: {FORMS}");
    let trimmed = name.trim();
    // `ctrl++` and `+` name the plus key itself.
    let (mod_part, key) = match trimmed.strip_suffix('+') {
        Some(head) if head.is_empty() || head.ends_with('+') => {
            (head.strip_suffix('+').unwrap_or(head), "+")
        }
        _ => trimmed.rsplit_once('+').unwrap_or(("", trimmed)),
    };
    let mut mods = Mods::empty();
    for m in mod_part.split('+').filter(|m| !m.is_empty()) {
        mods |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Mods::CTRL,
            "alt" | "opt" | "option" | "meta" => Mods::ALT,
            "shift" => Mods::SHIFT,
            "cmd" | "super" | "command" => Mods::SUPER,
            _ => return Err(bad()),
        };
    }
    if key.is_empty() {
        return Err(bad());
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Ok(character(c, mods, seq));
    }
    let code = ALIASES
        .iter()
        .chain(CODES)
        .find(|(n, _)| n.eq_ignore_ascii_case(key))
        .map(|&(_, code)| code)
        .filter(|&code| code != KeyCode::Unidentified)
        .ok_or_else(bad)?;
    let text = match code {
        KeyCode::Space if typing(mods) => Some(" ".to_owned()),
        _ => None,
    };
    Ok(press(seq, code, mods, text, None))
}

/// Whether a key with `mods` types its character: Control, Alt and Command never do.
fn typing(mods: Mods) -> bool {
    !mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER)
}

/// A single character: its physical key when it has one, its text when the modifiers let it
/// type (Shift makes a letter capital).
fn character(c: char, mods: Mods, seq: u64) -> KeyEvent {
    let lower = c.to_ascii_lowercase();
    let code = physical(lower);
    let shifted = mods.contains(Mods::SHIFT) || c.is_ascii_uppercase();
    let mods = if c.is_ascii_uppercase() { mods | Mods::SHIFT } else { mods };
    let text = typing(mods).then(|| {
        if shifted && lower.is_ascii_alphabetic() {
            lower.to_ascii_uppercase().to_string()
        } else {
            c.to_string()
        }
    });
    press(seq, code, mods, text, Some(lower))
}

fn press(
    seq: u64,
    code: KeyCode,
    mods: Mods,
    text: Option<String>,
    unshifted: Option<char>,
) -> KeyEvent {
    let consumed_mods = if text.is_some() { mods & Mods::SHIFT } else { Mods::empty() };
    KeyEvent {
        seq,
        action: KeyAction::Press,
        code,
        mods,
        consumed_mods,
        text,
        unshifted,
        composing: false,
        option_as_alt: mods.contains(Mods::ALT),
    }
}

/// The US-layout key a character is on; `Unidentified` for the rest, which then type by text.
const fn physical(c: char) -> KeyCode {
    match c {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(name: &str) -> (KeyCode, Mods, Option<String>) {
        let key = parse(name, 1).unwrap();
        (key.code, key.mods, key.text)
    }

    #[test]
    fn names_modifiers_and_characters() {
        assert_eq!(parsed("ctrl+c"), (KeyCode::C, Mods::CTRL, None));
        assert_eq!(parsed("Enter"), (KeyCode::Enter, Mods::empty(), None));
        assert_eq!(parsed("return"), (KeyCode::Enter, Mods::empty(), None));
        assert_eq!(parsed("shift+tab"), (KeyCode::Tab, Mods::SHIFT, None));
        assert_eq!(parsed("up"), (KeyCode::ArrowUp, Mods::empty(), None));
        assert_eq!(parsed("ArrowUp"), (KeyCode::ArrowUp, Mods::empty(), None));
        assert_eq!(parsed("ctrl+alt+Delete"), (KeyCode::Delete, Mods::CTRL | Mods::ALT, None));
        assert_eq!(parsed("x"), (KeyCode::X, Mods::empty(), Some("x".to_owned())));
        assert_eq!(parsed("X"), (KeyCode::X, Mods::SHIFT, Some("X".to_owned())));
        assert_eq!(parsed("shift+x"), (KeyCode::X, Mods::SHIFT, Some("X".to_owned())));
        assert_eq!(parsed("space"), (KeyCode::Space, Mods::empty(), Some(" ".to_owned())));
        assert_eq!(parsed("f12"), (KeyCode::F12, Mods::empty(), None));
        assert_eq!(parsed("ctrl++"), (KeyCode::Unidentified, Mods::CTRL, None));
        assert_eq!(parsed("+"), (KeyCode::Unidentified, Mods::empty(), Some("+".to_owned())));
        assert_eq!(parsed("é"), (KeyCode::Unidentified, Mods::empty(), Some("é".to_owned())));
    }

    #[test]
    fn a_bad_name_lists_the_forms() {
        for bad in ["hyper+c", "ctrl+", "", "enterr", "Unidentified", "ctrl+nope"] {
            let err = parse(bad, 1).unwrap_err();
            assert!(err.contains("[mods+]key"), "{bad:?}: {err}");
        }
    }
}
