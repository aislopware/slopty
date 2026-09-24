//! A file read for a file card: the first [`FILE_LINES`] lines of a text file on this
//! machine, or the word for why not; and the card's save ([`write()`]), which replaces the file
//! whole ([`replace`]).

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use slopty_proto::file::{FILE_BYTES, FILE_LINES, FileRead, WriteResult};

/// Read `path` for a card: at most [`FILE_BYTES`] from the start, then at most
/// [`FILE_LINES`] lines of it.
///
/// A NUL byte or invalid UTF-8 in what was read makes it `Binary`; anything the OS refuses
/// (missing, a directory, not permitted) is `Missing` with the OS's word. The bytes cut are
/// counted as lines only when they were read, so a file past the byte cap reports the lines
/// it had within it and `more_lines` from there.
#[must_use]
pub fn read(path: &Path) -> FileRead {
    let path = expand_home(path);
    // Looked at before it is opened: opening a named pipe waits for a writer that may never come.
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    if !meta.is_file() {
        return FileRead::Missing { error: kind_word(&meta) };
    }
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    let size = meta.len();
    let modified_ms = modified_ms(&meta);
    let mut bytes = Vec::with_capacity(usize::try_from(size.min(FILE_BYTES)).unwrap_or(0));
    if let Err(e) = (&mut file).take(FILE_BYTES).read_to_end(&mut bytes) {
        return FileRead::Missing { error: os_word(&e) };
    }
    if bytes.contains(&0) {
        return FileRead::Binary { size };
    }
    let truncated = size > FILE_BYTES;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if truncated => {
            // The cap may have split a character: keep what decodes and drop the tail.
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => return FileRead::Binary { size },
    };
    let (text, more_lines) = clip(&text, truncated);
    FileRead::Text { text, more_lines, size, modified_ms }
}

/// Save a file card: replace `path` with `text` unless the file changed on disk since
/// `base_modified_ms`, the modification time of the version the edit started from.
///
/// A file whose time is newer than the base is a `Conflict` and is left alone; no base writes
/// regardless. Anything but a regular file, and a text or a file on disk past [`FILE_BYTES`]
/// (the card only ever held its first part), is `Failed` before anything is touched.
#[must_use]
pub fn write(path: &Path, text: &str, base_modified_ms: Option<u64>) -> WriteResult {
    let path = expand_home(path);
    let failed = |error: String| WriteResult::Failed { error };
    if text.len() as u64 > FILE_BYTES {
        return failed(format!("{} bytes is past the {FILE_BYTES}-byte cap", text.len()));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if !meta.is_file() => return failed(kind_word(&meta)),
        Ok(meta) if meta.len() > FILE_BYTES => {
            return failed(format!("the file is {} bytes, past the cap", meta.len()));
        }
        Ok(meta) => {
            let on_disk = modified_ms(&meta);
            if base_modified_ms.is_some_and(|base| base < on_disk) {
                return WriteResult::Conflict { modified_ms: on_disk };
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return failed(os_word(&e)),
    }
    match replace(&path, text.as_bytes()) {
        Ok(meta) => WriteResult::Saved { size: meta.len(), modified_ms: modified_ms(&meta) },
        Err(e) => failed(os_word(&e)),
    }
}

/// Replace a file atomically, and answer the new file's metadata.
///
/// It writes a temporary file in the same directory, flushes it to disk and renames it over,
/// so a reader sees the old contents or the new, never half. An existing file keeps its
/// permissions (a script stays executable), a symbolic link keeps pointing at the file it
/// names, and anything but a regular file is refused.
///
/// # Errors
///
/// A target that is not a regular file, or whatever the OS refuses.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<std::fs::Metadata> {
    let (path, mode) = match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, kind_word(&meta)));
        }
        // Renaming over a link would swap the link for a file; the file it names is replaced.
        Ok(meta) => (std::fs::canonicalize(path)?, Some(meta.permissions())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (path.to_path_buf(), None),
        Err(e) => return Err(e),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Names no file"))?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let temp =
        dir.join(format!(".{}.{}-{nanos}.slopty-tmp", name.to_string_lossy(), std::process::id()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        if let Some(mode) = mode {
            file.set_permissions(mode)?;
        }
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        std::fs::metadata(&path)
    })();
    if written.is_err() {
        let _removed = std::fs::remove_file(&temp);
    }
    written
}

