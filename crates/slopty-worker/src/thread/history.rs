//! The person's past prompts on this machine, searched: the sessions behind
//! [`ThreadRequest::Sessions`](slopty_proto::thread::wire::ThreadRequest::Sessions) when it
//! carries words, or names no single agent and folder.
//!
//! The prompts come from each agent's own record of them ([`slopty_agent::history`]): Claude
//! Code's prompt history, Codex's prompt history and the rollouts of threads its TUI did not run,
//! and pi's session files. Nothing is written to them, and no Claude Code transcript is read.
//!
//! **The index.** Every file read is kept in memory with how far it was read and what it held.
//! The records are only ever appended to, so a later search reads only what was added since,
//! from the last whole line; a file that shrank or was replaced is read again from its start. A
//! search is bounded ([`Limits`]): a file larger than its share is read from its tail, and a scan
//! stops reading when its bytes or time run out. What it skipped is said in
//! [`Found::cut`], and the next search reads on from there.
//!
//! **The match.** Every word asked for must be in the prompt, as written, case ignored unless a
//! word has a capital, accents ignored (`nucleo-matcher`'s substring atoms). A whole word, or one
//! at a word's start, scores above one inside another word. The sessions are ranked by their best
//! prompt's score, then by when it was sent; each carries its best few prompts, cut round their
//! first match.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use nucleo_matcher::chars::{graphemes, normalize, to_lower_case};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use parking_lot::Mutex;
use slopty_agent::history::{self, Prompt, claude, codex, pi};
use slopty_core::WallMs;
use slopty_proto::search::Span;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::{PROMPT_HIT_BYTES, PastSession, PromptHit};

/// How many of a session's prompts that matched it carries.
pub const PROMPTS_PER_SESSION: usize = 3;

/// How much of a prompt a hit shows before its first match, in bytes.
const LEAD_BYTES: usize = 120;

/// How much of a file's head is read for the line that names its session, in bytes.
const HEAD_BYTES: u64 = 1024 * 1024;

/// Where this machine's agents keep their records; `None` for an agent whose directory is not
/// known.
#[derive(Clone, Debug, Default)]
pub struct Stores {
    /// Claude Code's directory (`~/.claude`).
    pub claude: Option<PathBuf>,
    /// Codex's (`$CODEX_HOME`, else `~/.codex`).
    pub codex: Option<PathBuf>,
    /// pi's agent directory (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`).
    pub pi: Option<PathBuf>,
}

impl Stores {
    /// Where the agents keep them for the person this process runs as.
    #[must_use]
    pub fn here() -> Self {
        let home = slopty_platform::dirs::home();
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| home.join(".codex"));
        Self {
            claude: Some(home.join(".claude")),
            codex: Some(codex),
            pi: slopty_agent::pi::sessions::agent_dir(),
        }
    }
}

/// What one search may read.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most of one file read, from its tail: the newest prompts.
    pub file_bytes: u64,
    /// The most read over a search.
    pub scan_bytes: u64,
    /// How long a search may read for.
    pub scan_time: Duration,
}

impl Default for Limits {
    /// Measured on 2026-10-04 (`docs/MEASUREMENTS.md`, "Prompt search"): a cold read of this
    /// machine's records takes well under these.
    fn default() -> Self {
        Self {
            file_bytes: 64 * 1024 * 1024,
            scan_bytes: 512 * 1024 * 1024,
            scan_time: Duration::from_secs(3),
        }
    }
}

/// A search.
#[derive(Clone, Copy, Debug)]
pub struct Ask<'a> {
    /// Only this agent's sessions.
    pub agent: Option<&'a AgentId>,
    /// Only sessions in this folder.
    pub cwd: Option<&'a str>,
    /// The words; empty for the sessions prompted last.
    pub query: &'a str,
    /// The most sessions.
    pub limit: usize,
}

/// What a search found.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Found {
    /// The sessions, best first.
    pub sessions: Vec<PastSession>,
    /// Why none could be looked for, in words.
    pub absent: Option<String>,
    /// Why some may be missing, in words.
    pub cut: Option<String>,
}

/// The prompt index of this machine's agents. Cheap to clone; a search blocks on the disk, so
/// it runs off the async threads.
#[derive(Clone, Debug)]
pub struct History {
    stores: Arc<Stores>,
    limits: Limits,
    index: Arc<Mutex<Index>>,
}

