//! The bar along the bottom: where the focused tile runs, and how things are going.
//!
//! On the left, where the human is: the server's state while it does not answer, then the
//! focused tile's worker (a warn dot before it while it is away), its directory (inside a
//! repository, the repository's name and the path within it) and the branch checked out there,
//! read as one path in the UI face. On the right, the ports forwarded here (which list them
//! when clicked), the uploads in flight, what is wrong with the focused worker's link when
//! something is (a link that is up says nothing), the round trip to it, the count of workers
//! only while one of them is not up (which opens the hosts popover: each worker's link,
//! connect and forget), and the agents at work. Who waits on the human is counted once, on the
//! bell. The frame time shows only with the stream stats (⌘⇧I). Each readout is meta text with
//! no icon, its figures tabular, and a state is a small dot of its fill beside quiet words: the
//! tile and the bell carry the loud marks. A phone keeps the worker, the round trip and the
//! agents.
//!
//! It is a view of its own, drawn cached: an echo in a terminal does not draw it again.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, ElementId, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Task,
    Window, div, px,
};
use slopty_client::layout::WorkerKey;
use slopty_proto::items::ItemKind;
use slopty_theme::Theme;

use super::navigator::{Mode, path_label, rtt_label, worker_health};
use super::{MenuRun, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize, Status, icon, status_icon};
use crate::kit::{self, meta, tabular};
use crate::palette::section_heading;

/// The bar's height.
pub(super) const STATUSBAR_H: f32 = 26.0;

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
}

impl std::fmt::Debug for Bar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bar")
            .field("hosts_open", &self.hosts_open)
            .field("hosts", &self.hosts)
            .field("add", &self.add.is_some())
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

/// Where a shell is, as the bar says it: inside a repository, the repository's name and the
/// path within it (`slopty`, `slopty/crates/ui`); elsewhere the tail the headers print.
#[must_use]
fn place(cwd: &str, repo: Option<&str>) -> String {
    let repo = repo.map(|r| r.trim_end_matches('/')).filter(|r| !r.is_empty());
    let within = repo.and_then(|repo| {
        let name = repo.rsplit('/').next().filter(|n| !n.is_empty())?;
        let rest = cwd.trim_end_matches('/').strip_prefix(repo)?;
        match rest.strip_prefix('/') {
            Some(rest) => Some(format!("{name}/{rest}")),
            None if rest.is_empty() => Some(name.to_owned()),
            None => None,
        }
    });
    within.unwrap_or_else(|| super::tile::cwd_tail(cwd))
}

impl WorkspaceView {
    /// The worker the status bar speaks for: the focused tile's, else the one a new tile
    /// would go to.
    pub(super) fn status_worker(&self) -> Option<WorkerKey> {
        self.focused().map(|t| t.worker).or_else(|| self.context_worker())
    }

