//! slopty: the session picker and Go to Directory, one card under the titlebar's identity
//! (`docs/decisions/layout.md`, "The picker and the palette are one keyboard door to everything").
//!
//! - **⌘K, or a press on the identity**: a "Filter or create…" field over every session on every
//!   machine. With nothing typed it lists the sessions the sidebar lists, in its order, under a
//!   header per machine while they span more than one (this Mac's first). The session on screen
//!   wears a ✓ and the first nine rows ⌘1 to ⌘9. Typed words filter every session, the archived
//!   too, by title, project, branch, machine and state (its mark's word: "needs you", "working",
//!   "failed", "done"). Then come what was said in them (`SearchChats`) and Claude Code's past
//!   sessions in the projects (`slopty.PastSessions`), which open where they stopped. Words that
//!   name no session start one in the project on screen, the agent ⇥ picks, titled as typed:
//!   the title is Slopty's, never typed to the agent.
//! - **⇧⌘G, Go to Directory**: the same card over a machine's folders, its projects and the
//!   folders gone to most first, filtered by typing (its home index, `SearchHomeFolders`) or a
//!   typed path. ⏎ opens a terminal there, or the agent ⇥ picks; ⌘[ and ⌘] step the machine.
//!
//! It works from the keyboard alone and answers nothing for the person.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::Focusable as _;
use gpui::prelude::*;
use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Pixels, Point, SharedString, Subscription,
    Task, Window, WindowId, actions, div, px,
};
use slopty_view_proto::{HarnessId, HomeFolderMatch, HomeFolderSearchResult};

use super::Shell;
use super::marks::Mark;
use super::new_session::{AGENTS, Agent, Start, agent_mark};
use super::past_sessions::{Listed, PastRow};
use super::session_search::{ChatWordsMatch, ChatWordsQuery};
use super::sidebar_status::{self, Filter, INACTIVE};
use super::thread_search;
use crate::composer::{ComposerInput, ComposerInputEvent};
use crate::icons::{self, icon};
use crate::popover;
use crate::state::AppState;
use crate::theme::{Theme, hairline};

actions!(
    shell,
    [
        // slopty: the session picker, ⌘K.
        OpenPicker,
        // slopty: Go to Directory, ⇧⌘G while no find field is open (`docs/decisions/layout.md`).
        GoToDirectory
    ]
);

/// What the card lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// The sessions, ⌘K.
    Sessions,
    /// A machine's folders, ⇧⌘G.
    Directories,
}

/// One row of the card.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Row {
    /// A machine's header over its sessions.
    Header(SharedString),
    Session(SessionRow),
    /// A session whose words match: its title, its program, and the words with its chat.
    Said { title: SharedString, harness: Option<HarnessId>, found: ChatWordsMatch },
    /// A Claude Code session its machine keeps that is no session here yet, and its project.
    Past { past: PastRow, project: Option<SharedString> },
    /// Start a session titled as typed.
    Create(String),
    /// A folder on the machine.
    Folder(Folder),
    /// Why a search afar found nothing, in the machine's words.
    Note(SharedString),
}

impl Row {
    /// Whether the cursor stops on it: a header is not a row one picks.
    fn pickable(&self) -> bool {
        !matches!(self, Self::Header(_) | Self::Note(_))
    }
}

/// A session's row.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SessionRow {
    pub chat_id: String,
    pub title: SharedString,
    /// Its project, and its machine while the rows span machines and no header says it.
    pub place: Option<SharedString>,
    pub mark: Mark,
    pub harness: Option<HarnessId>,
    /// It is the session on screen.
    pub current: bool,
    /// Its ⌘ digit, on the first nine.
    pub digit: Option<u8>,
    /// Its machine is out of reach: the row shows the last word heard from there.
    pub cached: bool,
}

/// A folder Go to Directory offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Folder {
    /// Absolute, on its machine.
    pub path: String,
    /// What the row says: home-relative where it can.
    pub shown: SharedString,
    /// One of the person's projects.
    pub project: bool,
    /// Typed, not found: the path as the person wrote it.
    pub typed: bool,
}

/// How long typing pauses before past sessions are asked for.
const ASK_AFTER: Duration = Duration::from_millis(150);
/// How many past sessions one ask lists.
const PAST_LIMIT: usize = 8;
/// How many sessions typed words list.
const TYPED_LIMIT: usize = 30;
/// How many sessions whose words match it lists (the palette's five).
const SAID_LIMIT: usize = 5;
/// How many folders one ask of the home index lists.
const FOLDER_LIMIT: u16 = 20;
/// Shorter words match nearly every folder under home; they are not sent.
const FOLDER_MIN_CHARS: usize = 2;
/// The pause before asking the home index again while it is still scanning, doubling to the cap.
const FOLDER_RETRY: Duration = Duration::from_millis(500);
const FOLDER_RETRY_MAX: Duration = Duration::from_secs(2);
/// The card's width: Rex's picker, a little wider for a machine beside each title.
const PICKER_WIDTH: f32 = 440.0;
/// A one-line row's height; two lines for words found and past sessions.
const ROW_HEIGHT: f32 = 30.0;
const ROW_TWO_LINES: f32 = 44.0;
/// The rows it shows before it scrolls.
const ROWS_SHOWN: f32 = 11.0;

thread_local! {
    /// Where each window's titlebar identity was last drawn: the card hangs under it, whether a
    /// press or a key asked for it.
    static IDENTITY: RefCell<HashMap<WindowId, Bounds<Pixels>>> = RefCell::new(HashMap::new());
}

/// The titlebar's identity, when its row names a session, its place noted for the card that
/// hangs under it; with none, the card centres under the titlebar.
pub(super) fn identity_anchor(identity: Option<AnyElement>, window: &Window) -> Option<AnyElement> {
    let id = window.window_handle().window_id();
    let Some(identity) = identity else {
        IDENTITY.with(|at| at.borrow_mut().remove(&id));
        return None;
    };
    Some(
        div()
            .min_w_0()
            .flex()
            .on_children_prepainted(move |bounds, _, _| {
                IDENTITY.with(|at| match bounds.first() {
                    Some(bounds) => at.borrow_mut().insert(id, *bounds),
                    None => at.borrow_mut().remove(&id),
                });
            })
            .child(identity)
            .into_any_element(),
    )
}

/// Where the card's top left goes in `window`: under the identity, else centred under the
/// titlebar.
fn anchor(viewport: gpui::Size<Pixels>, window: &Window) -> Point<Pixels> {
    let centred = gpui::point((viewport.width - px(PICKER_WIDTH)) / 2.0, px(Theme::TITLEBAR_HEIGHT));
    let id = window.window_handle().window_id();
    // Held inside the window by an 8 pt margin, however far right the identity stands.
    let widest = (viewport.width - px(PICKER_WIDTH + 8.0)).max(px(8.0));
    IDENTITY.with(|at| at.borrow().get(&id).copied()).map_or(centred, |bounds| {
        gpui::point((bounds.left() - px(8.0)).clamp(px(8.0), widest), bounds.bottom() + px(6.0))
    })
}

