//! The review tile, drawn: the scope switch, the file list, the diff and its foot.
//!
//! The diff is one virtualized list of rows (a file's head, a hunk's head, a line or a pair of
//! lines, a comment, the field a comment is written in), so a review of thousands of lines lays
//! out only what is in view.
//!
//! A press on a line comments on it; a drag over a hunk's lines, or a shift-press past the
//! line commented on, comments on the run. A comment carries the code it is on, quoted, so the
//! agent reads what was meant. The comments go to the agent at once, or into the thread's
//! draft to send with more words.
//!
//! A folder's changes are reviewed the same way with no thread ([`Reviewed::Folder`]): its
//! working tree against `HEAD` or against the branch's base, read from its repository
//! (`GitOp::Changes`). With no agent to tell, it keeps no comments and takes no keep or put
//! back; the commit sheet and who wrote each line are there as for a thread.
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
use gpui_kit::component::input::{Input, InputEvent, InputState};
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

/// How wide the tile has to be, at rest, for its diff to show both sides.
pub const SPLIT_FROM: f32 = 960.0;

/// How wide the tile has to be for the file list to sit beside the diff.
const LIST_FROM: f32 = 720.0;

/// The file list's width, in points at zoom 1.
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

/// The foot's send while the comments are on their way.
pub const SENDING: &str = "Sending…";

/// Said above the diff when the worker turns the comments' send down; they stay.
pub const NOT_SENT: &str = "Comments not sent";

/// The foot's way to put the comments in the thread's draft.
const ADD_TO_MESSAGE: &str = "Add to message";

/// The foot's way to keep every file as it is.
const MARK_REVIEWED: &str = "Mark reviewed";

/// How wide a letter of the foot's words is, as a share of their size.
const FOOT_LETTER: f32 = 0.55;

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
    /// A line of a hunk, in a column.
    Line(usize, usize, usize),
    /// A pair of lines of a hunk, side by side.
    Pair(usize, usize, usize),
    /// A comment waiting, by its place among them.
    Comment(usize),
    /// The field a comment is written in.
    Draft,
}

