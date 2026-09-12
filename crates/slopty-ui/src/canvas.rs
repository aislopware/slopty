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
    Action, App, AppContext as _, BorderStyle, Bounds, Context, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, PinchEvent, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, Size, StatefulInteractiveElement as _, Styled as _, SystemNotification,
    SystemNotificationAction, Task, Window, canvas, div, fill, outline, point, px, size,
};
use slopty_client::arrange::{self, Arrangeable, Heading};
use slopty_client::canvas::{
    CARD_ZOOM, Camera, CanvasDoc, FLIGHT, Flight, GAP, TERMINAL_SIZE, snap,
};
use slopty_core::{ClientId, ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{
    AgentAnswer, AgentEvent, AgentInfo, AgentSessionInfo, AgentSource, AgentStatus, AgentTask,
    BlockReason, OpenAgent, PermissionRequest, TranscriptUpdate,
};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, Rect};
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
use crate::note::{NoteView, NoteViewEvent};
use crate::palette::{CommandPalette, PaletteEvent, PaletteItem, PaletteRun};
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
            /// Open a Claude Code agent the host drives over its structured protocol.
            NewDrivenAgent,
            /// Pick a past Claude Code conversation on the host to resume as a driven agent.
            ResumeAgent,
            /// Put an empty note on the canvas.
            NewNote,
            /// Put a host window or display on the canvas.
            AddWindow,
            /// Close the active item (terminates its session).
            CloseItem,
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
            /// Move the keyboard focus to the next control (title-bar pills, badges, the
            /// composer's buttons), from anywhere, a terminal included.
            FocusNext,
            /// Move the keyboard focus to the previous control.
            FocusPrev,
            /// Open the command palette: every action by name, run by ↩.
            OpenPalette,
        ]
    );
}
pub use actions::{
    AddWindow, ArrangeByRepo, CloseItem, FitAll, FocusNext, FocusPrev, NewAgent, NewDrivenAgent,
    NewNote, NewTerminal, NextAttention, OpenPalette, ResumeAgent, ToggleMute, ToggleStats, ZoomIn,
    ZoomOut, ZoomReset, ZoomToItem,
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
    const CTX: Option<&str> = Some("Canvas");
    vec![
        KeyBinding::new("cmd-t", NewTerminal, CTX),
        KeyBinding::new("cmd-n", NewTerminal, CTX),
        KeyBinding::new("cmd-shift-t", NewAgent, CTX),
        KeyBinding::new("cmd-alt-t", NewDrivenAgent, CTX),
        KeyBinding::new("cmd-alt-r", ResumeAgent, CTX),
        KeyBinding::new("cmd-shift-n", NewNote, CTX),
        KeyBinding::new("cmd-o", AddWindow, CTX),
        KeyBinding::new("cmd-w", CloseItem, CTX),
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
        KeyBinding::new("ctrl-tab", FocusNext, CTX),
        KeyBinding::new("ctrl-shift-tab", FocusPrev, CTX),
        KeyBinding::new("cmd-shift-p", OpenPalette, CTX),
    ]
}

/// The palette's lines for the canvas's and the terminal's actions, with their keys.
#[must_use]
pub fn palette_items() -> Vec<PaletteItem> {
    use crate::terminal::{
        ClearScreen, CopyLastOutput, Find, NextPrompt, PrevPrompt, RerunLast, ToggleConversation,
    };
    let canvas = key_bindings();
    let terminal = crate::terminal::key_bindings();
    let c = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &canvas);
    let t = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &terminal);
    vec![
        c("New terminal", Box::new(NewTerminal)),
        c("New agent", Box::new(NewAgent)),
        c("New conversation (driven agent)", Box::new(NewDrivenAgent)),
        c("Resume a conversation", Box::new(ResumeAgent)),
        c("New note", Box::new(NewNote)),
        c("Add a window or display", Box::new(AddWindow)),
        c("Close item", Box::new(CloseItem)),
        c("Zoom in", Box::new(ZoomIn)),
        c("Zoom out", Box::new(ZoomOut)),
        c("Zoom to 100%", Box::new(ZoomReset)),
        c("Fit all", Box::new(FitAll)),
        c("Zoom to item", Box::new(ZoomToItem)),
        c("Arrange by repository", Box::new(ArrangeByRepo)),
        c("Next attention", Box::new(NextAttention)),
        c("Mute or unmute window", Box::new(ToggleMute)),
        c("Stream stats", Box::new(ToggleStats)),
        t("Find in terminal", Box::new(Find)),
        t("Previous prompt", Box::new(PrevPrompt)),
        t("Next prompt", Box::new(NextPrompt)),
        t("Copy last output", Box::new(CopyLastOutput)),
        t("Rerun last command", Box::new(RerunLast)),
        t("Clear the screen and history", Box::new(ClearScreen)),
        t("Show or hide the conversation", Box::new(ToggleConversation)),
    ]
}

/// The program a "+ agent" terminal runs.
pub const AGENT_COMMAND: &str = "claude";

/// A stable element id for `(part, item)`.
fn element_id(part: &str, id: ItemId) -> ElementId {
    ElementId::from(format!("{part}-{}", id.as_uuid()))
}

/// Title bar height at zoom 1, in points.
const TITLE_H: f32 = 28.0;
/// Resize grip size at zoom 1.
const GRIP: f32 = 14.0;
/// How long after the last zoom change the settled frame (exact rasters) is asked for. A
/// gesture reports every few milliseconds; a frame per report and one more after the pause.
const SETTLE: Duration = Duration::from_millis(80);
/// Smallest item on screen while dragging.
const MIN_ITEM: f32 = 160.0;
/// Size of a new note.
const NOTE_SIZE: (f32, f32) = (320.0, 240.0);
/// Minimap box (points) and its distance from the viewport's bottom-right corner.
const MINIMAP: (f32, f32) = (160.0, 100.0);
const MINIMAP_MARGIN: f32 = 12.0;
const MINIMAP_PAD: f32 = 6.0;
/// Zoom step for ⌘= / ⌘-.
const ZOOM_STEP: f32 = 1.25;
/// Largest item a picked window gets on the canvas, in points.
const MAX_PICKED: (f32, f32) = (1600.0, 1000.0);

