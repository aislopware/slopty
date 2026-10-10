//! The workspace's actions and the palette lines that name them, with the keys that run them
//! now: the keymap's ([`crate::keymap`]), where the default chords are.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

use gpui::{Action, KeyBinding, actions};

use crate::conversation::{CycleDensity, EditLastQueued, Interrupt, QueueMessage};
use crate::keymap::Scope;
use crate::palette::PaletteItem;

actions!(
    workspace,
    [
        /// ⌘⇧T: a new shell on the focused tile's worker, in a tab of its own.
        NewTerminal,
        /// ⌘T: an agent's composer in a tab of its own, on the focused tile's machine and in
        /// its folder: the agent last started there, else the first it offers.
        StartAgent,
        /// "New agent…": which agent, on which machine, in which folder, as steps.
        NewAgent,
        /// ⌘D: a new shell in a pane of its own right of the focused one.
        SplitRight,
        /// ⌘⇧D: a new shell in a pane of its own below the focused one.
        SplitDown,
        /// ⌘⌥T: the tab's terminal, a pane below the whole tab: a shell there the first time,
        /// then put away and brought back, its shell going on.
        TabTerminal,
        /// "Close other tabs": every tab of the project on show but the one on show, and what
        /// is in them.
        CloseOtherTabs,
        /// "Other tabs…": the tabs of the project on show, as a step.
        OtherTabs,
        /// "Move to project…": the other projects, as a step; the focused tile goes to a tab of
        /// its own in the one picked.
        MoveToProject,
        /// "New project…": New agent's steps for the agent that will orchestrate it (one that
        /// runs in a terminal), then the "New project" sheet over its tile.
        NewProject,
        /// Put an empty note beside the focused column.
        NewNote,
        /// Put a worker's window or display in the workspace.
        AddWindow,
        /// Open a file on the focused tile's worker (the palette, ready for a path).
        OpenFile,
        /// Open a folder on the focused tile's worker in a tile (the palette, ready for a
        /// path: the focused shell's directory).
        OpenFolder,
        /// Open a web page in a tile (the palette, ready for an address).
        OpenUrl,
        /// Review the changes of the focused folder's repository, or of the focused shell's,
        /// with no thread: what is not committed, and the whole branch.
        ReviewChanges,
        /// Open the review of what waits first under *To review*, after the one on show.
        ReviewNext,
        /// "Review a pull request…": which repository, then which pull request by number; an
        /// agent starts on it in a new worktree that checks it out, its whole change beside it.
        ReviewPull,
        /// Free the worktree the focused work is in: a folder's, or a thread's whose agent
        /// has exited.
        RemoveWorktree,
        /// Free every agent's worktree of the focused folder's clone whose work has landed and
        /// that nothing works in.
        RemoveMerged,
        /// Run one of the repository's own run scripts, read in the folder the focus works in.
        RunHere,
        /// Bring the focused file tile's file down whole, onto this device: the save panel
        /// on the Mac, the Files export sheet on iPhone and iPad.
        SaveCopy,
        /// Close the focused tile (a shell's session goes after the undo window).
        CloseItem,
        /// Put back the tile closed last, while the offer stands.
        UndoClose,
        /// Reveal the next thing on the attention ladder: an agent that needs the human, then
        /// a finish not yet looked at, failed first.
        NextAttention,
        /// Show the navigator at what needs the person: *Needs you*, then *To review*.
        ShowNeedsYou,
        /// Show the navigator and type into its filter.
        FilterNavigator,
        /// Silence or resume the focused remote window's audio on this client.
        ToggleMute,
        /// Show or hide the stream stats overlay on every remote window.
        ToggleStats,
        /// Type this device's clipboard text into the focused remote window, key by key: for
        /// a login window or a field that refuses paste.
        TypeClipboard,
        /// Stream the focused display tile from a display the worker makes for this device,
        /// sized to the tile and following it as it resizes; again, back to the physical one.
        ToggleSizedDisplay,
        /// Show the focused remote window or display in a window of its own on this Mac, the
        /// same stream going on; again, or closing that window, puts it back in its tile.
        ToggleOwnWindow,
        /// Send the system's own shortcuts (⌘Tab, ⌘Space, Mission Control) to the focused
        /// remote Mac while its tile has the keyboard, or leave them to this Mac.
        ToggleSystemKeys,
        /// Draw the curtain over the focused desktop's Mac, hiding its screens and holding its
        /// own keyboard and pointer off while this device drives it; again, lift it.
        ToggleCurtain,
        /// Show or hide the navigator: the workers, what runs on them, and what needs you.
        ToggleNavigator,
        /// Group the navigator's tiles by the machine each runs on, or back by project.
        ToggleNavigatorLens,
        /// Move the keyboard focus to the next control, from anywhere, a terminal included.
        FocusNext,
        /// Move the keyboard focus to the previous control.
        FocusPrev,
        /// Search everything (⌘K): the tiles, the threads worked in lately and the words said in
        /// any, the projects, the machines, the files and the commands.
        OpenPalette,
        /// The command palette (⌘⇧P): the search with `>` typed, so it lists every action by
        /// name, run by ↩; ⌫ widens it to everything.
        OpenCommands,
        /// The palette as a list of the ports forwarded from the workers; ↩ opens one in a
        /// tile or in the browser.
        ListPorts,
        /// Name the focused tile: a field in its header, ↩ keeps the name (blank clears
        /// it), Esc leaves it as it was.
        RenameItem,
        /// Search the files under the focused tile's directory on its worker, the matches
        /// grouped by file as they are found and ↩ opening one in a file tile at its line; or,
        /// with the scope chip, the open tiles, ↩ going to one with its find bar open.
        SearchInFiles,
        /// Focus the pane to the left of the focused one.
        FocusLeft,
        /// Focus the pane to the right.
        FocusRight,
        /// Focus the pane above.
        FocusUp,
        /// Focus the pane below.
        FocusDown,
        /// Move the focused tile into the pane to the left, or out along the tab's left edge.
        MoveLeft,
        /// Move the focused tile into the pane to the right, or out along the right edge.
        MoveRight,
        /// Move the focused tile into the pane above, or out along the top edge.
        MoveUp,
        /// Move the focused tile into the pane below, or out along the bottom edge.
        MoveDown,
        /// Zoom the focused pane over its tab, the navigator put away, or put both back.
        ZoomPane,
        /// Make the shares of every split in the tab equal.
        EqualizePanes,
        /// Show the project before the one on show, in the navigator's order.
        PreviousProject,
        /// Show the project after it.
        NextProject,
        /// Show the project's tab before the one on show.
        PreviousTab,
        /// Show the project's tab after it.
        NextTab,
        /// Show the project's last tab.
        LastTab,
        /// Back through the tabs visited.
        GoBack,
        /// Forward through the tabs visited.
        GoForward,
        /// Show the focused pane's tab before the one it shows.
        PreviousPaneTab,
        /// Show the focused pane's tab after it.
        NextPaneTab,
        /// Terminal text one point larger.
        FontLarger,
        /// Terminal text one point smaller.
        FontSmaller,
        /// Terminal text back to the settings' size.
        FontReset,
        /// The focused agent's tile to its next face: thread, terminal, and an orchestrator's
        /// board.
        SwitchFace,
        /// The focused page's address as a field in its header, all of it selected: ↩ goes
        /// there, Esc leaves it as it was. With no page focused, "Open URL…".
        EditAddress,
        /// The focused page back one page.
        PageBack,
        /// The focused page forward one page.
        PageForward,
        /// Load the focused page again.
        ReloadPage,
        /// Web Inspector on the focused page (a debug build of the Mac app).
        InspectPage,
        /// Undo in the page that holds the keyboard.
        PageUndo,
        /// Redo in the page that holds the keyboard.
        PageRedo,
        /// Cut the selection of the page that holds the keyboard.
        PageCut,
        /// Select all in the page that holds the keyboard: its field with the caret, or the page.
        PageSelectAll,
    ]
);

