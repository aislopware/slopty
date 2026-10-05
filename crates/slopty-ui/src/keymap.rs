//! The app's key bindings: one table of every command a key can run, with its default chords
//! and the key contexts it binds in, and `settings.toml`'s `[keys]` laid over it.
//!
//! A command lives in a scope, which `[keys.<scope>]` names (`workspace`, `terminal`, `file`),
//! under a name of its own (`new_terminal`). The file gives it a chord in the palette's key
//! syntax (`chord`), a list of them, or none. What
//! the file does not name keeps its default, so the table here is the one place a default is
//! written.
//!
//! One chord runs one command in a context. A chord the file gives a command is taken from a
//! default that held it there, and the app says so; two of the file's own on one chord keep the
//! first in the table's order, and the app says that too. The deeper binding wins in GPUI, so a
//! default in a context nested in the file's (a terminal's ⌘K inside the workspace, anything
//! under the app's) would shadow the file's chord there: it gives the chord up too. Two
//! defaults in nested contexts are the table's layering, not a clash.
//!
//! [`install`] binds a keymap in GPUI in place of the one before, keeping every binding that is
//! not the keymap's (gpui-kit's text fields, the app menu's), so a saved `settings.toml` rebinds
//! the keys at once. The map in effect is kept for this thread, GPUI's, so what shows a chord
//! (the palette, a menu, a hint) reads it through [`current`] without a context to hand.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, KeyBindingMetaIndex};
use slopty_settings::KeySettings;

use self::chord::Chord;
pub use self::chord::ChordError;
use crate::workspace::actions as ws;

mod chord;

/// Where a command's keys apply: `[keys.<name>]` in the file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Scope {
    /// The app itself, whatever has the keyboard.
    App,
    /// The workspace: tiles, columns and workspaces.
    Workspace,
    /// A web page tile.
    Page,
    /// A terminal.
    Terminal,
    /// A file tile's editor.
    File,
    /// An agent's conversation face.
    Conversation,
    /// A folder tile.
    Folder,
    /// Search in files.
    Search,
    /// A project's board.
    Project,
}

impl Scope {
    /// Every scope, in the order the Keyboard page lists them.
    pub const ALL: [Self; 9] = [
        Self::App,
        Self::Workspace,
        Self::Terminal,
        Self::Conversation,
        Self::Project,
        Self::File,
        Self::Folder,
        Self::Search,
        Self::Page,
    ];

    /// Its table under `[keys]`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Workspace => "workspace",
            Self::Page => "page",
            Self::Terminal => "terminal",
            Self::File => "file",
            Self::Conversation => "conversation",
            Self::Folder => "folder",
            Self::Search => "search",
            Self::Project => "project",
        }
    }

    /// Whether a key alone (an arrow, ↩) is a chord here: a folder's rows and a board's hold no
    /// text, so bare keys walk them. Elsewhere a key alone is typed into the
    /// terminal or a field, and a chord carries ⌘ or ⌃ (or is an F key).
    #[must_use]
    pub const fn takes_bare_keys(self) -> bool {
        matches!(self, Self::Folder | Self::Project)
    }

    fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// One thing a key can run, in one scope.
pub struct Command {
    scope: Scope,
    name: Cow<'static, str>,
    action: Box<dyn Action>,
    contexts: &'static [Option<&'static str>],
    defaults: Vec<String>,
}

impl Clone for Command {
    fn clone(&self) -> Self {
        Self {
            scope: self.scope,
            name: self.name.clone(),
            action: self.action.boxed_clone(),
            contexts: self.contexts,
            defaults: self.defaults.clone(),
        }
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("key", &self.key())
            .field("action", &self.action.name())
            .field("defaults", &self.defaults)
            .finish_non_exhaustive()
    }
}

impl Command {
    /// `action` as `scope`'s `name`, bound in each of `contexts` (`None` binds it everywhere)
    /// to `defaults`, chords in the palette's key syntax.
    #[must_use]
    pub fn new(
        scope: Scope,
        name: impl Into<Cow<'static, str>>,
        action: impl Action,
        defaults: &[&str],
        contexts: &'static [Option<&'static str>],
    ) -> Self {
        Self {
            scope,
            name: name.into(),
            action: Box::new(action),
            contexts,
            defaults: defaults.iter().map(|&d| d.to_owned()).collect(),
        }
    }

    /// Its scope.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// Its name in its scope's table (`new_terminal`).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Scope and name as the file spells them: `workspace.new_terminal`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}.{}", self.scope.name(), self.name)
    }

    /// Whether a key alone is a chord for it: its scope takes them, or every context it binds
    /// in holds no text to type into: a PDF's pages in a file tile (whose scope also holds the
    /// editor's commands), a folder, a board.
    #[must_use]
    pub fn takes_bare_keys(&self) -> bool {
        self.scope.takes_bare_keys()
            || (!self.contexts.is_empty() && self.contexts.iter().all(|c| TEXTLESS.contains(c)))
    }

    /// What it runs.
    #[must_use]
    pub fn action(&self) -> &dyn Action {
        self.action.as_ref()
    }

    /// Its default chords, as GPUI reads them.
    fn default_chords(&self) -> Vec<String> {
        let mut out = Vec::new();
        for text in &self.defaults {
            match canonical(text) {
                Ok(chord) => push_once(&mut out, chord),
                Err(e) => tracing::error!(key = self.key(), text, error = %e, "default chord"),
            }
        }
        out
    }
}

