//! The commit sheet: the repository a thread works in, as the person commits, pushes and opens
//! or merges its branch's pull request, from the thread's tile or its review's.
//!
//! It opens on the changed files and an empty message: a commit takes the person's words, so
//! nothing is suggested. A thread's sheet ticks only the files its agent changed, as the thread's
//! review over all its turns names them, so another thread's work in the same checkout is not
//! swept into it; the rest are listed unticked. A folder's sheet, or a thread whose review cannot
//! tell, ticks every one. On a thread's sheet the agent can be asked to commit
//! instead ("Ask `<agent>` to commit"): it knows why it changed what it did. The ask is one
//! message through its own door, after its turn, and the sheet reads the repository again once
//! the turn it went into ends. Slopty writes no message itself and calls no model. Over the files
//! stands the branch's pull request, its checks most pressing first, and its merge as the forge's
//! own summary allows it, always for the head the person is looking at: now, with a warning for a
//! failing check the base does not require; once checks or a review it waits on clear
//! (auto-merge, a merge queue where the branch has one); or, for a draft, after "Ready for
//! review". What git or gh said when it refused is shown in its own words, in the code face, under
//! the buttons.
//!
//! It is drawn over its tile on a scrim of the tile alone, so the rest of the workspace stays
//! in reach. What the person writes in it (the message, a pull request's title and description)
//! is kept as they type, on the worker's hub and in the drafts file ([`CommitDraft`]), so a sheet
//! closed and opened again, or the app relaunched, takes it back. A press on the scrim or Esc
//! closes it only while it holds no words; Close always does, the words kept. Its state is the
//! hub's ([`super::git::GitBook`]): two tiles on one repository show one answer.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::rc::Rc;

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
    uniform_list,
};
use gpui_kit::component::input::{self, Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_client::threads::Mirror;
use slopty_proto::RequestId;
use slopty_proto::git::{
    CheckBucket, Forge, GitOp, GitStatus, PullCheck, PullComments, PullStatus,
};
use slopty_proto::thread::wire::{Intent, PullSeen, PullStands, Review, ReviewScope};
use slopty_proto::thread::{Cap, Delivery, IntentId, Liveness, ThreadId, TurnState};
use slopty_theme::{Rgb, Theme, Typography, alpha};

use super::git::{self, Method, Pull, Repo, Said};
use super::hub::{HubEvent, ThreadHub};
use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};
use crate::kit::{self, ButtonKind};

/// File rows shown before the list scrolls.
const FILES_SHOWN: usize = 8;

/// Checks shown, the most pressing first, before "+N more checks".
const CHECKS_SHOWN: usize = 6;

/// The merge button while the forge waits on something that clears by itself: auto-merge.
pub const MERGE_WHEN_READY: &str = "Merge when ready";

/// The button that takes a draft pull request out of draft.
pub const MARK_READY: &str = "Ready for review";

/// The merged sheet's button while its agent still runs in the worktree.
#[must_use]
pub fn end_and_remove(agent: &str) -> String {
    format!("End {agent} and remove")
}

/// What "Ask `<agent>` to commit" sends the thread's agent.
pub const ASK_TO_COMMIT: &str = "Commit what you changed, with a message saying why.";

/// The agent's ask on its way: what it went as, and the thread's turns when it went, to know
/// when the turn it went into has ended.
#[derive(Clone, Copy, Debug)]
struct Asked {
    intent: IntentId,
    /// The turns the thread had when it was asked.
    turns: usize,
    /// It went into the turn under way, as a steer: that turn's end is the answer's.
    into_turn: bool,
}

/// What the sheet tells its tile.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommitEvent {
    /// The person closed it.
    Close,
    /// The pull request merged, and the person asked to free the agent's worktree the
    /// repository is, at this root: the workspace ends the agents still running in it in a
    /// terminal of their own, waits for them to exit, then asks the worker, as anywhere else.
    EndAndRemove(String),
}

/// What a commit sheet holds unsent.
#[derive(Clone, Default, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct CommitDraft {
    /// The commit's message.
    pub message: String,
    /// A pull request's title.
    pub title: String,
    /// Its description.
    pub body: String,
}

impl CommitDraft {
    /// Whether it holds no words.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        [&self.message, &self.title, &self.body].iter().all(|t| t.trim().is_empty())
    }
}

/// Which of the changed files are the thread's own: ticked as the sheet opens.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Own {
    /// Every one: a folder's sheet, or a thread whose review could not tell.
    All,
    /// The thread's review over this span is asked; none until it comes.
    Waiting(ReviewScope),
    /// These paths, from the repository's root.
    Files(HashSet<String>),
}

impl Own {
    /// What `review` names: each file by its path, and a renamed one by its old path too.
    fn of(review: &Review) -> Self {
        if review.absent.is_some() {
            return Self::All;
        }
        let paths = review.files.iter().flat_map(|f| f.old_path.iter().chain([&f.path]));
        Self::Files(paths.cloned().collect())
    }

    fn has(&self, path: &str) -> bool {
        match self {
            Self::All => true,
            Self::Waiting(_) => false,
            Self::Files(own) => own.contains(path),
        }
    }
}

/// Which face the sheet shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    /// The files, the message, commit and push, and the pull request over them.
    Commit,
    /// A pull request's title, description, base and draft, to open it.
    Open,
}

/// The commit sheet over a tile.
pub struct CommitSheet {
    hub: Entity<ThreadHub>,
    /// The folder the thread works in: the repository is the one holding it.
    repo: String,
    theme: Theme,
    page: Page,
    message: Entity<TextareaState>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    base: Entity<InputState>,
    draft: bool,
    /// Which files are the thread's own, ticked unless the person says otherwise.
    own: Own,
    /// The person's own ticks, by path, over what [`Self::own`] says.
    picked: HashMap<String, bool>,
    /// The merge method the person picked in this sheet; until then, the one the worker offers
    /// first ([`Self::method`]).
    picked_method: Option<Method>,
    methods_open: bool,
    delete_branch: bool,
    /// The commit this sheet asked for: its message goes once it is made.
    committing: Option<RequestId>,
    /// Why that commit did not go, in git's words, until the next one is asked.
    commit_failed: Option<String>,
    /// The thread whose agent "Ask `<agent>` to commit" asks; none for a folder's.
    ask: Option<ThreadId>,
    /// The agent's ask, until the turn it went into ends.
    asked: Option<Asked>,
    /// Why the worker turned the ask down.
    ask_refused: Option<String>,
    /// The thread's pull request as its row last said it: a move there asks the sheet's again.
    pull_seen: Option<PullSeen>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for CommitSheet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitSheet").field("repo", &self.repo).finish_non_exhaustive()
    }
}

impl EventEmitter<CommitEvent> for CommitSheet {}

impl Focusable for CommitSheet {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.message.focus_handle(cx)
    }
}