/// The palette's name for [`StartAgent`].
pub const NEW_AGENT_HERE: &str = "New agent";

/// The palette's name for [`OtherTabs`], and what its step's field says.
pub const OTHER_TABS: &str = "Other tabs\u{2026}";

/// The palette's name for [`MoveToProject`].
pub const MOVE_TO_PROJECT: &str = "Move to project\u{2026}";

/// The palette's name for [`SaveCopy`].
pub const SAVE_A_COPY: &str = "Save a copy\u{2026}";

/// The palette's line for ⌘⇧M while the focused stream's sound plays.
pub const MUTE_SOUND: &str = "Mute sound";

/// The same line while it is muted.
pub const UNMUTE_SOUND: &str = "Unmute sound";

/// ⌘1…⌘8: show tab `index` (0-based) of the project on show, as a browser's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct SelectTab {
    /// 0-based.
    pub index: usize,
}

/// "Other tabs…"'s line for a tab: show it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ShowTab {
    /// Which.
    pub id: slopty_client::layout::tiling::TabId,
}

/// "Move to project…"'s line for a project: the focused tile goes to a tab of its own there.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct MoveToProjectOf {
    /// The project's home.
    pub home: slopty_client::layout::GroupKey,
}

/// Start a thread of `agent` on `worker`, in `cwd`: the last step of "New agent…", a line for
/// each folder it offers.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct StartThread {
    /// Where it runs.
    pub worker: slopty_client::layout::WorkerKey,
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
    /// In which folder, as the worker spells it (`~` its home).
    pub cwd: String,
    /// In a new worktree of its own, made from the clone the folder is in.
    pub worktree: bool,
}

