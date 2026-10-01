//! A drop from the client, carried on the worker as a real drag session that lands at the point
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! The client's drag enters a stream's tile: the stream's input thread raises a window stream's
//! window and maps the point ([`Act::Enter`]), the drag helper puts its source window there with
//! the drag's items ([`ToHelper::SourceAt`]), and the input thread presses into it through the
//! HID tap ([`Act::Press`]), which begins the helper's session from that press. From then on the
//! client's moves carry the drag ([`Act::Move`]), and the helper reads off the system cursor what
//! the target under it would do, which goes back to the client ([`DragEvent::Operation`]). The
//! files go up from the moment the drag enters, into the drag's own folder
//! ([`crate::xfer::Transfers::drag_dir`]), so most are whole by the drop. At the drop the release
//! waits until every file and every piece of data the items carry is here, then lets go at the
//! point ([`Act::Release`]), and the helper's session says what the target did
//! ([`DragEvent::Ended`]).
//!
//! [`DropIn`] decides all of it from what it hears ([`Heard`]) and says what to do ([`Act`]), so
//! every rule is tested without a window server, a helper or a client. [`Drags`] holds the one
//! drag a worker carries at a time, since it has one pointer, and routes what the transfers, the
//! clipboard streams and the helper hear to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use parking_lot::Mutex;
use slopty_core::ClientId;
use slopty_input::InputError;
use slopty_proto::dnd::{FromHelper, Given, SourceItem, ToHelper};
use slopty_proto::drag::{DragEvent, DragId, DragInput, DragItem, DragOp, DragOps};
use slopty_proto::transfer::ClipType;
use tokio::sync::mpsc;

/// How long the helper has to put its source up, and then the press to begin a session there.
pub const START_WAIT: Duration = Duration::from_secs(2);

/// How long a released drop has to end: a target reads inside its `performDragOperation:`, and
/// blocks there for as long as a promise takes (MEASUREMENTS.md, "the drag helper's roles").
pub const LAND_WAIT: Duration = Duration::from_secs(30);

/// Why a drop did not land, as the client says it.
pub const BUSY: &str = "another drag is crossing this worker";
/// The press into the helper's source began no session.
const NOT_STARTED: &str = "the drag did not start on the worker";
/// The session ended before the drop, from the worker's side.
const ENDED_THERE: &str = "the drag ended on the worker";
/// The release never heard how it ended.
const UNFINISHED: &str = "the drop did not finish on the worker";
/// The helper went away mid-drag.
const HELPER_GONE: &str = "the worker's drag helper stopped";

/// What a drop hears.
#[derive(Debug)]
pub enum Heard {
    /// The client's next input for the drag: a move, the drop or leaving.
    Input(DragInput),
    /// Where the input thread found the entering point, in global points ([`Act::Enter`]).
    Mapped(Result<(f64, f64), InputError>),
    /// The drag helper.
    Helper(FromHelper),
    /// A top-level entry of the drag's upload is whole: its name in the upload, and where it
    /// landed.
    Landed {
        /// The name.
        name: String,
        /// Where.
        path: PathBuf,
    },
    /// A representation the client sent up for an item; `None` when it was refused or cut.
    Data {
        /// Which item.
        item: u16,
        /// Which representation.
        kind: ClipType,
        /// The bytes.
        bytes: Option<Vec<u8>>,
    },
    /// The drag's upload failed: for a person to read.
    Failed(String),
    /// The helper went away.
    HelperGone,
    /// The deadline [`Act::Deadline`] armed passed.
    Late,
}

/// What a drop asks of the stream, the helper and the client, in order.
#[derive(Debug, PartialEq)]
pub enum Act {
    /// Begin feeding a drag through the HID tap and map `(x, y)`: answered by
    /// [`Heard::Mapped`].
    Enter {
        /// Stream pixels.
        x: f32,
        /// Stream pixels.
        y: f32,
    },
    /// Press at `(x, y)`, on the helper's source.
    Press {
        /// Stream pixels.
        x: f32,
        /// Stream pixels.
        y: f32,
    },
    /// Carry the drag to `(x, y)`.
    Move {
        /// Stream pixels.
        x: f32,
        /// Stream pixels.
        y: f32,
    },
    /// Let go: the drop, or before any press, the end of feeding the drag.
    Release,
    /// End with nothing dropped.
    Cancel,
    /// Tell the helper.
    Helper(ToHelper),
    /// Tell the client.
    Tell(DragEvent),
    /// Hear [`Heard::Late`] after this long, in place of any deadline armed before; `None`
    /// disarms it.
    Deadline(Option<Duration>),
    /// The drag is over here: the worker may carry another.
    Over,
}

