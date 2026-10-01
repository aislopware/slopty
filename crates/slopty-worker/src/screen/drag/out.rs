//! A drag out of an app on the worker, caught when the client's pointer takes it out of the tile
//! (`docs/decisions/audio.md`, "Drag out").
//!
//! The worker holds the client's left button down, so it knows when a press may turn into a
//! drag: the drag pasteboard's change count is marked at the press, and read from the first move
//! with the button held until the release ([`Presses`]). A change means an app began a drag, and
//! what its items hold goes to the client ([`DragEvent::OutBegan`]), which draws them at its
//! pointer while it stays on the tile. The moves keep going to the app as ever, so its own hover
//! feedback shows in the picture, and a release on the tile drops on the worker.
//!
//! When the client's pointer leaves the tile with the button still held, the client drags on
//! from there and asks the worker to catch ([`slopty_proto::drag::DragInput::Catch`]): the drag
//! helper puts its catcher under the real pointer ([`ToHelper::CatcherAt`]), the drag is carried a
//! few pixels out and back over it until the catcher says it is over it, and the release lands
//! there. The app sees an ordinary copy. The catcher keeps the files as references, calls each
//! promised file into the drag's own folder, and keeps the data whole, and the client hears what it
//! caught ([`DragEvent::OutCaught`]): files by their path here, data inline while the event's
//! budget lasts and fetched under the drag ([`slopty_proto::transfer::Source::Drag`]) past it.
//!
//! [`DragOut`] decides all of it from what it hears ([`OutHeard`]) and says what to do
//! ([`OutAct`]), as [`super::DropIn`] does for a drop in.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use slopty_core::WallMs;
use slopty_input::InputError;
use slopty_proto::dnd::{CaughtData, FromHelper, ToHelper};
use slopty_proto::drag::{DragEvent, DragId, DragItem, FileMeta};
use slopty_proto::input::{Mods, MouseButton};
use slopty_proto::screen::ScreenInput;
use slopty_proto::transfer::{ClipType, INLINE_CLIP_BYTES, MODE_BITS, Rep};

use super::START_WAIT;

/// What a catch keeps past its event's budget: `(item, type, bytes)`.
pub type Kept = Vec<(u16, ClipType, Bytes)>;

/// How long the carried drag has to reach the catcher before it is carried over it again.
pub const REACH_WAIT: Duration = Duration::from_millis(250);

/// How many times the drag is carried over the catcher before the catch gives up.
pub const REACH_TRIES: u8 = 4;

/// How long a released catch has to say what it took, and then each promised file to be called
/// in: an app writes a promised file inside the drop, for as long as it takes.
pub const CATCH_WAIT: Duration = Duration::from_secs(30);

/// How far the drag is carried out and back over the catcher, in stream pixels: within its
/// window, and a move the drag manager takes as one.
const WIGGLE: f32 = 4.0;

/// The catcher never showed under the pointer.
const NO_CATCHER: &str = "the worker could not catch the drag";
/// The drag never reached the catcher.
const NOT_REACHED: &str = "the drag did not reach the worker's catcher";
/// The catch never said what it took.
const NOT_CAUGHT: &str = "the worker's catch did not finish";
/// The helper went away mid-catch.
const HELPER_GONE: &str = "the worker's drag helper stopped";

/// What the drag pasteboard's watch is asked as the client's left button goes ([`Presses`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Watching {
    /// The button went down: note the change count now.
    Mark,
    /// The button moved held: read the count every few milliseconds from now, and say what a
    /// drag that began carries, once.
    Arm,
    /// The button went up: stop reading.
    Disarm,
}

/// The client's left button on one stream, as the drag pasteboard's watch follows it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Presses {
    held: bool,
    armed: bool,
}

impl Presses {
    /// What the watch is asked for `input`, before it goes to the app. A press marks the count,
    /// the first move held arms the watch, and the release disarms it; nothing else asks
    /// anything.
    pub const fn input(&mut self, input: &ScreenInput) -> Option<Watching> {
        match input {
            ScreenInput::Button { button: MouseButton::Left, down: true, .. } => {
                self.held = true;
                self.armed = false;
                Some(Watching::Mark)
            }
            ScreenInput::Button { button: MouseButton::Left, down: false, .. } => {
                let was = self.held;
                self.held = false;
                self.armed = false;
                if was { Some(Watching::Disarm) } else { None }
            }
            ScreenInput::Move { .. } if self.held && !self.armed => {
                self.armed = true;
                Some(Watching::Arm)
            }
            _ => None,
        }
    }