/// The contexts the table binds in. The workspace's: a focused remote window gets every chord
/// (its editor's ⌘W, ⌘T are not ours to take, as on Parsec).
const CTX: Option<&str> = Some("Workspace && !Screen");
/// ⌃Tab alone stays with a remote window: the keyboard's way back out of it.
const RING: Option<&str> = Some("Workspace");
/// A remote window or display that has the keyboard: its app takes every chord, so the few of
/// ours a remote view needs (the palette, the sound, the stats) take ⌃ on top of their own.
const REMOTE: Option<&str> = Some("Workspace > Screen");
/// Any focused field (a find bar, the palette, a file's text): a gpui-kit input, whose own ⌘⇧F is
/// replace; ours is bound after it, so it wins there too.
const INPUT: Option<&str> = Some("Input");
/// Inside a file tile's editor, gpui-kit's input binds some of the workspace's chords to
/// editing (⌘⌥↑ adds a caret, ⌘F opens its own search); bound after it, ours win there.
const FILE_INPUT: Option<&str> = Some("FileEditor > Input");
const FILE: Option<&str> = Some(crate::file::CTX);
/// A file tile's find bar: the terminal's find keys, in the bar's own context.
const FILE_SEARCH: Option<&str> = Some("FileSearch");
/// A file tile's editor itself, not its fields: deeper than gpui-kit's `Input`, so ours win.
const FILE_TEXT: Option<&str> = Some("FileText > Input");
/// A file tile's "go to line" field.
const FILE_GO_TO: Option<&str> = Some(crate::file::GO_TO_CTX);
/// A file tile's symbol list.
const FILE_SYMBOLS: Option<&str> = Some(crate::file::SYMBOLS_CTX);
/// A file tile showing a PDF's pages: they hold no text to type into, so bare keys scroll them.
const FILE_PAGES: Option<&str> = Some("FileEditor && FilePages");
/// Contexts with no text field in them, where a key alone cannot be wanted for typing.
const TEXTLESS: [Option<&str>; 3] = [FILE_PAGES, FOLDER, BOARD];
/// A conversation face, and its composer, where the keyboard sits in a face.
const FACE: Option<&str> = Some(crate::conversation::CTX);
const FACE_INPUT: Option<&str> = Some("Conversation > Input");
/// A thread view's composer (`thread::view::COMPOSER_CTX`), which waits a message for the
/// turn under way.
const THREAD_INPUT: Option<&str> = Some("ThreadComposer > Input");
const FOLDER: Option<&str> = Some(crate::folder::CTX);
/// A project's board: its rows hold no text, so bare keys walk them.
const BOARD: Option<&str> = Some(crate::project::CTX);
/// Search in files, its fields holding the keyboard.
const SEARCH: Option<&str> = Some(crate::search::CTX);
/// A tile's window of its own: its picture takes every chord but the one that puts it back.
const POP_OUT: Option<&str> = Some(ws::POP_OUT_CTX);
/// A focused page that does not hold the keyboard: a page that does keeps ⌘← and ⌘→ for its
/// own fields.
const PAGE: Option<&str> = Some("Workspace && Page && !Screen");
/// A page that holds the keyboard: its edit keys are the page's, before the workspace's
/// (⌘Z is "Undo close" there while a closed tile's notice is up).
const PAGE_HELD: Option<&str> = Some("PageBody > NativeView");
/// A page's find bar (Esc is the bar's own).
const PAGE_SEARCH: Option<&str> = Some("PageSearch");
const TERMINAL: Option<&str> = Some("Terminal");
/// The terminal's search field itself: Esc in the grid goes to the program.
const TERMINAL_SEARCH: Option<&str> = Some("TerminalSearch");

const W: &[Option<&str>] = &[CTX];
/// The workspace while a closed tile's notice is up ([`ws::CLOSING_CTX`]).
const CLOSING: Option<&str> = Some("Workspace && ClosingOffered && !Screen");
const APP: &[Option<&str>] = &[None];

