//! A Claude Code transcript through the conversation decoder (`slopty_agent::conversation`).
//!
//! The transcript is JSONL that Claude Code writes and calls internal; its text is whatever the
//! model, the person and every tool said, so any byte can be in it. The decoder runs on the
//! worker over the file as it grows ([`slopty_agent::transcript::Tail`] hands it the complete
//! lines appended since the last read), and the thread model a session's record is read into
//! again ([`slopty_agent::claude::history::replay`]) takes the same records.
//!
//! The first byte picks how many lines one read hands over (1 to 16); the rest is the session's
//! files, the main transcript first. A line that starts with `\x01` names the file the lines
//! after it are appended to: the agent id after it is that subagent's file
//! (`<session>/subagents/agent-<id>.jsonl`), nothing after it the main one.
//!
//! What must hold, beside no panic:
//! - **reads:** the same lines handed over a few at a time or in runs as long as they come decode
//!   to the same conversation, and a client that applies nothing but the changes each read returns
//!   holds, after every read, the entries and turns of every thread the decoder holds;
//! - **unknown fields:** a field no Claude Code writes, put in every record, changes nothing (the
//!   decoder's own rule: it passes over what it does not know);
//! - **clipping:** no text it makes is longer than [`PROSE`], and a clipped text's reference
//!   ([`full_text`]) finds a whole text as long as the clip says, when one record bears its id;
//! - **heap:** a decode takes no more heap than its bytes bound.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_agent::conversation::{
    Change, Conversation, Entry, PROSE, ThreadId, ThreadState, Turn, full_text,
};
use slopty_agent::transcript::Lines;
use slopty_testkit::alloc;

/// The session every input's files belong to.
const SESSION: &str = "00000000-0000-4000-8000-0000000000f0";

/// The byte that starts a line naming the file the lines after it go to.
pub const FILE_MARK: char = '\u{1}';

/// The field no Claude Code writes, put in every record for the unknown-field relation.
const UNKNOWN: &str = "sloptyFuzzUnknown";

/// Heap bytes a decode may take per byte of the transcript: each record parsed into a
/// [`Value`], the entries made of it, and a client's copy of them, each a small multiple.
const HEAP_PER_BYTE: u64 = 512;

/// Heap bytes any decode may take, whatever its size.
const HEAP_SLACK: u64 = 4 << 20;

/// `files` as an input: the read size byte, then each file's text after the line that names
/// it (`None` for the main transcript, else the subagent's id).
#[must_use]
pub fn input(per_read: u8, files: &[(Option<&str>, &str)]) -> Vec<u8> {
    let mut out = vec![per_read];
    for (agent, text) in files {
        if let Some(id) = agent {
            out.extend(format!("{FILE_MARK}{id}\n").into_bytes());
        } else if out.len() > 1 {
            out.extend(format!("{FILE_MARK}\n").into_bytes());
        }
        out.extend_from_slice(text.as_bytes());
        if !text.ends_with('\n') {
            out.push(b'\n');
        }
    }
    out
}

/// Run one input.
pub fn run(data: &[u8]) {
    let Some((&per_read, rest)) = data.split_first() else { return };
    let text = String::from_utf8_lossy(rest);
    let lines = lines_of(&text);
    let per_read = usize::from(per_read % 16).saturating_add(1);
    let len = u64::try_from(rest.len()).unwrap_or(u64::MAX);
    let bound = HEAP_PER_BYTE.saturating_mul(len).saturating_add(HEAP_SLACK);

    let (runs, used) = alloc::measure(|| decode(&lines, usize::MAX, false));
    assert!(used.bytes <= bound, "{len} transcript bytes took {} heap bytes", used.bytes);
    let few = decode(&lines, per_read, true);
    assert_eq!(few, runs, "the lines read a few at a time decoded otherwise than in runs");

    let unknown: Vec<Line> = lines.iter().map(Line::with_unknown_field).collect();
    let marked = decode(&unknown, usize::MAX, false);
    assert_eq!(marked, runs, "a field no Claude Code writes changed the conversation");

    clipped_within_caps(&runs, &text);
    let records: Vec<Value> =
        lines.iter().filter_map(|l| serde_json::from_str(&l.text).ok()).collect();
    let _thread = slopty_agent::claude::history::replay(&records);
}

/// One line of a session's files: the file it is appended to and its text, newline included.
#[derive(Clone)]
struct Line {
    file: Option<String>,
    text: String,
}

impl Line {
    /// The line with [`UNKNOWN`] in its record, when it is one.
    fn with_unknown_field(&self) -> Self {
        let Ok(Value::Object(mut record)) = serde_json::from_str::<Value>(&self.text) else {
            return self.clone();
        };
        if record.contains_key(UNKNOWN) {
            return self.clone();
        }
        record.insert(UNKNOWN.to_owned(), Value::Bool(true));
        let mut text = Value::Object(record).to_string();
        text.push('\n');
        Self { file: self.file.clone(), text }
    }
}

/// The input's lines, each with the file it belongs to; a line naming a file is not one.
fn lines_of(text: &str) -> Vec<Line> {
    let mut file = None;
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        if let Some(named) = line.strip_prefix(FILE_MARK) {
            let id = named.trim_end_matches(['\n', '\r']);
            file = (!id.is_empty()).then(|| id.to_owned());
            continue;
        }
        let mut text = line.to_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        out.push(Line { file: file.clone(), text });
    }
    out
}

