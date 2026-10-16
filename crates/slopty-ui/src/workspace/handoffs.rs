//! A web page a program in a worker's shell hands this client (`$BROWSER`, `open`, `xdg-open`),
//! and a page on a worker's loopback a shell's link points to.
//!
//! This client takes pages, not edits: the shell shows files in its own Files pane, which
//! nothing outside it opens, so an `$EDITOR` a shell runs stays the worker's (`vi`) rather than
//! waiting on a client that cannot show it. `slopty_client::handoff` decides whether a page
//! opens or is offered; this is where it happens. One that opens goes to the person's browser,
//! behind a remote window that has the keyboard so the stream keeps the screen. One that is
//! offered is a notice naming its host with an Open button ([`WorkspaceEvent::Offer`]), never
//! opened unasked. A page on the worker's own loopback (a dev server's `localhost:5173`) is
//! the worker's: the notice names the address with its port and the worker it is on, and says
//! the port that forwards it here, where it opens, or that none does yet ([`loopback_offer`]).
//!
//! A path ⌘-clicked in a shell opens where a transcript's file link opens: in the Files pane of
//! the chat the shell was opened for ([`WorkspaceEvent::OpenFile`]), made absolute against the
//! shell's directory.
//!
//! An agent's ask of the window (`slopty ui`) is read here into the shell's own words
//! (`slopty_shell::shell::agent_ui`): who asks (its chat's title), its chat (the thread seated
//! at its session, or the chat a shell was opened for), its machine, a file as a link on that
//! machine, and a page on the worker's loopback at the port that forwards it here. The shell
//! does it, and its answer goes back to the worker.

use std::time::{Duration, Instant};

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_client::handoff::{self, Todo};
use slopty_core::SessionId;
use slopty_proto::handoff::{
    HandoffEvent, OfferReason, UiAct, UiAsk, UiChange, UiChat, UiDone, UiError, UiFailure,
    UiRegion, UiTab, UiTarget, UiWindow, Wary,
};
use slopty_shell::shell::agent_ui;

use super::{WorkspaceEvent, WorkspaceView};

/// How long a held-back page's notice stays: longer than a word, since it waits on a choice.
pub const OFFER_FOR: Duration = Duration::from_secs(20);

/// What the notice says of a page on worker `machine`'s own loopback that `asker` wants.
///
/// `host` and `port` are as the address names them, and `forward` the port on this device
/// that forwards it here, if one does yet.
fn loopback_offer(
    asker: &str,
    machine: &str,
    host: &str,
    port: u16,
    forward: Option<u16>,
) -> String {
    let wants = if asker.is_empty() || asker == machine {
        format!("{machine} wants to open its own {host}:{port}")
    } else {
        format!("{asker} wants to open {host}:{port} on {machine}")
    };
    match forward {
        Some(here) => format!("{wants}; it opens here at localhost:{here}"),
        None => format!("{wants}; nothing forwards that port here yet"),
    }
}

/// The page `url` names on a worker's loopback: its port, and the rest after the host.
fn on_loopback(url: &str) -> Option<(u16, String)> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let loopback = match parsed.host()? {
        url::Host::Domain(name) => name == "localhost" || name.ends_with(".localhost"),
        url::Host::Ipv4(ip) => ip.is_loopback(),
        url::Host::Ipv6(ip) => ip.is_loopback(),
    };
    if !loopback {
        return None;
    }
    let port = parsed.port_or_known_default()?;
    let mut path = parsed.path().to_owned();
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    if let Some(fragment) = parsed.fragment() {
        path.push('#');
        path.push_str(fragment);
    }
    Some((port, path))
}

impl WorkspaceView {
    /// Tell worker `key` which handoffs this client takes: pages. Sent first on every link.
    pub(super) fn declare_handoffs(&mut self, key: WorkerKey) {
        if let Some(w) = self.workers.get_mut(&key) {
            w.send(handoff::declare(true, false));
        }
    }

