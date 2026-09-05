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

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, PinchEvent, Pixels, Point, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, Size, Styled as _, Window, canvas, div, point, px, size,
};
use slopty_client::canvas::{CARD_ZOOM, Camera, CanvasDoc, GAP, TERMINAL_SIZE, snap};
use slopty_core::{ClientId, ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentEvent, AgentStatus, BlockReason};
use slopty_proto::canvas::{CanvasItem, CanvasOp, CanvasSync, ItemKind, Rect};
use slopty_proto::screen::{
    CaptureTarget, DisplayInfo, Quality, ScreenEvent, ScreenRequest, WindowInfo,
};
use slopty_proto::terminal::{OpenSession, SessionSummary, TermEvent, TermRequest, TermSize};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::colors::{hsla, hsla_alpha};
use crate::note::{NoteView, NoteViewEvent};
use crate::picker::{PickerEvent, WindowPicker};
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
        ]
    );
}
pub use actions::{AddWindow, CloseItem, FitAll, NewNote, NewTerminal, ZoomIn, ZoomOut, ZoomReset};

/// Key bindings for the canvas context.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    const CTX: Option<&str> = Some("Canvas");
    vec![
        KeyBinding::new("cmd-t", NewTerminal, CTX),
        KeyBinding::new("cmd-n", NewTerminal, CTX),
        KeyBinding::new("cmd-shift-n", NewNote, CTX),
        KeyBinding::new("cmd-o", AddWindow, CTX),
        KeyBinding::new("cmd-w", CloseItem, CTX),
        KeyBinding::new("cmd-=", ZoomIn, CTX),
        KeyBinding::new("cmd-shift-=", ZoomIn, CTX),
        KeyBinding::new("cmd--", ZoomOut, CTX),
        KeyBinding::new("cmd-0", ZoomReset, CTX),
        KeyBinding::new("cmd-1", FitAll, CTX),
    ]
}

/// A stable element id for `(part, item)`.
fn element_id(part: &str, id: ItemId) -> ElementId {
    ElementId::from(format!("{part}-{}", id.as_uuid()))
}