/// A folder step's "Clone `origin` into `into`": `worker` clones the repository another machine
/// has from `url`, the step saying how far it is, and `agent` then starts in the clone.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct CloneToStart {
    /// Where it is cloned, and the agent runs.
    pub worker: slopty_client::layout::WorkerKey,
    /// Which agent starts in it.
    pub agent: slopty_proto::thread::AgentId,
    /// The repository, as its origin names it (`github.com/aislopware/slopty`).
    pub origin: String,
    /// Where to clone it from, with no credentials in it.
    pub url: String,
    /// Where the clone goes on `worker`: absolute, or `~/…`.
    pub into: String,
    /// The agent orchestrates a new project ("New project…"), rather than working on its own.
    pub project: bool,
}

/// A run script picked in Run's step: it opens on `worker` in a terminal of its own.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct RunScriptOn {
    /// The machine it runs on.
    pub worker: slopty_client::layout::WorkerKey,
    /// The script, as the machine read it.
    pub script: slopty_proto::git::RunScript,
}

/// A line of "Review a pull request…": the agent starts on pull request `number` of
/// `repo` on `worker`, in a new worktree checking it out.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ReviewPullNumber {
    /// On which machine.
    pub worker: slopty_client::layout::WorkerKey,
    /// The repository, as the worker spells it.
    pub repo: String,
    /// The pull request's number.
    pub number: u32,
    /// The forge the number was typed for, where the text said which (`!12`, a page).
    pub forge: Option<slopty_proto::git::Forge>,
}

/// "Remove `<machine>`…": its confirm, then the worker and everything else of Slopty's off it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct RemoveMachine {
    /// The machine.
    pub worker: slopty_client::layout::WorkerKey,
}

/// "Resume a past session…", the last line of "New agent…"'s folder step: `agent`'s past
/// sessions on `worker` are listed next, the last prompted first.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ResumePastSession {
    /// On which machine.
    pub worker: slopty_client::layout::WorkerKey,
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
}

/// A past session picked: its thread's tile, the agent taken up again on it.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ResumeSession {
    /// On which machine.
    pub worker: slopty_client::layout::WorkerKey,
    /// The session, as its agent's record lists it.
    pub session: Box<slopty_proto::thread::wire::PastSession>,
}

/// An agent picked in "New agent…": the machine to start it on is asked next, then the
/// folder.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct NewAgentOf {
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
}

/// A machine picked in "New agent…": the folder to start `agent` in on `worker` is asked next.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct NewAgentOn {
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
    /// On which machine.
    pub worker: slopty_client::layout::WorkerKey,
}

/// An agent picked in "New project…": the machine to start its orchestrator on is asked next.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct NewProjectOf {
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
}

/// A machine picked in "New project…": the folder its orchestrator starts in is asked next.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct NewProjectOn {
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
    /// On which machine.
    pub worker: slopty_client::layout::WorkerKey,
}

