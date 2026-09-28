//! A folder tile's listing: the worker reads a directory and says what is in it.

use serde::{Deserialize, Serialize};

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
    /// Last modification, milliseconds since the Unix epoch.
    pub modified_ms: u64,
}