/// The card's state while it is up.
pub(super) struct Picker {
    mode: Mode,
    search: Entity<ComposerInput>,
    previous_focus: Option<FocusHandle>,
    focus_pending: bool,
    /// The row at the cursor, of the rows as listed.
    active: usize,
    scroll: gpui::ScrollHandle,
    /// The agent a start takes, of [`AGENTS`].
    agent: usize,
    /// Sessions by their words.
    said: ChatWordsQuery,
    /// Past sessions, why the machines that could not answer did not, why some may be missing,
    /// the ask that listed them, and the ask on its way.
    past: Vec<PastRow>,
    past_failed: Option<String>,
    past_cut: Option<String>,
    past_asked: u64,
    past_task: Option<Task<()>>,
    /// Go to Directory's machine and its folders.
    device: Option<String>,
    folders: FolderSearch,
    /// The start on its way, and the one its machine refused, in its words.
    starting: Option<Task<()>>,
    refused: Option<String>,
    _events: Subscription,
}

/// A machine's folders as its home index answers.
#[derive(Default)]
struct FolderSearch {
    /// Bumped per ask; an older answer is dropped.
    generation: u64,
    /// The folders gone to most, from the warming answer.
    visited: Vec<HomeFolderMatch>,
    found: Vec<HomeFolderMatch>,
    /// An ask reached the machine, which holds its index until released.
    started: bool,
    /// The machine predates `SearchHomeFolders`.
    unsupported: bool,
    warm_task: Option<Task<()>>,
    task: Option<Task<()>>,
}

impl FolderSearch {
    /// The machine's home, as an answer's path less its home-relative part.
    fn home(&self) -> Option<String> {
        self.visited.iter().chain(&self.found).find_map(|found| {
            let relative = found.relative.trim_matches('/');
            let home = found.path.strip_suffix(relative)?.trim_end_matches('/');
            (!home.is_empty()).then(|| home.to_owned())
        })
    }
}

/// The sessions the card lists for `typed` at `now`, before what is searched for afar.
pub(super) fn session_rows(state: &AppState, typed: &str, now: DateTime<Utc>) -> Vec<Row> {
    let terms = thread_search::search_terms(typed);
    let local = state.local_device_id.as_deref();
    let current = state.selected_chat.as_deref();
    let mut rows: Vec<SessionRow> = if terms.is_empty() {
        let (groups, _) = sidebar_status::list(state, Filter::default(), now);
        groups
            .into_iter()
            .filter(|group| group.key != INACTIVE)
            .flat_map(|group| group.rows)
            .map(|row| session_row(state, &row.chat, row.mark, row.cached, current))
            .collect()
    } else {
        let scored = state
            .chats
            .iter()
            .filter(|chat| chat.is_top_level())
            .filter_map(|chat| {
                let mark = Mark::of(state.display_status_for(chat, now), state.session_for(&chat.id), now);
                let pull_request = state
                    .change_request_for_chat(chat)
                    .map(|pr| format!("#{} {} {} {}", pr.number, pr.title, pr.head_ref, pr.base_ref))
                    .unwrap_or_default();
                let fields = thread_search::ThreadFields {
                    title: chat.title.as_deref().unwrap_or("New session"),
                    project: state.space_for_chat(chat).map_or("", |space| space.display_name()),
                    branch: crate::change_requests::conversation_branch(chat, &state.spaces).unwrap_or(""),
                    device: state.device_name(&chat.device_id).unwrap_or(""),
                    pull_request: &pull_request,
                    archived: chat.archived,
                };
                let score = thread_search::score_session(&fields, mark.word().unwrap_or(""), &terms)?;
                let active = chat.last_message_at.unwrap_or(chat.created_at).timestamp_millis();
                let cached = sidebar_status::out_of_reach(state, &chat.device_id, now);
                Some((session_row(state, chat, mark, cached, current), score, active))
            })
            .collect();
        thread_search::rank(scored, TYPED_LIMIT)
    };
    // Untyped, the rows stand under their machines' headers, this Mac's first, each machine's in
    // the list's order, once they span more than one; typed, best first, each saying its machine
    // whenever the sessions span more than one, however few of them match.
    let mut machines: Vec<String> = Vec::new();
    for row in &rows {
        let device = device_of(state, &row.chat_id);
        if !machines.contains(&device) {
            machines.push(device);
        }
    }
    let spans = if terms.is_empty() {
        machines.len() > 1
    } else {
        let mut devices = state.chats.iter().filter(|chat| chat.is_top_level()).map(|chat| chat.device_id.as_str());
        devices.next().is_some_and(|first| devices.any(|device| device != first))
    };
    if spans && terms.is_empty() {
        machines.sort_by_key(|device| (Some(device.as_str()) != local, machine_name(state, device)));
        rows.sort_by_key(|row| {
            let device = device_of(state, &row.chat_id);
            machines.iter().position(|machine| *machine == device)
        });
    }
    for (ix, row) in rows.iter_mut().enumerate() {
        row.digit = u8::try_from(ix + 1).ok().filter(|digit| *digit <= 9);
        if spans && !terms.is_empty() {
            let machine = machine_name(state, &device_of(state, &row.chat_id));
            row.place = Some(match row.place.take() {
                Some(project) => format!("{project} \u{b7} {machine}").into(),
                None => machine.into(),
            });
        }
    }
    let mut out = Vec::with_capacity(rows.len() + machines.len());
    let mut under: Option<String> = None;
    for row in rows {
        let device = device_of(state, &row.chat_id);
        if spans && terms.is_empty() && under.as_deref() != Some(device.as_str()) {
            out.push(Row::Header(machine_name(state, &device).into()));
            under = Some(device);
        }
        out.push(Row::Session(row));
    }
    out
}

/// A session's row, its project for its place.
fn session_row(
    state: &AppState,
    chat: &slopty_view_proto::Chat,
    mark: Mark,
    cached: bool,
    current: Option<&str>,
) -> SessionRow {
    let project = state.space_for_chat(chat).map(|space| state.representative_space(space).display_name().to_owned());
    SessionRow {
        chat_id: chat.id.clone(),
        title: crate::transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into(),
        place: project.map(SharedString::from),
        mark,
        harness: chat.config.as_ref().map(|config| config.harness),
        current: current == Some(chat.id.as_str()),
        digit: None,
        cached,
    }
}

fn device_of(state: &AppState, chat_id: &str) -> String {
    state.chats.iter().find(|chat| chat.id == chat_id).map(|chat| chat.device_id.clone()).unwrap_or_default()
}

/// A machine as a header says it: this Mac by that name, the others by theirs.
fn machine_name(state: &AppState, device: &str) -> String {
    if cfg!(target_os = "macos") && state.local_device_id.as_deref() == Some(device) {
        return "This Mac".to_owned();
    }
    state.device_name(device).unwrap_or("?").to_owned()
}

/// The folders Go to Directory lists on `device` for `typed`: a typed path first, then the
/// machine's projects that match, then what its home index found (or, untyped, the folders gone
/// to most), each folder once.
pub(super) fn folder_rows(state: &AppState, device: &str, typed: &str, search: &FolderSearchView<'_>) -> Vec<Row> {
    let typed = typed.trim();
    let words = thread_search::search_terms(typed);
    let home = search.home.as_deref();
    let shown = |path: &str| -> SharedString {
        match home.and_then(|home| path.strip_prefix(home)) {
            Some("") => "~".into(),
            Some(rest) if rest.starts_with('/') => format!("~{rest}").into(),
            _ => path.to_owned().into(),
        }
    };
    let mut folders: Vec<Folder> = Vec::new();
    let mut push = |folder: Folder| {
        if !folders.iter().any(|known| known.path == folder.path) {
            folders.push(folder);
        }
    };
    if crate::pickers::is_typed_path(typed)
        && let Some(path) = crate::pickers::typed_path_target(typed, home)
    {
        push(Folder { shown: shown(&path), path, project: false, typed: true });
    }
    let said = |path: &str, name: &str| {
        let haystack = format!("{} {}", name.to_lowercase(), path.to_lowercase());
        words.iter().all(|word| haystack.contains(word.as_str()))
    };
    for space in state.spaces.iter().filter(|space| space.device_id == device) {
        if words.is_empty() || said(&space.path, space.display_name()) {
            push(Folder { shown: shown(&space.path), path: space.path.clone(), project: true, typed: false });
        }
    }
    let found = if words.is_empty() { search.visited } else { search.found };
    for found in found {
        push(Folder { shown: shown(&found.path), path: found.path.clone(), project: false, typed: false });
    }
    folders.into_iter().map(Row::Folder).collect()
}

