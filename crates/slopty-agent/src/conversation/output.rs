//! What a background command prints, read from the file Claude Code writes it to.
//!
//! A Bash call with `run_in_background` returns at once, saying where its output goes
//! (`<tmp>/claude-<uid>/<project>/<session>/tasks/<task id>.output`), and the transcript hears
//! nothing more of it until the notice that it finished. [`Outputs`] tails those files for a
//! follower: it looks at a file only while its command runs, reads only its end, and only when
//! it grew, then once more after the notice (the command has exited, so what it wrote is all
//! there). A file is read only if it is where Claude Code puts a task's output, named for the
//! task the call started, so a transcript cannot make the worker read any other file.

use std::collections::{HashMap, HashSet};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

use super::{
    Body, Cap, Clipped, Conversation, Output, Part, ShellStatus, TextRef, ThreadId, ToolDetail,
};

/// The end of a file read for its tail: far more than [`TAIL`] shows, so a tail of long lines
/// still has its whole last lines.
const WINDOW: u64 = 64 * 1024;

/// What a tail keeps: a command's last lines, as a finished command's output is kept.
pub const TAIL: Cap = super::OUTPUT;

/// Most background commands tailed at once in a session; past it, the newest.
const TAILED: usize = 32;

/// Most of a file [`read_end`] reads for a follower who asked for all of it.
pub const WHOLE: u64 = 1024 * 1024;

/// The tails sent so far, by call.
#[derive(Debug, Default)]
pub struct Outputs {
    tails: HashMap<String, Tailed>,
}

#[derive(Debug, Default)]
struct Tailed {
    /// The file's length when it was last read; `None` before the first read.
    read: Option<u64>,
    /// Read for the last time: the command finished and its output was read after.
    done: bool,
}

/// Whether `path` is where Claude Code writes the output of task `task`: a file named for the
/// task in a `tasks` directory.
#[must_use]
fn is_task_output(path: &Path, task: &str) -> bool {
    let named = path.file_name().is_some_and(|name| *name == *format!("{task}.output"));
    let in_tasks = path.parent().and_then(Path::file_name).is_some_and(|dir| dir == "tasks");
    !task.is_empty() && !task.contains(['/', '\\']) && named && in_tasks && path.is_absolute()
}

/// The output file of the background call `id` of `thread`, when it has a valid one.
#[must_use]
pub fn file_of<'a>(
    conversation: &'a Conversation,
    thread: &ThreadId,
    id: &str,
) -> Option<&'a Path> {
    let entry = conversation.entries(thread).iter().find(|e| e.id == id)?;
    let Body::Tool(call) = &entry.body else { return None };
    let ToolDetail::Bash(bash) = &call.detail else { return None };
    let path = Path::new(bash.output_file.as_deref()?);
    is_task_output(path, bash.task_id.as_deref()?).then_some(path)
}

impl Outputs {
    /// The tails of the background commands of `conversation` that changed since the last
    /// call: every one on the first call.
    pub fn read(&mut self, conversation: &Conversation) -> Vec<Output> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let calls: Vec<_> = conversation.background().collect();
        for (thread, entry) in calls.iter().rev().take(TAILED) {
            let Body::Tool(call) = &entry.body else { continue };
            let ToolDetail::Bash(bash) = &call.detail else { continue };
            let Some(path) = file_of(conversation, thread, &entry.id) else { continue };
            seen.insert(entry.id.clone());
            let tailed = self.tails.entry(entry.id.clone()).or_default();
            if tailed.done {
                continue;
            }
            let finished = bash.status != ShellStatus::Running;
            let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
                // Never written: a command that printed nothing and has finished leaves none.
                tailed.done = finished;
                continue;
            };
            if tailed.read == Some(len) {
                tailed.done = finished;
                continue;
            }
            let Ok(text) = read_end(path, WINDOW) else { continue };
            tailed.read = Some(len);
            tailed.done = finished;
            let full = TextRef {
                record: entry.id.clone(),
                part: Part::Output { tool_use_id: entry.id.clone() },
            };
            let mut tail = Clipped::tail(text.trim_end_matches('\n'), TAIL, Some(full.clone()));
            if len > WINDOW {
                tail.full = Some(full);
            }
            out.push(Output {
                thread: (*thread).clone(),
                call: entry.id.clone(),
                tail,
                bytes: len,
            });
        }
        self.tails.retain(|id, _| seen.contains(id));
        out
    }
}

/// The last `window` bytes of the file at `path` as a person reads them: colour codes and
/// what a carriage return wrote over taken out, and a first line cut in half dropped.
///
/// # Errors
///
/// When the file cannot be read.
pub fn read_end(path: &Path, window: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(window).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if start > 0 { text.split_once('\n').map_or("", |(_, rest)| rest) } else { &text };
    Ok(readable(text))
}

/// Terminal output as it looks once drawn: escape sequences gone, and each line only what the
/// last carriage return in it left (a progress bar's final state).
fn readable(text: &str) -> String {
    let plain = strip_escapes(text);
    let mut out = String::with_capacity(plain.len());
    for (n, line) in plain.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        out.push_str(line.rsplit('\r').next().unwrap_or(line));
    }
    out
}

