//! Where Claude Code keeps a session's record, and the sessions it kept for a folder.
//!
//! Claude Code writes one transcript per session under
//! `<config>/projects/<escaped working directory>/<session id>.jsonl`, each subagent's beside it
//! (`<session id>/subagents/agent-<id>.jsonl`), where the escaping replaces every character that
//! is not an ASCII letter or digit with a `-` (so `/Users/x/.config` becomes `-Users-x--config`).
//! A Claude Code Slopty starts is given its session id, so its record is found by that id and its
//! folder, never guessed.
//!
//! Each live Claude Code also records itself in `<config>/sessions/<pid>.json` ([`live`]): its
//! pid, the session it holds, its folder, how it runs (`kind`) and its process's start
//! (`procStart`, `ps -o lstart=` in UTC), which tells a pid used again from the one that wrote.
//! It says there too what it does now (`status`: `idle`, `busy`, `waiting`), what a wait is for
//! (`waitingFor`, `needs`) and since when (`statusUpdatedAt`), rewriting the record as that
//! changes: what tells a session whose hooks are not heard waiting on the person
//! ([`Live::waiting`]).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::SystemTime;

use parking_lot::Mutex;
use slopty_core::WallMs;
use slopty_proto::thread::wire::{
    EXPANDED_CHARS, Expanded, PROMPT_HIT_BYTES, PastSession, PromptHit,
};
use slopty_proto::thread::{AgentId, ContentRef, Wait};

use crate::conversation::Part;

/// The variable that moves Claude Code's own directory.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// Claude Code's own directory, as it finds it: `CLAUDE_CONFIG_DIR` when set, else `~/.claude`.
#[must_use]
pub fn config_dir(home: &Path) -> PathBuf {
    std::env::var_os(CONFIG_DIR_ENV).map_or_else(|| home.join(".claude"), PathBuf::from)
}

/// The directory Claude Code writes a session's transcript into, for an agent running in `cwd`.
///
/// Claude Code names the directory after the working directory as the process sees it, which
/// on macOS is the resolved path (`/private/var/…` for a `/var/…` the worker was given), so
/// `cwd` is canonicalised first when it exists; a directory that does not is used as given.
#[must_use]
pub fn project_dir(home: &Path, cwd: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_missing| cwd.to_path_buf());
    config_dir(home).join("projects").join(escape(&resolved))
}