/// Every command of the table, in the order they bind.
///
/// ⌘ is the app's modifier. A ⌃ or ⌥ chord without ⌘ belongs to the terminal (⌥ is Meta), so
/// every layout chord carries ⌘ (`docs/decisions/workspace.md` has the layout's table).
#[must_use]
#[expect(clippy::too_many_lines, reason = "the one table of every default chord")]
pub fn defaults() -> Vec<Command> {
    use Scope::{Conversation, File, Folder, Page, Project, Search, Terminal, Workspace};

    use crate::conversation::{
        AskAside, BranchFromHere, CompactContext, CycleDensity, CycleEffort, EditLastQueued,
        Interrupt, QueueMessage, ResumeAgent, ReviewChanges, ShowAgentTerminal, TakeBack,
    };
    use crate::terminal as t;

    fn c(
        scope: Scope,
        name: &'static str,
        action: impl Action,
        defaults: &[&str],
        contexts: &'static [Option<&'static str>],
    ) -> Command {
        Command::new(scope, name, action, defaults, contexts)
    }
    let mut out = vec![
        c(Workspace, "new_terminal", ws::NewTerminal, &["cmd-t", "cmd-n"], W),
        c(Workspace, "new_agent", ws::NewAgent, &["cmd-shift-t"], W),
        c(Workspace, "new_note", ws::NewNote, &["cmd-shift-n"], W),
        c(Workspace, "add_window", ws::AddWindow, &["cmd-o"], W),
        c(Workspace, "open_file", ws::OpenFile, &["cmd-p"], W),
        c(Workspace, "open_folder", ws::OpenFolder, &[], W),
        c(Workspace, "open_url", ws::OpenUrl, &[], W),
        c(Workspace, "review_changes", ws::ReviewChanges, &[], W),
        c(Workspace, "open_last_offer", ws::OpenLastOffer, &[], W),
        c(Workspace, "save_copy", ws::SaveCopy, &[], W),
        c(Workspace, "close_tile", ws::CloseItem, &["cmd-w"], W),
        c(Workspace, "undo_close", ws::UndoClose, &["cmd-z"], &[CLOSING]),
        c(Workspace, "next_attention", ws::NextAttention, &["cmd-shift-a"], W),
        c(Workspace, "show_needs_you", ws::ShowNeedsYou, &["cmd-shift-u"], W),
        c(Workspace, "filter_navigator", ws::FilterNavigator, &["cmd-shift-e"], W),
        c(Workspace, "toggle_mute", ws::ToggleMute, &["cmd-shift-m"], W),
        c(Workspace, "toggle_stats", ws::ToggleStats, &["cmd-shift-i"], W),
        c(Workspace, "type_clipboard", ws::TypeClipboard, &[], W),
        c(Workspace, "toggle_sized_display", ws::ToggleSizedDisplay, &[], W),
        c(Workspace, "toggle_own_window", ws::ToggleOwnWindow, &["ctrl-cmd-n"], &[CTX, POP_OUT]),
        c(Workspace, "toggle_system_keys", ws::ToggleSystemKeys, &[], W),
        c(Workspace, "toggle_navigator", ws::ToggleNavigator, &["cmd-b"], W),
        c(Workspace, "toggle_navigator_lens", ws::ToggleNavigatorLens, &[], W),
        c(Workspace, "find", t::Find, &["cmd-f"], W),
        // Tab is the shell's; ⌃Tab enters the control ring from a terminal, then Tab walks it.
        c(Workspace, "focus_next", ws::FocusNext, &["ctrl-tab"], &[RING]),
        c(Workspace, "focus_previous", ws::FocusPrev, &["ctrl-shift-tab"], &[RING]),
        c(Workspace, "open_palette", ws::OpenPalette, &["cmd-shift-p"], W),
        // A remote VS Code's ⌘⇧P is its own; ⌃⌘⇧P is nobody's there.
        c(
            Workspace,
            "open_palette_in_remote_window",
            ws::OpenPalette,
            &["ctrl-cmd-shift-p"],
            &[REMOTE],
        ),
        c(
            Workspace,
            "toggle_mute_in_remote_window",
            ws::ToggleMute,
            &["ctrl-cmd-shift-m"],
            &[REMOTE],
        ),
        c(
            Workspace,
            "toggle_stats_in_remote_window",
            ws::ToggleStats,
            &["ctrl-cmd-shift-i"],
            &[REMOTE],
        ),
        c(Workspace, "list_ports", ws::ListPorts, &[], W),
        c(Workspace, "rename_tile", ws::RenameItem, &["cmd-e"], W),
        c(Workspace, "search_in_files", ws::SearchInFiles, &["cmd-shift-f"], &[CTX, INPUT]),
        c(Workspace, "focus_column_left", ws::FocusColumnLeft, &["cmd-alt-left"], W),
        c(Workspace, "focus_column_right", ws::FocusColumnRight, &["cmd-alt-right"], W),
        c(Workspace, "focus_up", ws::FocusUp, &["cmd-alt-up"], &[CTX, FILE_INPUT]),
        c(Workspace, "focus_down", ws::FocusDown, &["cmd-alt-down"], &[CTX, FILE_INPUT]),
        c(Workspace, "move_column_left", ws::MoveColumnLeft, &["cmd-alt-shift-left"], W),
        c(Workspace, "move_column_right", ws::MoveColumnRight, &["cmd-alt-shift-right"], W),
        c(Workspace, "move_up", ws::MoveUp, &["cmd-alt-shift-up"], W),
        c(Workspace, "move_down", ws::MoveDown, &["cmd-alt-shift-down"], W),
        // niri's Mod+Ctrl+Home/End; ⇧ stands for niri's Ctrl, as on the arrows.
        c(Workspace, "move_column_to_first", ws::MoveColumnToFirst, &["cmd-alt-shift-home"], W),
        c(Workspace, "move_column_to_last", ws::MoveColumnToLast, &["cmd-alt-shift-end"], W),
        // The page keys are the workspace level (niri's Mod on Page Up/Down).
        c(Workspace, "focus_workspace_up", ws::FocusWorkspaceUp, &["cmd-alt-pageup"], W),
        c(Workspace, "focus_workspace_down", ws::FocusWorkspaceDown, &["cmd-alt-pagedown"], W),
        c(Workspace, "consume_or_expel_left", ws::ConsumeOrExpelLeft, &["cmd-["], W),
        c(Workspace, "consume_or_expel_right", ws::ConsumeOrExpelRight, &["cmd-]"], W),
        c(Workspace, "cycle_width", ws::CycleWidth, &["cmd-r"], W),
        c(Workspace, "maximize_column", ws::MaximizeColumn, &["cmd-shift-enter"], W),
        c(Workspace, "fullscreen_tile", ws::FullscreenTile, &["ctrl-cmd-f"], W),
        c(Workspace, "center_column", ws::CenterColumn, &["cmd-alt-c"], W),
        c(Workspace, "toggle_tabbed", ws::ToggleTabbed, &["cmd-alt-t"], W),
        c(Workspace, "toggle_overview", ws::ToggleOverview, &["cmd-alt-o"], W),
        c(Workspace, "font_larger", ws::FontLarger, &["cmd-=", "cmd-shift-="], W),
        c(Workspace, "font_smaller", ws::FontSmaller, &["cmd--"], W),
        c(Workspace, "font_reset", ws::FontReset, &["cmd-0"], W),
        c(Workspace, "toggle_conversation", ws::ToggleConversation, &["cmd-j"], W),
        c(Workspace, "toggle_project_board", ws::ToggleProjectBoard, &["cmd-shift-j"], W),
        c(Workspace, "edit_address", ws::EditAddress, &["cmd-l"], W),
        c(Workspace, "toggle_trackpad", crate::screen::ToggleTrackpad, &[], W),
        c(Workspace, "toggle_remote_gestures", crate::screen::ToggleRemoteGestures, &[], W),
    ];
    for (index, key) in ('1'..='9').enumerate() {
        out.push(Command::new(
            Workspace,
            format!("focus_column_{key}"),
            ws::FocusColumn { index },
            &[format!("cmd-{key}").as_str()],
            W,
        ));
    }
    // niri's Mod+N: ⌘N is the column here.
    for (index, key) in ('1'..='9').enumerate() {
        out.push(Command::new(
            Workspace,
            format!("focus_workspace_{key}"),
            ws::FocusWorkspace { index },
            &[format!("cmd-alt-{key}").as_str()],
            W,
        ));
    }
    out.extend([
        // A browser's ⌘[ and ⌘] (and ⌘R) are the layout's here; ⌘← and ⌘→ are Chrome's others.
        c(Page, "back", ws::PageBack, &["cmd-left"], &[PAGE]),
        c(Page, "forward", ws::PageForward, &["cmd-right"], &[PAGE]),
        c(Page, "reload", ws::ReloadPage, &[], &[PAGE]),
        c(Page, "find_next", t::FindNext, &["cmd-g"], &[PAGE_SEARCH]),
        c(Page, "find_previous", t::FindPrev, &["cmd-shift-g"], &[PAGE_SEARCH]),
        c(Conversation, "cycle_density", CycleDensity, &["ctrl-o"], &[FACE, FACE_INPUT]),
        c(Conversation, "interrupt", Interrupt, &["escape"], &[FACE]),
        c(Conversation, "queue_message", QueueMessage, &["cmd-enter"], &[THREAD_INPUT]),
        c(Conversation, "edit_last_queued", EditLastQueued, &["alt-up"], &[THREAD_INPUT]),
        c(Conversation, "cycle_effort", CycleEffort, &[], &[FACE]),
        c(Conversation, "ask_aside", AskAside, &[], &[FACE]),
        c(Conversation, "review_changes", ReviewChanges, &[], &[FACE]),
        c(Conversation, "show_agent_terminal", ShowAgentTerminal, &[], &[FACE]),
        c(Conversation, "take_back", TakeBack, &[], &[FACE]),
        c(Conversation, "compact_context", CompactContext, &[], &[FACE]),
        c(Conversation, "branch_from_here", BranchFromHere, &[], &[FACE]),
        c(Conversation, "resume_agent", ResumeAgent, &[], &[FACE]),
        c(Conversation, "previous_prompt", t::PrevPrompt, &["cmd-up"], &[FACE, FACE_INPUT]),
        c(Conversation, "next_prompt", t::NextPrompt, &["cmd-down"], &[FACE, FACE_INPUT]),
        c(Conversation, "find", t::Find, &["cmd-f"], &[FACE, FACE_INPUT]),
        c(File, "save", crate::file::SaveFile, &["cmd-s"], &[FILE]),
        // In the editor too, over its own ⌘↩ (a new line), only while a program waits.
        c(File, "finish_edit", crate::file::FinishEdit, &["cmd-enter"], &[FILE, FILE_INPUT]),
        // The tile's own too, for a Markdown preview, which holds no field.
        c(File, "find", t::Find, &["cmd-f"], &[FILE_INPUT, FILE]),
        c(File, "close_find", t::CloseFind, &["escape"], &[FILE_SEARCH]),
        c(File, "find_next", t::FindNext, &["cmd-g"], &[FILE_SEARCH]),
        c(File, "find_previous", t::FindPrev, &["cmd-shift-g"], &[FILE_SEARCH]),
        // The editor's own commands, Zed's and VS Code's keys.
        c(File, "toggle_comment", crate::file::ToggleComment, &["cmd-/"], &[FILE_TEXT]),
        c(File, "go_to_line", crate::file::GoToLine, &["ctrl-g"], &[FILE_TEXT]),
        c(File, "close_go_to_line", crate::file::CloseGoToLine, &["escape"], &[FILE_GO_TO]),
        c(File, "move_line_up", crate::file::MoveLineUp, &["alt-up"], &[FILE_TEXT]),
        c(File, "move_line_down", crate::file::MoveLineDown, &["alt-down"], &[FILE_TEXT]),
        c(File, "duplicate_line", crate::file::DuplicateLine, &["alt-shift-down"], &[FILE_TEXT]),
        c(File, "jump_to_bracket", crate::file::JumpToBracket, &["cmd-shift-\\"], &[FILE_TEXT]),
        c(File, "toggle_soft_wrap", crate::file::ToggleSoftWrap, &[], &[FILE]),
        c(File, "toggle_preview", crate::file::TogglePreview, &["cmd-shift-v"], &[FILE]),
        // Zed's replace key; the find bar's toggles are the search surface's.
        c(
            File,
            "toggle_replace",
            crate::file::ToggleReplace,
            &["cmd-shift-h"],
            &[FILE_TEXT, FILE_SEARCH],
        ),
        c(
            File,
            "toggle_match_case",
            crate::search::ToggleMatchCase,
            &["cmd-alt-c"],
            &[FILE_SEARCH],
        ),
        c(
            File,
            "toggle_whole_word",
            crate::search::ToggleWholeWord,
            &["cmd-alt-w"],
            &[FILE_SEARCH],
        ),
        c(File, "toggle_regex", crate::search::ToggleRegex, &["cmd-alt-r"], &[FILE_SEARCH]),
        // Zed's and VS Code's outline key.
        c(File, "go_to_symbol", crate::file::GoToSymbol, &["cmd-shift-o"], &[FILE_TEXT]),
        // Sublime's, Zed's and VS Code's keys for more selections from the selected text.
        c(
            File,
            "select_next_occurrence",
            gpui_kit::component::input::SelectNextOccurrence,
            &["cmd-d"],
            &[FILE_TEXT],
        ),
        c(
            File,
            "select_all_occurrences",
            gpui_kit::component::input::SelectAllOccurrences,
            &["cmd-shift-l"],
            &[FILE_TEXT],
        ),
        c(File, "close_symbols", crate::file::CloseSymbols, &["escape"], &[FILE_SYMBOLS]),
        c(File, "next_symbol", crate::file::NextSymbol, &["down"], &[FILE_SYMBOLS]),
        c(File, "previous_symbol", crate::file::PreviousSymbol, &["up"], &[FILE_SYMBOLS]),
        // A PDF's pages, as Preview reads them.
        c(File, "scroll_down", crate::file::ScrollDown, &["down"], &[FILE_PAGES]),
        c(File, "scroll_up", crate::file::ScrollUp, &["up"], &[FILE_PAGES]),
        c(File, "next_screen", crate::file::NextScreen, &["pagedown", "space"], &[FILE_PAGES]),
        c(
            File,
            "previous_screen",
            crate::file::PreviousScreen,
            &["pageup", "shift-space"],
            &[FILE_PAGES],
        ),
        // A folder tile: the arrows walk its rows, ↩ opens one, ⌫ and ⌘↑ go up.
        c(Folder, "select_previous", crate::folder::SelectPrevious, &["up"], &[FOLDER]),
        c(Folder, "select_next", crate::folder::SelectNext, &["down"], &[FOLDER]),
        c(Folder, "select_first", crate::folder::SelectFirst, &["home"], &[FOLDER]),
        c(Folder, "select_last", crate::folder::SelectLast, &["end"], &[FOLDER]),
        c(Folder, "open", crate::folder::OpenSelected, &["enter"], &[FOLDER]),
        c(Folder, "open_parent", crate::folder::OpenParent, &["backspace", "cmd-up"], &[FOLDER]),
        // Finder's keys.
        c(Folder, "new_folder", crate::folder::NewFolder, &["cmd-shift-n"], &[FOLDER]),
        c(Folder, "rename", crate::folder::RenameSelected, &[], &[FOLDER]),
        c(Folder, "move_to_trash", crate::folder::TrashSelected, &["cmd-backspace"], &[FOLDER]),
        // A board: the arrows walk its cards, ↩ opens one's agent.
        c(Project, "select_previous", crate::project::SelectPrevious, &["up", "k"], &[BOARD]),
        c(Project, "select_next", crate::project::SelectNext, &["down", "j"], &[BOARD]),
        c(Project, "open", crate::project::OpenNode, &["enter"], &[BOARD]),
        // The search surface's toggles, VS Code's keys.
        c(Search, "toggle_match_case", crate::search::ToggleMatchCase, &["cmd-alt-c"], &[SEARCH]),
        c(Search, "toggle_whole_word", crate::search::ToggleWholeWord, &["cmd-alt-w"], &[SEARCH]),
        c(Search, "toggle_regex", crate::search::ToggleRegex, &["cmd-alt-r"], &[SEARCH]),
        c(Terminal, "copy", t::Copy, &["cmd-c"], &[TERMINAL]),
        c(Terminal, "paste", t::Paste, &["cmd-v"], &[TERMINAL]),
        c(Terminal, "find", t::Find, &["cmd-f"], &[TERMINAL]),
        c(Terminal, "find_next", t::FindNext, &["cmd-g"], &[TERMINAL]),
        c(Terminal, "find_previous", t::FindPrev, &["cmd-shift-g"], &[TERMINAL]),
        c(Terminal, "close_find", t::CloseFind, &["escape"], &[TERMINAL_SEARCH]),
        c(Terminal, "previous_prompt", t::PrevPrompt, &["cmd-up"], &[TERMINAL]),
        c(Terminal, "next_prompt", t::NextPrompt, &["cmd-down"], &[TERMINAL]),
        // ghostty's scroll keys; the ⌘ pair is what Terminal.app taught the Mac.
        c(Terminal, "scroll_page_up", t::ScrollPageUp, &["shift-pageup"], &[TERMINAL]),
        c(Terminal, "scroll_page_down", t::ScrollPageDown, &["shift-pagedown"], &[TERMINAL]),
        c(Terminal, "scroll_to_top", t::ScrollToTop, &["shift-home", "cmd-home"], &[TERMINAL]),
        c(Terminal, "scroll_to_bottom", t::ScrollToBottom, &["shift-end", "cmd-end"], &[TERMINAL]),
        c(Terminal, "select_all", t::SelectAll, &["cmd-a"], &[TERMINAL]),
        c(Terminal, "copy_last_output", t::CopyLastOutput, &["cmd-shift-c"], &[TERMINAL]),
        // WezTerm's copy mode chord (⌃⇧X), with ⌘ for ⌃ as every app chord here.
        c(Terminal, "copy_mode", t::CopyMode, &["cmd-shift-x"], &[TERMINAL]),
        // ⌘⇧↩ is the workspace's maximize-column; "Rerun last command" is in the palette.
        c(Terminal, "rerun_last", t::RerunLast, &[], &[TERMINAL]),
        c(Terminal, "copy_block_output", t::CopyBlockOutput, &[], &[TERMINAL]),
        c(Terminal, "attach_block", t::AttachBlock, &[], &[TERMINAL]),
        c(Terminal, "attach_selection", t::AttachSelection, &[], &[TERMINAL]),
        c(Terminal, "clear_screen", t::ClearScreen, &["cmd-k"], &[TERMINAL]),
    ]);
    // AppKit's menu and WebKit take these nowhere a page's field would expect; the page does
    // them itself (`browser::Edit`). UIKit hands a hardware keyboard's keys to a page that holds
    // them, which does its own edits, so on iOS the keymap never hears them there: no chord is
    // listed that would do nothing.
    if cfg!(target_os = "macos") {
        out.extend([
            c(Page, "undo", ws::PageUndo, &["cmd-z"], &[PAGE_HELD]),
            c(Page, "redo", ws::PageRedo, &["cmd-shift-z"], &[PAGE_HELD]),
            c(Page, "cut", ws::PageCut, &["cmd-x"], &[PAGE_HELD]),
            c(Page, "select_all", ws::PageSelectAll, &["cmd-a"], &[PAGE_HELD]),
        ]);
    }
    out.extend(crate::project::key_bindings());
    out
}

