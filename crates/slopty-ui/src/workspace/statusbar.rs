//! The bar along the bottom: where the focused tile runs, and how things are going.
//!
//! On the left, where the human is: the server's state while it does not answer, then the
//! focused tile's worker (a warn dot before it while it is away), its directory (inside a
//! repository, the repository's name and the path within it) and the branch checked out there
//! with what the working tree changed, the steps a faint "·" apart. The worker leads in the
//! secondary tone, the rest are muted. On the right, the link to the focused worker while it is
//! worth a look (a round trip past [`RTT_SHOWN_FROM`], or a DERP relay) or while the pointer is
//! over the bar, what is wrong with the link when something is, what the focused tile says of
//! itself (a file's language and caret; a page's host is its header's), the ports forwarded
//! here (which list them when clicked), the uploads in flight to tiles other than the focused
//! one (whose header says its own), the count of workers only while one of them is not up
//! (which opens the hosts popover: each worker's link, connect and forget), and the agents at
//! work off screen (a tile in view says its own), each a faint "·" from the next. Who waits on
//! the human is counted once, on the bell. The frame time shows only with the stream stats (⌘⇧I).
//! Each readout is meta text with no icon, its figures tabular, and a state is a small dot of its
//! fill beside quiet words: the tile and the bell carry the loud marks. A phone keeps the worker, a
//! slow round trip and the agents.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again, nor
//! does a round trip nobody would read.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Div, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Task,
    Window, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_core::SessionId;
use slopty_proto::items::{Item, ItemKind};
use slopty_theme::Theme;

use super::navigator::{Mode, RTT_SHOWN_FROM, host_line, path_label, rtt_label, worker_health};
use super::rollup::{META_SEPARATOR, meta_line};
use super::tile::repo_place;
use super::{MenuRun, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize, Status, icon, status_icon};
use crate::kit::{self, meta, separator, tabular};
use crate::palette::section_heading;

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
}

impl std::fmt::Debug for HostActions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostActions")
            .field("connect", &self.connect.is_some())
            .field("forget", &self.forget.is_some())
            .finish()
    }
}

