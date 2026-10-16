//! Where Claude Code keeps a session's record, and the sessions it kept for a folder.
//!
//! Claude Code writes one transcript per session under
//! `<config>/projects/<escaped working directory>/<session id>.jsonl`, each subagent's beside it
//! (`<session id>/subagents/agent-<id>.jsonl`), where the escaping replaces every character that
//! is not an ASCII letter or digit with a `-` (so `/Users/x/.config` becomes `-Users-x--config`).
//! The driven Claude Code is given its session id, so its record is found by that id and its
//! folder, never guessed.
//!
//! Each live Claude Code also records itself in `<config>/sessions/<pid>.json` ([`live`]): its
//! pid, the session it holds, its folder, how it runs (`kind`) and its process's start
//! (`procStart`, `ps -o lstart=` in UTC), which tells a pid used again from the one that wrote.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{
    EXPANDED_CHARS, Expanded, PROMPT_HIT_BYTES, PastSession, PromptHit,
};
use slopty_proto::thread::{AgentId, ContentRef};

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
/// [`Expanded::Gone`] when no file holds it any more.
#[must_use]
pub fn expand(home: &Path, cwd: &Path, session: &str, content: &ContentRef) -> Expanded {
    let Some((_thread, at)) = super::history::text_ref(content) else { return Expanded::Gone };
    let image = matches!(at.part, Part::Image { .. });
    for file in files(home, cwd, session) {
        let Ok(jsonl) = std::fs::read_to_string(&file) else { continue };
        if image {
            if let Some(bytes) = crate::conversation::image_bytes(&jsonl, &at) {
                return Expanded::Bytes(bytes);
            }
        } else if let Some(mut text) = crate::conversation::full_text(&jsonl, &at) {
            if let Some((cut, _)) = text.char_indices().nth(EXPANDED_CHARS) {
                text.truncate(cut);
            }
            return Expanded::Text(text);
        }
    }
    Expanded::Gone
}

/// How much of a transcript's head, and of its tail, a listing reads.
///
/// Claude Code writes the first prompt at the head and appends a session's title, last prompt
/// and branch as it goes, so the two ends say what its own resume picker shows without the
/// middle being read.
pub const GLANCE_BYTES: u64 = 64 * 1024;

/// Claude Code's past sessions for an agent run in `cwd`, the last written first, at most
/// `limit`.
///
/// Each comes from its transcript's name and modification time, and what the two ends of the
/// transcript say of it ([`GLANCE_BYTES`]): its title (the one the person gave it, else Claude
/// Code's own, else its summary, else its first prompt), its last prompt, and the branch it was
/// on (the fact [`BRANCH_FACT`](super::history::BRANCH_FACT)). A transcript that cannot be read
/// is listed by its name alone. A folder Claude Code never ran in has none.
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
    Ok(found
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
                updated_ms: modified.map(WallMs::of),
                thread: None,
                resume: vec![super::RESUME_FLAG.to_owned(), id.clone()],
                facts,
                native: id,
            }
        })
        .collect())
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
}

impl Glance {
    /// The session's title, as Claude Code's resume picker shows it.
    fn title(&self) -> Option<String> {
        [&self.named, &self.titled, &self.summary, &self.first]
            .into_iter()
            .flatten()
            .map(|t| t.trim())
            .find(|t| !t.is_empty())
            .map(str::to_owned)
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
    /// The session it holds: the one whose transcript it writes.
    pub session: String,
    /// The folder it runs in.
    pub cwd: Option<String>,
    /// How it runs: `interactive` in a terminal, else how it runs in the background (`bg`,
    /// `daemon`, `daemon-worker`).
    pub kind: String,
    /// Its process's start as `ps -o lstart=` says it in UTC, when it said.
    pub started: Option<String>,
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
    use std::io::Read as _;
    let Ok(entries) = std::fs::read_dir(config_dir(home).join(LIVE_DIR)) else {
        return Vec::new();
    };
    let mut found: Vec<Live> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|entry| {
            let mut bytes = Vec::new();
            std::fs::File::open(entry.path())
                .ok()?
                .take(LIVE_BYTES)
                .read_to_end(&mut bytes)
                .ok()?;
            let record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            let text = |key: &str| record.get(key).and_then(|v| v.as_str()).map(str::to_owned);
            let pid = u32::try_from(record.get("pid")?.as_u64()?).ok().filter(|p| *p > 0)?;
            let session = text("sessionId").filter(|s| super::is_session_id(s))?;
            Some(Live {
                pid,
                session,
                cwd: text("cwd"),
                kind: text("kind")
                    .filter(|k| !k.is_empty())
                    .unwrap_or_else(|| "interactive".to_owned()),
                started: text("procStart"),
            })
        })
        .collect();
    found.sort_by_key(|live| live.pid);
    found
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
}