/// A command bound everywhere, whatever has the keyboard: the app's own (the settings, adding
/// a worker), which live outside this crate.
#[must_use]
pub fn app_command(name: &'static str, action: impl Action, defaults: &[&str]) -> Command {
    Command::new(Scope::App, name, action, defaults, APP)
}

/// The table with a file's `[keys]` over it: each command's chords in effect, and what the
/// file said that did not hold.
pub struct Keymap {
    commands: Vec<Command>,
    /// Per command, its chords in effect, each as GPUI writes it ([`canonical`]).
    chords: Vec<Vec<String>>,
    /// Per command, whether the file set its chords.
    set: Vec<bool>,
    /// Per command, its first chord as the palette shows it (`⇧⌘T`), or nothing: read while
    /// drawing, so worked out once here.
    labels: Vec<String>,
    diagnostics: Vec<String>,
}

impl std::fmt::Debug for Keymap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keymap")
            .field("commands", &self.commands.len())
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl Default for Keymap {
    /// The table as it is, with none of the file's.
    fn default() -> Self {
        Self::new(&KeySettings::default(), Vec::new())
    }
}

/// What marks the keymap's bindings among GPUI's, so [`install`] replaces only them.
const OURS: KeyBindingMetaIndex = KeyBindingMetaIndex(u32::from_be_bytes(*b"KEYS"));