/// What the user chose on a blocked agent's badge; shown until the host reports the agent's
/// next state, so a second tap cannot send a second key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Answer {
    Allowed,
    Denied,
}

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

/// A note's title: its first non-empty line with Markdown's heading, list and quote marks
/// stripped, cut to [`NOTE_TITLE_CHARS`]; "note" while it is empty.
#[must_use]
pub fn note_title(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim())
        .find(|l| !l.is_empty());
    match line {
        None => "note".to_owned(),
        Some(line) if line.chars().count() > NOTE_TITLE_CHARS => {
            let cut: String = line.chars().take(NOTE_TITLE_CHARS).collect();
            format!("{}…", cut.trim_end())
        }
        Some(line) => line.to_owned(),
    }
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
    /// The badge text: "done 3.2 s", "failed (1) 1 min 4 s".
    #[must_use]
    pub fn label(&self) -> String {
        let secs = self.elapsed.as_secs_f64();
        let took = if secs < 60.0 {
            format!("{secs:.1} s")
        } else {
            let whole = self.elapsed.as_secs();
            format!("{} min {} s", whole / 60, whole % 60)
        };
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
    /// Badge answers sent but not yet reflected by the host.
    answered: HashMap<SessionId, Answer>,
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
    /// Where the keyboard was when the palette opened; it goes back there when it closes.
    palette_return: Option<FocusHandle>,
    /// What the palette chose, dispatched on the next frame once the focus is back.
    palette_action: Option<Box<dyn Action>>,
    /// The app's own lines for the palette (settings, hosts), after the canvas's.
    palette_extra: Vec<PaletteItem>,
    /// Focus the palette's field on the next frame.
    pending_focus_palette: bool,
    /// A `ListAgentSessions` is in flight for the resume picker.
    resume_wanted: bool,
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
    /// A block waiting for the agent card that "Ask the agent" is opening.
    pending_ask: Option<String>,
    /// A block waiting for its card's view (the session is known, the item not placed yet).
    pending_compose: Option<(SessionId, String)>,
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
            answered: HashMap::new(),
            finished: HashMap::new(),
            slow_command: SLOW_COMMAND,
            hooks_offered: false,
            screens: HashMap::new(),
            notes: HashMap::new(),
            pending_opens: HashMap::new(),
            titles_requested: false,
            titles: HashMap::new(),
            open_screen,
            picker: None,
            picker_wanted: false,
            palette: None,
            palette_return: None,
            palette_action: None,
            palette_extra: Vec::new(),
            pending_focus_palette: false,
            resume_wanted: false,
            display_wanted: false,
            show_stats: false,
            rtt: None,
            viewport: (point(px(0.0), px(0.0)), size(px(1.0), px(1.0))),
            fit_pending: false,
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
            pending_ask: None,
            pending_compose: None,
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
        let change = self.doc.apply_sync(sync, self.me);
        tracing::debug!(?change, version = self.doc.version(), "canvas sync");
        self.reconcile(cx);
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
        let Some((max_w, max_h)) = self.viewport_max() else { return };
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
        let asked = (summary.kind == SessionKind::Agent).then(|| self.pending_ask.take()).flatten();
        let id = summary.id;
        self.sessions.insert(id, summary);
        self.reconcile(cx);
        if let Some(text) = asked {
            self.compose_in(id, text, cx);
        }
        cx.notify();
    }

    /// "Ask the agent" on a command block: the block goes into the composer of the agent card
    /// the human is on, else the topmost one, else a new one opened in the active shell's
    /// directory (the block waits for it).
    pub fn ask_agent(&mut self, text: String, cx: &mut Context<Self>) {
        let is_agent = |item: &CanvasItem| match &item.kind {
            ItemKind::Terminal { session } => self.is_driven(*session).then_some(*session),
            _ => None,
        };
        let target = self
            .active
            .and_then(|id| self.doc.get(id))
            .and_then(is_agent)
            .or_else(|| self.doc.by_z().into_iter().rev().find_map(is_agent));
        if let Some(session) = target {
            self.reveal_session(session, cx);
            self.compose_in(session, text, cx);
        } else {
            self.pending_ask = Some(text);
            self.open_agent(self.active_cwd(), cx);
        }
    }

    /// Put `text` into a driven session's composer, as soon as the session has a view (the
    /// host says a session opened before it places the item that draws it).
    fn compose_in(&mut self, session: SessionId, text: String, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.compose(text, cx));
        } else {
            self.pending_compose = Some((session, text));
        }
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

    /// Tell every terminal view whether there is a shell to run a fenced block in, so the
    /// conversation draws the "run" button only when a click on it would go somewhere. Called
    /// whenever the set of shells can have changed.
    fn update_run_targets(&self, cx: &mut Context<Self>) {
        let can = self.run_target().is_some();
        for view in self.terminals.values() {
            view.update(cx, |v, cx| v.set_can_run_in_shell(can, cx));
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
        self.answered.remove(&session);
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
            ItemKind::Terminal { .. } | ItemKind::Note { .. } => None,
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
        // Whatever the host says next supersedes a pending badge answer.
        self.answered.remove(&session);
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
                Self::notify_system(&event, cx);
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
        if watched || done.elapsed < self.slow_command {
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
    /// the badge text as title, the detail as body, allow/deny buttons for a permission.
    /// GPUI drops it silently outside a bundle or when the user declined notifications.
    fn notify_system(event: &AgentEvent, cx: &Context<Self>) {
        let active = cx.active_window().is_some();
        tracing::debug!(active, session = %event.session, "agent banner");
        if active {
            return;
        }
        let actions = match &event.status {
            AgentStatus::Blocked(BlockReason::Permission { .. }) => vec![
                SystemNotificationAction { id: "allow".into(), label: "Allow".into() },
                SystemNotificationAction { id: "deny".into(), label: "Deny".into() },
            ],
            _ => Vec::new(),
        };
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
        let body = event.detail.clone().filter(|d| !d.is_empty()).unwrap_or_default();
        cx.show_system_notification(SystemNotification {
            tag: event.session.to_string().into(),
            title: title.into(),
            body: body.into(),
            actions,
        });
    }

    /// The user activated a notification: the body reveals the session, the buttons answer
    /// the permission prompt. Called from the app's response handler with the tag parsed.
    pub fn notification_response(
        &mut self,
        session: SessionId,
        action: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        match action {
            Some("allow") => self.allow_agent(session, cx),
            Some("deny") => self.deny_agent(session, cx),
            _ => self.reveal_session(session, cx),
        }
    }

    /// Sessions whose agent is waiting on the human and has not been answered from the
    /// badge, in reading order (top to bottom, left to right) so ⌘⇧A walks the canvas
    /// predictably.
    fn needs_you(&self) -> Vec<(ItemId, SessionId)> {
        let mut out: Vec<(Rect, ItemId, SessionId)> = self
            .doc
            .items()
            .filter_map(|i| match i.kind {
                ItemKind::Terminal { session } => Some((i.rect, i.id, session)),
                _ => None,
            })
            .filter(|(_, _, s)| {
                self.agents.get(s).is_some_and(needs_human) && !self.answered.contains_key(s)
            })
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

    /// "allow" on a permission badge. Claude Code's permission prompt is a numbered menu with
    /// "Yes" highlighted; Enter takes it, exactly as if typed in the terminal.
    pub fn allow_agent(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.answer_agent(session, "enter", Answer::Allowed, cx);
    }

    /// "deny" on a permission badge: Esc is the prompt's "No" (it says so on the option).
    pub fn deny_agent(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.answer_agent(session, "escape", Answer::Denied, cx);
    }

    fn answer_agent(
        &mut self,
        session: SessionId,
        key: &str,
        answer: Answer,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.terminals.get(&session) else {
            // No live view (sleeping item): the best we can do is bring the terminal up.
            self.reveal_session(session, cx);
            return;
        };
        if self.answered.contains_key(&session) {
            return;
        }
        if self.is_driven(session) {
            // A driven agent is answered by request id, not by a key: the view knows the
            // request and reports back through `TerminalViewEvent::AgentAnswered`.
            view.update(cx, |view, cx| view.answer(answer == Answer::Allowed, cx));
            return;
        }
        view.update(cx, |view, cx| {
            // A Control armed on the phone's key bar is for the next typed key, not for this.
            if view.sticky_control() {
                view.set_sticky_control(false, cx);
            }
            let keystroke =
                Keystroke { modifiers: Modifiers::default(), key: key.to_owned(), key_char: None };
            view.press(keystroke, cx);
        });
        self.answered.insert(session, answer);
        cx.dismiss_system_notification(&session.to_string());
        self.count_needs_you(cx);
        cx.notify();
    }

    /// Whether the host drives the session's agent (a `SessionKind::Agent` session).
    fn is_driven(&self, session: SessionId) -> bool {
        self.sessions.get(&session).is_some_and(|s| s.kind == SessionKind::Agent)
    }

    /// The driven view's Allow / Deny, by request id: the host answers Claude Code, and the
    /// badge clears as for a typed answer.
    fn answer_driven(&mut self, answer: AgentAnswer, cx: &mut Context<Self>) {
        let (session, allowed) = (answer.session, answer.allowed);
        self.send(ClientMsg::AgentAnswer(answer));
        let answer = if allowed { Answer::Allowed } else { Answer::Denied };
        self.answered.insert(session, answer);
        cx.dismiss_system_notification(&session.to_string());
        self.count_needs_you(cx);
        cx.notify();
    }

    /// Bring a session's terminal into view, make it active and give it the keyboard (the
    /// "answer" button on a question badge: the reply has to be typed).
    pub fn reveal_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(id) = self.doc.item_for_session(session).map(|i| i.id) else { return };
        self.activate(id, cx);
        self.reveal_pending = Some(id);
        self.pending_focus = Some(session);
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
        if let Some(picker) = &self.picker {
            picker.update(cx, |p, cx| p.set_theme(theme.clone(), cx));
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
            ScreenEvent::Cursor { .. } | ScreenEvent::ListingChanged => {}
        }
        cx.notify();
    }

    /// Requested quality for a new stream: full scale unless the canvas is zoomed out.
    fn quality_for(&self) -> Quality {
        let zoom = self.camera.zoom.clamp(0.25, 1.0);
        let scale = (zoom * 4.0).ceil() / 4.0;
        Quality { scale, ..Quality::default() }
    }

    /// Lines the app adds to the palette after the canvas's own (settings, hosts).
    pub fn extend_palette(&mut self, items: Vec<PaletteItem>) {
        self.palette_extra = items;
    }

    /// Every line the palette offers: the canvas's sessions to go to (agents waiting on the
    /// human first, as the picker orders them), then every action, then the app's own.
    #[must_use]
    pub fn palette_lines(&self, cx: &Context<Self>) -> Vec<PaletteItem> {
        let mut items: Vec<PaletteItem> = self
            .session_rows(cx)
            .into_iter()
            .map(|row| {
                PaletteItem::session(&row.title, &row.status.unwrap_or_default(), row.session)
            })
            .collect();
        items.extend(palette_items());
        items.extend(self.palette_extra.iter().cloned());
        items
    }

    /// ⌘⇧P: the command palette over whatever has the keyboard; the choice runs once it is
    /// gone and the focus is back.
    pub fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        self.palette_return = window.focused(cx);
        let items = self.palette_lines(cx);
        let theme = self.theme.clone();
        let palette = cx.new(|cx| CommandPalette::new(items, theme, window, cx));
        self.subscriptions.push(cx.subscribe(&palette, |this, _palette, event, cx| {
            this.palette = None;
            match event {
                PaletteEvent::Run(PaletteRun::Action(action)) => {
                    this.palette_action = Some(action.boxed_clone());
                }
                PaletteEvent::Run(PaletteRun::Session(session)) => {
                    // The terminal takes the keyboard, not whoever had it before.
                    this.palette_return = None;
                    this.reveal_session(*session, cx);
                }
                PaletteEvent::Dismiss => {}
            }
            cx.notify();
        }));
        self.pending_focus_palette = true;
        self.palette = Some(palette);
        cx.notify();
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
                PickerEvent::Resume(agent) => this.resume_agent_session(agent, cx),
                PickerEvent::Dismiss | PickerEvent::Everywhere => {}
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
                ItemKind::Terminal { session } => Some((i.rect, session)),
                _ => None,
            })
            .map(|(rect, session)| {
                let agent = self.agents.get(&session);
                let needs_you =
                    agent.is_some_and(needs_human) && !self.answered.contains_key(&session);
                let rank = match agent {
                    _ if needs_you => 0,
                    Some(_) => 1,
                    None => 2,
                };
                let row = SessionRow {
                    session,
                    title: self.terminal_title(session, cx),
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
        let shrink = (MAX_PICKED.0 / w).min(MAX_PICKED.1 / h).min(1.0);
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
        };
        self.propose(CanvasOp::Upsert(item));
        self.active = Some(id);
    }

    /// What a driven agent is writing now, for the view showing it.
    pub fn agent_partial(&self, session: SessionId, text: String, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.agent_partial(text, cx));
        }
    }

    /// What a driven agent says about itself, for the view showing it.
    pub fn agent_info(&self, session: SessionId, info: AgentInfo, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.agent_info(info, cx));
        }
    }

    /// The host's answer to a view's `@file` query.
    pub fn agent_files(
        &self,
        session: SessionId,
        query: String,
        paths: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.files(query, paths, cx));
        }
    }

    /// A subagent a driven agent spawned, for the view showing it.
    pub fn agent_task(&self, session: SessionId, task: AgentTask, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.agent_task(task, cx));
        }
    }

    /// The host's answer to ⌘⌥R: the conversations on disk for the directory asked about,
    /// shown as the resume picker.
    pub fn agent_sessions(
        &mut self,
        cwd: Option<String>,
        sessions: Vec<AgentSessionInfo>,
        cx: &mut Context<Self>,
    ) {
        if !std::mem::take(&mut self.resume_wanted) {
            return;
        }
        let theme = self.theme.clone();
        let picker = cx.new(|cx| WindowPicker::resume(sessions, cwd, theme, cx));
        self.subscriptions.push(cx.subscribe(&picker, |this, _picker, event, cx| {
            match event {
                PickerEvent::Resume(agent) => this.resume_agent_session(agent, cx),
                PickerEvent::Everywhere => {
                    // The whole host's list replaces this directory's when it arrives.
                    this.resume_wanted = true;
                    this.send(ClientMsg::ListAgentSessions { cwd: None });
                }
                _ => {}
            }
            this.picker = None;
            this.pending_focus_self = true;
            cx.notify();
        }));
        self.pending_focus_picker = true;
        self.picker = Some(picker);
        cx.notify();
    }

    /// A row of the resume picker: open the conversation as a driven agent in its directory,
    /// titled by its first prompt.
    fn resume_agent_session(&self, agent: &AgentSessionInfo, cx: &mut Context<Self>) {
        let title =
            if agent.title.is_empty() { AGENT_COMMAND.to_owned() } else { agent.title.clone() };
        self.open_agent_with(
            OpenAgent {
                cwd: Some(agent.cwd.clone()),
                resume: Some(agent.id.clone()),
                model: None,
                title: Some(title),
            },
            cx,
        );
    }

    /// A driven agent's tool call waiting on the human, for the view showing it.
    pub fn agent_permission(
        &self,
        session: SessionId,
        request: PermissionRequest,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.agent_permission(request, cx));
        }
    }

    /// A slice of an agent session's conversation, for the terminal showing it.
    pub fn transcript_update(&self, update: TranscriptUpdate, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(&update.session) {
            view.update(cx, |v, cx| v.transcript_update(update, cx));
        }
    }

    /// A session-stream event.
    pub fn term_event(&self, session: SessionId, event: TermEvent, cx: &mut Context<Self>) {
        match (self.terminals.get(&session), event) {
            (Some(view), event) => view.update(cx, |v, cx| v.apply(event, cx)),
            (None, TermEvent::Error(e)) => tracing::warn!(%session, error = %e, "host"),
            (None, _other) => {}
        }
    }

    /// Create views for terminal items whose session is alive; drop views whose item is gone.
    fn reconcile(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<SessionId> = self
            .doc
            .items()
            .filter_map(|i| match i.kind {
                ItemKind::Terminal { session } if !i.sleeping => Some(session),
                _ => None,
            })
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
                    TerminalViewEvent::Exited(_) => {
                        this.send(ClientMsg::Term { session: sid, req: TermRequest::Close });
                    }
                    TerminalViewEvent::Title(_) => cx.notify(),
                    TerminalViewEvent::Cwd { path, repo } => {
                        this.session_moved(sid, path.clone(), repo.clone());
                    }
                    TerminalViewEvent::Answered { allowed: true } => this.allow_agent(sid, cx),
                    TerminalViewEvent::Answered { allowed: false } => this.deny_agent(sid, cx),
                    TerminalViewEvent::AgentAnswered { request, allowed, answers, always } => {
                        this.answer_driven(
                            AgentAnswer {
                                session: sid,
                                request: request.clone(),
                                allowed: *allowed,
                                message: None,
                                answers: answers.clone(),
                                always: *always,
                            },
                            cx,
                        );
                    }
                    TerminalViewEvent::Notice(text) => cx.emit(CanvasEvent::Notice(text.clone())),
                    TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                        let done =
                            Finished { command: command.clone(), exit: *exit, elapsed: *elapsed };
                        this.command_finished(sid, done, cx);
                    }
                    TerminalViewEvent::RunInShell(code) => this.run_in_shell(code.clone(), cx),
                    TerminalViewEvent::AskAgent(text) => this.ask_agent(text.clone(), cx),
                },
            ));
            if self.is_driven(*session) {
                view.update(cx, TerminalView::set_driven);
            }
            self.send(ClientMsg::Term { session: *session, req: TermRequest::Attach { size } });
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
        if let Some((session, text)) = self.pending_compose.take() {
            self.compose_in(session, text, cx);
        }
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
                ItemKind::Terminal { .. } | ItemKind::Note { .. } => None,
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

    /// ⌘F with the canvas (not a terminal) focused: search in the active terminal.
    pub fn find_in_active(
        &mut self,
        action: &crate::terminal::Find,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.active_terminal() {
            view.update(cx, |view, cx| view.find(action, window, cx));
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

    /// ⌘⌥T: a Claude Code agent the host drives over its stream-json protocol, shown as a
    /// conversation card; it starts in the active terminal's directory when there is one.
    pub fn new_driven_agent(
        &mut self,
        _: &NewDrivenAgent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_agent(self.active_cwd(), cx);
    }

    /// Open a driven agent in `cwd` (the host's default when `None`); the host places it.
    pub fn open_agent(&self, cwd: Option<String>, cx: &mut Context<Self>) {
        self.open_agent_with(
            OpenAgent { cwd, resume: None, model: None, title: Some(AGENT_COMMAND.to_owned()) },
            cx,
        );
    }

    /// Open a driven agent as `req` says (a fresh conversation or a resumed one); the host
    /// places it.
    pub fn open_agent_with(&self, req: OpenAgent, cx: &mut Context<Self>) {
        tracing::debug!(cwd = ?req.cwd, resume = ?req.resume, "open driven agent");
        self.send(ClientMsg::OpenAgent(req));
        cx.notify();
    }

    /// ⌘⌥R: ask the host for the Claude Code conversations in the active terminal's
    /// directory (every directory on the host without one, as on the phone); the picker
    /// opens when the list arrives.
    pub fn resume_agent(&mut self, _: &ResumeAgent, _window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active.and_then(|id| self.doc.get(id)).and_then(|item| self.cwd_of(item));
        self.resume_wanted = true;
        self.send(ClientMsg::ListAgentSessions { cwd });
        cx.notify();
    }

    /// Open a session running `command` (the login shell when empty), titled after its
    /// program; the host places it. The self-test socket's way to put load on the canvas.
    pub fn open_command(&self, command: Vec<String>, cx: &mut Context<Self>) {
        let title = command.first().cloned();
        self.open_session(command, title, cx);
    }

    fn open_session(&self, command: Vec<String>, title: Option<String>, cx: &mut Context<Self>) {
        tracing::debug!(?command, "open session");
        self.send(ClientMsg::OpenSession(OpenSession {
            size: TermSize::default(),
            cwd: self.active_cwd(),
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
        let rect = self.doc.free_slot(NOTE_SIZE);
        let id = ItemId::new();
        let item = CanvasItem {
            id,
            kind: ItemKind::Note { text: String::new() },
            rect,
            z: self.doc.top_z().saturating_add(1),
            group: None,
            sleeping: false,
        };
        self.propose(CanvasOp::Upsert(item));
        self.active = Some(id);
        self.reveal_pending = Some(id);
        self.pending_focus_note = Some(id);
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
                let NoteViewEvent::Commit(text) = event;
                this.commit_note(item, text.clone());
                cx.notify();
            }));
            self.notes.insert(*id, view);
        }
        self.notes.retain(|id, _| notes.iter().any(|(n, _)| n == id));
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
            // has nothing to close, so drop the item straight from the document.
            ItemKind::Terminal { session } if self.sessions.contains_key(&session) => {
                self.send(ClientMsg::Term { session, req: TermRequest::Close });
            }
            ItemKind::Terminal { .. }
            | ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Note { .. } => {
                self.propose(CanvasOp::Remove(id));
            }
        }
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.take_camera();
        let (_, vp) = self.viewport;
        self.camera.zoom_at(factor, f32::from(vp.width) / 2.0, f32::from(vp.height) / 2.0);
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    /// ⌘=
    pub fn zoom_in(&mut self, _: &ZoomIn, _window: &mut Window, cx: &mut Context<Self>) {
        self.zoom_by(ZOOM_STEP, cx);
    }

    /// ⌘-
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

    /// ⌘1
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
    }

    /// Move the camera to `target`, over [`FLIGHT`] seconds unless animation is off.
    fn fly_to(&mut self, target: Camera, cx: &mut Context<Self>) {
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
        match item.kind {
            ItemKind::Terminal { session } => {
                self.sessions.get(&session).and_then(|s| s.cwd.clone())
            }
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
            Drag::Move { id, .. } | Drag::Resize { id, .. } => {
                if let Some(item) = self.doc.get(id) {
                    let r = item.rect;
                    let rect = Rect { x: snap(r.x), y: snap(r.y), w: snap(r.w), h: snap(r.h) };
                    self.propose(CanvasOp::Place { id, rect });
                }
            }
        }
        cx.notify();
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

        let kind = match item.kind {
            ItemKind::Terminal { .. } => "terminal",
            ItemKind::Window { .. } => "window",
            ItemKind::Display { .. } => "display",
            ItemKind::Note { .. } => "note",
        };
        let (title, focused) = match &item.kind {
            ItemKind::Terminal { session } => {
                let view = self.terminals.get(session);
                let focused = view.is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window));
                (self.terminal_title(*session, cx), focused)
            }
            ItemKind::Window { window: host_window } => {
                let view = self.screens.get(&item.id);
                let focused = view.is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window));
                let title = self
                    .titles
                    .get(&item.id)
                    .cloned()
                    .unwrap_or_else(|| format!("window {}", host_window.0));
                (title, focused)
            }
            ItemKind::Display { display } => {
                let view = self.screens.get(&item.id);
                let focused = view.is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window));
                (format!("display {display}"), focused)
            }
            ItemKind::Note { text } => {
                let focused =
                    self.notes.get(&item.id).is_some_and(|v| v.read(cx).editing(window, cx));
                (note_title(text), focused)
            }
        };
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
        // A session with an agent offers its conversation in place of the grid.
        let chat = agent.and_then(|(session, _)| {
            let view = self.terminals.get(&session)?;
            let on = view.read(cx).conversation().is_some();
            Some(chat_button(id, view.clone(), on, theme, chrome, cx))
        });
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
        let needs_human = agent
            .is_some_and(|(session, a)| needs_human(a) && !self.answered.contains_key(&session));
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
                cx.listener(move |this, ev, _w, cx| this.begin_move(id, ev, cx)),
            )
            .child(div().size(px(theme.spacing.sm * k)).rounded_full().bg(hsla(if focused {
                theme.surfaces.accent
            } else {
                theme.surfaces.text_muted
            })))
            // "take" sits left of the title so it stays reachable on a phone when the item is
            // wider than the screen.
            .when_some(take, gpui::ParentElement::child)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(ChromeText::new(title, px(ui_base), k).fill().zooming(chrome.zooming)),
            )
            .when_some(hooks, gpui::ParentElement::child)
            .when_some(chat, gpui::ParentElement::child)
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
        let entity = cx.entity();
        let rects: Vec<Rect> = items.iter().map(|(r, _)| *r).collect();
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
        let minimap = (!empty).then(|| self.render_minimap(cx));
        div()
            .id("canvas")
            .debug_selector(|| "canvas".to_owned())
            .key_context("Canvas")
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label("Canvas")
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(self.theme.surfaces.canvas))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::new_driven_agent))
            .on_action(cx.listener(Self::resume_agent))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::add_window))
            .on_action(cx.listener(Self::close_item))
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
            .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPrev, window, cx| window.focus_prev(cx))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .capture_pinch(cx.listener(Self::pinch))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_pan))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(record_bounds)
            .children(headings)
            .children(rendered)
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

