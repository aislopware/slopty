//! What the composer does besides send: its menus, the question it answers in place of the
//! field, prompt recall, the model it types, and the rewind it hands to the TUI.
//!
//! - **Menus.** A draft's leading `/` lists the agent's slash commands; an `@` starting a word
//!   lists the files under the agent's directory, asked of the worker as the word grows
//!   ([`crate::conversation::menu`]). The menu is the composer shell's section over the field. ↑/↓
//!   move, Enter or Tab picks, Esc closes it until the caret leaves the word. Picking only writes
//!   into the draft: sending still types the draft as the composer always does.
//! - **Questions.** An `AskUserQuestion` held for the face takes the composer's shell, one question
//!   at a time ([`crate::conversation::question`]). Digits 1–9 pick while the answer field is
//!   empty, ↑/↓ move and Enter goes on. The answer goes back through the permission hook, never as
//!   keys into the TUI's menu.
//! - **Recall.** ↑ in an empty composer brings back the prompt before, ↓ the one after, as a
//!   shell's history does.
//! - **Model.** The model in the foot lists the aliases `/model` takes; picking one types `/model
//!   <alias>` and Enter, only while the agent is idle.
//! - **Rewind.** A prompt's "Rewind" shows the TUI and types `/rewind` there: the person picks the
//!   point in Claude Code's own menu.

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Focusable as _, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};
use gpui_kit::component::input::{Input, RopeExt as _};
use slopty_proto::conversation::{Body, SlashCommand, ThreadId, ToolDetail, Verdict};

use super::ConversationView;
use crate::colors::hsla;
use crate::conversation::menu::{self, Token};
use crate::conversation::question::{Answering, Picked};
use crate::conversation::{FaceEvent, approval};
use crate::icons::IconName;
use crate::kit::{self, ButtonKind};

/// The widest a mention's chip grows, in points at zoom 1; a longer name is cut short.
const MENTION_WIDTH: f32 = 240.0;

/// Rows the composer's menu shows before it scrolls.
const MENU_ROWS: f32 = 8.0;

/// The models the picker offers, as `/model` takes them: alias and name.
pub(super) const MODELS: [(&str, &str); 4] =
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku"), ("default", "Default")];

/// What the composer's menu lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuRows {
    /// Slash commands that match.
    Commands(Vec<SlashCommand>),
    /// Paths the worker found for an `@` query; `None` while it has not answered.
    Paths(Option<Vec<String>>),
    /// The models `/model` takes.
    Models,
}

impl MenuRows {
    /// Rows it holds.
    #[must_use]
    pub(super) fn len(&self) -> usize {
        match self {
            Self::Commands(commands) => commands.len(),
            Self::Paths(paths) => paths.as_ref().map_or(0, Vec::len),
            Self::Models => MODELS.len(),
        }
    }

    /// Whether it holds nothing.
    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The composer's menu as the person left it.
#[derive(Clone, Debug, Default)]
pub(super) struct MenuState {
    /// Esc closed the menu for the word that starts here.
    pub dismissed: Option<usize>,
    /// The row the keyboard is on.
    pub selected: usize,
    /// The `@` query last asked of the worker.
    pub asked: Option<String>,
    /// The model list is open.
    pub models: bool,
}

impl ConversationView {
    // ----- keys ------------------------------------------------------------------------

    /// Whether the composer's field has the keyboard.
    pub(super) fn composer_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer.focus_handle(cx).is_focused(window)
    }

    /// Whether an input method holds uncommitted text in one of the face's fields (a Telex
    /// word, kana before conversion). The keys it reads then (Enter to commit, the arrows and
    /// Tab over its candidates, Esc to cancel) are the input method's: the face's own uses of
    /// them wait for the next press.
    pub(super) fn composing(&self, cx: &App) -> bool {
        self.composer.read(cx).is_composing()
            || self.answer_field.read(cx).is_composing()
            || self.deny.read(cx).is_composing()
            || self.find_field().is_some_and(|field| field.read(cx).is_composing())
    }

