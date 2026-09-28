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
#[cfg(target_vendor = "apple")]
fn order_before_later_writes(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: fcntl(2) with `F_BARRIERFSYNC` takes no third argument and only reads the
    // descriptor, which `file` keeps open for the call.
    let done = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) };
    if done == -1 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(not(target_vendor = "apple"))]
fn order_before_later_writes(file: &File) -> io::Result<()> {
    file.sync_data()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::replace;

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
}
