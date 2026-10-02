//! The workspace's actions and the palette lines that name them, with the keys that run them
//! now: the keymap's ([`crate::keymap`]), where the default chords are.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

use gpui::{Action, KeyBinding, actions};

use crate::conversation::{CycleDensity, Interrupt};
use crate::icons::IconName;
use crate::keymap::Scope;
use crate::palette::PaletteItem;

actions!(
    workspace,
    [
        /// Open a new shell on the focused tile's worker.
        NewTerminal,
        /// Open a new Claude Code agent on the focused tile's worker.
        NewAgent,
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
        /// Open the page a shell last asked to open that was held back in a notice.
        OpenLastOffer,
        /// Let go of the unsaved edits kept on this device for over a week without a tile to
        /// take them (their worker has not come back).
        DiscardOldUnsaved,
        /// The About panel: the mark, the version and the build.
        About,
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
        /// Open the inbox, or close it.
        ToggleInbox,
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
        /// Show or hide the navigator: the workers, what runs on them, and what needs you.
        ToggleNavigator,
        /// Group the navigator's tiles by repository across the workers, or back by worker.
        ToggleNavigatorLens,
        /// Move the keyboard focus to the next control, from anywhere, a terminal included.
        FocusNext,
        /// Move the keyboard focus to the previous control.
        FocusPrev,
        /// Open the command palette: every action by name, run by ↩.
        OpenPalette,
        /// The palette as a list of every worker and whether it is reachable; ↩ goes to one.
        ListWorkers,
        /// The palette as a list of the ports forwarded from the workers; ↩ opens one in a
        /// tile or in the browser.
        ListPorts,
        /// Name the focused tile: a field in its header, ↩ keeps the name (blank clears
        /// it), Esc leaves it as it was.
        RenameItem,
        /// Point the other clients at the focused tile: each of them is offered a jump to it.
        PointOthers,
        /// Find text in every tile: the palette lists the tiles it is in with their hit
        /// counts, and ↩ opens that tile's find bar on it.
        FindEverywhere,
        /// Search the files under the focused tile's directory on its worker: the matches
        /// grouped by file as they are found, ↩ opening one in a file tile at its line.
        SearchInFiles,
        /// Focus the column to the left.
        FocusColumnLeft,
        /// Focus the column to the right.
        FocusColumnRight,
        /// Focus the first column.
        FocusColumnFirst,
        /// Focus the last column.
        FocusColumnLast,
        /// Focus the workspace above.
        FocusWorkspaceUp,
        /// Focus the workspace below.
        FocusWorkspaceDown,
        /// Go back to the workspace focused before this one.
        FocusWorkspacePrevious,
        /// Focus the tile above, or the workspace above from the top tile.
        FocusUp,
        /// Focus the tile below, or the workspace below from the bottom tile.
        FocusDown,
        /// Swap the focused column with the one to its left.
        MoveColumnLeft,
        /// Swap the focused column with the one to its right.
        MoveColumnRight,
        /// Move the focused column to the start of the strip.
        MoveColumnToFirst,
        /// Move the focused column to the end of the strip.
        MoveColumnToLast,
        /// Carry the focused column to the workspace above, and follow it.
        MoveColumnToWorkspaceUp,
        /// Carry the focused column to the workspace below, and follow it.
        MoveColumnToWorkspaceDown,
        /// Swap the focused workspace with the one above.
        MoveWorkspaceUp,
        /// Swap the focused workspace with the one below.
        MoveWorkspaceDown,
        /// Move the focused tile up its column, or to the workspace above from the top.
        MoveUp,
        /// Move the focused tile down its column, or to the workspace below from the bottom.
        MoveDown,
        /// Put the focused tile into the column on its left, or out into a column of its own.
        ConsumeOrExpelLeft,
        /// Put the focused tile into the column on its right, or out into a column of its own.
        ConsumeOrExpelRight,
        /// Give the focused column the next preset width.
        CycleWidth,
        /// Give the focused column the previous preset width.
        CycleWidthBack,
        /// Narrow the focused column by a tenth of the workspace.
        NarrowColumn,
        /// Widen the focused column by a tenth of the workspace.
        WidenColumn,
        /// Toggle the focused column between its width and the whole workspace.
        MaximizeColumn,
        /// Toggle the focused tile filling the whole view, without gaps or chrome around it.
        FullscreenTile,
        /// Scroll the strip so the focused column is in the middle.
        CenterColumn,
        /// Scroll the strip so the fully visible columns sit in the middle as a group.
        CenterVisibleColumns,
        /// Widen the focused column over the room the fully visible columns leave free.
        ExpandColumn,
        /// Toggle the focused column between stacked tiles and tabs.
        ToggleTabbed,
        /// Toggle the overview: every workspace at half size.
        ToggleOverview,
        /// Terminal text one point larger.
        FontLarger,
        /// Terminal text one point smaller.
        FontSmaller,
        /// Terminal text back to the settings' size.
        FontReset,
        /// The focused agent terminal between its TUI and its conversation.
        ToggleConversation,
        /// The focused orchestrator's tile between its terminal and its project's board; from
        /// an agent on a task, its project's board.
        ToggleProjectBoard,
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

/// The palette's name for [`SaveCopy`].
pub const SAVE_A_COPY: &str = "Save a copy\u{2026}";

/// ⌘1…⌘9: focus column `index` (0-based) of the active workspace, as a browser's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct FocusColumn {
    /// 0-based.
    pub index: usize,
}

/// "New `agent` thread": start a thread of `agent` on `worker`, in `cwd`. The palette offers a
/// line for each agent the worker can start, as the worker's facts list them.
#[derive(Clone, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct StartThread {
    /// Where it runs.
    pub worker: slopty_client::layout::WorkerKey,
    /// Which agent.
    pub agent: slopty_proto::thread::AgentId,
    /// In which folder, as the worker spells it (`~` its home).
    pub cwd: String,
}