/// The "chat" pill in an agent terminal's title bar: the conversation in place of the grid.
fn chat_button(
    id: ItemId,
    view: Entity<TerminalView>,
    on: bool,
    theme: &Theme,
    chrome: Chrome,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    let tone = if on { theme.surfaces.accent } else { theme.surfaces.text_secondary };
    let pill = pill("chat", id, "chat", tone, theme, chrome).role(Role::Button).aria_label(if on {
        "show terminal"
    } else {
        "show chat"
    });
    tab_stop(pill, theme.surfaces.accent)
        .on_click(cx.listener(move |_this, _ev, window, cx| {
            view.update(cx, |v, cx| v.toggle_conversation(window, cx));
        }))
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
) -> gpui::Stateful<gpui::Div> {
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
        let answered = self.answered.get(&session).copied();
        let (label, color) = match (&agent.status, answered) {
            (AgentStatus::None, _) => return div().into_any_element(),
            (AgentStatus::Blocked(_), Some(Answer::Allowed)) => {
                ("allowed".to_owned(), theme.surfaces.text_muted)
            }
            (AgentStatus::Blocked(_), Some(Answer::Denied)) => {
                ("denied".to_owned(), theme.surfaces.text_muted)
            }
            (AgentStatus::Idle | AgentStatus::Blocked(BlockReason::IdlePrompt), _) => {
                (agent_status_text(agent), theme.surfaces.text_muted)
            }
            // Busy states (thinking, a tool) share the accent: the label says which.
            (AgentStatus::Working | AgentStatus::Tool { .. }, _) => {
                (agent_status_text(agent), theme.surfaces.accent)
            }
            (AgentStatus::Blocked(_), None) => (agent_status_text(agent), theme.surfaces.warn),
            (AgentStatus::Done, _) => (agent_status_text(agent), theme.surfaces.success),
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
        let buttons: Vec<gpui::AnyElement> = match (&agent.status, answered) {
            (AgentStatus::Blocked(BlockReason::Permission { .. }), None) => vec![
                button("allow", "allow", true)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.allow_agent(session, cx)))
                    .into_any_element(),
                button("deny", "deny", false)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.deny_agent(session, cx)))
                    .into_any_element(),
            ],
            (AgentStatus::Blocked(BlockReason::Question | BlockReason::Elicitation), None) => vec![
                button("answer", "answer", true)
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
    use slopty_proto::agent::{AgentKind, TranscriptBody, TranscriptEntry};
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

    /// The host opened a session it drives as an agent (`SessionKind::Agent`): the card is
    /// the conversation, and it is never a shell a fenced block can run in.
    fn host_opens_agent(
        view: &Entity<CanvasView>,
        cx: &mut VisualTestContext,
        session: SessionId,
        by: ClientId,
        rect: Rect,
        version: u64,
    ) -> ItemId {
        let item = CanvasItem {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            rect,
            z: u32::try_from(version).unwrap(),
            group: None,
            sleeping: false,
        };
        let id = item.id;
        let summary = SessionSummary {
            kind: SessionKind::Agent,
            id: session,
            title: "claude".into(),
            cwd: None,
            repo: None,
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
    fn a_note_is_titled_by_its_first_line() {
        assert_eq!(note_title(""), "note");
        assert_eq!(note_title("\n  \n# Plan for today\n- x"), "Plan for today");
        assert_eq!(note_title("- first item"), "first item");
        let long = "a".repeat(NOTE_TITLE_CHARS + 5);
        assert_eq!(note_title(&long), format!("{}…", "a".repeat(NOTE_TITLE_CHARS)));
    }

    #[test]
    fn a_finished_badge_says_the_status_and_the_time() {
        let done =
            |exit, secs| Finished { command: "x".into(), exit, elapsed: Duration::from_secs(secs) };
        assert_eq!(done(Some(0), 7).label(), "done 7.0 s");
        assert_eq!(done(None, 7).label(), "done 7.0 s");
        assert_eq!(done(Some(1), 65).label(), "failed (1) 1 min 5 s");
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
                ClientMsg::Term { session, req: TermRequest::Close } if *session == a
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

        let parts = ["chat", "badge", "allow", "deny"];
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
        assert!(at("Heading", "terminal shell") < at("Button", "show chat"));
        assert!(at("Button", "show chat") < at("Status", "allow? Bash"));
        assert!(at("Status", "allow? Bash") < at("Button", "allow"));
        assert!(at("Button", "allow") < at("Button", "deny"));
        assert!(at("Button", "deny") < at("Terminal", "shell"), "the grid after its title bar");
        assert!(at("Terminal", "shell") < at("Image", "Canvas overview"));

        // The terminal holds the keyboard (Tab is the shell's); ⌃Tab enters the ring, then
        // Tab walks it in reading order.
        assert!(terminal_focused(&view, cx, session), "the new shell has the keyboard");
        drain(&mut rx);
        let mut order = Vec::new();
        for step in 0..4 {
            cx.simulate_keystrokes(if step == 0 { "ctrl-tab" } else { "tab" });
            cx.run_until_parked();
            let tree = cx.update(|window, _cx| crate::a11y::tree(window));
            let focused = tree.iter().find(|n| n.focused).expect("a focused node");
            order.push(focused.label.clone().unwrap_or_default());
            if focused.label.as_deref() == Some("deny") {
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
                    "a focus ring around deny at {x},{y}"
                );
                break;
            }
        }
        order.retain(|l| l != "take over");
        assert_eq!(order, ["show chat", "allow", "deny"], "Tab order");

        // Enter (down, then up) on "deny" is the click: Esc goes to the prompt.
        cx.simulate_keystrokes("enter");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: Keystroke::parse("enter").unwrap() });
        cx.run_until_parked();
        let keys: Vec<_> = drain(&mut rx)
            .into_iter()
            .filter_map(|m| match m {
                ClientMsg::Term { req: TermRequest::Key(key), .. } => Some(key.code),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [slopty_proto::input::KeyCode::Escape], "{keys:?}");
        let tree = cx.update(|window, _| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.is("Button", Some("deny"))), "answered: {tree:#?}");
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

    /// ⌘⌥R asks the host for the conversations on disk; the answer opens the picker as a
    /// resume list, and a row opens that conversation as a driven agent in its directory,
    /// titled by its first prompt. A list for one directory offers every directory on the
    /// host; the whole-host list does not.
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
        assert!(tree.iter().any(|n| n.is("ListBoxOption", Some("Find in terminal ⌘F"))));
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
    }

    #[gpui::test]
    fn a_past_conversation_is_resumed_from_the_picker(cx: &mut TestAppContext) {
        let (view, mut rx, _me, cx) = canvas(cx);
        cx.simulate_keystrokes("cmd-alt-r");
        cx.run_until_parked();
        let asked = drain(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ClientMsg::ListAgentSessions { cwd: None }]),
            "{asked:?}"
        );
        let found = vec![
            AgentSessionInfo {
                id: "19146b4d".to_owned(),
                cwd: "/w/slopty".to_owned(),
                title: "fix the build".to_owned(),
                modified_ms: 0,
            },
            AgentSessionInfo {
                id: "2".to_owned(),
                cwd: "/w/slopty".to_owned(),
                title: String::new(),
                modified_ms: 0,
            },
        ];
        view.update_in(cx, |c, _window, cx| {
            c.agent_sessions(Some("/w/slopty".to_owned()), found.clone(), cx);
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Resume a conversation"))), "{tree:#?}");
        assert!(
            tree.iter().any(|n| n.role == "Button"
                && n.label.as_deref().is_some_and(|l| l.starts_with("Every directory, "))),
            "one directory's list offers the host: {tree:#?}"
        );
        let row = cx.debug_bounds("picker-everywhere-0").expect("the everywhere row");
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let asked = drain(&mut rx);
        assert!(
            matches!(asked.as_slice(), [ClientMsg::ListAgentSessions { cwd: None }]),
            "{asked:?}"
        );
        assert!(view.read_with(cx, |c, _| c.picker.is_none()), "closed until the host answers");
        view.update_in(cx, |c, _window, cx| c.agent_sessions(None, found, cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Resume a conversation"))), "{tree:#?}");
        assert!(
            !tree
                .iter()
                .any(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Every directory"))),
            "the whole host's list offers nothing wider: {tree:#?}"
        );
        assert!(
            tree.iter().any(|n| n.role == "Button"
                && n.label.as_deref().is_some_and(
                    |l| l.starts_with("fix the build, ") && l.ends_with(" d ago · /w/slopty")
                )),
            "{tree:#?}"
        );
        assert!(
            tree.iter()
                .any(|n| n.role == "Button"
                    && n.label.as_deref().is_some_and(|l| l.starts_with("2, "))),
            "an untitled conversation is named by its id: {tree:#?}"
        );
        let row = cx.debug_bounds("picker-agent-0").expect("the first row");
        cx.simulate_click(row.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        let opened = drain(&mut rx);
        assert!(
            matches!(
                opened.as_slice(),
                [ClientMsg::OpenAgent(OpenAgent { cwd: Some(cwd), resume: Some(id), model: None, title: Some(title) })]
                    if cwd == "/w/slopty" && id == "19146b4d" && title == "fix the build"
            ),
            "{opened:?}"
        );
        assert!(view.read_with(cx, |c, _| c.picker.is_none()), "the picker closed");

        // An answer nobody asked for (another client's, or a late one) opens nothing.
        view.update_in(cx, |c, _window, cx| c.agent_sessions(None, Vec::new(), cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |c, _| c.picker.is_none()));
    }

    /// A fenced block in an agent's answer offers "run" only while the canvas has a plain
    /// shell to run it in, and a click reveals that shell and types the code into it: one
    /// paste of exactly the code, then ↩.
    #[gpui::test]
    fn a_fenced_block_runs_in_the_canvas_shell(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let agent = SessionId::new();
        host_opens_agent(&view, cx, agent, me, SHELL, 1);
        view.update_in(cx, |c, _window, cx| {
            c.transcript_update(
                TranscriptUpdate {
                    session: agent,
                    reset: true,
                    entries: vec![TranscriptEntry {
                        at: None,
                        body: TranscriptBody::Assistant {
                            markdown: "Run this:\n\n```sh\necho hi\n```".to_owned(),
                        },
                    }],
                },
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("conversation-code-copy-0-1").is_some(), "the block is drawn");
        assert!(
            cx.debug_bounds("conversation-code-run-0-1").is_none(),
            "an agent session is not a shell: there is nowhere to run it"
        );

        // A shell joins the canvas. It takes the keyboard as the item we opened, so put the
        // conversation back in front first: the click has to be the thing that reveals it.
        let shell = SessionId::new();
        host_opens(&view, cx, shell, me, Rect { x: 760.0, ..SHELL }, 2);
        view.update_in(cx, |c, _window, cx| c.reveal_session(agent, cx));
        cx.run_until_parked();
        assert!(!terminal_focused(&view, cx, shell), "the conversation holds the keyboard");
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        let tree = cx.update(|window, _| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Button", Some("Run in shell"))), "{tree:#?}");
        let run = cx.debug_bounds("conversation-code-run-0-1").expect("the block's run button");
        drain(&mut rx);

        cx.simulate_click(run.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(terminal_focused(&view, cx, shell), "the shell was revealed and took the keyboard");
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
                assert_eq!(code, "echo hi", "the fenced lines alone, no fences and no prompt");
                assert_eq!(key.code, slopty_proto::input::KeyCode::Enter, "{key:?}");
            }
            other => panic!("a paste then one ↩ into the shell: {other:?}"),
        }

        // The shell stops being one (an agent is seen in it): the button goes with it.
        view.update_in(cx, |c, _window, cx| {
            c.agent_event(
                AgentEvent {
                    session: shell,
                    kind: AgentKind::ClaudeCode,
                    status: AgentStatus::Working,
                    agent_session: None,
                    detail: None,
                    attention: false,
                    source: AgentSource::Hook,
                },
                cx,
            );
        });
        view.update_in(cx, |c, _window, cx| c.reveal_session(agent, cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("conversation-code-run-0-1").is_none(),
            "a terminal an agent is working in is not a shell to run a snippet in"
        );
    }

    /// "Ask the agent" with no agent card on the canvas opens one in the active shell's
    /// directory, and the block lands in its composer once the card has one.
    #[gpui::test]
    fn asking_the_agent_opens_a_card_when_there_is_none(cx: &mut TestAppContext) {
        let (view, mut rx, me, cx) = canvas(cx);
        let shell = SessionId::new();
        host_opens_in(&view, cx, shell, me, SHELL, 1, Where::loose("/tmp/work"));
        drain(&mut rx);
        view.update_in(cx, |c, _window, cx| c.ask_agent("```\n$ false\n```\n\n".into(), cx));
        let sent = drain(&mut rx);
        assert!(
            matches!(
                sent.as_slice(),
                [ClientMsg::OpenAgent(OpenAgent { cwd: Some(cwd), resume: None, .. })] if cwd == "/tmp/work"
            ),
            "{sent:?}"
        );
        let agent = SessionId::new();
        host_opens_agent(&view, cx, agent, me, Rect { x: 800.0, ..SHELL }, 2);
        cx.run_until_parked();
        let text = view.read_with(cx, |c, cx| {
            c.terminal(agent).and_then(|v| v.read(cx).conversation().map(|k| k.composer_text(cx)))
        });
        assert_eq!(text.as_deref(), Some("```\n$ false\n```\n\n"), "the block waited for the card");
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
}
