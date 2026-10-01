//! A drag from this device over a remote window or display, the tile's half
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! The workspace finds the tile under the drag and reads what the drag carries; the tile maps
//! the drag's point to the stream's pixels and sends its entry, moves, drop and leaving to the
//! worker in order with the rest of its input ([`slopty_client::dnd::Hover`]). What a drop
//! would do is the worker's last word, which the system's drag shows as its badge. While a drag
//! is over the tile the worker's pointer is not drawn, since the system's drag is the pointer;
//! once dropped, a ring at the point turns until the worker says how the drop landed.

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Point, Styled as _,
    div, px,
};
use slopty_client::dnd::{Hover, Outcome, Phase, Read};
use slopty_proto::drag::{DragEvent, DragId, DragOp, DragOps, Promised};
use slopty_proto::screen::ScreenInput;

use super::ScreenView;
use crate::colors::hsla;
use crate::icons::{Status, status_icon};
use crate::kit;

/// A drag over the tile.
#[derive(Clone, Copy, Debug)]
pub(super) struct Dropping {
    hover: Hover,
    /// Where it was dropped, from the body's top left, while the worker lands it.
    ring: Option<Point<Pixels>>,
}

impl ScreenView {
    /// A drag carrying `read` entered the tile at `position` (window points), its source
    /// letting a target take it as `allowed` says: the worker begins its drag there. A drag
    /// still on the tile is left first.
    pub fn drag_enter(
        &mut self,
        position: Point<Pixels>,
        read: &Read,
        allowed: DragOps,
        cx: &mut Context<Self>,
    ) -> DragId {
        self.drag_leave(cx);
        let (hover, enter) = Hover::enter(self.to_stream_edge(position), read, allowed);
        self.input(ScreenInput::Drag(enter));
        self.drop = Some(Dropping { hover, ring: None });
        cx.notify();
        hover.drag()
    }

    /// The drag moved to `position`: what a drop there would do, as the worker last said.
    pub fn drag_move(&mut self, position: Point<Pixels>) -> DragOp {
        let at = self.to_stream_edge(position);
        let Some(dropping) = self.drop.as_mut() else { return DragOp::None };
        let moved = dropping.hover.moved(at);
        let op = dropping.hover.op();
        if let Some(moved) = moved {
            self.input(ScreenInput::Drag(moved));
        }
        op
    }

    /// The drag left the tile, or ended over it with no drop: the worker's drag ends too.
    pub fn drag_leave(&mut self, cx: &mut Context<Self>) {
        let Some(mut dropping) = self.drop.take() else { return };
        if let Some(leave) = dropping.hover.leave() {
            self.input(ScreenInput::Drag(leave));
        }
        cx.notify();
    }

    /// Whether a drop here now would be taken: the worker last said its target takes it.
    #[must_use]
    pub fn drag_takes(&self) -> bool {
        self.drop.is_some_and(|d| d.hover.phase() == Phase::Hovering && d.hover.op().takes())
    }

    /// Dropped at `position`, with the files its promises wrote: whether the drop goes to the
    /// worker. One the worker said nothing takes is refused here, and the worker's drag ends.
    pub fn drag_drop(
        &mut self,
        position: Point<Pixels>,
        promised: Vec<Promised>,
        cx: &mut Context<Self>,
    ) -> bool {
        let at = self.to_stream_edge(position);
        let origin = self.bounds.origin;
        let Some(dropping) = self.drop.as_mut() else { return false };
        let Some(drop) = dropping.hover.drop(at, promised) else {
            self.drag_leave(cx);
            return false;
        };
        dropping.ring = Some(position - origin);
        self.input(ScreenInput::Drag(drop));
        cx.notify();
        true
    }

    /// Dropped at `position`, where its promised files are still being written here: the ring
    /// shows at the point meanwhile, and [`Self::drag_drop`] follows once they are.
    pub fn drag_hold(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let origin = self.bounds.origin;
        if let Some(dropping) = self.drop.as_mut() {
            dropping.ring = Some(position - origin);
            cx.notify();
        }
    }

    /// The worker said `event` of a drag: how the drop ended, once it has.
    pub fn drag_heard(
        &mut self,
        event: &DragEvent,
        cx: &mut Context<Self>,
    ) -> Option<(DragId, Outcome)> {
        let dropping = self.drop.as_mut()?;
        let outcome = dropping.hover.heard(event)?;
        let drag = dropping.hover.drag();
        self.drop = None;
        cx.notify();
        Some((drag, outcome))
    }

    /// The drag over the tile, if one is.
    #[must_use]
    pub fn dragging(&self) -> Option<DragId> {
        self.drop.map(|d| d.hover.drag())
    }

    /// The ring at a drop's point while the worker lands it.
    pub(super) fn drop_ring(&self) -> Option<impl IntoElement + use<>> {
        let at = self.drop?.ring?;
        let theme = &self.theme;
        let side = theme.density.hit;
        let icon = theme.typography.icon();
        let ring = kit::elevate(div(), theme)
            .absolute()
            .left(at.x - px(side / 2.0))
            .top(at.y - px(side / 2.0))
            .size(px(side))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .debug_selector(|| "screen-drop-ring".to_owned())
            .child(status_icon(theme, Status::Working, px(icon), hsla(theme.surfaces.accent)));
        Some(ring)
    }
}

#[cfg(test)]
mod tests;
