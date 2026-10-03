//! A drag out of an app on a worker, the workspace's half (`docs/decisions/audio.md`, "Drag
//! out").
//!
//! The tile hands a drag out over as the pointer leaves it with the button held
//! ([`crate::screen::ScreenViewEvent::DragOut`]), and the workspace drags its items on from
//! that very mouse event as this Mac's own drag: each file a promise kept by bringing it down
//! from the worker, a promised one once the worker's catch has called it in, and each data item
//! given from what the catch kept as a target reads it. The link hands the catch to the drag
//! as it arrives, so a read on the main thread before it is in waits for it there.
//!
//! Each drag goes with a tag of its own ([`DragsOut`]), so when it comes back over a tile of
//! the worker it came from, the drop there names the worker's own files and nothing moves
//! (`drop_in`).

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use slopty_client::clip::Fetched;
use slopty_client::dnd::out::{DataAt, FileAt, Offer, Shared};
use slopty_client::layout::WorkerKey;
use slopty_platform::drag::{Data, Give, Keep, Promise};

use super::{WorkspaceView, bring_down_to, promise};

/// How long a promised file's keeping waits for the worker's catch to name it: an app writes a
/// promised file inside the catch's drop, for as long as it takes.
const CATCH_WAIT: Duration = Duration::from_secs(30);

/// How long a target reading data the catch kept waits for it to come down.
const FETCH_WAIT: Duration = Duration::from_secs(5);

/// How long a target reading data before the worker's catch is in waits for it, on the main
/// thread: the catcher's showing and four carries over it take about a second at most. A catch
/// held past this by a slow promise in the same drag gives that read nothing.
const CAUGHT_WAIT: Duration = Duration::from_secs(3);

/// Drags out of a worker's app this window carried on, newest last.
const DRAGS_OUT: usize = 4;

/// The drags out this window carried on, by the tag each went with, and the worker each came
/// from.
#[derive(Debug, Default)]
pub(in crate::workspace) struct DragsOut {
    next: u64,
    live: VecDeque<(u64, WorkerKey, Arc<Shared>)>,
}

impl DragsOut {
    /// The tag `worker`'s drag out `shared` goes with.
    fn tag(&mut self, worker: WorkerKey, shared: &Arc<Shared>) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.live.push_back((self.next, worker, Arc::clone(shared)));
        while self.live.len() > DRAGS_OUT {
            self.live.pop_front();
        }
        self.next
    }

    /// The drag out tagged `tag`, when it came from `worker`.
    pub(in crate::workspace) fn from(&self, tag: u64, worker: WorkerKey) -> Option<&Arc<Shared>> {
        self.live.iter().find(|(t, w, _)| *t == tag && *w == worker).map(|(_, _, shared)| shared)
    }
}

impl WorkspaceView {
    /// Drag what the worker `worker`'s drag out carries on from the mouse event being handled.
    /// Whether a drag began.
    pub(in crate::workspace) fn drag_out_of(
        &mut self,
        worker: WorkerKey,
        shared: &Arc<Shared>,
    ) -> bool {
        let Some(remote) = self.remote(worker) else { return false };
        remote.watch_drag_out(shared);
        let (mut files, mut data) = (Vec::new(), Vec::new());
        for offer in shared.offers() {
            match offer {
                Offer::File { path: Some(path), .. } => {
                    files.extend(promise(Arc::clone(&remote), &path));
                }
                Offer::File { name, path: None, promise: Some(n) } => {
                    let (remote, shared) = (Arc::clone(&remote), Arc::clone(shared));
                    let keep: Keep = Arc::new(move |dest| match shared.promised(n, CATCH_WAIT) {
                        FileAt::At(path) => bring_down_to(remote.as_ref(), &path, dest),
                        FileAt::Waiting => {
                            Err("the machine did not catch the drag in time".to_owned())
                        }
                        FileAt::Gone => Err("the app on the machine did not write it".to_owned()),
                    });
                    files.push(Promise { name, keep });
                }
                Offer::File { path: None, promise: None, .. } => {}
                Offer::Data { item, types } => {
                    let (remote, shared) = (Arc::clone(&remote), Arc::clone(shared));
                    let give: Give =
                        Arc::new(move |uti| match shared.data(item, uti, CAUGHT_WAIT) {
                            DataAt::Bytes(bytes) => Some(bytes),
                            DataAt::Fetch(rep) => match remote.clip_fetch(&rep, None, FETCH_WAIT) {
                                Fetched::Data(bytes) => Some(bytes),
                                Fetched::TooBig(_) | Fetched::Gone => None,
                            },
                            DataAt::Waiting | DataAt::Gone => None,
                        });
                    data.push(Data { types, give });
                }
            }
        }
        tracing::info!(drag = %shared.drag(), files = files.len(), data = data.len(), "a drag out goes on here");
        let tag = self.drags_out.tag(worker, shared);
        match &self.drag_sink {
            Some(sink) => sink(files),
            None => slopty_platform::drag::drag_out_items(files, data, Some(tag)),
        }
    }
}