    /// A program in a shell on `key` handed this client a page, or took one back; the link
    /// read it at `received`.
    pub fn handoff_event(
        &mut self,
        key: WorkerKey,
        event: HandoffEvent,
        received: Instant,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        match w.handoffs.heard(event, false, received.elapsed()) {
            Todo::Open { url, reply } => {
                w.send(reply);
                self.open_page(&url, cx);
            }
            Todo::Offer { url, host, open, why, reply } => {
                // The worker's own loopback is not this machine's: the page it means is at the
                // port that forwards it here, offered all the same.
                let loopback = matches!(why, OfferReason::Wary(Wary::Loopback));
                let local = loopback.then(|| self.forwarded(key, &url)).flatten();
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.send(reply);
                let asker = self.asker(key, open.session);
                let port = loopback.then(|| on_loopback(&url)).flatten().map(|(port, _)| port);
                let text = match port {
                    Some(port) => {
                        let machine = self.workers.get(&key).map(|w| w.name.clone());
                        let forward = self.forward_port(key, port);
                        loopback_offer(&asker, &machine.unwrap_or_default(), &host, port, forward)
                    }
                    None => format!("{asker} wants to open {host} ({why})"),
                };
                cx.emit(WorkspaceEvent::Offer {
                    worker: key,
                    id: open.id,
                    text,
                    url: local.unwrap_or(url),
                });
            }
            Todo::Edit { reply, .. } | Todo::Refuse { reply } => {
                w.send(reply);
            }
            Todo::Withdraw { id } => cx.emit(WorkspaceEvent::Withdrawn { worker: key, id }),
            Todo::Ui(ask) => self.agent_ui(key, ask, cx),
        }
    }

    /// An agent on `key` asks something of the person's window: the shell does it, once the
    /// update that heard it is over, and its answer goes back to the worker.
    fn agent_ui(&mut self, key: WorkerKey, ask: UiAsk, cx: &mut Context<Self>) {
        let UiAsk { id, session, act } = ask;
        let answer = move |view: &mut Self, outcome: Result<UiDone, UiFailure>| {
            if let Some(w) = view.workers.get_mut(&key) {
                w.send(handoff::answer(id, outcome));
            }
        };
        let act = match self.shell_act(key, act) {
            Ok(act) => act,
            Err(failure) => return answer(self, Err(failure)),
        };
        let seated = session.and_then(|session| self.seated(key, session, cx));
        let who = seated
            .as_ref()
            .map(|(_, title)| title.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| format!("An agent on {}", self.asker(key, session)));
        let chat = seated.map(|(chat, _)| chat);
        let ask = agent_ui::Ask { act, chat, device: Some(key.to_string()), who };
        let view = cx.entity().downgrade();
        cx.defer(move |cx| {
            let answered: agent_ui::Answered = Box::new(move |outcome, cx| {
                let outcome = outcome.map(ui_done).map_err(ui_failure);
                let _gone = view.update(cx, |view, _| answer(view, outcome));
            });
            agent_ui::act(ask, answered, cx);
        });
    }

    /// The chat of the agent seated at `session` on `key` (a thread's seat, or the chat a
    /// shell was opened for) and its title.
    fn seated(
        &self,
        key: WorkerKey,
        session: SessionId,
        cx: &gpui::App,
    ) -> Option<(String, String)> {
        let seat = session.to_string();
        let found = self.held_hub(key).and_then(|hub| {
            let hub = hub.read(cx);
            let rows = &hub.threads().rows().rows;
            rows.values()
                .find(|row| row.facts.get(slopty_proto::agent::SEAT_FACT) == Some(&seat))
                .map(|row| (row.id, row.title.clone()))
        });
        if let Some((thread, title)) = found {
            return Some((self.chat_id(thread), title));
        }
        let chat = self.chat_of(session)?;
        let title = self.summary(session).map(|s| s.title.clone()).unwrap_or_default();
        Some((chat, title))
    }

    /// `act` in the shell's words: a file as a link on `key`, a page as this machine reaches it.
    fn shell_act(&self, key: WorkerKey, act: UiAct) -> Result<agent_ui::Act, UiFailure> {
        Ok(match act {
            UiAct::State => agent_ui::Act::State,
            UiAct::Terminal { folder, command, region } => {
                agent_ui::Act::Terminal { folder, command, region }
            }
            UiAct::Open(target) => agent_ui::Act::Open(self.shell_target(key, target)?),
            UiAct::Region { region, change } => {
                agent_ui::Act::Region { region, change: shell_change(change) }
            }
            UiAct::Chat { chat } => agent_ui::Act::Chat(chat),
            UiAct::Split(target) => agent_ui::Act::Split(
                target.map(|target| self.shell_target(key, target)).transpose()?,
            ),
        })
    }