/// What [`folder_rows`] reads of a machine's home index.
pub(super) struct FolderSearchView<'a> {
    pub home: Option<String>,
    pub visited: &'a [HomeFolderMatch],
    pub found: &'a [HomeFolderMatch],
}

/// Whether the keyboard is in a terminal's or a Changes pane's find field. A key bound with no
/// context outranks one bound in a pane's context once the keyboard is deeper than the pane, so
/// there Go to Directory passes ⇧⌘G on to the pane's own Find Previous.
fn in_a_find_field(window: &Window) -> bool {
    let stack = window.context_stack();
    let within = |name: &str| stack.iter().any(|context| context.contains(name));
    within("TerminalSearch") || (within("Changes") && within("PaletteSearch"))
}

impl Shell {
    /// `root` taking the picker's actions. Over another overlay that holds the keyboard a key
    /// waits; the card's own keys shut it or turn it to the other list.
    pub(super) fn picker_actions<E: InteractiveElement>(&self, root: E, cx: &mut Context<Self>) -> E {
        root.on_action(cx.listener(|this, _: &OpenPicker, window, cx| {
            if this.overlay_owns_keyboard() && !this.picker_open() {
                return;
            }
            this.toggle_picker(Mode::Sessions, window, cx);
        }))
        .on_action(cx.listener(|this, _: &GoToDirectory, window, cx| {
            if this.overlay_owns_keyboard() && !this.picker_open() {
                return;
            }
            if in_a_find_field(window) {
                cx.propagate();
                return;
            }
            this.toggle_picker(Mode::Directories, window, cx);
        }))
    }

