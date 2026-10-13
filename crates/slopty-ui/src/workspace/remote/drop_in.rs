//! Drags from this Mac onto the window, the workspace's half (`docs/decisions/audio.md`, "Drag
//! and drop lands at the point, both ways").
//!
//! The platform asks what each point of a drag is over ([`Sink`]), and the window answers from
//! what it last drew: the views the shell hosts each know their place there
//! ([`slopty_kit::spot`]). Over a remote window or display's body the drag is the stream's: as
//! it enters, what it carries is read once, its big data goes up, and the view carries the drag
//! on to the worker ([`crate::screen::ScreenView::drag_enter`]). Its files go up into the drag's
//! landing on the worker only once it is dropped, which waits for them there: a big folder
//! passed over a window on its way elsewhere sends nothing. A drop of promised files waits for
//! them to be written here, then names them and sends them up. How the drop ended comes back
//! from the worker through the view, and a drop that did not land says why.
//!
//! Over the grid of a shell whose program asks for drops (Kitty drag and drop), the drag is the
//! program's (`docs/decisions/terminal.md`, "Drops are read lazily"): it too is read once as it
//! enters, and the terminal keeps what it carries; what the program accepts goes up while the
//! drag hovers. Over any other shell's grid a drop sends its files to the shell's directory and
//! types their paths there. Anywhere else the shell's own drop handling takes the drag, promised
//! files handed to it once they are written.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{AnyWindowHandle, AsyncApp, Context, Pixels, Point, WeakEntity, Window, point, px};
use slopty_client::ItemRef;
use slopty_client::clip::Fetched;
use slopty_client::dnd::{self, Outcome};
use slopty_core::SessionId;
use slopty_platform::file_drop::{DropSink, Dropped, Over, Taken};
use slopty_proto::drag::{DragId, DragOp, Promised};
use slopty_proto::transfer::{RepRef, Source};
use slopty_terminal::{DropHook, DropNews, SinkDropped};

use super::{Upload, WorkspaceView};
use crate::screen::ScreenView;

/// A drag from this Mac over the window: what it is over, and a drop waiting for its promised
/// files.
#[derive(Debug, Default)]
pub struct DropIn {
    over: Option<OverScreen>,
    /// The shell whose program the drag is over.
    term: Option<OverTerm>,
    /// The shell whose directory a drop would send its files to.
    plain: Option<OverPlain>,
    waiting: Option<Waiting>,
}

/// A shell a drag is over, its program asking for drops.
#[derive(Clone, Copy, Debug)]
struct OverTerm {
    tile: ItemRef,
    session: SessionId,
    drag: Option<DragId>,
}

/// A shell a drag is over whose program does not ask for drops, with the files the drag names.
#[derive(Clone, Debug)]
struct OverPlain {
    tile: ItemRef,
    session: SessionId,
    files: Vec<PathBuf>,
}

/// What a drag from this Mac carries, as the platform hands it over.
#[derive(Clone, Copy)]
pub struct Carried<'a> {
    /// The drag's pasteboard.
    pub board: &'a dyn slopty_platform::pasteboard::Pasteboard,
    /// What its source lets a target do.
    pub allowed: slopty_proto::drag::DragOps,
}

impl std::fmt::Debug for Carried<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Carried").field("allowed", &self.allowed).finish_non_exhaustive()
    }
}

/// The remote window or display a drag is over.
#[derive(Clone, Debug)]
struct OverScreen {
    tile: ItemRef,
    /// The drag the view carries to the worker, and the items that promise files; `None` when
    /// the drag carries nothing a remote app could take.
    drag: Option<(DragId, Vec<u16>)>,
    /// This Mac's files the drag carries, which go up into its landing once it is dropped.
    files: Vec<PathBuf>,
}

/// A drop waiting for the files its promises write here.
#[derive(Clone, Debug)]
enum Waiting {
    /// On a remote window or display, at a point.
    Screen { tile: ItemRef, drag: DragId, at: Point<Pixels>, items: Vec<u16> },
    /// On a shell's program: the promised files go up with the drag's own.
    Term { tile: ItemRef, session: SessionId, drag: DragId },
    /// On a shell: the files go to its directory with the drag's own.
    Plain { tile: ItemRef, session: SessionId, files: Vec<PathBuf> },
}

