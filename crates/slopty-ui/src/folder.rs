//! A folder tile: a directory on the worker, browsed in place.
//!
//! The item (`ItemKind::Folder`) names the directory; what is in it is not in the registry. The
//! workspace asks the worker for it (`ClientMsg::ListFolder`) when the tile appears, when it moves
//! and when it takes the keyboard, and the worker answers with the first entries, folders first
//! ([`Listing`]). ↑ and ↓ move the selection, ↩ or a click opens it: a folder in place (the
//! item moves with it, `ItemOp::SetFolder`), a file as a file tile beside this one. ⌫ or ⌘↑
//! goes up, and so does the header's arrow. A row dragged out of the tile is a file promise, as
//! a path dragged out of a shell is; files dropped on the tile go up into the folder.
//!
//! New folder, Rename or move and Move to Trash ask the worker through [`FsOp`]s; a name is
//! written in a field in the row's place, and the folder's watch lists it again once the op is
//! done. A folder past [`slopty_proto::folder::FOLDER_ENTRIES`] entries comes a page at a time
//! as the rows near its end are drawn ([`FolderPages`]).
//!
//! On iOS the Files picker stands in for Finder: the path bar's upload button ("Upload from
//! Files…") sends picked files up into the folder, and the selected row's save button ("Save to
//! Files…") brings it down and saves it there. Both are palette commands too. An iPad's touch
//! held on a row lifts it out to another app, found by [`FolderView::path_at`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_kit::component::input::{self as input, Input, InputEvent, InputState};
use gpui_kit::component::{Sizable as _, Size};
use slopty_client::folders::FolderPages;
use slopty_client::layout::WorkerKey;
use slopty_core::ItemId;
use slopty_proto::ClientMsg;
use slopty_proto::folder::{After, FolderEntry, FsOp, Listing};
use slopty_proto::orchestration::FileKind;
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};
use crate::palette::Plate;

mod menu;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        folder,
        [
            /// Select the entry below.
            SelectNext,
            /// Select the entry above.
            SelectPrevious,
            /// Select the first entry.
            SelectFirst,
            /// Select the last entry.
            SelectLast,
            /// Open the selected entry: a folder in place, a file in a tile beside.
            OpenSelected,
            /// Go up to the folder this one is in.
            OpenParent,
            /// Pick files here and send them up into this folder: the Files app on iOS, the open
            /// panel on a Mac.
            UploadFromFiles,
            /// Bring the selected entry down and save it here: with the Files app on iOS, into
            /// a folder picked on a Mac.
            SaveToFiles,
            /// Make a folder here, named in a field at the top of the rows.
            NewFolder,
            /// Rename the selected entry, or move it by a path, in a field in its row.
            RenameSelected,
            /// Move the selected entry to the worker's trash.
            TrashSelected,
        ]
    );
}
pub use actions::{
    NewFolder, OpenParent, OpenSelected, RenameSelected, SaveToFiles, SelectFirst, SelectLast,
    SelectNext, SelectPrevious, TrashSelected, UploadFromFiles,
};

/// The key context of a folder tile; its keys are bound in it.
pub const CTX: &str = "FolderView";

/// What a folder with nothing in it says.
pub(crate) const EMPTY_FOLDER: &str = "Empty folder";

/// An empty folder's one next step: a shell in it.
pub(crate) const NEW_SHELL: &str = "New shell here";
/// What a folder tile says when a file is at its path.
pub(crate) const NOT_A_FOLDER: &str = "Not a folder";
/// What a folder tile says when the worker could not list its path, over the reason.
pub(crate) const CANNOT_LIST: &str = "Cannot list this folder";
/// The header's way up, and the palette's.
pub(crate) const ENCLOSING_FOLDER: &str = "Enclosing folder";
/// The path bar's upload through the Files picker, and the palette's, on iOS.
pub const UPLOAD_FROM_FILES: &str = "Upload from Files\u{2026}";
/// The selected row's save through the Files picker, and the palette's, on iOS.
pub const SAVE_TO_FILES: &str = "Save to Files\u{2026}";
/// The palette's upload where the open panel picks the files: a Mac.
pub const UPLOAD: &str = "Upload\u{2026}";
/// The palette's download where a folder picked here takes the selected entry: a Mac.
pub const DOWNLOAD: &str = "Download\u{2026}";
/// The palette's line, and the field's name, for a new folder.
pub const NEW_FOLDER: &str = "New folder";
/// The palette's line for a rename, and a move by a path.
pub const RENAME_OR_MOVE: &str = "Rename or move\u{2026}";
/// The palette's line that trashes the selected entry.
pub const MOVE_TO_TRASH: &str = "Move to Trash";
/// Rows from the end of those listed at which the next page is asked for.
const PAGE_AHEAD: usize = 40;

/// Whether the Files picker stands in for Finder here: iOS, where nothing else reaches the
/// Files app, and no file can be dragged in or out of an iPhone.
pub const FILES_PICKER: bool = cfg!(target_os = "ios");

/// How far a pressed row travels before it is dragged out rather than clicked: the terminal's
/// slop for a path.
const DRAG_SLOP: f32 = 4.0;
/// The crumbs the path bar keeps after its first: deeper folders fold into one `…`.
const CRUMBS: usize = 3;
/// The width of a row's size column, room for "1023.9 KB".
const SIZE_W: f32 = 64.0;
/// The width of a row's age column, room for "59m".
const AGE_W: f32 = 32.0;

/// What a folder tile tells the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum FolderViewEvent {
    /// Send this to the worker: the next page of the folder.
    Ask(ClientMsg),
    /// Ask the worker to change its files: a new folder, a rename or move, a trash.
    Op(FsOp),
    /// The tile moved to this directory: the item follows, and the worker is asked for it.
    Browse(String),
    /// A file was opened: a file tile for it beside this one.
    OpenFile(String),
    /// A row was dragged out of the app.
    DragOut(String),
    /// Files picked in the Files app are to go up into this folder.
    UploadHere,
    /// The agent's worktree the folder is in, at this root, is to go.
    RemoveWorktree(String),
    /// A shell is to open in this directory: an empty folder's one next step.
    NewShell(String),
    /// This entry is to be brought down and saved with the Files app.
    SaveToFiles {
        /// Its path on the worker.
        path: String,
        /// Whether it is a folder.
        folder: bool,
    },
}

impl EventEmitter<FolderViewEvent> for FolderView {}