impl Keymap {
    /// [`defaults`] and `extra` (the app's own commands), with `keys` over them.
    #[must_use]
    pub fn new(keys: &KeySettings, extra: Vec<Command>) -> Self {
        let mut commands = defaults();
        commands.extend(extra);
        Self::resolve(commands, keys)
    }

    /// The same commands with `keys` over them instead.
    #[must_use]
    pub fn with_keys(&self, keys: &KeySettings) -> Self {
        Self::resolve(self.commands.clone(), keys)
    }

    fn resolve(commands: Vec<Command>, keys: &KeySettings) -> Self {
        let mut diagnostics = Vec::new();
        let mut set: Vec<Option<Vec<String>>> = vec![None; commands.len()];
        for (scope_name, table) in &keys.0 {
            let Some(scope) = Scope::named(scope_name) else {
                let known: Vec<&str> = Scope::ALL.map(Scope::name).to_vec();
                diagnostics.push(format!(
                    "unknown context `keys.{scope_name}` (the contexts are {})",
                    known.join(", ")
                ));
                continue;
            };
            for (name, chords) in table {
                let at = commands.iter().position(|c| c.scope == scope && c.name == *name);
                let Some(at) = at else {
                    diagnostics.push(format!("unknown action `keys.{scope_name}.{name}`"));
                    continue;
                };
                let mut read = Vec::new();
                for text in &chords.0 {
                    match canonical(text) {
                        Ok(chord) => push_once(&mut read, chord),
                        Err(e) => diagnostics.push(format!("`keys.{scope_name}.{name}`: {e}")),
                    }
                }
                if let Some(slot) = set.get_mut(at) {
                    *slot = Some(read);
                }
            }
        }
        let mut chords: Vec<Vec<String>> = commands
            .iter()
            .zip(&set)
            .map(|(command, set)| set.clone().unwrap_or_else(|| command.default_chords()))
            .collect();
        let set: Vec<bool> = set.iter().map(Option::is_some).collect();
        settle_clashes(&commands, &set, &mut chords, &mut diagnostics);
        let labels =
            chords.iter().map(|c| c.first().map(|c| label(c)).unwrap_or_default()).collect();
        Self { commands, chords, set, labels, diagnostics }
    }

