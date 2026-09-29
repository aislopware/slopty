//! Finds `OSC 133;A` (prompt start) and `OSC 133;D` (command end, with the exit status) in the
//! byte stream.
//!
//! libghostty-vt consumes OSC 133 for its per-row prompt flags, but a flag cannot tell two
//! prompts on adjacent rows apart (a command with no output), and the status `D` carries is not
//! exposed at all. Nor does its unknown-sequence callback help: it reports only the OSC numbers
//! libghostty does not implement, and 133 is one it does. So the engine watches the bytes itself
//! and notes the cursor row at each mark. The scanner keeps its state across writes: a sequence
//! split over two PTY reads is still found.
//!
//! A mark is found exactly where libghostty's parser (`Parser.zig`, `parse_table.zig`) acts on
//! it. Every state of that parser takes ESC as the start of a new sequence, so the scanner jumps
//! from one ESC to the next and never tracks CSI, DCS, APC or text. An OSC ends on BEL, or on
//! ESC whatever follows it (the ESC starts the next sequence), and CAN or SUB cancel it with no
//! effect. The other C0 controls inside an OSC are dropped, not part of its payload, and inside
//! an escape they run without ending it.

/// Which of its prompt a shell redraws after a resize, as libghostty-vt reads `133;A;redraw=`:
/// it clears those rows first. The terminal keeps the last one an `A` gave.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Redraw {
    /// Nothing: libghostty-vt's default for embedders.
    #[default]
    Off,
    /// The whole prompt (`redraw=1`: zsh, fish).
    Full,
    /// Its last row (`redraw=last`: bash).
    Last,
}

/// Longest OSC payload worth collecting. ghostty's own bash integration writes
/// `133;A;redraw=last;cl=line;aid=<pid>`, past 32 bytes; room is left for more parameters.
const MAX_PAYLOAD: usize = 128;

// Byte values the VT parser gives a meaning inside an OSC or an escape.
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;

/// Incremental scanner for `ESC ] 133 ; (A | P | C | D [; status]) … (BEL | ESC)`.
#[derive(Debug)]
#[expect(missing_copy_implementations, reason = "a copy would fork the stream's state")]
pub struct Scanner {
    state: State,
    /// The payload of the OSC in progress while it may still be a mark: `payload[..len]`.
    payload: [u8; MAX_PAYLOAD],
    len: usize,
}