/// A working directory as Claude Code names its project directory: every character that is not
/// an ASCII letter or digit becomes `-`.
#[must_use]
pub fn escape(cwd: &Path) -> String {
    cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Session `session`'s main transcript, for an agent run in `cwd`.
#[must_use]
pub fn transcript(home: &Path, cwd: &Path, session: &str) -> PathBuf {
    project_dir(home, cwd).join(format!("{session}.jsonl"))
}

/// Every record of session `session` run in `cwd`, as Claude Code wrote them.
///
/// Its main transcript's come first, then each subagent's, in the order of their names. A file
/// that is missing reads as none; a line that is not JSON is passed over. Each file is read a
/// line at a time, and a byte that is not UTF-8 costs only its character, never the file.
///
/// # Errors
///
/// When a file is there but cannot be read.
pub fn records(home: &Path, cwd: &Path, session: &str) -> std::io::Result<Vec<serde_json::Value>> {
    use std::io::BufRead as _;
    let mut records = Vec::new();
    for file in files(home, cwd, session) {
        let file = match std::fs::File::open(&file) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for line in std::io::BufReader::new(file).split(b'\n') {
            let line = line?;
            if let Ok(record) = serde_json::from_str(&String::from_utf8_lossy(&line)) {
                records.push(record);
            }
        }
    }
    Ok(records)
}

/// The files session `session` run in `cwd` is written in: its main transcript, then each
/// subagent's, in the order of their names.
fn files(home: &Path, cwd: &Path, session: &str) -> Vec<PathBuf> {
    let main = transcript(home, cwd, session);
    let agents = crate::conversation::subagents_dir(&main);
    let mut files = vec![main];
    if let Ok(listing) = std::fs::read_dir(&agents) {
        let mut found: Vec<PathBuf> = listing
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        found.sort();
        files.extend(found);
    }
    files
}

/// The whole of what `content` names, clipped in a thread of session `session` run in `cwd`.
///
/// It is read from the session's files: a text cut at [`EXPANDED_CHARS`], or a picture's bytes;
/// [`Expanded::Gone`] when no file holds it any more. Only the record's own line is read, found
/// by an index of each file's lines kept across expands, so a long session costs one line per
/// expand and a byte that is not UTF-8 costs only its own line.
#[must_use]
pub fn expand(home: &Path, cwd: &Path, session: &str, content: &ContentRef) -> Expanded {
    let Some((_thread, at)) = super::history::text_ref(content) else { return Expanded::Gone };
    let image = matches!(at.part, Part::Image { .. });
    for file in files(home, cwd, session) {
        let Some(line) = record_line(&file, &at.record) else { continue };
        if image {
            if let Some(bytes) = crate::conversation::image_bytes(&line, &at) {
                return Expanded::Bytes(bytes);
            }
        } else if let Some(mut text) = crate::conversation::full_text(&line, &at) {
            if let Some((cut, _)) = text.char_indices().nth(EXPANDED_CHARS) {
                text.truncate(cut);
            }
            return Expanded::Text(text);
        }
    }
    Expanded::Gone
}

/// How a record's id appears in a transcript line, Claude Code writing it compact.
const UUID_KEY: &str = "\"uuid\":\"";

/// How many transcript files keep their [`RecordIndex`] at once.
const INDEXED_FILES: usize = 8;

/// Where the line of each record named by a `"uuid"` lies in one transcript file, as far as it
/// was read (`len`, the end of its last whole line). Claude Code only appends, so a file found
/// longer is indexed on from `len`; one found shorter was written anew and is indexed again.
#[derive(Debug, Default)]
struct RecordIndex {
    len: u64,
    by_uuid: HashMap<String, Vec<(u64, usize)>>,
}

/// The index of each transcript file read lately, each behind its own lock so that expands of
/// different files read at once and two of the same file index it once.
type Indexes = HashMap<PathBuf, Arc<Mutex<RecordIndex>>>;

static INDEXES: LazyLock<Mutex<Indexes>> = LazyLock::new(Mutex::default);

/// The line of `file` that holds record `uuid` (its bytes decoded, a byte that is not UTF-8
/// replaced); `None` when the file has no such record or cannot be read.
fn record_line(file: &Path, uuid: &str) -> Option<String> {
    let index = {
        let mut all = INDEXES.lock();
        if !all.contains_key(file) && all.len() >= INDEXED_FILES {
            let old = all.keys().next().cloned();
            if let Some(old) = old {
                all.remove(&old);
            }
        }
        Arc::clone(all.entry(file.to_path_buf()).or_default())
    };
    find_line(&mut index.lock(), file, uuid)
}

/// The line of `file` that holds record `uuid`, found by `index` (read on first, and read again
/// whole when the lines it noted moved).
fn find_line(index: &mut RecordIndex, file: &Path, uuid: &str) -> Option<String> {
    for fresh in [false, true] {
        if fresh {
            *index = RecordIndex::default();
        }
        index.read_on(file).ok()?;
        let spots = index.by_uuid.get(uuid)?;
        for &(at, len) in spots {
            let Ok(line) = read_line_at(file, at, len) else { continue };
            let record: Option<serde_json::Value> = serde_json::from_str(&line).ok();
            if record.as_ref().and_then(|r| r.get("uuid")).and_then(serde_json::Value::as_str)
                == Some(uuid)
            {
                return Some(line);
            }
        }
        // The lines moved under the index: the file was written anew, as long or longer.
    }
    None
}

/// The `len` bytes of `file` at `at`, decoded.
fn read_line_at(file: &Path, at: u64, len: usize) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _};
    let mut opened = std::fs::File::open(file)?;
    opened.seek(std::io::SeekFrom::Start(at))?;
    let mut bytes = vec![0; len];
    opened.read_exact(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

impl RecordIndex {
    /// Note the lines of `file` past [`Self::len`]: every id a line holds is noted; the reader
    /// checks the record's own.
    fn read_on(&mut self, file: &Path) -> std::io::Result<()> {
        use std::io::{BufRead as _, Seek as _};
        let mut opened = std::fs::File::open(file)?;
        let size = opened.metadata()?.len();
        if size < self.len {
            *self = Self::default();
        }
        if size == self.len {
            return Ok(());
        }
        opened.seek(std::io::SeekFrom::Start(self.len))?;
        let mut reader = std::io::BufReader::with_capacity(256 * 1024, opened);
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 || line.last() != Some(&b'\n') {
                // The end, or a line Claude Code is still writing: read again from its start.
                return Ok(());
            }
            let text = String::from_utf8_lossy(&line);
            let mut rest: &str = &text;
            while let Some(found) = rest.find(UUID_KEY) {
                rest = rest.get(found.saturating_add(UUID_KEY.len())..).unwrap_or_default();
                let Some(end) = rest.find('"') else { break };
                let id = rest.get(..end).unwrap_or_default();
                if !id.is_empty() {
                    self.by_uuid.entry(id.to_owned()).or_default().push((self.len, line.len()));
                }
            }
            self.len = self.len.saturating_add(u64::try_from(line.len()).unwrap_or(u64::MAX));
        }
    }
}

