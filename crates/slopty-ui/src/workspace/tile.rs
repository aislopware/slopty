//! One tile: the header (kind, title, place, status, the actions shown on hover or focus) and
//! the body (a terminal, a remote window or display, a note, a file tile), with the pill that
//! says when the body cannot show what it should.

use std::collections::HashMap;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, App, Context, Div, ElementId, ExternalPaths, FontWeight,
    InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window,
    div, px,
};
use gpui_kit::component::input::Input;
use slopty_client::layout::{Placed, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus};
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::screen::SourceState;
use slopty_proto::terminal::{SessionState, SessionSummary, TermRequest};
use slopty_theme::{Theme, Typography};

use super::actions::{CloseItem, FullscreenTile};
use super::browsers::ADDRESS;
use super::{Field, WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::browser::BrowserView;
use crate::chrome_text::ChromeText;
use crate::colors::hsla;
use crate::folder::FolderView;
use crate::icons::{IconName, IconSize, Status};
use crate::terminal::TerminalView;
use crate::{add_worker, kit};

/// Below this zoom the overview draws a tile as its miniature: the header's surface without its
/// words, the body as it stands at the zoom, and a label at chrome size under it that names the
/// tile (`miniature.rs`).
pub(super) const SHAPES_BELOW: f32 = 0.5;

/// The divider between neighbouring tiles: one hairline, whatever the zoom.
const HAIRLINE: f32 = 1.0;

/// The widest a page's address gets beside its title, in points at zoom 1: the title is what
/// tells tiles apart, the address only says where.
const HEADER_URL_MAX: f32 = 180.0;

/// The height of an upload's progress bar along the bottom of the header.
const PROGRESS: f32 = 2.0;

/// The group every tile's header actions hover with.
const TILE_GROUP: &str = "tile";

/// The group a tab's close button hovers with.
const TAB_GROUP: &str = "tab";

/// The widest a tab grows in a tabbed column's tab row, in points at zoom 1: past it the tabs
/// read as one long bar rather than as tabs.
const TAB_MAX: f32 = 200.0;

/// The narrowest a tab is, in points at zoom 1: a four-letter title still makes a tab a
/// pointer can find, not a sliver.
const TAB_MIN: f32 = 120.0;

/// The group a header's window controls (fullscreen, close) hover with: they show while the
/// pointer is on the header itself, not anywhere over the body.
const HEADER_GROUP: &str = "tile-header";

/// How much faster a header's place shrinks than its title.
const PLACE_SHRINK: f32 = 1000.0;
/// How much faster than the title a header's readouts give way: an agent's pill shortens to
/// an ellipsis while the title still reads whole.
const STRIP_SHRINK: f32 = 20.0;

/// What the in-body pill says while a tile's worker is being dialled again.
pub const RECONNECTING: &str = "Reconnecting…";

/// What the in-body pill says for a shell whose session is gone and whose status is not known.
pub const SESSION_ENDED: &str = "Session ended";

/// The in-body pill's action for a worker on another build: the command that updates it, to
/// the clipboard.
pub const COPY_COMMAND: &str = "Copy command";

/// The accessible name of a tile's close button.
pub const CLOSE_TILE: &str = "Close tile";

/// The accessible name of a tile's fullscreen button.
pub const FULLSCREEN_TILE: &str = "Fullscreen tile";

/// The header button that shows an agent terminal's conversation.
pub const SHOW_CONVERSATION: &str = "Show conversation";

/// The same button while the conversation shows.
pub const SHOW_TERMINAL: &str = "Show terminal";

/// The accessible name of the [`HOOKS`] pill.
pub const INSTALL_HOOKS: &str = "Install hooks";

/// The accessible name of the [`TAKE`] pill.
pub const TAKE_OVER: &str = "Take over";

/// The pill offering the hooks that make an agent's status precise.
pub const HOOKS: &str = "Hooks";
/// The pill that takes a PTY's size from the client driving it.
pub const TAKE: &str = "Take";
/// A remote window's audio toggle: one name, pressed while this client has silenced it.
pub const MUTE: &str = "Mute";
/// A shell's body while its view attaches.
pub const ATTACHING: &str = "Attaching…";
/// A window or display asleep in the registry.
pub const SLEEPING: &str = "Sleeping";
/// A window or display whose stream was let go while it was off screen.
pub const PAUSED: &str = "Paused off screen";
/// A stream or a page on its way.
pub const OPENING: &str = "Opening…";
/// A file tile waiting for its text.
pub const READING: &str = crate::file::READING;
/// A note whose editor is not made yet.
pub const NOTE: &str = "Note";

/// How much of a note's first line the header shows.
pub const NOTE_TITLE_CHARS: usize = 40;

/// How the chrome is scaled this frame: `k`, the overview's zoom, and whether that zoom is
/// in motion (chrome text then paints from the raster ladder).
#[derive(Clone, Copy, Debug)]
pub(super) struct Chrome {
    pub k: f32,
    pub zooming: bool,
}

/// What a body with nothing to show yet says, and when.
enum Wait {
    /// A state that lasts (asleep, let go off screen): said at once.
    Lasting(SharedString),
    /// A wait the worker is about to end (opening, reading, attaching): said only past the
    /// loading grace.
    Loading(SharedString),
}

/// What surrounds a tile on the strip.
#[derive(Clone, Copy, Debug)]
struct Edges {
    /// A column continues to its right: it draws the divider on its right edge.
    right: bool,
    /// A tile is stacked below it in its column: it draws the divider on its bottom edge.
    below: bool,
}

/// A note's title, "note" while it is empty.
///
/// Its first non-empty line with Markdown's heading, list, quote and task marks stripped, cut
/// to [`NOTE_TITLE_CHARS`]. How far its tasks got is context, not title: the header says it
/// after the title, the navigator on the second line.
#[must_use]
pub fn note_title(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['#', '-', '*', '>', ' ']).trim())
        .map(|l| {
            ["[ ] ", "[x] ", "[X] "]
                .iter()
                .find_map(|mark| l.strip_prefix(mark))
                .unwrap_or(l)
                .trim()
        })
        .find(|l| !l.is_empty());
    match line {
        None => UNTITLED_NOTE.to_owned(),
        Some(line) if line.chars().count() > NOTE_TITLE_CHARS => {
            let cut: String = line.chars().take(NOTE_TITLE_CHARS).collect();
            format!("{}…", cut.trim_end())
        }
        Some(line) => line.to_owned(),
    }
}

/// An empty note's title.
pub const UNTITLED_NOTE: &str = "Untitled note";

/// How many of a note's task lines are ticked, and how many there are; `None` without any.
#[must_use]
pub fn note_progress(text: &str) -> Option<(usize, usize)> {
    let (done, total) = crate::markdown::task_counts(text);
    (total > 0).then_some((done, total))
}

/// What a note's header, row and palette line say of it, worked out once per text rather than
/// once per frame: a note holds up to 64 KiB.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct NoteFacts {
    /// [`note_title`].
    pub title: String,
    /// [`note_progress`].
    pub progress: Option<(usize, usize)>,
}

impl NoteFacts {
    pub(super) fn of(text: &str) -> Self {
        Self { title: note_title(text), progress: note_progress(text) }
    }
}

/// A file tile's title: the file's name.
#[must_use]
pub fn file_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(trimmed).to_owned()
}

/// A folder's name, as its header gives it: its last component, `/` for the root and `~` for
/// the home it was reached as.
#[must_use]
pub fn folder_title(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    file_title(trimmed)
}

/// The directory a file is in, as its header gives it after the title (`src`), so two `mod.rs`
/// tiles can be told apart; `None` for a bare name.
#[must_use]
pub fn file_dir(path: &str) -> Option<String> {
    let mut parts = path.trim_end_matches('/').rsplit('/');
    parts.next();
    parts.next().filter(|d| !d.is_empty()).map(str::to_owned)
}

/// Where a shell is, short: the last two components of `path`, with the home directory as `~`
/// (`~`, `~/src`, `oss/slopty`). `home` is the worker's, once it has said; until then a home is
/// recognised by its shape: `/Users/<name>` on a Mac, `/home/<name>` or `/root` elsewhere.
#[must_use]
pub fn cwd_tail(path: &str, home: Option<&str>) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    let home = match home.map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty()) {
        Some(home) => {
            let home: Vec<&str> = home.split('/').filter(|p| !p.is_empty()).collect();
            if parts.starts_with(&home) { home.len() } else { 0 }
        }
        None => match parts.as_slice() {
            ["Users" | "home", _, ..] => 2,
            ["root", ..] => 1,
            _ => 0,
        },
    };
    let (home, rest) = if trimmed.starts_with('/') && home > 0 {
        (true, parts.get(home..).unwrap_or_default())
    } else {
        (false, parts.as_slice())
    };
    match (home, rest) {
        (true, []) => "~".to_owned(),
        (true, [one]) => format!("~/{one}"),
        (_, [.., parent, name]) => format!("{parent}/{name}"),
        (false, [one]) if trimmed.starts_with('/') => format!("/{one}"),
        (false, [one]) => (*one).to_owned(),
        (false, []) => "/".to_owned(),
    }
}

/// What a shell is called with nothing better to say: no command, no title of its program's, no
/// place but home.
pub const TERMINAL: &str = "Terminal";

/// An agent's name, what its shell is called before the agent titles it.
pub(super) const fn agent_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::ClaudeCode => "Claude Code",
    }
}

/// Whether a title a shell's program set says more than the shell does. Not the program's own
/// name (the worker's word while nothing set a title) nor a shell's, and not a path or a
/// `user@host:path` prompt, which the place says better.
pub(super) fn own_title(title: &str, program: Option<&str>) -> bool {
    let name = |p: &str| p.rsplit('/').next().unwrap_or(p).trim_start_matches('-').to_owned();
    let title = title.trim();
    if title.is_empty() || title.starts_with(['/', '~']) {
        return false;
    }
    let word = if title.contains(char::is_whitespace) { title.to_owned() } else { name(title) };
    let shell = matches!(word.as_str(), "shell" | "sh" | "zsh" | "bash" | "fish" | "nu" | "login");
    let prompt =
        title.split_once(':').is_some_and(|(who, _)| who.contains('@') && !who.contains(' '));
    !shell && !prompt && program.is_none_or(|p| name(p) != word)
}

/// `cwd`'s path within `repo`: empty at its root, `None` outside it.
fn within(cwd: &str, repo: &str) -> Option<String> {
    let repo = repo.trim_end_matches('/');
    let rest = cwd.trim_end_matches('/').strip_prefix(repo).filter(|_| !repo.is_empty())?;
    match rest.strip_prefix('/') {
        Some(rest) => Some(rest.to_owned()),
        None => rest.is_empty().then(String::new),
    }
}