impl DropIn {
    /// The drag the window is carrying to a worker, if it is.
    #[must_use]
    pub fn drag(&self) -> Option<DragId> {
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
    /// Take every drag over `window`, the shell's: over a remote window or display, the worker
    /// drops it at the point there; over a shell, the shell takes it; elsewhere the shell's own
    /// drop handling. Files apps promise rather than name (Mail, Photos) land in a temporary
    /// directory first. Once, with the window.
    pub fn accept_dropped_files(window: &Window, cx: &Context<Self>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let host = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
            Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
            _ => return,
        };
        let sink = Sink::new(window.window_handle(), cx.entity().downgrade(), cx.to_async());
        slopty_platform::file_drop::install(host, Rc::new(sink));
    }

    /// The remote window or display whose body the window last drew on top at `p`.
    fn screen_at(&self, p: Point<Pixels>, window: &Window, cx: &Context<Self>) -> Option<ItemRef> {
        let (id, _) = self.screens.iter().find(|(_, view)| view.read(cx).shows_at(p, window))?;
        self.tile_of(*id)
    }

    /// The shell whose grid the window last drew on top at `p`.
    fn shell_at(
        &self,
        p: Point<Pixels>,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<(ItemRef, SessionId)> {
        let (session, _) =
            self.terminals.iter().find(|(_, view)| view.read(cx).shows_at(p, window))?;
        Some((self.tile_of_session(*session)?, *session))
    }

    /// A drag carrying `carried` is at `p`: what it is over. Entering a remote window begins
    /// the worker's drag, and leaving one ends it; its files go up only on the drop.
    pub fn drag_over(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        carried: Carried<'_>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Over {
        let Carried { board, allowed } = carried;
        if let Some(over) = self.drag_over_shell(state, p, board, window, cx) {
            return over;
        }
        let target = self.screen_at(p, window, cx);
        if let Some(over) = &state.over
            && Some(over.tile) == target
        {
            let Some(screen) = self.screens.get(&over.tile.item).cloned() else {
                return Over::Local;
            };
            if over.drag.is_none() {
                return Over::Remote(DragOp::None);
            }
            return Over::Remote(screen.update(cx, |v, _cx| v.drag_move(p)));
        }
        if let Some(over) = state.over.take() {
            self.leave_screen(&over, cx);
        }
        let Some(tile) = target else { return Over::Local };
        let Some(screen) = self.screens.get(&tile.item).cloned() else { return Over::Local };
        let read = dnd::read(board);
        if read.is_empty() {
            state.over = Some(OverScreen { tile, drag: None, files: Vec::new() });
            return Over::Remote(DragOp::None);
        }
        let drag = screen.update(cx, |v, cx| v.drag_enter(p, &read, allowed, cx));
        let promises = (0_u16..).zip(&read.items).filter(|(_, i)| i.promised.is_some());
        let promises = promises.map(|(n, _)| n).collect();
        tracing::info!(%drag, items = read.items.len(), files = read.files.len(), "drag over a remote window");
        if let Some(remote) = self.remote(tile.worker) {
            for push in read.pushes {
                let rep = RepRef { source: Source::Drag(drag), item: push.item, kind: push.kind };
                remote.send_clip(rep, Fetched::Data(push.bytes), false);
            }
        }
        state.over = Some(OverScreen { tile, drag: Some((drag, promises)), files: read.files });
        Over::Remote(screen.update(cx, |v, _cx| v.drag_move(p)))
    }

    /// The drag left the window from over a remote window or a shell, or ended there with no
    /// drop.
    pub fn drag_left(&mut self, state: &mut DropIn, cx: &mut Context<Self>) {
        self.leave_terminal(state, cx);
        state.plain = None;
        if let Some(over) = state.over.take() {
            self.leave_screen(&over, cx);
        }
    }

    /// The drag at `p` over a shell's grid: a program asking for drops has it enter, read
    /// once, and move over it; any other shell takes its files on the drop. `None` when it is
    /// not over a shell, having left the one it was.
    fn drag_over_shell(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        board: &dyn slopty_platform::pasteboard::Pasteboard,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Over> {
        let target = self.shell_at(p, window, cx);
        let taking = target.filter(|(_, session)| {
            self.terminals.get(session).is_some_and(|v| v.read(cx).takes_drag_at(p))
        });
        if let (Some(was), Some((_, now))) = (state.term, taking)
            && was.session == now
        {
            let view = self.terminals.get(&now)?.clone();
            return Some(Over::Remote(drag_op(view.update(cx, |v, cx| v.sink_move(p, cx)))));
        }
        self.leave_terminal(state, cx);
        let Some((tile, session)) = target else {
            state.plain = None;
            return None;
        };
        if let Some(over) = state.over.take() {
            self.leave_screen(&over, cx);
        }
        let read = dnd::read(board);
        if taking.is_none() {
            if state.plain.as_ref().is_none_or(|was| was.session != session) {
                state.plain = Some(OverPlain { tile, session, files: read.files });
            }
            return Some(Over::Remote(DragOp::Copy));
        }
        state.plain = None;
        if read.is_empty() {
            return Some(Over::Remote(DragOp::None));
        }
        let view = self.terminals.get(&session)?.clone();
        let store = dnd::term::TermDrag::new(read);
        let drag = store.drag();
        tracing::info!(%drag, %session, "drag over a program asking for drops");
        let hook = Self::drop_hook(tile, cx);
        let op = view.update(cx, |v, cx| {
            v.sink_enter(store, hook, cx);
            v.sink_move(p, cx)
        });
        state.term = Some(OverTerm { tile, session, drag: Some(drag) });
        Some(Over::Remote(drag_op(op)))
    }

    /// Where `tile`'s shell tells what goes up for its drag: run on the workspace once the
    /// terminal's own update is done.
    fn drop_hook(tile: ItemRef, cx: &Context<Self>) -> DropHook {
        let workspace = cx.entity().downgrade();
        Rc::new(move |news, cx: &mut gpui::App| {
            let workspace = workspace.clone();
            cx.defer(move |cx| {
                let _gone = workspace.update(cx, |w, cx| w.term_drop_news(tile, news, cx));
            });
        })
    }

    /// `tile`'s shell says what goes up for the drag over it: representations its program
    /// accepted, inline or as streams ahead of other transfers; its files, into the drag's
    /// landing; and once the program is done with the drop, nothing more.
    fn term_drop_news(&mut self, tile: ItemRef, news: DropNews, cx: &mut Context<Self>) {
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

    /// Send `files` of `drag` into its landing on `tile`'s worker, for its shell's program,
    /// which is told they will not come if they cannot go.
    fn upload_term_drag(
        &mut self,
        tile: ItemRef,
        drag: DragId,
        files: &[PathBuf],
        scratch: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) =
            self.terminals.keys().copied().find(|s| self.tile_of_session(*s) == Some(tile))
        else {
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

    /// The drag left the shell whose program it was over: the program hears it, and what goes
    /// up for it stops.
    fn leave_terminal(&mut self, state: &mut DropIn, cx: &mut Context<Self>) {
        let Some(was) = state.term.take() else { return };
        let Some(view) = self.terminals.get(&was.session).cloned() else { return };
        if let Some(drag) = view.update(cx, |v, cx| v.sink_leave(cx)) {
            self.stop_drag_uploads(drag, cx);
        }
    }

    /// The drag left `over`: the worker's drag ends, and its upload stops.
    fn leave_screen(&mut self, over: &OverScreen, cx: &mut Context<Self>) {
        if let Some(screen) = self.screens.get(&over.tile.item).cloned() {
            screen.update(cx, ScreenView::drag_leave);
        }
        if let Some((drag, _)) = over.drag {
            self.stop_drag_uploads(drag, cx);
        }
    }

    /// `over`'s drag was dropped: its files start up into its landing, what the drop waits
    /// for, listed with the transfers and holding a quit until they are in.
    fn drop_made(&mut self, over: &OverScreen, drag: DragId, cx: &mut Context<Self>) {
        if !over.files.is_empty() {
            let upload = Upload { dropped: true, ..Upload::to_drag(over.tile, drag) };
            let _started = self.upload(over.tile, &over.files, upload, cx);
        }
        cx.notify();
    }

    /// The person stopped the drop of `drag` on `tile` while its files went up: the window
    /// lets go of it on the worker, and nothing more goes up for it.
    pub(in crate::workspace) fn cancel_drop(
        &mut self,
        tile: ItemRef,
        drag: DragId,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(%drag, item = %tile.item, "drop cancelled");
        if let Some(screen) = self.screens.get(&tile.item).cloned()
            && screen.read(cx).dragging() == Some(drag)
        {
            screen.update(cx, ScreenView::drag_leave);
        }
        self.stop_drag_uploads(drag, cx);
    }

    /// Stop what goes up into `drag`'s landing.
    fn stop_drag_uploads(&mut self, drag: DragId, cx: &mut Context<Self>) {
        let xfers: Vec<_> =
            self.uploads.iter().filter(|(_, u)| u.drag == Some(drag)).map(|(x, _)| *x).collect();
        for xfer in xfers {
            self.cancel_upload(xfer, cx);
        }
    }

    /// Dropped at `p`, promising `promised` files: what becomes of it. A drop the worker said
    /// nothing takes is refused, and slides back. One that waits for promised files has them
    /// called in, and shows its ring meanwhile.
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
        if let Some(plain) = state.plain.take() {
            return self.drop_on_shell(state, plain, promised, cx);
        }
        let Some(over) = state.over.take() else { return Taken::Refused };
        let (Some((drag, items)), Some(screen)) =
            (over.drag.clone(), self.screens.get(&over.tile.item).cloned())
        else {
            self.leave_screen(&over, cx);
            return Taken::Refused;
        };
        if !screen.read(cx).drag_takes() {
            self.leave_screen(&over, cx);
            return Taken::Refused;
        }
        if promised == 0 {
            let taken = screen.update(cx, |v, cx| v.drag_drop(p, Vec::new(), cx));
            if taken {
                self.drop_made(&over, drag, cx);
            }
            return if taken { Taken::CallIn } else { Taken::Refused };
        }
        screen.update(cx, |v, cx| v.drag_hold(p, cx));
        self.drop_made(&over, drag, cx);
        state.waiting = Some(Waiting::Screen { tile: over.tile, drag, at: p, items });
        Taken::CallIn
    }

    /// Dropped on `plain`, a shell: its named files go to the shell's directory now, its
    /// promised ones once they are written here.
    fn drop_on_shell(
        &mut self,
        state: &mut DropIn,
        plain: OverPlain,
        promised: usize,
        cx: &mut Context<Self>,
    ) -> Taken {
        let OverPlain { tile, session, files } = plain;
        if promised > 0 {
            state.waiting = Some(Waiting::Plain { tile, session, files });
            return Taken::CallIn;
        }
        if files.is_empty() {
            return Taken::Refused;
        }
        let _started = self.upload(tile, &files, Upload::to_shell(tile, session), cx);
        Taken::AsIs
    }

    /// Dropped at `p` on `term`, whose program the drag was over: the program has it unless
    /// it refused it. Promised files it wants are called in, to go up with the drag's own.
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
                state.waiting =
                    Some(Waiting::Term { tile: term.tile, session: term.session, drag });
                Taken::CallIn
            }
        }
    }

