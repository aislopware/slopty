//! Drags from this device onto the window, the workspace's half
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! The platform asks what each point of a drag is over ([`Sink`]). Over a remote window or
//! display's body the drag is the tile's: as it enters, what it carries is read once, its files
//! start up into the drag's landing on the worker and its big data goes up beside them, and
//! the tile carries the drag on to the worker ([`crate::screen::ScreenView::drag_enter`]).
//! Anywhere else GPUI's own drop handling takes it. A drop of promised files waits for them to
//! be written here, then names them and sends them up. How the drop ended comes back from the
//! worker through the tile, and a drop that did not land says why.
//!
//! A drag out of a worker's app that comes back over a tile of that same worker carries the
//! worker's own files by their paths there, and nothing comes down or goes up for it.
//!
//! Over the grid of a terminal whose program asks for drops (Kitty drag and drop), the drag is
//! the program's (`docs/decisions/terminal.md`, "Drops are read lazily"): it too is read once
//! as it enters, and the terminal tile keeps what it carries. The program hears what the items
//! are and none of their bytes; what it accepts goes up while the drag hovers, its files into
//! the drag's landing, and anything else it reads on the drop the worker fetches from the tile.
//! A terminal whose program does not ask takes the drag as GPUI hands it, and types its paths.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyWindowHandle, AsyncApp, Context, Pixels, Point, WeakEntity, point, px};
#[cfg(target_os = "macos")]
use slopty_client::clip::Fetched;
use slopty_client::dnd::{self, Outcome};
use slopty_client::layout::TileRef;
#[cfg(target_os = "macos")]
use slopty_platform::file_drop::Taken;
use slopty_platform::file_drop::{DropSink, Dropped};
use slopty_proto::drag::{DragId, Promised};
#[cfg(target_os = "macos")]
use slopty_proto::items::ItemKind;
#[cfg(target_os = "macos")]
use slopty_proto::transfer::{RepRef, Source};

use super::{Upload, WorkspaceView};
use crate::screen::ScreenView;
#[cfg(target_os = "macos")]
use crate::terminal::{DropHook, DropNews, SinkDropped};

/// A drag from this device over the window: the remote tile it is over, if any, and a drop
/// waiting for its promised files.
#[derive(Debug, Default)]
pub struct DropIn {
    over: Option<OverTile>,
    /// The terminal whose program the drag is over.
    #[cfg(target_os = "macos")]
    term: Option<OverTerm>,
    waiting: Option<Waiting>,
}

/// The terminal a drag is over, its program asking for drops.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
struct OverTerm {
    tile: TileRef,
    session: slopty_core::SessionId,
    drag: Option<DragId>,
}

/// What a drag from this device carries, as the platform hands it over.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
pub struct Carried<'a> {
    /// The drag's pasteboard.
    pub board: &'a dyn slopty_platform::pasteboard::Pasteboard,
    /// What its source lets a target do.
    pub allowed: slopty_proto::drag::DragOps,
    /// The tag of a drag this window began (`DragsOut`), if it is one.
    pub own: Option<u64>,
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for Carried<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Carried")
            .field("allowed", &self.allowed)
            .field("own", &self.own)
            .finish_non_exhaustive()
    }
}

/// The remote tile a drag is over.
#[derive(Clone, Debug)]
struct OverTile {
    tile: TileRef,
    /// The drag the tile carries to the worker, and the items that promise files; `None` when
    /// the drag carries nothing a remote app could take.
    drag: Option<(DragId, Vec<u16>)>,
    /// A drag out of this tile's worker, back: its files are already there.
    #[cfg(target_os = "macos")]
    back: bool,
}

/// A drop on a remote tile, waiting for the files its promises write here.
#[derive(Clone, Debug)]
struct Waiting {
    tile: TileRef,
    drag: DragId,
    at: Point<Pixels>,
    items: Vec<u16>,
    /// A terminal's drop, for its program: the promised files go up with the drag's own.
    #[cfg(target_os = "macos")]
    term: Option<slopty_core::SessionId>,
}

