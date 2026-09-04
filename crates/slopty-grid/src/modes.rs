//! Terminal modes the client needs to see.
//!
//! Input *encoding* stays on the host (the engine knows every mode), but the client needs a few
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
        /// A password-style prompt is active (echo off); never predict here.
        const ECHO_OFF = 1 << 9;
        /// Line-buffered canonical input (a shell prompt); prediction is plausible.
        const CANONICAL = 1 << 10;
    }
}

impl TermModes {
    /// Whether a wheel event should scroll the client's viewport rather than be forwarded.
    #[must_use]
    pub const fn wheel_scrolls_viewport(self) -> bool {
        !self.contains(Self::ALT_SCREEN) && !self.contains(Self::MOUSE_TRACKING)
    }

    /// Whether local echo prediction is allowed at all in these modes.
    #[must_use]
    pub const fn prediction_allowed(self) -> bool {
        !self.contains(Self::ALT_SCREEN)
            && !self.contains(Self::ECHO_OFF)
            && !self.contains(Self::MOUSE_TRACKING)
            && !self.contains(Self::KITTY_KEYBOARD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_routing() {
        assert!(TermModes::empty().wheel_scrolls_viewport());
        assert!(!TermModes::ALT_SCREEN.wheel_scrolls_viewport());
        assert!(!TermModes::MOUSE_TRACKING.wheel_scrolls_viewport());
    }

    #[test]
    fn prediction_gates() {
        assert!(TermModes::CANONICAL.prediction_allowed());
        assert!(!(TermModes::CANONICAL | TermModes::ECHO_OFF).prediction_allowed());
        assert!(!TermModes::ALT_SCREEN.prediction_allowed());
    }
}
