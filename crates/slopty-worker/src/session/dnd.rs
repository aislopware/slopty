//! A viewer's drag and drop onto a program that asks for drops through the Kitty drag and drop
//! protocol (OSC 72).
//!
//! While the program asks, a drag over the tile is the program's to accept, and a drop gives
//! it the representations it reads. Files go as `file://` URLs of the copies the client
//! uploaded to this machine, never of the client's own files: the upload a drop already makes
//! names them, and a list still uploading is answered once it lands ([`Act::Data`]).

use slopty_core::ClientId;
use slopty_engine::{DropOperation, DropPoint, DropRep};
use tokio::sync::oneshot;

use super::{Actor, Cmd, SessionHandle, engine_error};
use crate::WorkerError;

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
    /// What the viewer has heard of its drag.
    Heard { reply: oneshot::Sender<Vec<Dropped>> },
}

/// What a viewer's drag hears back from the program.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Dropped {
    /// The program answered the drag over the tile: what a drop would do, and the MIME types
    /// it wants, most wanted first.
    Accepted {
        /// What a drop would do.
        operation: DropOperation,
        /// The types it wants; empty when it did not say.
        mimes: Vec<String>,
    },
    /// The program is done with the drop, having done `operation` with it.
    Concluded {
        /// What it did.
        operation: DropOperation,
    },
}

/// The session's side of the protocol.
#[derive(Debug, Default)]
pub(super) struct Drops {
    /// The program asks for drops.
    accepts: bool,
    /// The viewer whose drag is over the tile or was dropped, and what it has heard.
    viewer: Option<(ClientId, Vec<Dropped>)>,
}

impl Drops {
    pub(super) const fn new(accepts: bool) -> Self {
        Self { accepts, viewer: None }
    }

    pub(super) const fn accepts(&self) -> bool {
        self.accepts
    }

    pub(super) const fn target(&mut self, accepts: bool) {
        self.accepts = accepts;
    }

    pub(super) fn accepted(&mut self, operation: DropOperation, mimes: Vec<String>) {
        self.hear(Dropped::Accepted { operation, mimes });
    }

    pub(super) fn concluded(&mut self, operation: DropOperation) {
        self.hear(Dropped::Concluded { operation });
    }

    fn hear(&mut self, what: Dropped) {
        if let Some((_, heard)) = &mut self.viewer {
            heard.push(what);
        }
    }

    /// `client` drags now: a drag of another viewer is no longer heard.
    fn drags(&mut self, client: ClientId) {
        if self.viewer.as_ref().is_none_or(|(c, _)| *c != client) {
            self.viewer = Some((client, Vec::new()));
        }
    }
}

impl SessionHandle {
    /// `client`'s drag of `mimes` is over the tile at `at`.
    pub fn drag_over(
        &self,
        client: ClientId,
        at: DropPoint,
        mimes: Vec<String>,
    ) -> Result<(), WorkerError> {
        self.send(Cmd::Dnd { client, act: Act::Over { at, mimes } })
    }

    /// `client`'s drag left the tile without dropping.
    pub fn drag_left(&self, client: ClientId) -> Result<(), WorkerError> {
        self.send(Cmd::Dnd { client, act: Act::Left })
    }

    /// `client` dropped `reps` on the tile at `at`.
    pub fn drop_on(
        &self,
        client: ClientId,
        at: DropPoint,
        reps: Vec<DropRep>,
    ) -> Result<(), WorkerError> {
        self.send(Cmd::Dnd { client, act: Act::Drop { at, reps } })
    }

    /// The bytes of `client`'s dropped `mime` that were still coming, or `None` when they will
    /// not come.
    pub fn drop_data(
        &self,
        client: ClientId,
        mime: String,
        data: Option<Vec<u8>>,
    ) -> Result<(), WorkerError> {
        self.send(Cmd::Dnd { client, act: Act::Data { mime, data } })
    }

    /// What `client`'s drag has heard from the program so far.
    pub async fn drop_heard(&self, client: ClientId) -> Result<Vec<Dropped>, WorkerError> {
        let (reply, rx) = oneshot::channel();
        self.send(Cmd::Dnd { client, act: Act::Heard { reply } })?;
        rx.await.map_err(|_gone| WorkerError::SessionClosed)
    }
}

impl Actor {
    pub(super) fn dnd(&mut self, client: ClientId, act: Act) {
        let result = match act {
            Act::Over { at, mimes } => {
                self.drops.drags(client);
                self.engine.drag_over(at, &mimes).map(drop)
            }
            Act::Left => self.engine.drag_left(),
            Act::Drop { at, reps } => {
                self.drops.drags(client);
                self.engine.dropped(at, reps).map(drop)
            }
            Act::Data { mime, data } => self.engine.drop_data(&mime, data),
            Act::Heard { reply } => {
                let heard = match &self.drops.viewer {
                    Some((c, heard)) if *c == client => heard.clone(),
                    _ => Vec::new(),
                };
                let _gone = reply.send(heard);
                Ok(())
            }
        };
        if let Err(e) = result {
            self.send_to(client, &engine_error(&e));
        }
        // The program's answers go to its input, and what it said back is heard.
        self.after_output();
    }
}
