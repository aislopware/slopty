//! The composer's other work: its `/` and `@` menus, what is attached to the draft, and
//! changing a message that waits in the queue.
//!
//! - **Menus.** The draft is read as the conversation face reads it ([`menu`]): `/` lists the
//!   commands the thread says its agent takes, `@` the paths under the thread's directory that the
//!   worker's file index matches (`ClientMsg::FindFiles`). While an answer for what is typed now is
//!   on its way, the last answer filtered here stands in for it, so the list never blanks between
//!   keys. ↑/↓ move, ↵ or ⇥ pick, Esc closes the menu until the caret leaves the word. Picking only
//!   writes into the draft.
//! - **Models.** The model chip opens a menu of the models the agent can switch to, then how hard
//!   its model can think; picking one asks the agent to switch (`Intent::SetModel`,
//!   `Intent::SetEffort`), and the chip reads as the agent then says. The mode chip does the same
//!   with the modes the agent publishes (`Intent::SetMode`).
//! - **Place.** A draft in a folder that is a repository switches where it starts from its place
//!   chip: in the folder itself or in a new worktree of it. In a new worktree, its base chip lists
//!   the branches the clone knows (`GitOp::Branches`, asked once as the draft opens), the default
//!   and the one checked out first, the rest newest first; the one picked is the worktree's base
//!   (`NewWorktree::base`).
//! - **Attachments.** A pasted picture, files copied here, a drop on the tile or the picker's files
//!   go up through the workspace to the worker's attachment directory, for every agent; each shows
//!   as a chip ([`crate::conversation::chips`]) until the message goes, which carries their paths
//!   in `Intent::Send::attachments`. The worker gives them to the agent in its own form: a picture
//!   as a picture where the agent takes one, any other file by its path. ↵ while one is still on
//!   its way up arms the message: it goes as soon as the last one lands, and an upload that fails
//!   disarms it, saying so, rather than sending without the file.
//! - **Recall.** ↑ on the first line of an empty composer brings back the message sent before, ↓ on
//!   the last line the one after, past the newest to an empty draft, as a shell does. A recalled
//!   message that is edited is a draft like any other, and the arrows move in it.
//! - **Editing.** A waiting message's words take the composer and the draft is put aside; ↵ sends
//!   the change (`Intent::Edit`) and brings the draft back, Esc brings it back unchanged. A message
//!   that goes meanwhile leaves its words in the composer as a new draft, the one put aside after
//!   them.

use std::collections::HashMap;
use std::sync::Arc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClipboardItem, Context, Focusable as _, InteractiveElement as _,
    IntoElement as _, ParentElement as _, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_kit::component::input::RopeExt as _;
use slopty_proto::git::{Branch, Branches};
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, Command, Delivery, Effort, IntentId, ItemBody, Mode, Model};

use super::{ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::conversation::attach::{self, Attach, Attachment};
use crate::conversation::menu::{self, Token};
use crate::icons::Symbol;
use crate::kit::ButtonKind;

/// Rows the menu shows before it scrolls.
const MENU_ROWS: f32 = 8.0;

/// What the composer's menu lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuRows {
    /// The agent's commands that match.
    Commands(Vec<Command>),
    /// Paths the worker found for an `@` query; `None` while it has not answered.
    Paths(Option<Vec<String>>),
    /// An `@` with nothing after it yet.
    Hint,
    /// The models the agent can switch to, then how hard the model can be set to think, opened
    /// from the model chip: one walk for the keyboard, the models' rows first.
    Models { models: Vec<Model>, efforts: Vec<Effort> },
    /// The modes the agent can switch to, opened from the mode chip.
    Modes(Vec<Mode>),
    /// Where a draft starts, opened from its place chip: [`PLACES`] rows, the folder itself
    /// then a new worktree of it.
    Places,
    /// The branches its new worktree can start from, opened from its base chip, in the order
    /// [`bases`] gives them.
    Bases(Arc<Branches>),
}

/// The rows of a draft's place menu: in the folder itself, in a new worktree of it.
const PLACES: usize = 2;

/// The quiet head over the model menu's levels of effort, after its models.
const EFFORT_HEAD: &str = "Effort";

/// The branches of `branches` in the order a base menu lists them: `origin`'s default first,
/// then the one checked out, then the rest as the worker gave them, the newest first.
pub(super) fn bases(branches: &Branches) -> Vec<&Branch> {
    let lead = |b: &&Branch| {
        if branches.default.as_ref() == Some(&b.name) {
            0
        } else if branches.current.as_ref() == Some(&b.name) {
            1
        } else {
            2
        }
    };
    let mut listed: Vec<&Branch> = branches.list.iter().collect();
    listed.sort_by_key(lead);
    listed
}

impl MenuRows {
    fn len(&self) -> usize {
        match self {
            Self::Commands(commands) => commands.len(),
            Self::Paths(paths) => paths.as_ref().map_or(0, Vec::len),
            Self::Hint => 0,
            Self::Models { models, efforts } => models.len().saturating_add(efforts.len()),
            Self::Modes(modes) => modes.len(),
            Self::Places => PLACES,
            Self::Bases(branches) => branches.list.len(),
        }
    }
}

