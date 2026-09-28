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

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{
    Editor, EditorState, Input, InputEvent, InputState, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle, RopeExt as _,
};
use slopty_core::{ItemId, WallMs};
use slopty_proto::file::{FILE_BYTES, FileRead, WriteResult};
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::highlight::Syntax;
use crate::icons::{IconName, IconSize};
use crate::kit::{ButtonKind, FIND_PLACEHOLDER, size_label};
use crate::terminal::{CloseFind, Find, FindNext, FindPrev};

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        file,
        [
            /// Save the file tile's text to the worker.
            SaveFile,
        ]
    );
}
pub use actions::SaveFile;

/// The key context of a file tile; ⌘S is bound in it.
pub const CTX: &str = "FileEditor";

/// What a file tile's bar says when the disk changed under an edit.
pub(crate) const CHANGED_ON_DISK: &str = "Changed on disk";
/// The bar's way out that drops the edit.
pub(crate) const RELOAD: &str = "Reload";
/// The bar's way out that keeps the edit.
pub(crate) const OVERWRITE: &str = "Overwrite";
/// Text a file tile colours at most; a larger file is plain text.
///
/// The colours come from a parse of the whole text off the UI thread once typing pauses, about
/// 80–160 µs a line (MEASUREMENTS, 2026-09-28): 3 s for 20 000 lines and 30 s for 200 000.
/// A parse that has started runs to its end, so past this the pauses of ordinary typing would
/// keep several cores parsing.
pub const COLOURED_BYTES: usize = 2 << 20;
/// What a file tile says while its first read is out, once the wait is worth a word.
pub(crate) const READING: &str = "Reading…";
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
    /// A file too large to edit here: run this shell line in a terminal on the file's worker
    /// ([`terminal_command`] makes the program to run it).
    Run(String),
}

impl EventEmitter<FileViewEvent> for FileView {}

/// A version of the file: what the worker sent, or what this tile saved.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Version {
    /// The text as the worker sends it, without the file's final newline.
    text: String,
    /// Whether the file ends with a newline, which the worker's text leaves off.
    newline: bool,
    /// Its modification time on disk.
    modified_ms: WallMs,
}

impl Version {
    /// The version a text read describes.
    fn of(text: &str, final_newline: bool, modified_ms: WallMs) -> Self {
        Self { text: text.to_owned(), newline: final_newline, modified_ms }
    }