/// What a watcher compares between two looks at a file.
///
/// Its size and its modification time (nanoseconds since the Unix epoch, as the file system
/// keeps it). `None` when nothing readable is there, which is a state too (a file removed,
/// then written back).
#[must_use]
pub fn stamp(path: &Path) -> Option<(u64, u128)> {
    let meta = std::fs::metadata(expand_home(path)).ok()?;
    if meta.is_dir() {
        return None;
    }
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((meta.len(), modified.as_nanos()))
}

/// `~` or `~/…` as the worker's home directory (`$HOME`, `/tmp` when unset); any other path
/// as is. A client types `~/notes.md` into its palette without knowing the worker's home.
#[must_use]
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join(rest)
}

/// The first [`FILE_LINES`] lines of `text` and the count of the rest; a byte-capped read
/// drops its last, possibly partial, line and counts it.
fn clip(text: &str, truncated: bool) -> (String, u32) {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    if truncated {
        lines.pop();
    }
    let kept = lines.len().min(FILE_LINES as usize);
    let dropped = u32::try_from(lines.len().saturating_sub(kept)).unwrap_or(u32::MAX);
    let more = dropped.saturating_add(u32::from(truncated));
    lines.truncate(kept);
    (lines.join("\n"), more)
}

/// A file's modification time in milliseconds since the Unix epoch, 0 when the OS has none.
fn modified_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Why something that is there is not a file to read or write.
fn kind_word(meta: &std::fs::Metadata) -> String {
    if meta.is_dir() { "Is a directory" } else { "Not a regular file" }.to_owned()
}

