//! A worker's files as a File Provider names them: an identifier per item, its parent, and the
//! versions that tell the system when its copy is stale.
//!
//! The domain's root is the worker's home, so a worker shows in Finder as a place of its own,
//! as a synced folder does, and an item's identifier is its path under that home with `/`
//! between names ([`slopty_proto::folder::under_home`]): `""` is the home, `"src/main.rs"` a file
//! in it. A path is stable across
//! listings and needs no table, so the extension keeps nothing between its launches. A file
//! outside the home has no identifier: it is not in the domain.

use slopty_proto::folder::FolderEntry;
use slopty_proto::orchestration::FileKind;

/// The identifier of the domain's root: the worker's home.
pub const ROOT: &str = "";

/// One file or folder of a worker's home.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    /// Its path under the home; [`ROOT`] for the home.
    pub id: String,
    /// Its folder's identifier; the root's is the root's own.
    pub parent: String,
    /// Its name in its folder; the root is named for the worker.
    pub name: String,
    /// A folder, not a file.
    pub folder: bool,
    /// Bytes, for a file.
    pub size: u64,
    /// Last modification, in milliseconds since the epoch.
    pub modified_ms: u64,
    /// Entries inside, for a folder that could be read.
    pub children: Option<u32>,
    /// Hidden on the worker, as a Mac hides a file flagged so; a name starting with a dot is
    /// hidden by Finder itself.
    pub hidden: bool,
}

impl Item {
    /// The root, the home of the worker called `worker`.
    #[must_use]
    pub fn root(worker: &str) -> Self {
        Self {
            id: ROOT.to_owned(),
            parent: ROOT.to_owned(),
            name: worker.to_owned(),
            folder: true,
            size: 0,
            modified_ms: 0,
            children: None,
            hidden: false,
        }
    }

    /// The item `entry` of the folder `parent` lists. `None` for what a disk would not show
    /// as a file or a folder: a link that points nowhere, a pipe, a socket or a device, and a
    /// name no path can hold.
    #[must_use]
    pub fn of_entry(parent: &str, entry: &FolderEntry) -> Option<Self> {
        let folder = match entry.kind {
            FileKind::Dir => true,
            FileKind::File => false,
            FileKind::Symlink | FileKind::Other => return None,
        };
        Some(Self {
            id: child(parent, &entry.name)?,
            parent: parent.to_owned(),
            name: entry.name.clone(),
            folder,
            size: if folder { 0 } else { entry.size },
            modified_ms: entry.modified_ms.as_millis(),
            children: entry.items.filter(|_| folder),
            hidden: entry.hidden,
        })
    }

    /// The version of its contents: its size and modification time, which change whenever its
    /// bytes do, as `rsync` and `make` judge a file. The system fetches it again when this
    /// moves.
    #[must_use]
    pub fn content_version(&self) -> Vec<u8> {
        let mut version = Vec::with_capacity(16);
        version.extend_from_slice(&self.size.to_le_bytes());
        version.extend_from_slice(&self.modified_ms.to_le_bytes());
        version
    }

    /// The version of its metadata: the same pair, since a listing carries nothing more that
    /// the system shows.
    #[must_use]
    pub fn metadata_version(&self) -> Vec<u8> {
        self.content_version()
    }
}

/// The identifier of the entry `name` of the folder `parent`; `None` for a name that is no
/// single path component (empty, `.`, `..`, or holding a `/` or a NUL).
#[must_use]
pub fn child(parent: &str, name: &str) -> Option<String> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return None;
    }
    Some(if parent.is_empty() { name.to_owned() } else { format!("{parent}/{name}") })
}

/// The identifier of `id`'s folder; the root's is the root's own.
#[must_use]
pub fn parent(id: &str) -> &str {
    id.rsplit_once('/').map_or(ROOT, |(parent, _name)| parent)
}

/// `id`'s name in its folder; the root's is empty.
#[must_use]
pub fn name(id: &str) -> &str {
    id.rsplit_once('/').map_or(id, |(_parent, name)| name)
}

/// Where `id` is on the worker whose home is `home`, as a listing or a fetch names it.
#[must_use]
pub fn on_worker(home: &str, id: &str) -> String {
    let home = home.trim_end_matches('/');
    if id.is_empty() { home.to_owned() } else { format!("{home}/{id}") }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;

    use super::*;

    fn entry(name: &str, kind: FileKind, size: u64) -> FolderEntry {
        FolderEntry {
            name: name.to_owned(),
            kind,
            link: false,
            hidden: false,
            size,
            items: (kind == FileKind::Dir).then_some(3),
            modified_ms: WallMs::from_millis(1_790_000_000_000),
        }
    }

    /// An identifier is the path under the home: a child of the root is its name, a deeper
    /// one its parent's path and its name, and each gives back its parent and its name.
    #[test]
    fn an_identifier_is_the_path_under_the_home() {
        assert_eq!(child(ROOT, "src").as_deref(), Some("src"));
        assert_eq!(child("src", "main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!((parent("src/main.rs"), name("src/main.rs")), ("src", "main.rs"));
        assert_eq!((parent("src"), name("src")), (ROOT, "src"));
        assert_eq!((parent(ROOT), name(ROOT)), (ROOT, ROOT));
        assert_eq!(on_worker("/home/dev/", "src/main.rs"), "/home/dev/src/main.rs");
        assert_eq!(on_worker("/home/dev", ROOT), "/home/dev");
        for bad in ["", ".", "..", "a/b", "nul\0"] {
            assert_eq!(child("src", bad), None, "{bad:?}");
        }
    }

    /// A listing's files and folders are items, with a folder's count and a file's size; a
    /// dangling link, a pipe and a name no path holds are not.
    #[test]
    fn a_listing_shows_its_files_and_folders() {
        let file = Item::of_entry("src", &entry("main.rs", FileKind::File, 12)).unwrap();
        assert_eq!(
            (file.id.as_str(), file.parent.as_str(), file.folder),
            ("src/main.rs", "src", false)
        );
        assert_eq!((file.size, file.children), (12, None));
        let dir = Item::of_entry(ROOT, &entry("src", FileKind::Dir, 4096)).unwrap();
        assert_eq!(
            (dir.id.as_str(), dir.folder, dir.size, dir.children),
            ("src", true, 0, Some(3))
        );
        for (name, kind) in
            [("gone", FileKind::Symlink), ("fifo", FileKind::Other), ("..", FileKind::Dir)]
        {
            assert_eq!(Item::of_entry(ROOT, &entry(name, kind, 0)), None, "{name}");
        }
    }

    /// A file's version moves with its size or its modification time, and only then.
    #[test]
    fn a_files_version_moves_with_its_size_or_time() {
        let a = Item::of_entry(ROOT, &entry("a", FileKind::File, 5)).unwrap();
        let mut grown = a.clone();
        grown.size = 6;
        let mut touched = a.clone();
        touched.modified_ms += 1;
        let again = Item::of_entry(ROOT, &entry("a", FileKind::File, 5)).unwrap();
        assert_eq!(a.content_version(), again.content_version());
        assert_ne!(a.content_version(), grown.content_version());
        assert_ne!(a.content_version(), touched.content_version());
        assert_eq!(a.metadata_version(), a.content_version());
    }
}
