//! Files dragged from this device onto a program that asks for drops (Kitty drag and drop,
//! OSC 72; `docs/decisions/terminal.md`).
//!
//! While the program asks ([`TermState::drop_target`](slopty_client::TermState::drop_target)),
//! a drag of files over the grid goes to it cell by cell as a `text/uri-list` drag, and a drop
//! is the program's: it is told at once, the tile uploads the files to the shell's directory as
//! for any drop, and the program gets the `file://` URLs of the worker's copies once they have
//! landed ([`TerminalView::files_landed`]). A program that refuses the drag gets no drop, and
//! nothing is uploaded. A program not asking has its files' paths typed, as before.

use gpui::{Context, DragMoveEvent, ExternalPaths, Pixels, Point};
use slopty_client::term::{DragAnswer, URI_LIST, uri_list};
use slopty_client::xfer::paste_paths;
use slopty_proto::terminal::{DropOperation, DropPoint, DropRep, TermRequest};

use super::TerminalView;

/// A drag of files over the grid of a program that asks for drops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct FileDrag {
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
        if moved {
            if self.file_drag.is_none() {
                // A new drag: an answer to the last one says nothing of it.
                self.state.forget_drag_answer();
            }
            self.file_drag = Some(FileDrag { at, dropped: false });
            self.send(TermRequest::DragOver { at, mimes: vec![URI_LIST.to_owned()] }, cx);
        }
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
        let reps = vec![DropRep { mime: URI_LIST.to_owned(), data: None }];
        self.send(TermRequest::Drop { at: drag.at, reps }, cx);
        false
    }

    /// Files uploaded for this shell have landed at `paths` on the worker. A drop the program
    /// took gets their `file://` URLs; otherwise, or once the program stopped asking, their
    /// paths are typed.
    pub fn files_landed(&mut self, paths: &[String], cx: &Context<Self>) {
        let dropped = self.file_drag.take().is_some_and(|d| d.dropped);
        let req = if dropped && self.state.drop_target() {
            TermRequest::DropData { mime: URI_LIST.to_owned(), data: Some(uri_list(paths)) }
        } else {
            TermRequest::Paste { text: paste_paths(paths), confirmed: false }
        };
        self.send(req, cx);
    }

    /// Files uploaded for this shell will not land: a drop the program waits for is told they
    /// will not come.
    pub fn files_failed(&mut self, cx: &Context<Self>) {
        if self.file_drag.take().is_some_and(|d| d.dropped) {
            self.send(TermRequest::DropData { mime: URI_LIST.to_owned(), data: None }, cx);
        }
    }

    /// The drag's point over the grid at `at`, in cells and in the worker's pixels; `None` off
    /// the grid. Files from this device are copied up, never moved.
    fn drop_point(&self, at: Point<Pixels>) -> Option<DropPoint> {
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
