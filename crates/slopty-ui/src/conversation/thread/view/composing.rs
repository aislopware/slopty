//! The composer's other work: its `/` and `@` menus, what is attached to the draft, and
//! changing a message that waits in the queue.
//!
//! - **Menus.** The draft is read as the conversation face reads it ([`menu`]): `/` lists the
//!   commands the thread says its agent takes, `@` the paths under the thread's directory that the
//!   worker's file index matches (`ClientMsg::FindFiles`). While an answer for what is typed now is
//!   on its way, the last answer filtered here stands in for it, so the list never blanks between
//!   keys. ↑/↓ move, ↵ or ⇥ pick, Esc closes the menu until the caret leaves the word. Picking only
//!   writes into the draft.
//! - **Attachments.** A pasted picture, files copied here, a drop on the tile or the picker's files
//!   go up through the workspace as a drop on the face does; each shows as a chip
//!   ([`crate::conversation::chips`]) until the message goes, which carries their paths after its
//!   text ([`composer::with_paths`]). Nothing goes while one is still on its way up.
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
use slopty_proto::thread::{Command, IntentId};

use super::{ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::conversation::composer::{self, Attach, Attachment};
use crate::conversation::menu::{self, Token};
use crate::icons::IconName;
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
}

impl MenuRows {
    fn len(&self) -> usize {
        match self {
            Self::Commands(commands) => commands.len(),
            Self::Paths(paths) => paths.as_ref().map_or(0, Vec::len),
            Self::Hint => 0,
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
}

impl Composing {
    /// Whether a waiting message is being changed.
    pub(super) const fn editing(&self) -> bool {
        self.editing.is_some()
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
        match self.menu_token(cx)? {
            Token::Command { query } => {
                let all = &self.state(cx)?.commands;
                let found: Vec<Command> =
                    menu::commands(all, &query).into_iter().cloned().collect();
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

    /// The draft or its caret moved: the menu's row goes back to the top, a word the person
    /// closed the menu on is forgotten once the caret left it, and an `@` word asks the worker
    /// for what it now matches.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) {
        let token = {
            let composer = self.composer.read(cx);
            menu::token(&composer.value(), composer.cursor())
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
                .border_b_1()
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
    fn menu_row(&self, ix: usize, label: String, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
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
            .when(selected, |el| el.bg(hsla(s.overlay)))
            .when(!selected, |el| el.hover(move |el| el.bg(hsla(s.raised))))
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
                    .text_size(self.z(theme.typography.meta()))
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
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(command.description.clone())),
            )
            .children(menu::Listed::source_label(command).map(|source| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(source))
            }))
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
                    .text_size(self.z(theme.typography.meta()))
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

    /// Attachment `id` landed at `paths` on the worker: its chip stays until the message goes.
    pub fn attachment_landed(&mut self, id: u64, paths: &[String], cx: &mut Context<Self>) {
        if self.composing.attachments.land(id, paths) {
            if paths.is_empty() {
                self.composing.pictures.remove(&id);
            }
            cx.notify();
        }
    }

    /// Attachment `id`'s upload is over: landed, its chip stays for the message; not, it goes.
    pub fn attachment_ended(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.composing.attachments.over(id) {
            self.composing.pictures.remove(&id);
            cx.notify();
        }
    }

    /// The person takes attachment `id` off the draft: its chip goes at once, and the
    /// workspace stops its upload.
    pub fn detach(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.composing.attachments.end(id) {
            self.composing.pictures.remove(&id);
            cx.emit(ThreadViewEvent::Detach { id });
            cx.notify();
        }
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
        let bar = super::Activity::of(hub.threads(), self.thread, state);
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
                .text_size(self.z(theme.typography.meta()))
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
