//! The bar along the bottom: the ambient facts, thin and quiet, as `MonoCode`'s status line is.
//!
//! On the left, which machine the focused tile runs on: the server's state while it does not
//! answer, then the focused tile's worker (a warn dot before it while it is away). Where on it
//! (the checkout and the branch) is the title bar's breadcrumb's to say, and the directory the
//! tile header's. On the right, the link to the focused worker while it is worth a look (a
//! round trip past [`RTT_SHOWN_FROM`], or a DERP relay) or while the pointer is over the bar,
//! what is wrong with the link when something is, a newer Slopty while one is out (which opens
//! its release page when clicked), what the focused tile says of itself (a
//! file's language and caret; a page's host is its header's), the plan's windows the focused
//! machine's agents last published (`5h 23% · 7d 41%`, in `warn` from 80 %, which list every
//! machine's when clicked), the ports forwarded here (which list them when clicked), the transfers
//! in flight both ways while one is not the focused tile's own upload (whose header says it; they
//! list every transfer with its rate, time left and stop when clicked), each a faint "·" from the
//! next. No agent is counted here, nor a machine: the tiles mark what is at work, the bell counts
//! what needs the person, and the navigator's machine rows say how each machine is and what can
//! be done to it.
//! The frame time shows only with the stream stats (⌘⇧I). Each readout is meta text with no icon,
//! its figures tabular, and a state is a small dot of its fill beside quiet words: the tile and the
//! bell carry the loud marks. The bar sits on the navigator's tone with no rule over it. A phone
//! keeps the worker and a slow round trip.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again, nor
//! does a round trip nobody would read.

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
use slopty_client::pacing::PaintRate;
use slopty_client::relay::RelayNotice;
use slopty_client::update::Release;
use slopty_core::ItemId;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::Theme;

use super::WorkspaceView;
use super::navigator::{Mode, RTT_SHOWN_FROM, path_label, rtt_label, worker_health};
use super::rollup::{META_SEPARATOR, meta_line};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{IconName, IconSize, icon};
use crate::kit::{self, meta, separator, tabular};
use crate::palette::section_heading;
use crate::screen::fps_label;

/// The bar's height: a line of meta text and a base unit round it, a notch under a tile's
/// header, so the frame's two bars do not read as a second row of headers.
pub(super) const STATUSBAR_H: f32 = 24.0;

/// What the server being down costs, after its word: workers are reached at the addresses
/// this client last had for them, and none it has not met.
const SERVER_DOWN_MEANS: &str = "direct links only";

/// The bar's popovers' width, in points.
const POPOVER_W: f32 = 300.0;

/// How old a plan reading grows before the bar says its age: the Claude Code status line runs
/// only while a session is live, so a reading after a long idle is a past one.
const PLAN_AGED_FROM: Duration = Duration::from_mins(15);

/// A plan window used this far, in hundredths of a percent, is said in `warn`.
const PLAN_WARN_FROM_BP: u32 = 8_000;

/// How often the bar's clock reads what changes with time (the frame time, a stream's rate) and
/// draws the bar again: the frame time's percentile sorts the probe's ring, which is not work
/// for every frame.
const FRAME_READOUT_EVERY: Duration = Duration::from_secs(1);

/// The status bar's own state.
#[derive(Default)]
pub(super) struct Bar {
    /// The frame time's readout, as the bar's clock last read it.
    frame_text: RefCell<FrameReading>,
    /// Draws the bar again when the frame time's readout is due, while the stats show.
    tick: RefCell<Option<Task<()>>>,
    /// [`Self::tick`] waits to fire. A draw while it waits leaves it be: one that set it going
    /// again put the readouts off for good under a bar drawn more often than its clock.
    ticking: Cell<bool>,
    /// The focused stream's painted rate, as the bar's clock last read it.
    rate: Cell<Option<(ItemId, PaintRate)>>,
    /// The pointer is over the bar, which then shows the link however quick it is.
    hovered: bool,
    /// The plan windows' popover is up.
    plans_open: bool,
    /// The transfers' popover is up.
    transfers_open: bool,
    /// The popover just closed, drawn for the moment it takes to fade away.
    leaving: Option<Popover>,
    /// A newer Slopty that is out, as the app last found it.
    release: Option<Release>,
}

