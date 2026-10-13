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
//! the worker's: it opens at the port that forwards it here, or the notice says none does.
//!
//! A path ⌘-clicked in a shell opens where a transcript's file link opens: in the Files pane of
//! the chat the shell was opened for ([`WorkspaceEvent::OpenFile`]), made absolute against the
//! shell's directory.

use std::time::{Duration, Instant};

use gpui::Context;
use slopty_client::WorkerKey;
use slopty_client::handoff::{self, Todo};
use slopty_core::SessionId;
use slopty_proto::handoff::{HandoffEvent, OfferReason, Wary};

use super::{WorkspaceEvent, WorkspaceView};

/// How long a held-back page's notice stays: longer than a word, since it waits on a choice.
pub const OFFER_FOR: Duration = Duration::from_secs(20);

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
                let local = matches!(why, OfferReason::Wary(Wary::Loopback))
                    .then(|| self.forwarded(key, &url))
                    .flatten();
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.send(reply);
                let asker = self.asker(key, open.session);
                let text = format!("{asker} wants to open {host} ({why})");
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
        }
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
        let chat = w
            .doc
            .item_for_session(session)
            .and_then(|item| item.facts.get(super::CHAT_FACT).cloned());
        let Some(chat) = chat else {
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

    /// The page `url` on `key`'s loopback, at the port that forwards it here.
    fn forwarded(&self, key: WorkerKey, url: &str) -> Option<String> {
        let (port, path) = on_loopback(url)?;
        let local = self
            .ports
            .iter()
            .filter(|(session, _)| self.worker_of_session(**session) == Some(key))
            .flat_map(|(_, forwards)| forwards)
            .find(|f| f.port.number == port)?
            .local?;
        Some(format!("http://localhost:{local}{path}"))
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

/// `path` as the shell printed it, absolute: as it is, under the worker's home for `~/`, else
/// under the shell's directory `cwd`.
fn absolute(path: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
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
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_theme::Theme;

    use super::{absolute, escaped, on_loopback};
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, hear, link, mac};

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

    fn shell(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: String::new(),
            cwd: Some("/w/app".to_owned()),
            repo: None,
            branch: None,
            repo_id: None,
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 1,
            command: Vec::new(),
            progress: None,
            restored: None,
            program: Vec::new(),
        }
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
        let opens = heard.borrow().offers.last().map(|o| o.1.clone());
        assert_eq!(opens.as_deref(), Some("http://localhost:15173/app?x=1"), "at its forward");

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
}