    /// Whether the client holds the left button on the stream.
    #[must_use]
    pub const fn held(self) -> bool {
        self.held
    }
}

/// What a drag out hears.
#[derive(Debug)]
pub enum OutHeard {
    /// The client's pointer moved on the tile, the button held: stream pixels.
    Moved {
        /// Stream pixels.
        x: f32,
        /// Stream pixels.
        y: f32,
    },
    /// The client let go on the tile: the drag drops on the worker.
    Released,
    /// The client's pointer left the tile with the button held ([`DragInput::Catch`]).
    ///
    /// [`DragInput::Catch`]: slopty_proto::drag::DragInput::Catch
    Catch,
    /// Where the input thread found the drag's point, in global points ([`OutAct::Locate`]).
    Located(Result<(f64, f64), InputError>),
    /// The drag helper.
    Helper(FromHelper),
    /// The helper went away.
    HelperGone,
    /// The deadline [`OutAct::Deadline`] armed passed.
    Late,
}

/// What a drag out asks of the stream, the helper and the client, in order.
#[derive(Debug, PartialEq)]
pub enum OutAct {
    /// Find the stream pixel `(x, y)` in global points: answered by [`OutHeard::Located`].
    Locate {
        /// Stream pixels.
        x: f32,
        /// Stream pixels.
        y: f32,
    },
    /// Post this as the client's own input would be: the button is the client's press.
    Input(ScreenInput),
    /// End the drag with nothing dropped, and let go of the button.
    Cancel,
    /// Tell the helper.
    Helper(ToHelper),
    /// Tell the client.
    Tell(DragEvent),
    /// Keep what the catch took past the event's budget, for the client to fetch under the
    /// drag: `(item, type, bytes)`.
    Keep(Kept),
    /// Hear [`OutHeard::Late`] after this long, in place of any deadline armed before; `None`
    /// disarms it.
    Deadline(Option<Duration>),
    /// The drag is over here.
    Over,
}

/// Where a drag out is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// On the tile, the button held.
    Out,
    /// Asked where the pointer is, to put the catcher there.
    Locating,
    /// Asked the helper for its catcher.
    Placing,
    /// Carried over the catcher, `tries` times so far, until it says the drag is over it.
    Reaching { tries: u8 },
    /// Let go over the catcher; waiting for what it took.
    Released,
    /// The catch is in; `left` promised files still being called in.
    Calling { left: u16 },
    /// Done.
    Over,
}

/// What the catch took, gathered until every promised file is in.
#[derive(Debug, Default)]
struct Taken {
    files: Vec<String>,
    data: Vec<CaughtData>,
    promised: Vec<String>,
}

/// One drag out of an app on the worker, from its start to its catch. See the module docs.
#[derive(Debug)]
pub struct DragOut {
    drag: DragId,
    /// Where the client's pointer last was on the tile, in stream pixels.
    at: (f32, f32),
    /// Where promised files are called into.
    dir: PathBuf,
    phase: Phase,
    taken: Taken,
}