impl CommitSheet {
    /// The sheet for the repository holding `repo`, on `hub`'s worker: its status and its
    /// branch's pull request are asked at once, and the message field has the keyboard.
    pub fn new(
        hub: Entity<ThreadHub>,
        repo: String,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let message =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("Message").auto_grow(2, 6));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let body =
            cx.new(|cx| TextareaState::new(window, cx).placeholder("Description").auto_grow(3, 8));
        let base = cx.new(|cx| InputState::new(window, cx).placeholder("Base branch"));
        let watched = repo.clone();
        let hearing =
            cx.subscribe_in(&hub, window, move |this, _hub, event, window, cx| match event {
                HubEvent::Git(r) if *r == watched => this.heard(window, cx),
                HubEvent::Thread(t) if this.ask == Some(*t) => this.ask_moved(cx),
                HubEvent::Review(t) if this.ask == Some(*t) => this.own_reviewed(cx),
                HubEvent::Table => this.row_moved(cx),
                _ => {}
            });
        let typing = [
            cx.observe(&message, |this, _, cx| this.typed(cx)),
            cx.observe(&title, |this, _, cx| this.typed(cx)),
            cx.observe(&body, |this, _, cx| this.typed(cx)),
        ];
        let kept = hub.read(cx).commit_draft(&repo).cloned();
        if let Some(kept) = kept {
            message.update(cx, |m, cx| m.set_value(&kept.message, window, cx));
            title.update(cx, |t, cx| t.set_value(&kept.title, window, cx));
            body.update(cx, |b, cx| b.set_value(&kept.body, window, cx));
        }
        let sheet = Self {
            hub,
            repo,
            theme,
            page: Page::Commit,
            message,
            title,
            body,
            base,
            draft: false,
            own: Own::All,
            picked: HashMap::new(),
            picked_method: None,
            methods_open: false,
            delete_branch: true,
            committing: None,
            commit_failed: None,
            ask: None,
            asked: None,
            ask_refused: None,
            pull_seen: None,
            focus: cx.focus_handle(),
            _subscriptions: std::iter::once(hearing).chain(typing).collect(),
        };
        sheet.refresh(cx);
        sheet.message.update(cx, |m, cx| m.focus(window, cx));
        sheet
    }

    /// The sheet of `thread`'s repository, its agent offered the commit, its row's pull
    /// request followed, and only the files its review over all its turns names ticked: that
    /// review is asked now, and nothing is ticked until it comes.
    #[must_use]
    pub fn asking(mut self, thread: ThreadId, cx: &mut Context<Self>) -> Self {
        self.ask = Some(thread);
        let hub = self.hub.read(cx);
        self.pull_seen = hub.threads().rows().rows.get(&thread).and_then(|r| r.pull.clone());
        let first = hub.threads().mirror(thread).and_then(Mirror::state).map(|state| {
            state.turns.iter().map(|t| t.id).find(|t| *t != slopty_proto::thread::TurnId::BEFORE)
        });
        self.own = match first {
            // A thread not read yet, or one that has run no turn: nothing of its own.
            None | Some(None) => Own::Files(HashSet::new()),
            Some(Some(first)) => {
                let scope = ReviewScope::Since(first);
                self.hub.update(cx, |hub, cx| hub.ask_review(thread, scope.clone(), cx));
                Own::Waiting(scope)
            }
        };
        self
    }

    /// The folder whose repository it works.
    #[must_use]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Draw in `theme`.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Ask the status and the pull request again: the sheet opening, or the person's refresh.
    pub fn refresh(&self, cx: &mut Context<Self>) {
        let repo = self.repo.clone();
        self.hub.update(cx, |hub, cx| {
            let _status = hub.git_op(&repo, GitOp::Status, cx);
            let _pull = hub.git_op(&repo, GitOp::PullStatus, cx);
        });
    }

    fn state<'a>(&self, cx: &'a App) -> Option<&'a Repo> {
        self.hub.read(cx).git().repo(&self.repo)
    }

    fn busy<'a>(&self, cx: &'a App) -> Option<&'a GitOp> {
        self.hub.read(cx).git().busy(&self.repo)
    }

    /// The repository said something: a commit this sheet made takes its message away, and a
    /// pull request opened goes back to the files.
    fn heard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let said = self.state(cx).and_then(|r| r.said.clone());
        if let Some((request, said)) = said {
            if Some(request) == self.committing {
                self.committing = None;
                self.commit_failed = match &said {
                    Said::Refused { why } => Some(why.clone()),
                    Said::Failed { said } => Some(said.clone()),
                    _ => None,
                };
                if matches!(said, Said::Committed { .. }) {
                    self.message.update(cx, |m, cx| m.set_value("", window, cx));
                }
            }
            if matches!(said, Said::Opened { .. }) && self.page == Page::Open {
                self.page = Page::Commit;
                for field in [&self.title, &self.base] {
                    field.update(cx, |f, cx| f.set_value("", window, cx));
                }
                self.body.update(cx, |b, cx| b.set_value("", window, cx));
            }
        }
        let present: HashSet<&str> = self
            .state(cx)
            .and_then(|r| r.status.as_deref())
            .map(|s| s.files.iter().map(|f| f.path.as_str()).collect())
            .unwrap_or_default();
        self.picked.retain(|p, _| present.contains(p.as_str()));
        cx.notify();
    }

    // ----- what the person does --------------------------------------------------------

    /// The paths the commit takes: every file ticked, a renamed one by both its paths.
    fn chosen(&self, status: &GitStatus) -> Vec<String> {
        status
            .files
            .iter()
            .filter(|f| self.ticked(&f.path))
            .flat_map(|f| f.from.iter().cloned().chain([f.path.clone()]))
            .collect()
    }

    /// Whether `path` goes in the commit: the person's tick, else whether it is the thread's.
    fn ticked(&self, path: &str) -> bool {
        self.picked.get(path).copied().unwrap_or_else(|| self.own.has(path))
    }

    fn toggle_file(&mut self, path: String, cx: &mut Context<Self>) {
        let on = self.ticked(&path);
        self.picked.insert(path, !on);
        cx.notify();
    }

    /// Every file ticked, or, when every one already is, none.
    fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let Some(status) = self.state(cx).and_then(|r| r.status.clone()) else { return };
        let on = !status.files.iter().all(|f| self.ticked(&f.path));
        self.picked = status.files.iter().map(|f| (f.path.clone(), on)).collect();
        cx.notify();
    }

    /// The thread's review over all its turns came: the files it names are its own.
    fn own_reviewed(&mut self, cx: &mut Context<Self>) {
        let (Some(thread), Own::Waiting(scope)) = (self.ask, &self.own) else { return };
        let Some(review) = self.hub.read(cx).review(thread, scope) else { return };
        self.own = Own::of(review);
        cx.notify();
    }

    /// Commit the files ticked with the person's message, and push once it is made when
    /// `push` says.
    fn commit(&mut self, push: bool, cx: &mut Context<Self>) {
        let Some(status) = self.state(cx).and_then(|r| r.status.clone()) else { return };
        let paths = self.chosen(&status);
        let message = self.message.read(cx).value().to_string();
        if paths.is_empty() || message.trim().is_empty() || self.busy(cx).is_some() {
            return;
        }
        let repo = self.repo.clone();
        self.commit_failed = None;
        self.committing = self.hub.update(cx, |hub, cx| {
            if push {
                hub.commit_and_push(&repo, paths, message, cx)
            } else {
                hub.git_op(&repo, GitOp::Commit { paths, message }, cx)
            }
        });
    }

    /// The sheet's agent by name while its process still runs.
    fn running_agent(&self, cx: &App) -> Option<String> {
        let state = self.hub.read(cx).threads().mirror(self.ask?).and_then(Mirror::state)?;
        (state.status.liveness == Liveness::Live)
            .then(|| super::view::agent_label(&state.meta.agent))
    }

    /// The thread's row moved. The worker reads its pull request on its own clock and says it
    /// on the row, so a pull request that moved there (merged on the forge's page, a check
    /// that ended) is asked again here while the sheet is up.
    fn row_moved(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.ask else { return };
        let seen =
            self.hub.read(cx).threads().rows().rows.get(&thread).and_then(|r| r.pull.clone());
        if seen == self.pull_seen {
            return;
        }
        let was = std::mem::replace(&mut self.pull_seen, seen);
        if was.is_some() || self.pull_seen.is_some() {
            let repo = self.repo.clone();
            self.hub.update(cx, |hub, cx| {
                let _pull = hub.git_op(&repo, GitOp::PullStatus, cx);
            });
        }
    }

    /// The agent the sheet can ask to commit, by name: the thread's, where it takes a message.
    fn asker(&self, cx: &App) -> Option<String> {
        let state = self.hub.read(cx).threads().mirror(self.ask?).and_then(Mirror::state)?;
        let meta = &state.meta;
        (meta.can(Cap::QUEUE) || meta.can(Cap::STEER))
            .then(|| super::view::agent_label(&meta.agent))
    }

    /// Ask the agent to commit what it changed, after its turn where it queues, so the work in
    /// hand is not cut into.
    fn ask_agent(&mut self, cx: &mut Context<Self>) {
        self.tell(ASK_TO_COMMIT.to_owned(), cx);
    }

    /// Tell the thread's agent `text` on the person's press, after its turn where it queues; the
    /// sheet waits on it as on an ask to commit, and reads the repository again once its turn
    /// ends.
    fn tell(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(thread) = self.ask.filter(|_| self.asked.is_none()) else { return };
        let Some((delivery, turns, active)) = ({
            let hub = self.hub.read(cx);
            hub.threads().mirror(thread).and_then(Mirror::state).map(|state| {
                let active = state.turns.last().is_some_and(|t| t.state == TurnState::Active);
                (state.meta.delivery_after_turn(), state.turns.len(), active)
            })
        }) else {
            return;
        };
        let send = Intent::Send { text, delivery, attachments: Vec::new() };
        let intent = self.hub.update(cx, |hub, cx| hub.intent(thread, send, cx));
        let into_turn = active && delivery == Delivery::Steer;
        self.asked = Some(Asked { intent, turns, into_turn });
        self.ask_refused = None;
        cx.notify();
    }

    /// The thread moved while the agent was asked: turned down, it says why; once the turn the
    /// ask went into ends, the repository is read again for what the agent did.
    fn ask_moved(&mut self, cx: &mut Context<Self>) {
        let (Some(asked), Some(thread)) = (self.asked, self.ask) else { return };
        let hub = self.hub.read(cx);
        let sent = hub.threads().outbox().of(thread).find(|s| s.id == asked.intent);
        if let Some(why) = sent.filter(|s| s.failed()).map(|s| s.failure().unwrap_or_default()) {
            self.asked = None;
            self.ask_refused = Some(why);
            cx.notify();
            return;
        }
        let Some(state) = hub.threads().mirror(thread).and_then(Mirror::state) else { return };
        let waiting = state.pending.iter().any(|p| p.intent == asked.intent);
        let went = asked.into_turn || state.turns.len() > asked.turns;
        let rests = state.turns.last().is_none_or(|t| t.state != TurnState::Active);
        if !waiting && went && rests {
            self.asked = None;
            self.refresh(cx);
            cx.notify();
        }
    }

    fn op(&self, op: GitOp, cx: &mut Context<Self>) {
        if self.busy(cx).is_some() {
            return;
        }
        let repo = self.repo.clone();
        let _asked = self.hub.update(cx, |hub, cx| hub.git_op(&repo, op, cx));
    }

    fn open_pull_request(&self, cx: &mut Context<Self>) {
        let base = self.base.read(cx).value().trim().to_owned();
        self.op(
            GitOp::PullRequest {
                title: self.title.read(cx).value().trim().to_owned(),
                body: self.body.read(cx).value().to_string(),
                base: (!base.is_empty()).then_some(base),
                draft: self.draft,
            },
            cx,
        );
    }

    /// Merge the pull request the person is looking at, on their press alone: now while the
    /// forge would, or once what it waits on clears ([`MergeGate::WhenReady`]).
    fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(pull) = self.state(cx).and_then(|r| r.pull.status()).filter(|p| open(p)) else {
            return;
        };
        let auto = match merge_gate(pull) {
            MergeGate::Now { .. } => false,
            MergeGate::WhenReady { .. } => true,
            MergeGate::Draft | MergeGate::Waits(_) => return,
        };
        let op = GitOp::Merge {
            method: self.method(pull).wire().to_owned(),
            head: Some(pull.head_commit.clone()),
            delete_branch: self.delete_branch,
            auto,
        };
        self.methods_open = false;
        self.op(op, cx);
    }

    /// The merge method in force for `pull`: the person's pick in this sheet while its
    /// repository still allows it, else the one the worker offers first (the last used in this
    /// repository, or on the forge), else the first allowed.
    fn method(&self, pull: &PullStatus) -> Method {
        let offered = Method::offered(pull);
        self.picked_method
            .filter(|m| offered.contains(m))
            .or_else(|| Method::of_wire(&pull.method).filter(|m| offered.contains(m)))
            .or_else(|| offered.first().copied())
            .unwrap_or(Method::Squash)
    }

    fn show_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        // A new pull request merges into the branch's base, the one its worktree started from
        // as the worker reads it, until the person types another.
        let base = self.state(cx).and_then(|r| r.status.as_ref()?.merge_base.clone());
        if page == Page::Open
            && let Some(base) = base
            && self.base.read(cx).value().trim().is_empty()
        {
            self.base.update(cx, |b, cx| b.set_value(&base, window, cx));
        }
        let field = match page {
            Page::Commit => self.message.focus_handle(cx),
            Page::Open => self.title.focus_handle(cx),
        };
        window.focus(&field, cx);
        cx.notify();
    }

    fn close(cx: &mut Context<Self>) {
        cx.emit(CommitEvent::Close);
    }

    /// A press on the scrim, or Esc: the sheet closes only while it holds no words, which a
    /// stray press would otherwise put out of sight.
    fn dismiss(&self, cx: &mut Context<Self>) {
        if self.draft(cx).is_empty() {
            Self::close(cx);
        }
    }

    /// What the sheet holds unsent.
    fn draft(&self, cx: &App) -> CommitDraft {
        CommitDraft {
            message: self.message.read(cx).value().to_string(),
            title: self.title.read(cx).value().to_string(),
            body: self.body.read(cx).value().to_string(),
        }
    }

    /// The person typed: the words go to the hub, which keeps them for the next sheet here.
    fn typed(&self, cx: &mut Context<Self>) {
        let (draft, repo) = (self.draft(cx), self.repo.clone());
        self.hub.update(cx, |hub, cx| hub.set_commit_draft(&repo, draft, cx));
        cx.notify();
    }

    // ----- drawing: pieces -------------------------------------------------------------

    fn icon(&self, name: Symbol, tone: Rgb) -> Div {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(px(self.theme.typography.icon()))
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// A button that does nothing while `off`, set back so it reads so.
    fn button(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        kind: ButtonKind,
        off: bool,
    ) -> gpui::Stateful<Div> {
        kit::button(&self.theme, id, label, kind)
            .when(off, |el| el.opacity(alpha::PRESSED).cursor_default())
    }

    /// A tick box and its words, pressed as one.
    fn tick(
        &self,
        id: impl Into<SharedString>,
        label: SharedString,
        on: bool,
    ) -> gpui::Stateful<Div> {
        let s = self.theme.surfaces;
        let id = id.into();
        let selector = id.to_string();
        crate::a11y::tab_stop(
            div()
                .id(ElementId::Name(id))
                .debug_selector(move || selector)
                .role(Role::CheckBox)
                .aria_label(label)
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .flex_none()
                .flex()
                .items_center()
                .gap(px(self.theme.spacing.xs))
                .cursor_pointer()
                .child(kit::tick_box(&self.theme, on)),
            s.focus,
        )
    }

    /// Words in the code face in a well ([`kit::inset`]): what git or gh said, as it said it.
    fn said_block(&self, id: &'static str, words: &str, tone: Rgb) -> Div {
        let theme = &self.theme;
        div()
            .debug_selector(move || id.to_owned())
            .w_full()
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xs))
            .rounded(px(theme.radii.sm))
            .map(|el| kit::inset(el, theme))
            .font_family(self.mono())
            .text_size(px(theme.typography.small()))
            .text_color(hsla(tone))
            .whitespace_normal()
            .child(SharedString::from(words.trim_end().to_owned()))
    }

    /// A line under a part's head: quiet words in the meta size.
    fn quiet(&self, id: &'static str, words: impl Into<SharedString>) -> Div {
        kit::meta(div(), &self.theme)
            .debug_selector(move || id.to_owned())
            .min_w_0()
            .child(words.into())
    }

    // ----- drawing: the parts -----------------------------------------------------------

    fn head(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let status = self.state(cx).and_then(|r| r.status.as_deref());
        let forge = self.state(cx).map_or(Forge::GitHub, Repo::forge);
        let title = match self.page {
            Page::Commit => "Commit".to_owned(),
            Page::Open => format!("Open {}", forge.noun()),
        };
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .min_h(px(kit::Row::Two.height(theme)))
            .child(kit::title(theme, title).flex_none())
            .child(
                div()
                    .debug_selector(|| "commit-branch".to_owned())
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xxs))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(status.map(|_| self.icon(Symbol::ArrowTriangleBranch, s.text_muted)))
                    .children(status.map(|st| {
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(git::branch_line(st)))
                    })),
            )
            .child(
                kit::icon_button(theme, "commit-refresh", Symbol::ArrowClockwise, "Refresh")
                    .on_click(cx.listener(|this, _ev, _w, cx| this.refresh(cx))),
            )
            .child(
                kit::icon_button(theme, "commit-close", Symbol::Xmark, "Close")
                    .on_click(cx.listener(|_this, _ev, _w, cx| Self::close(cx))),
            )
    }

    /// The branch's pull request: its number and title, where it stands, its checks, and the
    /// merge while it is ready.
    fn pull_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx)?;
        let reading = self.hub.read(cx).git().reading(&self.repo, true);
        let part = kit::inset_x(div(), theme)
            .debug_selector(|| "commit-pull".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xs))
            .py(px(theme.spacing.sm));
        let part = match (&repo.pull, &repo.pull_unread) {
            (Pull::Known(pull), _) => part.child(self.pull_line(pull)).children(self.checks(pull)),
            (_, Some(why)) => {
                part.child(self.said_block("commit-pull-unread", why, s.text_secondary))
            }
            (Pull::None, None) => part.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .child(self.icon(Symbol::ArrowTrianglePull, s.text_muted))
                    .child(self.quiet(
                        "commit-no-pull",
                        format!("No {} for this branch", repo.forge().noun()),
                    )),
            ),
            (Pull::Unknown, None) if reading => part.child(self.quiet(
                "commit-pull-reading",
                format!("Reading the {}\u{2026}", repo.forge().noun()),
            )),
            (Pull::Unknown, None) => return None,
        };
        let next = repo.pull.status().and_then(|pull| self.next_steps(pull, repo, cx));
        let merge = repo.pull.status().and_then(|pull| self.merge_row(pull, repo, cx));
        let free = repo.pull.status().and_then(|pull| self.free_row(pull, repo, cx));
        Some(part.children(next).children(merge).children(free).into_any_element())
    }

    fn pull_line(&self, pull: &PullStatus) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let standing = pull.standing();
        let tone = standing_tone(theme, standing);
        let icon = match standing {
            PullStands::Merged => Symbol::ArrowTriangleMerge,
            _ => Symbol::ArrowTrianglePull,
        };
        let url = pull.url.clone();
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .text_size(px(theme.typography.small()))
            .child(self.icon(icon, tone))
            .child(
                div()
                    .id("commit-pull-number")
                    .debug_selector(|| "commit-pull-number".to_owned())
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!(
                        "{} {}",
                        pull.forge.title(),
                        pull.number
                    )))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(hsla(s.text_secondary))
                    .hover(gpui::Styled::underline)
                    .child(SharedString::from(format!("{}{}", pull.forge.mark(), pull.number)))
                    .on_click(move |_ev, _w, cx| cx.open_url(&url)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    .child(SharedString::from(pull.title.clone())),
            )
            .child(
                kit::pill(theme, tone)
                    .debug_selector(|| "commit-pull-standing".to_owned())
                    .child(SharedString::from(git::standing_words(pull))),
            )
            .into_any_element()
    }

    /// The checks, the most pressing first; each opens its page.
    fn checks(&self, pull: &PullStatus) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let mut checks: Vec<&PullCheck> = pull.checks.iter().collect();
        checks.sort_by_key(|c| std::cmp::Reverse(c.bucket()));
        let past = u64::try_from(checks.len().saturating_sub(CHECKS_SHOWN)).unwrap_or(u64::MAX);
        let more = past.saturating_add(u64::from(pull.more_checks));
        let mut rows: Vec<AnyElement> = checks
            .into_iter()
            .take(CHECKS_SHOWN)
            .enumerate()
            .map(|(ix, check)| {
                let (icon, tone) = match check.bucket() {
                    CheckBucket::Failed => (Symbol::XmarkCircle, s.error),
                    CheckBucket::Running => (Symbol::CircleDashed, s.text_secondary),
                    CheckBucket::Passed => (Symbol::CheckmarkCircle, s.text_muted),
                    CheckBucket::Skipped => (Symbol::Minus, s.text_muted),
                };
                let link = check.link.clone();
                div()
                    .id(ElementId::Name(format!("commit-check-{ix}").into()))
                    .debug_selector(move || format!("commit-check-{ix}"))
                    .role(if link.is_some() { Role::Link } else { Role::ListItem })
                    .aria_label(SharedString::from(format!("{}, {}", check.name, check.state)))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .pl(px(theme.spacing.xxs))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_secondary))
                    .child(self.icon(icon, tone))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(SharedString::from(check.name.clone())),
                    )
                    .children(check.workflow.clone().map(|w| {
                        div()
                            .min_w_0()
                            .flex_shrink_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(w))
                    }))
                    .when_some(link, |el, link| {
                        el.cursor_pointer()
                            .hover(move |el| el.text_color(hsla(s.text)))
                            .on_click(move |_ev, _w, cx| cx.open_url(&link))
                    })
                    .into_any_element()
            })
            .collect();
        if more > 0 {
            let words = format!("+{more} more checks");
            rows.push(
                self.quiet("commit-more-checks", words)
                    .pl(px(theme.spacing.xxs))
                    .into_any_element(),
            );
        }
        rows
    }

    /// What the thread's agent can be asked to do for its open pull request, each on the
    /// person's press: fix the checks that failed, address the review still open, bring the
    /// branch up to date with its base. None where no agent takes a message, or while one is
    /// asked.
    fn next_steps(&self, pull: &PullStatus, repo: &Repo, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        self.asker(cx)?;
        let off = self.asked.is_some();
        let comments = repo.comments.as_deref().filter(|c| c.number == pull.number);
        let mut steps: Vec<AnyElement> = Vec::new();
        if let Some(text) = fix_checks_words(pull) {
            steps.push(
                self.button("commit-fix-checks", "Fix the checks", ButtonKind::Secondary, off)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.tell(text.clone(), cx)))
                    .into_any_element(),
            );
        }
        if let Some(text) = comments.and_then(|c| address_review_words(pull, c)) {
            let listed = |c: &PullComments| u64::try_from(c.threads.len()).unwrap_or(u64::MAX);
            let open = comments.map_or(0, |c| listed(c).saturating_add(u64::from(c.more)));
            let label = format!("Address the review ({open})");
            steps.push(
                self.button("commit-address-review", label, ButtonKind::Secondary, off)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.tell(text.clone(), cx)))
                    .into_any_element(),
            );
        }
        if let Some(text) = up_to_date_words(pull) {
            steps.push(
                self.button(
                    "commit-bring-up-to-date",
                    "Bring up to date",
                    ButtonKind::Secondary,
                    off,
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| this.tell(text.clone(), cx)))
                .into_any_element(),
            );
        }
        if steps.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "commit-next-steps".to_owned())
                .w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(theme.spacing.xs))
                .pt(px(theme.spacing.xs))
                .children(steps)
                .into_any_element(),
        )
    }

    /// Once the pull request merged, the worktree the repository is goes on the person's
    /// press: its work has landed, so nothing is left for it to hold. Only for an agent's
    /// worktree under its clone's `.claude/worktrees/`, and not once it went. While the sheet's
    /// own agent still runs there the press ends it first, and says so ([`end_and_remove`]).
    fn free_row(&self, pull: &PullStatus, repo: &Repo, cx: &Context<Self>) -> Option<AnyElement> {
        if pull.standing() != PullStands::Merged {
            return None;
        }
        let root = crate::workspace::worktree_root(&self.repo)?;
        if matches!(repo.said, Some((_, Said::Freed { .. }))) {
            return None;
        }
        let label = self.running_agent(cx).map_or_else(
            || SharedString::from(super::view::exited::REMOVE_WORKTREE),
            |agent| end_and_remove(&agent).into(),
        );
        let busy = self.busy(cx).is_some();
        Some(
            div()
                .debug_selector(|| "commit-free".to_owned())
                .w_full()
                .flex()
                .items_center()
                .gap(px(self.theme.spacing.xs))
                .pt(px(self.theme.spacing.xs))
                .child(
                    self.button("commit-remove-worktree", label, ButtonKind::Secondary, busy)
                        .on_click(cx.listener(move |this, _ev, _w, cx| {
                            if this.busy(cx).is_none() {
                                cx.emit(CommitEvent::EndAndRemove(root.clone()));
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    /// The merge, while the pull request is open, as the forge's own summary allows it
    /// ([`merge_gate`]): now, with a warning for what is not green and not required; once what
    /// it waits on clears; after a draft is marked ready; or not, with the reason.
    fn merge_row(&self, pull: &PullStatus, repo: &Repo, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if !open(pull) {
            return None;
        }
        if let Some(why) = &repo.no_gh {
            return Some(self.said_block("commit-no-gh", why, s.text_secondary).into_any_element());
        }
        let busy = self.busy(cx).is_some();
        let (label, note): (SharedString, Option<AnyElement>) = match merge_gate(pull) {
            MergeGate::Now { warn } => {
                let note = warn.map(|warn| {
                    div()
                        .debug_selector(|| "commit-merge-warn".to_owned())
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.xs))
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(s.text_secondary))
                        .child(self.icon(Symbol::ExclamationmarkTriangle, s.warn))
                        .child(SharedString::from(warn))
                        .into_any_element()
                });
                (self.method(pull).verb().into(), note)
            }
            MergeGate::WhenReady { waits } => {
                // gh's words for an auto-merge it took stand in the outcome below; the row says
                // so rather than offer it twice.
                let taken = matches!(repo.said, Some((_, Said::Merged { .. })));
                let words = format!("Merge waits: {waits}");
                if taken {
                    return Some(self.quiet("commit-merge-waits", words).into_any_element());
                }
                (
                    MERGE_WHEN_READY.into(),
                    Some(self.quiet("commit-merge-waits", words).into_any_element()),
                )
            }
            MergeGate::Draft => {
                let ready = self
                    .button("commit-mark-ready", MARK_READY, ButtonKind::Secondary, busy)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.op(GitOp::MarkReady, cx)));
                return Some(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.md))
                        .pt(px(theme.spacing.xs))
                        .child(ready)
                        .child(self.quiet("commit-merge-waits", "Merge waits: still a draft"))
                        .into_any_element(),
                );
            }
            MergeGate::Waits(why) => {
                let words = format!("Merge waits: {why}");
                return Some(self.quiet("commit-merge-waits", words).into_any_element());
            }
        };
        let split = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(kit::HAIR)
            .child(
                self.button("commit-merge", label, ButtonKind::Primary, busy)
                    .rounded_r(px(0.0))
                    .on_click(cx.listener(|this, _ev, _w, cx| this.merge(cx))),
            )
            .child(
                kit::solid_pressable(
                    div()
                        .id("commit-merge-methods")
                        .debug_selector(|| "commit-merge-methods".to_owned())
                        .role(Role::Button)
                        .aria_label("Merge method")
                        .aria_expanded(self.methods_open)
                        .flex_none()
                        .h(px(theme.density.control))
                        .px(px(theme.spacing.xs))
                        .flex()
                        .items_center()
                        .rounded_r(px(theme.radii.sm))
                        .cursor_pointer(),
                    theme,
                )
                .child(self.icon(Symbol::ChevronDown, s.solid_ink))
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.methods_open = !this.methods_open;
                    cx.notify();
                })),
            );
        let delete = self
            .tick("commit-delete-branch", "Delete branch".into(), self.delete_branch)
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child("Delete branch")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.delete_branch = !this.delete_branch;
                cx.notify();
            }));
        let menu = self.methods_open.then(|| self.methods_menu(pull, cx));
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xs))
                .pt(px(theme.spacing.xs))
                .children(note)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.md))
                        .child(split)
                        .child(delete),
                )
                .children(menu)
                .into_any_element(),
        )
    }

    /// The merge's methods the repository allows, under the split button: the one in force
    /// ticked.
    fn methods_menu(&self, pull: &PullStatus, cx: &Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let mut menu = kit::Menu::new();
        let chosen = self.method(pull);
        for method in Method::offered(pull) {
            let this = this.clone();
            menu.push(
                kit::MenuItem::new(method.wire(), method.verb(), move |_w, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.picked_method = Some(method);
                        cx.notify();
                    });
                })
                .mark(kit::menu::Mark::Radio(method == chosen)),
            );
        }
        kit::MenuPanel::new("commit-method", "Merge method", Rc::new(menu), &self.theme, {
            move |window, cx| {
                let _gone = this.update(cx, |this, cx| {
                    this.methods_open = false;
                    window.focus(&this.focus, cx);
                    cx.notify();
                });
            }
        })
        .into_any_element()
    }

    /// The changed files, each ticked to go in the commit.
    fn files_part(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx);
        let part = kit::inset_x(div(), theme)
            .debug_selector(|| "commit-files".to_owned())
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.xxs))
            .py(px(theme.spacing.sm));
        if let Some(why) = repo.and_then(|r| r.unread.as_deref()) {
            return part
                .child(self.said_block("commit-unread", why, s.text_secondary))
                .into_any_element();
        }
        let Some(status) = repo.and_then(|r| r.status.as_deref()) else {
            return part
                .child(self.quiet("commit-reading", "Reading the changes\u{2026}"))
                .into_any_element();
        };
        if status.files.is_empty() {
            return part.child(self.quiet("commit-clean", "Nothing to commit")).into_any_element();
        }
        let total = status.files.len();
        let chosen = status.files.iter().filter(|f| self.ticked(&f.path)).count();
        let count = if chosen == total {
            files_words(total)
        } else {
            format!("{chosen} of {}", files_words(total))
        };
        let all = self
            .tick("commit-all", "Every file".into(), chosen == total)
            .when(chosen > 0 && chosen < total, |el| el.aria_toggled(Toggled::Mixed))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(SharedString::from(count))
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_all(cx)));
        let row = px(kit::Row::One.height(theme));
        #[expect(clippy::cast_precision_loss, reason = "a handful of rows")]
        let shown = total.min(FILES_SHOWN) as f32;
        let list = uniform_list(
            "commit-file-rows",
            total,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range.filter_map(|ix| this.file_row(ix, cx)).collect::<Vec<_>>()
            }),
        )
        .w_full()
        .h(row * shown);
        let more = (status.more > 0)
            .then(|| self.quiet("commit-more-files", format!("+{} more", status.more)));
        part.child(all).child(list).children(more).into_any_element()
    }

    fn file_row(&self, ix: usize, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let file = self.state(cx)?.status.as_ref()?.files.get(ix)?;
        let on = self.ticked(&file.path);
        let letters = git::letters(&file.xy);
        let tone = match letters.as_str() {
            "D" => s.error,
            "new" | "A" => s.success,
            l if l.len() == 2 => s.warn,
            _ => s.text_muted,
        };
        let name = match &file.from {
            Some(from) => format!("{from} \u{2192} {}", file.path),
            None => file.path.clone(),
        };
        let path = file.path.clone();
        Some(
            self.tick(format!("commit-file-{ix}"), SharedString::from(name.clone()), on)
                .w_full()
                .h(px(kit::Row::One.height(theme)))
                .rounded(px(theme.radii.sm))
                .hover(move |el| el.bg(hsla(s.hover)))
                .text_size(px(theme.typography.small()))
                .child(
                    kit::tabular(div())
                        .flex_none()
                        .w(px(theme.spacing.xl))
                        .text_color(hsla(tone))
                        .child(SharedString::from(letters)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(if on { s.text } else { s.text_muted }))
                        .child(SharedString::from(name)),
                )
                .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_file(path.clone(), cx)))
                .into_any_element(),
        )
    }

    /// A field on the sunk well, the kit's one inset round it.
    fn field(&self, child: impl IntoElement) -> Div {
        let theme = &self.theme;
        kit::sunk(div(), theme, slopty_theme::stroke::LINE)
            .w_full()
            .rounded(px(theme.radii.md))
            .border(kit::HAIR)
            .border_color(hsla(theme.surfaces.border))
            .px(px(theme.spacing.xs))
            .child(child)
    }

    /// The message and the buttons under it.
    fn commit_foot(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let repo = self.state(cx);
        let status = repo.and_then(|r| r.status.as_deref());
        let busy = self.busy(cx).is_some();
        let chosen = status.map(|st| self.chosen(st)).unwrap_or_default();
        let worded = !self.message.read(cx).value().trim().is_empty();
        let off = busy || chosen.is_empty() || !worded;
        // With nothing to commit and the branch ahead of its upstream, or with none, the second
        // way is the push alone.
        let push_only = chosen.is_empty()
            && status
                .is_some_and(|st| st.branch.is_some() && (st.ahead > 0 || st.upstream.is_none()));
        let no_pull = repo.is_some_and(|r| matches!(r.pull, Pull::None));
        let no_gh = repo.is_some_and(|r| r.no_gh.is_some());
        let open_words = format!("Open {}", repo.map_or(Forge::GitHub, Repo::forge).noun());
        let asker = self.asker(cx);
        let asked = self.asked.is_some();
        let second = if push_only {
            self.button("commit-push", "Push", ButtonKind::Secondary, busy)
                .on_click(cx.listener(|this, _ev, _w, cx| this.op(GitOp::Push, cx)))
        } else {
            self.button("commit-and-push", "Commit and push", ButtonKind::Secondary, off)
                .on_click(cx.listener(|this, _ev, _w, cx| this.commit(true, cx)))
        };
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .pt(px(theme.spacing.sm))
            .pb(px(theme.spacing.md))
            .child(
                self.field(
                    Textarea::new(&self.message)
                        .with_size(Size::Small)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Commit message"),
                ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .when(no_pull, |el| {
                        el.child(
                            self.button(
                                "commit-open-pull",
                                open_words,
                                ButtonKind::Ghost,
                                no_gh || busy,
                            )
                            .on_click(cx.listener(
                                move |this, _ev, window, cx| {
                                    if !no_gh {
                                        this.show_page(Page::Open, window, cx);
                                    }
                                },
                            )),
                        )
                    })
                    .children(asker.as_deref().map(|agent| {
                        let label = format!("Ask {agent} to commit");
                        self.button("commit-ask", label, ButtonKind::Ghost, asked || busy)
                            .on_click(cx.listener(|this, _ev, _w, cx| this.ask_agent(cx)))
                    }))
                    .child(div().flex_1())
                    .child(second)
                    .child(
                        self.button("commit-commit", "Commit", ButtonKind::Primary, off)
                            .on_click(cx.listener(|this, _ev, _w, cx| this.commit(false, cx))),
                    ),
            )
            .children(self.asking_line(asker.as_deref()))
            .children(self.outcome(cx))
    }

    /// What the agent's ask came to: waited on, or turned down in the worker's words.
    fn asking_line(&self, agent: Option<&str>) -> Option<AnyElement> {
        if let Some(why) = &self.ask_refused {
            let words =
                if why.is_empty() { "Not sent".to_owned() } else { format!("Not sent: {why}") };
            return Some(
                self.said_block("commit-ask-refused", &words, self.theme.surfaces.warn)
                    .into_any_element(),
            );
        }
        self.asked?;
        let agent = agent.unwrap_or("The agent");
        let words = format!("Waiting on {agent}\u{2026}");
        Some(self.quiet("commit-asked", words).into_any_element())
    }

    /// The pull request's title, description, base and draft, and the way to open it.
    fn open_page(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.state(cx);
        let busy = self.busy(cx).is_some();
        let no_gh = repo.and_then(|r| r.no_gh.clone());
        let draft = self
            .tick("commit-draft", "Open as a draft".into(), self.draft)
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            .child("Open as a draft")
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.draft = !this.draft;
                cx.notify();
            }));
        kit::inset_x(div(), theme)
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .py(px(theme.spacing.sm))
            .child(self.field(
                Input::new(&self.title).appearance(false).bordered(false).aria_label("Title"),
            ))
            .child(self.quiet(
                "commit-title-hint",
                "Left empty, the title and description come from the commits",
            ))
            .child(
                self.field(
                    Textarea::new(&self.body)
                        .with_size(Size::Small)
                        .appearance(false)
                        .bordered(false)
                        .aria_label("Description"),
                ),
            )
            .child(self.field(
                Input::new(&self.base).appearance(false).bordered(false).aria_label("Base branch"),
            ))
            .child(draft)
            .children(no_gh.map(|why| self.said_block("commit-no-gh", &why, s.text_secondary)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .child(div().flex_1())
                    .child(self.button("commit-back", "Back", ButtonKind::Ghost, false).on_click(
                        cx.listener(|this, _ev, window, cx| {
                            this.show_page(Page::Commit, window, cx);
                        }),
                    ))
                    .child(
                        self.button(
                            "commit-open",
                            format!("Open {}", repo.map_or(Forge::GitHub, Repo::forge).noun()),
                            ButtonKind::Primary,
                            busy || repo.is_some_and(|r| r.no_gh.is_some()),
                        )
                        .on_click(cx.listener(|this, _ev, _w, cx| this.open_pull_request(cx))),
                    ),
            )
            .children(self.outcome(cx))
    }

    /// A refusal in git's or gh's words, and, under the commit this sheet asked for, "Ask
    /// `<agent>` to fix" where the thread's agent takes a message: it is told git's words
    /// whole, to fix what stopped the commit and leave the commit to the person.
    fn missed_block(&self, id: &'static str, said: &str, cx: &Context<Self>) -> AnyElement {
        let block = self.said_block(id, said, self.theme.surfaces.error);
        let fix = self.commit_failed.as_ref().zip(self.asker(cx)).map(|(why, agent)| {
            let text = fix_commit_words(why);
            self.button(
                "commit-ask-fix",
                format!("Ask {agent} to fix"),
                ButtonKind::Secondary,
                self.asked.is_some(),
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.tell(text.clone(), cx)))
        });
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(self.theme.spacing.xs))
            .child(block)
            .children(fix.map(|el| div().flex().child(el)))
            .into_any_element()
    }

    /// What the last op came to, or what is on its way: under the buttons, a refusal in git's
    /// or gh's words.
    fn outcome(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        if let Some(op) = self.busy(cx) {
            let words = match op {
                GitOp::Commit { .. } => "Committing\u{2026}",
                GitOp::Push => "Pushing\u{2026}",
                GitOp::PullRequest { .. } => {
                    let forge = self.state(cx).map_or(Forge::GitHub, Repo::forge);
                    return Some(
                        // The branch goes up first: the forge opens one only for a branch it has.
                        self.quiet(
                            "commit-busy",
                            format!("Pushing and opening the {}\u{2026}", forge.noun()),
                        )
                        .into_any_element(),
                    );
                }
                GitOp::Merge { .. } => "Merging\u{2026}",
                GitOp::RemoveWorktree => "Removing the worktree\u{2026}",
                GitOp::PullReview { .. } => "Posting the review\u{2026}",
                GitOp::MarkReady => "Marking it ready\u{2026}",
                GitOp::Status
                | GitOp::PullStatus
                | GitOp::PullComments { .. }
                | GitOp::Changes { .. }
                | GitOp::Branches
                | GitOp::Worktrees
                | GitOp::Scripts
                | GitOp::FileDiff { .. }
                | GitOp::Blob { .. } => return None,
            };
            return Some(self.quiet("commit-busy", words).into_any_element());
        }
        let (_, said) = self.state(cx)?.said.as_ref()?;
        let line = |icon: Symbol, words: String| {
            div()
                .id("commit-said")
                .debug_selector(|| "commit-said".to_owned())
                .role(Role::Status)
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(icon, s.success))
                .child(SharedString::from(words))
        };
        Some(match said {
            Said::Committed { commit, files } => {
                let short: String = commit.chars().take(7).collect();
                line(
                    Symbol::CheckmarkCircle,
                    format!("Committed {short} \u{b7} {}", files_words(*files as usize)),
                )
                .into_any_element()
            }
            Said::Pushed { to } => {
                line(Symbol::ArrowUpToLine, format!("Pushed to {to}")).into_any_element()
            }
            Said::Opened { url } => {
                let open = url.clone();
                line(Symbol::ArrowTrianglePull, "Opened".to_owned())
                    .child(
                        div()
                            .id("commit-opened")
                            .debug_selector(|| "commit-opened".to_owned())
                            .role(Role::Link)
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .text_color(hsla(s.text))
                            .hover(gpui::Styled::underline)
                            .child(SharedString::from(url.clone()))
                            .on_click(move |_ev, _w, cx| cx.open_url(&open)),
                    )
                    .into_any_element()
            }
            Said::Reviewed { posted, url } => {
                let words = if *posted == 0 {
                    "Posted the review".to_owned()
                } else {
                    let notes = kit::count(u64::from(*posted), "comment", "comments");
                    format!("Posted the review \u{b7} {notes}")
                };
                line(Symbol::ArrowTrianglePull, words)
                    .when_some(url.clone(), |el, url| {
                        el.cursor_pointer().on_click(move |_ev, _w, cx| cx.open_url(&url))
                    })
                    .into_any_element()
            }
            Said::Merged { said } | Said::Freed { said } => {
                self.said_block("commit-merged", said, s.text_secondary).into_any_element()
            }
            Said::Refused { why } => self.missed_block("commit-refused", why, cx),
            Said::Failed { said } => self.missed_block("commit-failed", said, cx),
        })
    }
}

