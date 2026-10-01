//! A drag out of an app on a worker, the workspace's half (`docs/decisions/audio.md`, "Drag
//! out").
//!
//! The tile hands a drag out over as the pointer leaves it with the button held
//! ([`crate::screen::ScreenViewEvent::DragOut`]), and the workspace drags its items on from
//! that very mouse event as this Mac's own drag: each file a promise kept by bringing it down
//! from the worker, a promised one once the worker's catch has called it in, and each data item
//! given from what the catch kept as a target reads it.

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

impl WorkspaceView {
    /// Drag what the worker `worker`'s drag out carries on from the mouse event being handled.
    /// Whether a drag began.
    pub(in crate::workspace) fn drag_out_of(
        &self,
        worker: WorkerKey,
        shared: &Arc<Shared>,
    ) -> bool {
        let Some(remote) = self.remote(worker) else { return false };
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
                            Err("the worker did not catch the drag in time".to_owned())
                        }
                        FileAt::Gone => Err("the app on the worker did not write it".to_owned()),
                    });
                    files.push(Promise { name, keep });
                }
                Offer::File { path: None, promise: None, .. } => {}
                Offer::Data { item, types } => {
                    let (remote, shared) = (Arc::clone(&remote), Arc::clone(shared));
                    let give: Give = Arc::new(move |uti| match shared.data(item, uti) {
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
        match &self.drag_sink {
            Some(sink) => sink(files),
            None => slopty_platform::drag::drag_out_items(files, data),
        }
    }
}
