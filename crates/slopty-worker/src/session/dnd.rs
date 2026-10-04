//! A viewer's drag and drop onto a program that asks for drops through the Kitty drag and drop
//! protocol (OSC 72).
//!
//! While the program asks, a drag over the tile is the program's to accept, and a drop gives
//! it the types it reads. A drag names its items as it enters ([`TermRequest::DragEnter`]),
//! and the program is offered every type they carry ([`drop_offer`]), with none of their bytes
//! here yet: the viewer pushes what the program accepted during the hover, and the rest is
//! fetched from it when the program asks ([`ClipMsg::Fetch`] under [`Source::Drag`]). Files go
//! as `file://` URLs of the copies the viewer uploaded to this machine, never of its own files:
//! the upload names them once it lands ([`TermRequest::DropFiles`]). A drop the program never
//! accepted is refused.
//!
//! [`TermRequest::DragEnter`]: slopty_proto::terminal::TermRequest::DragEnter
//! [`TermRequest::DropFiles`]: slopty_proto::terminal::TermRequest::DropFiles

use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use slopty_core::ClientId;
use slopty_engine::{DropOperation, DropPoint, Dropped};
use slopty_proto::WorkerMsg;
use slopty_proto::drag::{DragId, DragItem};
use slopty_proto::terminal::{DropFrom, TermEvent, drop_offer};
use slopty_proto::transfer::{ClipMsg, ClipType, RepRef, Source};
use tokio::sync::mpsc;

use super::{Actor, engine_error};

/// The most bytes one type of a drop may carry to the program.
///
/// The program reads it on its input, base64 and all, so this keeps the answer well inside the
/// input queue, as a paste's copy is kept (`PASTE_CARRY_BYTES`). A larger one is answered as
/// not coming, and is fetched no further than this.
pub const MAX_DROP_REP_BYTES: usize = 8 * 1024 * 1024;

/// Where a session posts the fetches of a viewer's drag: that viewer's connection.
pub type DragFetch = mpsc::Sender<WorkerMsg>;

/// Bytes of a viewer's drag, pushed ahead of the drop or fetched for it.
#[derive(Debug)]
pub enum DragData {
    /// Representation `kind` of item `item`, whole.
    Whole {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
        /// Its bytes.
        bytes: Vec<u8>,
    },
    /// More of representation `kind` of item `item`, as its stream brings it.
    Chunk {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
        /// The next bytes.
        bytes: Bytes,
    },
    /// The stream of representation `kind` of item `item` ended: `complete` when all of it
    /// came.
    End {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
        /// All of it came.
        complete: bool,
    },
    /// Representation `kind` of item `item` will not come (too big, or not read).
    Gone {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
    },
    /// Nothing more of the drag will come: the viewer no longer has it.
    AllGone,
}

/// A viewer's part in a drag over the tile.
#[derive(Debug)]
pub(super) enum Act {
    /// Its drag `drag` of `items` entered the tile; what the program asks for and was not
    /// pushed is fetched through `fetch`, and is not coming without one.
    Enter { drag: DragId, items: Vec<DragItem>, fetch: Option<DragFetch> },
    /// Its drag is over the tile at `at`.
    Over { at: DropPoint },
    /// Its drag left the tile.
    Left,
    /// It dropped its drag at `at`.
    Drop { at: DropPoint },
    /// The files of its drag `drag` landed here, or will not come.
    Files { drag: DragId, landed: Option<Vec<String>> },
    /// Bytes of its drag `drag`.
    Data { drag: DragId, data: DragData },
}

/// The session's side of the protocol.
#[derive(Debug, Default)]
pub(super) struct Drops {
    /// The program asks for drops.
    accepts: bool,
    /// The viewer whose drag is over the tile or was dropped: the program's answers are its.
    viewer: Option<ClientId>,
    /// The drag over the tile.
    hover: Option<Drag>,
    /// The drop the program reads, until it concludes it.
    dropped: Option<Drag>,
}

impl Drops {
    pub(super) const fn new(accepts: bool) -> Self {
        Self { accepts, viewer: None, hover: None, dropped: None }
    }