    /// ↑ (`-1`) or ↓ (`1`): a question's options while its field has the keyboard; in the
    /// composer, the open menu's rows, else the prompts sent before. Otherwise the field's own.
    pub(super) fn arrow(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let taken = if self.answer_field.focus_handle(cx).is_focused(window) {
            self.answer_step(delta, cx)
        } else if self.composer_focused(window, cx) {
            self.menu_step(delta, cx) || self.recall(delta, window, cx)
        } else {
            false
        };
        if taken {
            cx.stop_propagation();
        }
    }

    /// Before a frame: a new question empties the answer field and takes the keyboard from
    /// the composer that had it; a settled one hands the keyboard back.
    pub(super) fn settle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer_focused = self.answer_field.focus_handle(cx).is_focused(window);
        if std::mem::take(&mut self.answer_reset) {
            let take = answer_focused || self.composer_focused(window, cx);
            self.answer_field.update(cx, |f, cx| {
                f.set_value("", window, cx);
                if take {
                    f.focus(window, cx);
                }
            });
        } else if self.answering.is_none() && answer_focused {
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
    }

    // ----- menus -----------------------------------------------------------------------

    /// The word the composer's caret is in, when it asks for a menu the person has not closed.
    pub(super) fn menu_token(&self, cx: &App) -> Option<Token> {
        if self.approvals.prompt().is_some() {
            return None;
        }
        let composer = self.composer.read(cx);
        let token = menu::token(&composer.value(), composer.cursor())?;
        (self.menu.dismissed != Some(token.start())).then_some(token)
    }

    /// What the composer's menu lists now, if it is open.
    #[must_use]
    pub(super) fn menu_rows(&self, cx: &App) -> Option<MenuRows> {
        if self.menu.models && self.approvals.prompt().is_none() {
            return Some(MenuRows::Models);
        }
        match self.menu_token(cx)? {
            Token::Command { query } => {
                let found: Vec<SlashCommand> =
                    menu::commands(self.model.commands(), &query).into_iter().cloned().collect();
                (!found.is_empty()).then_some(MenuRows::Commands(found))
            }
            Token::Mention { query, .. } => {
                Some(MenuRows::Paths(self.model.found(&query).map(<[String]>::to_vec)))
            }
        }
    }

    /// The draft or its caret moved: the menu's row goes back to the top when its word
    /// changed, a word the person closed the menu on is forgotten once the caret left it, and
    /// an `@` word asks the worker for what it now matches. A recalled prompt that was edited is
    /// the person's own draft.
    pub(super) fn composer_changed(&mut self, cx: &mut Context<Self>) {
        let draft = self.draft(cx);
        if self.recall.as_ref().is_some_and(|(_, text)| *text != draft) {
            self.recall = None;
        }
        let token = {
            let composer = self.composer.read(cx);
            menu::token(&draft, composer.cursor())
        };
        if self.menu.dismissed.is_some_and(|at| token.as_ref().is_none_or(|t| t.start() != at)) {
            self.menu.dismissed = None;
        }
        self.menu.selected = 0;
        self.menu.models = false;
        if let Some(Token::Mention { query, .. }) = token
            && self.menu.asked.as_deref() != Some(query.as_str())
        {
            self.menu.asked = Some(query.clone());
            cx.emit(FaceEvent::Search { query });
        }
        cx.notify();
    }