/// The view of one folder item.
pub struct FolderView {
    id: ItemId,
    /// The machine the folder is on.
    worker: WorkerKey,
    /// The directory the tile is at, as the item names it.
    path: String,
    /// What the worker last said, its pages joined, for `listed`; kept while the next listing
    /// is on its way so a move does not flash an empty tile.
    pages: FolderPages,
    /// The path `listing` answers.
    listed: Option<String>,
    /// The worker is to be asked for `path` ([`Self::take_request`]).
    wants: bool,
    selected: Option<usize>,
    /// The entry to select when the next listing comes: the folder just gone up from, or one
    /// just made or renamed.
    came_from: Option<String>,
    /// A name being written: a new folder's, or a new name for an entry.
    naming: Option<Naming>,
    /// The changes asked from this tile, drawn as done until the folder lists their result.
    asked: Vec<Asked>,
    /// The worker's home, which the path bar calls `~`.
    home: Option<String>,
    /// A row pressed and where, until it is let go or dragged out.
    press: Option<(usize, Point<Pixels>)>,
    /// The press became a drag: the click that ends it opens nothing.
    dragged: bool,
    /// A row's own menu, open where it was pressed.
    row_menu: Option<menu::RowMenu>,
    /// Where the list and its rows were drawn last, for [`Self::path_at`].
    drawn: Rc<RefCell<Drawn>>,
    theme: Theme,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    plate: Plate,
}

