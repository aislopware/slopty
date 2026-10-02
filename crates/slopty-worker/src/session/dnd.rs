//! A viewer's drag and drop onto a program that asks for drops through the Kitty drag and drop
//! protocol (OSC 72).
//!
//! While the program asks, a drag over the tile is the program's to accept, and a drop gives
//! it the representations it reads. Files go as `file://` URLs of the copies the client
//! uploaded to this machine, never of the client's own files: the upload a drop already makes
//! names them, and a list still uploading is answered once it lands
//! ([`TermRequest::DropData`](slopty_proto::terminal::TermRequest::DropData)).

use slopty_core::ClientId;
use slopty_engine::{DropOperation, DropPoint, DropRep};
use slopty_proto::terminal::TermEvent;

use super::{Actor, engine_error};

/// The most bytes one representation of a drop may carry to the program. The program reads it
/// on its input, base64 and all, so this keeps the answer well inside the input queue, as a
/// paste's copy is kept (`PASTE_CARRY_BYTES`). A larger one is answered as not coming.
const MAX_DROP_REP_BYTES: usize = 8 * 1024 * 1024;

/// A viewer's part in a drag over the tile.
#[derive(Debug)]
pub(super) enum Act {
    /// The drag of `mimes` is over the tile at `at`.
    Over { at: DropPoint, mimes: Vec<String> },
    /// The drag left the tile.
    Left,
    /// The drag was dropped at `at`, as `reps`.
    Drop { at: DropPoint, reps: Vec<DropRep> },
    /// A representation of the drop that was still coming: its bytes, or `None` when it will
    /// not come.
    Data { mime: String, data: Option<Vec<u8>> },
}

/// The session's side of the protocol.
#[derive(Debug, Default)]
pub(super) struct Drops {
    /// The program asks for drops.
    accepts: bool,
    /// The viewer whose drag is over the tile or was dropped: the program's answers are its.
    viewer: Option<ClientId>,
}

impl Drops {
    pub(super) const fn new(accepts: bool) -> Self {
        Self { accepts, viewer: None }
    }

    pub(super) const fn accepts(&self) -> bool {
        self.accepts
    }
}

/// A representation within [`MAX_DROP_REP_BYTES`]; a larger one does not come.
const fn fits(data: &[u8]) -> bool {
    data.len() <= MAX_DROP_REP_BYTES
}

impl Actor {
    pub(super) fn dnd(&mut self, client: ClientId, act: Act) {
        let result = match act {
            Act::Over { at, mimes } => {
                let told = self.engine.drag_over(at, &mimes);
                self.drags(client);
                told.map(|told| self.unless_told(client, told))
            }
            Act::Left if self.drops.viewer == Some(client) => self.engine.drag_left(),
            Act::Drop { at, reps } => {
                // Listed, so the program sees what was dropped, and answered as not coming.
                let mut refused = Vec::new();
                let reps = reps
                    .into_iter()
                    .map(|r| match r.data {
                        Some(d) if !fits(&d) => {
                            refused.push(r.mime.clone());
                            DropRep { mime: r.mime, data: None }
                        }
                        data => DropRep { mime: r.mime, data },
                    })
                    .collect();
                let told = self.engine.dropped(at, reps);
                self.drags(client);
                told.map(|told| self.unless_told(client, told)).and_then(|()| {
                    refused.iter().try_for_each(|mime| self.engine.drop_data(mime, None))
                })
            }
            Act::Data { mime, data } if self.drops.viewer == Some(client) => {
                self.engine.drop_data(&mime, data.filter(|d| fits(d)))
            }
            // Another viewer's drag is over the tile now: this one's leave or data is late.
            Act::Left | Act::Data { .. } => Ok(()),
        };
        if let Err(e) = result {
            self.send_to(client, &engine_error(&e));
        }
        // The program's answers go to its input, and what it said back to the viewer.
        self.after_output();
    }

    /// The program stopped asking for drops before `client` heard: it hears now, and a drop
    /// it made goes the way a drop on a terminal goes without the protocol.
    fn unless_told(&mut self, client: ClientId, told: bool) {
        if !told {
            self.send_to(client, &TermEvent::DropTarget { accepts: false });
        }
    }

    /// `client` drags now. A drop the new drag ended is concluded to the viewer whose it was
    /// before the answers are `client`'s.
    fn drags(&mut self, client: ClientId) {
        self.after_output();
        self.drops.viewer = Some(client);
    }

    /// The program registered for drops, or stopped: every viewer hears it.
    pub(super) fn drop_target(&mut self, accepts: bool) {
        if self.drops.accepts != accepts {
            self.drops.accepts = accepts;
            self.broadcast(&TermEvent::DropTarget { accepts });
        }
    }

    /// The program answered the drag over the tile.
    pub(super) fn drop_accepted(&mut self, operation: DropOperation, mimes: Vec<String>) {
        if let Some(viewer) = self.drops.viewer {
            self.send_to(viewer, &TermEvent::DropAccepted { operation, mimes });
        }
    }

    /// The program is done with the drop, or another drag ended it.
    pub(super) fn drop_concluded(&mut self, operation: DropOperation) {
        if let Some(viewer) = self.drops.viewer {
            self.send_to(viewer, &TermEvent::DropConcluded { operation });
        }
    }
}
