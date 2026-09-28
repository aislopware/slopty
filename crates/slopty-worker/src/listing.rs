//! What is in a directory: its first entries in some order, and the count of all of them.
//!
//! Orchestration's `ListDir` keeps them by name; a folder tile ([`folder`]) keeps folders first,
//! then names in any case.
//!
//! Only the names, and what the directory read says of each entry for free, are gathered from
//! the whole directory. An entry is looked at further only once it is kept, so a huge directory
//! costs its names and not a stat of each.

use std::collections::BinaryHeap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use slopty_proto::folder::{FOLDER_ENTRIES, FolderEntry, Listing};
use slopty_proto::orchestration::FileKind;

/// The names of the first `keep` entries of `path` in the order `key` gives them (a tie goes by
/// name), in that order, and how many entries the directory holds.
///
/// # Errors
///
/// The directory could not be read.
pub fn first<K: Ord>(
    path: &Path,
    keep: usize,
    mut key: impl FnMut(&std::fs::DirEntry) -> K,
) -> std::io::Result<(Vec<OsString>, u32)> {
    // The greatest kept entry on top, to be pushed out by a smaller one.
    let mut first: BinaryHeap<(K, OsString)> = BinaryHeap::with_capacity(keep.saturating_add(1));
    let mut total = 0_u32;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        total = total.saturating_add(1);
        let at = (key(&entry), entry.file_name());
        if first.len() < keep {
            first.push(at);
        } else if first.peek().is_some_and(|last| at < *last) {
            first.pop();
            first.push(at);
        }
    }
    Ok((first.into_sorted_vec().into_iter().map(|(_, name)| name).collect(), total))
}

/// A directory for a folder tile: its first [`FOLDER_ENTRIES`] entries, folders first, then by
/// name in any case; `NotFolder` when a file is there instead.
#[must_use]
pub fn folder(path: &Path) -> Listing {
    // Lexically clean (`a//b/./` is `a/b`), not resolved: a folder reached through a link keeps
    // the path it was reached by.
    let dir: PathBuf = crate::file::expand_home(path).components().collect();
    match std::fs::metadata(&dir) {
        Ok(meta) if !meta.is_dir() => return Listing::NotFolder,
        Ok(_) => {}
        Err(e) => return Listing::Missing { error: crate::file::os_word(&e) },
    }
    let keep = usize::try_from(FOLDER_ENTRIES).unwrap_or(usize::MAX);
    let (names, total) = match first(&dir, keep, folder_order) {
        Ok(listed) => listed,
        Err(e) => return Listing::Missing { error: crate::file::os_word(&e) },
    };
    let entries = names.iter().filter_map(|name| folder_entry(&dir, name)).collect();
    Listing::Listed { dir: dir.to_string_lossy().into_owned(), entries, total }
}

/// Folders before everything else, then the name with its case folded.
fn folder_order(entry: &std::fs::DirEntry) -> (bool, String) {
    let folder = entry.file_type().is_ok_and(|t| {
        if t.is_symlink() {
            std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
        } else {
            t.is_dir()
        }
    });
    (!folder, entry.file_name().to_string_lossy().to_lowercase())
}

/// One kept entry of `dir`, looked at; `None` when it went between the listing and the look.
fn folder_entry(dir: &Path, name: &OsStr) -> Option<FolderEntry> {
    let at = dir.join(name);
    let own = std::fs::symlink_metadata(&at).ok()?;
    let link = own.file_type().is_symlink();
    let target = if link { std::fs::metadata(&at).ok() } else { None };
    let meta = target.as_ref().unwrap_or(&own);
    let kind = if link && target.is_none() { FileKind::Symlink } else { kind(meta.file_type()) };
    let items = (kind == FileKind::Dir)
        .then(|| std::fs::read_dir(&at).ok())
        .flatten()
        .map(|entries| u32::try_from(entries.count()).unwrap_or(u32::MAX));
    let name = name.to_string_lossy().into_owned();
    Some(FolderEntry {
        hidden: name.starts_with('.') || flagged_hidden(&own),
        size: if kind == FileKind::File { meta.len() } else { 0 },
        modified_ms: modified_ms(meta),
        name,
        kind,
        link,
        items,
    })
}

