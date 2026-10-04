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
use slopty_proto::thread::wire::{Intent, Review, ReviewScope};
use slopty_proto::thread::{Delivery, IntentId, ThreadId};
use slopty_theme::{Theme, Typography};

use super::model::{self, Comment, Model, Scope, Side};
use crate::colors::{hsla, hsla_alpha};
use crate::conversation::diff::{self, Block, Kind, Line};
use crate::conversation::lines::{self, Ink};
use crate::conversation::thread::commit::{CommitEvent, CommitSheet};
use crate::conversation::thread::{HubEvent, ThreadHub};
use crate::conversation::{OpenCommit, RefreshPullRequest};
use crate::icons::{IconName, IconSize};
use crate::kit;

/// How wide the tile has to be, at rest, for its diff to show both sides.
pub const SPLIT_FROM: f32 = 960.0;

/// How wide the tile has to be for the file list to sit beside the diff.
const LIST_FROM: f32 = 720.0;

/// The file list's width, in points at zoom 1.
const LIST_WIDTH: f32 = 240.0;

/// How far past the viewport the diff lays rows out.
const OVERDRAW: f32 = 2048.0;

/// What the tile tells its host.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReviewEvent {
    /// The comments went to the agent: the thread is where its answer shows.
    CommentsSent {
        /// The thread.
        thread: ThreadId,
    },
    /// The comments are for the thread's draft, to go with more words: the host puts `text`
    /// at its end and gives it the keyboard.
    AddToMessage {
        /// The thread.
        thread: ThreadId,
        /// The comments as one message.
        text: String,
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

/// The review tile.
pub struct ReviewView {
    hub: Entity<ThreadHub>,
    thread: ThreadId,
    theme: Theme,
    zoom: f32,
    width: f32,
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
    /// The commit sheet over the tile, while it is open.
    commit: Option<(Entity<CommitSheet>, Subscription)>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ReviewView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewView")
            .field("thread", &self.thread)
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
        let draft = cx.new(|cx| InputState::new(window, cx).placeholder("Comment on this line"));
        let writing = cx.subscribe_in(&draft, window, |this, _input, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.add_comment(window, cx);
            }
        });
        let hearing = cx.subscribe(&hub, |this, _hub, event, cx| match event {
            HubEvent::Review(t) if *t == this.thread => this.reviewed(cx),
            HubEvent::Thread(t) if *t == this.thread => this.thread_moved(cx),
            HubEvent::Git(repo) if this.repo(cx).as_ref() == Some(repo) => cx.notify(),
            _ => {}
        });
        let watching = cx.observe(&draft, |_, _, cx| cx.notify());
        cx.on_release(move |this, cx| {
            this.hub.update(cx, |hub, cx| hub.close(thread, cx));
        })
        .detach();
        let mut view = Self {
            hub,
            thread,
            theme,
            zoom: 1.0,
            width: 0.0,
            scope: Scope::default(),
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
            commit: None,
            focus: cx.focus_handle(),
            _subscriptions: vec![writing, hearing, watching],
        };
        view.hub.update(cx, |hub, cx| hub.open(thread, cx));
        view.reviewed(cx);
        view.ask(cx);
        view.ask_pull(cx);
        view
    }

    /// The thread it reviews.
    #[must_use]
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// The title of the thread it reviews, once known.
    #[must_use]
    pub fn title(&self, cx: &App) -> Option<String> {
        self.hub.read(cx).threads().title(self.thread).map(str::to_owned)
    }

    /// The span on show.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Draw at the chrome's zoom `zoom`, in a tile `width` points wide at rest.
    pub fn set_layout(&mut self, zoom: f32, width: f32, cx: &mut Context<Self>) {
        let split = self.split();
        self.zoom = zoom;
        self.width = width;
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
        self.blocks.clear();
        self.list.remeasure();
        cx.notify();
    }

    /// Show `scope`.
    pub fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.asked = None;
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

    /// Ask for the scope on show, unless it was asked over the same turns already.
    fn ask(&mut self, cx: &mut Context<Self>) {
        let hub = self.hub.read(cx);
        let Some(state) = hub.threads().mirror(self.thread).and_then(Mirror::state) else {
            return;
        };
        let Some(scope) = self.scope.wire(state) else { return };
        if self.asked.as_ref() == Some(&scope) {
            return;
        }
        self.asked = Some(scope);
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| hub.ask_review(thread, scope, cx));
    }

    /// The folder the thread works in, when the worker said: its repository is the one the
    /// commit sheet and the pull request are of.
    fn repo(&self, cx: &App) -> Option<String> {
        let hub = self.hub.read(cx);
        let state = hub.threads().mirror(self.thread).and_then(Mirror::state)?;
        Some(state.meta.cwd.clone()).filter(|cwd| !cwd.trim().is_empty())
    }

    /// Ask the branch's pull request once the folder is known: the tile shows where it stands.
    fn ask_pull(&mut self, cx: &mut Context<Self>) {
        if self.pull_asked {
            return;
        }
        let Some(repo) = self.repo(cx) else { return };
        self.pull_asked = true;
        let _asked = self
            .hub
            .update(cx, |hub, cx| hub.git_op(&repo, slopty_proto::git::GitOp::PullStatus, cx));
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
        let hub = self.hub.read(cx);
        let waiting: HashSet<IntentId> = hub
            .threads()
            .outbox()
            .of(self.thread)
            .filter(|s| s.outcome.is_none())
            .map(|s| s.id)
            .collect();
        let acted = self.picks.iter().any(|id| !waiting.contains(id));
        self.picks.retain(|id| waiting.contains(id));
        if acted {
            self.asked = None;
        }
        self.ask(cx);
        self.ask_pull(cx);
        cx.notify();
    }

    /// The worker's review came.
    fn reviewed(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.hub.read(cx).review(self.thread).cloned() else { return };
        self.show(review);
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

    fn intent(&self, intent: Intent, cx: &mut Context<Self>) -> IntentId {
        let thread = self.thread;
        self.hub.update(cx, |hub, cx| hub.intent(thread, intent, cx))
    }

    /// Keep or put back the file at `at`, or one of its hunks. A refusal of an earlier try
    /// goes: this one speaks for it now.
    fn pick(&mut self, at: usize, hunk: Option<usize>, keep: bool, cx: &mut Context<Self>) {
        if let Some((refused, _)) = self.refused(at, hunk, cx) {
            self.hub.update(cx, |hub, cx| hub.dismiss(refused, cx));
        }
        let hunks = hunk.and_then(|h| u32::try_from(h).ok()).into_iter().collect();
        if let Some(intent) = self.model.pick(at, hunks, keep) {
            let id = self.intent(intent, cx);
            self.picks.insert(id);
        }
    }

    /// Keep every file shown.
    fn mark_reviewed(&mut self, cx: &mut Context<Self>) {
        for intent in self.model.keep_all() {
            let id = self.intent(intent, cx);
            self.picks.insert(id);
        }
    }

    /// Send the comments as one message.
    fn send_comments(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.model.take_message() else { return };
        let _id = self
            .intent(Intent::Send { text, delivery: Delivery::Steer, attachments: Vec::new() }, cx);
        self.rebuild();
        cx.emit(ReviewEvent::CommentsSent { thread: self.thread });
        cx.notify();
    }

    /// Put the comments into the thread's draft rather than send them: the host gives the
    /// draft the keyboard.
    fn add_to_message(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.model.take_message() else { return };
        self.rebuild();
        cx.emit(ReviewEvent::AddToMessage { thread: self.thread, text });
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
        });
        self.draft.update(cx, |d, cx| d.set_value("", window, cx));
        self.rebuild();
        cx.notify();
    }

    fn uncomment(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.model.uncomment(ix);
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
        self.hub.read(cx).threads().unshown(self.thread).find_map(|s| {
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
        self.hub.read(cx).refusals(self.thread).rev().find_map(|refusal| {
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

    fn icon(&self, name: IconName, tone: slopty_theme::Rgb) -> AnyElement {
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
        crate::a11y::tab_stop(el, s.accent)
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
            .min_h(self.z(kit::Row::One.height(theme)))
            .border_b(kit::hair(theme))
            .border_color(hsla(s.border_subtle))
            .text_size(self.z(theme.typography.small()))
            .children(Scope::ALL.into_iter().map(|scope| {
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
            .children(self.git_part(cx))
            .into_any_element()
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
                .child(self.icon(IconName::GitPullRequest, tone))
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
                Some(
                    div()
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
                        .on_click(cx.listener(move |this, _ev, _w, _cx| this.reveal(at))),
                )
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
        div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(self.z(self.theme.spacing.xxs))
            .children(refused)
            .child(
                self.action(format!("review-revert-{what}-{tag}"), "Revert", false)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.pick(at, hunk, false, cx))),
            )
            .child(
                self.action(format!("review-keep-{what}-{tag}"), "Keep", false)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.pick(at, hunk, true, cx))),
            )
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
        div()
            .debug_selector(move || format!("review-head-{at}"))
            .w_full()
            .px(self.z(theme.spacing.md))
            .pt(self.z(theme.spacing.lg))
            .child(
                div()
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
                    .children(
                        status.map(|st| div().flex_none().text_color(hsla(s.text_muted)).child(st)),
                    )
                    .children(kit::changes(theme, file.patch.added, file.patch.removed))
                    .child(div().flex_1())
                    .child(self.picks(at, None, "file", cx)),
            )
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

    fn hunk_head(&self, at: usize, hunk: usize, cx: &Context<Self>) -> AnyElement {
        let Some(block) = self.blocks.get(&at).and_then(|b| b.get(hunk)) else {
            return div().into_any_element();
        };
        let ink = self.ink(at);
        ink.hunk_head(block)
            .flex()
            .items_center()
            .child(div().flex_1())
            .child(self.picks(at, Some(hunk), "hunk", cx))
            .into_any_element()
    }

    fn line_row(&self, at: usize, hunk: usize, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(line) =
            self.blocks.get(&at).and_then(|b| b.get(hunk)).and_then(|b| b.lines.get(ix))
        else {
            return div().into_any_element();
        };
        let ink = self.ink(at);
        self.pickable(
            format!("review-line-{at}-{hunk}-{ix}"),
            (at, hunk, ix),
            ink.unified(line),
            cx,
        )
    }

    fn pair_row(&self, at: usize, hunk: usize, ix: usize, cx: &Context<Self>) -> AnyElement {
        let Some(block) = self.blocks.get(&at).and_then(|b| b.get(hunk)) else {
            return div().into_any_element();
        };
        let pairs = diff::pairs(block);
        let Some(pair) = pairs.get(ix).copied() else { return div().into_any_element() };
        let ink = self.ink(at);
        self.pickable(format!("review-pair-{at}-{hunk}-{ix}"), (at, hunk, ix), ink.split(pair), cx)
    }

    /// Row `ix` of hunk `hunk` of the file at `at`, drawn as `lines`, as a press and a drag
    /// pick it for a comment, washed while it is picked.
    fn pickable(
        &self,
        id: String,
        (at, hunk, ix): (usize, usize, usize),
        lines: Div,
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
            .text_size(self.z(self.theme.typography.small()))
            .child(lines)
            .when(picked, |el| el.child(div().absolute().inset_0().bg(wash)))
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
        self.note()
            .debug_selector(move || format!("review-comment-{ix}"))
            .child(self.icon(IconName::MessageSquare, s.text_muted))
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
                    .whitespace_normal()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(comment.body.clone())),
            )
            .child(
                div()
                    .id(ElementId::Name(id.into()))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label("Remove comment")
                    .cursor_pointer()
                    .child(self.icon(IconName::X, s.text_muted))
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.uncomment(ix, cx))),
            )
            .into_any_element()
    }

    fn draft_row(&self) -> AnyElement {
        let s = self.theme.surfaces;
        self.note()
            .debug_selector(|| "review-draft".to_owned())
            .child(self.icon(IconName::MessageSquare, s.text_muted))
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

    fn foot(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let n = self.model.comments().len();
        let send_words = match n {
            1 => "Send 1 comment".to_owned(),
            n => format!("Send {n} comments"),
        };
        let selector = "review-send";
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
            .when(n > 0, |el| {
                el.child(
                    self.action("review-add".to_owned(), "Add to message", false)
                        .on_click(cx.listener(|this, _ev, _w, cx| this.add_to_message(cx))),
                )
                .child(
                    div()
                        .id(selector)
                        .debug_selector(move || selector.to_owned())
                        .role(Role::Button)
                        .aria_label(SharedString::from(send_words.clone()))
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
            .when(!self.model.listed().is_empty(), |el| {
                el.child(
                    self.action("review-mark".to_owned(), "Mark reviewed", n == 0)
                        .on_click(cx.listener(|this, _ev, _w, cx| this.mark_reviewed(cx))),
                )
            })
            .into_any_element()
    }

    fn body(&self, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let empty = |words: String| {
            div()
                .id("review-empty")
                .debug_selector(|| "review-empty".to_owned())
                .role(Role::Status)
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
            return empty("Nothing changed".to_owned());
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
        let body = self.body(cx);
        let foot = self.foot(cx);
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
            .relative()
            .child(scopes)
            .child(body)
            .child(foot)
            .children(self.commit.as_ref().map(|(sheet, _)| sheet.clone()))
    }
}
