//! The workspace's actions, the keys that run them and the palette lines that name them.
//!
//! ⌘ is the app's modifier. A ⌃ or ⌥ chord without ⌘ belongs to the terminal (⌥ is Meta), so
//! every layout chord carries ⌘. The table is `docs/decisions/workspace.md`'s.

#![expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]

use gpui::{Action, KeyBinding, actions};

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
        /// Move the keyboard focus to the next control, from anywhere, a terminal included.
        FocusNext,
        /// Move the keyboard focus to the previous control.
        FocusPrev,
        /// Open the command palette: every action by name, run by ↩.
        OpenPalette,
        /// Name the focused tile: a field in its header, ↩ keeps the name (blank clears
        /// it), Esc leaves it as it was.
        RenameItem,
        /// Point the other clients at the focused tile: each of them is offered a jump to it.
        PointOthers,
        /// Find text in every tile: the palette lists the tiles it is in with their hit
        /// counts, and ↩ opens that tile's find bar on it.
        FindEverywhere,
        /// Focus the column to the left.
        FocusColumnLeft,
        /// Focus the column to the right.
        FocusColumnRight,
        /// Focus the first column.
        FocusColumnFirst,
        /// Focus the last column.
        FocusColumnLast,
        /// Focus the tile above, or the workspace above from the top tile.
        FocusUp,
        /// Focus the tile below, or the workspace below from the bottom tile.
        FocusDown,
        /// Swap the focused column with the one to its left.
        MoveColumnLeft,
        /// Swap the focused column with the one to its right.
        MoveColumnRight,
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
        /// Move the active file card's reading line up one line.
        LineUp,
        /// Move the active file card's reading line down one line.
        LineDown,
        /// Move the active file card's reading line up one page.
        PageUp,
        /// Move the active file card's reading line down one page.
        PageDown,
        /// Move the active file card's reading line to the first line.
        LineFirst,
        /// Move the active file card's reading line to the last line.
        LineLast,
    ]
);

/// ⌘1…⌘9: focus column `index` (0-based) of the active workspace, as a browser's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, gpui::Action)]
#[action(namespace = workspace, no_json)]
pub struct FocusColumn {
    /// 0-based.
    pub index: usize,
}

