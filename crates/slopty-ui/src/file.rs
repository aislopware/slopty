//! A file tile: a text file on the worker, open to edit.
//!
//! The item (`ItemKind::File`) names the path; the text is not in the registry. Each client
//! asks the worker for it (`ClientMsg::ReadFile`) when the tile appears, and the worker's watch
//! sends it again whenever the file changes on disk. The text sits in gpui-kit's code editor,
//! coloured by [`crate::highlight::editor`]. ⌘S sends it back whole (`ClientMsg::WriteFile`)
//! with the modification time the edit started from, so the worker refuses to write over a
//! change made meanwhile (`WriteResult::Conflict`); the tile then offers, inline, to reload the
//! disk's text or overwrite it.
//!
//! A change on disk while the tile is clean reloads it without a word, tints the lines that
//! changed and scrolls to the first (an agent's edit shows where it landed). While the tile is
//! dirty the same change marks the conflict instead, and the edit is kept.
//!
//! Any text file up to `FILE_BYTES` (16 MiB) is edited whole: a large one comes from the worker
//! on a bulk stream and goes back on one, which the link does out of sight. A file past the cap
//! says so and offers to open it in a terminal instead, in `$EDITOR` or `$PAGER`.
//!
//! A picture or a PDF is shown instead (`preview`): the worker knows one by its first bytes
//! and sends it whole, and the platform decodes it at the size the tile draws it.

mod compare;
pub mod decode;
pub mod edit;
mod editing;
pub mod find;
pub mod open_with;
mod preview;
mod reading;
mod search;
mod symbols;

use std::sync::atomic::{AtomicU64, Ordering};

use edit::{Format, Indent, Rules};
#[cfg(test)]
pub(crate) use editing::GO_TO_LINE;
pub use editing::{GO_TO_CTX, TEXT_CTX, palette_items as editor_palette_items};
use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle, Rope, RopeExt as _,
};
pub use preview::{NextScreen, PAGES_CTX, PreviousScreen, ScrollDown, ScrollUp};
pub use reading::{SHOW_PREVIEW, SHOW_SOURCE, is_markdown};
pub use search::SEARCH_CTX;
use slopty_client::layout::WorkerKey;
use slopty_client::unsaved::Unsaved;
use slopty_core::{ItemId, WallMs};
use slopty_proto::file::{FILE_BYTES, FileRead, WriteResult};
use slopty_proto::handoff::{EditOutcome, HandoffId};
use slopty_theme::{Theme, alpha};
pub use symbols::SYMBOLS_CTX;

use crate::authorship::{self, Authored, Opens};
use crate::colors::{hsla, hsla_alpha};
use crate::highlight::Syntax;
use crate::icons::{IconSize, Symbol};
use crate::kit::{ButtonKind, size_label};
use crate::terminal::Find;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        file,
        [
            /// Save the file tile's text to the worker.
            SaveFile,
            /// Answer the program waiting on this file tile: saved, and done with.
            FinishEdit,
            /// Comment the selected lines, or uncomment them, in the file's language.
            ToggleComment,
            /// Open the tile's "go to line" field.
            GoToLine,
            /// Close the "go to line" field, the caret back where it was.
            CloseGoToLine,
            /// Swap the selected lines with the one above.
            MoveLineUp,
            /// Swap the selected lines with the one below.
            MoveLineDown,
            /// Write the selected lines again below them.
            DuplicateLine,
            /// Put the caret on the bracket that pairs with the one at it.
            JumpToBracket,
            /// Wrap long lines at the tile's width, or stop.
            ToggleSoftWrap,
            /// Open the find bar with its replace field, or close the replace field.
            ToggleReplace,
            /// List the file's symbols to go to one.
            GoToSymbol,
            /// Close the symbol list, the caret back where it was.
            CloseSymbols,
            /// The next symbol in the list.
            NextSymbol,
            /// The previous symbol in the list.
            PreviousSymbol,
            /// Show a Markdown file's preview in place of its source, or back.
            TogglePreview,
        ]
    );
}
pub use actions::{
    CloseGoToLine, CloseSymbols, DuplicateLine, FinishEdit, GoToLine, GoToSymbol, JumpToBracket,
    MoveLineDown, MoveLineUp, NextSymbol, PreviousSymbol, SaveFile, ToggleComment, TogglePreview,
    ToggleReplace, ToggleSoftWrap,
};

/// The key context of a file tile; ⌘S is bound in it.
pub const CTX: &str = "FileEditor";
/// The tile's key context while it shows a PDF's pages: its own and the pages' ([`PAGES_CTX`]).
const PAGES_KEY_CONTEXT: &str = "FileEditor FilePages";

/// What a file tile's bar says when the disk changed under an edit.
pub(crate) const CHANGED_ON_DISK: &str = "Changed on disk";
/// The bar's way out that drops the edit.
pub(crate) const RELOAD: &str = "Reload";
/// The bar's way out that keeps the edit.
pub(crate) const OVERWRITE: &str = "Overwrite";
/// What a file tile's bar says of a file that is not on disk: not made yet, or deleted under a
/// tile that keeps its text.
pub(crate) const NOT_ON_DISK: &str = "Not on disk: saving makes it";
/// Text a file tile colours at most; a larger file is plain text.
///
/// The colours come from a parse of the whole text off the UI thread once typing pauses, about
/// 80–160 µs a line (MEASUREMENTS, 2026-09-28): 3 s for 20 000 lines and 30 s for 200 000.
/// A parse that has started runs to its end, so past this the pauses of ordinary typing would
/// keep several cores parsing.
pub const COLOURED_BYTES: usize = 2 << 20;
/// What a file tile says while its first read is out, once the wait is worth a word.
pub(crate) const READING: &str = "Reading…";
/// The body of a file tile not read yet while its machine is away.
pub(crate) const OPENS_WHEN_BACK: &str = "Opens when the machine is back";
/// What a file tile says of a file past the cap a tile holds.
pub(crate) const TOO_LARGE: &str = "Too large to open here";
/// What a file tile says of a file that is not text.
pub(crate) const NOT_TEXT: &str = "Not a text file";
/// What a file tile says of a file the worker could not read, over the reason.
pub(crate) const CANNOT_READ: &str = "Cannot read this file";
/// The way out of a file too large to edit here that opens it in `$EDITOR`.
pub(crate) const OPEN_IN_EDITOR: &str = "Open in editor";
/// The way out of a file too large to edit here that pages it.
pub(crate) const OPEN_IN_PAGER: &str = "Open in pager";
/// Why a save has no answer, after "Not saved: ".
pub(crate) const LINK_LOST: &str = "the link dropped before the worker answered";
/// What the bar of a file a program waits on says.
pub(crate) const PROGRAM_WAITS: &str = "A program is waiting for this file";
/// The bar's way to answer the waiting program once the file is saved.
pub(crate) const DONE: &str = "Done";
/// The bar's way to answer the waiting program without saving.
pub(crate) const GIVE_UP: &str = "Give up";

/// What a file tile tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileViewEvent {
    /// The find bar closed (Esc, ✕): the keyboard should go back to the editor.
    FindClosed,
    /// ⌘S or "Overwrite": write this text to the file.
    Save {
        /// The whole text, final newline included when the file had one.
        text: String,
        /// The version the edit started from; `None` overwrites whatever is there.
        base_modified_ms: Option<WallMs>,
    },
    /// "Reload": the edit is dropped; the file is to be read again.
    Reload,
    /// The person pressed who wrote a line: open the thread that did, at its turn.
    OpenThread(Opens),
    /// The editor holds the file as read anew ([`FileView::stamp`]): who wrote its lines is
    /// to be asked.
    Stamped,
    /// A file too large to edit here: run this shell line in a terminal on the file's worker
    /// ([`terminal_command`] makes the program to run it).
    Run(String),
    /// A fenced block's "Run" in a Markdown preview: type its code into the workspace's shell.
    RunBlock(String),
    /// The person is done with the file a program waits on (handoff `id`): saved and answered
    /// by the worker for [`EditOutcome::Done`], dropped for [`EditOutcome::Cancelled`].
    Edited {
        /// The handoff.
        id: HandoffId,
        /// How it ended.
        outcome: EditOutcome,
    },
}