impl DropIn {
    /// The drag the window is carrying to a worker, if it is.
    #[must_use]
    pub fn drag(&self) -> Option<DragId> {
        #[cfg(target_os = "macos")]
        if let Some(drag) = self.term.and_then(|t| t.drag) {
            return Some(drag);
        }
        self.over.as_ref().and_then(|o| o.drag.as_ref()).map(|(d, _)| *d)
    }
}

/// Window points from the platform's.
#[expect(clippy::cast_possible_truncation, reason = "points in a window")]
const fn points((x, y): (f64, f64)) -> Point<Pixels> {
    point(px(x as f32), px(y as f32))
}

impl WorkspaceView {
    /// The remote window or display whose body is under `p`, with its tile.
    #[cfg(target_os = "macos")]
    fn remote_body(&self, p: Point<Pixels>) -> Option<TileRef> {
        let (tile, body) = self.under(p)?;
        let remote = matches!(
            self.item(tile).map(|i| &i.kind),
            Some(ItemKind::Window { .. } | ItemKind::Display { .. })
        );
        (body && remote && self.screen(tile.item).is_some()).then_some(tile)
    }

    /// A drag carrying `carried` is at `p`: what it is over. Entering a remote tile begins
    /// the worker's drag and the drag's upload; leaving one ends both. A drag this window
    /// began, back over a tile of the worker it came from, carries that worker's own files,
    /// and uploads nothing.
    #[cfg(target_os = "macos")]
    pub fn drag_over(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        carried: Carried<'_>,
        cx: &mut Context<Self>,
    ) -> slopty_platform::file_drop::Over {
        use slopty_platform::file_drop::Over;
        use slopty_proto::drag::DragOp;
        let Carried { board, allowed, own } = carried;
        if let Some(over) = self.drag_over_terminal(state, p, board, cx) {
            return over;
        }
        let target = self.remote_body(p);
        if let Some(over) = &state.over
            && Some(over.tile) == target
        {
            let Some(screen) = self.screen(over.tile.item).cloned() else { return Over::Local };
            if over.drag.is_none() {
                return Over::Remote(DragOp::None);
            }
            return Over::Remote(screen.update(cx, |v, _cx| v.drag_move(p)));
        }
        if let Some(over) = state.over.take() {
            self.leave_tile(&over, cx);
        }
        let Some(tile) = target else { return Over::Local };
        let Some(screen) = self.screen(tile.item).cloned() else { return Over::Local };
        let back = own.and_then(|tag| self.drags_out.from(tag, tile.worker)).map(|s| s.back());
        let is_back = back.is_some();
        let read = back.map_or_else(
            || dnd::read(board),
            |items| dnd::Read { items, files: Vec::new(), pushes: Vec::new() },
        );
        if read.is_empty() {
            state.over = Some(OverTile { tile, drag: None, back: is_back });
            return Over::Remote(DragOp::None);
        }
        let drag = screen.update(cx, |v, cx| v.drag_enter(p, &read, allowed, cx));
        let promises = (0_u16..).zip(&read.items).filter(|(_, i)| i.promised.is_some());
        let promises = promises.map(|(n, _)| n).collect();
        tracing::info!(%drag, items = read.items.len(), files = read.files.len(), back = is_back, "drag over a remote tile");
        if !read.files.is_empty() {
            let _started = self.upload(tile, &read.files, Upload::to_drag(tile, drag), cx);
        }
        if let Some(remote) = self.remote(tile.worker) {
            for push in read.pushes {
                let rep = RepRef { source: Source::Drag(drag), item: push.item, kind: push.kind };
                remote.send_clip(rep, Fetched::Data(push.bytes), false);
            }
        }
        state.over = Some(OverTile { tile, drag: Some((drag, promises)), back: is_back });
        Over::Remote(screen.update(cx, |v, _cx| v.drag_move(p)))
    }

