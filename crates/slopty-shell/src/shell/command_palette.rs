// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Global search over actions, conversations and the focused chat's
//! workspace, using the sidebar's conversation rows. Results are grouped in
//! sections; tabs narrow them to one kind.
use super::*;
use crate::appearance::AppearanceMode;
use crate::files::client::{FilesClientError, FilesRequestContext, WorkspaceFilesClient};
use slopty_view_proto::{
    SearchWorkspaceContentRequest, SearchWorkspaceFilesRequest, WorkspaceContentMatch,
    WorkspaceEntryKind, WorkspaceFileSearchMatch, WorkspaceSearchIndexState, WorkspaceTarget,
};

/// Key context of the palette card: its mod-1…4 tab bindings live here.
pub(super) const KEY_CONTEXT: &str = "CommandPalette";
/// Rows a section shows under All; its own tab shows the long limit.
const ALL_TAB_LIMIT: usize = 5;
const THREADS_TAB_LIMIT: usize = 50;
const FILES_TAB_LIMIT: usize = 50;
const CONTENT_TAB_LIMIT: usize = 100;
const CONTENT_MATCHES_PER_FILE: usize = 3;
/// Shorter workspace queries match nearly everything; they are not sent.
const MIN_WORKSPACE_QUERY_CHARS: usize = 2;
/// Content search waits for typing to pause; name search does not.
const CONTENT_DEBOUNCE: Duration = Duration::from_millis(80);
/// Refresh partial results until the host finishes its initial scan,
/// doubling the wait up to [`CONTENT_INDEX_RETRY_MAX`]: without a content
/// index every refresh is a full grep, and a large folder can scan for long.
const CONTENT_INDEX_RETRY: Duration = Duration::from_millis(500);
const CONTENT_INDEX_RETRY_MAX: Duration = Duration::from_secs(2);
const FILE_INDEX_RETRY: Duration = Duration::from_millis(500);
const RESULTS_FADE_BAND: f32 = 18.0;
// slopty: what the chats said is searched once typing pauses on two characters or more
// (`SearchChats`), below the title matches.
/// Shorter queries match nearly every chat's words; they are not sent.
const MIN_CHAT_WORDS_CHARS: usize = 2;
/// The pause in typing before the chats' words are searched.
const CHAT_WORDS_DEBOUNCE: Duration = Duration::from_millis(200);
/// How many matches one search asks for.
const CHAT_WORDS_LIMIT: usize = 50;

pub(super) struct CommandPalette {
    search: Entity<ComposerInput>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    tab: Tab,
    /// The highlighted row, kept by identity so results arriving around it
    /// never move it; `active_index` is where it was, for when it vanishes.
    active: Option<RowKey>,
    active_index: usize,
    enter_press: EnterPress,
    // Claim focus during mount so the shell does not restore the composer
    // while this input is still absent from the dispatch tree.
    focus_pending: bool,
    scroll: gpui::ScrollHandle,
    /// File-name matches in the focused chat's workspace.
    files: WorkspaceQuery<Vec<WorkspaceFileSearchMatch>>,
    /// The workspace whose index a probe found ready during this palette
    /// session: name searches there skip the readiness probe, saving a round
    /// trip per keystroke on remote hosts.
    files_ready: Option<WorkspaceTarget>,
    /// Content matches in the focused chat's workspace.
    content: WorkspaceQuery<Vec<WorkspaceContentMatch>>,
    // slopty: what the chats said that matches the query.
    chat_words: ChatWordsQuery,
    _search_events: Subscription,
}

// slopty: one place a chat's words matched (`SearchChats`): the chat, who said it, a line around
// the match and the match's byte range in that line.
/// A chat whose words match the query.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatWordsMatch {
    chat_id: String,
    /// The agent said it; else the person did.
    agent: bool,
    /// The words around the match, on one line.
    excerpt: String,
    /// The match's byte range in `excerpt`; empty when the answer's range did not fit it.
    range: std::ops::Range<usize>,
}

impl ChatWordsMatch {
    /// One answer's entry, or `None` when it names no chat. A range that is not on the
    /// excerpt's character boundaries is dropped, not the match.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        let chat_id = value.get("chatId")?.as_str().filter(|id| !id.is_empty())?.to_owned();
        let agent = value.get("role").and_then(serde_json::Value::as_str) != Some("user");
        // One line: line breaks and tabs become spaces, each one byte for one, so the range
        // still lands.
        let excerpt: String = value
            .get("excerpt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
            .collect();
        let offset = |key: &str| {
            value.get(key).and_then(serde_json::Value::as_u64).and_then(|v| usize::try_from(v).ok())
        };
        let range = match (offset("matchStart"), offset("matchEnd")) {
            (Some(start), Some(end))
                if start < end
                    && end <= excerpt.len()
                    && excerpt.is_char_boundary(start)
                    && excerpt.is_char_boundary(end) =>
            {
                start..end
            }
            _ => 0..0,
        };
        Some(Self { chat_id, agent, excerpt, range })
    }

    /// An answer's matches, first per chat, in the answer's order; anything else answers none.
    fn parse_all(answer: &serde_json::Value) -> Vec<Self> {
        let list = answer.as_array().or_else(|| answer.get("matches").and_then(|m| m.as_array()));
        let mut seen = std::collections::HashSet::new();
        list.into_iter()
            .flatten()
            .filter_map(Self::parse)
            .filter(|found| seen.insert(found.chat_id.clone()))
            .collect()
    }
}

// slopty: the chats' words search's request state: an answer to an older query is dropped, and
// a failure (an older view model, a server that does not search) shows no matches.
#[derive(Default)]
struct ChatWordsQuery {
    /// Bumped per request; an answer to an older one is dropped.
    generation: u64,
    loading: bool,
    results: Vec<ChatWordsMatch>,
    task: Option<Task<()>>,
}

impl ChatWordsQuery {
    /// Forget the results, cancelling any request in flight.
    fn clear(&mut self) {
        self.generation += 1;
        self.loading = false;
        self.results.clear();
        self.task = None;
    }

    /// Start a request; the returned generation identifies its answer. Rows from the last
    /// answer stay up meanwhile.
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    /// Settle request `generation` with its answer; `false` when a newer one superseded it.
    fn finish(
        &mut self,
        generation: u64,
        answer: Result<serde_json::Value, slopty_view_rpc::RpcError>,
    ) -> bool {
        if self.generation != generation {
            return false;
        }
        self.loading = false;
        self.results = match answer {
            Ok(answer) => ChatWordsMatch::parse_all(&answer),
            Err(error) => {
                tracing::debug!(%error, "palette chat words search failed");
                Vec::new()
            }
        };
        true
    }
}

/// An asynchronous workspace section's request state. Rows from the last
/// answer stay up while a newer query runs.
#[derive(Default)]
struct WorkspaceQuery<T> {
    /// Bumped per request; an answer to an older one is dropped.
    generation: u64,
    loading: bool,
    /// The host's index was still scanning at the last answer.
    indexing: bool,
    error: Option<SharedString>,
    results: T,
    task: Option<Task<()>>,
}

impl<T: Default> WorkspaceQuery<T> {
    /// Forget everything, cancelling any request in flight.
    fn clear(&mut self) {
        self.generation += 1;
        *self = Self {
            generation: self.generation,
            ..Self::default()
        };
    }

    /// Start a request; the returned generation identifies its answer.
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    /// [`Self::status`] without failures: a default list that a host can't
    /// serve (one that predates empty-query search) quietly shows nothing.
    fn progress(&self) -> Option<SharedString> {
        if self.indexing {
            Some("Indexing…".into())
        } else if self.loading {
            Some("Searching…".into())
        } else {
            None
        }
    }

    /// Header status while loading, while the host indexes, or after a
    /// failure.
    fn status(&self) -> Option<SharedString> {
        if self.indexing {
            Some("Indexing…".into())
        } else if self.loading {
            Some("Searching…".into())
        } else {
            self.error.clone()
        }
    }

    /// Settle the request `generation` with its answer; `false` when a newer
    /// request superseded it.
    fn finish(
        &mut self,
        generation: u64,
        result: Result<(T, bool), FilesClientError>,
        what: &str,
    ) -> bool {
        if self.generation != generation {
            return false;
        }
        self.loading = false;
        match result {
            Ok((results, indexing)) => {
                self.error = None;
                self.indexing = indexing;
                self.results = results;
            }
            Err(error) => {
                tracing::warn!(%error, "palette {what} search failed");
                self.error = Some(search_error_message(&error, what));
                self.indexing = false;
                self.results = T::default();
            }
        }
        true
    }
}

/// The focused chat's workspace connection, when it has one.
fn palette_workspace(shell: &Shell, cx: &App) -> Option<WorkspaceFilesClient> {
    if shell.active_chat.is_empty() {
        return None;
    }
    let state = shell.state.read(cx);
    let context = FilesRequestContext::for_chat(state, &shell.active_chat)?;
    Some(WorkspaceFilesClient::new(state.engine()?.clone(), context))
}

/// `what` ("file" or "content") names the search in the message.
fn search_error_message(error: &FilesClientError, what: &str) -> SharedString {
    match error {
        FilesClientError::Unsupported(_) => {
            let subject = if what == "content" {
                "file contents"
            } else {
                "files"
            };
            format!("Couldn\u{2019}t search its {subject}: the chat\u{2019}s machine runs an older Slopty. Update it there.")
                .into()
        }
        FilesClientError::Transport(_) => "Couldn\u{2019}t reach the chat\u{2019}s machine. Check that it is on and online.".into(),
        FilesClientError::Encode(_)
        | FilesClientError::Decode(_)
        | FilesClientError::Request(_) => "Couldn\u{2019}t search. Try again.".into(),
    }
}

// X11 suppresses synthetic repeat releases but sends repeated keydowns with
// is_held=false. Keep our own latch until the physical key is released.
#[derive(Default)]
struct EnterPress {
    down: bool,
}

impl EnterPress {
    fn press(&mut self, is_held: bool) -> bool {
        let was_down = std::mem::replace(&mut self.down, true);
        !was_down && !is_held
    }

