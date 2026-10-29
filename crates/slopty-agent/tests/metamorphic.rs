//! Metamorphic relations of the transcript decoder, over the transcripts captured from a real
//! Claude Code (`tests/fixtures/conversation`).
//!
//! No oracle says what a transcript decodes to (the snapshots in `conversation.rs` pin that).
//! These hold the decoder to itself instead. Each relation changes how the files arrive, or
//! adds what Claude Code may write and the decoder does not read, and requires the same
//! decode:
//! - **arrival:** the files grow in pieces of any size, cut inside lines and characters, read in
//!   any order and as often as a watcher wakes, subagents before the main file too;
//! - **restart:** a file rewritten shorter and grown again ends as a fresh read of it;
//! - **records:** a record of a type no Claude Code writes, as bookkeeping or as a link in the
//!   chain, changes nothing;
//! - **fields:** a field no Claude Code writes, in a record, its message or a content block,
//!   changes nothing.
//!
//! Every relation runs on every scenario there is, so a new capture is held by them all.

#[cfg(test)]
mod metamorphic {
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};
    use slopty_agent::conversation::{Change, Entry, ThreadId, ThreadState, Transcripts, Turn};

    /// The session id the files are written under, as Claude Code names them.
    const SESSION: &str = "00000000-0000-4000-8000-0000000000e0";

    /// The type of record, and the field, no Claude Code writes.
    const UNKNOWN: &str = "sloptyMetamorphic";

    /// Every captured scenario's directory.
    fn scenarios() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conversation");
        let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("the captured scenarios")
            .map(|entry| entry.expect("an entry").path())
            .filter(|path| path.join("transcript.jsonl").is_file())
            .collect();
        found.sort();
        assert!(found.len() >= 6, "the scenarios under {}: {found:?}", root.display());
        found
    }

    /// A scenario's files: where each goes under a session's directory (the main transcript
    /// first, then each subagent's), and its text.
    fn files(scenario: &Path) -> Vec<(PathBuf, String)> {
        let read = |path: &Path| std::fs::read_to_string(path).expect("a captured file");
        let mut files = vec![(
            PathBuf::from(format!("{SESSION}.jsonl")),
            read(&scenario.join("transcript.jsonl")),
        )];
        if let Ok(agents) = std::fs::read_dir(scenario.join("subagents")) {
            let mut agents: Vec<PathBuf> = agents.map(|e| e.expect("an entry").path()).collect();
            agents.sort();
            for agent in agents {
                let name = agent.file_name().expect("a file name");
                let at = Path::new(SESSION).join("subagents").join(name);
                files.push((at, read(&agent)));
            }
        }
        files
    }

    /// A client's copy of the threads, kept from nothing but the changes.
    #[derive(Debug, Default, PartialEq)]
    struct Copy {
        threads: BTreeMap<ThreadId, (Vec<Entry>, Vec<Turn>)>,
    }

    impl Copy {
        fn apply(&mut self, changes: Vec<Change>) {
            for change in changes {
                match change {
                    Change::Upsert { thread, entry } => {
                        let entries = &mut self.threads.entry(thread).or_default().0;
                        match entries.iter_mut().find(|e| e.id == entry.id) {
                            Some(old) => *old = entry,
                            None => entries.push(entry),
                        }
                    }
                    Change::Remove { thread, id } => {
                        let (entries, turns) = self.threads.entry(thread).or_default();
                        entries.retain(|e| e.id != id);
                        turns.retain(|t| t.prompt != id);
                    }
                    Change::Reset { thread: Some(thread) } => {
                        self.threads.remove(&thread);
                    }
                    Change::Reset { thread: None } => self.threads.clear(),
                    other => self.turn(other),
                }
            }
        }

        /// A turn begun or gone on. What else a change says (the task list, what waits in the
        /// TUI's queue) is neither an entry nor a turn.
        fn turn(&mut self, change: Change) {
            if let Change::Turn { thread, turn } = change {
                let turns = &mut self.threads.entry(thread).or_default().1;
                match turns.iter_mut().find(|t| t.prompt == turn.prompt) {
                    Some(old) => *old = turn,
                    None => turns.push(turn),
                }
            }
        }

        /// The copy holds every thread's entries and turns as `snapshot` has them.
        fn holds(&self, snapshot: &[ThreadState], what: &str) {
            for thread in snapshot {
                let held = self.threads.get(&thread.id).map(|(e, t)| (e.as_slice(), t.as_slice()));
                let want = (thread.entries.as_slice(), thread.turns.as_slice());
                assert_eq!(held.unwrap_or_default(), want, "{what}: a client's {:?}", thread.id);
            }
        }
    }

    /// A session's directory being written, and the reader that follows it as the worker does.
    struct Session {
        dir: tempfile::TempDir,
        transcripts: Transcripts,
        copy: Copy,
    }

    impl Session {
        fn new(files: &[(PathBuf, String)]) -> Self {
            let dir = tempfile::tempdir().expect("a session directory");
            for (at, _) in files {
                let path = dir.path().join(at);
                std::fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
                std::fs::write(&path, "").expect("an empty file");
            }
            Self { dir, transcripts: Transcripts::default(), copy: Copy::default() }
        }

        fn path(&self, at: &Path) -> PathBuf {
            self.dir.path().join(at)
        }

        fn append(&self, at: &Path, bytes: &[u8]) {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(self.path(at))
                .expect("a file to append to");
            file.write_all(bytes).expect("an append");
        }

        fn replace(&self, at: &Path, bytes: &[u8]) {
            std::fs::write(self.path(at), bytes).expect("a rewrite");
        }

        /// A watcher woke: everything the files gained since is read.
        fn read(&mut self) {
            let main = self.path(Path::new(&format!("{SESSION}.jsonl")));
            let changes = self.transcripts.read(&main, &[]);
            self.copy.apply(changes);
        }

        fn snapshot(&self) -> Vec<ThreadState> {
            self.transcripts.conversation().snapshot()
        }
    }

    /// `files` written whole and read once.
    fn decoded(files: &[(PathBuf, String)]) -> Vec<ThreadState> {
        let mut session = Session::new(files);
        for (at, text) in files {
            session.append(at, text.as_bytes());
        }
        session.read();
        session.snapshot()
    }

    /// A small seeded generator (SplitMix64): the same cuts every run.
    struct Cuts(u64);

    impl Cuts {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        /// A number in `1..=most`.
        fn upto(&mut self, most: usize) -> usize {
            let most = u64::try_from(most.max(1)).expect("fits");
            usize::try_from(self.next().checked_rem(most).expect("not zero"))
                .expect("fits")
                .saturating_add(1)
        }
    }

    /// However the files arrive, they decode as one whole read does, and a client that applies
    /// only the changes holds what the decoder holds. Each seed cuts the files at places of its
    /// own (a few bytes at a time now and then, inside a character too) and appends the pieces
    /// in an order of its own, the watcher waking after some appends and not others; the first
    /// seed writes every subagent's file whole before the main one.
    #[test]
    fn however_the_files_arrive_they_decode_the_same() {
        /// The pieces a file is cut into, about: enough to cut most records, few enough that the
        /// reads stay cheap.
        const PIECES: usize = 48;
        for scenario in scenarios() {
            let files = files(&scenario);
            let whole = decoded(&files);
            for seed in 0..6_u64 {
                let what = format!("{}, seed {seed}", scenario.display());
                let mut cuts = Cuts(seed);
                let mut session = Session::new(&files);
                let mut left: Vec<(&Path, &[u8])> =
                    files.iter().map(|(at, text)| (at.as_path(), text.as_bytes())).collect();
                if seed == 0 {
                    left.rotate_left(1);
                }
                loop {
                    let open: Vec<usize> =
                        (0..left.len()).filter(|n| !left[*n].1.is_empty()).collect();
                    let Some(&first) = open.first() else { break };
                    let (pick, size) = if seed == 0 {
                        (first, usize::MAX)
                    } else {
                        let pick = open[cuts.upto(open.len()).saturating_sub(1)];
                        let len = files[pick].1.len();
                        let size =
                            if cuts.upto(6) == 1 { cuts.upto(3) } else { cuts.upto(len / PIECES) };
                        (pick, size)
                    };
                    let (at, rest) = left[pick];
                    let (piece, rest) = rest.split_at(size.min(rest.len()));
                    session.append(at, piece);
                    left[pick].1 = rest;
                    if cuts.upto(3) != 1 {
                        session.read();
                    }
                }
                session.read();
                session.read();
                let snapshot = session.snapshot();
                assert_eq!(snapshot, whole, "{what}");
                session.copy.holds(&snapshot, &what);
            }
        }
    }

    /// A file rewritten shorter (Claude Code rewrote it, or a new session reuses the path) and
    /// grown again to the whole ends as a fresh read of the whole: the main file, which starts
    /// every thread over, and a subagent's, which starts its own.
    #[test]
    fn a_file_rewritten_and_grown_again_ends_as_a_fresh_read() {
        for scenario in scenarios() {
            let files = files(&scenario);
            let whole = decoded(&files);
            for (at, text) in &files {
                let what = format!("{}, {} rewritten", scenario.display(), at.display());
                let mut session = Session::new(&files);
                for (at, text) in &files {
                    session.append(at, text.as_bytes());
                }
                session.read();
                let lines: Vec<&str> = text.split_inclusive('\n').collect();
                let kept: String = lines[..lines.len() / 2].concat();
                session.replace(at, kept.as_bytes());
                session.read();
                assert_ne!(session.snapshot(), whole, "{what}: half of it read");
                session.replace(at, text.as_bytes());
                session.read();
                let snapshot = session.snapshot();
                assert_eq!(snapshot, whole, "{what}");
                session.copy.holds(&snapshot, &what);
            }
        }
    }

    /// Each file's records changed by `change`, written back as Claude Code writes them.
    fn rewrite(
        files: &[(PathBuf, String)],
        mut change: impl FnMut(Vec<Value>) -> Vec<Value>,
    ) -> Vec<(PathBuf, String)> {
        files
            .iter()
            .map(|(at, text)| {
                let records: Vec<Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("a captured record"))
                    .collect();
                let text: String = change(records)
                    .iter()
                    .map(|r| [r.to_string(), "\n".to_owned()].concat())
                    .collect();
                (at.clone(), text)
            })
            .collect()
    }

    /// A record of a type no Claude Code writes changes nothing: one with nothing to say of the
    /// chain after every record, and one that is a link in it, its parent the record before and
    /// its child the record after, wherever the chain runs from one record to the next.
    #[test]
    fn a_record_no_claude_code_writes_changes_nothing() {
        for scenario in scenarios() {
            let files = files(&scenario);
            let whole = decoded(&files);
            let aside = rewrite(&files, |records| {
                records
                    .into_iter()
                    .flat_map(|record| [record, json!({ "type": UNKNOWN, "said": "aside" })])
                    .collect()
            });
            assert_eq!(decoded(&aside), whole, "{}: a record aside", scenario.display());

            let mut links = 0_u32;
            let linked = rewrite(&files, |records| {
                let mut out: Vec<Value> = Vec::new();
                let mut last: Option<Value> = None;
                for mut record in records {
                    let parent = record.get("parentUuid").and_then(Value::as_str);
                    let before = last.as_ref().and_then(|l| l.get("uuid")).and_then(Value::as_str);
                    if let (Some(parent), Some(before), Some(last)) = (parent, before, &last)
                        && parent == before
                    {
                        links = links.saturating_add(1);
                        let uuid = format!("00000000-0000-4000-8000-{links:012x}");
                        let mut link = json!({
                            "type": UNKNOWN,
                            "uuid": uuid,
                            "parentUuid": parent,
                        });
                        for key in ["isSidechain", "agentId", "sessionId", "timestamp", "cwd"] {
                            if let Some(value) = last.get(key) {
                                link[key] = value.clone();
                            }
                        }
                        out.push(link);
                        record["parentUuid"] = Value::String(uuid);
                    }
                    if record.get("uuid").is_some() {
                        last = Some(record.clone());
                    }
                    out.push(record);
                }
                out
            });
            assert!(links > 0, "{}: a chain to link into", scenario.display());
            assert_eq!(decoded(&linked), whole, "{}: {links} links", scenario.display());
        }
    }

    /// A field no Claude Code writes changes nothing: in every record, in its message and in
    /// each of the message's content blocks.
    #[test]
    fn a_field_no_claude_code_writes_changes_nothing() {
        let add = |value: &mut Value| {
            if let Some(fields) = value.as_object_mut() {
                fields.insert(UNKNOWN.to_owned(), json!({ "said": [1, "two", null] }));
            }
        };
        for scenario in scenarios() {
            let files = files(&scenario);
            let whole = decoded(&files);
            let widened = rewrite(&files, |mut records| {
                for record in &mut records {
                    add(record);
                    if let Some(message) = record.get_mut("message") {
                        add(message);
                        if let Some(blocks) =
                            message.get_mut("content").and_then(Value::as_array_mut)
                        {
                            blocks.iter_mut().for_each(add);
                        }
                    }
                }
                records
            });
            assert_eq!(decoded(&widened), whole, "{}", scenario.display());
        }
    }
}