impl EventEmitter<FileViewEvent> for FileView {}

/// A version of the file: what the worker sent, or what this tile saved.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    /// The text as the editor holds it: without the file's final newline, its BOM, or the
    /// `\r` of lines that all end `\r\n`.
    text: String,
    /// Whether the file ends with a newline, which the worker's text leaves off.
    newline: bool,
    /// How the file ends its lines and whether it has a BOM, put back on save.
    format: Format,
    /// Its modification time on disk.
    modified_ms: WallMs,
    /// What its `EditorConfig` asks.
    rules: Rules,
}

impl Version {
    /// The version a text read describes.
    fn of(
        text: &str,
        final_newline: bool,
        modified_ms: WallMs,
        editorconfig: &[(String, String)],
    ) -> Self {
        let rules = Rules::read(editorconfig);
        let (text, mut format) = Format::split(text, final_newline);
        if !final_newline && !text.contains('\n') {
            // No line break yet to follow: the first one is the EditorConfig's.
            format.crlf = rules.crlf.unwrap_or(format.crlf);
        }
        Self { text: text.into_owned(), newline: final_newline, format, modified_ms, rules }
    }

    /// A file that is not on disk yet: empty, ending with a newline once it has a line (or as
    /// its `EditorConfig` says), and based on the epoch, so its save makes it unless something
    /// made it meanwhile ([`FileRead::Absent`]).
    fn new_file(editorconfig: &[(String, String)]) -> Self {
        Self::of("", true, WallMs::ZERO, editorconfig)
    }

    /// The bytes the file holds for this text.
    fn file_text(&self, text: &str) -> String {
        self.format.join(text, self.newline)
    }
}

/// Why the tile's text and the file disagree, with the way out offered inline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trouble {
    /// The file changed on disk under an unsaved edit (or refused a save because it had).
    Conflict,
    /// The worker could not write the file; its word.
    Failed(String),
}

/// Numbers every change to a tile's text across all tiles, so of two tiles' edits to one file
/// the later is known. Zero is an edit kept from before the app started.
static EDITS: AtomicU64 = AtomicU64::new(0);

fn next_edit() -> u64 {
    EDITS.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
}

/// Where a tile's unsaved edit stands, told apart without reading its text: the backup of it
/// is behind when this differs from the one it was written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// The text's last change, numbered across all tiles; of two edits to one file, the later
    /// has the larger.
    pub edit: u64,
    /// The file ends with a newline.
    pub newline: bool,
    /// The modification time of the version the edit is based on.
    pub base_modified_ms: Option<WallMs>,
    /// The disk moved on under the edit.
    pub conflict: bool,
}

/// A tile's unsaved edit as its backup keeps it, taken without copying the text: the editor's
/// rope is shared in O(1), and read out into a string off the UI thread ([`Self::unsaved`]).
#[derive(Clone)]
pub struct Backup {
    /// The worker the file is on.
    pub worker: WorkerKey,
    /// The file tile it was typed in.
    pub item: ItemId,
    /// Its path there.
    pub path: String,
    /// The editor's text, without the final newline.
    pub text: Rope,
    /// Where the edit stands.
    pub mark: Mark,
}

impl std::fmt::Debug for Backup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backup")
            .field("worker", &self.worker)
            .field("item", &self.item)
            .field("path", &self.path)
            .field("bytes", &self.text.len())
            .field("mark", &self.mark)
            .finish()
    }
}

impl Backup {
    /// A backup read back from the store: an edit from before this run ([`Mark::edit`] zero).
    #[must_use]
    pub fn kept(unsaved: &Unsaved) -> Self {
        Self {
            worker: unsaved.worker,
            item: unsaved.item,
            path: unsaved.path.clone(),
            text: Rope::from_str(&unsaved.text),
            mark: Mark {
                edit: 0,
                newline: unsaved.newline,
                base_modified_ms: unsaved.base_modified_ms,
                conflict: unsaved.conflict,
            },
        }
    }

    /// What the store writes, the text read out whole: kept at `kept_ms`.
    #[must_use]
    pub fn unsaved(&self, kept_ms: WallMs) -> Unsaved {
        Unsaved {
            worker: self.worker,
            item: self.item,
            path: self.path.clone(),
            text: self.text.to_string(),
            newline: self.mark.newline,
            base_modified_ms: self.mark.base_modified_ms,
            conflict: self.mark.conflict,
            kept_ms,
        }
    }
}

/// A program waiting on the tile's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Waiting {
    /// Its handoff.
    id: HandoffId,
    /// "Done" was asked: it is answered once the text is on disk.
    finishing: bool,
}

/// Why a file cannot be edited here: it is past the cap a tile holds. The notice's two lines,
/// said as one.
#[must_use]
pub fn too_large_reason(size: u64) -> String {
    format!("{TOO_LARGE}: {}", too_large_detail(size))
}

/// How far past the cap a file is, under [`TOO_LARGE`].
fn too_large_detail(size: u64) -> String {
    format!("{}, over the {} a tile opens", size_label(size), size_label(FILE_BYTES))
}