/// The agents whose prompts are read, as the index names them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Pi];

    fn id(self) -> AgentId {
        AgentId::named(match self {
            Self::Claude => AgentId::CLAUDE_CODE,
            Self::Codex => AgentId::CODEX,
            Self::Pi => AgentId::PI,
        })
    }

    fn of(id: &AgentId) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| id.is(agent.id().0.as_str()))
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Pi => "pi",
        }
    }
}

/// What kind of record a file is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    ClaudeHistory,
    CodexHistory,
    CodexRollout,
    PiSession,
}

impl Kind {
    const fn agent(self) -> Agent {
        match self {
            Self::ClaudeHistory => Agent::Claude,
            Self::CodexHistory | Self::CodexRollout => Agent::Codex,
            Self::PiSession => Agent::Pi,
        }
    }
}

/// What tells a file changed: its identity, size and modification time.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Stamp {
    ino: u64,
    len: u64,
    modified: Option<SystemTime>,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self { ino: meta.ino(), len: meta.len(), modified: meta.modified().ok() }
    }
}

/// One file as far as it was read.
#[derive(Debug)]
struct Record {
    kind: Kind,
    /// The file it was read from: another file at the same path is read again from its start.
    ino: u64,
    /// How the file was when last read to its end; `None` while there is more to read.
    read_whole: Option<Stamp>,
    /// The first byte not read yet: just past the last whole line.
    read_to: u64,
    /// Its head was skipped, as larger than a file's share.
    skipped_head: bool,
    /// What its first line said: a rollout's thread, a pi session's id and folder.
    head: Option<Head>,
    /// Its prompts, in the order written.
    prompts: Vec<Kept>,
    /// A rollout's prompts as input items, kept apart from its events ([`codex::Said`]).
    items: Vec<Kept>,
}

/// What a file's first line names.
#[derive(Clone, Debug)]
enum Head {
    Rollout(codex::Meta),
    Pi { id: String, cwd: String },
}

/// Every file read, and the pastes Claude Code kept apart.
#[derive(Debug, Default)]
struct Index {
    records: HashMap<PathBuf, Record>,
    pastes: HashMap<String, Option<String>>,
}

/// What a search may still read.
struct Budget {
    bytes: u64,
    until: Instant,
    /// What was skipped, for [`Found::cut`].
    skipped: Vec<String>,
}

impl Budget {
    fn out_of_time(&self) -> bool {
        Instant::now() >= self.until
    }
}

impl History {
    /// The index of what `stores` hold, each search bounded by `limits`.
    #[must_use]
    pub fn new(stores: Stores, limits: Limits) -> Self {
        Self { stores: Arc::new(stores), limits, index: Arc::new(Mutex::new(Index::default())) }
    }

    /// The sessions `ask` finds, reading what was added to the records since the last search
    /// first. Blocks on the disk.
    #[must_use]
    pub fn search(&self, ask: &Ask<'_>) -> Found {
        let agents: Vec<Agent> = match ask.agent {
            None => Agent::ALL.to_vec(),
            Some(id) => {
                let Some(agent) = Agent::of(id) else {
                    let absent = format!("{} keeps no prompt history Slopty can read", id.0);
                    return Found { absent: Some(absent), ..Found::default() };
                };
                vec![agent]
            }
        };
        let present: Vec<Agent> =
            agents.into_iter().filter(|agent| self.dir(*agent).is_some_and(Path::is_dir)).collect();
        if present.is_empty() {
            let absent = match ask.agent {
                Some(id) => format!("{} has kept no prompts on this machine", id.0),
                None => "No agent has kept prompts on this machine".to_owned(),
            };
            return Found { absent: Some(absent), ..Found::default() };
        }
        let mut budget = Budget {
            bytes: self.limits.scan_bytes,
            until: Instant::now().checked_add(self.limits.scan_time).unwrap_or_else(Instant::now),
            skipped: Vec::new(),
        };
        let files = self.files(&present);
        let listed: HashSet<&PathBuf> = files.iter().map(|(path, ..)| path).collect();
        let wanted = wanted_folders(ask.cwd);
        let mut index = self.index.lock();
        index.records.retain(|path, record| {
            !present.contains(&record.kind.agent()) || listed.contains(path)
        });
        for (path, kind, meta) in &files {
            if budget.out_of_time() {
                budget.skipped.push("the read ran out of time".to_owned());
                break;
            }
            self.read(&mut index, path, *kind, meta, &mut budget);
        }
        let cut = cut_words(&mut budget.skipped, &index, &present);
        let sessions = self.rank(&index, &present, wanted.as_deref(), ask.query, ask.limit);
        drop(index);
        Found { sessions, absent: None, cut }
    }

