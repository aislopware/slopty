//! The composer's other work: its `/` and `@` menus, what is attached to the draft, and
//! changing a message that waits in the queue.
//!
//! - **Menus.** The draft is read as the conversation face reads it ([`menu`]): `/` lists the
//!   commands the thread says its agent takes, `@` the paths under the thread's directory that the
//!   worker's file index matches (`ClientMsg::FindFiles`). While an answer for what is typed now is
//!   on its way, the last answer filtered here stands in for it, so the list never blanks between
//!   keys. ↑/↓ move, ↵ or ⇥ pick, Esc closes the menu until the caret leaves the word. Picking only
//!   writes into the draft.
//! - **Models.** The model chip opens a menu of the models the agent can switch to; picking one
//!   asks the agent to switch (`Intent::SetModel`), and the chip reads as the agent then says. The
//!   mode chip does the same with the modes the agent publishes (`Intent::SetMode`).
//! - **Attachments.** A pasted picture, files copied here, a drop on the tile or the picker's files
//!   go up through the workspace as a drop on the face does; each shows as a chip
//!   ([`crate::conversation::chips`]) until the message goes, which carries their paths after its
//!   text ([`composer::with_paths`]). Files go up through the terminal the thread's agent runs in,
//!   so a thread with none (Codex, pi, an ACP agent) takes no attachment: the composer says so in
//!   words, and no chip waits for an upload that never starts. ↵ while one is still on its way up
//!   arms the message: it goes as soon as the last one lands, and an upload that fails disarms it,
//!   saying so, rather than sending without the file.
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
    StatefulInteractiveElement as _, Styled as _, Window, div,
};
use gpui_kit::component::input::RopeExt as _;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, Command, Delivery, IntentId, ItemBody, Mode, Model};

use super::{ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::conversation::composer::{self, Attach, Attachment};
use crate::conversation::menu::{self, Token};
use crate::icons::IconName;
use crate::kit::ButtonKind;

/// Rows the menu shows before it scrolls.
const MENU_ROWS: f32 = 8.0;

/// The command that compacts the context, as the menu lists it.
const COMPACT: &str = "compact";

/// Whether `state`'s agent compacts through Slopty ([`Cap::COMPACT`]) rather than by a command
/// it lists itself.
pub(super) fn compacts(state: &slopty_proto::thread::ThreadState) -> bool {
    state.meta.can(Cap::COMPACT) && !state.commands.iter().any(|c| c.name == COMPACT)
}

/// What the composer's menu lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuRows {
    /// The agent's commands that match.
    Commands(Vec<Command>),
    /// Paths the worker found for an `@` query; `None` while it has not answered.
    Paths(Option<Vec<String>>),
    /// An `@` with nothing after it yet.
    Hint,
    /// The models the agent can switch to, opened from the model chip.
    Models(Vec<Model>),
    /// The modes the agent can switch to, opened from the mode chip.
    Modes(Vec<Mode>),
    /// When to send the draft, opened from the clock by the send button.
    Later(Vec<super::later::LaterRow>),
}