/// How much of a transcript's head, and of its tail, a listing reads.
///
/// Claude Code writes the first prompt at the head and appends a session's title, last prompt
/// and branch as it goes, so the two ends say what its own resume picker shows without the
/// middle being read.
pub const GLANCE_BYTES: u64 = 64 * 1024;

/// Claude Code's past sessions for an agent run in `cwd`, the last active first, at most
/// `limit`.
///
/// Each comes from its transcript's name and what the two ends of the transcript say of it
/// ([`GLANCE_BYTES`]): its title (the one the person gave it, else Claude Code's own, else its
/// summary, else its first prompt), its last prompt, the branch it was on (the fact
/// [`BRANCH_FACT`](super::history::BRANCH_FACT)), and when it was last active, the time of its
/// last record ([`last_active`]). The `limit` most recently written transcripts are taken; one
/// that cannot be read is listed by its name and its file's time alone. A folder Claude Code
/// never ran in has none.
///
/// # Errors
///
/// When the project directory is there but cannot be listed.
pub fn sessions(home: &Path, cwd: &Path, limit: usize) -> std::io::Result<Vec<PastSession>> {
    let dir = project_dir(home, cwd);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found: Vec<(Option<SystemTime>, String)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| !t.is_dir()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_suffix(".jsonl").filter(|id| super::is_session_id(id))?;
            Some((entry.metadata().and_then(|m| m.modified()).ok(), id.to_owned()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.truncate(limit);
    let folder = cwd.to_string_lossy();
    let mut listed: Vec<PastSession> = found
        .into_iter()
        .map(|(modified, id)| {
            let glance = glance(&dir.join(format!("{id}.jsonl"))).unwrap_or_default();
            let mut facts = BTreeMap::new();
            if let Some(branch) = glance.branch.clone() {
                facts.insert(super::history::BRANCH_FACT.to_owned(), branch);
            }
            PastSession {
                agent: AgentId::named(AgentId::CLAUDE_CODE),
                cwd: Some(folder.clone().into_owned()),
                title: glance.title(),
                prompts: glance.last_prompt().into_iter().collect(),
                // When its last record was written, as Claude Code stamped it: the file's own
                // time moves when the file is copied or touched.
                updated_ms: glance.at.or_else(|| modified.map(WallMs::of)),
                thread: None,
                resume: vec![super::RESUME_FLAG.to_owned(), id.clone()],
                facts,
                native: id,
            }
        })
        .collect();
    listed.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| a.native.cmp(&b.native)));
    Ok(listed)
}

/// When the session whose transcript is at `path` was last active.
///
/// It is the time Claude Code wrote on its last record that carries one, read from the
/// transcript's tail ([`GLANCE_BYTES`]); `None` when no record in the tail carries a time, or
/// the file cannot be read.
///
/// Every surface that says when a past session was active says this time: the folder's
/// listing ([`sessions`]), a search over the person's prompts, and the thread once it is open.
#[must_use]
pub fn last_active(path: &Path) -> Option<WallMs> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(GLANCE_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    file.take(GLANCE_BYTES).read_to_end(&mut tail).ok()?;
    let mut lines = tail.split(|b| *b == b'\n');
    if from > 0 {
        // The window's first line is cut.
        lines.next();
    }
    lines
        .rev()
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .find_map(|record| stamped(&record))
}

/// The time Claude Code wrote on `record`, when it carries one (`timestamp`).
fn stamped(record: &serde_json::Value) -> Option<WallMs> {
    record.get("timestamp")?.as_str().and_then(crate::conversation::parse_ms)
}

/// What the two ends of a transcript say of its session.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct Glance {
    /// The title the person gave it (`custom-title`), the last one written.
    named: Option<String>,
    /// Claude Code's own title for it (`ai-title`).
    titled: Option<String>,
    /// Its summary (`summary`).
    summary: Option<String>,
    /// The first prompt in its head.
    first: Option<String>,
    /// The last prompt Claude Code noted (`last-prompt`).
    noted: Option<String>,
    /// The last prompt read, and when it was sent.
    last: Option<(String, Option<WallMs>)>,
    /// The branch the last record that names one was written on (`gitBranch`).
    branch: Option<String>,
    /// When the last record that carries a time was written (`timestamp`).
    at: Option<WallMs>,
}

impl Glance {
    /// The session's title, as Claude Code's resume picker shows it.
    fn title(&self) -> Option<String> {
        [&self.named, &self.titled, &self.summary]
            .into_iter()
            .flatten()
            .map(|t| t.trim().to_owned())
            .chain(self.first.as_deref().map(crate::title::title_of))
            .find(|t| !t.is_empty())
    }