    fn dir(&self, agent: Agent) -> Option<&Path> {
        match agent {
            Agent::Claude => self.stores.claude.as_deref(),
            Agent::Codex => self.stores.codex.as_deref(),
            Agent::Pi => self.stores.pi.as_deref(),
        }
    }

    /// Every record file of `agents`, the newest first so a scan cut short has read those.
    fn files(&self, agents: &[Agent]) -> Vec<(PathBuf, Kind, fs::Metadata)> {
        let mut files: Vec<(Option<SystemTime>, PathBuf, Kind, fs::Metadata)> = Vec::new();
        let mut add = |path: PathBuf, kind| {
            if let Ok(meta) = fs::metadata(&path)
                && meta.is_file()
            {
                files.push((meta.modified().ok(), path, kind, meta));
            }
        };
        for agent in agents {
            let Some(dir) = self.dir(*agent) else { continue };
            match agent {
                Agent::Claude => add(dir.join(claude::HISTORY), Kind::ClaudeHistory),
                Agent::Codex => {
                    add(dir.join(codex::HISTORY), Kind::CodexHistory);
                    for sessions in codex::SESSIONS {
                        for path in rollouts(&dir.join(sessions)) {
                            add(path, Kind::CodexRollout);
                        }
                    }
                }
                Agent::Pi => {
                    for folder in children(&dir.join("sessions")) {
                        for path in children(&folder) {
                            let name = path.file_name().and_then(|n| n.to_str());
                            if name.and_then(slopty_agent::pi::sessions::id_of).is_some() {
                                add(path, Kind::PiSession);
                            }
                        }
                    }
                }
            }
        }
        files.sort_by_key(|file| std::cmp::Reverse(file.0));
        files.into_iter().map(|(_, path, kind, meta)| (path, kind, meta)).collect()
    }