/// The OS's message without the "(os error N)" suffix.
fn os_word(e: &std::io::Error) -> String {
    let text = e.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_changes_with_the_file_and_is_none_for_what_is_not_one() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join("a.txt");
        assert_eq!(stamp(&path), None, "nothing there yet");
        assert_eq!(stamp(dir.path()), None, "a directory is not a file");
        std::fs::write(&path, "one").map_err(|e| e.to_string())?;
        let first = stamp(&path).ok_or("stamped")?;
        assert_eq!(first.0, 3);
        std::fs::write(&path, "two!").map_err(|e| e.to_string())?;
        let second = stamp(&path).ok_or("stamped")?;
        assert_ne!(first, second, "a write changes it");
        assert_eq!(second.0, 4);
        Ok(())
    }

    /// A named pipe is answered at once rather than opened: opening one waits for a writer, and
    /// a card on it would hold a blocking thread for good.
    #[test]
    fn a_named_pipe_is_not_opened() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let error = "Not a regular file".to_owned();
        assert_eq!(read(&fifo), FileRead::Missing { error });
        Ok(())
    }

    #[test]
    fn a_tilde_is_the_workers_home() {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        assert_eq!(expand_home(Path::new("~/a/b.rs")), home.join("a/b.rs"));
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("/x/~/y")), PathBuf::from("/x/~/y"), "only a leading ~");
        assert_eq!(expand_home(Path::new("~user/y")), PathBuf::from("~user/y"), "not ~user");
    }

    #[test]
    fn text_binary_missing_and_clipped() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, "one\ntwo\n").unwrap();
        match read(&text) {
            FileRead::Text { text, more_lines, size, modified_ms } => {
                assert_eq!((text.as_str(), more_lines, size), ("one\ntwo", 0, 8));
                assert!(modified_ms > 0);
            }
            other => panic!("{other:?}"),
        }
        let bin = dir.path().join("a.bin");
        std::fs::write(&bin, b"ab\0cd").unwrap();
        assert_eq!(read(&bin), FileRead::Binary { size: 5 });
        let latin = dir.path().join("latin.txt");
        std::fs::write(&latin, b"na\xefve\n").unwrap();
        assert_eq!(read(&latin), FileRead::Binary { size: 6 });
        assert_eq!(
            read(&dir.path().join("gone")),
            FileRead::Missing { error: "No such file or directory".to_owned() },
            "the os word without its number"
        );
        assert_eq!(read(dir.path()), FileRead::Missing { error: "Is a directory".to_owned() });
        let long = dir.path().join("long.txt");
        let body: String =
            (0..FILE_LINES + 5).map(|i| format!("l{i}\n")).collect::<Vec<_>>().concat();
        std::fs::write(&long, &body).unwrap();
        match read(&long) {
            FileRead::Text { text, more_lines, .. } => {
                assert_eq!(text.lines().count(), FILE_LINES as usize);
                assert_eq!(more_lines, 5);
            }
            other => panic!("{other:?}"),
        }
        let huge = dir.path().join("huge.txt");
        let line = "x".repeat(1023);
        let body = format!("{line}\n").repeat(600);
        std::fs::write(&huge, &body).unwrap();
        match read(&huge) {
            FileRead::Text { text, more_lines, size, .. } => {
                assert_eq!(size, 600 * 1024);
                assert_eq!(text.lines().count(), 511, "512 KiB less the cut last line");
                assert_eq!(more_lines, 1, "the rest is counted as one: the worker did not read it");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A file of exactly the byte cap is whole; one past it that the cap splits inside a
    /// character keeps what decodes and counts the cut line.
    #[test]
    fn the_byte_cap_is_inclusive_and_a_split_character_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("{}\n", "x".repeat(1023));
        let cap = usize::try_from(FILE_BYTES).unwrap();
        let exact = dir.path().join("exact.txt");
        std::fs::write(&exact, line.repeat(cap / 1024)).unwrap();
        match read(&exact) {
            FileRead::Text { text, more_lines, size, .. } => {
                assert_eq!((text.lines().count(), more_lines, size), (cap / 1024, 0, FILE_BYTES));
            }
            other => panic!("{other:?}"),
        }
        let split = dir.path().join("split.txt");
        let body = format!("first\n{}\u{e9}\ntail\n", "a".repeat(cap - 7));
        std::fs::write(&split, &body).unwrap();
        match read(&split) {
            FileRead::Text { text, more_lines, size, .. } => {
                assert_eq!((text.as_str(), more_lines), ("first", 1));
                assert_eq!(size, u64::try_from(body.len()).unwrap());
            }
            other => panic!("{other:?}"),
        }
    }

    fn saved(result: &WriteResult) -> u64 {
        match result {
            WriteResult::Saved { modified_ms, .. } => *modified_ms,
            other => panic!("not saved: {other:?}"),
        }
    }

    /// A save from the version on disk replaces it and keeps its mode; a save from an older
    /// version is a conflict and leaves the file as it was.
    #[test]
    fn a_save_replaces_the_file_and_an_edit_of_an_old_version_conflicts() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        std::fs::write(&path, "echo one\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        let FileRead::Text { modified_ms: base, .. } = read(&path) else { panic!("text") };
        let first = saved(&write(&path, "echo two\n", Some(base)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo two\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o750, "the mode is kept");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file stays behind");

        let stale = first.saturating_sub(1);
        assert_eq!(
            write(&path, "echo three\n", Some(stale)),
            WriteResult::Conflict { modified_ms: first }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo two\n", "nothing written");
        saved(&write(&path, "echo four\n", None));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo four\n", "no base forces it");
        saved(&write(&dir.path().join("new.txt"), "fresh", Some(base)));
    }

    /// A save through a symbolic link replaces the file it names and keeps the link.
    #[test]
    fn a_link_keeps_pointing_at_the_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        saved(&write(&link, "new", None));
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    /// A pipe, a directory and anything past the byte cap are refused before a byte is written.
    #[test]
    fn a_pipe_a_directory_and_an_oversized_text_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let refused = |error: &str| WriteResult::Failed { error: error.to_owned() };
        assert_eq!(write(&fifo, "x", None), refused("Not a regular file"));
        assert_eq!(write(dir.path(), "x", None), refused("Is a directory"));
        let big = dir.path().join("big.txt");
        let text = "x".repeat(usize::try_from(FILE_BYTES).unwrap() + 1);
        assert!(matches!(write(&big, &text, None), WriteResult::Failed { .. }));
        assert!(!big.exists(), "nothing written");
        std::fs::write(&big, &text).unwrap();
        assert!(
            matches!(write(&big, "short", None), WriteResult::Failed { .. }),
            "a file the card only held part of is not cut to that part"
        );
        assert_eq!(std::fs::metadata(&big).unwrap().len(), FILE_BYTES + 1);
    }
}
