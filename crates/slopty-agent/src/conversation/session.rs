//! A session's transcript files, read together as they grow.
//!
//! Claude Code writes a session's conversation to `<id>.jsonl` and each subagent's to
//! `<id>/subagents/agent-<agent id>.jsonl` beside it. [`Transcripts`] reads the main file first
//! and then every subagent file it finds there or is told of (the hooks name each subagent's
//! file as it stops), feeding one [`Conversation`], so a follower gets one stream of
//! [`Change`]s for all the threads. Beside them it tails the files background commands write
//! ([`Outputs`]).
//!
//! It notes where each record's line lies in its file as it reads ([`Index`]), so the whole of a
//! clipped text or a picture is read from that line alone ([`Transcripts::locate`],
//! [`Located::text`], [`Located::image`]), off the caller's task, however long the transcript.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use super::output::{self, Outputs};
use super::{
    Change, Conversation, Output, Part, TextRef, ThreadId, full_text, image_bytes, thread_of,
};
use crate::transcript::{Lines, Tail};

/// Where Claude Code writes the subagents of the session whose transcript is `main`.
#[must_use]
pub fn subagents_dir(main: &Path) -> PathBuf {
    main.with_extension("").join("subagents")
}

/// A session's transcripts as far as they have been read.
#[derive(Debug, Default)]
pub struct Transcripts {
    conversation: Conversation,
    /// The main transcript being read, and how far.
    main: Option<(PathBuf, Tail)>,
    /// Each subagent file, and how far.
    subagents: BTreeMap<PathBuf, Tail>,
    /// The background commands' output, as far as it was sent.
    outputs: Outputs,
    /// Where each record's line is.
    index: Index,
}

/// Where a line is: its file's place in [`Index::files`], and its start and length in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Spot {
    file: usize,
    at: u64,
    len: u64,
}

/// Where the line of each record named by a `"uuid"` lies, in the files read.
#[derive(Debug, Default)]
struct Index {
    files: Vec<PathBuf>,
    by_uuid: HashMap<String, Vec<Spot>>,
}

/// How a record's id appears in a transcript line, Claude Code writing it compact.
const UUID_KEY: &str = "\"uuid\":\"";

impl Index {
    fn file(&mut self, path: &Path) -> usize {
        if let Some(at) = self.files.iter().position(|p| p == path) {
            return at;
        }
        self.files.push(path.to_path_buf());
        self.files.len().saturating_sub(1)
    }

    /// Forget every line of the file at `path`: it is read again from its start.
    fn forget(&mut self, path: &Path) {
        let Some(file) = self.files.iter().position(|p| p == path) else { return };
        for spots in self.by_uuid.values_mut() {
            spots.retain(|s| s.file != file);
        }
        self.by_uuid.retain(|_, spots| !spots.is_empty());
    }

    /// Note where each line of `lines`, read from `path`, names a record. Every id a line holds
    /// is noted; the reader checks the record's own.
    fn note(&mut self, path: &Path, lines: &Lines) {
        let file = self.file(path);
        let mut at = lines.start;
        for line in lines.text.split_inclusive('\n') {
            let len = u64::try_from(line.len()).unwrap_or(u64::MAX);
            let mut rest = line;
            while let Some(found) = rest.find(UUID_KEY) {
                rest = rest.get(found.saturating_add(UUID_KEY.len())..).unwrap_or_default();
                let Some(end) = rest.find('"') else { break };
                let id = rest.get(..end).unwrap_or_default();
                if !id.is_empty() {
                    self.by_uuid.entry(id.to_owned()).or_default().push(Spot { file, at, len });
                }
            }
            at = at.saturating_add(len);
        }
    }
}

/// Where to read the whole of a clipped text or a picture from, taken from [`Transcripts`]
/// so the read can run anywhere ([`Transcripts::locate`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Located {
    reference: TextRef,
    /// A background command's output file, read from its end.
    output: Option<PathBuf>,
    /// The lines that may hold the record, in the order the thread's files are read.
    lines: Vec<(PathBuf, u64, u64)>,
    /// The thread's files, read whole should none of the lines hold the record after all.
    files: Vec<PathBuf>,
}

