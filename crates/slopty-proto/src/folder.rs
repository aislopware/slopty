//! A folder tile's listing: the worker reads a directory and says what is in it.

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
    /// A directory: its first [`FOLDER_ENTRIES`] entries, folders first, then by name in any
    /// case.
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
