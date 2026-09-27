//! The conversation decoder over transcripts captured from a real Claude Code
//! (`tests/fixtures/conversation`, written by `cargo xtask fixtures claude`; the records say
//! which version). Each scenario is decoded whole and pinned by a snapshot; a snapshot that
//! moves after a recapture is the transcript format moving under us.

#[cfg(test)]
mod conversation {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use slopty_agent::conversation::{Change, Conversation, Entry, ThreadId, Turn};
    use slopty_agent::transcript::Tail;

    fn scenario(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conversation").join(name)
    }

    /// The scenario's files, the main transcript first.
    fn files(name: &str) -> Vec<PathBuf> {
        let dir = scenario(name);
        let mut files = vec![dir.join("transcript.jsonl")];
        if let Ok(agents) = std::fs::read_dir(dir.join("subagents")) {
            let mut agents: Vec<PathBuf> = agents.map(|e| e.expect("entry").path()).collect();
            agents.sort();
            files.extend(agents);
        }
        files
    }

    fn decode(name: &str) -> Conversation {
        let mut conversation = Conversation::default();
        for file in files(name) {
            conversation.read(&mut Tail::default(), &file).expect("read");
        }
        conversation
    }

    /// A client's copy of a thread: its entries and its turns.
    #[derive(Default)]
    struct Copy {
        entries: Vec<Entry>,
        turns: Vec<Turn>,
    }

    /// A client's copy, kept from nothing but the changes.
    fn apply(copy: &mut BTreeMap<ThreadId, Copy>, changes: Vec<Change>) {
        for change in changes {
            match change {
                Change::Upsert { thread, entry } => {
                    let list = &mut copy.entry(thread).or_default().entries;
                    match list.iter_mut().find(|e| e.id == entry.id) {
                        Some(old) => *old = entry,
                        None => list.push(entry),
                    }
                }
                Change::Remove { thread, id } => {
                    let held = copy.entry(thread).or_default();
                    held.entries.retain(|e| e.id != id);
                    held.turns.retain(|t| t.prompt != id);
                }
                Change::Reset { thread: Some(thread) } => {
                    copy.remove(&thread);
                }
                Change::Reset { thread: None } => copy.clear(),
                Change::Tasks { .. } => {}
                Change::Turn { thread, turn } => {
                    let turns = &mut copy.entry(thread).or_default().turns;
                    match turns.iter_mut().find(|t| t.prompt == turn.prompt) {
                        Some(old) => *old = turn,
                        None => turns.push(turn),
                    }
                }
            }
        }
    }

    const SCENARIOS: [&str; 5] = ["edit", "tools", "interrupt", "compact", "permission"];

    #[test]
    fn edit() {
        insta::assert_yaml_snapshot!(decode("edit").snapshot());
    }

    #[test]
    fn tools() {
        insta::assert_yaml_snapshot!(decode("tools").snapshot());
    }

    #[test]
    fn interrupt() {
        insta::assert_yaml_snapshot!(decode("interrupt").snapshot());
    }

    #[test]
    fn compact() {
        insta::assert_yaml_snapshot!(decode("compact").snapshot());
    }

    #[test]
    fn permission() {
        insta::assert_yaml_snapshot!(decode("permission").snapshot());
    }

    /// Fed a line at a time, main and subagent files interleaved, the decoder ends where a whole
    /// read does, and a client that applies only the changes holds the same entries.
    #[test]
    fn a_line_at_a_time_ends_where_a_whole_read_does() {
        for name in SCENARIOS {
            let whole = decode(name);
            let mut conversation = Conversation::default();
            let mut copy = BTreeMap::new();
            let dir = tempfile::tempdir().expect("tempdir");
            let mut sources: Vec<(Vec<String>, PathBuf, Tail)> = files(name)
                .into_iter()
                .map(|file| {
                    let text = std::fs::read_to_string(&file).expect("read");
                    let relative = file.strip_prefix(scenario(name)).expect("inside").to_path_buf();
                    let growing = dir.path().join(relative);
                    std::fs::create_dir_all(growing.parent().expect("dir")).expect("mkdir");
                    std::fs::write(&growing, "").expect("create");
                    let lines = text.split_inclusive('\n').map(str::to_owned).rev().collect();
                    (lines, growing, Tail::default())
                })
                .collect();
            while sources.iter().any(|(lines, ..)| !lines.is_empty()) {
                for (lines, path, tail) in &mut sources {
                    let Some(line) = lines.pop() else { continue };
                    // Half a line first: the tail must hold it until the rest is written.
                    let parts: [&str; 2] = line.split_at(line.len() / 2).into();
                    for part in parts {
                        let mut text = std::fs::read_to_string(&*path).expect("read");
                        text.push_str(part);
                        std::fs::write(&*path, text).expect("append");
                        apply(&mut copy, conversation.read(tail, path).expect("read"));
                    }
                }
            }
            assert_eq!(conversation.snapshot(), whole.snapshot(), "{name}");
            for thread in whole.snapshot() {
                let held = copy.get(&thread.id);
                assert_eq!(
                    held.map(|c| c.entries.as_slice()),
                    Some(thread.entries.as_slice()),
                    "{name}"
                );
                assert_eq!(
                    held.map(|c| c.turns.as_slice()),
                    Some(thread.turns.as_slice()),
                    "{name}"
                );
            }
        }
    }
}