    /// Agents busy on their own, across every worker.
    pub(super) fn working_count(&self) -> usize {
        let sessions = self
            .agents
            .keys()
            .chain(self.server_agents.keys().filter(|s| !self.agents.contains_key(s)));
        sessions
            .filter(|s| self.agent_state(**s).and_then(Status::of_agent) == Some(Status::Working))
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
    fn frame_readout(&mut self, cx: &gpui::App) -> Option<SharedString> {
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
        self.bar.tick = stats.then(|| {
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
        let muted = hsla(s.text_muted);

        // A state is a dot of its fill beside its words; no readout wears an icon.
        let server = self.server_status.clone().map(|text| {
            let text = SharedString::from(sentence(&text));
            readout("server-status", text.clone())
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(hsla(s.text_secondary))
                .child(state_dot(theme, s.warn_fill))
                .child(text)
        });
        let worker = self.status_worker();
        let link = worker.and_then(|k| self.workers.get(&k));
        // With one worker its name says nothing the window does not, until its link is down.
        let lone = self.workers.len() == 1;
        let name = link.filter(|w| !lone || !w.status.is_up()).map(|w| {
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
        let focused_item = (!phone).then(|| self.focused().and_then(|t| self.item(t))).flatten();
        let session = focused_item.and_then(|item| match item.kind {
            ItemKind::Terminal { session } => self.summary(session),
            _ => None,
        });
        let cwd = focused_item
            .filter(|item| matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::File { .. }))
            .and_then(|item| self.cwd_of(item))
            .map(|cwd| SharedString::from(place(&cwd, session.and_then(|s| s.repo.as_deref()))));
        let cwd = cwd.map(|cwd| {
            readout("status-cwd", cwd.clone())
                .flex_initial()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .child(cwd)
        });
        let branch = session.and_then(|s| s.branch.clone()).map(|branch| {
            let branch = SharedString::from(branch);
            readout("status-branch", branch.clone())
                .aria_label(SharedString::from(format!("branch {branch}")))
                .child(branch)
        });
        // Worker › directory › branch: one path, its steps a quiet chevron apart.
        let step = || div().flex_none().text_color(muted).child("\u{203a}");
        let mut place_parts: Vec<gpui::AnyElement> = Vec::new();
        for part in [
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
        let left = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .children(server)
            .child(
                div()
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
        let transfers = (!phone && !self.uploads.is_empty()).then(|| {
            let (done, total) = self.uploads.values().fold((0_u64, 0_u64), |(d, t), u| {
                (d.saturating_add(u.done), t.saturating_add(u.total))
            });
            let text: SharedString = transfers_label(self.uploads.len(), done, total).into();
            tabular(readout("status-transfers", text.clone())).child(text)
        });
        // How the tailnet carries the link, beside its round trip; a DERP relay in the warning
        // tone, being the slow path.
        let path = link.filter(|w| w.status.is_up()).and_then(|w| w.path.as_ref()).map(|path| {
            let (text, slow) = path_label(path);
            let text = SharedString::from(text);
            readout("status-path", text.clone())
                .when(slow, |el| el.text_color(hsla(s.warn)))
                .child(text)
        });
        // The round trip in its own slot, held from the link's first frame at the width of any
        // figure it will show: the first sample lands without moving what is left of it. The
        // figure alone; its unit says what it is, and a screen reader hears the words.
        let rtt = link.filter(|w| w.status.is_up()).map(|w| {
            let figure = w.rtt.map(|rtt| {
                let text = SharedString::from(rtt_label(rtt));
                tabular(readout("status-rtt", text.clone()))
                    .aria_label(SharedString::from(format!("Round trip {text}")))
                    .child(text)
            });
            div()
                .debug_selector(|| "status-rtt-slot".to_owned())
                .flex_none()
                .flex()
                .justify_end()
                .min_w(px(theme.typography.small() * RTT_SLOT_EMS))
                .children(figure)
        });
        // The link says something only when it is not up: a word in its tone, the mark being
        // on the left.
        let health = link.and_then(|w| worker_health(&w.status)).map(|(mark, word)| {
            let word = SharedString::from(sentence(word));
            readout("status-link", word.clone()).text_color(hsla(mark.tone(theme))).child(word)
        });
        let frame = frame.map(|text| tabular(readout("status-frame", text.clone())).child(text));
        let workers = (!phone).then(|| self.workers_button(cx)).flatten();
        let working = self.working_count();
        let agents = (working > 0).then(|| {
            let text: SharedString = agent_summary(working).into();
            readout("status-agents", text.clone())
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .child(state_dot(theme, s.accent_fill))
                .child(tabular(div()).child(text))
        });
        let right = div()
            .debug_selector(|| "status-right".to_owned())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .children(ports)
            .children(transfers)
            .children(health)
            .children(path)
            .children(rtt)
            .children(frame)
            .children(workers)
            .children(agents);
        let hosts = (self.bar.hosts_open && !phone).then(|| self.render_hosts(window, cx));
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
            .font_family(theme.typography.ui_family.clone());
        meta(bar, theme).child(left).child(right).children(hosts).into_any_element()
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
            .rounded(px(theme.radii.md))
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
            .h(px(kit::Row::One.height(theme)))
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
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(w.name.clone())),
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
        Status::NeedsYou | Status::Away => s.warn_fill,
        Status::Done => s.success_fill,
        Status::Failed => s.error_fill,
    }
}

/// A small dot of a state's fill beside a readout's words.
fn state_dot(theme: &Theme, fill: slopty_theme::Rgb) -> Div {
    div().flex_none().size(px(theme.spacing.xs + theme.spacing.xxs)).rounded_full().bg(hsla(fill))
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
        assert_eq!(place("/w/oss/slopty", Some("/w/oss/slopty")), "slopty");
        assert_eq!(place("/w/oss/slopty/crates/ui/", Some("/w/oss/slopty")), "slopty/crates/ui");
        assert_eq!(place("/w/oss/slopty-two", Some("/w/oss/slopty")), "oss/slopty-two");
        assert_eq!(place("/Users/me/src", None), "~/src");
    }
}
