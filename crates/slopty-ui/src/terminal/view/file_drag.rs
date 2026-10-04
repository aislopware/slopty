//! Files dragged from this device onto a program that asks for drops (Kitty drag and drop,
//! OSC 72; `docs/decisions/terminal.md`).
//!
//! While the program asks ([`TermState::drop_target`](slopty_client::TermState::drop_target)),
//! a drag of files over the grid enters it naming its files, goes to it cell by cell, and a
//! drop is the program's: it is told at once, the tile uploads the files to the shell's
//! directory as for any drop, and the program gets the `file://` URLs of the worker's copies
//! once they have landed ([`TerminalView::files_landed`]). A program that refuses the drag gets
//! no drop, and nothing is uploaded. A program not asking has its files' paths typed, as
//! before.
//!
//! On a Mac the window's drop sink carries a drag over a program asking for drops instead
//! (`workspace/remote/drop_in.rs`), whatever it carries: texts, pictures and files alike. The
//! tile keeps what the drag carries ([`TermDrag`]) for the drag's life, and tells the workspace
//! what to send ([`DropNews`]): what the program accepts during the hover goes up at once, and
//! what it asks for on the drop is fetched from here ([`TerminalView::drag_fetch`]).

use gpui::{Context, DragMoveEvent, ExternalPaths, Pixels, Point};
#[cfg(target_os = "macos")]
use slopty_client::clip::Fetched;
#[cfg(target_os = "macos")]
use slopty_client::dnd::term::TermDrag;
use slopty_client::term::DragAnswer;
use slopty_client::xfer::paste_paths;
use slopty_proto::drag::{DragId, DragItem};
#[cfg(target_os = "macos")]
use slopty_proto::terminal::TermEvent;
use slopty_proto::terminal::{DropOperation, DropPoint, TermRequest};
#[cfg(target_os = "macos")]
use slopty_proto::transfer::{ClipType, RepRef};

use super::TerminalView;

/// A drag of files over the grid of a program that asks for drops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FileDrag {
    /// Which drag the program was told of.
    drag: DragId,
    /// Where the program was last told the drag is.
    at: DropPoint,
    /// It was dropped, and the program waits for its files.
    dropped: bool,
}

impl TerminalView {
    /// A drag of files moved: over the grid of a program that asks for drops, it is told
    /// which cell, once per cell; off the grid, or once it stops asking, the drag left.
    pub(super) fn files_dragged(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        cx: &Context<Self>,
    ) {
        if self.file_drag.is_some_and(|d| d.dropped) {
            return;
        }
        let Some(at) = self.drop_point(event.event.position).filter(|_| self.state.drop_target())
        else {
            self.file_drag_left(cx);
            return;
        };
        let moved = self.file_drag.is_none_or(|d| (d.at.col, d.at.row) != (at.col, at.row));
        if !moved {
            return;
        }
        let drag = if let Some(was) = self.file_drag {
            was.drag
        } else {
            // A new drag: an answer to the last one says nothing of it.
            self.state.forget_drag_answer();
            let drag = DragId::new();
            let items = event.drag(cx).paths().iter().filter_map(|p| file_item(p)).collect();
            self.send(TermRequest::DragEnter { drag, items }, cx);
            drag
        };
        self.file_drag = Some(FileDrag { drag, at, dropped: false });
        self.send(TermRequest::DragOver { at }, cx);
    }

    /// The drag the program was told of is no longer over the grid.
    pub(super) fn file_drag_left(&mut self, cx: &Context<Self>) {
        if self.file_drag.take_if(|d| !d.dropped).is_some() {
            self.send(TermRequest::DragLeave, cx);
        }
    }

    /// The drag over the grid was let go: the program is told of the drop, and the tile goes
    /// on to upload its files. Whether the drop is done with: a program that refused the
    /// drag takes no drop, and nothing goes up for it.
    pub(super) fn files_dropped(&mut self, cx: &Context<Self>) -> bool {
        let Some(drag) = self.file_drag.filter(|d| !d.dropped) else { return false };
        let refused = matches!(
            self.state.drag_answer(),
            Some(DragAnswer::Accepted { operation: DropOperation::None, .. })
        );
        if refused {
            self.file_drag_left(cx);
            return true;
        }
        self.file_drag = Some(FileDrag { dropped: true, ..drag });
        self.send(TermRequest::Drop { at: drag.at }, cx);
        false
    }

