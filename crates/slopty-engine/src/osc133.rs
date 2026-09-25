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
    /// Saw `ESC ]` and a payload that may still be `133;…`; collecting it.
    Osc {
        payload: [u8; MAX_PAYLOAD],
        len: usize,
        /// The last byte was `ESC` (an `ESC \` terminator in progress).
        esc: bool,
    },
    /// Inside an OSC that is not a mark (a title, an OSC 8 link): skipped to its terminator.
    Skip {
        /// The last byte was `ESC`.
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
    /// `133;C`: the command's output starts on the cursor row. libghostty takes the row
    /// out of the prompt on it but writes no cell, so the row would not be in a frame.
    OutputStart,
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
        const PREFIX: &[u8] = b"133;";
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            let at = i;
            i = i.saturating_add(1);
            let done = match &mut self.state {
                State::Ground => {
                    // Output is mostly text: jump to the next escape rather than stepping to it.
                    let skip = memchr::memchr(0x1b, bytes.get(at..).unwrap_or_default())?;
                    self.state = State::Esc;
                    i = at.saturating_add(skip).saturating_add(1);
                    None
                }
                State::Esc => {
                    self.state = match b {
                        b']' => State::Osc { payload: [0; MAX_PAYLOAD], len: 0, esc: false },
                        0x1b => State::Esc,
                        _ => State::Ground,
                    };
                    None
                }
                State::Osc { esc: true, .. } | State::Skip { esc: true } if b != b'\\' => {
                    // Not a terminator: the OSC was cut short, and this escape starts whatever
                    // comes next.
                    self.state = State::Esc;
                    i = at;
                    None
                }
                State::Osc { payload, len, esc } => {
                    if *esc || b == 0x07 {
                        Some(mark(payload.get(..*len).unwrap_or_default()))
                    } else if b == 0x1b {
                        *esc = true;
                        None
                    } else if *len >= MAX_PAYLOAD || PREFIX.get(*len).is_some_and(|&p| p != b) {
                        self.state = State::Skip { esc: false };
                        None
                    } else {
                        if let Some(slot) = payload.get_mut(*len) {
                            *slot = b;
                        }
                        *len = len.saturating_add(1);
                        None
                    }
                }
                State::Skip { esc } => {
                    if *esc || b == 0x07 {
                        Some(None)
                    } else {
                        // Jump to the terminator's first byte: an OSC 8 target or a title is
                        // not looked at byte by byte.
                        let rest = bytes.get(at..).unwrap_or_default();
                        let skip = memchr::memchr2(0x07, 0x1b, rest)?;
                        i = at.saturating_add(skip);
                        if rest.get(skip) == Some(&0x1b) {
                            *esc = true;
                            i = i.saturating_add(1);
                        }
                        None
                    }
                }
            };
            if let Some(found) = done {
                self.state = State::Ground;
                if let Some(mark) = found {
                    return Some(Found { end: i, mark });
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
        // `P` is a prompt start without `A`'s fresh line: what zsh's line-init prints when
        // a theme rebuilt PS1 after the marks went in, with the prompt already drawn.
        b'A' | b'P' => {
            let continuation = params.any(|p| p == b"k=s" || p == b"k=c");
            (!continuation).then_some(Mark::PromptStart)
        }
        b'C' => Some(Mark::OutputStart),
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
        assert_eq!(
            s.scan(b"\x1b]133;A;k=s\x07\x1b]133;P;k=i\x07\x1b]133;B\x07"),
            Some(Found { end: 24, mark: Mark::PromptStart }),
            "the continuation is skipped, the in-place P is a start, B is nothing"
        );
        assert_eq!(s.scan(b"\x1b]133;A;k=c\x07"), None);
    }

    #[test]
    fn other_sequences_are_ignored() {
        let mut s = Scanner::default();
        assert_eq!(
            s.scan(b"\x1b]133;C\x07\x1b]0;title\x07"),
            Some(Found { end: 8, mark: Mark::OutputStart }),
            "C is reported: the row it lands on changes without a cell written"
        );
        assert_eq!(
            s.scan(b"\x1b]133;P;k=i\x07"),
            Some(Found { end: 12, mark: Mark::PromptStart }),
            "P is a prompt start too, drawn in place"
        );
        assert_eq!(s.scan(b"\x1b]133;P;k=s\x07"), None, "a continuation, as with A");
        assert_eq!(s.scan(b"\x1b]0;title\x07\x1b[31m\x1b]8;;http://x\x1b\\"), None);
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

    /// What scanning costs per OSC that is not a mark (titles, OSC 8 links: a `ls
    /// --hyperlink` or a prompt theme writes one per name). `cargo nextest run -p
    /// slopty-engine --release --run-ignored only osc_scan_cost --no-capture` prints it
    /// (MEASUREMENTS.md "OSC 133 scan").
    #[test]
    #[ignore = "measurement, run by hand"]
    fn osc_scan_cost() {
        let unit = b"\x1b]8;;file:///Users/me/src/slopty/crates/a.rs\x1b\\a.rs\x1b]8;;\x1b\\  \x1b]0;t\x07";
        let buf: Vec<u8> = unit.iter().copied().cycle().take(unit.len() * 10_000).collect();
        let oscs = 30_000_u32;
        let mut ns = Vec::new();
        for _ in 0..20 {
            let mut s = Scanner::default();
            let t = std::time::Instant::now();
            assert_eq!(s.scan(std::hint::black_box(&buf)), None);
            ns.push(t.elapsed().as_nanos() / u128::from(oscs));
        }
        ns.sort_unstable();
        eprintln!(
            "osc_scan_cost: p50 {} ns per OSC, max {} ns",
            ns[ns.len() / 2],
            ns[ns.len() - 1]
        );
    }
}
