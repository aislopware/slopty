//! Terminal modes the client needs to see.
//!
//! Input *encoding* stays on the worker (the engine knows every mode), but the client needs a few
//! bits to decide what a wheel or a keystroke means locally: whether to scroll the viewport or
//! forward the wheel, and whether local echo prediction is safe.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Mode bits mirrored from the engine.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct TermModes: u16 {
        /// DEC 1049/47: the alternate screen is active.
        const ALT_SCREEN = 1 << 0;
        /// Any mouse tracking mode (9, 1000, 1002, 1003) is on: wheel and clicks go to the program.
        const MOUSE_TRACKING = 1 << 1;
        /// DEC 1007: wheel on the alt screen is delivered as cursor keys.
        const ALT_SCROLL = 1 << 2;
        /// DEC 2004: bracketed paste.
        const BRACKETED_PASTE = 1 << 3;
        /// DEC 1004: focus events are reported.
        const FOCUS_EVENTS = 1 << 4;
        /// Kitty keyboard protocol has any flag set.
        const KITTY_KEYBOARD = 1 << 5;
        /// DEC 2026: synchronized output is currently held open.
        const SYNC_OUTPUT = 1 << 6;
        /// DEC 25 off: the cursor is hidden.
        const CURSOR_HIDDEN = 1 << 7;
        /// DEC 1: application cursor keys.
        const APP_CURSOR_KEYS = 1 << 8;
        /// The pty does not echo what is typed (`termios` `ECHO` off). With [`Self::CANONICAL`]
        /// that is a password prompt; without it, a line editor (zle, readline, fish) that
        /// echoes the keys itself.
        const ECHO_OFF = 1 << 9;
        /// The pty's input is line-buffered (`termios` `ICANON`): a plain `read`, not a line
        /// editor.
        const CANONICAL = 1 << 10;
        /// DEC 1002: the program wants the pointer's moves while a button it was told of is
        /// down (a drag).
        const MOUSE_DRAG = 1 << 11;
        /// DEC 1003: the program wants every move of the pointer, buttons down or not.
        const MOUSE_MOTION = 1 << 12;
        /// The program asks for key repeats and releases (Kitty keyboard flag 2, report event
        /// types): the client sends a key's release only while this is on.
        const KEY_RELEASES = 1 << 13;
    }
}

impl TermModes {
    /// Whether typing plausibly echoes at the cursor, so local echo may be predicted: not
    /// on the alternate screen (a full-screen program draws what it likes), not at a password
    /// prompt (echo off with the tty buffering the line), not with the cursor hidden, not while
    /// a program owns the mouse. Echo off alone is every line editor at its prompt: zle,
    /// readline and fish turn off `ECHO` and `ICANON` and echo each key themselves. The kitty
    /// keyboard protocol does not count against it: it changes how a key is encoded, not
    /// whether the shell echoes it (fish turns it on at every prompt).
    #[must_use]
    pub const fn prediction_allowed(self) -> bool {
        !self.contains(Self::ECHO_OFF.union(Self::CANONICAL))
            && !self
                .intersects(Self::ALT_SCREEN.union(Self::CURSOR_HIDDEN).union(Self::MOUSE_TRACKING))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prediction_gates() {
        assert!(TermModes::empty().prediction_allowed());
        assert!(TermModes::CANONICAL.prediction_allowed());
        assert!(TermModes::KITTY_KEYBOARD.prediction_allowed(), "fish's prompt");
        // zsh's prompt reads `-icanon -echo` in `stty -a`: the line editor echoes.
        assert!(TermModes::ECHO_OFF.prediction_allowed(), "a line editor at its prompt");
        assert!(!(TermModes::ECHO_OFF | TermModes::CANONICAL).prediction_allowed(), "a password");
        for off in [
            TermModes::ECHO_OFF,
            TermModes::ALT_SCREEN,
            TermModes::CURSOR_HIDDEN,
            TermModes::MOUSE_TRACKING,
        ] {
            assert!(!(TermModes::CANONICAL | off).prediction_allowed(), "{off:?}");
        }
    }
}