    /// The bytes the file holds for this text.
    fn file_text(&self, text: &str) -> String {
        if self.newline { format!("{text}\n") } else { text.to_owned() }
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

/// The open find bar of a file tile.
struct FileSearch {
    input: Entity<InputState>,
    /// What the hits are for.
    needle: String,
    /// Lines (0-based) holding the needle, in order.
    hits: Vec<usize>,
    /// Index into `hits` of the one the tile is on.
    current: Option<usize>,
    _subscription: Subscription,
}

/// The lines holding `needle`, in order; none for an empty needle.
///
/// Found in the whole text at once: one pass for the matches and one count of the newlines
/// between them, with no copy per line. Smart case, the terminal's rule: a needle with no
/// capital matches in any case, one with a capital as typed.
#[must_use]
pub fn hit_lines(text: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let folded;
    let (haystack, needle) = if needle.chars().any(char::is_uppercase) {
        (text, needle.to_owned())
    } else {
        // Folding keeps every newline where it was, so the lines are counted in the fold.
        folded = text.to_lowercase();
        (folded.as_str(), needle.to_lowercase())
    };
    let (mut hits, mut line, mut counted) = (Vec::new(), 0_usize, 0_usize);
    for (at, _) in haystack.match_indices(needle.as_str()) {
        let between = haystack.get(counted..at).unwrap_or_default();
        line = line.saturating_add(between.matches('\n').count());
        counted = at;
        if hits.last() != Some(&line) {
            hits.push(line);
        }
    }
    hits
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
    path: String,
    /// What the worker last said, `None` until it answers.
    read: Option<FileRead>,
    /// The version the edit started from, once there is text.
    base: Option<Version>,
    /// The text sent to be written, until the worker answers.
    saving: Option<Version>,
    /// What stops a save, shown as a line under the header with its way out.
    trouble: Option<Trouble>,
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
    search: Option<FileSearch>,
    /// The grammar the path (or first line) names; none for a file the bundle cannot colour.
    syntax: Option<Syntax>,
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
    /// A tile for `path`, waiting on the worker.
    pub fn new(
        id: ItemId,
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
            path: path.to_owned(),
            read: None,
            base: None,
            saving: None,
            trouble: None,
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
            syntax: None,
            _editor_events: [events, redraw],
        }
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
        if self.shows_text() {
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
        if self.shows_text() && self.focus_handle.is_focused(window) {
            self.editor.update(cx, |e, cx| e.focus(window, cx));
        } else if !self.shows_text() && editor.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Land the caret on `line` (1-based, as a tool names it) and scroll there, now if the
    /// text is here, else when it arrives. `None` leaves the caret.
    pub fn focus_line(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        self.focus = line.and_then(|l| usize::try_from(l).ok()).map(|l| l.saturating_sub(1));
        if self.focus.is_some() {
            self.pending_line = self.focus;
        }
        cx.notify();
    }

    /// The editor's text changed by a keystroke (a replace from here emits nothing).
    fn edited(&mut self, cx: &mut Context<Self>) {
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
        match &read {
            FileRead::Text { text, modified_ms, final_newline, .. } => {
                let incoming = Version::of(text, *final_newline, *modified_ms);
                self.read_only = None;
                self.take_version(incoming, cx);
            }
            // The link hands on the text it announces, never the announcement.
            FileRead::Streamed { .. } => return,
            FileRead::Binary { .. } | FileRead::Missing { .. } | FileRead::TooLarge { .. } => {
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
            self.trouble = None;
            return;
        }
        let stale = self
            .base
            .as_ref()
            .is_some_and(|b| b.text == incoming.text && incoming.modified_ms <= b.modified_ms);
        if !stale {
            self.trouble = Some(Trouble::Conflict);
        }
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
        self.pending_text = Some(incoming.text.clone());
        self.base = Some(incoming);
        self.dirty = false;
        self.discard = false;
        self.trouble = None;
        // A save still out was of the text just dropped: its answer, if one comes, is about
        // nothing this tile holds, and must not keep ⌘S, "Overwrite" and "Reload" waiting.
        self.saving = None;
        cx.notify();
    }

    /// The link a save went out on dropped before the worker answered: the tile stops waiting
    /// and says the edit may not be on disk, so ⌘S can send it again once the worker is back.
    pub fn link_lost(&mut self, cx: &mut Context<Self>) {
        if self.saving.take().is_some() {
            tracing::info!(path = %self.path, "save unanswered: link lost");
            self.trouble = Some(Trouble::Failed(LINK_LOST.to_owned()));
            cx.notify();
        }
    }

    /// ⌘S: send the edit, based on the version it started from. Nothing when there is
    /// nothing to save, a save is already out, or a conflict is waiting for "Reload" or
    /// "Overwrite".
    pub fn save(&mut self, cx: &mut Context<Self>) {
        if !self.dirty || self.saving.is_some() || self.trouble == Some(Trouble::Conflict) {
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
        let newline = self.base.as_ref().is_some_and(|b| b.newline);
        let sent = Version { text, newline, modified_ms: base_modified_ms.unwrap_or(WallMs::ZERO) };
        let file = sent.file_text(&sent.text);
        tracing::debug!(path = %self.path, bytes = file.len(), ?base_modified_ms, "save file");
        self.saving = Some(sent);
        cx.emit(FileViewEvent::Save { text: file, base_modified_ms });
        cx.notify();
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
        let Some(sent) = self.saving.take() else { return };
        match result {
            WriteResult::Saved { modified_ms, .. } => {
                self.dirty = self.text(cx) != sent.text;
                self.base = Some(Version { modified_ms, ..sent });
                self.trouble = None;
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
        cx.notify();
    }

    /// ⌘F: open the find bar, or put the caret back in it with the text selected.
    pub fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(FIND_PLACEHOLDER));
            let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
                InputEvent::Change => this.search_changed(cx),
                InputEvent::PressEnter { shift, .. } => {
                    this.step_hit(if *shift { -1 } else { 1 }, cx);
                }
                InputEvent::Focus | InputEvent::Blur => {}
            });
            self.search = Some(FileSearch {
                input,
                needle: String::new(),
                hits: Vec::new(),
                current: None,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    /// Open the find bar on `needle` (a find in every tile chose this one): the field holds
    /// it and the tile lands on the first hit.
    pub fn find_with(&mut self, needle: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.find(window, cx);
        let Some(search) = &mut self.search else { return };
        search.input.update(cx, |input, cx| input.set_value(needle.to_owned(), window, cx));
        needle.clone_into(&mut search.needle);
        self.refresh_hits(cx);
    }

    /// The find bar's needle, when the bar is open.
    #[must_use]
    pub fn search_needle(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.needle.as_str())
    }

    /// Esc or ✕ in the find bar: close it; the editor takes the keyboard back.
    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.remark(cx);
            cx.emit(FileViewEvent::FindClosed);
            cx.notify();
        }
    }

    /// Whether the find bar is open.
    #[must_use]
    pub const fn finding(&self) -> bool {
        self.search.is_some()
    }

    /// The lines found (0-based) and which one the tile is on.
    #[must_use]
    pub fn hits(&self) -> Option<(&[usize], Option<usize>)> {
        self.search.as_ref().map(|s| (s.hits.as_slice(), s.current))
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let needle = search.input.read(cx).value().to_string();
        if needle == search.needle {
            return;
        }
        search.needle = needle;
        self.refresh_hits(cx);
    }

    /// Recount the hits for the needle (it, or the text, changed) and land on the first at
    /// or after the caret.
    fn refresh_hits(&mut self, cx: &mut Context<Self>) {
        let Some(needle) = self.search.as_ref().map(|s| s.needle.clone()) else { return };
        let hits = hit_lines(&self.text(cx), &needle);
        let from = usize::try_from(self.editor.read(cx).cursor_position().line).unwrap_or(0);
        let Some(search) = &mut self.search else { return };
        search.hits = hits;
        search.current = if search.hits.is_empty() {
            None
        } else {
            Some(search.hits.iter().position(|&line| line >= from).unwrap_or(0))
        };
        self.go_to_hit(cx);
        self.remark(cx);
        cx.notify();
    }

    /// ⌘G / ↩ (+1) and ⌘⇧G / ⇧↩ (−1), wrapping.
    pub fn step_hit(&mut self, delta: i64, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let count = i64::try_from(search.hits.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(search.current.unwrap_or(0)).unwrap_or(0);
        search.current = usize::try_from(at.saturating_add(delta).rem_euclid(count)).ok();
        self.go_to_hit(cx);
        cx.notify();
    }

    /// Select the current hit's text, which scrolls it into view.
    fn go_to_hit(&self, cx: &mut Context<Self>) {
        let Some(search) = &self.search else { return };
        let Some(line) = search.current.and_then(|c| search.hits.get(c)).copied() else { return };
        let needle = search.needle.clone();
        self.editor.update(cx, |e, cx| {
            let text = e.text();
            let start = text.line_start_offset(line);
            let row = text.slice_line(line).to_string();
            let at = if needle.chars().any(char::is_uppercase) {
                row.find(&needle)
            } else {
                row.to_lowercase().find(&needle.to_lowercase())
            };
            let from = start.saturating_add(at.unwrap_or(0));
            let to = from.saturating_add(if at.is_some() { needle.len() } else { 0 });
            e.set_selected_range(from..to.min(text.len()), cx);
        });
    }

    /// Paint the tints: the lines the last reload changed, and the find's hits.
    fn remark(&mut self, cx: &mut Context<Self>) {
        let s = &self.theme.surfaces;
        let (tint, hit) = (hsla_alpha(s.success, alpha::FAINT), hsla_alpha(s.warn, alpha::FAINT));
        let hits = self.search.as_ref().map(|s| s.hits.clone()).unwrap_or_default();
        let changed = self.changed.clone();
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
            marks.extend(
                hits.iter().filter(|row| **row < text.lines_len()).map(|row| line(*row, hit)),
            );
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
            None | Some(FileRead::Streamed { .. }) => READING.to_owned(),
            Some(FileRead::Text { .. }) => {
                let n = self.line_count(cx);
                let mut parts =
                    vec![if n == 1 { "1 line".to_owned() } else { format!("{n} lines") }];
                parts.extend(self.coloured_as().map(str::to_owned));
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
                parts.join(", ")
            }
            Some(FileRead::Binary { size }) => format!("binary, {}", size_label(*size)),
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
        self.notice(IconName::FileText, TOO_LARGE, Some(too_large_detail(size)), Some(ways))
    }

    /// The line the tile was opened at, 1-based, for a command that opens the file there.
    fn focus_line_number(&self) -> Option<u32> {
        self.focus.and_then(|l| u32::try_from(l.saturating_add(1)).ok())
    }

    /// What the body says instead of the text, one composed block in its middle: the kind's
    /// mark, what is so, why, and any ways on.
    fn notice(
        &self,
        icon: IconName,
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
        let theme = &self.theme;
        let s = &theme.surfaces;
        let k = self.zoom;
        let (text, mark, actions): (SharedString, _, Vec<AnyElement>) = match &self.trouble {
            Some(Trouble::Conflict) => (
                CHANGED_ON_DISK.into(),
                Some((IconName::CircleAlert, s.warn, s.warn_fill)),
                vec![
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
                        cx.listener(|this, _ev, _w, cx| {
                            this.overwrite(cx);
                        }),
                    ),
                ],
            ),
            Some(Trouble::Failed(error)) => (
                format!("Not saved: {error}").into(),
                Some((IconName::CircleX, s.error, s.error_fill)),
                Vec::new(),
            ),
            None => return None,
        };
        let id = self.id.as_uuid();
        let wash = mark.map_or_else(
            || hsla_alpha(s.text_muted, alpha::FAINT),
            |(_, _, fill)| hsla_alpha(fill, alpha::FAINT),
        );
        Some(
            div()
                .id("file-bar")
                .debug_selector(move || format!("file-bar-{id}"))
                .role(Role::Status)
                .aria_label(text.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm * k))
                .px(px(theme.spacing.inset() * k))
                .py(px(theme.spacing.xs * k))
                .border_b_1()
                .border_color(hsla(s.border))
                .bg(wash)
                .text_size(px(theme.typography.small() * k))
                .font_family(theme.typography.ui_family.clone())
                .children(mark.map(|(icon, tone, _)| {
                    crate::icons::icon(theme, icon, IconSize::Inline, hsla(tone))
                        .size(px(theme.typography.icon() * k))
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(hsla(if mark.is_some() { s.text } else { s.text_muted }))
                        .child(text),
                )
                .children(actions)
                .into_any_element(),
        )
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
            .border_1()
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
                .hover(move |st| st.bg(hsla(s.raised)))
                .active(move |st| st.bg(hsla(s.overlay))),
            ButtonKind::Primary | ButtonKind::Ghost | ButtonKind::Link => button
                .border_color(gpui::transparent_black())
                .text_color(hsla(s.text_secondary))
                .hover(move |st| st.bg(hsla(s.raised)).text_color(hsla(s.text)))
                .active(move |st| st.bg(hsla(s.overlay))),
        };
        crate::a11y::tab_stop(button, s.accent).on_click(on_click).into_any_element()
    }

    /// The find bar over the top-right corner, as the terminal's.
    fn render_search(&self, search: &FileSearch, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let wash = hsla_alpha(s.text, alpha::FAINT);
        let bare = move |id: &'static str| {
            div()
                .id(id)
                .px(px(spacing.xs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .text_color(hsla(s.text_muted))
                .hover(move |st| st.bg(wash))
        };
        let count: SharedString = if search.needle.is_empty() {
            SharedString::default()
        } else if search.hits.is_empty() {
            "No matches".into()
        } else {
            let at = search.current.map_or(0, |c| c.saturating_add(1));
            format!("{at}/{}", search.hits.len()).into()
        };
        div()
            .id("file-search")
            .debug_selector(|| "file-search".to_owned())
            .key_context("FileSearch")
            .absolute()
            .top(px(spacing.sm))
            .right(px(spacing.sm))
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .px(px(spacing.sm))
            .py(px(spacing.xs))
            .rounded(px(radii.sm))
            .map(|el| crate::kit::elevate(el, theme))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text))
            .font_family(theme.typography.ui_family.clone())
            .on_action(cx.listener(|this, _: &CloseFind, _window, cx| this.close_find(cx)))
            .on_action(cx.listener(|this, _: &FindNext, _window, cx| this.step_hit(1, cx)))
            .on_action(cx.listener(|this, _: &FindPrev, _window, cx| this.step_hit(-1, cx)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .role(Role::Group)
            .aria_label("Find in file")
            .child(div().w(px(180.0)).child(Input::new(&search.input).aria_label("Find in file")))
            .child(
                div()
                    .id("file-search-count")
                    .min_w(px(40.0))
                    .text_color(hsla(s.text_secondary))
                    .role(Role::Label)
                    .aria_label("Matches")
                    .aria_value(count.clone())
                    .child(count),
            )
            .child(
                bare("file-search-prev")
                    .role(Role::Button)
                    .aria_label("Previous match")
                    .child("↑")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(-1, cx))),
            )
            .child(
                bare("file-search-next")
                    .role(Role::Button)
                    .aria_label("Next match")
                    .child("↓")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.step_hit(1, cx))),
            )
            .child(
                bare("file-search-close")
                    .role(Role::Button)
                    .aria_label("Close find")
                    .child("✕")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.close_find(cx))),
            )
            .into_any_element()
    }
}