    /// Read what was added to the file at `path`, as `meta` found it, since it was last read,
    /// within `budget`.
    fn read(
        &self,
        index: &mut Index,
        path: &Path,
        kind: Kind,
        meta: &fs::Metadata,
        budget: &mut Budget,
    ) {
        let Index { records, pastes } = index;
        let stamp = Stamp::of(meta);
        let record =
            records.entry(path.to_path_buf()).or_insert_with(|| Record::new(kind, stamp.ino));
        if record.read_whole == Some(stamp) {
            return;
        }
        let shrank = stamp.len < record.read_to;
        if record.ino != stamp.ino || shrank {
            *record = Record::new(kind, stamp.ino);
        }
        if record.head.is_none() && matches!(kind, Kind::CodexRollout | Kind::PiSession) {
            record.head = first_line(path).and_then(|line| match kind {
                Kind::CodexRollout => codex::meta(&line).map(Head::Rollout),
                _ => pi::header(&line).map(|(id, cwd)| Head::Pi { id, cwd }),
            });
            if record.head.is_none() {
                record.read_whole = Some(stamp);
                return;
            }
        }
        // A rollout whose prompts are in Codex's history, or are nobody's, is read no further.
        if let Some(Head::Rollout(meta)) = &record.head
            && meta.kept != codex::Kept::Rollout
        {
            record.read_whole = Some(stamp);
            return;
        }
        let mut from = record.read_to;
        if from == 0 && stamp.len > self.limits.file_bytes {
            from = stamp.len.saturating_sub(self.limits.file_bytes);
            record.skipped_head = true;
        }
        let want = stamp.len.saturating_sub(from);
        let take = want.min(budget.bytes);
        if take < want {
            budget.skipped.push("the read reached its size".to_owned());
            if take == 0 {
                return;
            }
        }
        let bytes = match read_at(path, from, take) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!(path = %path.display(), "a record could not be read: {e}");
                return;
            }
        };
        budget.bytes = budget.bytes.saturating_sub(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        // A read from inside the file starts mid-line: that line is passed over.
        let start = if from > record.read_to {
            bytes.iter().position(|&b| b == b'\n').map_or(bytes.len(), |at| at.saturating_add(1))
        } else {
            0
        };
        let end = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |at| at.saturating_add(1));
        let whole = bytes.get(start..end.max(start)).unwrap_or_default();
        let claude_dir = self.stores.claude.clone();
        let mut kept_apart = |hash: &str| paste(pastes, claude_dir.as_deref(), hash);
        for line in whole.split(|&b| b == b'\n').filter(|line| !line.is_empty()) {
            let line = String::from_utf8_lossy(line);
            record.take(&line, &mut kept_apart);
        }
        record.read_to = from.saturating_add(u64::try_from(end.max(start)).unwrap_or(u64::MAX));
        // Only a file read to its end is up to date; one cut by the budget is read on next time.
        record.read_whole = (take == want).then_some(stamp);
    }

    /// The sessions of `agents` in `folders` that `query` finds, the best `limit`.
    fn rank(
        &self,
        index: &Index,
        agents: &[Agent],
        folders: Option<&[String]>,
        query: &str,
        limit: usize,
    ) -> Vec<PastSession> {
        let rollouts: HashMap<&str, (&Path, Option<&str>)> = index
            .records
            .iter()
            .filter_map(|(path, record)| match &record.head {
                Some(Head::Rollout(meta)) => {
                    Some((meta.id.as_str(), (path.as_path(), meta.cwd.as_deref())))
                }
                _ => None,
            })
            .collect();
        let pattern =
            Pattern::new(query, CaseMatching::Smart, Normalization::Smart, AtomKind::Substring);
        let words = Words::of(query);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut chars = Vec::new();
        let mut sessions: HashMap<(Agent, &str), Session<'_>> = HashMap::new();
        for record in index.records.values() {
            let agent = record.kind.agent();
            if !agents.contains(&agent) {
                continue;
            }
            for kept in record.prompts() {
                let prompt = &kept.prompt;
                let cwd = prompt.cwd.as_deref().or_else(|| {
                    (record.kind == Kind::CodexHistory)
                        .then(|| rollouts.get(prompt.session.as_str()).and_then(|r| r.1))
                        .flatten()
                });
                if folders.is_some_and(|wanted| !cwd.is_some_and(|cwd| in_folder(cwd, wanted))) {
                    continue;
                }
                let session = sessions.entry((agent, prompt.session.as_str())).or_default();
                session.see(prompt, cwd);
                if pattern.atoms.is_empty() {
                    session.hit(prompt, 0);
                } else if let Some(score) = words
                    .admit(kept)
                    .then(|| pattern.score(Utf32Str::new(&prompt.text, &mut chars), &mut matcher))
                    .flatten()
                {
                    session.hit(prompt, score);
                }
            }
        }
        let mut found: Vec<((Agent, &str), Session<'_>)> =
            sessions.into_iter().filter(|(_, session)| !session.hits.is_empty()).collect();
        found.sort_by(|a, b| b.1.key().cmp(&a.1.key()).then_with(|| a.0.1.cmp(b.0.1)));
        found.truncate(limit);
        found
            .into_iter()
            .map(|((agent, native), session)| {
                let resume = match agent {
                    Agent::Claude => self.claude_resumes(native, session.cwd),
                    Agent::Codex if rollouts.contains_key(native) => {
                        slopty_agent::codex::shared::resume_args(native)
                    }
                    Agent::Codex => Vec::new(),
                    Agent::Pi => slopty_agent::pi::sessions::resume_args(native),
                };
                let (cwd, first, last) = (session.cwd, session.first, session.last);
                let prompts = session
                    .best(if pattern.atoms.is_empty() { 1 } else { PROMPTS_PER_SESSION })
                    .into_iter()
                    .map(|prompt| hit(&pattern, &mut matcher, prompt))
                    .collect();
                PastSession {
                    agent: agent.id(),
                    native: native.to_owned(),
                    cwd: cwd.map(str::to_owned),
                    title: first.map(|p| slopty_agent::driven::title_of(&p.text)),
                    updated_ms: last.and_then(|p| p.at_ms),
                    thread: None,
                    resume,
                    facts: std::collections::BTreeMap::new(),
                    prompts,
                }
            })
            .collect()
    }

    /// The words that take Claude Code session `native` up again, when its transcript is
    /// there to take up: looked at, never opened.
    fn claude_resumes(&self, native: &str, cwd: Option<&str>) -> Vec<String> {
        let (Some(dir), Some(cwd)) = (self.stores.claude.as_deref(), cwd) else {
            return Vec::new();
        };
        let escaped = slopty_agent::discover::escape(Path::new(cwd));
        let transcript = dir.join("projects").join(escaped).join(format!("{native}.jsonl"));
        if slopty_agent::resume::is_session_id(native) && transcript.is_file() {
            vec![slopty_agent::resume::RESUME_FLAG.to_owned(), native.to_owned()]
        } else {
            Vec::new()
        }
    }
}