impl std::fmt::Debug for FolderView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderView")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("listed", &self.listed)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl FolderView {
    /// A tile at `path` on `worker`, waiting on it.
    pub fn new(
        id: ItemId,
        worker: WorkerKey,
        path: &str,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            id,
            worker,
            path: path.to_owned(),
            pages: FolderPages::new(path.to_owned()),
            listed: None,
            wants: true,
            selected: None,
            came_from: None,
            naming: None,
            asked: Vec::new(),
            home: None,
            press: None,
            dragged: false,
            row_menu: None,
            drawn: Rc::default(),
            theme,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            plate: Plate::default(),
        }
    }

    /// Item this tile belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// The directory the tile is at, as its item names it.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// What the worker last said, once it has.
    #[must_use]
    pub const fn listing(&self) -> Option<&Listing> {
        self.pages.listing()
    }

    /// The directory listed, absolute, once the worker has listed one.
    #[must_use]
    pub fn dir(&self) -> Option<&str> {
        match self.listing() {
            Some(Listing::Listed { dir, .. }) => Some(dir),
            _ => None,
        }
    }

    /// The entries listed, folders first.
    #[must_use]
    pub fn entries(&self) -> &[FolderEntry] {
        match self.listing() {
            Some(Listing::Listed { entries, .. }) => entries,
            _ => &[],
        }
    }

    /// The selected entry.
    #[must_use]
    pub fn selected(&self) -> Option<&FolderEntry> {
        self.entries().get(self.selected?)
    }

    /// Whether the tile moved and its new listing has not come yet.
    #[must_use]
    pub fn browsing(&self) -> bool {
        self.listed.as_deref() != Some(self.path.as_str())
    }

    /// The directory above this one, once listed; none at the root.
    #[must_use]
    pub fn parent(&self) -> Option<String> {
        parent_of(self.dir()?)
    }

    /// Where the entries are, for the self-test's dump: a summary as a screen reader hears it.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.listing() {
            None => crate::file::READING.to_owned(),
            Some(Listing::NotFolder) => NOT_A_FOLDER.to_owned(),
            Some(Listing::Missing { error }) => error.clone(),
            Some(Listing::Listed { entries, total, .. }) => {
                let shown = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                match (shown, *total) {
                    (_, 0) => EMPTY_FOLDER.to_owned(),
                    (shown, total) if shown < total => format!("First {shown} of {total} items"),
                    (_, total) => count_label(total),
                }
            }
        }
    }

    /// The worker's home, for the path bar's `~`.
    pub fn set_home(&mut self, home: Option<String>) {
        self.home = home.filter(|h| !h.is_empty());
    }

    /// Draw by another theme (the workspace swapped it).
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// The item was moved (by this tile, another client or the registry's snapshot): the
    /// listing of the new path is to be asked for. The old one stays until it comes.
    pub fn set_path(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.path != path {
            path.clone_into(&mut self.path);
            self.wants = true;
            cx.notify();
        }
    }

    /// Ask the worker again: the link it was asked on dropped, or the folder may have changed.
    pub const fn refresh(&mut self) {
        self.wants = true;
    }

    /// The path to ask the worker for, once: `None` when nothing new is wanted.
    pub fn take_request(&mut self) -> Option<String> {
        std::mem::take(&mut self.wants).then(|| self.path.clone())
    }

    /// The worker listed `asked`. An answer for a path the tile has moved from is dropped.
    ///
    /// The same folder listed again (its entries changed on disk, or it was asked again) keeps
    /// the selected entry, or the row where it was when it went, and leaves the scroll where
    /// the person put it; a new folder starts at its first row, or at the one just come up
    /// from.
    pub fn set_listing(&mut self, asked: &str, listing: Listing, cx: &mut Context<Self>) {
        if asked != self.path {
            return;
        }
        let kept = self.selected().map(|e| e.name.clone());
        let same_dir = self.listed.as_deref() == Some(asked);
        let came_from = self.came_from.take();
        let reveal = !same_dir || came_from.is_some();
        let wanted = came_from.or(if same_dir { kept } else { None });
        let was = self.selected.filter(|_| same_dir);
        if same_dir {
            self.asked.retain(|a| !a.answered);
        } else {
            self.pages = FolderPages::new(asked.to_owned());
            self.asked.clear();
        }
        let next = self.pages.listed(listing);
        self.listed = Some(asked.to_owned());
        let entries = self.entries();
        self.selected = (!entries.is_empty()).then(|| {
            wanted
                .and_then(|name| entries.iter().position(|e| e.name == name))
                .or_else(|| was.map(|ix| ix.min(entries.len().saturating_sub(1))))
                .unwrap_or(0)
        });
        if let Some(ix) = self.selected.filter(|_| reveal) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        if let Some(next) = next {
            cx.emit(FolderViewEvent::Ask(next));
        }
        cx.notify();
    }

    /// The worker sent the page of `asked` after `after`: its entries join the rows, and the
    /// page after it is asked for while more are wanted. A page of a folder the tile has left
    /// is dropped.
    pub fn set_page(
        &mut self,
        asked: &str,
        after: &After,
        listing: Listing,
        cx: &mut Context<Self>,
    ) {
        if self.listed.as_deref() != Some(asked) || asked != self.path {
            return;
        }
        if let Some(next) = self.pages.page(after, listing) {
            cx.emit(FolderViewEvent::Ask(next));
        }
        cx.notify();
    }

    /// The rows drawn reach `end`: near the last listed, the next page is asked for.
    fn drawn_to(&mut self, end: usize, cx: &mut Context<Self>) {
        let listed = self.entries().len();
        if end.saturating_add(PAGE_AHEAD) >= listed
            && self.pages.has_more()
            && let Some(next) = self.pages.more()
        {
            cx.emit(FolderViewEvent::Ask(next));
        }
    }

    /// Give the tile the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }

    /// Whether the tile has the keyboard.
    #[must_use]
    pub fn focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Move the selection by `delta` rows, stopping at the ends.
    pub fn select_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.entries().len();
        if count == 0 {
            return;
        }
        let last = count.saturating_sub(1);
        let at = self
            .selected
            .map_or(if delta > 1 { last } else { 0 }, |ix| ix.saturating_add_signed(delta));
        self.select(at.min(last), cx);
    }

    /// Select row `ix` and bring it into view.
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected != Some(ix) {
            self.selected = Some(ix);
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Open the selected entry: a folder here, a file beside. Nothing while a move waits for
    /// its listing, since the rows shown are the old folder's.
    pub fn open_selected(&mut self, cx: &mut Context<Self>) {
        if self.browsing() {
            return;
        }
        let (Some(dir), Some(entry)) = (self.dir(), self.selected()) else { return };
        let path = join(dir, &entry.name);
        if entry.kind == FileKind::Dir {
            self.browse(path, cx);
        } else {
            tracing::info!(%path, "folder opens a file");
            cx.emit(FolderViewEvent::OpenFile(path));
        }
    }

    /// Go up to the folder this one is in, the one just left selected there.
    pub fn open_parent(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.dir().filter(|_| !self.browsing()) else { return };
        let Some(parent) = parent_of(dir) else { return };
        self.came_from = dir.rsplit('/').next().map(str::to_owned);
        self.browse(parent, cx);
    }

    /// Move the tile to `path`.
    pub fn browse(&mut self, path: String, cx: &mut Context<Self>) {
        tracing::info!(%path, "folder moves");
        self.set_path(&path, cx);
        cx.emit(FolderViewEvent::Browse(path));
    }

    /// Write the name of a new folder here, in a field at the top of the rows. Nothing while a
    /// move waits for its listing.
    pub fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dir().is_none() || self.browsing() {
            return;
        }
        self.start_naming(Name::NewFolder, String::new(), window, cx);
    }

    /// Write a new name for the selected entry in its row: a plain name renames it, and a path
    /// (`../done/a.txt`, `~/archive/`) moves it there.
    pub fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.selected().filter(|_| !self.browsing()).map(|e| e.name.clone())
        else {
            return;
        };
        self.start_naming(Name::Rename { name: name.clone() }, name, window, cx);
    }

    /// Ask for the selected entry to go to the worker's trash.
    pub fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let Some((path, _)) = self.selected.and_then(|ix| self.entry_path(ix)) else { return };
        tracing::info!(%path, "folder trashes");
        self.ask(FsOp::Trash { path }, cx);
    }

    /// Ask the worker for `op`, drawn as done from now on.
    fn ask(&mut self, op: FsOp, cx: &mut Context<Self>) {
        self.asked.push(Asked { op: op.clone(), answered: false });
        cx.emit(FolderViewEvent::Op(op));
        cx.notify();
    }

    /// The worker answered `op`, or the link that would have answered it went (`done` then
    /// says whether the folder's next listing is to show it). Done, it stays drawn as done
    /// until that listing comes, so nothing flickers back between the two; refused, it is
    /// drawn as it was at once.
    pub fn answered(&mut self, op: &FsOp, done: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.asked.iter().position(|a| a.op == *op && !a.answered) else {
            return;
        };
        if done {
            if let Some(a) = self.asked.get_mut(ix) {
                a.answered = true;
            }
        } else {
            self.asked.remove(ix);
            cx.notify();
        }
    }

    /// What is asked of the entry `name` of the folder listed: a new name, or its going.
    pub(crate) fn fate(&self, name: &str) -> Option<Fate> {
        let path = join(self.dir()?, name);
        self.asked.iter().rev().find_map(|a| match &a.op {
            FsOp::Trash { path: p } if *p == path => Some(Fate::Leaving),
            FsOp::Move { from, to } if *from == path => {
                let here = parent_of(to) == parent_of(&path);
                let last = to.rsplit('/').next().filter(|_| here);
                Some(last.map_or(Fate::Leaving, |last| Fate::Renamed(last.to_owned())))
            }
            _ => None,
        })
    }

    /// The folders asked for here that the listing does not hold yet.
    fn made(&self) -> Vec<&str> {
        let Some(dir) = self.dir() else { return Vec::new() };
        let entries = self.entries();
        self.asked
            .iter()
            .filter_map(|a| match &a.op {
                FsOp::MakeDir { parent, name }
                    if parent.trim_end_matches('/') == dir.trim_end_matches('/')
                        && !entries.iter().any(|e| e.name == *name) =>
                {
                    Some(name.as_str())
                }
                _ => None,
            })
            .collect()
    }

    /// The field for `what`, holding `text`, takes the keyboard.
    fn start_naming(
        &mut self,
        what: Name,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field =
            cx.new(|cx| InputState::new(window, cx).placeholder(NEW_FOLDER).default_value(text));
        let subscription = cx.subscribe_in(&field, window, |this, _field, event, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.finish_naming(true, window, cx),
                // A click elsewhere: the name is not taken, and the click goes where it went.
                InputEvent::Blur => {
                    if this.naming.take().is_some() {
                        cx.notify();
                    }
                }
                InputEvent::Change | InputEvent::Focus => {}
            }
        });
        field.update(cx, |f, cx| f.focus(window, cx));
        self.naming = Some(Naming { field, what, _subscription: subscription });
        cx.notify();
    }

    /// The field closes: `take` asks for what it says, and the tile has the keyboard again.
    fn finish_naming(&mut self, take: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = self.naming.take() else { return };
        let text = naming.field.read(cx).value().trim().to_owned();
        window.focus(&self.focus, cx);
        cx.notify();
        let Some(dir) = self.dir().map(str::to_owned).filter(|_| take && !text.is_empty()) else {
            return;
        };
        let op = match naming.what {
            Name::NewFolder => FsOp::MakeDir { parent: dir.clone(), name: text },
            Name::Rename { name } if name == text => return,
            Name::Rename { name } => {
                FsOp::Move { from: join(&dir, &name), to: destination(&dir, &text, &name) }
            }
        };
        // What was made or renamed here is selected when the folder lists it.
        if let FsOp::MakeDir { name, .. } | FsOp::Move { to: name, .. } = &op {
            let (parent, last) = name.rsplit_once('/').unwrap_or(("", name));
            if parent.is_empty() || parent == dir.trim_end_matches('/') {
                self.came_from = Some(last.to_owned());
            }
        }
        tracing::info!(?op, "folder asks a change");
        self.ask(op, cx);
    }

    /// The name being written, where it is written: in place of `name`'s, or a new folder's.
    #[must_use]
    pub fn naming(&self, cx: &gpui::App) -> Option<(Option<&str>, String)> {
        let naming = self.naming.as_ref()?;
        let at = match &naming.what {
            Name::NewFolder => None,
            Name::Rename { name } => Some(name.as_str()),
        };
        Some((at, naming.field.read(cx).value().to_string()))
    }

    /// The field, drawn where a name goes.
    fn name_field(&self, label: &'static str) -> Option<AnyElement> {
        let naming = self.naming.as_ref()?;
        let theme = &self.theme;
        Some(
            div()
                .flex_1()
                .min_w_0()
                .text_color(hsla(theme.surfaces.text))
                .child(
                    Input::new(&naming.field)
                        .with_size(Size::Small)
                        .text_size(px(theme.typography.ui_size))
                        .aria_label(label),
                )
                .into_any_element(),
        )
    }

    /// Over the rows while a new folder is named: its mark and the field.
    fn new_folder_row(&self) -> Option<AnyElement> {
        if !matches!(self.naming.as_ref()?.what, Name::NewFolder) {
            return None;
        }
        let theme = &self.theme;
        let id = self.id.as_uuid();
        Some(
            div()
                .id("folder-new")
                .debug_selector(move || format!("folder-new-{id}"))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(theme.spacing.sm))
                .h(px(theme.density.row))
                .px(px(theme.spacing.inset()))
                .border_b(crate::kit::HAIR)
                .border_color(hsla(theme.surfaces.stroke))
                .child(self.lead(Symbol::FolderBadgePlus, hsla(theme.surfaces.text_secondary)))
                .children(self.name_field(NEW_FOLDER))
                .into_any_element(),
        )
    }

    /// A row's lead: `symbol` beside the row's name at its size and weight.
    fn lead(&self, mark: impl Into<crate::icons::Mark>, ink: gpui::Hsla) -> gpui::Div {
        let theme = &self.theme;
        let chrome = theme.roles().chrome;
        crate::icons::beside(theme, mark, chrome, ink)
            .size(px(IconSize::beside_slot(theme, chrome)))
    }

    /// Over the rows, a folder asked for here and not yet listed, drawn as made.
    fn made_row(&self, n: usize, name: &str) -> AnyElement {
        let theme = &self.theme;
        let id = self.id.as_uuid();
        div()
            .id(numbered("folder-made", n))
            .debug_selector(move || format!("folder-made-{n}-{id}"))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(name.to_owned()))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .h(px(theme.density.row))
            .px(px(theme.spacing.inset()))
            .opacity(ASKED)
            .child(self.lead(Symbol::Folder, hsla(theme.surfaces.text_secondary)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(theme.surfaces.text))
                    .child(SharedString::from(name.to_owned())),
            )
            .into_any_element()
    }

    /// A row pressed: selected at once, and remembered in case it becomes a drag.
    fn press(&mut self, ix: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.naming.is_some() {
            return;
        }
        self.press = Some((ix, at));
        self.dragged = false;
        self.select(ix, cx);
    }

    /// The pointer moved with the button down: past the slop, the pressed row is dragged out.
    fn drag(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((ix, from)) = self.press else { return };
        if (at - from).magnitude() < f64::from(DRAG_SLOP) {
            return;
        }
        self.press = None;
        let (Some(dir), Some(entry)) = (self.dir(), self.entries().get(ix)) else { return };
        let path = join(dir, &entry.name);
        self.dragged = true;
        tracing::info!(%path, "folder drags out");
        cx.emit(FolderViewEvent::DragOut(path));
    }

    /// A click on row `ix`: it opens, once. The second click of a double-click is the same
    /// gesture, and the end of a drag is not a click.
    fn clicked(&mut self, ix: usize, clicks: usize, cx: &mut Context<Self>) {
        self.press = None;
        if std::mem::take(&mut self.dragged) || clicks > 1 || self.naming.is_some() {
            return;
        }
        self.select(ix, cx);
        self.open_selected(cx);
    }

    /// The entry of row `ix` as a worker path, and whether it is a folder. None while a move
    /// waits for its listing, since the rows shown are the old folder's.
    fn entry_path(&self, ix: usize) -> Option<(String, bool)> {
        let (Some(dir), Some(entry)) =
            (self.dir().filter(|_| !self.browsing()), self.entries().get(ix))
        else {
            return None;
        };
        Some((join(dir, &entry.name), entry.kind == FileKind::Dir))
    }

    /// The entry of the row drawn under `at` (window points) as a worker path, and whether it
    /// is a folder: what a touch held there lifts out of an iPad.
    #[must_use]
    pub fn path_at(&self, at: Point<Pixels>) -> Option<(String, bool)> {
        let drawn = self.drawn.borrow();
        if !drawn.list.is_some_and(|list| list.contains(&at)) {
            return None;
        }
        let (ix, _) = drawn.rows.iter().find(|(_, bounds)| bounds.contains(&at))?;
        self.entry_path(*ix)
    }

    /// Files picked in the Files app are to go up into this folder.
    pub fn upload_here(&self, cx: &mut Context<Self>) {
        if self.dir().is_some() {
            tracing::info!(path = %self.path, "folder asks the Files picker for files");
            cx.emit(FolderViewEvent::UploadHere);
        }
    }

    /// Open the selected entry in the person's own editor, or the folder while none is
    /// selected, as Finder's "Open With" takes the selection.
    fn open_in_editor(&self, cx: &gpui::App) {
        use crate::file::open_with;
        let path = self.selected.and_then(|ix| self.entry_path(ix)).map(|(path, _)| path);
        let path = path.as_deref().unwrap_or(&self.path);
        if let Some(opening) = open_with::opening(self.worker, path, None, cx) {
            open_with::open(&opening, cx);
        }
    }

    /// The selected entry is to be brought down and saved with the Files app.
    pub fn save_selected(&self, cx: &mut Context<Self>) {
        let Some((path, folder)) = self.selected.and_then(|ix| self.entry_path(ix)) else {
            return;
        };
        tracing::info!(%path, "folder saves to Files");
        cx.emit(FolderViewEvent::SaveToFiles { path, folder });
    }

    /// What the body says instead of rows, one composed block in its middle: the kind's mark,
    /// what is so and, where there is one, why.
    fn notice(
        &self,
        icon: Symbol,
        title: impl Into<SharedString>,
        detail: Option<SharedString>,
        next: Option<AnyElement>,
    ) -> AnyElement {
        let id = self.id.as_uuid();
        let theme = &self.theme;
        div()
            .debug_selector(move || format!("folder-notice-{id}"))
            .flex_1()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                crate::kit::notice(theme, crate::kit::notice_mark(theme, icon), title, detail)
                    .children(next),
            )
            .into_any_element()
    }

    /// The folders from the top down to this one, each a way there; the root's own name, or
    /// `~` for the worker's home. Past [`CRUMBS`] deep, the middle folds into one `…`.
    fn crumbs(&self, dir: &str) -> Vec<(String, String)> {
        let home = self.home.as_deref().map(|h| h.trim_end_matches('/')).filter(|h| !h.is_empty());
        let under_home = home.and_then(|home| {
            let rest = dir.strip_prefix(home)?;
            (rest.is_empty() || rest.starts_with('/')).then_some((home, rest))
        });
        let (mut crumbs, rest) = match under_home {
            Some((home, rest)) => (vec![("~".to_owned(), home.to_owned())], rest),
            None => (vec![("/".to_owned(), "/".to_owned())], dir),
        };
        let mut at = crumbs.first().map(|(_, path)| path.clone()).unwrap_or_default();
        for part in rest.split('/').filter(|p| !p.is_empty()) {
            at = join(&at, part);
            crumbs.push((part.to_owned(), at.clone()));
        }
        if crumbs.len() > CRUMBS.saturating_add(2) {
            let tail = crumbs.split_off(crumbs.len().saturating_sub(CRUMBS));
            let folded = crumbs.pop().map(|(_, path)| path).unwrap_or_default();
            crumbs.truncate(1);
            crumbs.push(("\u{2026}".to_owned(), folded));
            crumbs.extend(tail);
        }
        crumbs
    }

    /// The bar over the rows: where the tile is, every folder above it a click away, and how
    /// much this one holds.
    fn path_bar(&self, dir: &str, total: u32, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let crumbs = self.crumbs(dir);
        let last = crumbs.len().saturating_sub(1);
        let mut trail = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(theme.spacing.xxs))
            .overflow_hidden()
            .whitespace_nowrap();
        for (n, (label, path)) in crumbs.into_iter().enumerate() {
            if n > 0 {
                trail = trail.child(
                    crate::icons::Drawn::disclosure(theme, Symbol::ChevronRight)
                        .slot(px(theme.typography.icon()), crate::palette::separator_ink(theme)),
                );
            }
            let here = n == last;
            let crumb = div()
                .id(numbered("folder-crumb", n))
                .debug_selector(move || format!("folder-crumb-{n}"))
                .role(if here { Role::Label } else { Role::Link })
                .aria_label(SharedString::from(label.clone()))
                .px(px(theme.spacing.xxs))
                .rounded(px(theme.radii.xs))
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .map(|el| if here { el.flex_shrink(1.0) } else { el.flex_none() })
                .text_color(hsla(if here { s.text } else { s.text_muted }))
                .child(label);
            let crumb = if here {
                crumb
            } else {
                crumb
                    .cursor_pointer()
                    .hover(move |st| st.text_color(hsla(s.text_secondary)))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.browse(path.clone(), cx);
                    }))
            };
            trail = trail.child(crumb);
        }
        let id = self.id.as_uuid();
        div()
            .id("folder-path")
            .debug_selector(move || format!("folder-path-{id}"))
            .role(Role::Navigation)
            .aria_label(SharedString::from(dir.to_owned()))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .h(px(theme.density.row))
            // A crumb's pad hangs out past the edge grid, so the first one's text stands on
            // the rows' icons and the header's glyph, not a pad's width right of them.
            .pl(px(theme.spacing.inset() - theme.spacing.xxs))
            .pr(px(theme.spacing.inset()))
            .border_b(crate::kit::HAIR)
            .border_color(hsla(s.stroke))
            .text_size(px(theme.typography.small()))
            .child(trail)
            .child(
                crate::kit::meta(crate::kit::tabular(div()), theme)
                    .flex_none()
                    .text_size(px(theme.typography.small()))
                    .child(count_label(total)),
            )
            .when_some(crate::workspace::worktree_root(dir), |bar, root| {
                bar.child(
                    crate::kit::icon_button(
                        theme,
                        format!("folder-remove-worktree-{id}"),
                        Symbol::Trash,
                        crate::workspace::REMOVE_WORKTREE,
                    )
                    .map(|el| {
                        let theme = Rc::new(theme.clone());
                        crate::kit::hint_timing(el).tooltip(move |_window, cx| {
                            let hint = crate::workspace::REMOVE_WORKTREE;
                            cx.new(|_| crate::kit::Hint::new(hint, "", Rc::clone(&theme))).into()
                        })
                    })
                    .on_click(cx.listener(move |_this, _ev, _window, cx| {
                        cx.stop_propagation();
                        cx.emit(FolderViewEvent::RemoveWorktree(root.clone()));
                    })),
                )
            })
            .when(FILES_PICKER, |bar| {
                bar.child(
                    crate::kit::icon_button(
                        theme,
                        format!("folder-upload-{id}"),
                        Symbol::ArrowUpToLine,
                        UPLOAD_FROM_FILES,
                    )
                    .on_click(cx.listener(|this, _ev, _window, cx| {
                        cx.stop_propagation();
                        this.upload_here(cx);
                    })),
                )
            })
            .into_any_element()
    }

    /// Row `ix`: the entry's kind, its name (a hidden one muted), then how big it is and how
    /// long ago it changed, in columns.
    fn row(
        &self,
        ix: usize,
        entry: &FolderEntry,
        now: SystemTime,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let chosen = self.selected == Some(ix);
        let folder = entry.kind == FileKind::Dir;
        let icon = match entry.kind {
            FileKind::Dir => Symbol::Folder.into(),
            FileKind::Symlink => Symbol::Link.into(),
            FileKind::File => crate::icons::file_mark(&entry.name),
            FileKind::Other => Symbol::Doc.into(),
        };
        let mut ink = RowInk::of(theme, entry, chosen);
        let fate = self.fate(&entry.name);
        ink.opacity = match fate {
            None => ink.opacity,
            Some(Fate::Renamed(_)) => ink.opacity.min(ASKED),
            Some(Fate::Leaving) => LEAVING,
        };
        let shown_name = match fate {
            Some(Fate::Renamed(name)) => name,
            _ => entry.name.clone(),
        };
        let detail = if folder {
            entry.items.map(count_label).unwrap_or_default()
        } else if entry.kind == FileKind::File {
            crate::kit::size_label(entry.size)
        } else {
            String::new()
        };
        let age = age(entry.modified_ms.as_millis(), now)
            .map(crate::palette::age_label)
            .unwrap_or_default();
        let column = |text: String, width: f32| {
            crate::kit::meta(crate::kit::tabular(div()), theme)
                .flex_none()
                .w(px(width))
                .text_size(px(theme.typography.small()))
                .text_right()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(text)
        };
        let pad = crate::palette::list_pad(theme);
        let drawn = Rc::clone(&self.drawn);
        let save = (FILES_PICKER && chosen).then(|| {
            crate::kit::icon_button(
                theme,
                format!("folder-save-{}", self.id.as_uuid()),
                Symbol::ArrowDownToLine,
                SAVE_TO_FILES,
            )
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _ev, _window, cx| {
                cx.stop_propagation();
                this.save_selected(cx);
            }))
        });
        let row = Self::row_menu_press(div().id(numbered("folder-row", ix)), ix, cx)
            .debug_selector(move || format!("folder-row-{ix}"))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(entry.name.clone()))
            .aria_selected(chosen)
            .relative()
            .w_full()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .h(px(theme.density.row))
            .px(px(theme.spacing.inset() - pad))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(!chosen, |el| el.hover(move |st| st.bg(hsla(s.hover))))
            .opacity(ink.opacity)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _window, cx| {
                    this.press(ix, ev.position, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, _window, cx| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    this.drag(ev.position, cx);
                }
            }))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _window, cx| {
                this.clicked(ix, ev.click_count(), cx);
            }))
            .child(self.lead(icon, hsla(ink.icon)))
            .child(self.renaming(&entry.name).unwrap_or_else(|| {
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(ink.name))
                    .when(chosen, |el| {
                        el.font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                    })
                    .child(SharedString::from(shown_name))
                    .into_any_element()
            }))
            .when(entry.link && entry.kind != FileKind::Symlink, |el| {
                el.child(
                    crate::icons::icon(theme, Symbol::Link, IconSize::Inline, hsla(s.text_muted))
                        .size(px(IconSize::Inline.slot(theme))),
                )
            })
            .child(column(detail, SIZE_W))
            .child(column(age, AGE_W))
            .children(save)
            .child(
                gpui::canvas(
                    move |bounds, _window, _cx| drawn.borrow_mut().rows.push((ix, bounds)),
                    |_bounds, (), _window, _cx| {},
                )
                .absolute()
                .size_full(),
            );
        if chosen { self.plate.mark(row, ix).into_any_element() } else { row.into_any_element() }
    }

    /// The field in `name`'s row while it is being renamed.
    fn renaming(&self, name: &str) -> Option<AnyElement> {
        match &self.naming.as_ref()?.what {
            Name::Rename { name: at } if at == name => self.name_field(RENAME_OR_MOVE),
            _ => None,
        }
    }

    /// The rows, drawn as far as they are seen: a folder can hold thousands.
    fn list(&self, count: usize, cx: &Context<Self>) -> AnyElement {
        let id = self.id.as_uuid();
        let drawn = Rc::clone(&self.drawn);
        let pad = crate::palette::list_pad(&self.theme);
        let rows = uniform_list(
            "folder-rows",
            count,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                this.drawn_to(range.end, cx);
                let now = SystemTime::now();
                let entries = this.entries();
                range
                    .filter_map(|ix| Some(this.row(ix, entries.get(ix)?, now, cx)))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full()
        // The fill sits the list's pad in from the tile's edges; the text on the edge grid.
        .p(px(pad));
        div()
            .id("folder-list")
            .debug_selector(move || format!("folder-list-{id}"))
            .role(Role::ListBox)
            .aria_label("Entries")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(self.plate.under(&self.theme))
            .child(
                gpui::canvas(
                    move |bounds, _window, _cx| {
                        let mut drawn = drawn.borrow_mut();
                        drawn.list = Some(bounds);
                        drawn.rows.clear();
                    },
                    |_bounds, (), _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .child(rows)
            .into_any_element()
    }

    /// The line under a listing with more to come: how much of the folder the rows are so far.
    fn foot(&self, shown: usize, total: u32) -> Option<AnyElement> {
        let shown = u32::try_from(shown).unwrap_or(u32::MAX);
        if shown >= total {
            return None;
        }
        let theme = &self.theme;
        let id = self.id.as_uuid();
        Some(
            crate::kit::meta(crate::kit::tabular(div()), theme)
                .id("folder-cut")
                .debug_selector(move || format!("folder-cut-{id}"))
                .role(Role::Status)
                .flex_none()
                .flex()
                .items_center()
                .h(px(theme.density.row))
                .px(px(theme.spacing.inset()))
                .border_t(crate::kit::HAIR)
                .border_color(hsla(theme.surfaces.stroke))
                .text_size(px(theme.typography.small()))
                .child(format!("{shown} of {total} listed"))
                .into_any_element(),
        )
    }
}

impl Focusable for FolderView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FolderView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = *self.id.as_uuid();
        // A body without rows has none to be found under a touch.
        *self.drawn.borrow_mut() = Drawn::default();
        let body: Vec<AnyElement> = match self.listing() {
            // Blank while an answer in time would fill it; past the grace, a word.
            None if !crate::screen::past_grace("folder-reading", window, cx) => Vec::new(),
            None => vec![self.notice(Symbol::Folder, crate::file::READING, None, None)],
            Some(Listing::NotFolder) => vec![self.notice(Symbol::Doc, NOT_A_FOLDER, None, None)],
            Some(Listing::Missing { error }) => vec![self.notice(
                Symbol::TextMagnifyingglass,
                CANNOT_LIST,
                Some(SharedString::from(error.clone())),
                None,
            )],
            Some(Listing::Listed { dir, entries, total }) => {
                let mut body = vec![self.path_bar(dir, *total, cx)];
                body.extend(self.new_folder_row());
                let made = self.made();
                let none_made = made.is_empty();
                body.extend(made.into_iter().enumerate().map(|(n, name)| self.made_row(n, name)));
                if entries.is_empty() && none_made {
                    // Nothing to open here: a shell in it is the one next step.
                    let path = self.path.clone();
                    let shell =
                        crate::kit::notice_action(&self.theme, "folder-new-shell", NEW_SHELL)
                            .on_click(cx.listener(move |_this, _ev, _w, cx| {
                                cx.emit(FolderViewEvent::NewShell(path.clone()));
                            }))
                            .into_any_element();
                    body.push(self.notice(Symbol::Folder, EMPTY_FOLDER, None, Some(shell)));
                } else {
                    body.push(self.list(entries.len(), cx));
                    body.extend(self.foot(entries.len(), *total));
                }
                body
            }
        };
        div()
            .id(SharedString::from(format!("folder-{id}")))
            .debug_selector(move || format!("folder-{id}"))
            .key_context(CTX)
            .track_focus(&self.focus)
            .role(Role::Group)
            .aria_label(SharedString::from(format!("Folder {}", self.dir().unwrap_or(&self.path))))
            .aria_value(SharedString::from(self.summary()))
            // While a name is written, the field has the keys: its ↩, ⌫ and arrows edit the
            // name, never open, go up or walk the rows.
            .when(self.naming.is_none(), |el| {
                el.on_action(cx.listener(|this, _: &SelectNext, _window, cx| this.select_by(1, cx)))
                    .on_action(
                        cx.listener(|this, _: &SelectPrevious, _window, cx| this.select_by(-1, cx)),
                    )
                    .on_action(cx.listener(|this, _: &SelectFirst, _window, cx| {
                        this.select_by(isize::MIN, cx);
                    }))
                    .on_action(cx.listener(|this, _: &SelectLast, _window, cx| {
                        this.select_by(isize::MAX, cx);
                    }))
                    .on_action(
                        cx.listener(|this, _: &OpenSelected, _window, cx| this.open_selected(cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &OpenParent, _window, cx| this.open_parent(cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &TrashSelected, _window, cx| this.trash_selected(cx)),
                    )
            })
            .when(crate::file::open_with::offers(self.worker, &self.path, cx), |el| {
                el.on_action(cx.listener(
                    |this, _: &crate::file::open_with::OpenInEditor, _window, cx| {
                        this.open_in_editor(cx);
                    },
                ))
            })
            .on_action(cx.listener(|this, _: &UploadFromFiles, _window, cx| this.upload_here(cx)))
            .on_action(cx.listener(|this, _: &SaveToFiles, _window, cx| this.save_selected(cx)))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| this.new_folder(window, cx)))
            .on_action(cx.listener(|this, _: &RenameSelected, window, cx| {
                this.rename_selected(window, cx);
            }))
            // Esc in the name's field puts it away; the folder has the keyboard again.
            .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                if this.naming.is_some() {
                    cx.stop_propagation();
                    this.finish_naming(false, window, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .font_family(self.theme.typography.ui_family.clone())
            .text_size(px(self.theme.typography.ui_size))
            .children(body)
            .children(self.row_menu_panel(cx))
    }
}

/// Where the list and its rows were drawn, in window points.
#[derive(Debug, Default)]
struct Drawn {
    /// The list, which clips the rows scrolled part out of it.
    list: Option<Bounds<Pixels>>,
    /// Each row drawn, by index.
    rows: Vec<(usize, Bounds<Pixels>)>,
}

/// The palette's lines that send files up and bring them down.
///
/// They are named for the Files picker on iOS, where it stands in for Finder (`ios`), and
/// plainly on a Mac, where the open panel and a picked folder do. Dragging does the same on a
/// Mac; these are its keyboard's way.
#[must_use]
pub fn files_palette_items(
    ios: bool,
    bindings: &[gpui::KeyBinding],
) -> Vec<crate::palette::PaletteItem> {
    let (upload, download) =
        if ios { (UPLOAD_FROM_FILES, SAVE_TO_FILES) } else { (UPLOAD, DOWNLOAD) };
    vec![
        crate::palette::PaletteItem::new(upload, Box::new(UploadFromFiles), bindings),
        crate::palette::PaletteItem::new(download, Box::new(SaveToFiles), bindings),
    ]
}

/// The palette's lines that change a folder's entries: a new folder, a rename or move, the
/// trash.
#[must_use]
pub fn folder_palette_items(bindings: &[gpui::KeyBinding]) -> Vec<crate::palette::PaletteItem> {
    let line = |label: &str, action: Box<dyn gpui::Action>| {
        crate::palette::PaletteItem::new(label, action, bindings)
    };
    vec![
        line(NEW_FOLDER, Box::new(NewFolder)),
        line(RENAME_OR_MOVE, Box::new(RenameSelected)),
        line(MOVE_TO_TRASH, Box::new(TrashSelected)),
    ]
}

/// A change asked from the tile.
#[derive(Debug)]
struct Asked {
    op: FsOp,
    /// Done or lost on the link: the folder's next listing shows how it went.
    answered: bool,
}

/// What a change asked from the tile does to one of its rows until the folder lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fate {
    /// Renamed in place: the row wears the new name.
    Renamed(String),
    /// Moved out of the folder or trashed: the row is set back.
    Leaving,
}

/// A name being written in a folder tile.
struct Naming {
    field: Entity<InputState>,
    what: Name,
    _subscription: Subscription,
}

/// What the name is for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Name {
    /// A folder to make here.
    NewFolder,
    /// A new name for the entry `name` here.
    Rename {
        /// Its name now.
        name: String,
    },
}