/// The status bar's own state.
#[derive(Default)]
pub(super) struct Bar {
    /// The frame time's readout, and when it was worked out.
    frame_text: Option<(Instant, Option<SharedString>)>,
    /// The hosts popover is up.
    hosts_open: bool,
    /// What the popover can do to each worker, as the app says.
    hosts: HashMap<WorkerKey, HostActions>,
    /// The popover's way to add a worker, as the app says.
    add: Option<MenuRun>,
    /// Draws the bar again when the frame time's readout is due, while the stats show.
    tick: Option<Task<()>>,
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

/// The agents at work: `2 working`; empty when none is. Who waits on the human is counted
/// once, on the bell.
#[must_use]
fn agent_summary(working: usize) -> String {
    if working == 0 { String::new() } else { format!("{working} working") }
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

    /// Agents busy on their own, across every worker, as their tiles mark them, but for those
    /// whose tile is on screen: its header says so already, as the focused tile's upload is
    /// its header's to say.
    pub(super) fn working_count(&self, cx: &App) -> usize {
        let sessions = self
            .agents
            .keys()
            .chain(self.server_agents.keys().filter(|s| !self.agents.contains_key(s)));
        let shown = |s: SessionId| {
            self.tile_of_session(s).is_some_and(|t| self.on_screen.contains(&t.item))
        };
        sessions
            .filter(|s| !shown(**s) && self.agent_mark(**s, cx) == Some(Status::Working))
            .count()
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
    fn frame_readout(&mut self, cx: &App) -> Option<SharedString> {
        let now = Instant::now();
        let fresh = self
            .bar
            .frame_text
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) < FRAME_READOUT_EVERY);
        if !fresh {
            let text = crate::frames::stats(cx)
                .filter(|s| s.frames > 0)
                .map(|s| format!("Frame {:.1} ms", s.draw_p50.as_secs_f64() * 1e3).into());
            self.bar.frame_text = Some((now, text));
        }
        self.bar.frame_text.as_ref().and_then(|(_, text)| text.clone())
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
                Some(meta_line([file.coloured_as(), Some(caret.as_str())]))
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                let screen = self.screens.get(&item.id)?.read(cx);
                let (w, h) = screen.size();
                (screen.frames() > 0)
                    .then(|| format!("{w}\u{d7}{h} \u{b7} {:.0} fps", screen.painted_fps()))
            }
            ItemKind::Terminal { session } if self.agent_state(*session).is_none() => {
                self.running_for(*session, cx).map(|ran| format!("Running {}", kit::duration(ran)))
            }
            // A page's host is its header's place: said there, not twice.
            ItemKind::Terminal { .. }
            | ItemKind::Note { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. } => None,
        }
    }

    /// Whether what the bar says of the focused tile changes with the clock: a command's
    /// running time, a stream's rate.
    fn focus_clocked(&self, cx: &App) -> bool {
        let Some(item) = self.focused().and_then(|t| self.item(t)) else { return false };
        match item.kind {
            ItemKind::Window { .. } | ItemKind::Display { .. } => true,
            ItemKind::Terminal { session } => self.running_for(session, cx).is_some(),
            _ => false,
        }
    }

    /// The bar, or nothing before the first worker.
    pub(super) fn render_statusbar(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        if self.workers.is_empty() {
            return gpui::Empty.into_any_element();
        }
        let phone = matches!(self.nav.drawn, Some(Mode::Drawer))
            || self.width(window) < self.layout.config().phone_below;
        let stats = self.show_stats && !phone;
        let frame = stats.then(|| self.frame_readout(cx)).flatten();
        // The frame time, a command's running time and a stream's rate change with the clock.
        let clocked = stats || self.focus_clocked(cx);
        self.bar.tick = clocked.then(|| {
            let bar = self.chrome.statusbar.downgrade();
            cx.spawn(async move |_this, cx| {
                cx.background_executor().timer(FRAME_READOUT_EVERY).await;
                let _gone = bar.update(cx, |_, cx| cx.notify());
            })
        });
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();

        // A state is a dot of its fill beside its words; no readout wears an icon. The
        // server's word says what it costs: only the workers' own addresses reach them now.
        let server = self.server_status.clone().map(|text| {
            let text = SharedString::from(sentence(&text));
            readout("server-status", text.clone())
                .aria_description(SERVER_DOWN_MEANS)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(hsla(s.text_secondary))
                .child(state_dot(theme, s.warn_fill))
                .child(text)
                .child(separator(theme))
                .child(SERVER_DOWN_MEANS)
        });
        let worker = self.status_worker();
        let link = worker.and_then(|k| self.workers.get(&k));
        let focused_item = (!phone).then(|| self.focused().and_then(|t| self.item(t))).flatten();
        let session = focused_item.and_then(|item| match item.kind {
            ItemKind::Terminal { session } => self.summary(session),
            _ => None,
        });
        let cwd = focused_item
            .filter(|item| matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::File { .. }))
            .and_then(|item| self.cwd_of(item));
        // The worker leads the path in the secondary tone: with one worker too, since where a
        // shell runs is the first thing a remote tool says.
        let name = link.map(|w| {
            let away = (!w.status.is_up()).then(|| {
                state_dot(theme, s.warn_fill).debug_selector(|| "status-worker-away".to_owned())
            });
            readout("status-worker", SharedString::from(w.name.clone()))
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(hsla(s.text_secondary))
                .children(away)
                .child(SharedString::from(w.name.clone()))
        });
        let cwd = cwd.map(|cwd| {
            let repo = session.and_then(|s| s.repo.as_deref());
            SharedString::from(repo_place(&cwd, repo, worker.and_then(|k| self.home_of(k))))
        });
        let cwd = cwd.map(|cwd| {
            readout("status-cwd", cwd.clone())
                .flex_initial()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(cwd)
        });
        // The branch, then what the working tree changed, as every diff's size is drawn: only
        // the signs in a diff's tones, so the bar holds no green or red words.
        let changes = session.and_then(|s| s.changes).filter(|c| c.files > 0).and_then(|c| {
            let size = kit::changes(theme, c.added, c.removed)?;
            let label = kit::changes_text(c.added, c.removed).unwrap_or_default();
            Some(readout("status-changes", SharedString::from(label)).child(size))
        });
        let branch = session.and_then(|s| s.branch.clone()).map(|branch| {
            let branch = SharedString::from(branch);
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing.sm))
                .child(
                    readout("status-branch", branch.clone())
                        .aria_label(SharedString::from(format!("branch {branch}")))
                        .child(branch),
                )
                .children(changes)
        });
        // The server's word while it is down, then worker · directory · branch: one path, its
        // steps a faint dot apart, as the right's readouts are.
        let step = || separator(theme);
        let mut place_parts: Vec<gpui::AnyElement> = Vec::new();
        for part in [
            server.map(gpui::IntoElement::into_any_element),
            name.map(gpui::IntoElement::into_any_element),
            cwd.map(gpui::IntoElement::into_any_element),
            branch.map(gpui::IntoElement::into_any_element),
        ]
        .into_iter()
        .flatten()
        {
            if !place_parts.is_empty() {
                place_parts.push(step().into_any_element());
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
        let link_readout = link.filter(|w| w.status.is_up()).and_then(|w| {
            let path = w.path.as_ref().map(path_label);
            let relayed = path.as_ref().is_some_and(|(_, slow)| *slow);
            let shown =
                self.bar.hovered || relayed || w.rtt.is_some_and(|rtt| rtt >= RTT_SHOWN_FROM);
            shown.then(|| self.render_link(path, w.rtt))
        });
        // The link says something only when it is not up: a word in its tone, the mark being
        // on the left.
        let health = link.and_then(|w| worker_health(&w.status)).map(|(mark, word)| {
            let word = SharedString::from(sentence(word));
            readout("status-link", word.clone()).text_color(hsla(mark.tone(theme))).child(word)
        });
        let frame = frame.map(|text| tabular(readout("status-frame", text.clone())).child(text));
        let workers = (!phone).then(|| self.workers_button(cx)).flatten();
        let working = self.working_count(cx);
        let agents = (working > 0).then(|| {
            let text: SharedString = agent_summary(working).into();
            readout("status-agents", text.clone())
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .child(state_dot(theme, s.accent_fill))
                .child(tabular(div()).child(text))
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
            .border_t_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                if this.bar.hovered != *hovered {
                    this.bar.hovered = *hovered;
                    App::notify(cx, statusbar);
                }
            }));
        meta(bar, theme).child(left).child(right).children(hosts).into_any_element()
    }

    /// The link to the focused worker: how the tailnet carries it (a DERP relay in the
    /// warning tone, being the slow path) and its round trip, warning past
    /// [`crate::screen::RTT_WARN_FROM`]. The figure sits in a slot as wide as any it shows,
    /// pinned at its right, so a new sample moves only its own digits; its unit says what it
    /// is, and a screen reader hears the words.
    fn render_link(&self, path: Option<(String, bool)>, rtt: Option<Duration>) -> Div {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let path = path.map(|(text, slow)| {
            let text = SharedString::from(text);
            readout("status-path", text.clone())
                .when(slow, |el| el.text_color(hsla(s.warn)))
                .child(text)
        });
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
    fn workers_button(&self, cx: &Context<Self>) -> Option<Stateful<Div>> {
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
    fn render_hosts(&self, window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
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
                let run = std::rc::Rc::clone(&run);
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
    /// and, under the pointer, what can be done to it. Clicked, it goes to the worker's tiles.
    fn host_row(&self, key: WorkerKey, cx: &Context<Self>) -> gpui::AnyElement {
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
        let path = w.path.as_ref().filter(|_| health.is_none()).map(path_label);
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
                        .children(w.rtt.map(|rtt| SharedString::from(rtt_label(rtt)))),
                ),
        };
        let actions = self.bar.hosts.get(&key).cloned().unwrap_or_default();
        let action = |id: String, label: &'static str, run: MenuRun| {
            let selector = id.clone();
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
            tab_stop(el, s.accent).on_click(cx.listener(move |this, _ev, window, cx| {
                cx.stop_propagation();
                this.bar.hosts_open = false;
                cx.notify();
                let run = std::rc::Rc::clone(&run);
                cx.defer_in(window, move |_this, window, cx| run(window, cx));
            }))
        };
        let connect = actions
            .connect
            .filter(|_| !w.status.is_up())
            .map(|run| action(format!("hosts-connect-{key}"), "Connect", run));
        let forget = actions.forget.map(|run| action(format!("hosts-forget-{key}"), "Forget", run));
        let hover_actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .invisible()
            .group_hover(group.clone(), gpui::Styled::visible)
            .children(connect)
            .children(forget);
        let label = SharedString::from(match health {
            Some((_, word)) => format!("{}, {word}", w.name),
            None => [Some(w.name.clone()), path.map(|(text, _)| text), w.rtt.map(rtt_label)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
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
        Status::Idle => s.text_muted,
        Status::Working => s.accent_fill,
        Status::Running => s.text_secondary,
        Status::NeedsYou | Status::Away => s.warn_fill,
        Status::Done => s.success_fill,
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
        assert_eq!(agent_summary(2), "2 working");
        assert_eq!(agent_summary(0), "", "waiting is the bell's to count");
        assert_eq!(transfers_label(1, 42, 100), "1 upload · 42%");
        assert_eq!(transfers_label(2, 0, 0), "2 uploads · 0%");
        assert_eq!(counted(1, "port", "ports"), "1 port");
        assert_eq!(counted(3, "worker", "workers"), "3 workers");
        assert_eq!(sentence("server unreachable"), "Server unreachable");
        assert_eq!(sentence(""), "");
    }

    /// Inside a repository the bar names it and the path within; elsewhere, the tail.
    #[test]
    fn a_place_in_a_repository_is_named_by_it() {
        assert_eq!(repo_place("/w/oss/slopty", Some("/w/oss/slopty"), None), "slopty");
        assert_eq!(
            repo_place("/w/oss/slopty/crates/ui/", Some("/w/oss/slopty"), None),
            "slopty/crates/ui"
        );
        assert_eq!(repo_place("/w/oss/slopty-two", Some("/w/oss/slopty"), None), "oss/slopty-two");
        assert_eq!(repo_place("/Users/me/src", None, None), "~/src");
    }
}
