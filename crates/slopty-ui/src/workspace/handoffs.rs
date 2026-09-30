//! What a program in a worker's shell hands this client, and what this client tells the
//! workers back about where the person is looking.
//!
//! A page a shell asks to open (`$BROWSER`, `open`, `xdg-open`) opens in this Mac's browser
//! when the person just typed into that shell; any other is held back in a notice naming its
//! host, with "Open" and "Dismiss". A file a shell asks to edit (`$EDITOR`) opens in a file
//! tile beside the shell, and when a program waits on it (`git commit`, `crontab -e`) the tile
//! says so and answers it with "Done" or "Give up". `slopty_client::handoff` decides which is
//! which; this is where it shows. The rulings are in `docs/decisions/terminal.md` ("A shell's
//! browser and editor are the client's").
//!
//! The shell this client has in front of the person is told to its worker
//! (`TermRequest::Focus`): the worker routes the next page or edit there, holds Claude Code's
//! phone pushes while it is looked at, and reports focus to the program (DEC 1004). A coding
//! agent's pull request and worktree ride on its tile's header.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AppContext as _, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use slopty_client::handoff::Todo;
use slopty_client::layout::WorkerKey;
use slopty_core::SessionId;
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentBranch, Review};
use slopty_proto::handoff::{EditFile, EditOutcome, HandoffEvent, HandoffId, OfferReason, Wary};
use slopty_proto::items::ItemKind;
use slopty_proto::terminal::TermRequest;

use super::WorkspaceView;
use super::actions::OpenLastOffer;
use super::toast::ToastKind;
use crate::colors::hsla;
use crate::file::FileView;

/// How long a held-back page's notice stays: longer than a word, since it waits on a choice.
const OFFER_FOR: std::time::Duration = std::time::Duration::from_secs(20);

/// What the workspace keeps of the handoffs and the focus it reported.
#[derive(Default)]
pub(super) struct HandoffState {
    /// Each agent session's pull request and worktree, as its worker last said.
    branches: HashMap<SessionId, AgentBranch>,
    /// The shell told it has this client's focus, and on which worker.
    focus: Option<(WorkerKey, SessionId)>,
    /// The page last held back, for "Open last offered page".
    last_offer: Option<String>,
}

/// A page held back in a notice.
#[derive(Clone, Debug)]
pub(super) struct Offer {
    /// The worker that asked.
    worker: WorkerKey,
    /// Its handoff.
    id: HandoffId,
    /// Who asked: the shell's tile title, else the worker's name.
    asker: String,
    /// The host a browser would visit, ASCII.
    host: String,
    /// The page, as parsed: what "Open" opens.
    url: String,
    /// Why it was held back.
    why: OfferReason,
    /// When this client read it off the link.
    received: Instant,
}

impl HandoffState {
    /// `session` is gone: so is its branch.
    pub(super) fn forget_session(&mut self, session: SessionId) {
        self.branches.remove(&session);
    }

    /// How many entries each map holds, for the leak check.
    #[cfg(test)]
    pub(super) fn sizes(&self) -> [(&'static str, usize); 1] {
        [("handoff.branches", self.branches.len())]
    }
}

impl Offer {
    /// Whether this is handoff `id` of `worker`.
    pub(super) fn is(&self, worker: WorkerKey, id: HandoffId) -> bool {
        self.worker == worker && self.id == id
    }

    /// The worker that asked, the handoff, and the page "Open" opens.
    pub(super) fn target(&self) -> (WorkerKey, HandoffId, String) {
        (self.worker, self.id, self.url.clone())
    }

    /// The host a browser would visit.
    #[cfg(test)]
    pub(super) fn host(&self) -> &str {
        &self.host
    }

    /// The notice's line, and what a screen reader hears of it.
    pub(super) fn line(&self) -> String {
        format!("{} wants to open {}", self.asker, self.host)
    }

