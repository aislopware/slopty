//! `CanvasView`: the infinite plane.
//!
//! Items live in canvas units; the [`Camera`] maps them to the viewport. Terminals keep their
//! grid across zoom (the element scales its paint geometry), and below
//! [`slopty_client::canvas::CARD_ZOOM`] terminals collapse to summary cards; video keeps painting.
//!
//! Interaction (macOS): two-finger scroll pans, pinch or ⌘-scroll zooms about the pointer,
//! dragging a title bar moves, the corner grip resizes, dragging empty space pans. Every
//! geometry change is applied locally first and proposed to the host on release.

use std::collections::HashMap;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, App, AppContext as _, BorderStyle, Bounds, Context, Div, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    PinchEvent, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Size, Stateful,
    StatefulInteractiveElement as _, Styled as _, SystemNotification, Task, Window, canvas, div,
    fill, outline, point, px, size,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_client::arrange::{self, Arrangeable, Heading};
use slopty_client::canvas::{
    CARD_ZOOM, Camera, CanvasChange, CanvasDoc, FLIGHT, Flight, GAP, TERMINAL_SIZE, snap,
};
use slopty_core::{ClientId, ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentSource, AgentStatus, BlockReason};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, Rect};
use slopty_proto::file::FileRead;
use slopty_proto::handshake::ClientKind;
use slopty_proto::screen::{
    CaptureTarget, DisplayInfo, Quality, ScreenEvent, ScreenRequest, WindowInfo,
};
use slopty_proto::terminal::{
    OpenSession, SessionKind, SessionSummary, TermEvent, TermRequest, TermSize,
};
use slopty_theme::{Theme, alpha};
use tokio::sync::mpsc;

use crate::a11y::tab_stop;
use crate::chrome_text::ChromeText;
use crate::colors::{hsla, hsla_alpha};
use crate::file::{FileView, FileViewEvent, LineMove};
use crate::note::{NoteView, NoteViewEvent};
use crate::palette::{self, CommandPalette, PaletteEvent, PaletteItem, PaletteRun};
use crate::picker::{PickerEvent, SessionRow, WindowPicker};
use crate::screen::{ScreenFactory, ScreenView};
use crate::terminal::{TerminalView, TerminalViewEvent};

/// Canvas actions (bound in [`key_bindings`]).
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        canvas,
        [
            /// Open a new shell on the host.
            NewTerminal,
            /// Open a new Claude Code agent on the host.
            NewAgent,
            /// Put an empty note on the canvas.
            NewNote,
            /// Put a host window or display on the canvas.
            AddWindow,
            /// Close the active item (terminates its session).
            CloseItem,
            /// Put back the card closed last, while the offer stands.
            UndoClose,
            /// Zoom in about the viewport centre.
            ZoomIn,
            /// Zoom out about the viewport centre.
            ZoomOut,
            /// Zoom to 100%.
            ZoomReset,
            /// Fit every item in the viewport.
            FitAll,
            /// Fit the active item in the viewport.
            ZoomToItem,
            /// Tidy the canvas into one block of items per repository.
            ArrangeByRepo,
            /// Reveal the next terminal whose agent is waiting on the human.
            NextAttention,
            /// Silence or resume the active remote window's audio on this client.
            ToggleMute,
            /// Show or hide the stream stats overlay on every remote window.
            ToggleStats,
            /// Move the keyboard focus to the next control (title-bar pills, badges), from
            /// anywhere, a terminal included.
            FocusNext,
            /// Move the keyboard focus to the previous control.
            FocusPrev,
            /// Open the command palette: every action by name, run by ↩.
            OpenPalette,
            /// Name the active card: a field in its title bar, ↩ keeps the name (blank
            /// clears it), Esc leaves it as it was.
            RenameItem,
            /// Point the other clients at the active card: each of them is offered a jump
            /// to it.
            PointOthers,
            /// Find text in every card: the palette lists the cards it is in with their hit
            /// counts, and ↩ opens that card's find bar on it.
            FindEverywhere,
            /// Activate and reveal the next card in reading order (rows top to bottom, left
            /// to right), wrapping; nothing active starts at the first.
            NextCard,
            /// Activate and reveal the previous card in reading order, wrapping.
            PrevCard,
            /// Activate and reveal the nearest card to the left of the active one.
            CardLeft,
            /// Activate and reveal the nearest card to the right of the active one.
            CardRight,
            /// Activate and reveal the nearest card above the active one.
            CardUp,
            /// Activate and reveal the nearest card below the active one.
            CardDown,
            /// Move the active file card's reading line up one line.
            LineUp,
            /// Move the active file card's reading line down one line.
            LineDown,
            /// Move the active file card's reading line up one page.
            PageUp,
            /// Move the active file card's reading line down one page.
            PageDown,
            /// Move the active file card's reading line to the first line.
            LineFirst,
            /// Move the active file card's reading line to the last line.
            LineLast,
        ]
    );
}
pub use actions::{
    AddWindow, ArrangeByRepo, CardDown, CardLeft, CardRight, CardUp, CloseItem, FindEverywhere,
    FitAll, FocusNext, FocusPrev, LineDown, LineFirst, LineLast, LineUp, NewAgent, NewNote,
    NewTerminal, NextAttention, NextCard, OpenPalette, PageDown, PageUp, PointOthers, PrevCard,
    RenameItem, ToggleMute, ToggleStats, UndoClose, ZoomIn, ZoomOut, ZoomReset, ZoomToItem,
};

/// Where the phone key bar sends its keys (see [`CanvasView::active_key_target`]).
#[derive(Clone, Debug)]
pub enum KeyTarget {
    /// A terminal: keys go through the grid and the predictor.
    Terminal(Entity<TerminalView>),
    /// A remote window: keys are injected on the host.
    Screen(Entity<ScreenView>),
}

/// Key bindings for the canvas context.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    // A focused remote window gets every chord (its editor's ⌘W, ⌘T, ⌘0 are not ours to
    // take, as on Parsec); ⌃Tab alone stays, the keyboard's way back to the canvas.
    const CTX: Option<&str> = Some("Canvas && !Screen");
    const RING_CTX: Option<&str> = Some("Canvas");
    const FILE_CTX: Option<&str> = Some("Canvas && file_card");
    vec![
        KeyBinding::new("cmd-t", NewTerminal, CTX),
        KeyBinding::new("cmd-n", NewTerminal, CTX),
        KeyBinding::new("cmd-shift-t", NewAgent, CTX),
        KeyBinding::new("cmd-shift-n", NewNote, CTX),
        KeyBinding::new("cmd-o", AddWindow, CTX),
        KeyBinding::new("cmd-w", CloseItem, CTX),
        KeyBinding::new("cmd-z", UndoClose, CTX),
        KeyBinding::new("cmd-=", ZoomIn, CTX),
        KeyBinding::new("cmd-shift-=", ZoomIn, CTX),
        KeyBinding::new("cmd--", ZoomOut, CTX),
        KeyBinding::new("cmd-0", ZoomReset, CTX),
        KeyBinding::new("cmd-1", FitAll, CTX),
        KeyBinding::new("cmd-2", ZoomToItem, CTX),
        KeyBinding::new("cmd-shift-r", ArrangeByRepo, CTX),
        KeyBinding::new("cmd-shift-a", NextAttention, CTX),
        KeyBinding::new("cmd-shift-m", ToggleMute, CTX),
        KeyBinding::new("cmd-shift-i", ToggleStats, CTX),
        KeyBinding::new("cmd-f", crate::terminal::Find, CTX),
        // Tab is the shell's; ⌃Tab enters the control ring from a terminal, then Tab walks it.
        KeyBinding::new("ctrl-tab", FocusNext, RING_CTX),
        KeyBinding::new("ctrl-shift-tab", FocusPrev, RING_CTX),
        KeyBinding::new("cmd-shift-p", OpenPalette, CTX),
        KeyBinding::new("cmd-e", RenameItem, CTX),
        KeyBinding::new("cmd-shift-o", PointOthers, CTX),
        KeyBinding::new("cmd-shift-f", FindEverywhere, CTX),
        // A focused field (a find bar, the palette, a note) is a gpui-kit input, whose own
        // ⌘⇧F is replace; ours is bound in its context after it, so it wins there too.
        KeyBinding::new("cmd-shift-f", FindEverywhere, Some("Input")),
        KeyBinding::new("cmd-]", NextCard, CTX),
        KeyBinding::new("cmd-[", PrevCard, CTX),
        KeyBinding::new("cmd-alt-left", CardLeft, CTX),
        KeyBinding::new("cmd-alt-right", CardRight, CTX),
        KeyBinding::new("cmd-alt-up", CardUp, CTX),
        KeyBinding::new("cmd-alt-down", CardDown, CTX),
        // The active file card's reading line. Only while a file card is active (the canvas
        // sets `file_card` on its context then): a binding matches before a focused terminal's
        // key handler runs, so an unscoped `up` would take the arrows from the shell.
        KeyBinding::new("up", LineUp, FILE_CTX),
        KeyBinding::new("down", LineDown, FILE_CTX),
        KeyBinding::new("pageup", PageUp, FILE_CTX),
        KeyBinding::new("pagedown", PageDown, FILE_CTX),
        KeyBinding::new("home", LineFirst, FILE_CTX),
        KeyBinding::new("end", LineLast, FILE_CTX),
        KeyBinding::new("cmd-up", LineFirst, FILE_CTX),
        KeyBinding::new("cmd-down", LineLast, FILE_CTX),
        // A file card's find bar: the terminal's find keys, in the bar's own context (no
        // terminal around it).
        KeyBinding::new("escape", crate::terminal::CloseFind, Some("FileSearch")),
        KeyBinding::new("cmd-g", crate::terminal::FindNext, Some("FileSearch")),
        KeyBinding::new("cmd-shift-g", crate::terminal::FindPrev, Some("FileSearch")),
    ]
}

/// The palette's lines for the canvas's and the terminal's actions, with their keys.
#[must_use]
pub fn palette_items() -> Vec<PaletteItem> {
    use crate::terminal::{
        ClearScreen, CopyLastOutput, Find, NextPrompt, NoteLastBlock, PrevPrompt, RerunLast,
    };
    let canvas = key_bindings();
    let terminal = crate::terminal::key_bindings();
    let c = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &canvas);
    let t = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &terminal);
    vec![
        c("New terminal", Box::new(NewTerminal)),
        c("New agent", Box::new(NewAgent)),
        c("New note", Box::new(NewNote)),
        c("Add a window or display", Box::new(AddWindow)),
        c("Close item", Box::new(CloseItem)),
        c("Undo close", Box::new(UndoClose)),
        c("Zoom in", Box::new(ZoomIn)),
        c("Zoom out", Box::new(ZoomOut)),
        c("Zoom to 100%", Box::new(ZoomReset)),
        c("Fit all", Box::new(FitAll)),
        c("Zoom to item", Box::new(ZoomToItem)),
        c("Arrange by repository", Box::new(ArrangeByRepo)),
        c("Next attention", Box::new(NextAttention)),
        c("Mute or unmute window", Box::new(ToggleMute)),
        c("Stream stats", Box::new(ToggleStats)),
        c("Name this card", Box::new(RenameItem)),
        c("Point the others at this card", Box::new(PointOthers)),
        c("Find in every card", Box::new(FindEverywhere)),
        c("Next card", Box::new(NextCard)),
        c("Previous card", Box::new(PrevCard)),
        c("Card to the left", Box::new(CardLeft)),
        c("Card to the right", Box::new(CardRight)),
        c("Card above", Box::new(CardUp)),
        c("Card below", Box::new(CardDown)),
        t("Find in terminal or file", Box::new(Find)),
        t("Previous prompt", Box::new(PrevPrompt)),
        t("Next prompt", Box::new(NextPrompt)),
        t("Copy last output", Box::new(CopyLastOutput)),
        t("Rerun last command", Box::new(RerunLast)),
        t("Keep last block as a card", Box::new(NoteLastBlock)),
        t("Clear the screen and history", Box::new(ClearScreen)),
    ]
}

/// The program a "+ agent" terminal runs.
pub const AGENT_COMMAND: &str = "claude";

/// What a client is, for a human: `Mac`, `iPad`, `iPhone`, `tool`.
const fn device_name(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::Mac => "Mac",
        ClientKind::IPad => "iPad",
        ClientKind::IPhone => "iPhone",
        ClientKind::Tool => "tool",
    }
}

/// A stable element id for `(part, item)`.
fn element_id(part: &str, id: ItemId) -> ElementId {
    ElementId::from(format!("{part}-{}", id.as_uuid()))
}

/// Title bar height at zoom 1, in points.
const TITLE_H: f32 = 28.0;
/// How many of the run-target shell's last commands the palette offers to run again.
const RERUN_LINES: usize = 5;
/// Resize grip size at zoom 1.
const GRIP: f32 = 14.0;
/// How long after the last zoom change the settled frame (exact rasters) is asked for. A
/// gesture reports every few milliseconds; a frame per report and one more after the pause.
const SETTLE: Duration = Duration::from_millis(80);
/// How long a moved viewport waits before the host is told where this client looks: a pan
/// reports every frame, and the other clients only need the places the camera rests.
const LOOK_EVERY: Duration = Duration::from_millis(100);
/// How long another client's pointing stays on offer before it goes by itself.
const POINT_FOR: Duration = Duration::from_secs(8);

/// How long a closed card can be taken back (⌘Z) before a shell's session is closed for good.
const UNDO_CLOSE: Duration = Duration::from_secs(5);
/// Smallest item on screen while dragging.
const MIN_ITEM: f32 = 160.0;
/// Size of a new note.
const NOTE_SIZE: (f32, f32) = (320.0, 240.0);
/// A new file card's size in canvas units.
const FILE_SIZE: (f32, f32) = (560.0, 420.0);
/// Minimap box (points) and its distance from the viewport's bottom-right corner.
const MINIMAP: (f32, f32) = (160.0, 100.0);
const MINIMAP_MARGIN: f32 = 12.0;
const MINIMAP_PAD: f32 = 6.0;
/// Zoom step for ⌘= / ⌘-.
const ZOOM_STEP: f32 = 1.25;
/// Largest item a picked window gets on the canvas, in points.
const MAX_PICKED: (f32, f32) = (1600.0, 1000.0);

/// Things the surrounding chrome may show.
#[derive(Clone, PartialEq, Debug)]
pub enum CanvasEvent {
    /// Zoom changed (for a status readout).
    Zoom(f32),
    /// A terminal rang its bell.
    Bell(SessionId),
    /// A coding agent in this session needs the human (permission, question, finished turn).
    Attention(SessionId),
    /// How many agents are waiting on the human right now (for a count in the chrome).
    NeedsYou(usize),
    /// Something the human should read in the top bar for a moment (a picture refused).
    Notice(String),
}

/// The field naming a card, open in its title bar (see [`CanvasView::rename_item`]).
/// The toast at the top of the canvas.
struct Toast {
    /// Tells a stale dismiss timer from the current toast's.
    seq: u64,
    /// What it says.
    what: ToastKind,
}

/// What a toast is.
enum ToastKind {
    /// Another client's pointing (`CanvasSync::Pointed`): a button that goes to the card.
    Pointed {
        /// Who pointed, as they are named.
        name: String,
        /// At what.
        item: ItemId,
    },
    /// A word to this client alone ("nobody else is here").
    Said(String),
    /// A card just closed: a button that takes it back, until [`UNDO_CLOSE`] passes.
    Closed {
        /// Which closing (`ClosedCard::seq`).
        seq: u64,
        /// The card's title as it was.
        title: String,
    },
}

/// A card taken off the canvas, until [`UNDO_CLOSE`] passes or ⌘Z puts it back as it was.
/// A live shell's session runs on through the wait, so its rows come back untouched.
struct ClosedCard {
    /// The item, to put back where it was.
    item: CanvasItem,
    /// The session kept alive for the wait, for a live shell; `None` for every other card,
    /// whose whole state is the item.
    session: Option<SessionId>,
    /// Which closing, for the timer and the toast.
    seq: u64,
}

struct Rename {
    id: ItemId,
    input: Entity<InputState>,
    /// Who had the keyboard before the field: it goes back there after ↩ or Esc.
    return_to: Option<FocusHandle>,
    _subscription: gpui::Subscription,
}

/// A banner's title: what the agent is doing, led by the card's name when the human gave it
/// one, so a banner from a canvas of several agents says which card it is about.
#[must_use]
pub fn banner_title(name: Option<&str>, what: &str) -> String {
    match name {
        Some(name) => format!("{name} · {what}"),
        None => what.to_owned(),
    }
}

/// A program's notification as a banner: its title led by the card's name, "Terminal" when
/// the protocol carried no title (OSC 9), and its body.
#[must_use]
pub fn program_banner(name: Option<&str>, title: &str, body: &str) -> (String, String) {
    let title = if title.trim().is_empty() { "Terminal" } else { title.trim() };
    (banner_title(name, title), body.trim().to_owned())
}

/// Whether two card rectangles share any area.
const fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// A note's title, "note" while it is empty.
///
/// Its first non-empty line with Markdown's heading, list, quote and task marks stripped, cut
/// to [`NOTE_TITLE_CHARS`]. A note with task lines counts them after it: `Plan · 1/3`.
#[must_use]
pub fn note_title(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim())
        .map(|l| {
            ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|mark| l.strip_prefix(mark))
                .unwrap_or(l)
                .trim()
        })
        .find(|l| !l.is_empty());
    let mut title = match line {
        None => "note".to_owned(),
        Some(line) if line.chars().count() > NOTE_TITLE_CHARS => {
            let cut: String = line.chars().take(NOTE_TITLE_CHARS).collect();
            format!("{}…", cut.trim_end())
        }
        Some(line) => line.to_owned(),
    };
    if let Some((done, total)) = note_progress(text) {
        title.push_str(" · ");
        title.push_str(&done.to_string());
        title.push('/');
        title.push_str(&total.to_string());
    }
    title
}

/// How many of a note's task lines are ticked, and how many there are; `None` without any.
#[must_use]
pub fn note_progress(text: &str) -> Option<(usize, usize)> {
    let mut done = 0_usize;
    let mut total = 0_usize;
    for segment in crate::markdown::segments(text) {
        if let crate::markdown::Segment::Task(task) = segment {
            total = total.saturating_add(1);
            done = done.saturating_add(usize::from(task.done));
        }
    }
    (total > 0).then_some((done, total))
}

/// How much of a note's first line the title bar shows.
pub const NOTE_TITLE_CHARS: usize = 40;

/// A shell command that finished while nobody was looking: what the title-bar badge says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    /// What was typed.
    pub command: String,
    /// Its exit status, when the shell said.
    pub exit: Option<u8>,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Finished {
    /// The badge text: "done 3.2 s", "failed (1) 1 m 04 s" (the row caption's clock).
    #[must_use]
    pub fn label(&self) -> String {
        let took = crate::terminal::took_label(self.elapsed);
        match self.exit {
            Some(0) | None => format!("done {took}"),
            Some(code) => format!("failed ({code}) {took}"),
        }
    }
}

/// How long a shell command has to run before its end, unwatched, is worth a badge: shorter
/// commands end before the human has looked away.
pub const SLOW_COMMAND: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
enum Drag {
    Move {
        id: ItemId,
        grab: Point<Pixels>,
        start: Rect,
    },
    Resize {
        id: ItemId,
        grab: Point<Pixels>,
        start: Rect,
    },
    Pan {
        last: Point<Pixels>,
    },
    /// Scrubbing the minimap: the camera centres on the pointer.
    Minimap,
}

/// How the minimap maps canvas units to its box: `screen = origin + (canvas - world_origin) *
/// scale`.
#[derive(Clone, Copy, Debug)]
struct MinimapMap {
    /// Box origin in window coordinates.
    origin: Point<Pixels>,
    /// Box size.
    size: Size<Pixels>,
    /// Canvas point drawn at the box's padded top-left.
    world: (f32, f32),
    scale: f32,
}

impl MinimapMap {
    /// Fit the union of `rects` (items and viewport) into the box.
    fn new(origin: Point<Pixels>, rects: impl IntoIterator<Item = Rect>) -> Self {
        let mut b: Option<(f32, f32, f32, f32)> = None;
        for r in rects {
            let e = b.get_or_insert((r.x, r.y, r.x + r.w, r.y + r.h));
            e.0 = e.0.min(r.x);
            e.1 = e.1.min(r.y);
            e.2 = e.2.max(r.x + r.w);
            e.3 = e.3.max(r.y + r.h);
        }
        let (x0, y0, x1, y1) = b.unwrap_or((0.0, 0.0, 1.0, 1.0));
        let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
        let inner = (MINIMAP.0 - MINIMAP_PAD - MINIMAP_PAD, MINIMAP.1 - MINIMAP_PAD - MINIMAP_PAD);
        let scale = (inner.0 / w).min(inner.1 / h);
        // Centre the drawing in the box.
        let world = (x0 - (inner.0 / scale - w) / 2.0, y0 - (inner.1 / scale - h) / 2.0);
        Self { origin, size: size(px(MINIMAP.0), px(MINIMAP.1)), world, scale }
    }

    fn to_box(self, r: Rect) -> Bounds<Pixels> {
        let x = (r.x - self.world.0).mul_add(self.scale, f32::from(self.origin.x) + MINIMAP_PAD);
        let y = (r.y - self.world.1).mul_add(self.scale, f32::from(self.origin.y) + MINIMAP_PAD);
        Bounds::new(point(px(x), px(y)), size(px(r.w * self.scale), px(r.h * self.scale)))
    }

    fn to_canvas(self, p: Point<Pixels>) -> (f32, f32) {
        (
            (f32::from(p.x) - f32::from(self.origin.x) - MINIMAP_PAD) / self.scale + self.world.0,
            (f32::from(p.y) - f32::from(self.origin.y) - MINIMAP_PAD) / self.scale + self.world.1,
        )
    }

    fn contains(self, p: Point<Pixels>) -> bool {
        Bounds::new(self.origin, self.size).contains(&p)
    }
}

/// The plane.
pub struct CanvasView {
    doc: CanvasDoc,
    camera: Camera,
    me: ClientId,
    out: mpsc::Sender<ClientMsg>,
    theme: Theme,
    terminals: HashMap<SessionId, Entity<TerminalView>>,
    sessions: HashMap<SessionId, SessionSummary>,
    /// Coding agents the host has observed, by session.
    agents: HashMap<SessionId, AgentEvent>,
    /// Terminal sessions oldest first, moved to the end when one is activated: the order the
    /// "run in shell" button picks its target from (the most recently focused shell, else the
    /// newest one). Sessions that are not shells any more are skipped, not removed, so a
    /// terminal an agent was seen in goes back to being a candidate if it ever stops being one.
    shell_recency: Vec<SessionId>,
    /// Holds the device out of idle sleep while an agent works in any session (thinking or
    /// running a tool): the human is waiting on it, not the other way round. Released when
    /// every agent is idle or waiting on the human.
    awake: Option<Task<()>>,
    /// Shell commands that ran long and finished while their item was not the active one, by
    /// session: badged on the title bar until the item is activated.
    finished: HashMap<SessionId, Finished>,
    /// A command that ran at least this long earns the badge when it ends unwatched.
    slow_command: Duration,
    /// `slopty hook install` has been offered to this human once; the offer stops showing
    /// whether they took it or not, and comes back only if the host reports it failed.
    hooks_offered: bool,
    screens: HashMap<ItemId, Entity<ScreenView>>,
    notes: HashMap<ItemId, Entity<NoteView>>,
    files: HashMap<ItemId, Entity<FileView>>,
    /// The line a file card opened at, for a view not made yet.
    file_focus: HashMap<ItemId, u32>,
    /// The paths the host was last asked to watch for the file cards, sorted.
    watched: Vec<String>,
    /// Streams requested from the host but not yet `Opened`, by target.
    pending_opens: HashMap<CaptureTarget, ItemId>,
    /// A `List` is in flight to name restored window items.
    titles_requested: bool,
    /// Item titles the picker gave us (the document only stores ids).
    titles: HashMap<ItemId, String>,
    open_screen: ScreenFactory,
    picker: Option<Entity<WindowPicker>>,
    /// A `List` is in flight for the picker.
    picker_wanted: bool,
    /// The command palette, while ⌘⇧P has it up.
    palette: Option<Entity<CommandPalette>>,
    /// The needle of a find in every card while its palette is up.
    find_needle: Option<String>,
    /// Hit counts per card for `find_needle`, with what ↩ on its line does: the shells' as
    /// the hosts answer, the rest counted here.
    find_hits: HashMap<ItemId, (u32, PaletteRun)>,
    /// A card to open its find bar on a needle once revealed (needs the window: from render).
    pending_find: Option<(SessionId, String)>,
    /// A file card to open its find bar on a needle once revealed.
    pending_find_file: Option<(ItemId, String)>,
    /// The last needle of a find in every card: the next one starts from it when no card
    /// offers its own.
    last_find: String,

    /// The field naming a card, while one is open in a title bar.
    rename: Option<Rename>,
    /// Where the keyboard goes once the name field closed (applied from `render`).
    rename_return: Option<FocusHandle>,
    /// The name field just opened: it takes the keyboard from `render`, after any focus the
    /// click that activated the card left pending.
    pending_focus_rename: bool,
    /// Where the keyboard was when the palette opened; it goes back there when it closes.
    palette_return: Option<FocusHandle>,
    /// What the palette chose, dispatched on the next frame once the focus is back.
    palette_action: Option<Box<dyn Action>>,
    /// The app's own lines for the palette (settings, hosts), after the canvas's.
    palette_extra: Vec<PaletteItem>,
    /// Focus the palette's field on the next frame.
    pending_focus_palette: bool,
    /// The next listing adds its first display straight away (the self-test socket's way
    /// to put a remote display on the canvas without the picker).
    display_wanted: bool,
    /// The stats overlay is on (applies to windows opened later too).
    show_stats: bool,
    /// Latest link RTT, handed to windows opened later.
    rtt: Option<Duration>,
    /// Viewport origin (window coordinates) and size, recorded each frame.
    viewport: (Point<Pixels>, Size<Pixels>),
    /// Fit every item into the viewport on the next frame (once the viewport is known).
    fit_pending: bool,
    /// Items the host placed for us before a frame measured the viewport: sized to it on
    /// the first frame, before the reveal that follows.
    fit_items_pending: Vec<ItemId>,
    /// The viewport last told to the host (`ClientMsg::Look`).
    looked: Option<Rect>,
    /// A `Look` is scheduled for `LOOK_EVERY` from the first change; it reads the viewport
    /// as it is then, so a pan of many frames is one message.
    look_pending: bool,
    /// The client whose viewport the camera keeps up with: every move they report is flown
    /// to, until this client moves the camera itself or they leave.
    following: Option<ClientId>,
    /// The card the canvas this one replaces (after a reconnect) had active, made active
    /// again once the snapshot brings it.
    resume_active: Option<ItemId>,
    /// The toast at the top: another client's pointing on offer, or a word to this client,
    /// until a click or [`POINT_FOR`].
    toast: Option<Toast>,
    /// Cards closed within the last [`UNDO_CLOSE`], oldest first; a shell's view stays in
    /// `terminals`, attached, so taking one back shows it as it was.
    closed: Vec<ClosedCard>,
    closed_seq: u64,
    /// A camera move in progress, advanced once per frame by the render loop.
    flight: Option<Flight>,
    /// The camera zoom the last frame drew, and whether this frame's differs (a pinch, a
    /// flight): terminals and chrome text then paint from the raster ladder, and one more
    /// frame is asked for so the settled zoom paints exact.
    zoom_drawn: Option<f32>,
    zooming: bool,
    /// Bumped by every frame in motion; the settle timer that finds it unchanged notifies.
    settle_generation: u64,
    /// A settle frame is owed: the zoom changed and `SETTLE` has not passed since. Frames the
    /// flood asks for in between stay in motion, so they paint from the ladder too.
    settle_pending: bool,
    /// Frames rendered while the zoom was in motion (tests).
    #[cfg(test)]
    zooming_frames: u32,
    /// When the last frame ran, so a flight advances by real elapsed time.
    frame_at: Option<std::time::Instant>,
    /// Whether camera moves are animated. Off under the self-test, where a frame is a step.
    animate: bool,
    /// Repository headings from the last [`CanvasView::arrange_by_repo`], in canvas units.
    headings: Vec<Heading>,
    /// Bring this item into view on the next frame (one we just created).
    reveal_pending: Option<ItemId>,
    drag: Option<Drag>,
    /// The minimap's mapping as of the last paint (for hit-testing and scrubbing).
    minimap: Option<MinimapMap>,
    active: Option<ItemId>,
    /// A terminal to focus on the next frame (one we just opened).
    pending_focus: Option<SessionId>,
    /// A note to put the caret in on the next frame (one we just created).
    pending_focus_note: Option<ItemId>,
    /// Focus the picker on the next frame.
    pending_focus_picker: bool,
    /// Focus the canvas itself on the next frame (after the picker closes).
    pending_focus_self: bool,
    focus: FocusHandle,
    subscriptions: Vec<gpui::Subscription>,
}

impl std::fmt::Debug for CanvasView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasView")
            .field("items", &self.doc.items().count())
            .field("camera", &self.camera)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<CanvasEvent> for CanvasView {}