impl Render for CommitSheet {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let body = match self.page {
            Page::Commit => div()
                .w_full()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.md))
                .children(self.pull_part(cx))
                .child(self.files_part(cx))
                .child(self.commit_foot(cx)),
            Page::Open => self.open_page(cx),
        };
        // The tile under the sheet dims, and a press on it closes a sheet that holds no words.
        div()
            .id("commit-scrim")
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .px(px(theme.spacing.md))
            .pt(px(theme.spacing.xxxl))
            .bg(kit::scrim(&theme))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _ev, _w, cx| this.dismiss(cx)),
            )
            .child(
                kit::dialog(&theme, kit::Overlay::List)
                    .id("commit-sheet")
                    .debug_selector(|| "commit-sheet".to_owned())
                    .track_focus(&self.focus)
                    .role(Role::Dialog)
                    .aria_label("Commit")
                    .capture_action(cx.listener(|this, _: &input::Escape, _w, cx| {
                        this.dismiss(cx);
                        cx.stop_propagation();
                    }))
                    .overflow_y_scroll()
                    .child(self.head(cx))
                    .child(div().pt(px(theme.spacing.md)).child(body)),
            )
    }
}

/// "1 file", "3 files".
fn files_words(n: usize) -> String {
    kit::count(n as u64, "file", "files")
}