    /// An address built to pass for another (a user name before an `@`, a look-alike
    /// international name): its host must be what the eye lands on.
    const fn deceptive(&self) -> bool {
        matches!(self.why, OfferReason::Wary(Wary::UserInfo | Wary::Idn))
    }

    /// Why it was held back and how long ago it was asked, in one muted line.
    fn detail(&self, now: Instant) -> String {
        let why = self.why.to_string();
        let mut chars = why.chars();
        let why = chars
            .next()
            .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect::<String>());
        let age = crate::palette::age_label(now.saturating_duration_since(self.received));
        let age = if age == "now" { "just now".to_owned() } else { format!("{age} ago") };
        format!("{why} \u{b7} {age}")
    }
}

/// The label of an agent's pull request chip: `#1234`, or `!1234` for a merge request.
#[must_use]
pub fn pr_label(pr: &slopty_proto::agent::PullRequest) -> String {
    let mark = if pr.merge_request { '!' } else { '#' };
    format!("{mark}{}", pr.number)
}

/// What a screen reader hears of the chip: "Pull request 1234, approved".
#[must_use]
pub fn pr_said(pr: &slopty_proto::agent::PullRequest) -> String {
    let kind = if pr.merge_request { "Merge request" } else { "Pull request" };
    let review = match pr.review {
        Some(Review::Approved) => ", approved",
        Some(Review::Pending) => ", waiting on review",
        Some(Review::ChangesRequested) => ", changes requested",
        Some(Review::Draft) => ", draft",
        None => "",
    };
    format!("{kind} {}{review}", pr.number)
}

impl WorkspaceView {
    /// Tell worker `key` which handoffs this client takes: pages always, files since the
    /// workspace shows file tiles. Sent on every link, first.
    pub(super) fn declare_handoffs(&self, key: WorkerKey) {
        self.send(key, slopty_client::handoff::declare(true, true));
    }

    /// A program in a shell on `key` handed this client a page or a file, or took one back;
    /// the link read it at `received`.
    pub fn handoff_event(
        &mut self,
        key: WorkerKey,
        event: HandoffEvent,
        received: Instant,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        match w.handoffs.heard(event, true, received.elapsed()) {
            Todo::Open { url, reply } => {
                self.open_page(&url, cx);
                self.send(key, reply);
            }
            Todo::Offer { url, host, open, why, reply } => {
                self.send(key, reply);
                let asker = self.asker(key, open.session);
                let offer = Offer { worker: key, id: open.id, asker, host, url, why, received };
                self.offer_page(offer, cx);
            }
            Todo::Edit { edit, reply, again } => {
                self.edit_file(key, &edit, again, cx);
                self.send(key, reply);
            }
            Todo::Refuse { reply } => self.send(key, reply),
            Todo::Withdraw { id } => self.withdraw_handoff(key, id, cx),
        }
        cx.notify();
    }

    /// Open `url` in the person's browser. In front, unless they are watching a remote window
    /// or display here: then it opens behind, and the stream keeps the screen.
    fn open_page(&self, url: &str, cx: &gpui::App) {
        tracing::info!(url, "open a page a shell handed over");
        if self.watching_a_stream() {
            slopty_platform::open_url_behind(url);
        } else {
            cx.open_url(url);
        }
    }

    /// Whether the tile in front of the person is a remote window or display.
    fn watching_a_stream(&self) -> bool {
        let tile = match self.popouts.active() {
            Some(item) => self.tile_of(item),
            None if self.app_active => self.focused(),
            None => None,
        };
        tile.and_then(|t| self.item(t))
            .is_some_and(|i| matches!(i.kind, ItemKind::Window { .. } | ItemKind::Display { .. }))
    }

    /// Who asks, as the notice names it: the shell's tile title, else the worker's name.
    fn asker(&self, key: WorkerKey, session: Option<SessionId>) -> String {
        session
            .and_then(|s| self.tile_of_session(s))
            .and_then(|t| self.item(t))
            .map(|i| self.tile_title(i))
            .or_else(|| self.workers.get(&key).map(|w| w.name.clone()))
            .unwrap_or_default()
    }