    /// What the side pane shows for an agent on `key`: a file by its link, a web page only
    /// as a browser reads its address, one on the worker's loopback at the port forwarding it
    /// here.
    fn shell_target(
        &self,
        key: WorkerKey,
        target: UiTarget,
    ) -> Result<agent_ui::Target, UiFailure> {
        match target {
            UiTarget::File { path, .. } if !path.starts_with('/') => {
                Err(UiFailure::new(UiError::Unknown, format!("{path} is not an absolute path")))
            }
            UiTarget::File { path, line } => {
                let at = line.map_or_else(String::new, |line| format!("#L{line}"));
                Ok(agent_ui::Target::File(format!("file://{}{at}", escaped(&path))))
            }
            UiTarget::Diff => Ok(agent_ui::Target::Diff),
            UiTarget::Page { url } => {
                let Some(page) = slopty_proto::handoff::page(&url) else {
                    let message = format!("{url} is not an http or https address with a host");
                    return Err(UiFailure::new(UiError::Unknown, message));
                };
                match page.wary {
                    Some(why @ (Wary::UserInfo | Wary::Idn)) => {
                        let message = format!("{why}, so the window does not open it for an agent");
                        Err(UiFailure::new(UiError::Failed, message))
                    }
                    Some(Wary::Loopback) if !self.is_here(key) => self
                        .forwarded(key, &page.url)
                        .map(agent_ui::Target::Page)
                        .ok_or_else(|| {
                            let port = on_loopback(&page.url)
                                .map_or_else(String::new, |(port, _)| format!(" {port}"));
                            let name =
                                self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
                            let message = format!(
                                "the page is on {name}'s own loopback, and nothing forwards its \
                                 port{port} here yet"
                            );
                            UiFailure::new(UiError::Elsewhere, message)
                        }),
                    _ => Ok(agent_ui::Target::Page(page.url)),
                }
            }
        }
    }