    pub(super) const fn accepts(&self) -> bool {
        self.accepts
    }
}

/// One viewer's drag over the tile, or its drop.
#[derive(Debug)]
struct Drag {
    id: DragId,
    client: ClientId,
    fetch: Option<DragFetch>,
    /// The types it is offered as, by their index in the program's list.
    offer: Vec<(String, DropFrom)>,
    /// The offer's MIME types, as each move tells them.
    mimes: Vec<String>,
    /// What arrived for it before the drop.
    early: HashMap<DropFrom, Early>,
    /// The representations fetched and not yet whole or gone.
    asked: HashSet<(u16, ClipType)>,
    /// The bytes streamed so far of each representation streaming after the drop.
    streamed: HashMap<(u16, ClipType), usize>,
}

/// Bytes of a drag that arrived before its drop.
#[derive(Debug)]
enum Early {
    Whole(Vec<u8>),
    Coming(Vec<u8>),
    Gone,
}

impl Drag {
    fn new(id: DragId, client: ClientId, items: &[DragItem], fetch: Option<DragFetch>) -> Self {
        let offer = drop_offer(items);
        let mimes = offer.iter().map(|(mime, _)| mime.clone()).collect();
        Self {
            id,
            client,
            fetch,
            offer,
            mimes,
            early: HashMap::new(),
            asked: HashSet::new(),
            streamed: HashMap::new(),
        }
    }

    /// The indices of the program's list that `from` gives.
    fn indices<'a>(&'a self, from: &'a DropFrom) -> impl Iterator<Item = usize> + 'a {
        self.offer.iter().enumerate().filter(move |(_, (_, f))| f == from).map(|(i, _)| i)
    }

    /// `data`, held until the drop.
    fn hold(&mut self, data: DragData) {
        let (from, early) = match data {
            DragData::Whole { item, kind, bytes } if bytes.len() <= MAX_DROP_REP_BYTES => {
                (DropFrom::Rep { item, kind }, Early::Whole(bytes))
            }
            DragData::Whole { item, kind, .. } | DragData::Gone { item, kind } => {
                (DropFrom::Rep { item, kind }, Early::Gone)
            }
            DragData::Chunk { item, kind, bytes } => {
                let from = DropFrom::Rep { item, kind };
                let early = match self.early.remove(&from) {
                    None => Early::Coming(bytes.to_vec()),
                    Some(Early::Coming(mut held))
                        if held.len().saturating_add(bytes.len()) <= MAX_DROP_REP_BYTES =>
                    {
                        held.extend_from_slice(&bytes);
                        Early::Coming(held)
                    }
                    Some(_) => Early::Gone,
                };
                (from, early)
            }
            DragData::End { item, kind, complete } => {
                let from = DropFrom::Rep { item, kind };
                let early = match self.early.remove(&from) {
                    Some(Early::Coming(held)) if complete => Early::Whole(held),
                    None if complete => Early::Whole(Vec::new()),
                    Some(Early::Whole(held)) => Early::Whole(held),
                    _ => Early::Gone,
                };
                (from, early)
            }
            DragData::AllGone => {
                for (_, from) in &self.offer {
                    if matches!(from, DropFrom::Rep { .. }) {
                        self.early.insert(from.clone(), Early::Gone);
                    }
                }
                return;
            }
        };
        self.early.insert(from, early);
    }
}

/// The `text/uri-list` a program is given for `paths`, the worker's copies of a drop's files:
/// one `file://` URL a line, each ended by CRLF (RFC 2483). A path that is not absolute is
/// left out.
fn uri_list(paths: &[String]) -> Vec<u8> {
    let mut list = String::new();
    for path in paths.iter().map(std::path::Path::new).filter(|p| p.is_absolute()) {
        list.push_str(&crate::clip::file_url(path));
        list.push_str("\r\n");
    }
    list.into_bytes()
}

