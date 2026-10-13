//! The readouts: what the app has to say for itself, each only while it has something to say,
//! in the window's cluster ([`super::cluster`]).
//!
//! The server while it does not answer; the focused machine's link on a DERP relay that has
//! held, with its fix; a newer Slopty while one is out (which opens its release page when
//! clicked); the plan's windows the focused machine's agents last published once one is 80 %
//! used (`5h 83% · 7d 41%`, in `warn`, which list every machine's when clicked); and the
//! transfers in flight both ways while one is not the focused tile's own upload (whose header
//! says it), as a glyph and a count, which list every transfer with its rate, time left and
//! stop when clicked. Their popovers drop from the cluster. The ports forwarded here are the
//! palette's "Forwarded ports"; the frame time is the stream stats' overlay's (⌘⇧I).
//!
//! Nothing here repeats what is said elsewhere: which machine a tile runs on is the navigator's
//! and the cluster's project button's, a link's round trip its machine row's, a file's language
//! and caret its tab bar's, a stream's size and rate its stats overlay's.
//!
//! Each readout is meta text, its figures tabular; a state is a small dot of its fill beside
//! quiet words. A phone's bars have no room for them.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_client::update::Release;
use slopty_kit::a11y::tab_stop;
use slopty_kit::colors::hsla;
use slopty_kit::draw::Draw;
use slopty_kit::icons::{Symbol, icon};
use slopty_kit::kit::{self, meta, separator, tabular};
use slopty_platform::notify::Alerts;
use slopty_theme::Theme;

use super::WorkspaceView;
use super::rollup::META_SEPARATOR;
use crate::palette::section_heading;

/// What the server being down costs, said under the pointer: workers are reached at the
/// addresses this client last had for them, and none it has not met.
const SERVER_DOWN_MEANS: &str = "Machines you reach directly still work";

/// The popovers' width, in points.
const POPOVER_W: f32 = 300.0;

/// How old a plan reading grows before the bar says its age: the Claude Code status line runs
/// only while a session is live, so a reading after a long idle is a past one.
const PLAN_AGED_FROM: Duration = Duration::from_mins(15);

/// A plan window used this far, in hundredths of a percent, is said in `warn`.
const PLAN_WARN_FROM_BP: u32 = 8_000;

/// The readouts' own state.
#[derive(Default)]
pub(super) struct Readouts {
    /// The plan windows' popover is up.
    plans_open: bool,
    /// The transfers' popover is up.
    transfers_open: bool,
    /// The popover just closed, drawn for the moment it takes to fade away.
    leaving: Option<Popover>,
    /// A newer Slopty that is out, as the app last found it.
    release: Option<Release>,
    /// Where the cluster was laid out in the window last frame, which the popovers drop from.
    at: Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
}

/// One of the readouts' popovers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Popover {
    Plans,
    Transfers,
}

impl Popover {
    /// The layer round it that a click closes it from, and its fades' names.
    const fn ids(self) -> (&'static str, &'static str) {
        match self {
            Self::Plans => ("plans-away", "plans-presence"),
            Self::Transfers => ("transfers-away", "transfers-presence"),
        }
    }
}

impl Readouts {
    /// Where the cluster's measure writes its bounds each frame.
    pub(super) fn at(&self) -> Rc<Cell<Option<gpui::Bounds<gpui::Pixels>>>> {
        Rc::clone(&self.at)
    }

    /// Whether `which` is up.
    const fn open(&self, which: Popover) -> bool {
        match which {
            Popover::Plans => self.plans_open,
            Popover::Transfers => self.transfers_open,
        }
    }

    const fn set_open(&mut self, which: Popover, open: bool) {
        match which {
            Popover::Plans => self.plans_open = open,
            Popover::Transfers => self.transfers_open = open,
        }
    }

    /// Whether `which` is drawn: up, or on its way out.
    pub(super) fn shown(&self, which: Popover) -> bool {
        self.open(which) || self.leaving == Some(which)
    }
}

impl std::fmt::Debug for Readouts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Readouts").field("release", &self.release).finish_non_exhaustive()
    }
}