impl Default for Scanner {
    fn default() -> Self {
        Self { state: State::Ground, payload: [0; MAX_PAYLOAD], len: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Anywhere but an escape or an OSC that may be a mark: waiting for the next ESC.
    Ground,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC ]` and a payload that may still be `133;…`; collecting it.
    Osc,
}

/// A completed mark.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Found {
    /// Bytes consumed up to and including the byte that ended the OSC (BEL, or the ESC that
    /// ends it and starts the next sequence).
    pub end: usize,
    /// Which mark.
    pub mark: Mark,
}

/// The marks worth stopping at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// `133;A`: a primary prompt starts on the cursor row (`k=s`/`k=c` continuations are not
    /// reported; they are rows of the same prompt).
    PromptStart {
        /// What the shell said it redraws after a resize (kitty's `redraw`, ghostty's
        /// `redraw=last`), when an `A` said it.
        redraw: Option<Redraw>,
    },
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
    /// Scan `bytes` from the start; stop at the first complete mark and say where it ended.
    /// Bytes after it are not looked at (call again with them). `None` means the whole slice
    /// was consumed; a sequence still open at the end carries over to the next call.
    pub fn scan(&mut self, bytes: &[u8]) -> Option<Found> {
        const PREFIX: &[u8] = b"133;";
        let mut i = 0;
        while let Some(&b) = bytes.get(i) {
            let at = i;
            i = i.saturating_add(1);
            match self.state {
                State::Ground => {
                    // Output is mostly text: jump to the next escape rather than stepping to it.
                    let skip = memchr::memchr(ESC, bytes.get(at..).unwrap_or_default())?;
                    self.state = State::Esc;
                    i = at.saturating_add(skip).saturating_add(1);
                }
                State::Esc => {
                    self.state = match b {
                        b']' => {
                            self.len = 0;
                            State::Osc
                        }
                        CAN | SUB => State::Ground,
                        // Run (DEL: dropped) with the escape still open; ESC opens it again.
                        0x00..=0x1f | 0x7f => State::Esc,
                        _ => State::Ground,
                    };
                }
                State::Osc => match b {
                    BEL | ESC => {
                        self.state = if b == ESC { State::Esc } else { State::Ground };
                        if let Some(mark) = mark(self.payload.get(..self.len).unwrap_or_default()) {
                            return Some(Found { end: i, mark });
                        }
                    }
                    CAN | SUB => self.state = State::Ground,
                    0x00..=0x1f => {}
                    _ => match self.payload.get_mut(self.len) {
                        Some(slot) if PREFIX.get(self.len).is_none_or(|&p| p == b) => {
                            *slot = b;
                            self.len = self.len.saturating_add(1);
                        }
                        // Not a mark (a title, an OSC 8 link), or too long to be one: it ends at
                        // the next ESC, or on a byte that cannot start a mark.
                        _ => self.state = State::Ground,
                    },
                },
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
            let (mut continuation, mut redraw) = (false, None);
            for param in params {
                match param {
                    b"k=s" | b"k=c" => continuation = true,
                    b"redraw=0" => redraw = Some(Redraw::Off),
                    b"redraw=1" => redraw = Some(Redraw::Full),
                    b"redraw=last" => redraw = Some(Redraw::Last),
                    _ => {}
                }
            }
            // libghostty reads `redraw` on a fresh-line prompt start (`A`) only.
            let redraw = redraw.filter(|_| kind == b'A');
            (!continuation).then_some(Mark::PromptStart { redraw })
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
        assert_eq!(
            s.scan(b"\x1b]133;D;130\x1b\\x"),
            Some(end(12, Some(130))),
            "the ESC of ST ends it; the backslash is fed after the mark"
        );
        assert_eq!(s.scan(b"\\x"), None);
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
        let start = Some(Found { end: 8, mark: Mark::PromptStart { redraw: None } });
        assert_eq!(s.scan(b"\x1b]133;A\x07$ "), start);
        assert_eq!(
            s.scan(b"\x1b]133;A;cl=line\x07"),
            Some(Found { end: 16, mark: Mark::PromptStart { redraw: None } })
        );
        assert_eq!(
            s.scan(b"\x1b]133;A;k=s\x07\x1b]133;P;k=i\x07\x1b]133;B\x07"),
            Some(Found { end: 24, mark: Mark::PromptStart { redraw: None } }),
            "the continuation is skipped, the in-place P is a start, B is nothing"
        );
        assert_eq!(s.scan(b"\x1b]133;A;k=c\x07"), None);
    }

    #[test]
    fn a_prompt_start_carries_what_the_shell_redraws() {
        let mut s = Scanner::default();
        let redraw = |s: &mut Scanner, bytes: &[u8]| match s.scan(bytes) {
            Some(Found { mark: Mark::PromptStart { redraw }, .. }) => redraw,
            other => panic!("{other:?}"),
        };
        assert_eq!(redraw(&mut s, b"\x1b]133;A;redraw=1\x07"), Some(Redraw::Full));
        assert_eq!(redraw(&mut s, b"\x1b]133;A;cl=line;redraw=last\x07"), Some(Redraw::Last));
        assert_eq!(redraw(&mut s, b"\x1b]133;A;redraw=0\x1b\\"), Some(Redraw::Off));
        assert_eq!(redraw(&mut s, b"\\\x1b]133;A;redraw=2\x07"), None, "not a value");
        assert_eq!(redraw(&mut s, b"\x1b]133;P;k=i;redraw=1\x07"), None, "read on `A` only");
        assert_eq!(
            redraw(&mut s, b"\x1b]133;A;redraw=last;cl=line;aid=4294967295\x07"),
            Some(Redraw::Last),
            "ghostty's own bash integration, past 32 bytes"
        );
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
            Some(Found { end: 12, mark: Mark::PromptStart { redraw: None } }),
            "P is a prompt start too, drawn in place"
        );
        assert_eq!(s.scan(b"\x1b]133;P;k=s\x07"), None, "a continuation, as with A");
        assert_eq!(s.scan(b"\x1b]0;title\x07\x1b[31m\x1b]8;;http://x\x1b\\"), None);
        assert_eq!(s.scan(b"\x1b[?2004h\x1bP+q544e\x1b\\\x1b_Gi=1;AAAA\x1b\\"), None);
        let long = [b"\x1b]133;D;".as_slice(), &[b'9'; MAX_PAYLOAD], b"\x07"].concat();
        assert_eq!(s.scan(&long), None, "overlong payloads are dropped");
    }

    /// An OSC ends where libghostty's parser ends it: on ESC whatever follows (the ESC starts
    /// the next sequence), so a cut-short mark still counts, as it does for the terminal.
    #[test]
    fn any_escape_ends_the_mark_and_starts_the_next_sequence() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"\x1b]133;D;3\x1b[0m"), Some(end(10, Some(3))));
        assert_eq!(s.scan(b"[0m"), None);
        assert_eq!(
            s.scan(b"\x1b]133;D;3\x1b]133;D;4\x07"),
            Some(end(10, Some(3))),
            "two marks back to back, the first cut short"
        );
        assert_eq!(s.scan(b"]133;D;4\x07"), Some(end(9, Some(4))));
        assert_eq!(s.scan(b"\x1b\x1b]133;D;2\x07"), Some(end(11, Some(2))), "ESC ESC");
        assert_eq!(s.scan(b"\x1b]0;x\x1b]133;D;5\x07"), Some(end(15, Some(5))), "after a title");
    }

    /// CAN and SUB cancel an OSC in progress, or an escape, and the terminal acts on nothing in
    /// it: a mark cut off by one is not reported, whole or split across reads.
    #[test]
    fn can_and_sub_cancel_a_mark() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"\x1b]133;A\x18$ "), None);
        assert_eq!(s.scan(b"\x1b]133;D;1\x1a\x07"), None, "the BEL after it is a bell");
        assert_eq!(s.scan(b"\x1b]133;D;1"), None);
        assert_eq!(s.scan(b"\x18\x07"), None, "cancelled in the next read");
        assert_eq!(s.scan(b"\x1b\x18]133;A\x07"), None, "an escape cancelled before its `]`");
        assert_eq!(
            s.scan(b"\x1b]133;A\x18\x1b]133;D;0\x07"),
            Some(end(18, Some(0))),
            "the next sequence after a cancel is read"
        );
    }

    /// The other C0 controls are dropped inside an OSC, and run inside an escape without
    /// ending it, as in libghostty's parse table.
    #[test]
    fn other_controls_do_not_end_a_mark() {
        let mut s = Scanner::default();
        assert_eq!(s.scan(b"\x1b]133;D;\x001\x0d\x07"), Some(end(12, Some(1))));
        assert_eq!(
            s.scan(b"\x1b\x05]133;A\x07"),
            Some(Found { end: 9, mark: Mark::PromptStart { redraw: None } })
        );
    }

    /// What scanning costs per OSC that is not a mark (titles, OSC 8 links: a `ls
    /// --hyperlink` or a prompt theme writes one per name). `cargo xtask bench --filter
    /// osc_scan_cost` runs it (MEASUREMENTS.md "OSC 133 scan").
    #[test]
    #[ignore = "measurement, run by hand"]
    fn osc_scan_cost() {
        let unit = b"\x1b]8;;file:///Users/me/src/slopty/crates/a.rs\x1b\\a.rs\x1b]8;;\x1b\\  \x1b]0;t\x07";
        let buf: Vec<u8> = unit.iter().copied().cycle().take(unit.len() * 10_000).collect();
        let oscs = 30_000;
        let mut scan =
            slopty_testkit::bench::Bench::new("engine.osc_scan_cost").series("per_osc").ops(oscs);
        for _ in 0..20 {
            let mut s = Scanner::default();
            assert_eq!(scan.time(|| s.scan(std::hint::black_box(&buf))), None);
        }
        scan.report().unwrap();
    }
}
