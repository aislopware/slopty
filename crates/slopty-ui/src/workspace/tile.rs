//! One tile: the header (kind, title, place, status, the actions shown on hover or focus) and
//! the body (a terminal, a remote window or display, a file tile), with the pill that
//! says when the body cannot show what it should.

use std::collections::HashMap;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, App, AppContext as _, Context, Div, ElementId, Entity, ExternalPaths,
    FontWeight, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, Render, SharedString, Stateful, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Window, div, px,
};
use gpui_kit::component::input::Input;
use slopty_client::layout::{Placed, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::Worktree;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::terminal::{SessionState, SessionSummary, TermRequest};
use slopty_proto::thread::{AgentId, ThreadId};
use slopty_theme::{Theme, Typography};

use super::actions::{AddWindow, CloseItem};
use super::browsers::ADDRESS;
use super::context_menus::Pressed;
use super::faces::{Face, ThreadStand};
use super::strip::Handed;
use super::{Field, MenuRun, WorkerStatus, WorkspaceView};
use crate::a11y::tab_stop;
use crate::browser::BrowserView;
use crate::chrome_text::ChromeText;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::folder::FolderView;
use crate::icons::{GitGlyph, IconSize, Mark, Status, Symbol};
use crate::{add_worker, kit};

/// Below this zoom the overview draws a tile as its miniature: the header's surface without its
/// words, the body as it stands at the zoom, and a label at chrome size under it that names the
/// tile (`miniature.rs`).
pub(super) const SHAPES_BELOW: f32 = 0.5;

/// A page's way back or forward.
type Go = fn(&mut BrowserView, &mut Context<BrowserView>);

/// The open fact a thread's row names its branch by.
const BRANCH_FACT: &str = "branch";

/// Where a thread's agent works, for its tile's header ([`WorkspaceView::header_place`]).
#[derive(Clone, Debug)]
struct HeaderPlace {
    /// The checkout's folder name.
    checkout: Option<String>,
    /// Its branch.
    branch: Option<String>,
    /// The checkout is a repository, so a press opens the commit sheet.
    commits: bool,
}

/// A pull request's place in a header: it outlasts the checkout and the branch beside it.
const PR_PRIORITY: kit::Priority = kit::Priority(kit::Priority::MEDIUM.0 + 16);

/// The widest a page's address gets beside its title, in points at zoom 1: the title is what
/// tells tiles apart, the address only says where.
const HEADER_URL_MAX: f32 = 180.0;

/// What a file's header says after its name while it holds an edit not yet on disk.
pub(crate) const EDITED: &str = "Edited";

/// The width of a program's progress bar (`OSC 9;4`) in its terminal's header, beside its
/// figure.
const PROGRESS_W: f32 = 48.0;

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

/// The upload pill's hover group, which turns its ring into "×".
const UPLOAD_GROUP: &str = "upload";

/// Where files dropped on an agent's tile go.
pub(super) const ATTACH_TO_MESSAGE: &str = "Attach to the message";

/// What the upload pill does when pressed, as it says.
pub(super) const STOP_UPLOAD: &str = "Stop upload";

/// What a single tile's header is made of, gathered by `render_header` for `header_row`.
struct HeaderParts<'a> {
    placed: &'a Placed,
    item: &'a Item,
    title: String,
    chrome: Chrome,
    /// Where it is, beside the title.
    place: Option<gpui::AnyElement>,
    /// The title is the page's address, which a click edits.
    address_title: bool,
    worker: Option<Stateful<Div>>,
    unsaved: Option<Stateful<Div>>,
    /// An agent's pull request and worktree, each with how much it is needed.
    branch: Vec<(&'static str, kit::Priority, gpui::AnyElement)>,
    upload: Option<gpui::AnyElement>,
    readouts: Vec<(&'static str, kit::Priority, gpui::AnyElement)>,
    /// What the kind says of how it stands, at rest: a size another client rules, a stream's
    /// health, the system's keys sent on.
    states: Option<Div>,
    /// The kind's own actions, under the pointer only.
    actions: Vec<gpui::AnyElement>,
    silenced: Option<gpui::AnyElement>,
    /// The face toggle, or a Markdown file's preview toggle.
    face: Option<gpui::AnyElement>,
}

/// How much faster a header's place shrinks than its title.
const PLACE_SHRINK: f32 = 1000.0;
/// How much faster than the title a header's readouts give way: an agent's pill shortens to
/// an ellipsis while the title still reads whole.
const STRIP_SHRINK: f32 = 20.0;

/// What the in-body pill says while a tile's worker is being dialled again.
pub const RECONNECTING: &str = "Reconnecting…";

/// How long a worker is out of reach before its tiles say for how long, and the step that
/// count moves in: "Reconnecting for 20 s", "Reconnecting for 1m 10s".
pub const AWAY_STEP: Duration = Duration::from_secs(10);

/// What a tile says while its worker is dialled again, `away` after it went: the one word
/// first, then for how long once that is worth saying, in [`AWAY_STEP`]s ([`kit::duration`]'s
/// one form), whole minutes past an hour.
#[must_use]
pub fn reconnecting(away: Duration) -> String {
    let secs = away.as_secs();
    let step = AWAY_STEP.as_secs();
    if secs < step {
        return RECONNECTING.to_owned();
    }
    let grain = if secs < 3_600 { step } else { 60 };
    let floored = secs.saturating_sub(secs.checked_rem(grain).unwrap_or_default());
    format!("Reconnecting for {}", kit::duration(Duration::from_secs(floored)))
}

/// What the in-body pill says for a shell whose session is gone and whose status is not known.
pub const SESSION_ENDED: &str = "Session ended";

/// The in-body pill's action for a worker on another build: the command that updates it, to
/// the clipboard.
pub const COPY_COMMAND: &str = "Copy command";

/// The accessible name of a tile's close button.
pub const CLOSE_TILE: &str = "Close tile";

/// The accessible name of the [`TAKE`] pill.
pub const TAKE_OVER: &str = "Take over";

/// The pill that takes a PTY's size from the client driving it.
pub const TAKE: &str = "Take";
/// A remote window's audio toggle: one name, pressed while this client has silenced it. Its
/// accessible name and its hint say whose sound it is ([`mute_label`]).
pub const MUTE: &str = "Mute";

/// What the audio toggle silences: the sound of `worker`, which every one of its tiles plays.
#[must_use]
pub(super) fn mute_label(worker: &str) -> String {
    if worker.is_empty() {
        format!("{MUTE} the machine's sound")
    } else {
        format!("{MUTE} {worker}'s sound")
    }
}
/// A shell's body while its view attaches.
pub const ATTACHING: &str = "Attaching…";
/// A window or display whose stream was let go while it was off screen.
pub const PAUSED: &str = "Paused off screen";
/// A stream or a page on its way.
pub const OPENING: &str = "Opening…";
/// A file tile waiting for its text.
pub const READING: &str = crate::file::READING;
/// What a review tile's header says, and its body while the review is on its way.
pub const REVIEW: &str = "Review";
/// What a folder's changes tile's header says, and its body while they are on their way.
pub const CHANGES: &str = "Changes";

/// How the chrome is scaled this frame: `k`, the overview's zoom, and whether that zoom is
/// in motion (chrome text then paints from the raster ladder).
#[derive(Clone, Copy, Debug)]
pub(super) struct Chrome {
    pub k: f32,
    pub zooming: bool,
}

/// What a body with nothing to show yet says, and when.
enum Wait {
    /// A state that lasts (let go off screen): said at once.
    Lasting(SharedString),
    /// A wait the worker is about to end (opening, reading, attaching): said only past the
    /// loading grace.
    Loading(SharedString),
}

/// What a pane says of an open that failed: its title, why, and its way to pick another.
/// A window or display that went is named for what it is, the rest by the failure's own words
/// ([`crate::screen::failure_text`]).
fn failed_words(
    item: &Item,
    what: &str,
    why: &slopty_proto::screen::ScreenFailure,
    machine: &str,
) -> (String, String, &'static str) {
    use slopty_proto::screen::ScreenFailure;
    let display = matches!(item.kind, ItemKind::Display { .. });
    let pick = if display { "Choose another display" } else { "Choose another window" };
    let (title, detail) = match why {
        ScreenFailure::Gone if display => (
            "Display is no longer available".to_owned(),
            format!("{what} is not connected to {machine} any more."),
        ),
        ScreenFailure::Gone => (
            "Window is no longer available".to_owned(),
            format!("{what} is not open on {machine} any more."),
        ),
        other => (format!("{what} did not open"), crate::screen::failure_text(other, machine)),
    };
    (title, detail, pick)
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

/// Where a shell is, short: the last two components of `path`, the home directory as `~`.
///
/// `~`, `~/src`, `oss/slopty`. `home` is the worker's, once it has said; until then a home is
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

/// A typed command as it names its shell: its first line, without the `cd <dir> &&` (or
/// `cd <dir>;`) it may lead with, since the shell's place already says where it runs.
/// `cd ~/srv/atlas && docker compose pull` is "docker compose pull"; a bare `cd ~/srv` stays.
#[must_use]
pub fn command_words(command: &str) -> &str {
    let mut rest = command.lines().next().unwrap_or_default().trim();
    while let Some(after) = rest.strip_prefix("cd").filter(|a| a.starts_with([' ', '\t'])) {
        let after = after.trim_start();
        let dir = shell_word(after);
        let tail = after.get(dir..).unwrap_or_default().trim_start();
        let Some(next) = tail.strip_prefix("&&").or_else(|| tail.strip_prefix(';')) else {
            break;
        };
        let next = next.trim_start();
        if next.is_empty() || dir == 0 {
            break;
        }
        rest = next;
    }
    rest
}

/// Whether `command` only moves the shell (`cd ~/srv`, a bare `cd`): its place says where to.
#[must_use]
pub fn only_moves(command: &str) -> bool {
    let words = command_words(command);
    match words.strip_prefix("cd") {
        Some("") => true,
        Some(after) if after.starts_with([' ', '\t']) => {
            let dir = after.trim_start();
            shell_word(dir) == dir.len()
        }
        _ => false,
    }
}

/// How many bytes the shell word at the start of `text` takes: quoted runs and a backslash's
/// character are part of it, and it ends at an unquoted blank, `;` or `&`.
fn shell_word(text: &str) -> usize {
    let mut quote = None;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            _ if escaped => escaped = false,
            (None | Some('"'), '\\') => escaped = true,
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, ' ' | '\t' | ';' | '&') => return at,
            _ => {}
        }
    }
    text.len()
}

/// What a shell is called with nothing better to say: no command, no title of its program's, no
/// place but home.
pub const TERMINAL: &str = "Terminal";

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

/// Where a shell is, as a header and its navigator row say it: inside a repository, the
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

/// What the away pill offers ([`WorkspaceView::away_actions`]).
#[derive(Default)]
struct AwayActions {
    grant: Option<SharedString>,
    wake: Option<MenuRun>,
    retry: Option<MenuRun>,
}

/// The away pill's button that copies the tailnet grant.
pub const COPY_GRANT: &str = "Copy grant";
/// What the app says once the grant is on the clipboard.
pub const GRANT_COPIED: &str =
    "Copied the tailnet grant: add it to Tailscale's access controls to let this device in";

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
const UPDATING: &str = "Updating Slopty on the machine";

impl WorkspaceView {
    /// The way to update `key` where it runs a different build and the app can run one now (not
    /// while one is under way): what the navigator's row and the hosts popover offer as Update,
    /// as its tiles' pills do.
    pub(super) fn update_run(&self, key: WorkerKey, cx: &App) -> Option<MenuRun> {
        let WorkerStatus::NeedsUpdate(notice) = &self.workers.get(&key)?.status else {
            return None;
        };
        let updates = cx.try_global::<add_worker::Updates>()?;
        if updates.runs.get(&notice.host).is_some_and(|run| run.failed.is_none()) {
            return None;
        }
        let start = updates.start.clone()?;
        let host = notice.host.clone();
        Some(std::rc::Rc::new(move |window, cx| start(&host, window, cx)))
    }
}

/// What a tile's header leads with: what the tile is. A file shows its type; a terminal an
/// agent runs in and a thread show the mark of their agent (`agent`, an `AgentId`'s name), and
/// a thread whose agent is not yet known the neutral glyph.
pub(super) fn kind_icon(item: &Item, agent: Option<&str>) -> Mark {
    let runs = matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::Thread { .. });
    if let Some(agent) = agent.filter(|_| runs) {
        return Mark::agent(agent);
    }
    let symbol = match &item.kind {
        ItemKind::Terminal { .. } => Symbol::Terminal,
        ItemKind::Thread { .. } => crate::icons::AGENT,
        ItemKind::Window { .. } => Symbol::Macwindow,
        ItemKind::Display { .. } => Symbol::Display,
        ItemKind::File { path } => crate::icons::file_symbol(path),
        ItemKind::Folder { .. } => Symbol::Folder,
        ItemKind::Browser { .. } => Symbol::Globe,
        ItemKind::Review { .. } | ItemKind::Changes { .. } => Symbol::PlusForwardslashMinus,
    };
    symbol.into()
}

