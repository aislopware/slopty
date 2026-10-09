//! A folder tile's listing, page by page, and the changes it asks of the worker's files.
//!
//! The changes: a new folder, a move or rename, a trip to the worker OS's own trash, and a
//! file's contents replaced only while it is the version the asker saw.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use crate::orchestration::FileKind;

/// Entries a listing carries at most; the rest are only counted.
///
/// A listing is one frame on the control stream, and this keeps it near a file tile's read in
/// size, so it never holds a terminal's echo behind it for long.
pub const FOLDER_ENTRIES: u32 = 2000;

/// What the worker found at a path asked for as a folder.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Listing {
    /// A directory: at most [`FOLDER_ENTRIES`] of its entries in its order (folders first, then
    /// by name in any case), the first of them or those after a page's [`After`].
    Listed {
        /// The directory listed, absolute (`~` spelled out), for the tile to say and to go up
        /// from.
        dir: String,
        /// The kept entries, in order.
        entries: Vec<FolderEntry>,
        /// Every entry the directory holds; more than `entries` when the listing was cut.
        total: u32,
    },
    /// Something other than a directory is there, for the asker to open as a file.
    NotFolder,
    /// Nothing listable at the path: missing, or not permitted.
    Missing {
        /// The OS's word for it.
        error: String,
    },
}

/// One entry of a folder.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FolderEntry {
    /// Its name in the directory.
    pub name: String,
    /// What it is, a symbolic link followed: a link to a directory is a [`FileKind::Dir`], and
    /// only a link that points nowhere stays a [`FileKind::Symlink`].
    pub kind: FileKind,
    /// It is a symbolic link.
    pub link: bool,
    /// Hidden from a plain listing: a dot name, or the Mac's hidden flag.
    pub hidden: bool,
    /// Bytes, for a file.
    pub size: u64,
    /// Entries inside, for a directory that could be read.
    pub items: Option<u32>,
    /// Last modification.
    pub modified_ms: WallMs,
}

/// Where a page of a folder starts: just after this entry in the folder's order.
///
/// An entry added or removed meanwhile so neither repeats nor skips one. A page past the first
/// is asked for with `ClientMsg::FolderPage` and answered with `WorkerMsg::FolderPage`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct After {
    /// The entry counts as a folder (a link to one does).
    pub folder: bool,
    /// Its name in the directory.
    pub name: String,
}

impl After {
    /// The page after `entry`, the last of the one before it.
    #[must_use]
    pub fn of(entry: &FolderEntry) -> Self {
        Self { folder: entry.kind == FileKind::Dir, name: entry.name.clone() }
    }
}

/// A change to the worker's files, asked with `ClientMsg::FsOp` and answered with
/// `WorkerMsg::FsDone`.
///
/// Paths are absolute on the worker, or `~/…` in its home; none may climb with `..`. Nothing
/// is ever unlinked, and nothing is replaced but by a [`FsOp::Replace`], over the version its
/// asker saw.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FsOp {
    /// Make an empty folder `name` in `parent`.
    MakeDir {
        /// The folder to make it in, which must exist.
        parent: String,
        /// One plain name: no `/`, not `.` or `..`.
        name: String,
    },
    /// Move `from` to `to`, a rename when both are in one folder. Refused when anything is at
    /// `to`, when `to` is inside `from`, or when it is on another volume.
    Move {
        /// What to move.
        from: String,
        /// Where it goes, its new name last.
        to: String,
    },
    /// Move `path` to the worker OS's own trash, where the person can put it back: the Finder's
    /// on a Mac, the freedesktop.org trash on Linux.
    Trash {
        /// What to trash.
        path: String,
    },
    /// Put the file `with` in place of the file at `path`, only while `path` is still at
    /// `base`: a file saved where it was opened (Finder's save-back), never over a change made
    /// meanwhile, which is refused as [`FsRefusal::Changed`]. `with` is a file the asker sent up
    /// first (an upload into the worker's drop directory); it is taken, and goes once its
    /// contents are in place. The file at `path` keeps its mode, and a link there keeps
    /// pointing at the file it names, which is the one replaced.
    Replace {
        /// The file whose contents go.
        path: String,
        /// The file whose contents take their place.
        with: String,
        /// The version of `path` the new contents were made from.
        base: FileVersion,
    },
}