impl Record {
    const fn new(kind: Kind, ino: u64) -> Self {
        Self {
            kind,
            ino,
            read_whole: None,
            read_to: 0,
            skipped_head: false,
            head: None,
            prompts: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Keep the prompt one whole `line` records, if it records one.
    fn take(&mut self, line: &str, kept_apart: &mut dyn FnMut(&str) -> Option<String>) {
        match (self.kind, &self.head) {
            (Kind::ClaudeHistory, _) => {
                self.prompts.extend(claude::prompt(line, kept_apart).map(Kept::new));
            }
            (Kind::CodexHistory, _) => self.prompts.extend(codex::history(line).map(Kept::new)),
            (Kind::CodexRollout, Some(Head::Rollout(meta))) => match codex::rollout(line, meta) {
                Some((prompt, codex::Said::Event)) => self.prompts.push(Kept::new(prompt)),
                Some((prompt, codex::Said::Item)) => self.items.push(Kept::new(prompt)),
                None => {}
            },
            (Kind::PiSession, Some(Head::Pi { id, cwd })) => {
                self.prompts.extend(pi::prompt(line, id, cwd).map(Kept::new));
            }
            _ => {}
        }
    }

    /// Its prompts: a rollout's events where it has them, else its input items.
    fn prompts(&self) -> &[Kept] {
        if self.kind == Kind::CodexRollout && self.prompts.is_empty() {
            &self.items
        } else {
            &self.prompts
        }
    }
}

/// A prompt as the index keeps it.
#[derive(Debug)]
struct Kept {
    prompt: Prompt,
    /// Its text as the matcher reads it for a word in lower case: by grapheme, accents
    /// folded, lower case. What [`Words`] looks in.
    folded: Box<str>,
}

impl Kept {
    fn new(prompt: Prompt) -> Self {
        let text = prompt.text.as_str();
        let folded = if text.is_ascii() {
            text.to_ascii_lowercase()
        } else {
            graphemes(text).map(|c| to_lower_case(normalize(c))).collect()
        };
        Self { prompt, folded: folded.into() }
    }
}

/// The words of a query, lower case, to pass over a prompt before the matcher scores it: a
/// prompt that lacks one cannot match. On this machine's records (`docs/MEASUREMENTS.md`, "Prompt
/// search") scoring every prompt took 45 ms a search; with the words, a rare word takes 3 to 5 ms.
struct Words(Option<Vec<String>>);

impl Words {
    /// `None` inside when the query has something only the matcher can judge: a character
    /// beyond ASCII, which it matches unfolded, or an escape.
    fn of(query: &str) -> Self {
        let plain = query.is_ascii() && !query.contains('\\');
        Self(plain.then(|| query.split_whitespace().map(str::to_ascii_lowercase).collect()))
    }

    /// Whether the matcher may find the words in `kept`.
    fn admit(&self, kept: &Kept) -> bool {
        self.0.as_ref().is_none_or(|words| words.iter().all(|w| kept.folded.contains(w.as_str())))
    }
}

/// One session as a search sees it.
#[derive(Default)]
struct Session<'a> {
    /// The folder it ran in, from its last prompt that says.
    cwd: Option<&'a str>,
    first: Option<&'a Prompt>,
    last: Option<&'a Prompt>,
    /// Its prompts that matched, with their scores.
    hits: Vec<(u32, &'a Prompt)>,
}

impl<'a> Session<'a> {
    fn see(&mut self, prompt: &'a Prompt, cwd: Option<&'a str>) {
        if self.first.is_none_or(|first| prompt.at_ms < first.at_ms) {
            self.first = Some(prompt);
        }
        if self.last.is_none_or(|last| prompt.at_ms >= last.at_ms) {
            self.last = Some(prompt);
            if cwd.is_some() {
                self.cwd = cwd;
            }
        }
        if self.cwd.is_none() {
            self.cwd = cwd;
        }
    }

    fn hit(&mut self, prompt: &'a Prompt, score: u32) {
        self.hits.push((score, prompt));
    }

    /// Its best score, then its newest prompt's time.
    fn key(&self) -> (u32, Option<WallMs>) {
        let best = self.hits.iter().map(|(score, _)| *score).max().unwrap_or_default();
        let newest = self.hits.iter().filter_map(|(_, p)| p.at_ms).max();
        (best, newest)
    }

    /// Its best `most` prompts, the newer first among equals.
    fn best(mut self, most: usize) -> Vec<&'a Prompt> {
        self.hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.at_ms.cmp(&a.1.at_ms)));
        self.hits.into_iter().take(most).map(|(_, p)| p).collect()
    }
}