impl Located {
    /// The whole text, up to [`output::WHOLE`] of a background command's output; `None` when
    /// it is not there.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        if matches!(self.reference.part, Part::Output { .. }) {
            return output::read_end(self.output.as_ref()?, output::WHOLE).ok();
        }
        self.find(|jsonl| full_text(jsonl, &self.reference))
    }

    /// The picture's bytes; `None` when it is not there or too large to send.
    #[must_use]
    pub fn image(&self) -> Option<Vec<u8>> {
        self.find(|jsonl| image_bytes(jsonl, &self.reference))
    }

    /// What `read` finds in the record's line, else in its files read whole (a line the index
    /// placed wrong, after a write that was not UTF-8); nothing when no line was noted.
    fn find<T>(&self, read: impl Fn(&str) -> Option<T>) -> Option<T> {
        if self.lines.is_empty() {
            return None;
        }
        let at_line =
            self.lines.iter().find_map(|(path, at, len)| read(&line_at(path, *at, *len).ok()?));
        at_line.or_else(|| {
            self.files.iter().find_map(|path| read(&std::fs::read_to_string(path).ok()?))
        })
    }
}

/// The `len` bytes at `at` in the file at `path`.
fn line_at(path: &Path, at: u64, len: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = Vec::new();
    file.take(len).read_to_end(&mut bytes)?;
    String::from_utf8(bytes).map_err(std::io::Error::other)
}