/// The worker's last answer to an `@` query.
#[derive(Clone, Debug)]
struct Found {
    query: String,
    paths: Vec<String>,
}

/// A waiting message being changed in the composer.
#[derive(Clone, Debug)]
pub(super) struct Editing {
    /// The intent that sent it.
    pub pending: IntentId,
    /// The draft put aside for it.
    pub aside: String,
}

/// The composer's state beyond its text.
#[derive(Debug, Default)]
pub(super) struct Composing {
    /// Esc closed the menu for the word that starts here.
    dismissed: Option<usize>,
    /// The row the keyboard is on.
    selected: usize,
    /// The `@` query last asked of the worker.
    asked: Option<String>,
    found: Option<Found>,
    scroll: ScrollHandle,
    attachments: attach::Attachments,
    /// A pasted picture's chip draws the picture, by attachment.
    pictures: HashMap<u64, Arc<gpui::Image>>,
    editing: Option<Editing>,
    /// The model chip's menu is open.
    models: bool,
    /// The mode chip's menu is open.
    modes: bool,
    /// A draft's place chip's menu is open.
    places: bool,
    /// A draft's base chip's menu is open.
    bases: bool,
    /// What the composer says above the field until the draft changes: an attachment it cannot
    /// take, a message waiting for its uploads.
    notice: Option<String>,
    /// ↵ was pressed while an attachment was on its way up: the message goes, so, in the window
    /// it was pressed in, once the last one lands.
    armed: Option<(Delivery, gpui::AnyWindowHandle)>,
    /// The sent message recalled into the composer, by how far back it is, and its words.
    recall: Option<(usize, String)>,
}

impl Composing {
    /// Whether a waiting message is being changed.
    pub(super) const fn editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Whether an attachment is still on its way up.
    pub(super) fn uploading(&self) -> bool {
        self.attachments.uploading()
    }

    /// Where the attachments landed on the worker, kept on the draft.
    pub(super) fn landed(&self) -> Vec<String> {
        self.attachments.paths()
    }

    /// What answers `query`: the worker's answer to it, else the last answer for a shorter
    /// word it starts with, filtered here; `None` while nothing does.
    fn paths_for(&self, query: &str) -> Option<Vec<String>> {
        let found = self.found.as_ref()?;
        if found.query == query {
            return Some(found.paths.clone());
        }
        if !query.starts_with(&found.query) {
            return None;
        }
        let narrowed: Vec<String> =
            found.paths.iter().filter(|p| crate::picker::matches(query, p)).cloned().collect();
        (!narrowed.is_empty()).then_some(narrowed)
    }
}

impl ThreadView {
    // ----- menus -----------------------------------------------------------------------

    /// The directory the agent works in, which `@` paths are under.
    fn root(&self, cx: &App) -> Option<String> {
        self.state(cx).map(|s| s.meta.cwd.clone()).filter(|cwd| !cwd.is_empty())
    }