/// The program that runs `line` in the worker user's login shell, as their terminal would: the
/// rc files set `$EDITOR`, `$PAGER` and `PATH`.
#[must_use]
pub fn terminal_command(line: &str) -> Vec<String> {
    ["/bin/sh", "-c", r#"exec "${SHELL:-/bin/sh}" -lic "$1""#, "sh", line].map(str::to_owned).into()
}

/// The shell line that pages `path` (`$PAGER`, else `less`).
#[must_use]
pub fn pager_command(path: &str) -> String {
    format!("${{PAGER:-less}} {}", crate::terminal::url::shell_word(path))
}

/// The view of one file item.
pub struct FileView {
    id: ItemId,
    /// The worker the file is on.
    worker: WorkerKey,
    path: String,
    /// What the worker last said, `None` until it answers.
    read: Option<FileRead>,
    /// The worker's link is down: a tile not read yet says it waits for the machine, not that
    /// it is reading.
    away: bool,
    /// The version the edit started from, once there is text.
    base: Option<Version>,
    /// The text sent to be written, until the worker answers.
    saving: Option<Version>,
    /// What stops a save, shown as a line under the header with its way out.
    trouble: Option<Trouble>,
    /// What the disk has under a conflicting edit, when it was read: what "Compare" shows the
    /// edit against.
    disk: Option<Version>,
    /// The comparison of the disk's text and the edit, while the tile shows it.
    comparing: Option<compare::Comparing>,
    /// The next read replaces the text whatever the edit ("Reload" was pressed).
    discard: bool,
    /// Why the file cannot be edited here (past the cap); none when it can.
    read_only: Option<String>,
    editor: Entity<EditorState>,
    /// The keyboard's place while the body shows no editor (a file not text, too large, not
    /// readable, or not read yet), so the workspace's keys and the palette still reach the
    /// tile; the editor takes it over once it is drawn.
    focus_handle: FocusHandle,
    /// Text the editor takes at the next frame: replacing it needs the window, which a
    /// worker's message does not come with.
    pending_text: Option<String>,
    /// A line (0-based) for the caret once the text is in.
    pending_line: Option<usize>,
    /// Whether the editor holds something other than the base.
    dirty: bool,
    /// Lines (0-based) the last reload changed; empty on a first read and after an edit.
    changed: Vec<usize>,
    /// The tints over the changed lines and the find's hits.
    marks: Option<RangeDecorationCollection>,
    /// The line the tile was opened at: an edit's place in the file.
    focus: Option<usize>,
    zoom: f32,
    /// Inner padding at zoom 1 (the theme's base spacing).
    pad: f32,
    /// Text size at zoom 1 (the theme's mono size).
    text_size: f32,
    theme: Theme,
    /// The find bar, while open.
    search: Option<search::FileSearch>,
    /// The program in a shell that waits for this file (`$EDITOR` handed it here), until the
    /// person is done with it or the program goes away.
    waiting: Option<Waiting>,
    /// An edit kept from before the app last ended, laid over the first read
    /// ([`Self::restore`]).
    restoring: Option<Backup>,
    /// The text's last change, numbered across all tiles ([`Mark::edit`]).
    edit: u64,
    /// The grammar the path (or first line) names; none for a file the bundle cannot colour.
    syntax: Option<Syntax>,
    /// The picture or PDF the file is, while it is one.
    preview: Option<preview::Preview>,
    /// A Markdown file's preview; none for any other file.
    reading: Option<reading::Reading>,
    /// The workspace has a shell for a fenced block's "Run".
    can_run: bool,
    /// How the file indents, read from its text at each read; Tab follows it.
    indent: Indent,
    /// Long lines wrap at the tile's width (on for prose).
    wrap: bool,
    /// Whether the editor wraps now; it is told at the next frame, which has the window.
    wrapped: bool,
    /// The bracket pair at the caret, opening first, tinted while the caret is at it.
    bracket: Option<(usize, usize)>,
    /// The caret and the edit the pair was found for: it is looked for again when either moves.
    bracket_at: Option<(usize, u64)>,
    /// The "go to line" field, while open.
    goto: Option<editing::GoTo>,
    /// The symbol list, while open.
    symbols: Option<symbols::Symbols>,
    /// Who wrote the file's lines as last read, while the worker has said
    /// ([`Self::set_authored`]).
    authored: Option<Authored>,
    /// [`Self::stamp`] as the host was last told it.
    stamped: Option<WallMs>,
    /// The editor's events, and its every change marking this view dirty: the tile draws
    /// this view from GPUI's view cache, which a change inside the editor would not otherwise
    /// invalidate.
    _editor_events: [Subscription; 2],
}

impl std::fmt::Debug for FileView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileView")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("dirty", &self.dirty)
            .field("trouble", &self.trouble)
            .finish_non_exhaustive()
    }
}