    /// The session's last prompt, cut to [`PROMPT_HIT_BYTES`].
    fn last_prompt(&self) -> Option<PromptHit> {
        let (text, at_ms) = match (&self.noted, &self.last) {
            (Some(noted), _) => (noted.trim(), None),
            (None, Some((text, at))) => (text.trim(), *at),
            (None, None) => return None,
        };
        if text.is_empty() {
            return None;
        }
        let clipped = crate::history::clip(text.to_owned(), PROMPT_HIT_BYTES);
        let cut_after = clipped.len() < text.len();
        Some(PromptHit { text: clipped, spans: Vec::new(), cut_before: false, cut_after, at_ms })
    }

    /// Take in one of the transcript's records, read in order; `head` while in its head.
    fn take(&mut self, record: &serde_json::Value, head: bool) {
        let text =
            |key: &str| record.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
        match record.get("type").and_then(serde_json::Value::as_str) {
            Some("custom-title") => later(&mut self.named, text("customTitle")),
            Some("ai-title") => later(&mut self.titled, text("aiTitle")),
            Some("summary") => later(&mut self.summary, text("summary")),
            Some("last-prompt") => later(&mut self.noted, text("lastPrompt")),
            Some("user") => {
                if let Some(prompt) = prompt_of(record) {
                    if head && self.first.is_none() {
                        self.first = Some(prompt.clone());
                    }
                    let at = text("timestamp").as_deref().and_then(crate::conversation::parse_ms);
                    self.last = Some((prompt, at));
                }
            }
            _ => {}
        }
        if let Some(branch) = text("gitBranch").filter(|b| !b.is_empty()) {
            self.branch = Some(branch);
        }
        if let Some(at) = stamped(record) {
            self.at = Some(at);
        }
    }
}

/// `value` in `slot` when there is one: a later record's word stands over an earlier one's.
fn later(slot: &mut Option<String>, value: Option<String>) {
    if value.is_some() {
        *slot = value;
    }
}

/// The person's prompt a transcript's `user` record carries: not one Claude Code added (meta,
/// a summary of what was compacted, a subagent's), not a tool's result, not a command or a
/// tagged block of its own (`<command-name>`, `<local-command-stdout>`).
fn prompt_of(record: &serde_json::Value) -> Option<String> {
    let flag = |key: &str| record.get(key).and_then(serde_json::Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") || flag("isSidechain") {
        return None;
    }
    let content = record.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => {
            if parts.iter().any(|part| part_kind(part) == Some("tool_result")) {
                return None;
            }
            let texts: Vec<&str> = parts
                .iter()
                .filter(|part| part_kind(part) == Some("text"))
                .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                .collect();
            texts.join("\n")
        }
        _ => return None,
    };
    let text = text.trim();
    let tagged = text.strip_prefix('<').is_some_and(|tag| {
        tag.split(['>', ' ']).next().is_some_and(|name| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        })
    });
    (!text.is_empty() && !tagged && !crate::history::is_command(text)).then(|| text.to_owned())
}

/// What a part of a message's content is (`text`, `tool_result`).
fn part_kind(part: &serde_json::Value) -> Option<&str> {
    part.get("type").and_then(serde_json::Value::as_str)
}

/// What the transcript at `path` says of its session at its two ends: its head and its tail,
/// [`GLANCE_BYTES`] each, read once when they meet; a line cut by either window is passed over.
fn glance(path: &Path) -> std::io::Result<Glance> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut read = |from: u64, bytes: u64| -> std::io::Result<Vec<u8>> {
        file.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        (&mut file).take(bytes).read_to_end(&mut buf)?;
        Ok(buf)
    };
    let whole = len <= GLANCE_BYTES.saturating_mul(2);
    let head = read(0, if whole { len } else { GLANCE_BYTES })?;
    let tail =
        if whole { Vec::new() } else { read(len.saturating_sub(GLANCE_BYTES), GLANCE_BYTES)? };
    let mut glance = Glance::default();
    let lines = |bytes: &[u8], cut_first: bool, cut_last: bool| -> Vec<serde_json::Value> {
        let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
        if cut_first && !lines.is_empty() {
            lines.remove(0);
        }
        if cut_last {
            lines.pop();
        }
        lines
            .into_iter()
            .filter_map(|line| serde_json::from_str(&String::from_utf8_lossy(line)).ok())
            .collect()
    };
    for record in lines(&head, false, !whole) {
        glance.take(&record, true);
    }
    for record in lines(&tail, true, false) {
        glance.take(&record, false);
    }
    Ok(glance)
}

