//! The workspace's actions, the keys that run them and the palette lines that name them.
//!
//! ⌘ is the app's modifier. A ⌃ or ⌥ chord without ⌘ belongs to the terminal (⌥ is Meta), so
//! every layout chord carries ⌘. The table is `docs/decisions/workspace.md`'s.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

use gpui::{Action, KeyBinding, actions};

use crate::conversation::{CycleDensity, Interrupt};
use crate::icons::IconName;
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
        /// Bring the focused file tile's file down whole, onto this device: the save panel
        /// on the Mac, the Files export sheet on iPhone and iPad.
        SaveCopy,
        /// Close the focused tile (a shell's session goes after the undo window).
        CloseItem,
        /// Put back the tile closed last, while the offer stands.
        UndoClose,
        /// Reveal the next terminal whose agent is waiting on the human.
        NextAttention,
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

/// The key context the workspace binds in. A focused remote window gets every chord (its
/// editor's ⌘W, ⌘T are not ours to take, as on Parsec); ⌃Tab alone stays, the keyboard's way
/// back out of it.
const CTX: Option<&str> = Some("Workspace && !Screen");
const RING_CTX: Option<&str> = Some("Workspace");
/// Inside a file tile's editor, gpui-kit's input binds some of the workspace's chords to
/// editing (⌘⌥↑ adds a caret, ⌘F opens its own search); bound here after it, ours win there.
const FILE_INPUT: Option<&str> = Some("FileEditor > Input");
/// A conversation face, and its composer.
const FACE: Option<&str> = Some(crate::conversation::CTX);
const FACE_INPUT: Option<&str> = Some("Conversation > Input");
/// A folder tile with the keyboard.
const FOLDER: Option<&str> = Some(crate::folder::CTX);
/// Search in files, its fields holding the keyboard.
const SEARCH: Option<&str> = Some(crate::search::CTX);
/// A tile's window of its own ([`super::popout`]): its picture takes every chord but the one
/// that puts it back.
const POP_OUT: Option<&str> = Some(super::popout::CTX);
/// A focused page that does not hold the keyboard: a page that does keeps ⌘← and ⌘→ for its
/// own fields.
const PAGE: Option<&str> = Some("Workspace && Page && !Screen");