    /// Whether the composer's field has the keyboard.
    pub(super) fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.focus_handle(cx).is_focused(window)
    }

    /// Whether an input method holds uncommitted text in the composer: the keys it reads then
    /// are its own.
    pub(super) fn composing(&self, cx: &App) -> bool {
        self.composer.read(cx).is_composing()
    }

    /// The word the caret is in, when it asks for a menu the person has not closed.
    fn menu_token(&self, cx: &App) -> Option<Token> {
        let composer = self.composer.read(cx);
        let token = menu::token(&composer.value(), composer.cursor())?;
        (self.composing.dismissed != Some(token.start())).then_some(token)
    }

    /// What the menu lists now, if it is open.
    pub(super) fn menu_rows(&self, cx: &App) -> Option<MenuRows> {
        if self.composing.models {
            let (models, efforts) = self.switches(cx);
            let open = !models.is_empty() || !efforts.is_empty();
            return open.then_some(MenuRows::Models { models, efforts });
        }
        if self.composing.modes {
            let modes = self.state(cx).map(|s| s.meta.modes.clone()).unwrap_or_default();
            return (!modes.is_empty()).then_some(MenuRows::Modes(modes));
        }
        if self.composing.places {
            return self.place_switch(cx).map(|_| MenuRows::Places);
        }
        if self.composing.bases {
            return self.base_switch(cx).map(MenuRows::Bases);
        }
        match self.menu_token(cx)? {
            Token::Command { query } => {
                let all = self.commands(cx);
                let found: Vec<Command> =
                    menu::commands(&all, &query).into_iter().cloned().collect();
                (!found.is_empty()).then_some(MenuRows::Commands(found))
            }
            Token::Mention { query, .. } => {
                self.root(cx)?;
                if query.is_empty() {
                    return Some(MenuRows::Hint);
                }
                Some(MenuRows::Paths(self.composing.paths_for(&query)))
            }
        }
    }

    /// The commands the `/` menu lists: the agent's own.
    #[must_use]
    pub fn commands(&self, cx: &App) -> Vec<Command> {
        self.state(cx).map(|state| state.commands.clone()).unwrap_or_default()
    }

    /// The draft or its caret moved: the menu's row goes back to the top, a word the person
    /// closed the menu on is forgotten once the caret left it, and an `@` word asks the worker
    /// for what it now matches. What the composer said about an attachment has been read.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(ThreadViewEvent::Drafted);
        self.draft_changed(cx);
        if self.composing.armed.is_none() {
            self.composing.notice = None;
        }
        let token = {
            let composer = self.composer.read(cx);
            let value = composer.value();
            if self.composing.recall.as_ref().is_some_and(|(_, words)| *words != *value) {
                self.composing.recall = None;
            }
            menu::token(&value, composer.cursor())
        };
        let menu = &mut self.composing;
        if menu.dismissed.is_some_and(|at| token.as_ref().is_none_or(|t| t.start() != at)) {
            menu.dismissed = None;
        }
        menu.selected = 0;
        if let Some(Token::Mention { query, .. }) = token
            && !query.is_empty()
            && menu.asked.as_deref() != Some(query.as_str())
            && let Some(root) = self.root(cx)
        {
            self.composing.asked = Some(query.clone());
            cx.emit(ThreadViewEvent::FindFiles { root, query });
        }
        cx.notify();
    }

    /// The worker found `paths` under `root` for `query`: the menu lists them while that is
    /// what the person is typing, or the start of it.
    pub fn files_found(
        &mut self,
        root: &str,
        query: &str,
        paths: &[String],
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.aside.as_ref().and_then(super::aside::Aside::view) {
            view.update(cx, |v, cx| v.files_found(root, query, paths, cx));
        }
        let asked = self.composing.asked.as_deref();
        if self.root(cx).as_deref() != Some(root) || asked.is_none_or(|a| !a.starts_with(query)) {
            return;
        }
        let longer = self.composing.found.as_ref().is_some_and(|f| f.query.len() > query.len());
        if !longer {
            let (query, paths) = (query.to_owned(), paths.to_vec());
            self.composing.found = Some(Found { query, paths });
            cx.notify();
        }
    }

    /// ↑ (`-1`) or ↓ (`1`) in the open menu, round. Whether a menu took the key.
    pub(super) fn menu_step(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(rows) = self.menu_rows(cx) else { return false };
        let count = rows.len();
        if count == 0 {
            return true;
        }
        let menu = &mut self.composing;
        menu.selected = if delta < 0 {
            menu.selected.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1))
        } else if menu.selected.saturating_add(1) < count {
            menu.selected.saturating_add(1)
        } else {
            0
        };
        menu.scroll.scroll_to_item(menu.selected);
        cx.notify();
        true
    }

    /// ↵ or ⇥ in the open menu: pick the row the keyboard is on. Whether a menu took the key.
    pub(super) fn menu_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(rows) = self.menu_rows(cx) else { return false };
        if rows.len() == 0 {
            return false;
        }
        // A new answer from the worker may have shortened the list under the keyboard's row.
        let ix = self.composing.selected.min(rows.len().saturating_sub(1));
        self.menu_pick(ix, window, cx);
        true
    }

    /// Esc with the menu open closes it for the word the caret is in. Whether it was open.
    pub(super) fn menu_close(&mut self, cx: &mut Context<Self>) -> bool {
        let c = &self.composing;
        if c.models || c.modes || c.places || c.bases {
            self.close_chip_menus();
            cx.notify();
            return true;
        }
        let Some(token) = self.menu_token(cx) else { return false };
        self.composing.dismissed = Some(token.start());
        cx.notify();
        true
    }

    /// Pick row `ix` of the open menu: a command or a path written into the draft.
    pub fn menu_pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rows) = self.menu_rows(cx) else { return };
        let (text, caret) = {
            let composer = self.composer.read(cx);
            (composer.value().to_string(), composer.cursor())
        };
        let written = match (rows, self.menu_token(cx)) {
            (MenuRows::Models { models, efforts }, _) => {
                let intent = match ix.checked_sub(models.len()) {
                    None => models.get(ix).map(|m| Intent::SetModel { model: m.id.clone() }),
                    Some(at) => efforts.get(at).map(|e| Intent::SetEffort { effort: e.id.clone() }),
                };
                let Some(intent) = intent else { return };
                self.composing.models = false;
                let _id = self.intent(intent, cx);
                // The keyboard goes back to the field, to write on or send.
                self.focus(window, cx);
                cx.notify();
                return;
            }
            (MenuRows::Modes(modes), _) => {
                let Some(mode) = modes.get(ix) else { return };
                self.composing.modes = false;
                let _id = self.intent(Intent::SetMode { mode: mode.id.clone() }, cx);
                // The keyboard goes back to the field, to write on or send.
                self.focus(window, cx);
                cx.notify();
                return;
            }
            (MenuRows::Places, _) => {
                self.composing.places = false;
                if let Some(draft) = &self.draft {
                    draft.update(cx, |d, cx| d.set_worktree(ix > 0, cx));
                }
                self.focus(window, cx);
                cx.notify();
                return;
            }
            (MenuRows::Bases(branches), _) => {
                let Some(base) = bases(&branches).get(ix).map(|b| b.name.clone()) else { return };
                self.composing.bases = false;
                if let Some(draft) = &self.draft {
                    draft.update(cx, |d, cx| d.set_base(Some(base), cx));
                }
                self.focus(window, cx);
                cx.notify();
                return;
            }
            (MenuRows::Commands(commands), _) => {
                let Some(command) = commands.get(ix) else { return };
                menu::pick_command(&text, &command.name)
            }
            (MenuRows::Paths(Some(paths)), Some(Token::Mention { at, .. })) => {
                let Some(path) = paths.get(ix) else { return };
                menu::pick_path(&text, at, caret, path)
            }
            _ => return,
        };
        self.set_draft(&written.0, written.1, window, cx);
    }

    /// Put `text` at the end of the draft, a blank line after what is there, and give the
    /// composer the keyboard: what a terminal block attached as context lands as.
    pub(crate) fn quote_into_draft(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let joined = match self.draft(cx).trim_end() {
            "" => text.to_owned(),
            before => format!("{before}\n\n{text}"),
        };
        self.set_draft(&joined, joined.len(), window, cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// The messages sent in this thread, newest first, each once where it was sent twice running.
    /// One the agent cut short is left out: its head is not what was typed.
    fn sent_messages(&self, cx: &App) -> Vec<String> {
        // A draft has sent nothing yet: ↑ brings back the first messages of earlier starts.
        if let Some(draft) = &self.draft {
            return draft.read(cx).recall().to_vec();
        }
        let Some(state) = self.state(cx) else { return Vec::new() };
        let mut sent: Vec<String> = state
            .items
            .iter()
            .rev()
            .filter_map(|item| match &item.body {
                ItemBody::User(message) if message.text.full.is_none() => {
                    Some(message.text.text.clone())
                }
                _ => None,
            })
            .filter(|words| !words.trim().is_empty())
            .collect();
        sent.dedup();
        sent
    }

    /// ↑ (`-1`) on the draft's first line or ↓ (`1`) on its last, with the composer empty or
    /// holding a recalled message: the message sent before or after it. Whether recall took
    /// the key.
    pub(super) fn recall(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (draft, caret) = {
            let composer = self.composer.read(cx);
            (composer.value().to_string(), composer.cursor())
        };
        let at = match &self.composing.recall {
            Some((at, words)) if *words == draft => Some(*at),
            _ if draft.is_empty() => None,
            _ => return false,
        };
        let (before, after) = draft.split_at(caret.min(draft.len()));
        if (delta < 0 && before.contains('\n')) || (delta > 0 && after.contains('\n')) {
            return false;
        }
        let next = match (at, delta < 0) {
            (None, true) => 0,
            (Some(at), true) => at.saturating_add(1),
            (None, false) => return false,
            (Some(0), false) => {
                self.composing.recall = None;
                self.set_draft("", 0, window, cx);
                return true;
            }
            (Some(at), false) => at.saturating_sub(1),
        };
        let Some(words) = self.sent_messages(cx).into_iter().nth(next) else {
            return at.is_some();
        };
        self.set_draft(&words, words.len(), window, cx);
        self.composing.recall = Some((next, words));
        true
    }

    /// Put `text` in the composer with the caret at byte `caret`, and bring the menus in step.
    pub(super) fn set_draft(
        &mut self,
        text: &str,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.update(cx, |c, cx| {
            c.set_value(text.to_owned(), window, cx);
            let position = c.text().offset_to_position(caret);
            c.set_cursor_position(position, window, cx);
        });
        self.composer_changed(cx);
    }

    /// The menu, as the composer's section over the field.
    pub(super) fn menu_section(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let rows = self.menu_rows(cx)?;
        let theme = &self.theme;
        let label = match &rows {
            MenuRows::Commands(_) => "Commands",
            MenuRows::Models { efforts, .. } if efforts.is_empty() => "Models",
            MenuRows::Models { models, .. } if models.is_empty() => "Effort",
            MenuRows::Models { .. } => "Model and effort",
            MenuRows::Modes(_) => "Modes",
            MenuRows::Places => "Place",
            MenuRows::Bases(_) => "Base branch",
            MenuRows::Paths(_) | MenuRows::Hint => "Files",
        };
        let body: Vec<AnyElement> = match &rows {
            MenuRows::Commands(commands) => commands
                .iter()
                .enumerate()
                .map(|(ix, command)| self.command_row(ix, command, cx))
                .collect(),
            MenuRows::Paths(Some(paths)) if paths.is_empty() => {
                vec![self.menu_note("No matching files or folders")]
            }
            MenuRows::Paths(Some(paths)) => {
                paths.iter().enumerate().map(|(ix, path)| self.path_row(ix, path, cx)).collect()
            }
            MenuRows::Paths(None) => vec![self.menu_note("Searching…")],
            MenuRows::Hint => vec![self.menu_note("Type to find a file or folder")],
            MenuRows::Modes(modes) => {
                let now = self.state(cx).and_then(|s| s.meters.mode.clone());
                modes
                    .iter()
                    .enumerate()
                    .map(|(ix, mode)| {
                        let Mode { id, label, description } = mode;
                        self.named_row(ix, (id, label, description.as_deref()), now.as_deref(), cx)
                    })
                    .collect()
            }
            MenuRows::Models { models, efforts } => self.model_rows(models, efforts, cx),
            MenuRows::Places => self.place_rows(cx),
            MenuRows::Bases(branches) => {
                let now = self.draft_base(cx);
                bases(branches)
                    .into_iter()
                    .enumerate()
                    .map(|(ix, branch)| {
                        let said = base_said(branches, branch);
                        let row = (&branch.name, &branch.name, Some(said.as_str()));
                        self.named_row(ix, row, now.as_deref(), cx)
                    })
                    .collect()
            }
        };
        Some(
            div()
                .id("thread-menu")
                .debug_selector(|| "thread-menu".to_owned())
                .role(Role::ListBox)
                .aria_label(label)
                .max_h(px(MENU_ROWS * theme.density.row))
                .overflow_y_scroll()
                .track_scroll(&self.composing.scroll)
                .flex()
                .flex_col()
                .pb(px(theme.spacing.xs))
                .text_color(hsla(theme.surfaces.text))
                .children(body)
                .into_any_element(),
        )
    }

    /// A quiet line in the menu's place: nothing matched, or the worker is looking.
    fn menu_note(&self, text: &'static str) -> AnyElement {
        let theme = &self.theme;
        div()
            .debug_selector(|| "thread-menu-note".to_owned())
            .h(px(theme.density.row))
            .flex()
            .items_center()
            .px(px(theme.spacing.sm))
            .text_size(px(theme.typography.small()))
            .text_color(hsla(theme.surfaces.text_muted))
            .child(text)
            .into_any_element()
    }

    /// One row of the menu, the keyboard's filled, a click picking it.
    pub(super) fn menu_row(
        &self,
        ix: usize,
        label: String,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let selected = ix == self.composing.selected;
        div()
            .id(("thread-menu-row", ix))
            .debug_selector(move || format!("thread-menu-{ix}"))
            .role(Role::ListBoxOption)
            .aria_label(SharedString::from(label))
            .aria_selected(selected)
            .flex_none()
            .h(px(theme.density.row))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(selected, |el| el.bg(hsla(s.selected)))
            .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.hover))))
            .on_click(cx.listener(move |this, _ev, window, cx| this.menu_pick(ix, window, cx)))
    }

    /// "Opens in the terminal" beside a command of Claude Code's own that shows a dialog there.
    fn dialog_label(&self, command: &Command, cx: &App) -> Option<String> {
        let claude = self
            .state(cx)
            .is_some_and(|st| st.meta.agent.0 == slopty_proto::thread::AgentId::CLAUDE_CODE);
        (claude && menu::may_open_dialog(command)).then(|| "Opens in the terminal".to_owned())
    }

    fn command_row(&self, ix: usize, command: &Command, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let name = format!("/{}", command.name);
        let hint = command.argument_hint.clone().filter(|h| !h.is_empty());
        self.menu_row(ix, format!("{name} {}", command.description), cx)
            .child(
                div()
                    .flex_none()
                    .font_family(self.mono())
                    .text_size(px(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .children(hint.map(|hint| {
                div()
                    .flex_none()
                    .font_family(self.mono())
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(hint))
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(command.description.clone())),
            )
            .children(self.dialog_label(command, cx).or_else(|| menu::source_label(command)).map(
                |source| {
                    div()
                        .flex_none()
                        .text_size(px(theme.typography.small()))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(source))
                },
            ))
            .into_any_element()
    }

    /// Shut the menus the composer's foot opens: the models and efforts, the modes, a draft's
    /// place and base. The keyboard starts the next one from its first row.
    pub(super) const fn close_chip_menus(&mut self) {
        self.composing.models = false;
        self.composing.modes = false;
        self.composing.places = false;
        self.composing.bases = false;
        self.composing.selected = 0;
    }

    /// The mode chip's menu open or shut; open, the keyboard walks it from its first row.
    pub(super) fn toggle_modes(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.modes;
        self.close_chip_menus();
        self.composing.modes = open;
        self.composing.selected = 0;
        cx.notify();
    }

    /// A mode or an effort the agent can switch to, by its `(id, label, description)`, with
    /// what it does in the agent's words, and a check on the one it is in (by its id or its
    /// name, as the agent says it).
    fn named_row(
        &self,
        ix: usize,
        (id, label, description): (&String, &String, Option<&str>),
        now: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let current = now.is_some_and(|n| n == id || n == label);
        let label = if label.trim().is_empty() { id } else { label };
        self.menu_row(ix, label.clone(), cx)
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .child(SharedString::from(label.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(description.map(|d| SharedString::from(d.to_owned()))),
            )
            .when(current, |el| el.child(self.icon(Symbol::Checkmark, s.text_secondary)))
            .into_any_element()
    }

    /// A draft's place chip's menu open or shut; open, the keyboard walks it from its first
    /// row.
    pub(super) fn toggle_places(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.places;
        self.close_chip_menus();
        self.composing.places = open;
        cx.notify();
    }

    /// A draft's base chip's menu open or shut; open, the keyboard walks it from its first
    /// row.
    pub(super) fn toggle_bases(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.bases;
        self.close_chip_menus();
        self.composing.bases = open;
        cx.notify();
    }

    /// The branches of the repository a draft's folder is in, when it can switch where it
    /// starts: a draft not sent, with no pull request to check out, whose folder the machine
    /// read as a repository ([`super::super::git::Repo::branches`]).
    pub(super) fn place_switch(&self, cx: &App) -> Option<Arc<Branches>> {
        let draft = self.draft.as_ref()?.read(cx);
        if draft.sent() || draft.place().pull.is_some() {
            return None;
        }
        let cwd = &draft.state().meta.cwd;
        self.hub.read(cx).git().repo(cwd)?.branches.clone()
    }

    /// The branches a draft's new worktree can start from, while it starts in one.
    pub(super) fn base_switch(&self, cx: &App) -> Option<Arc<Branches>> {
        let worktree = self.draft.as_ref()?.read(cx).place().worktree;
        self.place_switch(cx).filter(|b| worktree && !b.list.is_empty())
    }

    /// The branch a draft's new worktree starts from: the one chosen, else the one its clone
    /// has checked out.
    pub(super) fn draft_base(&self, cx: &App) -> Option<String> {
        let chosen = self.draft.as_ref()?.read(cx).place().base.clone();
        chosen.or_else(|| self.place_switch(cx)?.current.clone())
    }

    /// The place menu's rows: the folder itself, on the branch its clone has checked out, then
    /// a new worktree of it; a check on where the draft starts.
    fn place_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(draft) = &self.draft else { return Vec::new() };
        let place = draft.read(cx).place().clone();
        let folder = place.folder.clone().unwrap_or_else(|| "the folder".to_owned());
        let current = self.place_switch(cx).and_then(|b| b.current.clone());
        let on = current.map_or_else(|| "its checked-out commit".to_owned(), |b| format!("on {b}"));
        let rows = [
            (FOLDER_ROW.to_owned(), format!("In {folder}"), on),
            (WORKTREE_ROW.to_owned(), NEW_WORKTREE.to_owned(), "a branch of its own".to_owned()),
        ];
        let now = if place.worktree { WORKTREE_ROW } else { FOLDER_ROW };
        rows.iter()
            .enumerate()
            .map(|(ix, (id, label, said))| {
                self.named_row(ix, (id, label, Some(said.as_str())), Some(now), cx)
            })
            .collect()
    }

    /// The model chip's menu open or shut; open, the keyboard walks it from its first row.
    pub(super) fn toggle_models(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.models;
        self.close_chip_menus();
        self.composing.models = open;
        self.composing.selected = 0;
        cx.notify();
    }

    /// What the model chip's menu offers: the models the agent can switch to, and the levels
    /// its model can think at where it can switch those ([`Cap::SET_EFFORT`]).
    pub(super) fn switches(&self, cx: &App) -> (Vec<Model>, Vec<Effort>) {
        let Some(meta) = self.state(cx).map(|s| &s.meta) else { return (Vec::new(), Vec::new()) };
        let models = if meta.can(Cap::SET_MODEL) { meta.models.clone() } else { Vec::new() };
        let efforts = if meta.can(Cap::SET_EFFORT) { meta.efforts.clone() } else { Vec::new() };
        (models, efforts)
    }

    /// The model chip's menu: the models, a check on the one it runs, then under a quiet
    /// "Effort" the levels, a check on the one it thinks at. One walk for the keyboard.
    fn model_rows(
        &self,
        models: &[Model],
        efforts: &[Effort],
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = &self.theme;
        let meters = self.state(cx).map(|s| &s.meters);
        let model = meters.and_then(|m| m.model_id.clone());
        let effort = meters.and_then(|m| m.effort.clone());
        let mut rows: Vec<AnyElement> = models
            .iter()
            .enumerate()
            .map(|(ix, m)| self.model_row(ix, m, model.as_deref(), cx))
            .collect();
        if !models.is_empty() && !efforts.is_empty() {
            rows.push(
                div()
                    .debug_selector(|| "thread-menu-effort".to_owned())
                    .flex_none()
                    .h(px(theme.density.row))
                    .flex()
                    .items_end()
                    .px(px(theme.spacing.sm))
                    .pb(px(theme.spacing.xxs))
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(theme.surfaces.text_muted))
                    .child(EFFORT_HEAD)
                    .into_any_element(),
            );
        }
        rows.extend(efforts.iter().enumerate().map(|(at, e)| {
            let Effort { id, label, description } = e;
            let ix = models.len().saturating_add(at);
            self.named_row(ix, (id, label, description.as_deref()), effort.as_deref(), cx)
        }));
        rows
    }

    /// A model the agent can switch to, with a check on the one it runs.
    fn model_row(
        &self,
        ix: usize,
        model: &Model,
        now: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let current = now == Some(model.id.as_str());
        let label = if model.label.trim().is_empty() { &model.id } else { &model.label };
        let (name, provider) = crate::conversation::figures::spoken_model(label);
        self.menu_row(ix, label.clone(), cx)
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.7))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(provider.map(SharedString::from)),
            )
            .when(current, |el| el.child(self.icon(Symbol::Checkmark, s.text_secondary)))
            .into_any_element()
    }

    fn path_row(&self, ix: usize, path: &str, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let folder = path.ends_with('/');
        let trimmed = path.trim_end_matches('/');
        let (dir, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        let name = if folder { format!("{name}/") } else { name.to_owned() };
        self.menu_row(ix, path.to_owned(), cx)
            .child(self.icon(if folder { Symbol::Folder } else { Symbol::Doc }, s.text_muted))
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.6))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(dir.to_owned())),
            )
            .into_any_element()
    }

    // ----- attachments -----------------------------------------------------------------

    /// A paste into the composer: a picture with no text on the clipboard, or files copied
    /// here, are attached rather than pasted as text. Whether the paste was taken.
    pub fn paste_attachment(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        Attach::of_paste(item).map(|what| self.attach(what, cx)).is_some()
    }

    /// Say `words` above the field until the draft changes.
    fn say(&mut self, words: String, cx: &mut Context<Self>) {
        self.composing.notice = Some(words);
        cx.notify();
    }

    /// Show `what`'s chip and ask the workspace to send it up.
    pub fn attach(&mut self, what: Attach, cx: &mut Context<Self>) {
        let id = self.composing.attachments.add(what.name());
        if let Some(picture) = what.picture() {
            self.composing.pictures.insert(id, picture);
        }
        cx.emit(ThreadViewEvent::Attach { id, what });
        cx.notify();
    }

    /// Show the chip of `what`, which the workspace sends up itself (a drop on the tile), and
    /// say which it is.
    pub fn start_attachment(&mut self, what: &Attach) -> u64 {
        self.composing.attachments.add(what.name())
    }

    /// The chips of what is attached to the draft.
    #[must_use]
    pub fn attachments(&self) -> &[Attachment] {
        self.composing.attachments.chips()
    }

    /// Attachment `id` is `fraction` of the way up.
    pub fn attachment_progress(&mut self, id: u64, fraction: f32, cx: &mut Context<Self>) {
        if self.composing.attachments.progress(id, fraction) {
            cx.notify();
        }
    }

    /// Attachment `id` landed at `paths` on the worker: its chip stays until the message goes,
    /// which goes now if it was waiting for this one. Landing nowhere, it did not land.
    pub fn attachment_landed(&mut self, id: u64, paths: &[String], cx: &mut Context<Self>) {
        if self.composing.attachments.land(id, paths) {
            if paths.is_empty() {
                self.composing.pictures.remove(&id);
                self.upload_failed(cx);
            }
            self.send_armed(cx);
            cx.notify();
        }
    }

    /// Attachment `id`'s upload is over: landed, its chip stays for the message; not, it goes,
    /// and a message waiting for it waits no more.
    pub fn attachment_ended(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.composing.attachments.over(id) {
            self.composing.pictures.remove(&id);
            self.upload_failed(cx);
            cx.notify();
        }
        self.send_armed(cx);
    }

    /// The person takes attachment `id` off the draft: its chip goes at once, and the
    /// workspace stops its upload. A message waiting only for it goes now.
    pub fn detach(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.composing.attachments.end(id) {
            self.composing.pictures.remove(&id);
            cx.emit(ThreadViewEvent::Detach { id });
            self.send_armed(cx);
            cx.notify();
        }
    }

    /// ↵ while an attachment is on its way up: the message goes as `delivery` once the last
    /// one lands, and says so meanwhile.
    pub(super) fn arm(&mut self, delivery: Delivery, window: &Window, cx: &mut Context<Self>) {
        self.composing.armed = Some((delivery, window.window_handle()));
        self.say("Sends once the attachments are up".to_owned(), cx);
    }

    /// An upload ended without landing: a message waiting for it is not sent without it.
    fn upload_failed(&mut self, cx: &mut Context<Self>) {
        if self.composing.armed.take().is_some() {
            self.say("An attachment didn't upload, so nothing was sent".to_owned(), cx);
        }
    }

    /// The armed message goes, once nothing is on its way up.
    fn send_armed(&mut self, cx: &Context<Self>) {
        if self.composing.attachments.uploading() {
            return;
        }
        let Some((delivery, window)) = self.composing.armed.take() else { return };
        self.composing.notice = None;
        // Sending clears the field, which takes the window: had in a task of its own, since an
        // entity's update cannot reach its window.
        cx.spawn(async move |this, cx| {
            let _gone = window.update(cx, |_root, window, cx| {
                this.update(cx, |this, cx| this.submit(delivery, window, cx))
            });
        })
        .detach();
    }

    /// Whether a message waits for its attachments.
    #[must_use]
    pub const fn armed(&self) -> bool {
        self.composing.armed.is_some()
    }

    /// What the composer says above the field, if anything.
    #[must_use]
    pub fn composer_notice(&self) -> Option<&str> {
        self.composing.notice.as_deref()
    }

    /// The line over the field saying what the composer could not do, or waits for.
    pub(super) fn notice_strip(&self) -> Option<AnyElement> {
        let words = self.composing.notice.clone()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let waiting = self.composing.armed.is_some();
        Some(
            div()
                .id("thread-composer-notice")
                .debug_selector(|| "thread-composer-notice".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(if waiting {
                    self.spinner(true)
                } else {
                    self.icon(Symbol::InfoCircle, s.text_muted)
                })
                .child(div().min_w_0().flex_1().child(SharedString::from(words)))
                .into_any_element(),
        )
    }

    /// The message to send and where its attachments landed on the worker; `None` while there
    /// is nothing to send or an attachment is still on its way up.
    pub(super) fn take_message(&mut self, cx: &App) -> Option<(String, Vec<String>)> {
        let attachments = &self.composing.attachments;
        if attachments.uploading() {
            return None;
        }
        let text = self.draft(cx).trim().to_owned();
        let paths = attachments.paths();
        if text.is_empty() && paths.is_empty() {
            return None;
        }
        self.composing.attachments.clear();
        self.composing.pictures.clear();
        self.composing.asked = None;
        self.composing.found = None;
        self.composing.notice = None;
        Some((text, paths))
    }

    /// The chips over the field.
    pub(super) fn attachment_chips(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.composing.editing.is_some() {
            return None;
        }
        crate::conversation::chips::row(
            &self.theme,
            self.attachments(),
            &|id| self.composing.pictures.get(&id).cloned(),
            &|id| Box::new(cx.listener(move |this, _ev, _w, cx| this.detach(id, cx))),
        )
    }

    // ----- editing a waiting message ---------------------------------------------------

    /// Change waiting message `pending`, which reads `text`: its words take the composer.
    pub(super) fn start_edit(
        &mut self,
        pending: IntentId,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A refusal of an earlier change to it goes: this one speaks for it now.
        let refused: Vec<IntentId> = self
            .hub
            .read(cx)
            .threads()
            .unshown(self.thread)
            .filter(|s| s.failed())
            .filter(|s| matches!(&s.intent, Intent::Edit { pending: p, .. } if *p == pending))
            .map(|s| s.id)
            .collect();
        for id in refused {
            self.dismiss(id, cx);
        }
        let aside = match self.composing.editing.take() {
            Some(editing) => editing.aside,
            None => self.draft(cx),
        };
        self.composing.editing = Some(Editing { pending, aside });
        self.set_draft(text, text.len(), window, cx);
        self.composer.update(cx, |c, cx| c.focus(window, cx));
    }

    /// ↵ while editing: the change goes, and the draft put aside comes back. Nothing goes for
    /// words left as they were; an emptied message stays in the composer, since taking a
    /// message back is the line's ✕.
    pub(super) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.composing.editing.clone() else { return };
        let text = self.draft(cx).trim().to_owned();
        if text.is_empty() {
            return;
        }
        let was = self.queued_text(editing.pending, cx);
        if was.as_deref() != Some(text.as_str()) {
            let _id = self.intent(Intent::Edit { pending: editing.pending, text }, cx);
        }
        self.composing.editing = None;
        self.set_draft(&editing.aside, editing.aside.len(), window, cx);
    }

    /// Esc while editing: the draft put aside comes back, the message as it was. Whether a
    /// message was being changed.
    pub(super) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.composing.editing.take() else { return false };
        self.set_draft(&editing.aside, editing.aside.len(), window, cx);
        true
    }

    /// Before a frame: a message that went while it was being changed leaves its words in the
    /// composer as a new draft, the one put aside after them.
    pub(super) fn settle_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = &self.composing.editing else { return };
        let waiting =
            self.state(cx).is_some_and(|s| s.pending.iter().any(|p| p.intent == editing.pending));
        if waiting {
            return;
        }
        let Some(editing) = self.composing.editing.take() else { return };
        let words = self.draft(cx);
        let draft = if editing.aside.trim().is_empty() {
            words
        } else {
            format!("{}\n\n{}", words.trim_end(), editing.aside)
        };
        self.set_draft(&draft, draft.len(), window, cx);
    }

    /// What waiting message `pending` says as this client shows it.
    fn queued_text(&self, pending: IntentId, cx: &App) -> Option<String> {
        let hub = self.hub.read(cx);
        let state = self.state(cx)?;
        let bar =
            crate::conversation::thread::activity::Activity::of(hub.threads(), self.thread, state);
        bar.queue.into_iter().find(|q| q.intent == pending).map(|q| q.text)
    }

    /// The line over the field while a waiting message is being changed.
    pub(super) fn editing_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.composing.editing.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        Some(
            div()
                .id("thread-editing")
                .debug_selector(|| "thread-editing".to_owned())
                .role(Role::Status)
                .aria_label("Editing a queued message")
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(self.icon(Symbol::Pencil, s.text_muted))
                .child(div().flex_1().child("Editing a queued message"))
                .child(self.button("thread-edit-cancel", "Cancel", ButtonKind::Ghost).on_click(
                    cx.listener(|this, _ev, window, cx| {
                        let _was = this.cancel_edit(window, cx);
                    }),
                ))
                .into_any_element(),
        )
    }
}

/// The place menu's row for the folder itself, by its id.
const FOLDER_ROW: &str = "folder";

/// The place menu's row for a new worktree, by its id.
const WORKTREE_ROW: &str = "worktree";

/// The place menu's row for a new worktree, as it reads.
pub(super) const NEW_WORKTREE: &str = "In a new worktree";

/// What a base menu says of `branch` beside its name: `origin`'s default, the one checked out,
/// or that only `origin` has it, in that order of note.
fn base_said(branches: &Branches, branch: &Branch) -> String {
    let marks = [
        (branches.default.as_ref() == Some(&branch.name)).then_some("default"),
        (branches.current.as_ref() == Some(&branch.name)).then_some("checked out"),
        (!branch.local).then_some("on origin"),
    ];
    marks.into_iter().flatten().collect::<Vec<_>>().join(" \u{b7} ")
}