    /// Whether the card is up: it holds the keyboard.
    pub(super) fn picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// The session picker (⌘K, the identity's press) or Go to Directory (⇧⌘G); the same key
    /// again shuts it.
    pub(super) fn toggle_picker(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.as_ref().is_some_and(|picker| picker.mode == mode) {
            self.close_picker(Some(window), cx);
            return;
        }
        self.close_picker(None, cx);
        self.command_palette = None;
        crate::live::chrome::release_held_keys(cx);
        let placeholder = match mode {
            Mode::Sessions => "Filter or create\u{2026}",
            Mode::Directories => "Go to\u{2026}",
        };
        let search = cx.new(|cx| ComposerInput::with_context(placeholder, "PaletteSearch", cx));
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.picker_typed(cx);
            }
        });
        let (device, agent) = {
            let state = self.state.read(cx);
            let shown = state.selected_chat_row();
            let device = shown
                .map(|chat| chat.device_id.clone())
                .or_else(|| state.local_device_id.clone())
                .or_else(|| state.devices.first().map(|device| device.id.clone()));
            // A start takes the agent on screen; Go to Directory a terminal, as Rex's.
            let agent = match mode {
                Mode::Directories => AGENTS.len() - 1,
                Mode::Sessions => shown
                    .and_then(|chat| chat.config.as_ref())
                    .and_then(|config| AGENTS.iter().position(|agent| agent.id == config.harness.as_str()))
                    .unwrap_or(0),
            };
            (device, agent)
        };
        self.picker = Some(Picker {
            mode,
            search,
            previous_focus: window.focused(cx),
            focus_pending: true,
            active: 0,
            scroll: gpui::ScrollHandle::new(),
            agent,
            said: ChatWordsQuery::default(),
            past: Vec::new(),
            past_failed: None,
            past_cut: None,
            past_asked: 0,
            past_task: None,
            device,
            folders: FolderSearch::default(),
            starting: None,
            refused: None,
            _events: events,
        });
        if mode == Mode::Directories {
            self.ask_folders(true, cx);
        }
        self.picker_land_cursor(cx);
        cx.notify();
    }

    /// Shut the card, the keyboard back where it was, the machine's home index released.
    pub(super) fn close_picker(&mut self, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.take() else { return };
        self.release_folders(&picker, cx);
        if let (Some(window), Some(focus)) = (window, picker.previous_focus) {
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// The words changed: the cursor back to the first row, and what is searched afar asked again.
    fn picker_typed(&mut self, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let Some(picker) = self.picker.as_mut() else { return };
        picker.active = 0;
        picker.refused = None;
        picker.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        let typed = picker.search.read(cx).text().trim().to_owned();
        match picker.mode {
            Mode::Sessions => {
                picker.said.search(engine, typed, |shell| shell.picker.as_mut().map(|picker| &mut picker.said), cx);
                self.ask_past(cx);
            }
            Mode::Directories => self.ask_folders(false, cx),
        }
        self.picker_land_cursor(cx);
        cx.notify();
    }

    /// The rows as listed now.
    fn picker_rows(&self, cx: &gpui::App) -> Vec<Row> {
        let Some(picker) = self.picker.as_ref() else { return Vec::new() };
        let typed = picker.search.read(cx).text().trim().to_owned();
        let state = self.state.read(cx);
        match picker.mode {
            Mode::Sessions => {
                let now = crate::clock::utc(cx);
                let mut rows = session_rows(state, &typed, now);
                if typed.is_empty() {
                    return rows;
                }
                let listed: Vec<String> = rows
                    .iter()
                    .filter_map(|row| match row {
                        Row::Session(session) => Some(session.chat_id.clone()),
                        _ => None,
                    })
                    .collect();
                // Words that name no session start one, first, where ⏎ finds it.
                if listed.is_empty() {
                    rows.push(Row::Create(typed));
                }
                let said = picker
                    .said
                    .results
                    .iter()
                    .filter(|found| !listed.contains(&found.chat_id))
                    .filter_map(|found| {
                        let chat = state.chats.iter().find(|chat| chat.id == found.chat_id)?;
                        let title = crate::transcript::single_line(chat.title.as_deref().unwrap_or("New session"));
                        let harness = chat.config.as_ref().map(|config| config.harness);
                        Some(Row::Said { title: title.into(), harness, found: found.clone() })
                    })
                    .take(SAID_LIMIT);
                rows.extend(said);
                let before = rows.len();
                rows.extend(picker.past.iter().filter(|past| past.chat_id.is_none()).map(|past| {
                    let project = past
                        .space_id
                        .as_deref()
                        .and_then(|id| state.space_row(id))
                        .map(|space| SharedString::from(state.representative_space(space).display_name().to_owned()));
                    Row::Past { past: past.clone(), project }
                }));
                // A machine that could not look says why when nothing was listed, and a search
                // that read only part of what there is says so either way.
                let why = if rows.len() == before {
                    picker.past_failed.as_ref().or(picker.past_cut.as_ref())
                } else {
                    picker.past_cut.as_ref()
                };
                if let Some(why) = why {
                    rows.push(Row::Note(why.clone().into()));
                }
                rows
            }
            Mode::Directories => {
                let Some(device) = picker.device.as_deref() else { return Vec::new() };
                let view = FolderSearchView {
                    home: picker.folders.home(),
                    visited: &picker.folders.visited,
                    found: &picker.folders.found,
                };
                folder_rows(state, device, &typed, &view)
            }
        }
    }

    /// Keep the cursor off a header.
    fn picker_land_cursor(&mut self, cx: &mut Context<Self>) {
        let rows = self.picker_rows(cx);
        let Some(picker) = self.picker.as_mut() else { return };
        if rows.get(picker.active).is_some_and(|row| !row.pickable())
            && let Some(next) = rows.iter().skip(picker.active).position(Row::pickable)
        {
            picker.active += next;
        }
    }

    /// Step the cursor, over headers, round the ends.
    fn move_picker(&mut self, down: bool, cx: &mut Context<Self>) {
        let rows = self.picker_rows(cx);
        let Some(picker) = self.picker.as_mut() else { return };
        let count = rows.len();
        if !rows.iter().any(Row::pickable) {
            return;
        }
        let mut at = picker.active.min(count - 1);
        loop {
            at = if down { (at + 1) % count } else { (at + count - 1) % count };
            if rows[at].pickable() {
                break;
            }
        }
        picker.active = at;
        picker.scroll.scroll_to_item(at);
        cx.notify();
    }

    /// The agent a start takes, stepped.
    fn step_picker_agent(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_mut() else { return };
        let count = AGENTS.len();
        picker.agent = if forward { (picker.agent + 1) % count } else { (picker.agent + count - 1) % count };
        picker.refused = None;
        cx.notify();
    }

    /// Go to Directory's machine, stepped through the machines in order.
    fn step_picker_machine(&mut self, forward: bool, cx: &mut Context<Self>) {
        let devices: Vec<String> = self.state.read(cx).devices.iter().map(|device| device.id.clone()).collect();
        let Some(picker) = self.picker.as_mut().filter(|picker| picker.mode == Mode::Directories) else { return };
        if devices.len() < 2 {
            return;
        }
        let at = picker.device.as_ref().and_then(|device| devices.iter().position(|d| d == device)).unwrap_or(0);
        let next = if forward { (at + 1) % devices.len() } else { (at + devices.len() - 1) % devices.len() };
        let Some(old) = self.picker.take() else { return };
        self.release_folders(&old, cx);
        self.picker = Some(Picker {
            device: Some(devices[next].clone()),
            folders: FolderSearch::default(),
            active: 0,
            refused: None,
            ..old
        });
        self.ask_folders(true, cx);
        cx.notify();
    }

    /// Pick the row at the cursor.
    fn pick_active(&mut self, cx: &mut Context<Self>) {
        let rows = self.picker_rows(cx);
        // The row drawn at the cursor, the last one when the list shrank under it.
        let at = self.picker.as_ref().map(|picker| picker.active.min(rows.len().saturating_sub(1)));
        let Some(row) = at.and_then(|at| rows.get(at)).filter(|row| row.pickable()).cloned() else { return };
        self.pick(row, cx);
    }

    /// Pick `row`: open its session, take its past session up, start one, or go to its folder.
    fn pick(&mut self, row: Row, cx: &mut Context<Self>) {
        match row {
            Row::Header(_) | Row::Note(_) => {}
            Row::Session(SessionRow { chat_id, .. }) | Row::Said { found: ChatWordsMatch { chat_id, .. }, .. } => {
                self.close_picker(None, cx);
                self.open_chat(chat_id, cx);
            }
            Row::Past { past, .. } => {
                // One a live Claude Code holds elsewhere stays where it is, and the card says why.
                if let Some(why) = past.held_elsewhere() {
                    self.picker_refused(why, cx);
                    return;
                }
                self.close_picker(None, cx);
                self.open_past_session(past, cx);
            }
            Row::Create(title) => {
                let Some((device_id, cwd)) = self.create_target(cx) else {
                    self.picker_refused("Add a project first: a session starts in one.".to_owned(), cx);
                    return;
                };
                self.picker_start(Start { device_id, cwd, agent: self.picker_agent(), title: Some(title) }, cx);
            }
            Row::Folder(folder) => {
                let Some(device_id) = self.picker.as_ref().and_then(|picker| picker.device.clone()) else { return };
                let start = Start { device_id, cwd: folder.path, agent: self.picker_agent(), title: None };
                self.picker_start(start, cx);
            }
        }
    }

    /// The ⌘ digit's row: the session it marks. The jump's binding takes the key before the
    /// card's own keys see it, so the shell hands its slot here while the card is up; the slot
    /// counts from 0 (⌘1), the digits from 1.
    pub(super) fn pick_digit(&mut self, slot: usize, cx: &mut Context<Self>) {
        let rows = self.picker_rows(cx);
        let digit = slot.checked_add(1);
        let row = rows
            .into_iter()
            .find(|row| matches!(row, Row::Session(session) if session.digit.map(usize::from) == digit));
        if let Some(row) = row {
            self.pick(row, cx);
        }
    }

    fn picker_agent(&self) -> Agent {
        AGENTS[self.picker.as_ref().map_or(0, |picker| picker.agent)]
    }

    /// Where words that name no session start one: the project on screen, else the folder of
    /// the session on screen, else the project the sidebar shows, else the first project.
    fn create_target(&self, cx: &gpui::App) -> Option<(String, String)> {
        let state = self.state.read(cx);
        if let Some(chat) = state.selected_chat_row() {
            if let Some(space) = state.space_for_chat(chat) {
                return Some((space.device_id.clone(), space.path.clone()));
            }
            if let Some(cwd) = chat.cwd.clone() {
                return Some((chat.device_id.clone(), cwd));
            }
        }
        let space = state
            .selected_space_row()
            .or_else(|| self.settings.space_filter.as_deref().and_then(|id| state.space_row(id)))
            .or_else(|| state.spaces.first())?;
        Some((space.device_id.clone(), space.path.clone()))
    }

    /// Start a session from the card; it shuts once the session opens.
    fn picker_start(&mut self, start: Start, cx: &mut Context<Self>) {
        if self.picker.as_ref().is_none_or(|picker| picker.starting.is_some()) {
            return;
        }
        let task = self.begin_session(
            start,
            |this, cx| this.close_picker(None, cx),
            |this, why, cx| this.picker_refused(why, cx),
            cx,
        );
        if let Some(picker) = self.picker.as_mut() {
            picker.starting = task;
            picker.refused = None;
        }
        cx.notify();
    }

    fn picker_refused(&mut self, why: String, cx: &mut Context<Self>) {
        if let Some(picker) = self.picker.as_mut() {
            picker.starting = None;
            picker.refused = Some(why);
        }
        cx.notify();
    }

    /// Ask every project's machine for its Claude Code sessions the words find, after a pause in
    /// typing; nothing while nothing is typed.
    fn ask_past(&mut self, cx: &mut Context<Self>) {
        let (engine, spaces) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.spaces.iter().map(|space| space.id.clone()).collect::<Vec<_>>())
        };
        let Some(picker) = self.picker.as_mut() else { return };
        picker.past_asked = picker.past_asked.wrapping_add(1);
        let words = picker.search.read(cx).text().trim().to_owned();
        let (Some(engine), false, false) = (engine, words.is_empty(), spaces.is_empty()) else {
            picker.past.clear();
            picker.past_failed = None;
            picker.past_cut = None;
            picker.past_task = None;
            return;
        };
        let asked = picker.past_asked;
        let params = serde_json::json!({ "query": words, "limit": PAST_LIMIT, "spaceIds": spaces });
        picker.past_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ASK_AFTER).await;
            let listed = engine.client().call_as::<Listed>(super::past_sessions::calls::LIST, params).await;
            let _gone = this.update(cx, |this, cx| {
                let Some(picker) = this.picker.as_mut().filter(|picker| picker.past_asked == asked) else { return };
                picker.past_task = None;
                // A machine that cannot answer lists nothing, and says why; the sessions here
                // still stand.
                let (past, failed, cut) = match listed {
                    Ok(listed) => {
                        let words: Vec<String> = listed
                            .failures
                            .into_iter()
                            .map(|failure| match failure.device {
                                Some(name) if !failure.words.contains(&name) => {
                                    format!("{name}: {}", failure.words)
                                }
                                _ => failure.words,
                            })
                            .collect();
                        (listed.sessions, (!words.is_empty()).then(|| words.join("; ")), listed.cut)
                    }
                    Err(error) => (Vec::new(), Some(error.to_string()), None),
                };
                picker.past = past;
                picker.past_failed = failed;
                picker.past_cut = cut;
                cx.notify();
            });
        }));
    }

    /// Ask the machine's home index for the folders the words find; `warm` first asks for the
    /// folders gone to most, while nothing is typed.
    fn ask_folders(&mut self, warm: bool, cx: &mut Context<Self>) {
        let (engine, local) = {
            let state = self.state.read(cx);
            (state.engine().cloned(), state.local_device_id.clone())
        };
        let Some(picker) = self.picker.as_mut() else { return };
        let Some(engine) = engine else { return };
        let device = picker.device.clone();
        let typed = picker.search.read(cx).text().trim().to_owned();
        let search = &mut picker.folders;
        search.generation += 1;
        search.found.clear();
        search.task = None;
        if search.unsupported {
            return;
        }
        if warm && !search.started {
            search.started = true;
            search.warm_task = Some(ask_home_folders(engine.clone(), device.clone(), local.clone(), String::new(), None, cx));
        }
        if typed.chars().count() < FOLDER_MIN_CHARS || crate::pickers::is_typed_path(&typed) {
            return;
        }
        search.started = true;
        let generation = Some(search.generation);
        search.task = Some(ask_home_folders(engine, device, local, typed, generation, cx));
    }

    /// Tell the machine the card is done with its home index.
    fn release_folders(&self, picker: &Picker, cx: &mut Context<Self>) {
        if !picker.folders.started {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        let params = target(picker.device.as_deref(), self.state.read(cx).local_device_id.as_deref());
        cx.spawn(async move |_, _| {
            let method = slopty_view_rpc::methods::RELEASE_HOME_FOLDER_SEARCH;
            if let Err(error) = engine.client().call(method, params).await {
                tracing::debug!(%error, "home folder search release failed");
            }
        })
        .detach();
    }

    /// The card under the identity, while it is up.
    pub(super) fn render_picker(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = Theme::of(cx).for_popup();
        let rows = self.picker_rows(cx);
        let now = crate::clock::utc(cx);
        // Where words that name no session would start one, as its row says it.
        let create_place = {
            let target = self.create_target(cx);
            let state = self.state.read(cx);
            target.map(|(device, cwd)| {
                let folder = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(&cwd).to_owned();
                state
                    .spaces
                    .iter()
                    .find(|space| space.device_id == device && space.path == cwd)
                    .map_or(folder, |space| space.display_name().to_owned())
            })
        };
        let machines: Vec<(String, String)> = {
            let state = self.state.read(cx);
            state.devices.iter().map(|device| (device.id.clone(), machine_name(state, &device.id))).collect()
        };
        let picker = self.picker.as_mut()?;
        if std::mem::take(&mut picker.focus_pending) {
            window.focus(&picker.search.focus_handle(cx), cx);
        }
        let mode = picker.mode;
        let search = picker.search.clone();
        let scroll = picker.scroll.clone();
        let active = picker.active.min(rows.len().saturating_sub(1));
        let agent = AGENTS[picker.agent];
        let starting = picker.starting.is_some();
        let refused = picker.refused.clone();
        let device = picker.device.clone();
        let typed = search.read(cx).text().trim().to_owned();
        let words = (!typed.is_empty()).then_some(typed.as_str());
        let mut children: Vec<AnyElement> = Vec::with_capacity(rows.len());
        for (ix, row) in rows.iter().enumerate() {
            let Some(drawn) = picker_row(row, ix == active, words, agent, create_place.as_deref(), starting, now, &theme)
            else {
                match row {
                    Row::Header(name) => children.push(header(name.clone(), &theme).into_any_element()),
                    Row::Note(why) => children.push(note(why.clone(), &theme).into_any_element()),
                    _ => {}
                }
                continue;
            };
            let target = row.clone();
            children.push(
                drawn
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(target.clone(), cx)))
                    .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                        if let Some(picker) = this.picker.as_mut()
                            && picker.active != ix
                        {
                            picker.active = ix;
                            cx.notify();
                        }
                    }))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty().then(|| {
            let (title, hint) = match mode {
                Mode::Sessions => ("No sessions yet", "Type a name to start one."),
                Mode::Directories if typed.is_empty() => ("No folders yet", "Type a folder\u{2019}s name or a path."),
                Mode::Directories => ("No folder matches", "Try another name, or a path."),
            };
            super::command_palette::palette_empty(&theme, title, hint).into_any_element()
        });
        let body = div()
            .id("picker-rows")
            .max_h(px(ROW_HEIGHT * ROWS_SHOWN + 12.0))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(1.0))
            .p(px(6.0))
            .children(children)
            .children(empty);
        // Go to Directory says its machine, one tab each while there are more than one.
        let machine_tabs = (mode == Mode::Directories && machines.len() > 1).then(|| {
            let mut tabs = div()
                .id("picker-machines")
                .flex_none()
                .px(px(10.0))
                .py(px(6.0))
                .flex()
                .items_center()
                .gap(px(4.0))
                .border_b_1()
                .border_color(hairline(0.06))
                .role(gpui::Role::TabList)
                .aria_label("Machine");
            for (ix, (id, name)) in machines.iter().enumerate() {
                let selected = device.as_deref() == Some(id.as_str());
                let target = id.clone();
                tabs = tabs.child(
                    crate::surface_chrome::tab(("picker-machine", ix), selected, &theme)
                        .px(px(8.0))
                        .role(gpui::Role::Tab)
                        .aria_label(SharedString::from(name.clone()))
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_machine(target.clone(), cx)))
                        .child(SharedString::from(name.clone())),
                );
            }
            tabs
        });
        let foot = picker_foot(mode, agent, refused, &theme, window, cx);
        let label = match mode {
            Mode::Sessions => "Sessions",
            Mode::Directories => "Go to Directory",
        };
        let card = popover::popover_card_flush(&theme)
            .id("picker-card")
            .debug_selector(|| "picker-card".into())
            // slopty: no focus of its own: a press on a row the machine refuses, or on a machine's
            // tab, leaves the keyboard in the search, so typing goes on filtering.
            .role(gpui::Role::Dialog)
            .aria_label(label)
            .w(px(PICKER_WIDTH))
            .key_context("SessionPicker")
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let command = keystroke.modifiers.platform;
                match keystroke.key.as_str() {
                    "up" | "down" => this.move_picker(keystroke.key == "down", cx),
                    "enter" => this.pick_active(cx),
                    "tab" => this.step_picker_agent(!keystroke.modifiers.shift, cx),
                    "escape" => this.close_picker(Some(window), cx),
                    "[" | "]" if command => this.step_picker_machine(keystroke.key == "]", cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_picker(Some(window), cx)))
            .child(
                div()
                    .h(px(44.0))
                    .flex_none()
                    .px(px(14.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .border_b_1()
                    .border_color(hairline(0.06))
                    .child(popover::palette_search_icon(&theme))
                    .child(div().flex_1().min_w_0().text_size(crate::typography::ui_rems(14.0)).child(search)),
            )
            .children(machine_tabs)
            .child(body)
            .children(foot)
            .into_any_element();
        Some(popover::menu_at("picker-menu", anchor(viewport, window), card, None))
    }

    /// Go to Directory's machine, picked from its tab.
    fn pick_machine(&mut self, device: String, cx: &mut Context<Self>) {
        let Some(old) = self.picker.take() else { return };
        if old.device.as_deref() == Some(device.as_str()) {
            self.picker = Some(old);
            return;
        }
        self.release_folders(&old, cx);
        self.picker = Some(Picker { device: Some(device), folders: FolderSearch::default(), active: 0, refused: None, ..old });
        self.ask_folders(true, cx);
        cx.notify();
    }
}

