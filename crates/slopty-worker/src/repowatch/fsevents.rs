//! A repository followed on one `FSEvents` stream over its worktree and the git directories
//! outside it, however deep: an ignored folder churning costs the stream's delivery and a match,
//! and no kernel watch a folder.

use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::Repo;
use crate::fsevents::{
    self, LOST, PATH_CHANGED, Starting, Wanted, kFSEventStreamEventFlagItemInodeMetaMod,
    kFSEventStreamEventFlagItemIsDir, kFSEventStreamEventFlagItemIsFile,
    kFSEventStreamEventFlagItemModified, kFSEventStreamEventFlagItemRemoved,
    kFSEventStreamEventFlagItemRenamed, kFSEventStreamEventFlagRootChanged,
    kFSEventStreamEventFlagUnmount,
};

/// Seconds the stream gathers events before it calls: a burst's events then come together,
/// well inside the follower's own `QUIET`.
const LATENCY: f64 = 0.01;

/// An item made, removed, renamed, written or given a new mode.
const CHANGED: fsevents::Flags =
    PATH_CHANGED | kFSEventStreamEventFlagItemModified | kFSEventStreamEventFlagItemInodeMetaMod;

/// The watched root itself moved or went with its volume.
const ROOT_WENT: fsevents::Flags =
    kFSEventStreamEventFlagRootChanged | kFSEventStreamEventFlagUnmount;

/// An item removed or renamed, in or out.
const WENT: fsevents::Flags =
    kFSEventStreamEventFlagItemRemoved | kFSEventStreamEventFlagItemRenamed;

/// A running stream over one repository, stopped when dropped.
#[derive(Debug)]
pub(super) struct Watch {
    _stream: Starting,
}

impl Watch {
    /// Start a stream over `repo`, started off this thread; the repository is touched once it is
    /// up, and given out to looks if `FSEvents` refuses it. `None` when it may hold no watch at
    /// all (`watches`), a stream being one.
    pub(super) fn start(repo: Arc<Repo>, watches: usize) -> Option<Self> {
        if watches == 0 {
            tracing::info!(top = %repo.place.top.display(), "a repository may hold no stream");
            return None;
        }
        let place = &repo.place;
        let mut paths = vec![place.top.clone()];
        for dir in [&place.common, &place.git] {
            if !paths.iter().any(|at| dir.starts_with(at)) {
                paths.push(dir.clone());
            }
        }
        let (up, refused) = (Arc::clone(&repo), Arc::clone(&repo));
        // When its start was asked for: what last changed before is read after the watch is up.
        #[expect(
            clippy::disallowed_methods,
            reason = "compared with files' modification times, which the system clock stamps"
        )]
        let began = SystemTime::now();
        let handler = Box::new(move |path: &Path, flags: fsevents::Flags| {
            let place = &repo.place;
            let itself = flags & WENT != 0 && (path == place.git || path == place.top);
            if flags & ROOT_WENT != 0 || itself {
                repo.displaced();
                return;
            }
            if flags & LOST != 0 {
                repo.lost();
                return;
            }
            if flags & CHANGED == 0 {
                return;
            }
            let dir = if flags & kFSEventStreamEventFlagItemIsDir != 0 {
                Some(true)
            } else if flags & kFSEventStreamEventFlagItemIsFile != 0 {
                Some(false)
            } else {
                None
            };
            // fseventsd numbers a write only once it reads it, so a stream it starts while it
            // lags is handed, late, what changed before: the report when the watch came up
            // covers that, and a word for it again would say nothing new.
            let _said = repo.heard(path, dir, || !changed_before(path, began));
        });
        let wanted = Wanted { paths, latency: LATENCY, label: "io.slopty.repowatch", handler };
        let stream = Starting::spawn_or(
            wanted,
            None,
            Box::new(move || up.heard.came_up(&up.place.top)),
            Box::new(move || refused.heard.gave_out(&refused.place.top)),
        );
        Some(Self { _stream: stream })
    }
}

/// Whether `path` last changed (its ctime, which a write, a rename, a mode or a link count
/// moves, and no program can set back) before `began`. A path that is gone did not.
fn changed_before(path: &Path, began: SystemTime) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    let (Ok(secs), Ok(nanos)) = (u64::try_from(meta.ctime()), u32::try_from(meta.ctime_nsec()))
    else {
        return false;
    };
    UNIX_EPOCH.checked_add(Duration::new(secs, nanos)).is_some_and(|changed| changed < began)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path whose last change came before the watch began is old news; one that changed
    /// as or after it began, and one gone, are not.
    #[test]
    fn only_a_change_since_the_watch_began_is_news() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "one").unwrap();
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let secs = u64::try_from(meta.ctime()).unwrap();
        let nanos = u32::try_from(meta.ctime_nsec()).unwrap();
        let changed = UNIX_EPOCH + Duration::new(secs, nanos);
        let tick = Duration::from_micros(1);
        assert!(changed_before(&file, changed + tick), "written before the watch began");
        assert!(!changed_before(&file, changed), "written as it began");
        assert!(!changed_before(&file, changed - tick), "written after it began");
        assert!(!changed_before(&dir.path().join("gone"), changed + tick), "a path that is gone");
    }
}