/// The last step of "New project…": `agent` starts on `worker` in `cwd` at once, with no first
/// message, and the "New project" sheet opens over its terminal's tile once it has one.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct StartOrchestrator {
    /// Where it runs.
    pub worker: slopty_client::layout::WorkerKey,
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
    /// In which folder, as the worker spells it (`~` its home).
    pub cwd: String,
    /// In a new worktree of its own, made from the clone the folder is in.
    pub worktree: bool,
}

/// "Group the navigator by …": the fact keys it groups the tiles by, the first a tile has
/// winning ([`slopty_client::groups`]). The palette offers a line for each fact a tile in the
/// layout has.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct GroupNavigatorBy {
    /// The chain, `["agent", "machine"]` for "by agent".
    pub chain: Vec<String>,
}

/// "Scope to `project`": narrow the navigator, the attention sections, the inbox and the
/// bell's counts to one project; `None` lets go of the scope.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ScopeTo {
    /// The project's group.
    pub project: Option<slopty_client::layout::GroupKey>,
}

/// "Stop sharing the clipboard with `worker`", or share it again: kept in the settings by
/// the machine's name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct ShareClipboard {
    /// The machine.
    pub worker: slopty_client::layout::WorkerKey,
    /// Share it, or stop.
    pub share: bool,
}

/// "Edit `worker`'s settings": that machine's `settings.toml`, where its greeting said it is,
/// in a file tile; the machine applies what is saved as it reads it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct EditMachineSettings {
    /// The machine.
    pub worker: slopty_client::layout::WorkerKey,
}

/// "Make this agent `project`'s orchestrator": the focused terminal's agent becomes the one
/// the project's board talks to, in place of the one it had.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct MakeOrchestrator {
    /// The project.
    pub project: slopty_proto::project::ProjectId,
}

/// "Add to `project`": pin the focused tile to a project, so every client groups it there
/// whatever else it is; `None` takes the pin back.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct PinToProject {
    /// The project's group.
    pub project: Option<slopty_client::layout::GroupKey>,
}

/// A tile's window of its own ([`super::popout`]): its picture takes every chord but the one
/// that puts it back; the keymap binds that one there.
pub(crate) const POP_OUT_CTX: &str = super::popout::CTX;

/// What the workspace's key context holds while a closed tile's notice is up: ⌘Z takes it back
/// then, and only then, so the chord is free for whatever else wants it the rest of the time.
pub(crate) const CLOSING_CTX: &str = "ClosingOffered";

/// The workspace's key bindings in effect: every scope of the keymap but the terminal's and the
/// app's ([`crate::keymap`], where the table is).
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    crate::keymap::current().bindings(|scope| !matches!(scope, Scope::Terminal | Scope::App))
}