    /// Every command, in the table's order.
    #[must_use]
    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    /// Command `ix`'s chords in effect, as GPUI reads them.
    #[must_use]
    pub fn chords(&self, ix: usize) -> &[String] {
        self.chords.get(ix).map_or(&[], Vec::as_slice)
    }

    /// Command `ix`'s chords by default, as GPUI reads them.
    #[must_use]
    pub fn default_chords(&self, ix: usize) -> Vec<String> {
        self.commands.get(ix).map(Command::default_chords).unwrap_or_default()
    }

    /// The first chord that runs `action`, as the palette shows it (`⇧⌘T`); empty when none
    /// does. The first command in the table's order that has one gives it.
    #[must_use]
    pub fn label_of(&self, action: &dyn Action) -> &str {
        self.commands
            .iter()
            .zip(&self.labels)
            .find(|(command, label)| !label.is_empty() && command.action.partial_eq(action))
            .map_or("", |(_, label)| label.as_str())
    }

    /// Whether the file sets command `ix`'s chords.
    #[must_use]
    pub fn is_set(&self, ix: usize) -> bool {
        self.set.get(ix).copied().unwrap_or_default()
    }

    /// Command `name` of `scope`.
    #[must_use]
    pub fn find(&self, scope: Scope, name: &str) -> Option<usize> {
        self.commands.iter().position(|c| c.scope == scope && c.name == name)
    }

