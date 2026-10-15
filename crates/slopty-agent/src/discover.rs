//! Finding the JSONL transcript of a Claude Code session nobody told us about.
//!
//! A hook payload names its `transcript_path`; a `claude` the human started by hand does not.
//! Claude Code writes one file per session under
//! `~/.claude/projects/<escaped working directory>/<session uuid>.jsonl`, where the escaping
//! replaces every character that is not an ASCII letter or digit with a `-` (so
//! `/Users/x/.config` becomes `-Users-x--config`). The directory therefore follows from the
//! working directory the agent was started in, which the worker knows: the foreground process's
//! own cwd, or OSC 7 from the shell that launched it.
//!
//! Which file in that directory is *this* session is what Claude Code's own registry of its live
//! sessions says ([`crate::roster`]): the entry of the agent's process names its conversation,
//! and `/clear` or `/resume` changes it there ([`registered_transcript`]). Only an agent the
//! registry does not list is found by time ([`transcript_for`]): the newest `.jsonl` whose
//! modification time is at or after the moment the process was first seen. That is a guess,
//! wrong whenever another Claude Code writes in the same folder at once (a person's own session
//! beside one Slopty started), so it is never taken over the registry's word.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use slopty_core::WallMs;
use slopty_proto::thread::AgentId;
use slopty_proto::thread::wire::PastSession;

/// Claude Code's per-project directory, under its own ([`crate::roster::config_dir`]): the
/// registry and the transcripts are found under the same one.
#[must_use]
fn projects_dir(home: &Path) -> PathBuf {
    crate::roster::config_dir(home).join("projects")
}

/// The directory Claude Code writes a session's transcript into, for an agent running in `cwd`.
///
/// Claude Code names the directory after the working directory as the process sees it, which
/// on macOS is the resolved path (`/private/var/…` for a `/var/…` the worker was given), so
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
fn newest_transcript(dir: &Path, since: SystemTime) -> Option<PathBuf> {
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

/// What Claude Code's registry says of an agent's transcript ([`registered_transcript`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registered {
    /// The registry lists neither the process nor a child of it, or names no conversation.
    Unlisted,
    /// It names a conversation whose file is not there yet.
    Unwritten,
    /// The file the conversation is written to.
    At(PathBuf),
}

/// The transcript of the Claude Code process `pid` runs, as Claude Code's registry names it.
///
/// `listed` is the registry; a managed launcher runs Claude Code as its child, and `children`
/// lists a process's direct children. The entry's own working directory names the project
/// directory, `cwd` when it has none.
#[must_use]
pub fn registered_transcript(
    home: &Path,
    listed: &[crate::roster::Listed],
    pid: i32,
    children: impl Fn(i32) -> Vec<i32>,
    cwd: &Path,
) -> Registered {
    let named = |pid: i32| listed.iter().find(|l| l.pid == Some(pid));
    let entry = named(pid).or_else(|| children(pid).into_iter().find_map(named));
    let Some(entry) = entry else { return Registered::Unlisted };
    let id = entry.session_id.as_deref().filter(|id| crate::resume::is_session_id(id));
    let Some(id) = id else { return Registered::Unlisted };
    let cwd = entry.cwd.as_deref().map_or(cwd, Path::new);
    let path = project_dir(home, cwd).join(format!("{id}.jsonl"));
    if path.is_file() { Registered::At(path) } else { Registered::Unwritten }
}