/// Where a drop is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Asked the input thread where the point is.
    Mapping,
    /// Asked the helper to put its source there.
    Placing,
    /// Pressed into the source; waiting for its session.
    Pressed,
    /// The session is on: moves carry it.
    Live,
    /// Let go; waiting for how it ended.
    Released,
    /// Done.
    Over,
}

/// One representation an item carries, as the helper gives it.
#[derive(Debug)]
struct Rep {
    kind: ClipType,
    /// Its uniform type identifier on this Mac.
    uti: String,
    /// Still to come from the client: listed with no bytes inline.
    coming: bool,
}

/// One item of the drop, and what the release waits on for it.
#[derive(Debug)]
struct Item {
    /// The name its file goes up and lands under, once known: at the entry for a named file,
    /// at the drop for a promised one.
    name: Option<String>,
    /// It names or promises a file.
    is_file: bool,
    /// A promise the drop has not said the fate of.
    promise_open: bool,
    /// Where its file landed whole.
    landed: Option<PathBuf>,
    /// Its data.
    reps: Vec<Rep>,
}

impl Item {
    /// Whether everything it carries is here, or will not come.
    fn whole(&self) -> bool {
        let file = !self.is_file || self.landed.is_some() || self.name.is_none();
        file && !self.promise_open && self.reps.iter().all(|r| !r.coming)
    }
}

/// One drop from the client, from its entry to its end. See the module docs.
#[derive(Debug)]
pub struct DropIn {
    drag: DragId,
    allowed: DragOps,
    items: Vec<Item>,
    /// Where the drag entered, where the source is pressed.
    entered: (f32, f32),
    /// Where the client's drag is now.
    at: (f32, f32),
    /// Where the session was last carried to.
    carried: (f32, f32),
    /// The drop's point, once the client dropped.
    dropped: Option<(f32, f32)>,
    /// The operation last told.
    op: DragOp,
    phase: Phase,
    /// What the helper's source declares, until it is asked for.
    source: Option<Vec<SourceItem>>,
    /// Files that landed under a name no item has yet: a promised file, whose name comes with
    /// the drop, can land before the drop does.
    stray: Vec<(String, PathBuf)>,
}

impl DropIn {
    /// A drag entered at `(x, y)` with `items`, which `allowed` lets a target copy, link or
    /// move; files land under `dir` (`Transfers::drag_dir`). The first acts come back with it.
    #[must_use]
    pub fn enter(
        drag: DragId,
        (x, y): (f32, f32),
        allowed: DragOps,
        items: &[DragItem],
        dir: &Path,
    ) -> (Self, Vec<Act>) {
        let this = Self {
            drag,
            allowed,
            items: items.iter().map(Self::item).collect(),
            entered: (x, y),
            at: (x, y),
            carried: (x, y),
            dropped: None,
            op: DragOp::None,
            phase: Phase::Mapping,
            source: Some(source_items(items, dir)),
            stray: Vec::new(),
        };
        (this, vec![Act::Enter { x, y }, Act::Deadline(Some(START_WAIT))])
    }

    fn item(item: &DragItem) -> Item {
        let reps = item
            .reps
            .iter()
            .filter_map(|r| {
                let uti = slopty_input::pasteboard::type_on_board(&r.kind)?;
                Some(Rep { kind: r.kind.clone(), uti, coming: r.inline.is_none() })
            })
            .collect();
        Item {
            name: item.file.as_ref().map(|f| f.name.clone()),
            is_file: item.file.is_some() || item.promised.is_some(),
            promise_open: item.file.is_none() && item.promised.is_some(),
            landed: None,
            reps,
        }
    }

    /// The drag.
    #[must_use]
    pub const fn drag(&self) -> DragId {
        self.drag
    }

    /// Whether it is over.
    #[must_use]
    pub fn over(&self) -> bool {
        self.phase == Phase::Over
    }