/// The key context the workspace binds in. A focused remote window gets every chord (its
/// editor's ⌘W, ⌘T are not ours to take, as on Parsec); ⌃Tab alone stays, the keyboard's way
/// back out of it.
const CTX: Option<&str> = Some("Workspace && !Screen");
const RING_CTX: Option<&str> = Some("Workspace");
const FILE_CTX: Option<&str> = Some("Workspace && file_card");

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
        KeyBinding::new("cmd-alt-left", FocusColumnLeft, CTX),
        KeyBinding::new("cmd-alt-right", FocusColumnRight, CTX),
        KeyBinding::new("cmd-alt-up", FocusUp, CTX),
        KeyBinding::new("cmd-alt-down", FocusDown, CTX),
        KeyBinding::new("cmd-alt-shift-left", MoveColumnLeft, CTX),
        KeyBinding::new("cmd-alt-shift-right", MoveColumnRight, CTX),
        KeyBinding::new("cmd-alt-shift-up", MoveUp, CTX),
        KeyBinding::new("cmd-alt-shift-down", MoveDown, CTX),
        KeyBinding::new("cmd-[", ConsumeOrExpelLeft, CTX),
        KeyBinding::new("cmd-]", ConsumeOrExpelRight, CTX),
        KeyBinding::new("cmd-r", CycleWidth, CTX),
        KeyBinding::new("cmd-shift-r", CycleWidthBack, CTX),
        KeyBinding::new("cmd-alt--", NarrowColumn, CTX),
        KeyBinding::new("cmd-alt-=", WidenColumn, CTX),
        KeyBinding::new("cmd-shift-enter", MaximizeColumn, CTX),
        KeyBinding::new("ctrl-cmd-f", FullscreenTile, CTX),
        KeyBinding::new("cmd-alt-c", CenterColumn, CTX),
        KeyBinding::new("cmd-alt-t", ToggleTabbed, CTX),
        KeyBinding::new("cmd-alt-o", ToggleOverview, CTX),
        KeyBinding::new("cmd-=", FontLarger, CTX),
        KeyBinding::new("cmd-shift-=", FontLarger, CTX),
        KeyBinding::new("cmd--", FontSmaller, CTX),
        KeyBinding::new("cmd-0", FontReset, CTX),
        // The active file card's reading line. Only while a file card is focused (the
        // workspace sets `file_card` on its context then): a binding matches before a focused
        // terminal's key handler runs, so an unscoped `up` would take the arrows from a shell.
        KeyBinding::new("up", LineUp, FILE_CTX),
        KeyBinding::new("down", LineDown, FILE_CTX),
        KeyBinding::new("pageup", PageUp, FILE_CTX),
        KeyBinding::new("pagedown", PageDown, FILE_CTX),
        KeyBinding::new("home", LineFirst, FILE_CTX),
        KeyBinding::new("end", LineLast, FILE_CTX),
        KeyBinding::new("cmd-up", LineFirst, FILE_CTX),
        KeyBinding::new("cmd-down", LineLast, FILE_CTX),
        // A file card's find bar: the terminal's find keys, in the bar's own context (no
        // terminal around it).
        KeyBinding::new("escape", crate::terminal::CloseFind, Some("FileSearch")),
        KeyBinding::new("cmd-g", crate::terminal::FindNext, Some("FileSearch")),
        KeyBinding::new("cmd-shift-g", crate::terminal::FindPrev, Some("FileSearch")),
    ];
    for (n, key) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"].into_iter().enumerate() {
        out.push(KeyBinding::new(&format!("cmd-{key}"), FocusColumn { index: n }, CTX));
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
    let w = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &workspace);
    let t = |label: &str, action: Box<dyn Action>| PaletteItem::new(label, action, &terminal);
    vec![
        w("New terminal", Box::new(NewTerminal)),
        w("New agent", Box::new(NewAgent)),
        w("New note", Box::new(NewNote)),
        w("Add a window or display", Box::new(AddWindow)),
        w("Open a file", Box::new(OpenFile)),
        w("Close tile", Box::new(CloseItem)),
        w("Undo close", Box::new(UndoClose)),
        w("Next attention", Box::new(NextAttention)),
        w("Mute or unmute window", Box::new(ToggleMute)),
        w("Stream stats", Box::new(ToggleStats)),
        w("Name this tile", Box::new(RenameItem)),
        w("Point the others at this tile", Box::new(PointOthers)),
        w("Find in every tile", Box::new(FindEverywhere)),
        w("Column to the left", Box::new(FocusColumnLeft)),
        w("Column to the right", Box::new(FocusColumnRight)),
        w("First column", Box::new(FocusColumn { index: 0 })),
        w("Last column", Box::new(FocusColumnLast)),
        w("Tile or workspace above", Box::new(FocusUp)),
        w("Tile or workspace below", Box::new(FocusDown)),
        w("Move column left", Box::new(MoveColumnLeft)),
        w("Move column right", Box::new(MoveColumnRight)),
        w("Move tile up", Box::new(MoveUp)),
        w("Move tile down", Box::new(MoveDown)),
        w("Into the column on the left", Box::new(ConsumeOrExpelLeft)),
        w("Into the column on the right", Box::new(ConsumeOrExpelRight)),
        w("Next column width", Box::new(CycleWidth)),
        w("Previous column width", Box::new(CycleWidthBack)),
        w("Narrower column", Box::new(NarrowColumn)),
        w("Wider column", Box::new(WidenColumn)),
        w("Maximize column", Box::new(MaximizeColumn)),
        w("Fullscreen tile", Box::new(FullscreenTile)),
        w("Center column", Box::new(CenterColumn)),
        w("Tabbed column", Box::new(ToggleTabbed)),
        w("Overview", Box::new(ToggleOverview)),
        w("Larger text", Box::new(FontLarger)),
        w("Smaller text", Box::new(FontSmaller)),
        w("Default text size", Box::new(FontReset)),
        t("Find in terminal or file", Box::new(Find)),
        t("Previous prompt", Box::new(PrevPrompt)),
        t("Next prompt", Box::new(NextPrompt)),
        t("Copy last output", Box::new(CopyLastOutput)),
        t("Rerun last command", Box::new(RerunLast)),
        t("Keep last block as a note", Box::new(NoteLastBlock)),
        t("Clear the screen and history", Box::new(ClearScreen)),
    ]
}
