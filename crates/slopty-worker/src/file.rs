//! A file tile's file on this machine: read, sent and saved.
//!
//! [`read()`] takes a text file whole, up to [`FILE_BYTES`], or a picture or PDF whole
//! ([`media`]), or says why not; [`announce`] says how that read goes on the wire; [`write()`]
//! is the tile's save, which replaces the file whole (`slopty_platform::fs::replace`).

pub mod media;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use bytes::Bytes;
use slopty_core::{WallMs, XferId};
use slopty_proto::file::{Body, FILE_BYTES, FileRead, INLINE_FILE_BYTES, MEDIA_BYTES, WriteResult};

use crate::listing::modified_ms;

/// Read `path` for a tile: a text whole, or a picture or PDF whole, as its bytes.
///
/// A picture or PDF is known by its first bytes ([`media::sniff`]) and read up to
/// [`MEDIA_BYTES`]; past that it is `Binary`, since nothing of it could be shown. Any other file
/// past [`FILE_BYTES`] is `TooLarge` and is not read. A NUL byte or invalid UTF-8 makes it
/// `Binary`. Nothing at the path in a folder that is there is `Absent`, a file to make;
/// anything else the OS refuses (a missing folder, a directory, not permitted) is `Missing`
/// with the OS's word. The final newline is left off the text and reported, so a save can put
/// it back.
#[must_use]
pub fn read(path: &Path) -> FileRead {
    let path = expand_home(path);
    // Looked at before it is opened: opening a named pipe waits for a writer that may never come.
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && folder_is_there(&path) => {
            return FileRead::Absent { editorconfig: editorconfig(&path) };
        }
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    if !meta.is_file() {
        return FileRead::Missing { error: kind_word(&meta) };
    }
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) => return FileRead::Missing { error: os_word(&e) },
    };
    let modified_ms = modified_ms(&meta);
    let mut bytes = Vec::new();
    if let Err(e) = (&mut file).take(media::HEAD as u64).read_to_end(&mut bytes) {
        return FileRead::Missing { error: os_word(&e) };
    }
    let media_type = media::sniff(&bytes);
    let cap = if media_type.is_some() { MEDIA_BYTES } else { FILE_BYTES };
    let past = |size: u64| {
        if media_type.is_some() { FileRead::Binary { size } } else { FileRead::TooLarge { size } }
    };
    if meta.len() > cap {
        return past(meta.len());
    }
    bytes.reserve(usize::try_from(meta.len()).unwrap_or(0).saturating_sub(bytes.len()));
    // One byte past the cap tells a file that grew since it was looked at.
    let rest = cap.saturating_add(1).saturating_sub(bytes.len() as u64);
    if let Err(e) = (&mut file).take(rest).read_to_end(&mut bytes) {
        return FileRead::Missing { error: os_word(&e) };
    }
    let size = bytes.len() as u64;
    if size > cap {
        return past(size);
    }
    if let Some(media_type) = media_type {
        return FileRead::Media {
            media_type: media_type.to_owned(),
            bytes: Bytes::from(bytes),
            modified_ms,
        };
    }
    if bytes.contains(&0) {
        return FileRead::Binary { size };
    }
    let Ok(mut text) = String::from_utf8(bytes) else {
        return FileRead::Binary { size };
    };
    let final_newline = text.ends_with('\n');
    if final_newline {
        text.pop();
    }
    FileRead::Text { text, size, modified_ms, final_newline, editorconfig: editorconfig(&path) }
}

/// Whether the folder `path` would be in is a folder: a save there makes the file.
fn folder_is_there(path: &Path) -> bool {
    path.parent().is_some_and(Path::is_dir)
}

/// The properties the specification defines, whose values are case-insensitive.
const EDITORCONFIG_KEYS: [&str; 7] = [
    "indent_style",
    "indent_size",
    "tab_width",
    "end_of_line",
    "charset",
    "trim_trailing_whitespace",
    "insert_final_newline",
];