/// The Finder's hidden flag (`chflags hidden`), which `~/Library` carries.
#[cfg(target_os = "macos")]
fn flagged_hidden(meta: &std::fs::Metadata) -> bool {
    std::os::macos::fs::MetadataExt::st_flags(meta) & libc::UF_HIDDEN != 0
}

/// Only a Mac flags an entry hidden.
#[cfg(not(target_os = "macos"))]
const fn flagged_hidden(_meta: &std::fs::Metadata) -> bool {
    false
}

/// What an entry is, as the wire says it.
pub(crate) fn kind(t: std::fs::FileType) -> FileKind {
    use std::os::unix::fs::FileTypeExt as _;
    if t.is_symlink() {
        FileKind::Symlink
    } else if t.is_dir() {
        FileKind::Dir
    } else if t.is_fifo() || t.is_socket() || t.is_block_device() || t.is_char_device() {
        FileKind::Other
    } else {
        FileKind::File
    }
}

/// Last modification; zero when the OS does not say.
pub(crate) fn modified_ms(meta: &std::fs::Metadata) -> slopty_core::WallMs {
    meta.modified().map_or(slopty_core::WallMs::ZERO, slopty_core::WallMs::of)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(listing: Listing) -> (String, Vec<FolderEntry>, u32) {
        match listing {
            Listing::Listed { dir, entries, total } => (dir, entries, total),
            other => panic!("not listed: {other:?}"),
        }
    }

    /// Folders come first, then everything else, each by name whatever its case; a dot name is
    /// hidden, a link to a folder counts as one, and a folder says how much it holds.
    #[test]
    fn a_folder_lists_folders_first_then_names_in_any_case() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        for name in ["b.txt", "A.md", "c.rs", ".env"] {
            std::fs::write(dir.join(name), b"12345").unwrap();
        }
        for name in ["src", "Docs", ".git"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("src/main.rs"), b"").unwrap();
        std::fs::write(dir.join("src/lib.rs"), b"").unwrap();
        std::os::unix::fs::symlink("src", dir.join("linked")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("broken")).unwrap();

        let (at, entries, total) = listed(folder(dir));
        assert_eq!(at, dir.to_string_lossy());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [".git", "Docs", "linked", "src", ".env", "A.md", "b.txt", "broken", "c.rs"]
        );
        assert_eq!(total, 9);
        let by = |name: &str| entries.iter().find(|e| e.name == name).unwrap();
        assert!(by(".env").hidden && by(".git").hidden && !by("src").hidden);
        assert_eq!((by("src").kind, by("src").items), (FileKind::Dir, Some(2)));
        assert_eq!((by("linked").kind, by("linked").link), (FileKind::Dir, true));
        assert_eq!((by("broken").kind, by("broken").link), (FileKind::Symlink, true));
        assert_eq!(
            (by("b.txt").kind, by("b.txt").size, by("b.txt").items),
            (FileKind::File, 5, None)
        );
        assert!(by("b.txt").modified_ms > slopty_core::WallMs::from_millis(1_700_000_000_000));
    }

    /// Past the cap only the first entries in the folder's order come, and the count is whole:
    /// the tile says the listing was cut.
    #[test]
    fn a_huge_folder_is_cut_at_the_cap_with_a_whole_count() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let files = FOLDER_ENTRIES + 5;
        for i in 0..files {
            std::fs::write(dir.join(format!("f{i:05}")), b"").unwrap();
        }
        std::fs::create_dir_all(dir.join("zz")).unwrap();
        let (_, entries, total) = listed(folder(dir));
        assert_eq!(total, files + 1);
        assert_eq!(entries.len(), usize::try_from(FOLDER_ENTRIES).unwrap());
        assert_eq!(entries[0].name, "zz", "a folder is kept first, even past the cap");
        assert_eq!(entries[1].name, "f00000");
    }

    /// A file asked for as a folder is said to be one; nothing there is missing with the OS's
    /// word; `~` is the worker's home, and the path comes back clean.
    #[test]
    fn a_file_is_not_a_folder_and_a_path_comes_back_clean() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("a.txt");
        std::fs::write(&file, b"").unwrap();
        assert_eq!(folder(&file), Listing::NotFolder);
        let Listing::Missing { error } = folder(&root.path().join("nope")) else { panic!() };
        assert_eq!(error, "No such file or directory");
        let messy = PathBuf::from(format!("{}//./", root.path().display()));
        assert_eq!(listed(folder(&messy)).0, root.path().to_string_lossy());
    }
}