    /// What the file said that did not hold, one line each: a context or an action that does
    /// not exist, a chord that does not read, a chord two commands want.
    #[must_use]
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Whether `other` binds the same chords to the same commands.
    #[must_use]
    pub fn binds_as(&self, other: &Self) -> bool {
        self.chords == other.chords
            && self.commands.len() == other.commands.len()
            && self.commands.iter().zip(&other.commands).all(|(a, b)| a.key() == b.key())
    }

    /// The GPUI bindings of the commands of the scopes `of` takes, in the table's order.
    #[must_use]
    pub fn bindings(&self, of: impl Fn(Scope) -> bool) -> Vec<KeyBinding> {
        let mut out = Vec::new();
        for (command, chords) in self.commands.iter().zip(&self.chords) {
            if !of(command.scope) {
                continue;
            }
            for chord in chords {
                for context in command.contexts {
                    match binding(chord, command.action(), *context) {
                        Ok(binding) => out.push(binding.with_meta(OURS)),
                        Err(e) => {
                            tracing::error!(key = command.key(), chord, error = %e, "binding");
                        }
                    }
                }
            }
        }
        out
    }
}

fn binding(chord: &str, action: &dyn Action, context: Option<&str>) -> anyhow::Result<KeyBinding> {
    let predicate = context.map(KeyBindingContextPredicate::parse).transpose()?.map(Rc::new);
    Ok(KeyBinding::load(
        chord,
        action.boxed_clone(),
        predicate,
        false,
        None,
        &gpui::DummyKeyboardMapper,
    )?)
}

/// `text`, a chord in the palette's key syntax, read (`Chord::read`) and written as GPUI writes a
/// keystroke (`cmd-shift-t`), so one chord is one string however it was spelled.
///
/// # Errors
///
/// A word that names no key.
pub fn canonical(text: &str) -> Result<String, ChordError> {
    let keys = Chord::read(text)?.keys();
    Ok(gpui::Keystroke::parse(&keys).map_or(keys, |k| k.unparse()))
}

fn push_once(list: &mut Vec<String>, chord: String) {
    if !list.contains(&chord) {
        list.push(chord);
    }
}

/// One command per chord in a context. The file's commands claim their chords first, in the
/// table's order, then the defaults; a later claim on a held chord gives it up and is said. A
/// default gives a chord up as well where the file's holds it in a context around the default's
/// ([`encloses`]), since the deeper binding would win there. Two defaults never meet (a test
/// holds the table to it).
fn settle_clashes(
    commands: &[Command],
    set: &[bool],
    chords: &mut [Vec<String>],
    diagnostics: &mut Vec<String>,
) {
    let is_set = |ix: usize| set.get(ix).copied().unwrap_or_default();
    let mut held: Vec<(Option<&str>, String, usize)> = Vec::new();
    let order = (0..commands.len())
        .filter(|&ix| is_set(ix))
        .chain((0..commands.len()).filter(|&ix| !is_set(ix)));
    for ix in order {
        let (Some(command), Some(mine)) = (commands.get(ix), chords.get(ix)) else { continue };
        let mut kept = Vec::with_capacity(mine.len());
        for chord in mine {
            let holder = held.iter().find(|(context, held_chord, owner)| {
                held_chord == chord
                    && (command.contexts.contains(context)
                        || (is_set(*owner)
                            && !is_set(ix)
                            && command.contexts.iter().any(|inner| encloses(*context, *inner))))
            });
            match holder.map(|(_, _, owner)| *owner) {
                Some(owner) if is_set(owner) => {
                    let (Some(first), shown) = (commands.get(owner), label(chord)) else {
                        continue;
                    };
                    diagnostics.push(if is_set(ix) {
                        format!(
                            "{shown} is set for both `{}` and `{}`; `{}` keeps it",
                            first.key(),
                            command.key(),
                            first.key()
                        )
                    } else {
                        format!("{shown} runs `{}` now, no longer `{}`", first.key(), command.key())
                    });
                }
                _ => {
                    for context in command.contexts {
                        held.push((*context, chord.clone(), ix));
                    }
                    kept.push(chord.clone());
                }
            }
        }
        if let Some(slot) = chords.get_mut(ix) {
            *slot = kept;
        }
    }
}

