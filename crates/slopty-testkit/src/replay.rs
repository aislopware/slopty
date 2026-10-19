//! A recorded Claude Code turn, played again by the stand-in `claude` (`STUB_REPLAY`) as its own.
//!
//! The recordings are the conversation fixtures of `slopty-agent`
//! (`tests/fixtures/conversation/<name>`, each a `transcript.jsonl`, the `hooks.jsonl` that ran
//! beside it and any `subagents/agent-<id>.jsonl`). The driven stand-in
//! ([`crate::claude_stream`]) streams them over stream-json in the order Claude Code wrote them,
//! so a worker and an app see a real turn: tool groups, a task list, a subagent, a question.
//!
//! [`Recording::load`] reads a fixture; [`Recording::schedule`] makes it the stand-in's: every
//! session id, transcript path, folder and record id rewritten to the stand-in's own session, the
//! recorded prompt to the one it was given, the times moved so the turn starts now, and the steps
//! put in Claude Code's order. The recorded hooks only order the steps: a tool's call is written
//! before its `PreToolUse`, its `PostToolUse` comes before its result, a subagent's lines go to
//! its own file under the session's `subagents` folder, and `Stop` comes once the last answer is
//! written. A `PermissionRequest` is a step that waits (`Step::Hook`'s `waits`): the stand-in
//! asks for it as a `can_use_tool` and holds the turn there until the person answers.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where the fixtures name record ids: every id they carry starts so, the rest is a counter.
const FIXTURE_ID_PREFIX: &str = "00000000-0000-4000-8000-";

/// The conversation fixtures a replay may be named by, beside this crate in the checkout.
#[must_use]
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-agent/tests/fixtures/conversation")
}

/// The fixture `named`: a folder as it is, else one of [`fixtures`] by name.
#[must_use]
pub fn fixture(named: &str) -> PathBuf {
    let path = PathBuf::from(named);
    if path.is_dir() { path } else { fixtures().join(named) }
}

/// One recorded conversation: its transcript's lines, the hooks that ran, its subagents' lines.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    /// The main transcript, a record a line.
    pub lines: Vec<Value>,
    /// Each hook's payload, in the order it ran.
    pub hooks: Vec<Value>,
    /// Each subagent's file name (`agent-<id>.jsonl`) and its records.
    pub subagents: Vec<(String, Vec<Value>)>,
}

/// The stand-in's own session, which a replay is made to belong to.
#[derive(Debug, Clone)]
pub struct Session {
    /// The conversation it was started on (`--session-id`).
    pub id: String,
    /// Its conversation file, where the records are kept.
    pub transcript: PathBuf,
    /// The folder it runs in.
    pub cwd: String,
    /// The prompt it was given.
    pub prompt: String,
    /// Now, in ms since the epoch: when the replayed turn starts.
    pub now_ms: u64,
}

/// One thing the stand-in does in a replayed turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Append these records, a JSON document a line, to the file at `to` in one write.
    Write {
        /// The transcript: the session's own or a subagent's.
        to: PathBuf,
        /// The records.
        lines: Vec<String>,
    },
    /// A point the recorded hooks mark: the payload Claude Code sent there.
    Hook {
        /// The payload, as Claude Code would send it in this session.
        payload: Value,
        /// The turn holds here until the person answers (a `PermissionRequest`).
        waits: bool,
    },
}

impl Recording {
    /// Read the fixture in `dir`.
    ///
    /// # Errors
    ///
    /// When its transcript or hooks cannot be read or a line is not JSON.
    pub fn load(dir: &Path) -> io::Result<Self> {
        let lines = jsonl(&dir.join("transcript.jsonl"))?;
        let hooks = jsonl(&dir.join("hooks.jsonl"))?
            .into_iter()
            .map(|line| line.get("input").cloned().unwrap_or(line))
            .collect();
        let mut subagents = Vec::new();
        if let Ok(read) = std::fs::read_dir(dir.join("subagents")) {
            let mut files: Vec<PathBuf> = read.filter_map(|e| Some(e.ok()?.path())).collect();
            files.sort();
            for file in files {
                let Some(name) = file.file_name().and_then(|n| n.to_str()) else { continue };
                if Path::new(name).extension().is_some_and(|ext| ext == "jsonl") {
                    subagents.push((name.to_owned(), jsonl(&file)?));
                }
            }
        }
        Ok(Self { lines, hooks, subagents })
    }

