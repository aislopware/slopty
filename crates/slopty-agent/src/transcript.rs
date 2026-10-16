//! Reading a Claude Code transcript (JSONL) as it grows: the complete lines appended since the
//! last read ([`Tail`]), for the conversation decoder ([`crate::conversation`]).

use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// A reader that follows one transcript file as it grows.
///
/// [`Tail::read_lines`] returns what was appended since the previous call, keeping an
/// unterminated last line for the next one. A file that shrank (Claude Code rewrote it, or a
/// new session reuses the path) starts over, and the read says so.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Bytes consumed so far.
    offset: u64,
    /// The last line seen without its newline, waiting for the rest. Bytes, since a read may
    /// end inside a character the writer has not finished.
    partial: Vec<u8>,
}

/// What one [`Tail::read_lines`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Lines {
    /// The file was replaced or truncated: the reader started over from its beginning, and
    /// whatever the caller kept before is stale.
    pub restarted: bool,
    /// Where in the file [`Self::text`] begins, in bytes.
    pub start: u64,
    /// The complete lines appended, each with its newline.
    pub text: String,
}

impl Tail {
    /// The complete lines appended since the last read, as text; an unterminated last line waits
    /// for the next call. A missing file is an empty read, not an error (the agent may not have
    /// written it yet).
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read.
    pub fn read_lines(&mut self, path: &Path) -> std::io::Result<Lines> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lines::default()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        let restarted = len < self.offset;
        if restarted {
            *self = Self::default();
        }
        // What waits in `partial` began this far into the file, and so do the lines read now.
        let start = self.offset.saturating_sub(self.partial.len().try_into().unwrap_or(u64::MAX));
        if len == self.offset {
            return Ok(Lines { restarted, start, text: String::new() });
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut appended = Vec::new();
        BufReader::new(file).read_to_end(&mut appended)?;
        self.offset = self.offset.saturating_add(appended.len().try_into().unwrap_or(u64::MAX));
        self.partial.append(&mut appended);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Ok(Lines { restarted, start, text: String::new() });
        };
        let rest = self.partial.split_off(end.saturating_add(1));
        let complete = std::mem::replace(&mut self.partial, rest);
        Ok(Lines { restarted, start, text: String::from_utf8_lossy(&complete).into_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only what was appended comes back, a line still being written waits for its end (inside
    /// a character too), a shorter file starts over and says so, and only a missing file reads
    /// as nothing.
    #[test]
    fn a_tail_returns_only_what_was_appended_and_keeps_partial_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let mut tail = Tail::default();
        assert_eq!(tail.read_lines(&path).expect("missing file is empty"), Lines::default());

        let line = "{\"type\":\"assistant\",\"message\":\"Đang chạy thử\"}\n";
        let split = line.find('Đ').expect("Đ") + 1;
        assert!(!line.is_char_boundary(split));
        std::fs::write(&path, &line.as_bytes()[..split]).expect("write");
        assert_eq!(tail.read_lines(&path).expect("read").text, "", "no whole line yet");
        let _error = tail.read_lines(dir.path()).expect_err("a directory is not a transcript");
        std::fs::write(&path, line).expect("write the rest");
        let read = tail.read_lines(&path).expect("read");
        assert_eq!((read.restarted, read.start, read.text.as_str()), (false, 0, line));
        assert_eq!(tail.read_lines(&path).expect("read").text, "", "nothing new");

        let more = format!("{line}{line}");
        std::fs::write(&path, &more).expect("append");
        let read = tail.read_lines(&path).expect("read");
        assert_eq!(
            (read.start, read.text.as_str()),
            (u64::try_from(line.len()).expect("fits"), line)
        );

        std::fs::write(&path, "{\"type\":\"user\"").expect("rewrite");
        let read = tail.read_lines(&path).expect("read");
        assert!(read.restarted && read.text.is_empty(), "{read:?}");
    }
}
