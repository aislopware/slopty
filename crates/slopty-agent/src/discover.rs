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
//! Which file in that directory is *this* session is decided by time: the live session is the
//! one still being written, so the newest `.jsonl` whose modification time is at or after the
//! moment the process was first seen. A resumed conversation reuses its old file and moves its
//! mtime forward, so it is found the same way; a session that has not written since the worker
//! restarted is found as soon as it writes again.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Claude Code's per-project directory under the home directory.
#[must_use]
pub fn projects_dir(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
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
}