    /// The files a drop's promises wrote are here, or failed: they go where the drop was.
    fn drag_arrived(&mut self, waiting: Waiting, dropped: &Dropped, cx: &mut Context<Self>) {
        if !dropped.failed.is_empty() {
            Self::show_failure(format!("Not sent: {}", dropped.failed.join("; ")), cx);
        }
        let scratch = dropped.landing.clone();
        match waiting {
            Waiting::Term { tile, session, drag } => {
                let mut files = self
                    .terminals
                    .get(&session)
                    .map(|v| v.read(cx).sink_files(drag))
                    .unwrap_or_default();
                files.extend(dropped.paths.iter().cloned());
                self.upload_term_drag(tile, drag, &files, scratch, cx);
            }
            Waiting::Plain { tile, session, mut files } => {
                files.extend(dropped.paths.iter().cloned());
                if files.is_empty() {
                    Self::discard_landing(scratch, cx);
                    return;
                }
                let upload = Upload { scratch, ..Upload::to_shell(tile, session) };
                let _started = self.upload(tile, &files, upload, cx);
            }
            Waiting::Screen { tile, drag, at, items } => {
                let promised = items
                    .iter()
                    .enumerate()
                    .map(|(n, &item)| Promised {
                        item,
                        file: dropped.paths.get(n).and_then(|p| dnd::file_meta(p)),
                    })
                    .collect();
                let Some(screen) = self.screens.get(&tile.item).cloned() else {
                    Self::discard_landing(scratch, cx);
                    return;
                };
                let sent = screen.update(cx, |v, cx| v.drag_drop(at, promised, cx));
                if !sent || dropped.paths.is_empty() {
                    Self::discard_landing(scratch, cx);
                    return;
                }
                let upload = Upload { scratch, dropped: true, ..Upload::to_drag(tile, drag) };
                let _started = self.upload(tile, &dropped.paths, upload, cx);
            }
        }
    }