    /// The drag left the window from over a remote tile, or ended there with no drop.
    pub fn drag_left(&mut self, state: &mut DropIn, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        self.leave_terminal(state, cx);
        if let Some(over) = state.over.take() {
            self.leave_tile(&over, cx);
        }
    }

    /// The terminal under `p` whose program takes a drag there.
    #[cfg(target_os = "macos")]
    fn terminal_taking(&self, p: Point<Pixels>, cx: &Context<Self>) -> Option<OverTerm> {
        let (tile, true) = self.under(p)? else { return None };
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| &i.kind) else {
            return None;
        };
        let view = self.terminals.get(session)?;
        let over = OverTerm { tile, session: *session, drag: None };
        view.read(cx).takes_drag_at(p).then_some(over)
    }

    /// The drag at `p` over a terminal whose program takes it: it enters the program, read
    /// once, and moves over it; `None` when it is not over one, having left the one it was.
    #[cfg(target_os = "macos")]
    fn drag_over_terminal(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        board: &dyn slopty_platform::pasteboard::Pasteboard,
        cx: &mut Context<Self>,
    ) -> Option<slopty_platform::file_drop::Over> {
        use slopty_platform::file_drop::Over;
        let target = self.terminal_taking(p, cx);
        if let (Some(was), Some(now)) = (state.term, target)
            && was.session == now.session
        {
            let view = self.terminals.get(&now.session)?.clone();
            return Some(Over::Remote(drag_op(view.update(cx, |v, cx| v.sink_move(p, cx)))));
        }
        self.leave_terminal(state, cx);
        let mut term = target?;
        let read = dnd::read(board);
        if read.is_empty() {
            return None;
        }
        if let Some(over) = state.over.take() {
            self.leave_tile(&over, cx);
        }
        let view = self.terminals.get(&term.session)?.clone();
        let store = dnd::term::TermDrag::new(read);
        term.drag = Some(store.drag());
        tracing::info!(drag = %store.drag(), session = %term.session, "drag over a program asking for drops");
        let hook = Self::drop_hook(term.tile, cx);
        let op = view.update(cx, |v, cx| {
            v.sink_enter(store, hook, cx);
            v.sink_move(p, cx)
        });
        state.term = Some(term);
        Some(Over::Remote(drag_op(op)))
    }

    /// Where `tile`'s terminal tells what goes up for its drag: run on the workspace once the
    /// terminal's own update is done.
    #[cfg(target_os = "macos")]
    fn drop_hook(tile: TileRef, cx: &Context<Self>) -> DropHook {
        let workspace = cx.entity().downgrade();
        Rc::new(move |news, cx: &mut gpui::App| {
            let workspace = workspace.clone();
            cx.defer(move |cx| {
                let _gone = workspace.update(cx, |w, cx| w.term_drop_news(tile, news, cx));
            });
        })
    }

    /// `tile`'s terminal says what goes up for the drag over it: representations its program
    /// accepted, inline or as streams ahead of other transfers; its files, into the drag's
    /// landing; and once the program is done with the drop, nothing more.
    #[cfg(target_os = "macos")]
    fn term_drop_news(&mut self, tile: TileRef, news: DropNews, cx: &mut Context<Self>) {
        match news {
            DropNews::Push(pushes) => {
                let Some(remote) = self.remote(tile.worker) else { return };
                for (rep, bytes) in pushes {
                    remote.send_clip(rep, Fetched::Data(bytes), true);
                }
            }
            DropNews::Upload(drag, files) => self.upload_term_drag(tile, drag, &files, None, cx),
            DropNews::Ended(drag, operation) => {
                tracing::info!(%drag, ?operation, "terminal drop ended");
                self.stop_drag_uploads(drag, cx);
            }
        }
    }

    /// Send `files` of `drag` into its landing on `tile`'s worker, for the program of its
    /// terminal, which is told they will not come if they cannot go.
    #[cfg(target_os = "macos")]
    fn upload_term_drag(
        &mut self,
        tile: TileRef,
        drag: DragId,
        files: &[std::path::PathBuf],
        scratch: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            Self::discard_landing(scratch, cx);
            return;
        };
        let upload = Upload { drag: Some(drag), scratch, ..Upload::to_shell(tile, session) };
        if (files.is_empty() || !self.upload(tile, files, upload, cx))
            && let Some(view) = self.terminals.get(&session).cloned()
        {
            view.update(cx, |v, cx| v.files_failed(Some(drag), cx));
        }
    }

    /// The drag left the terminal it was over: its program hears it, and what goes up for it
    /// stops.
    #[cfg(target_os = "macos")]
    fn leave_terminal(&mut self, state: &mut DropIn, cx: &mut Context<Self>) {
        let Some(was) = state.term.take() else { return };
        let Some(view) = self.terminals.get(&was.session).cloned() else { return };
        if let Some(drag) = view.update(cx, |v, cx| v.sink_leave(cx)) {
            self.stop_drag_uploads(drag, cx);
        }
    }

    /// The drag left `over`: the worker's drag ends, and its upload stops.
    fn leave_tile(&mut self, over: &OverTile, cx: &mut Context<Self>) {
        if let Some(screen) = self.screen(over.tile.item).cloned() {
            screen.update(cx, ScreenView::drag_leave);
        }
        if let Some((drag, _)) = over.drag {
            self.stop_drag_uploads(drag, cx);
        }
    }

    /// Stop what goes up into `drag`'s landing.
    fn stop_drag_uploads(&mut self, drag: DragId, cx: &mut Context<Self>) {
        let xfers: Vec<_> =
            self.uploads.iter().filter(|(_, u)| u.drag == Some(drag)).map(|(x, _)| *x).collect();
        for xfer in xfers {
            self.cancel_upload(xfer, cx);
        }
    }

    /// Dropped at `p` over a remote tile, promising `promised` files: what becomes of it. A
    /// drop the worker said nothing takes is refused, and slides back. One that waits for
    /// promised files has them called in, and shows its ring meanwhile. A drag back onto its
    /// own worker is taken as it is.
    #[cfg(target_os = "macos")]
    pub fn drag_dropped(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        promised: usize,
        cx: &mut Context<Self>,
    ) -> Taken {
        if let Some(term) = state.term.take() {
            return self.drop_on_terminal(state, term, p, cx);
        }
        let Some(over) = state.over.take() else { return Taken::Refused };
        let (Some((drag, items)), Some(screen)) =
            (over.drag.clone(), self.screen(over.tile.item).cloned())
        else {
            self.leave_tile(&over, cx);
            return Taken::Refused;
        };
        if !screen.read(cx).drag_takes() {
            self.leave_tile(&over, cx);
            return Taken::Refused;
        }
        let sent = |taken| if taken { Taken::CallIn } else { Taken::Refused };
        if over.back {
            let taken = screen.update(cx, |v, cx| v.drag_drop(p, Vec::new(), cx));
            return if taken { Taken::AsIs } else { Taken::Refused };
        }
        if promised == 0 {
            return sent(screen.update(cx, |v, cx| v.drag_drop(p, Vec::new(), cx)));
        }
        screen.update(cx, |v, cx| v.drag_hold(p, cx));
        state.waiting = Some(Waiting { tile: over.tile, drag, at: p, items, term: None });
        Taken::CallIn
    }

    /// Dropped at `p` on `term`, whose program the drag was over: the program has it unless
    /// it refused it. Promised files it wants are called in, to go up with the drag's own.
    #[cfg(target_os = "macos")]
    fn drop_on_terminal(
        &self,
        state: &mut DropIn,
        term: OverTerm,
        p: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> Taken {
        let Some(view) = self.terminals.get(&term.session).cloned() else { return Taken::Refused };
        match view.update(cx, |v, cx| v.sink_drop(p, cx)) {
            SinkDropped::Refused => Taken::Refused,
            SinkDropped::Taken { drag, call_in: false } => {
                tracing::info!(%drag, session = %term.session, "dropped on a program");
                Taken::AsIs
            }
            SinkDropped::Taken { drag, call_in: true } => {
                let session = Some(term.session);
                let waiting =
                    Waiting { tile: term.tile, drag, at: p, items: Vec::new(), term: session };
                state.waiting = Some(waiting);
                Taken::CallIn
            }
        }
    }

    /// The files a remote drop's promises wrote are here, or failed: they are named in the
    /// drop, which goes to the worker, and go up into its landing.
    fn drag_arrived(&mut self, waiting: Waiting, dropped: &Dropped, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(session) = waiting.term {
            if !dropped.failed.is_empty() {
                let failed = format!("Not sent: {}", dropped.failed.join("; "));
                self.show_failure_at(waiting.tile, failed, cx);
            }
            let mut files = self
                .terminals
                .get(&session)
                .map(|v| v.read(cx).sink_files(waiting.drag))
                .unwrap_or_default();
            files.extend(dropped.paths.iter().cloned());
            let scratch = dropped.landing.clone();
            self.upload_term_drag(waiting.tile, waiting.drag, &files, scratch, cx);
            return;
        }
        let (tile, drag, at, items) = (waiting.tile, waiting.drag, waiting.at, waiting.items);
        let promised = items
            .iter()
            .enumerate()
            .map(|(n, &item)| Promised {
                item,
                file: dropped.paths.get(n).and_then(|p| dnd::file_meta(p)),
            })
            .collect();
        if !dropped.failed.is_empty() {
            self.show_failure_at(tile, format!("Not sent: {}", dropped.failed.join("; ")), cx);
        }
        let Some(screen) = self.screen(tile.item).cloned() else {
            Self::discard_landing(dropped.landing.clone(), cx);
            return;
        };
        let sent = screen.update(cx, |v, cx| v.drag_drop(at, promised, cx));
        if !sent || dropped.paths.is_empty() {
            Self::discard_landing(dropped.landing.clone(), cx);
            return;
        }
        let upload = Upload { scratch: dropped.landing.clone(), ..Upload::to_drag(tile, drag) };
        let _started = self.upload(tile, &dropped.paths, upload, cx);
    }

    /// The worker said how the drop of `drag` on `tile` ended: a drop that did not land says
    /// why, and what still goes up for it stops.
    pub(in crate::workspace) fn drag_ended(
        &mut self,
        tile: TileRef,
        drag: DragId,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(%drag, ?outcome, item = %tile.item, "drop ended");
        match outcome {
            Outcome::Landed(_) => {}
            Outcome::Refused => {
                self.stop_drag_uploads(drag, cx);
                self.show_notice_at(tile, "Nothing there took the drop".to_owned(), cx);
            }
            Outcome::Failed(why) => {
                self.stop_drag_uploads(drag, cx);
                self.show_failure_at(tile, format!("The drop did not land: {why}"), cx);
            }
        }
    }

    /// Files a drop called in for the tile under it, handed to it as a file drop.
    fn local_arrived(
        view: &WeakEntity<Self>,
        dropped: &Dropped,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        let position = points((dropped.x, dropped.y));
        // The tile the drop lands on takes the landing with the paths.
        let _gone = view.update(cx, |v, _cx| v.drop_landing.clone_from(&dropped.landing));
        if !dropped.paths.is_empty() {
            let paths = gpui::ExternalPaths(dropped.paths.iter().cloned().collect());
            for event in [
                gpui::FileDropEvent::Entered { position, paths },
                gpui::FileDropEvent::Pending { position },
                gpui::FileDropEvent::Submit { position },
            ] {
                let _handled = window.dispatch_event(gpui::PlatformInput::FileDrop(event), cx);
            }
        }
        // No tile took it (a drop on the bars, or between tiles): nothing uploads it.
        let _gone = view.update(cx, |v, cx| Self::discard_landing(v.drop_landing.take(), cx));
        if !dropped.failed.is_empty() {
            let text = format!("Not sent: {}", dropped.failed.join("; "));
            let _gone = view.update(cx, |v, cx| v.show_notice(text, cx));
        }
    }
}