/// A live Claude Code, as it records itself in `<config>/sessions/<pid>.json`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Live {
    /// Its process.
    pub pid: u32,
    /// The session it holds: the one whose transcript it writes. A `/clear` or `/resume` in its
    /// TUI moves it to another, and its record says so.
    pub session: String,
    /// The folder it runs in.
    pub cwd: Option<String>,
    /// How it runs: `interactive` in a terminal, else how it runs in the background (`bg`,
    /// `daemon`, `daemon-worker`).
    pub kind: String,
    /// Its process's start as `ps -o lstart=` says it in UTC, when it said.
    pub started: Option<String>,
    /// Its version, when it said.
    pub version: Option<String>,
    /// The session's name as its TUI shows it (Claude Code's title, or the person's), when it
    /// has one.
    pub name: Option<String>,
    /// What it says it does, when it says: `idle` at rest, `busy` at work, `waiting` on the
    /// person at a dialog.
    pub status: Option<String>,
    /// What a wait is for, in its words: `permission prompt` (a tool's permission, a plan's
    /// approval, and any dialog that says nothing else), `input needed` (a question),
    /// `sandbox request`, `goal proposal`, `dialog open` (another dialog of its own that holds
    /// the session: a fallback model, a usage limit, a notice).
    pub waiting_for: Option<String>,
    /// What the person is to do at that dialog, where it says ("choose: allow or deny the
    /// computer-use action").
    pub needs: Option<String>,
    /// When its status last changed.
    pub status_at: Option<WallMs>,
}

/// The status a live Claude Code records while it waits on the person.
const WAITING: &str = "waiting";

/// What a wait is for when the record says nothing else: Claude Code's own default.
const PERMISSION_PROMPT: &str = "permission prompt";

impl Live {
    /// What it waits on the person for while its record says it waits: a wait of
    /// [`Wait`]'s kinds (`permission`, `question`, else `input`), in its own words. The record
    /// names no call, so a caller that knows the call the dialog is about words it better.
    #[must_use]
    pub fn waiting(&self) -> Option<Wait> {
        if self.status.as_deref() != Some(WAITING) {
            return None;
        }
        let what = self.waiting_for.as_deref().map(str::trim).filter(|w| !w.is_empty());
        let what = what.unwrap_or(PERMISSION_PROMPT);
        let (kind, said) = match what {
            PERMISSION_PROMPT => (crate::hooks::PERMISSION, "Wants permission".to_owned()),
            "sandbox request" => (crate::hooks::PERMISSION, "Wants network access".to_owned()),
            "input needed" => (crate::hooks::QUESTION, "Has a question".to_owned()),
            "goal proposal" => (crate::hooks::INPUT, "Proposes a goal".to_owned()),
            "dialog open" => (crate::hooks::INPUT, "Has a dialog open".to_owned()),
            other => (crate::hooks::INPUT, capitalised(other)),
        };
        let needs = self.needs.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let text = needs.map_or(said, capitalised);
        Some(Wait { kind: kind.to_owned(), text })
    }
}

/// `text` with its first letter a capital.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// The directory, under Claude Code's, each live Claude Code records itself in.
pub const LIVE_DIR: &str = "sessions";

/// The largest record of a live Claude Code read: a few hundred bytes are written.
const LIVE_BYTES: u64 = 64 * 1024;