    /// ↑ (`-1`) or ↓ (`1`) in the open menu, round. Whether a menu took the key.
    pub(super) fn menu_step(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(rows) = self.menu_rows(cx) else { return false };
        let count = rows.len();
        if count == 0 {
            return true;
        }
        self.menu.selected = if delta < 0 {
            self.menu.selected.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1))
        } else if self.menu.selected.saturating_add(1) < count {
            self.menu.selected.saturating_add(1)
        } else {
            0
        };
        self.menu_scroll.scroll_to_item(self.menu.selected);
        cx.notify();
        true
    }

    /// Enter or Tab in the open menu: pick the row the keyboard is on. Whether a menu took
    /// the key.
    pub(super) fn menu_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(rows) = self.menu_rows(cx) else { return false };
        if rows.is_empty() {
            return false;
        }
        // A new answer from the worker may have shortened the list under the keyboard's row.
        self.menu_pick(self.menu.selected.min(rows.len().saturating_sub(1)), window, cx);
        true
    }

    /// Esc with the menu open closes it for the word the caret is in. Whether it was open.
    pub(super) fn menu_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.menu.models {
            self.menu.models = false;
            cx.notify();
            return true;
        }
        let Some(token) = self.menu_token(cx) else { return false };
        self.menu.dismissed = Some(token.start());
        cx.notify();
        true
    }

    /// Pick row `ix` of the open menu: a command or a path written into the draft, or a model
    /// typed into the terminal.
    pub fn menu_pick(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rows) = self.menu_rows(cx) else { return };
        let (text, caret) = {
            let composer = self.composer.read(cx);
            (composer.value().to_string(), composer.cursor())
        };
        let written = match (rows, self.menu_token(cx)) {
            (MenuRows::Models, _) => {
                if let Some((alias, _)) = MODELS.get(ix) {
                    self.pick_model(alias, cx);
                }
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

    /// Put `text` in the composer with the caret at byte `caret`, and bring the menus in
    /// step with it.
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

    /// The menu, as the composer shell's section over the field: a row a command or a path,
    /// the keyboard's row filled.
    pub(super) fn menu_section(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let rows = self.menu_rows(cx)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let label = match &rows {
            MenuRows::Commands(_) => "Commands",
            MenuRows::Paths(_) => "Files",
            MenuRows::Models => "Models",
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
            MenuRows::Models => MODELS
                .iter()
                .enumerate()
                .map(|(ix, model)| self.model_row(ix, *model, cx))
                .collect(),
        };
        Some(
            div()
                .id("composer-menu")
                .debug_selector(|| "composer-menu".to_owned())
                .role(Role::ListBox)
                .aria_label(label)
                .max_h(self.z(MENU_ROWS * theme.density.row))
                .overflow_y_scroll()
                .track_scroll(&self.menu_scroll)
                .flex()
                .flex_col()
                .text_color(hsla(s.text))
                .children(body)
                .into_any_element(),
        )
    }

    /// A quiet line in the menu's place: nothing matched, or the worker is looking.
    fn menu_note(&self, text: &'static str) -> AnyElement {
        let theme = &self.theme;
        div()
            .debug_selector(|| "composer-menu-note".to_owned())
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
        let selected = ix == self.menu.selected;
        div()
            .id(("composer-menu-row", ix))
            .debug_selector(move || format!("composer-menu-{ix}"))
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

    fn command_row(&self, ix: usize, command: &SlashCommand, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let name = format!("/{}", command.name);
        self.menu_row(ix, format!("{name} {}", command.description), cx)
            .child(
                div()
                    .flex_none()
                    .font_family(self.mono())
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
                    .child(SharedString::from(command.description.clone())),
            )
            .children(menu::Listed::source_label(command).map(|source| {
                div()
                    .flex_none()
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(source)
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

    fn model_row(&self, ix: usize, (alias, name): (&str, &str), cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let current =
            self.running_model().is_some_and(|running| running.to_lowercase().starts_with(alias));
        self.menu_row(ix, name.to_owned(), cx)
            .child(
                div()
                    .flex_1()
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(name.to_owned())),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(self.mono())
                    .text_size(self.z(theme.typography.meta()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(format!("/model {alias}"))),
            )
            .when(current, |el| el.child(self.icon(IconName::Check, s.accent)))
            .into_any_element()
    }

    // ----- the model -------------------------------------------------------------------

    /// Whether the agent can take a command typed now: no turn running, nothing asked.
    pub(super) fn idle(&self) -> bool {
        !self.turn_running() && self.approvals.prompt().is_none()
    }

    /// The model in the composer's foot, a way to the model list: while the agent is idle,
    /// and after its turn otherwise, as its hint says.
    pub(super) fn model_button(&self, model: &str, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let idle = self.idle();
        let open = self.menu.models;
        let hint_theme = std::rc::Rc::clone(&self.hint_theme);
        let chevron = crate::icons::icon(
            theme,
            IconName::ChevronDown,
            crate::icons::IconSize::Inline,
            hsla(s.text_muted),
        )
        .size(self.z(theme.typography.meta()));
        let button = self
            .foot_chip("composer-model", IconName::Asterisk, s.text_muted, model.to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(format!("Model: {model}")))
            .aria_expanded(open)
            .when(open, |el| el.bg(hsla(s.raised)))
            .when(idle, |el| el.cursor_pointer().hover(move |el| el.bg(hsla(s.raised))))
            .when(!idle, |el| el.opacity(slopty_theme::alpha::PRESSED))
            .child(chevron)
            .tooltip(move |_window, cx| {
                let theme = std::rc::Rc::clone(&hint_theme);
                let title = if idle { "Change the model" } else { "After this turn" };
                cx.new(|_| kit::Hint::new(title, "", theme)).into()
            })
            .when(idle, |el| el.on_click(cx.listener(|this, _ev, _w, cx| this.toggle_models(cx))));
        crate::a11y::tab_stop(button, s.accent).into_any_element()
    }

    /// A chip of the composer's foot: `icon` in `ink`, then `text`, as tall as the buttons
    /// beside it so the row reads as one line.
    pub(super) fn foot_chip(
        &self,
        id: &'static str,
        icon: IconName,
        ink: slopty_theme::Rgb,
        text: String,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &self.theme;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xxs))
            .min_w_0()
            .h(self.z(kit::icon_button_side(theme)))
            .px(self.z(theme.spacing.xs))
            .rounded(self.z(theme.radii.sm))
            .text_size(self.z(theme.typography.small()))
            .text_color(hsla(theme.surfaces.text_secondary))
            .child(
                crate::icons::icon(theme, icon, crate::icons::IconSize::Inline, hsla(ink))
                    .size(self.z(theme.typography.icon())),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(text)),
            )
    }

    /// Open or close the model list over the field, while the agent is idle.
    pub(super) fn toggle_models(&mut self, cx: &mut Context<Self>) {
        self.menu.models = !self.menu.models && self.idle();
        self.menu.selected = 0;
        cx.notify();
    }

    /// Type `/model <alias>` and Enter into the agent's terminal: `/model` takes its argument
    /// without its menu, so nothing picks in the TUI. Only while the agent is idle.
    pub fn pick_model(&mut self, alias: &str, cx: &mut Context<Self>) {
        self.menu.models = false;
        if !self.idle() {
            cx.notify();
            return;
        }
        self.send(format!("/model {alias}"), Vec::new(), cx);
    }

    // ----- recall ----------------------------------------------------------------------

    /// The session's prompts as typed, newest first.
    fn prompts_typed(&self) -> Vec<String> {
        let Some(main) = self.model.thread(&ThreadId::Main) else { return Vec::new() };
        main.entries()
            .iter()
            .rev()
            .filter_map(|entry| match &entry.body {
                Body::Prompt(prompt) => Some(self.prompt_words(prompt)),
                _ => None,
            })
            .filter(|words| !words.trim().is_empty())
            .collect()
    }

    /// ↑ (`-1`) or ↓ (`1`) with the composer empty or holding a recalled prompt: the prompt
    /// before or after it. Whether recall took the key.
    pub(super) fn recall(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let draft = self.draft(cx);
        let at = match &self.recall {
            Some((at, text)) if *text == draft => Some(*at),
            _ if draft.is_empty() => None,
            _ => return false,
        };
        let prompts = self.prompts_typed();
        let next = match (at, delta < 0) {
            (None, true) => 0,
            (Some(at), true) => at.saturating_add(1),
            (None, false) => return false,
            (Some(0), false) => {
                self.recall = None;
                self.set_draft("", 0, window, cx);
                return true;
            }
            (Some(at), false) => at.saturating_sub(1),
        };
        let Some(text) = prompts.get(next).cloned() else { return at.is_some() };
        self.set_draft(&text, text.len(), window, cx);
        self.recall = Some((next, text));
        true
    }

    // ----- the question ----------------------------------------------------------------

    /// A prompt came: an `AskUserQuestion` is answered here, from its first question.
    pub(super) fn start_answering(&mut self) {
        self.answering = self.approvals.prompt().and_then(|prompt| match &prompt.detail {
            ToolDetail::Question(question) if !question.questions.is_empty() => {
                Some(Answering::new(prompt.ask, question.questions.clone()))
            }
            _ => None,
        });
        self.answer_reset = self.answering.is_some();
    }

    /// The answer field's words.
    fn typed_answer(&self, cx: &App) -> String {
        self.answer_field.read(cx).value().to_string()
    }

    /// A pick, from a click or a digit: on to the next question, or the answer given.
    pub fn pick_option(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answering) = &mut self.answering else { return };
        let picked = answering.pick(ix);
        // A click on an option keeps the keyboard on the question.
        self.answer_field.update(cx, |f, cx| f.focus(window, cx));
        self.after_pick(picked, window, cx);
    }

    /// Next or Answer.
    pub fn answer_go_on(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.typed_answer(cx);
        let Some(answering) = &mut self.answering else { return };
        let picked = answering.go_on(typed);
        self.after_pick(picked, window, cx);
    }

    fn after_pick(&mut self, picked: Picked, window: &mut Window, cx: &mut Context<Self>) {
        match picked {
            Picked::Shown => {}
            Picked::Next => {
                let words =
                    self.answering.as_ref().map(|a| a.typed().to_owned()).unwrap_or_default();
                self.answer_field.update(cx, |f, cx| f.set_value(words, window, cx));
            }
            Picked::Done(answers) => self.answer(Verdict::Answer { answers }, cx),
        }
        cx.notify();
    }

    /// Skip: the questions go unanswered, and Claude goes on without them.
    fn skip_questions(&mut self, cx: &mut Context<Self>) {
        let message = "The person skipped these questions.".to_owned();
        self.answer(Verdict::Deny { message, interrupt: false }, cx);
    }

    /// The answer field changed: a lone digit typed into it picks that option.
    pub(super) fn answer_typed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.typed_answer(cx);
        let count =
            self.answering.as_ref().and_then(|a| a.question()).map_or(0, |q| q.options.len());
        let digit = typed.chars().next().and_then(|c| c.to_digit(10)).filter(|_| typed.len() == 1);
        if let Some(digit) = digit
            && (1..=9).contains(&digit)
            && (digit as usize) <= count
        {
            self.answer_field.update(cx, |f, cx| f.set_value("", window, cx));
            self.pick_option((digit as usize).saturating_sub(1), window, cx);
        } else {
            cx.notify();
        }
    }

    /// ↑/↓ while the answer field has the keyboard: the option it is on.
    pub(super) fn answer_step(&mut self, delta: isize, cx: &mut Context<Self>) -> bool {
        let Some(answering) = &mut self.answering else { return false };
        answering.step(delta);
        cx.notify();
        true
    }

    /// The question on show, in the composer's shell: its header and where it is among the
    /// questions, its text, its options, a field for words of one's own, and the ways on.
    pub(super) fn question_card(&self, cx: &Context<Self>) -> AnyElement {
        let Some(answering) = &self.answering else { return div().into_any_element() };
        let Some(question) = answering.question() else { return div().into_any_element() };
        let theme = &self.theme;
        let s = theme.surfaces;
        let busy = self.approvals.answering().is_some();
        let (at, count) = answering.position();
        let header = question.header.clone().unwrap_or_else(|| "Question".to_owned());
        let multi = question.multi_select;
        let options = question.options.iter().enumerate().map(|(ix, option)| {
            let picked = answering.is_picked(ix);
            let on = answering.cursor() == ix;
            let glyph = match (picked, multi) {
                (false, _) => IconName::Circle,
                (true, false) => IconName::CircleCheck,
                (true, true) => IconName::CircleDot,
            };
            div()
                .id(("question-option", ix))
                .debug_selector(move || format!("question-option-{ix}"))
                .role(if multi { Role::CheckBox } else { Role::RadioButton })
                .aria_label(SharedString::from(option.label.clone()))
                .aria_toggled(if picked { Toggled::True } else { Toggled::False })
                .flex_none()
                .min_h(self.z(theme.density.row_two_line))
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.sm))
                .px(self.z(theme.spacing.sm))
                .py(self.z(theme.spacing.xxs))
                .rounded(self.z(theme.radii.sm))
                .cursor_pointer()
                .when(on, |el| el.bg(hsla(s.overlay)))
                .when(!on, |el| el.hover(move |el| el.bg(hsla(s.raised))))
                .child(self.icon(glyph, if picked { s.accent } else { s.text_muted }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(self.z(theme.typography.small()))
                                .text_color(hsla(s.text))
                                .child(SharedString::from(option.label.clone())),
                        )
                        .children(option.description.clone().map(|d| {
                            div()
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .whitespace_normal()
                                .child(SharedString::from(d))
                        })),
                )
                .on_click(
                    cx.listener(move |this, _ev, window, cx| this.pick_option(ix, window, cx)),
                )
        });
        let ready = answering.answered(&self.typed_answer(cx));
        let go_label = if answering.last() { "Answer" } else { "Next" };
        let go = kit::button(theme, "question-go", go_label, ButtonKind::Primary)
            .when(busy || !ready, |el| el.opacity(slopty_theme::alpha::PRESSED))
            .on_click(cx.listener(|this, _ev, window, cx| this.answer_go_on(window, cx)));
        let skip = kit::button(theme, "question-skip", "Skip", ButtonKind::Ghost)
            .on_click(cx.listener(|this, _ev, _w, cx| this.skip_questions(cx)));
        div()
            .id("question")
            .debug_selector(|| "question".to_owned())
            .role(Role::AlertDialog)
            .aria_label(SharedString::from(question.text.clone()))
            .px(self.z(kit::FIELD_INSET))
            .flex()
            .flex_col()
            .gap(self.z(theme.spacing.sm))
            .text_size(self.z(theme.typography.ui_size))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .child(
                        div()
                            .flex_none()
                            .size(self.z(theme.spacing.sm))
                            .rounded_full()
                            .bg(hsla(s.warn_fill)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(hsla(s.text))
                            .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT))
                            .child(SharedString::from(header)),
                    )
                    .when(count > 1, |el| {
                        el.child(
                            kit::tabular(div())
                                .debug_selector(|| "question-position".to_owned())
                                .flex_none()
                                .text_size(self.z(theme.typography.meta()))
                                .text_color(hsla(s.text_muted))
                                .child(SharedString::from(format!("{at} of {count}"))),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(self.z(theme.typography.prose()))
                    .text_color(hsla(s.text))
                    .whitespace_normal()
                    .child(SharedString::from(question.text.clone())),
            )
            .child(div().flex().flex_col().children(options))
            .child(
                div().w_full().child(Input::new(&self.answer_field).aria_label("Your own answer")),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(self.z(theme.spacing.sm))
                    .children(multi.then(|| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(self.z(theme.typography.meta()))
                            .text_color(hsla(s.text_muted))
                            .child("Pick one or more")
                    }))
                    .when(!multi, |el| el.child(div().flex_1()))
                    .child(skip)
                    .child(go),
            )
            .into_any_element()
    }

    // ----- mentions in a sent prompt ---------------------------------------------------

    /// A sent prompt's words, its `@path` mentions set in the accent.
    pub(super) fn mentioned_text(&self, text: String) -> gpui::StyledText {
        let spans = menu::mentions(&text);
        if spans.is_empty() {
            return gpui::StyledText::new(text);
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let font = gpui::font(theme.typography.ui_family.clone());
        let run = |len: usize, tone| gpui::TextRun {
            len,
            font: font.clone(),
            color: hsla(tone),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let mut runs = Vec::new();
        let mut at = 0;
        for span in &spans {
            if span.start > at {
                runs.push(run(span.start.saturating_sub(at), s.text));
            }
            runs.push(run(span.len(), s.accent));
            at = span.end;
        }
        if text.len() > at {
            runs.push(run(text.len().saturating_sub(at), s.text));
        }
        gpui::StyledText::new(text).with_runs(runs)
    }

    /// The files a sent prompt mentions, a chip each under its words: the glyph and the name,
    /// the whole path on hover, a click opening it in a tile of its own.
    pub(super) fn mention_chips(
        &self,
        key: &str,
        text: &str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let mut paths: Vec<&str> = menu::mentions(text)
            .into_iter()
            .filter_map(|span| text.get(span))
            .map(menu::mention_path)
            .collect();
        paths.dedup();
        if paths.is_empty() {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let chips = paths.into_iter().enumerate().map(|(ix, path)| {
            let folder = path.ends_with('/');
            let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_owned();
            let full = path.to_owned();
            let hint_theme = std::rc::Rc::clone(&self.hint_theme);
            let hint = full.clone();
            crate::a11y::tab_stop(
                kit::pill_frame(theme, self.zoom)
                    .id(gpui::ElementId::Name(SharedString::from(format!("mention-{key}-{ix}"))))
                    .debug_selector({
                        let selector = format!("mention-{full}");
                        move || selector
                    })
                    .role(Role::Link)
                    .aria_label(SharedString::from(format!("Open {full}")))
                    .flex_none()
                    .max_w(self.z(MENTION_WIDTH))
                    .bg(hsla(s.overlay))
                    .text_color(hsla(s.text_secondary))
                    .cursor_pointer()
                    .hover(move |el| el.text_color(hsla(s.text)))
                    .child(
                        self.icon(
                            if folder { IconName::Folder } else { IconName::File },
                            s.text_muted,
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(SharedString::from(name)),
                    )
                    .tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::clone(&hint_theme);
                        cx.new(|_| kit::Hint::new(hint.clone(), "", theme)).into()
                    }),
                s.accent,
            )
            .on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.emit(FaceEvent::OpenPath { path: full.clone() });
            }))
        });
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap(self.z(theme.spacing.xs))
                .children(chips)
                .into_any_element(),
        )
    }

    // ----- rewind ----------------------------------------------------------------------

    /// A prompt's way back to it, beside its copy on hover: "Rewind…" hands the choice to
    /// Claude Code's own menu. Only on the session's own thread, and quiet while a turn runs.
    pub(super) fn rewind_button(
        &self,
        id: &str,
        group: &'static str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if self.thread != ThreadId::Main {
            return None;
        }
        let theme = &self.theme;
        let s = theme.surfaces;
        let idle = self.idle();
        let touch = theme.density == slopty_theme::Density::TOUCH;
        let hint_theme = std::rc::Rc::clone(&self.hint_theme);
        let selector = format!("rewind-{id}");
        Some(
            crate::a11y::tab_stop(
                div()
                    .id(gpui::ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .role(Role::Button)
                    .aria_label("Rewind…")
                    .flex_none()
                    .size(self.z(theme.typography.icon_large()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(self.z(theme.radii.xs))
                    .when(idle, |el| el.cursor_pointer().hover(move |el| el.bg(hsla(s.raised))))
                    .when(!idle, |el| el.opacity(slopty_theme::alpha::PRESSED))
                    .when(!touch, |el| el.invisible().group_hover(group, gpui::Styled::visible))
                    .child(self.icon(IconName::Undo2, s.text_muted))
                    .tooltip(move |_window, cx| {
                        let theme = std::rc::Rc::clone(&hint_theme);
                        let title = if idle { "Rewind in the terminal" } else { "After this turn" };
                        cx.new(|_| kit::Hint::new(title, "", theme)).into()
                    }),
                s.accent,
            )
            .on_click(cx.listener(|this, _ev, _w, cx| this.rewind(cx)))
            .into_any_element(),
        )
    }

    /// Hand the rewind to the TUI: show it and type `/rewind` there, for the person to pick
    /// the point in Claude Code's own menu. Only while the agent is idle.
    pub fn rewind(&self, cx: &mut Context<Self>) {
        if self.idle() {
            cx.emit(FaceEvent::Rewind);
        }
    }

    /// Whether the prompt held is a plan to approve: approved or kept, never allowed for good.
    pub(super) fn plan_asked(&self) -> bool {
        self.approvals.prompt().is_some_and(|p| p.tool == approval::PLAN_TOOL)
    }
}
