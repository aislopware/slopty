//! The navigator: what is there and where it runs, down the left of the strip.
//!
//! Three sections, in this order. *Needs you* appears only while an agent waits on the human.
//! *Workers* has one row per worker, disclosing its tiles. A worker whose link is up shows only
//! its round trip; one that is not shows a mark and a word for what is wrong. Its tiles come
//! in order of attention: what needs the human, then what finished unseen, then what is
//! working, then the rest, each class in reading order. *Workspaces* names each workspace with
//! its count of tiles. A row flies the camera to what it names. It never brings back a host
//! switcher: every worker's tiles stay in the one workspace, and the navigator only lists them.
//!
//! On a window wide enough it docks beside the strip, 248 pt by default, dragged from 200 to
//! 400 by the handle on its right edge; ⌘B shows or hides it, and both are kept with the
//! device's layout. On an iPad it opens over the strip, and on a phone it slides in as a
//! drawer over a scrim; either closes once a row is chosen.

use std::collections::HashSet;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, px,
};
use slopty_client::layout::{Navigator, TileRef, WorkerKey};
use slopty_proto::agent::AgentStatus;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::{Theme, alpha};

use super::actions::ToggleNavigator;
use super::agents::{Waiting, agent_status_text};
use super::tile::kind_icon;
use super::{WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize, Status, icon, status_mark};

/// A row's height, in points.
pub(super) const ROW_H: f32 = 28.0;

/// The handle's width, along the inside of the navigator's right edge.
const HANDLE_W: f32 = 6.0;

/// How the navigator sits in the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// Beside the strip, which narrows to make room.
    Docked,
    /// Over the strip, which keeps its width: an iPad, or a window too narrow to give the
    /// room without turning the strip into a phone's.
    Overlay,
    /// Over the strip and a scrim, from the left edge: a phone.
    Drawer,
}

/// How the navigator sits in a window `window_w` wide, `width` its own width. A window that
/// is a phone's gets the drawer; a touch screen (an iPad) or a window that would leave the
/// strip a phone's width gets the overlay; anything else docks it.
pub(super) fn mode(window_w: f32, width: f32, phone_below: f32, touch: bool) -> Mode {
    if window_w < phone_below {
        Mode::Drawer
    } else if touch || window_w - width < phone_below {
        Mode::Overlay
    } else {
        Mode::Docked
    }
}

/// What the navigator keeps for this run only; whether it is docked and its width are the
/// layout's, and saved with it.
#[derive(Debug, Default)]
pub(super) struct NavState {
    /// Open over the strip, where it does not dock.
    pub open: bool,
    /// Workers whose tiles are folded away.
    pub folded: HashSet<WorkerKey>,
    /// The handle is being dragged: where the pointer and the width were when it was pressed.
    pub resize: Option<(f32, f32)>,
    /// How it sat in the last frame drawn, and whether it showed.
    pub drawn: Option<Mode>,
}

/// A round trip as the navigator and the status bar print it: tenths under 10 ms, whole
/// milliseconds above, so a jittery link does not repaint the chrome on every sample.
pub(super) fn rtt_label(rtt: std::time::Duration) -> String {
    let ms = rtt.as_secs_f64() * 1e3;
    if ms < 10.0 { format!("{ms:.1} ms") } else { format!("{ms:.0} ms") }
}

/// A worker's link where it is not simply up: a mark and a short word. A link that is up
/// shows neither, so a list of workers that are fine is a list of names; the word says what
/// is wrong only when something is.
pub(super) const fn worker_health(status: &WorkerStatus) -> Option<(Status, &'static str)> {
    match status {
        WorkerStatus::Connected => None,
        WorkerStatus::Connecting => Some((Status::Working, "connecting")),
        WorkerStatus::Silent(_) => Some((Status::Away, "silent")),
        WorkerStatus::Reconnecting(_) => Some((Status::Away, "reconnecting")),
        WorkerStatus::Unreachable => Some((Status::Away, "unreachable")),
        WorkerStatus::Gone => Some((Status::Away, "gone")),
    }
}

