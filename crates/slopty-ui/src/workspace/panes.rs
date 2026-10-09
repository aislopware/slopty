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
//!
//! A pane new to the tab on show arrives from the edge it opened at: it fades in from the
//! ground as it travels the last [`ARRIVE_TRAVEL`] to its place, away from the sash it shares
//! with the pane it opened beside, on [`Pace::Pane`] (`MonoCode`'s 260 ms, eased out). Only its
//! place moves, by whole device pixels, never its size, and the fade is the ground laid over it
//! thinning out: its body is built once at the size it keeps and drawn again moved. The panes
//! of a tab shown anew, and every pane under Reduce Motion, are in place at once.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, Context, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Styled as _, Window, div, px,
};
use slopty_client::layout::Rect;
use slopty_client::layout::tiling::{Drop, TabId};
use slopty_client::layout::tree::{Laid, PaneId, Sash, Side, SplitAxis, TabFrame};
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::draw::Draw;
use crate::kit::{self, Pace};

/// What draws in the panes and moves their sashes.
pub(super) trait PaneHost: Sized + 'static {
    /// Its panes' pointer state.
    fn panes(&self) -> &Panes;

    /// Its panes' pointer state, to change.
    fn panes_mut(&mut self) -> &mut Panes;

    /// A sash was pressed, and a drag may follow.
    fn sash_pressed(&mut self, cx: &mut Context<Self>);

    /// Move `sash` by `delta` points (right or down), and draw again what that moved; how far
    /// it went.
    fn drag_sash(&mut self, sash: &Sash, delta: f32, cx: &mut Context<Self>) -> f32;

    /// The sash let go: what follows a pane's new size (a remote window, a display) may
    /// follow now.
    fn sash_released(&mut self, cx: &mut Context<Self>);

    /// Make the shares of the split at `path` equal.
    fn equalize(&mut self, path: &[usize], cx: &mut Context<Self>);
}

/// How far a new pane travels to its place as it fades in, in points: enough to say which
/// edge it came from, little enough that what it shows is read where it lands.
pub(super) const ARRIVE_TRAVEL: f32 = 24.0;

/// The panes' pointer state, the sash being dragged, and the panes arriving.
#[derive(Debug, Default)]
pub(super) struct Panes {
    grab: Option<Grab>,
    /// Read and written while the panes are built, which reads its host and never writes it.
    arrivals: RefCell<Arrivals>,
}

/// The panes of the tab on show the last frame drew, and those still arriving: when each came
/// and the way it travels in from (its offset's sign on each axis).
#[derive(Debug, Default)]
struct Arrivals {
    tab: Option<TabId>,
    known: HashSet<PaneId>,
    moving: HashMap<PaneId, (Instant, (f32, f32))>,
}

/// The tab on show and the instant its frame stands for, so new panes arrive; `moves` off (a
/// test's frame, Reduce Motion) puts them in place at once.
#[derive(Clone, Copy, Debug)]
pub(super) struct Arrive {
    pub tab: TabId,
    pub now: Instant,
    pub moves: bool,
}

impl Arrivals {
    /// Where each pane of `frame` stands at `arrive.now` as an offset and an opacity, for those
    /// still on their way; whether any is.
    fn step(&mut self, frame: &TabFrame, arrive: Arrive) -> HashMap<PaneId, ((f32, f32), f32)> {
        if self.tab != Some(arrive.tab) {
            self.tab = Some(arrive.tab);
            self.known.clear();
            self.moving.clear();
            self.known.extend(frame.panes.iter().map(|l| l.pane));
            return HashMap::new();
        }
        for laid in &frame.panes {
            if self.known.insert(laid.pane) && arrive.moves {
                self.moving.insert(laid.pane, (arrive.now, from_edge(laid, frame)));
            }
        }
        let length = Pace::Pane.duration();
        let curve = Pace::Pane.curve();
        self.moving.retain(|pane, (start, _)| {
            frame.rect(*pane).is_some()
                && arrive.now.saturating_duration_since(*start) < length
                && arrive.moves
        });
        self.moving
            .iter()
            .map(|(pane, (start, (dx, dy)))| {
                let gone = arrive.now.saturating_duration_since(*start);
                let t = curve.at(fraction(gone, length));
                let left = (1.0 - t) * ARRIVE_TRAVEL;
                (*pane, ((dx * left, dy * left), t))
            })
            .collect()
    }
}

/// How far `gone` is through `length`, from 0 to 1.
fn fraction(gone: Duration, length: Duration) -> f32 {
    if length.is_zero() { 1.0 } else { (gone.as_secs_f32() / length.as_secs_f32()).min(1.0) }
}