/// The transcript of an agent running in `cwd`, first seen at `since`, by time alone: the guess
/// for an agent Claude Code's registry does not list ([`registered_transcript`]).
#[must_use]
pub fn transcript_for(home: &Path, cwd: &Path, since: SystemTime) -> Option<PathBuf> {
    newest_transcript(&project_dir(home, cwd), since)
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
            let id = name.strip_suffix(".jsonl").filter(|id| crate::resume::is_session_id(id))?;
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
            resume: vec![crate::resume::RESUME_FLAG.to_owned(), id.clone()],
            facts: BTreeMap::new(),
            native: id,
        })
        .collect())
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
            project_dir(Path::new("/Users/x"), Path::new("/slopty-absent/p")),
            Path::new("/Users/x/.claude/projects/-slopty-absent-p"),
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

        // Written the very instant the session started: still this session's.
        touch(&dir.join("exact.jsonl"), started);
        assert_eq!(
            transcript_for(home.path(), cwd, started).as_deref(),
            Some(dir.join("exact.jsonl").as_path())
        );
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

    /// Two Claude Codes in one folder: each is read from the conversation Claude Code's registry
    /// names for its process (or the managed launcher's child), never from the file written
    /// last; one the registry does not list is still found by time.
    #[test]
    fn the_registry_names_the_transcript_over_the_newest_file() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        let dir = project_dir(home.path(), cwd);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let at = |secs: u64| {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)).expect("in range")
        };
        let mine = "53655038-c219-47c3-91a4-2bc72d07a295";
        let theirs = "4d070c6c-13d1-4acb-b3e8-4b23875f6f13";
        touch(&dir.join(format!("{mine}.jsonl")), at(1_000_001));
        // The person's own session beside it writes all the time.
        touch(&dir.join(format!("{theirs}.jsonl")), at(1_000_090));
        let entry = |pid: i32, session: &str| crate::roster::Listed {
            pid: Some(pid),
            session_id: Some(session.to_owned()),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            ..crate::roster::Listed::default()
        };
        let listed = [entry(46328, mine), entry(73656, theirs)];
        let none = |_pid: i32| Vec::new();
        let found = |pid: i32, children: &dyn Fn(i32) -> Vec<i32>| {
            registered_transcript(home.path(), &listed, pid, children, cwd)
        };
        assert_eq!(found(46328, &none), Registered::At(dir.join(format!("{mine}.jsonl"))));
        // A managed launcher's terminal runs the launcher; Claude Code is its child.
        let launcher = |pid: i32| if pid == 46323 { vec![46328] } else { Vec::new() };
        assert_eq!(found(46323, &launcher), Registered::At(dir.join(format!("{mine}.jsonl"))));
        assert_eq!(
            transcript_for(home.path(), cwd, at(1_000_000)),
            Some(dir.join(format!("{theirs}.jsonl"))),
            "by time alone the other session's file would be taken"
        );
        // A conversation not written yet is waited for, not guessed.
        let fresh = "0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b";
        let listed = [entry(500, fresh)];
        assert_eq!(
            registered_transcript(home.path(), &listed, 500, none, cwd),
            Registered::Unwritten
        );
        // A process the registry does not list is left to the guess.
        assert_eq!(
            registered_transcript(home.path(), &listed, 999, none, cwd),
            Registered::Unlisted
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

    /// Past sessions are the transcripts' names, the last written first, as many as asked; a
    /// file that is no transcript of a session is passed over, and a folder Claude Code never
    /// ran in has none. Each is resumed by its id.
    #[test]
    fn past_sessions_are_named_by_their_files_alone() {
        let home = tempfile::tempdir().expect("tempdir");
        let cwd = Path::new("/tmp/project");
        assert_eq!(sessions(home.path(), cwd, 10).expect("none"), []);
        let dir = project_dir(home.path(), cwd);
        std::fs::create_dir_all(dir.join("sub.jsonl")).expect("mkdir");
        let at = |secs: u64| {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs)).expect("in range")
        };
        touch(&dir.join("aaa-1.jsonl"), at(100));
        touch(&dir.join("bbb-2.jsonl"), at(300));
        touch(&dir.join("ccc-3.jsonl"), at(200));
        touch(&dir.join("notes.txt"), at(400));
        touch(&dir.join("bad id.jsonl"), at(500));
        let listed = sessions(home.path(), cwd, 2).expect("listed");
        let names: Vec<_> = listed.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(names, ["bbb-2", "ccc-3"]);
        assert_eq!(listed[0].updated_ms, Some(WallMs::from_millis(300_000)));
        assert_eq!(listed[0].resume, ["--resume", "bbb-2"]);
        assert_eq!(listed[0].title, None, "no transcript is read");
    }
}
