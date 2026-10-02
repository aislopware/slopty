//! The bar along the bottom: the ambient facts, thin and quiet, as `MonoCode`'s status line is.
//!
//! On the left, which machine the focused tile runs on: the server's state while it does not
//! answer, then the focused tile's worker (a warn dot before it while it is away). Where on it
//! (the checkout and the branch) is the title bar's breadcrumb's to say, and the directory the
//! tile header's. On the right, the link to the focused worker while it is worth a look (a
//! round trip past [`RTT_SHOWN_FROM`], or a DERP relay) or while the pointer is over the bar,
//! what is wrong with the link when something is, what the focused tile says of itself (a
//! file's language and caret; a page's host is its header's), the ports forwarded here (which
//! list them when clicked), the uploads in flight to tiles other than the focused one (whose
//! header says its own), the count of workers only while one of them is not up (which opens
//! the hosts popover: each worker's link, connect and forget), and each worker's agents:
//! working, waiting, blocked and to review (a turn that ended unseen), each a faint "·" from
//! the next. Who waits on the human is counted once, on the bell. The frame time shows only
//! with the stream stats (⌘⇧I). Each readout is meta text with no icon, its figures tabular,
//! and a state is a small dot of its fill beside quiet words: the tile and the bell carry the
//! loud marks. The bar sits on the navigator's tone with no rule over it. A phone keeps the
//! worker, a slow round trip and the agents.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again, nor
//! does a round trip nobody would read.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

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
use slopty_core::ItemId;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::Theme;

use super::navigator::{Mode, RTT_SHOWN_FROM, host_line, path_label, rtt_label, worker_health};
use super::rollup::{META_SEPARATOR, meta_line};
use super::{MenuRun, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{IconName, IconSize, Status, icon, status_icon};
use crate::kit::{self, meta, separator, tabular};
use crate::palette::section_heading;
use crate::screen::fps_label;

/// The bar's height: a line of meta text and a base unit round it, a notch under a tile's
/// header, so the frame's two bars do not read as a second row of headers.
pub(super) const STATUSBAR_H: f32 = 24.0;

/// What the server being down costs, after its word: workers are reached at the addresses
/// this client last had for them, and none it has not met.
const SERVER_DOWN_MEANS: &str = "direct links only";

/// The hosts popover's width, in points.
const HOSTS_W: f32 = 300.0;

/// How long a frame-time readout stands before it is worked out again: the percentile sorts
/// the probe's ring, which is not work for every frame.
const FRAME_READOUT_EVERY: Duration = Duration::from_secs(1);

/// What the app lets the hosts popover do to one worker.
#[derive(Clone, Default)]
pub struct HostActions {
    /// Dial it now rather than at the end of the backoff; offered while its link is down.
    pub connect: Option<MenuRun>,
    /// Forget it: offered for a worker added by address, which the server does not list.
    pub forget: Option<MenuRun>,
    /// Wake it from sleep: offered while the server can send it a magic packet
    /// (`slopty_client::directory::Directory::can_wake`). The palette offers it too.
    pub wake: Option<MenuRun>,
}

impl std::fmt::Debug for HostActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostActions")
            .field("connect", &self.connect.is_some())
            .field("forget", &self.forget.is_some())
            .field("wake", &self.wake.is_some())
            .finish()
    }
}

/// The status bar's own state.
#[derive(Default)]
pub(super) struct Bar {
    /// The frame time's readout, and when it was worked out.
    frame_text: RefCell<Option<(Instant, Option<SharedString>)>>,
    /// The hosts popover is up.
    hosts_open: bool,
    /// What the popover can do to each worker, as the app says.
    hosts: HashMap<WorkerKey, HostActions>,
    /// The way to add a worker, as the app says: the popover's, and the empty workspace's.
    add: Option<MenuRun>,
    /// Draws the bar again when the frame time's readout is due, while the stats show.
    tick: RefCell<Option<Task<()>>>,
    /// [`Self::tick`] waits to fire. A draw while it waits leaves it be: one that set it going
    /// again put the readouts off for good under a bar drawn more often than its clock.
    ticking: Cell<bool>,
    /// The focused stream's painted rate, as the bar's clock last read it.
    rate: Cell<Option<(ItemId, PaintRate)>>,
    /// The pointer is over the bar, which then shows the link however quick it is.
    hovered: bool,
}

