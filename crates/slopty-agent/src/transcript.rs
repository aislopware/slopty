//! The last thing the assistant said, read from a Claude Code transcript (JSONL).
//!
//! Each line is one record; assistant turns look like
//! `{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"…"},…]}}`.
//! Only the tail is read: a transcript grows to megabytes, and the answer is always in the
//! last few records.

use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// How much of the file's end is scanned; one assistant record with a long thinking block can
/// run to tens of kilobytes.
const TAIL_BYTES: u64 = 256 * 1024;

/// The last non-empty line of the last assistant text block in the transcript at `path`.
/// `None` when the file cannot be read or holds no assistant text in its tail.
#[must_use]
pub fn last_assistant_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = String::new();
    BufReader::new(file).read_to_string(&mut tail).ok()?;
    last_assistant_line_in(&tail, start > 0)
}

/// [`last_assistant_line`] over text already in memory; `cut` says the first line may be a
/// fragment (the read started mid-record) and must be skipped.
#[must_use]
pub fn last_assistant_line_in(tail: &str, cut: bool) -> Option<String> {
    let mut lines: Vec<&str> = tail.lines().collect();
    if cut {
        lines.drain(..1.min(lines.len()));
    }
    lines.iter().rev().find_map(|line| assistant_text(line))
}

/// The last non-empty text line of an assistant record; `None` for any other record.
fn assistant_text(line: &str) -> Option<String> {
    // Cheap pre-filter before parsing: most records are not assistant text.
    if !line.contains("\"assistant\"") {
        return None;
    }
    let record: serde_json::Value = serde_json::from_str(line).ok()?;
    if record.get("type").and_then(serde_json::Value::as_str) != Some("assistant")
        || record.get("isSidechain").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = record.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .rev()
            .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .find_map(|b| b.get("text").and_then(serde_json::Value::as_str))?,
        _ => return None,
    };
    last_line(text)
}

/// The last non-empty line of a message, trimmed.
#[must_use]
pub fn last_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIL: &str = concat!(
        r#"{"type":"user","message":{"role":"user","content":"fix it"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Looking at the build.\n\nRunning the tests now."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent chatter"}]}}"#,
        "\n",
        r#"{"type":"progress","data":{}}"#,
        "\n",
    );

    #[test]
    fn finds_the_last_main_thread_text() {
        assert_eq!(last_assistant_line_in(TAIL, false).as_deref(), Some("Running the tests now."));
    }

    #[test]
    fn a_cut_first_line_is_skipped() {
        let cut = &TAIL[10..];
        assert_eq!(last_assistant_line_in(cut, true).as_deref(), Some("Running the tests now."));
        assert_eq!(last_assistant_line_in("garbage\n", true), None);
        assert_eq!(last_assistant_line_in("", false), None);
    }

    #[test]
    fn reads_a_file_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let mut big = String::new();
        for _ in 0..3000 {
            big.push_str(r#"{"type":"user","message":{"role":"user","content":"padding padding padding padding"}}"#);
            big.push('\n');
        }
        big.push_str(TAIL);
        std::fs::write(&path, &big).expect("write");
        assert_eq!(last_assistant_line(&path).as_deref(), Some("Running the tests now."));
        assert_eq!(last_assistant_line(&dir.path().join("missing.jsonl")), None);
    }
}