impl FileView {
    /// A tile for `path` on `worker`, waiting on the worker.
    pub fn new(
        id: ItemId,
        worker: WorkerKey,
        path: &str,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .folding(false)
                .soft_wrap(false)
                .line_number_gap(px(theme.spacing.md));
            state.set_searchable(false, cx);
            state
        });
        let events = cx.subscribe(&editor, |this, _editor, event, cx| match event {
            InputEvent::Change => this.edited(cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        });
        let redraw = cx.observe(&editor, |_this, _editor, cx| cx.notify());
        Self {
            id,
            worker,
            path: path.to_owned(),
            read: None,
            away: false,
            base: None,
            saving: None,
            trouble: None,
            disk: None,
            comparing: None,
            discard: false,
            read_only: None,
            editor,
            focus_handle: cx.focus_handle(),
            pending_text: None,
            pending_line: None,
            dirty: false,
            changed: Vec::new(),
            marks: None,
            focus: None,
            zoom: 1.0,
            pad: 8.0,
            text_size: 13.0,
            theme,
            search: None,
            waiting: None,
            restoring: None,
            edit: 0,
            syntax: None,
            preview: None,
            reading: is_markdown(path).then(reading::Reading::new),
            can_run: false,
            indent: Indent::DEFAULT,
            wrap: false,
            wrapped: false,
            bracket: None,
            bracket_at: None,
            goto: None,
            symbols: None,
            authored: None,
            stamped: None,
            _editor_events: [events, redraw],
        }
    }

    /// When the file the editor holds was last changed on disk, while the editor holds just
    /// that: who wrote its lines is asked of the file as it was then.
    #[must_use]
    pub fn stamp(&self) -> Option<WallMs> {
        let base = self.base.as_ref().filter(|_| !self.dirty && self.comparing.is_none())?;
        (!base.modified_ms.is_zero()).then_some(base.modified_ms)
    }

    /// Who wrote the file's lines as [`Self::stamp`] read it; `None` while not known.
    pub fn set_authored(&mut self, authored: Option<Authored>, cx: &mut Context<Self>) {
        if self.authored != authored {
            self.authored = authored;
            cx.notify();
        }
    }

    /// Who wrote the file's lines, as last set.
    #[must_use]
    pub const fn authored(&self) -> Option<&Authored> {
        self.authored.as_ref()
    }

    /// In the corner of the text: who wrote the caret's line, when a thread did and the editor
    /// holds the file as it was read. A press opens that thread at the turn.
    fn render_author(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let authored = self.authored.as_ref()?;
        let stamp = self.stamp()?;
        if authored.authors.modified_ms != Some(stamp) || !self.shows_text() {
            return None;
        }
        let run = authored.at(self.reading_line(cx)?)?;
        let writer = authored.writers.get(&run.thread);
        let theme = &self.theme;
        let opens = Opens { thread: run.thread, turn: run.turn };
        let id = SharedString::from(format!("file-author-{}", self.id.as_uuid()));
        let mut tag = authorship::tag(theme, id, run, writer, None, crate::clock::now(cx));
        if writer.is_some() {
            tag = tag.on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.emit(FileViewEvent::OpenThread(opens));
            }));
        }
        let pill = crate::kit::elevate(crate::kit::pill_frame(theme, 1.0), theme)
            .debug_selector(|| "file-author".to_owned())
            .px(px(theme.spacing.xxs))
            .child(tag);
        Some(
            div()
                .absolute()
                .bottom(px(theme.spacing.md))
                .right(px(theme.spacing.lg))
                .child(pill)
                .into_any_element(),
        )
    }

    /// Item this tile belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// The path on the worker.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the worker last said, once it has.
    #[must_use]
    pub const fn read(&self) -> Option<&FileRead> {
        self.read.as_ref()
    }

    /// The editor's text as it stands (the text about to arrive, while one is pending).
    #[must_use]
    pub fn text(&self, cx: &gpui::App) -> String {
        self.pending_text.clone().unwrap_or_else(|| self.editor.read(cx).value().to_string())
    }

    /// Lines in the editor.
    #[must_use]
    pub fn line_count(&self, cx: &gpui::App) -> usize {
        match &self.pending_text {
            Some(text) => text.split('\n').count(),
            None => self.editor.read(cx).text().lines_len(),
        }
    }

    /// Whether the editor holds an edit not yet saved.
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty
    }

    /// Whether a save is waiting on the worker.
    #[must_use]
    pub const fn saving(&self) -> bool {
        self.saving.is_some()
    }

    /// What stops a save, if anything.
    #[must_use]
    pub const fn trouble(&self) -> Option<&Trouble> {
        self.trouble.as_ref()
    }

    /// Why the file cannot be edited here, when it cannot: it is past the cap.
    #[must_use]
    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// The lines the last reload changed (0-based).
    #[must_use]
    pub fn changed(&self) -> &[usize] {
        &self.changed
    }

    /// The grammar's name ("Rust"), for a file the bundle can colour.
    #[must_use]
    pub fn coloured_as(&self) -> Option<&'static str> {
        self.syntax.map(Syntax::name)
    }

    /// The editor, for tests and the self-test socket.
    #[must_use]
    pub const fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// Whether the body holds the text in its editor, rather than a notice saying why not (a
    /// file too large, not text, not readable) or nothing yet.
    #[must_use]
    pub const fn shows_text(&self) -> bool {
        self.base.is_some() || matches!(self.read, Some(FileRead::Text { .. }))
    }

    /// The caret's line and column, 1-based: what the status bar says of a focused file.
    #[must_use]
    pub fn caret(&self, cx: &gpui::App) -> (u32, u32) {
        let at = self.editor.read(cx).cursor_position();
        (at.line.saturating_add(1), at.character.saturating_add(1))
    }

    /// Open the file in the person's own editor at the caret's line, or the line in view while
    /// the body shows no text ([`open_with`]).
    fn open_in_editor(&self, cx: &gpui::App) {
        let line = if self.shows_text() { self.reading_line(cx) } else { self.focus_line_number() };
        if let Some(opening) = open_with::opening(self.worker, &self.path, line, cx) {
            open_with::open(&opening, cx);
        }
    }

    /// The caret's line, 1-based.
    #[must_use]
    pub fn reading_line(&self, cx: &gpui::App) -> Option<u32> {
        let line = self.pending_line.unwrap_or_else(|| {
            usize::try_from(self.editor.read(cx).cursor_position().line).unwrap_or(0)
        });
        u32::try_from(line.saturating_add(1)).ok()
    }

    /// Give the tile the keyboard: the editor when it is drawn, else the tile itself.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shows_text() && !self.previewing() {
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Whether the tile has the keyboard, in its editor, its find bar or itself.
    #[must_use]
    pub fn focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus_handle.contains_focused(window, cx)
    }

    /// Keep the keyboard on what is drawn: into the editor once text arrives for a tile that
    /// held it, and back to the tile when the text goes (the file turned binary or went away).
    fn settle_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = gpui::Focusable::focus_handle(self.editor.read(cx), cx);
        let editing = self.shows_text() && !self.previewing();
        if editing && self.focus_handle.is_focused(window) {
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        } else if !editing && editor.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Land the caret on `line` (1-based, as a tool names it) and scroll there, now if the
    /// text is here, else when it arrives. `None` leaves the caret.
    pub fn focus_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        self.focus = line.and_then(|l| usize::try_from(l).ok()).map(|l| l.saturating_sub(1));
        if self.focus.is_some() {
            self.pending_line = self.focus;
            // A line is a place in the source.
            if let Some(reading) = self.reading.as_mut() {
                reading.keep_source();
            }
        }
        cx.notify();
    }

    /// The editor's text changed by a keystroke (a replace from here emits nothing).
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.edit = next_edit();
        let dirty = self.base.as_ref().is_some_and(|base| {
            // Most keystrokes change the length, which settles it without reading the text.
            let text = self.editor.read(cx).text();
            text.len() != base.text.len() || *text != *base.text
        });
        if !self.changed.is_empty() {
            // The tint said what the last reload did; once the human edits, it says nothing.
            self.changed.clear();
            self.remark(cx);
        }
        if self.trouble.as_ref().is_some_and(|t| matches!(t, Trouble::Failed(_))) {
            self.trouble = None;
        }
        if self.search.is_some() {
            self.refresh_hits(cx);
        }
        if dirty != self.dirty {
            self.dirty = dirty;
        }
        cx.notify();
    }

    /// The worker read the file (the first time, after a change on disk, or on "Reload").
    pub fn set_read(&mut self, read: FileRead, cx: &mut Context<Self>) {
        self.take_read(read, cx);
        self.restamp(cx);
    }

    /// Tell the host when the file the editor holds is another than it was
    /// ([`FileViewEvent::Stamped`]): who wrote its lines is asked again.
    fn restamp(&mut self, cx: &mut Context<Self>) {
        let stamp = self.stamp();
        if stamp != self.stamped {
            self.stamped = stamp;
            if stamp.is_some() {
                cx.emit(FileViewEvent::Stamped);
            }
        }
    }

    fn take_read(&mut self, read: FileRead, cx: &mut Context<Self>) {
        self.away = false;
        if !matches!(read, FileRead::Streamed { .. })
            && let Some(kept) = self.restoring.take()
        {
            self.restore_over(&read, &kept, cx);
            // An edit kept unsaved is gone on with in the source.
            if let Some(reading) = self.reading.as_mut() {
                reading.keep_source();
            }
            self.read = Some(read);
            cx.notify();
            return;
        }
        match &read {
            FileRead::Text { text, modified_ms, final_newline, editorconfig, .. } => {
                let incoming = Version::of(text, *final_newline, *modified_ms, editorconfig);
                self.read_only = None;
                self.preview = None;
                self.take_version(incoming, cx);
            }
            FileRead::Absent { editorconfig } => {
                self.read_only = None;
                self.preview = None;
                let fresh = Version::new_file(editorconfig);
                match self.base.take() {
                    // Deleted under a clean tile: its text stays, unsaved, and a save makes the
                    // file again with the line endings it had.
                    Some(was)
                        if !self.dirty && !self.discard && was.modified_ms != WallMs::ZERO =>
                    {
                        self.dirty = !was.text.is_empty();
                        self.base =
                            Some(Version { newline: was.newline, format: was.format, ..fresh });
                        self.settle_conflict();
                    }
                    base => {
                        self.base = base;
                        self.take_version(fresh, cx);
                    }
                }
            }
            // The link hands on the text it announces, never the announcement.
            FileRead::Streamed { .. } => return,
            FileRead::Binary { .. }
            | FileRead::Missing { .. }
            | FileRead::TooLarge { .. }
            | FileRead::Media { .. } => {
                if let FileRead::Media { media_type, bytes, .. } = &read {
                    self.show_media(media_type, bytes, cx);
                }
                self.read_only = match &read {
                    FileRead::TooLarge { size } => Some(too_large_reason(*size)),
                    _ => None,
                };
                if self.dirty && !self.discard {
                    // Gone or turned binary under an edit: the edit stays, "Overwrite" puts
                    // it back.
                    self.trouble = Some(Trouble::Conflict);
                } else {
                    self.base = None;
                    self.dirty = false;
                    self.discard = false;
                    self.trouble = None;
                }
            }
        }
        self.read = Some(read);
        self.settle_preview(cx);
        cx.notify();
    }

    /// A text version arrived: taken silently, kept as the new base under an edit that
    /// already matches it, or marked as a conflict with the edit.
    fn take_version(&mut self, incoming: Version, cx: &mut Context<Self>) {
        let current = self.text(cx);
        let clean = !self.dirty || self.discard;
        if clean {
            if self.base.as_ref() != Some(&incoming) || self.discard {
                self.replace(incoming, cx);
            }
            return;
        }
        let ours = self.saving.as_ref().is_some_and(|s| s.text == incoming.text);
        if current == incoming.text || ours {
            // The disk has what this tile has (or what it just sent): nothing to settle.
            self.dirty = current != incoming.text;
            self.base = Some(incoming);
            self.settle_conflict();
            return;
        }
        let stale = self
            .base
            .as_ref()
            .is_some_and(|b| b.text == incoming.text && incoming.modified_ms <= b.modified_ms);
        if !stale {
            self.trouble = Some(Trouble::Conflict);
        }
        self.disk = Some(incoming);
        self.refresh_compare(cx);
    }

    /// Nothing is in the way of a save any more: the conflict, and what it compared, go.
    fn settle_conflict(&mut self) {
        self.trouble = None;
        self.disk = None;
        self.comparing = None;
    }

    /// Lay `kept`, an edit from before the app last ended, over the first read: the tile holds
    /// it unsaved, based on what the disk has now. When the disk moved on since the edit
    /// started (or was already said to), or no longer holds text, it is a conflict: "Reload"
    /// takes the disk's, "Overwrite" the edit.
    fn restore_over(&mut self, read: &FileRead, kept: &Backup, cx: &mut Context<Self>) {
        let disk = match read {
            FileRead::Text { text, modified_ms, final_newline, editorconfig, .. } => {
                Version::of(text, *final_newline, *modified_ms, editorconfig)
            }
            FileRead::Absent { editorconfig } => Version::new_file(editorconfig),
            _ => Version {
                text: String::new(),
                newline: kept.mark.newline,
                format: Format::default(),
                modified_ms: WallMs::ZERO,
                rules: Rules::default(),
            },
        };
        let text_read = matches!(read, FileRead::Text { .. } | FileRead::Absent { .. });
        if text_read && kept.text == *disk.text {
            tracing::info!(path = %self.path, "a kept edit the disk already has");
            self.replace(disk, cx);
            return;
        }
        let Mark { conflict, base_modified_ms, .. } = kept.mark;
        tracing::info!(path = %self.path, conflict, "an unsaved edit restored");
        let moved =
            !text_read || conflict || base_modified_ms.unwrap_or(WallMs::ZERO) != disk.modified_ms;
        self.replace(disk, cx);
        self.pending_text = Some(kept.text.to_string());
        self.pending_line = self.focus;
        self.changed.clear();
        self.dirty = true;
        self.edit = kept.mark.edit;
        if moved {
            self.trouble = Some(Trouble::Conflict);
        }
    }

    /// Lay `kept`, an edit from before the app last ended, over the file: at once when the
    /// first read is in, else when it comes ([`Self::set_read`]). A tile already edited keeps
    /// its own edit, the later one.
    pub fn restore(&mut self, kept: Backup, cx: &mut Context<Self>) {
        if self.dirty {
            return;
        }
        match self.read.take() {
            Some(read) if !matches!(read, FileRead::Streamed { .. }) => {
                self.restore_over(&read, &kept, cx);
                self.read = Some(read);
                cx.notify();
            }
            read => {
                self.read = read;
                self.restoring = Some(kept);
            }
        }
    }

    /// The worker the file is on.
    #[must_use]
    pub const fn worker(&self) -> WorkerKey {
        self.worker
    }

    /// Where the unsaved edit stands, while there is one not on disk; `None` when the tile is
    /// clean, or has no text yet. Reads no text: a pass over every tile costs nothing per byte.
    #[must_use]
    pub fn backup_mark(&self) -> Option<Mark> {
        if let Some(kept) = &self.restoring {
            return Some(kept.mark);
        }
        let base = self.base.as_ref().filter(|_| self.dirty)?;
        Some(Mark {
            edit: self.edit,
            newline: base.newline,
            base_modified_ms: (base.modified_ms != WallMs::ZERO).then_some(base.modified_ms),
            conflict: self.trouble == Some(Trouble::Conflict),
        })
    }

    /// The edit as a backup keeps it, while there is one not on disk: the whole text, the
    /// final newline, the version it started from, and whether the disk moved on under it.
    /// One still waiting to be laid over the first read is that edit. The text is shared with
    /// the editor, not copied.
    #[must_use]
    pub fn backup(&self, cx: &gpui::App) -> Option<Backup> {
        if let Some(kept) = &self.restoring {
            return Some(kept.clone());
        }
        let mark = self.backup_mark()?;
        let text = match &self.pending_text {
            Some(text) => Rope::from_str(text),
            None => self.editor.read(cx).text().clone(),
        };
        Some(Backup { worker: self.worker, item: self.id, path: self.path.clone(), text, mark })
    }

    /// The text becomes `incoming`, the edit (if any) dropped.
    fn replace(&mut self, incoming: Version, cx: &mut Context<Self>) {
        let reload = self.base.is_some();
        let old = self.base.as_ref().map(|b| b.text.clone()).unwrap_or_default();
        self.changed =
            if reload && !self.discard { changed_lines(&old, &incoming.text) } else { Vec::new() };
        // A changed line is the reason for this read; the opening line is where a first
        // text lands.
        self.pending_line =
            self.changed.first().copied().or(if reload { None } else { self.focus });
        let first = incoming.text.split('\n').next().unwrap_or_default();
        self.syntax = if incoming.text.len() <= COLOURED_BYTES {
            Syntax::for_path(&self.path, first)
        } else {
            None
        };
        let (indent, prose) = editing::opening_layout(&incoming.text, self.syntax);
        self.indent = incoming.rules.indent(indent);
        if !reload {
            // A reload keeps what the person chose.
            self.wrap = prose;
        }
        self.pending_text = Some(incoming.text.clone());
        self.base = Some(incoming);
        self.dirty = false;
        self.discard = false;
        self.settle_conflict();
        // A save still out was of the text just dropped: its answer, if one comes, is about
        // nothing this tile holds, and must not keep ⌘S, "Overwrite" and "Reload" waiting.
        self.saving = None;
        cx.notify();
    }

    /// The worker's link dropped, or was down when the tile opened. A save that went out on it
    /// stops waiting and says the edit may not be on disk, so ⌘S can send it again once the
    /// worker is back; a tile not read yet says it opens then. The next read ends it.
    pub fn link_lost(&mut self, cx: &mut Context<Self>) {
        if !self.away {
            self.away = true;
            cx.notify();
        }
        if self.saving.take().is_some() {
            tracing::info!(path = %self.path, "save unanswered: link lost");
            self.trouble = Some(Trouble::Failed(LINK_LOST.to_owned()));
            self.settle_finish(cx);
            cx.notify();
        }
    }

    /// A program in a shell waits on this file (handoff `id`, which `$EDITOR` handed here), or
    /// no longer does (`None`: it went away, and the tile stays an ordinary file tile).
    pub fn set_waiting(&mut self, id: Option<HandoffId>, cx: &mut Context<Self>) {
        if self.waiting.map(|w| w.id) != id {
            self.waiting = id.map(|id| Waiting { id, finishing: false });
            cx.notify();
        }
    }

    /// The handoff of the program waiting on this file, if one does.
    #[must_use]
    pub fn waiting(&self) -> Option<HandoffId> {
        self.waiting.map(|w| w.id)
    }

    /// Whether "Done" was asked and the tile is waiting on its save to answer the program.
    #[must_use]
    pub fn finishing(&self) -> bool {
        self.waiting.is_some_and(|w| w.finishing)
    }

    /// "Done" (⌘↩), or the tile closing: save the edit and, once the worker has written it,
    /// tell the waiting program ([`FileViewEvent::Edited`]). A conflict is settled first
    /// ("Reload" or "Overwrite"), and a save that fails leaves the program waiting.
    pub fn finish_edit(&mut self, cx: &mut Context<Self>) {
        if self.trouble == Some(Trouble::Conflict) {
            return;
        }
        let Some(waiting) = self.waiting.as_mut() else { return };
        waiting.finishing = true;
        self.save(cx);
        self.settle_finish(cx);
    }

    /// "Give up": tell the waiting program the edit is dropped, without saving (`git commit`
    /// then aborts). The text stays in the tile as it is.
    pub fn give_up(&mut self, cx: &mut Context<Self>) {
        let Some(waiting) = self.waiting.take() else { return };
        cx.emit(FileViewEvent::Edited { id: waiting.id, outcome: EditOutcome::Cancelled });
        cx.notify();
    }

    /// After "Done", each time a save settles: answer the program once the disk has the text,
    /// save again what was typed while the last save was out, and stop finishing when a save
    /// did not land, so the person settles it and asks again.
    fn settle_finish(&mut self, cx: &mut Context<Self>) {
        let Some(waiting) = self.waiting.filter(|w| w.finishing) else { return };
        if self.saving.is_some() {
            return;
        }
        if self.trouble.is_some() {
            self.waiting = Some(Waiting { finishing: false, ..waiting });
            return;
        }
        if self.dirty || self.is_new() {
            self.save(cx);
            return;
        }
        self.waiting = None;
        cx.emit(FileViewEvent::Edited { id: waiting.id, outcome: EditOutcome::Done });
        cx.notify();
    }

    /// Whether the file is not on disk yet: a save makes it, edited or not.
    #[must_use]
    pub fn is_new(&self) -> bool {
        matches!(self.read, Some(FileRead::Absent { .. }))
            && self.base.as_ref().is_some_and(|b| b.modified_ms == WallMs::ZERO)
    }

    /// ⌘S: send the edit, based on the version it started from. Nothing when there is
    /// nothing to save (a file not on disk yet always has: itself), a save is already out, or
    /// a conflict is waiting for "Reload" or "Overwrite".
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let unsaved = self.dirty || self.is_new();
        if !unsaved || self.saving.is_some() || self.trouble == Some(Trouble::Conflict) {
            return;
        }
        let Some(base) = self.base.clone() else { return };
        self.send(Some(base.modified_ms), cx);
    }

    /// "Overwrite": write the edit over whatever the disk has now.
    pub fn overwrite(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_some() {
            return;
        }
        self.trouble = None;
        self.send(None, cx);
    }

    fn send(&mut self, base_modified_ms: Option<WallMs>, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let (newline, format, rules) =
            self.base.as_ref().map(|b| (b.newline, b.format, b.rules)).unwrap_or_default();
        // A new file left empty is made empty, as `touch` makes it: no line, so none to end.
        let newline = rules.final_newline.unwrap_or(newline) && !(text.is_empty() && self.is_new());
        let modified_ms = base_modified_ms.unwrap_or(WallMs::ZERO);
        let sent = Version { text, newline, format, modified_ms, rules };
        let file = sent.file_text(&sent.text);
        tracing::debug!(path = %self.path, bytes = file.len(), ?base_modified_ms, "save file");
        self.saving = Some(sent);
        cx.emit(FileViewEvent::Save { text: file, base_modified_ms });
        cx.notify();
    }

    /// Before the person's save (⌘S, "Done", "Overwrite"), the trailing whitespace off the lines
    /// the edit touched, as one edit in the editor, so the tile holds what goes to disk. On
    /// unless the `EditorConfig` says `trim_trailing_whitespace = false`; off in Markdown, where
    /// two spaces end a line, unless it says `true`. A save the tile makes on its own (a tile
    /// closed on a waiting program) sends the text as it stands.
    pub fn trim_touched(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dirty {
            return;
        }
        let Some(base) = &self.base else { return };
        let prose = self.syntax.is_some_and(editing::is_prose);
        if !base.rules.trim.unwrap_or(!prose) {
            return;
        }
        let text = self.text(cx);
        let touched = changed_lines(&base.text, &text);
        let edit = {
            let editor = self.editor.read(cx);
            edit::trim_lines(editor.text(), &touched, &editor.selected_range())
        };
        if let Some(edit) = edit {
            self.apply(edit, window, cx);
        }
    }

    /// "Reload": drop the edit and take the disk's text.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.discard = true;
        self.trouble = None;
        cx.emit(FileViewEvent::Reload);
        cx.notify();
    }

    /// The worker answered a save.
    pub fn written(&mut self, result: WriteResult, cx: &mut Context<Self>) {
        self.take_written(result, cx);
        self.restamp(cx);
    }

    fn take_written(&mut self, result: WriteResult, cx: &mut Context<Self>) {
        let Some(sent) = self.saving.take() else { return };
        match result {
            WriteResult::Saved { modified_ms, .. } => {
                self.dirty = self.text(cx) != sent.text;
                self.base = Some(Version { modified_ms, ..sent });
                self.settle_conflict();
            }
            WriteResult::Conflict { modified_ms } => {
                tracing::info!(path = %self.path, ?modified_ms, "save refused: changed on disk");
                self.trouble = Some(Trouble::Conflict);
            }
            WriteResult::Failed { error } => {
                tracing::warn!(path = %self.path, %error, "save failed");
                self.trouble = Some(Trouble::Failed(error));
            }
        }
        self.settle_finish(cx);
        cx.notify();
    }

    /// Paint the tints: the lines the last reload changed, the find's matches and the bracket
    /// pair at the caret.
    fn remark(&mut self, cx: &mut Context<Self>) {
        let s = &self.theme.surfaces;
        let tint = hsla_alpha(s.success, alpha::FAINT);
        let found = self.search_marks();
        let changed = self.changed.clone();
        let brackets = self.bracket_marks();
        let marks = self.editor.update(cx, |e, _cx| {
            let text = e.text();
            let line = |row: usize, color| {
                let start = text.line_start_offset(row);
                RangeDecoration::new(start..text.line_end_offset(row).max(start.saturating_add(1)))
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color)
            };
            let mut marks: Vec<RangeDecoration> = changed
                .iter()
                .filter(|row| **row < text.lines_len())
                .map(|row| line(*row, tint))
                .collect();
            marks.extend(found.into_iter().filter(|m| m.range().end <= text.len()));
            marks.extend(brackets.into_iter().filter(|m| m.range().end <= text.len()));
            marks
        });
        match &self.marks {
            Some(collection) => collection.set(marks, cx),
            None => {
                self.marks = Some(
                    self.editor
                        .update(cx, |e, cx| e.create_range_decorations_collection(marks, cx)),
                );
            }
        }
    }

    /// Paint scale (the workspace's zoom) and the theme's inset and type size at scale 1.
    pub const fn set_layout(&mut self, zoom: f32, pad: f32, text_size: f32) {
        self.zoom = zoom;
        self.pad = pad;
        self.text_size = text_size;
    }

    /// Draw by another theme (the workspace swapped it).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.theme = theme;
        self.install_highlighter(cx);
        self.remark(cx);
        cx.notify();
    }

    /// The colours for this file's grammar in this theme.
    fn install_highlighter(&self, cx: &mut Context<Self>) {
        let factory = crate::highlight::editor::factory(self.syntax, self.theme.clone());
        let language = self.syntax.map_or_else(String::new, |s| s.name().to_lowercase());
        self.editor.update(cx, |e, cx| {
            e.set_highlighter_factory(factory, cx);
            e.set_highlighter(language, cx);
        });
    }

    /// Apply what waited for a window: new text, then the caret's line.
    fn apply_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.wrap != self.wrapped {
            let wrap = self.wrap;
            self.wrapped = wrap;
            self.editor.update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        }
        if let Some(text) = self.pending_text.take() {
            let caret = self.editor.read(cx).cursor();
            let scroll = self.editor.read(cx).scroll_offset();
            let reload = self.pending_line.is_none();
            self.editor.update(cx, |e, cx| {
                e.set_value(text, window, cx);
                if reload {
                    // A silent reload keeps the reader where they were.
                    let at = caret.min(e.text().len());
                    e.set_selected_range(at..at, cx);
                    e.set_scroll_offset(scroll, cx);
                }
            });
            self.apply_indent(cx);
            self.install_highlighter(cx);
            self.remark(cx);
            if self.search.is_some() {
                self.refresh_hits(cx);
            }
        }
        if let Some(line) = self.pending_line.take() {
            self.editor.update(cx, |e, cx| {
                let text = e.text();
                let at = text.line_start_offset(line.min(text.lines_len().saturating_sub(1)));
                e.set_selected_range(at..at, cx);
            });
        }
    }

    /// One line about the file for a screen reader: "212 lines, edited", "binary, 1.2 MB",
    /// "missing: No such file".
    #[must_use]
    pub fn summary(&self, cx: &gpui::App) -> String {
        match &self.read {
            None if self.away => OPENS_WHEN_BACK.to_owned(),
            None | Some(FileRead::Streamed { .. }) => READING.to_owned(),
            Some(FileRead::Text { .. } | FileRead::Absent { .. }) => {
                let n = self.line_count(cx);
                let mut parts =
                    vec![if n == 1 { "1 line".to_owned() } else { format!("{n} lines") }];
                parts.extend(self.coloured_as().map(str::to_owned));
                if self.is_new() {
                    parts.push("not on disk".to_owned());
                }
                let state = if self.saving.is_some() {
                    Some("saving")
                } else {
                    self.dirty.then_some("edited")
                };
                parts.extend(state.map(str::to_owned));
                parts.extend(self.trouble.as_ref().map(|t| {
                    match t {
                        Trouble::Conflict => "changed on disk",
                        Trouble::Failed(_) => "not saved",
                    }
                    .to_owned()
                }));
                if !self.changed.is_empty() {
                    parts.push(format!("{} changed", self.changed.len()));
                }
                if self.waiting.is_some() {
                    parts.push("a program waits".to_owned());
                }
                parts.join(", ")
            }
            Some(FileRead::Binary { size }) => format!("binary, {}", size_label(*size)),
            Some(FileRead::Media { media_type, bytes, .. }) => self.preview.as_ref().map_or_else(
                || format!("{media_type}, {}", size_label(bytes.len() as u64)),
                preview::Preview::summary,
            ),
            Some(FileRead::Missing { error }) => format!("missing: {error}"),
            Some(FileRead::TooLarge { size }) => format!("too large, {}", size_label(*size)),
        }
    }

    /// A file past the cap: what is so and how far past, and under it the ways to open it in
    /// a terminal on its worker instead ($EDITOR at the tile's line, or $PAGER).
    fn too_large(&self, size: u64, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let k = self.zoom;
        let editor = crate::terminal::url::editor_command(&self.path, self.focus_line_number());
        let pager = pager_command(&self.path);
        let ways = div()
            .mt(px(theme.spacing.sm * k))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .child(self.bar_button(
                "file-open-editor",
                OPEN_IN_EDITOR,
                ButtonKind::Secondary,
                cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(FileViewEvent::Run(editor.clone()));
                }),
            ))
            .child(self.bar_button(
                "file-open-pager",
                OPEN_IN_PAGER,
                ButtonKind::Ghost,
                cx.listener(move |_this, _ev, _w, cx| {
                    cx.emit(FileViewEvent::Run(pager.clone()));
                }),
            ));
        self.notice(Symbol::DocText, TOO_LARGE, Some(too_large_detail(size)), Some(ways))
    }

    /// The line the tile was opened at, 1-based, for a command that opens the file there.
    fn focus_line_number(&self) -> Option<u32> {
        self.focus.and_then(|l| u32::try_from(l.saturating_add(1)).ok())
    }

    /// What the body says instead of the text, one composed block in its middle: the kind's
    /// mark, what is so, why, and any ways on.
    fn notice(
        &self,
        icon: Symbol,
        title: &'static str,
        detail: Option<String>,
        ways: Option<gpui::Div>,
    ) -> AnyElement {
        let id = self.id.as_uuid();
        let (theme, k) = (&self.theme, self.zoom);
        let said = match &detail {
            Some(detail) => format!("{title}: {detail}"),
            None => title.to_owned(),
        };
        div()
            .id("file-notice")
            .debug_selector(move || format!("file-notice-{id}"))
            .role(Role::Status)
            .aria_label(SharedString::from(said))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(self.pad * k))
            .child(
                crate::kit::notice(
                    theme,
                    k,
                    crate::kit::notice_mark(theme, icon, k),
                    title,
                    detail.map(SharedString::from),
                )
                .children(ways),
            )
            .into_any_element()
    }

    /// The one line under the header: why the text is read-only, or what stopped a save and
    /// the ways out. None when all is well. Its text starts on the header's inset, after the
    /// tone's mark where there is trouble; "Reload" is the small secondary button and
    /// "Overwrite", the way that loses the disk's text, the quieter ghost.
    fn render_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let s = &self.theme.surfaces;
        let (text, mark, actions): (SharedString, _, Vec<AnyElement>) = match &self.trouble {
            Some(Trouble::Conflict) => (
                CHANGED_ON_DISK.into(),
                (Symbol::ExclamationmarkTriangle, s.warn, s.warn_fill),
                vec![
                    self.bar_button(
                        "file-compare",
                        if self.comparing() { compare::BACK_TO_EDIT } else { compare::COMPARE },
                        ButtonKind::Ghost,
                        cx.listener(|this, _ev, _w, cx| this.toggle_compare(cx)),
                    ),
                    self.bar_button(
                        "file-reload",
                        RELOAD,
                        ButtonKind::Secondary,
                        cx.listener(|this, _ev, _w, cx| {
                            this.reload(cx);
                        }),
                    ),
                    self.bar_button(
                        "file-overwrite",
                        OVERWRITE,
                        ButtonKind::Ghost,
                        cx.listener(|this, _ev, window, cx| {
                            this.trim_touched(window, cx);
                            this.overwrite(cx);
                        }),
                    ),
                ],
            ),
            Some(Trouble::Failed(error)) => (
                format!("Not saved: {error}").into(),
                (Symbol::XmarkCircle, s.error, s.error_fill),
                Vec::new(),
            ),
            // A program waiting on it says so on its own line, which "Done" makes the file from.
            None if self.is_new() && self.waiting.is_none() => {
                (NOT_ON_DISK.into(), (Symbol::DocBadgePlus, s.text_muted, s.text_muted), Vec::new())
            }
            None => return None,
        };
        Some(self.bar_line("file-bar", text, mark, actions))
    }

    /// The line under the header while a program waits on the file: what waits, "Done" (the
    /// secondary button: it saves) and "Give up" (the ghost). Under the trouble's line when
    /// there is one, since that is settled first.
    fn render_waiting(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.waiting?;
        let s = &self.theme.surfaces;
        let actions = vec![
            self.bar_button(
                "file-done",
                DONE,
                ButtonKind::Secondary,
                cx.listener(|this, _ev, window, cx| {
                    this.trim_touched(window, cx);
                    this.finish_edit(cx);
                }),
            ),
            self.bar_button(
                "file-give-up",
                GIVE_UP,
                ButtonKind::Ghost,
                cx.listener(|this, _ev, _w, cx| this.give_up(cx)),
            ),
        ];
        let mark = (Symbol::Terminal, s.accent, s.accent_fill);
        Some(self.bar_line("file-waiting", PROGRAM_WAITS.into(), mark, actions))
    }

    /// One line under the header: the tone's mark, what is so, and its ways out, on a faint
    /// wash of the tone.
    fn bar_line(
        &self,
        part: &'static str,
        text: SharedString,
        (icon, tone, fill): (Symbol, slopty_theme::Rgb, slopty_theme::Rgb),
        actions: Vec<AnyElement>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = self.zoom;
        let id = self.id.as_uuid();
        let wash = hsla_alpha(fill, alpha::FAINT);
        div()
            .id(part)
            .debug_selector(move || format!("{part}-{id}"))
            .role(Role::Status)
            .aria_label(text.clone())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm * k))
            .px(px(theme.spacing.inset() * k))
            .py(px(theme.spacing.xs * k))
            .border_b(crate::kit::hair(theme))
            .border_color(hsla(s.border))
            .bg(wash)
            .text_size(px(theme.typography.small() * k))
            .font_family(theme.typography.ui_family.clone())
            .child(
                crate::icons::icon(theme, icon, IconSize::Inline, hsla(tone))
                    .size(px(theme.typography.icon() * k)),
            )
            .child(div().flex_1().min_w_0().overflow_hidden().text_color(hsla(s.text)).child(text))
            .children(actions)
            .into_any_element()
    }

    /// One of the bar's ways out on the kit's pill frame, as tall as a header's words that act:
    /// secondary (the panel with a hairline) or ghost (text until the pointer is on it), scaled
    /// with the zoom.
    fn bar_button(
        &self,
        part: &'static str,
        label: &'static str,
        kind: ButtonKind,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let k = self.zoom;
        let id = self.id.as_uuid();
        let button = crate::kit::pill_frame(theme, k)
            .id(part)
            .debug_selector(move || format!("{part}-{id}"))
            .role(Role::Button)
            .aria_label(label)
            .flex_none()
            .border(crate::kit::hair(theme))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .child(label);
        let button = match kind {
            // The floating surface a step above whatever it sits on, as `kit::button`'s: on the
            // panel it sat below a notice's own surface and read as a hole.
            ButtonKind::Secondary => button
                .border_color(hsla(s.border))
                .bg(hsla(s.elevated))
                .text_color(hsla(s.text))
                .hover(move |st| st.bg(hsla(s.hover)))
                .active(move |st| st.bg(hsla(s.pressed))),
            ButtonKind::Primary | ButtonKind::Ghost | ButtonKind::Link => button
                .border_color(gpui::transparent_black())
                .text_color(hsla(s.text_secondary))
                .hover(move |st| st.bg(hsla(s.hover)).text_color(hsla(s.text)))
                .active(move |st| st.bg(hsla(s.pressed))),
        };
        crate::a11y::tab_stop(button, s.focus).on_click(on_click).into_any_element()
    }
}

