//! What a companion does in each state, and the frame that draws it: a body from
//! [`sprites`], set standing or sitting, its eyes open, shut or turned, and a prop laid over it.
//! A frame is built on the stack each time it is painted, so a step allocates nothing.

use slopty_proto::thread::attention::Rung;
use slopty_proto::thread::{Item, ItemBody, Liveness, ToolState, TurnId, kind};

use super::Kind;
use super::sprites::{self, Grid, Ink, SIDE};
use crate::icons::Status;

/// What a working agent is doing, from its newest running call's kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Task {
    /// No call runs: it looks up, a thought rising.
    Thinking,
    /// Editing or writing, or a kind it does not know: at a keyboard.
    Typing,
    /// Reading, searching or fetching: a page at its side.
    Reading,
    /// Running a command: a little screen beside it.
    Running,
    /// Starting a subagent: a little one hops off.
    Delegating,
    /// Keeping a plan or a task list: ticking a list.
    Listing,
}

impl Task {
    /// Every task, for the sprite checks.
    pub const ALL: [Self; 6] = [
        Self::Thinking,
        Self::Typing,
        Self::Reading,
        Self::Running,
        Self::Delegating,
        Self::Listing,
    ];

    /// What a running call of `kind` (a [`slopty_proto::thread::ToolCall::kind`]) looks like.
    /// The kinds are open: one this does not know types.
    #[must_use]
    pub fn of(call: &str) -> Self {
        match call {
            kind::READ | kind::SEARCH | kind::FETCH | kind::WEB_SEARCH => Self::Reading,
            kind::EXEC => Self::Running,
            kind::AGENT => Self::Delegating,
            kind::PLAN | kind::TASKS => Self::Listing,
            _ => Self::Typing,
        }
    }

    /// Its prop's frames.
    const fn prop(self) -> &'static [Grid; 4] {
        match self {
            Self::Thinking => &sprites::THOUGHT,
            Self::Typing => &sprites::KEYBOARD,
            Self::Reading => &sprites::PAGE,
            Self::Running => &sprites::TERMINAL,
            Self::Delegating => &sprites::MINI,
            Self::Listing => &sprites::LIST,
        }
    }
}

/// A companion's pose: what its state looks like, on the attention ladder's words.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Pose {
    /// At rest: standing, eyes open.
    Idle,
    /// Working on a turn, at its task. The only pose that moves on its own.
    Working(Task),
    /// Waiting on its own work: sitting, looking up at a little clock.
    Waiting,
    /// Waiting on the person: facing out, one arm up, the hand in amber.
    NeedsYou,
    /// Changes the person has not kept: holding a page up.
    ToReview,
    /// A turn done and not yet seen: both arms up.
    Done,
    /// Stopped on an error: sitting, eyes shut, a plaster in red.
    Failed,
    /// Sleeping until a wakeup, or in the yard while the person is away: curled up, a "z"
    /// over it.
    Asleep,
    /// Running but quiet a while: a yawn.
    Silent,
    /// Its agent ended, or its machine is out of reach: its outline only.
    Gone,
}

impl Pose {
    /// Every pose, for the sprite checks.
    pub const ALL: [Self; 15] = [
        Self::Idle,
        Self::Working(Task::Thinking),
        Self::Working(Task::Typing),
        Self::Working(Task::Reading),
        Self::Working(Task::Running),
        Self::Working(Task::Delegating),
        Self::Working(Task::Listing),
        Self::Waiting,
        Self::NeedsYou,
        Self::ToReview,
        Self::Done,
        Self::Failed,
        Self::Asleep,
        Self::Silent,
        Self::Gone,
    ];

    /// The pose of a mark's [`Status`]; none is an agent at rest. A navigator row knows only
    /// its status, so a working one types rather than guess its task from words.
    #[must_use]
    pub const fn of_status(status: Option<Status>) -> Self {
        match status {
            None | Some(Status::Idle) => Self::Idle,
            Some(Status::Working) => Self::Working(Task::Typing),
            Some(Status::Running) => Self::Waiting,
            Some(Status::NeedsYou) => Self::NeedsYou,
            Some(Status::Done) => Self::Done,
            Some(Status::Failed) => Self::Failed,
            Some(Status::Away) => Self::Gone,
        }
    }

