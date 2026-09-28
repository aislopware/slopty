//! A file sent up in parts ([`Verb::Upload`](slopty_proto::orchestration::Verb::Upload)).
//!
//! The parts are written where they go in a partial file beside the target, named for the
//! upload, so they may come in any order and a part sent again only writes the same bytes
//! again. The finish checks the partial's size and BLAKE3 digest against the caller's, then
//! renames it over the target in one step: nobody reads half a file under the real name, and a
//! partial that does not add up is dropped.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};

use slopty_core::XferId;
use slopty_proto::orchestration::{ErrorCode, UploadPart};
use slopty_proto::transfer::{Hash, MODE_BITS};

use super::{Failure, MAX_FILE_BYTES, io_failure};

/// Where upload `upload` to `path` gathers its parts: beside the target, on the same file
/// system, so the finish is a rename.
fn partial(path: &Path, upload: XferId) -> Result<PathBuf, Failure> {
    let name = path.file_name().ok_or_else(|| {
        Failure::new(ErrorCode::Invalid, format!("{} names no file", path.display()))
    })?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    Ok(dir.join(format!(".{}.{upload}.slopty-upload", name.to_string_lossy())))
}

/// Do one step of upload `upload` to `path`. Blocking file work: call it on the blocking pool.
///
/// # Errors
///
/// [`ErrorCode::Invalid`] for a part over the cap or a path that names no file;
/// [`ErrorCode::Failed`] for a finish that does not add up (the upload is dropped), a target
/// that is not a regular file, and whatever the disk refuses.
pub fn apply(path: &Path, upload: XferId, part: UploadPart) -> Result<(), Failure> {
    let partial = partial(path, upload)?;
    match part {
        UploadPart::Bytes { offset, bytes } => write_part(&partial, offset, &bytes),
        UploadPart::Finish { size, digest, mode } => {
            let finished = finish(path, &partial, size, &digest, mode);
            if finished.is_err() {
                let _removed = std::fs::remove_file(&partial);
            }
            finished
        }
        UploadPart::Abort => match std::fs::remove_file(&partial) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_failure(&partial, &e)),
            _removed => Ok(()),
        },
    }
}

fn write_part(partial: &Path, offset: u64, bytes: &[u8]) -> Result<(), Failure> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(Failure::new(
            ErrorCode::Invalid,
            format!("a part is at most {MAX_FILE_BYTES} bytes, not {}", bytes.len()),
        ));
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(partial)
        .map_err(|e| io_failure(partial, &e))?;
    file.write_all_at(bytes, offset).map_err(|e| io_failure(partial, &e))
}