/// Transfers in flight at a glance, `ups` up and `downs` down: `1 upload · 42%`,
/// `2 downloads · 10%`, `3 transfers · 40%`.
#[must_use]
fn transfers_label(ups: usize, downs: usize, done: u64, total: u64) -> String {
    let count = ups.saturating_add(downs);
    let (one, many) = match (ups, downs) {
        (_, 0) => ("upload", "uploads"),
        (0, _) => ("download", "downloads"),
        _ => ("transfer", "transfers"),
    };
    let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
    format!("{}{META_SEPARATOR}{percent}%", counted(count, one, many))
}

/// What the transfers popover is called.
const TRANSFERS: &str = "Transfers";

/// A count and its noun: `1 port`, `2 ports`.
#[must_use]
fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl WorkspaceView {
    /// The worker the readouts speak for: the focused tile's, else the one a new tile would
    /// go to.
    fn readout_worker(&self) -> Option<WorkerKey> {
        self.focused().map(|t| t.worker).or_else(|| self.context_worker())
    }

    /// Open `which`, or close it if it is up.
    fn toggle_popover(&mut self, which: Popover, cx: &mut Context<Self>) {
        if self.readouts.open(which) {
            self.close_popover(which, cx);
        } else {
            self.readouts.set_open(which, true);
            cx.notify();
        }
    }

    /// Close `which`, drawn fading out for the moment that takes.
    fn close_popover(&mut self, which: Popover, cx: &mut Context<Self>) {
        if !self.readouts.open(which) {
            return;
        }
        self.readouts.set_open(which, false);
        if self.chrome_moves(cx) {
            self.keep_leaving(
                which,
                kit::Pace::Exit.duration(),
                |this| &mut this.readouts.leaving,
                cx,
            );
        }
        cx.notify();
    }

    /// The readouts that warn and have something to say, for the cluster; nothing while none
    /// has.
    pub(super) fn render_readouts(
        &self,
        _window: &Window,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        if self.workers.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;

        // A state is a dot of its fill beside its words. Out of reach is muted, since `warn`
        // means "needs you" alone; what it costs is said under the pointer. Pressed, it offers
        // what can be done: try the server now, or connect to another.
        let server = self.server_status.clone().map(|text| {
            let text = SharedString::from(sentence(&text));
            let hint_theme = Rc::new(theme.clone());
            let at = Rc::clone(&self.anchors.at);
            let measure = gpui::canvas(
                move |bounds, _window, _cx| {
                    at.borrow_mut().insert(super::titlebar::MenuKind::Server, bounds);
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0();
            let el = button("readout-server", text.clone(), theme)
                .relative()
                .aria_description(SERVER_DOWN_MEANS)
                .aria_expanded(self.menu == Some(super::titlebar::MenuKind::Server))
                .text_color(hsla(s.text_secondary))
                .child(measure)
                .child(icon(theme, Symbol::WifiSlash, hsla(s.text_muted)))
                .child(text)
                .map(kit::hint_timing)
                .tooltip(move |_window, cx| {
                    let theme = Rc::clone(&hint_theme);
                    cx.new(|_| kit::Hint::new(SERVER_DOWN_MEANS, "", theme)).into()
                });
            tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, window, cx| {
                this.toggle_menu(super::titlebar::MenuKind::Server, window, cx);
            }))
        });
        // The focused machine's link on a DERP relay that has held: in words, quiet, with its
        // fix under the pointer. The navigator's row names the relay; this says what to do.
        let relay = self
            .readout_worker()
            .filter(|key| self.workers.get(key).is_some_and(|w| w.status.is_up()))
            .and_then(|key| self.relay_notice(key, cx))
            .map(|notice| {
                let text = SharedString::from(notice.note);
                let fix = notice.fix;
                let hint_theme = Rc::new(theme.clone());
                readout("readout-relay", text.clone())
                    .aria_description(fix)
                    .text_color(hsla(s.text_muted))
                    .child(text)
                    .map(kit::hint_timing)
                    .tooltip(move |_window, cx| {
                        let theme = Rc::clone(&hint_theme);
                        cx.new(|_| kit::Hint::new(fix, "", theme)).into()
                    })
            });
        let release = self.readouts.release.as_ref().map(|release| {
            let text = SharedString::from(release_line(release));
            let page = release.page.clone();
            let el = button("readout-release", text.clone(), theme).child(text);
            tab_stop(el, s.focus).on_click(move |_ev, _w, cx| cx.open_url(&page))
        });
        let parts: Vec<gpui::AnyElement> = [
            server.map(gpui::IntoElement::into_any_element),
            relay.map(gpui::IntoElement::into_any_element),
            release.map(gpui::IntoElement::into_any_element),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            return None;
        }
        let row = div()
            .id("readouts")
            .debug_selector(|| "readouts".to_owned())
            .role(Role::Group)
            .aria_label("Status")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .font_family(theme.typography.ui_family.clone())
            .children(parts);
        Some(meta(row, theme).into_any_element())
    }

    /// The plan's windows on the focused tile's machine, as its agent last published them (else
    /// the freshest reading there), in the cluster once one is 80 % used: `5h 83% · 7d 41%` in
    /// `warn`, with when a spent window comes back, and its age once it is past
    /// [`PLAN_AGED_FROM`]. Below that it says nothing. Clicked, every machine's readings.
    pub(super) fn plan_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = self.focused();
        let worker = tile.map(|t| t.worker).or_else(|| self.readout_worker())?;
        let agent = tile.and_then(|t| self.item(t)).and_then(|i| self.item_agent(i));
        let now = slopty_kit::clock::now(cx);
        let (_, reading) = self.faces.threads.meters().shown(worker, agent, now)?;
        let (text, warn) = plan_words(reading, now);
        if !warn && !self.readouts.plans_open {
            return None;
        }
        let label = SharedString::from(format!("{PLAN_USAGE} {text}"));
        let words = spaced(tabular(div().id("readout-plan-words")), &text, theme);
        let el = button("readout-plan", label, theme)
            .when(warn, |el| el.text_color(hsla(s.warn)))
            .child(words)
            .when(self.readouts.plans_open, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        Some(tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, _window, cx| {
            this.toggle_popover(Popover::Plans, cx);
        })))
    }

    /// Every transfer in flight at a glance, both ways, while one is not the focused tile's own
    /// upload (its header says that one, with its stop): the transfers' glyph and how many, the
    /// way and how far said to a screen reader. Clicked, the list of them.
    pub(super) fn transfers_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let rows = self.transfer_rows(cx.background_executor().now());
        let focused = self.focused();
        let own = self.uploads.values().filter(|u| Some(u.tile) == focused && u.listed()).count();
        if rows.len() <= own {
            return None;
        }
        let ups = rows.iter().filter(|r| r.up).count();
        let (done, total) = rows.iter().fold((0_u64, 0_u64), |(d, t), r| {
            (d.saturating_add(r.done), t.saturating_add(r.total))
        });
        let text = transfers_label(ups, rows.len().saturating_sub(ups), done, total);
        let count = SharedString::from(rows.len().to_string());
        let el = tabular(button("readout-transfers", text.into(), theme))
            .gap(px(theme.spacing.xxs))
            .child(icon(theme, Symbol::ArrowUpAndDown, hsla(s.icon_muted)))
            .child(count)
            .when(self.readouts.transfers_open, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        Some(tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, _window, cx| {
            this.toggle_popover(Popover::Transfers, cx);
        })))
    }

    /// Close the transfers popover: nothing is left in it.
    pub(in crate::workspace) const fn close_transfers(&mut self) {
        self.readouts.transfers_open = false;
    }

    /// Every transfer in flight, both ways: what goes, to or from which machine, how far, how
    /// fast and how long it has left, with its stop under the pointer or the keyboard. A click
    /// anywhere else closes it.
    pub(super) fn render_transfers(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let drop = self.drop_at(window);
        let rows: Vec<gpui::AnyElement> = self
            .transfer_rows(cx.background_executor().now())
            .into_iter()
            .map(|row| self.transfer_row(row, cx))
            .collect();
        let panel = kit::elevate(div(), theme)
            .id("transfers")
            .debug_selector(|| "transfers".to_owned())
            .role(Role::Dialog)
            .aria_label(TRANSFERS)
            .occlude()
            .absolute()
            .top(drop.top)
            .right(drop.right)
            .w(px(POPOVER_W))
            .flex()
            .flex_col()
            .pb(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(section_heading(theme, "transfers-heading".into(), TRANSFERS, true))
            .children(rows);
        self.popover(Popover::Transfers, panel, window, cx)
    }

    /// One transfer in the popover: its way's mark, its name over its machine and progress, a
    /// hairline of how far it got, and its stop.
    fn transfer_row(
        &self,
        row: super::remote::transfers::TransferRow,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let xfer = row.xfer;
        let group = SharedString::from(format!("transfers-row-{xfer}"));
        let (mark, way) =
            if row.up { (Symbol::ArrowUpToLine, "to") } else { (Symbol::ArrowDownToLine, "from") };
        let detail = format!("{}{META_SEPARATOR}{}", row.machine, row.words);
        let label =
            SharedString::from(format!("{}, {way} {}, {}", row.name, row.machine, row.words));
        let stop_id = format!("transfers-cancel-{xfer}");
        let stop_selector = stop_id.clone();
        let stop = div()
            .id(ElementId::Name(stop_id.into()))
            .debug_selector(move || stop_selector)
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Cancel {}", row.name)))
            .flex_none()
            .px(px(spacing.xs))
            .rounded(px(theme.radii.xs))
            .cursor_pointer()
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .map(kit::eased)
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .child("Cancel");
        let stop = tab_stop(stop, s.focus)
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            // Its own focus, as the navigator's row actions: not any focused ancestor's.
            .focus(gpui::Styled::visible)
            .focus_visible(move |st| st.outline_ring(slopty_kit::a11y::ring(s.focus)).visible())
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.cancel_transfer(xfer, cx);
            }));
        // How far it got, under the words: the figure says it, the bar shows it.
        let progress = row.fraction.map(|fraction| {
            div().mt(px(spacing.xxs)).w_full().child(
                kit::progress::Bar::new(
                    theme,
                    format!("transfers-bar-{xfer}"),
                    kit::progress::Progress::Share(fraction),
                )
                .label(SharedString::from(format!("{} {way} {}", row.name, row.machine))),
            )
        });
        div()
            .id(ElementId::Name(format!("transfers-row-{xfer}").into()))
            .debug_selector(move || format!("transfers-row-{xfer}"))
            .group(group)
            .role(Role::Label)
            .aria_label(label)
            .flex_none()
            .h(px(kit::Row::Two.height(theme)))
            .mx(px(spacing.xs))
            .px(px(spacing.xs))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .rounded(px(theme.radii.sm))
            .map(kit::eased)
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(crate::palette::lead_slot(theme, mark, hsla(s.icon_muted)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(
                        div()
                            .truncate()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(row.name)),
                    )
                    .child(
                        meta(tabular(div()), theme)
                            .debug_selector(move || format!("transfers-words-{xfer}"))
                            .truncate()
                            .child(SharedString::from(detail)),
                    )
                    .children(progress),
            )
            .child(stop)
            .into_any_element()
    }

    /// Every plan reading the machines' agents published, by machine and agent, with its age.
    /// A click anywhere else closes it.
    pub(super) fn render_plans(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let drop = self.drop_at(window);
        let now = slopty_kit::clock::now(cx);
        let rows: Vec<gpui::AnyElement> = self
            .faces
            .threads
            .meters()
            .all(now)
            .map(|(worker, agent, reading)| {
                let agent_id = slopty_proto::thread::AgentId(agent.to_owned());
                let who = format!(
                    "{}{META_SEPARATOR}{}",
                    self.worker_name(worker),
                    crate::conversation::thread::view::agent_label(&agent_id)
                );
                let (words, warn) = plan_words(reading, now);
                let age = crate::palette::age_label(reading.age(now));
                let words = format!("{words}{META_SEPARATOR}{age}");
                let selector = format!("plans-row-{worker}-{agent}");
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .role(Role::Label)
                    .aria_label(SharedString::from(format!("{who}, {words}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(spacing.sm))
                    .mx(px(spacing.xs))
                    .px(px(spacing.xs))
                    .py(px(spacing.xs))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(who)),
                    )
                    .child(
                        meta(tabular(div()), theme)
                            .flex_none()
                            .when(warn, |el| el.text_color(hsla(s.warn)))
                            .child(SharedString::from(words)),
                    )
                    .into_any_element()
            })
            .collect();
        let panel = kit::elevate(div(), theme)
            .id("plans")
            .debug_selector(|| "plans".to_owned())
            .role(Role::Dialog)
            .aria_label(PLAN_USAGE)
            .occlude()
            .absolute()
            .top(drop.top)
            .right(drop.right)
            .w(px(POPOVER_W))
            .flex()
            .flex_col()
            .pb(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(section_heading(theme, "plans-heading".into(), PLAN_USAGE, true))
            .children(rows);
        self.popover(Popover::Plans, panel, window, cx)
    }

    /// Where a popover of the cluster drops from: a gap under the top line, its right edge on
    /// the cluster's.
    fn drop_at(&self, window: &Window) -> Hang {
        let theme = &self.theme;
        let viewport = window.viewport_size();
        let gap = px(theme.spacing.xs);
        let line = px(super::titlebar::titlebar_height(theme)) + window.insets().effective().top;
        let right = self.readouts.at.get().map_or(gap, |b| viewport.width - b.right() + gap);
        Hang { top: line + gap, right }
    }

    /// `panel` over the window as the cluster's popover `which`: a click anywhere else closes it,
    /// and it fades in. Once closed it fades out where it stands for [`kit::Pace::Exit`], the
    /// window taking the pointer back at once but where the panel still shows, and turns back
    /// if it is opened again on its way out ([`kit::Presence`]).
    fn popover(
        &self,
        which: Popover,
        panel: Stateful<Div>,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let (away, shown) = which.ids();
        let viewport = window.viewport_size();
        let layer = div().id(away).relative().w(viewport.width).h(viewport.height);
        let open = self.readouts.open(which);
        let (layer, panel) = if open {
            let layer = layer.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| this.close_popover(which, cx)),
            );
            (layer, panel)
        } else {
            let gone = format!("{}-leaving", away.trim_end_matches("-away"));
            (layer, panel.debug_selector(move || gone))
        };
        let layer = layer.child(kit::presence(panel, shown, open));
        gpui::deferred(gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(layer))
            .with_priority(slopty_kit::layer::Layer::Popover.priority())
            .into_any_element()
    }
}