    /// The pose of a thread at `rung` whose agent is `liveness`, working at `task`.
    #[must_use]
    pub const fn of_thread(rung: Rung, liveness: Liveness, task: Task) -> Self {
        match (rung, liveness) {
            (Rung::NeedsYou, _) => Self::NeedsYou,
            (Rung::Failed, _) => Self::Failed,
            (Rung::ToReview, _) => Self::ToReview,
            (Rung::Working, _) => Self::Working(task),
            (Rung::Waiting | Rung::Idle, Liveness::Sleeping { .. }) => Self::Asleep,
            (Rung::Waiting, _) => Self::Waiting,
            (Rung::Idle, Liveness::Exited { .. }) => Self::Gone,
            (Rung::Idle, Liveness::Silent { .. }) => Self::Silent,
            (Rung::Idle, Liveness::Live) => Self::Idle,
        }
    }

    /// How many frames it steps through: a working one its task's four, a wave its two, a
    /// "z" its three (lively), the rest one.
    #[must_use]
    pub const fn frames(self) -> u32 {
        match self {
            Self::Working(_) => 4,
            Self::NeedsYou => 2,
            Self::Asleep => 3,
            _ => 1,
        }
    }

    /// Its place in a crowd, highest first: needs you, failed, to review, working, waiting,
    /// at rest, asleep, gone, as the attention ladder ranks them.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::NeedsYou => 7,
            Self::Failed => 6,
            Self::ToReview | Self::Done => 5,
            Self::Working(_) => 4,
            Self::Waiting => 3,
            Self::Idle | Self::Silent => 2,
            Self::Asleep => 1,
            Self::Gone => 0,
        }
    }
}

/// How a frame is turned from its pose's own: what the clock and the yard ask of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Beat {
    /// Which of the pose's frames.
    pub frame: u32,
    /// The eyes shut for a blink.
    pub blink: bool,
    /// Mid-stride: the feet drawn together.
    pub stride: bool,
}

/// The largest grid a frame is drawn on: the small one at twice the size.
pub(super) const LARGE: usize = SIDE * 2;

/// A frame being built: a grid of `side` cells a side, at most sixteen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Canvas {
    cells: [[Ink; LARGE]; LARGE],
    side: usize,
}

impl Canvas {
    /// The small grid `grid`.
    #[must_use]
    pub fn of(grid: &Grid) -> Self {
        let mut cells = [[Ink::Clear; LARGE]; LARGE];
        for (row, from) in cells.iter_mut().zip(grid) {
            for (cell, ink) in row.iter_mut().zip(from) {
                *cell = *ink;
            }
        }
        Self { cells, side: SIDE }
    }

    /// Its side, in cells.
    #[must_use]
    pub const fn side(&self) -> usize {
        self.side
    }

    /// The ink at column `x`, row `y`; clear off the grid.
    #[must_use]
    pub fn at(&self, x: usize, y: usize) -> Ink {
        if x >= self.side || y >= self.side {
            return Ink::Clear;
        }
        self.cells.get(y).and_then(|row| row.get(x)).copied().unwrap_or_default()
    }

    /// The ink beside `(x, y)` by `(dx, dy)`; clear off the grid.
    fn beside(&self, x: usize, y: usize, dx: isize, dy: isize) -> Ink {
        match (x.checked_add_signed(dx), y.checked_add_signed(dy)) {
            (Some(x), Some(y)) => self.at(x, y),
            _ => Ink::Clear,
        }
    }

    fn set(&mut self, x: usize, y: usize, ink: Ink) {
        if let Some(cell) = self.cells.get_mut(y).and_then(|row| row.get_mut(x)) {
            *cell = ink;
        }
    }