impl Bar {
    /// Let the frame time's readout go, so the first draw with the stats reads it afresh
    /// rather than print the one from when they last showed.
    pub(super) fn forget_frame_time(&self) {
        self.frame_text.take();
    }
}

/// The frame time's readout as the bar's clock read it.
#[derive(Clone, Default)]
enum FrameReading {
    /// Not read since the stats showed.
    #[default]
    Unread,
    /// Read: the readout, or nothing before the app's probe has timed a frame.
    Read(Option<SharedString>),
}

/// One of the bar's popovers.
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

impl Bar {
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

impl std::fmt::Debug for Bar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bar").field("hovered", &self.hovered).finish_non_exhaustive()
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

/// The round trip's slot, in ems of the bar's text: wide enough for `999 ms`, the most a link
/// that is up shows, in tabular figures.
const RTT_SLOT_EMS: f32 = 3.6;

/// A count and its noun: `1 port`, `2 ports`.
#[must_use]
fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl WorkspaceView {
    /// The worker the status bar speaks for: the focused tile's, else the one a new tile
    /// would go to.
    pub(super) fn status_worker(&self) -> Option<WorkerKey> {
        self.focused().map(|t| t.worker).or_else(|| self.context_worker())
    }

    /// Ports forwarded here, across every shell.
    fn forwarded_count(&self) -> usize {
        self.ports.values().flatten().filter(|f| f.local.is_some()).count()
    }

    /// Open `which`, or close it if it is up.
    fn toggle_popover(&mut self, which: Popover, cx: &mut Context<Self>) {
        if self.bar.open(which) {
            self.close_popover(which, cx);
        } else {
            self.bar.set_open(which, true);
            cx.notify();
        }
    }

    /// Close `which`, drawn fading out for the moment that takes.
    fn close_popover(&mut self, which: Popover, cx: &mut Context<Self>) {
        if !self.bar.open(which) {
            return;
        }
        self.bar.set_open(which, false);
        if self.chrome_moves(cx) {
            self.keep_leaving(which, kit::Pace::Exit.duration(), |this| &mut this.bar.leaving, cx);
        }
        cx.notify();
    }

    /// The frame time's readout, as the bar's clock last read it ([`Self::read_clock`]), read
    /// here only for the first draw after the stats show; nothing before the app's probe has
    /// timed a frame. A draw never reads the clock itself: one drawn from scratch a moment
    /// after the frame on screen must print what that frame printed.
    fn frame_readout(&self, cx: &App) -> Option<SharedString> {
        let mut reading = self.bar.frame_text.borrow_mut();
        if matches!(*reading, FrameReading::Unread) {
            *reading = FrameReading::Read(frame_text(cx));
        }
        match &*reading {
            FrameReading::Read(text) => text.clone(),
            FrameReading::Unread => None,
        }
    }

    /// The rate stream `id` is painted at ([`crate::screen::ScreenView::paint_rate`]), as the bar's
    /// clock last read it. Read from the stream only when the clock has not read it yet: the
    /// bar would otherwise be built again with every frame the stream paints, for a number it
    /// prints once a second.
    fn stream_rate(&self, id: ItemId, cx: &App) -> Option<PaintRate> {
        if let Some((read, rate)) = self.bar.rate.get()
            && read == id
        {
            return Some(rate);
        }
        let rate = self.screens.get(&id)?.read(cx).paint_rate();
        self.bar.rate.set(Some((id, rate)));
        Some(rate)
    }

    /// What the bar's clock reads before it draws the bar again: the focused stream's rate,
    /// and the frame time while the stats show.
    fn read_clock(&self, cx: &App) {
        self.bar.ticking.set(false);
        let focused = self.focused().map(|t| t.item);
        let rate = focused.and_then(|id| Some((id, self.screens.get(&id)?.read(cx).paint_rate())));
        self.bar.rate.set(rate);
        *self.bar.frame_text.borrow_mut() =
            if self.show_stats { FrameReading::Read(frame_text(cx)) } else { FrameReading::Unread };
    }

    /// What the bar says of the focused tile, by its kind: a file's language and caret, a
    /// stream's size and rate, a page's host, how long a shell's command has run.
    pub(super) fn focus_facts(&self, item: &Item, cx: &App) -> Option<String> {
        match &item.kind {
            ItemKind::File { .. } => {
                let file = self.files.get(&item.id)?.read(cx);
                // A notice in the body has no caret: "Ln 1, Col 1" under "Too large to open
                // here" named a place in text that is not there.
                if !file.shows_text() {
                    return None;
                }
                let (line, col) = file.caret(cx);
                let caret = format!("Ln {line}, Col {col}");
                let layout = file.layout_facts();
                let facts = layout.iter().map(|f| Some(f.as_str()));
                Some(meta_line(
                    std::iter::once(file.coloured_as()).chain(facts).chain([Some(caret.as_str())]),
                ))
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                let stream = self.stream(item.id).filter(|s| s.drawn)?;
                let (w, h) = stream.size;
                let rate = self.stream_rate(item.id, cx)?;
                Some(format!("{w}\u{d7}{h} \u{b7} {}", fps_label(rate)))
            }
            ItemKind::Terminal { session } if self.agent_state(*session).is_none() => {
                self.running_for(*session).map(|ran| format!("Running {}", kit::clock(ran)))
            }
            // A page's host is its header's place: said there, not twice.
            ItemKind::Terminal { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. }
            | ItemKind::Review { .. }
            | ItemKind::Changes { .. }
            | ItemKind::Thread { .. } => None,
        }
    }

    /// Whether what the bar says of the focused tile changes with the clock: a command's
    /// running time, a stream's rate.
    fn focus_clocked(&self) -> bool {
        let Some(item) = self.focused().and_then(|t| self.item(t)) else { return false };
        match item.kind {
            ItemKind::Window { .. } | ItemKind::Display { .. } => true,
            ItemKind::Terminal { session } => self.running_for(session).is_some(),
            _ => false,
        }
    }

    /// The bar, or nothing before the first worker.
    pub(super) fn render_statusbar(
        &self,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        if self.workers.is_empty() || self.key_bar_shown {
            // Up for a notice alone: the bar holds it and nothing else.
            return self.notices_bar(window, cx);
        }
        let phone = matches!(self.nav.drawn, Some(Mode::Drawer))
            || self.width(window) < self.layout.config().phone_below;
        let stats = self.show_stats && !phone;
        let frame = stats.then(|| self.frame_readout(cx)).flatten();
        // The frame time, a command's running time and a stream's rate change with the clock.
        let clocked = stats || self.focus_clocked();
        // Set going once and left to fire: a draw that set it going again put the readouts off
        // for as long as the bar was drawn more often than its clock ticks, a stream's rate
        // stuck at its first reading.
        if !clocked {
            self.bar.tick.borrow_mut().take();
            self.bar.ticking.set(false);
        } else if !self.bar.ticking.replace(true) {
            let (bar, this) = (self.chrome.statusbar.entity_id(), cx.weak_entity());
            *self.bar.tick.borrow_mut() = Some(cx.spawn(async move |cx| {
                cx.background_executor().timer(FRAME_READOUT_EVERY).await;
                cx.update(|cx| {
                    let Some(this) = this.upgrade() else { return };
                    this.read(cx).read_clock(cx);
                    cx.notify(bar);
                });
            }));
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();

        // A state is a dot of its fill beside its words; no readout wears an icon. Out of reach
        // is muted, since `warn` means "needs you" alone. The server's word says what it costs:
        // only the workers' own addresses reach them now.
        let server = self.server_status.clone().map(|text| {
            let text = SharedString::from(sentence(&text));
            readout("server-status", text.clone())
                .aria_description(SERVER_DOWN_MEANS)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(hsla(s.text_secondary))
                .child(state_dot(theme, s.text_muted))
                .child(text)
                .child(separator(theme))
                .child(SERVER_DOWN_MEANS)
        });
        let worker = self.status_worker();
        let link = worker.and_then(|k| self.workers.get(&k));
        let focused_item = (!phone).then(|| self.focused().and_then(|t| self.item(t))).flatten();
        // The worker, in the secondary tone: with one worker too, since which machine a tile
        // runs on is the first thing a remote tool says.
        let name = link.map(|w| {
            let away = (!w.status.is_up()).then(|| {
                state_dot(theme, s.text_muted).debug_selector(|| "status-worker-away".to_owned())
            });
            readout("status-worker", SharedString::from(w.name.clone()))
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(hsla(s.text_secondary))
                .children(away)
                .child(SharedString::from(w.name.clone()))
        });
        // The server's word while it is down, then the worker, a faint dot apart as the
        // right's readouts are.
        let mut place_parts: Vec<gpui::AnyElement> = Vec::new();
        for part in [
            server.map(gpui::IntoElement::into_any_element),
            name.map(gpui::IntoElement::into_any_element),
        ]
        .into_iter()
        .flatten()
        {
            if !place_parts.is_empty() {
                place_parts.push(separator(theme).into_any_element());
            }
            place_parts.push(part);
        }
        let left = div().flex_1().min_w_0().overflow_hidden().flex().items_center().child(
            div()
                .debug_selector(|| "status-place".to_owned())
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .children(place_parts),
        );

        let ports = (!phone).then(|| self.forwarded_count()).filter(|n| *n > 0).map(|n| {
            let text = SharedString::from(counted(n, "port", "ports"));
            let el = button("status-ports", text.clone(), theme).child(tabular(div()).child(text));
            tab_stop(el, s.accent).on_click(cx.listener(|this, _ev, window, cx| {
                this.list_ports(&super::actions::ListPorts, window, cx);
            }))
        });
        let transfers = (!phone).then(|| self.transfers_button(cx)).flatten();
        let release = self.bar.release.as_ref().filter(|_| !phone).map(|release| {
            let text = SharedString::from(release_line(release));
            let page = release.page.clone();
            let el = button("status-release", text.clone(), theme).child(text);
            tab_stop(el, s.accent).on_click(move |_ev, _w, cx| cx.open_url(&page))
        });
        let clock = cx.background_executor().now();
        let link_readout = link.filter(|w| w.status.is_up()).and_then(|w| {
            let path = w.relay.path().map(path_label);
            let relayed = w.relay.notice(clock);
            let rtt = self.shown_rtt(w);
            let shown = self.bar.hovered
                || relayed.is_some()
                || rtt.is_some_and(|rtt| rtt >= RTT_SHOWN_FROM);
            shown.then(|| self.render_link(path, relayed, rtt))
        });
        // The link says something only when it is not up: a word in its tone, the mark being
        // on the left.
        let health = link.and_then(|w| worker_health(&w.status)).map(|(mark, word)| {
            let word = SharedString::from(sentence(word));
            readout("status-link", word.clone()).text_color(hsla(mark.tone(theme))).child(word)
        });
        let frame = frame.map(|text| tabular(readout("status-frame", text.clone())).child(text));
        let facts = focused_item.and_then(|item| self.focus_facts(item, cx)).map(|text| {
            let text = SharedString::from(text);
            spaced(tabular(readout("status-facts", text.clone())), &text, theme)
        });
        let plan = (!phone).then(|| self.plan_button(cx)).flatten();
        // The link leads the right: it comes and goes with the pointer, and growing leftward
        // from the start of a cluster that is pinned right, it moves nothing else. The readouts
        // are parted by the faint dot the left's path steps are, never by space alone.
        let readouts = [
            link_readout.map(gpui::IntoElement::into_any_element),
            health.map(gpui::IntoElement::into_any_element),
            facts.map(gpui::IntoElement::into_any_element),
            plan.map(gpui::IntoElement::into_any_element),
            ports.map(gpui::IntoElement::into_any_element),
            transfers.map(gpui::IntoElement::into_any_element),
            release.map(gpui::IntoElement::into_any_element),
            frame.map(gpui::IntoElement::into_any_element),
        ];
        let mut right_parts: Vec<gpui::AnyElement> = Vec::new();
        for part in readouts.into_iter().flatten() {
            if !right_parts.is_empty() {
                right_parts.push(separator(theme).into_any_element());
            }
            right_parts.push(part);
        }
        let right = div()
            .debug_selector(|| "status-right".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .children(right_parts);
        let plans =
            (self.bar.shown(Popover::Plans) && !phone).then(|| self.render_plans(window, cx));
        let shows_transfers = self.bar.shown(Popover::Transfers);
        let transfer_list = (shows_transfers && !phone && self.transfers_in_flight())
            .then(|| self.render_transfers(window, cx));
        let statusbar = self.chrome.statusbar.entity_id();
        let bar = div()
            .id("statusbar")
            .debug_selector(|| "statusbar".to_owned())
            .role(Role::Group)
            .aria_label("Status")
            .size_full()
            .flex()
            .items_center()
            .gap(px(if phone { spacing.sm } else { spacing.md }))
            .pl(px(spacing.inset()) + safe.left)
            .pr(px(spacing.inset()) + safe.right)
            .bg(hsla(s.canvas))
            .font_family(theme.typography.ui_family.clone())
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if this.bar.hovered != *hovered {
                    this.bar.hovered = *hovered;
                    App::notify(cx, statusbar);
                }
            }));
        // The notices take the lane between the two, which no tile draws in.
        let notices = self.render_notices(cx);
        meta(bar, theme)
            .child(left)
            .children(notices)
            .child(right)
            .children(plans)
            .children(transfer_list)
            .into_any_element()
    }