impl Focusable for CanvasView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CanvasView {
    /// A canvas for one host link.
    pub fn new(
        me: ClientId,
        out: mpsc::Sender<ClientMsg>,
        sessions: Vec<SessionSummary>,
        open_screen: ScreenFactory,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            doc: CanvasDoc::default(),
            camera: Camera::default(),
            resume_active: None,
            me,
            out,
            theme,
            terminals: HashMap::new(),
            shell_recency: sessions
                .iter()
                .filter(|s| s.kind == SessionKind::Terminal)
                .map(|s| s.id)
                .collect(),
            sessions: sessions.into_iter().map(|s| (s.id, s)).collect(),
            agents: HashMap::new(),
            awake: None,
            finished: HashMap::new(),
            slow_command: SLOW_COMMAND,
            hooks_offered: false,
            screens: HashMap::new(),
            notes: HashMap::new(),
            files: HashMap::new(),
            file_focus: HashMap::new(),
            watched: Vec::new(),
            pending_opens: HashMap::new(),
            titles_requested: false,
            titles: HashMap::new(),
            open_screen,
            picker: None,
            picker_wanted: false,
            palette: None,
            rename: None,
            rename_return: None,
            pending_focus_rename: false,
            palette_return: None,
            find_needle: None,
            find_hits: HashMap::new(),
            pending_find: None,
            pending_find_file: None,
            last_find: String::new(),
            palette_action: None,
            palette_extra: Vec::new(),
            pending_focus_palette: false,
            display_wanted: false,
            show_stats: false,
            rtt: None,
            viewport: (point(px(0.0), px(0.0)), size(px(1.0), px(1.0))),
            fit_pending: false,
            fit_items_pending: Vec::new(),
            looked: None,
            following: None,
            toast: None,
            closed: Vec::new(),
            closed_seq: 0,
            look_pending: false,
            flight: None,
            zoom_drawn: None,
            zooming: false,
            settle_generation: 0,
            settle_pending: false,
            #[cfg(test)]
            zooming_frames: 0,
            frame_at: None,
            animate: true,
            headings: Vec::new(),
            reveal_pending: None,
            drag: None,
            minimap: None,
            active: None,
            pending_focus: None,
            pending_focus_note: None,
            pending_focus_picker: false,
            pending_focus_self: false,
            focus: cx.focus_handle(),
            subscriptions: Vec::new(),
        }
    }

    /// Camera zoom.
    #[must_use]
    pub const fn zoom(&self) -> f32 {
        self.camera.zoom
    }

    /// Every item, bottom to top.
    pub fn items(&self) -> Vec<&CanvasItem> {
        self.doc.by_z()
    }

    /// The camera (canvas → viewport mapping).
    #[must_use]
    pub const fn camera(&self) -> Camera {
        self.camera
    }

    /// Where the canvas this one replaces was: the camera, at once and without a flight, and
    /// the active card, once the snapshot brings it (a card gone meanwhile is nothing to
    /// activate). The app calls this on a reconnect, so a dropped link does not send the
    /// reader back to the origin.
    pub fn resume_at(&mut self, camera: Camera, active: Option<ItemId>, cx: &mut Context<Self>) {
        self.flight = None;
        self.camera = camera;
        self.resume_active = active;
        cx.emit(CanvasEvent::Zoom(camera.zoom));
        cx.notify();
    }

    /// Where a canvas rect sits in the window, in points, as of the last frame.
    #[must_use]
    pub fn window_bounds(&self, rect: Rect) -> Bounds<Pixels> {
        let s = self.camera.to_screen(rect);
        let (origin, _size) = self.viewport;
        Bounds::new(point(origin.x + px(s.x), origin.y + px(s.y)), size(px(s.w), px(s.h)))
    }

    /// The item that last took a click or a key.
    #[must_use]
    pub const fn active_item(&self) -> Option<ItemId> {
        self.active
    }

    /// Number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.doc.items().count()
    }

    /// True when nothing is on the canvas.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// This client's id on the wire.
    #[must_use]
    pub const fn me(&self) -> ClientId {
        self.me
    }

    /// The terminal view for `session`, if it is on the canvas.
    #[must_use]
    pub fn terminal(&self, session: SessionId) -> Option<&Entity<TerminalView>> {
        self.terminals.get(&session)
    }

    // ----- host events ---------------------------------------------------------------------

    /// A canvas snapshot or delta from the host.
    pub fn apply_sync(&mut self, sync: CanvasSync, cx: &mut Context<Self>) {
        // An item the host created for us (our `OpenSession`) becomes active and focused.
        let ours = match &sync {
            CanvasSync::Delta { by, op: CanvasOp::Upsert(item), .. } if *by == self.me => {
                match item.kind {
                    ItemKind::Terminal { session } => Some((item.id, session)),
                    _ => None,
                }
            }
            _ => None,
        };
        // A pointing carries the pointer's name only on the wire: taken before the document
        // reduces it to the item. One at a card this canvas does not have is nothing to offer.
        if let CanvasSync::Pointed { client, name, item } = &sync
            && *client != self.me
            && self.doc.get(*item).is_some()
        {
            self.show_toast(ToastKind::Pointed { name: name.clone(), item: *item }, cx);
        }
        let change = self.doc.apply_sync(sync, self.me);
        tracing::debug!(?change, version = self.doc.version(), "canvas sync");
        if let CanvasChange::Presence(client) = change
            && self.following == Some(client)
        {
            self.follow(client, cx);
        }
        self.reconcile(cx);
        if let Some(id) = self.resume_active.take_if(|id| self.doc.get(*id).is_some()) {
            // Active again as it was, its keyboard back in it; not raised — the reader did
            // nothing to the document.
            self.active = Some(id);
            if let Some(ItemKind::Terminal { session }) = self.doc.get(id).map(|i| &i.kind) {
                self.pending_focus = Some(*session);
            }
        }
        if let Some((id, session)) = ours {
            self.fit_to_viewport(id);
            self.active = Some(id);
            self.pending_focus = Some(session);
            self.reveal_pending = Some(id);
        }
        cx.notify();
    }

    /// Shrink an item the host just placed for us so it fits this viewport at zoom 1: a phone
    /// gets a phone-sized terminal (and, as its driver, a PTY of that size) instead of a desktop
    /// one it can only read zoomed out. Desktop viewports are larger than the default and are
    /// left alone.
    fn fit_to_viewport(&mut self, id: ItemId) {
        let Some((max_w, max_h)) = self.viewport_max() else {
            // The first shell arrives with the attach, often before the first frame: fit it
            // then, or a phone opens onto a desktop-sized card hanging off its edge.
            self.fit_items_pending.push(id);
            return;
        };
        let Some(item) = self.doc.get(id) else { return };
        if item.rect.w <= max_w && item.rect.h <= max_h {
            return;
        }
        let rect = Rect {
            x: item.rect.x,
            y: item.rect.y,
            w: item.rect.w.min(max_w),
            h: item.rect.h.min(max_h),
        };
        self.propose(CanvasOp::Place { id, rect });
    }

    /// `size` cut down to what fits this viewport at zoom 1: a card this client opens is
    /// phone-sized on a phone, its default size everywhere larger.
    fn fitted(&self, size: (f32, f32)) -> (f32, f32) {
        let Some((max_w, max_h)) = self.viewport_max() else { return size };
        (size.0.min(max_w), size.1.min(max_h))
    }

    /// The largest item that fits this viewport at zoom 1 with a `GAP` margin.
    fn viewport_max(&self) -> Option<(f32, f32)> {
        let (_, vp) = self.viewport;
        let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
        if vw <= 1.0 || vh <= 1.0 {
            return None;
        }
        let margin = 2.0 * GAP;
        Some((snap((vw - margin).max(MIN_ITEM)), snap((vh - margin).max(MIN_ITEM))))
    }

    /// Drive the terminal in `id` from here: size the item for this viewport (no larger than
    /// the viewport, no smaller than the default unless the viewport is) and take the PTY
    /// size. The phone uses it to take over a desktop terminal; the desktop to take it back.
    pub fn take_over(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(item) = self.doc.get(id) else { return };
        let ItemKind::Terminal { session } = item.kind else { return };
        let rect = item.rect;
        if let Some((max_w, max_h)) = self.viewport_max() {
            let w = rect.w.clamp(TERMINAL_SIZE.0.min(max_w), max_w);
            let h = rect.h.clamp(TERMINAL_SIZE.1.min(max_h), max_h);
            if (w, h) != (rect.w, rect.h) {
                self.propose(CanvasOp::Place { id, rect: Rect { x: rect.x, y: rect.y, w, h } });
            }
        }
        if let Some(view) = self.terminals.get(&session) {
            view.read(cx).drive();
        }
        self.touch_session(session);
        self.active = Some(id);
        self.pending_focus = Some(session);
        self.reveal_pending = Some(id);
        cx.notify();
    }

    /// A session appeared (ours or another client's).
    pub fn session_opened(&mut self, summary: SessionSummary, cx: &mut Context<Self>) {
        if summary.kind == SessionKind::Terminal && !self.shell_recency.contains(&summary.id) {
            self.shell_recency.push(summary.id);
        }
        self.sessions.insert(summary.id, summary);
        self.reconcile(cx);
        cx.notify();
    }

    /// `session` is a plain shell on this canvas: a terminal session (not one the host drives
    /// as an agent), drawn here, and one no coding agent has been seen in. The only thing a
    /// fenced block from an answer may be typed into.
    fn is_shell(&self, session: SessionId) -> bool {
        self.terminals.contains_key(&session)
            && !self.agents.contains_key(&session)
            && self.sessions.get(&session).is_some_and(|s| s.kind == SessionKind::Terminal)
    }

    /// The shell a fenced block runs in: the most recently activated one, else the newest.
    fn run_target(&self) -> Option<SessionId> {
        self.shell_recency.iter().rev().copied().find(|s| self.is_shell(*s))
    }

    /// Remember that `session` was just activated, so a "run in shell" goes to the shell the
    /// human was last in rather than whichever opened last.
    fn touch_session(&mut self, session: SessionId) {
        if let Some(at) = self.shell_recency.iter().position(|s| *s == session) {
            let s = self.shell_recency.remove(at);
            self.shell_recency.push(s);
        }
    }

    /// Tell every note whether there is a shell to run a fenced block in, so it draws the
    /// "run" button only when a click on it would go somewhere. Called whenever the set of
    /// shells can have changed.
    fn update_run_targets(&self, cx: &mut Context<Self>) {
        let can = self.run_target().is_some();
        for note in self.notes.values() {
            note.update(cx, |n, cx| n.set_can_run(can, cx));
        }
    }

    /// A "run" button on a fenced block was pressed: reveal the shell it goes to and type the
    /// code into it, as the block menu's "rerun" does — a paste, then ↩ once.
    pub fn run_in_shell(&mut self, code: String, cx: &mut Context<Self>) {
        let Some(target) = self.run_target() else { return };
        self.reveal_session(target, cx);
        if let Some(view) = self.terminals.get(&target).cloned() {
            view.update(cx, |v, cx| v.run_text(code, cx));
        }
    }

    /// A session changed directory (OSC 7), and the host says which repository that is in.
    /// The opening summary is only where the shell *started*; arrange-by-repo groups on where
    /// it is now.
    pub fn session_moved(&mut self, session: SessionId, cwd: String, repo: Option<String>) {
        if let Some(summary) = self.sessions.get_mut(&session) {
            summary.cwd = Some(cwd);
            summary.repo = repo;
        }
    }

    /// Hold the device awake while any agent works; let go when none does.
    fn update_awake(&mut self, cx: &Context<Self>) {
        let working = self
            .agents
            .values()
            .any(|a| matches!(a.status, AgentStatus::Working | AgentStatus::Tool { .. }));
        if !working {
            self.awake = None;
        } else if self.awake.is_none() {
            let acquisition = cx.prevent_idle_sleep("Slopty agent working");
            self.awake = Some(cx.spawn(async move |_this, _cx| match acquisition.await {
                Ok(guard) => {
                    let _guard = guard;
                    std::future::pending::<()>().await;
                }
                Err(e) => tracing::warn!(error = %e, "idle sleep prevention"),
            }));
        }
    }

    /// A session is gone.
    pub fn session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.sessions.remove(&session);
        self.agents.remove(&session);
        self.shell_recency.retain(|s| *s != session);
        self.update_awake(cx);
        self.reconcile(cx);
        self.count_needs_you(cx);
        cx.notify();
    }

    /// The stream view of the active item, if the active item is a window or display.
    #[must_use]
    pub fn active_screen(&self) -> Option<Entity<ScreenView>> {
        let item = self.doc.get(self.active?)?;
        match item.kind {
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                self.screens.get(&item.id).cloned()
            }
            ItemKind::Terminal { .. } | ItemKind::Note { .. } | ItemKind::File { .. } => None,
        }
    }

    /// The stream view of one item, if it has one open.
    #[must_use]
    pub fn screen(&self, item: ItemId) -> Option<&Entity<ScreenView>> {
        self.screens.get(&item)
    }

    /// ⌘⇧I: the stats overlay on every remote window (fps, bitrate, RTT, loss).
    pub fn toggle_stats(&mut self, _: &ToggleStats, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_hud(self.show_stats, cx));
        }
        cx.notify();
    }

    /// ⌘⇧M: silence or resume the active remote window's audio (this client only).
    pub fn toggle_mute(&mut self, _: &ToggleMute, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active_screen() {
            view.read(cx).toggle_mute();
            cx.notify();
        }
    }

    /// What the phone key bar drives: the active terminal, or the active remote window.
    #[must_use]
    pub fn active_key_target(&self) -> Option<KeyTarget> {
        if let Some(t) = self.active_terminal() {
            return Some(KeyTarget::Terminal(t));
        }
        self.active_screen().map(KeyTarget::Screen)
    }

    /// The terminal of the active item, if the active item is a terminal.
    #[must_use]
    pub fn active_terminal(&self) -> Option<Entity<TerminalView>> {
        let item = self.doc.get(self.active?)?;
        match item.kind {
            ItemKind::Terminal { session } => self.terminals.get(&session).cloned(),
            _ => None,
        }
    }

    /// The host observed a coding agent's state in a session.
    pub fn agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        let session = event.session;
        if let Some(view) = self.terminals.get(&session) {
            let status = (event.status != AgentStatus::None).then(|| event.status.clone());
            view.update(cx, |v, cx| v.set_agent_status(status, cx));
        }
        if event.status == AgentStatus::None {
            self.agents.remove(&session);
        } else {
            let attention = event.attention;
            let needs_human = needs_human(&event);
            if attention {
                self.notify_system(&event, cx);
            } else if !needs_human {
                cx.dismiss_system_notification(&session.to_string());
            }
            self.agents.insert(session, event);
            if attention {
                cx.emit(CanvasEvent::Attention(session));
            }
        }
        self.update_awake(cx);
        self.count_needs_you(cx);
        self.update_run_targets(cx);
        cx.notify();
    }

    /// What the host last said about a session's agent, source included.
    #[must_use]
    pub fn agent(&self, session: SessionId) -> Option<&AgentEvent> {
        self.agents.get(&session)
    }

    /// A shell command ended in `session`. Long enough, and in an item the human is not on,
    /// it earns a title-bar badge (like an agent's), cleared when the item is activated. A
    /// new end replaces an older badge.
    pub fn command_finished(&mut self, session: SessionId, done: Finished, cx: &mut Context<Self>) {
        let watched = self.doc.item_for_session(session).is_some_and(|i| self.active == Some(i.id));
        let slow = done.elapsed >= self.slow_command;
        tracing::info!(%session, watched, slow, elapsed = ?done.elapsed, "command finished");
        if watched || !slow {
            return;
        }
        self.finished.insert(session, done);
        cx.notify();
    }

    /// The badge a session's last long command left, if the item has not been looked at since.
    #[must_use]
    pub fn finished(&self, session: SessionId) -> Option<&Finished> {
        self.finished.get(&session)
    }

    /// How long a command has to run before its unwatched end is badged.
    pub const fn set_slow_command(&mut self, after: Duration) {
        self.slow_command = after;
    }

    /// Whether `slopty hook install` has already been offered on this canvas.
    #[must_use]
    pub const fn hooks_offered(&self) -> bool {
        self.hooks_offered
    }

    /// Ask the host to register `slopty hook` for its Claude Code, so the agents in its
    /// terminals report precisely instead of being read off their process and their title.
    /// Offered once per run from the title bar of a session attributed without hooks.
    pub fn install_hooks(&mut self, cx: &mut Context<Self>) {
        self.hooks_offered = true;
        self.send(ClientMsg::InstallHooks);
        cx.notify();
    }

    /// The host could not install the hooks: put the offer back so it can be tried again.
    pub fn hooks_offer_failed(&mut self, cx: &mut Context<Self>) {
        self.hooks_offered = false;
        cx.notify();
    }

    /// A banner through the notification centre when the human is not looking at the app:
    /// the badge text as title (led by the card's name when it has one — with several
    /// agents, "build box · Claude wants to use Bash" says which), the detail as body. GPUI
    /// drops it silently outside a bundle or when the user declined notifications.
    fn notify_system(&self, event: &AgentEvent, cx: &Context<Self>) {
        let active = cx.active_window().is_some();
        tracing::debug!(active, session = %event.session, "agent banner");
        if active {
            return;
        }
        let name = self
            .doc
            .items()
            .find(|i| matches!(i.kind, ItemKind::Terminal { session } if session == event.session))
            .and_then(|i| i.name.as_deref());
        let title = match &event.status {
            AgentStatus::Blocked(BlockReason::Permission { tool }) if tool.is_empty() => {
                "Claude needs permission".to_owned()
            }
            AgentStatus::Blocked(BlockReason::Permission { tool }) => {
                format!("Claude wants to use {tool}")
            }
            AgentStatus::Blocked(BlockReason::Question) => "Claude has a question".to_owned(),
            AgentStatus::Blocked(BlockReason::Elicitation) => "Claude needs input".to_owned(),
            AgentStatus::Done => "Claude finished".to_owned(),
            _ => agent_status_text(event),
        };
        let title = banner_title(name, &title);
        let body = event.detail.clone().filter(|d| !d.is_empty()).unwrap_or_default();
        cx.show_system_notification(SystemNotification {
            tag: event.session.to_string().into(),
            title: title.into(),
            body: body.into(),
            actions: Vec::new(),
        });
    }

    /// A program in a terminal asked for a desktop notification (OSC 9 / 777 / 99): the same
    /// banner an agent gets when the human is not looking, tagged by the session so a click
    /// reveals the card, and the dock bounce either way.
    fn notify_program(&self, session: SessionId, title: &str, body: &str, cx: &mut Context<Self>) {
        if cx.active_window().is_none() {
            let name = self
                .doc
                .items()
                .find(|i| matches!(i.kind, ItemKind::Terminal { session: s } if s == session))
                .and_then(|i| i.name.as_deref());
            let (title, body) = program_banner(name, title, body);
            cx.show_system_notification(SystemNotification {
                tag: session.to_string().into(),
                title: title.into(),
                body: body.into(),
                actions: Vec::new(),
            });
        }
        cx.emit(CanvasEvent::Attention(session));
    }

    /// The user activated a notification: it reveals the session (the answer is typed into
    /// the agent's own prompt there). Called from the app's response handler with the tag
    /// parsed.
    pub fn notification_response(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.reveal_session(session, cx);
    }

    /// Sessions whose agent is waiting on the human, in reading order (top to bottom, left to
    /// right) so ⌘⇧A walks the canvas predictably.
    fn needs_you(&self) -> Vec<(ItemId, SessionId)> {
        let mut out: Vec<(Rect, ItemId, SessionId)> = self
            .doc
            .items()
            .filter_map(|i| match i.kind {
                ItemKind::Terminal { session } => Some((i.rect, i.id, session)),
                _ => None,
            })
            .filter(|(_, _, s)| self.agents.get(s).is_some_and(needs_human))
            .collect();
        out.sort_by(|a, b| a.0.y.total_cmp(&b.0.y).then_with(|| a.0.x.total_cmp(&b.0.x)));
        out.into_iter().map(|(_, id, s)| (id, s)).collect()
    }

    /// How many agents are waiting on the human.
    #[must_use]
    pub fn needs_you_count(&self) -> usize {
        self.needs_you().len()
    }

    /// Tell the chrome the count (it shows a pill while it is non-zero).
    fn count_needs_you(&self, cx: &mut Context<Self>) {
        cx.emit(CanvasEvent::NeedsYou(self.needs_you_count()));
    }

    /// ⌘⇧A: reveal and focus the next terminal whose agent is waiting on the human, cycling
    /// from the active item.
    pub fn next_attention(
        &mut self,
        _: &NextAttention,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = self.needs_you();
        let Some(&(_, first)) = waiting.first() else { return };
        // After the active item when it is in the list, wrapping to the first; else the first.
        let next = self
            .active
            .and_then(|active| waiting.iter().position(|(id, _)| *id == active))
            .and_then(|i| waiting.iter().cycle().nth(i.saturating_add(1)))
            .map_or(first, |(_, s)| *s);
        self.reveal_session(next, cx);
    }

    /// Bring a session's terminal into view, make it active and give it the keyboard (the
    /// "answer" button on a question badge: the reply has to be typed).
    pub fn reveal_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(id) = self.doc.item_for_session(session).map(|i| i.id) else { return };
        self.reveal_item(id, cx);
        self.pending_focus = Some(session);
    }

    /// Bring a card into view and make it active, without asking for the keyboard: only a
    /// terminal can take it before its view exists.
    fn reveal_item(&mut self, id: ItemId, cx: &mut Context<Self>) {
        self.activate(id, cx);
        self.reveal_pending = Some(id);
        cx.notify();
    }

    /// Swap the theme everywhere: the plane, every terminal (which re-fits its grid to the
    /// new font on its next frame), every window's chrome, every note and the picker.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        for view in self.terminals.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.notes.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.files.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        if let Some(picker) = &self.picker {
            picker.update(cx, |p, cx| p.set_theme(theme.clone(), cx));
        }
        if self.theme.terminal != theme.terminal {
            // Every shell hears the new colours; the host applies the driver's.
            let colors = theme.terminal.wire();
            for session in self.terminals.keys() {
                self.send(ClientMsg::Term { session: *session, req: TermRequest::Colors(colors) });
            }
        }
        self.theme = theme;
        cx.notify();
    }

    /// Whether `session` is a terminal on this canvas (which host a banner belongs to).
    #[must_use]
    pub fn has_session(&self, session: SessionId) -> bool {
        self.terminals.contains_key(&session)
    }

    /// Link RTT (fanned out to every terminal's predictor and every window's overlay).
    pub fn set_rtt(&mut self, rtt: Option<Duration>, cx: &mut Context<Self>) {
        self.rtt = rtt;
        for view in self.terminals.values() {
            view.update(cx, |v, _| v.set_rtt(rtt));
        }
        for view in self.screens.values() {
            view.update(cx, |v, _| v.set_rtt(rtt));
        }
    }

    /// A remote-window event from the host.
    pub fn screen_event(&mut self, event: ScreenEvent, cx: &mut Context<Self>) {
        match event {
            ScreenEvent::Listing { windows, displays } => {
                self.fill_titles(&windows);
                if std::mem::take(&mut self.display_wanted)
                    && let Some(d) = displays.first()
                {
                    self.add_screen_item(
                        CaptureTarget::Display(d.id),
                        (d.w, d.h),
                        format!("display {}", d.id),
                    );
                }
                if self.picker_wanted {
                    self.picker_wanted = false;
                    self.show_picker(windows, displays, cx);
                }
            }
            ScreenEvent::Opened { stream, target, codec, width, height, .. } => {
                let Some(id) = self.pending_opens.remove(&target) else {
                    tracing::debug!(%stream, ?target, "opened stream nobody asked for; closing");
                    self.send(ClientMsg::Screen(ScreenRequest::Close(stream)));
                    return;
                };
                let handle = (self.open_screen)(stream, codec);
                let out = self.out.clone();
                let theme = self.theme.clone();
                let quality = self.quality_for();
                let opened =
                    crate::screen::Opened { stream, target, size: (width, height), quality };
                let view = cx.new(|cx| ScreenView::new(opened, handle, out, theme, cx));
                let (show_stats, rtt) = (self.show_stats, self.rtt);
                view.update(cx, |v, cx| {
                    v.set_rtt(rtt);
                    if show_stats {
                        v.set_hud(true, cx);
                    }
                });
                self.subscriptions.push(cx.subscribe(&view, move |this, _view, event, cx| {
                    match event {
                        crate::screen::ScreenViewEvent::Pressed => this.activate(id, cx),
                        crate::screen::ScreenViewEvent::Ready => cx.notify(),
                    }
                }));
                self.screens.insert(id, view);
            }
            ScreenEvent::Closed { stream, reason } => {
                let gone: Vec<ItemId> = self
                    .screens
                    .iter()
                    .filter(|(_, v)| v.read(cx).stream() == stream)
                    .map(|(id, _)| *id)
                    .collect();
                for id in gone {
                    tracing::info!(%stream, %reason, "screen closed by host");
                    self.screens.remove(&id);
                }
            }
            ScreenEvent::Geometry { stream, width, height } => {
                self.follow_geometry(stream, width, height, cx);
            }
            ScreenEvent::Clipboard { text } => {
                // Every open window shares the one host pasteboard.
                for view in self.screens.values() {
                    view.update(cx, |v, _| v.host_clipboard_changed(&text));
                }
                // Same text already here: leave the clipboard alone. With the host on this
                // very Mac (the dev loop) the write would bump the change count the host
                // watches and the two would echo the text back and forth forever.
                let same =
                    cx.read_from_clipboard().and_then(|i| i.text()).as_deref() == Some(&*text);
                if !same {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                }
            }
            ScreenEvent::Rate { stream, target_bps, verdict, capped } => {
                for view in self.screens.values() {
                    if view.read(cx).stream() == stream {
                        view.update(cx, |v, _| v.set_rate(target_bps, verdict, capped));
                    }
                }
            }
            ScreenEvent::Source { stream, state } => {
                for view in self.screens.values() {
                    if view.read(cx).stream() == stream {
                        view.update(cx, |v, cx| v.set_source_state(state, cx));
                    }
                }
            }
            ScreenEvent::Cursor { stream, shape } => {
                for view in self.screens.values() {
                    if view.read(cx).stream() == stream {
                        view.update(cx, |v, cx| v.set_cursor_shape(shape.clone(), cx));
                    }
                }
            }
            ScreenEvent::ListingChanged => {}
        }
        cx.notify();
    }

    /// Requested quality for a new stream: the settings' rate, ceiling and depth, at full
    /// scale unless the canvas is zoomed out.
    fn quality_for(&self) -> Quality {
        let zoom = self.camera.zoom.clamp(0.25, 1.0);
        let scale = (zoom * 4.0).ceil() / 4.0;
        crate::screen::quality_of(self.theme.behaviour.stream, scale)
    }

    /// Lines the app adds to the palette after the canvas's own (settings, hosts).
    pub fn extend_palette(&mut self, items: Vec<PaletteItem>) {
        self.palette_extra = items;
    }

    /// Every line the palette offers: the canvas's sessions to go to (agents waiting on the
    /// human first, as the picker orders them), its file cards, the other clients to follow,
    /// the last few commands of the shell a "run" would go to, then every action, then the
    /// app's own.
    #[must_use]
    pub fn palette_lines(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = self
            .session_rows(cx)
            .into_iter()
            .map(|row| {
                PaletteItem::session(&row.title, &row.status.unwrap_or_default(), row.session)
            })
            .collect();
        // Every file card, and every other card the human named: a name is a wish to find
        // it again.
        items.extend(self.doc.items().filter_map(|i| match &i.kind {
            ItemKind::Terminal { .. } => None,
            ItemKind::File { .. } => Some(PaletteItem::item(&self.card_title(i, cx), "file", i.id)),
            ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::Note { .. } => {
                i.name.as_deref().map(|name| PaletteItem::item(name, Self::kind_name(i), i.id))
            }
        }));
        // Every other client looking at the canvas: "Follow <name>".
        items.extend(
            self.doc.lookers().map(|l| PaletteItem::looker(&l.name, device_name(l.kind), l.client)),
        );
        // The shell a "run" would go to: its last few commands, to run again.
        if let Some(shell) = self.run_target()
            && let Some(view) = self.terminals.get(&shell)
        {
            let state = view.read(cx).state();
            items.extend(
                state.recent_commands(RERUN_LINES).iter().map(|c| PaletteItem::rerun(c, shell)),
            );
        }
        items.extend(palette_items());
        items.extend(self.palette_extra.iter().cloned());
        items
    }

    /// The word for what a card is: `terminal`, `window`, `display`, `note`, `file`.
    const fn kind_name(item: &CanvasItem) -> &'static str {
        match item.kind {
            ItemKind::Terminal { .. } => "terminal",
            ItemKind::Window { .. } => "window",
            ItemKind::Display { .. } => "display",
            ItemKind::Note { .. } => "note",
            ItemKind::File { .. } => "file",
        }
    }

    /// What a card's title bar says without a name: the shell's title, the window's, "display
    /// N", a note's first line, a file's `name · parent`.
    fn derived_title(&self, item: &CanvasItem, cx: &App) -> String {
        match &item.kind {
            ItemKind::Terminal { session } => self.terminal_title(*session, cx),
            ItemKind::Window { window } => {
                self.titles.get(&item.id).cloned().unwrap_or_else(|| format!("window {}", window.0))
            }
            ItemKind::Display { display } => format!("display {display}"),
            ItemKind::Note { text } => note_title(text),
            ItemKind::File { path } => file_title(path),
        }
    }

    /// What a card's title bar says: the name the human gave it, else `Self::derived_title`.
    #[must_use]
    pub fn card_title(&self, item: &CanvasItem, cx: &App) -> String {
        item.name.clone().unwrap_or_else(|| self.derived_title(item, cx))
    }

    /// ⌘E (or a double-click on a title bar): name the active card. The field in its title
    /// bar starts with the current name and shows the derived title as its placeholder; ↩
    /// keeps what is typed (blank clears the name), Esc or a click elsewhere leaves it.
    pub fn rename_item(&mut self, _: &RenameItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.start_rename(id, window, cx);
        }
    }

    fn start_rename(&mut self, id: ItemId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.doc.get(id).cloned() else { return };
        let placeholder = self.derived_title(&item, cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(item.name.clone().unwrap_or_default())
        });
        let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::PressEnter { .. } => this.finish_rename(true, true, cx),
            // A click elsewhere: the field goes, the name stays, whoever was clicked keeps
            // the keyboard.
            InputEvent::Blur => this.finish_rename(false, false, cx),
            InputEvent::Change | InputEvent::Focus => {}
        });
        let return_to = window.focused(cx);
        self.activate(id, cx);
        self.rename = Some(Rename { id, input, return_to, _subscription: subscription });
        self.pending_focus_rename = true;
        cx.notify();
    }

    /// The name field closes. `keep` writes its text into the document as the card's name
    /// (blank clears it); `back` gives the keyboard back to whoever had it before the field
    /// (after the frame, not from inside the input's own event).
    fn finish_rename(&mut self, keep: bool, back: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else { return };
        if keep && let Some(mut item) = self.doc.get(rename.id).cloned() {
            let text = rename.input.read(cx).value().trim().to_owned();
            item.name = (!text.is_empty()).then_some(text);
            self.propose(CanvasOp::Upsert(item));
        }
        if back {
            self.rename_return = rename.return_to.or_else(|| Some(self.focus.clone()));
        }
        cx.notify();
    }

    /// ⌘⇧P: the command palette over whatever has the keyboard; the choice runs once it is
    /// gone and the focus is back.
    pub fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let items = self.palette_lines(cx);
        let theme = self.theme.clone();
        let palette = cx.new(|cx| CommandPalette::new(items, theme, window, cx));
        self.show_palette(palette, window, cx);
    }

    /// ⌘⇧F: the palette as a find in every card. What is typed goes to every live terminal
    /// as a search; the cards it is found in are the lines, newest hit counts first as they
    /// answer, and ↩ reveals that card with its find bar on the needle.
    pub fn find_everywhere(
        &mut self,
        _: &FindEverywhere,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let seed = match self.active_needle(cx) {
            own if own.is_empty() => self.last_find.clone(),
            own => own,
        };
        let palette = cx.new(|cx| CommandPalette::find(&seed, theme, window, cx));
        self.find_needle = Some(String::new());
        self.find_hits.clear();
        self.show_palette(palette, window, cx);
        if !seed.is_empty() {
            self.find_changed(&seed, cx);
        }
    }

    /// The active card's own find-bar needle, to start a find in every card from: what was
    /// sought in one card is what is sought in all of them. Empty with no bar open (the last
    /// find in every card stands in then).
    fn active_needle(&self, cx: &App) -> String {
        let Some(active) = self.active.and_then(|id| self.doc.get(id)) else {
            return String::new();
        };
        let needle = match &active.kind {
            ItemKind::Terminal { session } => self
                .terminals
                .get(session)
                .and_then(|v| v.read(cx).search_needle().map(str::to_owned)),
            ItemKind::File { .. } => self
                .files
                .get(&active.id)
                .and_then(|v| v.read(cx).search_needle().map(str::to_owned)),
            ItemKind::Note { .. } | ItemKind::Window { .. } | ItemKind::Display { .. } => None,
        };
        needle.unwrap_or_default()
    }

    fn show_palette(
        &mut self,
        palette: Entity<CommandPalette>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_return = window.focused(cx);
        self.subscriptions.push(cx.subscribe(&palette, |this, _palette, event, cx| {
            if let PaletteEvent::Changed(text) = event {
                if this.find_needle.is_some() {
                    this.find_changed(text, cx);
                } else {
                    this.palette_changed(text);
                }
                return;
            }
            this.palette = None;
            this.find_needle = None;
            this.find_hits.clear();
            match event {
                PaletteEvent::Run(PaletteRun::Action(action)) => {
                    this.palette_action = Some(action.boxed_clone());
                }
                PaletteEvent::Run(PaletteRun::Session(session)) => {
                    // The terminal takes the keyboard, not whoever had it before.
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                }
                PaletteEvent::Run(PaletteRun::Item(item)) => this.go_to(*item, cx),
                PaletteEvent::Run(PaletteRun::Follow(client)) => this.follow(*client, cx),
                PaletteEvent::Run(PaletteRun::OpenFile { path, line }) => {
                    let path = this.absolute_in_active_shell(path);
                    this.open_file(&path, *line, cx);
                }
                PaletteEvent::Run(PaletteRun::OpenShell { cwd }) => {
                    this.open_session_in(Some(cwd.clone()), Vec::new(), None, cx);
                }
                PaletteEvent::Run(PaletteRun::OpenAgent { cwd }) => {
                    this.open_session_in(
                        Some(cwd.clone()),
                        vec![AGENT_COMMAND.to_owned()],
                        Some(AGENT_COMMAND.to_owned()),
                        cx,
                    );
                }
                PaletteEvent::Run(PaletteRun::FindIn { session, needle }) => {
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                    this.pending_find = Some((*session, needle.clone()));
                }
                PaletteEvent::Run(PaletteRun::Rerun { session, command }) => {
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                    if let Some(view) = this.terminals.get(session).cloned() {
                        let command = command.clone();
                        view.update(cx, |v, cx| v.run_text(command, cx));
                    }
                }
                PaletteEvent::Run(PaletteRun::FindInFile { item, needle }) => {
                    this.palette_return = None;
                    this.go_to(*item, cx);
                    this.pending_find_file = Some((*item, needle.clone()));
                }
                PaletteEvent::Dismiss | PaletteEvent::Changed(_) => {}
            }
            cx.notify();
        }));
        self.pending_focus_palette = true;
        self.palette = Some(palette);
        cx.notify();
    }

    /// The find-everywhere field changed: every live shell is asked for the needle (one hit
    /// each is enough: the count is what the line says); the notes and file cards are counted
    /// here, where their text is; and the lines start over.
    fn find_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        let needle = text.trim().to_owned();
        self.find_needle = Some(needle.clone());
        self.find_hits.clear();
        if needle.is_empty() {
            self.refresh_find_lines(cx);
            return;
        }
        needle.clone_into(&mut self.last_find);
        for id in self.reading_order() {
            let Some(item) = self.doc.get(id) else { continue };
            let (total, run) = match &item.kind {
                ItemKind::Terminal { session } => {
                    if !self.terminals.contains_key(session) {
                        continue;
                    }
                    self.send(ClientMsg::Term {
                        session: *session,
                        req: TermRequest::Search { needle: needle.clone(), max: 1, regex: false },
                    });
                    continue;
                }
                ItemKind::Note { text } => {
                    let lines: Vec<SharedString> = text.lines().map(SharedString::from).collect();
                    (crate::file::find_hits(&lines, &needle).len(), PaletteRun::Item(id))
                }
                ItemKind::File { .. } => {
                    let Some(view) = self.files.get(&id) else { continue };
                    let total = crate::file::find_hits(view.read(cx).lines(), &needle).len();
                    (total, PaletteRun::FindInFile { item: id, needle: needle.clone() })
                }
                ItemKind::Window { .. } | ItemKind::Display { .. } => continue,
            };
            if let Ok(total) = u32::try_from(total) {
                self.find_hits.insert(id, (total, run));
            }
        }
        self.refresh_find_lines(cx);
    }

    /// A shell answered the find-everywhere needle: its line says how many hits it holds.
    fn find_answered(
        &mut self,
        session: SessionId,
        needle: &str,
        total: u32,
        cx: &mut Context<Self>,
    ) {
        if self.find_needle.as_deref() != Some(needle) || needle.is_empty() {
            return;
        }
        if !self.terminals.contains_key(&session) {
            return;
        }
        let Some(item) = self.doc.item_for_session(session) else { return };
        let run = PaletteRun::FindIn { session, needle: needle.to_owned() };
        self.find_hits.insert(item.id, (total, run));
        self.refresh_find_lines(cx);
    }

    /// The find-everywhere lines: the cards with a hit, in reading order, as they are known.
    fn refresh_find_lines(&self, cx: &mut Context<Self>) {
        if self.find_needle.is_none() {
            return;
        }
        let lines: Vec<PaletteItem> = self
            .reading_order()
            .into_iter()
            .filter_map(|id| {
                let (total, run) = self.find_hits.get(&id)?;
                let item = self.doc.get(id)?;
                (*total > 0)
                    .then(|| PaletteItem::hits(&self.card_title(item, cx), *total, run.clone()))
            })
            .collect();
        if let Some(palette) = &self.palette {
            palette.update(cx, |p, cx| p.set_lines(lines, cx));
        }
    }

    /// The palette's field changed: a word worth a lookup is asked of the host's files under
    /// the active shell's directory, or the host's home when no shell is active.
    fn palette_changed(&self, text: &str) {
        if let Some(query) = palette::files_query(text) {
            let root = self.active_cwd().unwrap_or_else(|| "~".to_owned());
            self.send(ClientMsg::FindFiles { root, query: query.to_owned() });
        }
    }

    /// The host found files for the palette's text: they are its `Open <path>` lines.
    pub fn files_found(&self, root: &str, query: &str, paths: &[String], cx: &mut Context<Self>) {
        if let Some(palette) = &self.palette {
            palette.update(cx, |p, cx| p.set_found(root, query, paths, cx));
        }
    }

    /// Whether the palette is up.
    #[must_use]
    pub const fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    fn show_picker(
        &mut self,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let sessions = self.session_rows(cx);
        let picker = cx.new(|cx| WindowPicker::new(sessions, windows, displays, theme, cx));
        self.subscriptions.push(cx.subscribe(&picker, |this, _picker, event, cx| {
            match event {
                PickerEvent::Pick { target, size, title } => {
                    this.add_screen_item(*target, *size, title.clone());
                }
                PickerEvent::Jump(session) => this.reveal_session(*session, cx),
                PickerEvent::Dismiss => {}
            }
            this.picker = None;
            // The jump focuses its terminal; every other outcome hands focus back to the canvas.
            this.pending_focus_self = !matches!(event, PickerEvent::Jump(_));
            cx.notify();
        }));
        self.pending_focus_picker = true;
        self.picker = Some(picker);
    }

    /// The terminal sessions on the canvas for the picker: agents waiting on the human first,
    /// then other agents, then plain shells; ties in reading order.
    fn session_rows(&self, cx: &Context<Self>) -> Vec<SessionRow> {
        let mut rows: Vec<(u8, Rect, SessionRow)> = self
            .doc
            .items()
            .filter_map(|i| match i.kind {
                ItemKind::Terminal { session } => Some((i, session)),
                _ => None,
            })
            .map(|(i, session)| {
                let rect = i.rect;
                let agent = self.agents.get(&session);
                let needs_you = agent.is_some_and(needs_human);
                let rank = match agent {
                    _ if needs_you => 0,
                    Some(_) => 1,
                    None => 2,
                };
                let row = SessionRow {
                    session,
                    title: self.card_title(i, cx),
                    status: agent.map(agent_status_text),
                    needs_you,
                };
                (rank, rect, row)
            })
            .collect();
        rows.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.y.total_cmp(&b.1.y))
                .then_with(|| a.1.x.total_cmp(&b.1.x))
        });
        rows.into_iter().map(|(_, _, row)| row).collect()
    }

    /// What a terminal's title bar says: the program's title, else the session's, else "shell".
    #[must_use]
    pub fn terminal_title(&self, session: SessionId, cx: &App) -> String {
        self.terminals
            .get(&session)
            .and_then(|v| v.read(cx).state().title().map(str::to_owned))
            .or_else(|| self.sessions.get(&session).map(|s| s.title.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "shell".to_owned())
    }

    /// The host window changed size: keep the item's width and give it the new aspect, so the
    /// picture is never stretched and pointer mapping stays exact.
    fn follow_geometry(
        &mut self,
        stream: StreamId,
        width: u32,
        height: u32,
        cx: &mut Context<Self>,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let ids: Vec<ItemId> = self
            .screens
            .iter()
            .filter(|(_, v)| v.read(cx).stream() == stream)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(view) = self.screens.get(&id) {
                view.update(cx, |v, _| v.set_geometry(width, height));
            }
            let Some(old) = self.doc.get(id).map(|i| i.rect) else { continue };
            #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
            let aspect = height as f32 / width as f32;
            let mut rect = old;
            rect.h = snap(rect.w.mul_add(aspect, TITLE_H));
            if (rect.h - old.h).abs() >= 1.0 {
                self.propose(CanvasOp::Place { id, rect });
            }
        }
    }

    /// Put a window/display item on the canvas; `reconcile` opens its stream.
    fn add_screen_item(&mut self, target: CaptureTarget, size: (f32, f32), title: String) {
        let (mut w, mut h) = (size.0.max(160.0), size.1.max(120.0));
        // Its own shape, no larger than the cap and, on a phone, than the viewport.
        let (max_w, max_h) = self.fitted(MAX_PICKED);
        let shrink = (max_w / w).min((max_h - TITLE_H) / h).min(1.0);
        w = snap(w * shrink);
        h = snap(h.mul_add(shrink, TITLE_H));
        let rect = self.doc.free_slot((w, h));
        let kind = match target {
            CaptureTarget::Window(window) => ItemKind::Window { window },
            CaptureTarget::Display(display) => ItemKind::Display { display },
        };
        let id = ItemId::new();
        self.titles.insert(id, title);
        let item = CanvasItem {
            id,
            kind,
            rect,
            z: self.doc.top_z().saturating_add(1),
            group: None,
            sleeping: false,
            name: None,
        };
        self.propose(CanvasOp::Upsert(item));
        self.active = Some(id);
    }

    /// A session-stream event.
    pub fn term_event(&mut self, session: SessionId, event: TermEvent, cx: &mut Context<Self>) {
        if let TermEvent::Matches { needle, total, .. } = &event
            && self.find_needle.is_some()
        {
            let needle = needle.clone();
            self.find_answered(session, &needle, *total, cx);
        }
        match (self.terminals.get(&session), event) {
            (Some(view), event) => view.update(cx, |v, cx| v.apply(event, cx)),
            (None, TermEvent::Error(e)) => tracing::warn!(%session, error = %e, "host"),
            (None, _other) => {}
        }
    }

    /// Create views for terminal items whose session is alive; drop views whose item is gone.
    /// A card closed within [`UNDO_CLOSE`] has no item; a shell among them keeps its view,
    /// attached, and drops out of the stack the moment its session ends on its own.
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        self.closed.retain(|c| c.session.is_none_or(|s| self.sessions.contains_key(&s)));
        let wanted: Vec<SessionId> = self
            .doc
            .items()
            .filter_map(|i| match i.kind {
                ItemKind::Terminal { session } if !i.sleeping => Some(session),
                _ => None,
            })
            .chain(self.closed.iter().filter_map(|c| c.session))
            .filter(|s| self.sessions.contains_key(s))
            .collect();
        for session in &wanted {
            if self.terminals.contains_key(session) {
                continue;
            }
            let summary = self.sessions.get(session);
            let size = summary.map_or_else(TermSize::default, |s| TermSize {
                cols: s.cols,
                rows: s.rows,
                ..TermSize::default()
            });
            let out = self.out.clone();
            let theme = self.theme.clone();
            let view = cx.new(|cx| TerminalView::new(*session, size, out, theme, cx));
            let sid = *session;
            self.subscriptions.push(cx.subscribe(
                &view,
                move |this, _view, event, cx| match event {
                    TerminalViewEvent::Bell => cx.emit(CanvasEvent::Bell(sid)),
                    TerminalViewEvent::Notification { title, body } => {
                        this.notify_program(sid, title, body, cx);
                    }
                    TerminalViewEvent::Exited(_) => {
                        this.send(ClientMsg::Term { session: sid, req: TermRequest::Close });
                    }
                    TerminalViewEvent::CloseConfirmed => this.close_shell(sid, cx),
                    TerminalViewEvent::Title(_) => cx.notify(),
                    TerminalViewEvent::Cwd { path, repo } => {
                        this.session_moved(sid, path.clone(), repo.clone());
                    }
                    TerminalViewEvent::Notice(text) => cx.emit(CanvasEvent::Notice(text.clone())),
                    TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                        let done =
                            Finished { command: command.clone(), exit: *exit, elapsed: *elapsed };
                        this.command_finished(sid, done, cx);
                    }
                    TerminalViewEvent::NoteBlock(text) => this.note_beside(sid, text.clone(), cx),
                    TerminalViewEvent::ViewFile { path, line } => {
                        let path = this.absolute_in_session(sid, path);
                        this.open_file(&path, *line, cx);
                    }
                },
            ));
            self.send(ClientMsg::Term { session: *session, req: TermRequest::Attach { size } });
            // What this client paints with, so the driver's colours answer colour queries.
            self.send(ClientMsg::Term {
                session: *session,
                req: TermRequest::Colors(self.theme.terminal.wire()),
            });
            // A view born after the host reported the agent (a woken item) starts with its state.
            if let Some(agent) = self.agents.get(session) {
                let status = agent.status.clone();
                view.update(cx, |v, cx| v.set_agent_status(Some(status), cx));
            }
            self.terminals.insert(*session, view);
            if self.active.is_none() {
                self.active = self.doc.item_for_session(*session).map(|i| i.id);
            }
        }
        let gone: Vec<SessionId> =
            self.terminals.keys().filter(|s| !wanted.contains(s)).copied().collect();
        for session in gone {
            if self.sessions.contains_key(&session) {
                self.send(ClientMsg::Term { session, req: TermRequest::Detach });
            }
            self.terminals.remove(&session);
        }
        self.reconcile_screens();
        if let Some(active) = self.active
            && self.doc.get(active).is_none()
        {
            // The active item held the keyboard and is gone (its session closed, a peer
            // removed it): the topmost item becomes active and the canvas takes the keyboard
            // back on its next frame (and hands it to that item when it is a live terminal),
            // so the next shortcut still lands instead of dying on a handle nobody draws.
            self.active = self.doc.by_z().last().map(|i| i.id);
            self.pending_focus_self = true;
        }
        self.prune_headings();
        self.update_run_targets(cx);
    }

    /// Drop the headings whose block has lost every item. An emptied repository must not keep
    /// a label on the canvas, nor a rectangle ⌘1 would fit around nothing.
    fn prune_headings(&mut self) {
        let doc = &self.doc;
        self.headings.retain(|h| h.items.iter().any(|id| doc.get(*id).is_some()));
    }

    /// Open streams for window/display items that lack one; drop views whose item is gone.
    /// Window items restored from the document have no title until a `Listing` names them.
    fn fill_titles(&mut self, windows: &[WindowInfo]) {
        self.titles_requested = false;
        for item in self.doc.items() {
            let ItemKind::Window { window } = item.kind else { continue };
            if self.titles.contains_key(&item.id) {
                continue;
            }
            if let Some(info) = windows.iter().find(|w| w.id == window) {
                let title =
                    if info.title.is_empty() { info.app.clone() } else { info.title.clone() };
                self.titles.insert(item.id, title);
            }
        }
    }

    fn reconcile_screens(&mut self) {
        let untitled = self
            .doc
            .items()
            .any(|i| matches!(i.kind, ItemKind::Window { .. }) && !self.titles.contains_key(&i.id));
        if untitled && !self.titles_requested {
            self.titles_requested = true;
            self.send(ClientMsg::Screen(ScreenRequest::List));
        }
        let wanted: Vec<(ItemId, CaptureTarget)> = self
            .doc
            .items()
            .filter(|i| !i.sleeping)
            .filter_map(|i| match i.kind {
                ItemKind::Window { window } => Some((i.id, CaptureTarget::Window(window))),
                ItemKind::Display { display } => Some((i.id, CaptureTarget::Display(display))),
                ItemKind::Terminal { .. } | ItemKind::Note { .. } | ItemKind::File { .. } => None,
            })
            .collect();
        for &(id, target) in &wanted {
            if self.screens.contains_key(&id) || self.pending_opens.values().any(|&p| p == id) {
                continue;
            }
            if self.pending_opens.contains_key(&target) {
                continue;
            }
            let quality = self.quality_for();
            self.pending_opens.insert(target, id);
            self.send(ClientMsg::Screen(ScreenRequest::Open { target, quality }));
        }
        let gone: Vec<ItemId> = self
            .screens
            .keys()
            .filter(|id| !wanted.iter().any(|(w, _)| w == *id))
            .copied()
            .collect();
        for id in gone {
            // Dropping the view sends `Close` for its stream.
            self.screens.remove(&id);
            self.titles.remove(&id);
        }
        self.pending_opens.retain(|_, id| wanted.iter().any(|(w, _)| w == id));
    }

    fn send(&self, msg: ClientMsg) {
        if let Err(e) = self.out.try_send(msg) {
            tracing::warn!(error = %e, "outbound queue");
        }
    }

    fn propose(&mut self, op: CanvasOp) {
        self.doc.apply_op(&op);
        self.send(ClientMsg::Canvas(op));
    }

    // ----- commands ------------------------------------------------------------------------

    /// ⌘F with the canvas (not a terminal) focused: search in the active terminal, or in the
    /// active file card.
    pub fn find_in_active(
        &mut self,
        action: &crate::terminal::Find,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.active_terminal() {
            view.update(cx, |view, cx| view.find(action, window, cx));
        } else if let Some(view) = self.active.and_then(|id| self.files.get(&id)).cloned() {
            view.update(cx, |view, cx| view.find(window, cx));
        }
    }

    /// Open a new shell; the host places it.
    pub fn new_terminal(&mut self, _: &NewTerminal, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_session(Vec::new(), None, cx);
    }

    /// Where a session opened from the keyboard starts: the active terminal's directory when
    /// there is one (a shell beside a shell belongs to the same work), else the host's default.
    fn active_cwd(&self) -> Option<String> {
        self.active.and_then(|id| self.doc.get(id)).and_then(|item| self.cwd_of(item))
    }

    /// ⌘⇧T: a terminal running Claude Code. The bare name resolves on the host through the
    /// user's login shell, so `claude` is found wherever their rc files put it (or alias it).
    pub fn new_agent(&mut self, _: &NewAgent, _window: &mut Window, cx: &mut Context<Self>) {
        self.open_session(vec![AGENT_COMMAND.to_owned()], Some(AGENT_COMMAND.to_owned()), cx);
    }

    /// Open a session running `command` (the login shell when empty), titled after its
    /// program; the host places it. The self-test socket's way to put load on the canvas.
    pub fn open_command(&self, command: Vec<String>, cx: &mut Context<Self>) {
        let title = command.first().cloned();
        self.open_session(command, title, cx);
    }

    fn open_session(&self, command: Vec<String>, title: Option<String>, cx: &mut Context<Self>) {
        self.open_session_in(self.active_cwd(), command, title, cx);
    }

    /// Open a session in `cwd` (the host's default when `None`; `~` is the host's home).
    fn open_session_in(
        &self,
        cwd: Option<String>,
        command: Vec<String>,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(?command, ?cwd, "open session");
        self.send(ClientMsg::OpenSession(OpenSession {
            size: TermSize::default(),
            cwd,
            command,
            env: Vec::new(),
            title,
            attach: false,
        }));
        cx.notify();
    }

    /// ⌘⇧N: an empty note in the next free slot, revealed and focused right away (the
    /// document applies our op optimistically; the host's echo changes nothing).
    pub fn new_note(&mut self, _: &NewNote, _window: &mut Window, cx: &mut Context<Self>) {
        let rect = self.doc.free_slot(self.fitted(NOTE_SIZE));
        let id = ItemId::new();
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text: String::new() },
            rect,
            z: self.doc.top_z().saturating_add(1),
            group: None,
            sleeping: false,
            name: None,
        };
        self.propose(CanvasOp::Upsert(item));
        self.active = Some(id);
        self.reveal_pending = Some(id);
        self.pending_focus_note = Some(id);
        cx.notify();
    }

    /// A block saved as a note: a note card with `text`, in the free slot beside the shell's
    /// card (the canvas's next free slot without one), active and revealed but not editing,
    /// since its content is what was saved, not what is about to be typed.
    fn note_beside(&mut self, session: SessionId, text: String, cx: &mut Context<Self>) {
        let size = self.fitted(NOTE_SIZE);
        let rect = self.doc.item_for_session(session).map_or_else(
            || self.doc.free_slot(size),
            |shell| {
                let mut rect = Rect { x: shell.rect.x + shell.rect.w + GAP, ..shell.rect };
                rect.w = size.0;
                rect.h = size.1.min(shell.rect.h);
                if self.doc.items().any(|i| overlaps(i.rect, rect)) {
                    self.doc.free_slot(size)
                } else {
                    rect
                }
            },
        );
        let id = ItemId::new();
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text },
            rect,
            z: self.doc.top_z().saturating_add(1),
            group: None,
            sleeping: false,
            name: None,
        };
        self.propose(CanvasOp::Upsert(item));
        self.active = Some(id);
        self.reveal_pending = Some(id);
        cx.notify();
    }

    /// A note's editor settled: write its text into the document.
    fn commit_note(&mut self, id: ItemId, text: String) {
        let Some(mut item) = self.doc.get(id).cloned() else { return };
        item.kind = ItemKind::Note { text };
        self.propose(CanvasOp::Upsert(item));
    }

    /// Create editors for note items and drop the ones whose items are gone. Needs the window
    /// (the editor state does), so it runs from `render`.
    fn reconcile_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let notes: Vec<(ItemId, String)> = self
            .doc
            .items()
            .filter_map(|i| match &i.kind {
                ItemKind::Note { text } => Some((i.id, text.clone())),
                _ => None,
            })
            .collect();
        for (id, text) in &notes {
            if let Some(view) = self.notes.get(id) {
                let editing = view.read(cx).editing(window, cx);
                if !editing && view.read(cx).synced() != text {
                    view.update(cx, |v, cx| v.set_text(text, window, cx));
                }
                continue;
            }
            let theme = self.theme.clone();
            let view = cx.new(|cx| NoteView::new(*id, text, theme, window, cx));
            let item = *id;
            self.subscriptions.push(cx.subscribe(&view, move |this, _view, event, cx| {
                match event {
                    NoteViewEvent::Commit(text) => this.commit_note(item, text.clone()),
                    NoteViewEvent::Run(code) => this.run_in_shell(code.clone(), cx),
                }
                cx.notify();
            }));
            view.update(cx, |n, cx| n.set_can_run(self.run_target().is_some(), cx));
            self.notes.insert(*id, view);
        }
        self.notes.retain(|id, _| notes.iter().any(|(n, _)| n == id));
    }

    /// A file card for `path` on the host: an existing card for it is revealed, else a new
    /// one opens in the next free slot and asks the host for the text (the item goes into the
    /// shared document; every client reads the file for itself). `line` (1-based) is where
    /// the card lands: an edit's place in the file.
    pub fn open_file(&mut self, path: &str, line: Option<u32>, cx: &mut Context<Self>) {
        let existing = self.doc.items().find_map(|i| match &i.kind {
            ItemKind::File { path: p } if p == path => Some(i.id),
            _ => None,
        });
        let id = if let Some(id) = existing {
            self.request_file(id);
            id
        } else {
            let rect = self.doc.free_slot(self.fitted(FILE_SIZE));
            let id = ItemId::new();
            let item = CanvasItem {
                id,
                kind: ItemKind::File { path: path.to_owned() },
                rect,
                z: self.doc.top_z().saturating_add(1),
                group: None,
                sleeping: false,
                name: None,
            };
            tracing::info!(%id, %path, ?line, "open file card");
            self.propose(CanvasOp::Upsert(item));
            id
        };
        match self.files.get(&id) {
            Some(view) => view.update(cx, |v, cx| v.focus_line(line, cx)),
            // The view is made on the next frame; it lands there then.
            None => {
                if let Some(line) = line {
                    self.file_focus.insert(id, line);
                }
            }
        }
        self.active = Some(id);
        self.reveal_pending = Some(id);
        cx.notify();
    }

    /// `path` made absolute against the session's directory as the host last reported it
    /// (OSC 7, else where it started); a path with no directory known stays as it is.
    fn absolute_in_session(&self, session: SessionId, path: &str) -> String {
        if path.starts_with('/') {
            return path.to_owned();
        }
        match self.sessions.get(&session).and_then(|s| s.cwd.as_deref()) {
            Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
            None => path.to_owned(),
        }
    }

    /// `path` made absolute against the active shell's directory, when it is a shell; `~`
    /// is left for the host, whose home it names.
    fn absolute_in_active_shell(&self, path: &str) -> String {
        let session = self.active.and_then(|id| self.doc.get(id)).and_then(|i| match i.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        });
        match session {
            Some(session) if !path.starts_with('~') => self.absolute_in_session(session, path),
            _ => path.to_owned(),
        }
    }

    /// ↑/↓/⇞/⇟/Home/End with a file card active: its reading line moves. Nothing while an
    /// overlay (the palette, the picker) has the keys.
    fn move_file_line(&self, mv: LineMove, cx: &mut Context<Self>) {
        if self.palette.is_some() || self.picker.is_some() {
            return;
        }
        if let Some(view) = self.active.and_then(|id| self.files.get(&id)).cloned() {
            view.update(cx, |v, cx| v.move_line(mv, cx));
        }
    }

    /// The card's "edit" pill: the file in `$EDITOR` in the shell the human was last in, at
    /// the line being read (the current find hit, else the line the card opened at).
    fn edit_file(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(ItemKind::File { path }) = self.doc.get(id).map(|i| i.kind.clone()) else {
            return;
        };
        let line = self.files.get(&id).and_then(|v| v.read(cx).reading_line());
        self.run_in_shell(crate::terminal::url::editor_command(&path, line), cx);
    }

    /// Ask the host for a file card's text (again).
    fn request_file(&self, id: ItemId) {
        if let Some(ItemKind::File { path }) = self.doc.get(id).map(|i| &i.kind) {
            self.send(ClientMsg::ReadFile { path: path.clone() });
        }
    }

    /// The host read a file: every card for that path shows it.
    pub fn file_read(&self, path: &str, read: &FileRead, cx: &mut Context<Self>) {
        for view in self.files.values() {
            if view.read(cx).path() == path {
                view.update(cx, |v, cx| v.set_read(read.clone(), cx));
            }
        }
    }

    /// Create views for file items and ask the host for their text; drop the views whose
    /// items are gone. Runs from `render`, as the notes do.
    fn reconcile_files(&mut self, cx: &mut Context<Self>) {
        let files: Vec<(ItemId, String)> = self
            .doc
            .items()
            .filter_map(|i| match &i.kind {
                ItemKind::File { path } => Some((i.id, path.clone())),
                _ => None,
            })
            .collect();
        for (id, path) in &files {
            if self.files.contains_key(id) {
                continue;
            }
            let view = cx.new(|_cx| FileView::new(*id, path, self.theme.clone()));
            self.subscriptions.push(cx.subscribe(&view, |this, _view, event, cx| match event {
                FileViewEvent::FindClosed => {
                    this.pending_focus_self = true;
                    cx.notify();
                }
            }));
            if let Some(line) = self.file_focus.remove(id) {
                view.update(cx, |v, cx| v.focus_line(Some(line), cx));
            }
            self.files.insert(*id, view);
            self.send(ClientMsg::ReadFile { path: path.clone() });
        }
        self.files.retain(|id, _| files.iter().any(|(f, _)| f == id));
        // The host watches the set behind the cards and re-reads one that changes on disk.
        let mut paths: Vec<String> = files.into_iter().map(|(_, path)| path).collect();
        paths.sort_unstable();
        paths.dedup();
        if paths != self.watched {
            self.watched.clone_from(&paths);
            self.send(ClientMsg::WatchFiles { paths });
        }
    }

    /// The file cards on the canvas, for tests and the self-test dump.
    #[must_use]
    pub fn file(&self, id: ItemId) -> Option<&Entity<FileView>> {
        self.files.get(&id)
    }

    /// ⌘O: ask the host for its windows, then show the picker (which also lists the canvas's
    /// sessions, agents first, to jump to).
    pub fn add_window(&mut self, _: &AddWindow, _window: &mut Window, cx: &mut Context<Self>) {
        self.picker_wanted = true;
        self.send(ClientMsg::Screen(ScreenRequest::List));
        cx.notify();
    }

    /// Add the host's first display as an item, as picking it in the picker would.
    pub fn add_first_display(&mut self, cx: &mut Context<Self>) {
        self.display_wanted = true;
        self.send(ClientMsg::Screen(ScreenRequest::List));
        cx.notify();
    }

    /// Close the active item.
    pub fn close_item(&mut self, _: &CloseItem, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active else { return };
        let Some(item) = self.doc.get(id).cloned() else { return };
        tracing::debug!(%id, kind = ?item.kind, "close item");
        match item.kind {
            // A live session closes through the host, which removes the item; an ended one
            // has nothing to close, so drop the item straight from the document. A shell
            // whose command still runs asks first (its view's bar), and closes on its
            // `CloseConfirmed`.
            ItemKind::Terminal { session } if self.sessions.contains_key(&session) => {
                let asked = self
                    .terminals
                    .get(&session)
                    .is_some_and(|view| view.update(cx, TerminalView::ask_close));
                if !asked {
                    self.close_shell(session, cx);
                }
            }
            // An ended shell has no session to keep and no rows the host could replay, so
            // it goes for good; every other card is its item, and comes back from it.
            ItemKind::Terminal { .. } => self.propose(CanvasOp::Remove(id)),
            // A note's editor commits on a timer, so the last keystrokes may not be in the
            // document yet; take them from the field, or the undo would give back a note
            // missing the line that was just typed.
            ItemKind::Note { .. } => {
                let mut item = item;
                if let Some(text) = self.notes.get(&id).map(|view| view.read(cx).live_text(cx)) {
                    item.kind = ItemKind::Note { text };
                }
                self.remember_closed(item, None, cx);
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::File { .. } => {
                self.remember_closed(item, None, cx);
            }
        }
        cx.notify();
    }

    /// Take a live shell off the canvas, its session kept for [`UNDO_CLOSE`]: ⌘Z (or the
    /// toast) puts it back as it was, with its view; otherwise the host closes it then. A
    /// shell without a view has nothing to keep and closes at once.
    fn close_shell(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let item = self.doc.item_for_session(session).cloned();
        let (Some(item), true) = (item, self.terminals.contains_key(&session)) else {
            self.send(ClientMsg::Term { session, req: TermRequest::Close });
            return;
        };
        self.remember_closed(item, Some(session), cx);
    }

    /// Take a card off the canvas and offer it back for [`UNDO_CLOSE`]: the toast's button
    /// or ⌘Z puts it where it was, and the timer forgets it.
    fn remember_closed(
        &mut self,
        item: CanvasItem,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        self.closed_seq = self.closed_seq.wrapping_add(1);
        let seq = self.closed_seq;
        let title = self.card_title(&item, cx);
        let id = item.id;
        self.closed.push(ClosedCard { item, session, seq });
        self.propose(CanvasOp::Remove(id));
        self.reconcile(cx);
        self.show_toast_for(ToastKind::Closed { seq, title }, UNDO_CLOSE, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_CLOSE).await;
            let _gone = this.update(cx, |this, cx| this.forget_closed(seq, cx));
        })
        .detach();
    }

    /// [`UNDO_CLOSE`] passed for closing `seq`: a shell's session is closed by the host and
    /// its view goes; every other card was already off the canvas and is simply forgotten.
    fn forget_closed(&mut self, seq: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.closed.iter().position(|c| c.seq == seq) else { return };
        let closed = self.closed.remove(ix);
        if let Some(session) = closed.session {
            if self.sessions.contains_key(&session) {
                self.send(ClientMsg::Term { session, req: TermRequest::Close });
            }
            self.terminals.remove(&session);
        }
        if matches!(self.toast, Some(Toast { what: ToastKind::Closed { seq: s, .. }, .. }) if s == seq)
        {
            self.toast = None;
        }
        cx.notify();
    }

    /// ⌘Z: the card closed last comes back where it was, active and focused, a shell's
    /// session untouched. Nothing to take back is nothing.
    pub fn undo_close(&mut self, _: &UndoClose, _window: &mut Window, cx: &mut Context<Self>) {
        self.take_back(None, cx);
    }

    /// Put back the closing `seq` (the toast's), or the latest.
    fn take_back(&mut self, seq: Option<u64>, cx: &mut Context<Self>) {
        let ix = match seq {
            Some(seq) => self.closed.iter().position(|c| c.seq == seq),
            None => self.closed.len().checked_sub(1),
        };
        let Some(ix) = ix else { return };
        let closed = self.closed.remove(ix);
        if matches!(self.toast, Some(Toast { what: ToastKind::Closed { .. }, .. })) {
            self.toast = None;
        }
        tracing::debug!(item = %closed.item.id, session = ?closed.session, "card taken back");
        let id = closed.item.id;
        self.propose(CanvasOp::Upsert(closed.item));
        self.reconcile(cx);
        match closed.session {
            Some(session) => self.reveal_session(session, cx),
            None => self.reveal_item(id, cx),
        }
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.take_camera();
        let (_, vp) = self.viewport;
        self.camera.zoom_at(factor, f32::from(vp.width) / 2.0, f32::from(vp.height) / 2.0);
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    /// ⌘=.
    pub fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    /// ⌘-.
    pub fn zoom_out(&mut self, _: &ZoomOut, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(1.0 / ZOOM_STEP, cx);
    }

    /// ⌘0: back to 100 %, keeping the active item in the middle (or, with nothing active,
    /// whatever the viewport is centred on).
    pub fn zoom_reset(&mut self, _: &ZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        let viewport = self.viewport_size();
        let target = self.active_rect().map_or_else(
            || {
                let mut camera = self.camera;
                camera.zoom_at(1.0 / camera.zoom, viewport.0 / 2.0, viewport.1 / 2.0);
                camera
            },
            |rect| Camera::centred_on(rect, viewport, 1.0),
        );
        self.fly_to(target, cx);
    }

    /// ⌘1.
    pub fn fit_all(&mut self, _: &FitAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.fit_now(cx);
    }

    /// ⌘2: fit the active item, with the padding ⌘1 uses. Nothing active is nothing to do —
    /// ⌘1 is the action for "show me everything".
    pub fn zoom_to_item(&mut self, _: &ZoomToItem, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(rect) = self.active_rect() {
            let target = Camera::fitted([rect], self.viewport_size());
            self.fly_to(target, cx);
        }
    }

    /// The active item's rect, which is what ⌘2 and ⌘0 work on. There is no multi-selection on
    /// the canvas yet; when there is, this is where it joins.
    fn active_rect(&self) -> Option<Rect> {
        self.active.and_then(|id| self.doc.get(id)).map(|item| item.rect)
    }

    /// The viewport in canvas-independent points.
    fn viewport_size(&self) -> (f32, f32) {
        let (_, vp) = self.viewport;
        (f32::from(vp.width), f32::from(vp.height))
    }

    /// The viewport in canvas units, once a frame has measured it.
    fn view_rect(&self) -> Option<Rect> {
        let (w, h) = self.viewport_size();
        if w <= 1.0 || h <= 1.0 {
            return None;
        }
        let (x, y) = self.camera.to_canvas(0.0, 0.0);
        let (right, bottom) = self.camera.to_canvas(w, h);
        Some(Rect { x, y, w: right - x, h: bottom - y })
    }

    /// Tell the host where this client looks once the viewport has rested for `LOOK_EVERY`.
    /// Called every frame; a frame that shows the same viewport as the last message costs
    /// nothing.
    fn note_view(&mut self, cx: &Context<Self>) {
        if self.look_pending || self.view_rect() == self.looked {
            return;
        }
        self.look_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOOK_EVERY).await;
            let _gone = this.update(cx, |this, _cx| {
                this.look_pending = false;
                let view = this.view_rect();
                if view != this.looked {
                    this.looked = view;
                    this.send(ClientMsg::Look { view });
                }
            });
        })
        .detach();
    }

    /// The other clients' viewports: an outline in each one's colour with its name at the
    /// corner, drawn over the items and never in their way (the outline has no listeners); the
    /// name is a button that flies the camera to what that client sees.
    /// A colour for another client, stable across frames and clients: from its id.
    fn looker_colour(&self, client: ClientId) -> slopty_theme::Rgb {
        let theme = &self.theme;
        let palette = [
            theme.surfaces.accent,
            theme.surfaces.success,
            theme.surfaces.warn,
            theme.surfaces.error,
        ];
        let hue = usize::try_from(client.as_uuid().as_u128() % 4).unwrap_or(0);
        palette.get(hue).copied().unwrap_or(theme.surfaces.accent)
    }

    /// Who else is on this canvas: one pill per other client at the top right, named and
    /// tinted like their outline, whether or not their viewport is on this screen. A click
    /// follows them (a second click stops), like the outline's tag.
    fn render_here(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let mut lookers = self.doc.lookers().peekable();
        lookers.peek()?;
        let row = div()
            .id("here")
            .debug_selector(|| "here".to_owned())
            .role(Role::Group)
            .aria_label("Here")
            .absolute()
            .top(px(MINIMAP_MARGIN))
            .right(px(MINIMAP_MARGIN))
            .flex()
            .gap(px(theme.spacing.xs))
            .font_family(theme.typography.ui_family.clone());
        let row = lookers.fold(row, |row, l| {
            let client = l.client;
            let colour = self.looker_colour(client);
            let following = self.following == Some(client);
            let verb = if following { "following" } else { "follow" };
            let pill = div()
                .id(ElementId::from(SharedString::from(format!("here-{client}"))))
                .debug_selector({
                    let name = l.name.clone();
                    move || format!("here-{name}")
                })
                .role(Role::Button)
                .aria_label(format!("{verb} {}, {}", l.name, device_name(l.kind)))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .bg(if following {
                    hsla_alpha(colour, 1.0)
                } else {
                    hsla_alpha(theme.surfaces.panel, alpha::MINIMAP)
                })
                .border_1()
                .border_color(hsla_alpha(colour, alpha::LOOKER_TAG))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(if following {
                    theme.surfaces.accent_fg
                } else {
                    theme.surfaces.text
                }))
                .cursor_pointer()
                .child(div().flex_none().size(px(theme.spacing.sm)).rounded_full().bg(hsla(colour)))
                .child(SharedString::from(l.name.clone()));
            row.child(tab_stop(pill, theme.surfaces.accent).on_click(cx.listener(
                move |this, _ev, _w, cx| {
                    if this.following == Some(client) {
                        this.following = None;
                        cx.notify();
                    } else {
                        this.follow(client, cx);
                    }
                },
            )))
        });
        Some(row.into_any_element())
    }

    fn render_lookers(&self, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        self.doc
            .lookers()
            .filter(|l| self.on_screen(l.view))
            .map(|l| {
                let s = self.camera.to_screen(l.view);
                let colour = self.looker_colour(l.client);
                let label = SharedString::from(l.name.clone());
                let client = l.client;
                let following = self.following == Some(client);
                let verb = if following { "following" } else { "follow" };
                let tag = div()
                    .id(ElementId::from(SharedString::from(format!("follow-{}", l.client))))
                    .debug_selector({
                        let name = l.name.clone();
                        move || format!("follow-{name}")
                    })
                    .role(Role::Button)
                    .aria_label(format!("{verb} {}", l.name))
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .px(px(theme.spacing.xs))
                    .bg(hsla_alpha(colour, if following { 1.0 } else { alpha::LOOKER_TAG }))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(theme.surfaces.accent_fg))
                    .font_family(theme.typography.ui_family.clone())
                    .cursor_pointer()
                    .child(label.clone());
                let tag = tab_stop(tag, theme.surfaces.accent).on_click(cx.listener(
                    move |this, _ev, _w, cx| {
                        if this.following == Some(client) {
                            this.following = None;
                            cx.notify();
                        } else {
                            this.follow(client, cx);
                        }
                    },
                ));
                div()
                    .id(ElementId::from(SharedString::from(format!("looker-{}", l.client))))
                    .debug_selector({
                        let name = l.name.clone();
                        move || format!("looker-{name}")
                    })
                    .role(Role::Group)
                    .aria_label(label)
                    .absolute()
                    .left(px(s.x))
                    .top(px(s.y))
                    .w(px(s.w.max(1.0)))
                    .h(px(s.h.max(1.0)))
                    .border_1()
                    .border_color(hsla_alpha(colour, 0.9))
                    .rounded(px(theme.radii.md))
                    .child(tag)
                    .into_any_element()
            })
            .collect()
    }

    /// Whether camera moves are animated. The self-test turns this off so a dump right after
    /// an action sees where the camera ended up, not where it was passing through.
    pub const fn set_animation(&mut self, on: bool) {
        self.animate = on;
    }

    /// Where the camera is heading, while it is on its way there.
    #[must_use]
    pub fn flying_to(&self) -> Option<Camera> {
        self.flight.as_ref().map(Flight::target)
    }

    /// The repository headings the last arrange left, in canvas units.
    #[must_use]
    pub fn headings(&self) -> &[Heading] {
        &self.headings
    }

    /// The human took the camera: any pan, zoom, pinch or minimap scrub abandons a flight,
    /// so the 180 ms of an animation are never 180 ms of ignored input.
    const fn take_camera(&mut self) {
        self.flight = None;
        self.frame_at = None;
        self.following = None;
    }

    /// Keep up with `client`: fly to where they look now and again on every move they report.
    /// A client no longer looking anywhere is nobody to follow.
    fn follow(&mut self, client: ClientId, cx: &mut Context<Self>) {
        let Some(view) = self.doc.lookers().find(|l| l.client == client).map(|l| l.view) else {
            self.following = None;
            cx.notify();
            return;
        };
        let target = Camera::fitted([view], self.viewport_size());
        self.fly(target, cx);
        self.following = Some(client);
    }

    /// Show `what` for [`POINT_FOR`]: a newer toast replaces it and restarts the clock.
    fn show_toast(&mut self, what: ToastKind, cx: &mut Context<Self>) {
        self.show_toast_for(what, POINT_FOR, cx);
    }

    /// Show `what` for `during`: a newer toast replaces it and restarts the clock.
    fn show_toast_for(&mut self, what: ToastKind, during: Duration, cx: &mut Context<Self>) {
        let seq = self.toast.as_ref().map_or(0, |t| t.seq).wrapping_add(1);
        self.toast = Some(Toast { seq, what });
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(during).await;
            let _gone = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.seq == seq) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// ⌘⇧O: point the other clients at the active card, and say who was pointed — or that
    /// nobody else is here, in which case nothing is sent. Nothing active is nothing to
    /// point at. The host relays it and this client hears its own echo as nothing.
    pub fn point_others(&mut self, _: &PointOthers, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.active {
            self.point_at(item, cx);
        }
    }

    /// Point the other clients at `item` (⌘⇧O, or the active card's "point" pill).
    fn point_at(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let Some(title) = self.doc.get(item).map(|i| self.card_title(i, cx)) else { return };
        let mut names = self.doc.lookers().map(|l| l.name.as_str());
        let said = match (names.next(), names.count()) {
            (None, _) => "nobody else is here".to_owned(),
            (Some(one), 0) => format!("pointed {one} at {title}"),
            (Some(_), more) => format!("pointed {} others at {title}", more.saturating_add(1)),
        };
        if self.doc.lookers().next().is_some() {
            self.send(ClientMsg::Point { item });
        }
        self.show_toast(ToastKind::Said(said), cx);
    }

    /// ⌘]: the next card in reading order.
    pub fn next_card(&mut self, _: &NextCard, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_card(true, cx);
    }

    /// ⌘[: the previous card in reading order.
    pub fn prev_card(&mut self, _: &PrevCard, _window: &mut Window, cx: &mut Context<Self>) {
        self.step_card(false, cx);
    }

    /// Every card in reading order: rows top to bottom (by the top edge; cards snap to the
    /// grid, so neighbours placed by hand share one), left to right within a row.
    fn reading_order(&self) -> Vec<ItemId> {
        let mut items: Vec<&CanvasItem> = self.doc.items().collect();
        items.sort_by(|a, b| {
            a.rect.y.total_cmp(&b.rect.y).then_with(|| a.rect.x.total_cmp(&b.rect.x))
        });
        items.into_iter().map(|i| i.id).collect()
    }

    /// Go to the card after (or before) the active one in reading order, wrapping; nothing
    /// active starts at the first (or the last).
    fn step_card(&mut self, forward: bool, cx: &mut Context<Self>) {
        let order = self.reading_order();
        let Some(last) = order.len().checked_sub(1) else { return };
        let at = self.active.and_then(|active| order.iter().position(|id| *id == active));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => last,
            (Some(i), true) => {
                if i == last {
                    0
                } else {
                    i.saturating_add(1)
                }
            }
            (Some(i), false) => i.checked_sub(1).unwrap_or(last),
        };
        if let Some(&id) = order.get(next) {
            self.go_to(id, cx);
        }
    }

    /// ⌘⌥-arrow: the nearest card in that direction — its centre inside the 90° cone from
    /// the active card's centre, the closest by distance — as a window manager walks
    /// windows; nothing there leaves the focus where it is. Nothing active starts at the
    /// first card in reading order.
    fn step_towards(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        let Some(active) = self.active.and_then(|id| self.doc.get(id)) else {
            if let Some(&first) = self.reading_order().first() {
                self.go_to(first, cx);
            }
            return;
        };
        let centre = |r: &Rect| (r.w.mul_add(0.5, r.x), r.h.mul_add(0.5, r.y));
        let from = centre(&active.rect);
        let active_id = active.id;
        let nearest = self
            .doc
            .items()
            .filter(|item| item.id != active_id)
            .filter_map(|item| {
                let c = centre(&item.rect);
                let (vx, vy) = (c.0 - from.0, c.1 - from.1);
                let along = vx.mul_add(dx, vy * dy);
                let across = vx.mul_add(dy, -(vy * dx)).abs();
                (along > 0.0 && across <= along).then(|| (vx.hypot(vy), item.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, id)) = nearest {
            self.go_to(id, cx);
        }
    }

    /// Activate and reveal `id`; a terminal takes the keyboard too.
    fn go_to(&mut self, id: ItemId, cx: &mut Context<Self>) {
        if let Some(ItemKind::Terminal { session }) = self.doc.get(id).map(|i| &i.kind) {
            self.reveal_session(*session, cx);
        } else {
            self.activate(id, cx);
            self.reveal_pending = Some(id);
        }
    }

    /// The toast at the top: a pointing is `name points at title`, a button that goes to
    /// the card (active, revealed) and takes the toast with it — a card that has gone since
    /// takes the toast with it too; a word to this client is a status line.
    fn render_toast(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let toast = self.toast.as_ref()?;
        let style = |d: Stateful<Div>| {
            d.flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.md))
                .py(px(theme.spacing.xs))
                .rounded(px(theme.radii.md))
                .text_size(px(theme.typography.small()))
                .font_family(theme.typography.ui_family.clone())
        };
        let inner = match &toast.what {
            ToastKind::Pointed { name, item } => {
                let item = self.doc.get(*item)?;
                let id = item.id;
                let title = self.card_title(item, cx);
                let pill = style(div().id("pointed"))
                    .debug_selector(|| "pointed".to_owned())
                    .role(Role::Button)
                    .aria_label(format!("{name} points at {title}, go there"))
                    .bg(hsla(theme.surfaces.accent))
                    .text_color(hsla(theme.surfaces.accent_fg))
                    .cursor_pointer()
                    .child(SharedString::from(format!("{name} points at {title}")))
                    .child(div().opacity(0.8).child("· go"));
                tab_stop(pill, theme.surfaces.accent_fg)
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.toast = None;
                        this.go_to(id, cx);
                    }))
                    .into_any_element()
            }
            ToastKind::Closed { seq, title } => {
                let seq = *seq;
                let pill = style(div().id("closed"))
                    .debug_selector(|| "closed".to_owned())
                    .role(Role::Button)
                    .aria_label(format!("closed {title}, take it back"))
                    .bg(hsla(theme.surfaces.accent))
                    .text_color(hsla(theme.surfaces.accent_fg))
                    .cursor_pointer()
                    .child(SharedString::from(format!("closed {title}")))
                    .child(div().opacity(0.8).child("· take back ⌘Z"));
                tab_stop(pill, theme.surfaces.accent_fg)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.take_back(Some(seq), cx)))
                    .into_any_element()
            }
            ToastKind::Said(text) => style(div().id("said"))
                .debug_selector(|| "said".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(text.clone()))
                .bg(hsla_alpha(theme.surfaces.panel, alpha::MINIMAP))
                .border_1()
                .border_color(hsla_alpha(theme.surfaces.accent, alpha::LOOKER_TAG))
                .text_color(hsla(theme.surfaces.text))
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        };
        let row = div()
            .absolute()
            .top(px(MINIMAP_MARGIN))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(inner);
        Some(row.into_any_element())
    }

    /// Move the camera to `target` on this client's own account: it stops following anyone.
    fn fly_to(&mut self, target: Camera, cx: &mut Context<Self>) {
        self.following = None;
        self.fly(target, cx);
    }

    /// Move the camera to `target`, over [`FLIGHT`] seconds unless animation is off.
    fn fly(&mut self, target: Camera, cx: &mut Context<Self>) {
        let viewport = self.viewport_size();
        if !self.animate || viewport.0 <= 0.0 || viewport.1 <= 0.0 {
            self.flight = None;
            self.camera = target;
        } else {
            self.flight = Some(Flight::new(self.camera, target, viewport, FLIGHT));
            self.frame_at = None;
        }
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    /// Advance a flight by the time since the last frame. Called from the canvas element's
    /// prepaint, which is the only clock the UI has: no timers, and a still canvas costs
    /// nothing because a landed flight stops asking for frames.
    fn advance_flight(&mut self, cx: &mut Context<Self>) {
        let Some(flight) = self.flight.as_mut() else {
            self.frame_at = None;
            return;
        };
        let now = std::time::Instant::now();
        #[expect(clippy::cast_possible_truncation, reason = "a frame is milliseconds")]
        let dt = self.frame_at.map_or(0.0, |then| now.duration_since(then).as_secs_f64() as f32);
        self.frame_at = Some(now);
        self.camera = flight.advance(dt);
        let done = flight.done();
        if done {
            self.flight = None;
            self.frame_at = None;
        }
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    /// ⌘⇧R: one block of items per repository, the most recently used on the left, and the
    /// camera opened out to show the result.
    ///
    /// Not undoable: the canvas has no undo stack, here or for a drag.
    pub fn arrange_by_repo(
        &mut self,
        _: &ArrangeByRepo,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items: Vec<Arrangeable> = self
            .doc
            .by_z()
            .into_iter()
            .map(|item| Arrangeable {
                id: item.id,
                rect: item.rect,
                cwd: self.cwd_of(item),
                repo: self.repo_of(item),
                // The z order is the recency the canvas already keeps: raising an item on
                // focus is what makes it the most recent.
                activity: u64::from(item.z),
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let arrangement = arrange::arrange_by_repo(&items, (GAP, GAP));
        for (id, rect) in &arrangement.places {
            if self.doc.get(*id).is_some_and(|item| item.rect != *rect) {
                self.propose(CanvasOp::Place { id: *id, rect: *rect });
            }
        }
        let target = Camera::fitted(arrangement.rects(), self.viewport_size());
        self.headings = arrangement.headings;
        self.fly_to(target, cx);
    }

    /// The working directory of an item's session, for grouping. Only terminals have one.
    fn cwd_of(&self, item: &CanvasItem) -> Option<String> {
        match &item.kind {
            ItemKind::Terminal { session } => {
                self.sessions.get(session).and_then(|s| s.cwd.clone())
            }
            // A file card's directory, when the path is spelled from the root.
            ItemKind::File { path } if path.starts_with('/') => path
                .rsplit_once('/')
                .map(|(dir, _)| if dir.is_empty() { "/" } else { dir }.to_owned()),
            _ => None,
        }
    }

    /// The repository of an item's session, as the host resolved it. What arrange groups on;
    /// `None` when the host resolved none — a directory outside any repository, or one that has
    /// since been removed.
    fn repo_of(&self, item: &CanvasItem) -> Option<String> {
        match item.kind {
            ItemKind::Terminal { session } => {
                self.sessions.get(&session).and_then(|s| s.repo.clone())
            }
            _ => None,
        }
    }

    /// Fit every item on the next frame, when the viewport size is known. Used right after
    /// the first canvas snapshot so a phone does not open onto empty space beside a layout
    /// made on a desktop.
    pub const fn fit_when_painted(&mut self) {
        self.fit_pending = true;
    }

    fn fit_now(&mut self, cx: &mut Context<Self>) {
        let rects: Vec<Rect> =
            self.doc.items().map(|i| i.rect).chain(self.headings.iter().map(|h| h.rect)).collect();
        let target = Camera::fitted(rects, self.viewport_size());
        self.fly_to(target, cx);
    }

    // ----- pointer -------------------------------------------------------------------------

    fn local(&self, p: Point<Pixels>) -> Point<Pixels> {
        p - self.viewport.0
    }

    /// Tab from the canvas itself (nothing else focused) enters the keyboard ring; inside a
    /// terminal or a text field Tab is theirs.
    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) && crate::a11y::cycle(ev, window, cx) {
            cx.stop_propagation();
        }
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let delta = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * 20.0, l.y * 20.0),
        };
        self.take_camera();
        if ev.modifiers.platform {
            let local = self.local(ev.position);
            let factor = (-delta.1 * 0.01).exp();
            self.camera.zoom_at(factor, f32::from(local.x), f32::from(local.y));
            cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        } else {
            self.camera.pan(delta.0, delta.1);
        }
        cx.notify();
    }

    fn pinch(&mut self, ev: &PinchEvent, _w: &mut Window, cx: &mut Context<Self>) {
        self.take_camera();
        let local = self.local(ev.position);
        let factor = (1.0 + ev.delta).max(0.05);
        self.camera.zoom_at(factor, f32::from(local.x), f32::from(local.y));
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    fn begin_pan(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        if self.minimap.is_some_and(|m| m.contains(ev.position)) {
            self.drag = Some(Drag::Minimap);
            self.scrub_minimap(ev.position, cx);
        } else {
            self.drag = Some(Drag::Pan { last: ev.position });
        }
        cx.notify();
    }

    /// Centre the camera on the canvas point under `p` in the minimap.
    fn scrub_minimap(&mut self, p: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(map) = self.minimap else { return };
        self.take_camera();
        let (cx_, cy_) = map.to_canvas(p);
        let (_, vp) = self.viewport;
        self.camera.x = cx_ - f32::from(vp.width) / self.camera.zoom / 2.0;
        self.camera.y = cy_ - f32::from(vp.height) / self.camera.zoom / 2.0;
        cx.notify();
    }

    fn begin_move(&mut self, id: ItemId, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(start) = self.doc.get(id).map(|i| i.rect) else { return };
        self.activate(id, cx);
        self.drag = Some(Drag::Move { id, grab: ev.position, start });
        cx.stop_propagation();
        cx.notify();
    }

    fn begin_resize(&mut self, id: ItemId, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(start) = self.doc.get(id).map(|i| i.rect) else { return };
        self.activate(id, cx);
        self.drag = Some(Drag::Resize { id, grab: ev.position, start });
        cx.stop_propagation();
        cx.notify();
    }

    /// Keyboard focus must sit on an element that is drawn. Below [`CARD_ZOOM`] terminals are
    /// summary cards and their views are not in the frame, so a focus left on one would make
    /// GPUI drop every keystroke, ⌘ shortcuts included (found by the app self-test: after ⌘1
    /// zoomed two shells into cards, ⌘W did nothing). Cards hand the keyboard to the canvas;
    /// coming back to live grids hands it to the active terminal.
    fn keep_focus_rendered(&self, window: &mut Window, cx: &mut Context<Self>) {
        let card = self.camera.zoom < CARD_ZOOM;
        if card {
            let terminal_focused =
                self.terminals.values().any(|v| v.read(cx).focus_handle(cx).is_focused(window));
            if terminal_focused {
                window.focus(&self.focus, cx);
            }
        } else if self.focus.is_focused(window)
            && let Some(active) = self.active
            && let Some(ItemKind::Terminal { session }) = self.doc.get(active).map(|i| &i.kind)
            && let Some(view) = self.terminals.get(session)
        {
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Whether `item` gets an element this frame: it overlaps the viewport, or it is the active
    /// item or the one under a drag. Before the first paint the viewport is unknown and
    /// everything draws.
    fn draws(&self, item: &CanvasItem) -> bool {
        if self.active == Some(item.id) {
            return true;
        }
        let dragged = match self.drag {
            Some(Drag::Move { id, .. } | Drag::Resize { id, .. }) => id == item.id,
            _ => false,
        };
        dragged || self.on_screen(item.rect)
    }

    /// Whether `rect`, in canvas units, touches the viewport at the current camera. An unknown
    /// viewport draws everything: culling against a size we have not measured would hide the
    /// first frame.
    fn on_screen(&self, rect: Rect) -> bool {
        let (_, vp) = self.viewport;
        let (w, h) = (f32::from(vp.width), f32::from(vp.height));
        if w <= 0.0 || h <= 0.0 {
            return true;
        }
        let s = self.camera.to_screen(rect);
        s.x < w && s.y < h && s.x + s.w > 0.0 && s.y + s.h > 0.0
    }

    fn click_item(&mut self, id: ItemId, cx: &mut Context<Self>) {
        // Items own their clicks: the root must not start a pan or steal focus.
        cx.stop_propagation();
        self.activate(id, cx);
    }

    /// After a ring step: a remote window with no tab stop around it kept the keys, so the
    /// canvas itself takes them — ⌃Tab is always the way out of a window, whose chords are
    /// all the host's while it has the focus.
    fn leave_screen(&self, window: &mut Window, cx: &mut Context<Self>) {
        let on_screen = self
            .active_screen()
            .is_some_and(|screen| screen.read(cx).focus_handle(cx).is_focused(window));
        if on_screen {
            window.focus(&self.focus, cx);
        }
    }

    fn activate(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let session = match self.doc.get(id).map(|i| &i.kind) {
            Some(ItemKind::Terminal { session }) => Some(*session),
            _ => None,
        };
        if let Some(session) = session {
            self.touch_session(session);
        }
        self.active = Some(id);
        if let Some(ItemKind::Terminal { session }) = self.doc.get(id).map(|i| &i.kind) {
            self.finished.remove(session);
        }
        if self.doc.by_z().last().is_none_or(|top| top.id != id) {
            self.propose(CanvasOp::Raise(id));
        }
        cx.notify();
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let zoom = self.camera.zoom;
        match drag {
            Drag::Pan { last } => {
                self.take_camera();
                let d = ev.position - last;
                self.camera.pan(f32::from(d.x), f32::from(d.y));
                self.drag = Some(Drag::Pan { last: ev.position });
            }
            Drag::Minimap => self.scrub_minimap(ev.position, cx),
            Drag::Move { id, grab, start } => {
                let d = ev.position - grab;
                let rect = Rect {
                    x: start.x + f32::from(d.x) / zoom,
                    y: start.y + f32::from(d.y) / zoom,
                    ..start
                };
                self.doc.apply_op(&CanvasOp::Place { id, rect });
            }
            Drag::Resize { id, grab, start } => {
                let d = ev.position - grab;
                let rect = Rect {
                    w: (start.w + f32::from(d.x) / zoom).max(MIN_ITEM),
                    h: (start.h + f32::from(d.y) / zoom).max(MIN_ITEM),
                    ..start
                };
                self.doc.apply_op(&CanvasOp::Place { id, rect });
            }
        }
        cx.notify();
    }

    fn mouse_up(&mut self, _ev: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Pan { .. } | Drag::Minimap => {}
            Drag::Move { id, .. } => {
                if let Some(item) = self.doc.get(id) {
                    let r = item.rect;
                    let rect = Rect { x: snap(r.x), y: snap(r.y), w: snap(r.w), h: snap(r.h) };
                    self.propose(CanvasOp::Place { id, rect });
                }
            }
            Drag::Resize { id, start, .. } => {
                if let Some(item) = self.doc.get(id) {
                    let r = item.rect;
                    let rect = Rect { x: snap(r.x), y: snap(r.y), w: snap(r.w), h: snap(r.h) };
                    self.propose(CanvasOp::Place { id, rect });
                    self.resize_remote_window(id, start, rect, cx);
                }
            }
        }
        cx.notify();
    }

    /// The grip of a streamed window's card was let go: ask the host to give the window the
    /// size the card now has, at the scale the card drew the window at when the drag began
    /// (its width over the window's native pixels), so pulling a card twice as wide asks for
    /// a window twice as wide. The host's answer comes back as a `Geometry` event and
    /// [`Self::follow_geometry`] settles the card on the size the window really took.
    fn resize_remote_window(&self, id: ItemId, start: Rect, rect: Rect, cx: &Context<Self>) {
        let Some(view) = self.screens.get(&id).map(|v| v.read(cx)) else { return };
        if !matches!(view.target(), CaptureTarget::Window(_)) {
            return;
        }
        let (native_w, native_h) = view.size();
        #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
        let per_pixel = start.w / native_w.max(1) as f32;
        if per_pixel <= 0.0 {
            return;
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped ≥ 1")]
        let px = |units: f32| (units / per_pixel).round().max(1.0) as u32;
        let (width, height) = (px(rect.w), px(rect.h - TITLE_H));
        if (width, height) == (native_w, native_h) {
            return;
        }
        self.send(ClientMsg::Screen(ScreenRequest::Resize {
            stream: view.stream(),
            width,
            height,
        }));
    }

    // ----- render --------------------------------------------------------------------------

    fn render_item(
        &self,
        item: &CanvasItem,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let zoom = self.camera.zoom;
        let s = self.camera.to_screen(item.rect);
        let active = self.active == Some(item.id);
        let id = item.id;
        let card = zoom < CARD_ZOOM;
        // Cards keep full-size chrome; otherwise the title bar and everything in it (text,
        // pills, the badge and its answers) scale with the item so nothing spills or clips.
        let k = if card { 1.0 } else { zoom };
        let chrome = Chrome { k, zooming: self.zooming };
        let title_h = TITLE_H * k;
        let ui_base = self.theme.typography.ui_size - 1.0;
        let ui_size = ui_base * k;

        let kind = Self::kind_name(item);
        let title = self.card_title(item, cx);
        let focused = match &item.kind {
            ItemKind::Terminal { session } => self
                .terminals
                .get(session)
                .is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window)),
            ItemKind::Window { .. } | ItemKind::Display { .. } => self
                .screens
                .get(&item.id)
                .is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window)),
            ItemKind::Note { .. } => {
                self.notes.get(&item.id).is_some_and(|v| v.read(cx).editing(window, cx))
            }
            ItemKind::File { .. } => false,
        };
        let renaming = self.rename.as_ref().filter(|r| r.id == id).map(|r| r.input.clone());
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agents.get(&session).map(|a| (session, a)),
            _ => None,
        };
        let badge = agent.map(|(session, a)| self.agent_badge(id, session, a, chrome, cx));
        let finished = match &item.kind {
            ItemKind::Terminal { session } => {
                self.finished.get(session).map(|f| self.finished_badge(id, *session, f, chrome, cx))
            }
            _ => None,
        };
        // Somebody else is on the canvas: the active card offers to point them at it, so a
        // phone without ⌘⇧O can too.
        let point = (active && self.doc.lookers().next().is_some())
            .then(|| point_button(id, theme, chrome, cx));
        // A file card: read it again (the human edited it in a shell; an agent's edit reloads
        // it unasked).
        let reload = match item.kind {
            ItemKind::File { .. } => Some(reload_button(id, theme, chrome, cx)),
            _ => None,
        };
        // A file card: its find bar, for a phone without ⌘F.
        let find = match item.kind {
            ItemKind::File { .. } => Some(find_button(id, theme, chrome, cx)),
            _ => None,
        };
        // A file card: the file in `$EDITOR` at the line being read, in the canvas's shell —
        // only while there is a shell for it to go to.
        let edit = match item.kind {
            ItemKind::File { .. } if self.run_target().is_some() => {
                Some(edit_button(id, theme, chrome, cx))
            }
            _ => None,
        };
        // An agent the host had to guess at: offer the hooks that would make it precise.
        let hooks = agent
            .filter(|(_session, a)| a.source != AgentSource::Hook && !self.hooks_offered)
            .map(|_agent| hooks_button(id, theme, chrome, cx));
        // Another client's size rules this PTY: offer to take it (on the active item only, so
        // a wall of cards stays readable).
        let take = match item.kind {
            ItemKind::Terminal { session } if active => self
                .terminals
                .get(&session)
                .filter(|v| !v.read(cx).driving())
                .map(|_| take_button(id, theme, chrome, cx)),
            // A window with sound offers mute; a muted one always shows it, so a silenced item
            // is never mistaken for one whose sound simply stopped.
            ItemKind::Window { .. } | ItemKind::Display { .. } => self
                .screens
                .get(&id)
                .map(|v| v.read(cx))
                .filter(|v| v.muted() || (active && v.has_audio()))
                .map(|v| mute_button(id, v.muted(), theme, chrome, cx)),
            _ => None,
        };
        let needs_human = agent.is_some_and(|(_, a)| needs_human(a));
        let border = if needs_human {
            theme.surfaces.warn
        } else if focused || active {
            theme.surfaces.accent
        } else {
            theme.surfaces.border
        };

        let heading = SharedString::from(if kind == title {
            title.clone()
        } else {
            format!("{kind} {title}")
        });
        let title_bar = div()
            .id(element_id("title", id))
            .debug_selector(|| format!("title-{}", id.as_uuid()))
            .role(Role::Heading)
            .aria_label(heading)
            .h(px(title_h))
            .w_full()
            .flex()
            .items_center()
            .px(px(theme.spacing.sm * k))
            .gap(px(theme.spacing.sm * k))
            .bg(hsla(theme.surfaces.panel))
            .border_b_1()
            .border_color(hsla(theme.surfaces.border))
            .text_size(px(ui_size))
            .text_color(hsla(if active { theme.surfaces.text } else { theme.surfaces.text_muted }))
            .font_family(theme.typography.ui_family.clone())
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    // The second click of a double-click names the card; the first began a
                    // move that the mouse-up already ended.
                    if ev.click_count == 2 {
                        this.start_rename(id, window, cx);
                        cx.stop_propagation();
                    } else {
                        this.begin_move(id, ev, cx);
                    }
                }),
            )
            .child(div().size(px(theme.spacing.sm * k)).rounded_full().bg(hsla(if focused {
                theme.surfaces.accent
            } else {
                theme.surfaces.text_muted
            })))
            // "take" sits left of the title so it stays reachable on a phone when the item is
            // wider than the screen.
            .when_some(take, gpui::ParentElement::child)
            .child(match renaming {
                // The name field takes the title's place; a click in it must not start a move.
                Some(input) => div()
                    .id(element_id("rename", id))
                    .debug_selector(|| format!("rename-{}", id.as_uuid()))
                    .flex_1()
                    .overflow_hidden()
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .child(Input::new(&input).aria_label("Card name"))
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .overflow_hidden()
                    .child(ChromeText::new(title, px(ui_base), k).fill().zooming(chrome.zooming))
                    .into_any_element(),
            })
            .when_some(point, gpui::ParentElement::child)
            .when_some(find, gpui::ParentElement::child)
            .when_some(edit, gpui::ParentElement::child)
            .when_some(reload, gpui::ParentElement::child)
            .when_some(hooks, gpui::ParentElement::child)
            .when_some(finished, gpui::ParentElement::child)
            .when_some(badge, gpui::ParentElement::child);

        let body: gpui::AnyElement = match &item.kind {
            ItemKind::Terminal { session } => match (card, self.terminals.get(session)) {
                (false, Some(view)) => {
                    view.update(cx, |v, _| {
                        v.set_zoom(zoom);
                        v.set_zooming(chrome.zooming);
                    });
                    div().flex_1().w_full().overflow_hidden().child(view.clone()).into_any_element()
                }
                (true, Some(view)) => {
                    let (cols, rows) = {
                        let size = view.read(cx).state().size();
                        (size.cols, size.rows)
                    };
                    div()
                        .flex_1()
                        .w_full()
                        .p(px(theme.spacing.sm))
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(theme.surfaces.text_muted))
                        .font_family(theme.typography.ui_family.clone())
                        .child(SharedString::from(format!("{cols}×{rows}")))
                        .into_any_element()
                }
                (_, None) => div()
                    .flex_1()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(ui_size))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(if self.sessions.contains_key(session) {
                        "attaching…"
                    } else {
                        "session ended"
                    })
                    .into_any_element(),
            },
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                // Video paints at every zoom: the view asks the host for a stream scale that
                // matches its painted width, so a thumbnail costs a thumbnail-sized stream,
                // and a live picture beats a frame counter (a phone fitting a desktop layout
                // sits well below `CARD_ZOOM`).
                match self.screens.get(&item.id) {
                    Some(view) => {
                        let painted = s.w * window.scale_factor();
                        view.update(cx, |v, _| v.set_painted_width(painted));
                        div()
                            .flex_1()
                            .w_full()
                            .overflow_hidden()
                            .child(view.clone())
                            .into_any_element()
                    }
                    None => div()
                        .flex_1()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(ui_size))
                        .text_color(hsla(theme.surfaces.text_muted))
                        .child(if item.sleeping { "sleeping" } else { "opening…" })
                        .into_any_element(),
                }
            }
            ItemKind::Note { text: note_text } => match (card, self.notes.get(&item.id)) {
                (false, Some(view)) => {
                    let (pad, text_size) = (theme.spacing.sm, ui_size);
                    view.update(cx, |v, _| v.set_layout(zoom, pad, text_size));
                    div()
                        .flex_1()
                        .w_full()
                        .overflow_hidden()
                        .font_family(theme.typography.ui_family.clone())
                        .text_color(hsla(theme.surfaces.text))
                        .child(view.clone())
                        .into_any_element()
                }
                _ => div()
                    .flex_1()
                    .w_full()
                    .p(px(theme.spacing.sm))
                    .overflow_hidden()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .font_family(theme.typography.ui_family.clone())
                    .child(SharedString::from(note_summary(note_text)))
                    .into_any_element(),
            },
            ItemKind::File { .. } => match (card, self.files.get(&item.id)) {
                (false, Some(view)) => {
                    let (pad, text_size) = (theme.spacing.sm, theme.typography.small());
                    view.update(cx, |v, _| v.set_layout(zoom, pad, text_size));
                    div().flex_1().w_full().overflow_hidden().child(view.clone()).into_any_element()
                }
                (_, view) => div()
                    .flex_1()
                    .w_full()
                    .p(px(theme.spacing.sm))
                    .overflow_hidden()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .font_family(theme.typography.ui_family.clone())
                    .child(SharedString::from(
                        view.map_or_else(|| "reading…".to_owned(), |v| v.read(cx).summary()),
                    ))
                    .into_any_element(),
            },
        };

        let grip = div()
            .id(element_id("grip", id))
            .absolute()
            .right_0()
            .bottom_0()
            .size(px(GRIP * k))
            .cursor_nwse_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev, _w, cx| this.begin_resize(id, ev, cx)),
            );

        div()
            .id(element_id("item", id))
            .debug_selector(|| format!("item-{}", id.as_uuid()))
            .role(Role::Group)
            .absolute()
            .left(px(s.x))
            .top(px(s.y))
            .w(px(s.w))
            .h(px(s.h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(theme.radii.md * k))
            .border_1()
            .border_color(hsla(border))
            .bg(hsla(theme.terminal.bg))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| this.click_item(id, cx)),
            )
            .child(title_bar)
            .child(body)
            .child(grip)
            .into_any_element()
    }
}