/// The `EditorConfig` properties the `.editorconfig` files above `path` set for it, with the
/// specification's fallbacks (`indent_size = tab` takes `tab_width`, and the other way round).
///
/// A file that cannot be read or parsed sets nothing: the tile then follows the text alone, as
/// it does with no `.editorconfig` at all.
fn editorconfig(path: &Path) -> slopty_proto::file::EditorConfig {
    let mut properties = match ec4rs::properties_of(path) {
        Ok(properties) => properties,
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "editorconfig not read");
            return Vec::new();
        }
    };
    properties.use_fallbacks();
    properties
        .iter()
        .map(|(key, value)| {
            let value = value.into_str();
            let value = if EDITORCONFIG_KEYS.contains(&key) {
                value.to_lowercase()
            } else {
                value.to_owned()
            };
            (key.to_owned(), value)
        })
        .collect()
}

/// A read as it goes on the control stream, and the bytes to follow it on a bulk stream.
///
/// A text or media read past [`INLINE_FILE_BYTES`] is announced as [`FileRead::Streamed`] under
/// a new transfer, and its bytes are handed back to be sent under the same one. Anything else
/// goes inline as it is.
#[must_use]
pub fn announce(read: FileRead) -> (FileRead, Option<(XferId, Bytes)>) {
    match read {
        FileRead::Text { text, size, modified_ms, final_newline, editorconfig }
            if text.len() > INLINE_FILE_BYTES =>
        {
            let xfer = XferId::new();
            let body = Body::Text { final_newline, editorconfig };
            let text = Bytes::from(text.into_bytes());
            (FileRead::Streamed { xfer, size, modified_ms, body }, Some((xfer, text)))
        }
        FileRead::Media { media_type, bytes, modified_ms } if bytes.len() > INLINE_FILE_BYTES => {
            let xfer = XferId::new();
            let size = bytes.len() as u64;
            let body = Body::Media { media_type };
            (FileRead::Streamed { xfer, size, modified_ms, body }, Some((xfer, bytes)))
        }
        other => (other, None),
    }
}

/// How a save lands on disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rewrite {
    /// A new file renamed over the old one, so no reader ever sees half a save.
    Replace,
    /// Into the same file (same inode), for a file a waiting program holds open: `crontab -e`
    /// and `visudo` read the edited file back through the descriptor they opened before the
    /// editor ran, which a rename would leave on the old contents. The mode, owner and extended
    /// attributes stay, being the file's own.
    InPlace,
}