/// The window's drags and drops, as the platform hands them over (`file_drop`).
pub(super) struct Sink {
    handle: AnyWindowHandle,
    view: WeakEntity<WorkspaceView>,
    app: AsyncApp,
    state: Rc<RefCell<DropIn>>,
}

impl Sink {
    /// The drags over `handle`, the window `view` is the root of.
    pub(super) fn new(
        handle: AnyWindowHandle,
        view: WeakEntity<WorkspaceView>,
        app: AsyncApp,
    ) -> Self {
        Self { handle, view, app, state: Rc::default() }
    }

    /// Run `f` on the workspace in its window; `None` when either is gone, or busy: a drag
    /// that began in this window and runs its own loop inside an update, which then stays
    /// GPUI's.
    fn with<R>(
        &self,
        f: impl FnOnce(&mut WorkspaceView, &mut DropIn, &mut Context<WorkspaceView>) -> R,
    ) -> Option<R> {
        let mut app = self.app.clone();
        let mut state = self.state.try_borrow_mut().ok()?;
        self.handle
            .update(&mut app, |_root, _window, cx| {
                self.view.update(cx, |v, cx| f(v, &mut state, cx))
            })
            .ok()?
            .ok()
    }
}

impl DropSink for Sink {
    #[cfg(target_os = "macos")]
    fn over(
        &self,
        at: (f64, f64),
        board: &dyn slopty_platform::pasteboard::Pasteboard,
        allowed: slopty_proto::drag::DragOps,
        own: Option<u64>,
    ) -> slopty_platform::file_drop::Over {
        let carried = Carried { board, allowed, own };
        self.with(|v, state, cx| v.drag_over(state, points(at), carried, cx))
            .unwrap_or(slopty_platform::file_drop::Over::Local)
    }