impl CanvasView {
    /// The overview in the bottom-right corner: every item as a block, the viewport as an
    /// outline. Painted straight from the document each frame; the mapping is kept for
    /// hit-testing.
    /// The heading over a repository block: the repository's name, in canvas coordinates so it
    /// pans and zooms with the items under it.
    fn render_heading(&self, heading: &Heading) -> gpui::AnyElement {
        let s = self.camera.to_screen(heading.rect);
        let k = self.camera.zoom.clamp(0.25, 1.0);
        let theme = &self.theme;
        div()
            .id(ElementId::from(SharedString::from(format!("heading-{}", heading.key))))
            .debug_selector({
                let label = heading.label.clone();
                move || format!("heading-{label}")
            })
            .role(Role::Heading)
            .aria_label(heading.label.clone())
            .absolute()
            .left(px(s.x))
            .top(px(s.y))
            .w(px(s.w.max(1.0)))
            .h(px(s.h))
            .flex()
            .items_end()
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(theme.surfaces.text_muted))
            .font_family(theme.typography.ui_family.clone())
            .child(
                ChromeText::new(heading.label.clone(), px(theme.typography.small()), k)
                    .zooming(self.zooming),
            )
            .into_any_element()
    }

    fn render_minimap(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = self.theme.clone();
        let camera = self.camera;
        let (_, vp) = self.viewport;
        let viewport = Rect {
            x: camera.x,
            y: camera.y,
            w: f32::from(vp.width) / camera.zoom,
            h: f32::from(vp.height) / camera.zoom,
        };
        let items: Vec<(Rect, bool)> =
            self.doc.items().map(|i| (i.rect, self.active == Some(i.id))).collect();
        // The other clients' viewports are part of the overview: where they look is in the
        // box even when it is off this screen.
        let lookers: Vec<(Rect, slopty_theme::Rgb)> =
            self.doc.lookers().map(|l| (l.view, self.looker_colour(l.client))).collect();
        let entity = cx.entity();
        let rects: Vec<Rect> =
            items.iter().map(|(r, _)| *r).chain(lookers.iter().map(|(r, _)| *r)).collect();
        let map = canvas(
            move |bounds, _window, cx| {
                let rects = rects.into_iter().chain(std::iter::once(viewport));
                let map = MinimapMap::new(bounds.origin, rects);
                entity.update(cx, |this, _| this.minimap = Some(map));
                map
            },
            move |bounds, map: MinimapMap, window, _cx| {
                window.paint_quad(gpui::quad(
                    bounds,
                    px(theme.radii.md),
                    hsla_alpha(theme.surfaces.panel, alpha::MINIMAP),
                    px(1.0),
                    hsla(theme.surfaces.border),
                    BorderStyle::Solid,
                ));
                for (rect, active) in &items {
                    let color =
                        if *active { theme.surfaces.accent } else { theme.surfaces.text_muted };
                    window.paint_quad(fill(
                        map.to_box(*rect),
                        hsla_alpha(color, alpha::MINIMAP_ITEM),
                    ));
                }
                for (rect, colour) in &lookers {
                    window.paint_quad(outline(
                        map.to_box(*rect),
                        hsla_alpha(*colour, alpha::MINIMAP_LOOKER),
                        BorderStyle::Solid,
                    ));
                }
                window.paint_quad(outline(
                    map.to_box(viewport),
                    hsla(theme.surfaces.accent),
                    BorderStyle::Solid,
                ));
            },
        )
        .size_full();
        div()
            .id("minimap")
            .role(Role::Image)
            .aria_label("Canvas overview")
            .absolute()
            .right(px(MINIMAP_MARGIN))
            .bottom(px(MINIMAP_MARGIN))
            .w(px(MINIMAP.0))
            .h(px(MINIMAP.1))
            .child(map)
            .into_any_element()
    }
}

