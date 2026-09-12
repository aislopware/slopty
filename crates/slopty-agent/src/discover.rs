//! Finding the JSONL transcript of a Claude Code session nobody told us about.
//!
//! A hook payload names its `transcript_path`; a `claude` the human started by hand does not.
//! Claude Code writes one file per session under
//! `~/.claude/projects/<escaped working directory>/<session uuid>.jsonl`, where the escaping
//! replaces every character that is not an ASCII letter or digit with a `-` (so
//! `/Users/x/.config` becomes `-Users-x--config`). The directory therefore follows from the
//! working directory the agent was started in, which the host knows: the foreground process's
//! own cwd, or OSC 7 from the shell that launched it.
//!
//! Which file in that directory is *this* session is decided by time: the live session is the
//! one still being written, so the newest `.jsonl` whose modification time is at or after the
//! moment the process was first seen. A resumed conversation reuses its old file and moves its
//! mtime forward, so it is found the same way; a session that has not written since the host
//! restarted is found as soon as it writes again.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use slopty_proto::agent::{AgentSessionInfo, TranscriptEntry};

/// How much of a transcript is read to name it: the first prompt is in the first records.
const TITLE_SCAN_BYTES: u64 = 64 * 1024;

/// Claude Code's per-project directory under the home directory.
#[must_use]
pub fn projects_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

/// The directory Claude Code writes a session's transcript into, for an agent running in `cwd`.
///
/// Claude Code names the directory after the working directory as the process sees it, which
/// on macOS is the resolved path (`/private/var/…` for a `/var/…` the host was given), so
/// `cwd` is canonicalised first when it exists; a directory that does not is used as given.
#[must_use]
pub fn project_dir(home: &Path, cwd: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_missing| cwd.to_path_buf());
    projects_dir(home).join(escape(&resolved))
}

/// A working directory as Claude Code names its project directory: every character that is not
/// an ASCII letter or digit becomes `-`.
#[must_use]
pub fn escape(cwd: &Path) -> String {
    cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// The newest `.jsonl` in `dir` modified at or after `since`, or `None` when the directory does
/// not exist or holds no such file.
#[must_use]
pub fn newest_transcript(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if modified < since {
            continue;
        }
        if best.as_ref().is_none_or(|(at, _path)| modified > *at) {
            best = Some((modified, path));
        }
    }
    best.map(|(_at, path)| path)
}

/// The transcript of an agent running in `cwd`, first seen at `since`.
#[must_use]
pub fn transcript_for(home: &Path, cwd: &Path, since: SystemTime) -> Option<PathBuf> {
    newest_transcript(&project_dir(home, cwd), since)
}

/// The last `limit` entries of conversation `id` on disk for an agent running in `cwd`.
///
/// What a card resuming it shows before the agent says anything new. A transcript that is
/// not there (or unreadable) is an empty past, not an error: the agent will still run.
#[must_use]
pub fn conversation(home: &Path, cwd: &Path, id: &str, limit: usize) -> Vec<TranscriptEntry> {
    let path = project_dir(home, cwd).join(format!("{id}.jsonl"));
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut entries = crate::transcript::entries(&text);
    entries.drain(..entries.len().saturating_sub(limit));
    entries
}

/// The conversations Claude Code has on disk for an agent in `cwd`, newest first.
///
/// One per `.jsonl` in its project directory (subagent files, `agent-*.jsonl`, are not
/// conversations), named by the first prompt the human typed, at most `limit`. A file with no
/// prompt in it is not listed: there is nothing to resume.
#[must_use]
pub fn sessions(home: &Path, cwd: &Path, limit: usize) -> Vec<AgentSessionInfo> {
    let dir = project_dir(home, cwd);
    named(candidates(&dir, &cwd.to_string_lossy()), limit)
}

/// The conversations Claude Code has on disk for every directory on this host, newest first.
///
/// [`sessions`] over each project directory, at most `limit`. Only the newest candidates are
/// opened to be named, so a home with hundreds of transcripts costs `limit` reads, not all of
/// them. A transcript whose records do not name their directory is listed under the project
/// directory's name, as Claude Code escaped it.
#[must_use]
pub fn all_sessions(home: &Path, limit: usize) -> Vec<AgentSessionInfo> {
    let Ok(projects) = std::fs::read_dir(projects_dir(home)) else { return Vec::new() };
    let found = projects
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .flat_map(|entry| {
            let dir = entry.path();
            let fallback = entry.file_name().to_string_lossy().into_owned();
            candidates(&dir, &fallback)
        })
        .collect();
    named(found, limit)
}

/// One transcript that may be a conversation, before it is opened.
struct Candidate {
    id: String,
    path: PathBuf,
    /// The directory to list it under when its records do not name one.
    fallback_cwd: String,
    modified_ms: u64,
}

