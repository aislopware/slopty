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

use std::collections::{HashMap, HashSet, VecDeque};

use bytes::Bytes;
use slopty_core::{ClientId, XferId};
use slopty_engine::{DropOperation, DropPoint, Dropped, Streamed};
use slopty_proto::WorkerMsg;
use slopty_proto::drag::{DragId, DragItem};
use slopty_proto::terminal::{DROP_HELD_MAX_BYTES, DropFrom, TermEvent, drop_offer};
use slopty_proto::transfer::{ClipMsg, ClipType, RepRef, Source};
use tokio::sync::{mpsc, oneshot};

use super::{Actor, SessionHandle, engine_error};

/// How many bytes of one stream of a drag may be handed to the session and not yet taken by
/// the program's input. Enough to keep the tty fed while the next chunks come; what waits on
/// the worker for a slow program is this, in base64, whatever the type's size.
const STREAM_WINDOW_BYTES: usize = 1 << 20;

/// Where a session posts the fetches of a viewer's drag: that viewer's connection.
pub type DragFetch = mpsc::Sender<WorkerMsg>;

/// Told once a chunk of a drag's stream is taken: held for the drop, or written to the
/// program's input. Dropped untold when no more of the stream is wanted.
pub type Taken = oneshot::Sender<()>;

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
        /// The stream they came on.
        stream: XferId,
        /// Told when they are taken; dropped when no more is wanted.
        taken: Taken,
    },
    /// The stream of representation `kind` of item `item` ended: `complete` when all of it
    /// came.
    End {
        /// Which item, counted from 0.
        item: u16,
        /// Which representation.
        kind: ClipType,
        /// The stream that ended.
        stream: XferId,
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

/// One representation of a viewer's drag streaming into its session, paced by the program:
/// no more than a window of it (1 MiB) waits to be taken.
#[derive(Debug)]
pub struct DragStream {
    session: SessionHandle,
    drag: DragId,
    item: u16,
    kind: ClipType,
    stream: XferId,
    /// Chunks handed over and not yet taken, with their lengths.
    waiting: VecDeque<(usize, oneshot::Receiver<()>)>,
    /// Their bytes.
    waiting_bytes: usize,
}

impl DragStream {
    pub(super) fn new(
        session: SessionHandle,
        drag: DragId,
        rep: (u16, ClipType),
        stream: XferId,
    ) -> Self {
        let (item, kind) = rep;
        Self { session, drag, item, kind, stream, waiting: VecDeque::new(), waiting_bytes: 0 }
    }

    /// Hand the session the next `bytes`, once the window has room for them. False when the
    /// session wants no more of the stream (or is gone): the stream should stop.
    pub async fn send(&mut self, bytes: Bytes) -> bool {
        loop {
            let full = self.waiting_bytes.saturating_add(bytes.len()) > STREAM_WINDOW_BYTES;
            let Some((len, taken)) = self.waiting.front_mut() else { break };
            let told = if full {
                taken.await
            } else {
                match taken.try_recv() {
                    Ok(()) => Ok(()),
                    Err(oneshot::error::TryRecvError::Empty) => break,
                    Err(oneshot::error::TryRecvError::Closed) => return false,
                }
            };
            if told.is_err() {
                return false;
            }
            self.waiting_bytes = self.waiting_bytes.saturating_sub(*len);
            self.waiting.pop_front();
        }
        let len = bytes.len();
        let (taken, told) = oneshot::channel();
        let (item, kind, stream) = (self.item, self.kind.clone(), self.stream);
        let chunk = DragData::Chunk { item, kind, bytes, stream, taken };
        if self.session.drag_data(self.drag, chunk).is_err() {
            return false;
        }
        self.waiting.push_back((len, told));
        self.waiting_bytes = self.waiting_bytes.saturating_add(len);
        true
    }

    /// The stream ended: `complete` when all of it came.
    pub fn end(self, complete: bool) {
        let end = DragData::End { item: self.item, kind: self.kind, stream: self.stream, complete };
        let _gone = self.session.drag_data(self.drag, end);
    }
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
    /// Chunks streamed into the program's input, each told it was taken once the input is
    /// written up to where it ends.
    taken: VecDeque<(u64, Taken)>,
}

impl Drops {
    pub(super) const fn new(accepts: bool) -> Self {
        Self { accepts, viewer: None, hover: None, dropped: None, taken: VecDeque::new() }
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
    /// The bytes of each representation streaming after the drop that the engine holds for a
    /// request still to come. Past [`DROP_HELD_MAX_BYTES`] the stream is failed.
    held: HashMap<(u16, ClipType), usize>,
    /// The streams told to stop: what is still on its way of them is not heard, so it never
    /// mixes with a later stream of the same representation.
    stopped: HashSet<XferId>,
}

/// Bytes of a drag that arrived before its drop.
#[derive(Debug)]
enum Early {
    Whole(Vec<u8>),
    Coming(Vec<u8>),
    /// Too big to hold: the program's request fetches it, streamed.
    Over,
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
            held: HashMap::new(),
            stopped: HashSet::new(),
        }
    }

    /// More of a stream of representation `kind` of item `item`, held until the drop: false
    /// when no more of it is wanted, held or not.
    fn hold_chunk(&mut self, item: u16, kind: ClipType, bytes: &[u8], stream: XferId) -> bool {
        if self.stopped.contains(&stream) {
            return false;
        }
        let from = DropFrom::Rep { item, kind };
        let (early, wanted) = match self.early.remove(&from) {
            None => (Early::Coming(Vec::new()), true),
            Some(Early::Coming(held)) => (Early::Coming(held), true),
            Some(early @ Early::Over) => (early, false),
            Some(Early::Whole(_) | Early::Gone) => (Early::Gone, false),
        };
        let early = match early {
            Early::Coming(held) if held.len().saturating_add(bytes.len()) > DROP_HELD_MAX_BYTES => {
                Early::Over
            }
            Early::Coming(mut held) => {
                held.extend_from_slice(bytes);
                Early::Coming(held)
            }
            early => early,
        };
        let wanted = wanted && matches!(early, Early::Coming(_));
        self.early.insert(from, early);
        if !wanted {
            self.stopped.insert(stream);
        }
        wanted
    }

    /// The indices of the program's list that `from` gives.
    fn indices<'a>(&'a self, from: &'a DropFrom) -> impl Iterator<Item = usize> + 'a {
        self.offer.iter().enumerate().filter(move |(_, (_, f))| f == from).map(|(i, _)| i)
    }

    /// `data`, held until the drop. A type past [`DROP_HELD_MAX_BYTES`] is not held: its
    /// stream is stopped, and the program's request fetches it.
    fn hold(&mut self, data: DragData) {
        let (from, early) = match data {
            DragData::Whole { item, kind, bytes } if bytes.len() <= DROP_HELD_MAX_BYTES => {
                (DropFrom::Rep { item, kind }, Early::Whole(bytes))
            }
            DragData::Whole { item, kind, .. } => (DropFrom::Rep { item, kind }, Early::Over),
            DragData::Gone { item, kind } => (DropFrom::Rep { item, kind }, Early::Gone),
            DragData::Chunk { item, kind, bytes, stream, taken } => {
                if self.hold_chunk(item, kind, &bytes, stream) {
                    let _streaming = taken.send(());
                }
                return;
            }
            DragData::End { stream, .. } if self.stopped.contains(&stream) => return,
            DragData::End { item, kind, complete, .. } => {
                let from = DropFrom::Rep { item, kind };
                let early = match self.early.remove(&from) {
                    Some(Early::Coming(held)) if complete => Early::Whole(held),
                    None if complete => Early::Whole(Vec::new()),
                    Some(Early::Whole(held)) => Early::Whole(held),
                    Some(Early::Over) => Early::Over,
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
        let mut taken = None;
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
            Act::Data {
                drag,
                data: DragData::Chunk { item, kind, bytes, stream, taken: told },
            } => self
                .drag_chunk(drag, (item, kind), &bytes, stream)
                .map(|wanted| taken = wanted.then_some(told)),
            Act::Data { drag, data } => self.drag_data(drag, data),
        };
        if let Err(e) = result {
            self.send_to(client, &engine_error(&e));
        }
        // The program's answers go to its input, and what it said back to the viewer.
        self.after_output();
        if let Some(taken) = taken {
            // Taken once the input is written up to the chunk's answer, so a program that reads
            // slowly holds back the stream rather than having the worker hold its bytes.
            self.drops.taken.push_back((self.input.queued, taken));
            self.input_taken();
        }
    }

    /// Tell the streams whose chunks the program's input has taken.
    pub(super) fn input_taken(&mut self) {
        while self.drops.taken.front().is_some_and(|(end, _)| *end <= self.input.written) {
            if let Some((_, taken)) = self.drops.taken.pop_front() {
                let _gone = taken.send(());
            }
        }
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
                                let _held_or_answered = self.engine.drop_chunk(index, bytes)?;
                                if let DropFrom::Rep { item, kind } = &from {
                                    drag.held.insert((*item, kind.clone()), bytes.len());
                                    drag.asked.insert((*item, kind.clone()));
                                }
                            }
                            // Not here: the program's request fetches it.
                            Early::Over => {}
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
                Early::Coming(_) | Early::Over | Early::Gone => None,
            };
            self.engine.drop_data(index, data)?;
        }
        Ok(())
    }

    /// More bytes of a stream of drag `drag`: held while it hovers, handed to the program once
    /// it is dropped. False when no more of the stream is wanted.
    fn drag_chunk(
        &mut self,
        drag: DragId,
        key: (u16, ClipType),
        bytes: &Bytes,
        stream: XferId,
    ) -> Result<bool, slopty_engine::EngineError> {
        if let Some(hover) = self.drops.hover.as_mut().filter(|d| d.id == drag) {
            let (item, kind) = key;
            return Ok(hover.hold_chunk(item, kind, bytes, stream));
        }
        // Concluded, or another drag's: late bytes add nothing.
        let Some(dropped) = self.drops.dropped.as_mut().filter(|d| d.id == drag) else {
            return Ok(false);
        };
        if dropped.stopped.contains(&stream) {
            return Ok(false);
        }
        let from = DropFrom::Rep { item: key.0, kind: key.1.clone() };
        let indices: Vec<usize> = dropped.indices(&from).collect();
        let mut went = Vec::with_capacity(indices.len());
        for &index in &indices {
            went.push(self.engine.drop_chunk(index, bytes)?);
        }
        let Some(dropped) = self.drops.dropped.as_mut() else { return Ok(false) };
        if went.contains(&Streamed::Held) {
            let held = dropped.held.entry(key.clone()).or_default();
            *held = held.saturating_add(bytes.len());
            // Past the cap of what is held, the held copies fail; a request it is streaming
            // into goes on.
            if *held > DROP_HELD_MAX_BYTES {
                dropped.held.remove(&key);
                for (&index, _) in indices.iter().zip(&went).filter(|(_, w)| **w == Streamed::Held)
                {
                    self.engine.drop_end(index, false)?;
                }
                went.retain(|w| *w == Streamed::Answered);
            }
        }
        let wanted = went.iter().any(|w| *w != Streamed::Unwanted);
        if !wanted && let Some(dropped) = self.drops.dropped.as_mut() {
            dropped.asked.remove(&key);
            dropped.stopped.insert(stream);
        }
        Ok(wanted)
    }

    /// Bytes of drag `drag` other than a stream's next: held while it hovers, handed to the
    /// program once it is dropped.
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
        if let DragData::End { stream, .. } = &data
            && dropped.stopped.contains(stream)
        {
            return Ok(());
        }
        for key in sources {
            let from = DropFrom::Rep { item: key.0, kind: key.1.clone() };
            let indices: Vec<usize> = dropped.indices(&from).collect();
            dropped.held.remove(&key);
            dropped.asked.remove(&key);
            for index in indices {
                match &data {
                    DragData::Whole { bytes, .. } if bytes.len() <= DROP_HELD_MAX_BYTES => {
                        self.engine.drop_data(index, Some(bytes.clone()))?;
                    }
                    DragData::End { complete, .. } => self.engine.drop_end(index, *complete)?,
                    DragData::Chunk { .. }
                    | DragData::Whole { .. }
                    | DragData::Gone { .. }
                    | DragData::AllGone => {
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
        // Streamed into the answer as the program reads it, so no size is too big.
        let msg = WorkerMsg::Clip(ClipMsg::Fetch { rep, max: None, urgent: true });
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
        // Untold, the streams still going into it stop.
        self.drops.taken.clear();
        if let Some(viewer) = self.drops.viewer {
            self.send_to(viewer, &TermEvent::DropConcluded { operation });
        }
    }
}