impl std::fmt::Debug for Bar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bar")
            .field("hosts_open", &self.hosts_open)
            .field("hosts", &self.hosts)
            .field("add", &self.add.is_some())
            .field("hovered", &self.hovered)
            .finish_non_exhaustive()
    }
}

/// How one worker's agents stand, in Claude Code's own three words (busy on a turn, waiting on
/// work in the background, blocked on the person), and how many ended a turn nobody has looked
/// at yet. An agent at rest and seen is not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct AgentCounts {
    working: usize,
    waiting: usize,
    blocked: usize,
    review: usize,
}

impl AgentCounts {
    /// Count an agent its tile marks `status`.
    const fn add(&mut self, status: Status) {
        let n = match status {
            Status::Working => &mut self.working,
            Status::Running => &mut self.waiting,
            Status::NeedsYou => &mut self.blocked,
            Status::Idle | Status::Done | Status::Failed | Status::Away => return,
        };
        *n = n.saturating_add(1);
    }

    /// Count an agent whose turn ended unseen.
    const fn add_review(&mut self) {
        self.review = self.review.saturating_add(1);
    }

    const fn is_empty(self) -> bool {
        self.working == 0 && self.waiting == 0 && self.blocked == 0 && self.review == 0
    }

    /// Each count there is, with its word and the mark's tone.
    fn parts(self, theme: &Theme) -> impl Iterator<Item = (String, slopty_theme::Rgb)> {
        let s = theme.surfaces;
        [
            (self.working, "working", s.text_muted),
            (self.waiting, "waiting", s.text_muted),
            (self.blocked, "blocked", s.warn_fill),
            (self.review, "to review", s.accent_fill),
        ]
        .into_iter()
        .filter(|(n, ..)| *n > 0)
        .map(|(n, word, tone)| (format!("{n} {word}"), tone))
    }
}

