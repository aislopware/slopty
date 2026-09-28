//! A session's transcript files, read together as they grow.
//!
//! Claude Code writes a session's conversation to `<id>.jsonl` and each subagent's to
//! `<id>/subagents/agent-<agent id>.jsonl` beside it. [`Transcripts`] reads the main file first
//! and then every subagent file it finds there or is told of (the hooks name each subagent's
//! file as it stops), feeding one [`Conversation`], so a follower gets one stream of
//! [`Change`]s for all the threads. Beside them it tails the files background commands write
//! ([`Outputs`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::output::{self, Outputs};
use super::{
    Change, Conversation, Output, Part, TextRef, ThreadId, full_text_at, image_bytes, thread_of,
};
use crate::transcript::Tail;

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
            && let Ok(read) = self.conversation.read(tail, path)
        {
            if read.iter().any(|change| matches!(change, Change::Reset { thread: None })) {
                // The decoder dropped every thread: the subagents' files are read again too,
                // and every output is sent again.
                self.subagents.values_mut().for_each(|tail| *tail = Tail::default());
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
            if let Ok(read) = self.conversation.read(tail, path) {
                changes.extend(read);
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

    /// The whole of a clipped text of `thread`: from the subagent's own file, else from the
    /// main one (older versions wrote subagents there). `None` when neither has it.
    #[must_use]
    /// A background command's output is read from the end of its file, up to
    /// [`output::WHOLE`].
    pub fn full_text(&self, thread: &ThreadId, reference: &TextRef) -> Option<String> {
        if let Part::Output { tool_use_id } = &reference.part {
            let file = output::file_of(&self.conversation, thread, tool_use_id)?;
            return output::read_end(file, output::WHOLE).ok();
        }
        self.files(thread).find_map(|path| full_text_at(path, reference).ok().flatten())
    }

    /// The bytes of the picture `reference` names in `thread`; `None` when neither file has
    /// it, or it is too large to send.
    pub fn image(&self, thread: &ThreadId, reference: &TextRef) -> Option<Vec<u8>> {
        self.files(thread).find_map(|path| {
            let jsonl = std::fs::read_to_string(path).ok()?;
            image_bytes(&jsonl, reference)
        })
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