/// Whether a binding in `inner` would win over one in `outer` wherever `outer`'s applies: the
/// app's (no context) holds everywhere, and the workspace's around every context drawn in its
/// window. A tile's window of its own is not inside the workspace.
fn encloses(outer: Option<&str>, inner: Option<&str>) -> bool {
    match (outer, inner) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(outer), Some(inner)) => {
            outer == inner || ([CTX, RING].contains(&Some(outer)) && Some(inner) != POP_OUT)
        }
    }
}

/// The app's own ⌘Q, ⌘H and ⌘⌥H for `quit`, `hide` and `hide_others`.
///
/// They are bound everywhere but in a remote view sending system shortcuts to its worker,
/// where they quit or hide the remote app ([`crate::screen::SYSTEM_KEYS_CTX`]). A click or ⌃Tab
/// out of the view gives them back.
pub fn app_chords(
    quit: impl Action,
    hide: impl Action,
    hide_others: impl Action,
) -> [KeyBinding; 3] {
    let here = format!("!{}", crate::screen::SYSTEM_KEYS_CTX);
    [
        KeyBinding::new("cmd-q", quit, Some(&here)),
        KeyBinding::new("cmd-h", hide, Some(&here)),
        KeyBinding::new("cmd-alt-h", hide_others, Some(&here)),
    ]
}

/// A chord as the palette shows it (`⇧⌘T`), else as written.
#[must_use]
pub fn label(chord: &str) -> String {
    gpui::Keystroke::parse(chord)
        .map_or_else(|_| chord.to_owned(), |k| crate::palette::keys_label(&k))
}

thread_local! {
    /// The keymap bound in GPUI on this thread, GPUI's.
    static CURRENT: RefCell<Option<Rc<Keymap>>> = const { RefCell::new(None) };
}

/// The keymap in effect: the one [`install`] bound last, else the table's defaults.
#[must_use]
pub fn current() -> Rc<Keymap> {
    CURRENT.with(|current| Rc::clone(current.borrow_mut().get_or_insert_with(Rc::default)))
}

/// Bind `keymap` in GPUI in place of the keymap bound before.
///
/// Every other binding (a text field's, the app menu's) stays, ahead of the keymap's, so the
/// table's chords that must win over a text field's (⌘⇧F in any field) still do.
pub fn install(keymap: Keymap, cx: &mut App) {
    let others: Vec<KeyBinding> =
        cx.key_bindings().borrow().bindings().filter(|b| b.meta() != Some(OURS)).cloned().collect();
    let released = released(&others);
    cx.clear_key_bindings();
    cx.bind_keys(others);
    cx.bind_keys(keymap.bindings(|_| true));
    cx.bind_keys(released);
    CURRENT.with(|current| *current.borrow_mut() = Some(Rc::new(keymap)));
}

/// gpui-kit's window root (`gpui_base::Root`), the context around every view. The kit binds
/// Tab and ⇧Tab there to walk the focus, and a copy of a selection: ⌘C on macOS and iOS, ⌃C
/// elsewhere.
const KIT_ROOT: &str = "Root";

/// The views that take every key the table leaves them: a terminal's program and a remote
/// window's worker.
const KEY_TAKERS: [&str; 2] = ["Terminal", "Screen"];

/// What gives the chords without ⌘ that `others` bind around every view back to the views
/// that take every key. GPUI runs a key's binding before any view hears the key, so gpui-kit's
/// Tab would walk the focus out of a shell and its ⌃C would copy instead of interrupting the
/// program. Each is unbound by its action's name inside [`KEY_TAKERS`] alone, so the kit's
/// fields and menus, and every chord with ⌘, keep theirs.
fn released(others: &[KeyBinding]) -> Vec<KeyBinding> {
    let Ok(root) = gpui::KeyContext::parse(KIT_ROOT) else { return Vec::new() };
    let around = [root];
    let mut out = Vec::new();
    for other in others {
        let [stroke] = other.keystrokes() else { continue };
        let stroke = stroke.inner();
        let action = other.action();
        if stroke.modifiers.platform
            || gpui::is_no_action(action)
            || gpui::is_unbind(action)
            || !other.predicate().is_none_or(|p| p.eval(&around))
        {
            continue;
        }
        let chord = stroke.unparse();
        let unbind = gpui::Unbind(action.name().into());
        for context in KEY_TAKERS {
            match binding(&chord, &unbind, Some(context)) {
                Ok(binding) => out.push(binding.with_meta(OURS)),
                Err(e) => tracing::error!(chord, context, error = %e, "release"),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