impl DragOut {
    /// An app began a drag carrying `items` under the client's press, the pointer at `at`;
    /// promised files go into `dir` once caught. The first acts come back with it.
    #[must_use]
    pub fn began(
        drag: DragId,
        at: (f32, f32),
        items: Vec<DragItem>,
        dir: PathBuf,
    ) -> (Self, Vec<OutAct>) {
        let this = Self { drag, at, dir, phase: Phase::Out, taken: Taken::default() };
        (this, vec![OutAct::Tell(DragEvent::OutBegan { drag, items })])
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

    /// Whether the client's button is still down on the worker for it.
    const fn held(&self) -> bool {
        matches!(self.phase, Phase::Out | Phase::Locating | Phase::Placing | Phase::Reaching { .. })
    }

    /// Hear `heard`; what to do comes back, in order.
    pub fn hear(&mut self, heard: OutHeard) -> Vec<OutAct> {
        if self.over() {
            return Vec::new();
        }
        match heard {
            OutHeard::Moved { x, y } => {
                if self.phase == Phase::Out {
                    self.at = (x, y);
                }
                Vec::new()
            }
            OutHeard::Released if self.phase == Phase::Out => {
                self.phase = Phase::Over;
                vec![OutAct::Deadline(None), OutAct::Over]
            }
            OutHeard::Catch if self.phase == Phase::Out => {
                self.phase = Phase::Locating;
                let (x, y) = self.at;
                vec![OutAct::Locate { x, y }, OutAct::Deadline(Some(START_WAIT))]
            }
            OutHeard::Located(Ok((x, y))) if self.phase == Phase::Locating => {
                self.phase = Phase::Placing;
                let dir = self.dir.to_string_lossy().into_owned();
                vec![
                    OutAct::Helper(ToHelper::CatcherAt { drag: self.drag, x, y, dir }),
                    OutAct::Deadline(Some(START_WAIT)),
                ]
            }
            OutHeard::Located(Err(e)) if self.phase == Phase::Locating => {
                tracing::debug!(drag = %self.drag, error = %e, "no point for the catch");
                self.fail(NO_CATCHER)
            }
            // Late, or once the drag is past the step it answers.
            OutHeard::Released | OutHeard::Catch | OutHeard::Located(_) => Vec::new(),
            OutHeard::Helper(said) => self.helper(said),
            OutHeard::HelperGone => self.fail(HELPER_GONE),
            OutHeard::Late => match self.phase {
                Phase::Locating | Phase::Placing => self.fail(NO_CATCHER),
                Phase::Reaching { tries } if tries < REACH_TRIES => self.reach(tries),
                Phase::Reaching { .. } => self.fail(NOT_REACHED),
                Phase::Released => self.fail(NOT_CAUGHT),
                // What came is what the client gets: a promise not kept by now is left out.
                Phase::Calling { .. } => self.finish(),
                Phase::Out | Phase::Over => Vec::new(),
            },
        }
    }

    fn helper(&mut self, said: FromHelper) -> Vec<OutAct> {
        if said.drag() != self.drag {
            return Vec::new();
        }
        match said {
            FromHelper::Ready { .. } if self.phase == Phase::Placing => self.reach(0),
            FromHelper::Operation { op, .. }
                if op.takes() && matches!(self.phase, Phase::Reaching { .. }) =>
            {
                self.phase = Phase::Released;
                let (x, y) = self.at;
                vec![OutAct::Input(release(x, y)), OutAct::Deadline(Some(CATCH_WAIT))]
            }
            FromHelper::Caught { files, data, promises, .. } if self.phase == Phase::Released => {
                self.taken.files = files;
                self.taken.data = data;
                if promises == 0 {
                    return self.finish();
                }
                self.phase = Phase::Calling { left: promises };
                vec![OutAct::Deadline(Some(CATCH_WAIT))]
            }
            FromHelper::Promised { path, error, .. } => {
                let Phase::Calling { left } = self.phase else { return Vec::new() };
                match (path, error) {
                    (Some(path), _) => self.taken.promised.push(path),
                    (None, error) => {
                        tracing::debug!(drag = %self.drag, ?error, "a promise was not kept");
                    }
                }
                let left = left.saturating_sub(1);
                if left == 0 {
                    return self.finish();
                }
                self.phase = Phase::Calling { left };
                Vec::new()
            }
            FromHelper::Ready { .. }
            | FromHelper::Operation { .. }
            | FromHelper::Caught { .. }
            | FromHelper::Began { .. }
            | FromHelper::Ended { .. }
            | FromHelper::Asked { .. } => Vec::new(),
        }
    }

    /// Carry the drag out and back over the catcher, so the drag manager takes it as the
    /// target, the `tries`-th time.
    fn reach(&mut self, tries: u8) -> Vec<OutAct> {
        self.phase = Phase::Reaching { tries: tries.saturating_add(1) };
        let (x, y) = self.at;
        vec![
            OutAct::Input(ScreenInput::Move { x: x + WIGGLE, y }),
            OutAct::Input(ScreenInput::Move { x, y }),
            OutAct::Deadline(Some(REACH_WAIT)),
        ]
    }

    /// The catch is in: what it took goes to the client, and the drag is over here.
    fn finish(&mut self) -> Vec<OutAct> {
        self.phase = Phase::Over;
        let taken = std::mem::take(&mut self.taken);
        let (items, kept) = caught_items(&taken);
        let mut acts = Vec::new();
        if !kept.is_empty() {
            acts.push(OutAct::Keep(kept));
        }
        acts.extend([
            OutAct::Tell(DragEvent::OutCaught { drag: self.drag, items }),
            OutAct::Helper(ToHelper::Stop { drag: self.drag }),
            OutAct::Deadline(None),
            OutAct::Over,
        ]);
        acts
    }

    /// The catch failed: the drag is let go of with nothing dropped, when the button is still
    /// down, and the client hears why.
    fn fail(&mut self, error: &str) -> Vec<OutAct> {
        let held = self.held();
        self.phase = Phase::Over;
        let mut acts = Vec::new();
        if held {
            acts.push(OutAct::Cancel);
        }
        acts.extend([
            OutAct::Helper(ToHelper::Stop { drag: self.drag }),
            OutAct::Tell(DragEvent::OutFailed { drag: self.drag, error: error.to_owned() }),
            OutAct::Deadline(None),
            OutAct::Over,
        ]);
        acts
    }
}

/// The client's left button let go at `(x, y)`.
const fn release(x: f32, y: f32) -> ScreenInput {
    ScreenInput::Button {
        button: MouseButton::Left,
        down: false,
        x,
        y,
        clicks: 1,
        mods: Mods::empty(),
    }
}

/// What a catch took, as the client hears it: each file (named or called in) an item of its own
/// with its path here, then each item's data, inline while [`INLINE_CLIP_BYTES`] lasts for the
/// whole event and kept for a fetch past it. A file whose path no longer reads, and data past
/// the catcher's cap, are left out.
fn caught_items(taken: &Taken) -> (Vec<DragItem>, Kept) {
    let mut items: Vec<DragItem> = taken
        .files
        .iter()
        .chain(&taken.promised)
        .filter_map(|path| {
            let file = file_meta(Path::new(path))?;
            Some(DragItem { file: Some(file), promised: None, reps: Vec::new() })
        })
        .collect();
    let mut kept = Vec::new();
    let mut inline_left = INLINE_CLIP_BYTES;
    let mut of: Option<(u16, usize)> = None;
    for data in &taken.data {
        let Some(bytes) = &data.bytes else { continue };
        let at = match of {
            Some((item, at)) if item == data.item => at,
            _ => {
                items.push(DragItem { file: None, promised: None, reps: Vec::new() });
                let at = items.len().saturating_sub(1);
                of = Some((data.item, at));
                at
            }
        };
        let kind = slopty_input::pasteboard::clip_type(&data.uti);
        let inline = (bytes.len() <= inline_left).then(|| {
            inline_left = inline_left.saturating_sub(bytes.len());
            bytes.clone()
        });
        if inline.is_none() {
            let item = u16::try_from(at).unwrap_or(u16::MAX);
            kept.push((item, kind.clone(), Bytes::copy_from_slice(bytes)));
        }
        let size = Some(data.size);
        if let Some(item) = items.get_mut(at) {
            item.reps.push(Rep { kind, size, hash: None, inline });
        }
    }
    (items, kept)
}

/// What a file here is, as a drag names it: its name, size, kind, mode and modification time,
/// and its path, for the client to fetch it by.
#[must_use]
pub fn file_meta(path: &Path) -> Option<FileMeta> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::metadata(path).ok()?;
    let name = path.file_name()?.to_str()?.to_owned();
    Some(FileMeta {
        name,
        size: if meta.is_dir() { 0 } else { meta.len() },
        folder: meta.is_dir(),
        mode: meta.permissions().mode() & MODE_BITS,
        mtime_ms: meta.modified().map_or(WallMs::ZERO, WallMs::of),
        path: Some(path.to_string_lossy().into_owned()),
    })
}

#[cfg(test)]
mod tests;
