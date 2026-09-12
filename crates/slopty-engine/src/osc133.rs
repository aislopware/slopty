//! Finds `OSC 133;A` (prompt start) and `OSC 133;D` (command end, with the exit status) in the
//! byte stream.
//!
//! libghostty-vt consumes OSC 133 for its per-row prompt flags, but a flag cannot tell two
//! prompts on adjacent rows apart (a command with no output), and the status `D` carries is not
//! exposed at all. So the engine watches the bytes itself and notes the cursor row at each
//! mark. The scanner keeps its state across writes: a sequence split over two PTY reads is
//! still found.

/// Longest OSC payload worth collecting; `133;D;<status>` fits with room for parameters.
const MAX_PAYLOAD: usize = 32;

/// Incremental scanner for `ESC ] 133 ; (A | D [; status]) … (BEL | ESC \)`.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC ]`; collecting the payload.
    Osc {
        payload: Vec<u8>,
        /// The last byte was `ESC` (an `ESC \` terminator in progress).
        esc: bool,
    },
}

/// A completed mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Found {
    /// Bytes consumed up to and including the terminator.
    pub end: usize,
    /// Which mark.
    pub mark: Mark,
}

/// The marks worth stopping at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// `133;A`: a primary prompt starts on the cursor row (`k=s`/`k=c` continuations are not
    /// reported; they are rows of the same prompt).
    PromptStart,
    /// `133;D`: the command ended.
    CommandEnd {
        /// The status the shell reported, when it did and it fits.
        exit: Option<u8>,
    },
}

impl Scanner {
    /// Scan `bytes` from the start; stop at the first complete `133;A`/`133;D` and say where it
    /// ended. Bytes after it are not looked at (call again with them). `None` means the whole
    /// slice was consumed; a sequence still open at the end carries over to the next call.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<Found> {
        for (i, &b) in bytes.iter().enumerate() {
            let done = match &mut self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                    None
                }
                State::Esc => {
                    self.state = match b {
                        b']' => State::Osc { payload: Vec::with_capacity(16), esc: false },
                        0x1b => State::Esc,
                        _ => State::Ground,
                    };
                    None
                }
                State::Osc { payload, esc } => {
                    if *esc {
                        if b == b'\\' {
                            Some(mark(payload))
                        } else {
                            // Not a terminator: the OSC was cut short by another escape.
                            self.state = if b == 0x1b { State::Esc } else { State::Ground };
                            continue;
                        }
                    } else if b == 0x07 {
                        Some(mark(payload))
                    } else if b == 0x1b {
                        *esc = true;
                        None
                    } else if payload.len() >= MAX_PAYLOAD {
                        self.state = State::Ground;
                        None
                    } else {
                        payload.push(b);
                        None
                    }
                }
            };
            if let Some(found) = done {
                self.state = State::Ground;
                if let Some(mark) = found {
                    return Some(Found { end: i.saturating_add(1), mark });
                }
            }
        }
        None
    }
}

/// The mark a complete OSC payload is, if it is one we report.
fn mark(payload: &[u8]) -> Option<Mark> {
    let rest = payload.strip_prefix(b"133;")?;
    let (kind, params) = match rest {
        [kind, b';', params @ ..] => (*kind, params),
        [kind] => (*kind, [].as_slice()),
        _ => return None,
    };
    let mut params = params.split(|&b| b == b';');
    match kind {
        b'A' => {
            let continuation = params.any(|p| p == b"k=s" || p == b"k=c");
            (!continuation).then_some(Mark::PromptStart)
        }
        b'D' => {
            let exit = params
                .next()
                .and_then(|digits| std::str::from_utf8(digits).ok())
                .and_then(|text| text.parse::<u8>().ok());
            Some(Mark::CommandEnd { exit })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn end(end: usize, exit: Option<u8>) -> Found {
        Found { end, mark: Mark::CommandEnd { exit } }
    }

    #[test]
    fn finds_status_with_both_terminators() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"out\x1b]133;D;1\x07more"), Some(end(13, Some(1))));
        assert_eq!(s.scan(b"\x1b]133;D;130\x1b\\x"), Some(end(13, Some(130))));
    }

    #[test]
    fn split_sequences_and_missing_status() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"abc\x1b]13"), None);
        assert_eq!(s.scan(b"3;D"), None);
        assert_eq!(s.scan(b"\x07tail"), Some(end(1, None)));
        assert_eq!(s.scan(b"\x1b]133;D;aid=7\x07"), Some(end(14, None)));
    }

    #[test]
    fn prompt_starts_but_not_continuations() {
        let mut s = Scanner::default();
        let start = Some(Found { end: 8, mark: Mark::PromptStart });
        assert_eq!(s.scan(b"\x1b]133;A\x07$ "), start);
        assert_eq!(
            s.scan(b"\x1b]133;A;cl=line\x07"),
            Some(Found { end: 16, mark: Mark::PromptStart })
        );
        assert_eq!(s.scan(b"\x1b]133;A;k=s\x07\x1b]133;P;k=i\x07\x1b]133;B\x07"), None);
    }

    #[test]
    fn other_sequences_are_ignored() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"\x1b]133;C\x07\x1b]0;title\x07\x1b[31m\x1b]8;;http://x\x1b\\"), None);
        assert_eq!(s.scan(b"\x1b]133;D;3\x1b[0m"), None, "ESC that is not ST aborts the OSC");
        assert_eq!(
            s.scan(b"\x1b]133;D;3\x1b\x1b]133;D;4\x07"),
            Some(end(20, Some(4))),
            "the escape that cut the OSC short starts the next sequence"
        );
        assert_eq!(s.scan(b"\x1b\x1b]133;D;2\x07"), Some(end(11, Some(2))), "ESC ESC");
        assert_eq!(s.scan(b"\x1b]133;D;0\x07"), Some(end(10, Some(0))));
        let long = [b"\x1b]133;D;".as_slice(), &[b'9'; 64], b"\x07"].concat();
        assert_eq!(s.scan(&long), None, "overlong payloads are dropped");
    }
}