impl Transcripts {
    /// Read what the session's files gained since the last call.
    ///
    /// `main` is the transcript the agent writes now, and `known` the subagent files the hooks
    /// named, read beside those found in `main`'s `subagents` directory. A `main` other than
    /// the last one (the person cleared or resumed the conversation) starts over, and so does
    /// a main file that shrank; either way the changes open with [`Change::Reset`] of every
    /// thread and go on with everything there is. A file that cannot be read this time is
    /// passed over and read again from the same place next time.
    pub fn read(&mut self, main: &Path, known: &[PathBuf]) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.main.as_ref().is_none_or(|(path, _tail)| path != main) {
            *self = Self { main: Some((main.to_path_buf(), Tail::default())), ..Self::default() };
            changes.push(Change::Reset { thread: None });
        }
        if let Some((path, tail)) = &mut self.main
            && let Ok(lines) = tail.read_lines(path)
        {
            if lines.restarted {
                self.index.forget(path);
            }
            self.index.note(path, &lines);
            let read = self.conversation.take_lines(path, &lines);
            if read.iter().any(|change| matches!(change, Change::Reset { thread: None })) {
                // The decoder dropped every thread: the subagents' files are read again too,
                // and every output is sent again.
                for (path, tail) in &mut self.subagents {
                    *tail = Tail::default();
                    self.index.forget(path);
                }
                self.outputs = Outputs::default();
            }
            changes.extend(read);
        }
        let found = std::fs::read_dir(subagents_dir(main))
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| matches!(thread_of(path), ThreadId::Agent(_)));
        for path in found.chain(known.iter().cloned()) {
            self.subagents.entry(path).or_default();
        }
        for (path, tail) in &mut self.subagents {
            if let Ok(lines) = tail.read_lines(path) {
                if lines.restarted {
                    self.index.forget(path);
                }
                self.index.note(path, &lines);
                changes.extend(self.conversation.take_lines(path, &lines));
            }
        }
        changes
    }

    /// What the background commands printed since the last call: each changed tail, every
    /// one after the conversation started over.
    pub fn outputs(&mut self) -> Vec<Output> {
        self.outputs.read(&self.conversation)
    }

    /// The conversation as read so far.
    #[must_use]
    pub const fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// Where the whole of `reference`, clipped in `thread`, is read from: the record's line in
    /// the subagent's own file, else in the main one (older versions wrote subagents there),
    /// as the index has it; a background command's output from its file. Nothing is read
    /// here, so the caller reads it where it may block ([`Located::text`], [`Located::image`]).
    #[must_use]
    pub fn locate(&self, thread: &ThreadId, reference: &TextRef) -> Located {
        let output = match &reference.part {
            Part::Output { tool_use_id } => {
                output::file_of(&self.conversation, thread, tool_use_id).map(Path::to_path_buf)
            }
            _ => None,
        };
        let files: Vec<PathBuf> = self.files(thread).cloned().collect();
        let spots =
            self.index.by_uuid.get(&reference.record).map(Vec::as_slice).unwrap_or_default();
        let lines = files
            .iter()
            .flat_map(|path| {
                let file = self.index.files.iter().position(|p| p == path);
                spots
                    .iter()
                    .filter(move |s| Some(s.file) == file)
                    .map(|s| (path.clone(), s.at, s.len))
            })
            .collect();
        Located { reference: reference.clone(), output, lines, files }
    }

    /// The whole of a clipped text of `thread`, read now ([`Self::locate`]); `None` when
    /// neither file has it. A background command's output is read from the end of its file,
    /// up to [`output::WHOLE`].
    #[must_use]
    pub fn full_text(&self, thread: &ThreadId, reference: &TextRef) -> Option<String> {
        self.locate(thread, reference).text()
    }

    /// The bytes of the picture `reference` names in `thread`, read now; `None` when neither
    /// file has it, or it is too large to send.
    #[must_use]
    pub fn image(&self, thread: &ThreadId, reference: &TextRef) -> Option<Vec<u8>> {
        self.locate(thread, reference).image()
    }

    /// The files `thread`'s records can be in: a subagent's own, then the main one.
    fn files(&self, thread: &ThreadId) -> impl Iterator<Item = &PathBuf> {
        let own = self.subagents.keys().filter(move |path| thread_of(path) == *thread);
        own.chain(self.main.iter().map(|(path, _tail)| path))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::conversation::{Body, Part};

    fn append(path: &Path, text: &str) {
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(path).expect("open");
        file.write_all(text.as_bytes()).expect("write");
    }

    fn prompt(uuid: &str, parent: Option<&str>, text: &str) -> String {
        let record = serde_json::json!({
            "type": "user", "uuid": uuid, "parentUuid": parent,
            "timestamp": "2026-09-27T03:15:25.849Z",
            "message": { "role": "user", "content": text },
        });
        format!("{record}\n")
    }

    fn subagent(uuid: &str, text: &str) -> String {
        let record = serde_json::json!({
            "type": "assistant", "uuid": uuid, "parentUuid": null, "isSidechain": true,
            "agentId": "a1", "timestamp": "2026-09-27T03:15:26.000Z",
            "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
        });
        format!("{record}\n")
    }

    fn ids(changes: &[Change]) -> Vec<String> {
        changes
            .iter()
            .filter(|change| !matches!(change, Change::Turn { .. }))
            .map(|change| match change {
                Change::Upsert { thread, entry } => format!("{thread:?} {}", entry.id),
                Change::Remove { thread, id } => format!("{thread:?} -{id}"),
                Change::Tasks { thread, .. } => format!("{thread:?} tasks"),
                Change::Reset { thread } => format!("reset {thread:?}"),
                Change::Turn { .. } => String::new(),
            })
            .collect()
    }

    /// The first read is everything there is behind a reset; later reads bring only what was
    /// appended, subagent files found in their directory included.
    #[test]
    fn the_first_read_is_the_whole_session_then_only_what_grows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        append(&main, &prompt("u1", None, "hello"));
        let mut transcripts = Transcripts::default();
        assert_eq!(ids(&transcripts.read(&main, &[])), ["reset None", "Main u1"]);
        assert!(transcripts.read(&main, &[]).is_empty(), "nothing new");

        std::fs::create_dir_all(subagents_dir(&main)).expect("mkdir");
        append(&subagents_dir(&main).join("agent-a1.jsonl"), &subagent("s1", "found it"));
        append(&main, &prompt("u2", Some("u1"), "and then"));
        assert_eq!(
            ids(&transcripts.read(&main, &[])),
            ["Main u2", "Agent(\"a1\") s1:0"],
            "the main file first, then the subagent"
        );
    }

    /// `/clear` moves the agent to a new file: the changes reset every thread and bring the new
    /// file whole. A file the hooks name outside the directory is read too.
    #[test]
    fn another_transcript_starts_over() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (first, second) = (dir.path().join("s1.jsonl"), dir.path().join("s2.jsonl"));
        append(&first, &prompt("u1", None, "one"));
        append(&second, &prompt("v1", None, "two"));
        let elsewhere = dir.path().join("elsewhere").join("subagents").join("agent-a1.jsonl");
        std::fs::create_dir_all(elsewhere.parent().expect("dir")).expect("mkdir");
        append(&elsewhere, &subagent("s1", "named by a hook"));

        let mut transcripts = Transcripts::default();
        assert_eq!(ids(&transcripts.read(&first, &[])), ["reset None", "Main u1"]);
        assert_eq!(
            ids(&transcripts.read(&second, std::slice::from_ref(&elsewhere))),
            ["reset None", "Main v1", "Agent(\"a1\") s1:0"]
        );
        assert_eq!(transcripts.conversation().entries(&ThreadId::Main).len(), 1);
    }

    /// A main file rewritten shorter drops every thread in the decoder, so the subagents'
    /// files are read again from their top in the same read.
    #[test]
    fn a_rewritten_main_file_reads_the_subagents_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        append(&main, &prompt("u1", None, "a long first prompt"));
        std::fs::create_dir_all(subagents_dir(&main)).expect("mkdir");
        append(&subagents_dir(&main).join("agent-a1.jsonl"), &subagent("s1", "sub"));
        let mut transcripts = Transcripts::default();
        assert_eq!(ids(&transcripts.read(&main, &[])).len(), 3);

        std::fs::write(&main, prompt("w1", None, "short")).expect("rewrite");
        assert_eq!(
            ids(&transcripts.read(&main, &[])),
            ["reset None", "Main w1", "Agent(\"a1\") s1:0"]
        );
    }

    /// The whole of a clipped text is read from its record's own line, where the reads that
    /// brought it placed it, a line read in two parts among them: the rest of the file is never
    /// read, so the cost does not grow with the transcript. A record not read yet is not found.
    #[test]
    fn a_clipped_text_is_read_from_its_line_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        let long = "x".repeat(4_000);
        append(&main, &prompt("u1", None, "first"));
        let second = prompt("u2", Some("u1"), &long);
        let (head, rest) = second.split_at(100);
        append(&main, head);
        let mut transcripts = Transcripts::default();
        let _read = transcripts.read(&main, &[]);
        let reference = TextRef { record: "u2".to_owned(), part: Part::Block { index: 0 } };
        assert_eq!(transcripts.full_text(&ThreadId::Main, &reference), None, "not read yet");
        append(&main, rest);
        append(&main, &prompt("u3", Some("u2"), "third"));
        let _read = transcripts.read(&main, &[]);
        let located = transcripts.locate(&ThreadId::Main, &reference);
        assert_eq!(located.text().as_deref(), Some(long.as_str()));
        // Every other byte of the file spoiled, the same length: the line alone is read.
        let text = std::fs::read_to_string(&main).expect("read");
        let at = text.find(second.as_str()).expect("the line");
        let spoiled: String = text
            .char_indices()
            .map(|(i, c)| if (at..at + second.len()).contains(&i) || c == '\n' { c } else { '#' })
            .collect();
        std::fs::write(&main, spoiled).expect("spoil");
        assert_eq!(located.text().as_deref(), Some(long.as_str()), "read from its line");
        let first = TextRef { record: "u1".to_owned(), part: Part::Block { index: 0 } };
        assert_eq!(transcripts.full_text(&ThreadId::Main, &first), None, "its line is spoiled");
    }

    /// What reading the whole of one clipped text costs from a long transcript: from its indexed
    /// line, and as it was read before, the whole file read and searched. Run with
    /// `cargo test -p slopty-agent --release --lib expand_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run by hand"]
    fn expand_cost() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        let records = 20_000_u32;
        let mut text = String::new();
        for n in 0..records {
            let parent = n.checked_sub(1).map(|p| format!("u{p}"));
            text.push_str(&prompt(&format!("u{n}"), parent.as_deref(), &"word ".repeat(400)));
        }
        std::fs::write(&main, &text).expect("write");
        let mut transcripts = Transcripts::default();
        let started = std::time::Instant::now();
        let _read = transcripts.read(&main, &[]);
        let first_read = started.elapsed();
        let reference = TextRef { record: "u100".to_owned(), part: Part::Block { index: 0 } };
        let rounds = 50_u32;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            assert!(transcripts.locate(&ThreadId::Main, &reference).text().is_some());
        }
        let indexed = started.elapsed() / rounds;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let whole = std::fs::read_to_string(&main).expect("read");
            assert!(full_text(&whole, &reference).is_some());
        }
        let whole = started.elapsed() / rounds;
        let lines = Lines { restarted: false, start: 0, text: text.clone() };
        let started = std::time::Instant::now();
        Index::default().note(&main, &lines);
        let noting = started.elapsed();
        eprintln!(
            "expand_cost: {records} records, {} MiB; first read {} ms, the index's part {} ms; \
             one text from its line {} us, from the whole file {} us",
            text.len() / (1024 * 1024),
            first_read.as_millis(),
            noting.as_millis(),
            indexed.as_micros(),
            whole.as_micros()
        );
    }

    /// A follower's reads bring a background command's output as it grows, and asked for, its
    /// whole output and a picture's bytes come from where they are.
    #[test]
    fn outputs_and_pictures_are_found_for_a_follower() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        let tasks = dir.path().join("tasks");
        std::fs::create_dir_all(&tasks).expect("mkdir");
        let out = tasks.join("b1.output");
        let bytes = b"GIF89a\x02\x00\x03\x00".to_vec();
        let data = data_encoding::BASE64.encode(&bytes);
        let records = [
            serde_json::json!({
                "type": "user", "uuid": "u1", "message": { "content": [
                    { "type": "image", "source": {
                        "type": "base64", "media_type": "image/gif", "data": data } },
                    { "type": "text", "text": "run it" },
                ]},
            }),
            serde_json::json!({
                "type": "assistant", "uuid": "a1", "message": { "content": [{
                    "type": "tool_use", "id": "t1", "name": "Bash",
                    "input": { "command": "make", "run_in_background": true } }] },
            }),
            serde_json::json!({
                "type": "user", "uuid": "r1", "message": { "content": [{
                    "type": "tool_result", "tool_use_id": "t1",
                    "content": format!("Output is being written to: {}.", out.display()) }] },
                "toolUseResult": { "backgroundTaskId": "b1" },
            }),
        ];
        append(
            &main,
            &records.iter().map(|r| [r.to_string(), "\n".to_owned()].concat()).collect::<String>(),
        );
        append(&out, "cc -c a.c\n");
        let mut transcripts = Transcripts::default();
        transcripts.read(&main, &[]);
        let outputs = transcripts.outputs();
        assert_eq!(outputs.iter().map(|o| o.tail.text.as_str()).collect::<Vec<_>>(), ["cc -c a.c"]);
        assert!(transcripts.outputs().is_empty(), "nothing new");
        append(&out, "cc -c b.c\n");
        let reference = TextRef {
            record: "t1".to_owned(),
            part: Part::Output { tool_use_id: "t1".to_owned() },
        };
        assert_eq!(
            transcripts.full_text(&ThreadId::Main, &reference).as_deref(),
            Some("cc -c a.c\ncc -c b.c\n")
        );
        let picture =
            TextRef { record: "u1".to_owned(), part: Part::Image { tool_use_id: None, index: 0 } };
        assert_eq!(transcripts.image(&ThreadId::Main, &picture), Some(bytes));
        let missing = TextRef { record: "u9".to_owned(), part: picture.part };
        assert_eq!(transcripts.image(&ThreadId::Main, &missing), None);
    }

    /// A clipped subagent text is found in the subagent's own file.
    #[test]
    fn a_clipped_text_is_found_in_its_threads_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        append(&main, &prompt("u1", None, "hi"));
        std::fs::create_dir_all(subagents_dir(&main)).expect("mkdir");
        let long = "line\n".repeat(1_000);
        append(&subagents_dir(&main).join("agent-a1.jsonl"), &subagent("s1", &long));
        let mut transcripts = Transcripts::default();
        let changes = transcripts.read(&main, &[]);
        let clipped = changes.iter().find_map(|change| match change {
            Change::Upsert { entry, .. } => match &entry.body {
                Body::Text(text) => text.full.clone(),
                _ => None,
            },
            _ => None,
        });
        let reference = clipped.expect("the long answer is clipped");
        assert_eq!(reference, TextRef { record: "s1".to_owned(), part: Part::Block { index: 0 } });
        let thread = ThreadId::Agent("a1".to_owned());
        assert_eq!(transcripts.full_text(&thread, &reference), Some(long));
        assert_eq!(transcripts.full_text(&ThreadId::Main, &reference), None);
    }
}