/// The tone a pull request's standing is drawn in: a failure or a conflict in the error tone,
/// changes asked for in the warning's, ready in the accent, the rest quiet.
pub(crate) const fn standing_tone(theme: &Theme, standing: PullStands) -> Rgb {
    let s = theme.surfaces;
    match standing {
        PullStands::ChecksFailed | PullStands::Conflicted => s.error,
        PullStands::ChangesRequested => s.warn,
        PullStands::Ready | PullStands::Merged => s.accent,
        PullStands::Running | PullStands::Waiting | PullStands::Draft | PullStands::Closed => {
            s.text_muted
        }
    }
}

/// The longest message a next step sends its agent, in bytes: the review's threads past it are
/// left to the pull request's page.
const STEP_TEXT_MAX: usize = 32 * 1024;

/// What "Ask `<agent>` to fix" tells the agent when a commit failed: git's words whole, to fix
/// what stopped it and leave the commit to the person, who asked for it with their own words.
#[must_use]
pub fn fix_commit_words(said: &str) -> String {
    let said: String = said.chars().take(STEP_TEXT_MAX).collect();
    format!(
        "My commit failed. Git said:\n\n```\n{}\n```\n\nFix what stopped it, and leave the \
         commit to me.",
        said.trim_end()
    )
}

/// Whether `pull` is open, so a step on it means anything.
fn open(pull: &PullStatus) -> bool {
    pull.state.eq_ignore_ascii_case("OPEN")
}