/// How long a reload's diff may run on the UI thread before it settles for an approximation
/// (more lines tinted than changed). A small change to a large file ends well before it: the
/// common head and tail are trimmed first.
const DIFF_WITHIN: std::time::Duration = std::time::Duration::from_millis(8);

/// Lines (0-based) of `new` that differ from `old`.
///
/// Every inserted or replaced line, and for a pure deletion the line now standing where the
/// deleted ones were (clamped to the last line), so a deletion is still pointed at.
#[must_use]
pub fn changed_lines(old: &str, new: &str) -> Vec<usize> {
    let old: Vec<&str> = old.split('\n').collect();
    let new: Vec<&str> = new.split('\n').collect();
    let diff = similar::TextDiff::configure().timeout(DIFF_WITHIN).diff_slices(&old, &new);
    let mut changed = Vec::new();
    for op in diff.ops() {
        match *op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Insert { new_index, new_len, .. }
            | similar::DiffOp::Replace { new_index, new_len, .. } => {
                changed.extend(new_index..new_index.saturating_add(new_len));
            }
            similar::DiffOp::Delete { new_index, .. } => {
                if !new.is_empty() {
                    changed.push(new_index.min(new.len().saturating_sub(1)));
                }
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
        let id = *self.id.as_uuid();
        let search = self.search.as_ref().map(|s| self.render_search(s, cx));
        let bar = self.render_bar(cx);
        let theme = &self.theme;
        let mono = theme.typography.mono_families.first().cloned().unwrap_or_default();
        let text_size = self.text_size * self.zoom;
        let body = match &self.read {
            // Blank while a read in time would fill it; past the grace, a word.
            None if !crate::screen::past_grace("file-reading", window, cx) => {
                div().size_full().into_any_element()
            }
            None => self.notice(IconName::File, READING, None, None),
            Some(FileRead::TooLarge { size }) if self.base.is_none() => self.too_large(*size, cx),
            Some(FileRead::Binary { size }) if self.base.is_none() => {
                self.notice(IconName::File, NOT_TEXT, Some(size_label(*size)), None)
            }
            Some(FileRead::Missing { error }) if self.base.is_none() => {
                self.notice(IconName::FileText, CANNOT_READ, Some(error.clone()), None)
            }
            Some(_) => div()
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
            .key_context(CTX)
            .track_focus(&self.focus_handle)
            .role(Role::Document)
            .aria_label(SharedString::from(format!("File {}", self.path)))
            .aria_value(SharedString::from(self.summary(cx)))
            .on_action(cx.listener(|this, _: &SaveFile, _window, cx| this.save(cx)))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.find(window, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(mono)
            .text_size(px(text_size))
            .children(bar)
            .child(body)
            .children(search)
    }
}

#[cfg(test)]
mod tests;