    /// Hold back a page in a notice naming its host.
    fn offer_page(&mut self, offer: Offer, cx: &mut Context<Self>) {
        tracing::info!(host = %offer.host, why = ?offer.why, "a page held back");
        self.handoff.last_offer = Some(offer.url.clone());
        self.show_toast_for(ToastKind::Offered(Box::new(offer)), OFFER_FOR, cx);
    }

    /// "Open" on a held-back page: open it and take its notice down. The worker was answered
    /// when the notice showed.
    pub(super) fn open_offered(
        &mut self,
        worker: WorkerKey,
        id: HandoffId,
        url: &str,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_offer(worker, id);
        self.open_page(url, cx);
        cx.notify();
    }

    /// The palette's "Open last offered page".
    pub fn open_last_offer(
        &mut self,
        _: &OpenLastOffer,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(url) = self.handoff.last_offer.clone() else {
            self.show_notice("No page was offered".to_owned(), cx);
            return;
        };
        self.drop_offers(|offer| offer.url == url);
        self.open_page(&url, cx);
        cx.notify();
    }

    /// Show `edit.path` in a file tile beside its shell, focused, at its line; and when a
    /// program waits on it, the tile says so until the person is done.
    fn edit_file(&mut self, key: WorkerKey, edit: &EditFile, again: bool, cx: &mut Context<Self>) {
        tracing::info!(path = %edit.path, wait = edit.wait, again, "edit a file a shell handed over");
        // A new tile opens right of the focused column: the shell's, once it has the focus.
        if let Some(shell) = edit.session.and_then(|s| self.tile_of_session(s)) {
            self.tick();
            self.layout.focus(shell);
        }
        let Some(item) = self.show_file(Some(key), &edit.path, edit.line, cx) else { return };
        // A view made later, once the tile is echoed, asks `waiting_on` itself.
        if edit.wait
            && let Some(view) = self.files.get(&item)
        {
            view.update(cx, |v, cx| v.set_waiting(Some(edit.id), cx));
        }
    }

    /// The worker took handoff `id` back: its notice goes, and a tile waiting on it stays as
    /// an ordinary file tile. An id not known here is nothing.
    fn withdraw_handoff(&mut self, key: WorkerKey, id: HandoffId, cx: &mut Context<Self>) {
        self.dismiss_offer(key, id);
        for view in self.waiting_views(key, id, cx) {
            view.update(cx, |v, cx| v.set_waiting(None, cx));
        }
    }

    /// The file views on `key` (closed ones too) waiting on handoff `id`.
    fn waiting_views(
        &self,
        key: WorkerKey,
        id: HandoffId,
        cx: &Context<Self>,
    ) -> Vec<gpui::Entity<FileView>> {
        let open = self
            .files
            .iter()
            .filter(|(item, _)| self.tile_of(**item).is_some_and(|t| t.worker == key))
            .map(|(_, view)| view);
        let closed =
            self.closed.iter().filter(|c| c.tile.worker == key).filter_map(|c| c.file.as_ref());
        open.chain(closed).filter(|v| v.read(cx).waiting() == Some(id)).cloned().collect()
    }

    /// A file tile on `worker` is done with handoff `id`: answer the program.
    pub(super) fn file_edited(&mut self, worker: WorkerKey, id: HandoffId, outcome: EditOutcome) {
        let Some(w) = self.workers.get_mut(&worker) else { return };
        if let Some(reply) = w.handoffs.edited(id, outcome) {
            tracing::info!(id, ?outcome, "answered a program waiting on a file");
            w.send(reply);
        }
    }

    /// The program on `worker` waiting on `path`, for a file view made after its handoff.
    pub(super) fn file_wait(&self, worker: WorkerKey, path: &str) -> Option<HandoffId> {
        self.workers.get(&worker)?.handoffs.waiting_on(path).map(|edit| edit.id)
    }