fn finish(
    path: &Path,
    partial: &Path,
    size: u64,
    digest: &Hash,
    mode: Option<u32>,
) -> Result<(), Failure> {
    let (target, kept) = match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err(Failure::new(
                ErrorCode::Failed,
                format!("{} is not a regular file", path.display()),
            ));
        }
        // A link is kept: the file it names is replaced.
        Ok(meta) => {
            let target = std::fs::canonicalize(path).map_err(|e| io_failure(path, &e))?;
            (target, Some(meta.permissions()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (path.to_path_buf(), None),
        Err(e) => return Err(io_failure(path, &e)),
    };
    let mut file = match File::open(partial) {
        Ok(file) => file,
        // Nothing was sent: an empty file, as a zero-byte upload is.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && size == 0 => {
            File::create_new(partial).map_err(|e| io_failure(partial, &e))?
        }
        Err(e) => return Err(io_failure(partial, &e)),
    };
    let held = file.metadata().map_err(|e| io_failure(partial, &e))?.len();
    if held != size {
        return Err(Failure::new(
            ErrorCode::Failed,
            format!("the upload holds {held} bytes, not the {size} announced; it was dropped"),
        ));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(&mut file).map_err(|e| io_failure(partial, &e))?;
    if hasher.finalize().as_bytes() != digest {
        return Err(Failure::new(
            ErrorCode::Failed,
            "the parts do not add up to the digest announced; the upload was dropped",
        ));
    }
    // As `scp` does: a file replaced keeps its mode, a new one takes the caller's.
    let permissions = kept.or_else(|| {
        mode.map(|mode| std::os::unix::fs::PermissionsExt::from_mode(mode & MODE_BITS))
    });
    if let Some(permissions) = permissions {
        std::fs::set_permissions(partial, permissions).map_err(|e| io_failure(partial, &e))?;
    }
    crate::xfer::land(&file, partial, &target).map_err(|e| io_failure(&target, &e))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn bytes(offset: u64, bytes: &[u8]) -> UploadPart {
        UploadPart::Bytes { offset, bytes: bytes.to_vec() }
    }

    fn finish(whole: &[u8], mode: Option<u32>) -> UploadPart {
        let size = u64::try_from(whole.len()).unwrap();
        UploadPart::Finish { size, digest: *blake3::hash(whole).as_bytes(), mode }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn listed(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Parts land wherever they go, in any order and again; the file appears whole only at
    /// the finish, a new one with the mode asked for.
    #[test]
    fn parts_in_any_order_replace_the_file_at_the_finish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.bin");
        let upload = XferId::new();
        apply(&path, upload, bytes(0, b"old")).unwrap();
        apply(&path, upload, finish(b"old", Some(0o755))).unwrap();
        assert_eq!(mode(&path), 0o755, "a new file takes the mode asked for");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let upload = XferId::new();
        apply(&path, upload, bytes(6, b"world")).unwrap();
        apply(&path, upload, bytes(0, b"hello ")).unwrap();
        apply(&path, upload, bytes(6, b"world")).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "not in place before the finish");
        apply(&path, upload, finish(b"hello world", Some(0o644))).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        assert_eq!(mode(&path), 0o700, "a replaced file keeps its own");
        assert_eq!(listed(dir.path()), ["app.bin"], "no partial left behind");
    }

    /// A finish that does not add up (short, or other bytes) drops the upload and leaves the
    /// file as it was; the file's own mode carries over when none is asked for.
    #[test]
    fn an_upload_that_does_not_add_up_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"keep").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let short = XferId::new();
        apply(&path, short, bytes(0, b"abc")).unwrap();
        let failure = apply(&path, short, finish(b"hello", None)).unwrap_err();
        assert!(failure.message.contains("holds 3 bytes"), "{failure:?}");
        let other = XferId::new();
        apply(&path, other, bytes(0, b"HELLO")).unwrap();
        let failure = apply(&path, other, finish(b"hello", None)).unwrap_err();
        assert!(failure.message.contains("digest"), "{failure:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(listed(dir.path()), ["notes.txt"], "both partials dropped");

        let good = XferId::new();
        apply(&path, good, bytes(0, b"hello")).unwrap();
        apply(&path, good, finish(b"hello", None)).unwrap();
        assert_eq!((std::fs::read(&path).unwrap(), mode(&path)), (b"hello".to_vec(), 0o600));
    }

    /// An abort drops what was sent, twice is once; a part over the cap and a directory as the
    /// target are refused; an empty file needs no part.
    #[test]
    fn an_abort_drops_the_parts_and_bad_steps_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data");
        let upload = XferId::new();
        apply(&path, upload, bytes(0, b"x")).unwrap();
        apply(&path, upload, UploadPart::Abort).unwrap();
        apply(&path, upload, UploadPart::Abort).unwrap();
        assert!(listed(dir.path()).is_empty(), "{:?}", listed(dir.path()));
        let huge = vec![0; usize::try_from(MAX_FILE_BYTES).unwrap().saturating_add(1)];
        let refused = apply(&path, upload, UploadPart::Bytes { offset: 0, bytes: huge });
        assert_eq!(refused.unwrap_err().code, ErrorCode::Invalid);
        let failure = apply(dir.path(), upload, finish(b"", None)).unwrap_err();
        assert_eq!(failure.code, ErrorCode::Failed, "{failure:?}");
        apply(&path, XferId::new(), finish(b"", None)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }
}