    /// The recording as `to`'s turn: its steps in Claude Code's order (the module's). The
    /// recorded `SessionStart` is left out: the stand-in fired its own when it started.
    #[must_use]
    pub fn schedule(&self, to: &Session) -> Vec<Step> {
        let rewrite = Rewrite::new(self, to);
        let main = to.transcript.clone();
        let subagents_dir = to.transcript.with_extension("").join("subagents");
        let mut streams: Vec<Stream> = vec![Stream::new(main, &self.lines, &rewrite)];
        for (name, lines) in &self.subagents {
            streams.push(Stream::new(subagents_dir.join(name), lines, &rewrite));
        }
        let mut steps = Vec::new();
        let mut prompted = false;
        // Each call's input and id, by its `PreToolUse`: what a permission request is about.
        let mut called: Vec<(Value, Value)> = Vec::new();
        for hook in &self.hooks {
            let event = hook.get("hook_event_name").and_then(Value::as_str).unwrap_or_default();
            let mut payload = rewrite.value(hook);
            let tool = hook.get("tool_use_id").and_then(Value::as_str);
            match event {
                "SessionStart" => continue,
                "UserPromptSubmit" => {
                    steps.push(Step::Hook { payload, waits: false });
                    if !prompted {
                        prompted = true;
                        if let Some(main) = streams.first_mut() {
                            main.through(&mut steps, is_prompt);
                        }
                    }
                    continue;
                }
                "PreToolUse" => {
                    if let Some(id) = payload.get("tool_use_id") {
                        let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
                        called.push((input, id.clone()));
                    }
                    if let Some(id) = tool
                        && let Some(stream) = streams.iter_mut().find(|s| s.calls(id))
                    {
                        stream.through(&mut steps, |line| calls(line, id));
                    }
                }
                "SubagentStop" => {
                    let file = hook.get("agent_id").and_then(Value::as_str).map(agent_file);
                    let stream = file.and_then(|file| {
                        streams.iter_mut().find(|s| s.to.file_name() == Some(file.as_ref()))
                    });
                    if let Some(stream) = stream {
                        stream.rest(&mut steps);
                    }
                }
                "Stop" => {
                    if let Some(main) = streams.first_mut() {
                        let last = main.lines.iter().rposition(|(line, _)| answers(line));
                        if let Some(last) = last {
                            main.upto(&mut steps, last);
                        }
                    }
                }
                // Claude Code's hook names no call, where its `can_use_tool` does: the call asked
                // about is the last one called with its input.
                "PermissionRequest" if payload.get("tool_use_id").is_none() => {
                    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
                    let id = called.iter().rev().find(|(called, _)| *called == input);
                    if let (Some((_, id)), Some(fields)) = (id, payload.as_object_mut()) {
                        fields.insert("tool_use_id".to_owned(), id.clone());
                    }
                }
                _ => {}
            }
            let waits = event == "PermissionRequest";
            steps.push(Step::Hook { payload, waits });
        }
        for stream in &mut streams {
            stream.rest(&mut steps);
        }
        steps
    }
}

/// A subagent's transcript file, by its agent id.
fn agent_file(id: &str) -> String {
    format!("agent-{id}.jsonl")
}

/// One transcript being played: its records, rewritten and rendered, and how far it is written.
struct Stream {
    to: PathBuf,
    /// Each record as it is, to match on, and as it is written.
    lines: Vec<(Value, String)>,
    written: usize,
}

impl Stream {
    fn new(to: PathBuf, lines: &[Value], rewrite: &Rewrite) -> Self {
        let lines = lines
            .iter()
            .map(|line| {
                let line = rewrite.value(line);
                let text = line.to_string();
                (line, text)
            })
            .collect();
        Self { to, lines, written: 0 }
    }

    /// Whether a record still to be written makes call `id`.
    fn calls(&self, id: &str) -> bool {
        self.lines.iter().skip(self.written).any(|(line, _)| calls(line, id))
    }

    /// Write every record up to and with the first one still to be written that `matches`;
    /// nothing when none does.
    fn through(&mut self, steps: &mut Vec<Step>, matches: impl Fn(&Value) -> bool) {
        let found = self.lines.iter().skip(self.written).position(|(line, _)| matches(line));
        if let Some(found) = found.and_then(|at| at.checked_add(self.written)) {
            self.upto(steps, found);
        }
    }