    /// Every cell, as `(x, y, ink)`, row by row.
    pub fn cells(&self) -> impl Iterator<Item = (usize, usize, Ink)> + '_ {
        (0..self.side).flat_map(move |y| (0..self.side).map(move |x| (x, y, self.at(x, y))))
    }

    /// `overlay` laid over it: what it draws replaces what is under it, and `_` clears.
    fn lay(&mut self, overlay: &Grid) {
        for (y, row) in overlay.iter().enumerate() {
            for (x, ink) in row.iter().enumerate() {
                match ink {
                    Ink::Clear => {}
                    Ink::Erase => self.set(x, y, Ink::Clear),
                    _ => self.set(x, y, *ink),
                }
            }
        }
    }

    /// Sat down: every row a row lower, the feet tucked under.
    fn sit(&mut self) {
        for y in (0..self.side).rev() {
            for x in 0..self.side {
                let above = y.checked_sub(1).map_or(Ink::Clear, |up| self.at(x, up));
                self.set(x, y, above);
            }
        }
    }

    /// The eyes shut: each a line.
    fn shut_eyes(&mut self) {
        self.replace(Ink::Eye, Ink::Outline);
    }

    fn replace(&mut self, from: Ink, to: Ink) {
        for row in &mut self.cells {
            for cell in row.iter_mut().filter(|c| **c == from) {
                *cell = to;
            }
        }
    }

    /// The eyes turned by `(dx, dy)`, each only onto its own body.
    fn look(&mut self, dx: isize, dy: isize) {
        let mut moved = [[false; LARGE]; LARGE];
        for y in 0..self.side {
            for x in 0..self.side {
                let seen = moved.get(y).and_then(|r| r.get(x)).copied().unwrap_or(true);
                if self.at(x, y) != Ink::Eye || seen {
                    continue;
                }
                let (Some(tx), Some(ty)) = (x.checked_add_signed(dx), y.checked_add_signed(dy))
                else {
                    continue;
                };
                if matches!(self.at(tx, ty), Ink::Body | Ink::Shade) {
                    self.set(x, y, Ink::Body);
                    self.set(tx, ty, Ink::Eye);
                    if let Some(mark) = moved.get_mut(ty).and_then(|r| r.get_mut(tx)) {
                        *mark = true;
                    }
                }
            }
        }
    }

    /// Mid-stride: each foot on the last row a cell nearer the middle.
    fn stride(&mut self) {
        let Some(last) = self.side.checked_sub(1) else { return };
        let half = self.side / 2;
        let feet: [Option<usize>; LARGE] = {
            let mut feet = [None; LARGE];
            for (slot, x) in feet.iter_mut().zip(0..self.side) {
                *slot = (self.at(x, last) == Ink::Outline).then_some(x);
            }
            feet
        };
        for x in feet.into_iter().flatten() {
            let to = if x < half { x.saturating_add(1) } else { x.saturating_sub(1) };
            if self.at(to, last) == Ink::Clear {
                self.set(x, last, Ink::Clear);
                self.set(to, last, Ink::Outline);
            }
        }
    }

    /// A plaster on its head: the right end of the body's top row, in the cue's tone.
    fn plaster(&mut self) {
        let top = (0..self.side).find(|&y| (0..self.side).any(|x| self.at(x, y) == Ink::Body));
        if let Some(y) = top
            && let Some(x) = (0..self.side).rev().find(|&x| self.at(x, y) == Ink::Body)
        {
            self.set(x, y, Ink::Cue);
        }
    }

    /// A yawn: a mouth under the eyes, between them.
    fn mouth(&mut self) {
        let mut span: Option<(usize, usize, usize)> = None;
        for (x, y, ink) in self.cells() {
            if ink == Ink::Eye {
                span = Some(span.map_or((x, x, y), |(l, r, b)| (l.min(x), r.max(x), b.max(y))));
            }
        }
        let Some((left, right, bottom)) = span else { return };
        let (row, mid) = (bottom.saturating_add(1), left.saturating_add(right));
        for x in [mid / 2, mid.saturating_add(1) / 2] {
            if self.at(x, row) == Ink::Body {
                self.set(x, row, Ink::Outline);
            }
        }
    }

    /// Only its outline, in `ink`: every drawn cell with nothing beside it on one side.
    fn outline(&mut self, ink: Ink) {
        let before = *self;
        for y in 0..self.side {
            for x in 0..self.side {
                if before.at(x, y) == Ink::Clear {
                    continue;
                }
                let edge = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| before.beside(x, y, *dx, *dy) == Ink::Clear);
                self.set(x, y, if edge { ink } else { Ink::Clear });
            }
        }
    }

    /// At twice the size: each cell four. `smooth`, the body's stairs are rounded (`Scale2x`): a
    /// corner takes its neighbours' ink where two that meet there agree and the others do not,
    /// only between the body, its shade and nothing, so a sparkle, a prop or a cue stays crisp.
    #[must_use]
    pub fn doubled(&self, smooth: bool) -> Self {
        let mut out =
            Self { cells: [[Ink::Clear; LARGE]; LARGE], side: self.side.saturating_mul(2) };
        for y in 0..self.side {
            for x in 0..self.side {
                let p = self.at(x, y);
                let up = self.beside(x, y, 0, -1);
                let right = self.beside(x, y, 1, 0);
                let left = self.beside(x, y, -1, 0);
                let down = self.beside(x, y, 0, 1);
                let soft = |ink: Ink| matches!(ink, Ink::Clear | Ink::Body | Ink::Shade);
                let corner = |a: Ink, b: Ink, c: Ink, d: Ink| {
                    if smooth && soft(p) && soft(a) && a == b && a != c && b != d { a } else { p }
                };
                let (x2, y2) = (x.saturating_mul(2), y.saturating_mul(2));
                out.set(x2, y2, corner(left, up, down, right));
                out.set(x2.saturating_add(1), y2, corner(up, right, left, down));
                out.set(x2, y2.saturating_add(1), corner(down, left, right, up));
                out.set(x2.saturating_add(1), y2.saturating_add(1), corner(right, down, up, left));
            }
        }
        out.glint();
        out
    }

    /// A glint in each eye: its top left cell, on a grid large enough for eyes of four.
    fn glint(&mut self) {
        let before = *self;
        for (x, y, ink) in before.cells() {
            let corner =
                before.beside(x, y, -1, 0) != Ink::Eye && before.beside(x, y, 0, -1) != Ink::Eye;
            let eye =
                before.beside(x, y, 1, 0) == Ink::Eye && before.beside(x, y, 0, 1) == Ink::Eye;
            if ink == Ink::Eye && corner && eye {
                self.set(x, y, Ink::Glint);
            }
        }
    }
}