impl Render for CanvasView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(session) = self.pending_focus.take()
            && let Some(view) = self.terminals.get(&session)
        {
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if let Some((session, needle)) = self.pending_find.take()
            && let Some(view) = self.terminals.get(&session).cloned()
        {
            // After this frame: the card is drawn and focused first, then its find bar takes
            // the keyboard.
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&needle, window, cx));
            });
        }
        if let Some((item, needle)) = self.pending_find_file.take()
            && let Some(view) = self.files.get(&item).cloned()
        {
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&needle, window, cx));
            });
        }
        if std::mem::take(&mut self.pending_focus_picker)
            && let Some(picker) = &self.picker
        {
            let handle = picker.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_focus_self) {
            window.focus(&self.focus, cx);
        }
        if std::mem::take(&mut self.pending_focus_palette)
            && let Some(palette) = &self.palette
        {
            let handle = palette.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_focus_rename)
            && let Some(rename) = &self.rename
        {
            rename.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        if self.rename.is_none()
            && let Some(handle) = self.rename_return.take()
        {
            window.focus(&handle, cx);
        }
        if self.palette.is_none()
            && let Some(handle) = self.palette_return.take()
        {
            window.focus(&handle, cx);
            if let Some(action) = self.palette_action.take() {
                // Once this frame is done, from the element that had the keyboard.
                cx.defer_in(window, move |_this, window, cx| window.dispatch_action(action, cx));
            }
        }
        self.keep_focus_rendered(window, cx);
        self.reconcile_notes(window, cx);
        self.reconcile_files(cx);
        // A zoom that differs from the one drawn last frame is in motion. Once the changes
        // pause for `SETTLE`, one more frame is asked for so the final zoom paints exact;
        // asking right away doubled the frames of a gesture (a settle frame per step, each
        // rasterising every glyph at its intermediate size).
        let zoom = self.camera.zoom;
        let changed = self.zoom_drawn.is_some_and(|drawn| drawn.to_bits() != zoom.to_bits());
        self.zoom_drawn = Some(zoom);
        if changed {
            self.settle_pending = true;
            self.settle_generation = self.settle_generation.wrapping_add(1);
            let generation = self.settle_generation;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SETTLE).await;
                let _gone = this.update(cx, |this, cx| {
                    if this.settle_generation == generation {
                        this.settle_pending = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.zooming = self.settle_pending;
        #[cfg(test)]
        if self.zooming {
            self.zooming_frames = self.zooming_frames.saturating_add(1);
        }
        if let Some(id) = self.pending_focus_note.take()
            && let Some(view) = self.notes.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        let picker = self.picker.clone();
        let palette = self.palette.clone();
        let items = self.doc.by_z().into_iter().cloned().collect::<Vec<_>>();
        let entity = cx.entity();
        let record_bounds = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |this, cx| {
                    // This frame culled against the previous viewport; a resize that exposes
                    // an item needs one more frame to draw it. A notify while drawing only
                    // marks the view, so it goes through a deferred effect, which runs once
                    // the frame is done and dirties the window.
                    if this.viewport.1 != bounds.size {
                        let canvas = cx.weak_entity();
                        cx.defer(move |cx| {
                            // A released canvas has nothing left to redraw.
                            if let Some(canvas) = canvas.upgrade() {
                                canvas.update(cx, |_, cx| cx.notify());
                            }
                        });
                    }
                    this.viewport = (bounds.origin, bounds.size);
                    for id in std::mem::take(&mut this.fit_items_pending) {
                        this.fit_to_viewport(id);
                    }
                    if std::mem::take(&mut this.fit_pending) && !this.is_empty() {
                        this.fit_now(cx);
                    }
                    this.advance_flight(cx);
                    if let Some(rect) =
                        this.reveal_pending.take().and_then(|id| this.doc.get(id)).map(|i| i.rect)
                    {
                        this.camera.reveal(
                            rect,
                            (f32::from(bounds.size.width), f32::from(bounds.size.height)),
                        );
                        cx.emit(CanvasEvent::Zoom(this.camera.zoom));
                        cx.notify();
                    }
                });
            },
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();

        let empty = self.is_empty();
        // Only what the viewport shows is built: an off-screen terminal's element would still
        // shape and lay out every row. The active item and the one being dragged always are,
        // so focus and a drag never land on an element that was not drawn. A block's heading
        // is culled the same way, on its own rect — it is a label on the canvas, not chrome.
        let headings: Vec<gpui::AnyElement> = self
            .headings
            .iter()
            .filter(|h| self.on_screen(h.rect))
            .map(|h| self.render_heading(h))
            .collect();
        let rendered: Vec<gpui::AnyElement> = items
            .iter()
            .filter(|item| self.draws(item))
            .map(|item| self.render_item(item, window, cx))
            .collect();
        let lookers = self.render_lookers(cx);
        let here = self.render_here(cx);
        let pointed = self.render_toast(cx);
        self.note_view(cx);
        let minimap = (!empty).then(|| self.render_minimap(cx));
        let file_card_active = self
            .active
            .and_then(|id| self.doc.get(id))
            .is_some_and(|i| matches!(i.kind, ItemKind::File { .. }));
        let mut key_context = gpui::KeyContext::new_with_defaults();
        key_context.add("Canvas");
        if file_card_active {
            key_context.add("file_card");
        }
        div()
            .id("canvas")
            .debug_selector(|| "canvas".to_owned())
            .key_context(key_context)
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label("Canvas")
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(self.theme.surfaces.canvas))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(|this, _: &LineUp, _w, cx| {
                this.move_file_line(LineMove::Lines(-1), cx);
            }))
            .on_action(cx.listener(|this, _: &LineDown, _w, cx| {
                this.move_file_line(LineMove::Lines(1), cx);
            }))
            .on_action(cx.listener(|this, _: &PageUp, _w, cx| {
                this.move_file_line(LineMove::Pages(-1), cx);
            }))
            .on_action(cx.listener(|this, _: &PageDown, _w, cx| {
                this.move_file_line(LineMove::Pages(1), cx);
            }))
            .on_action(
                cx.listener(|this, _: &LineFirst, _w, cx| this.move_file_line(LineMove::First, cx)),
            )
            .on_action(
                cx.listener(|this, _: &LineLast, _w, cx| this.move_file_line(LineMove::Last, cx)),
            )
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::add_window))
            .on_action(cx.listener(Self::close_item))
            .on_action(cx.listener(Self::undo_close))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::fit_all))
            .on_action(cx.listener(Self::zoom_to_item))
            .on_action(cx.listener(Self::arrange_by_repo))
            .on_action(cx.listener(Self::next_attention))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(cx.listener(Self::toggle_stats))
            .on_action(cx.listener(Self::find_in_active))
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::rename_item))
            .on_action(cx.listener(Self::point_others))
            .on_action(cx.listener(Self::find_everywhere))
            .on_action(cx.listener(Self::next_card))
            .on_action(cx.listener(Self::prev_card))
            .on_action(cx.listener(|this, _: &CardLeft, _w, cx| this.step_towards(-1.0, 0.0, cx)))
            .on_action(cx.listener(|this, _: &CardRight, _w, cx| this.step_towards(1.0, 0.0, cx)))
            .on_action(cx.listener(|this, _: &CardUp, _w, cx| this.step_towards(0.0, -1.0, cx)))
            .on_action(cx.listener(|this, _: &CardDown, _w, cx| this.step_towards(0.0, 1.0, cx)))
            // Esc in the name field: the input's own action, taken here so the field closes
            // without a change and the canvas has the keyboard.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Escape, _window, cx| {
                    if this.rename.is_some() {
                        this.finish_rename(false, true, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                window.focus_next(cx);
                this.leave_screen(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPrev, window, cx| {
                window.focus_prev(cx);
                this.leave_screen(window, cx);
            }))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .capture_pinch(cx.listener(Self::pinch))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_pan))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(record_bounds)
            .children(headings)
            .children(rendered)
            .children(lookers)
            .children(here)
            .children(pointed)
            .children(minimap)
            .children(picker)
            .children(palette)
            .when(empty, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(self.theme.typography.ui_size))
                        .text_color(hsla(self.theme.surfaces.text_muted))
                        .font_family(self.theme.typography.ui_family.clone())
                        .child("⌘T opens a shell on the host · ⌘O adds a window"),
                )
            })
    }
}

/// A file card's title: the file's name, with the directory it is in when there is one
/// (`main.rs · src`), so two `mod.rs` cards can be told apart.
#[must_use]
pub fn file_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let name = parts.next().filter(|n| !n.is_empty()).unwrap_or(trimmed);
    match parts.next().filter(|d| !d.is_empty()) {
        Some(dir) => format!("{name} · {dir}"),
        None => name.to_owned(),
    }
}

/// The "reload" pill in a file card's title bar: read the file again.
fn reload_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("reload", id, "reload", theme.surfaces.text_secondary, theme, chrome)
        .role(Role::Button)
        .aria_label("Read the file again");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            this.request_file(id);
            cx.notify();
        }))
        .into_any_element()
}

/// The "edit" pill in a file card's title bar: `$EDITOR +line path` typed into the canvas's
/// shell (see [`CanvasView::edit_file`]).
fn edit_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("edit", id, "edit", theme.surfaces.text_secondary, theme, chrome)
        .role(Role::Button)
        .aria_label("Open the file in the editor");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _window, cx| this.edit_file(id, cx)))
        .into_any_element()
}

