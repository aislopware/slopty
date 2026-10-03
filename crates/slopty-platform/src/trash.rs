//! The OS's own trash, where a person can put back what was thrown away: never an unlink.
//!
//! - macOS: the Finder's trash, through `NSFileManager`'s `trashItemAtURL`, which picks the trash
//!   of the item's volume, names it apart from what is already there and records where it came from
//!   for Put Back.
//! - Linux: the freedesktop.org trash specification (1.0), which file managers and `gio trash`
//!   read: `$XDG_DATA_HOME/Trash` for the home's volume, else the volume's own `.Trash/$uid` or
//!   `.Trash-$uid`, with an `info/<name>.trashinfo` beside each entry saying where it was and when
//!   it went. The optional `directorysizes` cache is not written; the specification lets a reader
//!   rebuild it.

use std::path::{Path, PathBuf};

/// Why an entry could not go to the trash.
#[derive(Debug)]
pub enum TrashError {
    /// The entry's volume keeps no trash this user can use (a network share, a volume with no
    /// writable trash), so it stays where it is.
    NoTrash,
    /// The OS said no.
    Os(std::io::Error),
}

impl std::fmt::Display for TrashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTrash => f.write_str("this volume has no trash"),
            Self::Os(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TrashError {}

impl From<std::io::Error> for TrashError {
    fn from(e: std::io::Error) -> Self {
        Self::Os(e)
    }
}

/// Move `path` (a link itself, not what it points to) to this user's trash, and say where it
/// landed.
///
/// # Errors
///
/// [`TrashError::NoTrash`] when its volume has none; else what the OS said, `NotFound` when
/// nothing is there.
pub fn trash(path: &Path) -> Result<PathBuf, TrashError> {
    #[cfg(target_os = "macos")]
    {
        finder(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        freedesktop::Trash::for_user().put(path, std::time::SystemTime::now())
    }
}

/// The Finder's trash.
#[cfg(target_os = "macos")]
fn finder(path: &Path) -> Result<PathBuf, TrashError> {
    use objc2_foundation::{
        NSCocoaErrorDomain, NSFeatureUnsupportedError, NSFileManager, NSFileNoSuchFileError, NSURL,
    };

    let not_a_path =
        || std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a file system path");
    let url = NSURL::from_file_path(path).ok_or_else(not_a_path)?;
    let mut landed = None;
    let trashed = NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut landed));
    if let Err(e) = trashed {
        // SAFETY: `NSCocoaErrorDomain` is an immutable `NSString` constant Foundation exports
        // and never releases; reading the extern static is how objc2 exposes it.
        let cocoa = &*e.domain() == unsafe { NSCocoaErrorDomain };
        let code = e.code();
        return Err(if cocoa && code == NSFeatureUnsupportedError {
            TrashError::NoTrash
        } else if cocoa && code == NSFileNoSuchFileError {
            std::io::Error::from(std::io::ErrorKind::NotFound).into()
        } else {
            std::io::Error::other(e.localizedDescription().to_string()).into()
        });
    }
    landed.and_then(|url| url.to_file_path()).ok_or_else(|| not_a_path().into())
}