/// The small frame `kind` shows in `pose`, turned by `beat`.
#[must_use]
pub fn frame(kind: Kind, pose: Pose, beat: Beat) -> Canvas {
    let mut canvas = Canvas::of(kind.body());
    if let Kind::Blob(accessory) = kind {
        canvas.lay(Kind::accessory(accessory));
    }
    let f = usize::try_from(beat.frame).unwrap_or_default();
    let nth = |frames: &'static [Grid]| frames.get(f.checked_rem(frames.len()).unwrap_or(0));
    match pose {
        Pose::Idle => {}
        Pose::Working(task) => {
            if matches!(task, Task::Typing | Task::Reading | Task::Running | Task::Listing) {
                canvas.sit();
            }
            match task {
                Task::Thinking => canvas.look(0, -1),
                Task::Reading | Task::Running | Task::Listing => canvas.look(1, 0),
                Task::Typing | Task::Delegating => {}
            }
            if let Some(prop) = nth(task.prop()) {
                canvas.lay(prop);
            }
        }
        Pose::Waiting => {
            canvas.sit();
            canvas.look(0, -1);
            canvas.lay(&sprites::CLOCK);
        }
        Pose::NeedsYou => {
            if let Some(wave) = nth(&sprites::WAVE) {
                canvas.lay(wave);
            }
        }
        Pose::ToReview => canvas.lay(&sprites::HELD_PAGE),
        Pose::Done => canvas.lay(&sprites::CHEER),
        Pose::Failed => {
            canvas.sit();
            canvas.shut_eyes();
            canvas.plaster();
        }
        Pose::Asleep => {
            canvas.sit();
            canvas.shut_eyes();
            if let Some(z) = nth(&sprites::SNORE) {
                canvas.lay(z);
            }
        }
        Pose::Silent => {
            canvas.mouth();
            canvas.shut_eyes();
        }
        Pose::Gone => canvas.outline(Ink::Muted),
    }
    if beat.blink {
        canvas.shut_eyes();
    }
    if beat.stride && !matches!(pose, Pose::Gone) {
        canvas.stride();
    }
    canvas
}

