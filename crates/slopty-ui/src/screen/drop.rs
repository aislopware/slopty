//! A drag from this device over a remote window or display, the tile's half
//! (`docs/decisions/audio.md`, "Drag and drop lands at the point, both ways").
//!
//! The workspace finds the tile under the drag and reads what the drag carries; the tile maps
//! the drag's point to the stream's pixels and sends its entry, moves, drop and leaving to the
//! worker in order with the rest of its input ([`slopty_client::dnd::Hover`]). What a drop
//! would do is the worker's last word, which the system's drag shows as its badge. While a drag
//! is over the tile the worker's pointer is not drawn, since the system's drag is the pointer;
//! once dropped, a ring at the point turns until the worker says how the drop landed.
//!
//! The other way, a drag an app on the worker begins under this tile's press
//! ([`slopty_client::dnd::out`]): its items are drawn at the pointer while it stays on the tile,
//! since a window's picture does not hold the worker's drag image, and when the pointer leaves
//! the tile with the button held the workspace drags them on from here
//! ([`ScreenViewEvent::DragOut`]) and the worker catches its own drag.

use std::sync::Arc;

use gpui::{
    Context, InteractiveElement as _, IntoElement, MouseButton, MouseMoveEvent, ParentElement as _,
    Pixels, Point, Styled as _, div, px,
};
use slopty_client::dnd::out::{Outgoing, Shared};
use slopty_client::dnd::{Hover, Outcome, Phase, Read};
use slopty_proto::drag::{DragEvent, DragId, DragInput, DragItem, DragOp, DragOps, Promised};
use slopty_proto::screen::ScreenInput;

use super::{ScreenView, ScreenViewEvent};
use crate::colors::hsla;
use crate::icons::{IconSize, Status, Symbol, icon, status_icon};
use crate::kit;

/// A drag over the tile.
#[derive(Clone, Copy, Debug)]
pub(super) struct Dropping {
    hover: Hover,
    /// Where it was dropped, from the body's top left, while the worker lands it.
    ring: Option<Point<Pixels>>,
}

/// A drag out of an app on the worker under the tile's press.
#[derive(Debug)]
pub(super) struct Taking {
    shared: Arc<Shared>,
    /// What it carries, for the icons drawn at the pointer.
    items: Vec<DragItem>,
    /// Handed over to this device's own drag: the pointer left the tile.
    handed: bool,
}

/// The most item icons drawn at the pointer; past them a count says how many more.
const ICONS: usize = 3;

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
        if matches!(
            event,
            DragEvent::OutBegan { .. } | DragEvent::OutCaught { .. } | DragEvent::OutFailed { .. }
        ) {
            self.drag_out_heard(event, cx);
            return None;
        }
        let dropping = self.drop.as_mut()?;
        let outcome = dropping.hover.heard(event)?;
        let drag = dropping.hover.drag();
        self.drop = None;
        cx.notify();
        Some((drag, outcome))
    }

    /// The worker said `event` of a drag out of one of its apps: one begun under this tile's
    /// press while it is still held is drawn at the pointer; the catch reaches the drag that
    /// took it on, and a failed one is said.
    fn drag_out_heard(&mut self, event: &DragEvent, cx: &mut Context<Self>) {
        match event {
            DragEvent::OutBegan { drag, items } => {
                if !self.buttons.contains(&slopty_proto::input::MouseButton::Left) {
                    return;
                }
                let shared = Arc::new(Shared::new(Outgoing::began(*drag, items.clone())));
                self.taking = Some(Taking { shared, items: items.clone(), handed: false });
                cx.notify();
            }
            DragEvent::OutCaught { drag, .. } | DragEvent::OutFailed { drag, .. } => {
                let Some(taking) = self.taking.take_if(|t| t.shared.drag() == *drag) else {
                    return;
                };
                if let Some(error) = taking.shared.heard(event) {
                    cx.emit(ScreenViewEvent::DragOutFailed(error));
                }
                cx.notify();
            }
            DragEvent::Operation { .. } | DragEvent::Ended { .. } => {}
        }
    }

    /// The pointer moved anywhere in the window: a drag out of the worker's app it carries off
    /// the tile with the left button held is handed over, to go on as this device's own drag
    /// from this very move, and the worker catches its drag. Its release is the catch's now.
    pub(super) fn drag_out_moved(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(taking) = self.taking.as_mut().filter(|t| !t.handed) else { return };
        if ev.pressed_button != Some(MouseButton::Left) || self.bounds.contains(&ev.position) {
            return;
        }
        taking.handed = true;
        let shared = Arc::clone(&taking.shared);
        let drag = shared.drag();
        self.buttons.retain(|b| *b != slopty_proto::input::MouseButton::Left);
        self.input(ScreenInput::Drag(DragInput::Catch { drag }));
        cx.emit(ScreenViewEvent::DragOut(shared));
        cx.notify();
    }

    /// The left button came up on the tile: a drag out it held dropped on the worker.
    pub(super) fn drag_out_released(&mut self, cx: &mut Context<Self>) {
        if self.taking.take_if(|t| !t.handed).is_some() {
            cx.notify();
        }
    }

    /// The items of a drag out of the worker's app, drawn just past the pointer at `at` (from
    /// the body's top left) while it is on the tile: a file, a folder or data each, a few at
    /// most, and how many more.
    pub(super) fn taking_icons(
        &self,
        (x, y): (Pixels, Pixels),
    ) -> Option<impl IntoElement + use<>> {
        let taking = self.taking.as_ref().filter(|t| !t.handed)?;
        let theme = &self.theme;
        let side = theme.density.hit;
        let gap = theme.spacing.inset();
        let ink = hsla(theme.surfaces.text_muted);
        let shown = taking.items.iter().take(ICONS).map(|item| {
            let name = match &item.file {
                Some(file) if file.folder => Symbol::Folder,
                Some(_) => Symbol::Doc,
                None if item.promised.is_some() => Symbol::Doc,
                None => Symbol::DocText,
            };
            kit::elevate(div(), theme)
                .size(px(side))
                .rounded(px(theme.radii.sm))
                .flex()
                .items_center()
                .justify_center()
                .child(icon(theme, name, IconSize::Inline, ink))
        });
        let more = taking.items.len().saturating_sub(ICONS);
        let count = (more > 0).then(|| {
            kit::elevate(div(), theme)
                .h(px(side))
                .px(px(gap))
                .rounded(px(theme.radii.sm))
                .flex()
                .items_center()
                .text_size(px(theme.typography.small()))
                .text_color(ink)
                .child(format!("+{more}"))
        });
        let icons = div()
            .absolute()
            .left(x + px(gap))
            .top(y + px(gap))
            .flex()
            .gap(px(gap / 2.0))
            .debug_selector(|| "screen-drag-out-items".to_owned())
            .children(shown)
            .children(count);
        Some(icons)
    }

    /// Whether a drop is being landed: its ring shows, and the worker holds its button down
    /// at the point until the drop is in.
    pub(super) fn landing(&self) -> bool {
        self.drop.is_some_and(|d| d.ring.is_some())
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
            // Busy is muted: colour is kept for what needs the person.
            .child(status_icon(theme, Status::Working, px(icon), hsla(theme.surfaces.text_muted)));
        Some(ring)
    }
}

#[cfg(test)]
mod tests;
