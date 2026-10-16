//! A repository followed on one `FSEvents` stream over its worktree and the git directories
//! outside it, however deep: an ignored folder churning costs the stream's delivery and a match,
//! and no kernel watch a folder.

use std::path::Path;
use std::sync::Arc;

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
            let _said = repo.heard(path, dir);
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