/// `targetDeviceId` for a machine other than this one: the home index's address.
fn target(device: Option<&str>, local: Option<&str>) -> serde_json::Value {
    let mut params = serde_json::Map::new();
    if let Some(device) = device.filter(|device| Some(*device) != local) {
        params.insert("targetDeviceId".into(), serde_json::Value::String(device.to_owned()));
    }
    serde_json::Value::Object(params)
}

/// Ask `device`'s home index for `query`, again while it is still scanning, backing off; with no
/// `generation`, the warming ask, whose answer is the folders gone to most.
fn ask_home_folders(
    engine: crate::state::EngineHandle,
    device: Option<String>,
    local: Option<String>,
    query: String,
    generation: Option<u64>,
    cx: &mut Context<Shell>,
) -> Task<()> {
    let mut params = target(device.as_deref(), local.as_deref());
    params["query"] = serde_json::Value::String(query);
    params["limit"] = serde_json::Value::from(FOLDER_LIMIT);
    cx.spawn(async move |this, cx| {
        let mut delay = FOLDER_RETRY;
        loop {
            let method = slopty_view_rpc::methods::SEARCH_HOME_FOLDERS;
            let answer = engine.client().call(method, params.clone()).await;
            let again = this
                .update(cx, |this, cx| {
                    let Some(search) = this.picker.as_mut().map(|picker| &mut picker.folders) else { return false };
                    let found = match answer {
                        Err(slopty_view_rpc::RpcError::UnknownMethod(_)) => {
                            search.unsupported = true;
                            search.started = false;
                            cx.notify();
                            return false;
                        }
                        Err(error) => {
                            tracing::debug!(%error, "home folder search failed");
                            return false;
                        }
                        Ok(answer) => match serde_json::from_value::<HomeFolderSearchResult>(answer) {
                            Ok(found) => found,
                            Err(error) => {
                                tracing::debug!(%error, "home folder search answer did not decode");
                                return false;
                            }
                        },
                    };
                    match generation {
                        None => search.visited = found.matches,
                        Some(generation) if generation == search.generation => search.found = found.matches,
                        Some(_) => return false,
                    }
                    cx.notify();
                    generation.is_some() && found.indexing
                })
                .unwrap_or(false);
            if !again {
                break;
            }
            cx.background_executor().timer(delay).await;
            delay = (delay * 2).min(FOLDER_RETRY_MAX);
        }
    })
}