/// The palette's lines for the workspace's and the terminal's actions, with their keys.
#[must_use]
pub fn palette_items() -> Vec<PaletteItem> {
    use crate::terminal::{
        ClearScreen, CopyBlockOutput, CopyLastOutput, CopyMode, Find, NextPrompt, PrevPrompt,
        RerunLast,
    };
    let workspace = key_bindings();
    let terminal = crate::terminal::key_bindings();
    let w = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &workspace);
    let t = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &terminal);
    let mut items = vec![
        w("New terminal", Box::new(NewTerminal)),
        w(NEW_AGENT_HERE, Box::new(StartAgent)),
        w("New agent\u{2026}", Box::new(NewAgent)),
        w("Split right with a terminal", Box::new(SplitRight)),
        w("Split down with a terminal", Box::new(SplitDown)),
        w("Show or hide the tab's terminal", Box::new(TabTerminal)),
        w(super::agent_start::NEW_PROJECT, Box::new(NewProject)),
        w("New note", Box::new(NewNote)),
        w("Add a window or display", Box::new(AddWindow)),
        w("Open file…", Box::new(OpenFile)),
        w("Open folder…", Box::new(OpenFolder)),
        w(super::reviews::REVIEW_CHANGES, Box::new(ReviewChanges)),
        w(super::reviews::REVIEW_NEXT, Box::new(ReviewNext)),
        w(super::pull_review::REVIEW_PULL, Box::new(ReviewPull)),
        w(super::worktrees::REMOVE_WORKTREE, Box::new(RemoveWorktree)),
        w(super::worktrees::REMOVE_MERGED, Box::new(RemoveMerged)),
        w(super::run_scripts::RUN, Box::new(RunHere)),
        w("Enclosing folder", Box::new(crate::folder::OpenParent)),
        w("Save file", Box::new(crate::file::SaveFile)),
        w("Done with this file", Box::new(crate::file::FinishEdit)),
        w(SAVE_A_COPY, Box::new(SaveCopy)),
        w("Open URL…", Box::new(OpenUrl)),
        w("Edit page address", Box::new(EditAddress)),
        w("Page back", Box::new(PageBack)),
        w("Page forward", Box::new(PageForward)),
        w("Reload page", Box::new(ReloadPage)),
        w("Close tile", Box::new(CloseItem)),
        w("Undo close", Box::new(UndoClose)),
        w("Next thing that needs you", Box::new(NextAttention)),
        w("Show what needs you", Box::new(ShowNeedsYou)),
        w("Filter the navigator", Box::new(FilterNavigator)),
        w(MUTE_SOUND, Box::new(ToggleMute)),
        w("Stream stats", Box::new(ToggleStats)),
        w(crate::screen::TRACKPAD_MODE, Box::new(crate::screen::ToggleTrackpad)),
        w(crate::screen::REMOTE_GESTURES, Box::new(crate::screen::ToggleRemoteGestures)),
        w("Show or hide the navigator", Box::new(ToggleNavigator)),
        w("Name this tile", Box::new(RenameItem)),
        w(super::project_search::SEARCH_IN_FILES, Box::new(SearchInFiles)),
        w("Forwarded ports", Box::new(ListPorts)),
        w("Pane to the left", Box::new(FocusLeft)),
        w("Pane to the right", Box::new(FocusRight)),
        w("Pane above", Box::new(FocusUp)),
        w("Pane below", Box::new(FocusDown)),
        w("Move tile left", Box::new(MoveLeft)),
        w("Move tile right", Box::new(MoveRight)),
        w("Move tile up", Box::new(MoveUp)),
        w("Move tile down", Box::new(MoveDown)),
        w("Zoom pane", Box::new(ZoomPane)),
        w("Equalize panes", Box::new(EqualizePanes)),
        w("Previous project", Box::new(PreviousProject)),
        w("Next project", Box::new(NextProject)),
        w("Previous tab", Box::new(PreviousTab)),
        w("Next tab", Box::new(NextTab)),
        w("First tab", Box::new(SelectTab { index: 0 })),
        w("Last tab", Box::new(LastTab)),
        w("Back", Box::new(GoBack)),
        w("Forward", Box::new(GoForward)),
        w("Previous tab in the pane", Box::new(PreviousPaneTab)),
        w(OTHER_TABS, Box::new(OtherTabs)),
        w("Close other tabs", Box::new(CloseOtherTabs)),
        w(MOVE_TO_PROJECT, Box::new(MoveToProject)),
        w("Next tab in the pane", Box::new(NextPaneTab)),
        w("Switch thread, terminal or board", Box::new(SwitchFace)),
        w("Thread density", Box::new(CycleDensity)),
        w("Stop the agent", Box::new(Interrupt)),
        w("Queue message", Box::new(QueueMessage)),
        w("Edit the last queued message", Box::new(EditLastQueued)),
        w("Larger text", Box::new(FontLarger)),
        w("Smaller text", Box::new(FontSmaller)),
        w("Default text size", Box::new(FontReset)),
        t("Find in terminal, file or thread", Box::new(Find)),
        t("Previous prompt", Box::new(PrevPrompt)),
        t("Next prompt", Box::new(NextPrompt)),
        t("Copy last output", Box::new(CopyLastOutput)),
        t("Copy block output", Box::new(CopyBlockOutput)),
        t("Copy mode", Box::new(CopyMode)),
        t("Rerun last command", Box::new(RerunLast)),
        t("Clear the screen and history", Box::new(ClearScreen)),
    ];
    items.extend(crate::folder::files_palette_items(crate::folder::FILES_PICKER, &workspace));
    items.extend(crate::folder::folder_palette_items(&workspace));
    items.extend(crate::conversation::palette_items(&workspace));
    items.extend(crate::project::palette_items(&workspace));
    items.extend(crate::review::palette_items(&workspace));
    items.extend(crate::file::editor_palette_items(&workspace));
    // Only the Mac has a Web Inspector window of its own; iOS reaches it from Safari on a Mac.
    if cfg!(target_os = "macos") {
        items.push(w("Inspect page", Box::new(InspectPage)));
    }
    items
}

