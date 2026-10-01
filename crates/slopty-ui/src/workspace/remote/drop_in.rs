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

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AnyWindowHandle, AsyncApp, Context, Pixels, Point, WeakEntity, point, px};
use slopty_client::clip::Fetched;
use slopty_client::dnd::{self, Outcome};
use slopty_client::layout::TileRef;
use slopty_platform::file_drop::{DropSink, Dropped};
use slopty_proto::drag::{DragId, Promised};
use slopty_proto::items::ItemKind;
use slopty_proto::transfer::{RepRef, Source};

use super::{Upload, WorkspaceView};
use crate::screen::ScreenView;

/// A drag from this device over the window: the remote tile it is over, if any, and a drop
/// waiting for its promised files.
#[derive(Debug, Default)]
pub struct DropIn {
    over: Option<OverTile>,
    waiting: Option<Waiting>,
}

/// The remote tile a drag is over.
#[derive(Clone, Debug)]
struct OverTile {
    tile: TileRef,
    /// The drag the tile carries to the worker, and the items that promise files; `None` when
    /// the drag carries nothing a remote app could take.
    drag: Option<(DragId, Vec<u16>)>,
}

/// A drop on a remote tile, waiting for the files its promises write here.
#[derive(Clone, Debug)]
struct Waiting {
    tile: TileRef,
    drag: DragId,
    at: Point<Pixels>,
    items: Vec<u16>,
}

impl DropIn {
    /// The drag the window is carrying to a worker, if it is.
    #[must_use]
    pub fn drag(&self) -> Option<DragId> {
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

    /// A drag carrying what is on `board`, which its source lets a target take as `allowed`
    /// says, is at `p`: what it is over. Entering a remote tile begins the worker's drag and
    /// the drag's upload; leaving one ends both.
    #[cfg(target_os = "macos")]
    pub fn drag_over(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        board: &dyn slopty_platform::pasteboard::Pasteboard,
        allowed: slopty_proto::drag::DragOps,
        cx: &mut Context<Self>,
    ) -> slopty_platform::file_drop::Over {
        use slopty_platform::file_drop::Over;
        use slopty_proto::drag::DragOp;
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
        let read = dnd::read(board);
        if read.is_empty() {
            state.over = Some(OverTile { tile, drag: None });
            return Over::Remote(DragOp::None);
        }
        let drag = screen.update(cx, |v, cx| v.drag_enter(p, &read, allowed, cx));
        let promises = (0_u16..).zip(&read.items).filter(|(_, i)| i.promised.is_some());
        let promises = promises.map(|(n, _)| n).collect();
        tracing::info!(%drag, items = read.items.len(), files = read.files.len(), "drag over a remote tile");
        if !read.files.is_empty() {
            let _started = self.upload(tile, &read.files, Upload::to_drag(tile, drag), cx);
        }
        if let Some(remote) = self.remote(tile.worker) {
            for push in read.pushes {
                let rep = RepRef { source: Source::Drag(drag), item: push.item, kind: push.kind };
                remote.send_clip(rep, Fetched::Data(push.bytes), false);
            }
        }
        state.over = Some(OverTile { tile, drag: Some((drag, promises)) });
        Over::Remote(screen.update(cx, |v, _cx| v.drag_move(p)))
    }

    /// The drag left the window from over a remote tile, or ended there with no drop.
    pub fn drag_left(&mut self, state: &mut DropIn, cx: &mut Context<Self>) {
        if let Some(over) = state.over.take() {
            self.leave_tile(&over, cx);
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

    /// Dropped at `p` over a remote tile, `promised` files called in for it: whether it is
    /// taken. A drop the worker said nothing takes is refused, and slides back. One that waits
    /// for promised files shows its ring meanwhile.
    pub fn drag_dropped(
        &mut self,
        state: &mut DropIn,
        p: Point<Pixels>,
        promised: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(over) = state.over.take() else { return false };
        let (Some((drag, items)), Some(screen)) =
            (over.drag.clone(), self.screen(over.tile.item).cloned())
        else {
            self.leave_tile(&over, cx);
            return false;
        };
        if !screen.read(cx).drag_takes() {
            self.leave_tile(&over, cx);
            return false;
        }
        if promised == 0 {
            return screen.update(cx, |v, cx| v.drag_drop(p, Vec::new(), cx));
        }
        screen.update(cx, |v, cx| v.drag_hold(p, cx));
        state.waiting = Some(Waiting { tile: over.tile, drag, at: p, items });
        true
    }

    /// The files a remote drop's promises wrote are here, or failed: they are named in the
    /// drop, which goes to the worker, and go up into its landing.
    fn drag_arrived(&mut self, waiting: Waiting, dropped: &Dropped, cx: &mut Context<Self>) {
        let Waiting { tile, drag, at, items } = waiting;
        let promised = items
            .iter()
            .enumerate()
            .map(|(n, &item)| Promised {
                item,
                file: dropped.paths.get(n).and_then(|p| dnd::file_meta(p)),
            })
            .collect();
        if !dropped.failed.is_empty() {
            self.show_notice(format!("Not sent: {}", dropped.failed.join("; ")), cx);
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
                self.show_notice("Nothing there took the drop".to_owned(), cx);
            }
            Outcome::Failed(why) => {
                self.stop_drag_uploads(drag, cx);
                self.show_notice(format!("The drop did not land: {why}"), cx);
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
    ) -> slopty_platform::file_drop::Over {
        self.with(|v, state, cx| v.drag_over(state, points(at), board, allowed, cx))
            .unwrap_or(slopty_platform::file_drop::Over::Local)
    }

    #[cfg(target_os = "macos")]
    fn left(&self) {
        let _gone = self.with(WorkspaceView::drag_left);
    }

    #[cfg(target_os = "macos")]
    fn dropped(&self, at: (f64, f64), promised: usize) -> bool {
        self.with(|v, state, cx| v.drag_dropped(state, points(at), promised, cx)).unwrap_or(false)
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
