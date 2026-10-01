//! The shell integration state a checkpoint carries beside the screen: the lines prompts
//! started on, the statuses commands ended with, and the command blocks (OSC 133).
//!
//! The formatter writes what libghostty holds of the marks, each row's prompt flag and each
//! cell's content, but these three are the engine's own. A checkpoint appends them after each
//! screen it formats, in an OSC of Slopty's (`OSC 6973`), with every line relative to the
//! screen's oldest row; a fresh engine's first write takes them back, and the marks the
//! formatter replays on the way are not counted again.
//!
//! A program can write the same OSC, but only a fresh engine's first write is read for it, and
//! a program can write any OSC 133 mark it likes anyway: it forges nothing a shell could not.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use super::read::{Block, End};

/// How the payload starts. It ends with ST.
const OPEN: &[u8] = b"\x1b]6973;";
const ST: &[u8] = b"\x1b\\";

/// The marks of one screen, by absolute line.
#[derive(Debug, Default)]
pub(super) struct Carried {
    pub exit_marks: BTreeMap<u64, Option<u8>>,
    pub prompt_starts: BTreeSet<u64>,
    pub commands: VecDeque<Block>,
}

/// The marks of a screen whose oldest row is absolute line `base`, as the OSC that carries
/// them.
pub(super) struct Payload<'a> {
    pub base: u64,
    pub exit_marks: &'a BTreeMap<u64, Option<u8>>,
    pub prompt_starts: &'a BTreeSet<u64>,
    pub commands: &'a VecDeque<Block>,
}

/// A line relative to the screen's oldest row: an `n` before the distance when it is above it
/// (a command's end can be written above its prompt). Read back onto a replay with fewer rows
/// above, it lands on the oldest: a status there still reaches the prompt below it, and an
/// end there still ends its output where it starts.
struct Rel(u64, u64);

impl fmt::Display for Rel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(line, base) = *self;
        match line.checked_sub(base) {
            Some(below) => write!(f, "{below}"),
            None => write!(f, "n{}", base.saturating_sub(line)),
        }
    }
}

/// A value or `-` for none.
struct Opt<T>(Option<T>);

impl<T: fmt::Display> fmt::Display for Opt<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => v.fmt(f),
            None => f.write_str("-"),
        }
    }
}

/// `s=` the prompt starts; `e=` each status as `line:exit`; `c=` each block as
/// `prompt.output` and, once it ended, `.line.col.exit`. Lists are comma separated.
impl fmt::Display for Payload<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = self.base;
        f.write_str("\x1b]6973;s=")?;
        for (i, &line) in self.prompt_starts.iter().enumerate() {
            write!(f, "{}{}", if i == 0 { "" } else { "," }, Rel(line, base))?;
        }
        f.write_str(";e=")?;
        for (i, (&line, &exit)) in self.exit_marks.iter().enumerate() {
            write!(f, "{}{}:{}", if i == 0 { "" } else { "," }, Rel(line, base), Opt(exit))?;
        }
        f.write_str(";c=")?;
        for (i, block) in self.commands.iter().enumerate() {
            let output = Opt(block.output.map(|line| Rel(line, base)));
            write!(f, "{}{}.{output}", if i == 0 { "" } else { "," }, Rel(block.prompt, base))?;
            if let Some(end) = block.end {
                write!(f, ".{}.{}.{}", Rel(end.line, base), end.col, Opt(end.exit))?;
            }
        }
        f.write_str("\x1b\\")
    }
}

/// Where the first payload in `bytes` is: the bytes before it, its body, and the bytes after
/// it. `None` when there is none, or it is not terminated.
pub(super) fn split(bytes: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let at = memchr::memmem::find(bytes, OPEN)?;
    let (before, from) = bytes.split_at(at);
    let body = from.get(OPEN.len()..)?;
    let end = memchr::memmem::find(body, ST)?;
    Some((before, body.get(..end)?, body.get(end.saturating_add(ST.len())..)?))
}

/// Whether `bytes` holds a payload.
pub(super) fn holds(bytes: &[u8]) -> bool {
    split(bytes).is_some()
}