/// `prompt` as a hit for `pattern`: cut to [`PROMPT_HIT_BYTES`] round its first match, the
/// matches marked.
fn hit(pattern: &Pattern, matcher: &mut Matcher, prompt: &Prompt) -> PromptHit {
    let text = prompt.text.as_str();
    let mut chars = Vec::new();
    let mut at = Vec::new();
    let _score = pattern.indices(Utf32Str::new(text, &mut chars), matcher, &mut at);
    at.sort_unstable();
    at.dedup();
    let spans = spans_of(text, &at);
    let first = spans.first().map_or(0, |span| usize::try_from(span.start).unwrap_or(0));
    let (start, end) = window(text, first);
    let shown = text.get(start..end).unwrap_or_default().to_owned();
    let spans = spans
        .into_iter()
        .filter_map(|span| {
            let (s, e) = (to_usize(span.start).max(start), to_usize(span.end).min(end));
            (s < e).then(|| Span {
                start: to_u32(s.saturating_sub(start)),
                end: to_u32(e.saturating_sub(start)),
            })
        })
        .collect();
    PromptHit {
        text: shown,
        spans,
        cut_before: start > 0,
        cut_after: end < text.len(),
        at_ms: prompt.at_ms,
    }
}

/// The byte spans of the characters at `chars` (sorted, unique) in `text`, runs joined.
fn spans_of(text: &str, chars: &[u32]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut wanted = chars.iter().peekable();
    for (n, (at, c)) in text.char_indices().enumerate() {
        let Some(&&next) = wanted.peek() else { break };
        if usize::try_from(next).ok() != Some(n) {
            continue;
        }
        wanted.next();
        let (start, end) = (to_u32(at), to_u32(at.saturating_add(c.len_utf8())));
        match spans.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => spans.push(Span { start, end }),
        }
    }
    spans
}

/// The bytes of `text` a hit shows: [`PROMPT_HIT_BYTES`] from a little before `first`, on
/// characters' boundaries.
fn window(text: &str, first: usize) -> (usize, usize) {
    if text.len() <= PROMPT_HIT_BYTES {
        return (0, text.len());
    }
    let mut start =
        first.saturating_sub(LEAD_BYTES).min(text.len().saturating_sub(PROMPT_HIT_BYTES));
    while !text.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let mut end = start.saturating_add(PROMPT_HIT_BYTES).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (start, end)
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn to_usize(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

/// The folders a search for `cwd` takes as it: as asked with `~` expanded, and as the system
/// resolves it, since an agent records the folder it ran in resolved. `None` for every folder.
fn wanted_folders(cwd: Option<&str>) -> Option<Vec<String>> {
    let asked = crate::file::expand_home(Path::new(cwd?));
    let mut folders = vec![trimmed(&asked.to_string_lossy()).to_owned()];
    if let Ok(resolved) = fs::canonicalize(&asked) {
        folders.push(trimmed(&resolved.to_string_lossy()).to_owned());
    }
    Some(folders)
}

fn in_folder(cwd: &str, wanted: &[String]) -> bool {
    let cwd = trimmed(cwd);
    wanted.iter().any(|w| w == cwd)
}

fn trimmed(path: &str) -> &str {
    match path.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    }
}

/// Why a search may lack sessions, from what its read `skipped` and the records whose heads
/// were too large to read.
fn cut_words(skipped: &mut Vec<String>, index: &Index, agents: &[Agent]) -> Option<String> {
    for agent in agents {
        let large = index
            .records
            .values()
            .any(|record| record.kind.agent() == *agent && record.skipped_head);
        if large {
            skipped.push(format!("{}'s oldest prompts are past what is read", agent.name()));
        }
    }
    skipped.dedup();
    (!skipped.is_empty()).then(|| {
        let mut words = skipped.join("; ");
        if let Some(first) = words.get(..1) {
            words = format!("{}{}", first.to_uppercase(), words.get(1..).unwrap_or_default());
        }
        words
    })
}

/// A paste Claude Code kept apart, by its hash: read once, at most a prompt's worth.
fn paste(
    pastes: &mut HashMap<String, Option<String>>,
    claude: Option<&Path>,
    hash: &str,
) -> Option<String> {
    if let Some(text) = pastes.get(hash) {
        return text.clone();
    }
    let read = claude.and_then(|dir| {
        let path = dir.join(claude::PASTES).join(format!("{hash}.txt"));
        let limit = u64::try_from(history::PROMPT_BYTES).unwrap_or(u64::MAX);
        read_at(&path, 0, limit).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    });
    pastes.insert(hash.to_owned(), read.clone());
    read
}

/// The `len` bytes of the file at `path` from `from`, or fewer where it ends.
fn read_at(path: &Path, from: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0).min(64 * 1024 * 1024));
    file.take(len).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The first line of the file at `path`, from its first [`HEAD_BYTES`].