/// Key bindings for the workspace context.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    let mut out = vec![
        KeyBinding::new("cmd-t", NewTerminal, CTX),
        KeyBinding::new("cmd-n", NewTerminal, CTX),
        KeyBinding::new("cmd-shift-t", NewAgent, CTX),
        KeyBinding::new("cmd-shift-n", NewNote, CTX),
        KeyBinding::new("cmd-o", AddWindow, CTX),
        KeyBinding::new("cmd-w", CloseItem, CTX),
        KeyBinding::new("cmd-z", UndoClose, CTX),
        KeyBinding::new("cmd-shift-a", NextAttention, CTX),
        KeyBinding::new("cmd-shift-m", ToggleMute, CTX),
        KeyBinding::new("cmd-shift-i", ToggleStats, CTX),
        KeyBinding::new("ctrl-cmd-n", ToggleOwnWindow, CTX),
        KeyBinding::new("ctrl-cmd-n", ToggleOwnWindow, POP_OUT),
        KeyBinding::new("cmd-b", ToggleNavigator, CTX),
        KeyBinding::new("cmd-f", crate::terminal::Find, CTX),
        // Tab is the shell's; ⌃Tab enters the control ring from a terminal, then Tab walks it.
        KeyBinding::new("ctrl-tab", FocusNext, RING_CTX),
        KeyBinding::new("ctrl-shift-tab", FocusPrev, RING_CTX),
        KeyBinding::new("cmd-shift-p", OpenPalette, CTX),
        KeyBinding::new("cmd-e", RenameItem, CTX),
        KeyBinding::new("cmd-shift-o", PointOthers, CTX),
        KeyBinding::new("cmd-shift-f", FindEverywhere, CTX),
        // A focused field (a find bar, the palette, a note) is a gpui-kit input, whose own
        // ⌘⇧F is replace; ours is bound in its context after it, so it wins there too.
        KeyBinding::new("cmd-shift-f", FindEverywhere, Some("Input")),
        KeyBinding::new("cmd-alt-f", SearchInFiles, CTX),
        KeyBinding::new("cmd-alt-f", SearchInFiles, FILE_INPUT),
        // The search surface's toggles, VS Code's keys.
        KeyBinding::new("cmd-alt-c", crate::search::ToggleMatchCase, SEARCH),
        KeyBinding::new("cmd-alt-w", crate::search::ToggleWholeWord, SEARCH),
        KeyBinding::new("cmd-alt-r", crate::search::ToggleRegex, SEARCH),
        KeyBinding::new("cmd-alt-left", FocusColumnLeft, CTX),
        KeyBinding::new("cmd-alt-right", FocusColumnRight, CTX),
        KeyBinding::new("cmd-alt-up", FocusUp, CTX),
        KeyBinding::new("cmd-alt-down", FocusDown, CTX),
        KeyBinding::new("cmd-alt-shift-left", MoveColumnLeft, CTX),
        KeyBinding::new("cmd-alt-shift-right", MoveColumnRight, CTX),
        KeyBinding::new("cmd-alt-shift-up", MoveUp, CTX),
        KeyBinding::new("cmd-alt-shift-down", MoveDown, CTX),
        // niri's Mod+Home/End and Mod+Ctrl+Home/End; ⇧ stands for niri's Ctrl, as on the arrows.
        KeyBinding::new("cmd-alt-home", FocusColumnFirst, CTX),
        KeyBinding::new("cmd-alt-end", FocusColumnLast, CTX),
        KeyBinding::new("cmd-alt-shift-home", MoveColumnToFirst, CTX),
        KeyBinding::new("cmd-alt-shift-end", MoveColumnToLast, CTX),
        // The page keys are the workspace level: ⇧ moves the workspace itself, ⌃ carries the
        // column to the next one (niri's Mod+Shift and Mod+Ctrl on Page Up/Down).
        KeyBinding::new("cmd-alt-pageup", FocusWorkspaceUp, CTX),
        KeyBinding::new("cmd-alt-pagedown", FocusWorkspaceDown, CTX),
        KeyBinding::new("cmd-alt-shift-pageup", MoveWorkspaceUp, CTX),
        KeyBinding::new("cmd-alt-shift-pagedown", MoveWorkspaceDown, CTX),
        KeyBinding::new("ctrl-cmd-alt-pageup", MoveColumnToWorkspaceUp, CTX),
        KeyBinding::new("ctrl-cmd-alt-pagedown", MoveColumnToWorkspaceDown, CTX),
        KeyBinding::new("cmd-alt-`", FocusWorkspacePrevious, CTX),
        KeyBinding::new("cmd-[", ConsumeOrExpelLeft, CTX),
        KeyBinding::new("cmd-]", ConsumeOrExpelRight, CTX),
        KeyBinding::new("cmd-r", CycleWidth, CTX),
        KeyBinding::new("cmd-shift-r", CycleWidthBack, CTX),
        KeyBinding::new("cmd-alt--", NarrowColumn, CTX),
        KeyBinding::new("cmd-alt-=", WidenColumn, CTX),
        KeyBinding::new("cmd-shift-enter", MaximizeColumn, CTX),
        KeyBinding::new("ctrl-cmd-f", FullscreenTile, CTX),
        KeyBinding::new("cmd-alt-c", CenterColumn, CTX),
        KeyBinding::new("cmd-alt-shift-c", CenterVisibleColumns, CTX),
        KeyBinding::new("cmd-alt-shift-f", ExpandColumn, CTX),
        KeyBinding::new("cmd-alt-t", ToggleTabbed, CTX),
        KeyBinding::new("cmd-alt-o", ToggleOverview, CTX),
        KeyBinding::new("cmd-=", FontLarger, CTX),
        KeyBinding::new("cmd-shift-=", FontLarger, CTX),
        KeyBinding::new("cmd--", FontSmaller, CTX),
        KeyBinding::new("cmd-0", FontReset, CTX),
        KeyBinding::new("cmd-j", ToggleConversation, CTX),
        KeyBinding::new("cmd-l", EditAddress, CTX),
        // A browser's ⌘[ and ⌘] (and ⌘R) are the layout's here; ⌘← and ⌘→ are Chrome's others.
        KeyBinding::new("cmd-left", PageBack, PAGE),
        KeyBinding::new("cmd-right", PageForward, PAGE),
        KeyBinding::new("ctrl-o", CycleDensity, FACE),
        KeyBinding::new("escape", Interrupt, FACE),
        KeyBinding::new("cmd-up", crate::terminal::PrevPrompt, FACE),
        KeyBinding::new("cmd-down", crate::terminal::NextPrompt, FACE),
        KeyBinding::new("cmd-f", crate::terminal::Find, FACE),
        // The composer is where the keyboard sits in a face: bound after gpui-kit's input,
        // these win there.
        KeyBinding::new("ctrl-o", CycleDensity, FACE_INPUT),
        KeyBinding::new("cmd-up", crate::terminal::PrevPrompt, FACE_INPUT),
        KeyBinding::new("cmd-down", crate::terminal::NextPrompt, FACE_INPUT),
        KeyBinding::new("cmd-f", crate::terminal::Find, FACE_INPUT),
        KeyBinding::new("cmd-s", crate::file::SaveFile, Some(crate::file::CTX)),
        KeyBinding::new("cmd-f", crate::terminal::Find, FILE_INPUT),
        KeyBinding::new("cmd-alt-up", FocusUp, FILE_INPUT),
        KeyBinding::new("cmd-alt-down", FocusDown, FILE_INPUT),
        // A file tile's find bar: the terminal's find keys, in the bar's own context (no
        // terminal around it).
        KeyBinding::new("escape", crate::terminal::CloseFind, Some("FileSearch")),
        KeyBinding::new("cmd-g", crate::terminal::FindNext, Some("FileSearch")),
        KeyBinding::new("cmd-shift-g", crate::terminal::FindPrev, Some("FileSearch")),
        // A folder tile: the arrows walk its rows, ↩ opens one, ⌫ and ⌘↑ go up.
        KeyBinding::new("up", crate::folder::SelectPrevious, FOLDER),
        KeyBinding::new("down", crate::folder::SelectNext, FOLDER),
        KeyBinding::new("home", crate::folder::SelectFirst, FOLDER),
        KeyBinding::new("end", crate::folder::SelectLast, FOLDER),
        KeyBinding::new("enter", crate::folder::OpenSelected, FOLDER),
        KeyBinding::new("backspace", crate::folder::OpenParent, FOLDER),
        KeyBinding::new("cmd-up", crate::folder::OpenParent, FOLDER),
        // A page's find bar, the same keys (Esc is the bar's own).
        KeyBinding::new("cmd-g", crate::terminal::FindNext, Some("PageSearch")),
        KeyBinding::new("cmd-shift-g", crate::terminal::FindPrev, Some("PageSearch")),
    ];
    for (n, key) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"].into_iter().enumerate() {
        out.push(KeyBinding::new(&format!("cmd-{key}"), FocusColumn { index: n }, CTX));
        // niri's Mod+N and Mod+Ctrl+N. ⌘N is the column here, and ⇧ on a digit reaches the
        // app as its symbol on most layouts, so the column's carry takes ⌃.
        out.push(KeyBinding::new(&format!("cmd-alt-{key}"), FocusWorkspace { index: n }, CTX));
        out.push(KeyBinding::new(
            &format!("ctrl-cmd-alt-{key}"),
            MoveColumnToWorkspace { index: n },
            CTX,
        ));
    }
    out
}