    #[cfg(target_os = "macos")]
    fn left(&self) {
        let _gone = self.with(WorkspaceView::drag_left);
    }

    #[cfg(target_os = "macos")]
    fn dropped(&self, at: (f64, f64), promised: usize) -> Taken {
        self.with(|v, state, cx| v.drag_dropped(state, points(at), promised, cx))
            .unwrap_or(Taken::Refused)
    }

    fn arrived(&self, dropped: Dropped) {
        if dropped.remote {
            let delivered = self.with(|v, state, cx| match state.waiting.take() {
                Some(waiting) => v.drag_arrived(waiting, &dropped, cx),
                None => WorkspaceView::discard_landing(dropped.landing.clone(), cx),
            });
            if delivered.is_none()
                && let Some(landing) = &dropped.landing
            {
                slopty_platform::file_drop::discard(landing);
            }
            return;
        }
        let mut app = self.app.clone();
        let view = self.view.clone();
        let delivered = self.handle.update(&mut app, |_root, window, cx| {
            WorkspaceView::local_arrived(&view, &dropped, window, cx);
        });
        if let Err(e) = delivered {
            tracing::warn!(error = %e, "a drop for a window that is gone");
            if let Some(landing) = &dropped.landing {
                slopty_platform::file_drop::discard(landing);
            }
        }
    }
}

/// What a drop on a terminal does, as the drag shows it.
#[cfg(target_os = "macos")]
const fn drag_op(operation: slopty_proto::terminal::DropOperation) -> slopty_proto::drag::DragOp {
    use slopty_proto::drag::DragOp;
    use slopty_proto::terminal::DropOperation;
    match operation {
        DropOperation::None => DragOp::None,
        DropOperation::Copy => DragOp::Copy,
        DropOperation::Move => DragOp::Move,
    }
}