/// The agents' line in words, as a screen reader hears it: `2 working, 1 blocked`, each worker
/// named when there are more than one (`studio: 2 working; mini: 1 waiting`).
#[must_use]
fn agents_label(theme: &Theme, counts: &[(String, AgentCounts)]) -> String {
    let named = counts.len() > 1;
    counts
        .iter()
        .map(|(name, c)| {
            let said = c.parts(theme).map(|(text, _)| text).collect::<Vec<_>>().join(", ");
            if named { format!("{name}: {said}") } else { said }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Uploads in flight at a glance: `1 upload · 42%`.
#[must_use]
fn transfers_label(count: usize, done: u64, total: u64) -> String {
    let noun = if count == 1 { "upload" } else { "uploads" };
    let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
    format!("{count} {noun} · {percent}%")
}

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

    /// Each worker's agents, as their tiles mark them, in the workers' order: the ones this
    /// client follows, the ones only the server reports, and the threads no terminal speaks
    /// for. A worker with none at work is left out.
    pub(super) fn agent_counts(&self) -> Vec<(String, AgentCounts)> {
        let mut by_worker: BTreeMap<WorkerKey, AgentCounts> = BTreeMap::new();
        let followed = self.agents.keys().filter_map(|s| Some((*s, self.worker_of_session(*s)?)));
        let reported = self
            .server_agents
            .iter()
            .filter(|(s, _)| !self.agents.contains_key(s))
            .map(|(s, (worker, _))| (*s, *worker));
        for (session, worker) in followed.chain(reported) {
            if let Some(status) = self.agent_mark(session) {
                by_worker.entry(worker).or_default().add(status);
            }
        }
        for ended in self.to_review() {
            by_worker.entry(ended.worker).or_default().add_review();
        }
        // Threads no terminal speaks for (Codex, pi, an ACP agent), as their rows say.
        for (_, stand) in self.thread_stands() {
            let counts = by_worker.entry(stand.worker).or_default();
            match stand.rung {
                slopty_proto::thread::attention::Rung::ToReview => counts.add_review(),
                _ => {
                    if let Some(status) = stand.status() {
                        counts.add(status);
                    }
                }
            }
        }
        by_worker
            .into_iter()
            .filter(|(_, c)| !c.is_empty())
            .map(|(worker, c)| {
                (self.workers.get(&worker).map(|w| w.name.clone()).unwrap_or_default(), c)
            })
            .collect()
    }

    /// Ports forwarded here, across every shell.
    fn forwarded_count(&self) -> usize {
        self.ports.values().flatten().filter(|f| f.local.is_some()).count()
    }

    /// What the hosts popover can do to each worker, and its way to add one. The app says,
    /// since connecting and forgetting are its: the workspace only shows workers.
    pub fn set_host_actions(
        &mut self,
        hosts: HashMap<WorkerKey, HostActions>,
        add: Option<MenuRun>,
        cx: &mut Context<Self>,
    ) {
        self.bar.hosts = hosts;
        self.bar.add = add;
        cx.notify();
    }

    /// The app's way to add a worker, if it gave one.
    pub(super) fn add_worker_run(&self) -> Option<MenuRun> {
        self.bar.add.clone()
    }

    /// What the app lets this client do to `key`.
    pub(super) fn host_actions(&self, key: WorkerKey) -> Option<&HostActions> {
        self.bar.hosts.get(&key)
    }

    /// Whether the hosts popover is up.
    #[must_use]
    pub const fn hosts_open(&self) -> bool {
        self.bar.hosts_open
    }

    /// Open or close the hosts popover.
    pub fn toggle_hosts(&mut self, cx: &mut Context<Self>) {
        self.bar.hosts_open = !self.bar.hosts_open;
        cx.notify();
    }

    /// The frame time's readout, worked out at most once a [`FRAME_READOUT_EVERY`]; nothing
    /// before the app's probe has timed a frame.
    fn frame_readout(&self, cx: &App) -> Option<SharedString> {
        let now = Instant::now();
        let mut readout = self.bar.frame_text.borrow_mut();
        let fresh = readout
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) < FRAME_READOUT_EVERY);
        if !fresh {
            let text = crate::frames::stats(cx)
                .filter(|s| s.frames > 0)
                .map(|s| format!("Frame {:.1} ms", s.draw_p50.as_secs_f64() * 1e3).into());
            *readout = Some((now, text));
        }
        readout.as_ref().and_then(|(_, text)| text.clone())
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

    /// What the bar's clock reads before it draws the bar again: the focused stream's rate.
    fn read_clock(&self, cx: &App) {
        self.bar.ticking.set(false);
        let focused = self.focused().map(|t| t.item);
        let rate = focused.and_then(|id| Some((id, self.screens.get(&id)?.read(cx).paint_rate())));
        self.bar.rate.set(rate);
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
            | ItemKind::Note { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. }
            | ItemKind::Review { .. }
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
        // The focused tile's upload is its header's to say, with the way to stop it; the bar
        // counts the rest.
        let focused_tile = self.focused();
        let elsewhere: Vec<&super::remote::Upload> =
            self.uploads.values().filter(|u| Some(u.tile) != focused_tile).collect();
        let transfers = (!phone && !elsewhere.is_empty()).then(|| {
            let (done, total) = elsewhere.iter().fold((0_u64, 0_u64), |(d, t), u| {
                (d.saturating_add(u.done), t.saturating_add(u.total))
            });
            let text: SharedString = transfers_label(elsewhere.len(), done, total).into();
            spaced(tabular(readout("status-transfers", text.clone())), &text, theme)
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
        let workers = (!phone).then(|| self.workers_button(cx)).flatten();
        // Every worker's agents on one quiet line: a mark in its state's tone and a count, the
        // worker named when there are more than one.
        let counts = self.agent_counts();
        let agents = (!counts.is_empty()).then(|| {
            let named = counts.len() > 1;
            let label: SharedString = agents_label(theme, &counts).into();
            let mut parts: Vec<gpui::AnyElement> = Vec::new();
            for (name, c) in &counts {
                if !parts.is_empty() {
                    parts.push(separator(theme).into_any_element());
                }
                if named {
                    parts.push(div().child(SharedString::from(name.clone())).into_any_element());
                }
                for (text, tone) in c.parts(theme) {
                    parts.push(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(spacing.xs))
                            .child(state_dot(theme, tone))
                            .child(tabular(div()).child(SharedString::from(text)))
                            .into_any_element(),
                    );
                }
            }
            readout("status-agents", label)
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .children(parts)
        });
        let facts = focused_item.and_then(|item| self.focus_facts(item, cx)).map(|text| {
            let text = SharedString::from(text);
            spaced(tabular(readout("status-facts", text.clone())), &text, theme)
        });
        // The link leads the right: it comes and goes with the pointer, and growing leftward
        // from the start of a cluster that is pinned right, it moves nothing else. The readouts
        // are parted by the faint dot the left's path steps are, never by space alone.
        let readouts = [
            link_readout.map(gpui::IntoElement::into_any_element),
            health.map(gpui::IntoElement::into_any_element),
            facts.map(gpui::IntoElement::into_any_element),
            ports.map(gpui::IntoElement::into_any_element),
            transfers.map(gpui::IntoElement::into_any_element),
            frame.map(gpui::IntoElement::into_any_element),
            workers.map(gpui::IntoElement::into_any_element),
            agents.map(gpui::IntoElement::into_any_element),
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
        let hosts = (self.bar.hosts_open && !phone).then(|| self.render_hosts(window, cx));
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
            .children(hosts)
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
                    .min_w(px(theme.typography.meta() * RTT_SLOT_EMS))
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

    /// "N workers" with a dot in the worst link's tone, only while any is not up (all up, the
    /// count says nothing worth the bar); opens the hosts, as the "…" menu's Workers does.
    fn workers_button(&self, cx: &Draw<'_, Self>) -> Option<Stateful<Div>> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let count = self.workers.len();
        let down: Vec<Status> = self
            .workers
            .values()
            .filter_map(|w| worker_health(&w.status))
            .map(|(m, _)| m)
            .collect();
        let worst = *down.first()?;
        let text = counted(count, "worker", "workers");
        let label = format!("{text}, {} not connected", down.len());
        let dot = state_dot(theme, state_fill(theme, worst))
            .debug_selector(|| "status-workers-dot".to_owned());
        let el = button("status-workers", label.into(), theme)
            .child(dot)
            .child(tabular(div()).child(SharedString::from(text)))
            .when(self.bar.hosts_open, |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)));
        Some(
            tab_stop(el, s.accent)
                .on_click(cx.listener(|this, _ev, _window, cx| this.toggle_hosts(cx))),
        )
    }

    /// The hosts popover over the bar's right end: each worker with its link, and what can be
    /// done to it. A click anywhere else closes it.
    fn render_hosts(&self, window: &Window, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let rows: Vec<gpui::AnyElement> =
            self.workers.keys().map(|key| self.host_row(*key, cx)).collect();
        let add = self.bar.add.clone().map(|run| {
            let el = div()
                .id("hosts-add")
                .debug_selector(|| "hosts-add".to_owned())
                .role(Role::Button)
                .aria_label("Add a worker")
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .mx(px(spacing.xs))
                .px(px(spacing.xs))
                .py(px(spacing.xs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
                .child(
                    div()
                        .flex_none()
                        .size(px(theme.typography.icon_large()))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(theme, IconName::Plus, IconSize::Inline, hsla(s.text_muted))),
                )
                .child("Add a worker");
            tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, window, cx| {
                this.bar.hosts_open = false;
                cx.notify();
                let run = Rc::clone(&run);
                cx.defer_in(window, move |_this, window, cx| run(window, cx));
            }))
        });
        let panel = kit::elevate(div(), theme)
            .id("hosts")
            .debug_selector(|| "hosts".to_owned())
            .role(Role::Dialog)
            .aria_label("Workers")
            .occlude()
            .absolute()
            .bottom(px(STATUSBAR_H + spacing.xs) + safe.bottom)
            .right(px(spacing.md) + safe.right)
            .w(px(HOSTS_W))
            .flex()
            .flex_col()
            .pb(px(spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(section_heading(theme, "hosts-heading".into(), "Workers"))
            .children(rows)
            .when(add.is_some(), |el| {
                el.child(div().my(px(spacing.xs)).h(px(1.0)).bg(hsla(s.border_subtle)))
            })
            .children(add);
        let viewport = window.viewport_size();
        gpui::deferred(
            gpui::anchored().position(gpui::point(px(0.0), px(0.0))).child(
                div()
                    .id("hosts-away")
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _ev, _w, cx| {
                            this.bar.hosts_open = false;
                            cx.notify();
                        }),
                    )
                    .child(kit::fade_in(panel, "hosts-fade", cx)),
            ),
        )
        .with_priority(crate::palette::Layer::Popover.priority())
        .into_any_element()
    }

    /// One worker in the hosts popover: its mark, its name, its round trip or what is wrong,
    /// and what can be done to it, under the pointer or while the row or the action holds the
    /// keyboard (a screen reader finds them in the tree either way). Clicked, it goes to the
    /// worker's tiles.
    fn host_row(&self, key: WorkerKey, cx: &Draw<'_, Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let Some(w) = self.workers.get(&key) else { return div().into_any_element() };
        let health = worker_health(&w.status);
        let group = SharedString::from(format!("hosts-row-{key}"));
        let mark = match health {
            Some((mark, _)) => {
                status_icon(theme, mark, px(theme.typography.icon()), hsla(mark.tone(theme)))
            }
            None => icon(theme, IconName::Server, IconSize::Inline, hsla(s.text_muted))
                .into_any_element(),
        };
        let path = w.relay.path().filter(|_| health.is_none()).map(path_label);
        // The machine under the name, once the worker has said what it is.
        let machine =
            w.caps.as_ref().filter(|c| !c.os_version.is_empty()).map(|c| host_line(c, w.load));
        let machine_known = machine.is_some();
        let detail = match health {
            Some((mark, word)) => div().text_color(hsla(mark.tone(theme))).child(word),
            None => div()
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .children(path.clone().map(|(text, slow)| {
                    div()
                        .debug_selector(move || format!("hosts-path-{key}"))
                        .text_color(hsla(if slow { s.warn } else { s.text_muted }))
                        .child(SharedString::from(text))
                }))
                .child(
                    tabular(div().text_color(hsla(s.text_muted)))
                        .children(self.shown_rtt(w).map(|rtt| SharedString::from(rtt_label(rtt)))),
                ),
        };
        let actions = self.bar.hosts.get(&key).cloned().unwrap_or_default();
        let action = |id: String, label: &'static str, run: MenuRun| {
            let selector = id.clone();
            let group = group.clone();
            let el = div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .px(px(spacing.xs))
                .rounded(px(theme.radii.xs))
                .cursor_pointer()
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.overlay)).text_color(hsla(s.text)))
                .child(label);
            // Hidden one by one, not by the strip that holds them: a hidden parent hides its
            // children whatever their own focus says.
            let el = tab_stop(el, s.accent)
                .invisible()
                .group_hover(group, gpui::Styled::visible)
                .in_focus(gpui::Styled::visible)
                // In place of the stop's own, which this replaces: its ring, and in view.
                .focus_visible(move |st| st.outline_ring(crate::a11y::ring(s.accent)).visible());
            el.on_click(cx.listener(move |this, _ev, window, cx| {
                cx.stop_propagation();
                this.bar.hosts_open = false;
                cx.notify();
                let run = Rc::clone(&run);
                cx.defer_in(window, move |_this, window, cx| run(window, cx));
            }))
        };
        let connect = actions
            .connect
            .filter(|_| !w.status.is_up())
            .map(|run| action(format!("hosts-connect-{key}"), "Connect", run));
        let wake = actions.wake.map(|run| action(format!("hosts-wake-{key}"), "Wake", run));
        let forget = actions.forget.map(|run| action(format!("hosts-forget-{key}"), "Forget", run));
        let hover_actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .children(wake)
            .children(connect)
            .children(forget);
        let label = SharedString::from(match health {
            Some((_, word)) => format!("{}, {word}", w.name),
            None => {
                [Some(w.name.clone()), path.map(|(text, _)| text), self.shown_rtt(w).map(rtt_label)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        let el = div()
            .id(ElementId::Name(format!("hosts-row-{key}").into()))
            .debug_selector(move || format!("hosts-row-{key}"))
            .group(group)
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .h(px(if machine_known { kit::Row::Two } else { kit::Row::One }.height(theme)))
            .mx(px(spacing.xs))
            .px(px(spacing.xs))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.raised)))
            .child(
                div()
                    .flex_none()
                    .size(px(theme.typography.icon_large()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mark),
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
                            .child(SharedString::from(w.name.clone())),
                    )
                    .children(machine.map(|line| {
                        meta(div(), theme)
                            .debug_selector(move || format!("hosts-machine-{key}"))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(line)
                    })),
            )
            .child(hover_actions)
            .child(div().flex_none().text_size(px(theme.typography.small())).child(detail));
        tab_stop(el, s.accent)
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                this.bar.hosts_open = false;
                this.go_to_worker(key, cx);
            }))
            .into_any_element()
    }
}