    /// Files uploaded for this shell have landed at `paths` on the worker, for `drag` when
    /// they went up as a drag's. A drop the program took gets their `file://` URLs, and so
    /// does a drag still over it; otherwise, or once the program stopped asking, their paths
    /// are typed.
    pub fn files_landed(
        &mut self,
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac carries a drag through the drop sink")
        )]
        drag: Option<DragId>,
        paths: &[String],
        cx: &Context<Self>,
    ) {
        // A drag's files are its program's alone: a drag it is done with types nothing.
        #[cfg(target_os = "macos")]
        if let Some(drag) = drag {
            if self.sink_drags.has(drag) {
                self.send(TermRequest::DropFiles { drag, landed: Some(paths.to_vec()) }, cx);
            }
            return;
        }
        let dropped = self.file_drag.take().filter(|d| d.dropped);
        let req = if let Some(FileDrag { drag, .. }) = dropped
            && self.state.drop_target()
        {
            TermRequest::DropFiles { drag, landed: Some(paths.to_vec()) }
        } else {
            TermRequest::Paste { text: paste_paths(paths), confirmed: false }
        };
        self.send(req, cx);
    }

    /// Files uploaded for this shell will not land, for `drag` when they went up as a drag's:
    /// a drop the program waits for is told they will not come.
    pub fn files_failed(
        &mut self,
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac carries a drag through the drop sink")
        )]
        drag: Option<DragId>,
        cx: &Context<Self>,
    ) {
        #[cfg(target_os = "macos")]
        if let Some(drag) = drag {
            if self.sink_drags.has(drag) {
                self.send(TermRequest::DropFiles { drag, landed: None }, cx);
            }
            return;
        }
        if let Some(FileDrag { drag, .. }) = self.file_drag.take().filter(|d| d.dropped) {
            self.send(TermRequest::DropFiles { drag, landed: None }, cx);
        }
    }

    /// The drag's point over the grid at `at`, in cells and in the worker's pixels; `None` off
    /// the grid. Files from this device are copied up, never moved.
    pub(super) fn drop_point(&self, at: Point<Pixels>) -> Option<DropPoint> {
        let metrics = self.metrics?;
        let (col, row) = metrics.cell_at(at)?;
        let (x, y) = metrics.pixel_at(at);
        Some(DropPoint {
            col,
            row,
            x: i32::try_from(x).unwrap_or(i32::MAX),
            y: i32::try_from(y).unwrap_or(i32::MAX),
            copy: true,
            moves: false,
        })
    }
}

/// The file at `path` as a drag's item; `None` when it cannot be read.
fn file_item(path: &std::path::Path) -> Option<DragItem> {
    let file = slopty_client::dnd::file_meta(path)?;
    Some(DragItem { file: Some(file), promised: None, reps: Vec::new() })
}

/// What the workspace is to do for a drag the drop sink carries over the grid.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub enum DropNews {
    /// Send these representations up, under the drag: the program accepted them.
    Push(Vec<(RepRef, Vec<u8>)>),
    /// Send the drag's files up into its landing: the program accepted them.
    Upload(DragId, Vec<std::path::PathBuf>),
    /// The program is done with the drop, having done `operation`: what still goes up for it
    /// stops.
    Ended(DragId, DropOperation),
}

/// Where a tile tells the workspace [`DropNews`].
#[cfg(target_os = "macos")]
pub type DropHook = std::rc::Rc<dyn Fn(DropNews, &mut gpui::App)>;

/// What became of a drop the drop sink carried onto the grid.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkDropped {
    /// The program refused the drag: there is no drop.
    Refused,
    /// The program has the drop. `call_in`: the program wants the drag's files and some are
    /// only promised, so they are called in now, and go up with the rest once written.
    Taken {
        /// The drag.
        drag: DragId,
        /// Its files are called in.
        call_in: bool,
    },
}

/// The drags the drop sink carries over the grid: the one over it, and the one dropped until
/// the program concludes it.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(super) struct SinkDrags {
    hover: Option<SinkDrag>,
    dropped: Option<SinkDrag>,
}

#[cfg(target_os = "macos")]
struct SinkDrag {
    store: TermDrag,
    /// Where the program was last told it is.
    at: Option<DropPoint>,
    hook: DropHook,
}

#[cfg(target_os = "macos")]
impl SinkDrags {
    fn has(&self, drag: DragId) -> bool {
        self.find(drag).is_some()
    }

    fn find(&self, drag: DragId) -> Option<&SinkDrag> {
        [&self.hover, &self.dropped].into_iter().flatten().find(|d| d.store.drag() == drag)
    }
}