/// Rows of one hunk picked for a comment, by their place among the hunk's rows (its lines in
/// a column, its pairs side by side): where the press went down and where it is now.
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
    zoom: f32,
    width: f32,
    /// The tile's body at rest, under its header, in points.
    height: f32,
    scope: Scope,
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
    draft: Entity<InputState>,
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
        Self::of(hub, Reviewed::Thread(thread), theme, window, cx)
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
        Self::of(hub, Reviewed::Folder(path), theme, window, cx)
    }

    fn of(
        hub: Entity<ThreadHub>,
        reviewed: Reviewed,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let folder = matches!(reviewed, Reviewed::Folder(_));
        let draft = cx.new(|cx| InputState::new(window, cx).placeholder("Comment on this line"));
        let writing = cx.subscribe_in(&draft, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
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
            zoom: 1.0,
            width: 0.0,
            height: 0.0,
            scope: if folder { Scope::Uncommitted } else { Scope::default() },
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

    /// Draw at the chrome's zoom `zoom`, in a tile `width` points wide at rest whose body is
    /// `height` points tall.
    pub fn set_layout(&mut self, zoom: f32, width: f32, height: f32, cx: &mut Context<Self>) {
        let split = self.split();
        self.zoom = zoom;
        self.width = width;
        self.height = height;
        if split != self.split() {
            self.rebuild();
        }
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

    fn split(&self) -> bool {
        self.width >= SPLIT_FROM
    }

    fn z(&self, v: f32) -> gpui::Pixels {
        px(v * self.zoom)
    }

    // ----- what comes ------------------------------------------------------------------

    /// Ask for the scope on show, unless it was asked over the same turns already. While the
    /// agent reviews, or its findings are here, the change it was asked about stays on show.
    fn ask(&mut self, cx: &mut Context<Self>) {
        let thread = match &self.reviewed {
            Reviewed::Thread(thread) => *thread,
            Reviewed::Folder(path) => {
                let (path, scope) = (path.clone(), self.scope.wire_alone());
                let Some(ReviewScope::WorkingTree(against)) = scope else { return };
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
        let Some(scope) = self.pinned.filter(|_| held).or_else(|| self.scope.wire(state)) else {
            return;
        };
        if self.asked.as_ref() == Some(&scope) {
            return;
        }
        self.asked = Some(scope);
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
        let (hub, theme) = (self.hub.clone(), self.theme.clone());
        let sheet = cx.new(|cx| CommitSheet::new(hub, repo, theme, window, cx));
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
        let review = match (&self.reviewed, self.scope.wire_alone()) {
            (Reviewed::Thread(thread), _) => hub.review(*thread).cloned(),
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
        let split = self.split();
        for listed in self.model.listed() {
            let at = listed.at;
            rows.push(Row::File(at));
            let Some(blocks) = self.blocks.get(&at).filter(|b| !b.is_empty()) else {
                rows.push(Row::Bare(at));
                continue;
            };
            let path = self.model.file(at).map(|f| f.path.clone()).unwrap_or_default();
            for (hunk, block) in blocks.iter().enumerate() {
                rows.push(Row::Hunk(at, hunk));
                if split {
                    for (ix, pair) in diff::pairs(block).iter().enumerate() {
                        let row = Row::Pair(at, hunk, ix);
                        rows.push(row);
                        let lines: Vec<&Line> =
                            <[_; 2]>::from(*pair).into_iter().flatten().collect();
                        self.under(&mut rows, &path, &lines, row);
                    }
                } else {
                    for (ix, line) in block.lines.iter().enumerate() {
                        let row = Row::Line(at, hunk, ix);
                        rows.push(row);
                        self.under(&mut rows, &path, &[line], row);
                    }
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
        self.pinned = self.asked;
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
        let send = Intent::Send { text, delivery: Delivery::Steer, attachments: Vec::new() };
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

    /// The row of the diff that row `ix` of hunk `hunk` of the file at `at` is, in the layout
    /// on show.
    fn row_of(&self, at: usize, hunk: usize, ix: usize) -> Row {
        if self.split() { Row::Pair(at, hunk, ix) } else { Row::Line(at, hunk, ix) }
    }

    /// The lines of `span`, in the diff's order: a pair's removed line before its added one,
    /// a context line once.
    fn span_lines(&self, span: Span) -> Vec<Line> {
        let Some(block) = self.blocks.get(&span.at).and_then(|b| b.get(span.hunk)) else {
            return Vec::new();
        };
        let (lo, hi) = span.range();
        if !self.split() {
            return block.lines.get(lo..=hi).map(<[Line]>::to_vec).unwrap_or_default();
        }
        let (mut out, mut removed, mut added) = (Vec::new(), Vec::new(), Vec::new());
        for pair in diff::pairs(block).get(lo..=hi).unwrap_or_default() {
            match *pair {
                (Some(old), Some(new)) if std::ptr::eq(old, new) => {
                    out.append(&mut removed);
                    out.append(&mut added);
                    out.push(old.clone());
                }
                (old, new) => {
                    removed.extend(old.cloned());
                    added.extend(new.cloned());
                }
            }
        }
        out.append(&mut removed);
        out.append(&mut added);
        out
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
        // A comment is for an agent to read; a folder's changes have none.
        if self.own().is_none() {
            return;
        }
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
            after: self.row_of(span.at, span.hunk, span.range().1),
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

    fn reveal(&self, at: usize) {
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
        Ink { theme: &self.theme, zoom: self.zoom, digits }
    }

    /// The code's size: the terminal's, so code reads as it does where it was written; the
    /// numbers beside it stay at the chrome's small size.
    const fn code_size(&self) -> f32 {
        self.theme.typography.mono_size
    }

    fn icon(&self, name: Symbol, tone: slopty_theme::Rgb) -> AnyElement {
        crate::icons::icon(&self.theme, name, IconSize::Inline, hsla(tone))
            .size(self.z(self.theme.typography.icon()))
            .into_any_element()
    }

    /// A small text button at the chrome's zoom.
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
            .px(self.z(theme.spacing.sm))
            .py(self.z(theme.spacing.xxs))
            .border(kit::hair(theme))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
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

    fn scope_bar(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .id("review-scopes")
            .role(Role::TabList)
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xxs))
            .px(self.z(theme.spacing.md))
            .min_h(self.z(theme.density.header))
            .border_b(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .text_size(self.z(theme.typography.small()))
            .children(self.scopes().iter().copied().map(|scope| {
                let on = scope == self.scope;
                let id = format!("review-scope-{scope:?}");
                let selector = id.clone();
                div()
                    .id(ElementId::Name(id.into()))
                    .debug_selector(move || selector)
                    .role(Role::Tab)
                    .aria_label(scope.label())
                    .aria_selected(on)
                    .px(self.z(theme.spacing.sm))
                    .py(self.z(theme.spacing.xxs))
                    .rounded(self.z(theme.radii.sm))
                    .cursor_pointer()
                    .when(on, |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
                    .when(!on, |el| {
                        el.text_color(hsla(s.text_secondary))
                            .hover(move |el| el.text_color(hsla(s.text)))
                    })
                    .child(scope.label())
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.set_scope(scope, cx)))
            }))
            .child(div().flex_1())
            .children(self.refresh_part(cx))
            .children(self.review_part(cx))
            .children(self.git_part(cx))
            .into_any_element()
    }

    /// The spans the switch offers: a thread's turns, or a folder's working tree.
    const fn scopes(&self) -> &'static [Scope] {
        match self.reviewed {
            Reviewed::Thread(_) => &Scope::THREAD,
            Reviewed::Folder(_) => &Scope::FOLDER,
        }
    }

    /// A folder's changes are read when asked, so they are read again from here; a thread's
    /// follow its turns.
    fn refresh_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.own().is_none().then(|| {
            kit::icon_button_at(
                &self.theme,
                "review-refresh",
                Symbol::ArrowClockwise,
                "Refresh",
                self.zoom,
            )
            .on_click(cx.listener(|this, _ev, _w, cx| this.refresh(cx)))
            .into_any_element()
        })
    }

    /// The agent's own review: the way to ask it, where the agent has a door and there is a
    /// change on show, or that it runs. A tile too narrow for the words beside the scopes shows
    /// the agent's mark alone, its words in a hint.
    fn review_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let (_, name) = self.door_of(cx)?;
        let mark = self.agent_mark(cx);
        let roomy = self.width >= LIST_FROM;
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
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.sm))
                .py(self.z(theme.spacing.xxs))
                .text_color(hsla(s.text_secondary))
                .child(crate::icons::status_icon(
                    theme,
                    crate::icons::Status::Working,
                    self.z(theme.typography.icon()),
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
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .py(self.z(theme.spacing.xxs))
            .rounded(self.z(theme.radii.sm))
            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
            .text_color(hsla(s.text_secondary))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .child(mark)
            .on_click(cx.listener(|this, _ev, _w, cx| this.review_with_agent(cx)));
        Some(crate::a11y::tab_stop(hinted(el, label), s.focus).into_any_element())
    }

    /// The thread's agent's mark at the size of an icon, beside what it wrote or does: its
    /// own, or the neutral glyph for an agent with none or not yet known.
    fn agent_mark(&self, cx: &App) -> AnyElement {
        let theme = &self.theme;
        let mark = self.door_of(cx).map_or_else(
            || crate::icons::AGENT.into(),
            |(agent, _)| crate::icons::Mark::agent(&agent.0),
        );
        crate::icons::symbol(
            theme,
            mark,
            self.z(theme.typography.icon()),
            hsla(theme.surfaces.text_secondary),
        )
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
                .px(self.z(theme.spacing.md))
                .pt(self.z(theme.spacing.sm))
                .child(
                    kit::card(theme)
                        .id("review-findings")
                        .debug_selector(|| "review-findings".to_owned())
                        .role(Role::List)
                        .aria_label("The agent's review")
                        .w_full()
                        .max_h(self.z(FINDINGS_HEIGHT))
                        .overflow_y_scroll()
                        .text_size(self.z(theme.typography.small()))
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
    /// words in lines of the band's width at [`FOOT_LETTER`] of the small size a letter.
    fn findings_height(&self) -> f32 {
        let theme = &self.theme;
        let (sp, size) = (theme.spacing, theme.typography.small());
        let line = size * theme.typography.markdown_line_height;
        // The band's pads, a row's, the icon and the ✕ either side of the words, and their gaps.
        let inset = (sp.md + sp.sm + theme.typography.icon() + sp.xs) * 2.0;
        let per_line = ((self.width - inset) / (size * FOOT_LETTER)).max(1.0);
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
            .gap(self.z(theme.spacing.xs))
            .cursor_pointer()
            .child(self.agent_mark(cx))
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
                self.z(theme.typography.icon()),
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
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .min_h(self.z(theme.density.row))
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
        let agent = self.door_of(cx).map(|(agent, _)| agent);
        self.came.as_ref().map(|came| {
            let tone = if came.refused.is_some() { s.error } else { s.text_secondary };
            div()
                .debug_selector(|| "review-came".to_owned())
                .w_full()
                .flex()
                .items_start()
                .gap(self.z(theme.spacing.xs))
                .px(self.z(theme.spacing.sm))
                .py(self.z(theme.spacing.xs))
                .children(agent.as_ref().map(|_| self.agent_mark(cx)))
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
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.sm))
            .py(self.z(theme.spacing.xs))
            .border_t(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .child(self.icon(Symbol::TextBubble, s.text_muted))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(self.z(theme.spacing.xxs))
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
            .rounded(self.z(self.theme.radii.sm))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)))
            .child(self.icon(Symbol::Xmark, s.text_muted))
            .on_click(then)
    }

    /// At the scope bar's end: the branch's pull request where it stands, and the way to the
    /// commit sheet.
    fn git_part(&self, cx: &Context<Self>) -> Option<AnyElement> {
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
                .gap(self.z(theme.spacing.xxs))
                .px(self.z(theme.spacing.xs))
                .py(self.z(theme.spacing.xxs))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .text_color(hsla(s.text_secondary))
                .hover(move |el| el.bg(hsla(s.hover)))
                .child(self.icon(Symbol::ArrowTrianglePull, tone))
                .child(kit::tabular(div()).child(SharedString::from(format!("#{}", pull.number))))
                .child(div().text_color(hsla(tone)).child(SharedString::from(words)))
                .on_click(cx.listener(|this, _ev, window, cx| this.open_commit(window, cx)))
        });
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .children(pull)
                .child(
                    self.action("review-commit".to_owned(), "Commit\u{2026}", false).on_click(
                        cx.listener(|this, _ev, window, cx| this.open_commit(window, cx)),
                    ),
                )
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
            .w(self.z(LIST_WIDTH))
            .h_full()
            .overflow_y_scroll()
            .border_r(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .py(self.z(theme.spacing.xs))
            .text_size(self.z(theme.typography.small()))
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
                    .gap(self.z(theme.spacing.xs))
                    .px(self.z(theme.spacing.md))
                    .min_h(self.z(kit::Row::One.height(theme)))
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
                    .on_click(cx.listener(move |this, _ev, _w, _cx| this.reveal(at)));
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
            Row::Pair(at, hunk, pair) => self.pair_row(at, hunk, pair, cx),
            Row::Comment(c) => self.comment_row(c, cx),
            Row::Draft => self.draft_row(),
        };
        // Each file is a card: its head row draws the top edge, its rows the sides, its last
        // row the foot.
        let last =
            !matches!(self.rows.get(ix.saturating_add(1)), Some(r) if !matches!(r, Row::File(_)));
        let theme = &self.theme;
        let radius = self.z(theme.radii.sm);
        div()
            .w_full()
            .px(self.z(theme.spacing.md))
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .border_l(kit::hair(theme))
                    .border_r(kit::hair(theme))
                    .border_color(hsla(theme.surfaces.border_subtle))
                    .when(last, |el| {
                        el.border_b(kit::hair(theme))
                            .rounded_bl(radius)
                            .rounded_br(radius)
                            .pb(self.z(theme.spacing.xs))
                    })
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
                .text_size(self.z(self.theme.typography.small()))
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
                .text_size(self.z(self.theme.typography.small()))
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
            .gap(self.z(self.theme.spacing.xxs))
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
        let radius = self.z(theme.radii.sm);
        let head = div()
            .debug_selector(move || format!("review-head-row-{at}"))
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .px(self.z(theme.spacing.xs))
            .min_h(self.z(kit::Row::One.height(theme)))
            .text_size(self.z(theme.typography.small()))
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
            .children(kit::changes(theme, file.patch.added, file.patch.removed))
            .child(div().flex_1())
            .child(self.picks(at, None, "file", cx));
        div()
            .debug_selector(move || format!("review-head-{at}"))
            .w_full()
            .px(self.z(theme.spacing.md))
            .pt(self.z(theme.spacing.lg))
            .child(Self::file_menu_press(head, at, cx))
            .child(
                div()
                    .w_full()
                    .h(self.z(theme.spacing.xs))
                    .rounded_tl(radius)
                    .rounded_tr(radius)
                    .border_t(kit::hair(theme))
                    .border_l(kit::hair(theme))
                    .border_r(kit::hair(theme))
                    .border_color(hsla(s.border_subtle)),
            )
            .into_any_element()
    }

    fn bare(&self, at: usize) -> AnyElement {
        let s = self.theme.surfaces;
        let words = match self.model.file(at) {
            Some(f) if f.binary => "Binary file",
            _ => "No lines to show",
        };
        div()
            .px(self.z(self.theme.spacing.md))
            .py(self.z(self.theme.spacing.sm))
            .text_size(self.z(self.theme.typography.small()))
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

    fn pair_row(&self, at: usize, hunk: usize, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(block) = self.blocks.get(&at).and_then(|b| b.get(hunk)) else {
            return div().into_any_element();
        };
        let pairs = diff::pairs(block);
        let Some(pair) = pairs.get(ix).copied() else { return div().into_any_element() };
        let ink = self.ink(at);
        let id = format!("review-pair-{at}-{hunk}-{ix}");
        let tag = self.author_tag((at, hunk, ix), pair.1.and_then(|l| l.new), cx);
        self.pickable(id, (at, hunk, ix), ink.split(pair), tag, cx)
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
            .text_size(self.z(self.code_size()))
            .line_height(self.z(self.code_size() * self.theme.typography.markdown_line_height))
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
        let agent = comment.by.as_ref().and_then(|_| self.door_of(cx)).map(|(agent, _)| agent);
        let (title, rest) = match &comment.by {
            Some(_) => {
                comment.body.split_once('\n').map_or((comment.body.as_str(), ""), |(t, r)| (t, r))
            }
            None => (comment.body.as_str(), ""),
        };
        self.note()
            .debug_selector(move || format!("review-comment-{ix}"))
            .child(match &agent {
                Some(_) => self.agent_mark(cx),
                None => self.icon(Symbol::TextBubble, s.text_muted),
            })
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
                    .gap(self.z(theme.spacing.xxs))
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
        self.note()
            .debug_selector(|| "review-draft".to_owned())
            .child(self.icon(Symbol::TextBubble, s.text_muted))
            .child(div().min_w_0().flex_1().child(Input::new(&self.draft).aria_label("Comment")))
            .into_any_element()
    }

    /// A comment's card, under its line.
    fn note(&self) -> Div {
        let theme = &self.theme;
        let s = theme.surfaces;
        div()
            .w_full()
            .px(self.z(theme.spacing.md))
            .py(self.z(theme.spacing.xs))
            .flex()
            .items_start()
            .gap(self.z(theme.spacing.xs))
            .bg(hsla(s.panel))
            .border_t(kit::hair(theme))
            .border_b(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .text_size(self.z(theme.typography.small()))
    }

    fn mono(&self) -> SharedString {
        self.theme.typography.mono_families.first().cloned().unwrap_or_default().into()
    }

    /// The foot: what to do with the comments ("Add to message", "Send N comments") and
    /// "Mark reviewed". What does not fit the tile's width goes behind "More" beside the send,
    /// which always shows.
    fn foot(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let n = self.model.waiting();
        let away = self.comments_away();
        let send_words = match n {
            _ if away => SENDING.to_owned(),
            1 => "Send 1 comment".to_owned(),
            n => format!("Send {n} comments"),
        };
        let mark = !self.model.listed().is_empty();
        let fits = self.foot_fits(n > 0, &send_words, mark);
        let selector = "review-send";
        let add = || {
            self.action("review-add".to_owned(), ADD_TO_MESSAGE, false)
                .on_click(cx.listener(|this, _ev, _w, cx| this.add_to_message(cx)))
        };
        let marked = || {
            self.action("review-mark".to_owned(), MARK_REVIEWED, n == 0)
                .on_click(cx.listener(|this, _ev, _w, cx| this.mark_reviewed(cx)))
        };
        let more = (!fits).then(|| self.foot_more(n > 0, mark, cx));
        div()
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .px(self.z(theme.spacing.md))
            .min_h(self.z(kit::Row::Two.height(theme)))
            .border_t(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .text_size(self.z(theme.typography.small()))
            .child(div().flex_1())
            .when(n > 0 && fits, |el| el.child(add()))
            .when(n > 0, |el| {
                el.child(
                    div()
                        .id(selector)
                        .debug_selector(move || selector.to_owned())
                        .role(Role::Button)
                        .aria_label(SharedString::from(send_words.clone()))
                        .flex_none()
                        .whitespace_nowrap()
                        .px(self.z(theme.spacing.md))
                        .py(self.z(theme.spacing.xs))
                        .rounded(self.z(theme.radii.sm))
                        .map(|el| kit::solid_pressable(el, theme))
                        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        .cursor_pointer()
                        .child(SharedString::from(send_words))
                        .on_click(cx.listener(|this, _ev, _w, cx| this.send_comments(cx))),
                )
            })
            .when(mark && (fits || n == 0), |el| el.child(marked()))
            .children(more)
            .into_any_element()
    }

    /// Whether the foot's buttons all fit the tile at rest: each one's words at about
    /// [`FOOT_LETTER`] of the small size a letter, with its pads, the gaps between them and
    /// the foot's own pads. With no comments "Mark reviewed" stands alone and always fits.
    fn foot_fits(&self, comments: bool, send: &str, mark: bool) -> bool {
        if !comments {
            return true;
        }
        let theme = &self.theme;
        let (sp, letter) = (theme.spacing, theme.typography.small() * FOOT_LETTER);
        let words = |w: &str| f32::from(u16::try_from(w.chars().count()).unwrap_or(u16::MAX));
        let add = words(ADD_TO_MESSAGE).mul_add(letter, sp.sm * 2.0);
        let send = words(send).mul_add(letter, sp.md * 2.0);
        let mut needed = sp.md.mul_add(2.0, add + send + sp.sm);
        if mark {
            needed += words(MARK_REVIEWED).mul_add(letter, sp.sm * 2.0) + sp.sm;
        }
        needed <= self.width
    }

    /// "More" at the foot's end, and its menu while open: the foot's buttons that did not fit.
    fn foot_more(&self, comments: bool, mark: bool, cx: &Context<Self>) -> AnyElement {
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
            if comments {
                let to = this.clone();
                menu.push(kit::MenuItem::new("add", ADD_TO_MESSAGE, move |_w, cx| {
                    let _gone = to.update(cx, Self::add_to_message);
                }));
            }
            if mark {
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
        let empty = |words: String| {
            div()
                .id("review-empty")
                .debug_selector(|| "review-empty".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(words))
                .into_any_element()
        };
        let Some(review) = self.model.review() else {
            return empty("Reading the changes…".to_owned());
        };
        if let Some(why) = &review.absent {
            return empty(why.clone());
        }
        if review.files.is_empty() {
            return empty(self.scope.nothing().to_owned());
        }
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .when(self.width >= LIST_FROM, |el| el.child(self.file_list(cx)))
            .child(
                div().flex_1().min_w_0().h_full().child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix: usize, _window, cx| this.render_row(ix, cx)),
                    )
                    .size_full(),
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
        let foot = self.own().map(|_| self.foot(cx));
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
            .text_size(self.z(theme.typography.ui_size))
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