    /// Write every record up to and with the one at `last`.
    fn upto(&mut self, steps: &mut Vec<Step>, last: usize) {
        let end = last.saturating_add(1).min(self.lines.len());
        if end <= self.written {
            return;
        }
        let lines =
            self.lines.iter().take(end).skip(self.written).map(|(_, t)| t.clone()).collect();
        steps.push(Step::Write { to: self.to.clone(), lines });
        self.written = end;
    }

    /// Write every record left.
    fn rest(&mut self, steps: &mut Vec<Step>) {
        if let Some(last) = self.lines.len().checked_sub(1) {
            self.upto(steps, last);
        }
    }
}

/// The person's prompt: a user record whose content is words, not tool results.
fn is_prompt(line: &Value) -> bool {
    line.get("type").and_then(Value::as_str) == Some("user")
        && line.pointer("/message/content").is_some_and(Value::is_string)
}

/// An answer of the agent's: an assistant record with words in it.
fn answers(line: &Value) -> bool {
    line.get("type").and_then(Value::as_str) == Some("assistant")
        && line.pointer("/message/content").and_then(Value::as_array).is_some_and(|parts| {
            parts.iter().any(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        })
}

/// Whether `line` is the assistant record that makes call `id`.
fn calls(line: &Value, id: &str) -> bool {
    line.get("type").and_then(Value::as_str) == Some("assistant")
        && line.pointer("/message/content").and_then(Value::as_array).is_some_and(|parts| {
            parts.iter().any(|p| {
                p.get("type").and_then(Value::as_str) == Some("tool_use")
                    && p.get("id").and_then(Value::as_str) == Some(id)
            })
        })
}

/// What a recording's words become in the stand-in's session.
struct Rewrite {
    /// The recorded transcript's path without `.jsonl`, and the session's.
    stem: Option<(String, String)>,
    /// The recorded session id, and the session's.
    session: Option<(String, String)>,
    /// The recorded folder, and the session's.
    cwd: Option<(String, String)>,
    /// The recorded prompt, and the session's.
    prompt: Option<(String, String)>,
    /// The record ids' prefix in the session: its own id's first 24 characters.
    ids: String,
    /// How far the recorded times move, in ms, so the turn starts now.
    shift: i128,
}

impl Rewrite {
    fn new(rec: &Recording, to: &Session) -> Self {
        let field =
            |name: &str| rec.hooks.iter().find_map(|h| h.get(name)?.as_str().map(str::to_owned));
        let recorded_session = field("session_id").or_else(|| {
            rec.lines.iter().find_map(|l| l.get("sessionId")?.as_str().map(str::to_owned))
        });
        let stem = field("transcript_path").map(|path| {
            let new = to.transcript.with_extension("").to_string_lossy().into_owned();
            (path.strip_suffix(".jsonl").unwrap_or(&path).to_owned(), new)
        });
        let prompt = rec.hooks.iter().find_map(|h| {
            (h.get("hook_event_name")?.as_str()? == "UserPromptSubmit")
                .then(|| h.get("prompt")?.as_str().map(str::to_owned))
                .flatten()
        });
        let first =
            rec.lines.iter().find_map(|l| l.get("timestamp")?.as_str().and_then(parse_stamp));
        let shift =
            first.map_or(0, |first| i128::from(to.now_ms).saturating_sub(i128::from(first)));
        let ids = to.id.get(..FIXTURE_ID_PREFIX.len()).unwrap_or(FIXTURE_ID_PREFIX).to_owned();
        Self {
            stem,
            session: recorded_session.map(|s| (s, to.id.clone())),
            cwd: field("cwd").map(|cwd| (cwd, to.cwd.clone())),
            prompt: prompt.map(|p| (p, to.prompt.clone())),
            ids,
            shift,
        }
    }

    /// `value` with every string in it rewritten, and its time moved.
    fn value(&self, value: &Value) -> Value {
        let mut value = value.clone();
        self.walk(&mut value, None);
        value
    }

    fn walk(&self, value: &mut Value, key: Option<&str>) {
        match value {
            Value::String(s) => {
                if key == Some("timestamp")
                    && let Some(moved) = parse_stamp(s).and_then(|at| self.moved(at))
                {
                    *s = stamp(moved);
                } else if key == Some("lastPrompt")
                    && let Some((_, new)) = &self.prompt
                {
                    // Claude Code keeps the prompt's first words here, cut short.
                    s.clone_from(new);
                } else {
                    *s = self.text(s);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| self.walk(item, None)),
            Value::Object(fields) => {
                fields.iter_mut().for_each(|(k, v)| self.walk(v, Some(k.as_str())));
            }
            _ => {}
        }
    }

    fn moved(&self, at: u64) -> Option<u64> {
        u64::try_from(i128::from(at).checked_add(self.shift)?).ok()
    }

    /// One string of the recording as the session's.
    fn text(&self, s: &str) -> String {
        if let Some((old, new)) = &self.prompt
            && s == old
        {
            return new.clone();
        }
        let mut s = s.to_owned();
        if let Some((old, new)) = &self.stem {
            s = s.replace(old.as_str(), new);
        }
        if let Some((old, new)) = &self.session {
            s = s.replace(old.as_str(), new);
        }
        if let Some((old, new)) = &self.cwd {
            if s == *old {
                s.clone_from(new);
            } else {
                s = s.replace(&format!("{old}/"), &format!("{new}/"));
            }
        }
        s.replace(FIXTURE_ID_PREFIX, &self.ids)
    }
}

/// Every line of the JSONL file at `at`, each a document.
fn jsonl(at: &Path) -> io::Result<Vec<Value>> {
    let text = std::fs::read_to_string(at)?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(io::Error::other))
        .collect()
}

/// `ms` since the epoch as Claude Code stamps a record, in UTC to the millisecond
/// (`2026-10-13T05:46:04.748Z`).
#[must_use]
pub fn stamp(ms: u64) -> String {
    let (days, of_day) = (ms / 86_400_000, ms % 86_400_000);
    let (year, month, day) = civil(days);
    let (hour, minute) = (of_day / 3_600_000, of_day / 60_000 % 60);
    let (second, milli) = (of_day / 1000 % 60, of_day % 1000);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// A record's stamp (`2026-10-13T05:46:04.748Z`) as ms since the epoch.
#[must_use]
pub fn parse_stamp(text: &str) -> Option<u64> {
    let (date, time) = text.strip_suffix('Z')?.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<u64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, milli) = time.split_once('.').unwrap_or((time, "0"));
    let mut clock = clock.splitn(3, ':').map(str::parse::<u64>);
    let (hour, minute, second) = (clock.next()?.ok()?, clock.next()?.ok()?, clock.next()?.ok()?);
    let milli: u64 = milli.get(..milli.len().min(3))?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    secs.checked_mul(1000)?.checked_add(milli)
}

/// Days since 1970-01-01 as a civil date (Howard Hinnant's `civil_from_days`), for dates after
/// 1970.
fn civil(days: u64) -> (u64, u64, u64) {
    let z = days.saturating_add(719_468);
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe =
        doe.saturating_sub(doe / 1460).saturating_add(doe / 36_524).saturating_sub(doe / 146_096)
            / 365;
    let doy = doe
        .saturating_sub(yoe.saturating_mul(365).saturating_add(yoe / 4).saturating_sub(yoe / 100));
    let mp = doy.saturating_mul(5).saturating_add(2) / 153;
    let day = doy.saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5).saturating_add(1);
    let month = if mp < 10 { mp.saturating_add(3) } else { mp.saturating_sub(9) };
    let year = era.saturating_mul(400).saturating_add(yoe).saturating_add(u64::from(month <= 2));
    (year, month, day)
}