    /// The worker said how the drop of `drag` on `tile` ended: a drop that did not land says
    /// why, and what still goes up for it stops.
    pub(in crate::workspace) fn drag_ended(
        &mut self,
        tile: ItemRef,
        drag: DragId,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(%drag, ?outcome, item = %tile.item, "drop ended");
        match outcome {
            Outcome::Landed(_) => {}
            Outcome::Refused => {
                self.stop_drag_uploads(drag, cx);
                Self::show_notice("Nothing there took the drop".to_owned(), cx);
            }
            Outcome::Failed(why) => {
                self.stop_drag_uploads(drag, cx);
                Self::show_failure(format!("The drop did not land: {why}"), cx);
            }
        }
    }

    /// Delete a drop's landing that nothing will upload from, off the main thread.
    fn discard_landing(landing: Option<PathBuf>, cx: &Context<Self>) {
        if let Some(landing) = landing {
            cx.background_executor()
                .spawn(async move { slopty_platform::file_drop::discard(&landing) })
                .detach();
        }
    }

    /// Files a drop elsewhere in the window called in, handed to the shell's own drop handling
    /// as a file drop of their paths. Their landing stays for what took them; the platform
    /// sweeps it with a later run.
    fn local_arrived(
        view: &WeakEntity<Self>,
        dropped: &Dropped,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let position = points((dropped.x, dropped.y));
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
        if !dropped.failed.is_empty() {
            let text = format!("Not sent: {}", dropped.failed.join("; "));
            let _gone = view.update(cx, |_, cx| Self::show_failure(text, cx));
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
    /// The drags over `handle`, the window `view` serves.
    pub(super) fn new(
        handle: AnyWindowHandle,
        view: WeakEntity<WorkspaceView>,
        app: AsyncApp,
    ) -> Self {
        Self { handle, view, app, state: Rc::default() }
    }

    /// Run `f` on the workspace with the window; `None` when either is gone, or busy: a drag
    /// that began in this window and runs its own loop inside an update, which then stays
    /// GPUI's.
    fn with<R>(
        &self,
        f: impl FnOnce(&mut WorkspaceView, &mut DropIn, &Window, &mut Context<WorkspaceView>) -> R,
    ) -> Option<R> {
        let mut app = self.app.clone();
        let mut state = self.state.try_borrow_mut().ok()?;
        self.handle
            .update(&mut app, |_root, window, cx| {
                self.view.update(cx, |v, cx| f(v, &mut state, window, cx))
            })
            .ok()?
            .ok()
    }
}

impl DropSink for Sink {
    fn over(
        &self,
        at: (f64, f64),
        board: &dyn slopty_platform::pasteboard::Pasteboard,
        allowed: slopty_proto::drag::DragOps,
        _own: Option<u64>,
    ) -> Over {
        let carried = Carried { board, allowed };
        self.with(|v, state, window, cx| v.drag_over(state, points(at), carried, window, cx))
            .unwrap_or(Over::Local)
    }

    fn left(&self) {
        let _gone = self.with(|v, state, _window, cx| v.drag_left(state, cx));
    }

    fn dropped(&self, at: (f64, f64), promised: usize) -> Taken {
        self.with(|v, state, _window, cx| v.drag_dropped(state, points(at), promised, cx))
            .unwrap_or(Taken::Refused)
    }

    fn arrived(&self, dropped: Dropped) {
        if dropped.remote {
            let delivered = self.with(|v, state, _window, cx| match state.waiting.take() {
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

/// What a drop on a shell's program does, as the drag shows it.
const fn drag_op(operation: slopty_proto::terminal::DropOperation) -> DragOp {
    use slopty_proto::terminal::DropOperation;
    match operation {
        DropOperation::None => DragOp::None,
        DropOperation::Copy => DragOp::Copy,
        DropOperation::Move => DragOp::Move,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use gpui::{
        AnyView, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _,
        TestAppContext, VisualTestContext, div,
    };
    use parking_lot::Mutex;
    use slopty_client::remote::Remote;
    use slopty_client::xfer::{Download, XferError};
    use slopty_core::XferId;
    use slopty_platform::pasteboard::Memory;
    use slopty_proto::drag::DragOps;
    use slopty_proto::items::{Item, ItemKind, ItemSync};
    use slopty_proto::transfer::Dest;
    use slopty_theme::Theme;

    use super::*;
    use crate::workspace::rig::{KEY, link_with, mac, shell};

    /// A worker that keeps what is sent up to it.
    #[derive(Debug, Default)]
    struct Uploads(Mutex<Vec<(Vec<PathBuf>, Dest)>>);

    impl Remote for Uploads {
        fn upload(&self, _xfer: XferId, files: Vec<PathBuf>, dest: Dest, _again: bool) {
            self.0.lock().push((files, dest));
        }

        fn cancel(&self, _xfer: XferId) {}

        fn download(&self, _ask: Download) -> Result<Vec<PathBuf>, XferError> {
            Err(XferError::Worker("no downloads here".to_owned()))
        }

        fn clip_fetch(&self, _rep: &RepRef, _max: Option<u64>, _wait: Duration) -> Fetched {
            Fetched::Gone
        }

        fn send_clip(&self, _rep: RepRef, _answer: Fetched, _urgent: bool) {}

        fn forward(&self, _port: u16) -> Option<u16> {
            None
        }
    }

    /// The shell's window: a view in its left 400 points, nothing right of it.
    struct Host(Option<AnyView>);

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().child(div().w(px(400.0)).h_full().children(self.0.clone()))
        }
    }

    /// A file dragged from the Finder over a shell's grid is the shell's: dropped, it goes up to
    /// the shell's directory. Right of the shell the window's own handling takes the drag.
    #[gpui::test]
    fn a_file_dropped_on_a_shell_goes_to_its_directory(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let (host, cx) = cx.add_window_view(|_, _| Host(None));
        let session = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let uploads = Arc::new(Uploads::default());
        let remote: Arc<dyn Remote> = Arc::<Uploads>::clone(&uploads);
        let _rx = link_with(&workspace, cx, mac(|_| {}), vec![shell(session)], Some(remote));
        let item = Item {
            id: slopty_core::ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: std::collections::BTreeMap::new(),
        };
        let items = vec![item];
        workspace
            .update(cx, |v, cx| v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items }, cx));
        cx.run_until_parked();
        let terminal = workspace.read_with(cx, |v, _| v.terminals[&session].clone());
        host.update(cx, |host, cx| {
            host.0 = Some(terminal.into());
            cx.notify();
        });
        cx.run_until_parked();

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"hi").unwrap();
        let board = Memory::default();
        board.copy_files(&[&format!("file://{}", file.display())]);
        let mut state = DropIn::default();
        let over = |cx: &mut VisualTestContext, state: &mut DropIn, x: f32| {
            let carried = Carried { board: &board, allowed: DragOps::COPY };
            cx.update(|window, cx| {
                workspace.update(cx, |v, cx| {
                    v.drag_over(state, point(px(x), px(100.0)), carried, window, cx)
                })
            })
        };
        assert_eq!(over(cx, &mut state, 800.0), Over::Local, "nothing of Slopty's there");
        assert_eq!(over(cx, &mut state, 200.0), Over::Remote(DragOp::Copy), "the shell's");
        let taken = workspace
            .update(cx, |v, cx| v.drag_dropped(&mut state, point(px(200.0), px(100.0)), 0, cx));
        assert_eq!(taken, Taken::AsIs);
        let sent = uploads.0.lock().clone();
        assert_eq!(sent, [(vec![file], Dest::SessionCwd(session))]);
        // It is listed, as the app's toast shows it, until it is stopped.
        let rows = workspace.read_with(cx, |v, _| v.transfer_rows(std::time::Instant::now()));
        let [row] = rows.as_slice() else { panic!("one transfer: {rows:?}") };
        assert_eq!(
            (row.up, row.name.as_str(), row.machine.as_str()),
            (true, "notes.txt", "studio")
        );
        let xfer = row.xfer;
        workspace.update(cx, |v, cx| v.cancel_transfer(xfer, cx));
        let rows = workspace.read_with(cx, |v, _| v.transfer_rows(std::time::Instant::now()));
        assert!(rows.is_empty(), "stopped: {rows:?}");
    }
}
