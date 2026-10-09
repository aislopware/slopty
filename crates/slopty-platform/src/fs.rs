//! Replacing a file whole, so a reader or a crash sees the old contents or the new, never half.
//!
//! Every state file (the registry, the ledger, the known workers, Claude Code's settings) and
//! every save of a user's file goes through [`replace`]. Durability is chosen for small files
//! written often: the new bytes are ordered on the device before the rename, which is what keeps
//! a power cut from leaving an empty file, and the rename itself is handed to the device before
//! [`replace`] returns. Neither step waits for the drive's cache to empty (`F_FULLFSYNC`), which
//! costs tens of milliseconds a call and buys only that the last write survives a power cut.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rename `from` to `to` unless something is at `to`.
///
/// Something there fails it with `AlreadyExists` and leaves both as they were. One step where
/// the file system can (`renameat2`'s `RENAME_NOREPLACE`, `renamex_np`'s `RENAME_EXCL`); where
/// it cannot (some Linux network and overlay file systems), a look at `to` and then a plain
/// rename.
///
/// # Errors
///
/// `AlreadyExists`, or whatever the OS refuses.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(e) if [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP].contains(&e) => {
            if std::fs::symlink_metadata(to).is_ok() {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(from, to)
        }
        Err(e) => Err(e.into()),
    }
}

/// Replace `path` with `bytes`, atomically.
///
/// It writes a temporary file in the same directory, puts the bytes on the device ahead of
/// anything written after them, renames it over `path`, then syncs the directory so the new
/// name is on the device too. An existing file keeps its permissions (a script stays
/// executable), a symbolic link keeps pointing at the file it names and that file is replaced,
/// and anything but a regular file is refused. The directory must exist. A failure removes the
/// temporary file and leaves `path` as it was.
///
/// # Errors
///
/// A target that is a directory (`IsADirectory`) or another non-regular file (`InvalidInput`),
/// or whatever the OS refuses.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (path, permissions) = match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            return Err(io::Error::new(io::ErrorKind::IsADirectory, "Is a directory"));
        }
        Ok(meta) if !meta.is_file() => {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Not a regular file"));
        }
        // Renaming over a link would swap the link for a file.
        Ok(meta) => (std::fs::canonicalize(path)?, Some(meta.permissions())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (path.to_path_buf(), None),
        Err(e) => return Err(e),
    };
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Names no file"))?;
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let temp = dir.join(format!(".{}.{}.slopty-tmp", name.to_string_lossy(), unique()));
    let written = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        order_before_later_writes(&file)?;
        std::fs::rename(&temp, &path)
    })();
    if let Err(e) = written {
        let _removed = std::fs::remove_file(&temp);
        return Err(e);
    }
    rustix::fs::fsync(File::open(dir)?).map_err(io::Error::from)
}

/// Put a copy of `source` in place of the regular file at `path`, whole or not at all, once
/// `ready` says the moment has come.
///
/// The copy is made beside `path` (a clone where the file system makes one, as `copyfile(3)`
/// does on APFS), takes the mode of the file it replaces, and is put on the device ahead of
/// anything written after it. `ready` is asked just before the copy is renamed over `path`: an
/// error from it leaves `path` as it was. A symbolic link at `path` keeps pointing at the file
/// it names, and that file is replaced. `source` is left where it is.
///
/// # Errors
///
/// Nothing at `path` (`NotFound`), a directory (`IsADirectory`) or another non-regular file
/// (`InvalidInput`) there, `ready`'s error, or whatever the OS refuses.
pub fn replace_from(
    path: &Path,
    source: &Path,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        return Err(io::Error::new(io::ErrorKind::IsADirectory, "Is a directory"));
    }
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Not a regular file"));
    }
    // Renaming over a link would swap the link for a file.
    let path = std::fs::canonicalize(path)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Names no file"))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("/"));
    let temp = dir.join(format!(".{}.{}.slopty-tmp", name.to_string_lossy(), unique()));
    let written = (|| {
        std::fs::copy(source, &temp)?;
        std::fs::set_permissions(&temp, meta.permissions())?;
        // Read only: a file whose mode lets no one write it is synced all the same.
        order_before_later_writes(&File::open(&temp)?)?;
        ready()?;
        std::fs::rename(&temp, &path)
    })();
    if let Err(e) = written {
        let _removed = std::fs::remove_file(&temp);
        return Err(e);
    }
    rustix::fs::fsync(File::open(dir)?).map_err(io::Error::from)
}

/// A temporary name's part that no other call, in this process or an earlier one with the same
/// pid, has used: the pid, the clock and a count.
fn unique() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{n}", std::process::id())
}

/// Put `file`'s data on the device ahead of every later write, so the rename cannot land
/// without it.
///
/// On Apple platforms a plain `fsync` leaves the data in the drive's cache, free to be written
/// after the rename; `F_BARRIERFSYNC` adds the ordering, and Apple recommends it over
/// `F_FULLFSYNC` where ordering is what is needed. Elsewhere `fdatasync` is that ordering.
///
/// # Errors
///
/// When the system cannot order the file's data.
#[cfg(target_vendor = "apple")]
pub fn order_before_later_writes(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: fcntl(2) with `F_BARRIERFSYNC` takes no third argument and only reads the
    // descriptor, which `file` keeps open for the call.
    let done = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) };
    if done == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Put `file`'s data on the device ahead of every later write: `fdatasync` orders it here.
///
/// # Errors
///
/// When the system cannot sync the file's data.
#[cfg(not(target_vendor = "apple"))]
pub fn order_before_later_writes(file: &File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::{replace, replace_from};

    /// Creates, overwrites, follows a link to its file, keeps an existing file's mode, refuses a
    /// directory, and leaves nothing but the file behind.
    #[test]
    fn replace_swaps_the_contents_whole_and_keeps_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        replace(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        replace(&path, b"two, longer").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two, longer");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o751, "a save keeps the file's mode");

        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        replace(&link, b"three").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink(), "the link stays a link");
        assert_eq!(std::fs::read(&path).unwrap(), b"three", "the file it names is replaced");

        let refused = replace(dir.path(), b"x").unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::IsADirectory);
        assert!(replace(&dir.path().join("none/state.json"), b"x").is_err(), "no directory");

        let mut left: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["link.json", "state.json"], "no temporary file is left behind");
    }

    /// A copy takes a file's place whole and keeps its mode, through a link to it too; a `ready`
    /// that says no, a directory or nothing there leaves everything as it was, the source
    /// included.
    #[test]
    fn replace_from_puts_a_copy_in_place_only_when_ready() {
        let dir = tempfile::tempdir().unwrap();
        let (path, source) = (dir.path().join("plan.key"), dir.path().join("new.key"));
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::write(&source, b"new, longer").unwrap();

        let no = std::io::Error::other("not now");
        let refused = replace_from(&path, &source, || Err(no)).unwrap_err();
        assert_eq!(refused.to_string(), "not now");
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "left as it was");

        let link = dir.path().join("link.key");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        replace_from(&link, &source, || Ok(())).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new, longer");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink(), "the link stays a link");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "the file's own mode");
        assert_eq!(std::fs::read(&source).unwrap(), b"new, longer", "the source stays");

        let gone = replace_from(&dir.path().join("gone"), &source, || Ok(())).unwrap_err();
        assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);
        let folder = replace_from(dir.path(), &source, || Ok(())).unwrap_err();
        assert_eq!(folder.kind(), std::io::ErrorKind::IsADirectory);
        let mut left: Vec<_> =
            std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["link.key", "new.key", "plan.key"], "no temporary file is left");
    }
}