/// The runs of one ink along a row a frame paints as one quad each: `(x, y, width, ink)`.
pub fn runs(canvas: &Canvas) -> impl Iterator<Item = (usize, usize, usize, Ink)> + '_ {
    (0..canvas.side()).flat_map(move |y| {
        let mut x = 0;
        std::iter::from_fn(move || {
            while x < canvas.side() && canvas.at(x, y) == Ink::Clear {
                x = x.saturating_add(1);
            }
            if x >= canvas.side() {
                return None;
            }
            let (start, ink) = (x, canvas.at(x, y));
            while x < canvas.side() && canvas.at(x, y) == ink {
                x = x.saturating_add(1);
            }
            Some((start, y, x.saturating_sub(start), ink))
        })
    })
}

impl Task {
    /// What an agent working on turn `turn` of `items` is at: its newest call that has not
    /// ended, else thinking.
    #[must_use]
    pub fn at(items: &[Item], turn: TurnId) -> Self {
        items
            .iter()
            .rev()
            .take_while(|item| item.turn == turn)
            .find_map(|item| match &item.body {
                ItemBody::Tool(call)
                    if matches!(
                        call.state,
                        ToolState::Streaming | ToolState::Running | ToolState::Pending { .. }
                    ) =>
                {
                    Some(Self::of(&call.kind))
                }
                _ => None,
            })
            .unwrap_or(Self::Thinking)
    }
}

/// One quad a frame paints: a rectangle of one ink, in cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    /// Its left column.
    pub x: u8,
    /// Its top row.
    pub y: u8,
    /// Its width.
    pub width: u8,
    /// Its height.
    pub height: u8,
    /// What it is drawn in.
    pub ink: Ink,
}

/// The most rectangles a frame can be: a cell each.
const MOST: usize = LARGE * LARGE;

/// A frame's rectangles, kept on the stack.
#[derive(Clone, Copy, Debug)]
pub struct Rects {
    items: [Rect; MOST],
    len: usize,
}

impl Rects {
    /// Each rectangle.
    pub fn iter(&self) -> impl Iterator<Item = &Rect> {
        self.items.iter().take(self.len)
    }
}

/// The quads `canvas` paints: each row's runs of one ink, a run stacked under one of the same
/// place, width and ink in the row above joined to it.
#[must_use]
pub fn rects(canvas: &Canvas) -> Rects {
    let mut out = Rects { items: [Rect::default(); MOST], len: 0 };
    for (x, y, width, ink) in runs(canvas) {
        let (Ok(x), Ok(y), Ok(width)) = (u8::try_from(x), u8::try_from(y), u8::try_from(width))
        else {
            continue;
        };
        let above = out.items.iter_mut().take(out.len).find(|r| {
            r.x == x && r.width == width && r.ink == ink && r.y.saturating_add(r.height) == y
        });
        if let Some(rect) = above {
            rect.height = rect.height.saturating_add(1);
        } else if let Some(slot) = out.items.get_mut(out.len) {
            *slot = Rect { x, y, width, height: 1, ink };
            out.len = out.len.saturating_add(1);
        }
    }
    out
}