/// The word for what an item is: `terminal`, `window`, `display`, `file`, `folder`,
/// `browser`, `review`, `thread`, `changes`.
pub(super) const fn kind_name(item: &Item) -> &'static str {
    match item.kind {
        ItemKind::Terminal { .. } => "terminal",
        ItemKind::Window { .. } => "window",
        ItemKind::Display { .. } => "display",
        ItemKind::File { .. } => "file",
        ItemKind::Folder { .. } => "folder",
        ItemKind::Browser { .. } => "browser",
        ItemKind::Review { .. } => "review",
        ItemKind::Thread { .. } => "thread",
        ItemKind::Changes { .. } => "changes",
    }
}

/// A tile's heading as it is spoken: its kind or agent, then its title, the lead left out where
/// the title already says it ("Claude Code 2", a twin, not "Claude Code Claude Code 2").
pub(super) fn spoken_heading(kind: &str, title: &str) -> String {
    let said = title == kind || title.strip_prefix(kind).is_some_and(|rest| rest.starts_with(' '));
    if said { title.to_owned() } else { format!("{kind} {title}") }
}

impl WorkspaceView {
    /// The word a tile's spoken heading leads with: the agent's name where the tile wears its
    /// mark ([`kind_icon`]), as the person sees it, else its kind ([`kind_name`]). Only what is
    /// said changes: twins are numbered, and tiles grouped, by their kind.
    pub(super) fn spoken_kind(&self, item: &Item) -> String {
        let runs = matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::Thread { .. });
        match self.item_agent(item).filter(|_| runs) {
            Some(agent) => crate::conversation::thread::view::agent_label(&AgentId::named(agent)),
            None => kind_name(item).to_owned(),
        }
    }

    /// What a tile's title is placed by, the same in its header, its navigator row and its
    /// palette line: where a shell is, the folder a file is in, a page's address when the
    /// title is not it. A window or display has none. Kept with
    /// the titles ([`Self::number_twins`]).
    pub(super) fn tile_place(&self, item: &Item) -> Option<String> {
        match self.places.get(&item.id) {
            Some(place) => place.clone(),
            None => self.derived_place(item, &self.derived_title(item)),
        }
    }

    /// [`Self::tile_place`] worked out, for an item whose derived title is `title`: where it
    /// is. An agent with a mark of its own is not named here: the mark leads the title and
    /// names it to a screen reader. One that wears the neutral glyph is named first, in words
    /// ("opencode · code/atlas"), unless the title already is its name.
    fn derived_place(&self, item: &Item, title: &str) -> Option<String> {
        let unmarked = self
            .item_agent(item)
            .filter(|agent| crate::icons::AgentMark::of(agent) == crate::icons::AgentMark::Neutral)
            .map(|agent| super::projects::agent_label(&AgentId::named(agent)))
            .filter(|name| name != title);
        let place = self.derived_where(item, title);
        match (unmarked, place) {
            (Some(name), Some(place)) => {
                Some(format!("{name}{}{place}", super::rollup::META_SEPARATOR))
            }
            (name, place) => name.or(place),
        }
    }

    /// Where the item is, for [`Self::derived_place`].
    fn derived_where(&self, item: &Item, title: &str) -> Option<String> {
        match &item.kind {
            ItemKind::Terminal { session } => self.shell_context(*session, title),
            ItemKind::File { path } | ItemKind::Folder { path } => file_dir(path),
            // The folder whose changes they are: the title says what they are.
            ItemKind::Changes { path, .. } => Some(folder_title(path)),
            // The host alone: the path is the page's business, and the title names the page.
            ItemKind::Browser { .. } => self.page_facts(item.id).and_then(|page| {
                let host = page.short_url.split('/').next().unwrap_or_default();
                (!host.is_empty() && (item.name.is_some() || page.titled)).then(|| host.to_owned())
            }),
            ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Review { .. }
            | ItemKind::Thread { .. } => None,
        }
    }

    /// What a header says without a name: the shell's title, the window's, "Display N", a
    /// file's `name · parent`.
    pub(super) fn derived_title(&self, item: &Item) -> String {
        match &item.kind {
            ItemKind::Terminal { session } => self.terminal_title(*session),
            ItemKind::Window { window } => {
                self.titles.get(&item.id).cloned().unwrap_or_else(|| format!("Window {}", window.0))
            }
            ItemKind::Display { display } => format!("Display {}", display.0),
            ItemKind::File { path } => file_title(path),
            ItemKind::Folder { path } => folder_title(path),
            ItemKind::Browser { url } => self
                .page_facts(item.id)
                .map_or_else(|| crate::browser::short_url(url).to_owned(), |p| p.title.clone()),
            ItemKind::Review { .. } => REVIEW.to_owned(),
            ItemKind::Changes { .. } => CHANGES.to_owned(),
            ItemKind::Thread { thread } => self.thread_title(*thread),
        }
    }

    /// What a header says: the name the human gave the tile, else its derived title, as it
    /// was last worked out when the workspace changed.
    #[must_use]
    pub fn tile_title(&self, item: &Item) -> String {
        if let Some(name) = &item.name {
            return name.clone();
        }
        let derived = self.derived.get(&item.id);
        let title = derived.map_or_else(|| self.derived_title(item), String::clone);
        match self.twins.get(&item.id) {
            Some(n) => format!("{title} {n}"),
            None => title,
        }
    }

    /// Unnamed tiles of one worker and one kind that would read alike ("Terminal" and
    /// "Terminal") are told apart: shells by the command each last ran, then any still alike by
    /// a number after the first, in the order they were made: the second is "Terminal 2". A named
    /// tile keeps the name it was given, and tiles of two kinds are told apart by their icons:
    /// a shell and a folder at one directory both read as it.
    ///
    /// Worked out when a title may have changed ([`Self::titles_dirty`]), not once a frame: it
    /// derives every item's title. Every item's derived title and place are kept with the
    /// numbers, for every header, row and line to read, and so that a program's new title that
    /// leaves its tile's as it was is nobody's news ([`Self::retitled`]).
    pub(super) fn number_twins(&mut self) {
        let mut derived: HashMap<ItemId, String> =
            self.items().map(|(_, item)| (item.id, self.derived_title(item))).collect();
        // Shells that would read alike by their place ("atlas", "atlas 2", "atlas 3") are told
        // apart first by the command each last ran, which says what each is for; a number
        // tells apart only those that still read alike.
        let mut alike: HashMap<(WorkerKey, &str), u32> = HashMap::new();
        for (worker, item) in self.items().filter(|(_, i)| i.name.is_none()) {
            if let (ItemKind::Terminal { .. }, Some(title)) = (&item.kind, derived.get(&item.id)) {
                let count = alike.entry((worker, title.as_str())).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
        let by_command: Vec<(ItemId, String)> = self
            .items()
            .filter(|(_, i)| i.name.is_none())
            .filter_map(|(worker, item)| {
                let ItemKind::Terminal { session } = item.kind else { return None };
                let title = derived.get(&item.id)?;
                let shared = alike.get(&(worker, title.as_str())).is_some_and(|n| *n > 1);
                shared.then(|| self.last_command(session)).flatten().map(|c| (item.id, c))
            })
            .collect();
        drop(alike);
        derived.extend(by_command);
        let places = self
            .items()
            .map(|(_, item)| {
                let title = derived.get(&item.id).map_or("", String::as_str);
                (item.id, self.derived_place(item, title))
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
        let title = self.derived_title(item);
        if self.derived.get(&item.id) != Some(&title) {
            cx.notify();
        }
    }

    /// A terminal's title, the first that says something: the command it runs, a title its
    /// program set (not the shell's own name, a path or a prompt), the repository or
    /// directory it stands in, else "Terminal". An agent's shell takes its part in a project
    /// (the orchestrator, a task), else the agent's own title, else the agent's name.
    #[must_use]
    pub fn terminal_title(&self, session: SessionId) -> String {
        let shell = self.shell(session);
        let summary = self.session_on(session);
        let program = summary.and_then(|(_, s)| s.command.first()).map(String::as_str);
        let set = shell
            .and_then(|s| s.title.as_deref())
            .or_else(|| summary.map(|(_, s)| s.title.as_str()))
            .map(str::trim)
            .filter(|t| own_title(t, program));
        // An agent on a project is named by what it is there: the orchestrator, or its task.
        // Any other that has not titled itself is named by what its thread is about, once its
        // worker's table says: the kind's glyph already says "Claude".
        if self.agent_state(session).is_some() {
            let named = self
                .project_role(session)
                .or_else(|| set.map(str::to_owned))
                .or_else(|| self.session_thread(session).and_then(|t| self.thread_named(t)))
                .or_else(|| {
                    let agent = AgentId::named(self.session_agent(session)?);
                    Some(super::projects::agent_label(&agent))
                });
            if let Some(named) = named {
                return named;
            }
        }
        let running =
            shell.and_then(|s| s.running.as_deref()).map(command_words).filter(|c| !c.is_empty());
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

    /// How tall a tile's header is: none on a phone, whose bar above the strip is the focused
    /// tile's (its mark, its title, and its rows in "…"), so the screen keeps one bar of chrome.
    pub(super) const fn header_h(&self) -> f32 {
        if self.phone { 0.0 } else { self.theme.density.header }
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
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let tile = placed.tile;
        let Some(item) = self.item(tile) else {
            return self
                .render_starting(placed, chrome, cx)
                .or_else(|| self.render_missing(placed, chrome, cx));
        };
        let theme = &self.theme;
        let id = item.id;
        let worker_up = self.workers.get(&tile.worker).is_some_and(|w| w.link.is_some());

        let title = self.tile_title(item);
        let label = SharedString::from(title.clone());
        let shows = self.tile_shows(item);
        // A phone's bar is the focused tile's ([`Self::header_h`]): no header of its own.
        let header = (!self.phone).then(|| self.render_header(placed, item, title, chrome, cx));
        let body = self.render_body(placed, item, chrome, window, cx);
        // Files dropped on a shell go to its directory; on a thread, to its composer; on a
        // remote window, to the worker's clipboard; on a folder, into it.
        // On the Mac a drag over a remote window or display is the worker's own drag, carried
        // to the point under it (`remote::DropIn`), not a file drop.
        let takes_files = worker_up
            && match item.kind {
                ItemKind::Terminal { .. } | ItemKind::Folder { .. } | ItemKind::Thread { .. } => {
                    true
                }
                ItemKind::Window { .. } | ItemKind::Display { .. } => !cfg!(target_os = "macos"),
                _ => false,
            };

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
                .when_some(shows, gpui::StatefulInteractiveElement::aria_description)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .opacity(placed.alpha)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _ev, _w, cx| this.click_tile(tile, cx)),
                )
                .when(takes_files, |el| {
                    el.on_drag_move(cx.listener(
                        move |this, ev: &gpui::DragMoveEvent<ExternalPaths>, _w, cx| {
                            // Over it, it is this tile; leaving it, none, unless another
                            // tile already took the drag.
                            let over = ev.bounds.contains(&ev.event.position);
                            let was = this.files_over == Some(tile);
                            if over != was {
                                this.files_over = over.then_some(tile);
                                cx.notify();
                            }
                        },
                    ))
                    .on_drop(cx.listener(
                        move |this, paths: &ExternalPaths, _w, cx| {
                            this.files_over = None;
                            this.drop_files(tile, paths.paths(), cx);
                        },
                    ))
                })
                .map(|el| {
                    let inside = div().flex().flex_col().children(header).child(body).children(
                        (takes_files && self.files_over == Some(tile) && cx.has_active_drag())
                            .then(|| self.drop_overlay(tile, item, chrome)),
                    );
                    kit::panel(el, theme, self.stand(chrome.k), inside)
                })
                .into_any_element(),
        )
    }

    /// What a file dragged over the tile shows while it is over it: a wash inset from the body
    /// and one centred line saying where the file will land, as [`Self::drop_files`] takes it,
    /// which a screen reader hears as it arrives.
    fn drop_overlay(&self, tile: TileRef, item: &Item, chrome: Chrome) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let id = item.id;
        let words = self.drop_words(tile, item);
        let inset = theme.spacing.sm * k;
        div()
            .id(SharedString::from(format!("drop-overlay-{}", id.as_uuid())))
            .debug_selector(move || format!("drop-overlay-{}", id.as_uuid()))
            .role(Role::Status)
            .aria_label(SharedString::from(words.clone()))
            .absolute()
            .left(px(inset))
            .right(px(inset))
            .bottom(px(inset))
            .top(px(self.header_h().mul_add(k, inset)))
            .flex()
            .items_center()
            .justify_center()
            .px(px(theme.spacing.md * k))
            .rounded(px(theme.radii.md * k))
            .bg(crate::colors::hsla_alpha(s.accent_fill, slopty_theme::alpha::FAINT))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(s.text))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(words)),
            )
            .into_any_element()
    }

    /// Where files dropped on `item` go, in a line: a shell's directory on its machine, a
    /// folder, the message an agent's composer is writing, or a remote window.
    pub(super) fn drop_words(&self, tile: TileRef, item: &Item) -> String {
        let machine = self.worker_name(tile.worker);
        let home = self.home_of(tile.worker);
        let to =
            |place: String| format!("Upload to {machine}{}{place}", super::rollup::META_SEPARATOR);
        match &item.kind {
            ItemKind::Terminal { session } if self.shown_composer(*session).is_some() => {
                ATTACH_TO_MESSAGE.to_owned()
            }
            ItemKind::Thread { .. } => ATTACH_TO_MESSAGE.to_owned(),
            ItemKind::Terminal { session } => {
                self.session_tail(*session).map_or_else(|| format!("Upload to {machine}"), to)
            }
            ItemKind::Folder { path } => to(cwd_tail(path, home)),
            _ => format!("Drop on {}", self.tile_title(item)),
        }
    }

    /// Whether a tile's body may be drawn from its cached view: not in the frame the strip's
    /// focus comes to its tile or leaves it, which the body is laid out by. The keyboard moving
    /// needs nothing here. A view asks for its focus through its handle (its input handler, a
    /// caret, a focus ring), and GPUI builds again exactly the views whose answer changed, so a
    /// shell the keyboard left for the rename field in its header is built again without its
    /// input handler, and no other body is. Otherwise the focused body is replayed too:
    /// whatever has the keyboard in it is the view or a field the view renders as an entity
    /// (gpui-kit's inputs and textareas are views), and a field's notify dirties every view
    /// above it. So another tile's frame (a stream, a flood, an animation step) never draws it
    /// again, and every key, caret blink and selection does.
    fn cacheable(&self, placed: &Placed) -> bool {
        placed.focused == (self.drawn.focus.get() == Some(placed.tile))
    }

    /// A body's view as the strip draws it: from its cached drawing unless it is not
    /// [`Self::cacheable`]. Then it is built again in this frame: the strip's focus moved,
    /// which tells no view.
    fn body_view<V: Render>(
        &self,
        view: &Entity<V>,
        placed: &Placed,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        if self.cacheable(placed) {
            return view.clone().cached(StyleRefinement::default().size_full()).into_any_element();
        }
        let id = view.entity_id();
        cx.later(move |_window, cx| cx.notify(id));
        view.clone().into_any_element()
    }

    /// A tile fading out where it stood, over a fade: its panel and its header's glyph and
    /// title as they were, the content already gone. A panel at any zoom, as every tile is,
    /// and never scaled: text does not shrink on its way out. Nothing where chrome does not
    /// move.
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
        let k = chrome.k;
        let rect = self.panel_rect(closing.rect, k);
        let id = closing.tile.item;
        let was = self.closed.iter().rev().find(|c| c.tile == closing.tile).map(|c| &c.item);
        let header = was.filter(|_| k >= SHAPES_BELOW).map(|item| {
            let lead = hsla(s.text_secondary);
            div()
                .h(px(theme.density.header * k))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm * k))
                .px(px(theme.spacing.inset() * k))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(theme.typography.ui_size * k))
                .text_color(hsla(s.text_secondary))
                .font_family(theme.typography.ui_family.clone())
                .child(crate::palette::lead_slot(theme, self.kind_glyph(item), lead, k))
                .child(SharedString::from(self.tile_title(item)))
        });
        let ghost = div()
            .debug_selector(move || format!("closing-{}", id.as_uuid()))
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h));
        let ghost =
            kit::panel(ghost, theme, self.stand(k), div().flex().flex_col().children(header));
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
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tile = placed.tile;
        let id = item.id;
        let k = chrome.k;
        let focused = placed.focused;
        let kind = self.spoken_kind(item);
        let agent = match item.kind {
            ItemKind::Terminal { session } => self.agent_state(session).map(|a| (session, a)),
            _ => None,
        };
        let ink = title_ink(theme, focused);
        let heading = SharedString::from(spoken_heading(&kind, &title));
        // Every header lies inside its panel's top, on the panel's own surface with nothing
        // between it and the body, focused or not, so a tile reads as one piece and the strip
        // as panels, not as rows of bands. A page or a remote picture meets the header on
        // its own edge, with no rule between them: a rule there was the canvas's last divider.
        // At the overview's small zoom the miniature's label names the tile; a band on top of it in
        // another step, and a hairline under only some of them, read as tiles half drawn.
        let shapes = k < SHAPES_BELOW;
        let fills = self.offers_fullscreen(placed);
        let header = Self::tile_menu_press(div().id("title"), tile, Pressed::Header, cx)
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _window, cx| {
                    // The second click of a double-click on the header's empty part fills the
                    // screen, as a Mac's title bar zooms its window (the name's names the
                    // tile); the first began a move that its mouse-up ended.
                    if ev.click_count == 2 {
                        if fills {
                            this.focus_tile(tile, cx);
                            this.width_action(cx, slopty_client::layout::Layout::toggle_fullscreen);
                        }
                    } else {
                        this.begin_move(tile, ev, cx);
                    }
                    cx.stop_propagation();
                }),
            );
        if shapes {
            return header.into_any_element();
        }
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
        // An upload in flight says how far it got, as the kit's ring and its figure in a pill;
        // a click stops it, which the ring says by turning into "×" under the pointer, as
        // Safari's download button does (at rest on touch, which has no hover). An
        // attachment's is said by its chip over the composer, and said once. No line along the
        // header's foot: a line on an edge read as a stray rule.
        let upload = self.header_upload(tile).map(|(xfer, upload)| {
            let figure =
                kit::progress::Progress::Share(upload.fraction()).figure().unwrap_or_default();
            let side = px(IconSize::Inline.slot(theme) * k);
            let ring = kit::progress::ring(
                theme,
                SharedString::from(format!("upload-ring-{}", id.as_uuid())),
                upload.fraction(),
                s.accent_fill,
                side,
            );
            let touch = theme.density == slopty_theme::Density::TOUCH;
            let stop =
                crate::icons::icon(theme, Symbol::Xmark, IconSize::Inline, hsla(s.text_secondary))
                    .size(side);
            let mark = div()
                .relative()
                .flex_none()
                .size(side)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .when(touch, gpui::Styled::invisible)
                        .group_hover(UPLOAD_GROUP, gpui::Styled::invisible)
                        .child(ring),
                )
                .child(
                    div()
                        .debug_selector(move || format!("upload-stop-{}", id.as_uuid()))
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(!touch, |el| {
                            el.invisible().group_hover(UPLOAD_GROUP, gpui::Styled::visible)
                        })
                        .child(stop),
                )
                .into_any_element();
            let hint_theme = std::rc::Rc::new(theme.clone());
            let pill = pill("upload", id, (Some(mark), figure), s.text_secondary, theme, chrome)
                .group(UPLOAD_GROUP)
                .role(Role::Button)
                .aria_label(STOP_UPLOAD)
                .aria_value(SharedString::from(upload.label()));
            kit::hint_timing(tab_stop(kit::tabular(pill), s.focus))
                .tooltip(move |_window, cx| {
                    let theme = std::rc::Rc::clone(&hint_theme);
                    cx.new(|_| kit::Hint::new(STOP_UPLOAD, "", theme)).into()
                })
                .on_click(cx.listener(move |this, _ev, _w, cx| this.cancel_upload(xfer, cx)))
                .into_any_element()
        });
        // A tile showing a thread says where its agent works after the title, the checkout and
        // its branch, which open the commit sheet; the shell's directory would say it again.
        // Read from the worker's table as the workspace keeps it, never from the thread view:
        // what a render reads it redraws with, and the view redraws on every step of its
        // working mark.
        let thread = match &item.kind {
            ItemKind::Terminal { session } if self.face_shown(*session) => {
                self.session_thread(*session)
            }
            ItemKind::Thread { thread } => Some(*thread),
            _ => None,
        };
        let thread_place = thread.and_then(|thread| self.header_place(thread));
        let worktree = agent.and_then(|(session, _)| self.worktree_of(session).cloned());
        let mut branch = thread_place.clone().map_or_else(Vec::new, |at| {
            self.place_chips(placed.tile, at, worktree.as_ref(), chrome, cx)
        });
        branch.extend(self.pull_chip(item, chrome));
        branch.extend(
            agent.map_or_else(Vec::new, |(session, _)| self.branch_chips(id, session, chrome)),
        );
        let (kind_states, kind_actions) = self.header_actions(tile, item, chrome, cx);
        let silenced = self.silenced(tile, item, chrome, cx);
        let face = match agent {
            Some((session, _)) => self.face_toggle(tile, session, chrome, cx),
            None => self.preview_toggle(tile, chrome, cx),
        };
        let actions = |actions: Vec<gpui::AnyElement>| {
            div().flex().flex_none().items_center().gap(px(theme.spacing.xs * k)).children(actions)
        };
        // How long a command has run, beside its calm mark, while no agent speaks for the shell.
        let running = match &item.kind {
            ItemKind::Terminal { session } if agent.is_none() => self.running_for(*session),
            _ => None,
        };
        let running = running.map(|ran| {
            let text = SharedString::from(kit::clock(ran));
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
        // A program's progress report (`OSC 9;4`): the kit's bar with its figure beside it, where
        // a tile says how it stands, not a line along the terminal's edge.
        let report = match &item.kind {
            ItemKind::Terminal { session } => {
                self.terminals.get(session).and_then(|view| view.read(cx).progress())
            }
            _ => None,
        };
        let report = report.map(|progress| {
            div()
                .id("report")
                .debug_selector(move || format!("report-{}", id.as_uuid()))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs * k))
                .child(
                    div().w(px(PROGRESS_W * k)).child(
                        kit::progress::Bar::new(
                            theme,
                            format!("report-bar-{}", id.as_uuid()),
                            progress,
                        )
                        .height(px(theme.spacing.xs * k))
                        .label("Progress"),
                    ),
                )
                .children(progress.figure().map(|figure| {
                    kit::tabular(div())
                        .text_size(px(theme.typography.small() * k))
                        .text_color(hsla(s.text_secondary))
                        .child(SharedString::from(figure))
                }))
                .into_any_element()
        });
        // Each readout with how much it is needed beside the title.
        let readouts: Vec<(&'static str, kit::Priority, gpui::AnyElement)> = [
            report.map(|r| ("report", kit::Priority::MEDIUM, r)),
            running.map(|r| ("running", kit::Priority::MEDIUM, r)),
            finished.map(|f| ("finished", kit::Priority::HIGH, f)),
        ]
        .into_iter()
        .flatten()
        .collect();
        // The title's context: where a shell or a file is, a page's address when the title is
        // not it. Muted, in the UI face as every header's context
        // is, with no separator: the colour tells it from the title.
        let place = match &item.kind {
            _ if thread_place.is_some() => None,
            ItemKind::Terminal { .. } => {
                self.tile_place(item).and_then(|p| place_beside(p, &title))
            }
            // The path bar right under a folder's header is where it is, every folder above it
            // a click away; the parent beside the title said it twice, 20 pt apart.
            ItemKind::Folder { .. } => None,
            _ => self.tile_place(item),
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
        // The worker's name, where more than one could be meant (the tile's workspace holds
        // tiles of several): quiet text after a server glyph, a fact about the tile rather than
        // a control. A workspace on one machine never pays for it.
        let spans =
            self.layout.position(tile).and_then(|p| self.layout.workspaces().get(p.workspace));
        let several = spans.is_some_and(|ws| ws.workers().len() > 1);
        let worker = several.then(|| self.workers.get(&tile.worker)).flatten().map(|w| {
            let name = w.name.clone();
            let machine = crate::icons::machine(w.caps.as_ref().map(|c| c.form));
            let muted = hsla(s.text_muted);
            // The glyph in its name's quiet tier: a machine's own colour is the navigator's
            // head's alone.
            let tint = muted;
            div()
                .id("worker")
                .debug_selector(move || format!("worker-{}", id.as_uuid()))
                .role(Role::Label)
                .aria_label(SharedString::from(name.clone()))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs * k))
                .text_color(muted)
                .child(
                    crate::icons::icon(theme, machine, IconSize::Inline, tint)
                        .size(px(IconSize::Inline.slot(theme) * k)),
                )
                .child(
                    ChromeText::new(name, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                )
        });
        // A file with an edit not yet on disk says so in a word after its name, as macOS's
        // "Edited" follows a document's title; saving keeps it until the worker has written it.
        let unsaved = self.file_facts(id).unsaved;
        let unsaved = unsaved.then(|| {
            div()
                .id("unsaved")
                .debug_selector(move || format!("unsaved-{}", id.as_uuid()))
                .role(Role::Label)
                .aria_label(EDITED)
                .flex_none()
                .font_weight(FontWeight(Typography::REGULAR_WEIGHT))
                .text_color(hsla(s.text_muted))
                .child(
                    ChromeText::new(EDITED, px(theme.typography.small()), k)
                        .zooming(chrome.zooming),
                )
        });
        let tabbed = placed.tabs.is_some();
        let header = if tabbed {
            let readouts = readouts.into_iter().map(|(_, _, el)| el).collect();
            let buttons = f32::from(1_u8.saturating_add(u8::from(face.is_some())));
            let controls = self.hover_controls(tile, kind_actions, face, k, cx);
            let strip = self.trailing_strip(placed, readouts, controls, buttons, k);
            let states = (!kind_states.is_empty()).then(|| actions(kind_states));
            let branch = branch.into_iter().map(|(_, _, el)| el);
            let tabs = self.render_tabs(placed, chrome, cx);
            // What the tabs leave is the header's; the tile's controls end it.
            let rest = div()
                .flex_none()
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(theme.spacing.sm * k))
                .pr(px(theme.spacing.inset() * k))
                .children(branch)
                .when_some(upload, gpui::ParentElement::child)
                .children(states)
                .when_some(silenced, gpui::ParentElement::child)
                .child(strip);
            header.border_b_0().child(tabs).child(rest)
        } else {
            self.header_row(
                HeaderParts {
                    placed,
                    item,
                    title,
                    chrome,
                    place,
                    address_title,
                    worker,
                    unsaved,
                    branch,
                    upload,
                    readouts,
                    states: (!kind_states.is_empty()).then(|| actions(kind_states)),
                    actions: kind_actions,
                    silenced,
                    face,
                },
                header,
                cx,
            )
        };
        header.into_any_element()
    }

    /// The header's leading slot: what the tile is, its kind's symbol or its agent's own mark,
    /// in one fixed square so every title starts on the same edge, as the navigator's rows and
    /// the palette's do. It never changes while the tile lives; how the tile is doing ends the
    /// header ([`Self::header_state`]). The mark sits a step under the title's tone. On a Mac
    /// a file's slot is its proxy, dragged out as a document window's title icon is.
    fn leading_slot(
        &self,
        tile: TileRef,
        item: &Item,
        focused: bool,
        k: f32,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let id = item.id;
        // The lead wears its title's tier and weight: the focused title's text at the medium
        // weight, any other's secondary tone at the regular.
        let ink = title_ink(&self.theme, focused);
        let weight =
            if focused { crate::icons::Weight::Medium } else { crate::icons::Weight::Regular };
        let glyph = self.kind_glyph(item);
        let slot = crate::palette::lead_slot_weighted(&self.theme, glyph, weight, hsla(ink), k)
            .debug_selector(move || format!("kind-{}", id.as_uuid()))
            .into_any_element();
        self.file_proxy(item, tile, slot, k, cx)
    }

    /// How the tile is doing, at the header's end (working, waiting on the person, failed,
    /// done, away), never on its lead, and as its glyph alone: no pill of words beside it, which
    /// the navigator's line and the pointer's hint say. None at rest: a hollow ring read as an
    /// unticked radio button. A remote picture on its way turns its mark in the body, which
    /// says what opens.
    ///
    /// An agent's glyph carries its whole state to a screen reader and what it asks to the
    /// pointer. While a terminal's agent waits on the person, its glyph is the one state worth a
    /// click: it brings up the terminal, whose own prompt is answered there (Slopty never answers
    /// for the person). A thread's face answers in its own card, so its glyph only says it.
    fn header_state(
        &self,
        tile: TileRef,
        item: &Item,
        k: f32,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let id = item.id;
        let opening = self.opening(item);
        let status = self
            .tile_status(tile, item)
            .filter(|st| *st != Status::Idle)
            .filter(|st| !(opening && *st == Status::Working))?;
        let theme = &self.theme;
        let mark = crate::icons::status_mark(theme, Some(status), k)
            .debug_selector(move || format!("status-{}", id.as_uuid()));
        let (session, agent) = match item.kind {
            ItemKind::Terminal { session } => (Some(session), self.agent_state(session)),
            ItemKind::Thread { thread } => (None, self.thread_stand(thread)),
            _ => (None, None),
        };
        let Some(agent) = agent else { return Some(mark.into_any_element()) };
        let words = SharedString::from(super::agents::agent_status_text(agent));
        let hint =
            super::agents::agent_ask_text(agent).map_or_else(|| words.clone(), SharedString::from);
        let waiting = session
            .filter(|session| super::agents::needs_human(agent) && !self.face_shown(*session));
        let hint_theme = std::rc::Rc::new(theme.clone());
        let s = &theme.surfaces;
        let glyph = div()
            .id("agent-state")
            .role(if waiting.is_some() { Role::Button } else { Role::Status })
            .aria_label(words)
            .flex_none()
            .rounded(px(theme.radii.xs * k))
            .child(mark)
            .map(kit::hint_timing)
            .tooltip(move |_window, cx| {
                let (hint, theme) = (hint.clone(), std::rc::Rc::clone(&hint_theme));
                cx.new(|_| kit::Hint::new(hint, "", theme)).into()
            });
        Some(match waiting {
            Some(session) => tab_stop(
                glyph
                    .debug_selector(move || format!("agent-{}", id.as_uuid()))
                    .aria_description(super::agents::SHOW_PROMPT)
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover))),
                s.focus,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| this.reveal_session(session, cx)))
            .into_any_element(),
            None => glyph.into_any_element(),
        })
    }

    /// The field that renames `tile` (`id`) in its title's place while it is renamed (an
    /// address the place's too): its header's, or on a phone the bar's. A click in it must not
    /// start a move.
    pub(super) fn rename_field(&self, tile: TileRef, id: ItemId) -> Option<gpui::AnyElement> {
        let renaming = self.rename.as_ref().filter(|r| r.tile == tile)?;
        let (part, label) = match renaming.field {
            Field::Name => ("rename", "Tile name"),
            Field::Address => ("address", ADDRESS),
            Field::Project => ("project-name", "Project name"),
        };
        Some(
            div()
                .id(part)
                .debug_selector(move || format!("{part}-{}", id.as_uuid()))
                .flex_1()
                .overflow_hidden()
                .cursor_text()
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                .child(Input::new(&renaming.input).aria_label(label))
                .into_any_element(),
        )
    }

    /// The title, or the field that renames the tile in its place.
    fn header_name(
        &self,
        tile: TileRef,
        id: ItemId,
        title: String,
        chrome: Chrome,
    ) -> gpui::AnyElement {
        match self.rename_field(tile, id) {
            Some(field) => field,
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
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let url = self.page_facts(tile.item)?.url.clone();
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
    ///
    /// The row lies on the panel as a single header does, and fades at an edge while tabs lie
    /// hidden past it. A tab is a row's height and radius: the shown one rests on the hover
    /// step's fill, the rest are quiet words that take it under the pointer, so the tabs read
    /// as objects on the header, not as boxes in a band.
    fn render_tabs(
        &self,
        placed: &Placed,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
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
        // In a narrow column a tab gives way to its mark and four letters' room before the
        // row scrolls.
        let narrow = kit::Room::of(placed.target.w, theme).is_narrow();
        let tab_min = if narrow {
            theme
                .spacing
                .xs
                .mul_add(2.0, theme.typography.ui_size.mul_add(4.0, theme.typography.icon_large()))
        } else {
            TAB_MIN
        };
        // The shown tab keeps its close in its row, so in a narrow column its floor is the same
        // four letters' room past the close and its gap.
        let shown_min = if narrow {
            tab_min + theme.spacing.xs + kit::icon_button_side(theme)
        } else {
            tab_min
        };
        let tabs: Vec<gpui::AnyElement> = column
            .iter()
            .copied()
            .filter_map(|tab| {
                let item = self.item(tab)?;
                let id = item.id;
                let shown = tab == placed.tile;
                let floor = if shown { shown_min } else { tab_min };
                let on = shown && placed.focused;
                let ink = title_ink(theme, on);
                let weight =
                    if on { crate::icons::Weight::Medium } else { crate::icons::Weight::Regular };
                let glyph = self.kind_glyph(item);
                let slot = crate::palette::lead_slot_weighted(theme, glyph, weight, hsla(ink), k)
                    .debug_selector(move || format!("tab-slot-{}", id.as_uuid()));
                // How it is doing ends the tab, before its close button.
                let state =
                    self.tile_status(tab, item).filter(|st| *st != Status::Idle).map(|st| {
                        crate::icons::status_mark(theme, Some(st), k)
                            .debug_selector(move || format!("tab-status-{}", id.as_uuid()))
                    });
                let title = self.tile_title(item);
                let label = SharedString::from(title.clone());
                let name = self.header_name(tab, id, title, chrome);
                let close = kit::icon_button_at(
                    theme,
                    format!("tab-close-{}", id.as_uuid()),
                    Symbol::Xmark,
                    CLOSE_TILE,
                    k,
                )
                .on_click(
                    cx.listener(move |this, _ev, window, cx| this.close_tile(tab, window, cx)),
                );
                // The shown tab keeps its close in the row. Any other's takes no room at rest, so
                // a narrow tab keeps its four letters: it shows over the tab's end while the
                // pointer is on the tab, on the tab's hover fill made solid, so the name it covers
                // gives way under it.
                let close = if shown {
                    close.into_any_element()
                } else {
                    let under = hsla(s.hover.over(theme.content()));
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .pr(px(theme.spacing.xxs * k))
                        .flex()
                        .items_center()
                        .rounded(px(theme.radii.sm * k))
                        .group_hover(TAB_GROUP, move |el| el.bg(under))
                        .child(close.invisible().group_hover(TAB_GROUP, gpui::Styled::visible))
                        .into_any_element()
                };
                let el = div().id(SharedString::from(format!("tab-{}", id.as_uuid())));
                Some(
                    Self::tile_menu_press(el, tab, Pressed::Tab, cx)
                        .debug_selector(move || format!("tab-{}", id.as_uuid()))
                        .group(TAB_GROUP)
                        .relative()
                        .role(Role::Tab)
                        .aria_label(label)
                        .aria_selected(shown)
                        // As wide as its title, between the two bounds; tabs that do not fit give
                        // way alike down to the narrower one.
                        .flex_initial()
                        .min_w(px(floor * k))
                        .max_w(px(TAB_MAX * k))
                        .h(px(theme.density.row * k))
                        .flex()
                        .items_center()
                        .gap(px(theme.spacing.xs * k))
                        .pl(px(theme.spacing.xs * k))
                        .pr(px(theme.spacing.xxs * k))
                        .rounded(px(theme.radii.sm * k))
                        .text_color(hsla(ink))
                        .when(shown && placed.focused, |el| {
                            el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                        })
                        .map(|el| {
                            if shown {
                                el.bg(hsla(s.hover))
                            } else {
                                el.hover(move |el| el.bg(hsla(s.hover)))
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
                        .children(state)
                        .child(close)
                        .into_any_element(),
                )
            })
            .collect();
        // The tabs take their titles' widths and the header after them the rest. The first
        // tab's kind sits on the edge grid, where a single header's does: its pad round it.
        // Tabs that do not fit scroll, as Zed's tab bar does, and the shown one is brought
        // into view whenever it changes and whenever the column's width does, so a column that
        // narrows never leaves its shown tab past its edge. Between those the row stays where
        // the person scrolled it.
        let first = column.first().map_or(placed.tile.item, |t| t.item);
        let shown_ix = column.iter().position(|t| *t == placed.tile).unwrap_or_default();
        let width = placed.target.w;
        let handle = {
            let mut rows = self.drawn.tab_rows.borrow_mut();
            let (handle, last, wide) =
                rows.entry(first).or_insert_with(|| (gpui::ScrollHandle::new(), first, f32::NAN));
            if *last != placed.tile.item || (*wide - width).abs() >= 0.5 || wide.is_nan() {
                *last = placed.tile.item;
                *wide = width;
                // The first tab is brought in as far as the row's start, its pad with it: a
                // reveal stops at the tab's own edge, and would leave the pad hidden and faded.
                if shown_ix == 0 {
                    handle.set_offset(gpui::point(px(0.0), px(0.0)));
                } else {
                    handle.scroll_to_item(shown_ix);
                }
            }
            handle.clone()
        };
        let row = div()
            .id(SharedString::from(format!("tab-row-{}", first.as_uuid())))
            .debug_selector(move || format!("tab-row-{}", first.as_uuid()))
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs * k))
            .pl(px((theme.spacing.inset() - theme.spacing.xs) * k))
            .overflow_x_scroll()
            .track_scroll(&handle);
        // An edge past which tabs lie hidden fades out per pixel, as deep as they run past it,
        // so a row cut at its end reads as more tabs, not as a tab cut in half; a row that fits
        // fades nowhere. The fade is the tabs' own, over the panel's surface and the shown tab's
        // fill alike, never a wash in a colour of its own.
        let fade = gpui::EdgeFade::x(px(theme.spacing.xl * k));
        gpui::edge_fade(row.children(tabs), fade).hidden_by_scroll(&handle).into_any_element()
    }

    /// How the tile is doing, in the one status vocabulary: its agent's state (as its thread
    /// knows it too, [`Self::agent_mark`]); else, out of reach; else a remote picture on its way
    /// (none once its open failed); else a shell's last command, failed or finished unwatched.
    pub(super) fn tile_status(&self, tile: TileRef, item: &Item) -> Option<Status> {
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        };
        if let Some(status) = session.and_then(|s| self.agent_mark(s)) {
            return Some(status);
        }
        // A thread tile says where its thread stands as its worker's table row has it: a
        // thread driven over a protocol has no terminal whose agent could say it.
        if let ItemKind::Thread { thread } = item.kind
            && let Some(status) = self.thread_stand(thread).and_then(ThreadStand::status)
        {
            return Some(status);
        }
        let Some(worker) = self.workers.get(&tile.worker).filter(|w| w.link.is_some()) else {
            return Some(Status::Away);
        };
        // An open that failed is over: its pane says so, and the header waits on nothing.
        if worker.failed_opens.contains_key(&item.id) {
            return None;
        }
        if self.opening(item) {
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
        // The newest prompt carries the status of the command before it.
        let shell = self.shell(session)?;
        if shell.running.is_some() {
            return self.running_for(session).map(|_| Status::Running);
        }
        // A failure the grid is showing, washed and barred, is not marked a second time here.
        (shell.exit? != 0 && !shell.failure_in_view).then_some(Status::Failed)
    }

    /// How long `session`'s command has run, once that is past [`RUNNING_AFTER`]: what its
    /// tile's header and its navigator row count.
    ///
    /// [`RUNNING_AFTER`]: super::RUNNING_AFTER
    pub(super) fn running_for(&self, session: SessionId) -> Option<Duration> {
        let (now, _) = self.ticked()?;
        let ran = now.saturating_duration_since(self.shell(session)?.started?);
        (ran >= self.running_after).then_some(ran)
    }

    /// Whether a remote window or display is on its way: asked for and not yet drawn, while
    /// its worker is up. Not one let go off screen, which waits on nothing.
    fn opening(&self, item: &Item) -> bool {
        if !matches!(item.kind, ItemKind::Window { .. } | ItemKind::Display { .. }) {
            return false;
        }
        match self.screens.get(&item.id) {
            Some(_) => self.stream(item.id).is_some_and(|stream| stream.waiting),
            None => !self.parked.contains(&item.id),
        }
    }

    /// Where `thread`'s agent works, from its worker's table: the checkout by its folder's
    /// name and the branch; `None` while neither is known.
    fn header_place(&self, thread: ThreadId) -> Option<HeaderPlace> {
        let place = self.thread_place(thread);
        let checkout = place
            .and_then(|p| p.cwd.as_deref())
            .and_then(|cwd| cwd.trim_end_matches('/').rsplit('/').next())
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let branch = self
            .thread_facts(thread)
            .and_then(|facts| facts.get(BRANCH_FACT))
            .filter(|b| !b.is_empty())
            .cloned();
        let commits = place.is_some_and(|p| p.repo.is_some());
        (checkout.is_some() || branch.is_some()).then_some(HeaderPlace {
            checkout,
            branch,
            commits,
        })
    }

    /// Where a thread's agent works, as header items after the title: the checkout by its
    /// folder's name, then the branch with its glyph, each a press away from the commit sheet
    /// when the place is a repository. What the worktree's chip says already is left out. They
    /// stay while a long title narrows to its floor; then the branch leaves, then the checkout,
    /// which names the work's project, and the pull request last.
    fn place_chips(
        &self,
        tile: TileRef,
        at: HeaderPlace,
        worktree: Option<&Worktree>,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Vec<(&'static str, kit::Priority, gpui::AnyElement)> {
        let id = tile.item;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let checkout = at.checkout.filter(|c| worktree.is_none_or(|t| t.name != *c));
        let branch =
            at.branch.filter(|b| worktree.is_none_or(|t| t.branch.as_deref() != Some(b.as_str())));
        let chip = |part: &'static str, glyph: Option<Mark>, words: String| {
            let muted = hsla(s.text_muted);
            let said = SharedString::from(words.clone());
            let el = div()
                .id(part)
                .debug_selector(move || format!("{part}-{}", id.as_uuid()))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xxs * k))
                .px(px(theme.spacing.xxs * k))
                .rounded(px(theme.radii.xs * k))
                .text_color(muted)
                .children(glyph.map(|glyph| {
                    crate::icons::icon(theme, glyph, IconSize::Inline, muted)
                        .size(px(IconSize::Inline.slot(theme) * k))
                }))
                .child(
                    ChromeText::new(words, px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                );
            if !at.commits {
                return el.role(Role::Label).aria_label(said).into_any_element();
            }
            tab_stop(
                el.role(Role::Button)
                    .aria_label(SharedString::from(format!("Commit on {said}")))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text_secondary)))
                    .active(move |el| el.bg(hsla(s.pressed))),
                s.focus,
            )
            .on_click(cx.listener(move |this, _ev, window, cx| {
                cx.stop_propagation();
                let view = this.item(tile).and_then(|item| match item.kind {
                    ItemKind::Terminal { session } => this.thread_face(session),
                    ItemKind::Thread { .. } => this.thread_item(item.id),
                    _ => None,
                });
                if let Some(view) = view.cloned() {
                    view.update(cx, |v, cx| v.open_commit(window, cx));
                }
            }))
            .into_any_element()
        };
        checkout
            .map(|c| ("checkout", kit::Priority::MEDIUM, chip("thread-checkout", None, c)))
            .into_iter()
            .chain(branch.map(|b| {
                let glyph = Some(GitGlyph::Branch.into());
                ("branch", kit::Priority::MEDIUM, chip("thread-branch", glyph, b))
            }))
            .collect()
    }

    /// An agent's worktree, as the worker last said: its name, quiet, its branch in the hint.
    /// Words, not a chip: the header's one fill is the state's. Its pull request is the
    /// thread's ([`Self::pull_chip`]).
    fn branch_chips(
        &self,
        id: ItemId,
        session: SessionId,
        chrome: Chrome,
    ) -> Vec<(&'static str, kit::Priority, gpui::AnyElement)> {
        let Some(tree) = self.worktree_of(session) else { return Vec::new() };
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let hint_theme = std::rc::Rc::new(theme.clone());
        let muted = hsla(s.text_muted);
        let hint = tree.branch.as_ref().map_or_else(
            || format!("Worktree {}", tree.name),
            |b| format!("Worktree {} on {b}", tree.name),
        );
        let (hint, path, hint_theme) =
            (SharedString::from(hint), SharedString::from(tree.path.clone()), hint_theme);
        let worktree = div()
            .id("worktree")
            .debug_selector(move || format!("worktree-{}", id.as_uuid()))
            .role(Role::Label)
            .aria_label(hint.clone())
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs * k))
            .text_color(muted)
            .map(kit::hint_timing)
            .tooltip(move |_window, cx| {
                let (hint, path) = (hint.clone(), path.clone());
                let theme = std::rc::Rc::clone(&hint_theme);
                cx.new(|_| kit::Hint::new(hint, path, theme)).into()
            })
            .child(
                crate::icons::icon(theme, GitGlyph::Branch, IconSize::Inline, muted)
                    .size(px(IconSize::Inline.slot(theme) * k)),
            )
            .child(
                ChromeText::new(tree.name.clone(), px(theme.typography.small()), k)
                    .fill()
                    .zooming(chrome.zooming),
            )
            .into_any_element();
        vec![("worktree", kit::Priority::LOW, worktree)]
    }

    /// The pull request of `item`'s agent's branch, as its thread's row in its worker's table
    /// last read it, for every agent alike: a terminal's agent's or a thread tile's.
    pub(super) fn tile_pull(&self, item: &Item) -> Option<&slopty_proto::thread::wire::PullSeen> {
        let thread = match item.kind {
            ItemKind::Terminal { session } if self.agent_state(session).is_some() => {
                self.session_thread(session)?
            }
            ItemKind::Thread { thread } => thread,
            _ => return None,
        };
        self.thread_place(thread)?.pull.as_ref()
    }

    /// The chip of `item`'s agent's branch's pull request ([`Self::tile_pull`]), for
    /// every agent alike: its glyph by where it stands in that state's ink (open green, merged
    /// violet, closed red, a draft grey, as GitHub's), then its number in the quiet tier. A
    /// click opens its page; the pointer and a screen reader have its line ("#42: lint
    /// failed") and its title. It stays while the title narrows; the worktree's name goes
    /// first.
    fn pull_chip(
        &self,
        item: &Item,
        chrome: Chrome,
    ) -> Option<(&'static str, kit::Priority, gpui::AnyElement)> {
        let id = item.id;
        let pull = self.tile_pull(item)?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let glyph = GitGlyph::of_stands(pull.stands);
        let glyph_ink = glyph.state_ink(theme).unwrap_or(s.text_secondary);
        let line = SharedString::from(pull.line());
        let title = SharedString::from(pull.title.clone());
        let url = pull.url.clone();
        let hint_theme = std::rc::Rc::new(theme.clone());
        let chip = kit::pill_frame(theme, k)
            .id("pr")
            .debug_selector(move || format!("pr-{}", id.as_uuid()))
            .role(Role::Link)
            .aria_label(line.clone())
            .aria_description(title.clone())
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs * k))
            .text_color(hsla(s.text_secondary))
            .cursor_pointer()
            .hover(|el| el.bg(hsla(s.hover)))
            .active(|el| el.bg(hsla(s.pressed)))
            .map(kit::hint_timing)
            .tooltip(move |_window, cx| {
                let (line, title) = (line.clone(), title.clone());
                let theme = std::rc::Rc::clone(&hint_theme);
                cx.new(|_| kit::Hint::new(line, title, theme)).into()
            })
            .child(
                crate::icons::icon(theme, glyph, IconSize::Inline, hsla(glyph_ink))
                    .size(px(IconSize::Inline.slot(theme) * k)),
            )
            .child(
                ChromeText::new(format!("#{}", pull.number), px(theme.typography.small()), k)
                    .zooming(chrome.zooming),
            );
        let chip = tab_stop(chip, s.focus).on_click(move |_ev, _window, cx| cx.open_url(&url));
        Some(("pr", PR_PRIORITY, chip.into_any_element()))
    }

    /// A worker's sound was silenced or resumed: every remote tile copies its stream again, so
    /// each of that worker's says so at once.
    pub(super) fn sound_changed(&mut self, cx: &mut Context<Self>) {
        let screens: Vec<ItemId> = self.screens.keys().copied().collect();
        for id in screens {
            self.stream_changed(id, cx);
        }
    }

    /// A remote tile's toggle for its worker's sound, which every one of the worker's tiles
    /// silences and says; `None` for a tile with no stream.
    fn mute_toggle(
        &self,
        tile: TileRef,
        id: ItemId,
        muted: bool,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.screens.contains_key(&id) {
            return None;
        }
        let theme = &self.theme;
        let icon = if muted { Symbol::SpeakerSlash } else { Symbol::SpeakerWave2 };
        let label = SharedString::from(mute_label(&self.worker_name(tile.worker)));
        let hint_theme = std::rc::Rc::new(theme.clone());
        let hint = label.clone();
        let toggle =
            kit::icon_toggle(theme, format!("mute-{}", id.as_uuid()), icon, MUTE, muted, chrome.k)
                .aria_label(label)
                .map(kit::hint_timing)
                .tooltip(move |_window, cx| {
                    let theme = std::rc::Rc::clone(&hint_theme);
                    cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                });
        let toggle = toggle.on_click(cx.listener(move |this, _ev, _w, cx| {
            if let Some(view) = this.screens.get(&id) {
                view.read(cx).toggle_mute();
                this.sound_changed(cx);
            }
        }));
        Some(toggle.into_any_element())
    }

    /// The toggle of a silenced worker's sound, which every tile of it shows whether or not it
    /// is hovered or focused: a silenced tile must not pass for a quiet one.
    fn silenced(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let muted = self.stream(item.id).is_some_and(|stream| stream.muted);
        if !muted {
            return None;
        }
        self.mute_toggle(tile, item.id, true, chrome, cx)
    }

    /// What a tile's kind says and offers in its header: first what stands at rest because it
    /// is a state to see (another client rules the PTY's size, so take it; a stream's health;
    /// the system's keys going to the worker), then its actions, shown under the pointer only
    /// (the trackpad, mute, a page's ways back and forward, a folder's way up).
    fn header_actions(
        &self,
        tile: TileRef,
        item: &Item,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> (Vec<gpui::AnyElement>, Vec<gpui::AnyElement>) {
        let theme = &self.theme;
        let id = item.id;
        let stream = self.stream(id).copied().unwrap_or_default();
        let muted = stream.muted;
        let mut states: Vec<gpui::AnyElement> = Vec::new();
        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        match &item.kind {
            ItemKind::Terminal { session } => {
                let session = *session;
                // A phone's header has room for the tile's name and its state only. A thread
                // view's composer says the changes and the context itself, a click from the
                // review and the rate windows, so its header says neither a second time.
                let session = &session;
                // Another client's size rules this PTY: offer to take it.
                if self.shell(*session).is_some_and(|s| !s.driving) {
                    let pill = pill("take", id, (None, TAKE), theme.surfaces.accent, theme, chrome)
                        .role(Role::Button)
                        .aria_label(TAKE_OVER);
                    states.push(
                        tab_stop(pill, theme.surfaces.focus)
                            .on_click(
                                cx.listener(move |this, _ev, _w, cx| this.take_over(tile, cx)),
                            )
                            .into_any_element(),
                    );
                }
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                if let Some(view) = self.screens.get(&id) {
                    use crate::screen::ScreenView;
                    states.extend(ScreenView::health_mark(view, stream.header, theme, chrome.k));
                    let trackpad =
                        ScreenView::trackpad_button(view, stream.header, theme, chrome.k);
                    actions.extend(trackpad);
                }
                // Only while the system's shortcuts go to the worker: this Mac's ⌘Tab not
                // working is a state to see, and a click here gives it back.
                if stream.system_keys {
                    let toggle = kit::icon_toggle(
                        theme,
                        format!("system-keys-{}", id.as_uuid()),
                        Symbol::Command,
                        super::desktop::SEND_SYSTEM_KEYS,
                        true,
                        chrome.k,
                    );
                    states.push(
                        toggle
                            .on_click(cx.listener(move |this, _ev, _w, cx| {
                                this.flip_system_keys(id, cx);
                            }))
                            .into_any_element(),
                    );
                }
                // Silenced, the toggle is out of the hover's reach ([`Self::silenced`]).
                if !muted && stream.has_audio {
                    actions.extend(self.mute_toggle(tile, id, false, chrome, cx));
                }
            }
            // A page's ways back and forward are a matched pair of the header's bare icon
            // buttons, as close is, shown only while there is history that way. Reload is the
            // header's menu's and the palette's: ⌘R is the layout's.
            ItemKind::Browser { .. } => {
                if let Some(view) = self.browsers.get(&id).cloned() {
                    let uuid = id.as_uuid();
                    let k = chrome.k;
                    let (back, forward) = self
                        .page_facts(id)
                        .map_or((false, false), |p| (p.can_go_back, p.can_go_forward));
                    let ways: [(bool, &str, Symbol, &str, Go); 2] = [
                        (back, "back", Symbol::ChevronLeft, "Back", BrowserView::back),
                        (forward, "forward", Symbol::ChevronRight, "Forward", BrowserView::forward),
                    ];
                    for (shown, key, icon, label, go) in ways {
                        if !shown {
                            continue;
                        }
                        let target = view.clone();
                        actions.push(
                            kit::icon_button_at(theme, format!("{key}-{uuid}"), icon, label, k)
                                .on_click(move |_ev, _w, cx| target.update(cx, go))
                                .into_any_element(),
                        );
                    }
                }
            }
            // The way up is the header's bare icon button, as a page's way back is.
            ItemKind::Folder { .. } => {
                if let Some(view) = self.folders.get(&id).cloned()
                    && self.folder_facts(id).has_parent
                {
                    actions.push(
                        kit::icon_button_at(
                            theme,
                            format!("up-{}", id.as_uuid()),
                            Symbol::ArrowUp,
                            crate::folder::ENCLOSING_FOLDER,
                            chrome.k,
                        )
                        .on_click(move |_ev, _w, cx| view.update(cx, FolderView::open_parent))
                        .into_any_element(),
                    );
                }
            }
            ItemKind::File { .. }
            | ItemKind::Review { .. }
            | ItemKind::Changes { .. }
            | ItemKind::Thread { .. } => {}
        }
        (states, actions)
    }

    /// What an agent's tile shows, for a screen reader: its face, as its header's switch says
    /// ([`Face::label`]). `None` for any other tile.
    fn tile_shows(&self, item: &Item) -> Option<&'static str> {
        match item.kind {
            ItemKind::Thread { .. } => Some(Face::Thread.label()),
            ItemKind::Terminal { session } if self.faces_of(session).len() > 1 => {
                Some(self.tile_face(session).label())
            }
            _ => None,
        }
    }

    /// An agent tile's face toggle ([`Face`]): one icon button, the face ⌘J goes to next by
    /// its glyph and named for it ("Show terminal"), under the pointer only, as close is. A
    /// switch of a segment a face stood on every agent's header at rest, three to four buttons
    /// where the person mostly watches. `None` while the tile has one face, a shell no agent
    /// runs in.
    fn face_toggle(
        &self,
        tile: TileRef,
        session: SessionId,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        let faces = self.faces_of(session);
        if faces.len() < 2 {
            return None;
        }
        let shown = self.tile_face(session);
        let next = faces
            .iter()
            .skip_while(|face| **face != shown)
            .nth(1)
            .or_else(|| faces.first())
            .copied()?;
        let id = format!("face-{}-{}", next.key(), tile.item.as_uuid());
        Some(
            kit::icon_button_at(
                &self.theme,
                id,
                next.symbol(),
                super::context_menus::show_face(next),
                chrome.k,
            )
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                this.focus_tile(tile, cx);
                this.set_face(session, next, cx);
            }))
            .into_any_element(),
        )
    }

    /// The button that turns a Markdown file's tile between its preview and its source, as ⌘⇧V
    /// does; `None` for any other tile, or one with no text to show yet.
    fn preview_toggle(
        &self,
        tile: TileRef,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> Option<gpui::AnyElement> {
        // From the facts, not the view: reading the view would build the strip again at each
        // of its caret's blinks.
        let previewing = self.file_facts(tile.item).preview?;
        let (icon, label) = if previewing {
            (Symbol::Pencil, crate::file::SHOW_SOURCE)
        } else {
            (Symbol::Eye, crate::file::SHOW_PREVIEW)
        };
        let id = format!("preview-{}", tile.item.as_uuid());
        Some(
            kit::icon_button_at(&self.theme, id, icon, label, chrome.k)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.focus_tile(tile, cx);
                    if let Some(view) = this.files.get(&tile.item).cloned() {
                        view.update(cx, |v, cx| v.toggle_preview(window, cx));
                    }
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
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = chrome.k;
        let item = tile.item;
        let hover = s.hover;
        let took = SharedString::from(kit::duration(done.elapsed));
        let el = kit::tabular(div())
            .id("finished")
            .debug_selector(move || format!("finished-{}", item.as_uuid()))
            .role(Role::Button)
            .aria_label(SharedString::from(done.label()))
            .flex_none()
            .px(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.xs * k))
            .text_size(px(theme.typography.small() * k))
            .text_color(hsla(s.text_muted))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(hover)))
            .child(ChromeText::new(took, px(theme.typography.small()), k).zooming(chrome.zooming));
        tab_stop(el, s.focus)
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reveal_session(session, cx)))
            .into_any_element()
    }

    /// Whether filling the screen adds something to `placed`: not on a phone, where a column is
    /// the screen's width already.
    pub(super) fn offers_fullscreen(&self, placed: &Placed) -> bool {
        let view_w = f32::from(self.drawn.viewport.get().size.width);
        view_w >= self.layout.config().phone_below || placed.target.w + 1.0 < view_w
    }

    /// The controls a header shows under the pointer, on its own ground over what ends it at
    /// rest: the kind's actions, the face toggle, then close.
    fn hover_controls(
        &self,
        tile: TileRef,
        actions: Vec<gpui::AnyElement>,
        face: Option<gpui::AnyElement>,
        k: f32,
        cx: &Draw<'_, Self>,
    ) -> Div {
        let theme = &self.theme;
        let id = tile.item.as_uuid();
        div()
            .debug_selector(move || format!("controls-{id}"))
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .pl(px(theme.spacing.xs * k))
            // Not a ground: the backing that hides the title's end under the controls.
            .bg(hsla(theme.content()))
            .invisible()
            .group_hover(HEADER_GROUP, gpui::Styled::visible)
            .children(actions)
            .children(face)
            .child(self.tile_close(tile, k, cx))
    }

    /// A tile's close button. Fullscreen has no button: a double-click on the header's empty
    /// part fills the screen (as a Mac's title bar zooms its window), as do the palette's and
    /// the header's menu's "Fullscreen".
    fn tile_close(&self, tile: TileRef, k: f32, cx: &Draw<'_, Self>) -> Stateful<Div> {
        let id = tile.item.as_uuid();
        kit::icon_button_at(&self.theme, format!("close-{id}"), Symbol::Xmark, CLOSE_TILE, k)
            .on_click(cx.listener(move |this, _ev, window, cx| {
                this.close_tile(tile, window, cx);
            }))
    }

    /// A single tile's header row: its lead, its title and where it is, then at the trailing
    /// end its facts, its state and its readouts, laid out as a [`kit::priority_row`]. The
    /// title keeps a third of the header; a worktree's name and the worker go before a word
    /// of it, a pull request and the readouts while it narrows to that floor. No button stands
    /// at rest, focused or not: the kind's actions, the face toggle and close show under the
    /// pointer over the readouts' place, on the header's own ground, and keep no room at rest.
    /// Touch has no hover, and its header none of them: they are its long press's menu.
    fn header_row(
        &self,
        parts: HeaderParts<'_>,
        header: Stateful<Div>,
        cx: &Draw<'_, Self>,
    ) -> Stateful<Div> {
        let HeaderParts {
            placed,
            item,
            title,
            chrome,
            place,
            address_title,
            worker,
            unsaved,
            branch,
            upload,
            readouts,
            states,
            actions,
            silenced,
            face,
        } = parts;
        let theme = &self.theme;
        let tile = placed.tile;
        let id = item.id;
        let k = chrome.k;
        let focused = placed.focused;
        let lead = self.leading_slot(tile, item, focused, k, cx);
        let state = self.header_state(tile, item, k, cx);
        let touch = theme.density == slopty_theme::Density::TOUCH;
        let name = self.header_name(tile, id, title, chrome);
        let renaming = self.rename.as_ref().is_some_and(|r| r.tile == tile);
        // "Edited" follows the title it qualifies, as a document's title bar has it.
        let named = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .when(focused, |el| el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT)))
            .map(|el| {
                if address_title {
                    el.cursor_text().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev: &MouseDownEvent, window, cx| {
                            this.start_address(tile, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                } else {
                    // A double-click on the name names the tile; on the rest of the header it
                    // fills the screen. The first click began a move, as on the header.
                    el.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            if ev.click_count == 2 {
                                this.start_rename(tile, window, cx);
                                cx.stop_propagation();
                            }
                        }),
                    )
                }
            })
            .child(name)
            .when_some(unsaved, gpui::ParentElement::child);
        // The title and where it is, as one: the place gives way long before the name.
        let titled = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .child(named)
            .children(place);
        let inset = theme.spacing.inset() * k;
        let floor = (inset.mul_add(-2.0, placed.target.w * k) / 3.0).max(0.0);
        let mut row = kit::priority_row(SharedString::from(format!("header-row-{}", id.as_uuid())))
            .h_full()
            .gap(px(theme.spacing.sm * k))
            .item("lead", kit::Priority::ESSENTIAL, lead)
            .title(titled, px(floor))
            .title_fills(renaming)
            .end();
        if let Some(worker) = worker {
            row = row.item("worker", kit::Priority::LOW, worker);
        }
        for (key, priority, chip) in branch {
            row = row.item(key, priority, chip);
        }
        if let Some(upload) = upload {
            row = row.item("upload", kit::Priority::HIGH, upload);
        }
        if let Some(states) = states {
            row = row.item("states", kit::Priority::HIGH, states);
        }
        if let Some(silenced) = silenced {
            row = row.item("silenced", kit::Priority::MEDIUM, silenced);
        }
        // The state ends the header at rest. Under the pointer the controls take its place and
        // the readouts', as the tile is a click from its prompt there anyway; the keyboard and a
        // screen reader still reach a waiting agent's glyph.
        if let Some(state) = state {
            let state = div()
                .when(!touch, |el| el.group_hover(HEADER_GROUP, gpui::Styled::invisible))
                .child(state);
            row = row.item("state", kit::Priority::HIGH, state);
        }
        for (key, priority, readout) in readouts {
            let readout = div()
                .when(!touch, |el| el.group_hover(HEADER_GROUP, gpui::Styled::invisible))
                .child(readout);
            row = row.item(key, priority, readout);
        }
        let row = if touch {
            row
        } else {
            row.overlay(self.hover_controls(tile, actions, face, k, cx).h_full())
        };
        header.items_center().px(px(inset)).child(row)
    }

    /// A tabbed header's right end: one fixed strip where the tile's readouts (a finished
    /// command's time, a program's progress) sit at rest and the controls (the kind's actions,
    /// the face toggle, close) take their place while the pointer is on the header, as a single
    /// header's do. It is never narrower than the face toggle and close, so the swap moves
    /// nothing beside it. Touch has no hover, and the strip no controls: they are the long
    /// press's menu. Each button focuses its tile and runs the action its key runs, so a click
    /// and ⌘J or ⌘W do the same thing.
    fn trailing_strip(
        &self,
        placed: &Placed,
        readouts: Vec<gpui::AnyElement>,
        controls: Div,
        buttons: f32,
        k: f32,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let id = placed.tile.item.as_uuid();
        let touch = theme.density == slopty_theme::Density::TOUCH;
        let controls = (!touch).then(|| controls.absolute().top_0().bottom_0().right_0());
        let readouts = div()
            .debug_selector(move || format!("readouts-{id}"))
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xs * k))
            .when(!touch, |el| el.group_hover(HEADER_GROUP, gpui::Styled::invisible))
            .children(readouts);
        let mut strip = div();
        // Gives way well before the title does and well after the place, down to its buttons.
        strip.style().flex_shrink = Some(STRIP_SHRINK);
        let least = if touch { 0.0 } else { buttons * kit::icon_button_side(theme) * k };
        kit::tabular(
            strip
                .debug_selector(move || format!("strip-{id}"))
                .relative()
                .h_full()
                .min_w(px(least))
                .flex()
                .items_center()
                .justify_end(),
        )
        .child(readouts)
        .children(controls)
        .into_any_element()
    }

    /// Close `tile` as ⌘W closes the focused one: a shell asks first while its command runs,
    /// and the closing can be taken back.
    pub(super) fn close_tile(
        &mut self,
        tile: TileRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        if let Some(away) = self.away_state(tile.worker) {
            return Some(away);
        }
        let ItemKind::Terminal { session } = item.kind else { return None };
        match self.summary(session).map(|s| &s.state) {
            Some(SessionState::Exited { status }) => Some(BodyState::Exited(*status)),
            Some(SessionState::Running) => None,
            None if self.terminals.contains_key(&session) => None,
            None => Some(BodyState::Ended),
        }
    }

    /// What every tile of `worker` says while its link is down: how it is out of reach, or the
    /// build it runs; `None` while it is linked.
    pub(super) fn away_state(&self, worker: WorkerKey) -> Option<BodyState> {
        let worker = self.workers.get(&worker);
        if worker.is_some_and(|w| w.link.is_some()) {
            return None;
        }
        let name = worker.map_or("The machine", |w| w.name.as_str());
        Some(match worker.map(|w| &w.status) {
            Some(WorkerStatus::NeedsUpdate(notice)) => BodyState::NeedsUpdate(notice.clone()),
            Some(WorkerStatus::Unreachable) => {
                BodyState::Away(format!("{name} is unreachable").into())
            }
            Some(WorkerStatus::Gone) => BodyState::Away(format!("{name} is gone").into()),
            Some(WorkerStatus::NotGranted) => {
                BodyState::Away(format!("{name} does not let this device in").into())
            }
            _ => {
                let away =
                    worker.and_then(|w| w.away_since).map(|since| self.now().saturating_sub(since));
                BodyState::Away(reconnecting(away.unwrap_or_default()).into())
            }
        })
    }

    /// What the away pill offers for `worker`: the tailnet grant to copy when its policy turns
    /// this device away, a wake while it can be woken, and a dial now while its link is down.
    fn away_actions(&self, worker: WorkerKey) -> AwayActions {
        let status = self.workers.get(&worker).map(|w| &w.status);
        let host = self.host_actions(worker);
        let not_granted = matches!(status, Some(WorkerStatus::NotGranted));
        let asleep = matches!(status, Some(WorkerStatus::Unreachable | WorkerStatus::Gone));
        AwayActions {
            grant: self.tailnet_grant.clone().filter(|_| not_granted),
            wake: host.and_then(|h| h.wake.clone()).filter(|_| asleep),
            retry: host.and_then(|h| h.connect.clone()),
        }
    }

    /// The pill at the foot of a body saying what is wrong and what to do about it, over
    /// whatever the body still shows. Never a dialog: the rest of the workspace goes on.
    ///
    /// Over a body with nothing to show (a tile kept from the last run whose worker has not
    /// come back), `centred` says it as the body's one block instead: the state's mark in the
    /// notice's disc, what is so, why, then what to do, in the middle where the eye goes, as a
    /// window on its way says "Opening Safari on studio…".
    pub(super) fn render_state_pill(
        &self,
        tile: TileRef,
        state: &BodyState,
        centred: bool,
        chrome: Chrome,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        // A shell's pill restarts it: its session, while its item is here.
        let session = self.item(tile).and_then(|item| match item.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        });
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
                .max_w_full()
                .text_color(hsla(tone))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.hover)))
                .child(ChromeText::new(label, px(theme.typography.small()), k).fill());
            tab_stop(el, s.focus)
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
                        .text_color(hsla(s.text_muted))
                        .child(ChromeText::new(said, px(theme.typography.small()), k).fill())
                });
                (detail, copy)
            }
            BodyState::Away(_) => {
                let why = match self.workers.get(&tile.worker).map(|w| &w.status) {
                    Some(WorkerStatus::Reconnecting(why)) => Some(kit::first_line(why).to_owned()),
                    _ => None,
                };
                let detail = why.filter(|w| !w.trim().is_empty()).map(|said| {
                    div()
                        .debug_selector(move || format!("away-why-{}", id.as_uuid()))
                        .min_w_0()
                        .text_color(hsla(s.text_muted))
                        .child(ChromeText::new(said, px(theme.typography.small()), k).fill())
                });
                (detail, None)
            }
            _ => (None, None),
        };
        let away = matches!(state, BodyState::Away(_)).then(|| self.away_actions(tile.worker));
        let away = away.unwrap_or_default();
        let in_menu = |run: MenuRun| {
            move |_ev: &gpui::ClickEvent, window: &mut Window, cx: &mut App| run(window, cx)
        };
        let grant = away.grant.map(|grant| {
            button("copy-grant", COPY_GRANT).on_click(cx.listener(move |this, _ev, _w, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(grant.to_string()));
                this.show_notice(GRANT_COPIED.to_owned(), cx);
            }))
        });
        let wake = away.wake.map(|run| button("wake-worker", "Wake").on_click(in_menu(run)));
        let retry =
            away.retry.map(|run| button("retry-worker", "Retry now").on_click(in_menu(run)));
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
            add_worker::bar(theme, bar, "update-progress").map(|bar| {
                div()
                    .absolute()
                    .bottom_0()
                    .left(px(theme.spacing.md * k))
                    .right(px(theme.spacing.md * k))
                    .child(bar)
            })
        });
        if centred {
            let mark = crate::icons::notice_status(theme, status, hsla(status.ink(theme)), k);
            let buttons = div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap(px(theme.spacing.xs * k))
                .mt(px(theme.spacing.sm * k))
                .when_some(restart, gpui::ParentElement::child)
                .when_some(close, gpui::ParentElement::child)
                .when_some(update_button, gpui::ParentElement::child)
                .when_some(copy, gpui::ParentElement::child)
                .when_some(grant, gpui::ParentElement::child)
                .when_some(wake, gpui::ParentElement::child)
                .when_some(retry, gpui::ParentElement::child);
            let notice = kit::notice(theme, k, mark, text.clone(), None)
                .id("state")
                .debug_selector(move || format!("state-{}", id.as_uuid()))
                .role(Role::Status)
                .aria_label(text)
                .relative()
                .text_size(px(theme.typography.small() * k))
                .when_some(detail, gpui::ParentElement::child)
                .child(buttons)
                .when_some(bar, gpui::ParentElement::child);
            return div()
                .debug_selector(move || format!("state-centred-{}", id.as_uuid()))
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(notice)
                .into_any_element();
        }
        let actions = restart.is_some()
            || close.is_some()
            || copy.is_some()
            || update_button.is_some()
            || grant.is_some()
            || wake.is_some()
            || retry.is_some();
        // What is so, then why, the why ending in its own ellipsis; the buttons go on to a line
        // of their own where the tile is too narrow for both.
        let said = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .child(crate::icons::status_icon(
                theme,
                status,
                px(theme.typography.icon() * k),
                hsla(status.ink(theme)),
            ))
            .child(
                div().flex_none().max_w_full().child(
                    ChromeText::new(text.clone(), px(theme.typography.small()), k)
                        .fill()
                        .zooming(chrome.zooming),
                ),
            )
            .when_some(detail, gpui::ParentElement::child);
        let pill = div()
            .id("state")
            .debug_selector(move || format!("state-{}", id.as_uuid()))
            .role(Role::Status)
            .aria_label(text)
            .occlude()
            .max_w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap(px(theme.spacing.sm * k))
            .pl(px(theme.spacing.md * k))
            .pr(px(if actions { theme.spacing.xs } else { theme.spacing.md } * k))
            .py(px(theme.spacing.xs * k))
            .rounded(px(theme.radii.md * k))
            .map(|el| kit::elevate(el, theme))
            .text_size(px(theme.typography.small() * k))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text_secondary))
            .child(said)
            .when_some(restart, gpui::ParentElement::child)
            .when_some(close, gpui::ParentElement::child)
            .when_some(update_button, gpui::ParentElement::child)
            .when_some(copy, gpui::ParentElement::child)
            .when_some(grant, gpui::ParentElement::child)
            .when_some(wake, gpui::ParentElement::child)
            .when_some(retry, gpui::ParentElement::child)
            .when_some(bar, |el, bar| el.relative().child(bar));
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(theme.spacing.lg * k))
            .px(px(theme.spacing.md * k))
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
        cx: &Draw<'_, Self>,
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
            .hover(|s| s.bg(hsla(theme.surfaces.hover)))
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _ev, _w, cx| {
                    this.drag_out(worker, &path, cx);
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
        _cx: &Draw<'_, Self>,
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
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let bare = chrome.k < SHAPES_BELOW;
        // Once the overview rests, a tile of words is wholly under its summary: its body is not
        // drawn at all, so a flood in a shell nobody can read costs no frame. The focused one
        // is, as it keeps the keyboard.
        if bare && self.summed_up(placed, item) {
            let hidden = div().flex_1().min_h_0().into_any_element();
            return self.render_miniature(placed, item, hidden, cx);
        }
        let state = self.body_state(placed.tile, item).filter(|_| !bare);
        let empty = std::cell::Cell::new(false);
        let content = self.render_content(placed, item, chrome, &empty, window, cx);
        if bare {
            return self.render_miniature(placed, item, content, cx);
        }
        let content = self.set_back_in_doubt(placed.tile, content);
        let Some(state) = state else { return content };
        // A body with nothing in it says what is so in its middle, not at its foot.
        let pill = self.render_state_pill(placed.tile, &state, empty.get(), chrome, cx);
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

    /// A body that may show what is no longer so ([`Self::set_back`]), set back: its worker
    /// away, its link in doubt after a resume, its picture from a link that has gone. It says
    /// so in the frame the doubt starts, and comes back the frame the probe answers or the new
    /// link's stream shows.
    fn set_back_in_doubt(&self, tile: TileRef, content: gpui::AnyElement) -> gpui::AnyElement {
        if !self.set_back(tile) {
            return content;
        }
        div()
            .debug_selector(move || format!("in-doubt-{}", tile.item.as_uuid()))
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .opacity(slopty_theme::alpha::STRONG)
            .child(content)
            .into_any_element()
    }

    /// A body with nothing to show yet, saying why in one muted line: at once for a state
    /// that lasts (let go off screen), and only past [`crate::screen::LOADING_GRACE`]
    /// for one the worker is about to end (opening, reading, attaching), so a fast answer
    /// never flashes a word. Blank in the overview's shapes-only zoom.
    fn waiting_body(&self, item: &Item, wait: Wait, k: f32) -> gpui::AnyElement {
        let theme = &self.theme;
        let id = item.id;
        let (text, loading) = match wait {
            Wait::Lasting(text) => (text, false),
            Wait::Loading(text) => (text, true),
        };
        let said = (k >= SHAPES_BELOW).then(|| {
            let said = div()
                .id("waiting-words")
                .debug_selector(move || format!("waiting-{}", id.as_uuid()))
                .role(Role::Status)
                .aria_label(text.clone())
                .text_size(px(theme.typography.small() * k))
                .text_color(hsla(theme.surfaces.text_muted))
                .font_family(theme.typography.ui_family.clone())
                .child(text);
            if loading {
                let grace = SharedString::from(format!("grace-{}", id.as_uuid()));
                crate::screen::AfterGrace::new(grace, said).into_any_element()
            } else {
                said.into_any_element()
            }
        });
        div()
            .id(SharedString::from(format!("waiting-{}", id.as_uuid())))
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .children(said)
            .into_any_element()
    }

    /// A remote window or display on its way, past the loading grace: the calm mark, then
    /// "Opening Safari" and the worker under it, one composed block in the body's middle. The
    /// mark is the body's, not the header's, until the first frame: a sentence alone in the
    /// void with a spinner far above it read as two things waiting.
    fn opening_body(&self, tile: TileRef, item: &Item, k: f32) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = item.id;
        let what = self.derived_title(item);
        let worker = self.workers.get(&tile.worker).map(|w| w.name.clone());
        let said = SharedString::from(match &worker {
            Some(worker) => format!("Opening {what} on {worker}…"),
            None => format!("Opening {what}…"),
        });
        let block = (k >= SHAPES_BELOW).then(|| {
            let mark = crate::icons::notice_status(theme, Status::Running, hsla(s.text_muted), k);
            let block = kit::notice(
                theme,
                k,
                mark,
                format!("Opening {what}"),
                worker.map(SharedString::from),
            )
            .debug_selector(move || format!("waiting-{}", id.as_uuid()))
            .id("opening")
            .role(Role::Status)
            .aria_label(said);
            let grace = SharedString::from(format!("grace-{}", id.as_uuid()));
            crate::screen::AfterGrace::new(grace, block)
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

    /// A remote window or display the worker could not open: an end, said where the wait was,
    /// so nothing in the pane still looks like it waits. The error's mark, what is so as a task's
    /// title, why under it in the chrome's words, and the way to pick another in its place
    /// ([`Self::add_screen_item`] puts the pick where this one was).
    fn failed_body(
        &self,
        tile: TileRef,
        item: &Item,
        why: &slopty_proto::screen::ScreenFailure,
        k: f32,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let ty = &theme.typography;
        let roles = theme.roles();
        let id = item.id;
        let machine = self.workers.get(&tile.worker).map_or("The machine", |w| w.name.as_str());
        let (title, detail, pick) = failed_words(item, &self.derived_title(item), why, machine);
        let block = (k >= SHAPES_BELOW).then(|| {
            let choose = kit::button(theme, "choose-another", pick, kit::ButtonKind::Secondary)
                .h(px(theme.density.control * k))
                .px(px(theme.spacing.md * k))
                .text_size(px(ty.ui_size * k))
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.focus_tile(tile, cx);
                    this.add_window(&AddWindow, window, cx);
                }));
            div()
                .id("failed")
                .debug_selector(move || format!("failed-{}", id.as_uuid()))
                .role(Role::Status)
                .aria_label(SharedString::from(format!("{title}. {detail}")))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(theme.spacing.xs * k))
                .max_w_full()
                .px(px(theme.spacing.inset() * k))
                .font_family(ty.ui_family.clone())
                .text_center()
                .child(
                    crate::icons::icon(
                        theme,
                        Symbol::ExclamationmarkTriangle,
                        IconSize::Inline,
                        hsla(s.error),
                    )
                    .size(px(ty.icon_large() * k)),
                )
                .child(kit::typed(div(), roles.task_title, k).text_color(hsla(s.text)).child(title))
                .child(
                    kit::typed(div(), roles.chrome, k)
                        .text_color(hsla(s.text_secondary))
                        .child(detail),
                )
                .child(div().pt(px(theme.spacing.sm * k)).child(choose))
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the body's inputs, and `empty`, where it says it drew nothing for the pill"
    )]
    fn render_content(
        &self,
        placed: &Placed,
        item: &Item,
        chrome: Chrome,
        empty: &std::cell::Cell<bool>,
        window: &Window,
        cx: &Draw<'_, Self>,
    ) -> gpui::AnyElement {
        let theme = &self.theme;
        let k = chrome.k;
        let worker_up = self.workers.get(&placed.tile.worker).is_some_and(|w| w.link.is_some());
        let rest_w = (placed.target.w * k).max(1.0);
        let rest_h = ((placed.target.h - self.header_h()) * k).max(1.0);
        let fixed = |el: gpui::AnyElement| {
            div()
                .flex_1()
                .w_full()
                .relative()
                .overflow_hidden()
                .child(div().absolute().top_0().left_0().w(px(rest_w)).h(px(rest_h)).child(el))
                .into_any_element()
        };
        // The state pill says what is wrong; the body under it stays empty, and says so to
        // `empty`, so the pill stands in its middle.
        let well = || {
            empty.set(true);
            div().flex_1().w_full().into_any_element()
        };
        match &item.kind {
            ItemKind::Terminal { session } => match self.terminals.get(session) {
                _ if self.board_shown(*session)
                    && let Some(board) = self.board_view(*session).cloned() =>
                {
                    let handed = Handed::Board { zoom: k };
                    self.hand_over(cx, &board, handed, move |v, cx| v.set_zoom(k, cx));
                    let body = self.body_view(&board, placed, cx);
                    fixed(body)
                }
                _ if self.terminals.contains_key(session)
                    && self.face_shown(*session)
                    && self.body_state(placed.tile, item).is_none() =>
                {
                    let width = placed.target.w;
                    let handed = Handed::Face { zoom: k, width };
                    // The tile's header already says the title, the agent and its state, so the
                    // thread view draws none. Its view is made in the frame after it is wanted.
                    let Some(thread) = self.thread_face(*session) else { return well() };
                    self.hand_over(cx, thread, handed, move |v, cx| {
                        v.set_layout(k, width, cx);
                        v.set_header(false, cx);
                    });
                    fixed(self.body_view(thread, placed, cx))
                }
                Some(view) => {
                    let covered = self.body_state(placed.tile, item).is_some();
                    let zooming = chrome.zooming;
                    let handed = Handed::Shell { zoom: k, covered, zooming };
                    self.hand_over(cx, view, handed, move |v, _| {
                        v.set_zoom(k);
                        v.set_covered(covered);
                        v.set_zooming(zooming);
                    });
                    let body = self.body_view(view, placed, cx);
                    fixed(body)
                }
                None if !worker_up => well(),
                None if self.summary(*session).is_some() => {
                    self.waiting_body(item, Wait::Loading(ATTACHING.into()), k)
                }
                None => well(),
            },
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                match self.screens.get(&item.id) {
                    Some(_) if self.popouts.holds(item.id) => {
                        let id = item.id;
                        let wait = Wait::Lasting(super::popout::IN_OWN_WINDOW.into());
                        let popped = div()
                            .id(SharedString::from(format!("popped-{}", id.as_uuid())))
                            .role(Role::Button)
                            .aria_label(super::popout::SHOW_OWN_WINDOW)
                            .flex_1()
                            .w_full()
                            .flex()
                            .child(self.waiting_body(item, wait, k));
                        tab_stop(popped, self.theme.surfaces.focus)
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                this.raise_popped(id, cx);
                            }))
                            .into_any_element()
                    }
                    Some(view) => {
                        let painted = placed.rect.w * window.scale_factor();
                        let handed = Handed::Stream { painted };
                        self.hand_over(cx, view, handed, move |v, cx| {
                            v.set_painted_width(painted, cx);
                        });
                        let body = self.body_view(view, placed, cx);
                        div().flex_1().w_full().overflow_hidden().child(body).into_any_element()
                    }
                    None if !worker_up => well(),
                    None if self.parked.contains(&item.id) => {
                        self.waiting_body(item, Wait::Lasting(PAUSED.into()), k)
                    }
                    None => match self
                        .workers
                        .get(&placed.tile.worker)
                        .and_then(|w| w.failed_opens.get(&item.id))
                    {
                        Some(why) => self.failed_body(placed.tile, item, why, k, cx),
                        None => self.opening_body(placed.tile, item, k),
                    },
                }
            }
            ItemKind::Browser { .. } => match self.browsers.get(&item.id) {
                Some(view) => {
                    // Scaled, the page would lay itself out small: its picture shows instead.
                    let live = k >= 1.0 && !self.layout.overview_open();
                    self.hand_over(cx, view, Handed::Page { live }, move |v, cx| {
                        v.set_live(live, cx);
                    });
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(view.clone())
                        .into_any_element()
                }
                None => self.waiting_body(item, Wait::Loading(OPENING.into()), k),
            },
            ItemKind::File { .. } => match self.files.get(&item.id) {
                Some(view) => {
                    let (pad, text_size) = (theme.spacing.inset(), theme.typography.mono_size);
                    let handed = Handed::Text { zoom: k, pad, size: text_size };
                    self.hand_over(cx, view, handed, move |v, _| v.set_layout(k, pad, text_size));
                    let body = self.body_view(view, placed, cx);
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body)
                        .into_any_element()
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(READING.into()), k),
            },
            ItemKind::Folder { .. } => match self.folders.get(&item.id) {
                Some(view) => {
                    self.hand_over(cx, view, Handed::Folder { zoom: k }, move |v, _| v.set_zoom(k));
                    let body = self.body_view(view, placed, cx);
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .overflow_hidden()
                        .child(body)
                        .into_any_element()
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(READING.into()), k),
            },
            ItemKind::Review { thread } => match self.review_of(*thread).cloned() {
                Some(view) => {
                    let (width, height) = (placed.target.w, placed.target.h - self.header_h());
                    let handed = Handed::Review { zoom: k, width, height };
                    self.hand_over(cx, &view, handed, move |v, cx| {
                        v.set_layout(k, width, height, cx);
                    });
                    fixed(self.body_view(&view, placed, cx))
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(REVIEW.into()), k),
            },
            ItemKind::Changes { .. } => match self.changes_view(item.id).cloned() {
                Some(view) => {
                    let (width, height) = (placed.target.w, placed.target.h - self.header_h());
                    let handed = Handed::Review { zoom: k, width, height };
                    self.hand_over(cx, &view, handed, move |v, cx| {
                        v.set_layout(k, width, height, cx);
                    });
                    fixed(self.body_view(&view, placed, cx))
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(CHANGES.into()), k),
            },
            // The thread view, under the tile's header, which says its title already.
            ItemKind::Thread { .. } => match self.thread_item(item.id).cloned() {
                Some(view) => {
                    let width = placed.target.w;
                    let handed = Handed::Face { zoom: k, width };
                    self.hand_over(cx, &view, handed, move |v, cx| {
                        v.set_layout(k, width, cx);
                        v.set_header(false, cx);
                    });
                    fixed(self.body_view(&view, placed, cx))
                }
                None if !worker_up => well(),
                None => self.waiting_body(item, Wait::Loading(OPENING.into()), k),
            },
        }
    }
}