/// `text` in sentence case: the app and the link words come lowercase.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// The fill a state's dot wears: the brighter hue meant for marks, where its tone is meant
/// for text.
const fn state_fill(theme: &Theme, status: Status) -> slopty_theme::Rgb {
    let s = &theme.surfaces;
    match status {
        Status::Idle | Status::Working | Status::Running | Status::Away => s.text_muted,
        Status::NeedsYou => s.warn_fill,
        Status::Done => s.accent_fill,
        Status::Failed => s.error_fill,
    }
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
        .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readouts_say_what_they_count() {
        let theme = Theme::default();
        let mut one = AgentCounts::default();
        for status in [Status::Working, Status::Working, Status::NeedsYou, Status::Idle] {
            one.add(status);
        }
        assert_eq!(agents_label(&theme, &[("studio".into(), one)]), "2 working, 1 blocked");
        let mut other = AgentCounts::default();
        other.add(Status::Running);
        other.add_review();
        assert_eq!(
            agents_label(&theme, &[("studio".into(), one), ("mini".into(), other)]),
            "studio: 2 working, 1 blocked; mini: 1 waiting, 1 to review",
            "each worker named once there are two"
        );
        let mut rest = AgentCounts::default();
        rest.add(Status::Idle);
        rest.add(Status::Done);
        assert!(rest.is_empty(), "an agent at rest is not counted");
        assert_eq!(transfers_label(1, 42, 100), "1 upload · 42%");
        assert_eq!(transfers_label(2, 0, 0), "2 uploads · 0%");
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
