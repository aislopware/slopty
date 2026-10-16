//! Where Claude Code keeps a session's record, and the sessions it kept for a folder.
//!
//! Claude Code writes one transcript per session under
//! `<config>/projects/<escaped working directory>/<session id>.jsonl`, each subagent's beside it
//! (`<session id>/subagents/agent-<id>.jsonl`), where the escaping replaces every character that
//! is not an ASCII letter or digit with a `-` (so `/Users/x/.config` becomes `-Users-x--config`).
//! The driven Claude Code is given its session id, so its record is found by that id and its
//! folder, never guessed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{EXPANDED_CHARS, Expanded, PastSession};
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
/// that is missing reads as none; a line that is not JSON is passed over.
///
/// # Errors
///
/// When a file is there but cannot be read.
pub fn records(home: &Path, cwd: &Path, session: &str) -> std::io::Result<Vec<serde_json::Value>> {
    let mut records = Vec::new();
    for file in files(home, cwd, session) {
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        records.extend(text.lines().filter_map(|line| serde_json::from_str(line).ok()));
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

/// Claude Code's past sessions for an agent run in `cwd`, the last written first, at most
/// `limit`.
///
/// Each comes from its transcript's name and modification time alone. No transcript is
/// opened, since what the person said to Claude Code is theirs; what Slopty shows of one is what
/// its own thread of it holds. A folder Claude Code never ran in has none.
///
/// # Errors
///
/// When the project directory is there but cannot be listed.
pub fn sessions(home: &Path, cwd: &Path, limit: usize) -> std::io::Result<Vec<PastSession>> {
    let entries = match std::fs::read_dir(project_dir(home, cwd)) {
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
        .map(|(modified, id)| PastSession {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: Some(folder.clone().into_owned()),
            prompts: Vec::new(),
            title: None,
            updated_ms: modified.map(WallMs::of),
            thread: None,
            resume: vec![super::RESUME_FLAG.to_owned(), id.clone()],
            facts: BTreeMap::new(),
            native: id,
        })
        .collect())
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
}