    /// Take down the notice of held-back page `id` from `worker`.
    pub(super) fn dismiss_offer(&mut self, worker: WorkerKey, id: HandoffId) {
        self.drop_offers(|offer| offer.is(worker, id));
    }

    /// A worker said where an agent's branch stands: its tile's chip follows.
    pub fn agent_branch(&mut self, branch: AgentBranch, cx: &mut Context<Self>) {
        if branch.pr.is_none() && branch.worktree.is_none() {
            self.handoff.branches.remove(&branch.session);
        } else {
            self.handoff.branches.insert(branch.session, branch);
        }
        cx.notify();
    }

    /// Where `session`'s agent branch stands, while an agent runs there.
    #[must_use]
    pub fn branch_of(&self, session: SessionId) -> Option<&AgentBranch> {
        self.agent_state(session)?;
        self.handoff.branches.get(&session)
    }

    /// Tell the workers which shell is in front of the person: the focused tile's, while the
    /// window is key and the app active, or the shell in a window of its own that has the
    /// keyboard. Each change is one `false` to the shell let go and one `true` to the new one.
    /// Runs on every change of the workspace; sends only changes.
    pub(super) fn sync_focus_report(&mut self) {
        let wanted = self.shell_in_front();
        if wanted == self.handoff.focus {
            return;
        }
        if let Some((key, session)) = self.handoff.focus.take() {
            self.send(key, focus_msg(session, false));
        }
        if let Some((key, session)) = wanted
            && self.workers.get(&key).is_some_and(|w| w.send(focus_msg(session, true)))
        {
            self.handoff.focus = wanted;
        }
    }

    /// The shell in front of the person, and its worker.
    fn shell_in_front(&self) -> Option<(WorkerKey, SessionId)> {
        let tile = match self.popouts.active() {
            Some(item) => self.tile_of(item)?,
            None if self.app_active => self.focused()?,
            None => return None,
        };
        match self.item(tile)?.kind {
            ItemKind::Terminal { session } => Some((tile.worker, session)),
            _ => None,
        }
    }

    /// A new link to `key` starts focused on nothing: the shell in front is told again.
    pub(super) fn focus_link_reset(&mut self, key: WorkerKey) {
        if self.handoff.focus.is_some_and(|(k, _)| k == key) {
            self.handoff.focus = None;
        }
    }

    /// The notice of a held-back page: who asks to open which host, the host loudest when the
    /// address was built to deceive, and under it why it was held back and when it was asked.
    /// The whole address is in its hint, in the monospace face.
    pub(super) fn offer_body(&self, offer: &Offer) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (host_ink, host_weight) = if offer.deceptive() {
            (s.warn, FontWeight::SEMIBOLD)
        } else {
            (s.text, FontWeight::MEDIUM)
        };
        let url = SharedString::from(offer.url.clone());
        let hint_theme = Rc::new(theme.clone());
        div()
            .id("offer-body")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .tooltip(move |_window, cx| {
                cx.new(|_| crate::kit::Hint::new(url.clone(), "", Rc::clone(&hint_theme)).mono())
                    .into()
            })
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .whitespace_nowrap()
                    .child(
                        // The asker is a shell's title, as long as a program likes: it gives
                        // way, cut short, and the host always shows whole.
                        div()
                            .flex_initial()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(offer.asker.clone())),
                    )
                    .child(div().flex_none().child("\u{a0}wants to open\u{a0}"))
                    .child(
                        div()
                            .debug_selector(|| "offer-host".to_owned())
                            .flex_none()
                            .text_color(hsla(host_ink))
                            .font_weight(host_weight)
                            .child(SharedString::from(offer.host.clone())),
                    ),
            )
            .child(
                div()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(offer.detail(Instant::now()))),
            )
            .into_any_element()
    }
}

/// The focus report for `session`.
const fn focus_msg(session: SessionId, focused: bool) -> ClientMsg {
    ClientMsg::Term { session, req: TermRequest::Focus { focused } }
}
