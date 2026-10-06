//! The review tile, drawn: the scope switch, the file list, the diff and its foot.
//!
//! The diff is one virtualized list of rows (a file's head, a hunk's head, a line, a comment,
//! the field a comment is written in), so a review of thousands of lines lays out only what is
//! in view. It is unified at every width, as `MonoCode`'s is: the files stacked, each folding
//! to its head, and all of them at once from the scope bar (`docs/decisions/ui.md`, "A diff is
//! unified, its files stacked and folding").
//!
//! A press on a line comments on it; a drag over a hunk's lines, or a shift-press past the
//! line commented on, comments on the run. A comment carries the code it is on, quoted, so the
//! agent reads what was meant. The comments go to the agent at once, or into the thread's
//! draft to send with more words.
//!
//! A folder's changes are reviewed the same way with no thread ([`Reviewed::Folder`]): its
//! working tree against `HEAD` or against the branch's base, read from its repository
//! (`GitOp::Changes`). It takes comments as a thread's review does, and with no agent to tell,
//! its foot sends them to a new agent in the folder, quoted into that start's composer to be
//! added to before it goes ([`ReviewEvent::NewAgent`]). It takes no keep or put back; the
//! commit sheet and who wrote each line are there as for a thread.
//!
//! "Review with `<agent>`" asks the thread's agent for its own review of the change on show,
//! through its own door ([`Intent::Review`]), where it has one. The tile holds what it shows
//! while the agent reviews, and reads the findings from the agent's answer once it rests: each
//! one on a line of the diff becomes a comment under it, marked as the agent's, and one with no
//! line on show is a note above the diff. The person lets any go and sends the rest as one
//! message, with their own comments.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, ListAlignment, ListState,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list, px,
};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use slopty_client::threads::Mirror;
use slopty_proto::RequestId;
use slopty_proto::git::GitOp;
use slopty_proto::thread::wire::{Intent, Review, ReviewScope};
use slopty_proto::thread::{
    AgentId, Cap, Delivery, IntentId, ItemBody, Phase, ThreadId, ThreadState, TurnId, TurnState,
};
use slopty_theme::{Theme, Typography};

use super::findings::{self, Finding};
use super::model::{self, Comment, Model, Note, Scope, Side};
use crate::authorship::{Authored, Opens, Writer};
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::diff::{self, Block, Kind, Line};
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::commit::{CommitEvent, CommitSheet};
use crate::conversation::thread::{HubEvent, ThreadHub};
use crate::conversation::{OpenCommit, RefreshPullRequest, ReviewWithAgent};
use crate::icons::{IconSize, Symbol};
use crate::kit;

/// The file list's width, in points.
const LIST_WIDTH: f32 = 240.0;

mod authors;
mod file_menu;

pub use file_menu::{COPY_PATH, COPY_PATH_IN_REPOSITORY, revert_words};

/// The most the band of the agent's findings above the diff takes before it scrolls.
const FINDINGS_HEIGHT: f32 = 240.0;

/// The share of the diff's room the findings may take drawn whole; past it they fold to one
/// line that opens on demand.
const FINDINGS_SHARE: f32 = 0.25;

/// How far past the viewport the diff lays rows out.
const OVERDRAW: f32 = 2048.0;

/// The most lines a comment's field grows to before it scrolls: a suggestion of a few lines
/// shows whole, as in the thread's composer.
const COMMENT_ROWS: usize = 8;

/// The foot's send while the comments are on their way.
pub const SENDING: &str = "Sending…";

/// Said above the diff when the worker turns the comments' send down; they stay.
pub const NOT_SENT: &str = "Comments not sent";

/// The foot's way to put the comments in the thread's draft.
const ADD_TO_MESSAGE: &str = "Add to message";

/// A folder's foot: its comments start a new agent in the folder.
pub const SEND_TO_NEW_AGENT: &str = "Send to a new agent";

/// The foot's way to keep every file as it is.
const MARK_REVIEWED: &str = "Mark reviewed";

/// About how wide a letter of the findings' words is, as a share of their size: how tall the
/// band would be drawn whole is guessed from it before it is laid out.
const LETTER: f32 = 0.55;

/// What the review says while the change is read.
const READING: &str = "Reading the changes\u{2026}";

/// The empty review's way to the widest span of its kind.
const fn widen_words(scope: Scope) -> &'static str {
    match scope {
        Scope::WholeBranch => "Show the whole branch",
        Scope::LastTurn | Scope::SinceReviewed | Scope::AllTurns | Scope::Uncommitted => {
            "Show all turns"
        }
    }
}

/// The foot's "Add to message", by its key in its row.
const FOOT_ADD: &str = "add";

/// The foot's "Mark reviewed", by its key in its row.
const FOOT_MARK: &str = "mark";

/// What the other spans give way to in the scope bar: only Commit outranks them.
const SCOPE_PRIORITY: kit::Priority = kit::Priority(176);

/// The scope bar's refresh, by its key in its row.
const BAR_REFRESH: &str = "refresh";

/// The scope bar's switch that folds or opens every file, by its key in its row.
const BAR_FOLD: &str = "fold";

/// The fold switch while a file is open.
pub const COLLAPSE_ALL: &str = "Collapse all files";

/// The fold switch while every file is folded.
pub const EXPAND_ALL: &str = "Expand all files";

/// The scope bar's way to the agent's own review.
const BAR_AGENT: &str = "agent";

/// The scope bar's pull request.
const BAR_PULL: &str = "pull";

/// The scope bar's way to the commit sheet.
const BAR_COMMIT: &str = "commit";

/// The words of the way to the commit sheet.
const COMMIT: &str = "Commit\u{2026}";

/// What the tile tells its host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReviewEvent {
    /// The comments went to the agent: the thread is where its answer shows.
    CommentsSent {
        /// The thread.
        thread: ThreadId,
    },
    /// The comments are for the thread's draft, to go with more words: the host puts `text`
    /// at its end and gives it the keyboard, then says whether a composer took it
    /// ([`ReviewView::added`]). The comments stay until one did.
    AddToMessage {
        /// The thread.
        thread: ThreadId,
        /// The comments as one message.
        text: String,
        /// Which hand-over this is, for its answer.
        id: u64,
    },
    /// A folder's comments are for a new agent there: the host opens a start in `folder` with
    /// `text` in its composer, then says whether one took it ([`ReviewView::added`]). The
    /// comments stay until one did.
    NewAgent {
        /// The folder reviewed, where the agent starts.
        folder: String,
        /// The comments as one message.
        text: String,
        /// Which hand-over this is, for its answer.
        id: u64,
    },
    /// The person pressed who wrote a line: open the thread that did, at its turn.
    OpenThread(Opens),
    /// The person chose Open in a file's menu: open the file, whole, in a tile of its own on
    /// the review's machine.
    OpenFile {
        /// Its whole path on the machine.
        path: String,
    },
}

/// One row of the diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    /// A file's head: its path, its size, keep and put back.
    File(usize),
    /// A file with no lines to show: binary, or too large.
    Bare(usize),
    /// A hunk's head: where it is, keep and put back.
    Hunk(usize, usize),
    /// A line of a hunk.
    Line(usize, usize, usize),
    /// A comment waiting, by its place among them.
    Comment(usize),
    /// The field a comment is written in.
    Draft,
}

/// Lines of one hunk picked for a comment, by their place in the hunk: where the press went
/// down and where it is now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Span {
    at: usize,
    hunk: usize,
    from: usize,
    to: usize,
}

impl Span {
    /// The first and last rows picked.
    fn range(self) -> (usize, usize) {
        (self.from.min(self.to), self.from.max(self.to))
    }

    /// Whether row `ix` of hunk `hunk` of the file at `at` is picked.
    fn holds(self, at: usize, hunk: usize, ix: usize) -> bool {
        let (lo, hi) = self.range();
        self.at == at && self.hunk == hunk && (lo..=hi).contains(&ix)
    }
}

/// The agent's own review, asked and not yet answered.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Reviewing {
    /// The intent that asked it.
    intent: IntentId,
    /// The thread's last turn when it was asked: the answer is in the turns after it.
    after: Option<TurnId>,
    /// The agent, by name.
    agent: String,
}

/// How the agent's own review came out, said above the diff until the person lets it go.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Came {
    /// The agent, by name.
    agent: String,
    /// What it came to, in words.
    words: String,
    /// The intent the worker turned down, when it did not run.
    refused: Option<IntentId>,
}

/// The lines a comment is being written on.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Drafting {
    path: String,
    line: u32,
    end: u32,
    side: Side,
    anchor: u64,
    quote: String,
    /// The rows picked.
    span: Span,
    /// The row it hangs under: the last picked.
    after: Row,
}

/// What a review tile reviews.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reviewed {
    /// A thread's work, between the snapshots of its turns.
    Thread(ThreadId),
    /// A folder's working tree, with no thread: absolute, or `~/…`.
    Folder(String),
}