    /// Whether worker `key` is this machine: this client reaches it on its loopback.
    fn is_here(&self, key: WorkerKey) -> bool {
        let Some(address) = self.workers.get(&key).and_then(|w| w.address.as_deref()) else {
            return false;
        };
        let host = address.trim_start_matches('[').trim_end_matches(']');
        host.eq_ignore_ascii_case("localhost")
            || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    /// A shell's link to a page on its worker's loopback was followed: it opens at the port
    /// that forwards it here.
    pub(super) fn open_worker_page(&self, session: SessionId, url: &str, cx: &mut Context<Self>) {
        let Some(key) = self.worker_of_session(session) else { return };
        if let Some(here) = self.forwarded(key, url) {
            self.open_page(&here, cx);
        } else {
            let port = on_loopback(url).map_or_else(String::new, |(port, _)| format!(" {port}"));
            let name = self.workers.get(&key).map(|w| w.name.clone()).unwrap_or_default();
            Self::show_notice(format!("Nothing forwards {name}'s port{port} here yet"), cx);
        }
    }

    /// A path the person ⌘-clicked in shell `session`, at `line`: it opens in the Files pane of
    /// the chat the shell was opened for.
    pub(super) fn view_file(
        &self,
        session: SessionId,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.worker_of_session(session) else { return };
        let Some(w) = self.workers.get(&key) else { return };
        let Some(chat) = self.chat_of(session) else {
            Self::show_notice(format!("This shell has no chat to open {path} in"), cx);
            return;
        };
        let cwd = self.summary(session).and_then(|s| s.cwd.as_deref());
        let Some(absolute) = absolute(path, cwd, w.home.as_deref()) else {
            Self::show_notice(format!("Where {path} is, the shell has not said"), cx);
            return;
        };
        let at = line.map_or_else(String::new, |line| format!("#L{line}"));
        let target = format!("file://{}{at}", escaped(&absolute));
        cx.emit(WorkspaceEvent::OpenFile { chat, target });
    }

    /// A block or a selection the person attached in shell `session` goes to the draft of the
    /// chat the shell was opened for ([`WorkspaceEvent::Attach`]).
    pub(super) fn attach_to_chat(&self, session: SessionId, text: &str, cx: &mut Context<Self>) {
        match self.chat_of(session) {
            Some(chat) => cx.emit(WorkspaceEvent::Attach { chat, text: text.to_owned() }),
            None => Self::show_notice("This shell has no chat to attach to".to_owned(), cx),
        }
    }

    /// The chat shell `session` was opened for.
    pub(super) fn chat_of(&self, session: SessionId) -> Option<String> {
        let w = self.workers.get(&self.worker_of_session(session)?)?;
        w.doc.item_for_session(session)?.facts.get(super::CHAT_FACT).cloned()
    }

    /// The page `url` on `key`'s loopback, at the port that forwards it here.
    fn forwarded(&self, key: WorkerKey, url: &str) -> Option<String> {
        let (port, path) = on_loopback(url)?;
        let local = self.forward_port(key, port)?;
        Some(format!("http://localhost:{local}{path}"))
    }

    /// The port on this device that forwards `key`'s loopback port `port` here, if one does.
    fn forward_port(&self, key: WorkerKey, port: u16) -> Option<u16> {
        self.ports
            .iter()
            .filter(|(session, _)| self.worker_of_session(**session) == Some(key))
            .flat_map(|(_, forwards)| forwards)
            .find(|f| f.port.number == port)?
            .local
    }

    /// Open `url` in the person's browser: in front, unless a remote window or desktop has the
    /// keyboard, where it opens behind and the stream keeps the screen.
    pub(super) fn open_page(&self, url: &str, cx: &Context<Self>) {
        tracing::info!(url, "open a page a shell handed over");
        if self.screens.values().any(|view| view.read(cx).has_keyboard()) {
            slopty_platform::open_url_behind(url);
            return;
        }
        cx.open_url(url);
    }

    /// Who asks, as the notice names it: the shell's title, else the worker's name.
    fn asker(&self, key: WorkerKey, session: Option<SessionId>) -> String {
        session
            .and_then(|s| self.summary(s))
            .map(|s| s.title.clone())
            .filter(|title| !title.is_empty())
            .or_else(|| self.workers.get(&key).map(|w| w.name.clone()))
            .unwrap_or_default()
    }
}

const fn shell_change(change: UiChange) -> agent_ui::Change {
    match change {
        UiChange::Show => agent_ui::Change::Show,
        UiChange::Hide => agent_ui::Change::Hide,
        UiChange::Toggle => agent_ui::Change::Toggle,
        UiChange::Expand => agent_ui::Change::Expand,
        UiChange::Collapse => agent_ui::Change::Collapse,
    }
}

/// What the shell did, as the agent hears it.
fn ui_done(done: agent_ui::Done) -> UiDone {
    let tab = |tab: agent_ui::Tab| UiTab {
        id: tab.id,
        kind: tab.kind,
        title: tab.title,
        region: tab.region,
        selected: tab.selected,
        terminal: tab.terminal,
        target: tab.target,
    };
    let window = done.window;
    UiDone {
        did: done.did,
        made: done.made.map(tab),
        ran: done.ran,
        window: UiWindow {
            page: window.page,
            chat: window.chat.map(|chat| UiChat {
                id: chat.id,
                title: chat.title,
                machine: chat.machine,
            }),
            regions: window
                .regions
                .into_iter()
                .map(|r| UiRegion { name: r.name, shown: r.shown, expanded: r.expanded })
                .collect(),
            tabs: window.tabs.into_iter().map(tab).collect(),
            typing: window.typing,
        },
    }
}

/// Why the shell did nothing, as the agent hears it.
fn ui_failure(failure: agent_ui::Failure) -> UiFailure {
    let code = match failure.code {
        agent_ui::Code::NoWindow => UiError::NoWindow,
        agent_ui::Code::Unknown => UiError::Unknown,
        agent_ui::Code::Busy => UiError::Busy,
        agent_ui::Code::Elsewhere => UiError::Elsewhere,
        agent_ui::Code::Failed => UiError::Failed,
    };
    UiFailure::new(code, failure.message)
}

/// `path` as the shell printed it, absolute: as it is, under the worker's home for `~/`, else
/// under the shell's directory `cwd`.
pub(super) fn absolute(path: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    if path.starts_with('/') {
        return Some(path.to_owned());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(format!("{}/{rest}", home?.trim_end_matches('/')));
    }
    let rest = path.strip_prefix("./").unwrap_or(path);
    Some(format!("{}/{rest}", cwd?.trim_end_matches('/')))
}

/// `path` in a link: what would end it or be read as an escape, escaped.
fn escaped(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Instant;

    use gpui::{AppContext as _, TestAppContext};
    use slopty_client::tunnel::Forward;
    use slopty_core::{ItemId, SessionId, WallMs};
    use slopty_proto::ClientMsg;
    use slopty_proto::handoff::{
        EditFile, HandoffCaps, HandoffEvent, HandoffReply, OfferReason, OpenUrl, Wary,
    };
    use slopty_proto::items::{Item, ItemKind, ItemSync};
    use slopty_proto::orchestration::Port;
    use slopty_theme::Theme;

    use super::{absolute, escaped, loopback_offer, on_loopback};
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, hear, link, mac, shell};