    fn release(&mut self) {
        self.down = false;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    All,
    Threads,
    Commands,
    Files,
}

impl Tab {
    pub(super) const ALL: [Tab; 4] = [Tab::All, Tab::Threads, Tab::Commands, Tab::Files];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Threads => "Chats", // slopty
            Self::Commands => "Commands",
            Self::Files => "Files",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// The neighbouring tab, wrapping (tab / shift-tab).
    fn step(self, forward: bool) -> Self {
        let count = Self::ALL.len();
        let next = if forward {
            self.index() + 1
        } else {
            self.index() + count - 1
        };
        Self::ALL[next % count]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    RecentFiles,
    QuickActions,
    RecentThreads,
    Threads,
    Commands,
    Files,
    InFiles,
    // slopty: chats whose words match, below the title matches.
    InThreads,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            // slopty: sentence case, as every label in the app.
            Self::RecentFiles => "Recent files",
            Self::QuickActions => "Quick actions",
            // slopty: one name for a conversation, "chat" (M17).
            Self::RecentThreads => "Recent chats",
            Self::Threads => "Chats",
            Self::Commands => "Commands",
            Self::Files => "Files",
            Self::InFiles => "In files",
            Self::InThreads => "In chats",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Command {
    NewChat,
    NewProject,
    ResumeSession, // slopty: a past Claude Code session (`past_sessions`)
    Settings,
    Theme(AppearanceMode),
    ArchiveThread,
    /// slopty: a command of the menu bar's table (`app_menus::window_commands`), dispatched as
    /// its key would.
    Window(crate::app_menus::WindowCommand),
}

impl Command {
    fn label(&self) -> &'static str {
        match self {
            Self::NewChat => "New session",
            Self::NewProject => "New project\u{2026}", // slopty: it opens a picker
            Self::ResumeSession => super::past_sessions::RESUME, // slopty
            Self::Settings => "Settings", // slopty: the name its menu and button use
            Self::Theme(AppearanceMode::System) => "Switch to system theme",
            Self::Theme(AppearanceMode::Light) => "Switch to light theme",
            Self::Theme(AppearanceMode::Dark) => "Switch to dark theme",
            Self::ArchiveThread => "Archive chat", // slopty
            Self::Window(command) => command.label,
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::NewChat => icons::PLUS, // slopty: the titlebar's glyph for it
            Self::NewProject => icons::FOLDER,
            Self::ResumeSession => icons::CLOCK_CIRCLE, // slopty
            Self::Settings => icons::SETTINGS,
            Self::Theme(mode) => mode.icon(),
            Self::ArchiveThread => icons::ARCHIVE_MINIMALISTIC,
            Self::Window(command) => command.icon,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum RowKind {
    Command(Command),
    Chat(String),
    /// A path in the focused chat's workspace.
    File {
        path: String,
        is_dir: bool,
    },
    /// A line of a file in the focused chat's workspace.
    Content(WorkspaceContentMatch),
    // slopty: a chat by what was said in it.
    /// A chat whose words matched.
    ChatWords(ChatWordsMatch),
}

#[derive(Clone, Debug, PartialEq)]
struct PaletteRow {
    section: Section,
    kind: RowKind,
}

/// A row's identity across result refreshes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RowKey(Section, String);

impl PaletteRow {
    fn new(section: Section, kind: RowKind) -> Self {
        Self { section, kind }
    }

    fn key(&self) -> RowKey {
        let id = match &self.kind {
            RowKind::Command(command) => format!("command:{}", command.label()),
            RowKind::Chat(id) => format!("chat:{id}"),
            RowKind::File { path, .. } => format!("file:{path}"),
            RowKind::Content(found) => {
                format!("content:{}:{}:{}", found.path, found.line, found.column)
            }
            RowKind::ChatWords(found) => format!("words:{}", found.chat_id),
        };
        RowKey(self.section, id)
    }
}

/// One titled group of rows, already limited for the tab.
#[derive(Clone, Debug, PartialEq)]
struct PaletteSection {
    section: Section,
    rows: Vec<PaletteRow>,
    /// Shown beside the title: "Searching…", or why the search failed.
    status: Option<SharedString>,
}

impl PaletteSection {
    fn new(section: Section, kinds: impl IntoIterator<Item = RowKind>) -> Self {
        Self {
            section,
            rows: kinds
                .into_iter()
                .map(|kind| PaletteRow::new(section, kind))
                .collect(),
            status: None,
        }
    }

    fn with_status(mut self, status: Option<SharedString>) -> Self {
        self.status = status;
        self
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.status.is_none()
    }
}

fn matches_query(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| text.contains(word))
}

/// `can_archive`: a focused, unarchived chat exists for Archive chat. slopty: `window` adds the
/// menu bar's commands (the Commands tab, or any search).
fn commands_for(query: &str, is_dark: bool, can_archive: bool, window: bool) -> Vec<Command> {
    [
        Command::NewChat,
        Command::NewProject,
        Command::ResumeSession, // slopty
        Command::Settings,
        Command::Theme(if is_dark {
            AppearanceMode::Light
        } else {
            AppearanceMode::Dark
        }),
    ]
    .into_iter()
    .chain(can_archive.then_some(Command::ArchiveThread))
    .chain(
        crate::app_menus::window_commands()
            .filter(|_| window)
            .map(Command::Window),
    )
    .filter(|command| matches_query(query, command.label()))
    .collect()
}

/// A path's last segment and the folder holding it (empty at the root).
fn split_path(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((parent, name)) => (name, parent),
        None => (trimmed, ""),
    }
}

/// The scroll container's child index of flat row `row_ix`: each section
/// mounts a header before its rows.
fn child_index(sections: &[PaletteSection], row_ix: usize) -> usize {
    let mut rows_before = 0;
    for (section_ix, section) in sections.iter().enumerate() {
        if row_ix < rows_before + section.rows.len() {
            return row_ix + section_ix + 1;
        }
        rows_before += section.rows.len();
    }
    row_ix + sections.len()
}

/// The child to scroll into view for row `row_ix`: its section header when
/// it is the section's first row, so the header stays visible with it.
fn scroll_child_index(sections: &[PaletteSection], row_ix: usize) -> usize {
    let mut rows_before = 0;
    for (section_ix, section) in sections.iter().enumerate() {
        if row_ix < rows_before + section.rows.len() {
            let child = row_ix + section_ix + 1;
            return if row_ix == rows_before { child - 1 } else { child };
        }
        rows_before += section.rows.len();
    }
    child_index(sections, row_ix)
}

/// Navigation order: every row of every section, headers skipped.
fn flat_rows(sections: &[PaletteSection]) -> Vec<&PaletteRow> {
    sections.iter().flat_map(|section| &section.rows).collect()
}

/// Where the row `active` sits in `rows`: found by identity when still
/// listed, else `fallback` clamped.
fn row_position(active: Option<&RowKey>, fallback: usize, rows: &[&PaletteRow]) -> usize {
    active
        .and_then(|key| rows.iter().position(|row| &row.key() == key))
        .unwrap_or_else(|| fallback.min(rows.len().saturating_sub(1)))
}

impl CommandPalette {
    fn active_position(&self, rows: &[&PaletteRow]) -> usize {
        row_position(self.active.as_ref(), self.active_index, rows)
    }

    fn set_active(&mut self, rows: &[&PaletteRow], index: usize) {
        self.active_index = index;
        self.active = rows.get(index).map(|row| row.key());
    }

    fn reset_active(&mut self) {
        self.active = None;
        self.active_index = 0;
    }
}

impl Shell {
    pub(super) fn reset_command_palette_key_state(&mut self) {
        if let Some(palette) = self.command_palette.as_mut() {
            palette.enter_press.release();
        }
    }

    pub(super) fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            self.close_command_palette(window, cx);
            return;
        }
        self.close_add_space(cx);
        crate::live::chrome::release_held_keys(cx);
        let search = cx.new(|cx| {
            ComposerInput::with_context("Search chats, commands and files…", "PaletteSearch", cx)
        });
        let events = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                if let Some(palette) = this.command_palette.as_mut() {
                    palette.reset_active();
                    palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
                }
                this.search_palette_workspace(cx);
                this.search_palette_chat_words(cx); // slopty
                cx.notify();
            }
        });
        let previous_focus = window.focused(cx);
        self.command_palette = Some(CommandPalette {
            search,
            focus: cx.focus_handle(),
            previous_focus,
            tab: Tab::All,
            active: None,
            active_index: 0,
            enter_press: EnterPress::default(),
            focus_pending: true,
            scroll: gpui::ScrollHandle::new(),
            files: WorkspaceQuery::default(),
            files_ready: None,
            content: WorkspaceQuery::default(),
            chat_words: ChatWordsQuery::default(), // slopty
            _search_events: events,
        });
        // Load the default Files list (recently changed files) right away.
        self.search_palette_workspace(cx);
        cx.notify();
    }

    pub(super) fn select_palette_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        if palette.tab != tab {
            palette.tab = tab;
            palette.reset_active();
            palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
            cx.notify();
        }
    }

    pub(super) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.command_palette.take() {
            if let Some(focus) = palette.previous_focus {
                window.focus(&focus, cx);
            }
            cx.notify();
        }
    }

    /// Look the query up in the focused workspace: file names at once,
    /// contents once typing pauses.
    fn search_palette_workspace(&mut self, cx: &mut Context<Self>) {
        let workspace = palette_workspace(self, cx);
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let query = palette.search.read(cx).text().trim().to_owned();
        // An empty query lists the workspace's recent files (the default
        // Files list); a single character is too broad to search.
        let Some(client) =
            workspace.filter(|_| query.is_empty() || query.chars().count() >= MIN_WORKSPACE_QUERY_CHARS)
        else {
            palette.files.clear();
            palette.content.clear();
            return;
        };

        let generation = palette.files.begin();
        let request = SearchWorkspaceFilesRequest {
            target: client.target().clone(),
            query: query.clone(),
            include_ignored: false,
            limit: Some(FILES_TAB_LIMIT as u16),
        };
        let names = client.clone();
        let target = client.target().clone();
        // The default list is a plain request: readiness is probed for typed
        // queries, whose partial answers are worth retrying.
        let mut probe = !request.query.is_empty() && palette.files_ready.as_ref() != Some(&target);
        palette.files.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let mut ready = false;
                let result = async {
                    // SearchWorkspaceFiles keeps its list-shaped wire response.
                    // Check readiness BEFORE searching: a scan finishing after
                    // a partial answer must still trigger one final search.
                    // Once ready, the index stays built while in use, so later
                    // keystrokes go straight to the search.
                    let indexing = if probe {
                        match names.warm_search().await {
                            Ok(warm) => warm.state == WorkspaceSearchIndexState::Building,
                            // Older hosts search without the new index/warm method.
                            Err(FilesClientError::Unsupported(_)) => false,
                            Err(error) => return Err(error),
                        }
                    } else {
                        false
                    };
                    ready = probe && !indexing;
                    names
                        .search(request.clone())
                        .await
                        .map(|found| (found, indexing))
                }
                .await;
                probe &= !ready;
                let target = target.clone();
                let retry = this
                    .update(cx, |shell, cx| {
                        let Some(palette) = shell.command_palette.as_mut() else {
                            return false;
                        };
                        if ready {
                            palette.files_ready = Some(target);
                        }
                        if !palette.files.finish(generation, result, "file") {
                            return false;
                        }
                        cx.notify();
                        palette.files.indexing
                    })
                    .unwrap_or(false);
                if !retry {
                    break;
                }
                // Keep partial rows and selection; replacing the query or
                // closing the palette drops this task and cancels retries.
                cx.background_executor().timer(FILE_INDEX_RETRY).await;
            }
        }));

        if query.is_empty() {
            palette.content.clear();
            return;
        }
        let generation = palette.content.begin();
        let request = SearchWorkspaceContentRequest {
            target: client.target().clone(),
            query,
            limit: Some(CONTENT_TAB_LIMIT as u16),
            per_file_limit: Some(CONTENT_MATCHES_PER_FILE as u16),
        };
        palette.content.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONTENT_DEBOUNCE).await;
            let mut delay = CONTENT_INDEX_RETRY;
            loop {
                let result = client
                    .search_content(request.clone())
                    .await
                    .map(|found| (found.matches, found.indexing));
                let retry = this
                    .update(cx, |shell, cx| {
                        let Some(palette) = shell.command_palette.as_mut() else {
                            return false;
                        };
                        if !palette.content.finish(generation, result, "content") {
                            return false;
                        }
                        cx.notify();
                        palette.content.indexing
                    })
                    .unwrap_or(false);
                if !retry {
                    break;
                }
                // Keep partial rows and their selection visible. Replacing the
                // query or closing the palette drops this task, cancelling the
                // wait as well as any in-flight request.
                cx.background_executor().timer(delay).await;
                delay = (delay * 2).min(CONTENT_INDEX_RETRY_MAX);
            }
        }));
    }

    // slopty: what the chats said, searched once typing pauses (`SearchChats`); a query too
    // short to send clears the matches.
    /// Look the query up in every chat's words.
    fn search_palette_chat_words(&mut self, cx: &mut Context<Self>) {
        let engine = self.state.read(cx).engine().cloned();
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        let query = palette.search.read(cx).text().trim().to_owned();
        let Some(engine) = engine.filter(|_| query.chars().count() >= MIN_CHAT_WORDS_CHARS) else {
            palette.chat_words.clear();
            return;
        };
        let generation = palette.chat_words.begin();
        let params = serde_json::json!({ "query": query, "limit": CHAT_WORDS_LIMIT });
        palette.chat_words.task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHAT_WORDS_DEBOUNCE).await;
            let answer = engine.client().call(methods::SEARCH_CHATS, params).await;
            this.update(cx, |shell, cx| {
                let Some(palette) = shell.command_palette.as_mut() else {
                    return;
                };
                if palette.chat_words.finish(generation, answer) {
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The open palette's sections for its tab and query.
    fn palette_sections(&self, cx: &App) -> Vec<PaletteSection> {
        let Some(palette) = &self.command_palette else {
            return Vec::new();
        };
        let query = palette.search.read(cx).text().trim().to_lowercase();
        let can_archive = self.state.read(cx).archivable_selected_chat().is_some();
        let commands = |section| {
            // slopty: the short list leads an empty All tab; Commands and a search hold them all.
            let window = section == Section::Commands || !query.is_empty();
            PaletteSection::new(
                section,
                commands_for(&query, Theme::of(cx).appearance.is_dark(), can_archive, window)
                    .into_iter()
                    .map(RowKind::Command),
            )
        };
        let threads = |limit| {
            PaletteSection::new(
                Section::Threads,
                self.thread_matches(&query, limit, cx)
                    .into_iter()
                    .map(RowKind::Chat),
            )
        };
        let recent_threads = |limit| {
            PaletteSection::new(
                Section::RecentThreads,
                self.recent_threads(limit, cx)
                    .into_iter()
                    .map(RowKind::Chat),
            )
        };
        // Files opened here first, then the workspace's recently changed
        // files (the host's empty-query list), each path once.
        let recent_files = |limit| {
            let mut seen = std::collections::HashSet::new();
            let opened = self.recent_files(&self.active_chat).map(|path| (path.clone(), false));
            let changed = palette
                .files
                .results
                .iter()
                .filter(|found| found.kind != WorkspaceEntryKind::Directory)
                .map(|found| (found.path.clone(), false));
            PaletteSection::new(
                Section::RecentFiles,
                opened
                    .chain(changed)
                    .filter(|(path, _)| seen.insert(path.clone()))
                    .take(limit)
                    .map(|(path, is_dir)| RowKind::File { path, is_dir }),
            )
            .with_status(palette.files.progress())
        };
        let files = |limit| {
            PaletteSection::new(
                Section::Files,
                palette
                    .files
                    .results
                    .iter()
                    .take(limit)
                    .map(|found| RowKind::File {
                        path: found.path.clone(),
                        is_dir: found.kind == WorkspaceEntryKind::Directory,
                    }),
            )
            .with_status(palette.files.status())
        };
        let in_files = |limit| {
            PaletteSection::new(
                Section::InFiles,
                palette
                    .content
                    .results
                    .iter()
                    .take(limit)
                    .cloned()
                    .map(RowKind::Content),
            )
            .with_status(palette.content.status())
        };
        // slopty: chats by their words, each chat once, none a title match already lists.
        let in_threads = |limit, titled: &[String]| {
            let state = self.state.read(cx);
            PaletteSection::new(
                Section::InThreads,
                palette
                    .chat_words
                    .results
                    .iter()
                    .filter(|found| !titled.contains(&found.chat_id))
                    .filter(|found| {
                        state
                            .chats
                            .iter()
                            .any(|chat| chat.id == found.chat_id && chat.is_top_level())
                    })
                    .take(limit)
                    .cloned()
                    .map(RowKind::ChatWords),
            )
            .with_status(
                (palette.chat_words.loading && palette.chat_words.results.is_empty())
                    .then(|| "Searching…".into()),
            )
        };
        let titled = |section: &PaletteSection| -> Vec<String> {
            section
                .rows
                .iter()
                .filter_map(|row| match &row.kind {
                    RowKind::Chat(id) => Some(id.clone()),
                    _ => None,
                })
                .collect()
        };
        let sections = match (palette.tab, query.is_empty()) {
            (Tab::All, true) => vec![
                recent_files(ALL_TAB_LIMIT),
                commands(Section::QuickActions),
                recent_threads(ALL_TAB_LIMIT),
            ],
            (Tab::All, false) => {
                let threads = threads(ALL_TAB_LIMIT);
                let words = in_threads(ALL_TAB_LIMIT, &titled(&threads));
                vec![
                    threads,
                    words, // slopty
                    commands(Section::Commands),
                    files(ALL_TAB_LIMIT),
                    in_files(ALL_TAB_LIMIT),
                ]
            }
            (Tab::Threads, true) => vec![recent_threads(THREADS_TAB_LIMIT)],
            (Tab::Threads, false) => {
                let threads = threads(THREADS_TAB_LIMIT);
                // slopty: then the chats whose words match.
                let words = in_threads(THREADS_TAB_LIMIT, &titled(&threads));
                vec![threads, words]
            }
            (Tab::Commands, true) => vec![commands(Section::Commands)], // slopty: all of them
            (Tab::Commands, false) => vec![commands(Section::Commands)],
            (Tab::Files, true) => vec![recent_files(FILES_TAB_LIMIT)],
            (Tab::Files, false) => vec![files(FILES_TAB_LIMIT), in_files(CONTENT_TAB_LIMIT)],
        };
        sections
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect()
    }

    /// Unarchived top-level chats by latest activity, the focused one aside.
    fn recent_threads(&self, limit: usize, cx: &App) -> Vec<String> {
        let state = self.state.read(cx);
        let mut chats: Vec<_> = state
            .chats
            .iter()
            .filter(|chat| chat.is_top_level() && !chat.archived && chat.id != self.active_chat)
            .collect();
        chats
            .sort_by_key(|chat| std::cmp::Reverse(chat.last_message_at.unwrap_or(chat.created_at)));
        chats
            .into_iter()
            .take(limit)
            .map(|chat| chat.id.clone())
            .collect()
    }

    /// Top-level chats matching every word of `query`, best match first
    /// (see `thread_search`).
    fn thread_matches(&self, query: &str, limit: usize, cx: &App) -> Vec<String> {
        let state = self.state.read(cx);
        let terms = thread_search::search_terms(query);
        // Global history deliberately ignores the sidebar's project filter and
        // collapsed groups. Archived conversations remain searchable too.
        // Like the sidebar, only list top-level sessions, not side chats or MCP workers.
        let scored = state
            .chats
            .iter()
            .filter(|chat| chat.is_top_level())
            .filter_map(|chat| {
                let pull_request = state
                    .change_request_for_chat(chat)
                    .map(|pr| {
                        format!(
                            "#{} {} {} {}",
                            pr.number, pr.title, pr.head_ref, pr.base_ref
                        )
                    })
                    .unwrap_or_default();
                let fields = thread_search::ThreadFields {
                    title: chat.title.as_deref().unwrap_or("New session"),
                    project: state
                        .space_for_chat(chat)
                        .map(|s| s.display_name())
                        .unwrap_or(""),
                    branch: crate::change_requests::conversation_branch(chat, &state.spaces)
                        .unwrap_or(""),
                    device: state.device_name(&chat.device_id).unwrap_or(""),
                    pull_request: &pull_request,
                    archived: chat.archived,
                };
                let score = thread_search::score_thread(&fields, &terms)?;
                let active = chat.last_message_at.unwrap_or(chat.created_at);
                Some((chat.id.clone(), score, active.timestamp_millis()))
            })
            .collect();
        thread_search::rank(scored, limit)
    }

    /// Pointer motion moves the highlight, so hover and keyboard never light
    /// two rows. Motion only: rows scrolling under a resting pointer must not
    /// steal the keyboard's place.
    fn hover_command(&mut self, key: RowKey, ix: usize, cx: &mut Context<Self>) {
        if let Some(palette) = self.command_palette.as_mut()
            && palette.active.as_ref() != Some(&key)
        {
            palette.active = Some(key);
            palette.active_index = ix;
            cx.notify();
        }
    }

    fn move_palette_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let sections = self.palette_sections(cx);
        let rows = flat_rows(&sections);
        let count = rows.len();
        let Some(palette) = self.command_palette.as_mut() else {
            return;
        };
        if count == 0 {
            return;
        }
        let current = palette.active_position(&rows);
        let next = if down {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        palette.set_active(&rows, next);
        // A section's first row brings its header along, and the very first
        // row returns to the top, so a header never hides above the edge.
        if next == 0 {
            palette.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        } else {
            palette
                .scroll
                .scroll_to_item(scroll_child_index(&sections, next));
        }
        cx.notify();
    }

    fn activate_palette_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sections = self.palette_sections(cx);
        let rows = flat_rows(&sections);
        let Some(row) = self
            .command_palette
            .as_ref()
            .and_then(|palette| rows.get(palette.active_position(&rows)))
            .map(|row| (*row).clone())
        else {
            return;
        };
        self.activate_palette_row(row, window, cx);
    }

    fn activate_palette_row(
        &mut self,
        row: PaletteRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let RowKind::Command(Command::Theme(mode)) = row.kind {
            // Keep the palette open so this ordinary action updates to its next state.
            crate::appearance::set_mode(mode, cx);
            cx.notify();
            return;
        }
        self.close_command_palette(window, cx);
        match row.kind {
            RowKind::Command(Command::NewChat) => self.open_new_session(None, cx),
            RowKind::Command(Command::NewProject) => self.open_add_space(cx),
            RowKind::Command(Command::ResumeSession) => self.toggle_resume_picker(window, cx), // slopty
            RowKind::Command(Command::Settings) => self.open_last_settings(cx),
            RowKind::Command(Command::Theme(_)) => unreachable!(),
            RowKind::Command(Command::ArchiveThread) => self.archive_selected_chat(cx),
            // slopty: after the palette closed, so the key's handler sees the window as it is.
            RowKind::Command(Command::Window(command)) => {
                window.dispatch_action((command.action)(), cx)
            }
            RowKind::Chat(id) => self.open_chat(id, cx),
            RowKind::ChatWords(found) => self.open_chat(found.chat_id, cx), // slopty
            RowKind::File { path, is_dir } => {
                self.open_palette_path(path, is_dir, None, window, cx)
            }
            RowKind::Content(found) => {
                let line = u32::try_from(found.line).unwrap_or(u32::MAX);
                let column = Some(FileColumn::Byte(found.column));
                self.open_palette_path(found.path, false, Some((line, column)), window, cx)
            }
        }
    }

    /// Open a workspace path of the focused chat: a file in an editor tab
    /// (at `location`, when given), a folder revealed in the files tree.
    fn open_palette_path(
        &mut self,
        path: String,
        is_dir: bool,
        location: Option<(u32, Option<FileColumn>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_chat.is_empty() {
            return;
        }
        if is_dir {
            self.add_files_surface(window, cx);
            if let Some(files) = self.files.get(&self.panel_key(cx)).cloned() {
                files.update(cx, |files, cx| files.reveal_file_explicit(path, cx));
            }
            return;
        }
        let key = self.panel_key(cx);
        let was_open = self.panels.get(&key).changes_open;
        let from = self.right_target(cx);
        self.panels.update(&key, |panel| panel.changes_open = true);
        if !was_open {
            self.right_tween = Some(WidthTween::new(from, self.right_target(cx), self.now()));
        }
        let owner = (self.active_chat.clone(), self.state.clone());
        self.add_file_surface_at(owner, path, location, window, cx);
    }

    fn command_shortcut(&self, command: &Command, cx: &App) -> Option<String> {
        let id = match command {
            Command::NewChat => ShortcutId::NewSession,
            Command::NewProject => ShortcutId::NewProject,
            Command::ResumeSession => ShortcutId::ResumeSession, // slopty
            Command::Settings => return Some(crate::keys::combo_text("mod-,").to_string()),
            Command::ArchiveThread => ShortcutId::ArchiveSession,
            Command::Theme(_) => return None,
            // slopty: the key the keymap holds for it now.
            Command::Window(command) => {
                return crate::keys::for_action_anywhere((command.action)().as_ref(), cx)
                    .map(|keys| keys.to_string());
            }
        };
        let combo = self.settings.keymap.get(id);
        let valid = crate::settings::parse_combo(combo).is_ok();
        Some(
            crate::keys::combo_text(if valid { combo } else { id.default_combo() }).to_string(),
        )
    }

    fn render_palette_row(
        &mut self,
        row: &PaletteRow,
        ix: usize,
        active: bool,
        query: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        Some(match &row.kind {
            RowKind::Command(command) => {
                let label = command.label();
                let shortcut = self.command_shortcut(command, cx);
                let target = row.clone();
                popover::menu_row(theme, active, format!("command-action-{ix}"))
                    .id(("command-action", ix))
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .min_h(px(PALETTE_ROW_HEIGHT)) // slopty: a list row's 32
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_row(target.clone(), window, cx)
                    }))
                    .child(
                        icon(command.icon())
                            .size(px(16.0))
                            .flex_none()
                            .text_color(theme.text_muted),
                    )
                    .child(div().flex_1().min_w_0().child(popover::search_highlight(
                        label.into(),
                        Some(query),
                        theme,
                    )))
                    .when_some(shortcut, |row, shortcut| {
                        row.child(popover::kbd_hint(theme, &shortcut))
                    })
                    .into_any_element()
            }
            RowKind::File { path, is_dir } => {
                let (name, parent) = split_path(path);
                let identity = if *is_dir {
                    crate::file_icons::FileIconIdentity::directory(path, false)
                } else {
                    crate::file_icons::FileIconIdentity::file(path)
                };
                let target = row.clone();
                popover::menu_row(theme, active, format!("command-file-{ix}"))
                    .id(("command-file", ix))
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(path.clone()))
                    .min_h(px(PALETTE_ROW_HEIGHT)) // slopty: a list row's 32
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_row(target.clone(), window, cx)
                    }))
                    .child(crate::file_icons::icon(identity, theme.appearance).size(px(16.0)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .overflow_hidden()
                            .child(div().flex_none().child(popover::search_highlight(
                                SharedString::from(name.to_owned()),
                                Some(query),
                                theme,
                            )))
                            .when(!parent.is_empty(), |row| {
                                row.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(theme.text_muted)
                                        .child(SharedString::from(parent.to_owned())),
                                )
                            }),
                    )
                    .into_any_element()
            }
            RowKind::Content(found) => {
                let target = row.clone();
                let ranges = found
                    .ranges
                    .iter()
                    .map(|(start, end)| *start as usize..*end as usize)
                    .collect();
                popover::menu_row(theme, active, format!("command-content-{ix}"))
                    .id(("command-content", ix))
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!(
                        "{}:{} {}",
                        found.path, found.line, found.preview
                    )))
                    .min_h(px(PALETTE_ROW_HEIGHT)) // slopty: a list row's 32
                    .py(px(4.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_row(target.clone(), window, cx)
                    }))
                    .child(
                        crate::file_icons::icon(
                            crate::file_icons::FileIconIdentity::file(&found.path),
                            theme.appearance,
                        )
                        .size(px(16.0)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(200.0))
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(format!("{}:{}", found.path, found.line))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family(theme.font_mono.clone())
                            .child(popover::search_highlight_ranges(
                                SharedString::from(found.preview.clone()),
                                ranges,
                                theme,
                            )),
                    )
                    .into_any_element()
            }
            // slopty: the chat's title, and under it who said the words and the words, the
            // match in `text` over the faint line.
            RowKind::ChatWords(found) => {
                let state = self.state.read(cx);
                let chat = state.chats.iter().find(|chat| chat.id == found.chat_id)?;
                let title: SharedString =
                    transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into();
                let harness = chat.config.as_ref().map(|config| config.harness);
                let who = if found.agent { "Agent:" } else { "You:" };
                let target = row.clone();
                let glyph = match harness.map(crate::pickers::harness_brand_icon) {
                    Some((path, tint)) => icon(path).text_color(tint.unwrap_or(theme.text_muted)),
                    None => icon(icons::CHAT_ROUND_LINE).text_color(theme.text_muted),
                };
                popover::menu_row(theme, active, format!("command-words-{ix}"))
                    .id(("command-words", ix))
                    .rounded(px(popover::PALETTE_ITEM_RADIUS))
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("{title}, {who} {}", found.excerpt)))
                    .min_h(px(44.0))
                    .py(px(5.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.activate_palette_row(target.clone(), window, cx)
                    }))
                    .child(glyph.size(px(16.0)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(div().truncate().child(popover::search_highlight(
                                title,
                                Some(query),
                                theme,
                            )))
                            .child(chat_words_line(who, found, theme)),
                    )
                    .into_any_element()
            }
            RowKind::Chat(id) => {
                let state = self.state.read(cx);
                let chat = state.chats.iter().find(|chat| &chat.id == id)?;
                // Project-less sessions name only their device.
                let project = match (state.space_for_chat(chat), chat.space_id.as_deref()) {
                    (Some(space), _) => Some(space.display_name()),
                    (None, None) => None,
                    _ => Some("?"),
                };
                let folder = match (project, state.device_name(&chat.device_id)) {
                    (Some(project), Some(device)) => format!("{project} @ {device}"),
                    (Some(project), None) => project.to_string(),
                    (None, device) => device.unwrap_or_default().to_string(),
                };
                let branch = crate::change_requests::conversation_branch(chat, &state.spaces)
                    .map(str::trim)
                    .filter(|branch| !branch.is_empty())
                    .map(SharedString::from);
                let pr = state.change_request_for_chat(chat).cloned();
                let harness = chat.config.as_ref().map(|c| c.harness);
                self.render_chat_row(
                    id.clone(),
                    transcript::single_line(chat.title.as_deref().unwrap_or("New session")).into(),
                    None,
                    format_time_ago(chat.last_message_at.unwrap_or(chat.created_at), crate::clock::utc(cx))
                        .into(),
                    folder.into(),
                    branch,
                    pr,
                    harness,
                    state.display_status_for(chat, crate::clock::utc(cx)),
                    active,
                    chat.archived,
                    false,
                    None,
                    None,
                    Some(query),
                    theme,
                    cx,
                )
            }
        })
    }

    pub(super) fn render_command_palette(
        &mut self,
        viewport: gpui::Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let sections = self.palette_sections(cx);
        let palette = self.command_palette.as_mut()?;
        if std::mem::take(&mut palette.focus_pending) {
            window.focus(&palette.search.focus_handle(cx), cx);
        }
        let row_count = sections
            .iter()
            .map(|section| section.rows.len())
            .sum::<usize>();
        let active = palette.active_position(&flat_rows(&sections));
        let search = palette.search.clone();
        let query = search.read(cx).text().to_string();
        let focus = palette.focus.clone();
        let scroll = palette.scroll.clone();
        let current_tab = palette.tab;
        let theme = Theme::of(cx).for_popup();
        let mut children = Vec::new();
        let mut ix = 0;
        for (section_ix, section) in sections.iter().enumerate() {
            // End spacing belongs to the content, so it scrolls out of the
            // fade instead of leaving a permanent gutter beside the chrome.
            let mut header = div()
                .flex_none()
                .when(section_ix == 0, |header| header.pt(px(8.0)));
            // slopty: groups stand 16 apart with no rule between them (opencode's command
            // dialog), the space alone parting them.
            if section_ix > 0 {
                header = header.pt(px(14.0));
            }
            children.push(
                header
                    .child(palette_section_header(
                        &theme,
                        section.section.title(),
                        section.status.clone(),
                    ))
                    .into_any_element(),
            );
            for row in &section.rows {
                let key = row.key();
                let content = self.render_palette_row(row, ix, ix == active, &query, &theme, cx);
                let Some(content) = content else {
                    ix += 1;
                    continue;
                };
                children.push(
                    div()
                        .id(("command-result", ix))
                        .flex_none()
                        .on_mouse_move(cx.listener(move |this, _: &gpui::MouseMoveEvent, _, cx| {
                            this.hover_command(key.clone(), ix, cx)
                        }))
                        .when(ix + 1 == row_count, |row| row.pb(px(8.0)))
                        .child(div().px(px(8.0)).child(content))
                        .into_any_element(),
                );
                ix += 1;
            }
        }
        let body = div()
            .id("command-results")
            .min_h_0()
            .max_h(px(palette_results_height(viewport)))
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .flex()
            .flex_col()
            .gap(px(SIDEBAR_LIST_GAP))
            .children(children)
            .when(sections.is_empty(), |el| {
                el.child(palette_empty(
                    &theme,
                    "No results",
                    "Try a command, chat title, project, or device.",
                ))
            });
        let body = palette_results_fade(body, &scroll);
        let tabs = div()
            .flex_none()
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .gap(px(4.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.06))
            .children(Tab::ALL.into_iter().map(|tab| {
                let combo = crate::keys::combo_text(&format!("mod-{}", tab.index() + 1));
                // The app's tab chip (right-pane surfaces, PR board), not a
                // menu row: its height, type and washes.
                crate::surface_chrome::tab(("command-tab", tab.index()), tab == current_tab, &theme)
                    .px(px(8.0))
                    .role(gpui::Role::Tab)
                    .aria_label(tab.label())
                    .gap(px(6.0))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_palette_tab(tab, cx)))
                    .child(tab.label())
                    .child(popover::kbd_hint(&theme, &combo))
            }));
        let card = palette_card("command-palette", &focus, viewport, &theme)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, action: &SelectPaletteTab, _, cx| {
                if let Some(tab) = Tab::ALL.get(action.0) {
                    this.select_palette_tab(*tab, cx);
                }
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "up" | "down" => {
                            this.move_palette_selection(event.keystroke.key == "down", cx)
                        }
                        "enter" => {
                            let activate = this
                                .command_palette
                                .as_mut()
                                .is_some_and(|palette| palette.enter_press.press(event.is_held));
                            if !activate {
                                cx.stop_propagation();
                                return;
                            }
                            this.activate_palette_selection(window, cx);
                        }
                        "tab" => {
                            let forward = !event.keystroke.modifiers.shift;
                            if let Some(tab) = this
                                .command_palette
                                .as_ref()
                                .map(|palette| palette.tab.step(forward))
                            {
                                this.select_palette_tab(tab, cx);
                            }
                        }
                        "escape" => this.close_command_palette(window, cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }),
            )
            .on_key_up(cx.listener(|this, event: &gpui::KeyUpEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    if let Some(palette) = this.command_palette.as_mut() {
                        palette.enter_press.release();
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.close_command_palette(window, cx)),
            )
            .child(palette_header(
                &theme,
                search.into_any_element(),
                popover::kbd_hint(&theme, &crate::keys::combo_text("mod-k")),
            ))
            .child(tabs)
            .child(body)
            .child(
                palette_footer()
                    .child(command_key_hint(&theme, "↑ ↓", "Navigate", window, cx))
                    .child(command_key_hint(&theme, "⏎", "Open", window, cx))
                    .child(command_key_hint(&theme, "Esc", "Close", window, cx)),
            );
        Some(palette_overlay("command-palette", viewport, card))
    }
}