/// Save a file tile: rewrite `path` with `text` as `how` says, unless the file changed on disk
/// since `base_modified_ms`, the modification time of the version the edit started from.
///
/// A file whose time is newer than the base is a `Conflict` and is left alone; no base writes
/// regardless. Anything but a regular file, and a text past [`FILE_BYTES`], is `Failed` before
/// anything is touched.
#[must_use]
pub fn write(
    path: &Path,
    text: &[u8],
    base_modified_ms: Option<WallMs>,
    how: Rewrite,
) -> WriteResult {
    let path = expand_home(path);
    let failed = |error: String| WriteResult::Failed { error };
    if text.len() as u64 > FILE_BYTES {
        return failed(format!("{} bytes is past the {FILE_BYTES}-byte cap", text.len()));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if !meta.is_file() => return failed(kind_word(&meta)),
        Ok(meta) => {
            let on_disk = modified_ms(&meta);
            if base_modified_ms.is_some_and(|base| base < on_disk) {
                return WriteResult::Conflict { modified_ms: on_disk };
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return failed(os_word(&e)),
    }
    let written = match how {
        Rewrite::Replace => slopty_platform::fs::replace(&path, text),
        Rewrite::InPlace => rewrite_in_place(&path, text),
    };
    match written.and_then(|()| std::fs::metadata(&path)) {
        Ok(meta) => WriteResult::Saved { size: meta.len(), modified_ms: modified_ms(&meta) },
        Err(e) => failed(os_word(&e)),
    }
}

/// Write `text` over `path`'s own contents: from the start, then cut to its length, then to
/// disk before returning. Never empty in between, for a reader that looks early.
fn rewrite_in_place(path: &Path, text: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt as _;
    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(path)?;
    file.write_all_at(text, 0)?;
    file.set_len(text.len() as u64)?;
    file.sync_all()
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

/// `~` or `~/…` under the worker's home directory (`slopty_platform::dirs::home`); any other
/// path as is. A client types `~/notes.md` into its palette without knowing the worker's home.
#[must_use]
pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    slopty_platform::dirs::home().join(rest)
}

/// Why something that is there is not a file to read or write.
fn kind_word(meta: &std::fs::Metadata) -> String {
    if meta.is_dir() { "Is a directory" } else { "Not a regular file" }.to_owned()
}

/// The OS's message without the "(os error N)" suffix.
pub(crate) fn os_word(e: &std::io::Error) -> String {
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
    /// a tile on it would hold a blocking thread for good.
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
        let home = slopty_platform::dirs::home();
        assert_eq!(expand_home(Path::new("~/a/b.rs")), home.join("a/b.rs"));
        assert_eq!(expand_home(Path::new("~")), home);
        assert_eq!(expand_home(Path::new("/x/~/y")), PathBuf::from("/x/~/y"), "only a leading ~");
        assert_eq!(expand_home(Path::new("~user/y")), PathBuf::from("~user/y"), "not ~user");
    }

    #[test]
    fn text_binary_missing_and_the_final_newline() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("a.txt");
        std::fs::write(&text, "one\ntwo\n").unwrap();
        match read(&text) {
            FileRead::Text { text, size, modified_ms, final_newline, .. } => {
                assert_eq!((text.as_str(), size), ("one\ntwo", 8));
                assert!(final_newline, "the newline left off is reported");
                assert!(!modified_ms.is_zero());
            }
            other => panic!("{other:?}"),
        }
        let bare = dir.path().join("bare.txt");
        std::fs::write(&bare, "one\n\n").unwrap();
        assert!(
            matches!(read(&bare), FileRead::Text { text, final_newline: true, .. } if text == "one\n"),
            "only one newline is the file's last"
        );
        let bin = dir.path().join("a.bin");
        std::fs::write(&bin, b"ab\0cd").unwrap();
        assert_eq!(read(&bin), FileRead::Binary { size: 5 });
        let latin = dir.path().join("latin.txt");
        std::fs::write(&latin, b"na\xefve\n").unwrap();
        assert_eq!(read(&latin), FileRead::Binary { size: 6 });
        assert_eq!(
            read(&dir.path().join("gone")),
            FileRead::Absent { editorconfig: Vec::new() },
            "nothing there yet, in a folder that is: a file to make"
        );
        assert_eq!(
            read(&dir.path().join("no-folder/gone")),
            FileRead::Missing { error: "No such file or directory".to_owned() },
            "the os word without its number"
        );
        assert_eq!(read(dir.path()), FileRead::Missing { error: "Is a directory".to_owned() });
    }

    /// A file is read whole up to the cap, 200 000 lines included; one byte past it is
    /// `TooLarge` and nothing of it is read.
    #[test]
    fn a_file_is_whole_up_to_the_cap_and_too_large_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("long.rs");
        let body = (0..200_000).map(|i| format!("let l{i} = {i};\n")).collect::<Vec<_>>().concat();
        std::fs::write(&long, &body).unwrap();
        match read(&long) {
            FileRead::Text { text, size, final_newline, .. } => {
                assert_eq!(text.lines().count(), 200_000, "every line");
                assert_eq!(size, body.len() as u64);
                assert!(final_newline);
                assert!(text.ends_with("let l199999 = 199999;"), "down to the last line");
            }
            other => panic!("{other:?}"),
        }
        let cap = usize::try_from(FILE_BYTES).unwrap();
        let line = format!("{}\n", "x".repeat(1023));
        let exact = dir.path().join("exact.txt");
        std::fs::write(&exact, line.repeat(cap / 1024)).unwrap();
        assert!(
            matches!(read(&exact), FileRead::Text { size, .. } if size == FILE_BYTES),
            "the cap is inclusive"
        );
        let over = dir.path().join("over.txt");
        std::fs::write(&over, format!("{}!", line.repeat(cap / 1024))).unwrap();
        assert_eq!(read(&over), FileRead::TooLarge { size: FILE_BYTES + 1 });
    }

    /// A text read carries what the `.editorconfig` files above it set for it: the nearer file
    /// over the farther, the walk stopping at `root = true`, the specification's values in lower
    /// case and the fallbacks added, and a key it does not define kept as written.
    #[test]
    fn a_text_read_carries_its_editorconfig() {
        let dir = tempfile::tempdir().unwrap();
        let ec = |dir: &Path, body: &str| std::fs::write(dir.join(".editorconfig"), body).unwrap();
        ec(dir.path(), "root = true\n[*]\nindent_style = tab\ntrim_trailing_whitespace = TRUE\n");
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        ec(&src, "[*.py]\nindent_style = Space\nindent_size = 4\nx_house_style = KeepCase\n");
        std::fs::write(src.join("a.py"), "x = 1\n").unwrap();
        std::fs::write(src.join("b.txt"), "x\n").unwrap();
        let config = |name: &str| match read(&src.join(name)) {
            FileRead::Text { editorconfig, .. } => editorconfig,
            other => panic!("{other:?}"),
        };
        let py = config("a.py");
        let value = |key: &str| py.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(value("indent_style"), Some("space"), "the nearer file wins: {py:?}");
        assert_eq!(value("indent_size"), Some("4"));
        assert_eq!(value("tab_width"), Some("4"), "the fallback: {py:?}");
        assert_eq!(value("trim_trailing_whitespace"), Some("true"), "lower case");
        assert_eq!(value("x_house_style"), Some("KeepCase"), "an unknown key as written");
        let txt = config("b.txt");
        assert!(txt.contains(&("indent_style".to_owned(), "tab".to_owned())), "{txt:?}");
        assert!(!txt.iter().any(|(k, _)| k == "x_house_style"), "a section for other files");
        std::fs::write(dir.path().join("loose.txt"), "x").unwrap();
        ec(dir.path(), "root = true\n");
        let FileRead::Text { editorconfig, .. } = read(&dir.path().join("loose.txt")) else {
            panic!("text")
        };
        assert!(editorconfig.is_empty(), "nothing set is nothing sent: {editorconfig:?}");
    }

    /// A text that fits a clipboard's worth goes inline; a larger one is announced under a
    /// transfer and its text handed back to follow on a bulk stream. Nothing else streams.
    #[test]
    fn a_large_text_is_announced_and_streamed_and_a_small_one_rides_inline() {
        let text = |len: usize| FileRead::Text {
            text: "x".repeat(len),
            size: len as u64 + 1,
            modified_ms: WallMs::from_millis(7),
            final_newline: true,
            editorconfig: vec![("indent_style".to_owned(), "tab".to_owned())],
        };
        let small = text(INLINE_FILE_BYTES);
        assert_eq!(announce(small.clone()), (small, None), "the limit is inclusive");
        let (announced, stream) = announce(text(INLINE_FILE_BYTES + 1));
        let Some((xfer, body)) = stream else { panic!("a large text streams") };
        assert_eq!(body.len(), INLINE_FILE_BYTES + 1);
        assert_eq!(
            announced,
            FileRead::Streamed {
                xfer,
                size: INLINE_FILE_BYTES as u64 + 2,
                modified_ms: WallMs::from_millis(7),
                body: Body::Text {
                    final_newline: true,
                    editorconfig: vec![("indent_style".to_owned(), "tab".to_owned())],
                },
            }
        );
        let media = |len: usize| FileRead::Media {
            media_type: "image/png".to_owned(),
            bytes: Bytes::from(vec![7; len]),
            modified_ms: WallMs::from_millis(9),
        };
        let small = media(INLINE_FILE_BYTES);
        assert_eq!(announce(small.clone()), (small, None), "a small picture rides inline");
        let (announced, stream) = announce(media(INLINE_FILE_BYTES + 1));
        let Some((xfer, bytes)) = stream else { panic!("a large picture streams") };
        assert_eq!(bytes.len(), INLINE_FILE_BYTES + 1);
        assert_eq!(
            announced,
            FileRead::Streamed {
                xfer,
                size: INLINE_FILE_BYTES as u64 + 1,
                modified_ms: WallMs::from_millis(9),
                body: Body::Media { media_type: "image/png".to_owned() },
            }
        );
        for other in [FileRead::Binary { size: 1 << 20 }, FileRead::TooLarge { size: 1 << 30 }] {
            assert_eq!(announce(other.clone()), (other, None));
        }
    }

    /// A picture or PDF is read whole as its bytes, by its content whatever its name, past the
    /// text cap up to the media cap; past that it is binary, not read.
    #[test]
    fn a_picture_or_pdf_is_read_as_media_up_to_its_own_cap() {
        let dir = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        let shot = dir.path().join("shot");
        std::fs::write(&shot, &png).unwrap();
        match read(&shot) {
            FileRead::Media { media_type, bytes, modified_ms } => {
                assert_eq!(media_type, "image/png", "known without an extension");
                assert_eq!(bytes.as_ref(), png.as_slice(), "the file's own bytes");
                assert!(!modified_ms.is_zero());
            }
            other => panic!("{other:?}"),
        }
        let pdf = dir.path().join("big.pdf");
        let mut body = b"%PDF-1.7\n".to_vec();
        body.resize(usize::try_from(FILE_BYTES).unwrap() + 10, b' ');
        std::fs::write(&pdf, &body).unwrap();
        assert!(
            matches!(read(&pdf), FileRead::Media { media_type, bytes, .. }
                if media_type == "application/pdf" && bytes.len() == body.len()),
            "past the text cap a PDF is still shown"
        );
        let huge = dir.path().join("huge.png");
        let file = std::fs::File::create(&huge).unwrap();
        std::io::Write::write_all(&mut &file, &png).unwrap();
        file.set_len(MEDIA_BYTES + 1).unwrap();
        assert_eq!(read(&huge), FileRead::Binary { size: MEDIA_BYTES + 1 });
        let text = dir.path().join("notes.png");
        std::fs::write(&text, "not a picture\n").unwrap();
        assert!(matches!(read(&text), FileRead::Text { .. }), "a name alone makes no picture");
    }

    fn saved(result: &WriteResult) -> WallMs {
        match result {
            WriteResult::Saved { modified_ms, .. } => *modified_ms,
            other => panic!("not saved: {other:?}"),
        }
    }

    /// A file that is not there yet, in a folder that is, reads as one to make, with the
    /// `.editorconfig` it will have. A save of it from the epoch (what a tile's new file starts
    /// from) makes it, and the same save once something made the file meanwhile conflicts
    /// rather than writing over it.
    #[test]
    fn a_missing_file_in_a_folder_is_one_to_make_and_is_made_by_its_save() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".editorconfig"), "root = true\n[*.md]\nindent_size = 2\n")
            .unwrap();
        let path = dir.path().join("new.md");
        let FileRead::Absent { editorconfig } = read(&path) else { panic!("{:?}", read(&path)) };
        assert!(
            editorconfig.contains(&("indent_size".to_owned(), "2".to_owned())),
            "{editorconfig:?}"
        );
        saved(&write(&path, b"# New\n", Some(WallMs::ZERO), Rewrite::Replace));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# New\n");

        let raced = dir.path().join("raced.md");
        assert!(matches!(read(&raced), FileRead::Absent { .. }));
        std::fs::write(&raced, "made meanwhile\n").unwrap();
        assert!(
            matches!(
                write(&raced, b"mine\n", Some(WallMs::ZERO), Rewrite::Replace),
                WriteResult::Conflict { .. }
            ),
            "a file made since the read is not written over"
        );
        assert_eq!(std::fs::read_to_string(&raced).unwrap(), "made meanwhile\n");
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
        let first = saved(&write(&path, b"echo two\n", Some(base), Rewrite::Replace));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo two\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o750, "the mode is kept");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file stays behind");

        let stale = WallMs::from_millis(first.as_millis().saturating_sub(1));
        assert_eq!(
            write(&path, b"echo three\n", Some(stale), Rewrite::Replace),
            WriteResult::Conflict { modified_ms: first }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo two\n", "nothing written");
        saved(&write(&path, b"echo four\n", None, Rewrite::Replace));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "echo four\n", "no base forces it");
        saved(&write(&dir.path().join("new.txt"), b"fresh", Some(base), Rewrite::Replace));
    }

    /// A save in place lands in the file a program holds open, as `crontab -e` reads it back
    /// through the descriptor it opened before the editor ran; a replacing save would leave
    /// that descriptor on the old contents. The file keeps its inode, mode and extended
    /// attributes, and a shorter text leaves nothing of the longer one behind.
    #[test]
    fn a_save_in_place_reaches_a_descriptor_held_open() {
        use std::io::Seek as _;
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crontab.tmp");
        std::fs::write(&path, "0 * * * * old-and-long-line\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let tagged = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "dev.slopty.test", "kept"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        let inode = std::fs::metadata(&path).unwrap().ino();
        let mut held = std::fs::File::open(&path).unwrap();

        saved(&write(&path, b"5 * * * * new\n", None, Rewrite::InPlace));
        held.rewind().unwrap();
        let seen = std::io::read_to_string(&mut held).unwrap();
        assert_eq!(seen, "5 * * * * new\n", "the held descriptor reads the save");
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!((meta.ino(), meta.mode() & 0o7777), (inode, 0o600));
        if tagged {
            let read = std::process::Command::new("/usr/bin/xattr")
                .args(["-p", "dev.slopty.test"])
                .arg(&path)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&read.stdout).trim(), "kept");
        }

        saved(&write(&path, b"6 * * * * again\n", None, Rewrite::Replace));
        held.rewind().unwrap();
        let seen = std::io::read_to_string(&mut held).unwrap();
        assert_eq!(seen, "5 * * * * new\n", "a replacing save is not seen through it");
    }

    /// A save through a symbolic link replaces the file it names and keeps the link.
    #[test]
    fn a_link_keeps_pointing_at_the_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        saved(&write(&link, b"new", None, Rewrite::Replace));
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }

    /// A pipe, a directory and a text past the cap are refused before a byte is written; a
    /// file on disk past the cap takes a save of the whole text, since a tile only ever
    /// held whole files.
    #[test]
    fn a_pipe_a_directory_and_an_oversized_text_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo");
        let refused = |error: &str| WriteResult::Failed { error: error.to_owned() };
        assert_eq!(write(&fifo, b"x", None, Rewrite::Replace), refused("Not a regular file"));
        assert_eq!(write(dir.path(), b"x", None, Rewrite::Replace), refused("Is a directory"));
        let big = dir.path().join("big.txt");
        let text = vec![b'x'; usize::try_from(FILE_BYTES).unwrap() + 1];
        assert!(matches!(write(&big, &text, None, Rewrite::Replace), WriteResult::Failed { .. }));
        assert!(!big.exists(), "nothing written");
        std::fs::write(&big, &text).unwrap();
        saved(&write(&big, b"short", None, Rewrite::Replace));
        assert_eq!(std::fs::read_to_string(&big).unwrap(), "short");
    }
}