impl super::WorkspaceView {
    /// "Attach block to agent", and "Attach selection to agent" while text is selected, when the
    /// focused terminal has an agent to go to ([`super::WorkspaceView::block_target`]): the
    /// palette offers no line that would do nothing.
    pub(super) fn attach_lines(&self, cx: &gpui::App) -> Vec<PaletteItem> {
        let Some(session) = self.focused_session() else { return Vec::new() };
        if self.block_target(session).is_none() {
            return Vec::new();
        }
        let terminal = crate::terminal::key_bindings();
        let block = Box::new(crate::terminal::AttachBlock);
        let mut lines = vec![PaletteItem::new("Attach block to agent", block, &terminal)];
        if self.terminals.get(&session).is_some_and(|t| t.read(cx).has_selection()) {
            let selection = Box::new(crate::terminal::AttachSelection);
            lines.push(PaletteItem::new("Attach selection to agent", selection, &terminal));
        }
        lines
    }
}

/// Which of the workspace's focus-bound actions apply now, read once a frame. The workspace
/// listens for one only while it applies, so the dispatch tree says what the focus can do: the
/// palette offers only those ([`super::WorkspaceView::offered_lines`]), and the menu bar greys
/// the rest, as macOS greys an item nothing answers.
#[derive(Clone, Copy, Debug, Default)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per kind of focus, read together")]
pub(super) struct Applies {
    /// A tile has the focus.
    pub tile: bool,
    /// A page.
    pub page: bool,
    /// A remote window or display that streams.
    pub screen: bool,
    /// A display.
    pub display: bool,
    /// A file.
    pub file: bool,
    /// A terminal.
    pub terminal: bool,
    /// A terminal an agent runs in.
    pub agent: bool,
    /// A tile that takes files from this device: a shell, a folder or a remote picture.
    pub upload: bool,
    /// A remote tile with a window of its own on this Mac, or one that can have one.
    pub own_window: bool,
    /// Any tile streams a remote picture.
    pub streams: bool,
    /// A tile closed a moment ago can be taken back.
    pub undo: bool,
    /// A folder, or a shell in a repository: its changes can be reviewed.
    pub changes: bool,
    /// Work in an agent's worktree, which can be removed.
    pub worktree: bool,
    /// Work in a folder, whose repository's run scripts can be run.
    pub run: bool,
    /// The project on show has more than one tab.
    pub tabs: bool,
    /// The focused tile has another project to go to.
    pub projects: bool,
}

impl super::WorkspaceView {
    /// What applies to the focus now ([`Applies`]).
    pub(super) fn applies(&self) -> Applies {
        use slopty_proto::items::ItemKind;
        let focused = self.focused();
        let kind = focused.and_then(|t| self.item(t)).map(|i| &i.kind);
        let session = self.focused_session();
        let agent = session.and_then(|s| self.agent_state(s));
        Applies {
            tile: focused.is_some(),
            page: self.focused_page().is_some(),
            screen: self.active_screen().is_some(),
            display: matches!(kind, Some(ItemKind::Display { .. })),
            file: matches!(kind, Some(ItemKind::File { .. })),
            terminal: self.active_terminal().is_some(),
            agent: agent.is_some(),
            upload: matches!(
                kind,
                Some(
                    ItemKind::Terminal { .. }
                        | ItemKind::Folder { .. }
                        | ItemKind::Thread { .. }
                        | ItemKind::Window { .. }
                        | ItemKind::Display { .. }
                )
            ),
            own_window: focused
                .is_some_and(|t| self.popouts.holds(t.item) || self.can_pop_out(t.item)),
            streams: !self.screens.is_empty(),
            undo: !self.closed.is_empty(),
            changes: self.changes_here().is_some(),
            worktree: self.worktree_here().is_some(),
            run: self.run_here().is_some(),
            tabs: self.layout.shown_project().is_some_and(|p| p.tabs().len() > 1),
            projects: focused.is_some() && self.layout.projects().len() > 1,
        }
    }
}