/// Where `written` sends the entry `name` of `dir`.
///
/// A plain name renames it in `dir`; a path moves it, relative to `dir` unless it starts at the
/// root or the home, into the folder a trailing `/` names under its own name. `.` and `..` are
/// resolved here, since the worker takes no path that climbs.
#[must_use]
pub fn destination(dir: &str, written: &str, name: &str) -> String {
    let at = if written.starts_with('/') || written.starts_with('~') {
        written.to_owned()
    } else {
        join(dir, written)
    };
    let at = if at.ends_with('/') { join(&at, name) } else { at };
    let (root, rest) = at.strip_prefix('~').map_or(("", at.as_str()), |rest| ("~", rest));
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("{root}/{}", parts.join("/"))
}

/// How a row is inked: its icon, its name, and the whole row's opacity.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct RowInk {
    pub icon: slopty_theme::Rgb,
    pub name: slopty_theme::Rgb,
    pub opacity: f32,
}

impl RowInk {
    /// The icon a tier under its name, and the name's own ink when chosen
    /// (`docs/decisions/ui.md`, "An icon takes its words' size, weight and tier"). A hidden entry
    /// (a dot name, `UF_HIDDEN`) is the whole row set back, as Finder shows one: muted text
    /// alone was the same AA grey as every size and age beside it, so it did not read as
    /// hidden at all.
    pub(crate) const fn of(theme: &Theme, entry: &FolderEntry, chosen: bool) -> Self {
        let s = &theme.surfaces;
        if entry.hidden {
            return Self { icon: s.text_muted, name: s.text_muted, opacity: HIDDEN };
        }
        let icon = if chosen { s.text } else { s.text_secondary };
        Self { icon, name: s.text, opacity: 1.0 }
    }
}