/// A version of a file, as a listing shows it ([`FolderEntry::size`],
/// [`FolderEntry::modified_ms`]): any write moves one or the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FileVersion {
    /// Bytes.
    pub size: u64,
    /// Last modification.
    pub modified_ms: WallMs,
}

/// How an [`FsOp`] went.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FsOutcome {
    /// Done: where the entry now is (the new folder, the moved entry, or its place in the
    /// trash).
    Done {
        /// Absolute, `~` spelled out.
        path: String,
    },
    /// Not tried: the op would do something it must not, or could not be done as asked.
    Refused(FsRefusal),
    /// Tried, and the OS said no.
    Failed {
        /// The OS's word for it.
        error: String,
    },
}

/// Why an [`FsOp`] was not tried.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FsRefusal {
    /// A path that is not absolute (nor `~/…`), or climbs with `..`.
    NotAbsolute {
        /// The path as asked.
        path: String,
    },
    /// A name that is empty, holds a `/` or a NUL, or is `.` or `..`.
    BadName {
        /// The name as asked.
        name: String,
    },
    /// A place no op may move or trash: the file system's root, a volume's, the home or a
    /// folder holding it.
    Protected {
        /// The path.
        path: String,
    },
    /// Something is already at the destination; it is left as it was.
    Clash {
        /// The destination.
        path: String,
    },
    /// Nothing is at the source, or the folder to make or move into is not there.
    Missing {
        /// What is not there.
        path: String,
    },
    /// A folder moved into itself or a folder inside it.
    IntoItself,
    /// The destination is on another volume, and a move only renames.
    OtherVolume,
    /// The volume keeps no trash the worker can use (a network share, a volume without a
    /// writable trash).
    NoTrash,
    /// The file is no longer the version the change was made from: someone wrote it meanwhile.
    /// It is left as it is.
    Changed {
        /// The version there now.
        now: FileVersion,
    },
}

/// Where the worker's `path` is under its home `home`, as `/`-separated names.
///
/// `""` is the home itself. `None` for a path outside the home, or one that climbs (`..`),
/// stays (`.`) or doubles a slash.
///
/// A worker's place in a client's file manager (its File Provider domain) is rooted at its home
/// and names each item by this path, so a worker's file is found there by it.
#[must_use]
pub fn under_home(home: &str, path: &str) -> Option<String> {
    let home = home.trim_end_matches('/');
    let rest = path.strip_prefix(home)?;
    if rest.is_empty() {
        return Some(String::new());
    }
    let rest = rest.strip_prefix('/')?.trim_end_matches('/');
    if rest.is_empty() {
        return Some(String::new());
    }
    let sound = rest.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    sound.then(|| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a path in the home is under it, never by a path that climbs, stays or has an empty
    /// part; a sibling whose name starts like the home is outside it.
    #[test]
    fn only_a_path_in_the_home_is_under_it() {
        let home = "/Users/dev";
        assert_eq!(under_home(home, "/Users/dev/a/b.png").as_deref(), Some("a/b.png"));
        assert_eq!(under_home(home, "/Users/dev/a/").as_deref(), Some("a"));
        assert_eq!(under_home("/Users/dev/", "/Users/dev/a").as_deref(), Some("a"));
        assert_eq!(under_home(home, "/Users/dev").as_deref(), Some(""));
        assert_eq!(under_home(home, "/Users/dev/").as_deref(), Some(""));
        for outside in
            ["/Users/devops/x", "/tmp/x", "/Users/dev/../root", "/Users/dev//x", "/Users/dev/./x"]
        {
            assert_eq!(under_home(home, outside), None, "{outside}");
        }
    }
}