/// What "Fix the checks" tells the agent: each check that failed, by name, workflow and page,
/// and how its forge shows why; none while no check failed.
#[must_use]
pub fn fix_checks_words(pull: &PullStatus) -> Option<String> {
    let failed: Vec<&PullCheck> =
        pull.checks.iter().filter(|c| c.bucket() == CheckBucket::Failed).collect();
    if failed.is_empty() || !open(pull) {
        return None;
    }
    let named = format!("{} {}{}", pull.forge.noun(), pull.forge.mark(), pull.number);
    let mut text = format!("These checks failed on {named}:\n");
    for check in failed {
        let workflow = check.workflow.as_deref().map_or_else(String::new, |w| format!(" ({w})"));
        let link = check.link.as_deref().map_or_else(String::new, |l| format!(": {l}"));
        let _infallible = writeln!(text, "- {}{workflow}{link}", check.name);
    }
    let see = match pull.forge {
        Forge::GitHub => format!(
            "`gh pr checks {}` lists them, and `gh run view <run> --log-failed` shows a run's \
             failure",
            pull.number
        ),
        Forge::GitLab => "`glab ci view` shows the pipeline, and `glab ci trace <job>` a \
                          job's log"
            .to_owned(),
    };
    let _infallible = write!(
        text,
        "\nFind why each failed ({see}), fix the cause rather than the check, then commit and \
         push."
    );
    Some(text)
}