// slopty: a chat words match's second line: who said it, then the words in `text_faint` with
// the match in `text`, one line, cut at the row's end.
/// "Agent: …the words…", the match lit.
fn chat_words_line(who: &'static str, found: &ChatWordsMatch, theme: &Theme) -> gpui::Div {
    let lead = who.len() + 1;
    let line: SharedString = format!("{who} {}", found.excerpt).into();
    let mut highlights = vec![(
        0..who.len(),
        gpui::HighlightStyle { color: Some(theme.text_muted), ..Default::default() },
    )];
    if !found.range.is_empty() {
        highlights.push((
            found.range.start + lead..found.range.end + lead,
            gpui::HighlightStyle {
                color: Some(theme.text),
                font_weight: Some(gpui::FontWeight::MEDIUM),
                ..Default::default()
            },
        ));
    }
    div()
        .min_w_0()
        .truncate()
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(theme.text_faint)
        .child(gpui::StyledText::new(line).with_highlights(highlights))
}

/// A section's title, in the sidebar's secondary text style, with its
/// search status trailing.
fn palette_section_header(
    theme: &Theme,
    title: &'static str,
    status: Option<SharedString>,
) -> gpui::Div {
    div()
        .px(px(16.0))
        .pt(px(2.0))
        .pb(px(4.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        // slopty: opencode's group title, 13 at the body's weight in the muted ink.
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text_muted)
        .child(title)
        .when_some(status, |header, status| {
            header.child(div().min_w_0().truncate().opacity(0.7).child(status))
        })
}

/// The results list's max height; shared so every palette sits at one size.
pub(super) fn palette_results_height(viewport: gpui::Size<Pixels>) -> f32 {
    (f32::from(viewport.height) - 180.0).clamp(100.0, 360.0)
}

/// Scroll fades at whichever list edge hides rows.
pub(super) fn palette_results_fade(
    body: impl IntoElement,
    scroll: &gpui::ScrollHandle,
) -> crate::edge_fade::EdgeFaded {
    crate::edge_fade::edge_faded(RESULTS_FADE_BAND, true, true, body).fade_overflow_y(scroll)
}

pub(super) fn palette_empty(
    theme: &Theme,
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
) -> gpui::Div {
    div()
        .w_full()
        .py(px(24.0))
        .px(px(16.0))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.0))
        .text_size(crate::typography::ui_rems(13.0))
        .child(title.into())
        .child(div().text_color(theme.text_muted).child(hint.into()))
}

