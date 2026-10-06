//! The title bar's readouts: what the app has to say for itself, at the bar's trailing end
//! before the bell, each only while it has something to say. The server while it does not
//! answer; the focused machine's link on a DERP relay that has held, with its fix; the plan's
//! windows the focused machine's agents last published, once one is 80 % used (`7d 82%`, in `warn`,
//! which list every machine's when clicked); the ports forwarded here (which list them when
//! clicked); the transfers in flight both ways while one is not the focused tile's own upload
//! (whose header says it; they list every transfer with its rate, time left and stop when clicked);
//! a newer Slopty while one is out (which opens its release page when clicked); and the frame time
//! with the stream stats (⌘⇧I).
//!
//! Nothing here repeats what is said elsewhere: which machine a tile runs on is the navigator's
//! and the breadcrumb's, a link's round trip its machine row's, a file's language and
//! caret its tile's, a stream's size and rate its stats overlay's. They lived in a bar along
//! the window's bottom that was there whatever it had to say, mostly a worker's name said
//! twice (`docs/decisions/ui.md`, "No bar along the bottom").
//!
//! Each readout is meta text with no icon, its figures tabular; a state is a small dot of its
//! fill beside quiet words. A phone's bar has no room for them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Div, ElementId, InteractiveElement as _, IntoElement as _,
    MouseButton, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_client::update::Release;
use slopty_theme::Theme;

use super::WorkspaceView;
use super::rollup::META_SEPARATOR;
use super::titlebar::titlebar_height;
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{IconSize, Symbol, icon};
use crate::kit::{self, meta, separator, tabular};
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

/// How often the frame time's clock reads it and draws the title bar again, while the stats
/// show: the percentile sorts the probe's ring, which is not work for every frame.
const FRAME_READOUT_EVERY: Duration = Duration::from_secs(1);

/// The readouts' own state.
#[derive(Default)]
pub(super) struct Readouts {
    /// The frame time's readout, as the clock last read it.
    frame_text: RefCell<FrameReading>,
    /// Draws the title bar again when the frame time's readout is due, while the stats show.
    tick: RefCell<Option<Task<()>>>,
    /// [`Self::tick`] waits to fire. A draw while it waits leaves it be: one that set it going
    /// again put the readout off for good under a bar drawn more often than its clock.
    ticking: Cell<bool>,
    /// The plan windows' popover is up.
    plans_open: bool,
    /// The transfers' popover is up.
    transfers_open: bool,
    /// The popover just closed, drawn for the moment it takes to fade away.
    leaving: Option<Popover>,
    /// A newer Slopty that is out, as the app last found it.
    release: Option<Release>,
}

impl Readouts {
    /// Let the frame time's readout go, so the first draw with the stats reads it afresh
    /// rather than print the one from when they last showed.
    pub(super) fn forget_frame_time(&self) {
        self.frame_text.take();
    }
}