    /// A page on a worker's loopback is named with its port and the worker it is on, never as
    /// this device's own, with the port that forwards it here or that none does yet.
    #[test]
    fn a_loopback_offer_names_its_port_its_machine_and_its_forward() {
        assert_eq!(
            loopback_offer("vite", "studio", "localhost", 5173, Some(15173)),
            "vite wants to open localhost:5173 on studio; it opens here at localhost:15173"
        );
        assert_eq!(
            loopback_offer("studio", "studio", "localhost", 5173, None),
            "studio wants to open its own localhost:5173; nothing forwards that port here yet"
        );
        assert_eq!(
            loopback_offer("", "studio", "[::1]", 8080, None),
            "studio wants to open its own [::1]:8080; nothing forwards that port here yet"
        );
    }

    /// A page on the loopback keeps its path and query; one elsewhere is not the worker's.
    #[test]
    fn a_loopback_page_keeps_its_way_in() {
        assert_eq!(on_loopback("http://localhost:5173/a?b=1#c"), Some((5173, "/a?b=1#c".into())));
        assert_eq!(on_loopback("http://127.0.0.1:8080"), Some((8080, "/".into())));
        assert_eq!(on_loopback("https://[::1]/x"), Some((443, "/x".into())));
        assert_eq!(on_loopback("http://app.localhost:3000/"), Some((3000, "/".into())));
        assert_eq!(on_loopback("https://github.com/"), None);
        assert_eq!(on_loopback("file:///etc/hosts"), None);
    }

    fn page(id: u64, session: SessionId, url: &str, offer: Option<OfferReason>) -> HandoffEvent {
        let asked_ms = WallMs::ZERO;
        HandoffEvent::Open(OpenUrl {
            id,
            session: Some(session),
            url: url.to_owned(),
            asked_ms,
            offer,
        })
    }

    /// The client takes pages and no edits, said first on the link. A page opens in the
    /// person's browser and is answered taken; one held back is a notice naming its host with
    /// what Open opens, a page on the worker's loopback at the port forwarding it here; an edit
    /// is refused, so the shell's own editor runs; a page taken back takes its notice down. A
    /// link to the worker's loopback opens at its forward, or says none forwards it.
    #[gpui::test]
    fn a_shell_hands_this_client_its_pages(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let heard = hear(&workspace, cx);
        let session = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let mut rx = link(&workspace, cx, mac(|_| {}), vec![shell(session)]);
        let mut sent = || std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<ClientMsg>>();
        let first = sent();
        assert_eq!(
            first.first(),
            Some(&ClientMsg::HandoffCaps(HandoffCaps { open: true, edit: false })),
            "{first:?}"
        );

        let hand = |event, cx: &mut gpui::VisualTestContext| {
            workspace.update(cx, |v, cx| v.handoff_event(KEY, event, Instant::now(), cx));
            cx.run_until_parked();
        };
        hand(page(1, session, "https://github.com/login/device", None), cx);
        assert_eq!(cx.opened_url().as_deref(), Some("https://github.com/login/device"));
        assert_eq!(sent(), [ClientMsg::Handoff(HandoffReply::Taken { id: 1 })]);

        hand(page(2, session, "https://x.test/", Some(OfferReason::NotTyped)), cx);
        let why = OfferReason::NotTyped;
        assert_eq!(sent(), [ClientMsg::Handoff(HandoffReply::Offered { id: 2, why })]);
        let offer = heard.borrow().offers.last().cloned().expect("offered");
        assert_eq!(
            offer,
            (format!("studio wants to open x.test ({why})"), "https://x.test/".into())
        );

        let forward = Forward {
            port: Port { number: 5173, pid: 1, process: "vite".into(), session: Some(session) },
            local: Some(15173),
        };
        workspace.update(cx, |v, cx| v.ports_changed(session, vec![forward], cx));
        hand(page(3, session, "http://localhost:5173/app?x=1", None), cx);
        let why = OfferReason::Wary(Wary::Loopback);
        assert_eq!(sent(), [ClientMsg::Handoff(HandoffReply::Offered { id: 3, why })]);
        let offer = heard.borrow().offers.last().cloned().expect("offered");
        assert_eq!(
            offer,
            (
                "studio wants to open its own localhost:5173; it opens here at localhost:15173"
                    .to_owned(),
                "http://localhost:15173/app?x=1".to_owned()
            ),
            "at its forward"
        );

        let edit =
            EditFile { id: 4, session: Some(session), path: "/r/x".into(), line: None, wait: true };
        hand(HandoffEvent::Edit(edit), cx);
        assert_eq!(sent(), [ClientMsg::Handoff(HandoffReply::Refused { id: 4 })]);

        hand(HandoffEvent::Withdrawn { id: 2 }, cx);
        assert_eq!(heard.borrow().withdrawn, [2]);

        workspace.update(cx, |v, cx| v.open_worker_page(session, "http://localhost:3000/", cx));
        let said = heard.borrow().notices.last().cloned();
        assert_eq!(said.as_deref(), Some("Nothing forwards studio's port 3000 here yet"));
        workspace.update(cx, |v, cx| v.open_worker_page(session, "http://127.0.0.1:5173/b", cx));
        assert_eq!(cx.opened_url().as_deref(), Some("http://localhost:15173/b"));
    }

