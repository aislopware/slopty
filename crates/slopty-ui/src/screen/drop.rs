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

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement, MouseButton, MouseMoveEvent, ParentElement as _,
    Pixels, Point, SharedString, Styled as _, div, px,
};
use slopty_client::dnd::out::{Outgoing, Shared};
use slopty_client::dnd::{Hover, Outcome, Phase, Read};
use slopty_proto::drag::{DragEvent, DragId, DragInput, DragItem, DragOp, DragOps, Promised};
use slopty_proto::screen::ScreenInput;

use super::{ScreenView, ScreenViewEvent};

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

/// How far past the pointer a drag's ghost sits.
const GHOST_OFFSET: f32 = 12.0;

/// The side of a drop's landing disc.
const RING: f32 = 24.0;

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

    /// Whether the window last drew this body at `position`, on top: where a drag from another
    /// app is this window's or display's.
    #[must_use]
    pub fn shows_at(&self, position: Point<Pixels>, window: &gpui::Window) -> bool {
        self.spot.at(position, window)
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
    /// the body's top left) while it is on the tile: the shell's drag ghost (its files' drag
    /// chip), with the first item's icon and name, and how many more.
    pub(super) fn taking_icons(
        &self,
        (x, y): (Pixels, Pixels),
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let taking = self.taking.as_ref().filter(|t| !t.handed)?;
        let theme = slopty_shell::live::chrome::theme(cx);
        let first = taking.items.first();
        let file = first.and_then(|item| item.file.as_ref());
        let name = file.map_or_else(
            || first.and_then(|item| item.promised.clone()).unwrap_or_else(|| "Item".to_owned()),
            |file| file.name.clone(),
        );
        let identity = if file.is_some_and(|file| file.folder) {
            slopty_shell::file_icons::FileIconIdentity::directory(&name, false)
        } else {
            slopty_shell::file_icons::FileIconIdentity::file(&name)
        };
        let glyph = slopty_shell::file_icons::icon(identity, theme.appearance).size(px(14.0));
        let more = taking.items.len().saturating_sub(1);
        let ghost =
            div()
                .absolute()
                .left(x + px(GHOST_OFFSET))
                .top(y + px(GHOST_OFFSET))
                .debug_selector(|| "screen-drag-out-items".to_owned())
                .h(px(24.0))
                .max_w(px(220.0))
                .px(px(8.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .rounded(px(6.0))
                .bg(theme.surface_raised)
                .border_1()
                .border_color(theme.border_strong)
                .text_size(px(11.5))
                .text_color(theme.text)
                .opacity(0.85)
                .child(glyph)
                .child(div().min_w_0().truncate().child(SharedString::from(name)))
                .children((more > 0).then(|| {
                    div().flex_none().text_color(theme.text_muted).child(format!("+{more}"))
                }));
        Some(ghost)
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

    /// The ring at a drop's point while the worker lands it: the shell's small spinner in a
    /// frosted disc.
    pub(super) fn drop_ring(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let at = self.drop?.ring?;
        let theme = slopty_shell::live::chrome::theme(cx).for_popup();
        let view = cx.entity_id();
        let spinner = slopty_shell::loaders::mini_mono_spinner(
            "screen-drop",
            2.0,
            theme.text_muted,
            view,
            cx,
        );
        let disc = div()
            .size(px(RING))
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(slopty_shell::popover::surface_bg(&theme))
            .when(!theme.is_frost(), gpui::Styled::shadow_md)
            .flex()
            .items_center()
            .justify_center()
            .child(spinner);
        let ring = div()
            .absolute()
            .left(at.x - px(RING / 2.0))
            .top(at.y - px(RING / 2.0))
            .size(px(RING))
            .debug_selector(|| "screen-drop-ring".to_owned())
            .child(slopty_shell::frost::frosted(RING / 2.0, slopty_shell::frost::MENU_BLUR, disc));
        Some(ring)
    }
}

#[cfg(test)]
mod tests;