/// Where a session's file is: the main transcript, or a subagent's beside it.
fn path_of(file: Option<&str>) -> PathBuf {
    let dir = Path::new("/home/user/.claude/projects/-work");
    file.map_or_else(
        || dir.join(format!("{SESSION}.jsonl")),
        |id| dir.join(SESSION).join("subagents").join(format!("agent-{id}.jsonl")),
    )
}

/// The conversation `lines` decode to, handed over as a tail hands them: at most `per_read`
/// lines of one file in a read, and a read ending where the next line is another file's.
/// `check` holds a client applying the changes to the decoder's state after every read.
fn decode(lines: &[Line], per_read: usize, check: bool) -> Vec<ThreadState> {
    let mut conversation = Conversation::default();
    let mut client = Client::default();
    let mut offsets: HashMap<Option<String>, u64> = HashMap::new();
    let mut at = 0_usize;
    while let Some(first) = lines.get(at) {
        let run = lines
            .get(at..)
            .unwrap_or_default()
            .iter()
            .take(per_read)
            .take_while(|l| l.file == first.file)
            .count();
        let read = lines.get(at..at.saturating_add(run)).unwrap_or_default();
        at = at.saturating_add(run.max(1));
        let text: String = read.iter().map(|l| l.text.as_str()).collect();
        let offset = offsets.entry(first.file.clone()).or_default();
        let start = *offset;
        *offset = offset.saturating_add(u64::try_from(text.len()).unwrap_or(u64::MAX));
        let path = path_of(first.file.as_deref());
        let changes = conversation.take_lines(&path, &Lines { restarted: false, start, text });
        if check {
            client.apply(changes);
            client.holds(&conversation.snapshot());
        }
    }
    conversation.snapshot()
}

/// A client's copy of every thread, kept from nothing but the changes.
#[derive(Default)]
struct Client {
    threads: BTreeMap<ThreadId, (Vec<Entry>, Vec<Turn>)>,
}

impl Client {
    fn apply(&mut self, changes: Vec<Change>) {
        for change in changes {
            match change {
                Change::Upsert { thread, entry } => {
                    let entries = &mut self.threads.entry(thread).or_default().0;
                    match entries.iter_mut().find(|e| e.id == entry.id) {
                        Some(held) => *held = entry,
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
                Change::Turn { thread, turn } => {
                    let turns = &mut self.threads.entry(thread).or_default().1;
                    match turns.iter_mut().find(|t| t.prompt == turn.prompt) {
                        Some(held) => *held = turn,
                        None => turns.push(turn),
                    }
                }
                _ => {}
            }
        }
    }

    /// The copy holds what the decoder holds, thread by thread.
    fn holds(&self, snapshot: &[ThreadState]) {
        for thread in snapshot {
            let (entries, turns) =
                self.threads.get(&thread.id).map_or((&[][..], &[][..]), |(e, t)| (e, t));
            assert_eq!(entries, thread.entries.as_slice(), "a client's entries of {:?}", thread.id);
            assert_eq!(turns, thread.turns.as_slice(), "a client's turns of {:?}", thread.id);
        }
        for (id, (entries, _)) in &self.threads {
            let known = snapshot.iter().any(|t| &t.id == id);
            assert!(known || entries.is_empty(), "a client holds entries of {id:?}, gone");
        }
    }
}

/// Every clipped text in `snapshot` (an object with `text`, `lines`, `chars` and `full`) is
/// within the largest cap, and one with a reference finds its whole in `jsonl`.
fn clipped_within_caps(snapshot: &[ThreadState], jsonl: &str) {
    let value = serde_json::to_value(snapshot).expect("a snapshot serialises");
    let mut clips = Vec::new();
    texts_in(&value, &mut clips);
    // A reference names its record by id; with two records of one id it may find the other.
    let mut ids: HashMap<String, u32> = HashMap::new();
    for record in jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        if let Some(Value::String(id)) = record.get("uuid") {
            let count = ids.entry(id.clone()).or_default();
            *count = count.saturating_add(1);
        }
    }
    for clip in clips {
        let text = clip.get("text").and_then(Value::as_str).unwrap_or_default();
        let chars = text.chars().count();
        assert!(chars <= PROSE.chars.saturating_add(1), "a text of {chars} characters");
        let lines = text.lines().count();
        assert!(lines <= PROSE.lines, "a text of {lines} lines");
        let Some(full) = clip.get("full").filter(|f| !f.is_null()) else { continue };
        let reference = serde_json::from_value(full.clone()).expect("a text reference");
        let record = full.get("record").and_then(Value::as_str).unwrap_or_default();
        if ids.get(record) != Some(&1) {
            continue;
        }
        let whole = full_text(jsonl, &reference).expect("a clipped text's reference resolves");
        let said = clip.get("chars").and_then(Value::as_u64);
        let count = u64::try_from(whole.chars().count()).ok();
        assert_eq!(count, said, "the whole of a clip is another length than it says");
    }
}

/// The clipped texts in `value`, depth first.
fn texts_in<'v>(value: &'v Value, out: &mut Vec<&'v serde_json::Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if ["text", "lines", "chars", "full"].iter().all(|k| map.contains_key(*k)) {
                out.push(map);
            }
            map.values().for_each(|v| texts_in(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| texts_in(v, out)),
        _ => {}
    }
}