    /// Hear `heard`; what to do comes back, in order.
    pub fn hear(&mut self, heard: Heard) -> Vec<Act> {
        if self.over() {
            return Vec::new();
        }
        match heard {
            Heard::Input(input) => self.input(input),
            Heard::Mapped(Ok((x, y))) if self.phase == Phase::Mapping => {
                self.phase = Phase::Placing;
                let items = self.source.take().unwrap_or_default();
                vec![
                    Act::Helper(ToHelper::SourceAt { drag: self.drag, x, y, items }),
                    Act::Deadline(Some(START_WAIT)),
                ]
            }
            Heard::Mapped(Err(e)) if self.phase == Phase::Mapping => {
                tracing::debug!(drag = %self.drag, error = %e, "no point for the drag");
                self.end(DragOp::None, Some(&e.to_string()), false)
            }
            Heard::Mapped(_) => Vec::new(),
            Heard::Helper(said) => self.helper(&said),
            Heard::Landed { name, path } => self.landed(&name, path),
            Heard::Data { item, kind, bytes } => self.data(item, &kind, bytes),
            Heard::Failed(error) => self.end(DragOp::None, Some(&error), true),
            Heard::HelperGone => self.end(DragOp::None, Some(HELPER_GONE), true),
            Heard::Late => match self.phase {
                Phase::Released => self.end(DragOp::None, Some(UNFINISHED), false),
                Phase::Mapping | Phase::Placing | Phase::Pressed => {
                    self.end(DragOp::None, Some(NOT_STARTED), true)
                }
                Phase::Live | Phase::Over => Vec::new(),
            },
        }
    }

    fn input(&mut self, input: DragInput) -> Vec<Act> {
        match input {
            DragInput::Move { drag, x, y } if drag == self.drag => {
                self.at = (x, y);
                self.carry()
            }
            DragInput::Drop { drag, x, y, promised } if drag == self.drag => {
                self.at = (x, y);
                self.dropped = Some((x, y));
                for promise in promised {
                    let Some(item) = self.items.get_mut(usize::from(promise.item)) else {
                        continue;
                    };
                    if item.promise_open {
                        item.promise_open = false;
                        item.name = promise.file.map(|f| f.name);
                    }
                }
                // A promise the drop said nothing of is not coming either.
                for item in &mut self.items {
                    item.promise_open = false;
                }
                let drag = self.drag;
                let mut acts: Vec<Act> = (0_u16..)
                    .zip(&self.items)
                    .filter(|(_, item)| item.is_file && item.name.is_none())
                    .map(|(n, _)| gone_file(drag, n))
                    .collect();
                for (name, path) in std::mem::take(&mut self.stray) {
                    acts.extend(self.landed(&name, path));
                }
                acts.extend(self.carry());
                acts.extend(self.release());
                acts
            }
            DragInput::Leave { drag } if drag == self.drag => {
                let pressed = matches!(self.phase, Phase::Pressed | Phase::Live);
                self.phase = Phase::Over;
                let mut acts = vec![if pressed { Act::Cancel } else { Act::Release }];
                acts.push(Act::Helper(ToHelper::Stop { drag: self.drag }));
                acts.extend([Act::Deadline(None), Act::Over]);
                acts
            }
            DragInput::Enter { .. }
            | DragInput::Move { .. }
            | DragInput::Drop { .. }
            | DragInput::Leave { .. }
            | DragInput::Catch { .. } => Vec::new(),
        }
    }

    fn helper(&mut self, said: &FromHelper) -> Vec<Act> {
        if said.drag() != self.drag {
            return Vec::new();
        }
        match said {
            FromHelper::Ready { .. } if self.phase == Phase::Placing => {
                self.phase = Phase::Pressed;
                let (x, y) = self.entered;
                self.carried = (x, y);
                vec![Act::Press { x, y }, Act::Deadline(Some(START_WAIT))]
            }
            FromHelper::Began { .. } if self.phase == Phase::Pressed => {
                self.phase = Phase::Live;
                let mut acts = vec![Act::Deadline(None)];
                acts.extend(self.carry());
                acts.extend(self.release());
                acts
            }
            FromHelper::Operation { op, .. } => {
                let op = op.within(self.allowed);
                if op == self.op || !matches!(self.phase, Phase::Live) {
                    return Vec::new();
                }
                self.op = op;
                vec![Act::Tell(DragEvent::Operation { drag: self.drag, op })]
            }
            FromHelper::Ended { op, .. } => match self.phase {
                Phase::Released => self.end(op.within(self.allowed), None, false),
                _ => self.end(DragOp::None, Some(ENDED_THERE), true),
            },
            FromHelper::Ready { .. }
            | FromHelper::Began { .. }
            | FromHelper::Asked { .. }
            | FromHelper::Caught { .. }
            | FromHelper::Promised { .. } => Vec::new(),
        }
    }