/// ⌘⌥1…⌘⌥9: focus workspace `index` (0-based; past the last, the trailing empty one).
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct FocusWorkspace {
    /// 0-based.
    pub index: usize,
}

/// ⌃⌘⌥1…⌃⌘⌥9: carry the focused column to workspace `index` (0-based; past the last, the
/// trailing empty one), and follow it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct MoveColumnToWorkspace {
    /// 0-based.
    pub index: usize,
}

/// A tile's window of its own ([`super::popout`]): its picture takes every chord but the one
/// that puts it back; the keymap binds that one there.
pub(crate) const POP_OUT_CTX: &str = super::popout::CTX;

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
        ClearScreen, CopyBlockOutput, CopyLastOutput, Find, NextPrompt, NoteLastBlock, PrevPrompt,
        RerunLast,
    };
    let workspace = key_bindings();
    let terminal = crate::terminal::key_bindings();
    let w = |label: &str, icon: IconName, action: Box<dyn Action>| {
        PaletteItem::new(label, icon, action, &workspace)
    };
    let t = |label: &str, icon: IconName, action: Box<dyn Action>| {
        PaletteItem::new(label, icon, action, &terminal)
    };
    let mut items = vec![
        w("New terminal", IconName::SquareTerminal, Box::new(NewTerminal)),
        w("New agent", IconName::Sparkles, Box::new(NewAgent)),
        w("New note", IconName::StickyNote, Box::new(NewNote)),
        w("Add a window or display", IconName::AppWindow, Box::new(AddWindow)),
        w("Open file…", IconName::FileText, Box::new(OpenFile)),
        w("Open folder…", IconName::FolderOpen, Box::new(OpenFolder)),
        w("Enclosing folder", IconName::ArrowUp, Box::new(crate::folder::OpenParent)),
        w("Save file", IconName::Save, Box::new(crate::file::SaveFile)),
        w("Done with this file", IconName::Check, Box::new(crate::file::FinishEdit)),
        w(SAVE_A_COPY, IconName::Download, Box::new(SaveCopy)),
        w("Open URL…", IconName::Globe, Box::new(OpenUrl)),
        w("Open last offered page", IconName::ExternalLink, Box::new(OpenLastOffer)),
        w("Discard unsaved edits over a week old", IconName::Eraser, Box::new(DiscardOldUnsaved)),
        w("About Slopty", IconName::Info, Box::new(About)),
        w("Edit page address", IconName::Link, Box::new(EditAddress)),
        w("Page back", IconName::ArrowLeft, Box::new(PageBack)),
        w("Page forward", IconName::ArrowRight, Box::new(PageForward)),
        w("Reload page", IconName::RotateCw, Box::new(ReloadPage)),
        w("Close tile", IconName::X, Box::new(CloseItem)),
        w("Undo close", IconName::Undo2, Box::new(UndoClose)),
        w("Next thing that needs you", IconName::BellRing, Box::new(NextAttention)),
        w("Inbox", IconName::Bell, Box::new(ToggleInbox)),
        w("Filter the navigator", IconName::ListFilter, Box::new(FilterNavigator)),
        w("Mute sound", IconName::VolumeX, Box::new(ToggleMute)),
        w("Stream stats", IconName::Activity, Box::new(ToggleStats)),
        w(
            crate::screen::TRACKPAD_MODE,
            IconName::MousePointer2,
            Box::new(crate::screen::ToggleTrackpad),
        ),
        w(
            crate::screen::REMOTE_GESTURES,
            IconName::Hand,
            Box::new(crate::screen::ToggleRemoteGestures),
        ),
        w("Show or hide the navigator", IconName::PanelLeft, Box::new(ToggleNavigator)),
        w("Name this tile", IconName::Pencil, Box::new(RenameItem)),
        w("Point other devices at this tile", IconName::Cast, Box::new(PointOthers)),
        w("Find in every tile", IconName::Search, Box::new(FindEverywhere)),
        w(super::project_search::SEARCH_IN_FILES, IconName::FolderSearch, Box::new(SearchInFiles)),
        w("List workers", IconName::Server, Box::new(ListWorkers)),
        w("Forwarded ports", IconName::Cable, Box::new(ListPorts)),
        w("Column to the left", IconName::ArrowLeft, Box::new(FocusColumnLeft)),
        w("Column to the right", IconName::ArrowRight, Box::new(FocusColumnRight)),
        w("First column", IconName::ArrowLeftToLine, Box::new(FocusColumn { index: 0 })),
        w("Last column", IconName::ArrowRightToLine, Box::new(FocusColumnLast)),
        w("Tile or workspace above", IconName::ArrowUp, Box::new(FocusUp)),
        w("Tile or workspace below", IconName::ArrowDown, Box::new(FocusDown)),
        w("Workspace above", IconName::ChevronUp, Box::new(FocusWorkspaceUp)),
        w("Workspace below", IconName::ChevronDown, Box::new(FocusWorkspaceDown)),
        w("Previous workspace", IconName::ArrowUpDown, Box::new(FocusWorkspacePrevious)),
        w("First workspace", IconName::LayoutDashboard, Box::new(FocusWorkspace { index: 0 })),
        w("Move column left", IconName::MoveLeft, Box::new(MoveColumnLeft)),
        w("Move column right", IconName::MoveRight, Box::new(MoveColumnRight)),
        w("Move column to the start", IconName::ArrowLeftToLine, Box::new(MoveColumnToFirst)),
        w("Move column to the end", IconName::ArrowRightToLine, Box::new(MoveColumnToLast)),
        w(
            "Move column to the workspace above",
            IconName::MoveUp,
            Box::new(MoveColumnToWorkspaceUp),
        ),
        w(
            "Move column to the workspace below",
            IconName::MoveDown,
            Box::new(MoveColumnToWorkspaceDown),
        ),
        w(
            "Move column to the first workspace",
            IconName::MoveUp,
            Box::new(MoveColumnToWorkspace { index: 0 }),
        ),
        w("Move workspace up", IconName::MoveVertical, Box::new(MoveWorkspaceUp)),
        w("Move workspace down", IconName::MoveVertical, Box::new(MoveWorkspaceDown)),
        w("Move tile up", IconName::MoveUp, Box::new(MoveUp)),
        w("Move tile down", IconName::MoveDown, Box::new(MoveDown)),
        w(
            "Into the column on the left",
            IconName::BetweenHorizontalStart,
            Box::new(ConsumeOrExpelLeft),
        ),
        w(
            "Into the column on the right",
            IconName::BetweenHorizontalEnd,
            Box::new(ConsumeOrExpelRight),
        ),
        w("Next column width", IconName::ChevronsRight, Box::new(CycleWidth)),
        w("Previous column width", IconName::ChevronsLeft, Box::new(CycleWidthBack)),
        w("Narrower column", IconName::FoldHorizontal, Box::new(NarrowColumn)),
        w("Wider column", IconName::UnfoldHorizontal, Box::new(WidenColumn)),
        w("Maximize column", IconName::Maximize2, Box::new(MaximizeColumn)),
        w("Fullscreen tile", IconName::Expand, Box::new(FullscreenTile)),
        w("Center column", IconName::AlignCenterHorizontal, Box::new(CenterColumn)),
        w(
            "Center the visible columns",
            IconName::AlignCenterHorizontal,
            Box::new(CenterVisibleColumns),
        ),
        w("Fill the free width", IconName::UnfoldHorizontal, Box::new(ExpandColumn)),
        w("Tabbed column", IconName::PanelsTopLeft, Box::new(ToggleTabbed)),
        w("Overview", IconName::LayoutGrid, Box::new(ToggleOverview)),
        w("Show conversation or terminal", IconName::MessageSquare, Box::new(ToggleConversation)),
        w("Show project board or terminal", IconName::Workflow, Box::new(ToggleProjectBoard)),
        w("Project tree", IconName::ListTree, Box::new(crate::project::ShowTree)),
        w("Project board", IconName::Kanban, Box::new(crate::project::ShowBoard)),
        w("Project timeline", IconName::Clock, Box::new(crate::project::ShowTimeline)),
        w("Conversation density", IconName::ListChecks, Box::new(CycleDensity)),
        w("Stop the agent", IconName::Square, Box::new(Interrupt)),
        w("Larger text", IconName::AArrowUp, Box::new(FontLarger)),
        w("Smaller text", IconName::AArrowDown, Box::new(FontSmaller)),
        w("Default text size", IconName::Type, Box::new(FontReset)),
        t("Find in terminal, file or conversation", IconName::Search, Box::new(Find)),
        t("Previous prompt", IconName::ChevronUp, Box::new(PrevPrompt)),
        t("Next prompt", IconName::ChevronDown, Box::new(NextPrompt)),
        t("Copy last output", IconName::Copy, Box::new(CopyLastOutput)),
        t("Copy block output", IconName::Copy, Box::new(CopyBlockOutput)),
        t("Rerun last command", IconName::RotateCw, Box::new(RerunLast)),
        t("Keep last block as a note", IconName::NotebookPen, Box::new(NoteLastBlock)),
        t("Clear the screen and history", IconName::Eraser, Box::new(ClearScreen)),
    ];
    items.extend(crate::folder::files_palette_items(crate::folder::FILES_PICKER, &workspace));
    items.extend(crate::project::palette_items(&workspace));
    items.extend(crate::file::pages_palette_items(&workspace));
    items.extend(crate::file::editor_palette_items(&workspace));
    // Only the Mac has a Web Inspector window of its own; iOS reaches it from Safari on a Mac.
    if cfg!(target_os = "macos") {
        items.push(w("Inspect page", IconName::Wrench, Box::new(InspectPage)));
    }
    items
}

impl super::WorkspaceView {
    /// "Attach block to agent", while the focused terminal's blocks have an agent to go to
    /// ([`super::WorkspaceView::block_target`]): the palette offers no line that would do
    /// nothing.
    pub(super) fn attach_line(&self) -> Option<PaletteItem> {
        self.block_target(self.focused_session()?)?;
        let terminal = crate::terminal::key_bindings();
        let attach = Box::new(crate::terminal::AttachBlock);
        Some(PaletteItem::new("Attach block to agent", IconName::Paperclip, attach, &terminal))
    }
}
