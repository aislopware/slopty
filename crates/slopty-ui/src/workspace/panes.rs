//! A tab's panes, drawn where the tiling model lays them out ([`TabFrame`]), and the sashes
//! between them.
//!
//! Panes are square and meet edge to edge on the window's one ground; the line two of them
//! share is a 1 pt sash, drawn over the edge and taking no room. A press on a sash drags it:
//! each move asks the host to move it by what the pointer travelled since the last one, and the
//! host's model says how far it went, so the line keeps under the pointer up to each pane's
//! least room and stops there. A double-click on a sash makes its split's shares equal.
//!
//! What a pane holds is the host's: it draws each pane's body and its header ([`PaneHost`]).
//! The drop wash shows the panel a dragged tile would become ([`wash`]).

#![cfg_attr(not(test), expect(dead_code, reason = "drawn by the workspace once the strip goes"))]

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, Context, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Styled as _, div, px,
};
use slopty_client::layout::Rect;
use slopty_client::layout::tiling::Drop;
use slopty_client::layout::tree::{Laid, Sash, Side, SplitAxis, TabFrame};
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::draw::Draw;
use crate::kit;

/// What draws in the panes and moves their sashes.
pub(super) trait PaneHost: Sized + 'static {
    /// Its panes' pointer state.
    fn panes(&self) -> &Panes;

    /// Its panes' pointer state, to change.
    fn panes_mut(&mut self) -> &mut Panes;

    /// Move `sash` by `delta` points (right or down); how far it went.
    fn drag_sash(&mut self, sash: &Sash, delta: f32, cx: &mut Context<Self>) -> f32;

    /// The sash let go: what follows a pane's new size (a remote window, a display) may
    /// follow now.
    fn sash_released(&mut self, cx: &mut Context<Self>);

    /// Make the shares of the split at `path` equal.
    fn equalize(&mut self, path: &[usize], cx: &mut Context<Self>);
}

/// The panes' pointer state: the sash being dragged.
#[derive(Debug, Default)]
pub(super) struct Panes {
    grab: Option<Grab>,
}

/// A sash held: where the pointer pressed it, along its split's axis, and how far it has
/// moved since.
#[derive(Debug)]
struct Grab {
    sash: Sash,
    from: f32,
    moved: f32,
}

impl Panes {
    /// The sash being dragged, if one is.
    #[must_use]
    pub(super) fn dragging(&self) -> Option<&Sash> {
        self.grab.as_ref().map(|g| &g.sash)
    }
}

/// Where along `axis` the pointer stands.
fn along(axis: SplitAxis, p: Point<Pixels>) -> f32 {
    match axis {
        SplitAxis::Row => f32::from(p.x),
        SplitAxis::Column => f32::from(p.y),
    }
}

/// A rectangle as GPUI bounds.
fn bounds(r: Rect) -> Bounds<Pixels> {
    Bounds::new(gpui::point(px(r.x), px(r.y)), gpui::size(px(r.w), px(r.h)))
}

/// A selector for the sash of split `path` before child `before`: `sash-0.1-2`.
fn sash_key(sash: &Sash) -> String {
    let path: Vec<String> = sash.path.iter().map(ToString::to_string).collect();
    format!("sash-{}-{}", path.join("."), sash.before)
}

/// The panel a tile dropped as `drop` becomes in a pane at `rect`: the whole pane where it
/// joins its tabs, else the half on the edge it splits.
#[must_use]
pub(super) fn wash(rect: Rect, drop: Drop) -> Rect {
    let (w, h) = (rect.w / 2.0, rect.h / 2.0);
    match drop.edge {
        None => rect,
        Some(Side::Left) => Rect { w, ..rect },
        Some(Side::Right) => Rect { x: rect.x + w, w, ..rect },
        Some(Side::Top) => Rect { h, ..rect },
        Some(Side::Bottom) => Rect { y: rect.y + h, h, ..rect },
    }
}