/// `text` without its escape sequences: CSI (`ESC [ … final`), OSC (`ESC ] … BEL` or
/// `ESC \`) and the two-byte ones.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    /// A background call whose output goes to `file`, as Claude Code records it.
    fn session(c: &mut Conversation, file: &Path) {
        let call = json!({
            "type": "assistant", "uuid": "a1", "timestamp": "2026-09-27T04:16:28.402Z",
            "message": { "content": [{ "type": "tool_use", "id": "t1", "name": "Bash",
                "input": { "command": "npm run dev", "run_in_background": true } }] },
        });
        let text = format!(
            "Command running in background with ID: b1. Output is being written to: {}. You \
             will be notified when it completes.",
            file.display()
        );
        let result = json!({
            "type": "user", "uuid": "u1", "timestamp": "2026-09-27T04:16:28.418Z",
            "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1",
                "content": text }] },
            "toolUseResult": { "backgroundTaskId": "b1", "stdout": "", "stderr": "" },
        });
        c.ingest(&ThreadId::Main, &call);
        c.ingest(&ThreadId::Main, &result);
    }

    fn finish(c: &mut Conversation, file: &Path) {
        let notice = format!(
            "<task-notification>\n<task-id>b1</task-id>\n<tool-use-id>t1</tool-use-id>\n\
             <output-file>{}</output-file>\n<status>completed</status>\n<summary>Background \
             command \"dev\" completed (exit code 0)</summary>\n</task-notification>",
            file.display()
        );
        let record = json!({
            "type": "queue-operation", "operation": "enqueue", "content": notice,
            "timestamp": "2026-09-27T04:17:30.000Z",
        });
        c.ingest(&ThreadId::Main, &record);
    }

    fn append(path: &Path, text: &str) {
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(path).expect("open");
        file.write_all(text.as_bytes()).expect("write");
    }

    fn tails(outputs: &[Output]) -> Vec<(String, String, u64)> {
        outputs.iter().map(|o| (o.call.clone(), o.tail.text.clone(), o.bytes)).collect()
    }

    /// A running command's file is read when it grew and only then, its end as a person reads
    /// it; once the command finished it is read one last time and then left alone.
    #[test]
    fn a_background_commands_file_is_tailed_while_it_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks).expect("mkdir");
        let file = tasks.join("b1.output");
        let mut c = Conversation::default();
        session(&mut c, &file);
        let mut outputs = Outputs::default();
        assert!(outputs.read(&c).is_empty(), "nothing written yet");

        append(&file, "\u{1b}[32mcompiling\u{1b}[0m\n 10%\r 50%\r100%\n");
        let first = outputs.read(&c);
        assert_eq!(tails(&first), [("t1".to_owned(), "compiling\n100%".to_owned(), 34)]);
        let full = first.first().and_then(|o| o.tail.full.clone());
        assert!(full.is_none(), "the whole of it is there");
        assert!(outputs.read(&c).is_empty(), "unchanged, unread");

        append(&file, "ready on :3000\n");
        assert_eq!(tails(&outputs.read(&c))[0].1, "compiling\n100%\nready on :3000");
        append(&file, "bye\n");
        finish(&mut c, &file);
        assert_eq!(tails(&outputs.read(&c))[0].1, "compiling\n100%\nready on :3000\nbye");
        append(&file, "written after the end\n");
        assert!(outputs.read(&c).is_empty(), "a finished command is read once more, then left");
    }

    /// Only a task's own file under `tasks` is read, whatever the transcript names.
    #[test]
    fn only_a_tasks_own_file_is_read() {
        assert!(is_task_output(Path::new("/tmp/claude-1/p/s/tasks/b1.output"), "b1"));
        assert!(!is_task_output(Path::new("/tmp/claude-1/p/s/tasks/b2.output"), "b1"));
        assert!(!is_task_output(Path::new("/etc/passwd"), "b1"));
        assert!(!is_task_output(Path::new("/tmp/x/b1.output"), "b1"));
        assert!(!is_task_output(Path::new("tasks/b1.output"), "b1"), "relative");
        assert!(!is_task_output(Path::new("/tmp/tasks/.output"), ""));

        let dir = tempfile::tempdir().expect("tempdir");
        let elsewhere = dir.path().join("b1.output");
        append(&elsewhere, "secret\n");
        let mut c = Conversation::default();
        session(&mut c, &elsewhere);
        assert!(Outputs::default().read(&c).is_empty(), "not a task's output file");
    }

    /// A long log keeps its end, and a line the window cut in half is left out.
    #[test]
    fn a_long_log_keeps_its_last_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("log");
        let lines: String = (0..20_000).map(|n| ["line ", &n.to_string(), "\n"].concat()).collect();
        append(&file, &lines);
        let end = read_end(&file, 1_000).expect("read");
        assert!(end.starts_with("line "), "{end:?}");
        assert!(end.ends_with("line 19999\n"));
        let tail = Clipped::tail(&end, TAIL, None);
        assert_eq!(tail.text.lines().count(), TAIL.lines);
    }

    #[test]
    fn escapes_and_overwrites_are_taken_out() {
        assert_eq!(readable("a\u{1b}[1;31mb\u{1b}[0mc"), "abc");
        assert_eq!(readable("\u{1b}]0;title\u{7}x"), "x");
        assert_eq!(readable("\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"), "link");
        assert_eq!(readable("one\r\ntwo\r\n"), "one\ntwo\n");
        assert_eq!(readable("1/3\r2/3\r3/3"), "3/3");
    }
}