/// The palette's glass card; callers add key handling and sections.
pub(super) fn palette_card(
    id: &'static str,
    focus: &FocusHandle,
    viewport: gpui::Size<Pixels>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    // slopty: its footer's caps sink while their keys are held.
    crate::live::chrome::track_held_keys(div().id(id))
        .debug_selector(move || id.to_owned()) // slopty: for the tests
        .track_focus(focus)
        .w(px(560.0_f32.min(f32::from(viewport.width) - 32.0)))
        // slopty: between 280 and 480 tall (`PALETTE_MIN_HEIGHT`), within the window under its
        // top, its footer at its foot.
        .min_h(px(PALETTE_MIN_HEIGHT.min(
            f32::from(viewport.height - palette_top(viewport.height)) - 16.0,
        )))
        .max_h(px(PALETTE_MAX_HEIGHT))
        .flex()
        .flex_col()
        .rounded(px(16.0))
        // slopty: Amp's floating elevation, a ring inside the edge and the long soft shadows.
        .shadow(theme.elevation(crate::theme::Elevation::Floating))
        .bg(popover::surface_bg(theme))
        .text_color(theme.text)
}

pub(super) fn palette_header(theme: &Theme, search: AnyElement, hint: gpui::Div) -> gpui::Div {
    div()
        .min_h(px(44.0))
        .flex_none()
        .px(px(16.0))
        .py(px(8.0))
        .flex()
        .items_center()
        .gap(px(10.0))
        .border_b_1()
        .border_color(crate::theme::hairline(0.06))
        .child(popover::palette_search_icon(theme))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(crate::typography::ui_rems(14.0))
                .child(search),
        )
        .child(hint)
}

pub(super) fn palette_footer() -> gpui::Div {
    div()
        .flex_none()
        .mt_auto() // slopty: at the card's foot, however few rows answer
        .px(px(16.0))
        .py(px(7.0))
        .border_t_1()
        .border_color(crate::theme::hairline(0.06))
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(12.0))
}

// slopty: Claude desktop's overlay entrance (`dur-snap` 120 ms on `cubic(0.32,0.72,0,1)`), its
// card's .98 scale approximated by a 4 px settle as `motion::menu_in` approximates its own.
const OVERLAY_EASE: crate::motion::CubicBezier = crate::motion::CubicBezier::new(0.32, 0.72, 0.0, 1.0);
/// The scrim's fade in under a palette.
const SCRIM_IN: crate::motion::MotionSpec = crate::motion::MotionSpec::new(120, OVERLAY_EASE);
/// The palette card's fade and settle in.
const CARD_IN: crate::motion::MotionSpec = crate::motion::MotionSpec::new(150, OVERLAY_EASE);

/// slopty: a palette row's height, a list row's as Claude desktop draws them, and as the pickers'
/// rows stand (`popover::PICKER_ROW_HEIGHT`).
const PALETTE_ROW_HEIGHT: f32 = popover::PICKER_ROW_HEIGHT;

/// The tallest a palette stands, and the least: opencode's command dialog's 480 and 280, so its
/// results grow down inside a card whose foot moves little.
pub(super) const PALETTE_MAX_HEIGHT: f32 = popover::MODAL_MAX_HEIGHT;
pub(super) const PALETTE_MIN_HEIGHT: f32 = 280.0;

/// Where a palette's top edge stands in a window `height` tall: where a centred palette at its
/// tallest would start, 48 at the least (opencode's `max(48px, (100vh − 480px) / 2)`), so its
/// search field stays put however many rows answer.
pub(super) fn palette_top(height: Pixels) -> Pixels {
    popover::modal_top_edge(height)
}