/// Every `.jsonl` in `dir` that is not a subagent's, unopened; nothing when `dir` is missing.
fn candidates(dir: &Path, fallback_cwd: &str) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                return None;
            }
            let id = path.file_stem()?.to_str()?;
            if id.starts_with("agent-") {
                return None;
            }
            let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
            Some(Candidate {
                id: id.to_owned(),
                path,
                fallback_cwd: fallback_cwd.to_owned(),
                modified_ms: modified
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|d| u64::try_from(d.as_millis()).ok())
                    .unwrap_or(0),
            })
        })
        .collect()
}

/// The newest `limit` candidates that hold a prompt, named by it, newest first. Candidates are
/// opened in that order and only until `limit` are named.
fn named(mut found: Vec<Candidate>, limit: usize) -> Vec<AgentSessionInfo> {
    found.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then_with(|| a.id.cmp(&b.id)));
    found
        .into_iter()
        .filter_map(|c| {
            let (title, seen_cwd) = first_prompt(&c.path)?;
            Some(AgentSessionInfo {
                id: c.id,
                cwd: seen_cwd.unwrap_or(c.fallback_cwd),
                title,
                modified_ms: c.modified_ms,
            })
        })
        .take(limit)
        .collect()
}

/// The first line the human typed into a transcript (and the working directory the record
/// names), from its first [`TITLE_SCAN_BYTES`]; `None` when no prompt is in them. Slash
/// commands and their output are recorded as `<command-…>` tagged user records and are not
/// prompts; neither are meta records or sidechains.
fn first_prompt(path: &Path) -> Option<(String, Option<String>)> {
    let mut head = String::new();
    let file = std::fs::File::open(path).ok()?;
    let _read = file.take(TITLE_SCAN_BYTES).read_to_string(&mut head);
    head.lines().find_map(|line| {
        let record: Value = serde_json::from_str(line).ok()?;
        if record.get("type").and_then(Value::as_str) != Some("user")
            || record.get("isMeta").and_then(Value::as_bool) == Some(true)
            || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let content = record.get("message")?.get("content")?;
        let text = match content {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _other => return None,
        };
        let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        if first.starts_with('<') {
            return None;
        }
        let cwd = record.get("cwd").and_then(Value::as_str).map(str::to_owned);
        Some((crate::truncate(first), cwd))
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn touch(path: &Path, at: SystemTime) {
        let file = std::fs::File::create(path).expect("create");
        std::io::Write::write_all(&mut &file, b"{}\n").expect("write");
        file.set_modified(at).expect("mtime");
    }

    #[test]
    fn a_working_directory_becomes_claude_codes_project_directory() {
        assert_eq!(
            escape(Path::new("/Volumes/Lacie/Workspace/oss/slopty")),
            "-Volumes-Lacie-Workspace-oss-slopty"
        );
        assert_eq!(escape(Path::new("/Users/x/.config")), "-Users-x--config");
        assert_eq!(escape(Path::new("/private/tmp/a_b")), "-private-tmp-a-b");
        assert_eq!(
            project_dir(Path::new("/Users/x"), Path::new("/tmp/p")),
            Path::new("/Users/x/.claude/projects/-tmp-p"),
            "a directory that does not exist is named as given"
        );
        // One that exists is named by its resolved path, as the agent's own cwd would be.
        let real = tempfile::tempdir().expect("tempdir");
        let canonical = std::fs::canonicalize(real.path()).expect("canonical");
        assert_eq!(
            project_dir(Path::new("/Users/x"), real.path()),
            Path::new("/Users/x/.claude/projects").join(escape(&canonical))
        );
    }

    #[test]
    fn the_newest_recent_transcript_is_the_session() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        let dir = project_dir(home.path(), cwd);
        // No directory yet: nothing to find, and no error.
        assert_eq!(transcript_for(home.path(), cwd, SystemTime::UNIX_EPOCH), None);
        std::fs::create_dir_all(&dir).expect("mkdir");
        assert_eq!(transcript_for(home.path(), cwd, SystemTime::UNIX_EPOCH), None);

        let at = |secs: u64| {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)).expect("in range")
        };
        let started = at(1_000_000);
        // An old conversation in the same project, and a file that is not a transcript.
        touch(&dir.join("old.jsonl"), at(996_400));
        touch(&dir.join("notes.txt"), at(1_000_010));
        assert_eq!(transcript_for(home.path(), cwd, started), None, "nothing written since");

        touch(&dir.join("live.jsonl"), at(1_000_001));
        assert_eq!(
            transcript_for(home.path(), cwd, started).as_deref(),
            Some(dir.join("live.jsonl").as_path())
        );

        // A second session in the same directory: the one written last is this one.
        touch(&dir.join("newer.jsonl"), at(1_000_030));
        assert_eq!(
            transcript_for(home.path(), cwd, started).as_deref(),
            Some(dir.join("newer.jsonl").as_path())
        );
    }

    #[test]
    fn a_cleared_session_is_found_again_as_the_file_it_writes_now() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        let dir = project_dir(home.path(), cwd);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let at = |secs: u64| {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)).expect("in range")
        };
        let started = at(1_000_000);

        touch(&dir.join("before.jsonl"), at(1_000_005));
        assert_eq!(
            transcript_for(home.path(), cwd, started).as_deref(),
            Some(dir.join("before.jsonl").as_path())
        );

        // `/clear` starts a new file in the same project; the same lookup moves to it, so the
        // daemon stops tailing a conversation the human has left behind.
        touch(&dir.join("after.jsonl"), at(1_000_090));
        assert_eq!(
            transcript_for(home.path(), cwd, started).as_deref(),
            Some(dir.join("after.jsonl").as_path())
        );
    }

    #[test]
    fn a_conversation_on_disk_is_read_back_as_its_last_entries() {
        use std::fmt::Write as _;

        use slopty_proto::agent::TranscriptBody;
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        assert!(conversation(home.path(), cwd, "gone", 10).is_empty(), "no file: no past");
        let dir = project_dir(home.path(), cwd);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let mut body = String::new();
        for n in 1..=3 {
            writeln!(
                body,
                r#"{{"type":"user","message":{{"role":"user","content":"ask {n}"}}}}
{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"answer {n}"}}]}}}}"#
            )
            .expect("string");
        }
        std::fs::write(dir.join("s1.jsonl"), body).expect("write");
        let all = conversation(home.path(), cwd, "s1", 10);
        assert_eq!(all.len(), 6, "{all:?}");
        assert!(matches!(&all[0].body, TranscriptBody::User { text } if text == "ask 1"));
        let last = conversation(home.path(), cwd, "s1", 3);
        assert_eq!(last.len(), 3, "the limit keeps the newest: {last:?}");
        assert!(
            matches!(&last[0].body, TranscriptBody::Assistant { markdown } if markdown == "answer 2")
        );
        assert!(
            matches!(&last[2].body, TranscriptBody::Assistant { markdown } if markdown == "answer 3")
        );
    }

    #[test]
    fn the_conversations_on_disk_are_listed_newest_first_by_their_first_prompt() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        assert!(sessions(home.path(), cwd, 10).is_empty(), "no directory: nothing, no error");
        let dir = project_dir(home.path(), cwd);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let at = |secs: u64| {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)).expect("in range")
        };
        let write = |name: &str, body: &str, when: SystemTime| {
            let path = dir.join(name);
            std::fs::write(&path, body).expect("write");
            std::fs::File::open(&path).expect("open").set_modified(when).expect("mtime");
        };
        // A slash command's record and a meta record come before the first real prompt.
        write(
            "b2.jsonl",
            concat!(
                r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Caveat: local"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
                "\n",
                r#"{"type":"user","cwd":"/tmp/project","message":{"role":"user","content":[{"type":"text","text":"  fix the build\nplease"}]}}"#,
                "\n",
            ),
            at(2_000),
        );
        write(
            "a1.jsonl",
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","content":[]}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"add tests"}}"#,
                "\n",
            ),
            at(1_000),
        );
        // Never prompted, a subagent's file, and not a transcript: none of them is listed.
        write("c3.jsonl", r#"{"type":"assistant"}"#, at(3_000));
        write(
            "agent-x.jsonl",
            r#"{"type":"user","message":{"role":"user","content":"sub"}}"#,
            at(4_000),
        );
        write("notes.txt", "hello", at(5_000));

        let listed = sessions(home.path(), cwd, 10);
        let lines: Vec<(&str, &str, &str)> =
            listed.iter().map(|s| (s.id.as_str(), s.title.as_str(), s.cwd.as_str())).collect();
        assert_eq!(
            lines,
            [("b2", "fix the build", "/tmp/project"), ("a1", "add tests", "/tmp/project")]
        );
        assert_eq!(listed[0].modified_ms, 2_000_000);
        assert_eq!(sessions(home.path(), cwd, 1).len(), 1, "the limit keeps the newest");

        // Every directory: a second project's conversation joins the list in mtime order; one
        // whose records never name a directory is listed under the escaped directory name.
        let other = projects_dir(home.path()).join("-tmp-other");
        std::fs::create_dir_all(&other).expect("mkdir");
        let path = other.join("d4.jsonl");
        std::fs::write(
            &path,
            concat!(r#"{"type":"user","message":{"role":"user","content":"ship it"}}"#, "\n"),
        )
        .expect("write");
        std::fs::File::open(&path).expect("open").set_modified(at(1_500)).expect("mtime");
        let everywhere = all_sessions(home.path(), 10);
        let lines: Vec<(&str, &str, &str)> =
            everywhere.iter().map(|s| (s.id.as_str(), s.title.as_str(), s.cwd.as_str())).collect();
        assert_eq!(
            lines,
            [
                ("b2", "fix the build", "/tmp/project"),
                ("d4", "ship it", "-tmp-other"),
                ("a1", "add tests", "-tmp-project")
            ]
        );
        assert_eq!(all_sessions(home.path(), 2).len(), 2, "the limit spans directories");
        assert!(all_sessions(Path::new("/nonexistent"), 10).is_empty(), "no home: nothing");
    }
}