/// Where a shell is, as the status bar and a header say it: inside a repository, the
/// repository's name and the path within it (`slopty`, `slopty/crates/ui`); elsewhere the tail.
#[must_use]
pub(super) fn repo_place(cwd: &str, repo: Option<&str>, home: Option<&str>) -> String {
    let named = repo.and_then(|repo| {
        let name = repo.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty())?;
        let path = within(cwd, repo)?;
        Some(if path.is_empty() { name.to_owned() } else { format!("{name}/{path}") })
    });
    named.unwrap_or_else(|| cwd_tail(cwd, home))
}

/// A shell's name from where it is: the repository's last component, else the directory's;
/// none at the home directory, which says nothing.
pub(super) fn place_name(cwd: &str, repo: Option<&str>, home: Option<&str>) -> Option<String> {
    let last = |path: &str| {
        path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).map(str::to_owned)
    };
    repo.and_then(last).or_else(|| {
        let tail = cwd_tail(cwd, home);
        (!tail.starts_with('~') || tail.contains('/')).then(|| last(&tail)).flatten()
    })
}

/// The in-body state of a tile whose body cannot show what it should: what the pill says and
/// what it offers.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum BodyState {
    /// The tile's worker is out of reach; the text says how.
    Away(SharedString),
    /// The shell's program exited with this status (a signal negated); its session is
    /// still listed, so it can be started again where it was.
    Exited(i32),
    /// The shell's session is gone and nothing says how it ended.
    Ended,
    /// The tile's worker runs a different build: both builds, and the command that updates it.
    NeedsUpdate(slopty_client::update::UpdateNotice),
}

impl BodyState {
    /// What the pill says.
    pub(super) fn text(&self) -> SharedString {
        match self {
            Self::Away(text) => text.clone(),
            Self::Exited(status) if *status < 0 => {
                format!("Exited · signal {}", status.unsigned_abs()).into()
            }
            Self::Exited(status) => format!("Exited · code {status}").into(),
            Self::Ended => SESSION_ENDED.into(),
            Self::NeedsUpdate(notice) => notice.title().into(),
        }
    }

    /// The status its icon and tone come from.
    const fn status(&self) -> Status {
        match self {
            Self::Away(_) => Status::Away,
            // Nothing is broken and nothing is lost: an update is all it takes.
            Self::Exited(0) | Self::Ended | Self::NeedsUpdate(_) => Status::Idle,
            Self::Exited(_) => Status::Failed,
        }
    }
}

/// What the pill of a worker on a different build shows of its update, where the app can run
/// one ([`add_worker::Updates`]): the offer, the step under way with its bar, or why it failed.
struct UpdateState {
    status: Status,
    text: SharedString,
    detail: Option<String>,
    bar: Option<add_worker::Bar>,
    failed: bool,
    /// "Update" is on offer, so "Copy command" steps back to the secondary tone.
    offered: bool,
    start: Option<add_worker::Update>,
}

/// [`UpdateState`] for `state`; `None` for any other state, and where no update can run.
fn update_state(state: &BodyState, cx: &App) -> Option<UpdateState> {
    let BodyState::NeedsUpdate(notice) = state else { return None };
    let updates = cx.try_global::<add_worker::Updates>()?;
    let start = updates.start.clone();
    let Some(run) = updates.runs.get(&notice.host) else {
        return start.map(|start| UpdateState {
            status: state.status(),
            text: state.text(),
            detail: Some(notice.detail()),
            bar: None,
            failed: false,
            offered: true,
            start: Some(start),
        });
    };
    if let Some(failed) = &run.failed {
        return Some(UpdateState {
            status: Status::Failed,
            text: failed.title.clone().into(),
            detail: failed.hint.clone().or_else(|| failed.lines.last().cloned()),
            bar: None,
            failed: true,
            offered: start.is_some(),
            start,
        });
    }
    let step = run.current();
    Some(UpdateState {
        status: Status::Running,
        text: step.map_or_else(|| UPDATING.into(), |s| s.title.clone().into()),
        detail: step.and_then(|s| s.detail.clone()),
        bar: Some(run.bar),
        failed: false,
        offered: false,
        start: None,
    })
}

/// The pill's words while an update has no step to name yet.
const UPDATING: &str = "Updating the worker";

/// The icon a tile's header leads with: what the tile is.
pub(super) const fn kind_icon(item: &Item, agent: bool) -> IconName {
    match item.kind {
        ItemKind::Terminal { .. } if agent => IconName::Bot,
        ItemKind::Terminal { .. } => IconName::SquareTerminal,
        ItemKind::Window { .. } => IconName::AppWindow,
        ItemKind::Display { .. } => IconName::Monitor,
        ItemKind::Note { .. } => IconName::StickyNote,
        ItemKind::File { .. } => IconName::FileText,
        ItemKind::Folder { .. } => IconName::Folder,
        ItemKind::Browser { .. } => IconName::Globe,
    }
}

/// A note's progress as its header counts it after the name: `1/3`.
#[must_use]
pub(super) fn note_count(done: usize, total: usize) -> String {
    format!("{done}/{total}")
}

/// A note's progress as every list says it: `1 of 3 done`.
#[must_use]
pub(super) fn note_done(done: usize, total: usize) -> String {
    format!("{done} of {total} done")
}

/// The word for what an item is: `terminal`, `window`, `display`, `note`, `file`, `browser`.
pub(super) const fn kind_name(item: &Item) -> &'static str {
    match item.kind {
        ItemKind::Terminal { .. } => "terminal",
        ItemKind::Window { .. } => "window",
        ItemKind::Display { .. } => "display",
        ItemKind::Note { .. } => "note",
        ItemKind::File { .. } => "file",
        ItemKind::Folder { .. } => "folder",
        ItemKind::Browser { .. } => "browser",
    }
}

impl WorkspaceView {
    /// What a tile's title is placed by, the same in its header, its navigator row and its
    /// palette line: where a shell is, the folder a file is in, a page's address when the
    /// title is not it, how far a note's tasks got. A window or display has none. Kept with
    /// the titles ([`Self::number_twins`]).
    pub(super) fn tile_place(&self, item: &Item, cx: &App) -> Option<String> {
        match self.places.get(&item.id) {
            Some(place) => place.clone(),
            None => self.derived_place(item, &self.derived_title(item, cx), cx),
        }
    }