/// Mount a palette card over the scrimmed window, frosted like the composer.
// slopty: pinned from the top (`palette_top`), not centred, so the field does not move as the
// results change, and it comes in over Claude desktop's overlay entrance; `id` keys it.
pub(super) fn palette_overlay(
    id: &'static str,
    viewport: gpui::Size<Pixels>,
    card: gpui::Stateful<gpui::Div>,
) -> AnyElement {
    // Match the composer's 16px backdrop blur, including its opaque fallback.
    let card = crate::frost::frosted(16.0, crate::frost::MENU_BLUR, card);
    let card = div().relative().child(card).with_animation(
        SharedString::from(format!("{id}-in")),
        CARD_IN.animation(),
        |el, t| el.opacity(t).top(px(-4.0 * (1.0 - t))),
    );
    gpui::deferred(
        gpui::anchored()
            .position(gpui::point(px(0.0), px(0.0)))
            .child(
                div()
                    .occlude()
                    .w(viewport.width)
                    .h(viewport.height)
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt(palette_top(viewport.height))
                    .child(card)
                    // Match glass modals: quiet the background while
                    // preserving its color through the frosted palette.
                    .with_animation(
                        SharedString::from(format!("{id}-scrim")),
                        SCRIM_IN.animation(),
                        |el, t| el.bg(popover::scrim_alpha(0.35 * t)),
                    ),
            ),
    )
    .priority(2)
    .into_any_element()
}