    fn landed(&mut self, name: &str, path: PathBuf) -> Vec<Act> {
        let found = (0_u16..)
            .zip(&mut self.items)
            .find(|(_, i)| i.landed.is_none() && i.name.as_deref() == Some(name));
        let Some((n, item)) = found else {
            if self.dropped.is_none() {
                self.stray.push((name.to_owned(), path));
            }
            return Vec::new();
        };
        let bytes = path.to_string_lossy().as_bytes().to_vec();
        item.landed = Some(path);
        let told = Act::Helper(ToHelper::Data {
            drag: self.drag,
            item: n,
            uti: FILE_URL.to_owned(),
            bytes: Some(bytes),
        });
        let mut acts = vec![told];
        acts.extend(self.release());
        acts
    }

    fn data(&mut self, item: u16, kind: &ClipType, bytes: Option<Vec<u8>>) -> Vec<Act> {
        let Some(rep) = self
            .items
            .get_mut(usize::from(item))
            .and_then(|i| i.reps.iter_mut().find(|r| r.coming && r.kind == *kind))
        else {
            return Vec::new();
        };
        rep.coming = false;
        let told =
            Act::Helper(ToHelper::Data { drag: self.drag, item, uti: rep.uti.clone(), bytes });
        let mut acts = vec![told];
        acts.extend(self.release());
        acts
    }

    /// Carry the live session to where the client's drag is, when it is not there.
    fn carry(&mut self) -> Vec<Act> {
        if self.phase != Phase::Live || self.at == self.carried {
            return Vec::new();
        }
        self.carried = self.at;
        let (x, y) = self.at;
        vec![Act::Move { x, y }]
    }

    /// Let go, once the client dropped, the session is live, and everything is here.
    fn release(&mut self) -> Vec<Act> {
        let ready = self.phase == Phase::Live
            && self.dropped.is_some()
            && self.items.iter().all(Item::whole);
        if !ready {
            return Vec::new();
        }
        self.phase = Phase::Released;
        let mut acts = Vec::new();
        if let Some(at) = self.dropped
            && at != self.carried
        {
            self.carried = at;
            acts.push(Act::Move { x: at.0, y: at.1 });
        }
        acts.extend([Act::Release, Act::Deadline(Some(LAND_WAIT))]);
        acts
    }

    /// The drop is over: what the target did, or why nothing landed. `held` says the button is
    /// still down on the worker, which a cancel lets go of with nothing dropped.
    fn end(&mut self, op: DragOp, error: Option<&str>, held: bool) -> Vec<Act> {
        let pressed = held && matches!(self.phase, Phase::Pressed | Phase::Live);
        let entered = held && self.phase != Phase::Released;
        self.phase = Phase::Over;
        let mut acts = Vec::new();
        if pressed {
            acts.push(Act::Cancel);
        } else if entered {
            acts.push(Act::Release);
        }
        acts.push(Act::Helper(ToHelper::Stop { drag: self.drag }));
        let error = error.map(str::to_owned);
        acts.push(Act::Tell(DragEvent::Ended { drag: self.drag, op, error }));
        acts.extend([Act::Deadline(None), Act::Over]);
        acts
    }
}

/// The helper is told item `n` of `drag` gives no file.
fn gone_file(drag: DragId, n: u16) -> Act {
    Act::Helper(ToHelper::Data { drag, item: n, uti: FILE_URL.to_owned(), bytes: None })
}

/// The type a file's URL is on a Mac's pasteboard.
pub const FILE_URL: &str = "public.file-url";

/// What the helper's source declares for `items`: the inline data given at once, and each named
/// file's URL where it will land, under `dir`.
#[must_use]
pub fn source_items(items: &[DragItem], dir: &Path) -> Vec<SourceItem> {
    items
        .iter()
        .map(|item| SourceItem {
            file: item.file.as_ref().map(|f| dir.join(&f.name).to_string_lossy().into_owned()),
            is_file: item.file.is_some() || item.promised.is_some(),
            types: item
                .reps
                .iter()
                .filter_map(|r| slopty_input::pasteboard::type_on_board(&r.kind))
                .collect(),
            given: item
                .reps
                .iter()
                .filter_map(|r| {
                    let uti = slopty_input::pasteboard::type_on_board(&r.kind)?;
                    Some(Given { uti, bytes: r.inline.clone()? })
                })
                .collect(),
        })
        .collect()
}