    /// [`Self::tile_place`] worked out, for an item whose derived title is `title`.
    fn derived_place(&self, item: &Item, title: &str, cx: &App) -> Option<String> {
        match &item.kind {
            ItemKind::Terminal { session } => self.shell_context(*session, title),
            ItemKind::File { path } | ItemKind::Folder { path } => file_dir(path),
            // The host alone: the path is the page's business, and the title names the page.
            ItemKind::Browser { .. } => self.browsers.get(&item.id).and_then(|v| {
                let v = v.read(cx);
                let host = v.short_url().split('/').next().unwrap_or_default();
                (!host.is_empty() && (item.name.is_some() || !v.page().title.trim().is_empty()))
                    .then(|| host.to_owned())
            }),
            ItemKind::Note { text } => {
                self.note_progress_of(item.id, text).map(|(done, total)| note_done(done, total))
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => None,
        }
    }

    /// What a header says without a name: the shell's title, the window's, "Display N", a
    /// note's first line, a file's `name · parent`.
    pub(super) fn derived_title(&self, item: &Item, cx: &App) -> String {
        match &item.kind {
            ItemKind::Terminal { session } => self.terminal_title(*session, cx),
            ItemKind::Window { window } => {
                self.titles.get(&item.id).cloned().unwrap_or_else(|| format!("Window {}", window.0))
            }
            ItemKind::Display { display } => format!("Display {}", display.0),
            ItemKind::Note { text } => self
                .note_facts
                .get(&item.id)
                .map_or_else(|| note_title(text), |facts| facts.title.clone()),
            ItemKind::File { path } => file_title(path),
            ItemKind::Folder { path } => folder_title(path),
            ItemKind::Browser { url } => self
                .browsers
                .get(&item.id)
                .map_or_else(|| crate::browser::short_url(url).to_owned(), |v| v.read(cx).title()),
        }
    }

    /// What a header says: the name the human gave the tile, else its derived title, as it
    /// was last worked out when the workspace changed.
    #[must_use]
    pub fn tile_title(&self, item: &Item, cx: &App) -> String {
        if let Some(name) = &item.name {
            return name.clone();
        }
        let derived = self.derived.get(&item.id);
        let title = derived.map_or_else(|| self.derived_title(item, cx), String::clone);
        match self.twins.get(&item.id) {
            Some(n) => format!("{title} {n}"),
            None => title,
        }
    }

    /// Unnamed tiles of one worker and one kind that would read alike ("Terminal" and
    /// "Terminal") are told apart by a number after the first, in the order they were made: the
    /// second is "Terminal 2". A named tile keeps the name it was given, and tiles of two kinds
    /// are told apart by their icons: a shell and a folder at one directory both read as it.
    ///
    /// Worked out when a title may have changed ([`Self::titles_dirty`]), not once a frame: it
    /// derives every item's title. Every item's derived title and place are kept with the
    /// numbers, for every header, row and line to read, and so that a program's new title that
    /// leaves its tile's as it was is nobody's news ([`Self::retitled`]).
    pub(super) fn number_twins(&mut self, cx: &App) {
        let derived: HashMap<ItemId, String> =
            self.items().map(|(_, item)| (item.id, self.derived_title(item, cx))).collect();
        let places = self
            .items()
            .map(|(_, item)| {
                let title = derived.get(&item.id).map_or("", String::as_str);
                (item.id, self.derived_place(item, title, cx))
            })
            .collect();
        let mut seen: HashMap<(WorkerKey, &str, &str), u32> = HashMap::new();
        let mut twins = HashMap::new();
        for (worker, item) in self.items().filter(|(_, i)| i.name.is_none()) {
            let Some(title) = derived.get(&item.id) else { continue };
            let count = seen.entry((worker, kind_name(item), title)).or_insert(0);
            *count = count.saturating_add(1);
            if *count > 1 {
                twins.insert(item.id, *count);
            }
        }
        drop(seen);
        self.twins = twins;
        self.derived = derived;
        self.places = places;
    }

    /// A shell's program set a title. News for the chrome only when the tile's own title
    /// follows it: a path, a prompt or the shell's own name leave it as it was.
    pub(super) fn retitled(&self, session: SessionId, cx: &mut Context<Self>) {
        let Some(item) = self.tile_of_session(session).and_then(|t| self.item(t)) else { return };
        let title = self.derived_title(item, cx);
        if self.derived.get(&item.id) != Some(&title) {
            cx.notify();
        }
    }

    /// How far note `id`'s tasks got, from its facts when they are worked out.
    pub(super) fn note_progress_of(&self, id: ItemId, text: &str) -> Option<(usize, usize)> {
        self.note_facts.get(&id).map_or_else(|| note_progress(text), |facts| facts.progress)
    }

    /// A terminal's title, the first that says something: the command it runs, a title its
    /// program set (not the shell's own name, a path or a prompt), the repository or
    /// directory it stands in, else "Terminal". An agent's shell takes the agent's own title, else
    /// the agent's name.
    #[must_use]
    pub fn terminal_title(&self, session: SessionId, cx: &App) -> String {
        let view = self.terminals.get(&session).map(|v| v.read(cx));
        let summary = self.session_on(session);
        let program = summary.and_then(|(_, s)| s.command.first()).map(String::as_str);
        let set = view
            .and_then(TerminalView::title)
            .or_else(|| summary.map(|(_, s)| s.title.as_str()))
            .map(str::trim)
            .filter(|t| own_title(t, program));
        // An agent that has not titled itself is named by its task, its session's first
        // prompt, once its face has read it: the kind's glyph already says "Claude".
        if let Some(agent) = self.agent_state(session).filter(|a| a.status != AgentStatus::None) {
            return set
                .map(str::to_owned)
                .or_else(|| self.faces.views.get(&session)?.read(cx).first_prompt())
                .unwrap_or_else(|| agent_name(agent.kind).to_owned());
        }
        let running = view
            .and_then(|v| v.state().running_command())
            .and_then(|c| c.lines().next())
            .map(str::trim)
            .filter(|c| !c.is_empty());
        running
            .or(set)
            .map(str::to_owned)
            .or_else(|| {
                let (home, s) = summary?;
                place_name(s.cwd.as_deref()?, s.repo.as_deref(), home)
            })
            .unwrap_or_else(|| TERMINAL.to_owned())
    }

    /// A shell's context after its title: where it is, less what the title already says. A
    /// shell named by its repository gives the path within it, else its branch; one named by
    /// its directory gives the path to it; any other gives the repository and the path within
    /// it, else the directory.
    fn shell_context(&self, session: SessionId, title: &str) -> Option<String> {
        let (home, s) = self.session_on(session)?;
        let cwd = s.cwd.as_deref()?;
        let repo = s.repo.as_deref();
        let named = place_name(cwd, repo, home);
        if named.is_none_or(|name| name != title) {
            return Some(repo_place(cwd, repo, home));
        }
        match repo {
            Some(repo) => within(cwd, repo).filter(|p| !p.is_empty()).or_else(|| s.branch.clone()),
            None => Some(cwd_tail(cwd, home)),
        }
    }

    /// The summary of `session`, with its worker's home.
    pub(super) fn session_on(&self, session: SessionId) -> Option<(Option<&str>, &SessionSummary)> {
        self.workers.values().find_map(|w| w.sessions.get(&session).map(|s| (w.home.as_deref(), s)))
    }

    /// `session`'s directory, short, its worker's home written `~` ([`cwd_tail`]).
    pub(super) fn session_tail(&self, session: SessionId) -> Option<String> {
        let (home, s) = self.session_on(session)?;
        Some(cwd_tail(s.cwd.as_deref()?, home))
    }

    /// `worker`'s home directory, once it has said.
    pub(super) fn home_of(&self, worker: WorkerKey) -> Option<&str> {
        self.workers.get(&worker)?.home.as_deref()
    }

    /// One tile as the frame places it, `rect` already in the strip's coordinates.
    pub(super) fn render_tile(
        &self,
        placed: &Placed,
        chrome: Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let item = self.item(tile)?;
        let theme = &self.theme;
        let id = item.id;
        let worker_up = self.workers.get(&tile.worker).is_some_and(|w| w.link.is_some());

        let title = self.tile_title(item, cx);
        let label = SharedString::from(title.clone());
        let header = self.render_header(placed, item, title, chrome, cx);
        let body = self.render_body(placed, item, chrome, window, cx);
        // Files dropped on a shell go to its directory; on a remote window, to the worker's
        // clipboard; on a folder, into it.
        let takes_files = worker_up
            && matches!(
                item.kind,
                ItemKind::Terminal { .. }
                    | ItemKind::Window { .. }
                    | ItemKind::Display { .. }
                    | ItemKind::Folder { .. }
            );
        let accent = theme.surfaces.accent;

        // The open animation grows the tile about its centre.
        let rect = placed.rect;
        let (width, height) = (rect.w * placed.scale, rect.h * placed.scale);
        let (left, top) = (rect.x + (rect.w - width) / 2.0, rect.y + (rect.h - height) / 2.0);
        Some(
            div()
                .id(ElementId::Uuid(*id.as_uuid()))
                .debug_selector(move || format!("item-{}", id.as_uuid()))
                .group(TILE_GROUP)
                .role(Role::Group)
                .aria_label(label)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .opacity(placed.alpha)
                .flex()
                .flex_col()
                .overflow_hidden()
                .bg(hsla(theme.terminal.bg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .when(takes_files, |el| {
                    el.drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_1().border_color(hsla(accent))
                    })
                    .on_drop(cx.listener(
                        move |this, paths: &ExternalPaths, _w, cx| {
                            this.drop_files(tile, paths.paths(), cx);
                        },
                    ))
                })
                .child(header)
                .child(body)
                .into_any_element(),
        )
    }

    /// The hairlines a tile owes its neighbours: on its right edge where a column follows, on
    /// its bottom edge where a tile is stacked below. Laid over the tile's own edge, so a
    /// neighbour coming or going never moves its content by a point (a terminal would re-fit
    /// its grid). The strip draws them above every tile: a neighbour's edge, rounded to the same
    /// pixel at a fractional zoom, would otherwise paint over a line its tile drew.
    pub(super) fn render_dividers(&self, placed: &Placed) -> Vec<gpui::AnyElement> {
        let edges = self.edges(placed);
        let id = placed.tile.item;
        let rect = placed.rect;
        let (width, height) = (rect.w * placed.scale, rect.h * placed.scale);
        let (left, top) = (rect.x + (rect.w - width) / 2.0, rect.y + (rect.h - height) / 2.0);
        let line = || div().absolute().opacity(placed.alpha).bg(hsla(self.theme.surfaces.border));
        let right = edges.right.then(|| {
            line()
                .debug_selector(move || format!("divider-right-{}", id.as_uuid()))
                .left(px(left + width - HAIRLINE))
                .top(px(top))
                .w(px(HAIRLINE))
                .h(px(height))
                .into_any_element()
        });
        let below = edges.below.then(|| {
            line()
                .debug_selector(move || format!("divider-below-{}", id.as_uuid()))
                .left(px(left))
                .top(px(top + height - HAIRLINE))
                .w(px(width))
                .h(px(HAIRLINE))
                .into_any_element()
        });
        right.into_iter().chain(below).collect()
    }

    /// Where another tile continues from this one. Read off its place in the layout: two
    /// lengths, no walk over the tiles.
    fn edges(&self, placed: &Placed) -> Edges {
        let pos = placed.pos;
        let columns = self.layout.workspaces().get(pos.workspace).map_or(&[][..], |w| w.columns());
        let stacked = columns.get(pos.column).map_or(1, |c| c.tiles().len());
        // A tabbed column shows one tile at a time: nothing is below it.
        let stacked = if placed.tabs.is_some() { 1 } else { stacked };
        Edges {
            right: !placed.fullscreen && pos.column.saturating_add(1) < columns.len(),
            below: pos.tile.saturating_add(1) < stacked,
        }
    }

    /// Whether a tile's body may be drawn from its cached view. A cached view replays last
    /// frame's paint, the keyboard's input handler and key listeners included, until the view
    /// itself is notified. So a body is drawn afresh in the frame the focus comes to it or
    /// leaves it, and a focused body in the frame the keyboard moves at all, even within the
    /// tile (from a shell to the rename field in its header): a replayed shell would still take
    /// the text. Otherwise the focused body is replayed too: whatever has the keyboard in it is
    /// the view or a field the view renders as an entity (gpui-kit's inputs and textareas are
    /// views), and a field's notify dirties every view above it. So another tile's frame (a
    /// stream, a flood, an animation step) never draws it again, and every key, caret blink and
    /// selection does.
    fn cacheable(&self, placed: &Placed, window: &Window, cx: &App) -> bool {
        let moved = placed.focused != (self.drawn_focus == Some(placed.tile));
        let keys_moved = placed.focused && window.focused(cx) != self.drawn_keys;
        !moved && !keys_moved
    }

    /// A tile fading out where it stood, over a fade: its surface and its header's glyph and
    /// title as they were, the content already gone. Square and frameless at any zoom, as every
    /// tile is, and never scaled: text does not shrink on its way out. Nothing where chrome
    /// does not move.
    pub(super) fn render_closing(
        &self,
        closing: &slopty_client::layout::Closing,
        chrome: Chrome,
        cx: &App,
    ) -> Option<gpui::AnyElement> {
        if !self.chrome_moves(cx) {
            return None;
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (rect, k) = (closing.rect, chrome.k);
        let id = closing.tile.item;
        let was = self.closed.iter().rev().find(|c| c.tile == closing.tile).map(|c| &c.item);
        let header = was.filter(|_| k >= SHAPES_BELOW).map(|item| {
            let muted = hsla(s.text_muted);
            div()
                .h(px(theme.density.header * k))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm * k))
                .px(px(theme.spacing.inset() * k))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(theme.typography.ui_size * k))
                .text_color(hsla(s.text_secondary))
                .font_family(theme.typography.ui_family.clone())
                .child(crate::palette::status_slot(theme, kind_icon(item, false), None, muted, k))
                .child(SharedString::from(self.tile_title(item, cx)))
        });
        let ghost = div()
            .debug_selector(move || format!("closing-{}", id.as_uuid()))
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(hsla(theme.terminal.bg))
            .children(header);
        let fade = kit::Pace::Fade.animation();
        Some(
            ghost
                .with_animation(
                    SharedString::from(format!("closing-{}", id.as_uuid())),
                    fade,
                    |el, t| el.opacity(1.0 - t),
                )
                .into_any_element(),
        )
    }

    fn render_header(
        &self,
        placed: &Placed,
        item: &Item,
        title: String,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = placed.tile;
        let id = item.id;
        let k = chrome.k;
        let focused = placed.focused;
        let kind = kind_name(item);
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session).map(|a| (session, a)),
            _ => None,
        };
        // The focused title leads at the medium weight in primary text; the rest step back to
        // the secondary tone at the regular weight, so a wall of tiles reads as titles still.
        let ink = if focused { s.text } else { s.text_secondary };
        let heading = SharedString::from(if kind == title {
            title.clone()
        } else {
            format!("{kind} {title}")
        });
        // Focus is told by the header: the focused one is its body's surface in primary text,
        // with nothing between them, so the tile reads as one piece; the others step down to
        // the panel, muted, with a quiet hairline over their bodies. A page or a remote picture
        // is another program's surface, never quite the content's, so its header keeps the
        // hairline focused too. A tabbed column's header is its tab row, whose tabs draw their
        // own edges.
        let foreign = matches!(
            item.kind,
            ItemKind::Browser { .. } | ItemKind::Window { .. } | ItemKind::Display { .. }
        );
        // At the overview's small zoom the miniature's label names the tile; a band on top of it in
        // another step, and a hairline under only some of them, read as tiles half drawn.
        let shapes = k < SHAPES_BELOW;
        let header = div()
            .id("title")
            .debug_selector(move || format!("title-{}", id.as_uuid()))
            .group(HEADER_GROUP)
            .role(Role::Heading)
            .aria_label(heading)
            .relative()
            .h(px(theme.density.header * k))
            .w_full()
            .flex_none()
            .flex()
            .text_size(px(theme.typography.ui_size * k))
            .text_color(hsla(ink))
            .font_family(theme.typography.ui_family.clone())
            .cursor_grab()
            .map(|el| {
                if focused || shapes { el.bg(hsla(theme.content())) } else { el.bg(hsla(s.panel)) }
            })
            .when(!shapes && (!focused || foreign), |el| {
                el.border_b_1().border_color(hsla(s.border_subtle))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    // The second click of a double-click names the tile; the first began a
                    // move that its mouse-up ended.
                    if ev.click_count == 2 {
                        this.start_rename(tile, window, cx);
                    } else {
                        this.begin_move(tile, ev, cx);
                    }
                    cx.stop_propagation();
                }),
            );
        if shapes {
            return header.into_any_element();
        }
        // The face's approval card is the tile's statement while it shows: a pill over it would
        // say the same thing a few hundred points higher.
        let badge = agent
            .filter(|(session, _)| !self.face_asks(*session, cx))
            .and_then(|(session, a)| self.agent_badge(tile, session, a, chrome, cx));
        let unwatched = match &item.kind {
            ItemKind::Terminal { session } => self.finished.get(session).map(|f| (*session, f)),
            _ => None,
        };
        // A command that ended unwatched: the slot's mark says how, so a good end reads as the
        // time it took alone, and a failure keeps its exit code. That it went unseen is the
        // navigator's and the tab's to say: this tile is on screen.
        let finished = unwatched.map(|(session, f)| {
            if f.exit.is_some_and(|code| code != 0) {
                self.finished_badge(tile, session, f, chrome, cx)
            } else {
                self.finished_took(tile, session, f, chrome, cx)
            }
        });
        // An agent the worker had to guess at: offer the hooks that would make it precise.
        let hooks = agent
            .filter(|(_, a)| a.source != AgentSource::Hook && !self.hooks_offered(tile.worker))
            .map(|_| {
                let worker = tile.worker;
                let pill = pill("hooks", id, HOOKS, s.warn, theme, chrome)
                    .role(Role::Button)
                    .aria_label(INSTALL_HOOKS);
                tab_stop(pill, s.accent)
                    .on_click(cx.listener(move |this, _ev, _w, cx| this.install_hooks(worker, cx)))
                    .into_any_element()
            });
        // An upload in flight says how far it got; a click stops it. An attachment's is said
        // by its chip over the composer, and said once.
        let upload = self.header_upload(tile).map(|(xfer, upload)| {
            let pill = pill("upload", id, upload.label(), s.text_secondary, theme, chrome)
                .role(Role::Button)
                .aria_label("Cancel upload");
            // How far it got, as a line along the header's foot in the accent fill.
            let bar = div()
                .debug_selector(move || format!("upload-progress-{}", id.as_uuid()))
                .absolute()
                .left_0()
                .bottom_0()
                .h(px(PROGRESS * k))
                .w(gpui::relative(upload.fraction()))
                .bg(hsla(s.accent_fill));
            let pill = tab_stop(kit::tabular(pill), s.accent)
                .on_click(cx.listener(move |this, _ev, _w, cx| this.cancel_upload(xfer, cx)))
                .into_any_element();
            (pill, bar)
        });
        let (upload, progress) = upload.unzip();
        let ports = self.port_pills(tile, item, chrome, cx);
        let actions = self.header_actions(tile, item, chrome, cx);
        let face = agent.and_then(|(session, _)| self.face_toggle(tile, session, chrome, cx));
        // The kind's own actions stay out of sight until the tile is hovered or focused: a wall
        // of tiles reads as titles, not buttons. Touch has no hover, so the focused tile shows
        // them.
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .when(!focused, |el| el.invisible().group_hover(TILE_GROUP, gpui::Styled::visible))
            .children(actions);
        // How long a command has run, beside its calm mark, while no agent speaks for the shell.
        let running = match &item.kind {
            ItemKind::Terminal { session } if agent.is_none() => self.running_for(*session, cx),
            _ => None,
        };
        let running = running.map(|ran| {
            let text = SharedString::from(kit::duration(ran));
            kit::tabular(div())
                .id("running")
                .debug_selector(move || format!("running-{}", id.as_uuid()))
                .role(Role::Status)
                .aria_label(text.clone())
                .flex_none()
                .text_size(px(theme.typography.small() * k))
                .text_color(hsla(s.text_secondary))
                .child(text)
                .into_any_element()
        });
        // How far a note's tasks got, as a count after its name.
        let tasks = match &item.kind {
            ItemKind::Note { text } => self.note_progress_of(id, text),
            _ => None,
        };
        let tasks = tasks.map(|(done, total)| {
            kit::tabular(div())
                .id("tasks")
                .debug_selector(move || format!("tasks-{}", id.as_uuid()))
                .role(Role::Status)
                .aria_label(SharedString::from(format!("{done} of {total} done")))
                .flex_none()
                .text_size(px(theme.typography.meta() * k))
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(note_count(done, total)))
                .into_any_element()
        });
        let readouts: Vec<gpui::AnyElement> =
            badge.into_iter().chain(running).chain(finished).chain(tasks).collect();
        let strip = self.trailing_strip(placed, readouts, face, k, cx);
        // The title's context: where a shell or a file is, a page's address when the title is
        // not it, how far a note's tasks got. Muted, in the UI face as every header's context
        // is, with no separator: the colour tells it from the title.
        let place = match &item.kind {
            ItemKind::Terminal { .. } => {
                self.tile_place(item, cx).and_then(|p| place_beside(p, &title))
            }
            // How far a note's tasks got is a readout at the end, as a command's time is. The
            // path bar right under a folder's header is where it is, every folder above it a
            // click away; the parent beside the title said it twice, 20 pt apart.
            ItemKind::Note { .. } | ItemKind::Folder { .. } => None,
            _ => self.tile_place(item, cx),
        };
        // A directory keeps the folder it ends in.
        let path = matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::File { .. });
        let field = self.rename.as_ref().filter(|r| r.tile == tile).map(|r| r.field);
        let page = matches!(item.kind, ItemKind::Browser { .. });
        // A page with no title of its own is titled by its address, which is then what a
        // click turns into the address field.
        let address_title = page && field.is_none() && place.is_none();
        let address = if page && field != Some(Field::Address) {
            place.as_ref().and_then(|_| self.address_place(tile, focused, chrome, cx))
        } else {
            None
        };
        let text_place = place.filter(|_| !page).map(|text| {
            let mut place = div();
            // Gives way long before the title does: a narrow tile keeps its name whole and
            // loses where it is first.
            place.style().flex_shrink = Some(PLACE_SHRINK);
            place
                .id("place")
                .debug_selector(move || format!("place-{}", id.as_uuid()))
                .role(Role::Label)
                .aria_label(SharedString::from(text.clone()))
                .min_w_0()
                .max_w(px(HEADER_URL_MAX * k))
                .overflow_hidden()
                .text_color(hsla(s.text_muted))
                .child(
                    if path {
                        ChromeText::new(text, px(theme.typography.small()), k).fill_from_start()
                    } else {
                        ChromeText::new(text, px(theme.typography.small()), k).fill()
                    }
                    .zooming(chrome.zooming),
                )
                .into_any_element()
        });
        let place = address.or(text_place);
        // The worker's name, where more than one could be meant: quiet text after a server
        // glyph, a fact about the tile rather than a control.
        let worker =
            (self.workers.len() > 1).then(|| self.workers.get(&tile.worker)).flatten().map(|w| {
                let name = w.name.clone();
                let muted = hsla(s.text_muted);
                div()
                    .id("worker")
                    .debug_selector(move || format!("worker-{}", id.as_uuid()))
                    .role(Role::Label)
                    .aria_label(SharedString::from(name.clone()))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs * k))
                    .text_color(muted)
                    .child(
                        crate::icons::icon(theme, IconName::Server, IconSize::Inline, muted)
                            .size(px(theme.typography.small() * k)),
                    )
                    .child(
                        ChromeText::new(name, px(theme.typography.small()), k)
                            .zooming(chrome.zooming),
                    )
            });
        // A file with an edit not yet on disk says so with a dot after its name, as an editor's
        // tab does; saving keeps the dot until the worker has written it.
        let unsaved = self.files.get(&id).is_some_and(|v| {
            let v = v.read(cx);
            v.dirty() || v.saving()
        });
        let unsaved = unsaved.then(|| {
            div()
                .id("unsaved")
                .debug_selector(move || format!("unsaved-{}", id.as_uuid()))
                .role(Role::Image)
                .aria_label("Unsaved changes")
                .flex_none()
                .size(px(theme.spacing.sm * k))
                .rounded_full()
                .bg(hsla(s.text_secondary))
        });
        let tabbed = placed.tabs.is_some();
        let header = if tabbed {
            let tabs = self.render_tabs(placed, chrome, cx);
            // What the tabs leave is the bar's, hairline and all; the tile's controls end it.
            let rest = div()
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(theme.spacing.sm * k))
                .pr(px(theme.spacing.inset() * k))
                .border_b_1()
                .border_color(hsla(s.border_subtle))
                .children(ports)
                .when_some(upload, gpui::ParentElement::child)
                .when_some(hooks, gpui::ParentElement::child)
                .child(actions)
                .child(strip);
            header.bg(hsla(s.panel)).border_b_0().child(tabs).child(rest)
        } else {
            let lead = self.leading_slot(tile, item, focused, k, cx);
            let name = self.header_name(tile, id, title, chrome);
            // The title keeps its width and what is beside it gives way: the place first, then
            // the readouts at the end (an agent's pill), the title last. Each takes what it
            // needs, and an empty stretch between them takes the rest.
            // The unsaved dot follows the title it qualifies, as an editor's tab has it, not
            // the far end of the bar.
            let named = div()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs * k))
                .when(focused, |el| el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT)))
                .when(address_title, |el| {
                    el.cursor_text().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                            this.start_address(tile, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                })
                .child(name)
                .when_some(unsaved, gpui::ParentElement::child);
            let renaming = self.rename.as_ref().is_some_and(|r| r.tile == tile);
            let named = if renaming { named.flex_1() } else { named };
            header
                .items_center()
                .px(px(theme.spacing.inset() * k))
                .gap(px(theme.spacing.sm * k))
                .child(lead)
                .child(named)
                .children(place)
                .when(!renaming, |el| el.child(div().flex_1()))
                .when_some(worker, gpui::ParentElement::child)
                .children(ports)
                .when_some(upload, gpui::ParentElement::child)
                .when_some(hooks, gpui::ParentElement::child)
                .child(actions)
                .child(strip)
        };
        header.when_some(progress, gpui::ParentElement::child).into_any_element()
    }

    /// The header's leading slot: the kind's icon at rest and the status mark once there is
    /// one (working, failed, done, away), in one fixed square so every title starts on the
    /// same edge, as the navigator's rows and the palette's do. An agent waiting on the human
    /// keeps its kind's glyph: the state chip beside the title says it, and a warn mark in the
    /// slot said it a second time. An idle agent keeps it too: rest shows the kind, and a
    /// hollow ring beside a title read as an unticked radio button. The glyph sits a step under
    /// the title's tone. On a Mac a file's slot is its proxy, dragged out as a document
    /// window's title icon is.
    fn leading_slot(
        &self,
        tile: TileRef,
        item: &Item,
        focused: bool,
        k: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let id = item.id;
        let s = &self.theme.surfaces;
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session).is_some(),
            _ => false,
        };
        // A remote picture on its way turns its mark in the body, which says what opens.
        let opening = self.opening(item, cx);
        let status = self
            .tile_status(tile, item, cx)
            .filter(|st| !(agent && matches!(st, Status::NeedsYou | Status::Idle)))
            .filter(|st| !(opening && *st == Status::Working));
        let ink = if focused { s.text_secondary } else { s.text_muted };
        let slot =
            crate::palette::status_slot(&self.theme, kind_icon(item, agent), status, hsla(ink), k)
                .debug_selector(move || format!("status-{}", id.as_uuid()))
                .into_any_element();
        self.file_proxy(item, tile, slot, k, cx)
    }

    /// The title, or the field that renames the tile in its place.
    fn header_name(
        &self,
        tile: TileRef,
        id: ItemId,
        title: String,
        chrome: Chrome,
    ) -> gpui::AnyElement {
        let renaming =
            self.rename.as_ref().filter(|r| r.tile == tile).map(|r| (r.field, r.input.clone()));
        match renaming {
            // The field takes the title's place (an address the place's too); a click in it
            // must not start a move.
            Some((field, input)) => {
                let (part, label) = match field {
                    Field::Name => ("rename", "Tile name"),
                    Field::Address => ("address", ADDRESS),
                };
                div()
                    .id(part)
                    .debug_selector(move || format!("{part}-{}", id.as_uuid()))
                    .flex_1()
                    .overflow_hidden()
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .child(Input::new(&input).aria_label(label))
                    .into_any_element()
            }
            None => div()
                .debug_selector(move || format!("name-{}", id.as_uuid()))
                .min_w_0()
                .overflow_hidden()
                .child(
                    ChromeText::new(title, px(self.theme.typography.ui_size), chrome.k)
                        .fill()
                        .zooming(chrome.zooming),
                )
                .into_any_element(),
        }
    }

    /// A page's address as its header's place, as a browser's address bar shows it at rest:
    /// the host in the title's ink between a quiet scheme and path, the path giving way first.
    /// A click turns the header into the address field.
    fn address_place(
        &self,
        tile: TileRef,
        focused: bool,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let url = self.browsers.get(&tile.item)?.read(cx).page().url.clone();
        let (scheme, host, rest) = crate::browser::address_parts(&url)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let id = tile.item;
        let ink = if focused { s.text } else { s.text_secondary };
        let part = |text: &str, color, shrink: f32| {
            let mut part = div();
            part.style().flex_shrink = Some(shrink);
            part.min_w_0().overflow_hidden().text_color(hsla(color)).child(
                ChromeText::new(text.to_owned(), px(theme.typography.small()), k)
                    .fill()
                    .zooming(chrome.zooming),
            )
        };
        let mut place = div();
        place.style().flex_shrink = Some(PLACE_SHRINK);
        Some(
            place
                .id("place")
                .debug_selector(move || format!("place-{}", id.as_uuid()))
                .role(Role::Button)
                .aria_label(ADDRESS)
                .aria_value(SharedString::from(url.clone()))
                .min_w_0()
                .max_w(px(HEADER_URL_MAX * k))
                .overflow_hidden()
                .flex()
                .items_center()
                .cursor_text()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                        this.start_address(tile, window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(part(scheme, s.text_muted, 0.0))
                .child(part(host, ink, 1.0))
                .when(!rest.is_empty(), |el| el.child(part(rest, s.text_muted, PLACE_SHRINK)))
                .into_any_element(),
        )
    }

    /// A tabbed column's tab row: a tab per tile in the column, each with its leading slot,
    /// its title and a close button that shows on the tab's hover (always on the one shown).
    /// The shown tab is its body's surface with no edge under it, as the focused header is;
    /// the rest sit on the panel with the bar's hairline under them.
    fn render_tabs(&self, placed: &Placed, chrome: Chrome, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let pos = placed.pos;
        let column = self
            .layout
            .workspaces()
            .get(pos.workspace)
            .and_then(|w| w.columns().get(pos.column))
            .map(|c| c.tiles().iter().map(slopty_client::layout::Tile::tile).collect::<Vec<_>>())
            .unwrap_or_default();
        let tabs = column.into_iter().filter_map(|tab| {
            let item = self.item(tab)?;
            let id = item.id;
            let shown = tab == placed.tile;
            let ink = match (shown, placed.focused) {
                (true, true) => s.text,
                (true, false) => s.text_secondary,
                (false, _) => s.text_muted,
            };
            let agent = match item.kind {
                ItemKind::Terminal { session } => self.agent_state(session).is_some(),
                _ => false,
            };
            let status = self.tile_status(tab, item, cx);
            let slot =
                crate::palette::status_slot(theme, kind_icon(item, agent), status, hsla(ink), k)
                    .debug_selector(move || format!("tab-slot-{}", id.as_uuid()));
            let title = self.tile_title(item, cx);
            let label = SharedString::from(title.clone());
            let name = self.header_name(tab, id, title, chrome);
            let close = kit::icon_button_at(
                theme,
                format!("tab-close-{}", id.as_uuid()),
                IconName::X,
                CLOSE_TILE,
                k,
            )
            .on_click(cx.listener(move |this, _ev, window, cx| this.close_tile(tab, window, cx)))
            .when(!shown, |el| el.invisible().group_hover(TAB_GROUP, gpui::Styled::visible));
            Some(
                div()
                    .id(SharedString::from(format!("tab-{}", id.as_uuid())))
                    .debug_selector(move || format!("tab-{}", id.as_uuid()))
                    .group(TAB_GROUP)
                    .role(Role::Tab)
                    .aria_label(label)
                    .aria_selected(shown)
                    // As wide as its title, between the two bounds; tabs that do not fit give
                    // way alike down to the narrower one.
                    .flex_initial()
                    .min_w(px(TAB_MIN * k))
                    .max_w(px(TAB_MAX * k))
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(theme.spacing.xs * k))
                    // On the edge grid, as a single header's slot is: the first tab's kind sits
                    // where the tile above's or below's does.
                    .pl(px(theme.spacing.inset() * k))
                    .pr(px(theme.spacing.xs * k))
                    .border_r_1()
                    .text_color(hsla(ink))
                    .when(shown && placed.focused, |el| {
                        el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    })
                    .map(|el| {
                        if shown {
                            el.border_color(hsla(s.border_subtle)).bg(hsla(theme.content()))
                        } else {
                            el.border_b_1()
                                .border_color(hsla(s.border_subtle))
                                .hover(move |el| el.bg(hsla(s.raised)))
                        }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            if ev.click_count == 2 {
                                this.start_rename(tab, window, cx);
                            } else {
                                this.begin_move(tab, ev, cx);
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .child(slot)
                    .child(div().flex_auto().min_w_0().overflow_hidden().child(name))
                    .child(close),
            )
        });
        // The tabs take their titles' widths and the bar after them the rest, so a few tabs
        // keep their titles whole and the bar's hairline runs on from the last.
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .children(tabs)
            .child(div().flex_1().h_full().border_b_1().border_color(hsla(s.border_subtle)))
            .into_any_element()
    }

    /// How the tile is doing, in the one status vocabulary: its agent's state (as its face
    /// knows it, [`Self::agent_mark`]); else, out of reach; else a remote picture on its way;
    /// else a shell's last command, failed or finished unwatched.
    pub(super) fn tile_status(&self, tile: TileRef, item: &Item, cx: &App) -> Option<Status> {
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        };
        if let Some(status) = session.and_then(|s| self.agent_mark(s, cx)) {
            return Some(status);
        }
        if self.workers.get(&tile.worker).is_none_or(|w| w.link.is_none()) {
            return Some(Status::Away);
        }
        if self.opening(item, cx) {
            return Some(Status::Working);
        }
        let session = session?;
        let failed = |exit: i64| if exit == 0 { Status::Done } else { Status::Failed };
        if let Some(done) = self.finished.get(&session) {
            return Some(failed(done.exit.map_or(0, i64::from)));
        }
        if let Some(SessionState::Exited { status }) = self.summary(session).map(|s| &s.state) {
            return Some(failed(i64::from(*status)));
        }
        // The newest prompt carries the status of the command before it: one lookup in the
        // prompt index, never a walk over the rows.
        let view = self.terminals.get(&session)?.read(cx);
        let state = view.state();
        if state.command_running() {
            return self.running_for(session, cx).map(|_| Status::Running);
        }
        let prompt = state.prompt_before(slopty_grid::LineIndex(u64::MAX))?;
        let exit = state.line(prompt)?.mark.exit()?;
        // A failure the grid is showing, washed and barred, is not marked a second time here.
        let shown = view.failure_in_view();
        (exit != 0 && !shown).then_some(Status::Failed)
    }

    /// How long `session`'s command has run, once that is past [`RUNNING_AFTER`]: what its
    /// tile's header, its navigator row and the status bar count.
    ///
    /// [`RUNNING_AFTER`]: super::RUNNING_AFTER
    pub(super) fn running_for(&self, session: SessionId, cx: &App) -> Option<std::time::Duration> {
        let ran = self.terminals.get(&session)?.read(cx).running_for()?;
        (ran >= self.running_after).then_some(ran)
    }

    /// Whether a remote window or display is on its way: asked for and not yet drawn, while
    /// its worker is up. Neither asleep nor let go off screen, which wait on nothing.
    fn opening(&self, item: &Item, cx: &App) -> bool {
        if !matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. }) {
            return false;
        }
        match self.screens.get(&item.id) {
            Some(view) => {
                let view = view.read(cx);
                view.frames() == 0 && view.source_state() == SourceState::Live
            }
            None => !item.sleeping && !self.parked.contains(&item.id),
        }
    }

    /// The ports a shell listens on, served here, each one pill of the header's form: the
    /// number opens the page in a tile, the arrow at its end in the default browser.
    fn port_pills(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let id = item.id;
        let ItemKind::Terminal { session } = &item.kind else { return Vec::new() };
        self.forwards(*session)
            .iter()
            .filter(|f| f.local.is_some())
            .map(|forward| {
                let number = forward.port.number;
                let label = match forward.local {
                    Some(local) if local != number => format!("{number} \u{2192} {local}"),
                    _ => format!("{number}"),
                };
                let tone = theme.surfaces.text_secondary;
                let raised = theme.surfaces.raised;
                let k = chrome.k;
                // Words, not a chip: the header's one fill is the state's. The two ends share
                // one corner, and each takes the hover fill alone under the pointer.
                let part = |part: String, label: SharedString, name: String| {
                    let selector = format!("{part}-{}", id.as_uuid());
                    kit::pill_frame(theme, k)
                        .id(SharedString::from(part))
                        .debug_selector(move || selector)
                        .role(Role::Link)
                        .aria_label(SharedString::from(name))
                        .flex_none()
                        .rounded_none()
                        .cursor_pointer()
                        .hover(move |el| el.bg(hsla(raised)))
                        .child(
                            ChromeText::new(label, px(theme.typography.small()), k)
                                .zooming(chrome.zooming),
                        )
                };
                let in_tile = part(
                    format!("port-{number}"),
                    label.into(),
                    format!("Open port {number} in a tile"),
                )
                .pl(px(theme.spacing.sm * k))
                .pr(px(theme.spacing.xs * k));
                let out = part(
                    format!("port-out-{number}"),
                    "\u{2197}".into(),
                    format!("Open port {number} in the browser"),
                )
                .pl(px(theme.spacing.xs * k))
                .pr(px(theme.spacing.sm * k));
                let url = forward.worker_url();
                let worker = tile.worker;
                let forward = forward.clone();
                // A port is a number: set in the mono face with figures of one width, as a
                // path is.
                kit::tabular(div())
                    .flex()
                    .flex_none()
                    .rounded(px(theme.radii.sm * k))
                    .overflow_hidden()
                    .text_size(px(theme.typography.small() * k))
                    .text_color(hsla(tone))
                    .font_family(crate::palette::mono_family(theme))
                    .child(tab_stop(in_tile, theme.surfaces.accent).on_click(cx.listener(
                        move |this, _ev, _w, cx| this.open_browser(Some(worker), &url, cx),
                    )))
                    .child(
                        tab_stop(out, theme.surfaces.accent)
                            .on_click(move |_ev, _w, _cx| Self::open_forward(&forward)),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// What a tile's kind offers in its header: take the PTY's size, mute, a page's back and
    /// reload.
    fn header_actions(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = &self.theme;
        let id = item.id;
        // A muted window always says so: a silenced tile must not pass for a quiet one.
        let muted = self.screens.get(&id).is_some_and(|v| v.read(cx).muted());
        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        match &item.kind {
            ItemKind::Terminal { session } => {
                let session = *session;
                // A phone's header has room for the tile's name and its state only.
                if self.face_shown(session)
                    && !self.layout.is_phone()
                    && let Some(view) = self.faces.views.get(&session)
                {
                    actions.extend(crate::conversation::ConversationView::header_chips(
                        view, chrome.k, cx,
                    ));
                }
                let session = &session;
                // Another client's size rules this PTY: offer to take it.
                if self.terminals.get(session).is_some_and(|v| !v.read(cx).driving()) {
                    let pill = pill("take", id, TAKE, theme.surfaces.accent, theme, chrome)
                        .role(Role::Button)
                        .aria_label(TAKE_OVER);
                    actions.push(
                        tab_stop(pill, theme.surfaces.accent)
                            .on_click(
                                cx.listener(move |this, _ev, _w, cx| this.take_over(tile, cx)),
                            )
                            .into_any_element(),
                    );
                }
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                if let Some(view) = self.screens.get(&id)
                    && let Some(mark) =
                        crate::screen::ScreenView::health_mark(view, theme, chrome.k, cx)
                {
                    actions.push(mark);
                }
                if let Some(view) = self.screens.get(&id)
                    && let Some(button) =
                        crate::screen::ScreenView::trackpad_button(view, theme, chrome.k, cx)
                {
                    actions.push(button);
                }
                // Only while the system's shortcuts go to the worker: this Mac's ⌘Tab not
                // working is a state to see, and a click here gives it back.
                if self.screens.get(&id).is_some_and(|v| v.read(cx).system_keys()) {
                    let toggle = kit::icon_toggle(
                        theme,
                        format!("system-keys-{}", id.as_uuid()),
                        IconName::Command,
                        super::desktop::SEND_SYSTEM_KEYS,
                        true,
                        chrome.k,
                    );
                    actions.push(
                        toggle
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                this.flip_system_keys(id, cx);
                            }))
                            .into_any_element(),
                    );
                }
                if let Some(view) = self.screens.get(&id).map(|v| v.read(cx))
                    && (muted || view.has_audio())
                {
                    let icon = if muted { IconName::VolumeX } else { IconName::Volume2 };
                    let toggle = kit::icon_toggle(
                        theme,
                        format!("mute-{}", id.as_uuid()),
                        icon,
                        MUTE,
                        muted,
                        chrome.k,
                    );
                    actions.push(
                        toggle
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                if let Some(view) = this.screens.get(&id) {
                                    view.read(cx).toggle_mute();
                                    cx.notify();
                                }
                            }))
                            .into_any_element(),
                    );
                }
            }
            // A page's way back and its reload are the header's bare icon buttons, as
            // fullscreen and close are: nothing in the bar has a fill until the pointer is on it.
            ItemKind::Browser { .. } => {
                if let Some(view) = self.browsers.get(&id).cloned() {
                    let uuid = id.as_uuid();
                    let k = chrome.k;
                    if view.read(cx).page().can_go_back {
                        let target = view.clone();
                        actions.push(
                            kit::icon_button_at(
                                theme,
                                format!("back-{uuid}"),
                                IconName::ArrowLeft,
                                "Back",
                                k,
                            )
                            .on_click(move |_ev, _w, cx| target.update(cx, BrowserView::back))
                            .into_any_element(),
                        );
                    }
                    actions.push(
                        kit::icon_button_at(
                            theme,
                            format!("reload-{uuid}"),
                            IconName::RotateCw,
                            "Reload",
                            k,
                        )
                        .on_click(move |_ev, _w, cx| view.update(cx, BrowserView::reload))
                        .into_any_element(),
                    );
                }
            }
            // The way up is the header's bare icon button, as a page's way back is.
            ItemKind::Folder { .. } => {
                if let Some(view) = self.folders.get(&id).cloned()
                    && view.read(cx).parent().is_some()
                {
                    actions.push(
                        kit::icon_button_at(
                            theme,
                            format!("up-{}", id.as_uuid()),
                            IconName::ArrowUp,
                            crate::folder::ENCLOSING_FOLDER,
                            chrome.k,
                        )
                        .on_click(move |_ev, _w, cx| view.update(cx, FolderView::open_parent))
                        .into_any_element(),
                    );
                }
            }
            ItemKind::Note { .. } | ItemKind::File { .. } => {}
        }
        actions
    }

    /// The button that turns an agent's tile between its TUI and its face: a control, so it
    /// sits with fullscreen and close in the trailing strip, never among the readouts it once
    /// split. `None` while no agent runs in the shell.
    fn face_toggle(
        &self,
        tile: TileRef,
        session: SessionId,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.agent_state(session).is_none_or(|a| a.status == AgentStatus::None) {
            return None;
        }
        let face = self.face_shown(session);
        let (icon, label) = if face {
            (IconName::SquareTerminal, SHOW_TERMINAL)
        } else {
            (IconName::MessageSquare, SHOW_CONVERSATION)
        };
        let id = format!("face-{}", tile.item.as_uuid());
        Some(
            kit::icon_button_at(&self.theme, id, icon, label, chrome.k)
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    this.focus_tile(tile, cx);
                    this.show_face(session, !face, cx);
                }))
                .into_any_element(),
        )
    }

    /// A shell command that ended well while the human looked elsewhere: the time it took,
    /// quiet in the meta size, since the slot's check already says it is done. Still a button
    /// to it; a screen reader hears the whole of it.
    fn finished_took(
        &self,
        tile: TileRef,
        session: SessionId,
        done: &super::Finished,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let item = tile.item;
        let raised = s.raised;
        let took = SharedString::from(kit::duration(done.elapsed));
        let el = kit::tabular(div())
            .id("finished")
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(SharedString::from(done.label()))
            .flex_none()
            .px(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.xs * k))
            .text_size(px(theme.typography.meta() * k))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(raised)))
            .child(ChromeText::new(took, px(theme.typography.meta()), k).zooming(chrome.zooming));
        tab_stop(el, s.accent)
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reveal_session(session, cx)))
            .into_any_element()
    }

    /// The header's right end: one fixed strip where the tile's readouts (the agent's pill, a
    /// finished command's time) sit at rest and the controls (an agent's face toggle,
    /// fullscreen, close) take their place while the pointer is on the header. It is never
    /// narrower than the buttons, so the swap moves nothing beside it. The focused tile with
    /// nothing to say shows its buttons at rest (touch has no hover). Each button focuses its
    /// tile and runs the action its key runs, so a click and ⌘J, ⌃⌘F or ⌘W do the same thing.
    fn trailing_strip(
        &self,
        placed: &Placed,
        readouts: Vec<gpui::AnyElement>,
        face: Option<gpui::AnyElement>,
        k: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let (tile, focused) = (placed.tile, placed.focused);
        let id = tile.item.as_uuid();
        // On a phone a column is the screen's width already: fullscreen would add nothing.
        let view_w = f32::from(self.viewport.size.width);
        let offer_fullscreen =
            view_w >= self.layout.config().phone_below || placed.target.w + 1.0 < view_w;
        let fullscreen = offer_fullscreen.then(|| {
            kit::icon_button_at(
                theme,
                format!("fullscreen-{id}"),
                IconName::Maximize2,
                FULLSCREEN_TILE,
                k,
            )
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.focus_tile(tile, cx);
                // The action runs from the focused element up; the workspace takes the focus when
                // nothing inside it has it, so the action reaches the handler on its root.
                if !this.focus.contains_focused(window, cx) {
                    window.focus(&this.focus, cx);
                }
                window.dispatch_action(Box::new(FullscreenTile), cx);
            }))
        });
        let close = kit::icon_button_at(theme, format!("close-{id}"), IconName::X, CLOSE_TILE, k)
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.close_tile(tile, window, cx);
            }));
        let quiet = readouts.is_empty();
        let buttons = f32::from(
            1_u8.saturating_add(u8::from(offer_fullscreen))
                .saturating_add(u8::from(face.is_some())),
        );
        let controls = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .when(!(focused && quiet), |el| {
                el.invisible().group_hover(HEADER_GROUP, gpui::Styled::visible)
            })
            .children(face)
            .children(fullscreen)
            .child(close);
        let readouts = div()
            .debug_selector(move || format!("readouts-{id}"))
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .group_hover(HEADER_GROUP, gpui::Styled::invisible)
            .children(readouts);
        let mut strip = div();
        // Gives way well before the title does and well after the place, down to its buttons.
        strip.style().flex_shrink = Some(STRIP_SHRINK);
        kit::tabular(
            strip
                .debug_selector(move || format!("strip-{id}"))
                .relative()
                .h_full()
                .min_w(px(buttons * kit::icon_button_side(theme) * k))
                .flex()
                .items_center()
                .justify_end(),
        )
        .child(readouts)
        .child(controls)
        .into_any_element()
    }

    /// Close `tile` as ⌘W closes the focused one: a shell asks first while its command runs,
    /// and the closing can be taken back.
    fn close_tile(&mut self, tile: TileRef, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_tile(tile, cx);
        self.close_item(&CloseItem, window, cx);
    }

    /// Start an exited shell again: the same command in the same directory on the same worker,
    /// in a column beside the old one, which goes.
    fn restart_session(&mut self, tile: TileRef, session: SessionId, cx: &mut Context<Self>) {
        let Some(summary) = self.summary(session) else { return };
        let cwd = summary.cwd.clone();
        let command = summary.command.clone();
        let title = command.first().cloned();
        self.focus_tile(tile, cx);
        self.open_session_on(tile.worker, cwd, command, title, cx);
        self.send(tile.worker, ClientMsg::Term { session, req: TermRequest::Close });
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
    }

    /// What the body cannot show and why, if anything: the worker out of reach, the shell
    /// exited, the session gone.
    pub(super) fn body_state(&self, tile: TileRef, item: &Item) -> Option<BodyState> {
        let worker = self.workers.get(&tile.worker);
        if worker.is_none_or(|w| w.link.is_none()) {
            let name = worker.map_or("The worker", |w| w.name.as_str());
            return Some(match worker.map(|w| &w.status) {
                Some(WorkerStatus::NeedsUpdate(notice)) => BodyState::NeedsUpdate(notice.clone()),
                Some(WorkerStatus::Unreachable) => {
                    BodyState::Away(format!("{name} is unreachable").into())
                }
                Some(WorkerStatus::Gone) => BodyState::Away(format!("{name} is gone").into()),
                _ => BodyState::Away(RECONNECTING.into()),
            });
        }
        let ItemKind::Terminal { session } = item.kind else { return None };
        match self.summary(session).map(|s| &s.state) {
            Some(SessionState::Exited { status }) => Some(BodyState::Exited(*status)),
            Some(SessionState::Running) => None,
            None if self.terminals.contains_key(&session) => None,
            None => Some(BodyState::Ended),
        }
    }

    /// The pill at the foot of a body saying what is wrong and what to do about it, over
    /// whatever the body still shows. Never a dialog: the rest of the workspace goes on.
    fn render_state_pill(
        &self,
        tile: TileRef,
        item: &Item,
        state: &BodyState,
        chrome: Chrome,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let id = tile.item;
        let update = update_state(state, cx);
        let status = update.as_ref().map_or_else(|| state.status(), |u| u.status);
        let text = update.as_ref().map_or_else(|| state.text(), |u| u.text.clone());
        let quiet = update.as_ref().is_some_and(|u| u.offered);
        let button = |part: &'static str, label: &'static str| {
            let tone = if quiet && part == "copy-command" { s.text_secondary } else { s.accent };
            let el = kit::pill_frame(theme, k)
                .id(part)
                .debug_selector(move || format!("{part}-{}", id.as_uuid()))
                .role(Role::Button)
                .aria_label(label)
                .flex_none()
                .text_color(hsla(tone))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .child(ChromeText::new(label, px(theme.typography.small()), k));
            tab_stop(el, s.accent)
        };
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        };
        let restart = match (state, session) {
            (BodyState::Exited(_), Some(session)) => Some(button("restart", "Restart").on_click(
                cx.listener(move |this, _ev, _w, cx| this.restart_session(tile, session, cx)),
            )),
            _ => None,
        };
        let close = matches!(state, BodyState::Exited(_) | BodyState::Ended).then(|| {
            button("close-ended", "Close").on_click(
                cx.listener(move |this, _ev, window, cx| this.close_tile(tile, window, cx)),
            )
        });
        let (detail, copy) = match state {
            BodyState::NeedsUpdate(notice) => {
                let command = notice.command();
                let running = update.as_ref().is_some_and(|u| u.bar.is_some());
                let copy = (!running).then(|| {
                    button("copy-command", COPY_COMMAND).on_click(move |_ev, _w, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()));
                    })
                });
                let said =
                    update.as_ref().map_or_else(|| Some(notice.detail()), |u| u.detail.clone());
                let detail = said.map(|said| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(hsla(s.text_muted))
                        .child(ChromeText::new(said, px(theme.typography.small()), k))
                });
                (detail, copy)
            }
            _ => (None, None),
        };
        // Offered only where the app can run `ssh`; again after a failure, never while it runs.
        let start = update.as_ref().and_then(|u| u.start.clone());
        let host = match state {
            BodyState::NeedsUpdate(notice) => notice.host.clone(),
            _ => String::new(),
        };
        let again = update.as_ref().is_some_and(|u| u.failed);
        let update_button = start.map(|start| {
            let label = if again { add_worker::TRY_AGAIN } else { add_worker::UPDATE };
            button("update-worker", label).on_click(move |_ev, window, cx| start(&host, window, cx))
        });
        let bar = update.as_ref().and_then(|u| u.bar).and_then(|bar| {
            add_worker::bar(theme, bar, "update-progress", cx).map(|bar| {
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(theme.spacing.md * k))
                    .right(px(theme.spacing.md * k))
                    .child(bar)
            })
        });
        let actions =
            restart.is_some() || close.is_some() || copy.is_some() || update_button.is_some();
        let pill = div()
            .id("state")
            .debug_selector(move || format!("state-{}", id.as_uuid()))
            .role(Role::Status)
            .aria_label(text.clone())
            .occlude()
            .max_w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .pl(px(theme.spacing.md * k))
            .pr(px(if actions { theme.spacing.xs } else { theme.spacing.md } * k))
            .py(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.md * k))
            .map(|el| kit::elevate(el, theme))
            .text_size(px(theme.typography.small() * k))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text_secondary))
            .child(
                crate::icons::icon(
                    theme,
                    status.icon(),
                    IconSize::Inline,
                    hsla(status.tone(theme)),
                )
                .size(px(theme.typography.icon() * k)),
            )
            .child(ChromeText::new(text, px(theme.typography.small()), k).zooming(chrome.zooming))
            .when_some(detail, gpui::ParentElement::child)
            .when_some(restart, gpui::ParentElement::child)
            .when_some(close, gpui::ParentElement::child)
            .when_some(update_button, gpui::ParentElement::child)
            .when_some(copy, gpui::ParentElement::child)
            .when_some(bar, |el, bar| el.relative().child(bar));
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(theme.spacing.lg * k))
            .flex()
            .justify_center()
            .child(pill)
            .into_any_element()
    }

    /// A file tile's proxy: its kind icon made draggable, as a Mac document window's title
    /// icon is. Dragged, the file leaves the app as a promise the worker keeps; the rest of the
    /// header still moves the tile.
    #[cfg(target_os = "macos")]
    fn file_proxy(
        &self,
        item: &Item,
        tile: TileRef,
        lead: gpui::AnyElement,
        k: f32,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let ItemKind::File { path } = &item.kind else { return lead };
        let theme = &self.theme;
        let id = item.id;
        let path = path.clone();
        let worker = tile.worker;
        div()
            .id("file-proxy")
            .debug_selector(move || format!("file-proxy-{}", id.as_uuid()))
            .role(Role::Button)
            .aria_label("Drag the file out")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(theme.typography.icon_large() * k))
            .rounded(px(theme.radii.xs * k))
            .hover(|s| s.bg(hsla(theme.surfaces.raised)))
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    this.drag_out(worker, &path);
                    cx.stop_propagation();
                }),
            )
            .child(lead)
            .into_any_element()
    }

    /// No file leaves the app by a drag here: the slot is only the slot.
    #[cfg(not(target_os = "macos"))]
    #[expect(clippy::unused_self, reason = "the macOS twin draws the proxy")]
    const fn file_proxy(
        &self,
        _item: &Item,
        _tile: TileRef,
        lead: gpui::AnyElement,
        _k: f32,
        _cx: &Context<Self>,
    ) -> gpui::AnyElement {
        lead
    }

    /// The body: its content and the state pill. A terminal's grid is sized from where its
    /// tile comes to rest, not from the rectangle in motion, so a sliding or springing column
    /// resizes no PTY frame by frame: the grid is laid out at the resting size and clipped to
    /// the moving one. Nothing lies over an unfocused body: its header says it is not focused.
    fn render_body(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let bare = chrome.k < SHAPES_BELOW;
        // A face held through a dropped link says the worker is away in its own composer.
        let held = match item.kind {
            ItemKind::Terminal { session } => self.faces.held.contains(&session),
            _ => false,
        };
        let state = self
            .body_state(placed.tile, item)
            .filter(|state| !bare && (!held || !matches!(state, BodyState::Away(_))));
        let content = self.render_content(placed, item, chrome, window, cx);
        if bare {
            return self.render_miniature(placed, item, content, cx);
        }
        let Some(state) = state else { return content };
        let pill = self.render_state_pill(placed.tile, item, &state, chrome, cx);
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .flex()
            .flex_col()
            .child(content)
            .child(pill)
            .into_any_element()
    }

    /// A body with nothing to show yet, saying why in one muted line: at once for a state
    /// that lasts (asleep, let go off screen), and only past [`crate::screen::LOADING_GRACE`]
    /// for one the worker is about to end (opening, reading, attaching), so a fast answer
    /// never flashes a word. Blank in the overview's shapes-only zoom.
    fn waiting_body(
        &self,
        item: &Item,
        wait: Wait,
        k: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let id = item.id;
        let (text, loading) = match wait {
            Wait::Lasting(text) => (text, false),
            Wait::Loading(text) => (text, true),
        };
        let grace = SharedString::from(format!("grace-{}", id.as_uuid()));
        let shown = k >= SHAPES_BELOW && (!loading || crate::screen::past_grace(grace, window, cx));
        div()
            .id(SharedString::from(format!("waiting-{}", id.as_uuid())))
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .when(shown, |el| {
                el.debug_selector(move || format!("waiting-{}", id.as_uuid()))
                    .role(Role::Status)
                    .aria_label(text.clone())
                    .text_size(px(theme.typography.small() * k))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .font_family(theme.typography.ui_family.clone())
                    .child(text)
            })
            .into_any_element()
    }

    /// A remote window or display on its way, past the loading grace: the calm mark, then
    /// "Opening Safari" and the worker under it, one composed block in the body's middle. The
    /// mark is the body's, not the header's, until the first frame: a sentence alone in the
    /// void with a spinner far above it read as two things waiting.
    fn opening_body(
        &self,
        tile: TileRef,
        item: &Item,
        k: f32,
        window: &mut Window,
        cx: &mut App,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = item.id;
        let what = self.derived_title(item, cx);
        let worker = self.workers.get(&tile.worker).map(|w| w.name.clone());
        let said = SharedString::from(match &worker {
            Some(worker) => format!("Opening {what} on {worker}…"),
            None => format!("Opening {what}…"),
        });
        let grace = SharedString::from(format!("grace-{}", id.as_uuid()));
        let shown = k >= SHAPES_BELOW && crate::screen::past_grace(grace, window, cx);
        let block = shown.then(|| {
            let mark = crate::icons::status_icon(
                theme,
                Status::Running,
                px(theme.typography.icon_large() * k),
                hsla(s.text_muted),
            );
            kit::notice(theme, k, mark, format!("Opening {what}"), worker.map(SharedString::from))
                .debug_selector(move || format!("waiting-{}", id.as_uuid()))
                .id("opening")
                .role(Role::Status)
                .aria_label(said)
        });
        div()
            .id(SharedString::from(format!("waiting-{}", id.as_uuid())))
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .children(block)
            .into_any_element()
    }

    /// What the body shows under any state pill: the view, or an empty well where the pill
    /// says why there is none. A terminal under a state pill leaves out its own lines-below
    /// pill, which would sit in the same place.
    fn render_content(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let worker_up = self.workers.get(&placed.tile.worker).is_some_and(|w| w.link.is_some());
        let rest_w = (placed.target.w * k).max(1.0);
        let rest_h = ((placed.target.h - theme.density.header) * k).max(1.0);
        let fixed = |el: gpui::AnyElement| {
            div()
                .flex_1()
                .w_full()
                .relative()
                .overflow_hidden()
                .child(div().absolute().top_0().left_0().w(px(rest_w)).h(px(rest_h)).child(el))
                .into_any_element()
        };
        // The state pill says what is wrong; the body under it stays empty.
        let well = || div().flex_1().w_full().into_any_element();
        match &item.kind {
            ItemKind::Terminal { session } => match self.terminals.get(session) {
                Some(_) if self.quick.holds(item.id) => {
                    let wait = Wait::Lasting(super::quick::IN_QUICK_TERMINAL.into());
                    div()
                        .id(SharedString::from(format!("quick-{}", item.id.as_uuid())))
                        .flex_1()
                        .w_full()
                        .flex()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.show_quick_terminal(std::time::Instant::now(), cx);
                        }))
                        .child(self.waiting_body(item, wait, k, window, cx))
                        .into_any_element()
                }
                _ if self.faces.held.contains(session)
                    || (self.terminals.contains_key(session)
                        && self.face_shown(*session)
                        && self.body_state(placed.tile, item).is_none()) =>
                {
                    let Some(face) = self.faces.views.get(session) else { return well() };
                    face.update(cx, |v, cx| v.set_layout(k, placed.target.w, cx));
                    let body = if self.cacheable(placed, window, cx) {
                        face.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        face.clone().into_any_element()
                    };
                    fixed(body)
                }
                Some(view) => {
                    let covered = self.body_state(placed.tile, item).is_some();
                    let restyled = view.update(cx, |v, _| {
                        v.set_zoom(k);
                        let covered = v.set_covered(covered);
                        v.set_zooming(chrome.zooming) || covered
                    });
                    let body = if self.cacheable(placed, window, cx) && !restyled {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    fixed(body)
                }
                None if !worker_up => well(),
                None if self.summary(*session).is_some() => {
                    self.waiting_body(item, Wait::Loading(ATTACHING.into()), k, window, cx)
                }
                None => well(),
            },
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                match self.screens.get(&item.id) {
                    Some(_) if self.popouts.holds(item.id) => {
                        let id = item.id;
                        let wait = Wait::Lasting(super::popout::IN_OWN_WINDOW.into());
                        div()
                            .id(SharedString::from(format!("popped-{}", id.as_uuid())))
                            .flex_1()
                            .w_full()
                            .flex()
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.raise_popped(id, cx);
                            }))
                            .child(self.waiting_body(item, wait, k, window, cx))
                            .into_any_element()
                    }
                    Some(view) => {
                        let painted = placed.rect.w * window.scale_factor();
                        view.update(cx, |v, cx| v.set_painted_width(painted, cx));
                        let body = if self.cacheable(placed, window, cx) {
                            view.clone()
                                .cached(StyleRefinement::default().size_full())
                                .into_any_element()
                        } else {
                            view.clone().into_any_element()
                        };
                        div().flex_1().w_full().overflow_hidden().child(body).into_any_element()
                    }
                    None if !worker_up => well(),
                    None if item.sleeping => {
                        self.waiting_body(item, Wait::Lasting(SLEEPING.into()), k, window, cx)
                    }
                    None if self.parked.contains(&item.id) => {
                        self.waiting_body(item, Wait::Lasting(PAUSED.into()), k, window, cx)
                    }
                    None => self.opening_body(placed.tile, item, k, window, cx),
                }
            }
            ItemKind::Note { .. } => match self.notes.get(&item.id) {
                Some(view) => {
                    let (pad, text_size) = (theme.spacing.inset(), theme.typography.ui_size);
                    view.update(cx, |v, cx| v.set_layout(k, pad, text_size, cx));
                    // Cached as a file tile is: a note's Markdown is laid out again only when
                    // the note changes, not on every frame a shell or a stream draws. Focused,
                    // the keyboard is in its editor, which the note watches.
                    let body = if self.cacheable(placed, window, cx) {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    div()
                        .flex_1()
                        .w_full()
                        .overflow_hidden()
                        .font_family(theme.typography.ui_family.clone())
                        .text_color(hsla(theme.surfaces.text))
                        .child(body)
                        .into_any_element()
                }
                None => self.waiting_body(item, Wait::Loading(NOTE.into()), k, window, cx),
            },
            ItemKind::Browser { .. } => match self.browsers.get(&item.id) {
                Some(view) => {
                    let frame = self.frames_drawn;
                    view.update(cx, |v, _| v.set_drawn(placed.alpha, frame));
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(view.clone())
                        .into_any_element()
                }
                None => self.waiting_body(item, Wait::Loading(OPENING.into()), k, window, cx),
            },
            ItemKind::File { .. } => match self.files.get(&item.id) {
                Some(view) => {
                    let (pad, text_size) = (theme.spacing.inset(), theme.typography.mono_size);
                    view.update(cx, |v, _| v.set_layout(k, pad, text_size));
                    let body = if self.cacheable(placed, window, cx) {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body)
                        .into_any_element()
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(READING.into()), k, window, cx),
            },
            ItemKind::Folder { .. } => match self.folders.get(&item.id) {
                Some(view) => {
                    view.update(cx, |v, _| v.set_zoom(k));
                    let body = if self.cacheable(placed, window, cx) {
                        view.clone()
                            .cached(StyleRefinement::default().size_full())
                            .into_any_element()
                    } else {
                        view.clone().into_any_element()
                    };
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body)
                        .into_any_element()
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(READING.into()), k, window, cx),
            },
        }
    }
}