/// How far a hidden row is set back: present, but behind the rest, as a read inbox row is.
const HIDDEN: f32 = slopty_theme::alpha::STRONG;
/// A row a change was asked of, drawn as done before the worker's listing says it is.
const ASKED: f32 = slopty_theme::alpha::STRONG;
/// A row on its way out of the folder: still there until the listing drops it, faded to the
/// edge of being seen.
const LEAVING: f32 = slopty_theme::alpha::RING;

/// An element id for the `n`th of a kind.
fn numbered(kind: &'static str, n: usize) -> gpui::ElementId {
    gpui::ElementId::NamedInteger(kind.into(), u64::try_from(n).unwrap_or(u64::MAX))
}

/// How long ago `modified_ms` (Unix milliseconds) was at `now`; none when the worker gave no
/// time.
fn age(modified_ms: u64, now: SystemTime) -> Option<std::time::Duration> {
    let now = now.duration_since(std::time::UNIX_EPOCH).ok()?;
    (modified_ms > 0).then(|| now.saturating_sub(std::time::Duration::from_millis(modified_ms)))
}

/// `name` in `dir`.
#[must_use]
pub fn join(dir: &str, name: &str) -> String {
    format!("{}/{name}", dir.trim_end_matches('/'))
}

/// The directory `dir` is in; none for the root.
#[must_use]
pub fn parent_of(dir: &str) -> Option<String> {
    let trimmed = dir.trim_end_matches('/');
    let (parent, _) = trimmed.rsplit_once('/')?;
    Some(if parent.is_empty() { "/".to_owned() } else { parent.to_owned() })
}