impl WorkspaceView {
    /// The server's line on a phone, whose bar has no room for the readouts: under the
    /// navigator's header while the server is out of reach, what that means, and its first
    /// door (try it now) on the line's end.
    pub(super) fn phone_server_line(&self) -> Option<gpui::AnyElement> {
        let status = self.server_status.as_ref()?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let text = SharedString::from(sentence(status));
        let retry = self.server_entries.first().map(|entry| {
            let run = Rc::clone(&entry.run);
            let el = button("nav-server-retry", entry.label.clone(), theme)
                .flex_none()
                .text_color(hsla(s.accent))
                .child(entry.label.clone());
            tab_stop(el, s.focus).on_click(move |_ev, window, cx| run(window, cx))
        });
        Some(
            div()
                .id("nav-server")
                .debug_selector(|| "nav-server".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .aria_description(SERVER_DOWN_MEANS)
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.inset()))
                .py(px(theme.spacing.xxs))
                .text_size(px(theme.typography.small()))
                .child(icon(theme, Symbol::WifiSlash, hsla(s.text_muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_color(hsla(s.text_secondary)).child(text))
                        .child(div().text_color(hsla(s.text_muted)).child(SERVER_DOWN_MEANS)),
                )
                .children(retry)
                .into_any_element(),
        )
    }
}

