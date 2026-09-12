//! Where a byte stream stands with respect to VT syntax: at a sequence boundary or inside one.
//!
//! The host checkpoints a session's state after a quiet spell. A checkpoint replaces the output
//! ptyd kept, so if the last bytes before it were half an escape sequence (or half a UTF-8
//! character), the half that arrives after a restart would print as text. [`Boundary`] follows
//! the parser's shape closely enough to say whether the stream is at a boundary; it is a
//! heuristic for pacing, never for parsing.

/// Parser-shaped state over the bytes fed so far.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Boundary {
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum State {
    #[default]
    Ground,
    /// After `ESC`, possibly with intermediates.
    Esc,
    /// Inside `CSI`, before the final byte.
    Csi,
    /// Inside an `OSC` string.
    Osc,
    /// `ESC` seen inside an `OSC` string (a `ST` may follow).
    OscEsc,
    /// Inside a `DCS`, `APC`, `PM` or `SOS` string.
    Str,
    /// `ESC` seen inside such a string.
    StrEsc,
    /// Inside a multi-byte UTF-8 sequence, this many continuation bytes still owed.
    Utf8(u8),
}

impl Boundary {
    /// Follow `bytes`.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    /// True when the stream ends at a sequence and character boundary.
    #[must_use]
    pub const fn is_ground(self) -> bool {
        matches!(self.state, State::Ground)
    }

    fn step(&mut self, b: u8) {
        // CAN and SUB abort any sequence; ESC starts one from anywhere.
        if matches!(b, 0x18 | 0x1a) {
            self.state = State::Ground;
            return;
        }
        self.state = match self.state {
            State::Ground => Self::after_ground(b),
            State::Utf8(left) => {
                if matches!(b, 0x80..=0xbf) {
                    match left.saturating_sub(1) {
                        0 => State::Ground,
                        n => State::Utf8(n),
                    }
                } else {
                    // Not a continuation: the character was cut short; this byte starts afresh.
                    Self::after_ground(b)
                }
            }
            State::Esc => match b {
                b'[' => State::Csi,
                b']' => State::Osc,
                b'P' | b'_' | b'^' | b'X' => State::Str,
                0x1b | 0x20..=0x2f => State::Esc,
                _ => State::Ground,
            },
            State::Csi => match b {
                0x40..=0x7e => State::Ground,
                0x1b => State::Esc,
                _ => State::Csi,
            },
            State::Osc => match b {
                0x07 => State::Ground,
                0x1b => State::OscEsc,
                _ => State::Osc,
            },
            State::Str => {
                if b == 0x1b {
                    State::StrEsc
                } else {
                    State::Str
                }
            }
            State::OscEsc | State::StrEsc => {
                if b == b'\\' {
                    State::Ground
                } else {
                    // Not a string terminator: the string was cut short by a new escape, and
                    // this byte is the one after that escape.
                    self.state = State::Esc;
                    self.step(b);
                    return;
                }
            }
        };
    }

    /// The state after `b` arrives at a boundary.
    const fn after_ground(b: u8) -> State {
        match b {
            0x1b => State::Esc,
            0xc0..=0xdf => State::Utf8(1),
            0xe0..=0xef => State::Utf8(2),
            0xf0..=0xf7 => State::Utf8(3),
            _ => State::Ground,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Boundary;

    fn ground_after(bytes: &[u8]) -> bool {
        let mut b = Boundary::default();
        b.feed(bytes);
        b.is_ground()
    }

    #[test]
    fn plain_text_and_complete_sequences_are_boundaries() {
        assert!(ground_after(b"hello\r\n"));
        assert!(ground_after(b"\x1b[31mred\x1b[0m"));
        assert!(ground_after(b"\x1b]0;title\x07"));
        assert!(ground_after(b"\x1b]0;title\x1b\\"));
        assert!(ground_after(b"\x1bP+q544e\x1b\\"));
        assert!(ground_after(b"\x1b(B"));
        assert!(ground_after("héllo ─".as_bytes()));
        assert!(ground_after(b"\x1b[?1049h\x1b[H"));
    }

    #[test]
    fn half_sequences_and_half_characters_are_not() {
        assert!(!ground_after(b"\x1b"));
        assert!(!ground_after(b"\x1b["));
        assert!(!ground_after(b"\x1b[3"));
        assert!(!ground_after(b"\x1b[?104"));
        assert!(!ground_after(b"\x1b]0;tit"));
        assert!(!ground_after(b"\x1b]0;title\x1b"));
        assert!(!ground_after(b"\x1b("));
        assert!(!ground_after(b"\xe2\x94"));
        assert!(!ground_after(b"\xf0\x9f\x98"));
        assert!(!ground_after(b"\xc3"), "half of a two-byte character");
        // Every string introducer opens a string, and ESC ESC is still an escape.
        assert!(!ground_after(b"\x1bP+q"));
        assert!(!ground_after(b"\x1b_Gx"));
        assert!(!ground_after(b"\x1b^x"));
        assert!(!ground_after(b"\x1bXx"));
        assert!(!ground_after(b"\x1b\x1b"));
        assert!(ground_after(b"\x1b\x1b[0m"));
        // An escape that cuts a string short is followed, not dropped.
        assert!(!ground_after(b"\x1b]0;half\x1b["));
        assert!(!ground_after(b"\x1bP+q\x1b["));
    }

    #[test]
    fn the_state_carries_across_feeds_and_aborts_recover() {
        let mut b = Boundary::default();
        b.feed(b"abc\x1b[");
        assert!(!b.is_ground());
        b.feed(b"31m");
        assert!(b.is_ground());
        b.feed(b"\x1b]0;half");
        b.feed(b"\x18");
        assert!(b.is_ground(), "CAN aborts");
        b.feed(b"\x1b]0;half\x1b[0m");
        assert!(b.is_ground(), "an escape inside an OSC starts a new sequence");
        b.feed(b"\xe2x");
        assert!(b.is_ground(), "a broken character does not swallow the byte after it");
    }
}