/// What "Address the review" tells the agent: each point still open, where it is and what was
/// said, the reviewer's own words first; none when nothing is open.
#[must_use]
pub fn address_review_words(pull: &PullStatus, comments: &PullComments) -> Option<String> {
    if comments.threads.is_empty() || !open(pull) {
        return None;
    }
    let named = format!("{} {}{}", pull.forge.noun(), pull.forge.mark(), pull.number);
    let mut text = format!(
        "Address the review still open on {named} ({}). For each point, change the code, or \
         where you disagree, tell me why instead. Then commit and push.\n",
        pull.url
    );
    let mut left = u64::from(comments.more);
    for (ix, thread) in comments.threads.iter().enumerate() {
        let place = match (&thread.path, thread.line) {
            (Some(path), Some(line)) => format!("{path}, line {line}"),
            (Some(path), None) => path.clone(),
            (None, _) => "The review".to_owned(),
        };
        let outdated = if thread.outdated { " (the code has changed since)" } else { "" };
        let mut point = format!("\n{}. {place}{outdated}:\n", ix.saturating_add(1));
        for note in &thread.notes {
            let body = note.body.lines().collect::<Vec<_>>().join("\n   ");
            let _infallible = writeln!(point, "   {}: {body}", note.author);
        }
        if text.len().saturating_add(point.len()) > STEP_TEXT_MAX {
            let unsaid = comments.threads.len().saturating_sub(ix);
            left = left.saturating_add(u64::try_from(unsaid).unwrap_or(u64::MAX));
            break;
        }
        text.push_str(&point);
    }
    if left > 0 {
        let _infallible = writeln!(text, "\n{left} more on its page: {}", pull.url);
    }
    Some(text)
}