fn first_line(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut line = Vec::new();
    BufReader::new(file.take(HEAD_BYTES)).read_until(b'\n', &mut line).ok()?;
    (line.last() == Some(&b'\n')).then(|| String::from_utf8_lossy(&line).into_owned())
}

/// The entries of directory `dir`, or none.
fn children(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Codex's rollouts under `dir`, by day (`YYYY/MM/DD/rollout-*.jsonl`), or directly in it.
fn rollouts(dir: &Path) -> Vec<PathBuf> {
    let is_rollout =
        |path: &Path| path.file_name().and_then(|n| n.to_str()).and_then(codex::id_of).is_some();
    let mut found = Vec::new();
    for entry in children(dir) {
        if is_rollout(&entry) {
            found.push(entry);
            continue;
        }
        for month in children(&entry) {
            for day in children(&month) {
                found.extend(children(&day).into_iter().filter(|path| is_rollout(path)));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words pass over a prompt only when the matcher would too: case, accents and an
    /// escape left to the matcher.
    #[test]
    fn the_words_pass_over_only_what_cannot_match() {
        let kept = |text: &str| {
            Kept::new(Prompt { session: "s".into(), cwd: None, at_ms: None, text: text.into() })
        };
        let matches = |query: &str, text: &str| {
            let pattern =
                Pattern::new(query, CaseMatching::Smart, Normalization::Smart, AtomKind::Substring);
            let mut chars = Vec::new();
            pattern
                .score(Utf32Str::new(text, &mut chars), &mut Matcher::new(Config::DEFAULT))
                .is_some()
        };
        for (query, text) in [
            ("login test", "Fix the Login TEST"),
            ("Login", "fix the login"),
            ("cafe", "the café menu"),
            ("café", "the cafe menu"),
            ("a\\ b", "a b"),
            ("e", "CAFÉ"),
            ("uber", "Über alles"),
            ("zz", "nothing here"),
        ] {
            let admitted = Words::of(query).admit(&kept(text));
            assert!(admitted || !matches(query, text), "{query:?} in {text:?}");
        }
        assert!(!Words::of("zz").admit(&kept("nothing here")), "an ASCII miss is passed over");
    }

    /// Matches become byte spans, runs joined, multi-byte characters whole; a long prompt is
    /// shown round its first match, the spans moved with it.
    #[test]
    fn a_hit_marks_its_matches_round_the_first() {
        assert_eq!(
            spans_of("añb cd", &[1, 2, 4]),
            [Span { start: 1, end: 4 }, Span { start: 5, end: 6 }]
        );
        let pattern =
            Pattern::new("needle", CaseMatching::Smart, Normalization::Smart, AtomKind::Substring);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let text = format!("{}needle{}", "a".repeat(1_000), "b".repeat(1_000));
        let prompt = Prompt { session: "s".into(), cwd: None, at_ms: None, text };
        let hit = hit(&pattern, &mut matcher, &prompt);
        assert!(hit.cut_before && hit.cut_after);
        assert_eq!(hit.text.len(), PROMPT_HIT_BYTES);
        let span = hit.spans.first().copied().expect("a span");
        assert_eq!(hit.text.get(to_usize(span.start)..to_usize(span.end)), Some("needle"));
        assert_eq!(to_usize(span.start), LEAD_BYTES);
    }
}