/// The most lines a reload's diff compares exactly, once the common head and tail are trimmed.
/// Myers' worst case is quadratic in it; past it every line between the head and the tail is
/// tinted. The approximation is chosen by size, never by the clock, so a reload always tints
/// the same lines however busy the machine is (MEASUREMENTS, "a reload's diff").
const DIFF_EXACT_LINES: usize = 2_000;

/// Lines (0-based) of `new` that differ from `old`.
///
/// Every inserted or replaced line, and for a pure deletion the line now standing where the
/// deleted ones were (clamped to the last line), so a deletion is still pointed at.
#[must_use]
pub fn changed_lines(old: &str, new: &str) -> Vec<usize> {
    let old: Vec<&str> = old.split('\n').collect();
    let new: Vec<&str> = new.split('\n').collect();
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let old_rest = old.get(head..).unwrap_or_default();
    let new_rest = new.get(head..).unwrap_or_default();
    let tail = old_rest.iter().rev().zip(new_rest.iter().rev()).take_while(|(a, b)| a == b).count();
    let old_mid = old_rest.get(..old_rest.len().saturating_sub(tail)).unwrap_or_default();
    let new_mid = new_rest.get(..new_rest.len().saturating_sub(tail)).unwrap_or_default();
    let last = new.len().saturating_sub(1);
    let deleted_at = |at: usize| at.min(last);
    if old_mid.is_empty() && new_mid.is_empty() {
        return Vec::new();
    }
    if old_mid.len().saturating_add(new_mid.len()) > DIFF_EXACT_LINES {
        return if new_mid.is_empty() {
            vec![deleted_at(head)]
        } else {
            (head..head.saturating_add(new_mid.len())).collect()
        };
    }
    let diff = similar::TextDiff::configure().diff_slices(old_mid, new_mid);
    let mut changed = Vec::new();
    for op in diff.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Insert { new_index, new_len, .. }
            | similar::DiffOp::Replace { new_index, new_len, .. } => {
                let from = head.saturating_add(new_index);
                changed.extend(from..from.saturating_add(new_len));
            }
            similar::DiffOp::Delete { new_index, .. } => {
                changed.push(deleted_at(head.saturating_add(new_index)));
            }
        }
    }
    changed.dedup();
    changed
}