/// Every live Claude Code's record of itself under `home` ([`LIVE_DIR`]), as written.
///
/// One whose process has ended without taking its record away is still here, and is the
/// caller's to tell apart. A record that cannot be read or says no pid or session is passed
/// over.
#[must_use]
pub fn live(home: &Path) -> Vec<Live> {
    let Ok(entries) = std::fs::read_dir(config_dir(home).join(LIVE_DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<Live> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|entry| live_at(&entry.path()))
        .collect();
    found.sort_by_key(|live| live.pid);
    found
}

/// The record process `pid` keeps of itself under `home`, if it keeps one ([`live`]).
#[must_use]
pub fn live_of(home: &Path, pid: u32) -> Option<Live> {
    live_at(&config_dir(home).join(LIVE_DIR).join(format!("{pid}.json")))
        .filter(|live| live.pid == pid)
}

/// The record in the file at `path`.
fn live_at(path: &Path) -> Option<Live> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(LIVE_BYTES).read_to_end(&mut bytes).ok()?;
    let record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let text = |key: &str| record.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    let pid = u32::try_from(record.get("pid")?.as_u64()?).ok().filter(|p| *p > 0)?;
    let session = text("sessionId").filter(|s| super::is_session_id(s))?;
    Some(Live {
        pid,
        session,
        cwd: text("cwd"),
        kind: text("kind").filter(|k| !k.is_empty()).unwrap_or_else(|| "interactive".to_owned()),
        started: text("procStart"),
        version: text("version").filter(|v| !v.trim().is_empty()),
        name: text("name").filter(|n| !n.trim().is_empty()),
        status: text("status").filter(|s| !s.trim().is_empty()),
        waiting_for: text("waitingFor"),
        needs: text("needs"),
        status_at: record
            .get("statusUpdatedAt")
            .and_then(serde_json::Value::as_u64)
            .map(WallMs::from_millis),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder is named as Claude Code names its project directory: resolved when it exists,
    /// as given when it does not.
    #[test]
    fn a_working_directory_becomes_claude_codes_project_directory() {
        assert_eq!(
            escape(Path::new("/Volumes/Lacie/Workspace/oss/slopty")),
            "-Volumes-Lacie-Workspace-oss-slopty"
        );
        assert_eq!(escape(Path::new("/Users/x/.config")), "-Users-x--config");
        let real = tempfile::tempdir().expect("tempdir");
        let canonical = std::fs::canonicalize(real.path()).expect("canonical");
        assert!(project_dir(Path::new("/Users/x"), real.path()).ends_with(escape(&canonical)));
    }

    /// A session's records are its main transcript's, then each subagent's; a torn line is
    /// passed over and a session with no file has none.
    #[test]
    fn a_sessions_records_are_its_transcripts() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/slopty-absent/work");
        assert_eq!(records(home.path(), cwd, "s1").expect("none").len(), 0, "no file");
        let main = transcript(home.path(), cwd, "s1");
        let agents = crate::conversation::subagents_dir(&main);
        std::fs::create_dir_all(&agents).expect("mkdir");
        std::fs::write(&main, "{\"uuid\":\"a\"}\n{torn\n").expect("main");
        std::fs::write(agents.join("agent-x.jsonl"), "{\"uuid\":\"b\"}\n").expect("agent");
        let read = records(home.path(), cwd, "s1").expect("read");
        let ids: Vec<&str> = read.iter().filter_map(|r| r["uuid"].as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    /// A byte that is not UTF-8 costs its record's character, never the transcript: the rest
    /// of its line and every other line are read.
    #[test]
    fn a_byte_not_utf8_costs_only_its_character() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/slopty-absent/work");
        let main = transcript(home.path(), cwd, "s2");
        std::fs::create_dir_all(main.parent().expect("dir")).expect("mkdir");
        let mut bytes = b"{\"uuid\":\"a\",\"text\":\"word".to_vec();
        bytes.extend([0xff, b'"', b'}', b'\n']);
        bytes.extend(b"{\"uuid\":\"b\"}\n");
        std::fs::write(&main, bytes).expect("main");
        let read = records(home.path(), cwd, "s2").expect("read");
        let ids: Vec<&str> = read.iter().filter_map(|r| r["uuid"].as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(read[0]["text"], "word\u{fffd}");
    }

    /// A clipped text is read from its record's line alone: a byte that is not UTF-8 in
    /// another line costs nothing, a record appended later is found, and a transcript written
    /// anew is indexed again.
    #[test]
    fn an_expand_reads_its_records_line_alone() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/slopty-absent/work");
        let main = transcript(home.path(), cwd, "s3");
        std::fs::create_dir_all(main.parent().expect("dir")).expect("mkdir");
        let said = |uuid: &str, text: &str| {
            let record = serde_json::json!({
                "type": "assistant", "uuid": uuid,
                "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] },
            });
            format!("{record}\n")
        };
        let ask = |uuid: &str| {
            let at = crate::conversation::TextRef {
                record: uuid.to_owned(),
                part: Part::Block { index: 0 },
            };
            crate::claude::history::content_ref(&crate::conversation::ThreadId::Main, &at)
        };
        let mut bytes = b"{\"uuid\":\"bad\",\"text\":\"".to_vec();
        bytes.extend([0xff, b'"', b'}', b'\n']);
        bytes.extend(said("a", "first").as_bytes());
        std::fs::write(&main, &bytes).expect("main");
        let expand = |uuid| expand(home.path(), cwd, "s3", &ask(uuid));
        assert_eq!(expand("a"), Expanded::Text("first".to_owned()));
        assert_eq!(expand("b"), Expanded::Gone, "not written yet");

        let mut file = std::fs::OpenOptions::new().append(true).open(&main).expect("open");
        std::io::Write::write_all(&mut file, said("b", "second").as_bytes()).expect("append");
        std::io::Write::write_all(&mut file, b"{\"uuid\":\"torn").expect("torn");
        assert_eq!(expand("b"), Expanded::Text("second".to_owned()), "appended");

        std::fs::write(&main, said("c", "anew")).expect("anew");
        assert_eq!(expand("c"), Expanded::Text("anew".to_owned()), "written anew");
        assert_eq!(expand("a"), Expanded::Gone);
    }

    /// One record of session `id`'s transcript, as Claude Code writes it.
    fn user(id: &str, text: &str, branch: &str) -> String {
        serde_json::json!({
            "type": "user", "sessionId": id, "gitBranch": branch,
            "timestamp": "2026-10-15T10:00:00.000Z",
            "message": { "role": "user", "content": text },
        })
        .to_string()
    }

    /// A listing names each session as Claude Code's own picker does, from the two ends of its
    /// transcript: the title the person gave it over Claude Code's and the first prompt, the
    /// last prompt Claude Code noted, and the branch of the last record; what Claude Code added
    /// (meta, a tool's result, a command) is no prompt. A transcript longer than both ends is
    /// read at its head and its tail alone, and one that cannot be read is listed by its name.
    #[test]
    fn a_listing_names_each_session_from_the_ends_of_its_transcript() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = tempfile::tempdir().expect("cwd");
        let dir = project_dir(home.path(), cwd.path());
        std::fs::create_dir_all(&dir).expect("mkdir");
        let meta = serde_json::json!({
            "type": "user", "isMeta": true, "message": { "role": "user", "content": "Caveat" },
        });
        let result = serde_json::json!({
            "type": "user", "message": { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "ok" },
            ] },
        });
        let lines = [
            meta.to_string(),
            user("a", "<command-name>/model</command-name>", "main"),
            user("a", "Explain the parser", "main"),
            result.to_string(),
            user("a", "Now add tests", "feature"),
        ];
        std::fs::write(dir.join("a.jsonl"), format!("{}\n", lines.join("\n"))).expect("a");
        let listed = sessions(home.path(), cwd.path(), 10).expect("listed");
        assert_eq!(listed[0].title.as_deref(), Some("Explain the parser"), "its first prompt");
        assert_eq!(listed[0].prompts[0].text, "Now add tests", "its last prompt");
        assert_eq!(listed[0].facts.get("branch").map(String::as_str), Some("feature"));

        // A long one: the ends are read, the middle never is.
        let filler = user("b", "a prompt in the middle", "middle");
        let mut text = format!("{}\n", user("b", "First words", "main"));
        while u64::try_from(text.len()).expect("len") < GLANCE_BYTES.saturating_mul(3) {
            text.push_str(&filler);
            text.push('\n');
        }
        for (kind, key, value) in [
            ("ai-title", "aiTitle", "Claude's title"),
            ("custom-title", "customTitle", "Named by the person"),
            ("last-prompt", "lastPrompt", "The last words"),
        ] {
            text.push_str(
                &serde_json::json!({ "type": kind, key: value, "sessionId": "b" }).to_string(),
            );
            text.push('\n');
        }
        text.push_str(&user("b", "x", "release"));
        std::fs::write(dir.join("b.jsonl"), text).expect("b");
        std::fs::write(dir.join("c.jsonl"), [0xff_u8, b'\n']).expect("c");
        let listed = sessions(home.path(), cwd.path(), 10).expect("listed");
        let b = listed.iter().find(|s| s.native == "b").expect("b");
        assert_eq!(b.title.as_deref(), Some("Named by the person"));
        assert_eq!(b.prompts[0].text, "The last words");
        assert_eq!(b.facts.get("branch").map(String::as_str), Some("release"));
        let c = listed.iter().find(|s| s.native == "c").expect("c");
        assert_eq!((c.title.as_deref(), c.prompts.len()), (None, 0), "by its name alone");
    }

    /// Each live Claude Code's record is read as written, how it runs and its process's start
    /// with it; a record with no session, a key beside it and a torn one are passed over.
    #[test]
    fn live_claude_codes_are_read_from_their_records() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = config_dir(home.path()).join(LIVE_DIR);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let record = serde_json::json!({
            "pid": 4242, "sessionId": "9d2e7a10-5c4b", "cwd": "/w", "kind": "interactive",
            "procStart": "Fri Oct 16 16:35:24 2026", "status": "idle",
        });
        std::fs::write(dir.join("4242.json"), record.to_string()).expect("live");
        std::fs::write(dir.join("4242.abc.key"), "secret").expect("key");
        std::fs::write(dir.join("7.json"), r#"{"pid":7}"#).expect("no session");
        std::fs::write(dir.join("8.json"), "{torn").expect("torn");
        std::fs::write(dir.join("9.json"), r#"{"pid":9,"sessionId":"s9"}"#).expect("bare");
        let live = live(home.path());
        assert_eq!(live.len(), 2, "{live:?}");
        assert_eq!(live[0].pid, 9);
        assert_eq!((live[0].kind.as_str(), live[0].started.as_deref()), ("interactive", None));
        assert_eq!(live[1].session, "9d2e7a10-5c4b");
        assert_eq!(live[1].started.as_deref(), Some("Fri Oct 16 16:35:24 2026"));
        assert_eq!(live[1].cwd.as_deref(), Some("/w"));
    }

    /// What Claude Code 2.1.312 records of itself at rest, at work and at its two dialogs (its
    /// own records, scrubbed): waiting on the person at a dialog alone, for a permission or a
    /// question as it says, since when it says. A dialog that says what it needs is said in
    /// those words, another dialog is input, and a record of a Claude Code from before it said
    /// any of this waits on nothing.
    #[test]
    fn a_live_claude_code_says_when_it_waits_on_the_person() {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = config_dir(home.path()).join(LIVE_DIR);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let read = |record: &str| {
            std::fs::write(dir.join("4242.json"), record).expect("live");
            live_of(home.path(), 4242).expect("read")
        };
        let permission = include_str!("../../tests/fixtures/registry/waiting-permission.json");
        let samples = [
            (include_str!("../../tests/fixtures/registry/idle.json"), "idle", None),
            (include_str!("../../tests/fixtures/registry/busy.json"), "busy", None),
            (permission, "waiting", Some(("permission", "Wants permission"))),
            (
                include_str!("../../tests/fixtures/registry/waiting-question.json"),
                "waiting",
                Some(("question", "Has a question")),
            ),
        ];
        for (record, status, waits) in samples {
            let live = read(record);
            assert_eq!(live.status.as_deref(), Some(status), "{live:?}");
            assert!(live.status_at.is_some_and(|at| at > WallMs::ZERO), "{live:?}");
            let wait = live.waiting();
            let said = wait.as_ref().map(|w| (w.kind.as_str(), w.text.as_str()));
            assert_eq!(said, waits, "{live:?}");
        }

        let mut record: serde_json::Value = serde_json::from_str(permission).expect("json");
        record["needs"] = "choose: allow or deny the computer-use action".into();
        let wait = read(&record.to_string()).waiting().expect("waits");
        assert_eq!(
            (wait.kind.as_str(), wait.text.as_str()),
            ("permission", "Choose: allow or deny the computer-use action")
        );
        record["waitingFor"] = "dialog open".into();
        record.as_object_mut().expect("object").remove("needs");
        let wait = read(&record.to_string()).waiting().expect("waits");
        assert_eq!((wait.kind.as_str(), wait.text.as_str()), ("input", "Has a dialog open"));
        for key in ["status", "waitingFor", "statusUpdatedAt"] {
            record.as_object_mut().expect("object").remove(key);
        }
        let live = read(&record.to_string());
        assert_eq!((live.status.as_deref(), live.waiting(), live.status_at), (None, None, None));
    }

    /// A past session's time is its last record's, as Claude Code stamped it, never its file's:
    /// a transcript copied or touched today still reads as the day it was last active, and the
    /// listing puts the last active first. A record with no time (a title) is passed over; a
    /// transcript with no time at all falls back to its file's.
    #[test]
    fn a_past_sessions_time_is_its_last_records() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = tempfile::tempdir().expect("cwd");
        let dir = project_dir(home.path(), cwd.path());
        std::fs::create_dir_all(&dir).expect("mkdir");
        let at = |id: &str, text: &str, stamp: &str| {
            serde_json::json!({
                "type": "user", "sessionId": id, "timestamp": stamp,
                "message": { "role": "user", "content": text },
            })
            .to_string()
        };
        let title =
            serde_json::json!({ "type": "ai-title", "aiTitle": "Parser", "sessionId": "old" });
        let old = [
            at("old", "Explain the parser", "2026-10-14T09:00:00.000Z"),
            at("old", "Now add tests", "2026-10-15T10:01:00.000Z"),
            title.to_string(),
        ];
        std::fs::write(dir.join("old.jsonl"), format!("{}\n", old.join("\n"))).expect("old");
        let newer = at("newer", "Fix the build", "2026-10-16T08:00:00.000Z");
        std::fs::write(dir.join("newer.jsonl"), format!("{newer}\n")).expect("newer");
        // Written last, so its file is the newest; its records say otherwise.
        std::fs::write(dir.join("old.jsonl"), format!("{}\n", old.join("\n"))).expect("touched");
        std::fs::write(dir.join("bare.jsonl"), "{\"type\":\"summary\"}\n").expect("bare");

        let stamp = |text: &str| crate::conversation::parse_ms(text).expect("a time");
        let listed = sessions(home.path(), cwd.path(), 10).expect("listed");
        let old_at = listed.iter().find(|s| s.native == "old").and_then(|s| s.updated_ms);
        assert_eq!(old_at, Some(stamp("2026-10-15T10:01:00.000Z")), "its last record's");
        assert_eq!(last_active(&dir.join("old.jsonl")), old_at, "one time for every surface");
        let order: Vec<&str> = listed.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(order, ["bare", "newer", "old"], "the last active first");
        let bare = listed.iter().find(|s| s.native == "bare").expect("bare");
        assert!(bare.updated_ms.is_some(), "no time in it: its file's");
        assert_eq!(last_active(&dir.join("bare.jsonl")), None);
        assert_eq!(last_active(&dir.join("absent.jsonl")), None);
    }
}