/// The review tile.
pub struct ReviewView {
    hub: Entity<ThreadHub>,
    reviewed: Reviewed,
    theme: Theme,
    width: f32,
    /// The tile's body at rest, under its header, in points.
    height: f32,
    scope: Scope,
    /// The branch a folder's whole branch is compared with; its base when `None`.
    branch: Option<String>,
    /// The scope asked for, and over which turn: asked again when the thread moves to a new
    /// turn.
    asked: Option<ReviewScope>,
    model: Model,
    /// Each file's diff, numbered and coloured once per review.
    blocks: HashMap<usize, Rc<[Block]>>,
    rows: Vec<Row>,
    list: ListState,
    /// The rows the pointer is picking, while it is down.
    marking: Option<Span>,
    drafting: Option<Drafting>,
    draft: Entity<TextareaState>,
    /// Keeps and put-backs this tile sent, until the worker has acted on them: then the
    /// review is asked for again.
    picks: HashSet<IntentId>,
    /// The branch's pull request was asked for, once the thread's folder was known.
    pull_asked: bool,
    /// The last of the person's git ops in a folder's repository that this tile read its
    /// changes again after.
    said: Option<RequestId>,
    /// The commit sheet over the tile, while it is open.
    commit: Option<(Entity<CommitSheet>, Subscription)>,
    /// The person's comments on their way to the agent, kept until the worker takes the send.
    sending: Option<(IntentId, model::Batch)>,
    /// The comments handed to the thread's draft, kept until a composer takes them, and the
    /// number of the last hand-over.
    adding: Option<(u64, model::Batch)>,
    adds: u64,
    /// The foot's "More" menu is open.
    more_open: bool,
    /// What the foot left out at its last layout, which its "More" offers.
    foot_dropped: kit::Dropped,
    /// The scope bar's "More" menu is open.
    scopes_open: bool,
    /// What the scope bar left out at its last layout, which its "More" offers.
    scopes_dropped: kit::Dropped,
    /// The agent's own review, while it runs.
    reviewing: Option<Reviewing>,
    /// How the last one came out, until the person lets it go.
    came: Option<Came>,
    /// The span the agent was asked to review, held on show while it reviews and while its
    /// findings are here, though the thread moves on.
    pinned: Option<ReviewScope>,
    focus: FocusHandle,
    /// Who wrote the lines of each file, by its place in the review, once the worker has said.
    authored: HashMap<usize, Authored>,
    /// The files whose authors were asked for this review.
    authors_asked: HashSet<usize>,
    /// The threads its lines' authors name, as the host names them ([`Self::set_writers`]).
    writers: HashMap<ThreadId, Writer>,
    /// The row under the pointer, by its file, hunk and place: who wrote it shows there.
    hovered: Option<(usize, usize, usize)>,
    /// The hunk whose head is under the pointer, by its file and place.
    hunk_hovered: Option<(usize, usize)>,
    /// The person opened the agent's findings while they are folded to their summary.
    findings_open: bool,
    /// A file's own menu, while it is open.
    file_menu: Option<file_menu::FileMenu>,
    /// The files folded to their heads, by path: kept across the review's updates and spans.
    folded: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ReviewView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewView")
            .field("reviewed", &self.reviewed)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl Focusable for ReviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ReviewView {
    /// The review of `thread` from `hub`, over the last turn. The thread is followed while the
    /// tile is open: its review frames come on its stream.
    pub fn new(
        hub: Entity<ThreadHub>,
        thread: ThreadId,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::of(hub, Reviewed::Thread(thread), None, theme, window, cx)
    }

    /// The review of the folder `path`'s changes from `hub`'s machine, with no thread: what is
    /// not committed, read from its repository.
    pub fn folder(
        hub: Entity<ThreadHub>,
        path: String,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::of(hub, Reviewed::Folder(path), None, theme, window, cx)
    }

    /// A folder's whole branch since it left `branch`, as a finished task's review shows what
    /// its merge into the project's target would bring; the scope bar can still turn it to
    /// what is not committed.
    pub fn branch(
        hub: Entity<ThreadHub>,
        path: String,
        branch: String,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::of(hub, Reviewed::Folder(path), Some(branch), theme, window, cx)
    }

    fn of(
        hub: Entity<ThreadHub>,
        reviewed: Reviewed,
        branch: Option<String>,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let folder = matches!(reviewed, Reviewed::Folder(_));
        // Several lines, kept as pasted: ↵ adds the comment and ⇧↵ breaks the line, as in the
        // thread's composer.
        let draft = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Comment on this line")
                .auto_grow(1, COMMENT_ROWS)
                .submit_on_enter(true)
        });
        let writing = cx.subscribe_in(&draft, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = event {
                this.add_comment(window, cx);
            }
        });
        let hearing = cx.subscribe(&hub, |this, _hub, event, cx| match event {
            HubEvent::Review(t) if this.own() == Some(*t) => this.reviewed(cx),
            HubEvent::Thread(t) if this.own() == Some(*t) => this.thread_moved(cx),
            HubEvent::Git(repo) if this.repo(cx).as_ref() == Some(repo) => this.git_moved(cx),
            HubEvent::Authors => this.authors_came(cx),
            _ => {}
        });
        let watching = cx.observe(&draft, |_, _, cx| cx.notify());
        if let Reviewed::Thread(thread) = &reviewed {
            let thread = *thread;
            cx.on_release(move |this, cx| {
                this.hub.update(cx, |hub, cx| hub.close(thread, cx));
            })
            .detach();
        }
        let mut view = Self {
            hub,
            reviewed,
            theme,
            width: 0.0,
            height: 0.0,
            scope: match (folder, &branch) {
                (true, Some(_)) => Scope::WholeBranch,
                (true, None) => Scope::Uncommitted,
                (false, _) => Scope::default(),
            },
            branch,
            asked: None,
            model: Model::default(),
            blocks: HashMap::new(),
            rows: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(OVERDRAW)),
            marking: None,
            drafting: None,
            draft,
            picks: HashSet::new(),
            pull_asked: false,
            said: None,
            commit: None,
            sending: None,
            adding: None,
            adds: 0,
            more_open: false,
            foot_dropped: kit::Dropped::default(),
            scopes_open: false,
            scopes_dropped: kit::Dropped::default(),
            reviewing: None,
            came: None,
            pinned: None,
            focus: cx.focus_handle(),
            authored: HashMap::new(),
            authors_asked: HashSet::new(),
            writers: HashMap::new(),
            hovered: None,
            hunk_hovered: None,
            findings_open: false,
            file_menu: None,
            folded: HashSet::new(),
            _subscriptions: vec![writing, hearing, watching],
        };
        if let Some(thread) = view.own() {
            view.hub.update(cx, |hub, cx| hub.open(thread, cx));
        }
        view.reviewed(cx);
        view.ask(cx);
        view.ask_pull(cx);
        view
    }

    /// The thread it reviews; none for a folder's changes.
    #[must_use]
    pub const fn thread(&self) -> Option<ThreadId> {
        self.own()
    }

    /// What it reviews.
    #[must_use]
    pub const fn subject(&self) -> &Reviewed {
        &self.reviewed
    }

    const fn own(&self) -> Option<ThreadId> {
        match self.reviewed {
            Reviewed::Thread(thread) => Some(thread),
            Reviewed::Folder(_) => None,
        }
    }

    /// The title of the thread it reviews, once known.
    #[must_use]
    pub fn title(&self, cx: &App) -> Option<String> {
        self.hub.read(cx).threads().title(self.own()?).map(str::to_owned)
    }

    /// The span on show.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Draw in a tile `width` points wide at rest whose body is `height` points tall.
    pub fn set_layout(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        self.width = width;
        self.height = height;
        self.list.remeasure();
        cx.notify();
    }

    /// Draw in `theme`.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if let Some((sheet, _)) = &self.commit {
            sheet.update(cx, |sheet, cx| sheet.set_theme(theme.clone(), cx));
        }
        self.theme = theme;
        self.list.remeasure();
        cx.notify();
    }

    /// Show `scope`.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.asked = None;
            self.pinned = None;
            self.ask(cx);
            cx.notify();
        }
    }

    /// The tile's room, from its width at rest: a wide one sets the file list beside the diff.
    fn room(&self) -> kit::Room {
        kit::Room::of(self.width, &self.theme)
    }

    // ----- what comes ------------------------------------------------------------------

    /// Ask for the scope on show, unless it was asked over the same turns already. While the
    /// agent reviews, or its findings are here, the change it was asked about stays on show.
    fn ask(&mut self, cx: &mut Context<Self>) {
        let thread = match &self.reviewed {
            Reviewed::Thread(thread) => *thread,
            Reviewed::Folder(path) => {
                let (path, scope) = (path.clone(), self.scope.wire_alone(self.branch.as_deref()));
                let Some(ReviewScope::WorkingTree(against)) = scope.clone() else { return };
                self.asked = scope;
                let op = GitOp::Changes { against };
                let _asked = self.hub.update(cx, |hub, cx| hub.git_op(&path, op, cx));
                return;
            }
        };
        let hub = self.hub.read(cx);
        let Some(state) = hub.threads().mirror(thread).and_then(Mirror::state) else {
            return;
        };
        let held = self.reviewing.is_some() || self.model.has_findings();
        let pinned = self.pinned.clone().filter(|_| held);
        // A thread on a pull request's branch reads its whole change against the branch that
        // pull request merges into, as the forge names it, not a guessed base.
        let base = state.pull.as_ref().map(|pull| pull.base.as_str());
        let Some(scope) = pinned.or_else(|| self.scope.wire(state, base)) else {
            return;
        };
        if self.asked.as_ref() == Some(&scope) {
            return;
        }
        self.asked = Some(scope.clone());
        self.hub.update(cx, |hub, cx| hub.ask_review(thread, scope, cx));
    }

    /// The folder the thread works in, when the worker said: its repository is the one the
    /// commit sheet and the pull request are of.
    fn repo(&self, cx: &App) -> Option<String> {
        let thread = match &self.reviewed {
            Reviewed::Thread(thread) => *thread,
            Reviewed::Folder(path) => return Some(path.clone()),
        };
        let hub = self.hub.read(cx);
        let state = hub.threads().mirror(thread).and_then(Mirror::state)?;
        Some(state.meta.cwd.clone()).filter(|cwd| !cwd.trim().is_empty())
    }

    /// Whether the person's own editor opens the reviewed folder ([`crate::file::open_with`]).
    fn editor_opens(&self, cx: &App) -> bool {
        self.repo(cx).is_some_and(|repo| {
            crate::file::open_with::offers_named(self.hub.read(cx).worker(), &repo, cx)
        })
    }

    /// Open the reviewed folder, its repository, in the person's own editor.
    fn open_in_editor(&self, cx: &App) {
        use crate::file::open_with;
        let Some(repo) = self.repo(cx) else { return };
        if let Some(opening) = open_with::opening_named(self.hub.read(cx).worker(), &repo, None, cx)
        {
            open_with::open(&opening, cx);
        }
    }

    /// Ask the branch's pull request once the folder is known: the tile shows where it stands.
    /// The repository's root is asked with it, once, where it is not known: the review names
    /// its files from there.
    fn ask_pull(&mut self, cx: &mut Context<Self>) {
        if self.pull_asked {
            return;
        }
        let Some(repo) = self.repo(cx) else { return };
        self.pull_asked = true;
        let rooted = self.root(cx).is_some();
        let _asked = self.hub.update(cx, |hub, cx| {
            if !rooted {
                let _status = hub.git_op(&repo, GitOp::Status, cx);
            }
            hub.git_op(&repo, GitOp::PullStatus, cx)
        });
    }

    /// The root of the repository the reviewed folder is in, as its status last said: where
    /// the review's paths start.
    fn root(&self, cx: &App) -> Option<String> {
        let repo = self.repo(cx)?;
        let git = self.hub.read(cx).git().repo(&repo)?;
        git.status.as_ref().map(|status| status.root.clone())
    }

    /// Open the commit sheet over the tile.
    pub fn open_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((sheet, _)) = &self.commit {
            sheet.update(cx, |sheet, cx| sheet.refresh(cx));
            return;
        }
        let Some(repo) = self.repo(cx) else { return };
        let (hub, theme, thread) = (self.hub.clone(), self.theme.clone(), self.own());
        // A thread's review offers its agent the commit; a folder's has none to ask.
        let sheet = cx.new(|cx| {
            let sheet = CommitSheet::new(hub, repo, theme, window, cx);
            match thread {
                Some(thread) => sheet.asking(thread),
                None => sheet,
            }
        });
        let closing =
            cx.subscribe_in(&sheet, window, |this, _sheet, event, window, cx| match event {
                CommitEvent::Close => {
                    this.commit = None;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }
            });
        self.commit = Some((sheet, closing));
        cx.notify();
    }

    fn refresh_pull(&mut self, cx: &mut Context<Self>) {
        if let Some((sheet, _)) = &self.commit {
            sheet.update(cx, |sheet, cx| sheet.refresh(cx));
            return;
        }
        self.pull_asked = false;
        self.ask_pull(cx);
    }

    /// The thread moved on: a new turn asks the last-turn review again, and a keep or a put
    /// back the worker acted on asks the review again.
    fn thread_moved(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.own() else { return };
        let hub = self.hub.read(cx);
        let waiting: HashSet<IntentId> = hub
            .threads()
            .outbox()
            .of(thread)
            .filter(|s| s.outcome.is_none())
            .map(|s| s.id)
            .collect();
        let acted = self.picks.iter().any(|id| !waiting.contains(id));
        self.picks.retain(|id| waiting.contains(id));
        if acted {
            self.asked = None;
        }
        self.settle_review(cx);
        self.settle_send(cx);
        self.ask(cx);
        self.ask_pull(cx);
        cx.notify();
    }

    /// The worker's review came: a thread's on its stream, a folder's from its repository.
    fn reviewed(&mut self, cx: &mut Context<Self>) {
        let hub = self.hub.read(cx);
        let review = match (&self.reviewed, self.scope.wire_alone(self.branch.as_deref())) {
            (Reviewed::Thread(thread), _) => {
                self.asked.as_ref().and_then(|scope| hub.review(*thread, scope)).cloned()
            }
            (Reviewed::Folder(path), Some(ReviewScope::WorkingTree(against))) => {
                hub.git().repo(path).and_then(|r| r.changes.get(&against)).cloned()
            }
            (Reviewed::Folder(_), _) => None,
        };
        let Some(review) = review else { return };
        // A folder hears every word of its repository; only a new reading is shown again.
        let folder = self.own().is_none();
        if folder && self.model.review().is_some_and(|shown| Arc::ptr_eq(shown, &review)) {
            return;
        }
        self.show(review);
        self.author_review(cx);
        cx.notify();
    }

    /// The repository said something: a folder's changes came, or a commit or a merge went,
    /// after which they are read again.
    fn git_moved(&mut self, cx: &mut Context<Self>) {
        if let Reviewed::Folder(path) = &self.reviewed {
            let said = self.hub.read(cx).git().repo(path).and_then(|r| r.said.clone());
            let went = said.as_ref().is_some_and(|(_, said)| said.went());
            let request = said.map(|(request, _)| request);
            if went && request != self.said {
                self.said = request;
                self.ask(cx);
            }
            self.reviewed(cx);
        }
        cx.notify();
    }

    /// Read the folder's changes again.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.asked = None;
        self.ask(cx);
        cx.notify();
    }

    /// Show `review`.
    pub fn show(&mut self, review: Arc<Review>) {
        self.model.set_review(review);
        self.blocks.clear();
        if let Some(review) = self.model.review().cloned() {
            for (at, file) in review.files.iter().enumerate() {
                if !file.binary {
                    self.blocks.insert(at, diff::thread_blocks(&file.path, &file.patch));
                }
            }
        }
        self.drafting = None;
        self.marking = None;
        self.rebuild();
    }

    /// Lay the diff's rows out again: every file in the list's order, its hunks, its lines,
    /// the comments under the lines they are on.
    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        for listed in self.model.listed() {
            let at = listed.at;
            rows.push(Row::File(at));
            if self.model.file(at).is_some_and(|f| self.folded.contains(&f.path)) {
                continue;
            }
            let Some(blocks) = self.blocks.get(&at).filter(|b| !b.is_empty()) else {
                rows.push(Row::Bare(at));
                continue;
            };
            let path = self.model.file(at).map(|f| f.path.clone()).unwrap_or_default();
            for (hunk, block) in blocks.iter().enumerate() {
                rows.push(Row::Hunk(at, hunk));
                for (ix, line) in block.lines.iter().enumerate() {
                    let row = Row::Line(at, hunk, ix);
                    rows.push(row);
                    self.under(&mut rows, &path, &[line], row);
                }
            }
        }
        let old = self.rows.len();
        self.list.splice(0..old, rows.len());
        self.rows = rows;
    }

    /// What hangs under `row`, whose lines are `lines`: their comments, and the field when one
    /// is being written there.
    fn under(&self, rows: &mut Vec<Row>, path: &str, lines: &[&Line], row: Row) {
        for (ix, c) in self.model.comments().iter().enumerate() {
            if c.path == path && lines.iter().any(|l| on(l, c.side, c.end)) {
                rows.push(Row::Comment(ix));
            }
        }
        if self.drafting.as_ref().is_some_and(|d| d.after == row) {
            rows.push(Row::Draft);
        }
    }

    // ----- what the person does --------------------------------------------------------

    /// Send `intent` to the thread; nothing for a folder's changes, which have none.
    fn intent(&self, intent: Intent, cx: &mut Context<Self>) -> Option<IntentId> {
        let thread = self.own()?;
        Some(self.hub.update(cx, |hub, cx| hub.intent(thread, intent, cx)))
    }

    /// Keep or put back the file at `at`, or one of its hunks. A refusal of an earlier try
    /// goes: this one speaks for it now.
    fn pick(&mut self, at: usize, hunk: Option<usize>, keep: bool, cx: &mut Context<Self>) {
        if let Some((refused, _)) = self.refused(at, hunk, cx) {
            self.hub.update(cx, |hub, cx| hub.dismiss(refused, cx));
        }
        let hunks = hunk.and_then(|h| u32::try_from(h).ok()).into_iter().collect();
        if let Some(id) = self.model.pick(at, hunks, keep).and_then(|i| self.intent(i, cx)) {
            self.picks.insert(id);
        }
    }

    /// Keep every file shown.
    fn mark_reviewed(&mut self, cx: &mut Context<Self>) {
        for intent in self.model.keep_all() {
            if let Some(id) = self.intent(intent, cx) {
                self.picks.insert(id);
            }
        }
    }

    /// The agent that reviews the thread's changes through its own door, by name, where it has
    /// one ([`Cap::REVIEW`]).
    #[must_use]
    pub fn door(&self, cx: &App) -> Option<String> {
        self.door_of(cx).map(|(_, name)| name)
    }

    /// The agent that reviews through its own door, and its name.
    fn door_of(&self, cx: &App) -> Option<(AgentId, String)> {
        let hub = self.hub.read(cx);
        let state = hub.threads().mirror(self.own()?).and_then(Mirror::state)?;
        let agent = state.meta.agent.clone();
        let name = crate::conversation::thread::view::agent_label(&agent);
        state.meta.can(Cap::REVIEW).then_some((agent, name))
    }

    /// Whether the agent's own review runs now.
    #[must_use]
    pub const fn reviewing(&self) -> bool {
        self.reviewing.is_some()
    }

    /// Ask the thread's agent for its own review of the change on show. The span stays on show
    /// until its findings are answered or let go.
    pub fn review_with_agent(&mut self, cx: &mut Context<Self>) {
        if self.reviewing.is_some() {
            return;
        }
        let Some(agent) = self.door(cx) else { return };
        let Some(review) = self.model.review().filter(|r| !r.files.is_empty()) else { return };
        let (Some(from), Some(to)) = (review.from.clone(), review.to.clone()) else { return };
        let Some(thread) = self.own() else { return };
        let after = self
            .hub
            .read(cx)
            .threads()
            .mirror(thread)
            .and_then(Mirror::state)
            .and_then(|s| s.last_turn().map(|t| t.id));
        let Some(intent) = self.intent(Intent::Review { from, to }, cx) else { return };
        self.pinned.clone_from(&self.asked);
        self.came = None;
        self.reviewing = Some(Reviewing { intent, after, agent });
        cx.notify();
    }

    /// The agent's review was refused, or has ended: its findings are read from what it
    /// answered, each put on its line in the diff or kept as a note.
    fn settle_review(&mut self, cx: &App) {
        let Some(asked) = self.reviewing.clone() else { return };
        let Some(thread) = self.own() else { return };
        let hub = self.hub.read(cx);
        let refused = hub.refusals(thread).find(|r| r.id == asked.intent).map(|r| r.words.clone());
        if let Some(words) = refused {
            self.reviewing = None;
            self.pinned = None;
            self.came = Some(Came { agent: asked.agent, words, refused: Some(asked.intent) });
            return;
        }
        let Some(state) = hub.threads().mirror(thread).and_then(Mirror::state) else {
            return;
        };
        let Some(answer) = answered(state, &asked) else { return };
        self.reviewing = None;
        let found = findings::read(&answer);
        let agent = asked.agent;
        let words = match found.len() {
            0 => findings::summary(&answer).map_or_else(
                || format!("{agent} raised nothing"),
                |said| format!("{agent} raised nothing: {said}"),
            ),
            1 => format!("{agent} raised 1 finding"),
            n => format!("{agent} raised {n} findings"),
        };
        for finding in found {
            match self.placed(&agent, &finding) {
                Some(comment) => self.model.comment(comment),
                None => self.model.note(Note { by: agent.clone(), finding }),
            }
        }
        if !self.model.has_findings() {
            self.pinned = None;
        }
        self.came = Some(Came { agent, words, refused: None });
        self.rebuild();
    }

    /// `finding` as a comment on its lines in the diff on show, when they are in it.
    fn placed(&self, agent: &str, finding: &Finding) -> Option<Comment> {
        let place = finding.place.as_ref()?;
        let (from, to) = place.lines?;
        let review = self.model.review()?;
        let path = findings::resolve(&place.path, review.files.iter().map(|f| f.path.as_str()))?;
        let at = review.files.iter().position(|f| f.path == path)?;
        let blocks = self.blocks.get(&at)?;
        let lines: Vec<&Line> = blocks
            .iter()
            .flat_map(|b| b.lines.iter())
            .filter(|l| l.kind != Kind::Removed && l.new.is_some_and(|n| (from..=to).contains(&n)))
            .collect();
        let (first, last) = (lines.first()?, lines.last()?);
        Some(Comment {
            path: path.to_owned(),
            line: first.new?,
            end: last.new?,
            side: Side::New,
            anchor: model::anchor(&first.text),
            quote: diff::quote(path, &lines),
            body: finding.text(),
            by: Some(agent.to_owned()),
        })
    }

    /// Let the word of how the agent's review came out go.
    fn dismiss_came(&mut self, cx: &mut Context<Self>) {
        if let Some(refused) = self.came.take().and_then(|c| c.refused) {
            self.hub.update(cx, |hub, cx| hub.dismiss(refused, cx));
        }
        cx.notify();
    }

    fn unnote(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.model.unnote(ix);
        self.release_pin();
        cx.notify();
    }

    /// The agent's findings are all gone: the tile follows its scope again.
    fn release_pin(&mut self) {
        if self.reviewing.is_none() && !self.model.has_findings() && self.pinned.take().is_some() {
            self.asked = None;
        }
    }

    /// Whether comments are on their way, sent or handed to the draft: they wait for that to
    /// end before going again.
    #[must_use]
    pub const fn comments_away(&self) -> bool {
        self.sending.is_some() || self.adding.is_some()
    }

    /// Send the comments as one message. They stay until the worker takes it
    /// ([`Self::settle_send`]): a send turned down loses none.
    fn send_comments(&mut self, cx: &mut Context<Self>) {
        if self.comments_away() {
            return;
        }
        let Some((text, batch)) = self.model.message() else { return };
        // Sent now, as the thread's own composer sends (`ThreadMeta::delivery_now`).
        let delivery = self.own().and_then(|thread| {
            let hub = self.hub.read(cx);
            hub.threads().mirror(thread).and_then(Mirror::state).map(|s| s.meta.delivery_now())
        });
        let send = Intent::Send {
            text,
            delivery: delivery.unwrap_or(Delivery::Steer),
            attachments: Vec::new(),
        };
        let Some(id) = self.intent(send, cx) else { return };
        self.sending = Some((id, batch));
        self.came = None;
        cx.notify();
    }

    /// The worker answered the comments' send: taken, they go; turned down, they stay, and
    /// why is said above the diff.
    fn settle_send(&mut self, cx: &mut Context<Self>) {
        let Some((id, _)) = &self.sending else { return };
        let Some(thread) = self.own() else { return };
        let hub = self.hub.read(cx);
        let sent = hub.threads().outbox().of(thread).find(|s| s.id == *id);
        if sent.is_some_and(|s| s.outcome.is_none()) {
            return;
        }
        if let Some(why) = sent.filter(|s| s.failed()).map(|s| s.failure().unwrap_or_default()) {
            let agent = hub
                .threads()
                .mirror(thread)
                .and_then(Mirror::state)
                .map(|st| crate::conversation::thread::view::agent_label(&st.meta.agent))
                .unwrap_or_default();
            let words =
                if why.is_empty() { NOT_SENT.to_owned() } else { format!("{NOT_SENT}: {why}") };
            let refused = self.sending.take().map(|(id, _)| id);
            self.came = Some(Came { agent, words, refused });
            return;
        }
        let Some((_, batch)) = self.sending.take() else { return };
        self.model.forget(&batch);
        self.release_pin();
        self.rebuild();
        cx.emit(ReviewEvent::CommentsSent { thread });
    }

    /// Put the comments into the thread's draft rather than send them: the host gives the
    /// draft the keyboard, and says whether a composer took them ([`Self::added`]).
    fn add_to_message(&mut self, cx: &mut Context<Self>) {
        if self.comments_away() {
            return;
        }
        let Some(thread) = self.own() else { return };
        let Some((text, batch)) = self.model.message() else { return };
        self.adds = self.adds.wrapping_add(1);
        let id = self.adds;
        self.adding = Some((id, batch));
        cx.emit(ReviewEvent::AddToMessage { thread, text, id });
        cx.notify();
    }

    /// A folder's comments go to a new agent there: the host opens its start with them in the
    /// composer, and says whether one took them ([`Self::added`]).
    fn send_to_new_agent(&mut self, cx: &mut Context<Self>) {
        if self.comments_away() {
            return;
        }
        let Reviewed::Folder(folder) = &self.reviewed else { return };
        let folder = folder.clone();
        let Some((text, batch)) = self.model.message() else { return };
        self.adds = self.adds.wrapping_add(1);
        let id = self.adds;
        self.adding = Some((id, batch));
        cx.emit(ReviewEvent::NewAgent { folder, text, id });
        cx.notify();
    }

    /// Keep a finding as a note beside the comments, as the agent's own review does, and press
    /// "Add to message": what the host does with comments, with no diff to comment on.
    #[cfg(test)]
    pub(crate) fn add_note_to_message(&mut self, words: &str, cx: &mut Context<Self>) {
        let finding = Finding { title: words.to_owned(), body: String::new(), place: None };
        self.model.note(Note { by: "Codex".to_owned(), finding });
        self.add_to_message(cx);
    }

    /// How many comments and notes wait to be sent.
    #[must_use]
    pub const fn waiting(&self) -> usize {
        self.model.waiting()
    }

    /// Hand-over `id` of the comments to the thread's draft ended: `taken` by a composer, they
    /// go; else they stay, to be sent or added again.
    pub fn added(&mut self, id: u64, taken: bool, cx: &mut Context<Self>) {
        if self.adding.as_ref().is_none_or(|(at, _)| *at != id) {
            return;
        }
        let Some((_, batch)) = self.adding.take() else { return };
        if taken {
            self.model.forget(&batch);
            self.came = None;
            self.release_pin();
            self.rebuild();
        }
        cx.notify();
    }

    /// The lines of `span`, in the diff's order.
    fn span_lines(&self, span: Span) -> Vec<Line> {
        let Some(block) = self.blocks.get(&span.at).and_then(|b| b.get(span.hunk)) else {
            return Vec::new();
        };
        let (lo, hi) = span.range();
        block.lines.get(lo..=hi).map(<[Line]>::to_vec).unwrap_or_default()
    }

    /// The pointer went down on row `ix` of a hunk: it is picked, and a drag picks on from it.
    /// With shift, the run goes from the row a comment is being written on.
    fn press_line(
        &mut self,
        at: usize,
        hunk: usize,
        ix: usize,
        shift: bool,
        cx: &mut Context<Self>,
    ) {
        let from = self
            .drafting
            .as_ref()
            .map(|d| d.span)
            .filter(|s| shift && s.at == at && s.hunk == hunk)
            .map_or(ix, |s| s.from);
        self.marking = Some(Span { at, hunk, from, to: ix });
        cx.notify();
    }

    /// The pointer dragged onto row `ix` of a hunk: the run reaches it, within the hunk it
    /// started in.
    fn drag_line(&mut self, at: usize, hunk: usize, ix: usize, cx: &mut Context<Self>) {
        if let Some(span) = &mut self.marking
            && span.at == at
            && span.hunk == hunk
            && span.to != ix
        {
            span.to = ix;
            cx.notify();
        }
    }

    /// The pointer let go: a comment starts on the rows it picked.
    fn release_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(span) = self.marking.take() {
            self.start_comment(span, window, cx);
        }
    }

    /// Start a comment on the lines of `span`, under its last row. Its numbers are the new
    /// file's, the old file's for removals alone.
    fn start_comment(&mut self, span: Span, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.model.file(span.at).map(|f| f.path.clone()) else { return };
        let lines = self.span_lines(span);
        let new: Vec<&Line> = lines.iter().filter(|l| l.kind != Kind::Removed).collect();
        let (side, numbered) = if new.is_empty() {
            (Side::Old, lines.iter().filter(|l| l.old.is_some()).collect::<Vec<_>>())
        } else {
            (Side::New, new.into_iter().filter(|l| l.new.is_some()).collect())
        };
        let number = |l: &&Line| if side == Side::Old { l.old } else { l.new };
        let (Some(first), Some(line), Some(end)) = (
            numbered.first(),
            numbered.iter().filter_map(number).min(),
            numbered.iter().filter_map(number).max(),
        ) else {
            return;
        };
        let quoted: Vec<&Line> = lines.iter().collect();
        self.drafting = Some(Drafting {
            path: path.clone(),
            line,
            end,
            side,
            anchor: model::anchor(&first.text),
            quote: diff::quote(&path, &quoted),
            span,
            after: Row::Line(span.at, span.hunk, span.range().1),
        });
        let placeholder =
            if end > line { "Comment on these lines" } else { "Comment on this line" };
        self.draft.update(cx, |d, cx| {
            d.set_value("", window, cx);
            d.set_placeholder(placeholder, window, cx);
            d.focus(window, cx);
        });
        self.rebuild();
        cx.notify();
    }

    fn add_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drafting.take() else { return };
        let body = self.draft.read(cx).value().to_string();
        self.model.comment(Comment {
            path: d.path,
            line: d.line,
            end: d.end,
            side: d.side,
            anchor: d.anchor,
            quote: d.quote,
            body,
            by: None,
        });
        self.draft.update(cx, |d, cx| d.set_value("", window, cx));
        self.rebuild();
        cx.notify();
    }

    fn uncomment(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.model.uncomment(ix);
        self.release_pin();
        self.rebuild();
        cx.notify();
    }

    /// Fold the file at `at` to its head, or open it again.
    fn toggle_file(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(path) = self.model.file(at).map(|f| f.path.clone()) else { return };
        if !self.folded.remove(&path) {
            self.folded.insert(path);
        }
        self.rebuild();
        cx.notify();
    }

    /// Whether every file is folded to its head: the scope bar's switch then opens them all.
    pub(super) fn all_folded(&self) -> bool {
        self.model.review().is_some_and(|r| r.files.iter().all(|f| self.folded.contains(&f.path)))
    }

    /// Fold every file to its head, or open every one while all are folded.
    fn fold_all(&mut self, cx: &mut Context<Self>) {
        if self.all_folded() {
            self.folded.clear();
        } else if let Some(review) = self.model.review() {
            self.folded = review.files.iter().map(|f| f.path.clone()).collect();
        }
        self.rebuild();
        cx.notify();
    }

    fn reveal(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(file) = self.model.file(at)
            && self.folded.remove(&file.path)
        {
            self.rebuild();
            cx.notify();
        }
        if let Some(ix) = self.rows.iter().position(|r| *r == Row::File(at)) {
            self.list.scroll_to_reveal_item(ix);
        }
    }

    /// What this tile sent for the file at `at` that the worker has not acted on: "Keeping",
    /// "Putting back".
    fn picking(&self, at: usize, hunk: Option<usize>, cx: &App) -> Option<&'static str> {
        let path = &self.model.file(at)?.path;
        let hunk = hunk.and_then(|h| u32::try_from(h).ok());
        self.hub.read(cx).threads().unshown(self.own()?).find_map(|s| {
            let (pick, words) = match &s.intent {
                Intent::Keep(p) => (p, "Keeping"),
                Intent::Revert(p) => (p, "Putting back"),
                _ => return None,
            };
            let covers = pick.path == *path
                && (pick.hunks.is_empty() || hunk.is_some_and(|h| pick.hunks.contains(&h)));
            (covers && !s.failed() && s.outcome.is_none()).then_some(words)
        })
    }

    /// The worker's refusal of the last keep or put back of the file at `at`, or of its hunk:
    /// the refusal and why, in words.
    fn refused(&self, at: usize, hunk: Option<usize>, cx: &App) -> Option<(IntentId, String)> {
        let path = &self.model.file(at)?.path;
        let hunk = hunk.and_then(|h| u32::try_from(h).ok());
        self.hub.read(cx).refusals(self.own()?).rev().find_map(|refusal| {
            let Some(Intent::Keep(pick) | Intent::Revert(pick)) = &refusal.intent else {
                return None;
            };
            let covers = pick.path == *path
                && match hunk {
                    Some(h) => pick.hunks.is_empty() || pick.hunks.contains(&h),
                    None => pick.hunks.is_empty(),
                };
            covers.then(|| (refusal.id, refusal.words.clone()))
        })
    }

    // ----- drawing -----------------------------------------------------------------------

    fn ink(&self, at: usize) -> Ink<'_> {
        let digits = self.blocks.get(&at).map_or(1, |b| lines::digits(b));
        Ink { theme: &self.theme, digits }
    }

    /// The code's size: the terminal's, so code reads as it does where it was written; the
    /// numbers beside it stay at the chrome's small size.
    const fn code_size(&self) -> f32 {
        self.theme.typography.mono_size
    }

    fn icon(&self, name: Symbol, tone: slopty_theme::Rgb) -> AnyElement {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(px(self.theme.typography.icon()))
            .into_any_element()
    }

    /// A small text button.
    fn action(&self, id: String, label: &'static str, primary: bool) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let selector = id.clone();
        let el = div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xxs))
            .border(kit::HAIR)
            .rounded(px(theme.radii.sm))
            .text_size(px(theme.typography.small()))
            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
            .cursor_pointer()
            .child(label);
        let el = if primary {
            kit::solid_pressable(el.border_color(hsla(s.solid)), theme)
        } else {
            el.border_color(gpui::transparent_black())
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
        };
        crate::a11y::tab_stop(el, s.focus)
    }

    /// The switch of spans, then the bar's actions at its end: one row that fits the tile
    /// (`kit::priority_row`), each item at its shaped width. The span on show and the refresh
    /// never leave; the pull request, the agent's review, the other spans and Commit leave in
    /// that order and wait behind "More" (`docs/decisions/ui.md`, "How surfaces adapt to their
    /// room").
    fn scope_bar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let mut row = kit::priority_row("review-scope-row")
            .h_full()
            .gap(px(theme.spacing.xxs))
            .dropped(&self.scopes_dropped);
        for scope in self.scopes().iter().copied() {
            let on = scope == self.scope;
            let id = format!("review-scope-{scope:?}");
            let selector = id.clone();
            let tab = div()
                .id(ElementId::Name(id.into()))
                .debug_selector(move || selector)
                .role(Role::Tab)
                .aria_label(scope.label())
                .aria_selected(on)
                .flex_none()
                .whitespace_nowrap()
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .when(on, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .when(!on, |el| {
                    el.text_color(hsla(s.text_secondary))
                        .hover(move |el| el.text_color(hsla(s.text)))
                })
                .child(scope.label())
                .on_click(cx.listener(move |this, _ev, _w, cx| this.set_scope(scope, cx)));
            let priority = if on { kit::Priority::ESSENTIAL } else { SCOPE_PRIORITY };
            row = row.item(scope.label(), priority, tab);
        }
        let mut row = row.end();
        if let Some(fold) = self.fold_part(cx) {
            row = row.item(BAR_FOLD, kit::Priority::ESSENTIAL, fold);
        }
        if let Some(refresh) = self.refresh_part(cx) {
            row = row.item(BAR_REFRESH, kit::Priority::ESSENTIAL, refresh);
        }
        if let Some(review) = self.review_part(cx) {
            row = row.item(BAR_AGENT, kit::Priority(144), review);
        }
        if let Some(pull) = self.pull_part(cx) {
            row = row.item(BAR_PULL, kit::Priority(112), pull);
        }
        if let Some(commit) = self.commit_part(cx) {
            row = row.item(BAR_COMMIT, kit::Priority::HIGH, commit);
        }
        div()
            .id("review-scopes")
            .role(Role::TabList)
            .flex_none()
            .w_full()
            .h(px(theme.density.header))
            .px(px(theme.spacing.md))
            .text_size(px(theme.typography.small()))
            .child(row.menu(self.scopes_more(cx)))
            .into_any_element()
    }

    /// "More" at the scope bar's end, while something left it, and its menu while open: what
    /// left, each doing what it does in the bar.
    fn scopes_more(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let button = kit::icon_button(theme, "review-scopes-more", Symbol::Ellipsis, "More")
            .aria_expanded(self.scopes_open)
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.scopes_open = !this.scopes_open;
                cx.notify();
            }));
        let this = cx.entity().downgrade();
        let menu = self.scopes_open.then(|| {
            let mut menu = kit::Menu::new();
            let dropped = self.scopes_dropped.keys();
            for scope in self.scopes().iter().copied() {
                if !dropped.iter().any(|k| k == scope.label()) {
                    continue;
                }
                let to = this.clone();
                let key = format!("{scope:?}");
                menu.push(kit::MenuItem::new(key, scope.label(), move |_w, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.scopes_open = false;
                        this.set_scope(scope, cx);
                    });
                }));
            }
            if dropped.iter().any(|k| k == BAR_AGENT)
                && self.reviewing.is_none()
                && let Some((_, name)) = self.door_of(cx)
            {
                let to = this.clone();
                let words = format!("Review with {name}");
                menu.push(kit::MenuItem::new("agent", words, move |_w, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.scopes_open = false;
                        this.review_with_agent(cx);
                    });
                }));
            }
            if dropped.iter().any(|k| k == BAR_PULL || k == BAR_COMMIT) {
                let to = this.clone();
                menu.push(kit::MenuItem::new("commit", COMMIT, move |window, cx| {
                    let _gone = to.update(cx, |this, cx| {
                        this.scopes_open = false;
                        this.open_commit(window, cx);
                    });
                }));
            }
            let panel = kit::MenuPanel::new("review-scopes-more", "More", Rc::new(menu), theme, {
                move |window, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.scopes_open = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    });
                }
            });
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::TopRight).child(panel))
                .with_priority(crate::palette::Layer::Submenu.priority())
        });
        div().relative().flex_none().child(button).children(menu).into_any_element()
    }

    /// The spans the switch offers: a thread's turns, or a folder's working tree.
    const fn scopes(&self) -> &'static [Scope] {
        match self.reviewed {
            Reviewed::Thread(_) => &Scope::THREAD,
            Reviewed::Folder(_) => &Scope::FOLDER,
        }
    }

    /// The switch that folds every file to its head, or opens them all while all are folded,
    /// as `MonoCode`'s diff has; none while there are no files.
    fn fold_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.model.review().filter(|r| !r.files.is_empty())?;
        let (glyph, words) = if self.all_folded() {
            (Symbol::Unfold, EXPAND_ALL)
        } else {
            (Symbol::Fold, COLLAPSE_ALL)
        };
        Some(
            kit::icon_button(&self.theme, "review-fold-all", glyph, words)
                .on_click(cx.listener(|this, _ev, _w, cx| this.fold_all(cx)))
                .into_any_element(),
        )
    }

    /// A folder's changes are read when asked, so they are read again from here; a thread's
    /// follow its turns.
    fn refresh_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.own().is_none().then(|| {
            kit::icon_button(&self.theme, "review-refresh", Symbol::ArrowClockwise, "Refresh")
                .on_click(cx.listener(|this, _ev, _w, cx| this.refresh(cx)))
                .into_any_element()
        })
    }

    /// The agent's own review: the way to ask it, where the agent has a door and there is a
    /// change on show, or that it runs. Its words name the agent, so no mark stands beside
    /// them; a tile too narrow for the words beside the scopes shows the conversation's glyph
    /// alone, its words in a hint.
    fn review_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (_, name) = self.door_of(cx)?;
        let roomy = self.room().is_wide();
        let mark = (!roomy).then(|| self.icon(crate::icons::AGENT, s.text_secondary));
        let hint_theme = theme.clone();
        let hinted = move |el: gpui::Stateful<Div>, words: String| {
            if roomy {
                el.child(SharedString::from(words))
            } else {
                kit::hint_timing(el).tooltip(move |_window, cx| {
                    let theme = Rc::new(hint_theme.clone());
                    cx.new(|_| kit::Hint::new(words.clone(), "", theme)).into()
                })
            }
        };
        if self.reviewing.is_some() {
            let words = format!("{name} is reviewing\u{2026}");
            let pill = div()
                .id("review-by-agent-running")
                .debug_selector(|| "review-by-agent-running".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xxs))
                .text_color(hsla(s.text_secondary))
                .child(crate::icons::status_icon(
                    theme,
                    crate::icons::Status::Working,
                    px(theme.typography.icon()),
                    hsla(s.text_muted),
                ));
            return Some(hinted(pill, words).into_any_element());
        }
        self.model.review().filter(|r| !r.files.is_empty() && r.from.is_some())?;
        let label = format!("Review with {name}");
        let selector = "review-by-agent";
        let el = div()
            .id(selector)
            .debug_selector(move || selector.to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(label.clone()))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xxs))
            .rounded(px(theme.radii.sm))
            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(s.text_secondary))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .children(mark)
            .on_click(cx.listener(|this, _ev, _w, cx| this.review_with_agent(cx)));
        Some(crate::a11y::tab_stop(hinted(el, label), s.focus).into_any_element())
    }

    /// Above the diff: how the agent's own review came out, and its findings that have no line
    /// on show, each with the way to let it go. Drawn whole they would take more than
    /// [`FINDINGS_SHARE`] of the diff's room, they fold to one line that says what came, and
    /// open on demand: the change under review keeps the tile.
    fn findings_band(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.came.is_none() && self.model.notes().is_empty() {
            return None;
        }
        let theme = &self.theme;
        let folds = self.findings_fold();
        let open = !folds || self.findings_open;
        let head = if folds { Some(self.findings_summary(open, cx)) } else { self.came_row(cx) };
        let notes =
            self.model.notes().iter().enumerate().map(|(ix, note)| self.note_row(ix, note, cx));
        Some(
            div()
                .flex_none()
                .w_full()
                .px(px(theme.spacing.md))
                .pt(px(theme.spacing.sm))
                .child(
                    kit::card(theme)
                        .id("review-findings")
                        .debug_selector(|| "review-findings".to_owned())
                        .role(Role::List)
                        .aria_label("The agent's review")
                        .w_full()
                        .max_h(px(FINDINGS_HEIGHT))
                        .overflow_y_scroll()
                        .text_size(px(theme.typography.small()))
                        .line_height(gpui::relative(theme.typography.markdown_line_height))
                        .children(head)
                        .when(open, |el| el.children(notes)),
                )
                .into_any_element(),
        )
    }

    /// Whether the findings, drawn whole, would take more than [`FINDINGS_SHARE`] of the diff's
    /// room: the body under the switch and over the foot, less what the band itself takes.
    fn findings_fold(&self) -> bool {
        let theme = &self.theme;
        let room = self.height - theme.density.header - kit::Row::Two.height(theme);
        let band = self.findings_height().min(FINDINGS_HEIGHT) + theme.spacing.sm;
        band > FINDINGS_SHARE * (room - band)
    }

    /// About how tall the findings are drawn whole, in points at rest: each piece of their
    /// words in lines of the band's width at [`LETTER`] of the small size a letter.
    fn findings_height(&self) -> f32 {
        let theme = &self.theme;
        let (sp, size) = (theme.spacing, theme.typography.small());
        let line = size * theme.typography.markdown_line_height;
        // The band's pads, a row's, the icon and the ✕ either side of the words, and their gaps.
        let inset = (sp.md + sp.sm + theme.typography.icon() + sp.xs) * 2.0;
        let per_line = ((self.width - inset) / (size * LETTER)).max(1.0);
        let lines = |words: &str| {
            let letters = f32::from(u16::try_from(words.chars().count()).unwrap_or(u16::MAX));
            (letters / per_line).ceil().max(1.0)
        };
        let row = |pieces: f32, lines: f32| {
            (pieces - 1.0).max(0.0).mul_add(sp.xxs, lines.mul_add(line, sp.xs * 2.0))
        };
        let head = self.came.as_ref().map_or(0.0, |came| row(1.0, lines(&came.words)));
        let notes: f32 = self
            .model
            .notes()
            .iter()
            .map(|note| {
                let f = &note.finding;
                let place = f.place.as_ref().map(findings::Place::words);
                let pieces = [place.as_deref(), Some(f.title.as_str()), Some(f.body.as_str())];
                let said: Vec<&str> =
                    pieces.into_iter().flatten().filter(|w| !w.is_empty()).collect();
                let count = f32::from(u8::try_from(said.len()).unwrap_or(u8::MAX));
                row(count, said.iter().map(|w| lines(w)).sum())
            })
            .sum();
        head + notes
    }

    /// The folded findings' one line: what came, in the error's tone when the review was turned
    /// down, the way to open or fold them, and the way to let the word go.
    fn findings_summary(&self, open: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let notes = self.model.notes().len();
        let (words, tone) = match &self.came {
            Some(came) if came.refused.is_some() => (came.words.clone(), s.error),
            Some(came) => (came.words.clone(), s.text_secondary),
            None if notes == 1 => ("1 finding not in the diff".to_owned(), s.text_secondary),
            None => (format!("{notes} findings not in the diff"), s.text_secondary),
        };
        let toggle = div()
            .id("review-findings-toggle")
            .debug_selector(|| "review-findings-toggle".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(words.clone()))
            .aria_expanded(open)
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .cursor_pointer()
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(tone))
                    .child(SharedString::from(words)),
            )
            .child(kit::Disclosure::new(
                "review-findings-chevron",
                open,
                theme,
                px(theme.typography.icon()),
                hsla(s.text_muted),
            ))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.findings_open = !this.findings_open;
                cx.notify();
            }));
        div()
            .debug_selector(|| "review-came".to_owned())
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.sm))
            .min_h(px(theme.density.row))
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(crate::a11y::tab_stop(toggle, s.focus))
            .children(self.came.as_ref().map(|_| {
                self.close(
                    "review-came-close",
                    "Dismiss",
                    cx.listener(|this, _ev, _w, cx| this.dismiss_came(cx)),
                )
            }))
            .into_any_element()
    }

    /// How the agent's review came out, in full, with the way to let the word go.
    fn came_row(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        self.came.as_ref().map(|came| {
            let tone = if came.refused.is_some() { s.error } else { s.text_secondary };
            div()
                .debug_selector(|| "review-came".to_owned())
                .w_full()
                .flex()
                .items_start()
                .gap(px(theme.spacing.xs))
                .px(px(theme.spacing.sm))
                .py(px(theme.spacing.xs))
                .child(
                    div()
                        .id("review-came-words")
                        .role(Role::Status)
                        .aria_label(SharedString::from(came.words.clone()))
                        .min_w_0()
                        .flex_1()
                        .whitespace_normal()
                        .text_color(hsla(tone))
                        .child(SharedString::from(came.words.clone())),
                )
                .child(self.close(
                    "review-came-close",
                    "Dismiss",
                    cx.listener(|this, _ev, _w, cx| this.dismiss_came(cx)),
                ))
                .into_any_element()
        })
    }

    /// A finding with no line on show: where it is, what it says, and the way to let it go.
    fn note_row(&self, ix: usize, note: &Note, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let finding = &note.finding;
        let id = format!("review-note-{ix}");
        let selector = id.clone();
        let label = format!(
            "{}. {}",
            finding.title,
            finding.place.as_ref().map(findings::Place::words).unwrap_or_default()
        );
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .role(Role::ListItem)
            .aria_label(SharedString::from(label))
            .w_full()
            .flex()
            .items_start()
            .gap(px(theme.spacing.xs))
            .px(px(theme.spacing.sm))
            .pt(px(theme.spacing.sm))
            .pb(px(theme.spacing.xs))
            .child(self.icon(Symbol::TextBubble, s.text_secondary))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xxs))
                    .children(finding.place.as_ref().map(|place| {
                        div()
                            .font_family(self.mono())
                            .text_color(hsla(s.text_muted))
                            .child(SharedString::from(place.words()))
                    }))
                    .child(
                        div()
                            .whitespace_normal()
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(s.text))
                            .child(SharedString::from(finding.title.clone())),
                    )
                    .when(!finding.body.is_empty(), |el| {
                        el.child(
                            div()
                                .whitespace_normal()
                                .text_color(hsla(s.text_secondary))
                                .child(SharedString::from(finding.body.clone())),
                        )
                    }),
            )
            .child(self.close(
                format!("review-unnote-{ix}"),
                "Let the finding go",
                cx.listener(move |this, _ev, _w, cx| this.unnote(ix, cx)),
            ))
            .into_any_element()
    }

    /// A quiet ✕ that lets something go.
    fn close(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        then: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<Div> {
        let id: SharedString = id.into();
        let selector = id.clone();
        let s = self.theme.surfaces;
        div()
            .id(ElementId::Name(id))
            .debug_selector(move || selector.to_string())
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .rounded(px(self.theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(self.icon(Symbol::Xmark, s.text_muted))
            .on_click(then)
    }

    /// The branch's pull request, once this client has heard of it: its number and where it
    /// stands, which open the commit sheet.
    fn pull_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let repo = self.repo(cx)?;
        let hub = self.hub.read(cx);
        let pull = hub.git().repo(&repo).and_then(|r| r.pull.status()).map(|pull| {
            let tone = crate::conversation::thread::commit::standing_tone(theme, pull.standing());
            let words = crate::conversation::thread::git::standing_words(pull);
            div()
                .id("review-pull")
                .debug_selector(|| "review-pull".to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Pull request {}, {words}", pull.number)))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs))
                .px(px(theme.spacing.xs))
                .py(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.hover)))
                .child(self.icon(Symbol::ArrowTrianglePull, tone))
                .child(kit::tabular(div()).child(SharedString::from(format!("#{}", pull.number))))
                .child(div().text_color(hsla(tone)).child(SharedString::from(words)))
                .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx)))
        });
        pull.map(IntoElement::into_any_element)
    }

    /// The way to the commit sheet on the reviewed repository.
    fn commit_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.repo(cx)?;
        Some(
            self.action("review-commit".to_owned(), COMMIT, false)
                .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx)))
                .into_any_element(),
        )
    }

    fn file_list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .id("review-files")
            .debug_selector(|| "review-files".to_owned())
            .role(Role::List)
            .aria_label("Files")
            .flex_none()
            .w(px(LIST_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .bg(hsla(s.ground))
            .py(px(theme.spacing.xs))
            .text_size(px(theme.typography.small()))
            .children(self.model.listed().iter().filter_map(|listed| {
                let file = self.model.file(listed.at)?;
                let at = listed.at;
                let name = file.path.rsplit('/').next().unwrap_or(&file.path).to_owned();
                let tone = if listed.quiet { s.text_muted } else { s.text };
                let id = format!("review-file-{at}");
                let selector = id.clone();
                let row = div()
                    .id(ElementId::Name(id.into()))
                    .debug_selector(move || selector)
                    .role(Role::ListItem)
                    .aria_label(SharedString::from(file.path.clone()))
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs))
                    .px(px(theme.spacing.md))
                    .min_h(px(kit::Row::One.height(theme)))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(hsla(tone))
                            .child(SharedString::from(name)),
                    )
                    .children(kit::changes(theme, file.patch.added, file.patch.removed))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.reveal(at, cx)));
                Some(Self::file_menu_press(row, at, cx))
            }))
            .into_any_element()
    }

    fn render_row(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(ix).copied() else { return div().into_any_element() };
        let inner = match row {
            Row::File(at) => return self.file_head(at, cx),
            Row::Bare(at) => self.bare(at),
            Row::Hunk(at, hunk) => self.hunk_head(at, hunk, cx),
            Row::Line(at, hunk, line) => self.line_row(at, hunk, line, cx),
            Row::Comment(c) => self.comment_row(c, cx),
            Row::Draft => self.draft_row(),
        };
        // One plane: a file is its head on the band and its lines on the content, with no frame
        // round them; its last line keeps a base unit under it.
        let last =
            !matches!(self.rows.get(ix.saturating_add(1)), Some(r) if !matches!(r, Row::File(_)));
        let theme = &self.theme;
        div()
            .w_full()
            .px(px(theme.spacing.md))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .when(last, |el| el.pb(px(theme.spacing.xs)))
                    .child(inner),
            )
            .into_any_element()
    }

    /// Keep and put back, or what is on its way.
    fn picks(&self, at: usize, hunk: Option<usize>, what: &str, cx: &Context<Self>) -> AnyElement {
        if self.own().is_none() {
            return div().into_any_element();
        }
        let s = self.theme.surfaces;
        if let Some(words) = self.picking(at, hunk, cx) {
            return div()
                .flex_none()
                .text_size(px(self.theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(words)
                .into_any_element();
        }
        let tag = hunk.map_or_else(|| format!("{at}"), |h| format!("{at}-{h}"));
        let refused = self.refused(at, hunk, cx).map(|(_, words)| {
            let selector = format!("review-refused-{what}-{tag}");
            div()
                .debug_selector(move || selector)
                .min_w_0()
                .max_w(gpui::relative(0.5))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(self.theme.typography.small()))
                .text_color(hsla(s.error))
                .child(SharedString::from(words))
        });
        // A hunk's stay in their place while hidden, so showing them moves nothing, and the
        // keyboard still reaches them: one with the focus shows itself.
        let shown = refused.is_some() || hunk.is_none_or(|h| self.hunk_shown(at, h));
        let ring = s.focus;
        let quiet = |el: gpui::Stateful<Div>| {
            if shown {
                el
            } else {
                el.opacity(0.0)
                    .focus_visible(move |st| st.outline_ring(crate::a11y::ring(ring)).opacity(1.0))
            }
        };
        div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(self.theme.spacing.xxs))
            .children(refused)
            .child(quiet(
                self.action(format!("review-revert-{what}-{tag}"), "Revert", false)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.pick(at, hunk, false, cx))),
            ))
            .child(quiet(
                self.action(format!("review-keep-{what}-{tag}"), "Keep", false)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.pick(at, hunk, true, cx))),
            ))
            .into_any_element()
    }

    /// A file's head: its name, then its folder muted, how it changed, keep and put back;
    /// under it the top edge of the file's card.
    fn file_head(&self, at: usize, cx: &Context<Self>) -> AnyElement {
        let Some(file) = self.model.file(at) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let (name, dir) = lines::name_first(&file.path);
        let status = match (&file.from, &file.to) {
            (None, Some(_)) => Some("Added"),
            (Some(_), None) => Some("Removed"),
            _ => None,
        };
        let radius = px(theme.radii.sm);
        let open = !self.folded.contains(&file.path);
        let side = px(theme.typography.icon());
        let fold = crate::a11y::tab_stop(
            div()
                .id(("review-fold", at))
                .debug_selector(move || format!("review-fold-{at}"))
                .role(Role::Button)
                .aria_label(SharedString::from(file.path.clone()))
                .aria_expanded(open)
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _ev, _w, cx| this.toggle_file(at, cx))),
            s.focus,
        )
        .child(kit::Disclosure::new(
            format!("review-fold-chevron-{at}"),
            open,
            theme,
            side,
            hsla(s.text_muted),
        ))
        .child(
            div()
                .flex_none()
                .max_w(gpui::relative(0.6))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(hsla(s.text))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .child(SharedString::from(name.to_owned())),
        )
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(dir.to_owned())),
        )
        .children(status.map(|st| div().flex_none().text_color(hsla(s.text_muted)).child(st)))
        .children(kit::changes(theme, file.patch.added, file.patch.removed));
        let head = div()
            .debug_selector(move || format!("review-head-row-{at}"))
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.sm))
            .rounded(radius)
            .map(|el| kit::inset(el, theme))
            .min_h(px(kit::Row::One.height(theme)))
            .text_size(px(theme.typography.small()))
            .child(fold)
            .child(self.picks(at, None, "file", cx));
        div()
            .debug_selector(move || format!("review-head-{at}"))
            .w_full()
            .px(px(theme.spacing.md))
            .pt(px(theme.spacing.lg))
            .pb(px(theme.spacing.xs))
            .child(Self::file_menu_press(head, at, cx))
            .into_any_element()
    }

    fn bare(&self, at: usize) -> AnyElement {
        let s = self.theme.surfaces;
        let words = match self.model.file(at) {
            Some(f) if f.binary => "Binary file",
            _ => "No lines to show",
        };
        div()
            .px(px(self.theme.spacing.md))
            .py(px(self.theme.spacing.sm))
            .text_size(px(self.theme.typography.small()))
            .text_color(hsla(s.text_muted))
            .child(words)
            .into_any_element()
    }

    /// A hunk's head. In a file of more than one hunk it holds the hunk's own keep and put
    /// back, shown while the pointer is on the hunk or the keyboard on them; on a touch screen
    /// always. A file of one hunk has only its head's: the same choice twice is noise.
    fn hunk_head(&self, at: usize, hunk: usize, cx: &Context<Self>) -> AnyElement {
        let Some(blocks) = self.blocks.get(&at) else { return div().into_any_element() };
        let Some(block) = blocks.get(hunk) else { return div().into_any_element() };
        let ink = self.ink(at);
        let head = ink.hunk_head(block).flex().items_center();
        if blocks.len() < 2 {
            return head.into_any_element();
        }
        let id = format!("review-hunk-{at}-{hunk}");
        let selector = id.clone();
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .w_full()
            .child(head.child(div().flex_1()).child(self.picks(at, Some(hunk), "hunk", cx)))
            .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                let was = this.hunk_hovered;
                if *hovered {
                    this.hunk_hovered = Some((at, hunk));
                } else if was == Some((at, hunk)) {
                    this.hunk_hovered = None;
                }
                if was != this.hunk_hovered {
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// Whether hunk `hunk` of the file at `at` shows its keep and put back: the pointer is on
    /// its head or its lines, or a finger has no hover to show them by.
    pub(super) fn hunk_shown(&self, at: usize, hunk: usize) -> bool {
        self.theme.density == slopty_theme::Density::TOUCH
            || self.hunk_hovered == Some((at, hunk))
            || self.hovered.is_some_and(|(a, h, _)| (a, h) == (at, hunk))
    }

    fn line_row(&self, at: usize, hunk: usize, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(line) =
            self.blocks.get(&at).and_then(|b| b.get(hunk)).and_then(|b| b.lines.get(ix))
        else {
            return div().into_any_element();
        };
        let ink = self.ink(at);
        let id = format!("review-line-{at}-{hunk}-{ix}");
        let new = line.new.filter(|_| line.kind != Kind::Removed);
        let tag = self.author_tag((at, hunk, ix), new, cx);
        self.pickable(id, (at, hunk, ix), ink.unified_numbered(line), tag, cx)
    }

    /// Row `ix` of hunk `hunk` of the file at `at`, drawn as `lines`, as a press and a drag
    /// pick it for a comment, washed while it is picked; who wrote it, `tag`, shows while the
    /// pointer is on it.
    fn pickable(
        &self,
        id: String,
        (at, hunk, ix): (usize, usize, usize),
        lines: Div,
        tag: Option<AnyElement>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let picked = self
            .marking
            .or_else(|| self.drafting.as_ref().map(|d| d.span))
            .is_some_and(|span| span.holds(at, hunk, ix));
        let wash = hsla_alpha(self.theme.surfaces.accent, slopty_theme::alpha::FAINT);
        let selector = id.clone();
        div()
            .id(ElementId::Name(id.into()))
            .debug_selector(move || selector)
            .relative()
            .w_full()
            .cursor_pointer()
            .font_family(self.mono())
            .text_size(px(self.code_size()))
            .line_height(px(self.code_size() * self.theme.typography.markdown_line_height))
            .child(lines)
            .when(picked, |el| el.child(div().absolute().inset_0().bg(wash)))
            .children(tag)
            .on_hover(cx.listener(move |this, hovered: &bool, _w, cx| {
                this.hover_line((at, hunk, ix), *hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    this.press_line(at, hunk, ix, ev.modifiers.shift, cx);
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _w, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    this.drag_line(at, hunk, ix, cx);
                }
            }))
            .into_any_element()
    }

    fn comment_row(&self, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(comment) = self.model.comments().get(ix) else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = format!("review-uncomment-{ix}");
        let selector = id.clone();
        let ranged = comment.end > comment.line;
        let (title, rest) = match &comment.by {
            Some(_) => {
                comment.body.split_once('\n').map_or((comment.body.as_str(), ""), |(t, r)| (t, r))
            }
            None => (comment.body.as_str(), ""),
        };
        self.note()
            .debug_selector(move || format!("review-comment-{ix}"))
            // A finding and a person's comment wear the same glyph: a finding's bold first
            // line, and the banner over the diff, say whose it is.
            .child(self.icon(Symbol::TextBubble, s.text_muted))
            .when(ranged, |el| {
                el.child(
                    kit::tabular(div())
                        .flex_none()
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(comment.place())),
                )
            })
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(theme.spacing.xxs))
                    .whitespace_normal()
                    .child(
                        div()
                            .text_color(hsla(s.text))
                            .when(comment.by.is_some(), |el| {
                                el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            })
                            .child(SharedString::from(title.to_owned())),
                    )
                    .when(!rest.trim().is_empty(), |el| {
                        el.child(
                            div()
                                .text_color(hsla(s.text_secondary))
                                .child(SharedString::from(rest.trim().to_owned())),
                        )
                    }),
            )
            .child(
                div()
                    .id(ElementId::Name(id.into()))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label("Remove comment")
                    .cursor_pointer()
                    .child(self.icon(Symbol::Xmark, s.text_muted))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.uncomment(ix, cx))),
            )
            .into_any_element()
    }

    fn draft_row(&self) -> AnyElement {
        let s = self.theme.surfaces;
        // A field to type in: raised off the diff, where the comments stand on the panel.
        kit::raised(self.note(), &self.theme)
            .rounded(px(self.theme.radii.sm))
            .debug_selector(|| "review-draft".to_owned())
            .child(self.icon(Symbol::TextBubble, s.text_secondary))
            .child(div().min_w_0().flex_1().child(Textarea::new(&self.draft).aria_label("Comment")))
            .into_any_element()
    }

    /// A comment's card, under its line.
    fn note(&self) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .w_full()
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.xs))
            .flex()
            .items_start()
            .gap(px(theme.spacing.xs))
            .bg(hsla(s.ground))
            .text_size(px(theme.typography.small()))
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// The foot: what to do with the comments ("Add to message", "Send N comments") and
    /// "Mark reviewed", one row that fits the tile at the buttons' shaped widths
    /// (`kit::priority_row`). The send never leaves; "Mark reviewed", then "Add to message",
    /// go behind "More" when there is no room for them. A folder's, while comments wait, is
    /// its one way to send them: to a new agent there.
    fn foot(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let n = self.model.waiting();
        let away = self.comments_away();
        let folder = self.own().is_none();
        let send_words = match n {
            _ if away => SENDING.to_owned(),
            _ if folder => SEND_TO_NEW_AGENT.to_owned(),
            1 => "Send 1 comment".to_owned(),
            n => format!("Send {n} comments"),
        };
        let mark = !folder && !self.model.listed().is_empty();
        let selector = if folder { "review-send-new" } else { "review-send" };
        let mut row = kit::priority_row("review-foot-row")
            .h_full()
            .gap(px(theme.spacing.sm))
            .dropped(&self.foot_dropped)
            .end();
        if n > 0 {
            let send = div()
                .id(selector)
                .debug_selector(move || selector.to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(send_words.clone()))
                .flex_none()
                .whitespace_nowrap()
                .px(px(theme.spacing.md))
                .py(px(theme.spacing.xs))
                .rounded(px(theme.radii.sm))
                .map(|el| kit::solid_pressable(el, theme))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .cursor_pointer()
                .child(SharedString::from(send_words))
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if folder { this.send_to_new_agent(cx) } else { this.send_comments(cx) }
                }));
            if !folder {
                let add = self
                    .action("review-add".to_owned(), ADD_TO_MESSAGE, false)
                    .on_click(cx.listener(|this, _ev, _w, cx| this.add_to_message(cx)));
                row = row.item(FOOT_ADD, kit::Priority(160), add);
            }
            row = row.item("send", kit::Priority::ESSENTIAL, send);
        }
        if mark {
            let marked = self
                .action("review-mark".to_owned(), MARK_REVIEWED, n == 0)
                .on_click(cx.listener(|this, _ev, _w, cx| this.mark_reviewed(cx)));
            // With nothing to send it is the foot's one action, and stays.
            let priority = if n == 0 { kit::Priority::ESSENTIAL } else { kit::Priority::MEDIUM };
            row = row.item(FOOT_MARK, priority, marked);
        }
        div()
            .flex_none()
            .w_full()
            .h(px(kit::Row::Two.height(theme)))
            .px(px(theme.spacing.md))
            .text_size(px(theme.typography.small()))
            .child(row.menu(self.foot_more(cx)))
            .into_any_element()
    }

    /// "More" at the foot's end, while something left it, and its menu while open: the foot's
    /// buttons that did not fit.
    fn foot_more(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let button = kit::icon_button(theme, "review-more", Symbol::Ellipsis, "More")
            .aria_expanded(self.more_open)
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.more_open = !this.more_open;
                cx.notify();
            }));
        let this = cx.entity().downgrade();
        let menu = self.more_open.then(|| {
            let mut menu = kit::Menu::new();
            if self.foot_dropped.contains(FOOT_ADD) {
                let to = this.clone();
                menu.push(kit::MenuItem::new("add", ADD_TO_MESSAGE, move |_w, cx| {
                    let _gone = to.update(cx, Self::add_to_message);
                }));
            }
            if self.foot_dropped.contains(FOOT_MARK) {
                let to = this.clone();
                menu.push(kit::MenuItem::new("mark", MARK_REVIEWED, move |_w, cx| {
                    let _gone = to.update(cx, Self::mark_reviewed);
                }));
            }
            let panel = kit::MenuPanel::new("review-more", "More", Rc::new(menu), theme, {
                move |window, cx| {
                    let _gone = this.update(cx, |this, cx| {
                        this.more_open = false;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    });
                }
            });
            gpui::deferred(gpui::anchored().anchor(gpui::Anchor::BottomRight).child(panel))
                .with_priority(crate::palette::Layer::Submenu.priority())
        });
        div().relative().flex_none().child(button).children(menu).into_any_element()
    }

    fn body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        // What the tile says instead of a diff, one block in its middle (`kit::notice`), with
        // the one next step where there is one.
        let empty = |mark: AnyElement, words: String, next: Option<AnyElement>| {
            div()
                .id("review-empty")
                .debug_selector(|| "review-empty".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .justify_center()
                .child(kit::notice(theme, mark, words, None).children(next))
                .into_any_element()
        };
        let diff_mark =
            || kit::notice_mark(theme, Symbol::PlusForwardslashMinus).into_any_element();
        let Some(review) = self.model.review() else {
            let reading = crate::icons::notice_status(
                theme,
                crate::icons::Status::Running,
                hsla(s.text_muted),
            );
            return empty(reading.into_any_element(), READING.to_owned(), None);
        };
        if let Some(why) = &review.absent {
            return empty(diff_mark(), why.clone(), None);
        }
        if review.files.is_empty() {
            // The widest span of the kind is the one next step from a narrower one that holds
            // nothing: the change may lie in an earlier turn, or already be committed.
            let widest = self.scopes().last().copied().filter(|widest| *widest != self.scope);
            let next = widest.map(|widest| {
                kit::notice_action(theme, "review-widen", widen_words(widest))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.set_scope(widest, cx)))
                    .into_any_element()
            });
            return empty(diff_mark(), self.scope.nothing().to_owned(), next);
        }
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .when(self.room().is_wide(), |el| el.child(self.file_list(cx)))
            .child(
                // No rule under the scope bar or over the foot: the diff fades where it slides
                // under either, and only while some of it lies past that edge.
                div().flex_1().min_w_0().h_full().child(
                    gpui::edge_fade(
                        list(
                            self.list.clone(),
                            cx.processor(|this, ix: usize, _window, cx| this.render_row(ix, cx)),
                        )
                        .size_full(),
                        gpui::EdgeFade::y(px(theme.spacing.lg)),
                    )
                    .hidden_by_list(&self.list),
                ),
            )
            .into_any_element()
    }
}