/// What "Bring up to date" tells the agent, while the branch is behind its base or conflicts
/// with it; none otherwise.
#[must_use]
pub fn up_to_date_words(pull: &PullStatus) -> Option<String> {
    let conflicts = pull.mergeable.eq_ignore_ascii_case("CONFLICTING")
        || pull.merge_state.eq_ignore_ascii_case("DIRTY");
    let behind = pull.merge_state.eq_ignore_ascii_case("BEHIND");
    if !(conflicts || behind) || !open(pull) {
        return None;
    }
    let base = &pull.base;
    let why =
        if conflicts { format!("conflicts with `{base}`") } else { format!("is behind `{base}`") };
    let named = format!("{} {}{}", pull.forge.noun(), pull.forge.mark(), pull.number);
    Some(format!(
        "This branch {why} ({named}). Fetch `origin`, bring `origin/{base}` into it the way this \
         branch is kept (merge, or rebase if it is rebased), resolve any conflicts so both \
         sides' intent holds, run the tests, then commit and push."
    ))
}

/// What the sheet offers for an open pull request's merge.
///
/// It is read from the forge's own summary ([`PullStatus::merge_state`]) rather than from the
/// checks alone: a check the base branch does not require fails without blocking the forge's
/// merge, so it warns and does not refuse.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MergeGate {
    /// The forge merges it now (`CLEAN`, `HAS_HOOKS`, `UNSTABLE`); `warn` says what is not
    /// green that the forge does not require.
    Now {
        /// "Merges with 1 failing check", and kin.
        warn: Option<String>,
    },
    /// The forge takes it once what it waits on clears by itself (checks still running, a
    /// review not yet given, a merge queue): offered as auto-merge, with what it waits on.
    WhenReady {
        /// What it waits on, to follow "Merge waits: ": "checks running".
        waits: String,
    },
    /// Still a draft: marked ready first.
    Draft,
    /// Nothing to press until someone acts: why, to follow "Merge waits: ".
    Waits(String),
}