    /// The bar with only the notices, right-aligned where the readouts would end.
    fn notices_bar(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let safe = window.insets().effective();
        let bar = div()
            .id("statusbar")
            .debug_selector(|| "statusbar".to_owned())
            .role(Role::Group)
            .aria_label("Status")
            .size_full()
            .flex()
            .items_center()
            .justify_end()
            .pl(px(theme.spacing.inset()) + safe.left)
            .pr(px(theme.spacing.inset()) + safe.right)
            .bg(hsla(theme.surfaces.canvas))
            .font_family(theme.typography.ui_family.clone())
            .children(self.render_notices(cx));
        meta(bar, theme).into_any_element()
    }

    /// The link to the focused worker: how the tailnet carries it (a DERP relay in the
    /// warning tone, being the slow path) and its round trip, warning past
    /// [`crate::screen::RTT_WARN_FROM`]. A DERP relay that has held is said in words instead,
    /// quietly, with its fix under the pointer. The figure sits in a slot as wide as any it
    /// shows, pinned at its right, so a new sample moves only its own digits; its unit says
    /// what it is, and a screen reader hears the words.
    fn render_link(
        &self,
        path: Option<(String, bool)>,
        relayed: Option<RelayNotice>,
        rtt: Option<Duration>,
    ) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let path = match relayed {
            Some(RelayNotice { note, fix, .. }) => {
                let text = SharedString::from(note);
                let hint_theme = Rc::new(theme.clone());
                Some(
                    readout("status-relay", text.clone())
                        .aria_description(fix)
                        .text_color(hsla(s.text_muted))
                        .map(kit::hint_timing)
                        .tooltip(move |_window, cx| {
                            let theme = Rc::clone(&hint_theme);
                            cx.new(|_| kit::Hint::new(fix, "", theme)).into()
                        })
                        .child(text),
                )
            }
            None => path.map(|(text, slow)| {
                let text = SharedString::from(text);
                readout("status-path", text.clone())
                    .when(slow, |el| el.text_color(hsla(s.warn)))
                    .child(text)
            }),
        };
        let figure = rtt.map(|rtt| {
            let text = SharedString::from(rtt_label(rtt));
            tabular(readout("status-rtt", text.clone()))
                .aria_label(SharedString::from(format!("Round trip {text}")))
                .when(rtt >= crate::screen::RTT_WARN_FROM, |el| el.text_color(hsla(s.warn)))
                .child(text)
        });
        div()
            .debug_selector(|| "status-link-readout".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .children(path)
            .child(
                div()
                    .debug_selector(|| "status-rtt-slot".to_owned())
                    .flex_none()
                    .flex()
                    .justify_end()
                    .min_w(px(theme.typography.small() * RTT_SLOT_EMS))
                    .children(figure),
            )
    }