/// The freedesktop.org trash, built on every Unix so its tests run on any host.
#[cfg(any(not(target_os = "macos"), test))]
pub(crate) mod freedesktop {
    use std::ffi::OsStr;
    use std::fs::{self, OpenOptions};
    use std::io::{ErrorKind, Write as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use super::TrashError;

    /// Names tried for one entry before giving up: `name`, `name.2`, `name.3`…
    const NAMES_MOST: u32 = 10_000;

    /// The sticky bit, `S_ISVTX` in `<sys/stat.h>`: only an entry's owner may rename it.
    const STICKY: u32 = 0o1000;

    /// One user's trash cans: the home's, and each volume's own.
    #[derive(Debug, Clone)]
    pub(crate) struct Trash {
        /// `$XDG_DATA_HOME/Trash`.
        home: PathBuf,
        /// Whose `.Trash/$uid` and `.Trash-$uid` a volume's are.
        uid: u32,
    }

    /// One trash can: where an entry goes, and how its `Path=` is said.
    struct Can {
        dir: PathBuf,
        /// The volume's top directory, which `Path=` is relative to; `None` in the home trash,
        /// whose `Path=` is absolute.
        top: Option<PathBuf>,
    }

    impl Trash {
        /// The trash cans of user `uid`, whose home trash is `home`.
        pub(crate) const fn new(home: PathBuf, uid: u32) -> Self {
            Self { home, uid }
        }

        /// The home trash.
        #[cfg(test)]
        pub(crate) fn home(&self) -> &Path {
            &self.home
        }

        /// This user's, as the environment says now.
        #[cfg(not(target_os = "macos"))]
        pub(crate) fn for_user() -> Self {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| crate::dirs::home().join(".local").join("share"));
            Self::new(data.join("Trash"), rustix::process::getuid().as_raw())
        }

        /// Move `path` into the trash can of its volume, its info file written first so the
        /// entry is never in the trash without it, and say where it landed.
        pub(crate) fn put(&self, path: &Path, now: SystemTime) -> Result<PathBuf, TrashError> {
            fs::symlink_metadata(path)?;
            let name = path.file_name().ok_or(TrashError::NoTrash)?;
            let parent = fs::canonicalize(path.parent().ok_or(TrashError::NoTrash)?)?;
            let path = parent.join(name);
            let dev = fs::metadata(&parent)?.dev();
            let can = self.can(&parent, dev)?;
            let said = match &can.top {
                None => path.clone(),
                Some(top) => {
                    path.strip_prefix(top).map_or_else(|_| path.clone(), Path::to_path_buf)
                }
            };
            let info = info_text(&said, now);
            for n in 1..=NAMES_MOST {
                let mut trashed = name.to_os_string();
                if n > 1 {
                    trashed.push(format!(".{n}"));
                }
                let mut info_name = trashed.clone();
                info_name.push(".trashinfo");
                let info_path = can.dir.join("info").join(&info_name);
                let made =
                    OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info_path);
                let mut file = match made {
                    Ok(file) => file,
                    Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e.into()),
                };
                let to = can.dir.join("files").join(&trashed);
                let moved = file
                    .write_all(info.as_bytes())
                    .and_then(|()| file.sync_all())
                    .and_then(|()| crate::fs::rename_new(&path, &to));
                match moved {
                    Ok(()) => return Ok(to),
                    Err(e) => {
                        // The info file is this call's own, made a moment ago.
                        let _gone = fs::remove_file(&info_path);
                        if e.kind() != ErrorKind::AlreadyExists {
                            return Err(e.into());
                        }
                    }
                }
            }
            Err(std::io::Error::new(ErrorKind::AlreadyExists, "no free name in the trash").into())
        }

        /// The can for an entry in `parent`, on device `dev`: the home trash when it is on the
        /// same volume, else the volume's own.
        fn can(&self, parent: &Path, dev: u64) -> Result<Can, TrashError> {
            if existing(&self.home).is_some_and(|at| at.dev() == dev) {
                make_private(&self.home)?;
                if fs::metadata(&self.home)?.dev() == dev {
                    return ready(Can { dir: self.home.clone(), top: None });
                }
            }
            let top = top_of(parent, dev);
            let shared = top.join(".Trash");
            // The administrator's shared trash counts only as a real, sticky directory.
            let sound = fs::symlink_metadata(&shared)
                .is_ok_and(|m| m.file_type().is_dir() && m.mode() & STICKY != 0);
            if sound {
                let own = shared.join(self.uid.to_string());
                if make_private(&own).is_ok() && owned_dir(&own, self.uid) {
                    return ready(Can { dir: own, top: Some(top) });
                }
            }
            let own = top.join(format!(".Trash-{}", self.uid));
            if make_private(&own).is_ok() && owned_dir(&own, self.uid) {
                return ready(Can { dir: own, top: Some(top) });
            }
            Err(TrashError::NoTrash)
        }
    }

    /// `can` with its `files` and `info` made.
    fn ready(can: Can) -> Result<Can, TrashError> {
        for sub in ["files", "info"] {
            make_private(&can.dir.join(sub))?;
        }
        Ok(can)
    }

    /// Make `dir` and any missing parents, each new one only its owner's.
    fn make_private(dir: &Path) -> std::io::Result<()> {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
    }

    /// `dir` is a directory, not a link, and `uid`'s.
    fn owned_dir(dir: &Path, uid: u32) -> bool {
        fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir() && m.uid() == uid)
    }

    /// `path`'s metadata, or its nearest existing ancestor's.
    fn existing(path: &Path) -> Option<fs::Metadata> {
        path.ancestors().find_map(|at| fs::metadata(at).ok())
    }

    /// The top directory of the volume `dir` is on (device `dev`): its highest ancestor still on
    /// that device.
    fn top_of(dir: &Path, dev: u64) -> PathBuf {
        dir.ancestors()
            .take_while(|at| fs::metadata(at).is_ok_and(|m| m.dev() == dev))
            .last()
            .unwrap_or(dir)
            .to_path_buf()
    }

    /// An info file's text: where the entry was, escaped as a URL path is, and when it went,
    /// in local time as the specification asks.
    pub(crate) fn info_text(path: &Path, now: SystemTime) -> String {
        format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            escape(path.as_os_str()),
            local_time(now)
        )
    }

    /// RFC 2396 escaping: every byte but the unreserved ones and `/` as `%XX`.
    pub(crate) fn escape(path: &OsStr) -> String {
        let mut out = String::with_capacity(path.len());
        for &b in path.as_bytes() {
            if b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b) {
                out.push(char::from(b));
            } else {
                out.push('%');
                for nibble in [b >> 4, b & 0xf] {
                    out.extend(
                        char::from_digit(u32::from(nibble), 16).map(|c| c.to_ascii_uppercase()),
                    );
                }
            }
        }
        out
    }

    /// `YYYY-MM-DDThh:mm:ss` in this machine's time zone.
    pub(crate) fn local_time(at: SystemTime) -> String {
        let secs = at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let t = libc::time_t::try_from(secs).unwrap_or(libc::time_t::MAX);
        // SAFETY: an all-zero `tm` is a valid value of the plain C struct.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: POSIX `localtime_r` reads the `time_t` and writes only the `tm` it is given,
        // both live for the call; it is the reentrant form, safe from any thread.
        let filled = unsafe { libc::localtime_r(&raw const t, &raw mut tm) };
        if filled.is_null() {
            return "1970-01-01T00:00:00".to_owned();
        }
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            i64::from(tm.tm_year).saturating_add(1900),
            tm.tm_mon.saturating_add(1),
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }

    /// The original path an info file names, unescaped; for the tests and for putting back.
    #[cfg(test)]
    pub(crate) fn unescape(said: &str) -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt as _;
        let mut pieces = said.split('%');
        let mut out = pieces.next().unwrap_or_default().as_bytes().to_vec();
        for piece in pieces {
            let byte = piece.get(..2).and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = byte {
                out.push(byte);
                out.extend_from_slice(piece.get(2..).unwrap_or_default().as_bytes());
            } else {
                out.push(b'%');
                out.extend_from_slice(piece.as_bytes());
            }
        }
        std::ffi::OsString::from_vec(out)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::freedesktop::{Trash, escape, info_text, local_time, unescape};
    use super::*;

    fn can(root: &Path) -> Trash {
        Trash::new(root.join("data/Trash"), rustix::process::getuid().as_raw())
    }

    /// An entry on the home's volume goes to the home trash under its own name, its info file
    /// naming where it was; a second of the same name gets the next free one, and a folder goes
    /// whole. Nothing is unlinked: every byte is in the trash.
    #[test]
    fn an_entry_goes_to_the_home_trash_with_its_info() {
        let root = tempfile::tempdir().unwrap();
        let trash = can(root.path());
        let work = root.path().join("work");
        fs_write(&work.join("a b%.txt"), "one");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);

        let landed = trash.put(&work.join("a b%.txt"), now).unwrap();
        assert_eq!(landed, trash.home().join("files/a b%.txt"));
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "one");
        assert!(!work.join("a b%.txt").exists());
        let info = std::fs::read_to_string(trash.home().join("info/a b%.txt.trashinfo")).unwrap();
        let canonical = std::fs::canonicalize(&work).unwrap().join("a b%.txt");
        assert_eq!(info, info_text(&canonical, now));
        let said = info.lines().nth(1).unwrap().strip_prefix("Path=").unwrap();
        assert_eq!(unescape(said), canonical.as_os_str());

        fs_write(&work.join("a b%.txt"), "two");
        let again = trash.put(&work.join("a b%.txt"), now).unwrap();
        assert_eq!(again, trash.home().join("files/a b%.txt.2"));
        assert!(trash.home().join("info/a b%.txt.2.trashinfo").exists());

        fs_write(&work.join("dir/inner.txt"), "three");
        let dir = trash.put(&work.join("dir"), now).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("inner.txt")).unwrap(), "three");
        let mode = std::fs::metadata(trash.home()).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o700);
    }

    /// A link goes itself, not what it points to; nothing at the path is `NotFound`, and no
    /// info file is left behind for it.
    #[test]
    fn a_link_goes_itself_and_nothing_is_not_found() {
        let root = tempfile::tempdir().unwrap();
        let trash = can(root.path());
        let work = root.path().join("work");
        fs_write(&work.join("target.txt"), "kept");
        std::os::unix::fs::symlink("target.txt", work.join("link")).unwrap();
        let landed = trash.put(&work.join("link"), SystemTime::now()).unwrap();
        assert!(std::fs::symlink_metadata(&landed).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(work.join("target.txt")).unwrap(), "kept");

        let gone = trash.put(&work.join("nope"), SystemTime::now()).unwrap_err();
        assert!(matches!(&gone, TrashError::Os(e) if e.kind() == std::io::ErrorKind::NotFound));
        let infos = std::fs::read_dir(trash.home().join("info")).unwrap().count();
        assert_eq!(infos, 1, "only the link's");
    }

    #[test]
    fn a_path_is_escaped_as_a_url_path_and_back() {
        let odd = std::ffi::OsStr::new("/w/a b/ü%#?.txt");
        assert_eq!(escape(odd), "/w/a%20b/%C3%BC%25%23%3F.txt");
        assert_eq!(unescape(&escape(odd)), odd);
    }

    #[test]
    fn a_deletion_date_is_a_plain_local_time() {
        let said = local_time(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(said.len(), "2023-11-14T22:13:20".len(), "{said}");
        assert!(said.starts_with("2023-11-1"), "{said}");
        assert_eq!(said.as_bytes()[10], b'T');
    }

    /// The Finder's trash takes a file on a Mac and says where it landed; the test takes it
    /// back out again, so nothing of it stays in the person's trash.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_finder_trash_takes_a_file_and_says_where() {
        let root = tempfile::tempdir().unwrap();
        let name = format!("slopty-trash-test-{}.txt", std::process::id());
        let file = root.path().join(&name);
        fs_write(&file, "bin me");
        let landed = trash(&file).unwrap();
        assert!(!file.exists());
        let back = std::fs::read_to_string(&landed);
        let _back_out = std::fs::rename(&landed, &file);
        assert_eq!(back.unwrap(), "bin me");
        assert!(
            landed.components().any(|c| c.as_os_str() == ".Trash" || c.as_os_str() == ".Trashes"),
            "{landed:?}"
        );
        assert!(matches!(trash(&file.with_extension("gone")), Err(TrashError::Os(_))));
    }

    fn fs_write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}