/// Where a shell is, beside a title that may already name the directory it stands in: then
/// nothing, since the status bar has the whole path. The directory above it read as the cwd:
/// "drop-here ~" beside a prompt in "~/drop-here".
pub(super) fn place_beside(place: String, title: &str) -> Option<String> {
    let last = place.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    (last != title && place != title).then_some(place)
}

/// A header action in words ("Take", "Mute", an upload's progress): the bare
/// [`kit::pill_frame`], its words in its tone, and the `raised` fill under the pointer, so it
/// stands as tall as the state's pill beside it.
/// A header holds one filled chip at most, the state's (the agent's pill); every other word
/// in it is a ghost, so the state is the one shape that stands out. Scaled by the chrome's
/// `k`. Its id is scoped by the tile's.
fn pill(
    part: impl Into<SharedString>,
    item: ItemId,
    label: impl Into<SharedString>,
    tone: slopty_theme::Rgb,
    theme: &Theme,
    chrome: Chrome,
) -> Stateful<Div> {
    let k = chrome.k;
    let (raised, pressed) = (theme.surfaces.raised, theme.surfaces.overlay);
    let part: SharedString = part.into();
    let selector = format!("{part}-{}", item.as_uuid());
    kit::pill_frame(theme, k)
        .id(part)
        .debug_selector(move || selector)
        .flex_none()
        .text_color(hsla(tone))
        .cursor_pointer()
        .hover(move |el| el.bg(hsla(raised)))
        .active(move |el| el.bg(hsla(pressed)))
        .child(ChromeText::new(label, px(theme.typography.small()), k).zooming(chrome.zooming))
}