/// The "find" pill in a file card's title bar: ⌘F for a phone (see [`FileView::find`]).
fn find_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("find", id, "find", theme.surfaces.text_secondary, theme, chrome)
        .role(Role::Button)
        .aria_label("Find in the file");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, window, cx| {
            if let Some(view) = this.files.get(&id).cloned() {
                this.activate(id, cx);
                view.update(cx, |v, cx| v.find(window, cx));
            }
            cx.notify();
        }))
        .into_any_element()
}

/// What a zoomed-out note card shows: its first non-empty line, clipped.
fn note_summary(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("empty note");
    line.chars().take(48).collect()
}

/// The "mute"/"muted" pill in a remote window's title bar (see [`CanvasView::toggle_mute`]).
fn mute_button(
    id: ItemId,
    muted: bool,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let (label, tone) =
        if muted { ("muted", theme.surfaces.warn) } else { ("mute", theme.surfaces.accent) };
    let pill = pill("mute", id, label, tone, theme, chrome)
        .role(Role::Button)
        .aria_label(if muted { "unmute" } else { "mute" });
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            if let Some(view) = this.screens.get(&id) {
                view.read(cx).toggle_mute();
                cx.notify();
            }
        }))
        .into_any_element()
}

/// The "take" pill in a terminal's title bar (see [`CanvasView::take_over`]).
fn take_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("take", id, "take", theme.surfaces.accent, theme, chrome)
        .role(Role::Button)
        .aria_label("take over");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _w, cx| this.take_over(id, cx)))
        .into_any_element()
}

/// The "point" pill on the active card while somebody else is on the canvas: point them at
/// it (see [`CanvasView::point_at`]).
fn point_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("point", id, "point", theme.surfaces.text_secondary, theme, chrome)
        .role(Role::Button)
        .aria_label("Point the others at this card");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _w, cx| this.point_at(id, cx)))
        .into_any_element()
}

/// How the item chrome is scaled this frame: `k`, the title bar's zoom factor, and whether the
/// zoom is in motion (chrome text then paints from the raster ladder).
#[derive(Clone, Copy, Debug)]
struct Chrome {
    k: f32,
    zooming: bool,
}

/// A title-bar pill: `small()` type on a faint fill of its tone, the tone as text, `radii.xs`;
/// hover deepens the fill. Everything is scaled by `k`, the title bar's zoom factor.
fn pill(
    part: &'static str,
    item: ItemId,
    label: &'static str,
    tone: slopty_theme::Rgb,
    theme: &Theme,
    chrome: Chrome,
) -> Stateful<Div> {
    let k = chrome.k;
    div()
        .id(element_id(part, item))
        .debug_selector(move || format!("{part}-{}", item.as_uuid()))
        .flex_none()
        .px(px(theme.spacing.sm * k))
        .py(px(theme.spacing.xxs * k))
        .rounded(px(theme.radii.xs * k))
        .bg(hsla_alpha(tone, alpha::TINT))
        .text_size(px(theme.typography.small() * k))
        .text_color(hsla(tone))
        .cursor_pointer()
        .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_STRONG)))
        .child(ChromeText::new(label, px(theme.typography.small()), k).zooming(chrome.zooming))
}

/// The "hooks" pill on an agent the host had to guess at: `slopty hook install` on the host,
/// so the pill stops being a guess. Shown once, on the first such session.
fn hooks_button(
    id: ItemId,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let pill = pill("hooks", id, "hooks", theme.surfaces.warn, theme, chrome)
        .role(Role::Button)
        .aria_label("install hooks");
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |this, _ev, _w, cx| this.install_hooks(cx)))
        .into_any_element()
}

/// Whether the agent is waiting on the human (an idle prompt is not worth an outline).
fn needs_human(agent: &AgentEvent) -> bool {
    matches!(&agent.status, AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt)
}

/// One short line for an agent's state: the badge text, the picker's status column.
#[must_use]
pub fn agent_status_text(agent: &AgentEvent) -> String {
    let detail = agent.detail.as_deref().filter(|d| !d.is_empty());
    match &agent.status {
        AgentStatus::None => String::new(),
        AgentStatus::Idle => "claude".to_owned(),
        AgentStatus::Working => detail.unwrap_or("working").to_owned(),
        AgentStatus::Tool { tool } => detail.unwrap_or(tool).to_owned(),
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            format!("allow? {}", detail.unwrap_or(tool))
        }
        AgentStatus::Blocked(BlockReason::Question) => {
            format!("asking: {}", detail.unwrap_or("a question"))
        }
        AgentStatus::Blocked(BlockReason::Elicitation) => {
            format!("needs input: {}", detail.unwrap_or("an answer"))
        }
        AgentStatus::Blocked(BlockReason::IdlePrompt) => "idle".to_owned(),
        AgentStatus::Done => format!("done: {}", detail.unwrap_or("turn finished")),
    }
}

impl CanvasView {
    /// The agent pill in a terminal's title bar: a coloured dot and a short line, plus the
    /// one-tap answers when the agent is waiting on the human: "allow" / "deny" for a
    /// permission prompt (Enter / Esc into the terminal), "answer" for a question (bring the
    /// terminal up so the reply can be typed). A sent answer shows as such until the host
    /// reports what the agent did next.
    fn agent_badge(
        &self,
        item: ItemId,
        session: SessionId,
        agent: &AgentEvent,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let ui_size = (theme.typography.ui_size - 1.0) * k;
        let (label, color) = match &agent.status {
            AgentStatus::None => return div().into_any_element(),
            AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt) => {
                (agent_status_text(agent), theme.surfaces.text_muted)
            }
            // Busy states (thinking, a tool) share the accent: the label says which.
            AgentStatus::Working | AgentStatus::Tool { .. } => {
                (agent_status_text(agent), theme.surfaces.accent)
            }
            AgentStatus::Blocked(_) => (agent_status_text(agent), theme.surfaces.warn),
            AgentStatus::Done => (agent_status_text(agent), theme.surfaces.success),
        };
        let button = |part: &'static str, text: &'static str, accent: bool| {
            let tone = if accent { theme.surfaces.accent } else { theme.surfaces.text_secondary };
            let pad = if cfg!(target_os = "ios") { theme.spacing.md } else { theme.spacing.sm };
            let pill = div()
                .id(element_id(part, item))
                .debug_selector(move || format!("{part}-{}", item.as_uuid()))
                .role(Role::Button)
                .aria_label(text)
                .flex_none()
                .flex()
                .items_center()
                .px(px(pad * k))
                .py(px(theme.spacing.xs * k))
                .rounded(px(theme.radii.xs * k))
                .bg(hsla_alpha(tone, alpha::TINT_STRONG))
                .text_size(px(theme.typography.small() * k))
                .text_color(hsla(theme.surfaces.text))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_PRESSED)))
                .child(
                    ChromeText::new(text, px(theme.typography.small()), k).zooming(chrome.zooming),
                );
            tab_stop(pill, theme.surfaces.accent)
        };
        // Blocked on the human: a "go" button reveals the terminal, where the TUI's own
        // prompt takes the answer; Slopty never answers for the human.
        let buttons: Vec<gpui::AnyElement> = match &agent.status {
            AgentStatus::Blocked(
                BlockReason::Permission { .. } | BlockReason::Question | BlockReason::Elicitation,
            ) => vec![
                button("go", "go", true)
                    .on_click(
                        cx.listener(move |this, _ev, _w, cx| this.reveal_session(session, cx)),
                    )
                    .into_any_element(),
            ],
            _ => Vec::new(),
        };
        let pill = div()
            .id(element_id("agent", item))
            .debug_selector(move || format!("agent-{}", item.as_uuid()))
            .role(Role::Status)
            .aria_label(SharedString::from(label.clone()))
            .flex()
            .items_center()
            .flex_none()
            .max_w(px(ui_size * if buttons.is_empty() { 22.0 } else { 14.0 }))
            .overflow_hidden()
            .gap(px(theme.spacing.xs * k))
            .px(px(theme.spacing.sm * k))
            .py(px(theme.spacing.xxs * k))
            .rounded(px(theme.radii.xs * k))
            .bg(hsla_alpha(color, alpha::TINT))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(color))
            .child(
                div()
                    .flex_none()
                    .size(px((theme.spacing.xs + theme.spacing.xxs) * k))
                    .rounded_full()
                    .bg(hsla(color)),
            )
            .child(
                div().overflow_hidden().child(
                    ChromeText::new(label, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            );
        div()
            .id(element_id("badge", item))
            .debug_selector(move || format!("badge-{}", item.as_uuid()))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .child(pill)
            .children(buttons)
            .into_any_element()
    }
}