impl Render for FileView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_pending(window, cx);
        self.settle_focus(window, cx);
        self.refresh_bracket(cx);
        let id = *self.id.as_uuid();
        let search = self.search.as_ref().map(|s| self.render_search(s, cx));
        let goto = self.goto.as_ref().map(|g| self.render_goto(g, cx));
        let symbols = self.symbols.as_ref().map(|l| self.render_symbols(l, cx));
        let bar = self.render_bar(cx);
        let waiting = self.render_waiting(cx);
        let author = self.render_author(cx);
        let theme = &self.theme;
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let text_size = self.text_size * self.zoom;
        let compared = self.comparing.as_ref().filter(|_| self.read.is_some());
        let body = match &self.read {
            _ if let Some(comparing) = compared => self.render_compare(comparing),
            // Blank while a read in time would fill it; past the grace, a word.
            None if !self.away && !crate::screen::past_grace("file-reading", window, cx) => {
                div().size_full().into_any_element()
            }
            None if self.away => self.notice(Symbol::Doc, OPENS_WHEN_BACK, None, None),
            None => self.notice(Symbol::Doc, READING, None, None),
            Some(FileRead::TooLarge { size }) if self.base.is_none() => self.too_large(*size, cx),
            Some(FileRead::Binary { size }) if self.base.is_none() => {
                self.notice(Symbol::Doc, NOT_TEXT, Some(size_label(*size)), None)
            }
            Some(FileRead::Missing { error }) if self.base.is_none() => {
                self.notice(Symbol::DocText, CANNOT_READ, Some(error.clone()), None)
            }
            Some(FileRead::Media { .. }) if self.base.is_none() => self.render_preview(cx),
            Some(_) if self.previewing() => self.render_reading(cx),
            Some(_) => div()
                .key_context(TEXT_CTX)
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    Editor::new(&self.editor)
                        .appearance(false)
                        .bordered(false)
                        .aria_label(SharedString::from(format!("Text of {}", self.path)))
                        .font_family(mono.clone())
                        .text_size(px(text_size))
                        .h_full(),
                )
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("file-{id}")))
            .debug_selector(move || format!("file-{id}"))
            .key_context(if self.shows_pages() { PAGES_KEY_CONTEXT } else { CTX })
            .track_focus(&self.focus_handle)
            .role(Role::Document)
            .aria_label(SharedString::from(format!("File {}", self.path)))
            .aria_value(SharedString::from(self.summary(cx)))
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                this.trim_touched(window, cx);
                this.save(cx);
            }))
            .when(self.waiting.is_some(), |el| {
                el.on_action(cx.listener(|this, _: &FinishEdit, window, cx| {
                    this.trim_touched(window, cx);
                    this.finish_edit(cx);
                }))
            })
            .on_action(cx.listener(|this, _: &Find, window, cx| this.find(window, cx)))
            .when(open_with::offers(self.worker, &self.path, cx), |el| {
                el.on_action(cx.listener(|this, _: &open_with::OpenInEditor, _w, cx| {
                    this.open_in_editor(cx);
                }))
            })
            .when(self.has_preview(), |el| {
                el.on_action(cx.listener(|this, _: &TogglePreview, window, cx| {
                    this.toggle_preview(window, cx);
                }))
            })
            .when(self.shows_text() && !self.previewing(), |el| Self::editing_keys(el, cx))
            .when(self.shows_pages(), |el| Self::page_keys(el, cx))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(mono)
            .text_size(px(text_size))
            .children(bar)
            .children(waiting)
            .child(body)
            .children(author)
            .children(search)
            .children(goto)
            .children(symbols)
    }
}

#[cfg(test)]
mod tests;