/// Where a tile's row stands among its worker's: what needs the human first, then what
/// finished or has news not yet seen, then what is working, then the rest.
pub(super) const fn attention(status: Option<Status>, unseen: bool) -> u8 {
    match status {
        Some(Status::NeedsYou) => 0,
        _ if unseen => 1,
        Some(Status::Done | Status::Failed) => 1,
        Some(Status::Working) => 2,
        Some(Status::Idle | Status::Away) | None => 3,
    }
}

/// The unseen dot on the edge of a row's status lane: something ended there while the human
/// was elsewhere. `ring` is the row's own surface, so the dot stands clear of the mark.
pub(super) fn unseen_dot(theme: &Theme, selector: String, ring: slopty_theme::Rgb) -> Div {
    let s = &theme.surfaces;
    div()
        .debug_selector(move || selector)
        .absolute()
        .top_0()
        .right_0()
        .size(px(theme.spacing.xs + theme.spacing.xxs))
        .rounded_full()
        .border_1()
        .border_color(hsla(ring))
        .bg(hsla(s.accent))
}

/// One row of a list in the frame: the navigator's and the inbox's. A tab stop named `label`;
/// the pointer washes it `raised`, and the selected row sits on `overlay`.
pub(super) fn row(
    theme: &Theme,
    id: impl Into<ElementId>,
    selector: String,
    label: SharedString,
    selected: bool,
) -> Stateful<Div> {
    let s = theme.surfaces;
    let spacing = theme.spacing;
    let el = div()
        .id(id)
        .debug_selector(move || selector)
        .role(Role::Button)
        .aria_label(label)
        .flex_none()
        .h(px(ROW_H))
        .mx(px(spacing.xs))
        .px(px(spacing.xs))
        .flex()
        .items_center()
        .gap(px(spacing.xs))
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .text_size(px(theme.typography.ui_size))
        .when(selected, |el| el.bg(hsla(s.overlay)))
        .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.raised))));
    tab_stop(el, s.accent)
}

/// A row's title, cut with an ellipsis.
pub(super) fn title(text: impl Into<SharedString>, color: gpui::Hsla) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(color)
        .child(text.into())
}

/// Muted caption type on the right of a row.
pub(super) fn caption(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .text_size(px(theme.typography.caption()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text.into())
}

/// A section's heading: small muted text.
pub(super) fn heading(theme: &Theme, selector: &'static str, text: &'static str) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex_none()
        .px(px(theme.spacing.md))
        .pt(px(theme.spacing.md))
        .pb(px(theme.spacing.xs))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text)
}

impl WorkspaceView {
    /// ⌘B: dock or undock the navigator where it docks (kept with the layout), else open or
    /// close it over the strip.
    pub fn toggle_navigator(
        &mut self,
        _: &ToggleNavigator,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.navigator_mode(window) {
            Mode::Docked => {
                let nav = self.layout.navigator();
                self.layout.set_navigator(Navigator { shown: !nav.shown, ..nav });
                self.nav.open = false;
                self.layout_touched(cx);
            }
            Mode::Overlay | Mode::Drawer => self.nav.open = !self.nav.open,
        }
        cx.notify();
    }

    /// How the navigator sits in `window`.
    pub(super) fn navigator_mode(&self, window: &Window) -> Mode {
        let window_w = f32::from(window.viewport_size().width);
        let width = self.layout.navigator().width;
        mode(window_w, width, self.layout.config().phone_below, cfg!(target_os = "ios"))
    }

    /// Whether the navigator is drawn in `mode`.
    pub(super) fn navigator_visible(&self, mode: Mode) -> bool {
        !self.workers.is_empty()
            && match mode {
                Mode::Docked => self.layout.navigator().shown,
                Mode::Overlay | Mode::Drawer => self.nav.open,
            }
    }