#[cfg(target_os = "macos")]
impl std::fmt::Debug for SinkDrags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let drag = |d: &Option<SinkDrag>| d.as_ref().map(|d| d.store.drag());
        f.debug_struct("SinkDrags")
            .field("hover", &drag(&self.hover))
            .field("dropped", &drag(&self.dropped))
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl TerminalView {
    /// Whether a drag at `at` (window points) is the program's: it asks for drops, and `at`
    /// is on the grid.
    #[must_use]
    pub fn takes_drag_at(&self, at: Point<Pixels>) -> bool {
        self.state.drop_target() && self.drop_point(at).is_some()
    }

    /// A drag the drop sink carries entered the grid, carrying `store`; what is to go up for
    /// it is told through `hook`. The program hears what the items are.
    pub fn sink_enter(&mut self, store: TermDrag, hook: DropHook, cx: &Context<Self>) {
        // A new drag: an answer to the last one says nothing of it.
        self.state.forget_drag_answer();
        let (drag, items) = (store.drag(), store.items());
        self.sink_drags.hover = Some(SinkDrag { store, at: None, hook });
        self.send(TermRequest::DragEnter { drag, items }, cx);
    }

    /// The drag is at `at`: the program is told when it moved to another cell. What a drop
    /// there would do, as the program last said; a copy until it says.
    pub fn sink_move(&mut self, at: Point<Pixels>, cx: &Context<Self>) -> DropOperation {
        let point = self.drop_point(at);
        if let Some(hover) = &mut self.sink_drags.hover
            && let Some(point) = point
            && hover.at.is_none_or(|was| (was.col, was.row) != (point.col, point.row))
        {
            hover.at = Some(point);
            self.send(TermRequest::DragOver { at: point }, cx);
        }
        match self.state.drag_answer() {
            Some(DragAnswer::Accepted { operation, .. }) => *operation,
            _ => DropOperation::Copy,
        }
    }

    /// The drag left the grid without a drop: the program hears it, and the drag's id is
    /// handed back for what still goes up for it to stop.
    pub fn sink_leave(&mut self, cx: &Context<Self>) -> Option<DragId> {
        let left = self.sink_drags.hover.take()?;
        self.send(TermRequest::DragLeave, cx);
        Some(left.store.drag())
    }

    /// The drag was let go at `at`. A drag the program refused is not dropped; one it took is
    /// the program's, and its files, unless the program wants them, are told not to come.
    pub fn sink_drop(&mut self, at: Point<Pixels>, cx: &Context<Self>) -> SinkDropped {
        let refused = matches!(
            self.state.drag_answer(),
            Some(DragAnswer::Accepted { operation: DropOperation::None, .. })
        );
        let Some(mut hover) = self.sink_drags.hover.take() else { return SinkDropped::Refused };
        let point = self.drop_point(at).or(hover.at);
        let Some(point) = point.filter(|_| !refused) else {
            self.send(TermRequest::DragLeave, cx);
            return SinkDropped::Refused;
        };
        hover.at = Some(point);
        let drag = hover.store.drag();
        self.send(TermRequest::Drop { at: point }, cx);
        let call_in = hover.store.files_wanted() && hover.store.promises();
        if hover.store.has_files() && !hover.store.files_wanted() {
            self.send(TermRequest::DropFiles { drag, landed: None }, cx);
        }
        self.sink_drags.dropped = Some(hover);
        SinkDropped::Taken { drag, call_in }
    }

    /// The paths of the dropped drag's files, its own and those its promises wrote, which go
    /// up together once the promises are kept.
    #[must_use]
    pub fn sink_files(&self, drag: DragId) -> Vec<std::path::PathBuf> {
        self.sink_drags.find(drag).map(|d| d.store.files().to_vec()).unwrap_or_default()
    }

    /// The worker's fetch of representation `kind` of item `item` of `drag`: `None` when the
    /// drag is not this tile's.
    #[must_use]
    pub fn drag_fetch(
        &self,
        drag: DragId,
        item: u16,
        kind: &ClipType,
        max: Option<u64>,
    ) -> Option<Fetched> {
        self.sink_drags.find(drag).map(|d| d.store.fetch(item, kind, max))
    }

    /// `event` from the worker, before the client state takes it: the program's answer to the
    /// drag sends what it accepted, and its conclusion ends the drop.
    pub(super) fn sink_heard(&mut self, event: &TermEvent, cx: &mut Context<Self>) {
        match event {
            TermEvent::DropAccepted { mimes, .. } => {
                let Some(hover) = &mut self.sink_drags.hover else { return };
                let accepted = hover.store.accepted(mimes);
                let hook = std::rc::Rc::clone(&hover.hook);
                if !accepted.pushes.is_empty() {
                    hook(DropNews::Push(accepted.pushes), cx);
                }
                if accepted.upload {
                    let files = hover.store.files().to_vec();
                    hook(DropNews::Upload(hover.store.drag(), files), cx);
                }
            }
            TermEvent::DropConcluded { operation } => {
                if let Some(dropped) = self.sink_drags.dropped.take() {
                    (dropped.hook)(DropNews::Ended(dropped.store.drag(), *operation), cx);
                }
            }
            _ => {}
        }
    }
}