impl WorkspaceView {
    /// This device's notes in its navigator, under the server's line, while they do not reach
    /// the person: never asked for, the line asks ([`NOTES_ASK`]); turned off, it says so and
    /// opens the system's settings. Allowed here while the server says it cannot push to this
    /// phone (no APNs key named there, or its token gone), it says the notes reach nobody,
    /// with no door: the fix is on the server's machine. The app sets it only where pushes are
    /// the way an agent reaches the person (a phone, an iPad).
    pub(super) fn notes_line(&self) -> Option<gpui::AnyElement> {
        let (alerts, door) = self.notes_door.as_ref()?;
        let (glyph, text, action) = match alerts {
            Alerts::Unasked => (Symbol::Bell, NOTES_ASK, Some(NOTES_ALLOW)),
            Alerts::Denied => (Symbol::BellSlash, NOTES_OFF_LINE, Some(NOTES_TURN_ON)),
            Alerts::Allowed if self.pushable == Some(false) => {
                (Symbol::BellSlash, NOTES_UNPUSHABLE, None)
            }
            Alerts::Allowed | Alerts::Unavailable => return None,
        };
        let theme = &self.theme;
        let s = &theme.surfaces;
        let door = action.map(|action| {
            let run = Rc::clone(door);
            let action = SharedString::from(action);
            let el = button("nav-notes-door", action.clone(), theme)
                .flex_none()
                .text_color(hsla(s.accent))
                .child(action);
            tab_stop(el, s.focus).on_click(move |_ev, window, cx| run(window, cx))
        });
        Some(
            div()
                .id("nav-notes")
                .debug_selector(|| "nav-notes".to_owned())
                .role(Role::Group)
                .aria_label(text)
                .aria_description(NOTES_MEANS)
                .flex_none()
                .w_full()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.inset()))
                .py(px(theme.spacing.xs))
                // A notice row, not stray words between the filter and the machines: the
                // element's ground with the warning's bar down its edge, 12/16.
                .bg(hsla(s.element))
                .border_l(px(slopty_theme::stroke::NOTICE))
                .border_color(hsla(s.warn_fill))
                .map(|el| kit::typed(el, theme.roles().metadata))
                .child(icon(theme, glyph, hsla(s.text_muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_color(hsla(s.text_secondary)).child(text))
                        .child(div().text_color(hsla(s.text_muted)).child(NOTES_MEANS)),
                )
                .children(door)
                .into_any_element(),
        )
    }
}