/// The panes of `frame`, each drawn by `body` at its rectangle in the layer's own
/// coordinates, the sashes over their edges, and `drop`'s wash over the pane it lands in. Built
/// from `host` as it is, read and never written ([`Draw`]).
pub(super) fn render<V: PaneHost>(
    host: &V,
    theme: &Theme,
    frame: &TabFrame,
    drop: Option<Drop>,
    mut body: impl FnMut(&Laid) -> gpui::AnyElement,
    cx: &Draw<'_, V>,
) -> gpui::Stateful<gpui::Div> {
    let dragging = host.panes().dragging().cloned();
    let panes: Vec<gpui::AnyElement> = frame
        .panes
        .iter()
        .map(|laid| {
            let id = laid.pane.get();
            let r = laid.rect;
            kit::pane_surface(theme)
                .id(("pane", id))
                .debug_selector(move || format!("pane-{id}"))
                .absolute()
                .left(px(r.x))
                .top(px(r.y))
                .w(px(r.w))
                .h(px(r.h))
                .child(body(laid))
                .into_any_element()
        })
        .collect();
    let sashes: Vec<gpui::AnyElement> = frame
        .sashes
        .iter()
        .map(|s| {
            let held = dragging.as_ref() == Some(s);
            let line = match s.axis {
                SplitAxis::Row => Rect {
                    x: s.line.x - f32::from(kit::HAIR) / 2.0,
                    w: f32::from(kit::HAIR),
                    ..s.line
                },
                SplitAxis::Column => Rect {
                    y: s.line.y - f32::from(kit::HAIR) / 2.0,
                    h: f32::from(kit::HAIR),
                    ..s.line
                },
            };
            let axis = match s.axis {
                SplitAxis::Row => gpui::Axis::Vertical,
                SplitAxis::Column => gpui::Axis::Horizontal,
            };
            let key = sash_key(s);
            let pressed = s.clone();
            kit::sash(gpui::SharedString::from(key.clone()), theme, axis, bounds(line), held)
                .debug_selector(move || key)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut V, ev: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        if ev.click_count == 2 {
                            this.panes_mut().grab = None;
                            this.equalize(&pressed.path, cx);
                            cx.notify();
                            return;
                        }
                        let from = along(pressed.axis, ev.position);
                        this.panes_mut().grab =
                            Some(Grab { sash: pressed.clone(), from, moved: 0.0 });
                        cx.notify();
                    }),
                )
                .into_any_element()
        })
        .collect();
    let washed = drop.and_then(|d| {
        let rect = frame.rect(d.pane)?;
        let w = wash(rect, d);
        let ink = hsla_alpha(theme.surfaces.accent_fill, alpha::FAINT);
        Some(
            div()
                .debug_selector(|| "drop-wash".to_owned())
                .absolute()
                .left(px(w.x))
                .top(px(w.y))
                .w(px(w.w))
                .h(px(w.h))
                .bg(ink)
                .into_any_element(),
        )
    });
    let release = |this: &mut V, cx: &mut Context<V>| {
        if this.panes_mut().grab.take().is_some() {
            this.sash_released(cx);
            cx.notify();
        }
    };
    div()
        .id("panes")
        .debug_selector(|| "panes".to_owned())
        .relative()
        .size_full()
        .overflow_hidden()
        .bg(hsla(theme.surfaces.ground))
        .when(dragging.is_some(), |el| {
            el.on_mouse_move(cx.listener(move |this: &mut V, ev: &MouseMoveEvent, _w, cx| {
                let Some(grab) = this.panes().grab.as_ref() else { return };
                if ev.pressed_button != Some(MouseButton::Left) {
                    release(this, cx);
                    return;
                }
                let sash = grab.sash.clone();
                let want = along(sash.axis, ev.position) - grab.from - grab.moved;
                let went = this.drag_sash(&sash, want, cx);
                if let Some(grab) = this.panes_mut().grab.as_mut() {
                    grab.moved += went;
                }
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this: &mut V, _ev: &MouseUpEvent, _w, cx| release(this, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |this: &mut V, _ev: &MouseUpEvent, _w, cx| release(this, cx)),
            )
        })
        .children(panes)
        .children(sashes)
        .children(washed)
}
