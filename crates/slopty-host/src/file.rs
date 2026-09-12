//! A file read for a file card: the first [`FILE_LINES`] lines of a text file on this
//! machine, or the word for why not.

use std::io::Read as _;
use std::path::Path;
use std::time::UNIX_EPOCH;

use slopty_proto::file::{FILE_BYTES, FILE_LINES, FileRead};

/// Read `path` for a card: at most [`FILE_BYTES`] from the start, then at most
/// [`FILE_LINES`] lines of it.
///
/// A NUL byte or invalid UTF-8 in what was read makes it `Binary`; anything the OS refuses
/// (missing, a directory, not permitted) is `Missing` with the OS's word. The bytes cut are
/// counted as lines only when they were read, so a file past the byte cap reports the lines
/// it had within it and `more_lines` from there.
#[must_use]
pub fn read(path: &Path) -> FileRead {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    if meta.is_dir() {
        return FileRead::Missing { error: "Is a directory".to_owned() };
    }
    let size = meta.len();
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let mut bytes = Vec::with_capacity(usize::try_from(size.min(FILE_BYTES)).unwrap_or(0));
    if let Err(e) = (&mut file).take(FILE_BYTES).read_to_end(&mut bytes) {
        return FileRead::Missing { error: os_word(&e) };
    }
    if bytes.contains(&0) {
        return FileRead::Binary { size };
    }
    let truncated = size > FILE_BYTES;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if truncated => {
            // The cap may have split a character: keep what decodes and drop the tail.
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => return FileRead::Binary { size },
    };
    let (text, more_lines) = clip(&text, truncated);
    FileRead::Text { text, more_lines, size, modified_ms }
}

/// The first [`FILE_LINES`] lines of `text` and the count of the rest; a byte-capped read
/// drops its last, possibly partial, line and counts it.
fn clip(text: &str, truncated: bool) -> (String, u32) {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if truncated {
        lines.pop();
    }
    let kept = lines.len().min(FILE_LINES as usize);
    let dropped = u32::try_from(lines.len().saturating_sub(kept)).unwrap_or(u32::MAX);
    let more = dropped.saturating_add(u32::from(truncated));
    lines.truncate(kept);
    (lines.join("\n"), more)
}

/// The OS's message without the "(os error N)" suffix.
fn os_word(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_binary_missing_and_clipped() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, "one\ntwo\n").unwrap();
        match read(&text) {
            FileRead::Text { text, more_lines, size, modified_ms } => {
                assert_eq!((text.as_str(), more_lines, size), ("one\ntwo", 0, 8));
                assert!(modified_ms > 0);
            }
            other => panic!("{other:?}"),
        }
        let bin = dir.path().join("a.bin");
        std::fs::write(&bin, b"ab\0cd").unwrap();
        assert_eq!(read(&bin), FileRead::Binary { size: 5 });
        let latin = dir.path().join("latin.txt");
        std::fs::write(&latin, b"na\xefve\n").unwrap();
        assert_eq!(read(&latin), FileRead::Binary { size: 6 });
        assert!(matches!(read(&dir.path().join("gone")), FileRead::Missing { .. }));
        assert_eq!(read(dir.path()), FileRead::Missing { error: "Is a directory".to_owned() });
        let long = dir.path().join("long.txt");
        let body: String =
            (0..FILE_LINES + 5).map(|i| format!("l{i}\n")).collect::<Vec<_>>().concat();
        std::fs::write(&long, &body).unwrap();
        match read(&long) {
            FileRead::Text { text, more_lines, .. } => {
                assert_eq!(text.lines().count(), FILE_LINES as usize);
                assert_eq!(more_lines, 5);
            }
            other => panic!("{other:?}"),
        }
        let huge = dir.path().join("huge.txt");
        let line = "x".repeat(1023);
        let body = format!("{line}\n").repeat(600);
        std::fs::write(&huge, &body).unwrap();
        match read(&huge) {
            FileRead::Text { text, more_lines, size, .. } => {
                assert_eq!(size, 600 * 1024);
                assert_eq!(text.lines().count(), 511, "512 KiB less the cut last line");
                assert_eq!(more_lines, 1, "the rest is counted as one: the host did not read it");
            }
            other => panic!("{other:?}"),
        }
    }
}