/// How many entries, as a folder's size is said: `1 item`, `12 items`.
#[must_use]
pub fn count_label(n: u32) -> String {
    if n == 1 { "1 item".to_owned() } else { format!("{n} items") }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;

    use super::*;

    #[test]
    fn a_path_joins_and_goes_up_to_the_root() {
        assert_eq!(join("/w", "src"), "/w/src");
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(parent_of("/w/src").as_deref(), Some("/w"));
        assert_eq!(parent_of("/w").as_deref(), Some("/"));
        assert_eq!(parent_of("/"), None);
        assert_eq!(count_label(1), "1 item");
        assert_eq!(count_label(0), "0 items");
    }

    /// A name written for an entry renames it beside itself; a path moves it, from this
    /// folder unless it starts at the root or the home, under its own name into a folder that
    /// ends in `/`, with `.` and `..` resolved.
    #[test]
    fn a_written_name_renames_and_a_path_moves() {
        assert_eq!(destination("/w", "b.txt", "a.txt"), "/w/b.txt");
        assert_eq!(destination("/w", "docs/b.txt", "a.txt"), "/w/docs/b.txt");
        assert_eq!(destination("/w", "docs/", "a.txt"), "/w/docs/a.txt");
        assert_eq!(destination("/w", "/tmp/", "a.txt"), "/tmp/a.txt");
        assert_eq!(destination("/w", "~/old.txt", "a.txt"), "~/old.txt");
        assert_eq!(destination("/w/src", "../done/", "a.txt"), "/w/done/a.txt");
        assert_eq!(destination("/w", "./b.txt", "a.txt"), "/w/b.txt");
        assert_eq!(destination("/w", "../../../x", "a.txt"), "/x", "no higher than the root");
    }

    /// A hidden entry reads as set back: its name falls well under the contrast of the sizes
    /// and ages in the same list, where muted text alone had matched them.
    #[test]
    fn a_hidden_entry_is_set_back() {
        for variant in [slopty_theme::Variant::Light, slopty_theme::Variant::Dark] {
            let theme = Theme::new(variant);
            let file = |name: &str, hidden| FolderEntry {
                name: name.to_owned(),
                kind: FileKind::File,
                link: false,
                hidden,
                size: 6,
                items: None,
                modified_ms: WallMs::ZERO,
            };
            let plain = RowInk::of(&theme, &file("README.md", false), false);
            let hidden = RowInk::of(&theme, &file(".env", true), false);
            let under = theme.content();
            let seen =
                |ink: slopty_theme::Rgb, opacity: f32| under.mix(ink, opacity).contrast(under);
            let meta = seen(theme.surfaces.text_muted, 1.0);
            let name = seen(hidden.name, hidden.opacity);
            assert!(name * 1.25 < meta, "hidden {name:.2} against the meta's {meta:.2}");
            assert!(name >= 1.8, "still legible: {name:.2}");
            assert!(
                (plain.opacity - 1.0).abs() < f32::EPSILON && plain.name == theme.surfaces.text
            );
        }
    }
}