/// What a device's navigator says while its notes were never asked for.
pub(super) const NOTES_ASK: &str = "Notify me when an agent needs me";
/// Its door: the system's question.
pub(super) const NOTES_ALLOW: &str = "Allow";
/// What it says while the person turned them off.
pub(super) const NOTES_OFF_LINE: &str = "Notifications are off";
/// Its door: the system's settings, where they are turned on.
pub(super) const NOTES_TURN_ON: &str = "Turn on";
/// What it says while notes are allowed here and the server cannot push to this device: a phone
/// or an iPad alike.
pub(super) const NOTES_UNPUSHABLE: &str = "The server can't send notifications to this device";
/// What each costs, under it.
pub(super) const NOTES_MEANS: &str = "Nothing reaches you here while Slopty is away.";

/// Where a popover drops from the cluster, as offsets from the window's edges.
struct Hang {
    top: gpui::Pixels,
    right: gpui::Pixels,
}

/// `text` in sentence case: the app and the link words come lowercase.
pub(super) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// What the plan popover is called.
const PLAN_USAGE: &str = "Plan usage";

/// A reading's windows still holding at `now` as the bar says them, `5h 23% · 7d 41%`, a spent
/// one with when it comes back (`5h 100% until 14:00`), and the reading's age once it is past
/// [`PLAN_AGED_FROM`]; and whether any window is far enough used to say in `warn`.
fn plan_words(
    reading: &slopty_client::meters::Reading,
    now: slopty_core::WallMs,
) -> (String, bool) {
    let mut warn = false;
    let mut parts: Vec<String> = reading
        .current(now)
        .map(|limit| {
            warn |= limit.used_bp >= PLAN_WARN_FROM_BP;
            let pct = limit.used_bp.saturating_add(50) / 100;
            let name = slopty_client::meters::short_name(&limit.name);
            let until = limit
                .resets_ms
                .filter(|_| limit.used_bp >= 10_000)
                .and_then(crate::conversation::figures::clock)
                .map(|at| format!(" until {at}"))
                .unwrap_or_default();
            format!("{name} {pct}%{until}")
        })
        .collect();
    let age = reading.age(now);
    if age >= PLAN_AGED_FROM {
        parts.push(format!("{} ago", crate::palette::age_label(age)));
    }
    (parts.join(META_SEPARATOR), warn)
}

