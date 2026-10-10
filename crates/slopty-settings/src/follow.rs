//! Following the file for changes, as every reader of it does: the app, and the worker and the
//! server, which apply an edit of their own table as they read it.
//!
//! A reader looks at the file's modification time and size once every [`POLL`] rather than
//! watching it. Editors save atomically (write a temporary file, rename it over the old one),
//! which ends a per-file `FSEvents` or kqueue watch and forces watching the directory; a `stat` a
//! second sees a create, a replace and a delete alike, costs nothing, and adds no dependency.

use std::path::Path;
use std::time::{Duration, SystemTime};

/// How often a reader looks at the file.
pub const POLL: Duration = Duration::from_secs(1);

/// What a reader compares between looks.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    exists: bool,
}

impl Stamp {
    /// The file's stamp now (a missing file has one too, so a delete is a change).
    #[must_use]
    pub fn of(path: &Path) -> Self {
        std::fs::metadata(path).map_or_else(
            |_| Self::default(),
            |m| Self { modified: m.modified().ok(), len: m.len(), exists: true },
        )
    }
}

/// The file's stamp as a reader last loaded or wrote it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Seen(Stamp);

impl Seen {
    /// The file at `path` as it is now, about to be loaded.
    #[must_use]
    pub fn of(path: &Path) -> Self {
        Self(Stamp::of(path))
    }

    /// Whether the file at `path` changed since it was last seen; it is seen now either way.
    pub fn changed(&mut self, path: &Path) -> bool {
        let now = Stamp::of(path);
        let changed = now != self.0;
        self.0 = now;
        changed
    }

    /// The reader wrote the file at `path` itself: that is no change to load again.
    pub fn saw(&mut self, path: &Path) {
        self.0 = Stamp::of(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_tracks_writes_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let missing = Stamp::of(&path);
        std::fs::write(&path, "a").unwrap();
        let one = Stamp::of(&path);
        assert_ne!(missing, one);
        std::fs::write(&path, "ab").unwrap();
        assert_ne!(one, Stamp::of(&path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(Stamp::of(&path), missing);
    }

    /// A change is seen once, and a reader's own write is none.
    #[test]
    fn a_change_is_seen_once_and_a_readers_own_write_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut seen = Seen::of(&path);
        assert!(!seen.changed(&path), "nothing yet");
        std::fs::write(&path, "[worker]\n").unwrap();
        assert!(seen.changed(&path), "a write from elsewhere");
        assert!(!seen.changed(&path), "seen once");
        std::fs::write(&path, "[network]\nallow = []\n").unwrap();
        seen.saw(&path);
        assert!(!seen.changed(&path), "its own write");
    }
}