impl Actor {
    pub(super) fn dnd(&mut self, client: ClientId, act: Act) {
        let result = match act {
            Act::Enter { drag, items, fetch } => {
                self.drops.hover = Some(Drag::new(drag, client, &items, fetch));
                Ok(())
            }
            Act::Over { at } => match self.drops.hover.as_ref().filter(|d| d.client == client) {
                Some(drag) => {
                    let told = self.engine.drag_over(at, &drag.mimes);
                    self.drags(client);
                    told.map(|told| self.unless_told(client, told))
                }
                None => Ok(()),
            },
            Act::Left => {
                let _left = self.drops.hover.take_if(|d| d.client == client);
                if self.drops.viewer == Some(client) { self.engine.drag_left() } else { Ok(()) }
            }
            Act::Drop { at } => self.drop_at(client, at),
            Act::Files { drag, landed } => {
                let data = landed.map(|paths| uri_list(&paths));
                self.drag_files(client, drag, data)
            }
            Act::Data { drag, data } => self.drag_data(drag, data),
        };
        if let Err(e) = result {
            self.send_to(client, &engine_error(&e));
        }
        // The program's answers go to its input, and what it said back to the viewer.
        self.after_output();
    }

    /// The viewer whose drag `drag` is over the tile or was dropped.
    pub(super) fn drops_client(&self, drag: DragId) -> Option<ClientId> {
        [&self.drops.hover, &self.drops.dropped]
            .into_iter()
            .flatten()
            .find(|d| d.id == drag)
            .map(|d| d.client)
    }

    /// `client` dropped its drag at `at`: the program has it, with what arrived for it so far,
    /// unless it never accepted it.
    fn drop_at(
        &mut self,
        client: ClientId,
        at: DropPoint,
    ) -> Result<(), slopty_engine::EngineError> {
        let Some(drag) = self.drops.hover.take_if(|d| d.client == client) else {
            // Left already, or never entered: there is nothing to give.
            self.send_to(client, &TermEvent::DropConcluded { operation: DropOperation::None });
            return Ok(());
        };
        match self.engine.dropped(at, &drag.mimes)? {
            Dropped::NotAsked => self.unless_told(client, false),
            // The engine concluded it as nothing; that is this viewer's to hear.
            Dropped::Refused => self.drops.viewer = Some(client),
            Dropped::Given => {
                self.drags(client);
                let mut drag = drag;
                let early = std::mem::take(&mut drag.early);
                for (from, early) in early {
                    let indices: Vec<usize> = drag.indices(&from).collect();
                    for index in indices {
                        match &early {
                            Early::Whole(bytes) => {
                                self.engine.drop_data(index, Some(bytes.clone()))?;
                            }
                            Early::Coming(bytes) => {
                                self.engine.drop_chunk(index, bytes)?;
                                if let DropFrom::Rep { item, kind } = &from {
                                    drag.streamed.insert((*item, kind.clone()), bytes.len());
                                    drag.asked.insert((*item, kind.clone()));
                                }
                            }
                            Early::Gone => self.engine.drop_data(index, None)?,
                        }
                    }
                }
                self.drops.dropped = Some(drag);
            }
        }
        Ok(())
    }

    /// The files of `client`'s drag `drag` landed, as `list`, or will not come.
    fn drag_files(
        &mut self,
        client: ClientId,
        drag: DragId,
        list: Option<Vec<u8>>,
    ) -> Result<(), slopty_engine::EngineError> {
        let early = list.map_or(Early::Gone, Early::Whole);
        if let Some(hover) =
            self.drops.hover.as_mut().filter(|d| d.id == drag && d.client == client)
        {
            hover.early.insert(DropFrom::Files, early);
            return Ok(());
        }
        let Some(dropped) = self.drops.dropped.as_ref().filter(|d| d.id == drag) else {
            return Ok(());
        };
        let indices: Vec<usize> = dropped.indices(&DropFrom::Files).collect();
        for index in indices {
            let data = match &early {
                Early::Whole(list) => Some(list.clone()),
                Early::Coming(_) | Early::Gone => None,
            };
            self.engine.drop_data(index, data)?;
        }
        Ok(())
    }