/// The frame time's readout as the clock read it.
#[derive(Clone, Default)]
enum FrameReading {
    /// Not read since the stats showed.
    #[default]
    Unread,
    /// Read: the readout, or nothing before the app's probe has timed a frame.
    Read(Option<SharedString>),
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
    fn shown(&self, which: Popover) -> bool {
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

    /// Ports forwarded here, across every shell.
    fn forwarded_count(&self) -> usize {
        self.ports.values().flatten().filter(|f| f.local.is_some()).count()
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

    /// The frame time's readout, as the clock last read it ([`Self::read_clock`]), read here
    /// only for the first draw after the stats show; nothing before the app's probe has timed a
    /// frame. A draw never reads the clock itself: one drawn from scratch a moment after the
    /// frame on screen must print what that frame printed.
    fn frame_readout(&self, cx: &App) -> Option<SharedString> {
        let mut reading = self.readouts.frame_text.borrow_mut();
        if matches!(*reading, FrameReading::Unread) {
            *reading = FrameReading::Read(frame_text(cx));
        }
        match &*reading {
            FrameReading::Read(text) => text.clone(),
            FrameReading::Unread => None,
        }
    }

    /// What the clock reads before it draws the title bar again: the frame time while the
    /// stats show.
    fn read_clock(&self, cx: &App) {
        self.readouts.ticking.set(false);
        *self.readouts.frame_text.borrow_mut() =
            if self.show_stats { FrameReading::Read(frame_text(cx)) } else { FrameReading::Unread };
    }

    /// Keep the frame time's clock going while the stats show, and let it go once they do not.
    /// Set going once and left to fire: a draw that set it going again put the readout off for
    /// as long as the bar was drawn more often than its clock ticks.
    fn keep_clock(&self, stats: bool, cx: &Draw<'_, Self>) {
        if !stats {
            self.readouts.tick.borrow_mut().take();
            self.readouts.ticking.set(false);
        } else if !self.readouts.ticking.replace(true) {
            let (bar, this) = (self.chrome.titlebar.entity_id(), cx.weak_entity());
            *self.readouts.tick.borrow_mut() = Some(cx.spawn(async move |cx| {
                cx.background_executor().timer(FRAME_READOUT_EVERY).await;
                cx.update(|cx| {
                    let Some(this) = this.upgrade() else { return };
                    this.read(cx).read_clock(cx);
                    cx.notify(bar);
                });
            }));
        }
    }

    /// The readouts that have something to say, for the title bar's trailing end, with the
    /// popover that is up; nothing on a phone's bar, nor while none has anything to say.
    pub(super) fn render_readouts(
        &self,
        phone: bool,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let stats = self.show_stats && !phone;
        self.keep_clock(stats, cx);
        if phone || self.workers.is_empty() {
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
                .child(icon(theme, Symbol::WifiSlash, IconSize::Inline, hsla(s.text_muted)))
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
        let ports = Some(self.forwarded_count()).filter(|n| *n > 0).map(|n| {
            let text = SharedString::from(counted(n, "port", "ports"));
            let el = button("readout-ports", text.clone(), theme).child(tabular(div()).child(text));
            tab_stop(el, s.focus).on_click(cx.listener(|this, _ev, window, cx| {
                this.list_ports(&super::actions::ListPorts, window, cx);
            }))
        });
        let transfers = self.transfers_button(cx);
        let release = self.readouts.release.as_ref().map(|release| {
            let text = SharedString::from(release_line(release));
            let page = release.page.clone();
            let el = button("readout-release", text.clone(), theme).child(text);
            tab_stop(el, s.focus).on_click(move |_ev, _w, cx| cx.open_url(&page))
        });
        let frame = stats.then(|| self.frame_readout(cx)).flatten();
        let frame = frame.map(|text| tabular(readout("readout-frame", text.clone())).child(text));
        let plan = self.plan_button(cx);
        let parts: Vec<gpui::AnyElement> = [
            server.map(gpui::IntoElement::into_any_element),
            relay.map(gpui::IntoElement::into_any_element),
            plan.map(gpui::IntoElement::into_any_element),
            ports.map(gpui::IntoElement::into_any_element),
            transfers.map(gpui::IntoElement::into_any_element),
            release.map(gpui::IntoElement::into_any_element),
            frame.map(gpui::IntoElement::into_any_element),
        ]
        .into_iter()
        .flatten()
        .collect();
        let plans = self.readouts.shown(Popover::Plans).then(|| self.render_plans(window, cx));
        let transfer_list = (self.readouts.shown(Popover::Transfers) && self.transfers_in_flight())
            .then(|| self.render_transfers(window, cx));
        if parts.is_empty() && plans.is_none() && transfer_list.is_none() {
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
            .children(parts)
            .children(plans)
            .children(transfer_list);
        Some(meta(row, theme).into_any_element())
    }

    /// The plan's windows on the focused tile's machine, as its agent last published them (else
    /// the freshest reading there), once one is 80 % used: `5h 23% · 7d 82%` in `warn`, with
    /// when a spent window comes back, and its age once it is past [`PLAN_AGED_FROM`]. Under
    /// that it is not news, and the composer's meter says the context. Clicked, every
    /// machine's readings.
    fn plan_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = self.focused();
        let worker = tile.map(|t| t.worker).or_else(|| self.readout_worker())?;
        let agent = tile.and_then(|t| self.item(t)).and_then(|i| self.item_agent(i));
        let now = crate::clock::now(cx);
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
    /// upload (its header says that one, with its stop): how many and how far, together.
    /// Clicked, the list of them.
    fn transfers_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let rows = self.transfer_rows(cx.background_executor().now());
        let focused = self.focused();
        let own =
            self.uploads.values().filter(|u| Some(u.tile) == focused && u.drag.is_none()).count();
        if rows.len() <= own {
            return None;
        }
        let ups = rows.iter().filter(|r| r.up).count();
        let (done, total) = rows.iter().fold((0_u64, 0_u64), |(d, t), r| {
            (d.saturating_add(r.done), t.saturating_add(r.total))
        });
        let text = transfers_label(ups, rows.len().saturating_sub(ups), done, total);
        let el =
            spaced(tabular(button("readout-transfers", text.clone().into(), theme)), &text, theme)
                .when(self.readouts.transfers_open, |el| {
                    el.bg(hsla(s.hover)).text_color(hsla(s.text))
                });
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
    fn render_transfers(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
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
            .top(px(titlebar_height(theme) + spacing.xs) + safe.top)
            .right(px(spacing.md) + safe.right)
            .w(px(POPOVER_W))
            .flex()
            .flex_col()
            .pb(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(section_heading(theme, "transfers-heading".into(), TRANSFERS))
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
            .focus_visible(move |st| st.outline_ring(crate::a11y::ring(s.focus)).visible())
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
            .child(crate::palette::lead_slot(theme, mark, hsla(s.text_secondary), 1.0))
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
    fn render_plans(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let now = crate::clock::now(cx);
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
                    super::projects::agent_label(&agent_id)
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
            .top(px(titlebar_height(theme) + spacing.xs) + safe.top)
            .right(px(spacing.md) + safe.right)
            .w(px(POPOVER_W))
            .flex()
            .flex_col()
            .pb(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(section_heading(theme, "plans-heading".into(), PLAN_USAGE))
            .children(rows);
        self.popover(Popover::Plans, panel, window, cx)
    }

    /// `panel` over the window as the bar's popover `which`: a click anywhere else closes it,
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
            .with_priority(crate::palette::Layer::Popover.priority())
            .into_any_element()
    }
}

/// `text` in sentence case: the app and the link words come lowercase.
fn sentence(text: &str) -> String {
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

/// The frame time's readout now: the median of the frames the app's probe timed, or nothing
/// before it has timed one.
fn frame_text(cx: &App) -> Option<SharedString> {
    crate::frames::stats(cx)
        .filter(|s| s.frames > 0)
        .map(|s| format!("Frame {:.1} ms", s.draw_p50.as_secs_f64() * 1e3).into())
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
    /// The newer Slopty that is out, or `None` once this build is the latest: the title bar
    /// says it quietly until then.
    pub fn set_release(&mut self, release: Option<Release>, cx: &mut Context<Self>) {
        if self.readouts.release != release {
            self.readouts.release = release;
            App::notify(cx, self.chrome.titlebar.entity_id());
        }
    }
}

/// What the title bar says of a newer release: "Slopty 0.2.0 is out".
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

/// A readout that does something when clicked: washed under the pointer.
fn button(selector: &'static str, text: SharedString, theme: &Theme) -> Stateful<Div> {
    let s = theme.surfaces;
    readout(selector, text)
        .role(Role::Button)
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs))
        .px(px(theme.spacing.xs))
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