impl MenuRows {
    fn len(&self) -> usize {
        match self {
            Self::Commands(commands) => commands.len(),
            Self::Paths(paths) => paths.as_ref().map_or(0, Vec::len),
            Self::Hint => 0,
            Self::Models(models) => models.len(),
            Self::Modes(modes) => modes.len(),
            Self::Later(rows) => rows.len(),
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
    attachments: composer::Attachments,
    /// A pasted picture's chip draws the picture, by attachment.
    pictures: HashMap<u64, Arc<gpui::Image>>,
    editing: Option<Editing>,
    /// The model chip's menu is open.
    models: bool,
    /// The mode chip's menu is open.
    modes: bool,
    /// What the composer says above the field until the draft changes: an attachment it cannot
    /// take, a message waiting for its uploads.
    notice: Option<String>,
    /// ↵ was pressed while an attachment was on its way up: the message goes, so, in the window
    /// it was pressed in, once the last one lands.
    armed: Option<(Delivery, gpui::AnyWindowHandle)>,
    /// The sent message recalled into the composer, by how far back it is, and its words.
    recall: Option<(usize, String)>,
    /// The menu of when to send is open.
    later_menu: bool,
    /// When the draft goes, where it is set to go later.
    later: Option<Delivery>,
}

impl Composing {
    /// Whether the menu of when to send is open.
    pub(super) const fn later_open(&self) -> bool {
        self.later_menu
    }

    /// Open or shut the menu of when to send.
    pub(super) const fn open_later(&mut self, open: bool) {
        self.later_menu = open;
    }

    /// When the draft goes, where it is set to go later.
    pub(super) const fn later(&self) -> Option<Delivery> {
        self.later
    }

    /// Set when the draft goes, or let ↵ say again; the menu shuts.
    pub(super) const fn set_later(&mut self, later: Option<Delivery>) {
        self.later = later;
        self.later_menu = false;
    }

    /// Whether a waiting message is being changed.
    pub(super) const fn editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Whether an attachment is still on its way up.
    pub(super) fn uploading(&self) -> bool {
        self.attachments.uploading()
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
            let models = self.state(cx).map(|s| s.meta.models.clone()).unwrap_or_default();
            return (!models.is_empty()).then_some(MenuRows::Models(models));
        }
        if self.composing.modes {
            let modes = self.state(cx).map(|s| s.meta.modes.clone()).unwrap_or_default();
            return (!modes.is_empty()).then_some(MenuRows::Modes(modes));
        }
        if self.composing.later_open() {
            return Some(MenuRows::Later(self.later_rows(cx)));
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

    /// The commands the `/` menu lists: the agent's own, and `/compact` where Slopty compacts
    /// the context through the agent's own door ([`Cap::COMPACT`]) and the agent lists no
    /// command of that name.
    #[must_use]
    pub fn commands(&self, cx: &App) -> Vec<Command> {
        let Some(state) = self.state(cx) else { return Vec::new() };
        let mut all = state.commands.clone();
        if compacts(state) {
            all.push(Command {
                name: COMPACT.to_owned(),
                description: "Summarize the conversation to free up context".to_owned(),
                argument_hint: None,
                source: "built-in".to_owned(),
            });
        }
        all
    }

    /// Whether the draft asks Slopty to compact the context: `/compact` alone, where the agent
    /// compacts through its door rather than by a command of its own.
    pub(super) fn compact_asked(&self, cx: &App) -> bool {
        self.state(cx).is_some_and(compacts)
            && self.draft(cx).trim() == format!("/{COMPACT}")
            && self.composing.attachments.chips().is_empty()
    }

    /// The draft or its caret moved: the menu's row goes back to the top, a word the person
    /// closed the menu on is forgotten once the caret left it, and an `@` word asks the worker
    /// for what it now matches. What the composer said about an attachment has been read.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) {
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
        if self.composing.models || self.composing.modes || self.composing.later_open() {
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
            (MenuRows::Models(models), _) => {
                let Some(model) = models.get(ix) else { return };
                self.composing.models = false;
                let _id = self.intent(Intent::SetModel { model: model.id.clone() }, cx);
                cx.notify();
                return;
            }
            (MenuRows::Later(rows), _) => {
                let Some(row) = rows.get(ix) else { return };
                self.send_later(Some(row.delivery), cx);
                // The draft is what is being sent later: the keyboard goes back to it.
                self.composer.update(cx, |c, cx| c.focus(window, cx));
                return;
            }
            (MenuRows::Modes(modes), _) => {
                let Some(mode) = modes.get(ix) else { return };
                self.composing.modes = false;
                let _id = self.intent(Intent::SetMode { mode: mode.id.clone() }, cx);
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
            MenuRows::Models(_) => "Models",
            MenuRows::Modes(_) => "Modes",
            MenuRows::Later(_) => "Send later",
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
            MenuRows::Later(rows) => {
                rows.iter().enumerate().map(|(ix, row)| self.later_row(ix, row, cx)).collect()
            }
            MenuRows::Modes(modes) => {
                let now = self.state(cx).and_then(|s| s.meters.mode.clone());
                modes
                    .iter()
                    .enumerate()
                    .map(|(ix, mode)| self.mode_row(ix, mode, now.as_deref(), cx))
                    .collect()
            }
            MenuRows::Models(models) => {
                let now = self.state(cx).and_then(|s| s.meters.model_id.clone());
                models
                    .iter()
                    .enumerate()
                    .map(|(ix, model)| self.model_row(ix, model, now.as_deref(), cx))
                    .collect()
            }
        };
        Some(
            div()
                .id("thread-menu")
                .debug_selector(|| "thread-menu".to_owned())
                .role(Role::ListBox)
                .aria_label(label)
                .max_h(self.z(MENU_ROWS * theme.density.row))
                .overflow_y_scroll()
                .track_scroll(&self.composing.scroll)
                .flex()
                .flex_col()
                .pb(self.z(theme.spacing.xs))
                .border_b(crate::kit::hair(theme))
                .border_color(hsla(theme.surfaces.border_subtle))
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
            .h(self.z(theme.density.row))
            .flex()
            .items_center()
            .px(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.small()))
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
            .h(self.z(theme.density.row))
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.sm))
            .px(self.z(theme.spacing.sm))
            .rounded(self.z(theme.radii.sm))
            .cursor_pointer()
            .when(selected, |el| el.bg(hsla(s.selected)))
            .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.hover))))
            .on_click(cx.listener(move |this, _ev, window, cx| this.menu_pick(ix, window, cx)))
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
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .children(hint.map(|hint| {
                div()
                    .flex_none()
                    .font_family(self.mono())
                    .text_size(self.z(theme.typography.small()))
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
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(command.description.clone())),
            )
            .children(menu::Listed::source_label(command).map(|source| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(source))
            }))
            .into_any_element()
    }

    /// Shut the menus the composer's foot opens: the models, the modes, when to send. The
    /// keyboard starts the next one from its first row.
    pub(super) const fn close_chip_menus(&mut self) {
        self.composing.models = false;
        self.composing.modes = false;
        self.composing.later_menu = false;
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

    /// A mode the agent can switch to, with what it does in the agent's words, and a check on
    /// the one it is in (by its id or its name, as the agent says it).
    fn mode_row(
        &self,
        ix: usize,
        mode: &Mode,
        now: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let current = now.is_some_and(|n| n == mode.id || n == mode.label);
        let label = if mode.label.trim().is_empty() { &mode.id } else { &mode.label };
        self.menu_row(ix, label.clone(), cx)
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.5))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(label.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(mode.description.clone().map(SharedString::from)),
            )
            .when(current, |el| el.child(self.icon(IconName::Check, s.text_secondary)))
            .into_any_element()
    }

    /// The model chip's menu open or shut; open, the keyboard walks it from its first row.
    pub(super) fn toggle_models(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.models;
        self.close_chip_menus();
        self.composing.models = open;
        self.composing.selected = 0;
        cx.notify();
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
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .children(provider.map(SharedString::from)),
            )
            .when(current, |el| el.child(self.icon(IconName::Check, s.text_secondary)))
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
            .child(self.icon(if folder { IconName::Folder } else { IconName::File }, s.text_muted))
            .child(
                div()
                    .flex_none()
                    .max_w(gpui::relative(0.6))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(dir.to_owned())),
            )
            .into_any_element()
    }

    // ----- attachments -----------------------------------------------------------------

    /// A paste into the composer: a picture with no text on the clipboard, or files copied
    /// here, are attached rather than pasted as text. Whether the paste was taken: one this
    /// thread cannot take is taken all the same, said so, since a picture or a path on this
    /// Mac means nothing pasted as text to the agent.
    pub fn paste_attachment(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        Attach::of_paste(item).map(|what| self.attach(what, cx)).is_some()
    }

    /// Why this thread takes no attachment, in words; `None` when it takes them. Files go up
    /// through the terminal its agent runs in.
    pub fn attachments_refused(&self, cx: &App) -> Option<String> {
        let meta = &self.state(cx)?.meta;
        meta.terminal.is_none().then(|| {
            format!("Files can't be attached to {} threads yet", super::agent_name(&meta.agent))
        })
    }

    /// Say `words` above the field until the draft changes.
    fn say(&mut self, words: String, cx: &mut Context<Self>) {
        self.composing.notice = Some(words);
        cx.notify();
    }

    /// The attach button: the picker, or why this thread takes no files.
    pub(super) fn pick_files(&mut self, cx: &mut Context<Self>) {
        match self.attachments_refused(cx) {
            Some(why) => self.say(why, cx),
            None => cx.emit(ThreadViewEvent::PickFiles),
        }
    }

    /// Show `what`'s chip and ask the workspace to send it up; or say why this thread takes no
    /// attachment, with no chip.
    pub fn attach(&mut self, what: Attach, cx: &mut Context<Self>) {
        if let Some(why) = self.attachments_refused(cx) {
            self.say(why, cx);
            return;
        }
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
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(if waiting {
                    self.spinner(true)
                } else {
                    self.icon(IconName::Info, s.text_muted)
                })
                .child(div().min_w_0().flex_1().child(SharedString::from(words)))
                .into_any_element(),
        )
    }

    /// The message to send, the landed attachments' paths after its text; `None` while there
    /// is nothing to send or an attachment is still on its way up.
    pub(super) fn take_message(&mut self, cx: &App) -> Option<String> {
        let attachments = &self.composing.attachments;
        if attachments.uploading() {
            return None;
        }
        let text = composer::with_paths(self.draft(cx).trim(), &attachments.paths());
        if text.trim().is_empty() {
            return None;
        }
        self.composing.attachments.clear();
        self.composing.pictures.clear();
        self.composing.asked = None;
        self.composing.found = None;
        self.composing.notice = None;
        Some(text)
    }

    /// The chips over the field.
    pub(super) fn attachment_chips(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.composing.editing.is_some() {
            return None;
        }
        crate::conversation::chips::row(
            &self.theme,
            self.zoom,
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
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(self.icon(IconName::Pencil, s.text_muted))
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