/// Title bar height at zoom 1, in points.
const TITLE_H: f32 = 28.0;
/// Resize grip size at zoom 1.
const GRIP: f32 = 14.0;
/// Smallest item on screen while dragging.
const MIN_ITEM: f32 = 160.0;
/// Size of a new note.
const NOTE_SIZE: (f32, f32) = (320.0, 240.0);
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
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Move { id: ItemId, grab: Point<Pixels>, start: Rect },
    Resize { id: ItemId, grab: Point<Pixels>, start: Rect },
    Pan { last: Point<Pixels> },
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
    /// Viewport origin (window coordinates) and size, recorded each frame.
    viewport: (Point<Pixels>, Size<Pixels>),
    /// Fit every item into the viewport on the next frame (once the viewport is known).
    fit_pending: bool,
    /// Bring this item into view on the next frame (one we just created).
    reveal_pending: Option<ItemId>,
    drag: Option<Drag>,
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
            me,
            out,
            theme,
            terminals: HashMap::new(),
            sessions: sessions.into_iter().map(|s| (s.id, s)).collect(),
            agents: HashMap::new(),
            screens: HashMap::new(),
            notes: HashMap::new(),
            pending_opens: HashMap::new(),
            titles_requested: false,
            titles: HashMap::new(),
            open_screen,
            picker: None,
            picker_wanted: false,
            viewport: (point(px(0.0), px(0.0)), size(px(1.0), px(1.0))),
            fit_pending: false,
            reveal_pending: None,
            drag: None,
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
        self.active = Some(id);
        self.pending_focus = Some(session);
        self.reveal_pending = Some(id);
        cx.notify();
    }

    /// A session appeared (ours or another client's).
    pub fn session_opened(&mut self, summary: SessionSummary, cx: &mut Context<Self>) {
        self.sessions.insert(summary.id, summary);
        self.reconcile(cx);
        cx.notify();
    }

    /// A session is gone.
    pub fn session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.sessions.remove(&session);
        self.agents.remove(&session);
        self.reconcile(cx);
        cx.notify();
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
        if event.status == AgentStatus::None {
            self.agents.remove(&session);
        } else {
            let attention = event.attention;
            self.agents.insert(session, event);
            if attention {
                cx.emit(CanvasEvent::Attention(session));
            }
        }
        cx.notify();
    }

    /// Link RTT (fanned out to every terminal's predictor).
    pub fn set_rtt(&self, rtt: Option<std::time::Duration>, cx: &mut Context<Self>) {
        for view in self.terminals.values() {
            view.update(cx, |v, _| v.set_rtt(rtt));
        }
    }

    /// A remote-window event from the host.
    pub fn screen_event(&mut self, event: ScreenEvent, cx: &mut Context<Self>) {
        match event {
            ScreenEvent::Listing { windows, displays } => {
                self.fill_titles(&windows);
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
                self.subscriptions
                    .push(cx.subscribe(&view, |_this, _view, _event, cx| cx.notify()));
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

    fn show_picker(
        &mut self,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let picker = cx.new(|cx| WindowPicker::new(windows, displays, theme, cx));
        self.subscriptions.push(cx.subscribe(&picker, |this, _picker, event, cx| {
            match event {
                PickerEvent::Pick { target, size, title } => {
                    this.add_screen_item(*target, *size, title.clone());
                }
                PickerEvent::Dismiss => {}
            }
            this.picker = None;
            this.pending_focus_self = true;
            cx.notify();
        }));
        self.pending_focus_picker = true;
        self.picker = Some(picker);
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
                },
            ));
            self.send(ClientMsg::Term { session: *session, req: TermRequest::Attach { size } });
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
            self.active = self.doc.by_z().last().map(|i| i.id);
        }
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

    /// Open a new shell; the host places it.
    pub fn new_terminal(&mut self, _: &NewTerminal, _window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!("open session");
        self.send(ClientMsg::OpenSession(OpenSession {
            size: TermSize::default(),
            cwd: None,
            command: Vec::new(),
            env: Vec::new(),
            title: None,
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
            let view = cx.new(|cx| NoteView::new(*id, text, window, cx));
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

    /// ⌘O: ask the host for its windows, then show the picker.
    pub fn add_window(&mut self, _: &AddWindow, _window: &mut Window, cx: &mut Context<Self>) {
        self.picker_wanted = true;
        self.send(ClientMsg::Screen(ScreenRequest::List));
        cx.notify();
    }

    /// Close the active item.
    pub fn close_item(&mut self, _: &CloseItem, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active else { return };
        let Some(item) = self.doc.get(id).cloned() else { return };
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

    /// ⌘0
    pub fn zoom_reset(&mut self, _: &ZoomReset, _window: &mut Window, cx: &mut Context<Self>) {
        let (_, vp) = self.viewport;
        let factor = 1.0 / self.camera.zoom;
        self.camera.zoom_at(factor, f32::from(vp.width) / 2.0, f32::from(vp.height) / 2.0);
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    /// ⌘1
    pub fn fit_all(&mut self, _: &FitAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.fit_now(cx);
    }

    /// Fit every item on the next frame, when the viewport size is known. Used right after
    /// the first canvas snapshot so a phone does not open onto empty space beside a layout
    /// made on a desktop.
    pub const fn fit_when_painted(&mut self) {
        self.fit_pending = true;
    }

    fn fit_now(&mut self, cx: &mut Context<Self>) {
        let (_, vp) = self.viewport;
        let rects: Vec<Rect> = self.doc.items().map(|i| i.rect).collect();
        self.camera.fit(rects, (f32::from(vp.width), f32::from(vp.height)));
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    // ----- pointer -------------------------------------------------------------------------

    fn local(&self, p: Point<Pixels>) -> Point<Pixels> {
        p - self.viewport.0
    }

    fn scroll_wheel(&mut self, ev: &ScrollWheelEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let delta = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * 20.0, l.y * 20.0),
        };
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
        let local = self.local(ev.position);
        let factor = (1.0 + ev.delta).max(0.05);
        self.camera.zoom_at(factor, f32::from(local.x), f32::from(local.y));
        cx.emit(CanvasEvent::Zoom(self.camera.zoom));
        cx.notify();
    }

    fn begin_pan(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        self.drag = Some(Drag::Pan { last: ev.position });
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

    fn click_item(&mut self, id: ItemId, cx: &mut Context<Self>) {
        // Items own their clicks: the root must not start a pan or steal focus.
        cx.stop_propagation();
        self.activate(id, cx);
    }

    fn activate(&mut self, id: ItemId, cx: &mut Context<Self>) {
        self.active = Some(id);
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
                let d = ev.position - last;
                self.camera.pan(f32::from(d.x), f32::from(d.y));
                self.drag = Some(Drag::Pan { last: ev.position });
            }
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
            Drag::Pan { .. } => {}
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
        let title_h = if card { TITLE_H } else { TITLE_H * zoom };
        let ui_size = if card { 12.0 } else { 12.0 * zoom };

        let (title, focused) = match item.kind {
            ItemKind::Terminal { session } => {
                let view = self.terminals.get(&session);
                let focused = view.is_some_and(|v| v.read(cx).focus_handle(cx).is_focused(window));
                let title = view
                    .and_then(|v| v.read(cx).state().title().map(str::to_owned))
                    .or_else(|| self.sessions.get(&session).map(|s| s.title.clone()))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "shell".to_owned());
                (title, focused)
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
            ItemKind::Note { .. } => {
                let focused =
                    self.notes.get(&item.id).is_some_and(|v| v.read(cx).editing(window, cx));
                ("note".to_owned(), focused)
            }
        };
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agents.get(&session),
            _ => None,
        };
        let badge = agent.map(|a| agent_badge(a, theme, ui_size));
        // Another client's size rules this PTY: offer to take it (on the active item only, so
        // a wall of cards stays readable).
        let take = match item.kind {
            ItemKind::Terminal { session } if active => self
                .terminals
                .get(&session)
                .filter(|v| !v.read(cx).driving())
                .map(|_| take_button(id, theme, ui_size, cx)),
            _ => None,
        };
        let needs_human = agent.is_some_and(
            |a| matches!(&a.status, AgentStatus::Blocked(why) if *why != BlockReason::IdlePrompt),
        );
        let border = if needs_human {
            theme.terminal.palette(3)
        } else if focused || active {
            theme.surfaces.accent
        } else {
            theme.surfaces.border
        };

        let title_bar = div()
            .id(element_id("title", id))
            .h(px(title_h))
            .w_full()
            .flex()
            .items_center()
            .px(px(10.0 * if card { 1.0 } else { zoom }))
            .gap(px(6.0))
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
            .child(div().size(px(7.0 * if card { 1.0 } else { zoom })).rounded_full().bg(
                hsla_alpha(
                    if focused { theme.surfaces.accent } else { theme.surfaces.text_muted },
                    0.9,
                ),
            ))
            // "take" sits left of the title so it stays reachable on a phone when the item is
            // wider than the screen.
            .when_some(take, gpui::ParentElement::child)
            .child(
                div().flex_1().overflow_hidden().text_ellipsis().child(SharedString::from(title)),
            )
            .when_some(badge, gpui::ParentElement::child);

        let body: gpui::AnyElement = match &item.kind {
            ItemKind::Terminal { session } => match (card, self.terminals.get(session)) {
                (false, Some(view)) => {
                    view.update(cx, |v, _| v.set_zoom(zoom));
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
                        .p(px(10.0))
                        .text_size(px(11.0))
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
                    view.update(cx, |v, _| v.set_zoom(zoom));
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
                    .p(px(10.0))
                    .overflow_hidden()
                    .text_size(px(11.0))
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
            .size(px(GRIP * if card { 1.0 } else { zoom }))
            .cursor_nwse_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev, _w, cx| this.begin_resize(id, ev, cx)),
            );

        div()
            .id(element_id("item", id))
            .absolute()
            .left(px(s.x))
            .top(px(s.y))
            .w(px(s.w))
            .h(px(s.h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(theme.radius * if card { 1.0 } else { zoom }))
            .border_1()
            .border_color(hsla(border))
            .bg(hsla(theme.terminal.bg))
            .shadow_md()
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
        self.reconcile_notes(window, cx);
        if let Some(id) = self.pending_focus_note.take()
            && let Some(view) = self.notes.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        let picker = self.picker.clone();
        let items = self.doc.by_z().into_iter().cloned().collect::<Vec<_>>();
        let entity = cx.entity();
        let record_bounds = canvas(
            move |bounds, _window, cx| {
                entity.update(cx, |this, cx| {
                    this.viewport = (bounds.origin, bounds.size);
                    if std::mem::take(&mut this.fit_pending) && !this.is_empty() {
                        this.fit_now(cx);
                    }
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
        let rendered: Vec<gpui::AnyElement> =
            items.iter().map(|item| self.render_item(item, window, cx)).collect();
        div()
            .id("canvas")
            .key_context("Canvas")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hsla(self.theme.surfaces.canvas))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::add_window))
            .on_action(cx.listener(Self::close_item))
            .on_action(cx.listener(Self::zoom_in))
            .on_action(cx.listener(Self::zoom_out))
            .on_action(cx.listener(Self::zoom_reset))
            .on_action(cx.listener(Self::fit_all))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .capture_pinch(cx.listener(Self::pinch))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::begin_pan))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .child(record_bounds)
            .children(rendered)
            .children(picker)
            .when(empty, |el| {
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(13.0))
                        .text_color(hsla(self.theme.surfaces.text_muted))
                        .font_family(self.theme.typography.ui_family.clone())
                        .child("⌘T opens a shell on the host · ⌘O adds a window"),
                )
            })
    }
}

/// The agent pill in a terminal's title bar: a coloured dot and a short word or the detail.
/// What a zoomed-out note card shows: its first non-empty line, clipped.
fn note_summary(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("empty note");
    line.chars().take(48).collect()
}

/// The "take" pill in a terminal's title bar (see [`CanvasView::take_over`]).
fn take_button(
    id: ItemId,
    theme: &Theme,
    ui_size: f32,
    cx: &Context<CanvasView>,
) -> gpui::AnyElement {
    div()
        .id(element_id("take", id))
        .flex_none()
        .px(px(ui_size * 0.5))
        .py(px(ui_size * 0.1))
        .rounded(px(ui_size * 0.35))
        .bg(hsla_alpha(theme.surfaces.accent, 0.25))
        .text_color(hsla(theme.surfaces.text))
        .cursor_pointer()
        .child("take")
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _ev, _w, cx| {
                cx.stop_propagation();
                this.take_over(id, cx);
            }),
        )
        .into_any_element()
}