/// A machine's header over its sessions.
fn header(name: SharedString, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("picker-header-{name}")))
        .px(px(10.0))
        .pt(px(6.0))
        .pb(px(2.0))
        .text_size(crate::typography::ui_rems(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_muted)
        .role(gpui::Role::Heading)
        .aria_label(name.clone())
        .child(name)
}

/// What a row draws: its glyph, its words, what trails them, and what a screen reader hears.
struct Parts {
    key: String,
    glyph: &'static str,
    tint: Option<gpui::Hsla>,
    title: AnyElement,
    second: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    said: String,
}

/// Why a search afar found nothing: a quiet line a screen reader hears as a status.
fn note(why: SharedString, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!("picker-note-{why}")))
        .px(px(10.0))
        .py(px(6.0))
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_muted)
        .role(gpui::Role::Status)
        .aria_label(why.clone())
        .child(why)
}

/// One row: its glyph, its words, and what trails them; `None` for a header or a note, each
/// drawn as one.
fn picker_row(
    row: &Row,
    active: bool,
    words: Option<&str>,
    agent: Agent,
    create_place: Option<&str>,
    starting: bool,
    now: DateTime<Utc>,
    theme: &Theme,
) -> Option<gpui::Stateful<gpui::Div>> {
    let small = crate::typography::ui_rems(12.0);
    let quiet = |text: SharedString| div().flex_none().text_size(small).text_color(theme.text_faint).child(text);
    let parts = match row {
        Row::Header(_) | Row::Note(_) => return None,
        Row::Session(session) => {
            let (glyph, tint) = session.harness.map_or((icons::TERMINAL, None), crate::pickers::harness_brand_icon);
            let mut trailing = Vec::new();
            trailing.extend(super::marks::mark_element(
                SharedString::from(format!("picker-mark-{}", session.chat_id)),
                format!("picker-mark-{}", session.chat_id),
                &session.mark,
                crate::loaders::status_mark::MARK_SMALL,
                theme.bg,
            ));
            if session.current {
                trailing.push(icon(icons::CHECK).size(px(12.0)).flex_none().text_color(theme.text_muted).into_any_element());
            }
            if let Some(digit) = session.digit.filter(|_| !crate::shell::touch_screen()) {
                trailing.push(popover::kbd_hint(theme, &format!("\u{2318}{digit}")).into_any_element());
            }
            let said = [
                Some(session.title.to_string()),
                session.place.as_ref().map(ToString::to_string),
                session.mark.word().map(str::to_owned),
                session.cached.then(|| "cached".to_owned()),
                session.current.then(|| "on screen".to_owned()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
            let title = div()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(6.0))
                .when(session.cached, |title| title.opacity(0.6))
                .child(div().min_w_0().truncate().child(popover::search_highlight(session.title.clone(), words, theme)))
                .children(session.place.clone().map(|place| quiet(place).min_w_0().truncate()))
                .into_any_element();
            Parts { key: format!("session-{}", session.chat_id), glyph, tint, title, second: None, trailing, said }
        }
        Row::Said { title, harness, found } => {
            let (glyph, tint) = harness.map_or((icons::TERMINAL, None), crate::pickers::harness_brand_icon);
            let who = if found.agent { "Agent" } else { "You" };
            Parts {
                key: format!("said-{}", found.chat_id),
                glyph,
                tint,
                title: div().min_w_0().truncate().child(title.clone()).into_any_element(),
                second: Some(super::session_search::chat_words_line(who, found, theme).into_any_element()),
                trailing: Vec::new(),
                said: format!("{title}, {who}: {}", found.excerpt),
            }
        }
        Row::Past { past, project } => {
            let label: SharedString = past.label().into();
            // Its project, its machine and its branch, as far as each is known.
            let place = [project.as_ref().map(ToString::to_string), past.device.clone(), past.branch.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" \u{b7} ");
            // While a live Claude Code elsewhere holds it, it says so where its age would stand.
            let when = past.running_words().map(SharedString::from).or_else(|| {
                past.updated().map(|at| SharedString::from(crate::state::format_time_ago(at, now)))
            });
            // The prompt that matched under the title, its words lit, unless it is the title.
            let matched = past
                .prompt
                .as_ref()
                .filter(|prompt| crate::transcript::single_line(&prompt.text).as_str() != label.as_ref());
            let title = div()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(6.0))
                .child(div().min_w_0().truncate().child(popover::search_highlight(label.clone(), words, theme)))
                .children((!place.is_empty()).then(|| quiet(place.clone().into()).min_w_0().truncate()))
                .into_any_element();
            Parts {
                key: format!("past-{}", past.key),
                glyph: icons::CLOCK_CIRCLE,
                tint: None,
                // A screen reader hears the prompt that matched too.
                said: [
                    Some(format!("{} {label}", super::past_sessions::PAST_SESSION)),
                    matched.map(|prompt| crate::transcript::single_line(&prompt.text)),
                    (!place.is_empty()).then_some(place),
                    when.as_ref().map(ToString::to_string),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
                title,
                second: matched.map(|prompt| super::past_sessions::prompt_text(prompt, theme)),
                trailing: when.map(|when| quiet(when).into_any_element()).into_iter().collect(),
            }
        }
        Row::Create(name) => {
            let (mark, tint) = agent_mark(agent);
            let doing = if starting { "Starting" } else { "Start" };
            let where_ = create_place.map(|place| format!(" in {place}")).unwrap_or_default();
            let title = div()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(6.0))
                .child(div().flex_none().child(format!("{doing} \u{201c}{name}\u{201d}")))
                .child(quiet(format!("{}{where_}", agent.name).into()).min_w_0().truncate())
                .into_any_element();
            Parts {
                key: format!("create-{name}"),
                glyph: icons::PLUS,
                tint: None,
                title,
                second: None,
                trailing: vec![icon(mark).size(px(13.0)).flex_none().text_color(tint.unwrap_or(theme.text_muted)).into_any_element()],
                said: format!("{doing} {} \u{201c}{name}\u{201d}{where_}", agent.name),
            }
        }
        Row::Folder(folder) => Parts {
            key: format!("folder-{}", folder.path),
            glyph: if folder.project { icons::FOLDER_WITH_FILES } else { icons::FOLDER },
            tint: None,
            title: div()
                .min_w_0()
                .truncate()
                .child(popover::search_highlight(folder.shown.clone(), words.filter(|_| !folder.typed), theme))
                .into_any_element(),
            second: None,
            trailing: folder.project.then(|| quiet("Project".into()).into_any_element()).into_iter().collect(),
            said: format!("Go to {}", folder.shown),
        },
    };
    let Parts { key, glyph, tint, title, second, trailing, said } = parts;
    Some(
        row_frame(key, active, theme, said, second.is_some())
            .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint.unwrap_or(theme.text_muted)))
            .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.0)).child(title).children(second))
            .child(div().flex_none().flex().items_center().gap(px(6.0)).children(trailing)),
    )
}