/// A civil date as days since 1970-01-01 (Howard Hinnant's `days_from_civil`); `None` before.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    let year = if month <= 2 { year.checked_sub(1)? } else { year };
    let (era, yoe) = (year / 400, year % 400);
    let mp = if month > 2 { month.checked_sub(3)? } else { month.checked_add(9)? };
    let doy = (mp.checked_mul(153)?.checked_add(2)? / 5).checked_add(day.checked_sub(1)?)?;
    let doe =
        yoe.checked_mul(365)?.checked_add(yoe / 4)?.checked_sub(yoe / 100)?.checked_add(doy)?;
    era.checked_mul(146_097)?.checked_add(doe)?.checked_sub(719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "5f3c2a10-1111-4222-8333-944455566677";

    fn session(prompt: &str) -> Session {
        Session {
            id: SESSION.to_owned(),
            transcript: PathBuf::from(format!("/tmp/t/{SESSION}.jsonl")),
            cwd: "/repo".to_owned(),
            prompt: prompt.to_owned(),
            now_ms: parse_stamp("2026-10-16T09:00:00.000Z").unwrap(),
        }
    }

    fn played(name: &str) -> (Recording, Vec<Step>) {
        let rec = Recording::load(&fixture(name)).unwrap();
        let steps = rec.schedule(&session("Survey the files"));
        (rec, steps)
    }

    fn events(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .filter_map(|s| match s {
                Step::Hook { payload, .. } => {
                    Some(payload["hook_event_name"].as_str().unwrap().to_owned())
                }
                Step::Write { .. } => None,
            })
            .collect()
    }

    /// Every record and every hook payload belongs to the stand-in's session: its id, its
    /// transcript, its folder, ids under its own prefix, and the prompt it was given.
    #[test]
    fn a_replay_is_rewritten_to_the_session_that_plays_it() {
        let (_, steps) = played("tools");
        let all: String = steps
            .iter()
            .map(|s| match s {
                Step::Write { lines, .. } => lines.join("\n"),
                Step::Hook { payload, .. } => payload.to_string(),
            })
            .collect();
        assert!(!all.contains("00000000-0000-4000-8000-000000000001"), "the recorded session");
        assert!(!all.contains(FIXTURE_ID_PREFIX), "a recorded id");
        assert!(!all.contains("/home/user/.claude"), "the recorded transcript");
        assert!(!all.contains("\"/work\"") && !all.contains("/work/"), "the recorded folder");
        assert!(all.contains("/repo/main.rs") || all.contains("/repo/notes.md"));
        assert!(!all.contains("Do these steps in order"), "the recorded prompt");
        let hooks: Vec<&Value> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Hook { payload, .. } => Some(payload),
                Step::Write { .. } => None,
            })
            .collect();
        assert!(hooks.iter().all(|h| h["session_id"] == SESSION));
        assert!(hooks.iter().all(|h| h["cwd"] == "/repo"));
        let transcript = format!("/tmp/t/{SESSION}.jsonl");
        assert!(hooks.iter().all(|h| h["transcript_path"] == transcript.as_str()));
        let prompt = hooks.iter().find(|h| h["hook_event_name"] == "UserPromptSubmit").unwrap();
        assert_eq!(prompt["prompt"], "Survey the files");
        let stop = hooks.iter().find(|h| h["hook_event_name"] == "SubagentStop").unwrap();
        assert_eq!(
            stop["agent_transcript_path"],
            format!("/tmp/t/{SESSION}/subagents/agent-a0000000000000001.jsonl")
        );
    }

    /// The turn starts now: the first record is stamped with the session's now, and the gaps
    /// between records are the recorded ones.
    #[test]
    fn a_replay_starts_now_and_keeps_its_pace() {
        let (rec, steps) = played("tools");
        let stamps: Vec<u64> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Write { lines, .. } => Some(lines.clone()),
                Step::Hook { .. } => None,
            })
            .flatten()
            .filter_map(|l| {
                let line: Value = serde_json::from_str(&l).unwrap();
                parse_stamp(line.get("timestamp")?.as_str()?)
            })
            .collect();
        let recorded: Vec<u64> =
            rec.lines.iter().filter_map(|l| parse_stamp(l.get("timestamp")?.as_str()?)).collect();
        assert_eq!(stamps.first().copied(), Some(session("").now_ms));
        let last = |v: &[u64]| v.iter().max().unwrap().saturating_sub(*v.iter().min().unwrap());
        assert!(last(&recorded) > 0);
        assert!(stamps.len() >= recorded.len());
    }

    /// Nothing is lost or written twice, and the main transcript keeps its order.
    #[test]
    fn every_record_is_written_once_in_its_order() {
        let (rec, steps) = played("tools");
        let main = PathBuf::from(format!("/tmp/t/{SESSION}.jsonl"));
        let written: Vec<String> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Write { to, lines } if *to == main => Some(lines.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(written.len(), rec.lines.len());
        let uuids: Vec<String> = written
            .iter()
            .filter_map(|l| {
                let line: Value = serde_json::from_str(l).unwrap();
                line.get("uuid")?.as_str().map(str::to_owned)
            })
            .collect();
        let recorded: Vec<String> = rec
            .lines
            .iter()
            .filter_map(|l| {
                let uuid = l.get("uuid")?.as_str()?;
                Some(uuid.replace(
                    FIXTURE_ID_PREFIX,
                    SESSION.get(..FIXTURE_ID_PREFIX.len()).unwrap_or(SESSION),
                ))
            })
            .collect();
        assert_eq!(uuids, recorded);
        let sub: usize = steps
            .iter()
            .filter_map(|s| match s {
                Step::Write { to, lines } if to.parent().unwrap().ends_with("subagents") => {
                    Some(lines.len())
                }
                _ => None,
            })
            .sum();
        assert_eq!(sub, rec.subagents.iter().map(|(_, l)| l.len()).sum::<usize>());
    }

    /// A call's `PreToolUse` fires once the call is in its transcript and before its result is,
    /// its `PostToolUse` before its result is written, and `Stop` once the answer is.
    #[test]
    fn hooks_fire_where_claude_code_runs_them() {
        let (_, steps) = played("tools");
        let mut seen = String::new();
        for step in &steps {
            match step {
                Step::Write { lines, .. } => lines.iter().for_each(|l| seen.push_str(l)),
                Step::Hook { payload, .. } => {
                    let event = payload["hook_event_name"].as_str().unwrap();
                    if let Some(id) = payload.get("tool_use_id").and_then(Value::as_str) {
                        let call = format!("\"id\":\"{id}\"");
                        let result = format!("\"tool_use_id\":\"{id}\"");
                        assert!(seen.contains(&call), "{event} {id} after its call");
                        if event == "PreToolUse" || event.starts_with("PostToolUse") {
                            assert!(!seen.contains(&result), "{event} {id} before its result");
                        }
                    }
                    if event == "Stop" {
                        assert!(seen.contains("\"text\":\"done\""), "Stop after the answer");
                    }
                }
            }
        }
        let fired = events(&steps);
        assert_eq!(fired.first().map(String::as_str), Some("UserPromptSubmit"));
        assert_eq!(fired.last().map(String::as_str), Some("Stop"));
        assert!(!fired.iter().any(|e| e == "SessionStart"), "the stand-in fired its own");
        assert!(
            fired.iter().any(|e| e == "SubagentStart") && fired.iter().any(|e| e == "PreToolUse")
        );
    }

    /// A permission request is a step that waits, after its call's `PreToolUse` and before its
    /// call runs; nothing of what follows is written before it.
    #[test]
    fn a_permission_request_holds_the_turn_until_it_is_answered() {
        let (_, steps) = played("permission");
        let at = steps
            .iter()
            .position(|s| matches!(s, Step::Hook { waits: true, .. }))
            .expect("a request that waits");
        let Step::Hook { payload, .. } = &steps[at] else { panic!("not a hook") };
        assert_eq!(payload["hook_event_name"], "PermissionRequest");
        assert_eq!(payload["tool_input"]["command"], "touch refused.txt");
        let called = steps[..at].iter().rev().find_map(|s| match s {
            Step::Hook { payload: p, .. } if p["hook_event_name"] == "PreToolUse" => Some(p),
            _ => None,
        });
        let id = called.map(|p| p["tool_use_id"].clone()).unwrap_or_default();
        assert!(id.as_str().is_some_and(|id| !id.is_empty()), "its call's id: {id}");
        assert_eq!(payload["tool_use_id"], id, "the request names the call it is about");
        let before: String = steps[..at]
            .iter()
            .filter_map(|s| match s {
                Step::Write { lines, .. } => Some(lines.join("\n")),
                Step::Hook { .. } => None,
            })
            .collect();
        assert!(before.contains("touch refused.txt"), "the call is in the transcript");
        assert!(!before.contains("\"tool_use_id\":\"toolu_01\""), "its result is not");
        assert_eq!(steps.iter().filter(|s| matches!(s, Step::Hook { waits: true, .. })).count(), 3);
    }

    #[test]
    fn stamps_read_back_as_written() {
        for text in
            ["2026-10-13T05:46:04.748Z", "2000-02-29T23:59:59.001Z", "1970-01-01T00:00:00.000Z"]
        {
            assert_eq!(stamp(parse_stamp(text).unwrap()), text);
        }
        assert_eq!(parse_stamp("1970-01-01T00:00:01.000Z"), Some(1000));
        assert_eq!(parse_stamp("not a time"), None);
    }
}