    /// The navigator's width, in points.
    #[must_use]
    pub const fn navigator_width(&self) -> f32 {
        self.layout.navigator().width
    }

    /// A row was chosen: the inbox closes, and over the strip the navigator gets out of the
    /// way of what it chose.
    fn navigated(&mut self) {
        self.menu = None;
        self.nav.open = false;
        if self.layout.overview_open() {
            self.layout.set_overview(false);
        }
    }

    /// Fly to `tile` and focus it.
    pub(super) fn go_to_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.focus_tile(tile, cx);
    }

    /// Go to an agent waiting on the human: its tile, or, with none here, a new one on its
    /// worker (the same as ⌘⇧A does for it).
    pub(super) fn go_to_waiting(&mut self, waiting: Waiting, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        if waiting.tile.is_some() {
            self.reveal_session(waiting.session, cx);
            return;
        }
        self.show_untiled(waiting.worker, waiting.session, cx);
    }

    /// Switch to workspace `ix`.
    fn go_to_workspace(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tick();
        self.navigated();
        self.layout.focus_workspace(ix);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// The handle moved to `x` (window points): the width follows, within its clamps.
    fn resize_navigator_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some((grab, from)) = self.nav.resize else { return };
        let nav = self.layout.navigator();
        let width = Navigator::clamp_width(from + x - grab);
        if (width - nav.width).abs() > f32::EPSILON {
            self.layout.set_navigator(Navigator { width, ..nav });
            cx.notify();
        }
    }

    fn end_navigator_resize(&mut self, cx: &mut Context<Self>) {
        if self.nav.resize.take().is_some() {
            self.layout_touched(cx);
            cx.notify();
        }
    }

    /// Whether a coding agent runs in `item`'s terminal.
    pub(super) fn runs_agent(&self, item: &Item) -> bool {
        let ItemKind::Terminal { session } = item.kind else { return false };
        self.agent_state(session).is_some_and(|a| a.status != AgentStatus::None)
    }

    /// The navigator's panel as `mode` places it; `None` while it is hidden.
    pub(super) fn render_navigator(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<(Mode, gpui::AnyElement)> {
        let mode = self.navigator_mode(window);
        let visible = self.navigator_visible(mode);
        self.nav.drawn = visible.then_some(mode);
        if !visible {
            self.nav.resize = None;
            return None;
        }
        Some((mode, self.navigator_panel(mode, window, cx)))
    }

    /// The panel itself, docked or laid over the strip.
    fn navigator_panel(&self, mode: Mode, window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let safe = window.insets().effective();
        let width = match mode {
            Mode::Drawer => {
                let room = theme.spacing.xl.mul_add(-2.0, f32::from(window.viewport_size().width));
                self.navigator_width().min(room)
            }
            Mode::Docked | Mode::Overlay => self.navigator_width(),
        };
        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        let waiting = self.needs_you();
        if !waiting.is_empty() {
            sections.push(heading(theme, "nav-needs-you", "Needs you").into_any_element());
            sections.extend(waiting.into_iter().map(|w| self.waiting_row("nav", w, cx)));
        }
        sections.push(heading(theme, "nav-workers", "Workers").into_any_element());
        let order = self.reading_order();
        for key in self.workers.keys() {
            sections.extend(self.worker_rows(*key, &order, cx));
        }
        sections.push(heading(theme, "nav-workspaces", "Workspaces").into_any_element());
        sections.extend(self.workspace_rows(cx));
        let list = div()
            .id("navigator-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(px(theme.spacing.md))
            .children(sections);
        let handle = (mode != Mode::Drawer).then(|| Self::render_handle(cx));
        let panel = div()
            .id("navigator")
            .debug_selector(|| "navigator".to_owned())
            .role(Role::Navigation)
            .aria_label("Navigator")
            .occlude()
            .relative()
            .flex_none()
            .h_full()
            .w(px(width) + if mode == Mode::Docked { px(0.0) } else { safe.left })
            .pl(safe.left)
            .flex()
            .flex_col()
            .bg(hsla(s.panel))
            .border_r_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            .when(mode != Mode::Docked, gpui::Styled::shadow_sm)
            .child(list)
            .children(handle);
        match mode {
            Mode::Docked => panel.into_any_element(),
            Mode::Overlay | Mode::Drawer => {
                let scrim = if mode == Mode::Drawer {
                    hsla_alpha(s.canvas, alpha::SCRIM)
                } else {
                    gpui::transparent_black()
                };
                div()
                    .id("navigator-away")
                    .debug_selector(|| "navigator-away".to_owned())
                    .absolute()
                    .inset_0()
                    .bg(scrim)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _w, cx| {
                            this.nav.open = false;
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                            .child(panel),
                    )
                    .into_any_element()
            }
        }
    }

    /// The strip of the right edge the pointer drags, and the listeners that follow the
    /// pointer wherever it goes once the handle is pressed. They are there in every frame the
    /// handle is, so the first move after the press is followed without waiting for a frame.
    fn render_handle(cx: &Context<Self>) -> gpui::AnyElement {
        let entity = cx.entity().downgrade();
        let follow = canvas(
            |_bounds, _window, _cx| (),
            move |_bounds, (), window, _cx| {
                let moved = entity.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _window, cx| {
                    let resizing =
                        moved.read_with(cx, |this, _| this.nav.resize.is_some()).unwrap_or(false);
                    if phase != gpui::DispatchPhase::Capture || !resizing {
                        return;
                    }
                    let _gone = moved.update(cx, |this, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            this.resize_navigator_to(f32::from(ev.position.x), cx);
                        } else {
                            this.end_navigator_resize(cx);
                        }
                    });
                });
                let released = entity;
                window.on_mouse_event(move |_ev: &MouseUpEvent, phase, _window, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        let _gone = released.update(cx, Self::end_navigator_resize);
                    }
                });
            },
        )
        .absolute()
        .size_0();
        div()
            .id("navigator-handle")
            .debug_selector(|| "navigator-handle".to_owned())
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(HANDLE_W))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &gpui::MouseDownEvent, _w, cx| {
                    this.nav.resize = Some((f32::from(ev.position.x), this.navigator_width()));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(follow)
            .into_any_element()
    }

    /// A row of *Needs you*: the mark, what waits, and on which worker.
    pub(super) fn waiting_row(
        &self,
        prefix: &str,
        waiting: Waiting,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let what = waiting
            .tile
            .and_then(|t| Some(self.card_title(t, self.item(t)?, cx)))
            .or_else(|| self.agent_state(waiting.session).map(agent_status_text))
            .unwrap_or_else(|| "Agent".to_owned());
        let worker = self.workers.get(&waiting.worker).map(|w| w.name.clone()).unwrap_or_default();
        let session = waiting.session;
        let label = SharedString::from(format!("{what}, needs you on {worker}"));
        row(
            theme,
            ElementId::Name(format!("{prefix}-waiting-{session}").into()),
            format!("{prefix}-waiting-{session}"),
            label,
            false,
        )
        .child(status_mark(theme, Some(Status::NeedsYou), 1.0))
        .child(title(what, hsla(s.text)))
        .child(caption(theme, worker))
        .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_waiting(waiting, cx)))
        .into_any_element()
    }

    /// A worker's row, and its tiles' rows beneath it unless it is folded. `order` is every
    /// tile in reading order.
    fn worker_rows(
        &self,
        key: WorkerKey,
        order: &[TileRef],
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let Some(w) = self.workers.get(&key) else { return Vec::new() };
        let folded = self.nav.folded.contains(&key);
        let health = worker_health(&w.status);
        let rtt = w.rtt.filter(|_| health.is_none()).map(rtt_label);
        let label = SharedString::from(format!(
            "{}{}{}",
            w.name,
            health.map(|(_, word)| format!(", {word}")).unwrap_or_default(),
            if folded { ", folded" } else { "" }
        ));
        let chevron = if folded { IconName::ChevronRight } else { IconName::ChevronDown };
        let header = row(
            theme,
            ElementId::Name(format!("nav-worker-{key}").into()),
            format!("nav-worker-{key}"),
            label,
            false,
        )
        .child(icon(theme, chevron, IconSize::Inline, hsla(s.text_muted)))
        .child(title(w.name.clone(), hsla(s.text)))
        .children(health.map(|(_, word)| caption(theme, word)))
        .children(rtt.map(|rtt| caption(theme, rtt)))
        .child(status_mark(theme, health.map(|(mark, _)| mark), 1.0))
        .on_click(cx.listener(move |this, _ev, _w, cx| {
            if !this.nav.folded.remove(&key) {
                this.nav.folded.insert(key);
            }
            cx.notify();
        }))
        .into_any_element();
        let mut rows = vec![header];
        if folded {
            return rows;
        }
        let focused = self.focused();
        let mut tiles: Vec<(u8, TileRef, &Item, Option<Status>, bool)> = order
            .iter()
            .filter(|t| t.worker == key)
            .filter_map(|&tile| {
                let item = w.doc.get(tile.item)?;
                let mark = self.tile_status(tile, item, cx);
                let unwatched = match item.kind {
                    ItemKind::Terminal { session } => self.finished.contains_key(&session),
                    _ => false,
                };
                let unseen = unwatched && !matches!(mark, Some(Status::Working | Status::NeedsYou));
                Some((attention(mark, unseen), tile, item, mark, unseen))
            })
            .collect();
        // Stable: within a class the tiles keep their reading order.
        tiles.sort_by_key(|(class, ..)| *class);
        for (_, tile, item, mark, unseen) in tiles {
            let name = self.card_title(tile, item, cx);
            let selected = focused == Some(tile);
            let label = [Some(name.as_str()), mark.map(Status::label), unseen.then_some("unseen")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
            let kind = kind_icon(item, self.runs_agent(item));
            let ink = if selected { s.text } else { s.text_secondary };
            let id = tile.item.as_uuid();
            let ring = if selected { s.overlay } else { s.panel };
            let lane = status_mark(theme, mark, 1.0)
                .relative()
                .children(unseen.then(|| unseen_dot(theme, format!("nav-unseen-{id}"), ring)));
            rows.push(
                row(
                    theme,
                    ElementId::Name(format!("nav-tile-{id}").into()),
                    format!("nav-tile-{id}"),
                    label.into(),
                    selected,
                )
                .pl(px(theme.spacing.xl))
                .child(icon(theme, kind, IconSize::Inline, hsla(s.text_muted)))
                .child(title(name, hsla(ink)))
                .child(lane)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_tile(tile, cx)))
                .into_any_element(),
            );
        }
        rows
    }

    /// A row per workspace with something in it or a name, with its count of tiles.
    fn workspace_rows(&self, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let active = self.layout.active_workspace();
        self.layout
            .workspaces()
            .iter()
            .enumerate()
            .filter(|(_, ws)| !ws.columns().is_empty() || ws.name().is_some())
            .map(|(ix, ws)| {
                let name = self.workspace_name_at(ix);
                let count: usize = ws.columns().iter().map(|c| c.tiles().len()).sum();
                let selected = ix == active;
                let ink = if selected { s.text } else { s.text_secondary };
                let noun = if count == 1 { "tile" } else { "tiles" };
                row(
                    theme,
                    ("nav-workspace", ix),
                    format!("nav-workspace-{ix}"),
                    format!("{name}, {count} {noun}").into(),
                    selected,
                )
                .child(icon(theme, IconName::LayoutGrid, IconSize::Inline, hsla(s.text_muted)))
                .child(title(name, hsla(ink)))
                .child(caption(theme, count.to_string()))
                .on_click(cx.listener(move |this, _ev, _w, cx| this.go_to_workspace(ix, cx)))
                .into_any_element()
            })
            .collect()
    }
}