    /// An agent's ask reaches the shell and its answer the worker: with no shell window up, the
    /// worker hears there is none.
    #[gpui::test]
    fn an_agents_ask_is_answered_to_its_worker(cx: &mut TestAppContext) {
        use slopty_proto::handoff::{UiAct, UiAsk, UiError};
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let session = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let mut rx = link(&workspace, cx, mac(|_| {}), vec![shell(session)]);
        while rx.try_recv().is_ok() {}
        let ask = UiAsk { id: 5, session: Some(session), act: UiAct::State };
        workspace.update(cx, |v, cx| {
            v.handoff_event(KEY, HandoffEvent::Ui(ask), Instant::now(), cx);
        });
        cx.run_until_parked();
        let sent: Vec<ClientMsg> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let [ClientMsg::Handoff(HandoffReply::Ui { id: 5, outcome })] = sent.as_slice() else {
            panic!("one answer: {sent:?}");
        };
        let failure = outcome.as_ref().as_ref().err().map(|failure| failure.code);
        assert_eq!(failure, Some(UiError::NoWindow), "{outcome:?}");
    }

    /// An agent's file is a link on its own machine; a page is read as a browser reads it, one
    /// on the worker's loopback at the port that forwards it here or refused while none does,
    /// and one with a name before its host refused.
    #[gpui::test]
    fn an_agents_targets_are_read_for_the_shell(cx: &mut TestAppContext) {
        use slopty_proto::handoff::{UiError, UiTarget};
        use slopty_shell::shell::agent_ui::Target;
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let session = SessionId::new();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(session)]);
        let forward = Forward {
            port: Port { number: 5173, pid: 1, process: "vite".into(), session: Some(session) },
            local: Some(15173),
        };
        workspace.update(cx, |v, cx| v.ports_changed(session, vec![forward], cx));
        let read = |target| workspace.read_with(cx, |v, _| v.shell_target(KEY, target));
        let page = |url: &str| UiTarget::Page { url: url.to_owned() };
        let file = UiTarget::File { path: "/w/a b.rs".to_owned(), line: Some(3) };
        assert_eq!(read(file), Ok(Target::File("file:///w/a%20b.rs#L3".to_owned())));
        let relative = UiTarget::File { path: "src/a.rs".to_owned(), line: None };
        assert_eq!(read(relative).map_err(|f| f.code), Err(UiError::Unknown));
        assert_eq!(
            read(page("http://localhost:5173/app?x=1")),
            Ok(Target::Page("http://localhost:15173/app?x=1".to_owned())),
            "at its forward"
        );
        assert_eq!(
            read(page("http://127.0.0.1:3000/")).map_err(|f| f.code),
            Err(UiError::Elsewhere)
        );
        assert_eq!(read(page("https://u@evil.test/")).map_err(|f| f.code), Err(UiError::Failed));
        assert_eq!(read(page("mailto:a@b.test")).map_err(|f| f.code), Err(UiError::Unknown));
        assert_eq!(
            read(page("https://example.com")),
            Ok(Target::Page("https://example.com/".into()))
        );
        assert_eq!(read(UiTarget::Diff), Ok(Target::Diff));
    }

    /// A path as a shell printed it is made absolute on its machine: as it is, under the home
    /// for `~/`, else under the shell's directory; what a link would misread is escaped.
    #[test]
    fn a_printed_path_is_made_absolute() {
        let (cwd, home) = (Some("/w/app/"), Some("/Users/me"));
        assert_eq!(absolute("/etc/hosts", cwd, home).as_deref(), Some("/etc/hosts"));
        assert_eq!(absolute("~/x.md", cwd, home).as_deref(), Some("/Users/me/x.md"));
        assert_eq!(absolute("./src/a.rs", cwd, home).as_deref(), Some("/w/app/src/a.rs"));
        assert_eq!(absolute("src/a.rs", None, home), None, "no directory said");
        assert_eq!(escaped("/a b/c#1?.md%"), "/a%20b/c%231%3F.md%25");
    }

    /// A path ⌘-clicked in a shell opened for a chat opens in that chat's Files, absolute on
    /// the shell's machine and at its line; a shell that is no chat's says so.
    #[gpui::test]
    fn a_path_clicked_in_a_shell_opens_in_its_chat_s_files(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let heard = hear(&workspace, cx);
        let (mine, loose) = (SessionId::new(), SessionId::new());
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(mine), shell(loose)]);
        let item = |session, chat: Option<&str>| Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: chat
                .map(|chat| BTreeMap::from([("chat".to_owned(), chat.to_owned())]))
                .unwrap_or_default(),
        };
        let items = vec![item(mine, Some("chat-1")), item(loose, None)];
        workspace
            .update(cx, |v, cx| v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items }, cx));
        cx.run_until_parked();

        workspace.update(cx, |v, cx| v.view_file(mine, "src/main rs.rs", Some(12), cx));
        let opened = heard.borrow().files.clone();
        assert_eq!(
            opened,
            [("chat-1".to_owned(), "file:///w/app/src/main%20rs.rs#L12".to_owned())]
        );
        workspace.update(cx, |v, cx| v.view_file(loose, "a.rs", None, cx));
        assert_eq!(heard.borrow().files.len(), 1, "no chat to open it in");
        let said = heard.borrow().notices.last().cloned();
        assert_eq!(said.as_deref(), Some("This shell has no chat to open a.rs in"));
    }

    /// A block or a selection attached in a shell opened for a chat goes to that chat's draft;
    /// a shell opened for no chat offers no attaching, and a key bound to it says so.
    #[gpui::test]
    fn a_block_attached_in_a_shell_goes_to_its_chat_s_draft(cx: &mut TestAppContext) {
        let workspace = cx.new(|_| WorkspaceView::new(Theme::default()));
        let cx = cx.add_empty_window();
        let heard = hear(&workspace, cx);
        let (mine, loose) = (SessionId::new(), SessionId::new());
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), vec![shell(mine), shell(loose)]);
        let item = |session, chat: Option<&str>| Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: chat
                .map(|chat| BTreeMap::from([("chat".to_owned(), chat.to_owned())]))
                .unwrap_or_default(),
        };
        let items = vec![item(mine, Some("chat-1")), item(loose, None)];
        workspace
            .update(cx, |v, cx| v.apply_sync(KEY, ItemSync::Snapshot { version: 1, items }, cx));
        cx.run_until_parked();

        let (shown, bare) = workspace
            .read_with(cx, |v, _| (v.terminals[&mine].clone(), v.terminals[&loose].clone()));
        let block = "```sh\n$ make\nok\n```";
        shown.update(cx, |_, cx| {
            cx.emit(slopty_terminal::TerminalViewEvent::Attach(block.to_owned()));
        });
        assert_eq!(heard.borrow().attached, [("chat-1".to_owned(), block.to_owned())]);

        cx.update(|window, cx| {
            bare.update(cx, |v, cx| v.attach_block(&slopty_terminal::AttachBlock, window, cx));
        });
        assert_eq!(heard.borrow().attached.len(), 1, "no chat to attach to");
        let said = heard.borrow().notices.last().cloned();
        assert_eq!(said.as_deref(), Some("No agent to attach the block to"));
    }
}