/// The way `laid` travels in from: away from the sash it shares with the pane it opened beside,
/// its leading one first (a pane opened to the right of another comes in from the right). A
/// pane with no sash (alone in its tab) only fades.
fn from_edge(laid: &Laid, frame: &TabFrame) -> (f32, f32) {
    let r = laid.rect;
    let near = |a: f32, b: f32| (a - b).abs() < 0.5;
    let spans = |s: &Sash| match s.axis {
        SplitAxis::Row => s.line.y < r.y + r.h && r.y < s.line.y + s.line.h,
        SplitAxis::Column => s.line.x < r.x + r.w && r.x < s.line.x + s.line.w,
    };
    let edges = frame.sashes.iter().filter(|s| spans(s)).filter_map(|s| match s.axis {
        SplitAxis::Row if near(s.line.x, r.x) => Some((0, (1.0, 0.0))),
        SplitAxis::Column if near(s.line.y, r.y) => Some((0, (0.0, 1.0))),
        SplitAxis::Row if near(s.line.x, r.x + r.w) => Some((1, (-1.0, 0.0))),
        SplitAxis::Column if near(s.line.y, r.y + r.h) => Some((1, (0.0, -1.0))),
        _ => None,
    });
    edges.min_by_key(|(rank, _)| *rank).map_or((0.0, 0.0), |(_, way)| way)
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

/// The wash over where a drop would land: a pane here, a project's row in the navigator.
pub(super) fn drop_ink(theme: &Theme) -> gpui::Hsla {
    hsla_alpha(theme.surfaces.accent_fill, alpha::FAINT)
}

/// What a tab's panes are drawn from: where the tiling lays them out, where a dragged tile
/// would land, and the clock new panes arrive on.
pub(super) struct Shown<'a> {
    pub frame: &'a TabFrame,
    pub drop: Option<Drop>,
    pub arrive: Option<Arrive>,
}

/// The panes of `shown.frame`, each drawn by `body` at its rectangle in the layer's own
/// coordinates, the sashes over their edges, and `shown.drop`'s wash over the pane it lands in.
/// Built from `host` as it is, read and never written ([`Draw`]). With `shown.arrive`, a pane
/// new to its tab travels in from its edge, and the frame asks for the next while one is on its
/// way.
pub(super) fn render<V: PaneHost>(
    host: &V,
    theme: &Theme,
    shown: &Shown<'_>,
    mut body: impl FnMut(&Laid) -> gpui::AnyElement,
    window: &Window,
    cx: &Draw<'_, V>,
) -> gpui::Stateful<gpui::Div> {
    let Shown { frame, drop, arrive } = *shown;
    let dragging = host.panes().dragging().cloned();
    let arriving = arrive
        .map(|arrive| host.panes().arrivals.borrow_mut().step(frame, arrive))
        .unwrap_or_default();
    if !arriving.is_empty() {
        window.request_animation_frame();
    }
    // A travel by whole device pixels, and the fade as the ground laid over the pane thinning
    // out: the body is drawn again from last frame moved (`gpui::fast::shift`), not built
    // again each step, which a fade of its own opacity or a part of a pixel would cost.
    let scale = window.scale_factor();
    let snap = |v: f32| (v * scale).round() / scale;
    let ground = theme.surfaces.ground;
    let panes: Vec<gpui::AnyElement> = frame
        .panes
        .iter()
        .map(|laid| {
            let id = laid.pane.get();
            let r = laid.rect;
            let ((dx, dy), shown) = arriving.get(&laid.pane).copied().unwrap_or(((0.0, 0.0), 1.0));
            // The ground's share of the pane still covered: the fade's own step, not a token.
            let covered = 1.0 - shown;
            let cover =
                (covered > 0.0).then(|| div().absolute().inset_0().bg(hsla_alpha(ground, covered)));
            kit::pane_surface(theme)
                .id(("pane", id))
                .debug_selector(move || format!("pane-{id}"))
                .absolute()
                .left(px(r.x + snap(dx)))
                .top(px(r.y + snap(dy)))
                .w(px(r.w))
                .h(px(r.h))
                .child(body(laid))
                .children(cover)
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
                        this.sash_pressed(cx);
                        cx.notify();
                    }),
                )
                .into_any_element()
        })
        .collect();
    let washed = drop.and_then(|d| {
        let rect = frame.rect(d.pane)?;
        let w = wash(rect, d);
        let ink = drop_ink(theme);
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