    /// Whether the status bar prints `key`'s round trip, going from `was` to `now`: the
    /// focused worker's, while it is slow or the pointer is over the bar.
    pub(super) fn status_prints_rtt(
        &self,
        key: WorkerKey,
        was: Option<Duration>,
        now: Option<Duration>,
    ) -> bool {
        let slow = |rtt: Option<Duration>| rtt.is_some_and(|rtt| rtt >= RTT_SHOWN_FROM);
        self.status_worker() == Some(key) && (self.bar.hovered || slow(was) || slow(now))
    }

    /// The plan's windows on the focused tile's machine, as its agent last published them (else
    /// the freshest reading there): `5h 23% · 7d 41%`, in `warn` from 80 %, with when a spent
    /// window comes back, and its age once it is past [`PLAN_AGED_FROM`]. None where no agent
    /// there published any. Clicked, every machine's readings.
    fn plan_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = self.focused();
        let worker = tile.map(|t| t.worker).or_else(|| self.status_worker())?;
        let agent = tile.and_then(|t| self.item(t)).and_then(|i| self.item_agent(i));
        let now = crate::clock::now(cx);
        let (_, reading) = self.faces.threads.meters().shown(worker, agent, now)?;
        let (text, warn) = plan_words(reading, now);
        let label = SharedString::from(format!("{PLAN_USAGE} {text}"));
        let words = spaced(tabular(div().id("status-plan-words")), &text, theme);
        let el = button("status-plan", label, theme)
            .when(warn, |el| el.text_color(hsla(s.warn)))
            .child(words)
            .when(self.bar.plans_open, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        Some(tab_stop(el, s.accent).on_click(cx.listener(|this, _ev, _window, cx| {
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
            spaced(tabular(button("status-transfers", text.clone().into(), theme)), &text, theme)
                .when(self.bar.transfers_open, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)));
        Some(tab_stop(el, s.accent).on_click(cx.listener(|this, _ev, _window, cx| {
            this.toggle_popover(Popover::Transfers, cx);
        })))
    }

    /// Close the transfers popover: nothing is left in it.
    pub(in crate::workspace) const fn close_transfers(&mut self) {
        self.bar.transfers_open = false;
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
            .bottom(px(STATUSBAR_H + spacing.xs) + safe.bottom)
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
            if row.up { (IconName::Upload, "to") } else { (IconName::Download, "from") };
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
        let stop = tab_stop(stop, s.accent)
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            // Its own focus, as the navigator's row actions: not any focused ancestor's.
            .focus(gpui::Styled::visible)
            .focus_visible(move |st| st.outline_ring(crate::a11y::ring(s.accent)).visible())
            .on_click(cx.listener(move |this, _ev, _window, cx| {
                cx.stop_propagation();
                this.cancel_transfer(xfer, cx);
            }));
        // How far it got, as a hairline under the words: the figure says it, the line shows it.
        let progress = row.fraction.map(|fraction| {
            div()
                .debug_selector(move || format!("transfers-bar-{xfer}"))
                .mt(px(spacing.xxs))
                .h(px(1.0))
                .w_full()
                .bg(hsla(s.border_subtle))
                .child(div().h_full().w(gpui::relative(fraction)).bg(hsla(s.accent_fill)))
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
            .child(
                div()
                    .flex_none()
                    .size(px(theme.typography.icon_large()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(theme, mark, IconSize::Inline, hsla(s.text_muted))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(hsla(s.text))
                            .child(SharedString::from(row.name)),
                    )
                    .child(
                        meta(tabular(div()), theme)
                            .debug_selector(move || format!("transfers-words-{xfer}"))
                            .overflow_hidden()
                            .text_ellipsis()
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
            .bottom(px(STATUSBAR_H + spacing.xs) + safe.bottom)
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
        let open = self.bar.open(which);
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

/// A small dot of a state's fill beside a readout's words.
fn state_dot(theme: &Theme, fill: slopty_theme::Rgb) -> Div {
    div().flex_none().size(px(theme.spacing.xs + theme.spacing.xxs)).rounded_full().bg(hsla(fill))
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

/// A readout: a status the screen reader reads as `text`, on one line.
impl WorkspaceView {
    /// The newer Slopty that is out, or `None` once this build is the latest: the bar says it
    /// quietly until then.
    pub fn set_release(&mut self, release: Option<Release>, cx: &mut Context<Self>) {
        if self.bar.release != release {
            self.bar.release = release;
            App::notify(cx, self.chrome.statusbar.entity_id());
        }
    }
}

/// What the bar says of a newer release: "Slopty 0.2.0 is out".
pub(super) fn release_line(release: &Release) -> String {
    format!("Slopty {} is out", release.version)
}

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

    /// Inside a repository a place is named by it and the path within; elsewhere, the tail.
    #[test]
    fn a_place_in_a_repository_is_named_by_it() {
        use crate::workspace::tile::repo_place;
        assert_eq!(repo_place("/w/oss/slopty", Some("/w/oss/slopty"), None), "slopty");
        assert_eq!(
            repo_place("/w/oss/slopty/crates/ui/", Some("/w/oss/slopty"), None),
            "slopty/crates/ui"
        );
        assert_eq!(repo_place("/w/oss/slopty-two", Some("/w/oss/slopty"), None), "oss/slopty-two");
        assert_eq!(repo_place("/Users/me/src", None, None), "~/src");
    }
}