/// Where a shell is, beside a title that may already name the directory it stands in: then
/// nothing, since the title says it. The directory above it read as the cwd:
/// "drop-here ~" beside a prompt in "~/drop-here".
pub(super) fn place_beside(place: String, title: &str) -> Option<String> {
    let last = place.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    (last != title && place != title).then_some(place)
}

/// A header action in words ("Take", "Mute", an upload's progress): the bare
/// [`kit::pill_frame`], a mark before its words where it has one (an upload's ring), its words
/// in its tone, and the `raised` fill under the pointer, so it
/// stands as tall as the state's pill beside it.
/// A header holds one filled chip at most, the state's (the agent's pill); every other word
/// in it is a ghost, so the state is the one shape that stands out. Scaled by the chrome's
/// `k`. Its id is scoped by the tile's.
fn pill(
    part: impl Into<SharedString>,
    item: ItemId,
    (mark, label): (Option<gpui::AnyElement>, impl Into<SharedString>),
    tone: slopty_theme::Rgb,
    theme: &Theme,
    chrome: Chrome,
) -> Stateful<Div> {
    let k = chrome.k;
    let (hover, pressed) = (theme.surfaces.hover, theme.surfaces.pressed);
    let part: SharedString = part.into();
    let selector = format!("{part}-{}", item.as_uuid());
    kit::pill_frame(theme, k)
        .id(part)
        .debug_selector(move || selector)
        .flex_none()
        .text_color(hsla(tone))
        .cursor_pointer()
        .hover(move |el| el.bg(hsla(hover)))
        .active(move |el| el.bg(hsla(pressed)))
        .when(mark.is_some(), |el| el.gap(px(theme.spacing.xs * k)))
        .children(mark)
        .child(ChromeText::new(label, px(theme.typography.small()), k).zooming(chrome.zooming))
}

/// A header's title tone: focus is said by tone as well as weight. The focused tile's title
/// leads in primary text at the medium weight; every other steps back a tier, to the secondary
/// tone at the regular weight, so a wall of tiles reads as titles still and one of them as the
/// one in hand, not as one bold word among greys. The place after a title stays muted in both.
/// A tab row's tabs follow it too.
pub(super) const fn title_ink(theme: &Theme, focused: bool) -> slopty_theme::Rgb {
    if focused { theme.surfaces.text } else { theme.surfaces.text_secondary }
}