/// The palette's lines for the workspace's and the terminal's actions, with their keys.
#[must_use]
pub fn palette_items() -> Vec<PaletteItem> {
    use crate::terminal::{
        ClearScreen, CopyLastOutput, Find, NextPrompt, NoteLastBlock, PrevPrompt, RerunLast,
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
        w("New agent", IconName::Bot, Box::new(NewAgent)),
        w("New note", IconName::StickyNote, Box::new(NewNote)),
        w("Add a window or display", IconName::AppWindow, Box::new(AddWindow)),
        w("Open a file", IconName::FileText, Box::new(OpenFile)),
        w("Open folder…", IconName::FolderOpen, Box::new(OpenFolder)),
        w("Enclosing folder", IconName::ArrowUp, Box::new(crate::folder::OpenParent)),
        w("Save file", IconName::Save, Box::new(crate::file::SaveFile)),
        w(SAVE_A_COPY, IconName::Download, Box::new(SaveCopy)),
        w("Open URL…", IconName::Globe, Box::new(OpenUrl)),
        w("Edit page address", IconName::Link, Box::new(EditAddress)),
        w("Page back", IconName::ArrowLeft, Box::new(PageBack)),
        w("Page forward", IconName::ArrowRight, Box::new(PageForward)),
        w("Reload page", IconName::RotateCw, Box::new(ReloadPage)),
        w("Close tile", IconName::X, Box::new(CloseItem)),
        w("Undo close", IconName::Undo2, Box::new(UndoClose)),
        w("Next agent that needs you", IconName::BellRing, Box::new(NextAttention)),
        w("Mute", IconName::VolumeX, Box::new(ToggleMute)),
        w("Stream stats", IconName::Activity, Box::new(ToggleStats)),
        w(
            crate::screen::TRACKPAD_MODE,
            IconName::MousePointer2,
            Box::new(crate::screen::ToggleTrackpad),
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
        w("Conversation density", IconName::ListChecks, Box::new(CycleDensity)),
        w("Stop the agent", IconName::Square, Box::new(Interrupt)),
        w("Larger text", IconName::AArrowUp, Box::new(FontLarger)),
        w("Smaller text", IconName::AArrowDown, Box::new(FontSmaller)),
        w("Default text size", IconName::Type, Box::new(FontReset)),
        t("Find in terminal, file or conversation", IconName::Search, Box::new(Find)),
        t("Previous prompt", IconName::ChevronUp, Box::new(PrevPrompt)),
        t("Next prompt", IconName::ChevronDown, Box::new(NextPrompt)),
        t("Copy last output", IconName::Copy, Box::new(CopyLastOutput)),
        t("Rerun last command", IconName::RotateCw, Box::new(RerunLast)),
        t("Keep last block as a note", IconName::NotebookPen, Box::new(NoteLastBlock)),
        t("Clear the screen and history", IconName::Eraser, Box::new(ClearScreen)),
    ];
    items.extend(crate::folder::files_palette_items(crate::folder::FILES_PICKER, &workspace));
    // Only a debug build's page is open to Web Inspector, and only the Mac has one of its own.
    if cfg!(all(debug_assertions, target_os = "macos")) {
        items.push(w("Inspect page", IconName::Wrench, Box::new(InspectPage)));
    }
    items
}