fn agent_badge(agent: &AgentEvent, theme: &Theme, ui_size: f32) -> gpui::AnyElement {
    let (label, color) = match &agent.status {
        AgentStatus::None => return div().into_any_element(),
        AgentStatus::Idle => ("claude".to_owned(), theme.surfaces.text_muted),
        AgentStatus::Working => ("working".to_owned(), theme.surfaces.accent),
        AgentStatus::Tool { tool } => {
            (agent.detail.clone().unwrap_or_else(|| tool.clone()), theme.terminal.palette(6))
        }
        AgentStatus::Blocked(BlockReason::Permission { tool }) => {
            let what = agent.detail.clone().unwrap_or_else(|| tool.clone());
            (format!("allow? {what}"), theme.terminal.palette(3))
        }
        AgentStatus::Blocked(BlockReason::Question) => {
            ("asking you".to_owned(), theme.terminal.palette(3))
        }
        AgentStatus::Blocked(BlockReason::Elicitation) => {
            ("needs input".to_owned(), theme.terminal.palette(3))
        }
        AgentStatus::Blocked(BlockReason::IdlePrompt) => {
            ("idle".to_owned(), theme.surfaces.text_muted)
        }
        AgentStatus::Done => ("done".to_owned(), theme.terminal.palette(2)),
    };
    div()
        .flex()
        .items_center()
        .flex_none()
        .max_w(px(ui_size * 22.0))
        .overflow_hidden()
        .gap(px(ui_size * 0.4))
        .px(px(ui_size * 0.5))
        .py(px(ui_size * 0.1))
        .rounded(px(ui_size * 0.5))
        .bg(hsla_alpha(color, 0.12))
        .text_size(px(ui_size * 0.9))
        .text_color(hsla(color))
        .child(div().flex_none().size(px(ui_size * 0.5)).rounded_full().bg(hsla(color)))
        .child(div().overflow_hidden().text_ellipsis().child(SharedString::from(label)))
        .into_any_element()
}