/// `el` holding `text`, its parts (joined by [`META_SEPARATOR`]) set apart by the faint dot.
fn spaced(el: Stateful<Div>, text: &str, theme: &Theme) -> Stateful<Div> {
    let mut el = el.flex().items_center().gap(px(theme.spacing.xs));
    for (i, part) in text.split(META_SEPARATOR).enumerate() {
        if i > 0 {
            el = el.child(separator(theme));
        }
        el = el.child(SharedString::from(part.to_owned()));
    }
    el
}

impl WorkspaceView {
    /// The newer Slopty that is out, or `None` once this build is the latest: the cluster says
    /// it quietly until then.
    pub fn set_release(&mut self, release: Option<Release>, cx: &mut Context<Self>) {
        if self.readouts.release != release {
            self.readouts.release = release;
            App::notify(cx, self.chrome.cluster.entity_id());
        }
    }
}

/// What the cluster says of a newer release: "Slopty 0.2.0 is out".
pub(super) fn release_line(release: &Release) -> String {
    format!("Slopty {} is out", release.version)
}

/// A readout: a status the screen reader reads as `text`, on one line.
fn readout(selector: &'static str, text: SharedString) -> Stateful<Div> {
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .role(Role::Status)
        .aria_label(text)
        .flex_none()
        .whitespace_nowrap()
}