/// The marks in a payload's body, for a screen whose oldest row is absolute line `base`.
/// `None` when the body is not one a checkpoint writes.
pub(super) fn decode(body: &[u8], base: u64) -> Option<Carried> {
    let body = std::str::from_utf8(body).ok()?;
    let mut fields = body.split(';');
    let starts = fields.next()?.strip_prefix("s=")?;
    let exits = fields.next()?.strip_prefix("e=")?;
    let commands = fields.next()?.strip_prefix("c=")?;
    if fields.next().is_some() {
        return None;
    }
    let mut marks = Carried::default();
    for line in items(starts) {
        marks.prompt_starts.insert(abs(line, base)?);
    }
    for mark in items(exits) {
        let (line, exit_code) = mark.split_once(':')?;
        marks.exit_marks.insert(abs(line, base)?, exit(exit_code).ok()?);
    }
    for block in items(commands) {
        let mut parts = block.split('.');
        let prompt = abs(parts.next()?, base)?;
        let output = match parts.next()? {
            "-" => None,
            line => Some(abs(line, base)?),
        };
        let end = match parts.next() {
            None => None,
            Some(line) => Some(End {
                line: abs(line, base)?,
                col: parts.next()?.parse().ok()?,
                exit: exit(parts.next()?).ok()?,
            }),
        };
        if parts.next().is_some() {
            return None;
        }
        marks.commands.push_back(Block { prompt, output, end });
    }
    Some(marks)
}

fn items(list: &str) -> impl Iterator<Item = &str> {
    list.split(',').filter(|item| !item.is_empty())
}

fn abs(rel: &str, base: u64) -> Option<u64> {
    match rel.strip_prefix('n') {
        Some(above) => Some(base.saturating_sub(above.parse().ok()?)),
        None => base.checked_add(rel.parse().ok()?),
    }
}

/// A status: `-` for none, else a number.
fn exit(value: &str) -> Result<Option<u8>, std::num::ParseIntError> {
    if value == "-" { Ok(None) } else { value.parse().map(Some) }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn a_payload_reads_back_as_written_from_another_base() {
        let exit_marks = BTreeMap::from([(98, Some(3)), (104, None), (110, Some(0))]);
        let prompt_starts = BTreeSet::from([100, 105]);
        let commands = VecDeque::from([
            Block {
                prompt: 100,
                output: Some(101),
                end: Some(End { line: 98, col: 4, exit: Some(3) }),
            },
            Block { prompt: 105, output: None, end: None },
            Block { prompt: 106, output: Some(107), end: None },
        ]);
        let payload = Payload {
            base: 100,
            exit_marks: &exit_marks,
            prompt_starts: &prompt_starts,
            commands: &commands,
        }
        .to_string();
        assert_eq!(payload, "\x1b]6973;s=0,5;e=n2:3,4:-,10:0;c=0.1.n2.4.3,5.-,6.7\x1b\\",);
        let (before, body, after) = split(format!("ab{payload}cd").as_bytes()).map_or_else(
            || panic!("a payload"),
            |(b, body, a)| (b.to_vec(), body.to_vec(), a.to_vec()),
        );
        assert_eq!((before.as_slice(), after.as_slice()), (&b"ab"[..], &b"cd"[..]));
        let marks = decode(&body, 7).unwrap();
        let moved = |l: u64| l - 93;
        assert_eq!(
            marks.exit_marks,
            exit_marks.iter().map(|(&l, &e)| (moved(l), e)).collect::<BTreeMap<_, _>>(),
        );
        assert_eq!(marks.prompt_starts, prompt_starts.iter().map(|&l| moved(l)).collect());
        let blocks = |c: &VecDeque<Block>| format!("{c:?}");
        let shifted: VecDeque<Block> = commands
            .iter()
            .map(|b| Block {
                prompt: moved(b.prompt),
                output: b.output.map(moved),
                end: b.end.map(|e| End { line: moved(e.line), ..e }),
            })
            .collect();
        assert_eq!(blocks(&marks.commands), blocks(&shifted));
    }

    #[test]
    fn a_body_a_checkpoint_does_not_write_is_refused() {
        for body in
            ["", "s=;e=", "s=x;e=;c=", "s=;e=1;c=", "s=;e=;c=1", "s=;e=;c=1.2.3", "s=;e=;c=;x"]
        {
            assert!(decode(body.as_bytes(), 0).is_none(), "{body:?}");
        }
        assert!(decode(b"s=;e=;c=", 0).is_some_and(|m| m.commands.is_empty()));
        assert!(split(b"\x1b]6973;s=;e=;c=").is_none(), "not terminated");
    }
}