/// What the sheet offers for `pull`'s merge while it is open.
#[must_use]
pub fn merge_gate(pull: &PullStatus) -> MergeGate {
    let state = |s: &str| pull.merge_state.eq_ignore_ascii_case(s);
    let count = |bucket: CheckBucket| pull.checks.iter().filter(|c| c.bucket() == bucket).count();
    let (failing, running) = (count(CheckBucket::Failed), count(CheckBucket::Running));
    let changes = pull.review.eq_ignore_ascii_case("CHANGES_REQUESTED");
    if pull.draft || state("DRAFT") {
        return MergeGate::Draft;
    }
    if pull.mergeable.eq_ignore_ascii_case("CONFLICTING") || state("DIRTY") {
        return MergeGate::Waits(format!("conflicts with {}", pull.base));
    }
    if state("CLEAN") || state("HAS_HOOKS") || state("UNSTABLE") {
        let checks = |n: usize| kit::count(n as u64, "check", "checks");
        let warn = if failing > 0 {
            Some(format!("Merges with {} failing", checks(failing)))
        } else if changes {
            Some("Merges with changes requested".to_owned())
        } else if running > 0 {
            let verb = if running == 1 { "runs" } else { "run" };
            Some(format!("Merges while {} still {verb}", checks(running)))
        } else {
            None
        };
        return MergeGate::Now { warn };
    }
    if state("BLOCKED") {
        // A failure the base requires, or changes asked for, waits on someone's work, so
        // auto-merge would only hide it: the next steps above say what to do.
        if failing > 0 {
            return MergeGate::Waits("checks failing".to_owned());
        }
        if changes {
            return MergeGate::Waits("changes requested".to_owned());
        }
        let waits = if running > 0 {
            "checks running"
        } else if pull.review.eq_ignore_ascii_case("REVIEW_REQUIRED") {
            "a review"
        } else {
            "the branch's rules"
        };
        return MergeGate::WhenReady { waits: waits.to_owned() };
    }
    if state("BEHIND") {
        return MergeGate::Waits(format!("behind {}", pull.base));
    }
    let forge = match pull.forge {
        Forge::GitHub => "GitHub",
        Forge::GitLab => "GitLab",
    };
    MergeGate::Waits(format!("{forge} to finish checking it"))
}