/// A readout that does something when clicked, `MonoCode`'s footer chip: the density's chip
/// tall at the least radius, the strong hover's wash under the pointer.
pub(super) fn button(selector: &'static str, text: SharedString, theme: &Theme) -> Stateful<Div> {
    let s = theme.surfaces;
    readout(selector, text)
        .role(Role::Button)
        .h(px(theme.density.chip))
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs))
        .px(px(theme.spacing.xs + theme.spacing.xxs))
        .rounded(px(theme.radii.xs))
        .cursor_pointer()
        .map(kit::eased)
        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window used 80 % or more says the reading in `warn`; a spent one says when it comes
    /// back; a reading past a quarter of an hour says its age; a window past its reset is left.
    #[test]
    fn a_plan_window_far_used_warns_and_a_spent_one_says_when_it_comes_back() {
        use slopty_client::meters::Reading;
        use slopty_core::WallMs;
        use slopty_proto::thread::Limit;
        let limit = |name: &str, used_bp, resets: Option<u64>| Limit {
            name: name.to_owned(),
            used_bp,
            resets_ms: resets.map(WallMs::from_millis),
        };
        let now = WallMs::from_millis(1_000_000_000);
        let fresh = Reading { limits: vec![limit("five-hour", 2_349, None)], heard: now };
        assert_eq!(plan_words(&fresh, now), ("5h 23%".to_owned(), false));
        let far = Reading { limits: vec![limit("seven-day", 8_000, None)], heard: now };
        assert_eq!(plan_words(&far, now), ("7d 80%".to_owned(), true));
        let back = now.as_millis().saturating_add(3_600_000);
        let spent = Reading {
            limits: vec![limit("five-hour", 10_000, Some(back)), limit("seven-day", 100, Some(1))],
            heard: WallMs::from_millis(now.as_millis().saturating_sub(20 * 60_000)),
        };
        let (words, warn) = plan_words(&spent, now);
        let at = crate::conversation::figures::clock(WallMs::from_millis(back)).unwrap_or_default();
        assert_eq!(words, format!("5h 100% until {at} \u{b7} 20m ago"));
        assert!(warn);
    }

    #[test]
    fn the_readouts_say_what_they_count() {
        assert_eq!(transfers_label(1, 0, 42, 100), "1 upload \u{b7} 42%");
        assert_eq!(transfers_label(2, 0, 0, 0), "2 uploads \u{b7} 0%");
        assert_eq!(transfers_label(0, 2, 1, 10), "2 downloads \u{b7} 10%");
        assert_eq!(transfers_label(1, 2, 2, 5), "3 transfers \u{b7} 40%");
        assert_eq!(counted(1, "port", "ports"), "1 port");
        assert_eq!(counted(3, "worker", "workers"), "3 workers");
        assert_eq!(sentence("server unreachable"), "Server unreachable");
        assert_eq!(sentence(""), "");
    }
}