/// A row's frame: the menu's row, a button a screen reader hears by `said`.
fn row_frame(key: String, active: bool, theme: &Theme, said: String, two: bool) -> gpui::Stateful<gpui::Div> {
    popover::menu_row(theme, active, format!("picker-row-{key}"))
        .id(SharedString::from(format!("picker-row-{key}")))
        .debug_selector({
            let key = format!("picker-row-{key}");
            move || key
        })
        .rounded(px(popover::PALETTE_ITEM_RADIUS))
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(said))
        .min_h(px(if two { ROW_TWO_LINES } else { ROW_HEIGHT }))
        .gap(px(8.0))
}

/// The card's foot: its keys, and why a start was refused.
fn picker_foot(
    mode: Mode,
    agent: Agent,
    refused: Option<String>,
    theme: &Theme,
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> Option<AnyElement> {
    use super::command_palette::{command_key_hint, palette_footer};
    if let Some(why) = refused {
        return Some(
            div()
                .id("picker-refused")
                .px(px(14.0))
                .py(px(8.0))
                .border_t_1()
                .border_color(hairline(0.06))
                .text_size(crate::typography::ui_rems(12.0))
                .text_color(theme.danger)
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(why.clone()))
                .child(SharedString::from(why))
                .into_any_element(),
        );
    }
    if crate::shell::touch_screen() {
        return None;
    }
    let mut foot = palette_footer()
        .child(command_key_hint(theme, "\u{23CE}", "Open", window, cx))
        .child(command_key_hint(theme, "Tab", agent.name, window, cx));
    if mode == Mode::Directories {
        foot = foot.child(command_key_hint(theme, "\u{2318}[ \u{2318}]", "Machine", window, cx));
    }
    Some(foot.child(command_key_hint(theme, "Esc", "Close", window, cx)).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;
    use slopty_view_proto::{Chat, Device, Space};

    fn chat(id: &str, device: &str, space: &str, title: &str, at: DateTime<Utc>) -> Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "deviceId": device, "spaceId": space, "archived": false,
            "createdAt": at, "updatedAt": at, "lastMessageAt": at,
        }))
        .unwrap()
    }

    fn device(id: &str, name: &str, now: DateTime<Utc>) -> Device {
        serde_json::from_value(serde_json::json!({ "id": id, "name": name, "platform": "macos", "lastSeenAt": now }))
            .unwrap()
    }

    fn space(id: &str, device: &str, path: &str) -> Space {
        serde_json::from_value(serde_json::json!({
            "id": id, "deviceId": device, "path": path, "name": id, "createdAt": "2026-10-30T00:00:00Z",
        }))
        .unwrap()
    }

    fn state(now: DateTime<Utc>) -> AppState {
        let mut state = AppState::default();
        state.local_device_id = Some("mac".into());
        state.devices = vec![device("mac", "studio", now), device("box", "box", now)];
        state.spaces = vec![space("app", "mac", "/work/app"), space("api", "box", "/srv/api")];
        state.chats = vec![
            chat("a", "box", "api", "Fix the lexer", now - TimeDelta::minutes(3)),
            chat("b", "mac", "app", "Write the docs", now - TimeDelta::minutes(2)),
            chat("c", "mac", "app", "Tidy the tests", now - TimeDelta::minutes(1)),
        ];
        state.selected_chat = Some("b".into());
        state
    }

    fn titles(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Header(name) => format!("# {name}"),
                Row::Session(session) => session.title.to_string(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// Untyped, the sessions stand under their machines, this Mac's first, each in the list's
    /// order; the one on screen is ticked and the first nine carry their digits.
    #[test]
    fn untyped_the_picker_lists_every_session_under_its_machine() {
        let now = Utc::now();
        let rows = session_rows(&state(now), "", now);
        let shown = titles(&rows);
        let this_mac = if cfg!(target_os = "macos") { "# This Mac" } else { "# studio" };
        assert_eq!(shown.first().map(String::as_str), Some(this_mac), "this Mac's header first: {shown:?}");
        let mac = shown.iter().position(|t| t == "Write the docs").unwrap();
        let other = shown.iter().position(|t| t == "Fix the lexer").unwrap();
        assert!(mac < other, "this Mac's first: {shown:?}");
        assert!(shown.iter().any(|t| t == "# box"), "a header per machine: {shown:?}");
        let sessions: Vec<&SessionRow> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Session(session) => Some(session),
                _ => None,
            })
            .collect();
        assert_eq!(sessions.iter().map(|s| s.digit).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3)]);
        assert!(sessions.iter().find(|s| s.chat_id == "b").unwrap().current, "the one on screen is ticked");
        assert!(!sessions.iter().find(|s| s.chat_id == "a").unwrap().current);
    }

    /// Typed words filter by title, project and machine, best first, each row saying its
    /// machine; words that match nothing list nothing here (the card offers to start one).
    #[test]
    fn typing_filters_the_sessions_by_title_project_and_machine() {
        let now = Utc::now();
        let state = state(now);
        assert_eq!(titles(&session_rows(&state, "lexer", now)), vec!["Fix the lexer"]);
        assert_eq!(titles(&session_rows(&state, "app", now)).len(), 2, "by project");
        let by_machine = session_rows(&state, "box", now);
        assert_eq!(titles(&by_machine), vec!["Fix the lexer"], "by machine");
        let Row::Session(row) = &by_machine[0] else { panic!("a session") };
        assert_eq!(row.place.as_deref(), Some("api \u{b7} box"), "typed rows say their machine");
        assert!(session_rows(&state, "nothing like it", now).is_empty());
    }

    /// Go to Directory lists the machine's projects, then the folders gone to most; typed words
    /// narrow them, and a typed path leads as it was written.
    #[test]
    fn go_to_directory_lists_projects_then_folders_and_a_typed_path() {
        let now = Utc::now();
        let state = state(now);
        let visited = vec![HomeFolderMatch { path: "/Users/me/notes".into(), relative: "notes".into(), is_repo: false }];
        let view = FolderSearchView { home: Some("/Users/me".into()), visited: &visited, found: &[] };
        let paths = |rows: Vec<Row>| -> Vec<String> {
            rows.into_iter()
                .map(|row| match row {
                    Row::Folder(folder) => folder.shown.to_string(),
                    other => panic!("a folder, not {other:?}"),
                })
                .collect()
        };
        assert_eq!(paths(folder_rows(&state, "mac", "", &view)), vec!["/work/app", "~/notes"]);
        assert_eq!(paths(folder_rows(&state, "box", "", &view)), vec!["/srv/api", "~/notes"]);
        assert_eq!(paths(folder_rows(&state, "mac", "app", &view)), vec!["/work/app"], "typed words narrow");
        let typed = folder_rows(&state, "mac", "~/code/x", &view);
        let Row::Folder(first) = &typed[0] else { panic!("a folder") };
        assert_eq!(first.path, "/Users/me/code/x", "a typed path, home expanded");
        assert!(first.typed);
    }

    /// ⌘K opens the session picker and ⇧⌘G turns it to Go to Directory, whose own key shuts
    /// it. With the inspector's find open, ⇧⌘G is Find Previous there instead: no card comes. (A
    /// terminal's open find card takes the key in its own context first: `slopty_terminal`'s
    /// `cmd_shift_g_goes_on_while_find_is_shut`.)
    #[gpui::test]
    fn shift_cmd_g_is_go_to_directory_unless_a_find_is_open(cx: &mut gpui::TestAppContext) {
        use crate::settings::platform_combo;
        let (shell, cx) = super::super::retention_tests::launch(cx, super::super::retention_tests::DESKTOP);
        // The card as the last frame drew it, which the next key is dispatched through.
        let card = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            shell.read_with(cx, |shell, _| shell.picker.as_ref().map(|picker| picker.mode))
        };
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_session_terminals([("a".to_owned(), "t-a".to_owned())].into());
                cx.notify();
            });
            shell.open_chat("a".into(), cx);
        });
        assert_eq!(card(cx), None);
        cx.simulate_keystrokes(&platform_combo("mod-k"));
        assert_eq!(card(cx), Some(Mode::Sessions), "⌘K: the sessions");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), Some(Mode::Directories), "⇧⌘G: the folders, in the same card");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), None, "its own key shuts it");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), Some(Mode::Directories), "no find open: Go to Directory");
        cx.simulate_keystrokes("escape");
        assert_eq!(card(cx), None, "Esc puts it away");

        // A Changes pane with the keyboard in its own keys, as it has them once drawn (⌘F opens
        // its find there): its find shut, ⇧⌘G goes on to Go to Directory, whose Esc gives the
        // keyboard back; its find open, ⇧⌘G steps back there.
        cx.update(|window, cx| shell.update(cx, |shell, cx| shell.add_diff_surface(window, cx)));
        let (id, diff) = shell.read_with(cx, |shell, _| (shell.diff_seq, shell.diffs[&shell.diff_seq].clone()));
        // A diff to read: an empty Changes has no find to open.
        diff.update(cx, |changes, cx| {
            changes.show_patch("/work/app", "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old();\n+new();\n", cx);
        });
        assert_eq!(card(cx), None);
        assert_eq!(card(cx), None);
        let pane = super::super::panes::Pane::View(super::super::RightSurface::Diff(id));
        let in_changes = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| shell.read(cx).pane_holds_keyboard(pane, window, cx))
        };
        let find_has_keyboard =
            |cx: &mut gpui::VisualTestContext| cx.update(|window, cx| diff.read(cx).find_has_keyboard(window, cx));
        assert!(in_changes(cx), "the new Changes pane has the keyboard");
        cx.simulate_keystrokes(&platform_combo("mod-f"));
        assert_eq!(card(cx), None);
        assert!(find_has_keyboard(cx), "in Changes' own keys: ⌘F opens its find");
        cx.simulate_keystrokes("escape");
        assert_eq!(card(cx), None);
        assert!(!find_has_keyboard(cx) && in_changes(cx), "its find shut, the pane keeps the keyboard");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), Some(Mode::Directories), "Changes' find shut: Go to Directory");
        cx.simulate_keystrokes("escape");
        assert_eq!(card(cx), None);
        assert!(in_changes(cx), "Esc gives the keyboard back to the pane");
        cx.simulate_keystrokes(&platform_combo("mod-f"));
        assert_eq!(card(cx), None);
        assert!(find_has_keyboard(cx), "Changes' find is open again");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), None, "⇧⌘G steps back in Changes' find");
        assert!(find_has_keyboard(cx), "which keeps the keyboard");
        cx.update(|_, cx| shell.update(cx, |shell, cx| shell.close_view(super::super::RightSurface::Diff(id), cx)));
        assert_eq!(card(cx), None);
        assert!(!in_changes(cx), "closed, the pane gives the keyboard back");

        // The inspector summoned with the keyboard, and its find opened there.
        cx.simulate_keystrokes(&platform_combo("mod-i"));
        assert_eq!(card(cx), None);
        cx.simulate_keystrokes(&platform_combo("mod-f"));
        assert_eq!(card(cx), None);
        assert!(cx.update(|_, cx| crate::transcript::find::find_open(cx)), "the inspector's find is open");
        cx.simulate_keystrokes(&platform_combo("mod-shift-g"));
        assert_eq!(card(cx), None, "⇧⌘G steps back in it");
        assert!(cx.update(|_, cx| crate::transcript::find::find_open(cx)), "and leaves it open");
    }
}
