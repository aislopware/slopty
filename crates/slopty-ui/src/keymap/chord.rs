//! A chord in the palette's key syntax, as `settings.toml`'s `[keys]` spells one: modifiers
//! (`cmd`, `ctrl`, `alt`, `shift`, and the Mac's names for them) and one key, joined by `-`
//! (`` ctrl-` ``, `cmd-alt-space`, `f12`, `ctrl--`, `escape`). Case and surrounding space do
//! not matter.

/// Why a chord from the settings was not taken.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ChordError {
    /// A word that is neither a modifier nor a key the chord can name.
    UnknownKey(String),
}

impl std::fmt::Display for ChordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey(word) => write!(f, "no key is called \"{word}\""),
        }
    }
}

impl std::error::Error for ChordError {}

/// A key and its modifiers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Chord {
    /// The key as the palette's syntax names it (`` ` ``, `space`, `f12`).
    key: &'static str,
    cmd: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

impl Chord {
    /// Read a chord in the palette's key syntax. Any modifiers go, none included.
    ///
    /// # Errors
    ///
    /// A word that names no key.
    pub(super) fn read(text: &str) -> Result<Self, ChordError> {
        let text = text.trim().to_ascii_lowercase();
        let (mods, key) = match text.strip_suffix("--") {
            Some(mods) => (mods, "-"),
            None => text.rsplit_once('-').unwrap_or(("", text.as_str())),
        };
        let key = match key {
            "return" => "enter",
            "esc" => "escape",
            other => other,
        };
        let key = KEYS
            .iter()
            .find(|name| **name == key)
            .copied()
            .ok_or_else(|| ChordError::UnknownKey(key.to_owned()))?;
        let mut chord = Self { key, cmd: false, ctrl: false, alt: false, shift: false };
        for word in mods.split('-').filter(|w| !w.is_empty()) {
            match word {
                "cmd" | "command" | "super" => chord.cmd = true,
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "shift" => chord.shift = true,
                other => return Err(ChordError::UnknownKey(other.to_owned())),
            }
        }
        Ok(chord)
    }

    /// The chord in the palette's syntax as GPUI reads it: `ctrl-alt-shift-cmd-` and the key.
    pub(super) fn keys(self) -> String {
        let mods =
            [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.shift, "shift-"), (self.cmd, "cmd-")];
        let mut out: String = mods.iter().filter(|(on, _)| *on).map(|(_, word)| *word).collect();
        out.push_str(self.key);
        out
    }
}

/// The keys a chord can name, in the palette's syntax.
const KEYS: [&str; 81] = [
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "=",
    "-",
    "]",
    "[",
    "'",
    ";",
    "\\",
    ",",
    "/",
    ".",
    "`",
    "enter",
    "tab",
    "space",
    "backspace",
    "escape",
    "home",
    "pageup",
    "delete",
    "end",
    "pagedown",
    "left",
    "right",
    "down",
    "up",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "f13",
    "f14",
    "f15",
    "f16",
    "f17",
    "f18",
    "f19",
    "f20",
];

#[cfg(test)]
mod tests {
    use super::{Chord, ChordError};

    /// The palette's syntax: the Mac's names for the modifiers, case and space ignored, a
    /// trailing `--` is the minus key, any modifiers or none, and a word that is no key refused.
    #[test]
    fn a_chord_reads_in_the_palettes_syntax() {
        let keys = |text: &str| Chord::read(text).map(Chord::keys);
        assert_eq!(keys("ctrl-`"), Ok("ctrl-`".to_owned()));
        assert_eq!(keys(" Cmd-Alt-Space "), Ok("alt-cmd-space".to_owned()), "case and space");
        assert_eq!(keys("ctrl--"), Ok("ctrl--".to_owned()), "the minus key");
        assert_eq!(keys("Option-Command-Return"), Ok("alt-cmd-enter".to_owned()), "Mac names");
        assert_eq!(keys("escape"), Ok("escape".to_owned()), "a bare key");
        assert_eq!(keys("Shift-PageUp"), Ok("shift-pageup".to_owned()));
        assert_eq!(keys("alt-a"), Ok("alt-a".to_owned()), "⌥ alone");
        assert_eq!(keys("f12"), Ok("f12".to_owned()));
        assert_eq!(keys("hyper-a"), Err(ChordError::UnknownKey("hyper".into())));
        assert_eq!(keys("ctrl-f21"), Err(ChordError::UnknownKey("f21".into())));
        assert_eq!(keys("ctrl-"), Err(ChordError::UnknownKey(String::new())));
        assert_eq!(keys("cmd-wat"), Err(ChordError::UnknownKey("wat".into())));
    }
}