    /// Bytes of drag `drag`: held while it hovers, handed to the program once it is dropped.
    fn drag_data(
        &mut self,
        drag: DragId,
        data: DragData,
    ) -> Result<(), slopty_engine::EngineError> {
        if let Some(hover) = self.drops.hover.as_mut().filter(|d| d.id == drag) {
            hover.hold(data);
            return Ok(());
        }
        let Some(dropped) = self.drops.dropped.as_mut().filter(|d| d.id == drag) else {
            // Concluded, or another drag's: late bytes add nothing.
            return Ok(());
        };
        let sources: Vec<(u16, ClipType)> = match &data {
            DragData::AllGone => dropped.asked.drain().collect(),
            DragData::Whole { item, kind, .. }
            | DragData::Chunk { item, kind, .. }
            | DragData::End { item, kind, .. }
            | DragData::Gone { item, kind } => vec![(*item, kind.clone())],
        };
        for (item, kind) in sources {
            let key = (item, kind);
            let from = DropFrom::Rep { item: key.0, kind: key.1.clone() };
            let indices: Vec<usize> = dropped.indices(&from).collect();
            // Past the cap a stream is as one that failed, and the rest of it is not heard.
            let over = match &data {
                DragData::Chunk { bytes, .. } => {
                    let streamed = dropped.streamed.entry(key.clone()).or_default();
                    let before = *streamed;
                    *streamed = streamed.saturating_add(bytes.len());
                    if before > MAX_DROP_REP_BYTES {
                        continue;
                    }
                    *streamed > MAX_DROP_REP_BYTES
                }
                DragData::End { .. } => {
                    dropped.streamed.remove(&key).is_some_and(|n| n > MAX_DROP_REP_BYTES)
                }
                _ => false,
            };
            if !matches!(data, DragData::Chunk { .. }) || over {
                dropped.asked.remove(&key);
            }
            for index in indices {
                match &data {
                    _ if over => self.engine.drop_end(index, false)?,
                    DragData::Whole { bytes, .. } if bytes.len() <= MAX_DROP_REP_BYTES => {
                        self.engine.drop_data(index, Some(bytes.clone()))?;
                    }
                    DragData::Chunk { bytes, .. } => self.engine.drop_chunk(index, bytes)?,
                    DragData::End { complete, .. } => self.engine.drop_end(index, *complete)?,
                    DragData::Whole { .. } | DragData::Gone { .. } | DragData::AllGone => {
                        self.engine.drop_data(index, None)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The program asked for the dropped type at `index`, which is not here: fetch it from
    /// the viewer, once, or answer that it is not coming.
    pub(super) fn drop_wants(&mut self, index: usize) {
        let Some(drag) = self.drops.dropped.as_mut() else { return };
        // The files come as their upload lands, never by a fetch.
        let Some((_, DropFrom::Rep { item, kind })) = drag.offer.get(index) else { return };
        let key = (*item, kind.clone());
        if !drag.asked.insert(key.clone()) {
            return;
        }
        let Some(fetch) = drag.fetch.clone() else {
            drag.asked.remove(&key);
            if let Err(e) = self.engine.drop_data(index, None) {
                tracing::debug!(session = %self.id, error = %e, "a drop request could not be failed");
            }
            // Its failure goes to the program now, not with whatever it next writes.
            self.after_output();
            return;
        };
        let rep = RepRef { source: Source::Drag(drag.id), item: key.0, kind: key.1 };
        let max = Some(MAX_DROP_REP_BYTES as u64);
        let msg = WorkerMsg::Clip(ClipMsg::Fetch { rep, max, urgent: true });
        tokio::task::spawn_local(async move {
            let _gone = fetch.send(msg).await;
        });
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

    /// The program is done with the drop, or another drag ended it, or it was refused.
    pub(super) fn drop_concluded(&mut self, operation: DropOperation) {
        self.drops.dropped = None;
        if let Some(viewer) = self.drops.viewer {
            self.send_to(viewer, &TermEvent::DropConcluded { operation });
        }
    }
}