/// The drag a worker carries, one at a time, and what is heard for it.
#[derive(Debug, Default)]
pub struct Drags {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    live: Option<Live>,
    /// What was heard for a drag before its stream claimed it: a file can land before the
    /// stream's task has taken the entry the control stream carried ahead of it.
    early: Vec<(DragId, Heard)>,
    /// What the last catches of drags out took past their events' budgets, newest last, for
    /// the client to fetch under the drag (`out::OutAct::Keep`).
    kept: std::collections::VecDeque<(DragId, out::Kept)>,
}

/// The most early news kept: a drag's files and representations.
const EARLY: usize = 256;

/// How many catches' data is kept: the client fetches it as its own drop lands, so only the
/// latest drags out are still being dropped.
const KEPT: usize = 2;

#[derive(Debug)]
struct Live {
    drag: DragId,
    client: ClientId,
    news: mpsc::UnboundedSender<Heard>,
}

/// A claim on the worker's one drag: what is heard for it, until it is dropped.
#[derive(Debug)]
pub struct Claim {
    drags: Arc<Drags>,
    drag: DragId,
    news: mpsc::UnboundedReceiver<Heard>,
}

impl Claim {
    /// What is heard for the drag.
    pub const fn news(&mut self) -> &mut mpsc::UnboundedReceiver<Heard> {
        &mut self.news
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut inner = self.drags.inner.lock();
        if inner.live.as_ref().is_some_and(|l| l.drag == self.drag) {
            inner.live = None;
        }
    }
}

impl Drags {
    /// Carry `drag` for `client`, unless another drag is on: `None` then.
    pub fn claim(self: &Arc<Self>, client: ClientId, drag: DragId) -> Option<Claim> {
        let mut inner = self.inner.lock();
        if inner.live.as_ref().is_some_and(|l| !l.news.is_closed()) {
            return None;
        }
        let (tx, news) = mpsc::unbounded_channel();
        let early = std::mem::take(&mut inner.early);
        for (of, heard) in early {
            if of == drag {
                let _gone = tx.send(heard);
            } else {
                inner.early.push((of, heard));
            }
        }
        inner.live = Some(Live { drag, client, news: tx });
        drop(inner);
        Some(Claim { drags: Arc::clone(self), drag, news })
    }

    /// The drag being carried, and for whom.
    #[must_use]
    pub fn live(&self) -> Option<(DragId, ClientId)> {
        self.inner.lock().live.as_ref().map(|l| (l.drag, l.client))
    }

    /// `heard` for `drag`: to it when it is the one being carried, kept a while for it when it
    /// is not yet.
    pub fn tell(&self, drag: DragId, heard: Heard) {
        let mut inner = self.inner.lock();
        match &inner.live {
            Some(live) if live.drag == drag => {
                let _gone = live.news.send(heard);
            }
            _ => {
                if inner.early.len() >= EARLY {
                    inner.early.remove(0);
                }
                inner.early.push((drag, heard));
            }
        }
    }

    /// The helper went away: the drag being carried hears it.
    pub fn helper_gone(&self) {
        if let Some(live) = &self.inner.lock().live {
            let _gone = live.news.send(Heard::HelperGone);
        }
    }

    /// Keep what the catch of `drag` took past its event's budget, for the client to fetch.
    pub fn keep(&self, drag: DragId, data: out::Kept) {
        let mut inner = self.inner.lock();
        inner.kept.retain(|(of, _)| *of != drag);
        inner.kept.push_back((drag, data));
        while inner.kept.len() > KEPT {
            inner.kept.pop_front();
        }
        drop(inner);
    }

    /// Representation `kind` of item `item` that the catch of `drag` kept.
    #[must_use]
    pub fn kept(&self, drag: DragId, item: u16, kind: &ClipType) -> Option<Bytes> {
        let inner = self.inner.lock();
        let (_, data) = inner.kept.iter().find(|(of, _)| *of == drag)?;
        let found = data.iter().find(|(n, k, _)| *n == item && k == kind);
        let bytes = found.map(|(_, _, bytes)| bytes.clone());
        drop(inner);
        bytes
    }

    /// Forget what was kept for `drag`: its stream will never claim it.
    pub fn forget(&self, drag: DragId) {
        self.inner.lock().early.retain(|(of, _)| *of != drag);
    }
}

pub mod out;

#[cfg(test)]
mod tests;