/// A footer legend: the keys on a key cap (footers keep their caps, unlike
/// the plain inline legends) and the verb after it. Arrow and return keys
/// are icons, which sit optically centered on the cap where font glyphs
/// don't; other keys ("Esc") are text.
// slopty: the cap sinks while its key is held (`live::chrome::pressable_cap`).
pub(super) fn command_key_hint(
    theme: &Theme,
    keys: &str,
    label: &'static str,
    window: &mut Window,
    cx: &gpui::App,
) -> gpui::Div {
    let glyph = |path: &'static str| {
        crate::icons::icon(path)
            .size(px(12.5))
            .text_color(theme.text_muted)
    };
    let cap = popover::key_cap(theme);
    let cap = match keys {
        "⏎" | "↵" => cap.child(glyph(crate::icons::RETURN)),
        "←" => cap.child(glyph(crate::icons::ARROW_LEFT)),
        "↑ ↓" => cap
            .child(glyph(crate::icons::ARROW_UP))
            .child(glyph(crate::icons::ARROW_DOWN)),
        _ => cap
            .font_family(popover::KEY_GLYPH_FONT)
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(theme.text_muted)
            .child(SharedString::from(keys.to_string())),
    };
    let selector = format!("key-cap-{keys}");
    let cap = crate::live::chrome::pressable_cap(cap, keys, 0.05, window, cx)
        .debug_selector(move || selector); // slopty: for the tests
    div()
        .flex()
        .items_center()
        .gap(px(5.0))
        .child(cap)
        .child(
            div()
                .text_size(crate::typography::ui_rems(10.0))
                .text_color(theme.text_muted)
                .child(label),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::AppearanceMode;
    use gpui::{AppContext, TestAppContext};

    fn palette_window(cx: &mut TestAppContext) -> (gpui::WindowHandle<Shell>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
            crate::app_menus::init(cx);
        });
        let window = cx.add_window(|_, cx| {
            let state = cx.new(|_| AppState::new());
            Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    // slopty: Slopty's server is the only backend.
                    default_harness: slopty_view_proto::HarnessId::MOCK,
                },
                cx,
            )
        });
        (window, dir)
    }

    fn chat(id: &str, parent: Option<&str>, archived: bool, age: i64) -> slopty_view_proto::Chat {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": id, "deviceId": "local", "archived": archived,
            "parentChatId": parent,
            // slopty: recent, so a chat stays out of the sidebar's shut Inactive group.
            "createdAt": Utc::now() - chrono::Duration::minutes(age),
        }))
        .unwrap()
    }

    fn search_chats(shell: &Shell, query: &str, cx: &mut Context<Shell>) -> Vec<String> {
        shell
            .command_palette
            .as_ref()
            .unwrap()
            .search
            .update(cx, |input, cx| {
                input.set_text(query, cx);
            });
        shell
            .palette_sections(cx)
            .into_iter()
            .flat_map(|section| section.rows)
            .filter_map(|row| match row.kind {
                RowKind::Chat(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// slopty: a footer cap sinks while its key is held and rises as it comes up; a key that
    /// closed the palette, coming up where nothing listened, leaves no cap sunk next time.
    #[gpui::test]
    fn a_footer_cap_sinks_while_its_key_is_held(cx: &mut TestAppContext) {
        let (window, _dir) = keyed_palette_window(cx);
        cx.update(|cx| crate::motion::set_reduced_motion(cx, true));
        let open = |cx: &mut TestAppContext| {
            window
                .update(cx, |shell, window, cx| shell.toggle_command_palette(window, cx))
                .unwrap();
        };
        open(cx);
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        let key = |down: bool, key: &'static str, cx: &mut gpui::VisualTestContext| {
            let keystroke = gpui::Keystroke::parse(key).unwrap();
            cx.update(|window, cx| {
                window.dispatch_event(
                    if down {
                        gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                            keystroke,
                            is_held: false,
                            prefer_character_input: false,
                        })
                    } else {
                        gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke })
                    },
                    cx,
                );
            });
            draw(cx);
        };
        draw(cx);
        draw(cx);
        let rest = cx.debug_bounds("key-cap-↑ ↓").expect("the Navigate cap");
        let esc = cx.debug_bounds("key-cap-Esc").expect("the Close cap");
        key(true, "down", cx);
        let held = cx.debug_bounds("key-cap-↑ ↓").unwrap();
        assert_eq!(held.origin.y, rest.origin.y + px(1.0), "sunk while ↓ is held");
        assert_eq!(cx.debug_bounds("key-cap-Esc").unwrap(), esc, "the others stay up");
        key(false, "down", cx);
        assert_eq!(cx.debug_bounds("key-cap-↑ ↓").unwrap(), rest, "up again");
        key(true, "escape", cx);
        assert!(cx.debug_bounds("key-cap-Esc").is_none(), "Escape closed the palette");
        cx.update(|window, cx| {
            window
                .root::<Shell>()
                .flatten()
                .unwrap()
                .update(cx, |shell, cx| shell.toggle_command_palette(window, cx));
        });
        draw(cx);
        draw(cx);
        assert_eq!(cx.debug_bounds("key-cap-Esc").unwrap(), esc, "no cap left sunk");
    }

    #[gpui::test]
    fn history_excludes_child_chats_with_and_without_search(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(vec![
                        chat("manual-sidechat", Some("main-session"), false, 0),
                        chat("mcp-worker", Some("main-session"), false, 1),
                        chat("archived-sidechat", Some("main-session"), true, 2),
                        chat("orphan-sidechat", Some("deleted-parent"), false, 3),
                        chat("main-session", None, false, 4),
                        chat("archived-session", None, true, 5),
                    ]);
                });
                shell.toggle_command_palette(window, cx);
                // Recent Threads leaves archived threads to search.
                assert_eq!(search_chats(shell, "", cx), ["main-session"]);
                for query in [
                    "manual-sidechat",
                    "mcp-worker",
                    "archived-sidechat",
                    "orphan-sidechat",
                ] {
                    assert!(
                        search_chats(shell, query, cx).is_empty(),
                        "{query} must stay out of global history"
                    );
                }
                assert_eq!(search_chats(shell, "main-session", cx), ["main-session"]);
                assert_eq!(
                    search_chats(shell, "archived-session", cx),
                    ["archived-session"]
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn child_chats_do_not_consume_history_result_slots(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    let children = (0..THREADS_TAB_LIMIT).map(|ix| {
                        chat(&format!("child-{ix}"), Some("session-0"), false, ix as i64)
                    });
                    let sessions = (0..=THREADS_TAB_LIMIT).map(|ix| {
                        chat(
                            &format!("session-{ix}"),
                            None,
                            false,
                            (THREADS_TAB_LIMIT + ix) as i64,
                        )
                    });
                    state.apply_chats(children.chain(sessions).collect());
                });
                shell.toggle_command_palette(window, cx);
                shell.select_palette_tab(Tab::Threads, cx);
                let expected: Vec<_> = (0..THREADS_TAB_LIMIT)
                    .map(|ix| format!("session-{ix}"))
                    .collect();
                assert_eq!(search_chats(shell, "", cx), expected);
                let oldest = format!("session-{THREADS_TAB_LIMIT}");
                assert_eq!(search_chats(shell, &oldest, cx), [oldest]);
            })
            .unwrap();
    }

    fn sections(shell: &Shell, cx: &App) -> Vec<(Section, Vec<RowKind>)> {
        shell
            .palette_sections(cx)
            .into_iter()
            .map(|section| {
                (
                    section.section,
                    section.rows.into_iter().map(|row| row.kind).collect(),
                )
            })
            .collect()
    }

    fn titled(id: &str, title: &str, archived: bool, age: i64) -> slopty_view_proto::Chat {
        let mut chat = chat(id, None, archived, age);
        chat.title = Some(title.into());
        chat
    }

    #[gpui::test]
    fn thread_search_ranks_by_score_with_archived_below(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(vec![
                        titled("mid-word", "Redeploy the edge", false, 0),
                        titled("archived", "Deploy notes", true, 1),
                        titled("older", "Deploy staging", false, 3),
                        titled("newer", "Deploy production", false, 2),
                        titled("unrelated", "Fix the composer", false, 0),
                    ]);
                });
                shell.toggle_command_palette(window, cx);
                assert_eq!(
                    search_chats(shell, "deploy", cx),
                    ["newer", "older", "mid-word", "archived"]
                );
                shell.select_palette_tab(Tab::Threads, cx);
                assert_eq!(search_chats(shell, "deploy staging", cx), ["older"]);
            })
            .unwrap();
    }

    #[gpui::test]
    fn all_tab_lists_threads_before_commands_and_caps_them(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(
                        (0..8)
                            .map(|ix| titled(&format!("t{ix}"), "New project idea", false, ix))
                            .collect(),
                    );
                });
                shell.toggle_command_palette(window, cx);
                shell
                    .command_palette
                    .as_ref()
                    .unwrap()
                    .search
                    .update(cx, |input, cx| input.set_text("new", cx));
                let sections = sections(shell, cx);
                assert_eq!(sections[0].0, Section::Threads);
                assert_eq!(sections[0].1.len(), ALL_TAB_LIMIT);
                assert_eq!(sections[1].0, Section::Commands);
                shell.select_palette_tab(Tab::Threads, cx);
                assert_eq!(search_chats(shell, "new", cx).len(), 8);
            })
            .unwrap();
    }

    #[gpui::test]
    fn empty_state_without_a_focused_chat_has_no_files_or_archive(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(vec![chat("only", None, false, 0)]);
                });
                shell.toggle_command_palette(window, cx);
                let sections = sections(shell, cx);
                assert_eq!(
                    sections
                        .iter()
                        .map(|(section, _)| *section)
                        .collect::<Vec<_>>(),
                    [Section::QuickActions, Section::RecentThreads]
                );
                assert!(
                    !sections[0]
                        .1
                        .contains(&RowKind::Command(Command::ArchiveThread))
                );
            })
            .unwrap();
    }

    #[gpui::test]
    fn empty_state_lists_recent_files_actions_and_threads(cx: &mut TestAppContext) {
        let (window, _dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, cx| {
                    state.apply_chats(vec![
                        chat("focused", None, false, 0),
                        chat("side", Some("focused"), false, 1),
                        chat("older", None, false, 3),
                        chat("newer", None, false, 2),
                        chat("archived", None, true, 1),
                    ]);
                    state.select_chat(Some("focused".into()), cx);
                });
                shell.active_chat = "focused".into();
                for path in ["src/a.rs", "README.md", "src/a.rs", "docs/b.md"] {
                    shell.note_recent_file("focused", path);
                }
                shell.note_recent_file("newer", "elsewhere.rs");
                shell.toggle_command_palette(window, cx);
                let sections = sections(shell, cx);
                let file = |path: &str| RowKind::File {
                    path: path.into(),
                    is_dir: false,
                };
                assert_eq!(sections[0].0, Section::RecentFiles);
                assert_eq!(
                    sections[0].1,
                    [file("docs/b.md"), file("src/a.rs"), file("README.md")]
                );
                assert_eq!(sections[1].0, Section::QuickActions);
                assert!(
                    sections[1]
                        .1
                        .contains(&RowKind::Command(Command::ArchiveThread))
                );
                assert_eq!(sections[2].0, Section::RecentThreads);
                assert_eq!(
                    sections[2].1,
                    [RowKind::Chat("newer".into()), RowKind::Chat("older".into())]
                );
            })
            .unwrap();
    }

    // slopty: the palette's headings are in sentence case, as every label in the app; New session
    // wears the titlebar's glyph for it, and New project, which opens a picker, says so.
    #[test]
    fn the_palette_speaks_as_the_app_does() {
        for section in [
            Section::RecentFiles,
            Section::QuickActions,
            Section::RecentThreads,
            Section::Threads,
            Section::Commands,
            Section::Files,
            Section::InFiles,
            Section::InThreads,
        ] {
            let title = section.title();
            let rest = title.split_whitespace().skip(1);
            assert!(rest.clone().all(|word| word.chars().all(char::is_lowercase)), "{title}");
        }
        assert_eq!(Command::NewChat.icon(), icons::PLUS);
        assert!(Command::NewProject.label().ends_with('\u{2026}'));
    }

    #[test]
    fn archive_is_offered_only_with_a_focused_chat() {
        assert!(!commands_for("archive", true, false, false).contains(&Command::ArchiveThread));
        assert_eq!(
            commands_for("archive", true, true, false),
            vec![Command::ArchiveThread]
        );
    }

    #[test]
    fn file_rows_split_name_and_parent_folder() {
        assert_eq!(
            split_path("src/shell/palette.rs"),
            ("palette.rs", "src/shell")
        );
        assert_eq!(split_path("README.md"), ("README.md", ""));
        assert_eq!(split_path("crates/ui/"), ("ui", "crates"));
    }

    /// The engine side of a shell under test: requests out, replies in.
    struct FakeEngine {
        runtime: tokio::runtime::Runtime,
        requests: tokio::sync::mpsc::Receiver<String>,
        replies: tokio::sync::mpsc::Sender<String>,
    }

    impl FakeEngine {
        /// Every request sent so far for `method`; other methods are
        /// answered with an empty object (or a ready search index).
        fn take(&mut self, method: &str, cx: &mut TestAppContext) -> Vec<serde_json::Value> {
            self.take_after(method, CONTENT_DEBOUNCE, cx)
        }

        // slopty: what was asked once the clock moved `step` per round; zero asks what was
        // sent without letting any pause end.
        fn take_after(
            &mut self,
            method: &str,
            step: Duration,
            cx: &mut TestAppContext,
        ) -> Vec<serde_json::Value> {
            let mut found = Vec::new();
            loop {
                cx.executor().advance_clock(step);
                cx.run_until_parked();
                let Ok(request) = self.requests.try_recv() else {
                    return found;
                };
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                if request["method"] == method {
                    found.push(request);
                } else if request["method"] == methods::WARM_WORKSPACE_SEARCH {
                    self.reply(&request, serde_json::json!({ "state": "ready" }));
                } else {
                    self.reply(&request, serde_json::json!({}));
                }
            }
        }

        fn reply(&self, request: &serde_json::Value, ok: serde_json::Value) {
            self.send(serde_json::json!({ "id": request["id"], "ok": ok }));
        }

        fn fail(&self, request: &serde_json::Value, err: &str) {
            self.send(serde_json::json!({ "id": request["id"], "err": err }));
        }

        fn send(&self, reply: serde_json::Value) {
            // The client's reader task runs on this runtime: let it drain.
            self.runtime.block_on(async {
                self.replies.send(reply.to_string()).await.unwrap();
                while self.replies.capacity() < self.replies.max_capacity() {
                    tokio::task::yield_now().await;
                }
            });
        }
    }

    /// A palette over chat "focused" (plus "deploy-thread"), wired to a fake
    /// engine. `focused: false` leaves no chat in focus.
    fn engine_palette(
        cx: &mut TestAppContext,
        focused: bool,
    ) -> (gpui::WindowHandle<Shell>, tempfile::TempDir, FakeEngine) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (out, requests) = tokio::sync::mpsc::channel(64);
        let (replies, inbound) = tokio::sync::mpsc::channel(64);
        let engine = {
            let _guard = runtime.enter();
            crate::state::EngineHandle::from_test_client(slopty_view_rpc::RpcClient::new(out, inbound))
        };
        let (window, dir) = palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.state.update(cx, |state, cx| {
                    state.local_device_id = Some("local".into());
                    state.apply_chats(vec![
                        chat("focused", None, false, 0),
                        titled("deploy-thread", "Deploy the docs", false, 1),
                    ]);
                    state.set_test_engine(engine);
                    if focused {
                        state.select_chat(Some("focused".into()), cx);
                    }
                });
                if focused {
                    shell.active_chat = "focused".into();
                }
                shell.toggle_command_palette(window, cx);
            })
            .unwrap();
        let fake = FakeEngine {
            runtime,
            requests,
            replies,
        };
        (window, dir, fake)
    }

    fn type_query(window: gpui::WindowHandle<Shell>, query: &str, cx: &mut TestAppContext) {
        window
            .update(cx, |shell, _, cx| {
                shell
                    .command_palette
                    .as_ref()
                    .unwrap()
                    .search
                    .update(cx, |input, cx| input.set_text(query, cx));
            })
            .unwrap();
    }

    fn palette_section(
        window: gpui::WindowHandle<Shell>,
        section: Section,
        cx: &mut TestAppContext,
    ) -> Option<PaletteSection> {
        window
            .read_with(cx, |shell, cx| {
                shell
                    .palette_sections(cx)
                    .into_iter()
                    .find(|found| found.section == section)
            })
            .unwrap()
    }

    fn name_match(path: &str, kind: &str) -> serde_json::Value {
        serde_json::json!({ "path": path, "name": path, "kind": kind, "score": 1 })
    }

    /// Settle the independent focus warm-up before controlling search probes.
    fn file_search_palette(
        cx: &mut TestAppContext,
    ) -> (gpui::WindowHandle<Shell>, tempfile::TempDir, FakeEngine) {
        let (window, dir, mut engine) = engine_palette(cx, true);
        for warm in engine.take(methods::WARM_WORKSPACE_SEARCH, cx) {
            engine.reply(&warm, serde_json::json!({ "state": "ready" }));
        }
        cx.run_until_parked();
        (window, dir, engine)
    }

    fn answer_file_index(
        engine: &mut FakeEngine,
        state: &str,
        cx: &mut TestAppContext,
    ) -> serde_json::Value {
        let warm = engine.take(methods::WARM_WORKSPACE_SEARCH, cx);
        assert_eq!(warm.len(), 1);
        assert_eq!(warm[0]["params"]["chatId"], "focused");
        engine.reply(&warm[0], serde_json::json!({ "state": state }));
        let search = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        assert_eq!(search.len(), 1);
        search.into_iter().next().unwrap()
    }

    /// Every request sent so far, each answered: warm-ups as `warm`, name
    /// searches with no matches. Returns the methods in order.
    fn answer_all(engine: &mut FakeEngine, warm: &str, cx: &mut TestAppContext) -> Vec<String> {
        let mut methods_seen = Vec::new();
        loop {
            cx.executor().advance_clock(CONTENT_DEBOUNCE);
            cx.run_until_parked();
            let Ok(request) = engine.requests.try_recv() else {
                return methods_seen;
            };
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            let method = request["method"].as_str().unwrap().to_owned();
            match method.as_str() {
                methods::WARM_WORKSPACE_SEARCH if warm == "unsupported" => engine.fail(
                    &request,
                    &format!("unknown method: {}", methods::WARM_WORKSPACE_SEARCH),
                ),
                methods::WARM_WORKSPACE_SEARCH => {
                    engine.reply(&request, serde_json::json!({ "state": warm }))
                }
                methods::SEARCH_WORKSPACE_FILES => engine.reply(&request, serde_json::json!([])),
                // slopty: the chats' words search is not the workspace's.
                methods::SEARCH_CHATS => {
                    engine.reply(&request, serde_json::json!([]));
                    continue;
                }
                _ => engine.reply(&request, content_answer(Vec::new(), false)),
            }
            if method != methods::SEARCH_WORKSPACE_CONTENT {
                methods_seen.push(method);
            }
        }
    }

    // slopty: the chats' words: a match each, after the pause, from the answer's shape.
    fn words(chat: &str, role: &str, excerpt: &str, start: usize, end: usize) -> serde_json::Value {
        serde_json::json!({
            "chatId": chat, "role": role, "excerpt": excerpt,
            "matchStart": start, "matchEnd": end,
        })
    }

    /// The `SearchChats` requests sent once typing paused.
    fn chat_word_requests(engine: &mut FakeEngine, cx: &mut TestAppContext) -> Vec<serde_json::Value> {
        cx.executor().advance_clock(CHAT_WORDS_DEBOUNCE);
        engine.take(methods::SEARCH_CHATS, cx)
    }

    #[test]
    fn a_chat_words_answer_is_read_defensively() {
        let found = ChatWordsMatch::parse_all(&serde_json::json!([
            words("a", "agent", "both tests\npass now", 11, 15),
            words("a", "user", "a second match of a", 0, 1),
            words("b", "user", "naïve pass", 3, 5),
            { "role": "agent", "excerpt": "no chat" },
            words("c", "agent", "short", 2, 99),
        ]));
        assert_eq!(found.len(), 3, "one per chat, none without a chat: {found:?}");
        assert!(found[0].agent);
        assert_eq!(found[0].excerpt, "both tests pass now", "one line");
        assert_eq!(&found[0].excerpt[found[0].range.clone()], "pass");
        assert!(!found[1].agent);
        assert!(found[1].range.is_empty(), "inside a character: no highlight, still a match");
        assert!(found[2].range.is_empty(), "past the excerpt");
        let wrapped =
            ChatWordsMatch::parse_all(&serde_json::json!({ "matches": [words("d", "agent", "x", 0, 1)] }));
        assert_eq!(wrapped.len(), 1);
        assert!(ChatWordsMatch::parse_all(&serde_json::json!({})).is_empty());
    }

    // slopty: chats whose words match come after the title matches, a title match never twice,
    // and only once typing paused on two characters.
    #[gpui::test]
    fn chat_words_follow_title_matches_after_the_pause(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, false);
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(vec![
                        titled("deploy-thread", "Deploy the docs", false, 1),
                        titled("quiet", "Fix the composer", false, 2),
                        chat("side", Some("quiet"), false, 3),
                    ]);
                });
                shell.select_palette_tab(Tab::Threads, cx);
            })
            .unwrap();
        type_query(window, "d", cx);
        assert!(chat_word_requests(&mut engine, cx).is_empty(), "one character is not sent");
        type_query(window, "deploy", cx);
        cx.executor().advance_clock(CHAT_WORDS_DEBOUNCE - Duration::from_millis(1));
        assert!(
            engine.take_after(methods::SEARCH_CHATS, Duration::ZERO, cx).is_empty(),
            "waits for the pause"
        );
        let asked = chat_word_requests(&mut engine, cx);
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["params"]["query"], "deploy");
        assert_eq!(asked[0]["params"]["limit"], CHAT_WORDS_LIMIT);
        let searching = palette_section(window, Section::InThreads, cx).expect("searching");
        assert_eq!(searching.status.as_deref(), Some("Searching…"));
        engine.reply(
            &asked[0],
            serde_json::json!([
                words("deploy-thread", "agent", "deploy it", 0, 6),
                words("quiet", "agent", "then deploy to staging", 5, 11),
                words("side", "agent", "deploy the side", 0, 6),
                words("gone", "user", "deploy", 0, 6),
            ]),
        );
        cx.run_until_parked();
        let shown = window.read_with(cx, |shell, cx| sections(shell, cx)).unwrap();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0], (Section::Threads, vec![RowKind::Chat("deploy-thread".into())]));
        assert_eq!(shown[1].0, Section::InThreads);
        let ids: Vec<_> = shown[1]
            .1
            .iter()
            .map(|kind| match kind {
                RowKind::ChatWords(found) => found.chat_id.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, ["quiet"], "no title match twice, no side chat, no chat unknown here");
        // The keyboard reaches the words row after the title row.
        window
            .update(cx, |shell, _, cx| {
                shell.move_palette_selection(true, cx);
                let sections = shell.palette_sections(cx);
                let rows = flat_rows(&sections);
                let at = shell.command_palette.as_ref().unwrap().active_position(&rows);
                assert!(matches!(&rows[at].kind, RowKind::ChatWords(found) if found.chat_id == "quiet"));
            })
            .unwrap();
        // Cleared below two characters.
        type_query(window, "q", cx);
        assert!(palette_section(window, Section::InThreads, cx).is_none());
    }

    // slopty: an answer to an older query is dropped; a failed search shows no words.
    #[gpui::test]
    fn stale_or_failed_chat_word_answers_show_nothing(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, false);
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.apply_chats(vec![titled("quiet", "Fix the composer", false, 2)]);
                });
                shell.select_palette_tab(Tab::Threads, cx);
            })
            .unwrap();
        type_query(window, "old", cx);
        let old = chat_word_requests(&mut engine, cx);
        type_query(window, "new words", cx);
        let new = chat_word_requests(&mut engine, cx);
        assert_eq!((old.len(), new.len()), (1, 1));
        engine.reply(&old[0], serde_json::json!([words("quiet", "agent", "old", 0, 3)]));
        cx.run_until_parked();
        let section = palette_section(window, Section::InThreads, cx);
        assert!(
            section.as_ref().is_none_or(|section| section.rows.is_empty()),
            "an older query's answer is dropped: {section:?}"
        );
        engine.fail(&new[0], &format!("unknown method: {}", methods::SEARCH_CHATS));
        cx.run_until_parked();
        assert!(palette_section(window, Section::InThreads, cx).is_none(), "nothing to show");
    }

    #[gpui::test]
    fn the_files_tab_lists_recently_changed_files_with_no_query(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        let listed = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        assert_eq!(listed.len(), 1, "opening asks for the default list once");
        assert_eq!(listed[0]["params"]["query"], "");
        engine.reply(
            &listed[0],
            serde_json::json!([
                name_match("src/recent.rs", "file"),
                name_match("src", "directory"),
                name_match("README.md", "file"),
            ]),
        );
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| shell.select_palette_tab(Tab::Files, cx))
            .unwrap();
        let recent = palette_section(window, Section::RecentFiles, cx).expect("a default list");
        let paths: Vec<_> = recent
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                RowKind::File { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        // Files only; no content search runs without a query.
        assert_eq!(paths, ["src/recent.rs", "README.md"]);
        assert!(engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx).is_empty());
    }

    #[gpui::test]
    fn a_ready_index_skips_the_probe_until_the_palette_reopens(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        let probed = [
            methods::WARM_WORKSPACE_SEARCH,
            methods::SEARCH_WORKSPACE_FILES,
        ];
        type_query(window, "ne", cx);
        assert_eq!(answer_all(&mut engine, "ready", cx), probed);
        for query in ["nee", "need"] {
            type_query(window, query, cx);
            assert_eq!(
                answer_all(&mut engine, "ready", cx),
                [methods::SEARCH_WORKSPACE_FILES],
                "{query}"
            );
        }

        window
            .update(cx, |shell, window, cx| {
                shell.close_command_palette(window, cx);
                shell.toggle_command_palette(window, cx);
            })
            .unwrap();
        type_query(window, "ne", cx);
        assert_eq!(answer_all(&mut engine, "ready", cx), probed);
    }

    #[gpui::test]
    fn a_building_index_keeps_probing(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        type_query(window, "ne", cx);
        answer_all(&mut engine, "building", cx);
        type_query(window, "nee", cx);
        assert_eq!(
            answer_all(&mut engine, "ready", cx),
            [
                methods::WARM_WORKSPACE_SEARCH,
                methods::SEARCH_WORKSPACE_FILES
            ]
        );
    }

    #[gpui::test]
    fn an_older_host_is_probed_once(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        type_query(window, "ne", cx);
        assert_eq!(
            answer_all(&mut engine, "unsupported", cx),
            [
                methods::WARM_WORKSPACE_SEARCH,
                methods::SEARCH_WORKSPACE_FILES
            ]
        );
        type_query(window, "nee", cx);
        assert_eq!(
            answer_all(&mut engine, "unsupported", cx),
            [methods::SEARCH_WORKSPACE_FILES]
        );
    }

    #[gpui::test]
    fn file_search_refreshes_partial_results_until_indexed(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        type_query(window, "needle", cx);
        let first = answer_file_index(&mut engine, "building", cx);
        engine.reply(&first, serde_json::json!([]));
        cx.run_until_parked();
        let empty = palette_section(window, Section::Files, cx).unwrap();
        assert!(empty.rows.is_empty());
        assert_eq!(empty.status.as_deref(), Some("Indexing…"));
        assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());

        cx.executor().advance_clock(FILE_INDEX_RETRY);
        let second = answer_file_index(&mut engine, "building", cx);
        assert_eq!(second["params"], first["params"]);
        engine.reply(
            &second,
            serde_json::json!([name_match("needle.rs", "file")]),
        );
        cx.run_until_parked();
        let partial = palette_section(window, Section::Files, cx).unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.status.as_deref(), Some("Indexing…"));
        let selected = partial.rows[0].key();
        window
            .update(cx, |shell, _, cx| {
                shell.select_palette_tab(Tab::Files, cx);
                shell.command_palette.as_mut().unwrap().active = Some(selected.clone());
            })
            .unwrap();

        cx.executor().advance_clock(FILE_INDEX_RETRY);
        // Even though the index is ready now, search again: the previous
        // answer was obtained while it was still building.
        let third = answer_file_index(&mut engine, "ready", cx);
        assert_eq!(third["params"], first["params"]);
        assert_eq!(palette_section(window, Section::Files, cx), Some(partial));
        engine.reply(
            &third,
            serde_json::json!([
                name_match("another-needle.rs", "file"),
                name_match("needle.rs", "file")
            ]),
        );
        cx.run_until_parked();
        let complete = palette_section(window, Section::Files, cx).unwrap();
        assert_eq!(complete.rows.len(), 2);
        assert_eq!(complete.status, None);
        window
            .read_with(cx, |shell, cx| {
                let sections = shell.palette_sections(cx);
                let rows = flat_rows(&sections);
                let palette = shell.command_palette.as_ref().unwrap();
                assert_eq!(rows[palette.active_position(&rows)].key(), selected);
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());
        assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
    }

    #[gpui::test]
    fn changing_query_cancels_file_index_retries(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        type_query(window, "needle", cx);
        let first = answer_file_index(&mut engine, "building", cx);
        engine.reply(&first, serde_json::json!([]));
        cx.run_until_parked();
        type_query(window, "updated", cx);
        cx.executor().advance_clock(FILE_INDEX_RETRY);
        let updated = answer_file_index(&mut engine, "ready", cx);
        assert_eq!(updated["params"]["query"], "updated");
        engine.reply(
            &updated,
            serde_json::json!([name_match("updated.rs", "file")]),
        );
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());
        assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
    }

    #[gpui::test]
    fn clearing_or_closing_cancels_file_index_retries(cx: &mut TestAppContext) {
        for close in [false, true] {
            let (window, _dir, mut engine) = file_search_palette(cx);
            type_query(window, "needle", cx);
            let first = answer_file_index(&mut engine, "building", cx);
            engine.reply(&first, serde_json::json!([]));
            cx.run_until_parked();
            if close {
                window
                    .update(cx, |shell, window, cx| {
                        shell.close_command_palette(window, cx);
                    })
                    .unwrap();
            } else {
                type_query(window, "", cx);
            }
            cx.executor().advance_clock(Duration::from_secs(10));
            assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());
            assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
        }
    }

    #[gpui::test]
    fn file_search_on_an_older_host_keeps_the_legacy_search(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = file_search_palette(cx);
        type_query(window, "needle", cx);
        let warm = engine.take(methods::WARM_WORKSPACE_SEARCH, cx);
        assert_eq!(warm.len(), 1);
        engine.fail(&warm[0], "unknown method: WarmWorkspaceSearch");
        let search = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        assert_eq!(search.len(), 1);
        engine.reply(
            &search[0],
            serde_json::json!([name_match("needle.rs", "file")]),
        );
        cx.run_until_parked();
        let files = palette_section(window, Section::Files, cx).unwrap();
        assert_eq!(files.rows.len(), 1);
        assert_eq!(files.status, None);
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());
        assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
    }

    #[gpui::test]
    fn file_index_retries_stop_on_warm_or_search_error(cx: &mut TestAppContext) {
        for fail_warm in [false, true] {
            let (window, _dir, mut engine) = file_search_palette(cx);
            type_query(window, "needle", cx);
            let first = answer_file_index(&mut engine, "building", cx);
            engine.reply(&first, serde_json::json!([name_match("needle.rs", "file")]));
            cx.run_until_parked();
            cx.executor().advance_clock(FILE_INDEX_RETRY);
            let request = if fail_warm {
                let warm = engine.take(methods::WARM_WORKSPACE_SEARCH, cx);
                assert_eq!(warm.len(), 1);
                warm.into_iter().next().unwrap()
            } else {
                answer_file_index(&mut engine, "building", cx)
            };
            engine.fail(&request, "search failed");
            cx.run_until_parked();
            let files = palette_section(window, Section::Files, cx).unwrap();
            assert!(files.rows.is_empty());
            assert_eq!(files.status.as_deref(), Some("Couldn\u{2019}t search. Try again."));
            cx.executor().advance_clock(Duration::from_secs(10));
            assert!(engine.take(methods::WARM_WORKSPACE_SEARCH, cx).is_empty());
            assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
        }
    }

    #[gpui::test]
    fn file_names_need_a_focused_chat_and_two_characters(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, false);
        type_query(window, "co", cx);
        assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
        assert!(palette_section(window, Section::Files, cx).is_none());

        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "c", cx);
        assert!(engine.take(methods::SEARCH_WORKSPACE_FILES, cx).is_empty());
        type_query(window, "co", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["params"]["chatId"], "focused");
        assert_eq!(requests[0]["params"]["query"], "co");
        let searching = palette_section(window, Section::Files, cx).unwrap();
        assert!(searching.rows.is_empty());
        assert_eq!(searching.status.as_deref(), Some("Searching…"));

        engine.reply(
            &requests[0],
            serde_json::json!([
                name_match("src/composer.rs", "file"),
                name_match("src", "directory")
            ]),
        );
        cx.run_until_parked();
        let files = palette_section(window, Section::Files, cx).unwrap();
        assert_eq!(files.status, None);
        assert_eq!(
            files
                .rows
                .into_iter()
                .map(|row| row.kind)
                .collect::<Vec<_>>(),
            [
                RowKind::File {
                    path: "src/composer.rs".into(),
                    is_dir: false
                },
                RowKind::File {
                    path: "src".into(),
                    is_dir: true
                },
            ]
        );
    }

    #[gpui::test]
    fn stale_file_answers_are_dropped(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "ab", cx);
        let first = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        type_query(window, "abc", cx);
        let second = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        engine.reply(
            &second[0],
            serde_json::json!([name_match("abc.rs", "file")]),
        );
        cx.run_until_parked();
        engine.reply(&first[0], serde_json::json!([name_match("ab.rs", "file")]));
        cx.run_until_parked();
        let files = palette_section(window, Section::Files, cx).unwrap();
        assert_eq!(
            files
                .rows
                .into_iter()
                .map(|row| row.kind)
                .collect::<Vec<_>>(),
            [RowKind::File {
                path: "abc.rs".into(),
                is_dir: false
            }]
        );
    }

    #[gpui::test]
    fn arriving_files_keep_the_selected_row(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "deploy", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_FILES, cx);
        // Select the thread; the Files section then lands below it.
        window
            .update(cx, |shell, _, cx| shell.move_palette_selection(false, cx))
            .unwrap();
        let selected = |cx: &mut TestAppContext| {
            window
                .read_with(cx, |shell, _| {
                    shell.command_palette.as_ref().unwrap().active.clone()
                })
                .unwrap()
        };
        let before = selected(cx);
        engine.reply(
            &requests[0],
            serde_json::json!([name_match("deploy.md", "file")]),
        );
        cx.run_until_parked();
        assert_eq!(selected(cx), before);
        assert!(before.is_some());
    }

    #[gpui::test]
    fn activating_a_file_row_opens_it_for_the_focused_chat(cx: &mut TestAppContext) {
        let (window, _dir, _engine) = engine_palette(cx, true);
        window
            .update(cx, |shell, window, cx| {
                let row = PaletteRow::new(
                    Section::Files,
                    RowKind::File {
                        path: "src/composer.rs".into(),
                        is_dir: false,
                    },
                );
                shell.activate_palette_row(row, window, cx);
                assert!(shell.command_palette.is_none());
                assert!(
                    shell
                        .file_surface_paths
                        .values()
                        .any(|path| path == "src/composer.rs")
                );
                assert_eq!(
                    shell.recent_files("focused").next().map(String::as_str),
                    Some("src/composer.rs")
                );
            })
            .unwrap();
    }

    fn content_match(path: &str, line: u64) -> serde_json::Value {
        serde_json::json!({
            "path": path, "line": line, "column": 4,
            "preview": "let needle = 1;", "ranges": [[4, 10]],
        })
    }

    fn content_answer(matches: Vec<serde_json::Value>, indexing: bool) -> serde_json::Value {
        serde_json::json!({ "matches": matches, "truncated": false, "indexing": indexing })
    }

    #[gpui::test]
    fn content_matches_arrive_after_the_debounce(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        assert_eq!(requests.len(), 1);
        let params = &requests[0]["params"];
        assert_eq!(params["chatId"], "focused");
        assert_eq!(params["query"], "needle");
        assert_eq!(params["limit"], CONTENT_TAB_LIMIT);
        assert_eq!(params["perFileLimit"], CONTENT_MATCHES_PER_FILE);

        let matches = (1..=8)
            .map(|line| content_match("src/lib.rs", line))
            .collect();
        engine.reply(&requests[0], content_answer(matches, false));
        cx.run_until_parked();
        let in_files = palette_section(window, Section::InFiles, cx).unwrap();
        assert_eq!(in_files.status, None);
        assert_eq!(in_files.rows.len(), ALL_TAB_LIMIT);
        let RowKind::Content(first) = &in_files.rows[0].kind else {
            panic!("content row expected");
        };
        assert_eq!((first.line, first.ranges.as_slice()), (1, &[(4, 10)][..]));
        window
            .update(cx, |shell, _, cx| shell.select_palette_tab(Tab::Files, cx))
            .unwrap();
        assert_eq!(
            palette_section(window, Section::InFiles, cx)
                .unwrap()
                .rows
                .len(),
            8
        );
    }

    #[gpui::test]
    fn content_search_reports_indexing_and_hides_when_empty(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&requests[0], content_answer(Vec::new(), true));
        cx.run_until_parked();
        assert_eq!(
            palette_section(window, Section::InFiles, cx)
                .unwrap()
                .status
                .as_deref(),
            Some("Indexing…")
        );

        type_query(window, "needles", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&requests[0], content_answer(Vec::new(), false));
        cx.run_until_parked();
        assert!(palette_section(window, Section::InFiles, cx).is_none());
    }

    #[gpui::test]
    fn content_search_refreshes_partial_results_until_indexed(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let first = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&first[0], content_answer(Vec::new(), true));
        cx.run_until_parked();
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );

        cx.executor().advance_clock(CONTENT_INDEX_RETRY);
        let second = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["params"], first[0]["params"]);
        engine.reply(
            &second[0],
            content_answer(vec![content_match("src/lib.rs", 2)], true),
        );
        cx.run_until_parked();
        let partial = palette_section(window, Section::InFiles, cx).unwrap();
        assert_eq!(partial.rows.len(), 1);
        assert_eq!(partial.status.as_deref(), Some("Indexing…"));
        let selected = partial.rows[0].key();
        window
            .update(cx, |shell, _, cx| {
                shell.select_palette_tab(Tab::Files, cx);
                shell.command_palette.as_mut().unwrap().active = Some(selected.clone());
            })
            .unwrap();

        cx.executor().advance_clock(CONTENT_INDEX_RETRY * 2);
        let third = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        assert_eq!(third.len(), 1);
        assert_eq!(third[0]["params"], first[0]["params"]);
        assert_eq!(palette_section(window, Section::InFiles, cx), Some(partial));
        engine.reply(
            &third[0],
            content_answer(
                vec![
                    content_match("src/lib.rs", 1),
                    content_match("src/lib.rs", 2),
                ],
                false,
            ),
        );
        cx.run_until_parked();
        let complete = palette_section(window, Section::InFiles, cx).unwrap();
        assert_eq!(complete.rows.len(), 2);
        assert_eq!(complete.status, None);
        window
            .read_with(cx, |shell, cx| {
                let sections = shell.palette_sections(cx);
                let rows = flat_rows(&sections);
                let palette = shell.command_palette.as_ref().unwrap();
                assert_eq!(rows[palette.active_position(&rows)].key(), selected);
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn content_index_retries_back_off_to_two_seconds(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let mut pending = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        let early = Duration::from_millis(100);
        for wait in [500, 1000, 2000, 2000].map(Duration::from_millis) {
            engine.reply(&pending[0], content_answer(Vec::new(), true));
            cx.run_until_parked();
            // `take` itself advances the clock by the debounce; stay short of
            // the wait, then cross it.
            cx.executor().advance_clock(wait - early);
            assert!(
                engine
                    .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                    .is_empty(),
                "refreshed before {wait:?}"
            );
            cx.executor().advance_clock(early);
            pending = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
            assert_eq!(pending.len(), 1, "no refresh after {wait:?}");
        }
        engine.reply(&pending[0], content_answer(Vec::new(), false));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn changing_the_query_cancels_content_index_retries(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let first = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&first[0], content_answer(Vec::new(), true));
        cx.run_until_parked();

        type_query(window, "updated", cx);
        cx.run_until_parked();
        cx.executor().advance_clock(CONTENT_INDEX_RETRY);
        let updated = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0]["params"]["query"], "updated");
        engine.reply(&updated[0], content_answer(Vec::new(), false));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn closing_the_palette_cancels_content_index_retries(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let first = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&first[0], content_answer(Vec::new(), true));
        cx.run_until_parked();
        window
            .update(cx, |shell, window, cx| {
                shell.close_command_palette(window, cx)
            })
            .unwrap();
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn content_index_retries_stop_on_error(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let first = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.reply(&first[0], content_answer(Vec::new(), true));
        cx.run_until_parked();
        cx.executor().advance_clock(CONTENT_INDEX_RETRY);
        let retry = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        assert_eq!(retry.len(), 1);
        engine.fail(&retry[0], "search failed");
        cx.run_until_parked();
        let section = palette_section(window, Section::InFiles, cx).unwrap();
        assert_eq!(section.status.as_deref(), Some("Couldn\u{2019}t search. Try again."));
        cx.executor().advance_clock(Duration::from_secs(10));
        assert!(
            engine
                .take(methods::SEARCH_WORKSPACE_CONTENT, cx)
                .is_empty()
        );
    }

    #[gpui::test]
    fn content_search_on_an_older_host_explains_itself(cx: &mut TestAppContext) {
        let (window, _dir, mut engine) = engine_palette(cx, true);
        type_query(window, "needle", cx);
        let requests = engine.take(methods::SEARCH_WORKSPACE_CONTENT, cx);
        engine.fail(&requests[0], "unknown method: SearchWorkspaceContent");
        cx.run_until_parked();
        let in_files = palette_section(window, Section::InFiles, cx).unwrap();
        assert!(in_files.rows.is_empty());
        assert!(
            in_files
                .status
                .is_some_and(|status| status.contains("runs an older Slopty. Update it there."))
        );
    }

    #[gpui::test]
    fn activating_a_content_row_opens_its_file(cx: &mut TestAppContext) {
        let (window, _dir, _engine) = engine_palette(cx, true);
        window
            .update(cx, |shell, window, cx| {
                let found: WorkspaceContentMatch =
                    serde_json::from_value(content_match("src/lib.rs", 42)).unwrap();
                shell.activate_palette_row(
                    PaletteRow::new(Section::InFiles, RowKind::Content(found)),
                    window,
                    cx,
                );
                assert!(
                    shell
                        .file_surface_paths
                        .values()
                        .any(|path| path == "src/lib.rs")
                );
            })
            .unwrap();
    }

    /// A shell with the real app keymap and two sidebar sessions.
    fn keyed_palette_window(
        cx: &mut TestAppContext,
    ) -> (gpui::WindowHandle<Shell>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            settings::init(settings::UiSettings::default(), dir.path(), cx);
            // slopty: no History surface (ruled out).
            crate::composer::init(cx);
            apply_keymap(
                cx,
                &KeymapConfig::default(),
            );
        });
        let (window, _) = palette_window(cx);
        window
            .update(cx, |shell, _, cx| {
                shell.state.update(cx, |state, _| {
                    state.connection = slopty_view_proto::view::ConnectionStatus::Ready;
                    state.workspace_scope = Some(WorkspaceScope::Local);
                    state.local_device_id = Some("local".into());
                    state.apply_chats(vec![
                        chat("first", None, false, 0),
                        chat("second", None, false, 1),
                    ]);
                    state.selected_chat = Some("first".into());
                });
            })
            .unwrap();
        (window, dir)
    }

    fn palette_tab(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) -> Option<Tab> {
        window
            .read_with(cx, |shell, _| shell.command_palette.as_ref().map(|p| p.tab))
            .unwrap()
    }

    fn selected_chat(window: gpui::WindowHandle<Shell>, cx: &mut TestAppContext) -> Option<String> {
        window
            .read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.clone())
            .unwrap()
    }

    #[gpui::test]
    fn digit_shortcuts_switch_tabs_while_the_palette_is_open(cx: &mut TestAppContext) {
        let (window, _dir) = keyed_palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();

        cx.simulate_keystrokes(window.into(), &platform_combo("mod-2"));
        assert_eq!(palette_tab(window, cx), Some(Tab::Threads));
        assert_eq!(selected_chat(window, cx).as_deref(), Some("first"));
        cx.simulate_keystrokes(window.into(), &platform_combo("mod-4"));
        assert_eq!(palette_tab(window, cx), Some(Tab::Files));
        cx.simulate_keystrokes(window.into(), &platform_combo("mod-1"));
        assert_eq!(palette_tab(window, cx), Some(Tab::All));
    }

    #[gpui::test]
    fn digit_shortcuts_jump_to_sessions_once_the_palette_closes(cx: &mut TestAppContext) {
        let (window, _dir) = keyed_palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx);
                shell.close_command_palette(window, cx);
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();

        cx.simulate_keystrokes(window.into(), &platform_combo("mod-2"));
        assert_eq!(palette_tab(window, cx), None);
        assert_eq!(selected_chat(window, cx).as_deref(), Some("second"));
    }

    #[gpui::test]
    fn tab_rotates_the_palette_tabs(cx: &mut TestAppContext) {
        let (window, _dir) = keyed_palette_window(cx);
        window
            .update(cx, |shell, window, cx| {
                shell.toggle_command_palette(window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();

        for expected in [Tab::Threads, Tab::Commands, Tab::Files, Tab::All] {
            cx.simulate_keystrokes(window.into(), "tab");
            assert_eq!(palette_tab(window, cx), Some(expected));
        }
        cx.simulate_keystrokes(window.into(), "shift-tab");
        assert_eq!(palette_tab(window, cx), Some(Tab::Files));
    }

    #[test]
    fn selection_follows_its_row_when_rows_arrive_above_it() {
        let chats = |ids: &[&str]| {
            PaletteSection::new(
                Section::Threads,
                ids.iter().map(|id| RowKind::Chat((*id).into())),
            )
        };
        let before = [chats(&["a", "b", "c"])];
        let active = flat_rows(&before)[2].key();
        let after = [
            PaletteSection::new(Section::Commands, [RowKind::Command(Command::NewChat)]),
            chats(&["a", "b", "c"]),
        ];
        assert_eq!(row_position(Some(&active), 2, &flat_rows(&after)), 3);
        // A vanished row falls back to its old place, clamped.
        let shorter = [chats(&["a"])];
        assert_eq!(row_position(Some(&active), 2, &flat_rows(&shorter)), 0);
        // Two headers precede the fourth row in the scroll container.
        assert_eq!(child_index(&after, 3), 5);
        assert_eq!(child_index(&after, 0), 1);
        // Keyboard scrolling brings a section's header along with its first
        // row; later rows scroll to themselves.
        assert_eq!(scroll_child_index(&after, 0), 0);
        assert_eq!(scroll_child_index(&after, 1), 2);
        assert_eq!(scroll_child_index(&after, 2), 4);
    }

    #[test]
    fn x11_unflagged_enter_repeats_activate_once_until_release() {
        let mut enter = EnterPress::default();
        // The pinned X11 backend drops synthetic repeat releases and emits
        // every repeated KeyDownEvent with is_held=false.
        assert!(enter.press(false));
        for _ in 0..35 {
            assert!(!enter.press(false));
        }
        enter.release();
        assert!(enter.press(false));
    }

    #[test]
    fn flagged_enter_repeats_do_not_activate() {
        let mut enter = EnterPress::default();
        assert!(!enter.press(true));
        assert!(!enter.press(false));
        enter.release();
        assert!(enter.press(false));
        assert!(!enter.press(true));
    }

    #[test]
    fn action_search_hides_empty_section_and_preserves_order() {
        assert_eq!(
            commands_for("", true, false, false),
            vec![
                Command::NewChat,
                Command::NewProject,
                Command::ResumeSession, // slopty
                Command::Settings,
                Command::Theme(AppearanceMode::Light)
            ]
        );
        assert_eq!(
            commands_for("new", true, false, false),
            vec![Command::NewChat, Command::NewProject]
        );
        assert_eq!(
            commands_for("settings", true, false, false),
            vec![Command::Settings]
        );
        assert_eq!(
            commands_for("theme", true, false, false),
            vec![Command::Theme(AppearanceMode::Light)]
        );
        assert!(commands_for("deployment", true, false, false).is_empty());
    }

    #[test]
    fn the_palette_holds_the_menu_bars_commands() {
        let labels = |query| {
            commands_for(query, true, false, true)
                .iter()
                .map(Command::label)
                .collect::<Vec<_>>()
        };
        assert_eq!(labels("side pane"), ["Toggle side pane", "Expand side pane"]);
        assert_eq!(labels("reopen"), ["Reopen closed tab"]);
        assert!(commands_for("side pane", true, false, false).is_empty());
    }

    #[test]
    fn theme_action_targets_the_opposite_resolved_appearance() {
        assert_eq!(
            commands_for("theme", true, false, false),
            vec![Command::Theme(AppearanceMode::Light)]
        );
        assert_eq!(
            commands_for("theme", false, false, false),
            vec![Command::Theme(AppearanceMode::Dark)]
        );
        assert_eq!(
            commands_for("light", true, false, false),
            vec![Command::Theme(AppearanceMode::Light)]
        );
        assert_eq!(
            commands_for("dark", false, false, false),
            vec![Command::Theme(AppearanceMode::Dark)]
        );
    }

    #[test]
    fn search_matches_words_across_chat_metadata() {
        assert!(matches_query(
            "mac auth",
            "Fix authentication Slopty @ MacBook main"
        ));
        assert!(matches_query("  ", "Any chat"));
        assert!(!matches_query("mac windows", "Slopty @ MacBook"));
    }
}