impl CanvasView {
    /// The badge for a long shell command that ended unwatched: its status and how long it
    /// took, in the success or warn tone. A press activates the item (which clears it).
    fn finished_badge(
        &self,
        item: ItemId,
        session: SessionId,
        done: &Finished,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let tone = match done.exit {
            Some(0) | None => theme.surfaces.success,
            Some(_) => theme.surfaces.warn,
        };
        let label = done.label();
        let pill = div()
            .id(element_id("finished", item))
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(label.clone())
            .flex_none()
            .overflow_hidden()
            .px(px(theme.spacing.sm * k))
            .py(px(theme.spacing.xxs * k))
            .rounded(px(theme.radii.xs * k))
            .bg(hsla_alpha(tone, alpha::TINT))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(tone))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla_alpha(tone, alpha::TINT_STRONG)))
            .child(
                ChromeText::new(label, px(theme.typography.small()), k)
                    .fill()
                    .zooming(chrome.zooming),
            );
        tab_stop(pill, theme.surfaces.accent)
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                this.reveal_session(session, cx);
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    //! The canvas in a headless GPUI window: real layout, real key and mouse dispatch, no
    //! process, no permissions, no pixels. The host is a channel: the test reads what the
    //! canvas sends and feeds back the deltas a host would.

    use std::sync::Arc;

    use gpui::{Modifiers, TestAppContext, VisualTestContext, point, px, size};
    use slopty_grid::{Cursor, Line, LineIndex, RowUpdate, SemanticMark, Style, TermModes};
    use slopty_proto::agent::AgentKind;
    use slopty_proto::input::{KeyCode, Mods};
    use slopty_proto::screen::ScreenInput;
    use slopty_proto::terminal::{Frame, SessionState, SessionSummary};

    use super::*;

    const VIEWPORT: (f32, f32) = (1000.0, 700.0);
    const SHELL: Rect = Rect { x: 0.0, y: 0.0, w: 720.0, h: 440.0 };

    /// A focused canvas for one fake host, drawn once so the viewport is known.
    fn canvas(
        cx: &mut TestAppContext,
    ) -> (Entity<CanvasView>, mpsc::Receiver<ClientMsg>, ClientId, &mut VisualTestContext) {
        let me = ClientId::new();
        let (tx, rx) = mpsc::channel(64);
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(key_bindings());
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let factory: ScreenFactory =
                Arc::new(|_stream, _codec| panic!("this canvas opens no screens"));
            let mut view = CanvasView::new(me, tx, Vec::new(), factory, Theme::default(), cx);
            // A headless frame is a step, not a moment: the flight itself is unit-tested in
            // `slopty_client::canvas` with a clock of its own, so these tests assert where the
            // camera lands. `a_camera_move_is_a_flight` turns it back on.
            view.set_animation(false);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
        cx.run_until_parked();
        (view, rx, me, cx)
    }

    /// A window on the canvas streams, and a streaming window keeps the device awake: the Mac
    /// out of idle sleep, the phone's screen on. Putting the window to sleep lets go.
    #[gpui::test]
    fn a_streaming_window_keeps_the_device_awake(cx: &mut TestAppContext) {
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let factory: ScreenFactory =
                Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
            let mut view = CanvasView::new(me, tx, Vec::new(), factory, Theme::default(), cx);
            view.set_animation(false);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
        cx.run_until_parked();
        let window = slopty_core::WindowId(7);
        let item = |sleeping: bool| CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Window { window },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping,
            name: None,
        };
        let mut placed = item(false);
        let id = placed.id;
        view.update_in(cx, |c, _window, cx| {
            let op = CanvasOp::Upsert(placed.clone());
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op }, cx);
        });
        cx.run_until_parked();
        let target = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|msg| match msg {
                ClientMsg::Screen(ScreenRequest::Open { target, .. }) => Some(target),
                _ => None,
            })
            .expect("the canvas asks the host for the window's stream");
        assert_eq!(target, CaptureTarget::Window(window));
        assert_eq!(cx.active_idle_sleep_preventions(), 0, "nothing streams yet");

        view.update_in(cx, |c, _window, cx| {
            let opened = ScreenEvent::Opened {
                stream: StreamId(1),
                target,
                codec: slopty_proto::screen::VideoCodec::Hevc,
                width: 1280,
                height: 800,
                scale: 2.0,
                hdr: false,
            };
            c.screen_event(opened, cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 1, "a streaming window holds the device");

        placed = item(true);
        placed.id = id;
        view.update_in(cx, |c, _window, cx| {
            let op = CanvasOp::Upsert(placed);
            c.apply_sync(CanvasSync::Delta { version: 2, by: me, op }, cx);
        });
        cx.run_until_parked();
        assert_eq!(cx.active_idle_sleep_preventions(), 0, "a sleeping window lets go");
    }

    /// Letting go of a window card's grip asks the host for the window size the card now
    /// stands for: the card's new size over the scale it drew the window at, less the title
    /// bar. A card left at its size asks nothing; a display card never asks.
    #[gpui::test]
    fn the_grip_asks_the_host_to_resize_the_window(cx: &mut TestAppContext) {
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let factory: ScreenFactory =
                Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
            let mut view = CanvasView::new(me, tx, Vec::new(), factory, Theme::default(), cx);
            view.set_animation(false);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
        cx.run_until_parked();
        let open = |view: &Entity<CanvasView>,
                    cx: &mut VisualTestContext,
                    id: ItemId,
                    kind: ItemKind,
                    target: CaptureTarget,
                    stream: StreamId| {
            let item = CanvasItem {
                id,
                kind,
                rect: Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 + TITLE_H },
                z: 1,
                group: None,
                sleeping: false,
                name: None,
            };
            view.update_in(cx, |c, _window, cx| {
                let op = CanvasOp::Upsert(item);
                c.apply_sync(CanvasSync::Delta { version: 1, by: me, op }, cx);
                let opened = ScreenEvent::Opened {
                    stream,
                    target,
                    codec: slopty_proto::screen::VideoCodec::Hevc,
                    width: 1280,
                    height: 800,
                    scale: 2.0,
                    hdr: false,
                };
                c.screen_event(opened, cx);
            });
            cx.run_until_parked();
        };
        let resizes = |rx: &mut mpsc::Receiver<ClientMsg>| -> Vec<(StreamId, u32, u32)> {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|msg| match msg {
                    ClientMsg::Screen(ScreenRequest::Resize { stream, width, height }) => {
                        Some((stream, width, height))
                    }
                    _ => None,
                })
                .collect()
        };
        let window = ItemId::new();
        let wid = slopty_core::WindowId(9);
        let kind = ItemKind::Window { window: wid };
        open(&view, cx, window, kind, CaptureTarget::Window(wid), StreamId(1));
        let start = Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 + TITLE_H };
        let wider = Rect { w: 960.0, h: 500.0 + TITLE_H, ..start };
        view.update(cx, |c, cx| c.resize_remote_window(window, start, wider, cx));
        assert_eq!(
            resizes(&mut rx),
            vec![(StreamId(1), 1920, 1000)],
            "1.5× as wide, 1.25× as tall"
        );
        view.update(cx, |c, cx| c.resize_remote_window(window, start, start, cx));
        assert!(resizes(&mut rx).is_empty(), "the same size asks nothing");

        let display = ItemId::new();
        let kind = ItemKind::Display { display: 2 };
        open(&view, cx, display, kind, CaptureTarget::Display(2), StreamId(2));
        view.update(cx, |c, cx| c.resize_remote_window(display, start, wider, cx));
        assert!(resizes(&mut rx).is_empty(), "a display is not resized");
    }

    /// A focused remote window gets every chord, the canvas's own included: ⌘W goes to the
    /// host as the remote editor's close-tab, the card stays. ⌃Tab is the way back: it moves
    /// the focus out of the window, and the same ⌘W then closes the card.
    #[gpui::test]
    fn a_focused_remote_window_takes_every_chord(cx: &mut TestAppContext) {
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
        cx.update(|cx| cx.bind_keys(key_bindings()));
        let (view, cx) = cx.add_window_view(|window, cx| {
            let factory: ScreenFactory =
                Arc::new(|stream, _codec| slopty_client::ScreenHandle::detached(stream));
            let mut view = CanvasView::new(me, tx, Vec::new(), factory, Theme::default(), cx);
            view.set_animation(false);
            window.focus(&view.focus, cx);
            view
        });
        cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
        cx.run_until_parked();
        let window = ItemId::new();
        let wid = slopty_core::WindowId(9);
        let item = CanvasItem {
            id: window,
            kind: ItemKind::Window { window: wid },
            rect: Rect { x: 0.0, y: 0.0, w: 640.0, h: 400.0 + TITLE_H },
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        view.update_in(cx, |c, _window, cx| {
            let op = CanvasOp::Upsert(item);
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op }, cx);
            c.screen_event(
                ScreenEvent::Opened {
                    stream: StreamId(1),
                    target: CaptureTarget::Window(wid),
                    codec: slopty_proto::screen::VideoCodec::Hevc,
                    width: 1280,
                    height: 800,
                    scale: 2.0,
                    hdr: false,
                },
                cx,
            );
            c.activate(window, cx);
        });
        cx.run_until_parked();
        let screen = view.read_with(cx, |c, _| c.active_screen()).expect("a screen view");
        cx.update(|window, cx| {
            let focus = screen.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        });
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        let keys = |rx: &mut mpsc::Receiver<ClientMsg>| -> Vec<(KeyCode, Mods)> {
            std::iter::from_fn(|| rx.try_recv().ok())
                .filter_map(|msg| match msg {
                    ClientMsg::Screen(ScreenRequest::Input {
                        input: ScreenInput::Key { code, mods, .. },
                        ..
                    }) => Some((code, mods)),
                    _ => None,
                })
                .collect()
        };

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert!(view.read_with(cx, |c, _| c.doc.get(window).is_some()), "the card stays");
        assert!(
            keys(&mut rx)
                .iter()
                .any(|(code, mods)| *code == crate::keys::key_code("w")
                    && mods.contains(Mods::SUPER)),
            "⌘W went to the host"
        );

        cx.simulate_keystrokes("ctrl-tab");
        cx.run_until_parked();
        let focused = cx.update(|window, cx| screen.read(cx).focus_handle(cx).is_focused(window));
        assert!(!focused, "⌃Tab leaves the window");
        while rx.try_recv().is_ok() {}
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert!(view.read_with(cx, |c, _| c.doc.get(window).is_none()), "the canvas's ⌘W again");
        assert!(keys(&mut rx).is_empty(), "nothing went to the host");
    }

    /// An agent at work keeps the device awake (the human is waiting on it); an agent that
    /// idles, waits on the human, or whose session closes lets go.
    #[gpui::test]
    fn a_working_agent_keeps_the_device_awake(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let other = SessionId::new();
        host_opens(&view, cx, session, me, SHELL, 1);
        host_opens(&view, cx, other, me, Rect { x: 800.0, ..SHELL }, 2);
        let event = |session: SessionId, status: AgentStatus| AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: false,
            source: AgentSource::Hook,
        };
        let holds = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            cx.active_idle_sleep_preventions()
        };
        view.update_in(cx, |c, _window, cx| c.agent_event(event(session, AgentStatus::Idle), cx));
        assert_eq!(holds(cx), 0, "an idle agent holds nothing");
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(event(session, AgentStatus::Working), cx);
        });
        assert_eq!(holds(cx), 1, "a working agent holds the device");
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(event(other, AgentStatus::Tool { tool: "Bash".to_owned() }), cx);
        });
        assert_eq!(holds(cx), 1, "one hold covers every agent");
        view.update_in(cx, |c, _window, cx| {
            let blocked = AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
            c.agent_event(event(session, blocked), cx);
        });
        assert_eq!(holds(cx), 1, "the other agent still works");
        view.update_in(cx, |c, _window, cx| c.session_closed(other, cx));
        assert_eq!(holds(cx), 0, "waiting on the human is not work");
    }

    /// `debug_bounds` wants a static selector; tests may leak a handful.
    fn selector(part: &str, id: ItemId) -> &'static str {
        Box::leak(format!("{part}-{}", id.as_uuid()).into_boxed_str())
    }

    /// The host opened `session` for `by` and placed it at `rect`.
    fn host_opens(
        view: &Entity<CanvasView>,
        cx: &mut VisualTestContext,
        session: SessionId,
        by: ClientId,
        rect: Rect,
        version: u64,
    ) -> ItemId {
        host_opens_in(view, cx, session, by, rect, version, Where::default())
    }

    /// Where a test session runs: what OSC 7 said, and the repository the host resolved it to.
    /// The root is absent when the host found none, not when the host is old — the handshake
    /// requires exact protocol equality, so every host this client talks to sends both fields.
    #[derive(Clone, Copy, Default)]
    struct Where<'a> {
        cwd: Option<&'a str>,
        repo: Option<&'a str>,
    }

    impl<'a> Where<'a> {
        /// A host that resolved the repository, which is what arrange keys on.
        const fn rooted(cwd: &'a str, repo: &'a str) -> Self {
            Self { cwd: Some(cwd), repo: Some(repo) }
        }

        /// A directory with no repository behind it: the fallback the heuristic still covers.
        const fn loose(cwd: &'a str) -> Self {
            Self { cwd: Some(cwd), repo: None }
        }
    }

    /// [`host_opens`], saying where the session runs (what arrange groups on).
    #[expect(clippy::too_many_arguments, reason = "a test fixture, not an interface")]
    fn host_opens_in(
        view: &Entity<CanvasView>,
        cx: &mut VisualTestContext,
        session: SessionId,
        by: ClientId,
        rect: Rect,
        version: u64,
        at: Where<'_>,
    ) -> ItemId {
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            rect,
            z: u32::try_from(version).unwrap(),
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        let summary = SessionSummary {
            kind: SessionKind::Terminal,
            id: session,
            title: "shell".into(),
            cwd: at.cwd.map(str::to_owned),
            repo: at.repo.map(str::to_owned),
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 1,
            command: Vec::new(),
        };
        view.update_in(cx, |c, _window, cx| {
            c.session_opened(summary, cx);
            c.apply_sync(CanvasSync::Delta { version, by, op: CanvasOp::Upsert(item) }, cx);
        });
        cx.run_until_parked();
        id
    }

    fn frame(rows: &[&str]) -> TermEvent {
        TermEvent::Frame(Frame {
            seq: 1,
            full: true,
            epoch: 0,
            cols: 80,
            rows: u16::try_from(rows.len()).unwrap(),
            cursor: Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(0),
            total_lines: rows.len() as u64,
            input_ack: 0,
            images: Vec::new(),
            updates: rows
                .iter()
                .enumerate()
                .map(|(row, text)| RowUpdate {
                    row: u16::try_from(row).unwrap(),
                    line: Line::from_text(text, 80, Style::DEFAULT),
                })
                .collect(),
        })
    }

    /// A frame of marked rows with the cursor on `cursor_row`, the shell-integration shape a
    /// host sends for a command block.
    fn marked_frame(seq: u64, rows: &[(&str, SemanticMark)], cursor_row: u16) -> TermEvent {
        TermEvent::Frame(Frame {
            seq,
            full: seq == 1,
            epoch: 0,
            cols: 80,
            rows: u16::try_from(rows.len()).unwrap(),
            cursor: Cursor { row: cursor_row, ..Cursor::default() },
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(0),
            total_lines: rows.len() as u64,
            input_ack: 0,
            images: Vec::new(),
            updates: rows
                .iter()
                .enumerate()
                .map(|(row, (text, mark))| {
                    let mut line = Line::from_text(text, 80, Style::DEFAULT);
                    line.mark = *mark;
                    RowUpdate { row: u16::try_from(row).unwrap(), line }
                })
                .collect(),
        })
    }

    /// ⌘W on a shell whose command runs sends the host nothing until the shell's bar is
    /// confirmed with ↩; Esc keeps the shell and its session.
    #[gpui::test]
    fn a_busy_shell_closes_only_when_confirmed(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let _id = host_opens(&view, cx, session, me, SHELL, 1);
        assert!(terminal_focused(&view, cx, session));
        let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
        view.update_in(cx, |c, _window, cx| {
            let typed = [("$ sleep 9", prompt), ("", SemanticMark::Output)];
            c.term_event(session, marked_frame(1, &typed, 0), cx);
            c.term_event(session, marked_frame(2, &typed, 1), cx);
        });
        cx.run_until_parked();
        let closes = |sent: &[ClientMsg]| {
            sent.iter()
                .filter(|m| {
                    matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session)
                })
                .count()
        };
        drain(&mut rx);
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        assert_eq!(closes(&drain(&mut rx)), 0, "a running command: the shell asks first");
        assert!(cx.debug_bounds("close-confirm").is_some(), "the bar is up");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_eq!(closes(&drain(&mut rx)), 0, "Esc keeps it");
        assert!(cx.debug_bounds("close-confirm").is_none());
        cx.simulate_keystrokes("cmd-w");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(m, ClientMsg::Canvas(CanvasOp::Remove(_)))),
            "\u{21a9} takes the card off the canvas: {sent:?}"
        );
        assert_eq!(closes(&sent), 0, "the session waits for a change of mind");
        cx.executor().advance_clock(UNDO_CLOSE);
        cx.run_until_parked();
        assert_eq!(closes(&drain(&mut rx)), 1, "then the host closes it");
    }

    /// ⌘W on an idle shell takes its card off the canvas but leaves its session running for
    /// five seconds; ⌘Z (or the toast's button) within them puts the card back as it was,
    /// with the same view, active and focused. Once they pass the host closes the session,
    /// the view goes, and ⌘Z has nothing to take back.
    #[gpui::test]
    fn a_closed_shell_can_be_taken_back_for_five_seconds(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let session = SessionId::new();
        let id = host_opens(&view, cx, session, me, SHELL, 1);
        assert!(terminal_focused(&view, cx, session));
        let first = view.read_with(cx, |c, _| c.terminal(session).cloned().unwrap());
        let closes = |sent: &[ClientMsg]| {
            sent.iter()
                .filter(|m| {
                    matches!(m, ClientMsg::Term { session: s, req: TermRequest::Close } if *s == session)
                })
                .count()
        };
        let upserts = |sent: &[ClientMsg]| {
            sent.iter()
                .filter_map(|m| match m {
                    ClientMsg::Canvas(CanvasOp::Upsert(item)) => Some(item.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        drain(&mut rx);

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(m, ClientMsg::Canvas(CanvasOp::Remove(i)) if *i == id)),
            "{sent:?}"
        );
        assert_eq!(closes(&sent), 0, "the session is kept");
        view.read_with(cx, |c, _| {
            assert!(c.items().iter().all(|i| i.id != id), "the card is off the canvas");
            assert!(c.terminal(session).is_some(), "its view stays, attached");
        });
        assert!(cx.debug_bounds("closed").is_some(), "the toast offers to take it back");

        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        let back = upserts(&sent);
        assert_eq!(back.len(), 1, "{sent:?}");
        assert_eq!((back[0].id, back[0].rect), (id, SHELL), "back where it was");
        assert_eq!(closes(&sent), 0);
        view.read_with(cx, |c, _| {
            assert_eq!(c.active_item(), Some(id));
            assert!(c.terminal(session).is_some_and(|v| *v == first), "the same view");
        });
        assert!(terminal_focused(&view, cx, session), "and the keyboard is in it");
        assert!(cx.debug_bounds("closed").is_none(), "the toast went with the undo");

        // The toast's button takes it back too.
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        drain(&mut rx);
        let toast = cx.debug_bounds("closed").expect("the toast again");
        cx.simulate_click(toast.center(), Modifiers::default());
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert_eq!(upserts(&sent).len(), 1, "{sent:?}");
        assert_eq!(closes(&sent), 0);

        // Five seconds after a close, the host closes the session and nothing comes back.
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        drain(&mut rx);
        cx.executor().advance_clock(UNDO_CLOSE);
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert_eq!(closes(&sent), 1, "{sent:?}");
        assert!(view.read_with(cx, |c, _| c.terminal(session).is_none()), "the view went");
        assert!(cx.debug_bounds("closed").is_none(), "and the toast");
        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(upserts(&sent).is_empty(), "nothing to take back: {sent:?}");
    }

    /// A note holds the only copy of what was typed into it, so closing one offers it back
    /// for the same five seconds — with its text — and lets go after them.
    #[gpui::test]
    fn a_closed_note_comes_back_with_its_text(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let id = ItemId::new();
        let rect = SHELL;
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text: "ship it".to_owned() },
            rect,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
            c.activate(id, cx);
        });
        cx.run_until_parked();
        drain(&mut rx);

        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(m, ClientMsg::Canvas(CanvasOp::Remove(i)) if *i == id)),
            "{sent:?}"
        );
        view.read_with(cx, |c, _| assert!(c.items().iter().all(|i| i.id != id)));
        assert!(cx.debug_bounds("closed").is_some(), "the toast offers it back");

        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        let back: Vec<&CanvasItem> = sent
            .iter()
            .filter_map(|m| match m {
                ClientMsg::Canvas(CanvasOp::Upsert(item)) => Some(item),
                _ => None,
            })
            .collect();
        assert_eq!(back.len(), 1, "{sent:?}");
        assert_eq!((back[0].id, back[0].rect), (id, rect), "back where it was");
        assert!(
            matches!(&back[0].kind, ItemKind::Note { text } if text == "ship it"),
            "with what was typed into it: {:?}",
            back[0].kind
        );
        view.read_with(cx, |c, _| assert_eq!(c.active_item(), Some(id)));
        assert!(cx.debug_bounds("closed").is_none(), "the toast went with the undo");

        // Five seconds later the offer is gone and ⌘Z finds nothing.
        cx.simulate_keystrokes("cmd-w");
        cx.run_until_parked();
        drain(&mut rx);
        cx.executor().advance_clock(UNDO_CLOSE);
        cx.run_until_parked();
        assert!(cx.debug_bounds("closed").is_none(), "the toast went with the wait");
        cx.simulate_keystrokes("cmd-z");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            !sent.iter().any(|m| matches!(m, ClientMsg::Canvas(CanvasOp::Upsert(_)))),
            "nothing to take back: {sent:?}"
        );
    }

    /// The editor carries typing into the document on a timer, so a note closed mid-sentence
    /// has to be remembered from the field rather than from the document, or the take-back
    /// would hand back a note missing the line that prompted the close.
    #[gpui::test]
    fn a_note_closed_before_its_commit_keeps_what_was_typed(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let id = ItemId::new();
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text: "ship".to_owned() },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
            c.activate(id, cx);
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds(selector("note-read", id)).expect("the rendered note");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        cx.simulate_keystrokes("!");
        cx.run_until_parked();
        // No `advance_clock`: the commit timer has not fired, so the document still says "ship".
        view.read_with(cx, |c, _| {
            let stored = c.items().iter().find_map(|i| match &i.kind {
                ItemKind::Note { text } => Some(text.clone()),
                _ => None,
            });
            assert_eq!(stored.as_deref(), Some("ship"), "the commit is still pending");
        });
        drain(&mut rx);

        // Driven rather than typed: the keystroke path is covered above, and here the
        // editor had the keyboard until the card went.
        view.update_in(cx, |c, window, cx| {
            c.close_item(&CloseItem, window, cx);
            c.undo_close(&UndoClose, window, cx);
        });
        cx.run_until_parked();
        let sent = drain(&mut rx);
        let back = sent.iter().rev().find_map(|m| match m {
            ClientMsg::Canvas(CanvasOp::Upsert(item)) if item.id == id => Some(item.clone()),
            _ => None,
        });
        let back = back.unwrap_or_else(|| panic!("the note comes back: {sent:?}"));
        assert!(
            matches!(&back.kind, ItemKind::Note { text } if text == "ship!"),
            "with the keystroke the timer had not carried over: {:?}",
            back.kind
        );
    }

    /// A shell command that ran long and ended in an item the human is not on badges its
    /// title bar with the status and the time; the same end in the active item badges
    /// nothing; pressing the badge goes to the item and clears it.
    #[gpui::test]
    fn a_long_command_that_ends_unwatched_badges_its_item(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let other = SessionId::new();
        let id = host_opens(&view, cx, session, me, SHELL, 1);
        let other_id = host_opens(&view, cx, other, me, Rect { x: 800.0, ..SHELL }, 2);
        view.update_in(cx, |c, window, _cx| {
            c.set_slow_command(Duration::ZERO);
            window.set_a11y_active(true);
        });
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(other_id), "on the other");

        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let run = |view: &Entity<CanvasView>, cx: &mut VisualTestContext, session: SessionId| {
            let typed = [
                ("$ sleep 9", prompt(None)),
                ("", SemanticMark::Output),
                ("", SemanticMark::Output),
            ];
            let done =
                [("$ sleep 9", prompt(None)), ("", SemanticMark::Output), ("$ ", prompt(Some(0)))];
            view.update_in(cx, |c, _window, cx| {
                c.term_event(session, marked_frame(1, &typed, 0), cx);
                c.term_event(session, marked_frame(2, &typed, 1), cx);
                c.term_event(session, marked_frame(3, &done, 2), cx);
            });
            cx.run_until_parked();
        };

        run(&view, cx, other);
        assert!(view.read_with(cx, |c, _| c.finished(other).is_none()), "watched: no badge");

        run(&view, cx, session);
        let label = view.read_with(cx, |c, _| c.finished(session).map(Finished::label));
        assert_eq!(label.as_deref(), Some("done 0.0 s"));
        let badge = cx.debug_bounds(selector("finished", id)).expect("the badge is drawn");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("done 0.0 s"))), "{tree:?}");

        cx.simulate_click(badge.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(id), "the badge goes there");
        assert!(view.read_with(cx, |c, _| c.finished(session).is_none()), "and clears");
        assert!(cx.debug_bounds(selector("finished", id)).is_none());
    }

    #[test]
    fn a_programs_banner_has_a_title_even_when_the_protocol_gave_none() {
        assert_eq!(
            program_banner(None, "", "build done"),
            ("Terminal".to_owned(), "build done".to_owned())
        );
        assert_eq!(
            program_banner(Some("build box"), "Tests", " all green\n"),
            ("build box · Tests".to_owned(), "all green".to_owned())
        );
    }

    #[test]
    fn a_named_card_leads_its_banner() {
        assert_eq!(banner_title(None, "Claude wants to use Bash"), "Claude wants to use Bash");
        assert_eq!(
            banner_title(Some("build box"), "Claude wants to use Bash"),
            "build box · Claude wants to use Bash"
        );
    }

    #[test]
    fn a_note_is_titled_by_its_first_line() {
        assert_eq!(note_title(""), "note");
        assert_eq!(note_title("\n  \n# Plan for today\n- x"), "Plan for today");
        assert_eq!(note_title("- first item"), "first item");
        assert_eq!(
            note_title("- [ ] ship\n- [x] test\n- [X] tag"),
            "ship · 2/3",
            "a checklist counts"
        );
        assert_eq!(note_title("# Plan\n\n- [ ] ship"), "Plan · 0/1");
        assert_eq!(note_progress("no tasks"), None);
        let long = "a".repeat(NOTE_TITLE_CHARS + 5);
        assert_eq!(note_title(&long), format!("{}…", "a".repeat(NOTE_TITLE_CHARS)));
    }

    #[test]
    fn a_finished_badge_says_the_status_and_the_time() {
        let done =
            |exit, secs| Finished { command: "x".into(), exit, elapsed: Duration::from_secs(secs) };
        assert_eq!(done(Some(0), 7).label(), "done 7.0 s");
        assert_eq!(done(None, 7).label(), "done 7.0 s");
        assert_eq!(done(Some(1), 65).label(), "failed (1) 1 m 05 s", "as the row caption reads");
    }

    /// Another client's viewport is an outline labelled with its name, placed where the
    /// camera puts it; when that client leaves, the outline goes.
    #[gpui::test]
    fn another_clients_viewport_is_an_outline_with_its_name(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let phone = ClientId::new();
        let looking = |view: Option<Rect>| CanvasSync::Presence {
            client: phone,
            kind: ClientKind::IPhone,
            name: "phone".to_owned(),
            view,
        };
        let at = Rect { x: 40.0, y: 60.0, w: 300.0, h: 500.0 };
        view.update(cx, |c, cx| c.apply_sync(looking(Some(at)), cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let outline = tree
            .iter()
            .find(|n| n.role == "Group" && n.label.as_deref() == Some("phone"))
            .expect("the phone's outline is in the tree");
        let expected = view.read_with(cx, |c, _| c.camera.to_screen(at));
        assert!(
            (outline.bounds[0] - expected.x).abs() < 1.0
                && (outline.bounds[2] - expected.w).abs() < 1.0,
            "{:?} vs {expected:?}",
            outline.bounds
        );
        // My own presence, echoed by the host, is not drawn.
        view.update(cx, |c, cx| {
            c.apply_sync(
                CanvasSync::Presence {
                    client: me,
                    kind: ClientKind::Mac,
                    name: "me".to_owned(),
                    view: Some(at),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.label.as_deref() == Some("me")));

        view.update(cx, |c, cx| c.apply_sync(looking(None), cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.label.as_deref() == Some("phone")), "gone with the client");
    }

    /// The overview fits where the others look, so a client off this screen is still in the box.
    #[gpui::test]
    fn the_minimap_fits_where_the_others_look(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let id = ItemId::new();
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text: "here".to_owned() },
            rect: Rect { x: 0.0, y: 0.0, w: 300.0, h: 200.0 },
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
        });
        cx.run_until_parked();
        let far = Rect { x: 9_000.0, y: 9_000.0, w: 400.0, h: 300.0 };
        let inside = |map: MinimapMap, r: Rect| {
            let b = map.to_box(r);
            let (x, y) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (ox, oy) = (f32::from(map.origin.x), f32::from(map.origin.y));
            x >= ox
                && y >= oy
                && x + f32::from(b.size.width) <= ox + MINIMAP.0 + 1.0
                && y + f32::from(b.size.height) <= oy + MINIMAP.1 + 1.0
        };
        let map = view.read_with(cx, |c, _| c.minimap.expect("drawn"));
        assert!(!inside(map, far), "far is outside the overview before anyone looks there");
        view.update(cx, |c, cx| {
            c.apply_sync(
                CanvasSync::Presence {
                    client: ClientId::new(),
                    kind: ClientKind::Mac,
                    name: "other".to_owned(),
                    view: Some(far),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let map = view.read_with(cx, |c, _| c.minimap.expect("drawn"));
        assert!(inside(map, far), "the overview grew to include where they look");
        assert!(
            inside(map, Rect { x: 0.0, y: 0.0, w: 300.0, h: 200.0 }),
            "and still shows the item"
        );
    }

    /// The name on another client's outline is a button: it flies the camera to what that
    /// client sees.
    #[gpui::test]
    fn the_name_tag_goes_to_what_they_see(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let far = Rect { x: 5_000.0, y: 3_000.0, w: 400.0, h: 700.0 };
        view.update(cx, |c, cx| {
            // Pan so the outline is on screen; only its tag has to be clickable.
            c.camera = Camera::fitted([far], c.viewport_size());
            c.camera.pan(200.0, 100.0);
            c.apply_sync(
                CanvasSync::Presence {
                    client: ClientId::new(),
                    kind: ClientKind::IPad,
                    name: "pad".to_owned(),
                    view: Some(far),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("follow pad"))), "{tree:?}");
        let tag = cx.debug_bounds("follow-pad").expect("the tag is drawn");
        cx.simulate_click(tag.center(), Modifiers::default());
        cx.run_until_parked();
        let (camera, viewport) = view.read_with(cx, |c, _| (c.camera, c.viewport_size()));
        let expected = Camera::fitted([far], viewport);
        assert!(
            (camera.x - expected.x).abs() < 0.5 && (camera.y - expected.y).abs() < 0.5,
            "{camera:?} vs {expected:?}"
        );
    }

    /// The name tag toggles following: the camera keeps up with every move that client reports
    /// until this client moves the camera itself, and a client that leaves is nobody to follow.
    #[gpui::test]
    fn following_keeps_up_with_them_until_you_move(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let pad = ClientId::new();
        let far = Rect { x: 5_000.0, y: 3_000.0, w: 400.0, h: 700.0 };
        let presence = |view: Option<Rect>| CanvasSync::Presence {
            client: pad,
            kind: ClientKind::IPad,
            name: "pad".to_owned(),
            view,
        };
        view.update(cx, |c, cx| {
            c.camera = Camera::fitted([far], c.viewport_size());
            c.camera.pan(200.0, 100.0);
            c.apply_sync(presence(Some(far)), cx);
        });
        cx.run_until_parked();
        let tag = cx.debug_bounds("follow-pad").expect("the tag is drawn");
        cx.simulate_click(tag.center(), Modifiers::default());
        cx.run_until_parked();
        let labels = |cx: &mut VisualTestContext| {
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            tree.iter()
                .filter(|n| {
                    // The outline's tag, not the here row's pill (that one names the device).
                    n.role == "Button"
                        && n.label
                            .as_deref()
                            .is_some_and(|l| l.ends_with("ing pad") || l == "follow pad")
                })
                .filter_map(|n| n.label.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(cx), vec!["following pad".to_owned()]);
        // They move: the camera goes with them.
        let moved = Rect { x: 9_000.0, y: 1_000.0, w: 300.0, h: 500.0 };
        view.update(cx, |c, cx| c.apply_sync(presence(Some(moved)), cx));
        cx.run_until_parked();
        let (camera, viewport) = view.read_with(cx, |c, _| (c.camera, c.viewport_size()));
        let expected = Camera::fitted([moved], viewport);
        assert!(
            (camera.x - expected.x).abs() < 0.5 && (camera.y - expected.y).abs() < 0.5,
            "{camera:?} vs {expected:?}"
        );
        // This client pans: following ends, and their next move is theirs alone.
        view.update(cx, |c, cx| {
            c.take_camera();
            c.camera.pan(50.0, 0.0);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.following), None);
        let before = view.read_with(cx, |c, _| c.camera);
        view.update(cx, |c, cx| c.apply_sync(presence(Some(far)), cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.camera), before, "not followed");
        // Back where their outline is on screen; follow again, then they leave: nobody to
        // follow.
        view.update(cx, |c, cx| {
            c.camera = Camera::fitted([far], c.viewport_size());
            c.camera.pan(200.0, 100.0);
            cx.notify();
        });
        cx.run_until_parked();
        let tag = cx.debug_bounds("follow-pad").expect("the tag is drawn again");
        cx.simulate_click(tag.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.following), Some(pad));
        view.update(cx, |c, cx| c.apply_sync(presence(None), cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.following), None);
        assert!(labels(cx).is_empty(), "no tag without a viewport");
    }

    /// The palette lists every other client as "Follow <name>" with its device on the right,
    /// The "here" row names every other client whether or not their viewport is on this
    /// screen; a pill follows them like the outline's tag, and it goes when they leave.
    #[gpui::test]
    fn the_here_row_names_the_others_and_follows_on_a_click(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        assert!(cx.debug_bounds("here").is_none(), "alone: no row");
        let pad = ClientId::new();
        let phone = ClientId::new();
        let far = Rect { x: 5_000.0, y: 3_000.0, w: 400.0, h: 700.0 };
        let presence = |client: ClientId, kind: ClientKind, name: &str, view: Option<Rect>| {
            CanvasSync::Presence { client, kind, name: name.to_owned(), view }
        };
        view.update(cx, |c, cx| {
            c.apply_sync(presence(pad, ClientKind::IPad, "pad", Some(far)), cx);
            c.apply_sync(
                presence(
                    phone,
                    ClientKind::IPhone,
                    "phone",
                    Some(Rect { x: 0.0, y: 0.0, w: 300.0, h: 500.0 }),
                ),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("follow-pad").is_none(), "off screen: no outline tag");
        let pill = cx.debug_bounds("here-pad").expect("a pill even so");
        assert!(cx.debug_bounds("here-phone").is_some());
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let labels: Vec<String> = tree
            .iter()
            .filter(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.contains(", i")))
            .filter_map(|n| n.label.clone())
            .collect();
        assert_eq!(labels, ["follow pad, iPad", "follow phone, iPhone"], "{tree:#?}");
        cx.simulate_click(pill.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.following), Some(pad));
        let (camera, viewport) = view.read_with(cx, |c, _| (c.camera, c.viewport_size()));
        let expected = Camera::fitted([far], viewport);
        assert!(
            (camera.x - expected.x).abs() < 0.5 && (camera.y - expected.y).abs() < 0.5,
            "{camera:?} vs {expected:?}"
        );
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            tree.iter().any(|n| n.label.as_deref() == Some("following pad, iPad")),
            "{tree:#?}"
        );
        // A second click stops following; a client that leaves loses its pill.
        let pill = cx.debug_bounds("here-pad").expect("still here");
        cx.simulate_click(pill.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.following), None);
        view.update(cx, |c, cx| c.apply_sync(presence(pad, ClientKind::IPad, "pad", None), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("here-pad").is_none());
        assert!(cx.debug_bounds("here-phone").is_some());
    }

    /// Another client's pointing is a toast naming them and the card; its click goes to the
    /// card and takes the toast; an unclicked toast goes by itself; a pointing at a card this
    /// canvas does not have, or this client's own echo, is no toast at all.
    #[gpui::test]
    fn a_pointing_offers_the_card_until_a_click_or_the_clock(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let far = Rect { x: 5_000.0, y: 3_000.0, w: 400.0, h: 300.0 };
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: "Ship it\nby friday".to_owned() },
            rect: far,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        let pad = ClientId::new();
        let pointed = |client: ClientId, item: ItemId| CanvasSync::Pointed {
            client,
            name: "pad".to_owned(),
            item,
        };
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
            c.apply_sync(pointed(me, id), cx);
            c.apply_sync(pointed(pad, ItemId::new()), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("pointed").is_none(), "my echo and an unknown card: nothing");
        assert!(!view.read_with(cx, |c, _| c.on_screen(far)), "the note starts off screen");

        view.update(cx, |c, cx| c.apply_sync(pointed(pad, id), cx));
        cx.run_until_parked();
        let toast = cx.debug_bounds("pointed").expect("the toast");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            tree.iter().any(|n| n.role == "Button"
                && n.label.as_deref() == Some("pad points at Ship it, go there")),
            "{tree:#?}"
        );
        cx.simulate_click(toast.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("pointed").is_none(), "the click takes the toast");
        assert_eq!(view.read_with(cx, |c, _| c.active), Some(id));
        assert!(view.read_with(cx, |c, _| c.on_screen(far)), "and goes to the card");

        view.update(cx, |c, cx| c.apply_sync(pointed(pad, id), cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("pointed").is_some());
        cx.executor().advance_clock(POINT_FOR);
        cx.run_until_parked();
        assert!(cx.debug_bounds("pointed").is_none(), "gone by itself");
    }

    /// ⌘⇧O (and the palette's line) tells the host which card is active and says who was
    /// pointed; nothing active says nothing, and nobody else here sends nothing and says so.
    #[gpui::test]
    fn pointing_the_others_names_the_active_card(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let status = |cx: &mut VisualTestContext| {
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            tree.iter().find(|n| n.role == "Status").and_then(|n| n.label.clone())
        };
        let points = |rx: &mut mpsc::Receiver<ClientMsg>| {
            drain(rx)
                .into_iter()
                .filter_map(|m| match m {
                    ClientMsg::Point { item } => Some(item),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        assert!(points(&mut rx).is_empty(), "nothing active");
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: "here".to_owned() },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
            c.active = Some(id);
        });
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        assert!(points(&mut rx).is_empty(), "nobody to point");
        assert_eq!(status(cx).as_deref(), Some("nobody else is here"));
        let pill = selector("point", id);
        assert!(cx.debug_bounds(pill).is_none(), "alone: no pill either");
        let pad = ClientId::new();
        view.update(cx, |c, cx| {
            c.apply_sync(
                CanvasSync::Presence {
                    client: pad,
                    kind: ClientKind::IPad,
                    name: "pad".to_owned(),
                    view: Some(SHELL),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds(pill).expect("somebody here: the active card's pill");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(points(&mut rx), vec![id], "the pill points");
        assert_eq!(status(cx).as_deref(), Some("pointed pad at here"));
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        assert_eq!(points(&mut rx), vec![id], "so does the key");
        view.update(cx, |c, cx| {
            c.apply_sync(
                CanvasSync::Presence {
                    client: ClientId::new(),
                    kind: ClientKind::Mac,
                    name: "desk".to_owned(),
                    view: Some(SHELL),
                },
                cx,
            );
        });
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("p o i n t space t h e");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(points(&mut rx), vec![id], "the palette's line does the same");
        assert_eq!(status(cx).as_deref(), Some("pointed 2 others at here"));
        cx.executor().advance_clock(POINT_FOR);
        cx.run_until_parked();
        assert_eq!(status(cx), None, "gone by itself");
    }

    /// ⌘] and ⌘[ walk the cards in reading order — rows by their top edge, left to right —
    /// wrapping at both ends, revealing each; nothing active starts at the first or the last.
    /// The first shell comes with the attach, before a frame has measured the viewport: the
    /// phone still gets a phone-sized card, fitted on the first frame (and the host asked to
    /// place it so), not a desktop one hanging off its right edge.
    #[gpui::test]
    fn a_shell_placed_before_the_first_frame_is_fitted_on_it(cx: &mut TestAppContext) {
        let me = ClientId::new();
        let (tx, mut rx) = mpsc::channel(64);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let factory: ScreenFactory =
                Arc::new(|_stream, _codec| panic!("this canvas opens no screens"));
            let mut view = CanvasView::new(me, tx, Vec::new(), factory, Theme::default(), cx);
            view.set_animation(false);
            window.focus(&view.focus, cx);
            view
        });
        let phone = (393.0, 852.0);
        cx.simulate_resize(size(px(phone.0), px(phone.1)));
        // Back to the state before the first frame: the viewport unmeasured (a headless
        // window draws on creation; a phone attaches before it draws).
        view.update(cx, |c, _| c.viewport = (point(px(0.0), px(0.0)), size(px(1.0), px(1.0))));
        let session = SessionId::new();
        let id = host_opens_in(&view, cx, session, me, SHELL, 1, Where::loose("/tmp/work"));
        let (rect, max) = view.read_with(cx, |c, _| {
            (c.items().into_iter().find(|i| i.id == id).unwrap().rect, c.viewport_max())
        });
        let (max_w, _) = max.expect("the frame measured the phone");
        assert!(
            (rect.w - max_w).abs() < f32::EPSILON && rect.w < SHELL.w && rect.h >= SHELL.h,
            "{rect:?} {max_w}"
        );
        let placed = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| matches!(msg, ClientMsg::Canvas(CanvasOp::Place { .. })))
            .count();
        assert_eq!(placed, 1, "the host is asked to place it at the phone's size");
        let camera = view.read_with(cx, |c, _| c.camera());
        let right = (rect.x + rect.w - camera.x) * camera.zoom;
        assert!(right <= phone.0, "revealed inside the phone: {camera:?} {rect:?}");
    }

    /// A note or a file card this client opens is its default size on a desktop and, on a
    /// phone, no wider or taller than the viewport leaves — a 560 pt file card on a 393 pt
    /// phone had its find bar off the right edge.
    #[gpui::test]
    fn a_phone_opens_notes_and_file_cards_that_fit_it(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        let sizes = |cx: &mut VisualTestContext| {
            view.read_with(cx, |c, _| {
                let of = |wanted: fn(&ItemKind) -> bool| {
                    c.items().into_iter().find(|i| wanted(&i.kind)).map(|i| (i.rect.w, i.rect.h))
                };
                (
                    of(|k| matches!(k, ItemKind::Note { .. })),
                    of(|k| matches!(k, ItemKind::File { .. })),
                )
            })
        };
        view.update_in(cx, |c, window, cx| {
            c.new_note(&NewNote, window, cx);
            c.open_file("/tmp/work/a.txt", None, cx);
        });
        cx.run_until_parked();
        assert_eq!(sizes(cx), (Some(NOTE_SIZE), Some(FILE_SIZE)), "a desktop keeps the defaults");
        view.update_in(cx, |c, _window, _cx| {
            let ids: Vec<ItemId> = c.items().into_iter().map(|i| i.id).collect();
            for id in ids {
                c.propose(CanvasOp::Remove(id));
            }
        });

        let phone = (393.0, 852.0);
        cx.simulate_resize(size(px(phone.0), px(phone.1)));
        cx.run_until_parked();
        view.update_in(cx, |c, window, cx| {
            c.new_note(&NewNote, window, cx);
            c.open_file("/tmp/work/b.txt", None, cx);
        });
        cx.run_until_parked();
        let (note, file) = sizes(cx);
        let max_w = snap(2.0_f32.mul_add(-GAP, phone.0));
        let (note, file) = (note.expect("the note"), file.expect("the file card"));
        assert!(note.0 <= max_w && note.0 >= MIN_ITEM, "the note fits the phone: {note:?}");
        assert!(file.0 <= max_w && file.0 >= MIN_ITEM, "the file card fits the phone: {file:?}");
        assert!(file.0 < FILE_SIZE.0, "cut down from the default: {file:?}");

        // A display added from the host keeps its shape inside the phone too.
        view.update_in(cx, |c, _window, cx| {
            c.add_first_display(cx);
            c.screen_event(
                ScreenEvent::Listing {
                    windows: Vec::new(),
                    displays: vec![DisplayInfo {
                        id: 1,
                        w: 1920.0,
                        h: 1080.0,
                        scale: 2.0,
                        hz: 60.0,
                        hdr: false,
                    }],
                },
                cx,
            );
        });
        cx.run_until_parked();
        let display = view.read_with(cx, |c, _| {
            c.items()
                .into_iter()
                .find(|i| matches!(i.kind, ItemKind::Display { .. }))
                .map(|i| (i.rect.w, i.rect.h))
        });
        let display = display.expect("the display");
        assert!(display.0 <= max_w, "the display fits the phone: {display:?}");
        let shape = (display.1 - TITLE_H) / display.0;
        assert!((shape - 1080.0 / 1920.0).abs() < 0.05, "its shape is kept: {display:?}");
    }

    /// The palette and the picker are their desktop width on a desktop and, on a phone, what
    /// the screen leaves after a margin each side; neither runs off the edge.
    #[gpui::test]
    fn the_palette_and_the_picker_fit_the_screen_they_are_on(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        let within = |cx: &mut VisualTestContext, selector: &'static str, width: f32| {
            let b = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is up"));
            let (left, right) = (f32::from(b.origin.x), f32::from(b.right()));
            assert!(left >= 0.0 && right <= width, "{selector} on {width} wide: {b:?}");
            f32::from(b.size.width)
        };
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        assert!((within(cx, "palette", VIEWPORT.0) - 520.0).abs() < 0.5, "the desktop width");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        view.update_in(cx, |c, _window, cx| {
            c.screen_event(ScreenEvent::Listing { windows: Vec::new(), displays: Vec::new() }, cx);
        });
        cx.run_until_parked();
        assert!((within(cx, "picker", VIEWPORT.0) - 560.0).abs() < 0.5, "the desktop width");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("picker").is_none(), "Esc closed the picker");

        let phone = (393.0, 852.0);
        cx.simulate_resize(size(px(phone.0), px(phone.1)));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        assert!(within(cx, "palette", phone.0) < phone.0, "a margin each side");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-o");
        cx.run_until_parked();
        view.update_in(cx, |c, _window, cx| {
            c.screen_event(ScreenEvent::Listing { windows: Vec::new(), displays: Vec::new() }, cx);
        });
        cx.run_until_parked();
        assert!(within(cx, "picker", phone.0) < phone.0, "a margin each side");
    }

    /// ⌘⌥-arrows walk to the nearest card whose centre lies in that direction's cone; a
    /// direction with nothing there leaves the focus where it is, and nothing active starts
    /// at the first card.
    #[gpui::test]
    fn the_cards_are_walked_by_direction(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let note = |x: f32, y: f32| CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: format!("{x},{y}") },
            rect: Rect { x, y, w: 200.0, h: 100.0 },
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        // `far` is to the right too, but `right` is nearer; `below_right` is under `right`,
        // not under `first` (its centre is outside first's downward cone).
        let (first, right, far, below_right) =
            (note(0.0, 0.0), note(300.0, 0.0), note(1300.0, 40.0), note(750.0, 700.0));
        let ids = [first.id, right.id, far.id, below_right.id];
        view.update(cx, |c, cx| {
            for (version, item) in (1_u64..).zip([first, right, far, below_right]) {
                c.apply_sync(CanvasSync::Delta { version, by: me, op: CanvasOp::Upsert(item) }, cx);
            }
        });
        cx.run_until_parked();
        let active = |cx: &mut VisualTestContext| view.read_with(cx, |c, _| c.active);
        let step = |cx: &mut VisualTestContext, key: &str| {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
        };
        step(cx, "cmd-alt-right");
        assert_eq!(active(cx), Some(ids[0]), "nothing active: the first card");
        step(cx, "cmd-alt-right");
        assert_eq!(active(cx), Some(ids[1]), "the nearer of the two to the right");
        step(cx, "cmd-alt-right");
        assert_eq!(active(cx), Some(ids[2]));
        step(cx, "cmd-alt-right");
        assert_eq!(active(cx), Some(ids[2]), "nothing further right");
        step(cx, "cmd-alt-left");
        assert_eq!(active(cx), Some(ids[1]));
        step(cx, "cmd-alt-down");
        assert_eq!(active(cx), Some(ids[3]));
        step(cx, "cmd-alt-up");
        assert_eq!(active(cx), Some(ids[1]));
        step(cx, "cmd-alt-left");
        assert_eq!(active(cx), Some(ids[0]));
        step(cx, "cmd-alt-down");
        assert_eq!(active(cx), Some(ids[0]), "below-right is outside the downward cone");
    }

    #[gpui::test]
    fn the_cards_are_walked_in_reading_order(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let note = |x: f32, y: f32| CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: format!("{x},{y}") },
            rect: Rect { x, y, w: 200.0, h: 100.0 },
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        // Upserted out of order: the walk is by position, never by arrival or z.
        let (right, first, below) = (note(300.0, 0.0), note(0.0, 0.0), note(0.0, 4_000.0));
        let order = [first.id, right.id, below.id];
        let last = below.id;
        view.update(cx, |c, cx| {
            for (version, item) in [1_u64, 2, 3].into_iter().zip([right, first, below]) {
                c.apply_sync(CanvasSync::Delta { version, by: me, op: CanvasOp::Upsert(item) }, cx);
            }
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.reading_order()), order);
        let active = |cx: &mut VisualTestContext| view.read_with(cx, |c, _| c.active);
        assert_eq!(active(cx), None);
        for expected in order.iter().chain([&order[0]]) {
            cx.simulate_keystrokes("cmd-]");
            cx.run_until_parked();
            assert_eq!(active(cx).as_ref(), Some(expected));
        }
        let far = Rect { x: 0.0, y: 4_000.0, w: 200.0, h: 100.0 };
        assert!(!view.read_with(cx, |c, _| c.on_screen(far)), "back at the top");
        cx.simulate_keystrokes("cmd-[");
        cx.run_until_parked();
        assert_eq!(active(cx), Some(last));
        assert!(view.read_with(cx, |c, _| c.on_screen(far)), "revealed");
        view.update(cx, |c, cx| {
            c.active = None;
            cx.notify();
        });
        cx.simulate_keystrokes("cmd-[");
        cx.run_until_parked();
        assert_eq!(active(cx), Some(last), "nothing active: the last");
    }

    /// and ↩ follows: the camera goes to their viewport wherever it is.
    #[gpui::test]
    fn the_palette_follows_another_client(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let pad = ClientId::new();
        let far = Rect { x: 5_000.0, y: 3_000.0, w: 400.0, h: 700.0 };
        view.update(cx, |c, cx| {
            c.apply_sync(
                CanvasSync::Presence {
                    client: pad,
                    kind: ClientKind::IPad,
                    name: "pad".to_owned(),
                    view: Some(far),
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("follow-pad").is_none(), "off screen: no tag to click");
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("f o l l o w space p a d");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<String> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.clone())
            .collect();
        assert_eq!(options, ["Follow pad iPad"], "{tree:#?}");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none());
        assert_eq!(view.read_with(cx, |c, _| c.following), Some(pad));
        let (camera, viewport) = view.read_with(cx, |c, _| (c.camera, c.viewport_size()));
        let expected = Camera::fitted([far], viewport);
        assert!(
            (camera.x - expected.x).abs() < 0.5 && (camera.y - expected.y).abs() < 0.5,
            "{camera:?} vs {expected:?}"
        );
    }

    /// The host hears where this client looks once the viewport rests, and again only when it
    /// moves: a pan of many frames is one message.
    #[gpui::test]
    fn a_resting_viewport_is_told_to_the_host_once(cx: &mut TestAppContext) {
        let (view, mut rx, _me, cx) = canvas(cx);
        let looks = |rx: &mut mpsc::Receiver<ClientMsg>| {
            drain(rx)
                .into_iter()
                .filter_map(|m| match m {
                    ClientMsg::Look { view } => Some(view),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert!(looks(&mut rx).is_empty(), "nothing before the wait");
        cx.executor().advance_clock(LOOK_EVERY);
        cx.run_until_parked();
        let first = looks(&mut rx);
        let expected = view.read_with(cx, |c, _| c.view_rect());
        assert_eq!(first, vec![expected], "the viewport as measured");
        assert!(expected.is_some_and(|r| (r.w - VIEWPORT.0).abs() < f32::EPSILON), "{expected:?}");

        for _ in 0..5 {
            view.update(cx, |c, cx| {
                c.camera.pan(-20.0, 0.0);
                cx.notify();
            });
            cx.run_until_parked();
        }
        assert!(looks(&mut rx).is_empty(), "a pan in progress says nothing");
        cx.executor().advance_clock(LOOK_EVERY);
        cx.run_until_parked();
        let moved = looks(&mut rx);
        assert_eq!(moved.len(), 1, "one message for the whole pan: {moved:?}");
        let start = expected.map_or(0.0, |r| r.x);
        assert!(moved[0].is_some_and(|r| (r.x - start - 100.0).abs() < 1.0), "{moved:?}");

        view.update(cx, |_c, cx| cx.notify());
        cx.run_until_parked();
        cx.executor().advance_clock(LOOK_EVERY);
        cx.run_until_parked();
        assert!(looks(&mut rx).is_empty(), "an unmoved viewport is not repeated");
    }

    fn drain(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            out.push(msg);
        }
        out
    }

    fn terminal_focused(
        view: &Entity<CanvasView>,
        cx: &mut VisualTestContext,
        session: SessionId,
    ) -> bool {
        cx.update(|window, cx| {
            let c = view.read(cx);
            c.terminal(session).is_some_and(|t| t.read(cx).focus_handle(cx).is_focused(window))
        })
    }

    fn canvas_focused(view: &Entity<CanvasView>, cx: &mut VisualTestContext) -> bool {
        cx.update(|window, cx| view.read(cx).focus.is_focused(window))
    }

    /// One finger down and up, as the phone delivers it (`PlatformInput::Touch`, recognized by
    /// GPUI into a tap): on an item it activates the item and gives it the keyboard, the same
    /// as a click; on bare canvas the keyboard stays with the active terminal (the canvas hands
    /// it back on the next frame, see `keep_focus_rendered`).
    /// A finger tap (touch Started then Ended in place) through gpui's touch recognizer lands
    /// as a click: on an item it activates it; on bare canvas the keyboard stays where it was.
    #[gpui::test]
    fn a_finger_tap_activates_what_it_lands_on(cx: &mut TestAppContext) {
        use gpui::{TouchEvent, TouchId, TouchPhase};

        let (view, _rx, me, cx) = canvas(cx);
        let (first, second) = (SessionId::new(), SessionId::new());
        // Two shells side by side, both inside the test viewport, so opening the second
        // reveals nothing and pans nothing.
        let left = Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 };
        let right = Rect { x: 440.0, ..left };
        let a = host_opens(&view, cx, first, me, left, 1);
        let b = host_opens(&view, cx, second, me, right, 2);
        assert!(terminal_focused(&view, cx, second), "the newest shell has the keyboard");
        let tap = |cx: &mut VisualTestContext, at: Point<Pixels>| {
            for phase in [TouchPhase::Started, TouchPhase::Ended] {
                cx.simulate_event(TouchEvent {
                    id: TouchId(1),
                    phase,
                    position: at,
                    predicted_position: None,
                    force: None,
                });
            }
        };
        let bounds_a = cx.debug_bounds(selector("item", a)).expect("the first item");
        let bounds_b = cx.debug_bounds(selector("item", b)).expect("the second item");
        assert!(bounds_a.right() < bounds_b.left(), "{bounds_a:?} is left of {bounds_b:?}");
        tap(cx, bounds_a.center());
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(a), "a tap activates");
        assert!(terminal_focused(&view, cx, first), "and focuses the shell under it");
        let bare = point(bounds_a.center().x, bounds_a.bottom() + px(40.0));
        tap(cx, bare);
        assert!(terminal_focused(&view, cx, first), "bare canvas leaves the keyboard where it was");
        assert!(!canvas_focused(&view, cx));
        tap(cx, bounds_b.center());
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(b));
        assert!(terminal_focused(&view, cx, second));
    }

    /// A one-finger drag on bare canvas through gpui's touch recognizer: the content follows
    /// the finger on the finger's axis.
    #[gpui::test]
    fn a_finger_pan_on_bare_canvas_moves_the_content_with_the_finger(cx: &mut TestAppContext) {
        use gpui::{TouchEvent, TouchId, TouchPhase};

        let (view, _rx, me, cx) = canvas(cx);
        let a = host_opens(
            &view,
            cx,
            SessionId::new(),
            me,
            Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 },
            1,
        );
        let before = cx.debug_bounds(selector("item", a)).expect("the item");
        let start = point(before.center().x, before.bottom() + px(40.0));
        let touch = |cx: &mut VisualTestContext, phase: TouchPhase, at: Point<Pixels>| {
            cx.simulate_event(TouchEvent {
                id: TouchId(1),
                phase,
                position: at,
                predicted_position: None,
                force: None,
            });
        };
        touch(cx, TouchPhase::Started, start);
        for step in 1_u8..=6 {
            touch(cx, TouchPhase::Moved, point(start.x - px(20.0 * f32::from(step)), start.y));
        }
        let end = point(start.x - px(120.0), start.y);
        touch(cx, TouchPhase::Ended, end);
        let after = cx.debug_bounds(selector("item", a)).expect("the item");
        eprintln!("DBG before={before:?} after={after:?}");
        assert!(
            (f32::from(after.origin.x - before.origin.x) + 120.0).abs() < 2.0,
            "{before:?} → {after:?}"
        );
        assert_eq!(after.origin.y, before.origin.y);
    }

    /// A finger pan over a shell (gpui's touch recognizer turns it into scroll events) scrolls
    /// the shell's history while there is some that way, and the canvas holds still; a shell
    /// with nothing to scroll lets the same pan move the canvas.
    #[gpui::test]
    fn a_finger_pan_over_a_shell_scrolls_its_history_before_the_canvas(cx: &mut TestAppContext) {
        use gpui::{TouchEvent, TouchId, TouchPhase};

        let (view, _rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let a = host_opens(&view, cx, session, me, Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }, 1);
        let touch = |cx: &mut VisualTestContext, phase: TouchPhase, at: Point<Pixels>| {
            cx.simulate_event(TouchEvent {
                id: TouchId(1),
                phase,
                position: at,
                predicted_position: None,
                force: None,
            });
        };
        let pan_down = |cx: &mut VisualTestContext, start: Point<Pixels>| {
            touch(cx, TouchPhase::Started, start);
            for step in 1_u8..=6 {
                touch(cx, TouchPhase::Moved, point(start.x, start.y + px(20.0 * f32::from(step))));
            }
            touch(cx, TouchPhase::Ended, point(start.x, start.y + px(120.0)));
            cx.run_until_parked();
        };
        let offset = |cx: &mut VisualTestContext| {
            view.read_with(cx, |c, cx| {
                c.terminal(session).map_or(0, |t| t.read(cx).state().view_offset())
            })
        };
        // No history: the pan is the canvas's.
        let before = cx.debug_bounds(selector("item", a)).expect("the item");
        pan_down(cx, before.center());
        let after = cx.debug_bounds(selector("item", a)).expect("the item");
        assert!(
            (f32::from(after.origin.y - before.origin.y) - 120.0).abs() < 2.0,
            "nothing to scroll: the canvas moved {before:?} → {after:?}"
        );
        assert_eq!(offset(cx), 0);
        // With history above, a finger pulling down scrolls the shell into it and the canvas
        // holds still.
        view.update_in(cx, |c, _window, cx| {
            let TermEvent::Frame(mut f) = frame(&["$ echo hi", "hi", ""]) else { return };
            f.first_visible_line = LineIndex(50);
            f.total_lines = 53;
            c.term_event(session, TermEvent::Frame(f), cx);
        });
        cx.run_until_parked();
        let before = cx.debug_bounds(selector("item", a)).expect("the item");
        pan_down(cx, before.center());
        let after = cx.debug_bounds(selector("item", a)).expect("the item");
        assert!(offset(cx) > 0, "the shell scrolled into its history");
        assert_eq!(after.origin, before.origin, "and the canvas stayed");
    }

    #[gpui::test]
    fn cmd_n_asks_the_host_for_a_shell_and_its_echo_places_and_focuses_it(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        assert!(cx.debug_bounds("canvas").is_some(), "the canvas is drawn");
        assert!(view.read_with(cx, |c, _| c.minimap.is_none()), "no minimap when empty");

        cx.simulate_keystrokes("cmd-n");
        let sent = drain(&mut rx);
        assert!(
            matches!(sent.as_slice(), [ClientMsg::OpenSession(OpenSession { cwd: None, .. })]),
            "no shell to inherit from: {sent:?}"
        );

        let session = SessionId::new();
        let id = host_opens_in(&view, cx, session, me, SHELL, 1, Where::loose("/tmp/work"));
        // Our own upsert: the item is drawn at its rect, active, the terminal takes the
        // keyboard, it attached itself to the host and the minimap appears.
        let bounds = cx.debug_bounds(selector("item", id)).expect("item drawn");
        assert_eq!(bounds.size, size(px(SHELL.w), px(SHELL.h)));
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(id));
        assert!(terminal_focused(&view, cx, session));
        assert!(view.read_with(cx, |c, _| c.minimap.is_some()));
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Term { session: s, req: TermRequest::Attach { .. } } if *s == session
            )),
            "{sent:?}"
        );
        // Its colours follow the attach, and again when the theme changes them.
        let dark = Theme::default().terminal.wire();
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Term { session: s, req: TermRequest::Colors(c) } if *s == session && *c == dark
            )),
            "{sent:?}"
        );
        view.update(cx, |c, cx| c.set_theme(Theme::new(slopty_theme::Variant::Light), cx));
        let light = Theme::new(slopty_theme::Variant::Light).terminal.wire();
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Term { session: s, req: TermRequest::Colors(c) } if *s == session && *c == light
            )),
            "{sent:?}"
        );
        view.update(cx, |c, cx| c.set_theme(Theme::default(), cx));
        let _back = drain(&mut rx);

        // A shell beside a shell starts where that shell is (⌘⇧T's agent too).
        cx.simulate_keystrokes("cmd-n");
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                    if cwd == "/tmp/work" && command.is_empty()
            ),
            "{sent:?}"
        );
        cx.simulate_keystrokes("cmd-shift-t");
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                    if cwd == "/tmp/work" && command == &[AGENT_COMMAND.to_owned()]
            ),
            "{sent:?}"
        );
    }

    #[gpui::test]
    fn typed_keys_reach_the_focused_terminal_and_host_frames_fill_its_rows(
        cx: &mut TestAppContext,
    ) {
        let (view, mut rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        host_opens(&view, cx, session, me, SHELL, 1);
        drain(&mut rx);

        cx.simulate_keystrokes("a");
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::Term { session: s, req: TermRequest::Key(_) }] if *s == session
            ),
            "{sent:?}"
        );

        view.update_in(cx, |c, _window, cx| {
            c.term_event(session, frame(&["$ echo hi", "hi", ""]), cx);
        });
        cx.run_until_parked();
        let rows = view.read_with(cx, |c, cx| c.terminal(session).unwrap().read(cx).rows());
        assert_eq!(rows, ["$ echo hi", "hi", ""]);
    }

    /// Below `CARD_ZOOM` the grids are not drawn. Focus must move to the canvas or GPUI drops
    /// every keystroke aimed at the undrawn terminal (the app self-test found ⌘W dead after
    /// ⌘1); ⌘0 gives the active terminal the keyboard back.
    #[gpui::test]
    fn zooming_out_to_cards_hands_the_keyboard_to_the_canvas_and_back(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let a = SessionId::new();
        let b = SessionId::new();
        let first = host_opens(&view, cx, a, me, SHELL, 1);
        let second = host_opens(&view, cx, b, me, Rect { x: 1500.0, ..SHELL }, 2);
        drain(&mut rx);
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(second));
        assert!(terminal_focused(&view, cx, b));

        cx.simulate_keystrokes("cmd-1");
        let zoom = view.read_with(cx, |c, _| c.zoom());
        assert!(zoom < CARD_ZOOM, "fit-all zoom {zoom} is card zoom");
        assert!(canvas_focused(&view, cx), "the canvas holds the keyboard in card mode");
        assert!(!terminal_focused(&view, cx, b));

        let card = cx.debug_bounds(selector("item", first)).expect("card drawn");
        cx.simulate_click(card.center(), Modifiers::default());
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(first));

        cx.simulate_keystrokes("cmd-0");
        let zoom = view.read_with(cx, |c, _| c.zoom());
        assert!((zoom - 1.0).abs() < 1e-3, "{zoom}");
        assert!(terminal_focused(&view, cx, a), "the active terminal took the keyboard back");

        cx.simulate_keystrokes("cmd-w");
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Canvas(CanvasOp::Remove(id)) if *id == first
            )),
            "{sent:?}"
        );
    }

    /// The colour of the frame painted around item `id`, read from the scene, not from pixels.
    fn border_of(cx: &mut VisualTestContext, id: ItemId) -> Option<gpui::Hsla> {
        let bounds = cx.debug_bounds(selector("item", id))?;
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        let near = |scaled: gpui::ScaledPixels, logical: Pixels| {
            f32::from(logical).mul_add(-scale, scaled.0).abs() < 1.0
        };
        quads
            .iter()
            .find(|q| {
                near(q.bounds.origin.x, bounds.origin.x)
                    && near(q.bounds.origin.y, bounds.origin.y)
                    && near(q.bounds.size.width, bounds.size.width)
                    && q.border_widths.top.0 > 0.0
            })
            .map(|q| q.border_color)
    }

    /// The solid fill painted over the whole window (the canvas surface), if any.
    fn window_fill(cx: &mut VisualTestContext) -> Option<gpui::Hsla> {
        let (scale, quads) = cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
        quads
            .iter()
            .find(|q| {
                VIEWPORT.0.mul_add(-scale, q.bounds.size.width.0).abs() < 1.0
                    && VIEWPORT.1.mul_add(-scale, q.bounds.size.height.0).abs() < 1.0
                    && q.border_widths.top.0 == 0.0
            })
            .and_then(|q| q.background.as_solid())
    }

    /// The active item's frame is painted in the accent colour; the others in the border
    /// colour. Read from the scene, not from pixels.
    #[gpui::test]
    fn the_active_item_is_painted_with_the_accent_border(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let a = SessionId::new();
        let b = SessionId::new();
        let first = host_opens(&view, cx, a, me, SHELL, 1);
        let second = host_opens(&view, cx, b, me, Rect { x: 760.0, ..SHELL }, 2);
        let theme = Theme::default();
        assert_eq!(border_of(cx, second), Some(hsla(theme.surfaces.accent)));
        assert_eq!(border_of(cx, first), Some(hsla(theme.surfaces.border)));
    }

    /// An agent waiting on the human outlines its item in the warn tone, over the accent of
    /// the active item; a theme swap to the light variant repaints outline and canvas from the
    /// light tokens, so both tables are wired through.
    #[gpui::test]
    fn a_blocked_agent_outlines_its_item_in_the_warn_tone_in_both_variants(
        cx: &mut TestAppContext,
    ) {
        let (view, _rx, me, cx) = canvas(cx);
        let a = SessionId::new();
        let item = host_opens(&view, cx, a, me, SHELL, 1);
        let dark = Theme::default();
        assert_eq!(border_of(cx, item), Some(hsla(dark.surfaces.accent)), "active, no agent");
        assert_eq!(window_fill(cx), Some(hsla(dark.surfaces.canvas)));

        let blocked = AgentEvent {
            session: a,
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            agent_session: None,
            detail: None,
            attention: true,
            source: AgentSource::Hook,
        };
        view.update(cx, |v, cx| v.agent_event(blocked.clone(), cx));
        cx.run_until_parked();
        assert_eq!(border_of(cx, item), Some(hsla(dark.surfaces.warn)));

        let light = Theme::new(slopty_theme::Variant::Light);
        view.update(cx, |v, cx| v.set_theme(light.clone(), cx));
        cx.run_until_parked();
        assert_eq!(border_of(cx, item), Some(hsla(light.surfaces.warn)));
        assert_eq!(window_fill(cx), Some(hsla(light.surfaces.canvas)));
        assert_ne!(light.surfaces.warn, dark.surfaces.warn, "the light table has its own tone");

        // Working again: the outline is the accent once more (this item is still active).
        let working = AgentEvent { status: AgentStatus::Working, ..blocked };
        view.update(cx, |v, cx| v.agent_event(working, cx));
        cx.run_until_parked();
        assert_eq!(border_of(cx, item), Some(hsla(light.surfaces.accent)));
    }

    /// `inner` lies within `outer` (a pixel of slack for rounding).
    fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
        let slack = px(1.0);
        inner.origin.x + slack >= outer.origin.x
            && inner.origin.y + slack >= outer.origin.y
            && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + slack
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + slack
    }

    #[gpui::test]
    fn title_bar_pills_shrink_with_the_zoom_and_stay_inside_the_bar(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let a = SessionId::new();
        let item = host_opens(&view, cx, a, me, SHELL, 1);
        let blocked = AgentEvent {
            session: a,
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            agent_session: None,
            detail: None,
            attention: true,
            source: AgentSource::Hook,
        };
        view.update(cx, |v, cx| v.agent_event(blocked, cx));
        cx.run_until_parked();

        let parts = ["badge", "go"];
        let title_full = cx.debug_bounds(selector("title", item)).expect("title bar drawn");
        assert_eq!(title_full.size.height, px(TITLE_H));
        let full: Vec<_> =
            parts.iter().map(|p| cx.debug_bounds(selector(p, item)).expect(p)).collect();
        for (part, b) in parts.iter().zip(&full) {
            assert!(within(*b, title_full), "{part} at zoom 1: {b:?} in {title_full:?}");
        }

        // Zoom 0.6 is the last zoom before cards: the bar is 16.8 pt and every control scales
        // with it instead of keeping full-size text and padding.
        view.update(cx, |v, cx| v.zoom_by(0.6, cx));
        cx.run_until_parked();
        let zoom = view.read_with(cx, |c, _| c.zoom());
        assert!((CARD_ZOOM..0.61).contains(&zoom), "{zoom}");
        let title = cx.debug_bounds(selector("title", item)).expect("title bar drawn");
        // Layout rounds to whole pixels: 16.8 pt draws as 17.
        assert!(TITLE_H.mul_add(-0.6, f32::from(title.size.height)).abs() <= 1.0, "{title:?}");
        for (part, before) in parts.iter().zip(&full) {
            let b = cx.debug_bounds(selector(part, item)).expect(part);
            assert!(within(b, title), "{part} at zoom 0.6: {b:?} in {title:?}");
            assert!(b.size.height < before.size.height, "{part} shrank: {b:?} < {before:?}");
        }
    }

    /// Every control in an item's title bar has a role and a label a screen reader can say,
    /// in reading order; Tab from the canvas walks the ring in that order, Enter clicks, and
    /// the focused pill wears the accent ring.
    #[gpui::test]
    fn the_title_bar_is_read_and_tabbed_in_reading_order(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let _item = host_opens(&view, cx, session, me, SHELL, 1);
        view.update(cx, |c, cx| {
            c.agent_event(
                AgentEvent {
                    session,
                    kind: AgentKind::ClaudeCode,
                    status: AgentStatus::Blocked(BlockReason::Permission {
                        tool: "Bash".to_owned(),
                    }),
                    agent_session: None,
                    detail: None,
                    attention: false,
                    source: AgentSource::Hook,
                },
                cx,
            );
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let at = |role: &str, label: &str| {
            tree.iter()
                .position(|n| n.is(role, Some(label)))
                .unwrap_or_else(|| panic!("{role} {label:?} in {tree:#?}"))
        };
        assert!(at("Group", "Canvas") < at("Heading", "terminal shell"), "the canvas first");
        assert!(at("Heading", "terminal shell") < at("Status", "allow? Bash"));
        assert!(at("Status", "allow? Bash") < at("Button", "go"));
        assert!(at("Button", "go") < at("Terminal", "shell"), "the grid after its title bar");
        assert!(at("Terminal", "shell") < at("Image", "Canvas overview"));

        // The terminal holds the keyboard (Tab is the shell's); ⌃Tab enters the ring, then
        // Tab walks it in reading order.
        assert!(terminal_focused(&view, cx, session), "the new shell has the keyboard");
        drain(&mut rx);
        let mut order = Vec::new();
        for step in 0..3 {
            cx.simulate_keystrokes(if step == 0 { "ctrl-tab" } else { "tab" });
            cx.run_until_parked();
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            let focused = tree.iter().find(|n| n.focused).expect("a focused node");
            order.push(focused.label.clone().unwrap_or_default());
            if focused.label.as_deref() == Some("go") {
                // The ring: an accent hairline around the focused pill.
                let (scale, quads) =
                    cx.update(|window, _| (window.scale_factor(), window.painted_quads()));
                let accent = hsla(Theme::default().surfaces.accent);
                let [x, y, ..] = focused.bounds;
                assert!(
                    quads.iter().any(|q| q.border_color == accent
                        && q.border_widths.top.0 > 0.0
                        && x.mul_add(-scale, q.bounds.origin.x.0).abs() < 1.0
                        && y.mul_add(-scale, q.bounds.origin.y.0).abs() < 1.0),
                    "a focus ring around go at {x},{y}"
                );
                break;
            }
        }
        order.retain(|l| l != "take over");
        assert_eq!(order, ["go"], "Tab order");

        // Enter (down, then up) on "go" is the click: the terminal has the keyboard again,
        // and nothing is typed into it — the answer is the human's to give in the TUI.
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: gpui::Keystroke::parse("enter").unwrap() });
        cx.run_until_parked();
        assert!(terminal_focused(&view, cx, session), "the shell has the keyboard");
        let keys: Vec<_> = drain(&mut rx)
            .into_iter()
            .filter(|m| matches!(m, ClientMsg::Term { req: TermRequest::Key(_), .. }))
            .collect();
        assert!(keys.is_empty(), "nothing typed: {keys:?}");
    }

    /// An agent the host attributed without hooks draws the same pill as a hooked one, and
    /// offers `slopty hook install` once beside it.
    #[gpui::test]
    fn an_agent_seen_without_hooks_gets_the_pill_and_offers_the_hooks(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        let item = host_opens(&view, cx, session, me, SHELL, 1);
        assert!(cx.debug_bounds(selector("agent", item)).is_none(), "a shell has no pill");

        // The host saw a `claude` in the foreground and nothing else: idle, from the process.
        let seen = |status: AgentStatus, source: AgentSource| AgentEvent {
            session,
            kind: AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: false,
            source,
        };
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(seen(AgentStatus::Idle, AgentSource::Process), cx);
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("agent", item)).is_some(), "the pill is drawn");
        let offer = cx.debug_bounds(selector("hooks", item)).expect("the hooks offer");
        // The guess and the offer are both spoken: a screen reader hears what the pill says
        // and reaches the offer as a button.
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Status", Some("claude"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Button", Some("install hooks"))), "{tree:#?}");

        // Its title said a turn started: the pill follows, the offer stays.
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(seen(AgentStatus::Working, AgentSource::Title), cx);
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |c, _cx| c.agent(session).map(|a| a.status.clone())),
            Some(AgentStatus::Working)
        );
        assert!(cx.debug_bounds(selector("hooks", item)).is_some());
        // The pill is laid out at its label's width (an auto-sized wrapper around a
        // fill-width leaf once collapsed to the dot and its padding, the word only in the
        // a11y label): wider than dot, gaps and padding by seven glyphs of a small label.
        let pill = cx.debug_bounds(selector("agent", item)).expect("the pill");
        let theme = Theme::default();
        let without_label =
            theme.spacing.xs.mul_add(2.0, theme.spacing.sm.mul_add(2.0, theme.spacing.xxs));
        let width = f32::from(pill.size.width);
        let seven_glyphs = 28.0;
        assert!(width >= without_label + seven_glyphs, "pill {width} px, chrome {without_label}");

        // Taking the offer asks the host to install and never asks again.
        let (x, y) = (offer.center().x, offer.center().y);
        cx.simulate_click(point(x, y), Modifiers::none());
        cx.run_until_parked();
        assert!(
            drain(&mut rx).contains(&ClientMsg::InstallHooks),
            "the host was asked to install the hooks"
        );
        assert!(view.read_with(cx, |c, _cx| c.hooks_offered()));
        assert!(cx.debug_bounds(selector("hooks", item)).is_none(), "offered once");

        // A hook now speaks for the same session: the pill is the host's, the offer is moot.
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(seen(AgentStatus::Blocked(BlockReason::Question), AgentSource::Hook), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("agent", item)).is_some());
        assert!(cx.debug_bounds(selector("hooks", item)).is_none());

        // The agent's process went away: the host says so and the pill goes with it.
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(seen(AgentStatus::None, AgentSource::Process), cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("agent", item)).is_none(), "{item:?}");
    }

    /// A resize that brings a culled item into the viewport draws it on the next frame with
    /// no other event: the frame that learns the new size asks for another.
    #[gpui::test]
    fn a_resize_that_exposes_a_culled_item_draws_it(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let (a, b) = (SessionId::new(), SessionId::new());
        let near = host_opens(&view, cx, a, me, SHELL, 1);
        let beyond = Rect { x: VIEWPORT.0 + 100.0, y: 0.0, ..SHELL };
        let far = host_opens(&view, cx, b, me, beyond, 2);
        view.update(cx, |c, cx| {
            c.activate(near, cx);
            c.camera = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("item", far)).is_none(), "off-screen item culled");

        // Widen the window past the item: nothing else happens, and it is there.
        cx.simulate_resize(size(px(VIEWPORT.0 + 800.0), px(VIEWPORT.1)));
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("item", far)).is_some(), "exposed by the resize");
        assert!(cx.debug_bounds(selector("item", near)).is_some());
    }

    #[gpui::test]
    fn items_outside_the_viewport_are_not_drawn_unless_active(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let (a, b) = (SessionId::new(), SessionId::new());
        let near = host_opens(&view, cx, a, me, SHELL, 1);
        let far_rect = Rect { x: 5000.0, y: 5000.0, ..SHELL };
        let far = host_opens(&view, cx, b, me, far_rect, 2);
        // The newest item is active, so it is drawn even though it is off-screen.
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(far));
        assert!(cx.debug_bounds(selector("item", near)).is_some(), "near item drawn");
        assert!(cx.debug_bounds(selector("item", far)).is_some(), "active item drawn off-screen");

        // Activate the near one and look at it: the far one leaves the tree.
        view.update(cx, |c, cx| {
            c.activate(near, cx);
            c.camera = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("item", near)).is_some());
        assert!(cx.debug_bounds(selector("item", far)).is_none(), "off-screen item culled");

        // Pan the camera onto it and make it active: it is drawn again, and the near one,
        // neither visible nor active, is culled in turn.
        view.update(cx, |c, cx| {
            c.camera.pan(-5000.0, -5000.0);
            c.activate(far, cx);
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds(selector("item", far)).expect("far item drawn after the pan");
        assert!(f32::from(bounds.origin.x) < VIEWPORT.0, "{bounds:?}");
        assert!(cx.debug_bounds(selector("item", near)).is_none(), "near item is off-screen now");
    }

    /// ⌘2 fills the viewport with the active item; ⌘0 goes back to 100 % without losing it.
    /// Neither does anything a mouse could not, but both do it in one key.
    #[gpui::test]
    fn zoom_to_the_active_item_and_back_to_full_size(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        let a = host_opens(&view, cx, SessionId::new(), host, SHELL, 1);
        let far = Rect { x: 4000.0, y: 2500.0, w: 300.0, h: 200.0 };
        let b = host_opens(&view, cx, SessionId::new(), host, far, 2);

        cx.simulate_keystrokes("cmd-1");
        cx.run_until_parked();
        let all = view.read_with(cx, |c, _| c.zoom());
        assert!(all < CARD_ZOOM, "fit-all is zoomed out: {all}");

        let card = cx.debug_bounds(selector("item", b)).expect("the far item is drawn");
        cx.simulate_click(card.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(b));

        cx.simulate_keystrokes("cmd-2");
        cx.run_until_parked();
        let (zoom, on_screen) = view.read_with(cx, |c, _| {
            (c.zoom(), c.camera().to_screen(c.doc.get(b).expect("still there").rect))
        });
        assert!(zoom > all, "the one item fills more of the viewport: {zoom} vs {all}");
        assert!(
            on_screen.x >= 0.0
                && on_screen.y >= 0.0
                && on_screen.x + on_screen.w <= VIEWPORT.0 + 1.0
                && on_screen.y + on_screen.h <= VIEWPORT.1 + 1.0,
            "the item is inside the viewport: {on_screen:?}"
        );

        cx.simulate_keystrokes("cmd-0");
        cx.run_until_parked();
        let (zoom, on_screen) = view.read_with(cx, |c, _| {
            (c.zoom(), c.camera().to_screen(c.doc.get(b).expect("still there").rect))
        });
        assert!((zoom - 1.0).abs() < 1e-3, "100 %: {zoom}");
        let (cx_, cy_) = (on_screen.x + on_screen.w / 2.0, on_screen.y + on_screen.h / 2.0);
        assert!((cx_ - VIEWPORT.0 / 2.0).abs() < 1.0, "still in the middle: {on_screen:?}");
        assert!((cy_ - VIEWPORT.1 / 2.0).abs() < 1.0, "still in the middle: {on_screen:?}");
        assert!(view.read_with(cx, |c, _| c.doc.get(a).is_some()), "nothing moved");
    }

    /// With animation on, a camera action starts a flight instead of jumping: the camera is
    /// After a reconnect the app makes a new canvas and tells it where the old one was: the
    /// camera is there at once, and the card that was active is active again once the
    /// snapshot brings it — or nothing is, when it has gone.
    #[gpui::test]
    fn a_new_canvas_resumes_where_the_last_one_was(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        let kept = ItemId::new();
        let gone = ItemId::new();
        let note = |id: ItemId, z: u32| CanvasItem {
            id,
            kind: ItemKind::Note { text: "here".to_owned() },
            rect: Rect {
                x: 600.0 * f32::from(u8::try_from(z).unwrap_or(1)),
                y: 0.0,
                w: 300.0,
                h: 200.0,
            },
            z,
            group: None,
            sleeping: false,
            name: None,
        };
        let camera = Camera { x: 480.0, y: -120.0, zoom: 0.5 };
        view.update(cx, |c, cx| c.resume_at(camera, Some(kept), cx));
        assert_eq!(view.read_with(cx, |c, _| c.camera()), camera, "at once, no flight");
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), None, "nothing to activate yet");
        view.update(cx, |c, cx| {
            let items = vec![note(kept, 1), note(ItemId::new(), 2)];
            c.apply_sync(CanvasSync::Snapshot { version: 2, items }, cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(kept));
        assert_eq!(view.read_with(cx, |c, _| c.camera()), camera, "the snapshot leaves it");

        // A card gone meanwhile: the camera still, nothing active.
        let (view, _rx, _me, cx) = canvas(cx);
        view.update(cx, |c, cx| c.resume_at(camera, Some(gone), cx));
        view.update(cx, |c, cx| {
            c.apply_sync(CanvasSync::Snapshot { version: 2, items: vec![note(kept, 1)] }, cx);
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), None);
        assert_eq!(view.read_with(cx, |c, _| c.camera()), camera);
    }

    /// still where it was and knows where it is going. How it gets there is
    /// `slopty_client::canvas`'s flight, which has a clock of its own in its own tests.
    #[gpui::test]
    fn a_camera_move_is_a_flight(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        host_opens(&view, cx, SessionId::new(), host, SHELL, 1);
        let before = view.read_with(cx, |c, _| c.camera());
        view.update_in(cx, |c, _window, cx| {
            c.set_animation(true);
            cx.notify();
        });

        cx.simulate_keystrokes("cmd-1");
        let (now, target) = view.read_with(cx, |c, _| (c.camera(), c.flying_to()));
        let target = target.expect("a flight is in progress");
        assert_ne!(target, before, "it is going somewhere");
        assert!(
            now == before || (now.zoom - target.zoom).abs() > f32::EPSILON,
            "the camera has not jumped to the target: {now:?}"
        );
    }

    /// ⌘⇧R puts each repository's shells in their own block, most recent on the left, and
    /// leaves a heading over each one that a screen reader can read.
    #[gpui::test]
    fn arrange_gathers_the_shells_of_a_repository(cx: &mut TestAppContext) {
        let (view, mut rx, host, cx) = canvas(cx);
        let scattered = [
            Rect { x: 1200.0, y: 30.0, w: 300.0, h: 200.0 },
            Rect { x: 40.0, y: 900.0, w: 300.0, h: 200.0 },
            Rect { x: 2200.0, y: 400.0, w: 300.0, h: 200.0 },
        ];
        let root = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            scattered[0],
            1,
            Where::loose("/w/app"),
        );
        let deep = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            scattered[1],
            2,
            Where::loose("/w/app/src"),
        );
        let other = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            scattered[2],
            3,
            Where::loose("/w/tools"),
        );
        while rx.try_recv().is_ok() {}

        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();

        let rects = view.read_with(cx, |c, _| {
            [root, deep, other].map(|id| c.doc.get(id).expect("still there").rect)
        });
        for (moved, was) in rects.iter().zip(scattered) {
            assert_ne!((moved.x, moved.y), (was.x, was.y), "everything was tidied");
            assert!((moved.w - was.w).abs() < f32::EPSILON, "sizes are kept");
        }
        // The two shells of one repository share a row, and the repository raised last is the
        // leftmost block, so the newest work is where the eye starts.
        assert!((rects[0].y - rects[1].y).abs() < f32::EPSILON, "{rects:?}");
        assert!(rects[2].x < rects[0].x.min(rects[1].x), "{rects:?}");

        let headings = view.read_with(cx, |c, _| c.headings().to_vec());
        assert_eq!(
            headings.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["tools", "app"],
            "the most recently raised repository leads"
        );
        // The host was asked to move them, not told.
        let moves = std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|msg| matches!(msg, ClientMsg::Canvas(CanvasOp::Place { .. })))
            .count();
        assert_eq!(moves, 3, "one Place per item");

        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        for label in ["app", "tools"] {
            assert!(
                tree.iter().any(|n| n.role == "Heading" && n.label.as_deref() == Some(label)),
                "a heading a screen reader can read: {tree:?}"
            );
        }
    }

    /// A shell that `cd`s into another repository arranges under the new one. The opening
    /// summary only says where it started; OSC 7 says where it is.
    #[gpui::test]
    fn a_shell_that_changes_directory_changes_block(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        let stay = SessionId::new();
        let moves = SessionId::new();
        host_opens_in(&view, cx, stay, host, SHELL, 1, Where::rooted("/w/app", "/w/app"));
        let wanderer = host_opens_in(
            &view,
            cx,
            moves,
            host,
            Rect { x: 900.0, y: 40.0, w: 300.0, h: 200.0 },
            2,
            Where::rooted("/w/app/src", "/w/app"),
        );

        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |c, _| c.headings().len()),
            1,
            "one repository while both are in it"
        );

        // The shell announces its new directory the way the host relays OSC 7, with the
        // repository the host resolved for it.
        view.update_in(cx, |c, _window, cx| {
            c.term_event(
                moves,
                TermEvent::Cwd { path: "/w/tools/src".into(), repo: Some("/w/tools".into()) },
                cx,
            );
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();

        let headings = view.read_with(cx, |c, _| c.headings().to_vec());
        let labels: Vec<&str> = headings.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(labels, ["tools", "app"], "it left the repository it started in: {labels:?}");
        let tools = headings.iter().find(|h| h.label == "tools").expect("the new block");
        assert_eq!(tools.items, vec![wanderer], "and took only itself: {:?}", tools.items);
    }

    /// A flight is the camera's *default* move, never a lock: panning during one keeps the pan.
    #[gpui::test]
    fn panning_during_a_flight_takes_the_camera(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        host_opens(&view, cx, SessionId::new(), host, SHELL, 1);
        host_opens(
            &view,
            cx,
            SessionId::new(),
            host,
            Rect { x: 4000.0, y: 2500.0, w: 300.0, h: 200.0 },
            2,
        );
        view.update_in(cx, |c, _window, cx| {
            c.set_animation(true);
            cx.notify();
        });

        cx.simulate_keystrokes("cmd-1");
        assert!(view.read_with(cx, |c, _| c.flying_to().is_some()), "a flight is in progress");

        // One scroll step, the way a trackpad sends it.
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(200.0), px(200.0)),
            delta: ScrollDelta::Pixels(point(px(30.0), px(-40.0))),
            modifiers: Modifiers::default(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        assert!(view.read_with(cx, |c, _| c.flying_to().is_none()), "the pan took the camera");
        let panned = view.read_with(cx, |c, _| c.camera());
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.camera()), panned, "and no frame took it back");
    }

    /// Closing every item of a repository takes its heading with it: ⌘1 must not keep fitting
    /// an empty block, and a screen reader must not keep reading a label for nothing.
    #[gpui::test]
    fn an_emptied_repository_loses_its_heading(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        let stays =
            host_opens_in(&view, cx, SessionId::new(), host, SHELL, 1, Where::loose("/w/app"));
        let goes = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            Rect { x: 2200.0, y: 400.0, w: 300.0, h: 200.0 },
            2,
            Where::loose("/w/tools"),
        );

        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.headings().len()), 2);

        // The host removes the other repository's only shell.
        view.update_in(cx, |c, _window, cx| {
            c.apply_sync(
                CanvasSync::Delta { version: 3, by: host, op: CanvasOp::Remove(goes) },
                cx,
            );
        });
        cx.run_until_parked();
        let headings = view.read_with(cx, |c, _| c.headings().to_vec());
        assert_eq!(
            headings.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["app"],
            "the emptied block is gone"
        );

        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            !tree.iter().any(|n| n.role == "Heading" && n.label.as_deref() == Some("tools")),
            "and nothing reads it out: {tree:?}"
        );

        // ⌘1 now fits the remaining item, not the space the empty block used to hold.
        cx.simulate_keystrokes("cmd-1");
        cx.run_until_parked();
        let on_screen = view.read_with(cx, |c, _| {
            c.camera().to_screen(c.doc.get(stays).expect("still there").rect)
        });
        assert!(on_screen.w > VIEWPORT.0 / 2.0, "the survivor fills the view: {on_screen:?}");
    }

    /// Protocol 14: two shells in sibling subdirectories of one checkout are one block, which
    /// the cwd heuristic could not see, and a worktree of the same project is its own.
    #[gpui::test]
    fn the_hosts_repository_root_decides_the_blocks(cx: &mut TestAppContext) {
        let (view, _rx, host, cx) = canvas(cx);
        let a = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            SHELL,
            1,
            Where::rooted("/w/slopty/crates/a", "/w/slopty"),
        );
        let b = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            Rect { x: 900.0, y: 40.0, w: 300.0, h: 200.0 },
            2,
            Where::rooted("/w/slopty/crates/b", "/w/slopty"),
        );
        let worktree = host_opens_in(
            &view,
            cx,
            SessionId::new(),
            host,
            Rect { x: 1800.0, y: 600.0, w: 300.0, h: 200.0 },
            3,
            Where::rooted("/w/slopty-wt/regex/crates/a", "/w/slopty-wt/regex"),
        );

        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-r");
        cx.run_until_parked();

        let headings = view.read_with(cx, |c, _| c.headings().to_vec());
        assert_eq!(
            headings.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(),
            ["regex", "slopty"],
            "the worktree is its own repository, and it was raised last"
        );
        let slopty = headings.iter().find(|h| h.label == "slopty").expect("the checkout");
        assert_eq!(slopty.items.len(), 2, "the siblings are one block: {:?}", slopty.items);
        let regex = headings.iter().find(|h| h.label == "regex").expect("the worktree");
        assert_eq!(regex.items, vec![worktree]);

        // Side by side, in one row, is what "one block" means on the canvas.
        let (ra, rb) = view
            .read_with(cx, |c, _| (c.doc.get(a).expect("a").rect, c.doc.get(b).expect("b").rect));
        assert!((ra.y - rb.y).abs() < f32::EPSILON, "{ra:?} {rb:?}");

        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        for label in ["slopty", "regex"] {
            assert!(
                tree.iter().any(|n| n.role == "Heading" && n.label.as_deref() == Some(label)),
                "a heading a screen reader can read: {tree:?}"
            );
        }
    }

    /// Resolving the monospace family lists the installed fonts, a trip to the font server that
    /// costs tens of milliseconds: three shells drawn on the canvas list them once, not once per
    /// view (the card → grid flip drew twenty first frames at once and paid twenty walks).
    #[gpui::test]
    fn the_installed_fonts_are_listed_once_for_every_shell(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let mut ids = Vec::new();
        for (i, x) in [0.0, 320.0, 640.0].into_iter().enumerate() {
            let rect = Rect { x, y: 0.0, w: 300.0, h: 200.0 };
            let version = u64::try_from(i).unwrap().saturating_add(1);
            ids.push(host_opens(&view, cx, SessionId::new(), me, rect, version));
        }
        cx.run_until_parked();
        for id in ids {
            assert!(cx.debug_bounds(selector("item", id)).is_some(), "every shell is drawn");
        }
        let picks = cx.update(|_window, cx| crate::terminal::family_picks(cx));
        assert_eq!(picks, 1, "one walk of the installed fonts for the whole app");
    }

    /// A grid that hangs half off the viewport prepares only the rows the viewport shows;
    /// one fully inside prepares them all. (Whole off-screen items are not built at all.)
    #[gpui::test]
    fn only_the_rows_inside_the_viewport_are_prepared(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let rows_prepared = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| crate::terminal::rows_prepared(cx))
        };
        let grid_rows = |view: &Entity<CanvasView>, cx: &mut VisualTestContext, session| {
            view.read_with(cx, |c, cx| {
                c.terminals.get(&session).and_then(|t| t.read(cx).metrics()).map(|m| m.rows)
            })
        };
        let inside = SessionId::new();
        host_opens(&view, cx, inside, me, Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }, 1);
        cx.run_until_parked();
        let rows = grid_rows(&view, cx, inside).expect("laid out");
        assert!(rows > 4, "{rows}");
        assert_eq!(rows_prepared(cx), usize::from(rows), "every row of a grid fully on screen");

        // The lower one starts 150 pt above the viewport's bottom edge: its title bar and a
        // few rows show.
        let low = SessionId::new();
        let y = VIEWPORT.1 - 150.0;
        host_opens(&view, cx, low, me, Rect { x: 0.0, y, w: 400.0, h: 300.0 }, 2);
        cx.run_until_parked();
        let rows = grid_rows(&view, cx, low).expect("laid out");
        let prepared = rows_prepared(cx);
        assert!(prepared >= 1 && prepared < usize::from(rows) / 2, "{prepared} of {rows}");
    }

    /// A zoom step is one frame in motion (terminals and chrome paint from the raster ladder)
    /// followed by one settled frame the canvas asks for itself, so the final zoom is painted
    /// exact without waiting for anything else to redraw.
    #[gpui::test]
    fn a_zoom_step_is_one_frame_in_motion_then_one_settled(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        let session = SessionId::new();
        host_opens(&view, cx, session, me, SHELL, 1);
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.zooming_frames), 0, "opening is not a zoom");
        let motion = |view: &Entity<CanvasView>, cx: &mut VisualTestContext| {
            view.read_with(cx, |c, cx| c.terminals[&session].read(cx).motion_frames())
        };
        assert_eq!(motion(&view, cx), 0);

        cx.simulate_keystrokes("cmd-=");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.zooming_frames), 1, "one frame in motion");
        assert_eq!(motion(&view, cx), 1, "the terminal saw that frame");
        assert!(view.read_with(cx, |c, _| c.zooming), "no settle frame before the pause");
        // A second step inside the pause is another frame in motion, not a settle.
        cx.simulate_keystrokes("cmd-=");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.zooming_frames), 2);
        assert_eq!(motion(&view, cx), 2);
        assert!(view.read_with(cx, |c, _| c.zooming));

        cx.executor().advance_clock(SETTLE);
        cx.run_until_parked();
        assert!(!view.read_with(cx, |c, _| c.zooming), "the settled frame paints exact");
        assert_eq!(view.read_with(cx, |c, _| c.zooming_frames), 2, "and is not in motion");
        assert!(view.read_with(cx, |c, _| c.zoom_drawn.is_some_and(|z| z > 1.0)));
    }

    /// Chrome labels are shaped once at their base size: three shells titled alike share one
    /// entry, and a zoom step shapes nothing new (the words are painted at the zoom).
    #[gpui::test]
    fn chrome_labels_are_shaped_once_across_items_and_zoom_steps(cx: &mut TestAppContext) {
        let (view, _rx, me, cx) = canvas(cx);
        for (i, x) in [0.0, 320.0, 640.0].into_iter().enumerate() {
            let rect = Rect { x, y: 0.0, w: 300.0, h: 200.0 };
            let version = u64::try_from(i).unwrap().saturating_add(1);
            host_opens(&view, cx, SessionId::new(), me, rect, version);
        }
        cx.run_until_parked();
        let labels = |cx: &mut VisualTestContext| {
            cx.update(|_window, cx| crate::chrome_text::cached_labels(cx))
        };
        // Three title bars share `shell`; the active item alone offers the `take` pill.
        assert_eq!(labels(cx), 2, "`shell` and `take`");
        cx.simulate_keystrokes("cmd-=");
        cx.executor().advance_clock(SETTLE);
        cx.run_until_parked();
        assert_eq!(labels(cx), 2, "a zoom step and its settled frame shape nothing");
        assert!(view.read_with(cx, |c, _| c.camera.zoom > 1.0));
    }

    /// ⌘⇧F: what is typed goes to every card as a search; the cards it is found in are the
    /// palette's lines with their hit counts, and ↩ reveals one with its find bar on the
    /// needle. A card with no hit is no line.
    #[gpui::test]
    fn find_in_every_card_lists_the_cards_with_hits_and_opens_ones_find_bar(
        cx: &mut TestAppContext,
    ) {
        use slopty_proto::terminal::SearchMatch;
        let (view, mut rx, me, cx) = canvas(cx);
        let (a, b) = (SessionId::new(), SessionId::new());
        let id_a = host_opens(&view, cx, a, me, SHELL, 1);
        let _id_b = host_opens(&view, cx, b, me, Rect { x: 800.0, ..SHELL }, 2);
        // A note below the shells and a file card hold the needle too; they are counted here.
        let note = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: "Errors\nno\nan ERROR again".to_owned() },
            rect: Rect { x: 0.0, y: 600.0, w: 300.0, h: 200.0 },
            z: 3,
            group: None,
            sleeping: false,
            name: None,
        };
        let note_id = note.id;
        view.update_in(cx, |c, _window, cx| {
            c.apply_sync(CanvasSync::Delta { version: 3, by: me, op: CanvasOp::Upsert(note) }, cx);
            c.open_file("/tmp/work/log.txt", None, cx);
        });
        cx.run_until_parked(); // the file's view is made on the next frame
        view.update_in(cx, |c, _window, cx| {
            c.file_read(
                "/tmp/work/log.txt",
                &FileRead::Text {
                    text: "err one\nfine\nerr two".to_owned(),
                    more_lines: 0,
                    size: 21,
                    modified_ms: 1,
                },
                cx,
            );
        });
        cx.run_until_parked();
        let file_id = view.read_with(cx, |c, _| {
            c.items().into_iter().find(|i| matches!(i.kind, ItemKind::File { .. })).map(|i| i.id)
        });
        let file_id = file_id.expect("the file card");
        let title_of = |cx: &mut VisualTestContext, id: ItemId| {
            view.read_with(cx, |c, cx| c.card_title(c.doc.get(id).expect("the card"), cx))
        };
        let (file_title, note_title) = (title_of(cx, file_id), title_of(cx, note_id));
        drain(&mut rx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-f");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
        cx.simulate_keystrokes("e r r");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        let asked: Vec<SessionId> = sent
            .iter()
            .filter_map(|m| match m {
                ClientMsg::Term {
                    session,
                    req: TermRequest::Search { needle, max: 1, regex: false },
                } if needle == "err" => Some(*session),
                _ => None,
            })
            .collect();
        assert_eq!(asked, [a, b], "each shell is asked once for the final needle: {sent:?}");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            options,
            [format!("{file_title} 2 hits"), format!("{note_title} 2 hits")],
            "the cards counted here are lines at once, in reading order: {tree:#?}"
        );

        let hit = SearchMatch { line: LineIndex(2), col: 0, len: 3 };
        view.update_in(cx, |c, _window, cx| {
            let answer = |total: u32| TermEvent::Matches {
                needle: "err".to_owned(),
                total,
                matches: (total > 0).then_some(hit).into_iter().collect(),
            };
            c.term_event(a, answer(3), cx);
            // A shell without a hit is no line.
            c.term_event(b, answer(0), cx);
            // A stale answer (the needle moved on) is nothing.
            c.term_event(
                a,
                TermEvent::Matches { needle: "er".into(), total: 9, matches: vec![] },
                cx,
            );
        });
        cx.run_until_parked();
        let title = view.read_with(cx, |c, cx| c.card_title(c.doc.get(id_a).unwrap(), cx));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            options,
            [
                format!("{title} 3 hits"),
                format!("{file_title} 2 hits"),
                format!("{note_title} 2 hits")
            ],
            "{tree:#?}"
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none(), "gone after ↩");
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(id_a));
        let needle = view.read_with(cx, |c, cx| {
            c.terminals.get(&a).map(|v| v.read(cx).search_needle().map(str::to_owned))
        });
        assert_eq!(needle, Some(Some("err".to_owned())), "the card's find bar holds the needle");
        assert!(cx.debug_bounds("terminal-search").is_some(), "the find bar is up");
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Term { session, req: TermRequest::Search { needle, max, .. } }
                    if *session == a && needle == "err" && *max > 1
            )),
            "the card searches for its own hits: {sent:?}"
        );
        assert!(view.read_with(cx, |c, _| c.find_needle.is_none()), "the fan-out is over");

        // ⌘⇧F again, from the terminal's find bar (a field would take it as replace): the
        // file card's line (the first: no shell has answered this time) opens its own find
        // bar on the needle, on the first hit.
        cx.simulate_keystrokes("cmd-shift-f");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_some(), "⌘⇧F works from a field");
        cx.simulate_keystrokes("e r r enter");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(file_id));
        let hits = view.read_with(cx, |c, cx| {
            c.files.get(&file_id).map(|v| v.read(cx).hits().map(|(h, at)| (h.to_vec(), at)))
        });
        assert_eq!(hits, Some(Some((vec![0, 2], Some(0)))), "finding, on the first hit");
        assert!(cx.debug_bounds("file-search").is_some(), "the file's find bar is up");

        // ⌘⇧F from a card whose bar holds a needle starts from that needle: the lines are up
        // and the shell asked before anything is typed.
        drain(&mut rx);
        cx.simulate_keystrokes("cmd-shift-f");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            options,
            [format!("{file_title} 2 hits"), format!("{note_title} 2 hits")],
            "the file card's needle seeds the find: {tree:#?}"
        );
        let sent = drain(&mut rx);
        assert!(
            sent.iter().any(|m| matches!(
                m,
                ClientMsg::Term { session, req: TermRequest::Search { needle, max: 1, .. } }
                    if *session == a && needle == "err"
            )),
            "the shell is asked for the seed: {sent:?}"
        );

        // From a card with no bar of its own, the last find is the start.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.update_in(cx, |c, _window, cx| c.go_to(note_id, cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(note_id));
        cx.simulate_keystrokes("cmd-shift-f");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let lines = tree.iter().filter(|n| n.role == "ListBoxOption").count();
        assert_eq!(lines, 2, "the last needle stands in: {tree:#?}");
    }

    /// A directory typed into the palette, spelled from the host's root or home with a
    /// slash at the end, offers a shell and an agent there: the phone's way to a project no
    /// card is in yet.
    #[gpui::test]
    fn a_directory_in_the_palette_opens_a_shell_or_an_agent_there(cx: &mut TestAppContext) {
        let (view, mut rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("/ s r v / a /");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            options,
            ["New terminal in /srv/a shell", "New agent in /srv/a agent"],
            "{tree:#?}"
        );
        let _asked = drain(&mut rx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none(), "gone after ↩");
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                    if cwd == "/srv/a" && command.is_empty()
            ),
            "{sent:?}"
        );
        assert!(view.read_with(cx, |v, _| v.items().is_empty()), "the host places it");

        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("/ s r v / a / down enter");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                    if cwd == "/srv/a" && command == &[AGENT_COMMAND.to_owned()]
            ),
            "{sent:?}"
        );
    }

    /// ⌘⇧P opens the command palette over the canvas: typing filters the lines by every
    /// word, ↩ runs the selected one once the palette is gone and the focus is back, Esc
    /// closes it with nothing run.
    #[gpui::test]
    fn the_command_palette_runs_an_action_by_name(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_some(), "the palette is up");
        assert!(view.read_with(cx, |v, _| v.palette_open()));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Commands"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("New terminal ⌘T"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("Find in terminal or file ⌘F"))));
        let field_focused = cx.update(|window, cx| {
            view.read(cx)
                .palette
                .as_ref()
                .is_some_and(|p| p.read(cx).focus_handle(cx).is_focused(window))
        });
        assert!(field_focused, "the field has the keyboard");

        // Esc: nothing runs, the keyboard goes back where it was.
        cx.simulate_keystrokes("z o o m down");
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none(), "Esc closes it");
        let focused = cx.update(|window, cx| view.read(cx).focus.is_focused(window));
        assert!(focused, "the canvas has the keyboard back");
        let zoom = view.read_with(cx, |v, _| v.zoom());
        assert!((zoom - 1.0).abs() < f32::EPSILON, "nothing ran: {zoom}");

        // ↩: the one line left runs, once the palette is gone.
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("n o t e");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(options, ["New note ⇧⌘N"], "one line matches");
        assert!(view.read_with(cx, |v, _| v.items().is_empty()));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none(), "gone after ↩");
        let notes = view.read_with(cx, |v, _| {
            v.items().iter().filter(|i| matches!(i.kind, ItemKind::Note { .. })).count()
        });
        assert_eq!(notes, 1, "the action ran");

        // A session on the canvas is a line too: "Go to <title>" reveals and focuses it.
        let session = SessionId::new();
        let id = host_opens(&view, cx, session, ClientId::new(), SHELL, 1);
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let first = tree.iter().find(|n| n.role == "ListBoxOption").and_then(|n| n.label.clone());
        assert_eq!(first.as_deref(), Some("Go to shell"), "sessions come first: {tree:#?}");
        cx.simulate_keystrokes("g o space t o space s h e l l enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none());
        assert_eq!(view.read_with(cx, |v, _| v.active_item()), Some(id), "revealed");
        let terminal_focused = cx.update(|window, cx| {
            view.read(cx)
                .active_terminal()
                .is_some_and(|t| t.read(cx).focus_handle(cx).is_focused(window))
        });
        assert!(terminal_focused, "and its terminal has the keyboard");

        // A file card is a line too, after the sessions: "Go to main.rs · src" reveals it.
        view.update_in(cx, |c, _window, cx| c.open_file("/w/src/main.rs", None, cx));
        view.update_in(cx, |c, _window, cx| c.reveal_session(session, cx));
        cx.run_until_parked();
        let file = view
            .read_with(cx, |v, _| {
                v.items().iter().find(|i| matches!(i.kind, ItemKind::File { .. })).map(|i| i.id)
            })
            .expect("the card");
        assert_ne!(view.read_with(cx, |v, _| v.active_item()), Some(file));
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let labels: Vec<String> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.clone())
            .take(2)
            .collect();
        assert_eq!(labels, ["Go to shell", "Go to main.rs · src file"], "{tree:#?}");
        cx.simulate_keystrokes("m a i n enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none());
        assert_eq!(view.read_with(cx, |v, _| v.active_item()), Some(file), "the card revealed");
    }

    /// A path a shell's view asks to see is made absolute against that shell's directory.
    #[gpui::test]
    fn a_path_typed_into_the_palette_opens_a_file_card(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let shell = SessionId::new();
        host_opens_in(&view, cx, shell, me, SHELL, 1, Where::loose("/tmp/work"));
        view.update_in(cx, |c, _window, cx| c.reveal_session(shell, cx));
        cx.run_until_parked();
        drain(&mut rx);

        // A relative path with a line: the line is offered first and alone, ↩ opens the card in
        // the active shell's directory, landed on the line.
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("s r c / l i b . r s : 7");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(options, ["Open src/lib.rs line 7"], "{tree:#?}");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none(), "gone after ↩");
        let (id, path) = view.read_with(cx, |c, _| {
            c.items()
                .into_iter()
                .find_map(|i| match &i.kind {
                    ItemKind::File { path } => Some((i.id, path.clone())),
                    _ => None,
                })
                .expect("a file card")
        });
        assert_eq!(path, "/tmp/work/src/lib.rs");
        assert_eq!(view.read_with(cx, |c, _| c.active_item()), Some(id), "active");
        let focus = view.read_with(cx, |c, cx| c.file(id).map(|f| f.read(cx).focus()));
        assert_eq!(focus, Some(Some(6)), "landed on line 7");
        assert!(
            drain(&mut rx).iter().any(
                |m| matches!(m, ClientMsg::ReadFile { path } if path == "/tmp/work/src/lib.rs")
            ),
            "read asked for the absolute path"
        );

        // A word is asked of the host's files under the active item's directory — the file
        // card's while it is active, the shell's once revealed; what the host finds are `Open`
        // lines after the commands (a directory is not a card), ↩ opens one.
        drain(&mut rx);
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("l i");
        cx.run_until_parked();
        let asked = drain(&mut rx);
        assert!(
            asked.iter().any(|m| matches!(m, ClientMsg::FindFiles { root, query }
                if root == "/tmp/work/src" && query == "li")),
            "the active card's directory: {asked:?}"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.update_in(cx, |c, _window, cx| c.reveal_session(shell, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("m a i n");
        cx.run_until_parked();
        let asked = drain(&mut rx);
        assert!(
            asked.iter().any(|m| matches!(m, ClientMsg::FindFiles { root, query }
                if root == "/tmp/work" && query == "main")),
            "{asked:?}"
        );
        view.update_in(cx, |c, _window, cx| {
            c.files_found(
                "/tmp/work",
                "main",
                &["src/main.rs".to_owned(), "docs/manual/".to_owned()],
                cx,
            );
        });
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(
            options,
            [
                "Open src/main.rs file",
                "New terminal in docs/manual shell",
                "New agent in docs/manual agent"
            ],
            "{tree:#?}"
        );
        // The found directory: a shell there, rooted where the host looked.
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), command, .. })]
                    if cwd == "/tmp/work/docs/manual" && command.is_empty()
            ),
            "{sent:?}"
        );
        view.update_in(cx, |c, _window, cx| c.reveal_session(shell, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("m a i n");
        cx.run_until_parked();
        drain(&mut rx);
        view.update_in(cx, |c, _window, cx| {
            c.files_found("/tmp/work", "main", &["src/main.rs".to_owned()], cx);
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let paths: Vec<String> = view.read_with(cx, |c, _| {
            c.items()
                .into_iter()
                .filter_map(|i| match &i.kind {
                    ItemKind::File { path } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(paths, ["/tmp/work/src/lib.rs", "/tmp/work/src/main.rs"]);
        view.update_in(cx, |c, _window, cx| c.reveal_session(shell, cx));
        cx.run_until_parked();

        // `~` is the host's home, not the shell's directory; a word without a slash is a command.
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("~ / n o t e s . m d enter");
        cx.run_until_parked();
        let paths: Vec<String> = view.read_with(cx, |c, _| {
            c.items()
                .into_iter()
                .filter_map(|i| match &i.kind {
                    ItemKind::File { path } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(paths, ["/tmp/work/src/lib.rs", "/tmp/work/src/main.rs", "~/notes.md"]);
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("n o t e");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<&str> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert_eq!(options, ["Go to notes.md · ~ file", "New note ⇧⌘N"], "no typed-path line");
        assert!(
            drain(&mut rx).iter().any(|m| matches!(m, ClientMsg::FindFiles { root, query }
                if root == "~" && query == "note")),
            "no shell active: the host's home is searched"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn find_in_a_file_card_steps_through_its_lines(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        view.update_in(cx, |c, _window, cx| c.open_file("/w/a.txt", None, cx));
        cx.run_until_parked();
        view.update_in(cx, |c, _window, cx| {
            c.file_read(
                "/w/a.txt",
                &FileRead::Text {
                    text: "Alpha\nbeta\nalpha beta\ngamma".to_owned(),
                    more_lines: 0,
                    size: 27,
                    modified_ms: 1,
                },
                cx,
            );
        });
        cx.run_until_parked();
        let id = view.read_with(cx, |c, _| c.active_item().expect("the card is active"));
        let hits = |cx: &mut VisualTestContext| {
            view.read_with(cx, |c, cx| {
                c.file(id).unwrap().read(cx).hits().map(|(h, c)| (h.to_vec(), c))
            })
        };

        // ⌘F on the canvas with the card active opens its bar and gives the field the keys.
        cx.simulate_keystrokes("cmd-f");
        cx.run_until_parked();
        assert!(cx.debug_bounds("file-search").is_some(), "the bar is up");
        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 2], Some(0))), "two lines, on the first");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let count = tree.iter().find(|n| n.label.as_deref() == Some("Matches"));
        assert_eq!(count.and_then(|n| n.value.as_deref()), Some("1/2"), "{tree:#?}");

        // ⌘G, ↩ and ⇧↩ step and wrap.
        cx.simulate_keystrokes("cmd-g");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 2], Some(1))));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 2], Some(0))), "wrapped");
        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![0, 2], Some(1))), "back around");

        // The text changes under an open bar: the hits follow.
        view.update_in(cx, |c, _window, cx| {
            c.file_read(
                "/w/a.txt",
                &FileRead::Text {
                    text: "beta\nalpha".to_owned(),
                    more_lines: 0,
                    size: 10,
                    modified_ms: 2,
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert_eq!(hits(cx), Some((vec![1], Some(0))));

        // Esc closes the bar and the canvas has the keyboard again.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("file-search").is_none(), "closed");
        assert_eq!(hits(cx), None);
        let focused = cx.update(|window, cx| view.read(cx).focus.is_focused(window));
        assert!(focused, "the canvas has the keyboard back");

        // The title bar's "find" pill is the phone's ⌘F.
        let pill = cx.debug_bounds(selector("find", id)).expect("the card's find pill");
        cx.simulate_click(pill.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("file-search").is_some(), "the pill opens the bar");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Find in the file"))), "{tree:#?}");
    }

    /// ⌘E names the active card in its title bar: ↩ keeps the name (the document, the
    /// heading, the palette's "Go to" line and the host all see it), Esc leaves the old one,
    /// a blank name clears it, and a double-click on the title bar opens the same field.
    #[gpui::test]
    fn a_card_is_named_from_its_title_bar(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let session = SessionId::new();
        let id = host_opens(&view, cx, session, me, SHELL, 1);
        view.update_in(cx, |c, _window, cx| c.reveal_session(session, cx));
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        let heading = |cx: &mut VisualTestContext| {
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            tree.iter().find(|n| n.role == "Heading").and_then(|n| n.label.clone())
        };
        let name = |cx: &mut VisualTestContext| {
            view.read_with(cx, |c, _| c.doc.get(id).and_then(|i| i.name.clone()))
        };
        assert_eq!(heading(cx).as_deref(), Some("terminal shell"));

        // ⌘E from the shell: the field is up with the keys, ↩ keeps the name.
        cx.simulate_keystrokes("cmd-e");
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("rename", id)).is_some(), "the field is in the bar");
        cx.simulate_keystrokes("b u i l d space b o x enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("rename", id)).is_none(), "closed on ↩");
        assert_eq!(name(cx).as_deref(), Some("build box"));
        assert_eq!(heading(cx).as_deref(), Some("terminal build box"));
        let sent = rx.try_recv();
        assert!(
            matches!(&sent, Ok(ClientMsg::Canvas(CanvasOp::Upsert(i))) if i.name.as_deref() == Some("build box")),
            "the name goes to the host: {sent:?}"
        );
        let shell_focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                view.read(cx)
                    .active_terminal()
                    .is_some_and(|t| t.read(cx).focus_handle(cx).is_focused(window))
            })
        };
        assert!(shell_focused(cx), "the shell has the keyboard back");

        // The palette goes to the card by its name.
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let first = tree.iter().find(|n| n.role == "ListBoxOption").and_then(|n| n.label.clone());
        assert_eq!(first.as_deref(), Some("Go to build box"), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("Name this card ⌘E"))));
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        // Esc leaves the name as it was; the field started with it.
        cx.simulate_keystrokes("cmd-e");
        cx.run_until_parked();
        let text = view.read_with(cx, |c, cx| c.rename.as_ref().map(|r| r.input.read(cx).value()));
        assert_eq!(text.as_deref(), Some("build box"));
        cx.simulate_keystrokes("x escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("rename", id)).is_none(), "closed on Esc");
        assert_eq!(name(cx).as_deref(), Some("build box"), "unchanged");
        assert!(rx.try_recv().is_err(), "nothing sent");
        assert!(shell_focused(cx), "the shell has the keyboard back after Esc");

        // Zoomed out to cards (the arranged canvas of the app self-test), the field still
        // takes the keyboard: the card's own focus rules must not blur it.
        cx.simulate_keystrokes("cmd-- cmd-- cmd-- cmd--");
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.zoom()) < CARD_ZOOM, "cards now");
        cx.simulate_keystrokes("cmd-e");
        cx.run_until_parked();
        let field_focused = cx.update(|window, cx| {
            view.read(cx)
                .rename
                .as_ref()
                .is_some_and(|r| r.input.read(cx).focus_handle(cx).is_focused(window))
        });
        assert!(field_focused, "the field has the keyboard at card zoom");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-0");
        cx.run_until_parked();

        // A double-click on the title bar opens the field too; a blank name clears it.
        let bar = cx.debug_bounds(selector("title", id)).expect("the title bar");
        let at = gpui::point(bar.origin.x + bar.size.width / 2.0, bar.center().y);
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds(selector("rename", id)).is_some(), "a double-click names");
        cx.simulate_keystrokes("cmd-a backspace enter");
        cx.run_until_parked();
        assert_eq!(name(cx), None, "blank clears the name");
        assert_eq!(heading(cx).as_deref(), Some("terminal shell"));
    }

    /// The reading-line keys are bound only while a file card is active: a binding matches
    /// before a focused terminal's key handler runs, so an unscoped arrow would never reach
    /// the shell (the iOS hardware-keyboard self-test caught exactly that).
    #[gpui::test]
    fn arrow_keys_reach_a_focused_shell_beside_a_file_card(cx: &mut TestAppContext) {
        use slopty_proto::input::KeyCode;
        let (view, mut rx, me, cx) = canvas(cx);
        view.update_in(cx, |c, _window, cx| c.open_file("/w/a.txt", None, cx));
        cx.run_until_parked();
        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, Rect { x: 760.0, ..SHELL }, 2);
        view.update_in(cx, |c, _window, cx| c.reveal_session(shell, cx));
        cx.run_until_parked();
        assert!(terminal_focused(&view, cx, shell));
        drain(&mut rx);
        cx.simulate_keystrokes("down up pagedown home end");
        cx.run_until_parked();
        let keys: Vec<_> = drain(&mut rx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Term { req: TermRequest::Key(key), .. } => Some(key.code),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            [KeyCode::ArrowDown, KeyCode::ArrowUp, KeyCode::PageDown, KeyCode::Home, KeyCode::End],
            "every key went to the shell"
        );
    }

    #[gpui::test]
    fn arrow_keys_move_a_file_cards_reading_line(cx: &mut TestAppContext) {
        let (view, _rx, _me, cx) = canvas(cx);
        view.update_in(cx, |c, _window, cx| c.open_file("/w/a.txt", None, cx));
        cx.run_until_parked();
        let id = view.read_with(cx, |c, _| c.active_item().expect("the card"));
        let lines = (1..=200).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        view.update_in(cx, |c, _window, cx| {
            c.file_read(
                "/w/a.txt",
                &FileRead::Text { text: lines, more_lines: 0, size: 1, modified_ms: 1 },
                cx,
            );
        });
        cx.run_until_parked();
        let focus = |cx: &mut VisualTestContext| {
            view.read_with(cx, |c, cx| c.file(id).unwrap().read(cx).focus())
        };
        assert_eq!(focus(cx), None, "no reading line until a key");
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(0), "the first key lands on the top line shown");
        cx.simulate_keystrokes("down down up");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(1));
        cx.simulate_keystrokes("pagedown");
        cx.run_until_parked();
        let page = focus(cx).unwrap();
        assert!(page > 1, "a page is more than a line: {page}");
        cx.simulate_keystrokes("end");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(199));
        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(199), "clamped at the end");
        cx.simulate_keystrokes("home");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(0));
        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(0), "clamped at the start");
        // The reading line is what "edit" opens on.
        let line = view.read_with(cx, |c, cx| c.file(id).unwrap().read(cx).reading_line());
        assert_eq!(line, Some(1));

        // A click on a row makes it the reading line.
        let fifth: &'static str =
            Box::leak(format!("file-line-{}-5", id.as_uuid()).into_boxed_str());
        let row = cx.debug_bounds(fifth).expect("the sixth row is drawn");
        cx.simulate_click(row.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(focus(cx), Some(5));
    }

    #[gpui::test]
    fn a_file_cards_edit_pill_opens_the_editor_at_the_line_read(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        view.update_in(cx, |c, _window, cx| c.open_file("/w/a.txt", Some(2), cx));
        cx.run_until_parked();
        let id = view.read_with(cx, |c, _| c.active_item().expect("the card"));
        assert!(cx.debug_bounds(selector("edit", id)).is_none(), "no shell: nowhere to edit");

        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, Rect { x: 760.0, ..SHELL }, 2);
        view.update_in(cx, |c, _window, cx| {
            c.file_read(
                "/w/a.txt",
                &FileRead::Text {
                    text: "alpha\nbeta\nalpha".to_owned(),
                    more_lines: 0,
                    size: 16,
                    modified_ms: 1,
                },
                cx,
            );
            // Back to the card: the shell's arrival panned the camera to it.
            c.activate(id, cx);
            c.reveal_pending = Some(id);
        });
        cx.run_until_parked();
        // The reveal moved the camera during a frame; the next frame draws the card there.
        view.update(cx, |_c, cx| cx.notify());
        cx.run_until_parked();
        let typed = |rx: &mut mpsc::Receiver<ClientMsg>| -> Vec<String> {
            drain(rx)
                .into_iter()
                .filter_map(|m| match m {
                    ClientMsg::Term { session, req: TermRequest::Paste(code) }
                        if session == shell =>
                    {
                        Some(code)
                    }
                    _ => None,
                })
                .collect()
        };
        drain(&mut rx);

        // The line the card opened at.
        let edit = cx.debug_bounds(selector("edit", id)).expect("a shell: the edit pill");
        cx.simulate_click(edit.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(typed(&mut rx), ["${EDITOR:-vi} +2 '/w/a.txt'"]);
        assert!(terminal_focused(&view, cx, shell), "the shell took the keyboard");

        // The current find hit wins while finding.
        view.update_in(cx, |c, window, cx| {
            c.activate(id, cx);
            c.reveal_pending = Some(id);
            c.files[&id].update(cx, |v, cx| v.find(window, cx));
        });
        cx.run_until_parked();
        view.update(cx, |_c, cx| cx.notify());
        cx.run_until_parked();
        // The hits are lines 1 and 3; the card opened at line 2, so it lands on 3.
        cx.simulate_keystrokes("a l p h a");
        cx.run_until_parked();
        let edit = cx.debug_bounds(selector("edit", id)).expect("the edit pill");
        cx.simulate_click(edit.center(), Modifiers::default());
        cx.run_until_parked();
        assert_eq!(typed(&mut rx), ["${EDITOR:-vi} +3 '/w/a.txt'"]);
    }

    /// "Save as note" on a block puts a note card with the block's Markdown beside the
    /// shell, active and revealed, its caret not in it; a second one lands in a free slot.
    #[gpui::test]
    fn a_block_saved_as_a_note_lands_beside_the_shell(cx: &mut TestAppContext) {
        const TEXT: &str = "# make\n\n```sh\nmake\n```\n\n```\nok\n```\n";
        let (view, _rx, me, cx) = canvas(cx);
        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, SHELL, 1);
        let save = |view: &Entity<CanvasView>, cx: &mut VisualTestContext| {
            view.update_in(cx, |c, _window, cx| {
                let terminal = c.terminals[&shell].clone();
                terminal.update(cx, |_v, cx| {
                    cx.emit(TerminalViewEvent::NoteBlock(TEXT.to_owned()));
                });
            });
            cx.run_until_parked();
        };
        save(&view, cx);
        let notes =
            |view: &Entity<CanvasView>, cx: &mut VisualTestContext| -> Vec<(String, Rect)> {
                view.read_with(cx, |c, _| {
                    c.items()
                        .into_iter()
                        .filter_map(|i| match &i.kind {
                            ItemKind::Note { text } => Some((text.clone(), i.rect)),
                            _ => None,
                        })
                        .collect()
                })
            };
        let first = notes(&view, cx);
        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].0, TEXT);
        let rect = first[0].1;
        assert!((rect.x - (SHELL.x + SHELL.w + GAP)).abs() < 0.5, "beside the shell: {rect:?}");
        assert!((rect.y - SHELL.y).abs() < 0.5, "{rect:?}");
        let id = view.read_with(cx, |c, _| c.active_item()).expect("the note is active");
        assert!(view.read_with(cx, |c, _| c.doc.get(id).is_some_and(|i| i.rect == rect)));
        let editing = cx.update(|window, cx| {
            view.read(cx).notes.get(&id).is_some_and(|n| n.read(cx).editing(window, cx))
        });
        assert!(!editing, "saved content: nothing to type yet");
        save(&view, cx);
        let second = notes(&view, cx);
        assert_eq!(second.len(), 2, "{second:?}");
        assert!(!overlaps(second[0].1, second[1].1), "the second finds its own slot: {second:?}");
    }

    #[gpui::test]
    fn a_shells_relative_path_opens_a_card_in_its_directory(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let shell = SessionId::new();
        host_opens_in(&view, cx, shell, me, SHELL, 1, Where::loose("/tmp/work"));
        view.update_in(cx, |c, _window, cx| {
            c.session_moved(shell, "/tmp/work/sub".to_owned(), None);
            let terminal = c.terminals[&shell].clone();
            terminal.update(cx, |_v, cx| {
                cx.emit(TerminalViewEvent::ViewFile { path: "a/b.rs".to_owned(), line: None });
            });
        });
        cx.run_until_parked();
        let paths: Vec<String> = view.read_with(cx, |c, _| {
            c.items()
                .into_iter()
                .filter_map(|i| match &i.kind {
                    ItemKind::File { path } => Some(path.clone()),
                    _ => None,
                })
                .collect()
        });
        assert_eq!(paths, ["/tmp/work/sub/a/b.rs"], "the directory the shell moved to");
        assert!(
            drain(&mut rx).iter().any(
                |m| matches!(m, ClientMsg::ReadFile { path } if path == "/tmp/work/sub/a/b.rs")
            ),
            "read asked for the absolute path"
        );
    }

    #[test]
    fn a_file_card_is_titled_by_its_name_and_directory() {
        assert_eq!(file_title("/w/slopty/src/main.rs"), "main.rs · src");
        assert_eq!(file_title("/main.rs"), "main.rs");
        assert_eq!(file_title("main.rs"), "main.rs");
    }

    /// A note of commands is a runbook: each fenced block is drawn with a "copy" button, and
    /// "run" once the canvas has a shell — a click types the code into
    /// it (a paste, then ↩) without putting the caret in the note.
    #[gpui::test]
    fn a_notes_fenced_block_runs_in_the_canvas_shell(cx: &mut TestAppContext) {
        const TEXT: &str = "# Deploy\n\n```sh\necho hi\n```\n\nthen check.";
        let (view, mut rx, me, cx) = canvas(cx);
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: TEXT.to_owned() },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        view.update_in(cx, |c, _window, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
        });
        cx.run_until_parked();
        let copy = selector("note-code-copy", id);
        let copy: &'static str = Box::leak(format!("{copy}-1").into_boxed_str());
        let run: &'static str =
            Box::leak(format!("{}-1", selector("note-code-run", id)).into_boxed_str());
        assert!(cx.debug_bounds(copy).is_some(), "the block is its own element");
        assert!(cx.debug_bounds(run).is_none(), "no shell yet: nowhere to run it");
        let bounds = cx.debug_bounds(copy).expect("copy");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|i| i.text()));
        assert_eq!(copied.as_deref(), Some("echo hi"));
        let editing = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                view.read(cx).notes.get(&id).is_some_and(|n| n.read(cx).editing(window, cx))
            })
        };
        assert!(!editing(cx), "a button press is not a click into the note");

        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, Rect { x: 760.0, ..SHELL }, 2);
        cx.run_until_parked();
        // The reveal of the new shell flies the camera: let it land before reading bounds.
        cx.executor().advance_clock(Duration::from_secs(2));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let bounds = cx.debug_bounds(run).expect("with a shell the block has a run button");
        drain(&mut rx);
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(terminal_focused(&view, cx, shell), "the shell was revealed and took the keyboard");
        assert!(!editing(cx));
        let sent: Vec<TermRequest> = drain(&mut rx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Term { session, req }
                    if session == shell
                        && matches!(req, TermRequest::Paste(_) | TermRequest::Key(_)) =>
                {
                    Some(req)
                }
                _ => None,
            })
            .collect();
        match sent.as_slice() {
            [TermRequest::Paste(code), TermRequest::Key(key)] => {
                assert_eq!(code, "echo hi");
                assert_eq!(key.code, KeyCode::Enter, "{key:?}");
            }
            other => panic!("a paste then one ↩ into the shell: {other:?}"),
        }
    }

    /// The palette lists the last commands of the shell a "run" goes to, newest first and
    /// once each, and ↩ on one types it into that shell again.
    #[gpui::test]
    fn the_palette_reruns_a_recent_command(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        assert!(
            view.update(cx, |c, cx| c.palette_lines(cx))
                .iter()
                .all(|l| !matches!(l.run, PaletteRun::Rerun { .. })),
            "no shell: nothing to run again"
        );
        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, SHELL, 1);
        let prompt = |exit| SemanticMark::Prompt { exit, input: Some(2) };
        let rows = [
            ("$ make", prompt(None)),
            ("ok", SemanticMark::Output),
            ("$ cargo test", prompt(Some(0))),
            ("ok", SemanticMark::Output),
            ("$ make", prompt(Some(0))),
            ("ok", SemanticMark::Output),
            ("$ ", prompt(Some(0))),
        ];
        view.update_in(cx, |c, _window, cx| c.term_event(shell, marked_frame(1, &rows, 6), cx));
        cx.run_until_parked();
        let labels: Vec<String> = view
            .update(cx, |c, cx| c.palette_lines(cx))
            .into_iter()
            .filter(|l| matches!(l.run, PaletteRun::Rerun { .. }))
            .map(|l| l.label)
            .collect();
        assert_eq!(labels, ["Rerun make", "Rerun cargo test"], "newest first, once each");
        cx.executor().advance_clock(Duration::from_secs(2));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-shift-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("r e r u n space c a r g o");
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let options: Vec<String> = tree
            .iter()
            .filter(|n| n.role == "ListBoxOption")
            .filter_map(|n| n.label.clone())
            .collect();
        assert_eq!(options, ["Rerun cargo test shell"], "{tree:#?}");
        drain(&mut rx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(cx.debug_bounds("palette").is_none());
        assert!(terminal_focused(&view, cx, shell), "the shell took the keyboard");
        let sent: Vec<TermRequest> = drain(&mut rx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Term { session, req }
                    if session == shell
                        && matches!(req, TermRequest::Paste(_) | TermRequest::Key(_)) =>
                {
                    Some(req)
                }
                _ => None,
            })
            .collect();
        match sent.as_slice() {
            [TermRequest::Paste(code), TermRequest::Key(key)] => {
                assert_eq!(code, "cargo test");
                assert_eq!(key.code, KeyCode::Enter, "{key:?}");
            }
            other => panic!("a paste then one ↩ into the shell: {other:?}"),
        }
    }

    /// A note reads as Markdown until someone edits it: unfocused it is the rendered
    /// document a screen reader reads out, a click on it puts the caret in the editor, and
    /// the text survives the round trip unchanged.
    #[gpui::test]
    fn a_note_reads_as_markdown_until_it_is_edited(cx: &mut TestAppContext) {
        const TEXT: &str = "# Title\n- item";
        let (view, _rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: TEXT.to_owned() },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        view.update_in(cx, |c, _window, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
        });
        cx.run_until_parked();
        let editing = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                view.read(cx).notes.get(&id).is_some_and(|n| n.read(cx).editing(window, cx))
            })
        };

        // Nobody is editing: the rendered document, with the text to read out and no editor.
        assert!(!editing(cx), "a note nobody touched is not being edited");
        let read = selector("note-read", id);
        let bounds = cx.debug_bounds(read).expect("the rendered note");
        assert!(bounds.size.width > px(0.0) && bounds.size.height > px(0.0), "{bounds:?}");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let document = tree
            .iter()
            .find(|n| n.is("Document", Some("Note")))
            .unwrap_or_else(|| panic!("the rendered note reads itself out: {tree:#?}"));
        assert_eq!(document.value.as_deref(), Some(TEXT));
        assert!(!tree.iter().any(|n| n.role.ends_with("TextInput")), "no editor yet: {tree:#?}");

        // A click on it means "edit this": the editor takes over, carrying the same text.
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(editing(cx), "the caret is in the note");
        assert!(cx.debug_bounds(read).is_none(), "the rendered note gave way to the editor");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let input = tree
            .iter()
            .find(|n| n.role.ends_with("TextInput"))
            .unwrap_or_else(|| panic!("the editor is what is on screen: {tree:#?}"));
        assert_eq!(input.label.as_deref(), Some("Note"));
        assert_eq!(input.value.as_deref(), Some(TEXT), "the editor opened on the same text");

        // Typing appends at the end and settles into the document; the blur puts the reader
        // back, now reading the new text.
        cx.simulate_keystrokes("!");
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
        view.update_in(cx, |c, window, cx| window.focus(&c.focus, cx));
        cx.run_until_parked();
        assert!(!editing(cx), "the keyboard left the note");
        let edited = format!("{TEXT}!");
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        let document = tree
            .iter()
            .find(|n| n.is("Document", Some("Note")))
            .unwrap_or_else(|| panic!("the reader is back: {tree:#?}"));
        assert_eq!(document.value.as_deref(), Some(edited.as_str()), "the text round-trips");
        let stored = view.read_with(cx, |c, _| {
            c.items()
                .iter()
                .find_map(|i| match &i.kind {
                    ItemKind::Note { text } => Some(text.clone()),
                    _ => None,
                })
                .expect("the note is in the document")
        });
        assert_eq!(stored, edited, "the document holds the text the editor typed");
    }

    #[gpui::test]
    fn a_notes_task_ticks_on_a_click_without_opening_the_editor(cx: &mut TestAppContext) {
        const TEXT: &str = "# Plan\n- [ ] ship\n- [x] test\n";
        let (view, mut rx, me, cx) = canvas(cx);
        cx.update(|window, _cx| window.set_a11y_active(true));
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Note { text: TEXT.to_owned() },
            rect: SHELL,
            z: 1,
            group: None,
            sleeping: false,
            name: None,
        };
        let id = item.id;
        view.update_in(cx, |c, _window, cx| {
            c.apply_sync(CanvasSync::Delta { version: 1, by: me, op: CanvasOp::Upsert(item) }, cx);
        });
        cx.run_until_parked();
        let first = selector("note-task", id);
        let first: &'static str = Box::leak(format!("{first}-0").into_boxed_str());
        let second: &'static str =
            Box::leak(format!("{}-1", selector("note-task", id)).into_boxed_str());
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(
            tree.iter().any(|n| n.is("CheckBox", Some("To do: ship"))),
            "the open task reads as a box to tick: {tree:#?}"
        );
        assert!(tree.iter().any(|n| n.is("CheckBox", Some("Done: test"))), "{tree:#?}");
        assert!(cx.debug_bounds(second).is_some(), "each task line is its own row");

        drain(&mut rx);
        let bounds = cx.debug_bounds(first).expect("the first task's box");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        let editing = cx.update(|window, cx| {
            view.read(cx).notes.get(&id).is_some_and(|n| n.read(cx).editing(window, cx))
        });
        assert!(!editing, "a tick is not a click into the note");
        let ticked = "# Plan\n- [x] ship\n- [x] test\n";
        let sent = drain(&mut rx).into_iter().find_map(|m| match m {
            ClientMsg::Canvas(CanvasOp::Upsert(i)) if i.id == id => match i.kind {
                ItemKind::Note { text } => Some(text),
                _ => None,
            },
            _ => None,
        });
        assert_eq!(sent.as_deref(), Some(ticked), "the tick went to the document at once");
        let synced =
            view.read_with(cx, |c, cx| c.notes.get(&id).map(|n| n.read(cx).synced().to_owned()));
        assert_eq!(synced.as_deref(), Some(ticked));
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("CheckBox", Some("Done: ship"))), "{tree:#?}");

        // Off again from the same box.
        let bounds = cx.debug_bounds(first).expect("the box stays where it was");
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        let synced =
            view.read_with(cx, |c, cx| c.notes.get(&id).map(|n| n.read(cx).synced().to_owned()));
        assert_eq!(synced.as_deref(), Some(TEXT), "a second click unticks it");
    }
}