/// The agent's answer to review `asked`, once it rests: every answer it wrote in the turns
/// after the one the review was asked over, the first of them ended, and the request no longer
/// waiting. `None` while it works, waits on its own background work, or has not begun.
fn answered(state: &ThreadState, asked: &Reviewing) -> Option<String> {
    if state.pending.iter().any(|p| p.intent == asked.intent)
        || matches!(state.status.phase, Phase::Working | Phase::Waiting)
    {
        return None;
    }
    let after: Vec<TurnId> =
        state.turns.iter().map(|t| t.id).filter(|t| asked.after.is_none_or(|a| *t > a)).collect();
    let ended = state
        .turns
        .iter()
        .filter(|t| after.contains(&t.id))
        .any(|t| !matches!(t.state, TurnState::Active));
    if !ended {
        return None;
    }
    let said: Vec<&str> = state
        .items
        .iter()
        .filter(|i| after.contains(&i.turn))
        .filter_map(|i| match &i.body {
            ItemBody::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    Some(said.join("\n\n"))
}

/// Whether `line` is the one numbered `number` on `side`.
fn on(line: &Line, side: Side, number: u32) -> bool {
    match side {
        Side::Old => line.kind == Kind::Removed && line.old == Some(number),
        Side::New => line.kind != Kind::Removed && line.new == Some(number),
    }
}

impl Render for ReviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let s = theme.surfaces;
        let scopes = self.scope_bar(cx);
        let band = self.findings_band(cx);
        let body = self.body(cx);
        let door = self.door_of(cx).is_some();
        // A folder's foot is there only for its comments.
        let foot = (self.own().is_some() || self.model.waiting() > 0).then(|| self.foot(cx));
        div()
            .id("review")
            .debug_selector(|| "review".to_owned())
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label("Review")
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| this.release_line(window, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| this.release_line(window, cx)),
            )
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(hsla(theme.content()))
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(s.text))
            .on_action(cx.listener(|this, _: &OpenCommit, window, cx| this.open_commit(window, cx)))
            .on_action(cx.listener(|this, _: &RefreshPullRequest, _w, cx| this.refresh_pull(cx)))
            .when(door, |el| {
                el.on_action(
                    cx.listener(|this, _: &ReviewWithAgent, _w, cx| this.review_with_agent(cx)),
                )
            })
            .when(self.editor_opens(cx), |el| {
                el.on_action(cx.listener(
                    |this, _: &crate::file::open_with::OpenInEditor, _w, cx| {
                        this.open_in_editor(cx);
                    },
                ))
            })
            .relative()
            .child(scopes)
            .children(band)
            .child(body)
            .children(foot)
            .children(self.commit.as_ref().map(|(sheet, _)| sheet.clone()))
            .children(self.file_menu_panel(cx))
    }
}
