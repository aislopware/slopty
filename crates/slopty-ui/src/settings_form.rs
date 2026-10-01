//! The settings form: `settings.toml` as sections of rows, each a label, one line on what it
//! does, and the control that sets it.
//!
//! The rows are the file's keys as `slopty_settings` describes them ([`schema::rows`]): a
//! key's words, type, default, range and choices come from the settings types, and this only
//! places it on a page. The file stays the one source of truth. The form holds the file's
//! text, reads every row's value out of it and writes an edit back into it a line at a time
//! ([`edit::write`]), so a comment or a key the form does not know survives an edit.
//!
//! An edit applies as it is made, as System Settings' do: a switch, a choice or a font at
//! once, a stepper's and a field's once the hand pauses ([`SETTLE`]), so a run of clicks or
//! keystrokes writes the file once. A value the key does not take (a colour that is not one, a
//! host with a bad port) is never written; its row says why until it is typed again.
//!
//! On a desktop the sections are a sidebar with the search field at its top, as System
//! Settings has it, and the page beside it holds the chosen section's rows under their group
//! labels. A query lists every row that matches, under its section's name; so does a window too
//! narrow for the sidebar, all sections in one column.
//!
//! The keyboard walks it: ↓ from the search goes to the first row's control, ↑ and ↓ go from row
//! to row, ← and → move a choice or a stepper, Space or Return turns a switch, and ↑ and ↓ in the
//! sidebar move between sections. Tab goes along the ring as everywhere else.
//!
//! Two pages under the sections hold no table's rows. Keyboard lists every command of the app's
//! keymap with the chords that run it, as the palette words them: a press on a command's chords
//! records the next chord typed into `[keys]` (⌫ unbinds it, Esc keeps what it had), and a
//! command the file sets has a way back to its default. About says which build this is and
//! where the project lives. A query finds commands too, by their words, keys or name in the
//! file.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, Keystroke, ParentElement as _, Render, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_settings::edit::{self, Value};
use slopty_settings::schema::{Choice, Kind};
use slopty_theme::{Rgb, Theme, Typography};

use crate::colors::hsla;
use crate::icons::{IconName, IconSize};

#[path = "settings_form_schema.rs"]
pub mod schema;

use schema::{KeyRow, Row, Section, rows};

/// What the search field says before anything is typed.
pub const SEARCH_PLACEHOLDER: &str = "Search settings";

/// What the page says when the query matches no row.
pub const NO_MATCHES: &str = "No settings match";

/// What a command's chords say while the next chord typed is recorded for it.
pub const PRESS_KEYS: &str = "Press the new keys";

/// What a command's chords say while recording, after a key alone that the command cannot take.
pub const NEEDS_MODIFIER: &str = "Hold \u{2318} or \u{2303} with the key";

/// What a command's chords say while recording, after a key that has no name in a chord.
pub const CANT_BIND: &str = "This key can't be bound";

/// What a command's chords say when nothing runs it.
pub const NO_KEYS: &str = "None";

/// The name of a command's button that puts its default chords back.
pub const RESET_KEYS: &str = "Reset to default";

/// What the font list says while it looks for the installed monospace families.
pub const FINDING_FONTS: &str = "Finding monospace fonts";

/// What an empty colour field says: the theme's own colour applies.
pub const THEME_COLOUR: &str = "Theme";

/// How long a stepper or a field waits after the last click or keystroke before its value is
/// written: past the gap between two quick presses, well under the time it takes to look at
/// what changed.
pub const SETTLE: Duration = Duration::from_millis(300);

/// The sidebar's width: the longest section name and the search field's word at the UI size.
const NAV_WIDTH: f32 = 168.0;

/// A field's width: a colour's hex beside its swatch. A longer host scrolls in its field, and
/// the row's words keep one line beside it.
const FIELD_WIDTH: f32 = 140.0;

/// The font picker's width: a family's name, drawn in itself.
const FONT_WIDTH: f32 = 184.0;

/// The page's height before the window takes some back, in two-line rows.
const PAGE_ROWS: f32 = 11.0;

/// How many families the font list shows before it scrolls.
const FONT_ROWS: f32 = 6.0;

/// What the form asks of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsFormEvent {
    /// Rows changed the file: this is its whole new text, to write and apply now.
    Apply(String),
    /// ⌘↩ on a control that is not a text field (a field's Return is the dialog's own): done.
    Done,
}

/// The form over one `settings.toml` text.
pub struct SettingsForm {
    theme: Theme,
    /// The tokens a row's hint is drawn with, shared by every hint.
    hint_theme: Rc<Theme>,
    /// The file's text as the rows last wrote it or the dialog last handed it over.
    text: String,
    /// The text last handed to the dialog to apply.
    applied: String,
    /// The write waiting for the hand to pause ([`SETTLE`]); dropping it cancels it.
    pending: Option<Task<()>>,
    /// Rows typed into since their value was last checked.
    typed: Vec<usize>,
    /// One per row of [`rows`]: why its value was not written.
    errors: Vec<Option<String>>,
    /// The page shown when nothing is searched.
    section: Section,
    search: Entity<InputState>,
    /// The search field's text.
    query: String,
    /// One per row of [`rows`]: what takes the keyboard for a control that is not a field.
    handles: Vec<FocusHandle>,
    /// One per row of [`rows`]: the field of a row typed into.
    fields: Vec<Option<Entity<InputState>>>,
    /// One per section, for its tab in the sidebar.
    tabs: Vec<FocusHandle>,
    /// The monospace families, once looked for.
    fonts: Option<Vec<SharedString>>,
    /// The font list is open under its row.
    font_open: bool,
    /// The family the keyboard is on in the open list.
    font_at: usize,
    /// The switch just turned and a count of turns: only it slides, and only once.
    moved: Option<(usize, usize)>,
    turns: usize,
    /// Each row's place among the page's children, for scrolling it into view.
    placed: Vec<(usize, usize)>,
    /// The sidebar is gone: every section in one column.
    narrow: bool,
    /// The Keyboard page's lines for the text, read again when the text changes.
    keys: Option<KeyPage>,
    /// One per command of the keymap, for its chords' control.
    key_handles: Vec<FocusHandle>,
    /// The command whose next chord is being recorded.
    recording: Option<Recording>,
    scroll: ScrollHandle,
    font_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for SettingsForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsForm")
            .field("section", &self.section)
            .field("query", &self.query)
            .finish_non_exhaustive()
    }
}

impl SettingsForm {
    /// A form over `text`, a `settings.toml`.
    pub fn new(text: &str, theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(SEARCH_PLACEHOLDER));
        let mut subscriptions =
            vec![cx.subscribe_in(&search, window, |this, input, event, window, cx| match event {
                InputEvent::Change => {
                    this.query = input.read(cx).value().to_string();
                    this.font_open = false;
                    this.scroll.scroll_to_item(0);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.step_from(None, true, window, cx),
                InputEvent::Focus | InputEvent::Blur => {}
            })];
        let mut fields = Vec::with_capacity(rows().len());
        for (ix, row) in rows().iter().enumerate() {
            let placeholder = match row.field.kind {
                Kind::Text | Kind::List => row.field.example.as_deref().unwrap_or_default(),
                Kind::Colour => THEME_COLOUR,
                Kind::Switch | Kind::Choice(_) | Kind::Number(_) | Kind::Font => {
                    fields.push(None);
                    continue;
                }
            };
            let value = field_text(text, row);
            let state = cx.new(|cx| {
                InputState::new(window, cx).placeholder(placeholder).default_value(value)
            });
            subscriptions.push(cx.subscribe(&state, move |this, _state, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.type_into(ix, cx);
                }
            }));
            fields.push(Some(state));
        }
        let handle = |cx: &mut Context<Self>| cx.focus_handle().tab_index(0).tab_stop(true);
        Self {
            hint_theme: Rc::new(theme.clone()),
            theme,
            text: text.to_owned(),
            applied: text.to_owned(),
            pending: None,
            typed: Vec::new(),
            errors: vec![None; rows().len()],
            section: Section::Appearance,
            search,
            query: String::new(),
            handles: rows().iter().map(|_| handle(cx)).collect(),
            fields,
            tabs: Section::ALL.iter().map(|_| handle(cx)).collect(),
            fonts: None,
            font_open: false,
            font_at: 0,
            moved: None,
            turns: 0,
            placed: Vec::new(),
            narrow: false,
            keys: None,
            key_handles: crate::keymap::current().commands().iter().map(|_| handle(cx)).collect(),
            recording: None,
            scroll: ScrollHandle::new(),
            font_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The file's text as the form holds it.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The section shown when nothing is searched.
    #[must_use]
    pub const fn section(&self) -> Section {
        self.section
    }

    /// Why row `ix`'s value was not written, if it was not.
    #[must_use]
    pub fn error(&self, ix: usize) -> Option<&str> {
        self.errors.get(ix).and_then(Option::as_deref)
    }

    /// Take `text` (the dialog's TOML field, edited by hand) and show what it holds. It is
    /// applied with the next change a row makes.
    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        text.clone_into(&mut self.text);
        self.pending = None;
        self.typed.clear();
        self.errors.fill(None);
        for (row, field) in rows().iter().zip(&self.fields) {
            let Some(field) = field else { continue };
            let value = field_text(text, row);
            if field.read(cx).value().as_ref() != value {
                field.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
        cx.notify();
    }

    /// Write what is still waiting for a pause now, and hand back the text to apply if it
    /// changed since the last one: the dialog is closing or turning to the file's face.
    pub fn flush(&mut self, cx: &mut Context<Self>) -> Option<String> {
        self.pending = None;
        self.check_typed(cx);
        cx.notify();
        (self.text != self.applied).then(|| {
            self.applied.clone_from(&self.text);
            self.text.clone()
        })
    }

    /// New tokens.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.hint_theme = Rc::new(theme.clone());
            self.theme = theme;
            cx.notify();
        }
    }

    /// Give the search field the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |state, cx| state.focus(window, cx));
    }

    /// What takes the keyboard for `section`'s tab in the sidebar.
    #[must_use]
    pub fn tab_handle(&self, section: Section) -> Option<FocusHandle> {
        self.tabs.get(section.index()).cloned()
    }

    /// Whether a text field (the search or a row's) holds the keyboard: its Escape and Return
    /// are its own actions, which the dialog answers.
    #[must_use]
    pub fn typing(&self, window: &Window, cx: &App) -> bool {
        self.fields_iter().any(|field| field.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Whether an input method holds uncommitted text in one of the form's fields (a Telex
    /// word, kana before conversion): the keys it reads then (the arrows, Enter, Esc) are its
    /// own.
    #[must_use]
    pub fn composing(&self, cx: &App) -> bool {
        self.fields_iter().any(|field| field.read(cx).is_composing())
    }

    /// The search field and every field of the form.
    fn fields_iter(&self) -> impl Iterator<Item = &Entity<InputState>> {
        std::iter::once(&self.search).chain(self.fields.iter().flatten())
    }

    /// The height the form asks for; a short window gets less and the page scrolls.
    #[must_use]
    pub fn height(theme: &Theme) -> f32 {
        PAGE_ROWS * theme.density.row_two_line
    }

    /// The rows shown, in order: the query's matches across every section, else the chosen
    /// section's rows, or every row when the window has no room for the sidebar.
    fn visible(&self) -> Vec<usize> {
        rows()
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if self.query.trim().is_empty() {
                    self.narrow || r.section == self.section
                } else {
                    crate::picker::matches(&self.query, &r.haystack())
                }
            })
            .map(|(ix, _)| ix)
            .collect()
    }

    // ----- values ----------------------------------------------------------------------------

    /// Row `ix`'s value in the file, else its default.
    fn value(&self, row: &Row) -> Value {
        edit::read(&self.text, row.table(), row.key()).unwrap_or_else(|| row.field.default.clone())
    }

    fn switch_on(&self, row: &Row) -> bool {
        match (self.value(row), &row.field.default) {
            (Value::Bool(on), _) | (_, &Value::Bool(on)) => on,
            _ => false,
        }
    }

    /// The option the file holds, else the default's, else the first.
    fn choice(&self, row: &Row, options: &'static [Choice]) -> Option<&'static Choice> {
        let spelled = |value: Value| match value {
            Value::Str(value) => Some(value),
            Value::Bool(on) => Some(on.to_string()),
            Value::Number(_) | Value::List(_) | Value::Other(_) => None,
        };
        let find =
            |value: Option<String>| options.iter().find(|c| Some(&c.value) == value.as_ref());
        find(spelled(self.value(row)))
            .or_else(|| find(spelled(row.field.default.clone())))
            .or_else(|| options.first())
    }

    fn number(&self, row: &Row) -> f64 {
        match (self.value(row), &row.field.default) {
            (Value::Number(n), _) | (_, &Value::Number(n)) => n,
            _ => 0.0,
        }
    }

    fn family(&self, row: &Row) -> SharedString {
        match (self.value(row), &row.field.default) {
            (Value::Str(family), _) if !family.trim().is_empty() => family.into(),
            (_, Value::Str(family)) => family.clone().into(),
            _ => SharedString::default(),
        }
    }

    // ----- edits -----------------------------------------------------------------------------

    /// Write `literal` as row `ix`'s value into the text, when the key takes it; otherwise
    /// the row says why and the text stays as it was.
    fn write(&mut self, ix: usize, literal: &str) {
        let Some(row) = rows().get(ix) else { return };
        let checked = row.field.check(literal);
        if let Some(error) = self.errors.get_mut(ix) {
            *error = checked.as_ref().err().cloned();
        }
        if checked.is_ok() {
            self.text = edit::write(&self.text, row.table(), row.key(), literal);
        }
    }

    /// Hand the text to the dialog to apply now, along with anything still waiting.
    fn apply(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.flush(cx) {
            cx.emit(SettingsFormEvent::Apply(text));
        }
    }

    /// Apply once the hand pauses: each call starts the wait again.
    fn apply_later(&mut self, cx: &mut Context<Self>) {
        self.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _gone = this.update(cx, Self::apply);
        }));
        cx.notify();
    }

    /// A keystroke in row `ix`'s field: the row's error goes, since the fix is in progress, and
    /// the value is checked and written once the typing pauses.
    fn type_into(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.typed.contains(&ix) {
            self.typed.push(ix);
        }
        if let Some(error) = self.errors.get_mut(ix) {
            *error = None;
        }
        self.apply_later(cx);
    }

    /// Write what was typed in the fields since the last check.
    fn check_typed(&mut self, cx: &App) {
        for ix in std::mem::take(&mut self.typed) {
            let (Some(row), Some(Some(field))) = (rows().get(ix), self.fields.get(ix)) else {
                continue;
            };
            let typed = field.read(cx).value().to_string();
            if typed == field_text(&self.text, row) {
                continue;
            }
            let literal = match row.field.kind {
                Kind::List => edit::list(&typed),
                _ => edit::quoted(typed.trim()),
            };
            self.write(ix, &literal);
        }
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        let on = !self.switch_on(row);
        self.moved = Some((ix, self.turns));
        self.turns = self.turns.wrapping_add(1);
        self.write(ix, if on { "true" } else { "false" });
        self.apply(cx);
    }

    fn choose(&mut self, ix: usize, value: &str, cx: &mut Context<Self>) {
        self.write(ix, &edit::quoted(value));
        self.apply(cx);
    }

    /// Move row `ix`'s choice one option to the left or the right, stopping at the ends.
    fn shift_choice(&mut self, ix: usize, right: bool, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        let Kind::Choice(options) = &row.field.kind else { return };
        let current = self.choice(row, options);
        let at = options.iter().position(|c| Some(c) == current).unwrap_or_default();
        let next = if right { at.saturating_add(1) } else { at.saturating_sub(1) };
        if let Some(choice) = options.get(next) {
            self.choose(ix, &choice.value, cx);
        }
    }

    /// Step row `ix`'s number once, up or down; it is written once the clicks pause.
    fn nudge(&mut self, ix: usize, up: bool, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        let Kind::Number(n) = &row.field.kind else { return };
        let next = schema::snap(schema::next(self.number(row), n.step, up), n.min, n.max, n.step);
        self.write(ix, &schema::number(next, n.step, n.integer));
        self.apply_later(cx);
    }

    fn pick_font(&mut self, ix: usize, family: &str, cx: &mut Context<Self>) {
        self.font_open = false;
        self.write(ix, &edit::quoted(family));
        self.apply(cx);
    }

    /// Open the font list on the family in use, looking for the families the first time;
    /// or close it.
    fn toggle_fonts(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.font_open = !self.font_open;
        if self.font_open {
            self.font_at = self.current_font_at(ix);
            if self.fonts.is_none() {
                Self::find_fonts(ix, cx);
            }
        }
        cx.notify();
    }

    fn current_font_at(&self, ix: usize) -> usize {
        let Some(row) = rows().get(ix) else { return 0 };
        let family = self.family(row);
        self.fonts
            .as_ref()
            .and_then(|fonts| fonts.iter().position(|f| *f == family))
            .unwrap_or_default()
    }

    /// Ask the text system which installed families are monospace, off the main thread: it
    /// loads every family once, which is not a thing to do between two frames.
    fn find_fonts(ix: usize, cx: &Context<Self>) {
        let text_system = Arc::clone(cx.text_system());
        let finding = cx.background_spawn(async move { monospace_families(&text_system) });
        cx.spawn(async move |this, cx| {
            let fonts = finding.await;
            let _gone = this.update(cx, |this, cx| {
                this.fonts = Some(fonts);
                this.font_at = this.current_font_at(ix);
                this.font_scroll.scroll_to_item(this.font_at);
                cx.notify();
            });
        })
        .detach();
    }

    // ----- keyboard --------------------------------------------------------------------------

    /// Give row `ix`'s control the keyboard and bring it into view.
    fn focus_row(&self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.fields.get(ix) {
            Some(Some(field)) => field.update(cx, |state, cx| state.focus(window, cx)),
            _ => {
                if let Some(handle) = self.handles.get(ix) {
                    window.focus(handle, cx);
                }
            }
        }
        if let Some((_, child)) = self.placed.iter().find(|(row, _)| *row == ix) {
            self.scroll.scroll_to_item(*child);
        }
        cx.notify();
    }

    /// The row after (or before) `from` takes the keyboard; above the first, the search does.
    fn step_from(
        &mut self,
        from: Option<usize>,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.visible();
        let at = from.and_then(|from| rows.iter().position(|row| *row == from));
        let target = match (at, down) {
            (None, true) => rows.first().copied(),
            (None, false) => None,
            (Some(at), true) => rows.get(at.saturating_add(1)).copied(),
            (Some(0), false) => {
                self.font_open = false;
                self.focus(window, cx);
                return;
            }
            (Some(at), false) => rows.get(at.saturating_sub(1)).copied(),
        };
        if let Some(target) = target {
            self.font_open = false;
            self.focus_row(target, window, cx);
        }
    }

    /// A key on row `ix`'s control (not a field: a field has its own actions).
    fn control_key(
        &mut self,
        ix: usize,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &ev.keystroke;
        let mods = stroke.modifiers;
        if mods.platform && stroke.key == "enter" {
            cx.emit(SettingsFormEvent::Done);
            cx.stop_propagation();
            return;
        }
        if mods.platform || mods.control || mods.alt || mods.function {
            return;
        }
        let Some(row) = rows().get(ix) else { return };
        let listing = self.font_open && row.field.kind == Kind::Font;
        let handled = match (stroke.key.as_str(), &row.field.kind) {
            ("escape", _) if listing => {
                self.font_open = false;
                cx.notify();
                true
            }
            ("up" | "down", _) if listing => {
                let count = self.fonts.as_ref().map_or(0, Vec::len);
                self.font_at = if stroke.key == "up" {
                    self.font_at.saturating_sub(1)
                } else {
                    self.font_at.saturating_add(1).min(count.saturating_sub(1))
                };
                self.font_scroll.scroll_to_item(self.font_at);
                cx.notify();
                true
            }
            ("up", _) => {
                self.step_from(Some(ix), false, window, cx);
                true
            }
            ("down", _) => {
                self.step_from(Some(ix), true, window, cx);
                true
            }
            ("left" | "right", Kind::Choice(_)) => {
                self.shift_choice(ix, stroke.key == "right", cx);
                true
            }
            ("left" | "-", Kind::Number(_)) => {
                self.nudge(ix, false, cx);
                true
            }
            ("right" | "+" | "=", Kind::Number(_)) => {
                self.nudge(ix, true, cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn select(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            self.search.update(cx, |state, cx| state.set_value("", window, cx));
            self.query.clear();
        }
        self.section = section;
        self.font_open = false;
        self.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// ↑ and ↓ move between the sections, taking the page along; → goes into the page.
    fn tab_key(
        &mut self,
        section: Section,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &ev.keystroke;
        if stroke.modifiers.platform && stroke.key == "enter" {
            cx.emit(SettingsFormEvent::Done);
            cx.stop_propagation();
            return;
        }
        let at = section.index();
        let next = match stroke.key.as_str() {
            "up" => Section::ALL.get(at.saturating_sub(1)),
            "down" => Section::ALL.get(at.saturating_add(1)),
            "right" => {
                self.select(section, window, cx);
                self.step_from(None, true, window, cx);
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        if let Some(&next) = next {
            self.select(next, window, cx);
            if let Some(handle) = self.tab_handle(next) {
                window.focus(&handle, cx);
            }
        }
        cx.stop_propagation();
    }

    // ----- drawing ---------------------------------------------------------------------------

    fn search_field(&self, cx: &Context<Self>) -> Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        well(theme)
            .id("settings-search")
            .debug_selector(|| "settings-search".to_owned())
            .flex_none()
            .gap(px(theme.spacing.xs))
            .px(px(crate::kit::FIELD_INSET))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                if !this.composing(cx) {
                    this.step_from(None, true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(crate::icons::icon(
                theme,
                IconName::Search,
                IconSize::Inline,
                hsla(s.text_muted),
            ))
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.search)
                        .appearance(false)
                        .px_0()
                        .text_size(px(theme.typography.ui_size))
                        .aria_label(SEARCH_PLACEHOLDER),
                ),
            )
    }

    fn sidebar(&self, cx: &Context<Self>) -> Div {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let searching = !self.query.trim().is_empty();
        let tabs = Section::ALL.iter().zip(&self.tabs).map(|(&section, handle)| {
            let selected = !searching && section == self.section;
            let el = div()
                .id(("settings-section", section.index()))
                .debug_selector(move || format!("settings-section-{}", section.index()))
                .track_focus(handle)
                .role(gpui::accesskit::Role::Tab)
                .aria_label(section.label())
                .aria_selected(selected)
                .flex_none()
                .h(px(theme.density.row))
                // The pages that set nothing stand apart from the ones that do.
                .when(section == Section::Keyboard, |el| el.mt(px(spacing.md)))
                .flex()
                .items_center()
                .px(px(spacing.sm))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .map(|el| {
                    if selected {
                        el.bg(hsla(s.overlay))
                            .text_color(hsla(s.text))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    } else {
                        el.text_color(hsla(s.text_secondary))
                            .hover(move |el| el.bg(hsla(s.raised)).text_color(hsla(s.text)))
                    }
                })
                .on_click(
                    cx.listener(move |this, _ev, window, cx| this.select(section, window, cx)),
                )
                .on_key_down(cx.listener(move |this, ev, window, cx| {
                    this.tab_key(section, ev, window, cx);
                }))
                .child(section.label());
            crate::a11y::tab_stop(el, s.accent)
        });
        div()
            .flex_none()
            .w(px(NAV_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(spacing.xxs))
            .p(px(spacing.sm))
            // A tone step from the page, not a rule: the sidebar of the window's own frame.
            .bg(hsla(s.panel))
            .child(self.search_field(cx))
            .child(
                div()
                    .id("settings-sections")
                    .role(gpui::accesskit::Role::TabList)
                    .aria_label("Sections")
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xxs))
                    .pt(px(spacing.sm))
                    .children(tabs),
            )
    }

    /// A quiet label over the rows it names: a group on a section's page, a section's name
    /// over its rows in a search or a single column.
    ///
    /// The page's first label stands on the search field's line: the field's top and height,
    /// its words centred, so the two columns start together.
    fn heading(&self, text: &'static str, n: usize, first: bool) -> AnyElement {
        let theme = &self.theme;
        let label = crate::kit::label(theme, text)
            .id(("settings-heading", n))
            .debug_selector(move || format!("settings-heading-{n}"))
            .role(gpui::accesskit::Role::Heading)
            .aria_label(text);
        if first {
            label.mt(px(theme.spacing.sm)).h(px(theme.density.row)).flex().items_center()
        } else {
            label.pt(px(theme.spacing.lg)).pb(px(theme.spacing.xxs))
        }
        .into_any_element()
    }

    /// The Keyboard page's lines for the form's text: the keymap in effect with the text's
    /// `[keys]` over it.
    fn key_page(&mut self) -> &KeyPage {
        if self.keys.as_ref().is_some_and(|page| page.text != self.text) {
            self.keys = None;
        }
        self.keys.get_or_insert_with(|| KeyPage::read(&self.text))
    }

    /// Record the next chord typed as command `ix`'s, or stop recording it when it is.
    ///
    /// The chord is taken before any binding sees it, so a chord the app binds (⌘T) is recorded
    /// rather than run; a keystroke in another window (a tile's own window) is left alone.
    /// Leaving the control stops it.
    fn record_keys(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.recording.take().is_some_and(|r| r.command == ix) {
            cx.notify();
            return;
        }
        let form = cx.entity().downgrade();
        let home = window.window_handle();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != home {
                return;
            }
            let _gone = form.update(cx, |form, cx| form.record(&event.keystroke, window, cx));
            cx.stop_propagation();
        });
        let blur = self.key_handles.get(ix).map(|handle| {
            window.focus(handle, cx);
            cx.on_blur(handle, window, |this, _window, cx| {
                this.recording = None;
                cx.notify();
            })
        });
        self.recording = Some(Recording { command: ix, said: None, _keys: keys, _blur: blur });
        cx.notify();
    }

    /// A chord typed while recording: it becomes the command's, alone. Esc keeps what the
    /// command had and ⌫ leaves it none; a modifier on its own is not a chord yet, and Tab
    /// stops recording and goes along the ring as it does everywhere.
    ///
    /// A key the terminal or a field would type (a letter, Tab, an arrow) needs ⌘ or ⌃, or is
    /// an F key, except for a folder's rows, which bare keys walk. A key that cannot be one
    /// keeps the recording and says why.
    fn record(&mut self, stroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.recording.as_ref().map(|r| r.command) else { return };
        let m = stroke.modifiers;
        let bare = !(m.control || m.alt || m.shift || m.platform);
        let key = stroke.key.as_str();
        if matches!(key, "shift" | "control" | "alt" | "platform" | "cmd" | "fn" | "function") {
            return;
        }
        if key == "tab" && !(m.control || m.alt || m.platform) {
            self.recording = None;
            if m.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            cx.notify();
            return;
        }
        let literal = match key {
            "escape" if bare => None,
            "backspace" if bare => Some(edit::quoted("")),
            _ => {
                let bare_ok = crate::keymap::current()
                    .commands()
                    .get(ix)
                    .is_some_and(crate::keymap::Command::takes_bare_keys);
                let said = match crate::keymap::canonical(&chord_text(stroke)) {
                    Ok(chord) if m.platform || m.control || function_key(key) || bare_ok => {
                        Ok(chord)
                    }
                    Ok(_) => Err(NEEDS_MODIFIER),
                    Err(_) => Err(CANT_BIND),
                };
                match said {
                    Ok(chord) => Some(edit::quoted(&chord)),
                    Err(why) => {
                        if let Some(recording) = &mut self.recording {
                            recording.said = Some(why);
                        }
                        cx.notify();
                        return;
                    }
                }
            }
        };
        self.recording = None;
        match literal {
            Some(literal) => {
                self.write_keys(ix, Some(&literal));
                self.apply(cx);
            }
            None => cx.notify(),
        }
    }

    /// Put command `ix`'s default chords back: its line leaves the file.
    fn reset_keys(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.recording = None;
        self.write_keys(ix, None);
        self.apply(cx);
    }

    /// Set command `ix`'s chords in `[keys.<scope>]` to `literal`, or take its line out.
    fn write_keys(&mut self, ix: usize, literal: Option<&str>) {
        let keymap = crate::keymap::current();
        let Some(command) = keymap.commands().get(ix) else { return };
        let table = format!("keys.{}", command.scope().name());
        self.text = match literal {
            Some(literal) => edit::write(&self.text, &table, command.name(), literal),
            None => edit::remove(&self.text, &table, command.name()),
        };
    }

    /// A command: what it does and its name in the file, then its chords on key caps, which a
    /// press records anew, and a way back to its defaults when the file sets it.
    fn key_line(&self, row: &KeyRow, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let ix = row.command;
        let recording = self.recording.as_ref().filter(|r| r.command == ix).map(|r| r.said);
        let prompt = recording.map(|said| said.unwrap_or(PRESS_KEYS));
        let meta = if row.set {
            let was =
                if row.defaults.is_empty() { NO_KEYS.to_owned() } else { row.defaults.join(", ") };
            format!("{} \u{b7} default {was}", row.key)
        } else {
            row.key.clone()
        };
        let chords: AnyElement = if let Some(prompt) = prompt {
            div().text_color(hsla(s.accent)).child(prompt).into_any_element()
        } else if row.keys.is_empty() {
            div().text_color(hsla(s.text_muted)).child(NO_KEYS).into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(spacing.xxs))
                .children(row.keys.iter().map(|k| crate::kit::key_cap(theme, k.clone())))
                .into_any_element()
        };
        let well = well(theme)
            .id(("settings-chord", ix))
            .debug_selector(move || format!("settings-chord-{ix}"))
            .role(gpui::accesskit::Role::Button)
            .aria_label(format!("Keys for {}", row.label))
            .aria_value(prompt.map_or_else(|| row.keys.join(", "), str::to_owned))
            .min_w(px(FIELD_WIDTH))
            .justify_end()
            .px(px(crate::kit::FIELD_INSET))
            .text_size(px(theme.typography.small()))
            .cursor_pointer()
            .when(prompt.is_none(), |el| el.hover(move |el| el.bg(hsla(s.overlay))))
            .on_click(cx.listener(move |this, _ev, window, cx| this.record_keys(ix, window, cx)))
            .child(chords);
        let well = match self.key_handles.get(ix) {
            Some(handle) => well.track_focus(handle),
            None => well,
        };
        let reset = row.set.then(|| {
            crate::kit::icon_button(
                theme,
                format!("settings-key-reset-{ix}"),
                IconName::Undo2,
                RESET_KEYS,
            )
            .on_click(cx.listener(move |this, _ev, _window, cx| this.reset_keys(ix, cx)))
        });
        div()
            .id(("settings-key", ix))
            .debug_selector(move || format!("settings-key-{ix}"))
            .role(gpui::accesskit::Role::ListItem)
            .aria_label(row.label.clone())
            .aria_value(row.keys.join(", "))
            .aria_description(row.key.clone())
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .min_h(px(theme.density.row_two_line))
            .py(px(spacing.xs))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xxs))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme.typography.ui_size))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(s.text))
                            .child(row.label.clone()),
                    )
                    .child(crate::kit::meta(div(), theme).truncate().child(meta)),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .children(reset)
                    .child(crate::a11y::tab_stop(well, s.accent)),
            )
            .into_any_element()
    }

    /// What the text's `[keys]` says that did not hold, over the Keyboard page's lines: the first
    /// thing, and how many more.
    fn keys_said(&self, said: &[String]) -> Option<AnyElement> {
        let first = said.first()?;
        let more = said.len().saturating_sub(1);
        let text = if more == 0 { first.clone() } else { format!("{first} (+{more} more)") };
        let theme = &self.theme;
        Some(
            crate::kit::meta(div(), theme)
                .id("settings-keys-said")
                .debug_selector(|| "settings-keys-said".to_owned())
                .role(gpui::accesskit::Role::Alert)
                .aria_label(text.clone())
                .pt(px(theme.spacing.sm))
                .text_color(hsla(theme.surfaces.error))
                .child(text)
                .into_any_element(),
        )
    }

    /// The About page: the app, its version and build on one quiet line, then its links.
    fn about(&self, first: bool, children: &mut Vec<AnyElement>) {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let (version, build) = schema::about();
        let said = format!("Version {version}");
        children.push(
            div()
                .id("settings-about")
                .debug_selector(|| "settings-about".to_owned())
                .role(gpui::accesskit::Role::Group)
                .aria_label("About")
                .aria_description(format!("{said}, {build}"))
                .when(first, |el| el.mt(px(spacing.sm)))
                .child(crate::kit::brand(
                    theme,
                    Some(
                        crate::kit::tabular(crate::kit::meta(div(), theme))
                            .flex()
                            .items_center()
                            .gap(px(spacing.xs))
                            .child(said)
                            .child(crate::kit::separator(theme))
                            .child(build)
                            .into_any_element(),
                    ),
                ))
                .into_any_element(),
        );
        children.push(self.heading("Links", children.len(), false));
        for (n, (words, url)) in schema::LINKS.into_iter().enumerate() {
            let link = div()
                .id(("settings-link", n))
                .debug_selector(move || format!("settings-link-{n}"))
                .role(gpui::accesskit::Role::Link)
                .aria_label(words)
                .aria_description(url)
                .flex_none()
                .h(px(theme.density.row))
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_size(px(theme.typography.ui_size))
                .text_color(hsla(s.accent))
                .cursor_pointer()
                .on_click(move |_ev, _window, _cx| slopty_platform::open_url(url))
                .child(words)
                .child(crate::icons::icon(
                    theme,
                    IconName::ExternalLink,
                    IconSize::Inline,
                    hsla(s.accent),
                ));
            children.push(crate::a11y::tab_stop(link, s.accent).into_any_element());
        }
    }

    fn page(&mut self, shown: &[usize], cx: &Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        let searching = !self.query.trim().is_empty();
        let by_section = self.narrow || searching;
        let whole_keyboard = !searching && (self.narrow || self.section == Section::Keyboard);
        let (keyboard, said): (Vec<KeyRow>, Vec<String>) = if searching {
            let query = self.query.clone();
            let page = self.key_page();
            let found = page.rows.iter().filter(|r| crate::picker::matches(&query, &r.haystack()));
            (found.cloned().collect(), Vec::new())
        } else if whole_keyboard {
            let page = self.key_page();
            (page.rows.clone(), page.said.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        let about = !searching && (self.narrow || self.section == Section::About);
        let mut children: Vec<AnyElement> = Vec::new();
        let mut placed = Vec::new();
        let mut last: Option<&'static str> = None;
        for &ix in shown {
            let Some(row) = rows().get(ix) else { continue };
            let heading = if by_section { row.section.label() } else { row.group };
            if last != Some(heading) {
                children.push(self.heading(heading, children.len(), last.is_none()));
                last = Some(heading);
            }
            placed.push((ix, children.len()));
            children.push(self.row(ix, row, cx));
            if self.font_open && row.field.kind == Kind::Font {
                children.push(self.font_list(ix, row, cx));
            }
        }
        if let Some(said) = self.keys_said(&said) {
            children.push(said);
        }
        for row in &keyboard {
            let heading = if by_section { Section::Keyboard.label() } else { row.group };
            if last != Some(heading) {
                children.push(self.heading(heading, children.len(), last.is_none()));
                last = Some(heading);
            }
            children.push(self.key_line(row, cx));
        }
        if about {
            if by_section {
                children.push(self.heading(Section::About.label(), children.len(), last.is_none()));
            }
            self.about(children.is_empty(), &mut children);
        }
        if children.is_empty() {
            children.push(quiet(&theme, "settings-empty", NO_MATCHES).into_any_element());
        }
        self.placed = placed;
        // What scrolls past an edge fades into the dialog's surface, so a row cut at the edge
        // reads as more to come.
        let (offset, most) = (self.scroll.offset().y, self.scroll.max_offset().y);
        let depth = px(theme.spacing.lg);
        let surface = theme.surfaces.elevated;
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(
                div()
                    .id("settings-page")
                    .debug_selector(|| "settings-page".to_owned())
                    .role(gpui::accesskit::Role::TabPanel)
                    .aria_label(if by_section { "Settings" } else { self.section.label() })
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px(px(theme.spacing.inset()))
                    .pb(px(theme.spacing.md))
                    .children(children),
            )
            .when(offset < px(0.0), |el| {
                el.child(crate::kit::edge_fade(crate::kit::Edge::Top, surface, depth))
            })
            .when(-offset < most, |el| {
                el.child(crate::kit::edge_fade(crate::kit::Edge::Bottom, surface, depth))
            })
            .into_any_element()
    }

    /// A row: its label, and under it the line on what it does, or why the value typed was not
    /// written, in the error's colour, until it is typed again.
    fn row(&self, ix: usize, row: &Row, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let under = if let Some(error) = self.error(ix) {
            crate::kit::meta(div(), theme)
                .id(("settings-row-error", ix))
                .debug_selector(move || format!("settings-row-error-{ix}"))
                .role(gpui::accesskit::Role::Alert)
                .aria_label(error.to_owned())
                .text_color(hsla(s.error))
                .child(SharedString::from(error.to_owned()))
                .into_any_element()
        } else {
            // One line, so every row stands as tall as the next; the whole of it is the line's
            // hint.
            let (meta, hint_theme) = (row.meta(), Rc::clone(&self.hint_theme));
            crate::kit::meta(div(), theme)
                .id(("settings-row-meta", ix))
                .debug_selector(move || format!("settings-row-meta-{ix}"))
                .truncate()
                .child(meta)
                .tooltip(move |_window, cx| {
                    let theme = Rc::clone(&hint_theme);
                    cx.new(|_| crate::kit::Hint::new(meta, "", theme)).into()
                })
                .into_any_element()
        };
        div()
            .id(("settings-row", ix))
            .debug_selector(move || format!("settings-row-{ix}"))
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .min_h(px(theme.density.row_two_line))
            .py(px(spacing.xs))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.xxs))
                    .child(
                        div()
                            .text_size(px(theme.typography.ui_size))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                            .text_color(hsla(s.text))
                            .child(row.label()),
                    )
                    .child(under),
            )
            .child(div().flex_none().child(self.control(ix, row, cx)))
            .into_any_element()
    }

    fn control(&self, ix: usize, row: &Row, cx: &Context<Self>) -> AnyElement {
        match &row.field.kind {
            Kind::Switch => self.switch(ix, row, self.switch_on(row), cx),
            Kind::Choice(options) => {
                self.segmented(ix, row, options, self.choice(row, options), cx)
            }
            Kind::Number(_) => self.stepper(ix, row, cx),
            Kind::Font => self.font(ix, row, self.family(row), cx),
            Kind::Text | Kind::List | Kind::Colour => self.field(ix, row, cx),
        }
    }

    /// `el` as row `ix`'s control: its focus handle on the keyboard ring, its keys, its
    /// accessible name and what it does (or why its value was not written).
    fn stop(&self, ix: usize, row: &Row, el: Stateful<Div>, cx: &Context<Self>) -> Stateful<Div> {
        let el = match self.handles.get(ix) {
            Some(handle) => el.track_focus(handle),
            None => el,
        };
        let described = self.error(ix).unwrap_or_else(|| row.meta()).to_owned();
        let el = el.aria_label(row.label()).aria_description(described).on_key_down(cx.listener(
            move |this, ev, window, cx| {
                this.control_key(ix, ev, window, cx);
            },
        ));
        crate::a11y::tab_stop(el, self.theme.surfaces.accent)
    }

    /// A switch: the accent track with the knob at its end when on, a quiet one when off. The
    /// knob slides when it is turned, unless motion is reduced.
    fn switch(&self, ix: usize, row: &Row, on: bool, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let (height, knob) = (spacing.lg, 2.0_f32.mul_add(-spacing.xxs, spacing.lg));
        let width = spacing.xl + spacing.xs;
        let travel = 2.0_f32.mul_add(-spacing.xxs, width - knob);
        let (track, ink) =
            if on { (s.accent_fill, s.accent_ink) } else { (s.overlay, s.text_secondary) };
        let to = if on { travel } else { 0.0 };
        let dot = div()
            .debug_selector(move || format!("settings-knob-{ix}"))
            .flex_none()
            .size(px(knob))
            .rounded_full()
            .bg(hsla(ink));
        let dot = match self.moved {
            Some((moved, turn)) if moved == ix && crate::kit::motion(cx) => {
                let from = travel - to;
                let slide = crate::kit::Pace::Settle.animation();
                dot.with_animation(("settings-knob", turn), slide, move |el, t| {
                    el.ml(px((to - from).mul_add(t, from)))
                })
                .into_any_element()
            }
            _ => dot.ml(px(to)).into_any_element(),
        };
        let el = div()
            .id(("settings-switch", ix))
            .debug_selector(move || format!("settings-switch-{ix}"))
            .role(gpui::accesskit::Role::Switch)
            .aria_toggled(if on {
                gpui::accesskit::Toggled::True
            } else {
                gpui::accesskit::Toggled::False
            })
            .flex_none()
            .w(px(width))
            .h(px(height))
            .flex()
            .items_center()
            .px(px(spacing.xxs))
            .rounded_full()
            .bg(hsla(track))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _ev, _window, cx| this.toggle(ix, cx)))
            .child(dot);
        self.stop(ix, row, el, cx).into_any_element()
    }

    /// A segmented choice: the options side by side in a well, the chosen one on the selected
    /// fill. ← and → move it.
    fn segmented(
        &self,
        ix: usize,
        row: &Row,
        options: &'static [Choice],
        current: Option<&'static Choice>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let current_label = current.map_or("", |c| c.title.as_str());
        let segments = options.iter().enumerate().map(|(n, choice)| {
            let chosen = current == Some(choice);
            let (value, label) = (choice.value.as_str(), choice.title.as_str());
            div()
                .id(("option", n))
                .debug_selector(move || format!("settings-option-{ix}-{n}"))
                .role(gpui::accesskit::Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if chosen {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
                .h_full()
                .flex()
                .items_center()
                .px(px(spacing.sm))
                .rounded(px(theme.radii.xs))
                .text_size(px(theme.typography.small()))
                .cursor_pointer()
                .map(|el| {
                    if chosen {
                        el.bg(hsla(s.overlay))
                            .text_color(hsla(s.text))
                            .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                    } else {
                        el.text_color(hsla(s.text_secondary))
                            .hover(move |el| el.text_color(hsla(s.text)))
                    }
                })
                .on_click(cx.listener(move |this, _ev, _window, cx| this.choose(ix, value, cx)))
                .child(label)
        });
        let el = well(theme)
            .id(("settings-choice", ix))
            .debug_selector(move || format!("settings-choice-{ix}"))
            .role(gpui::accesskit::Role::RadioGroup)
            .aria_value(current_label)
            .gap(px(spacing.xxs))
            .p(px(spacing.xxs))
            .children(segments);
        self.stop(ix, row, el, cx).into_any_element()
    }

    /// A number between its bounds: − and + round the value and its unit. ← and → step it.
    fn stepper(&self, ix: usize, row: &Row, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let Kind::Number(n) = &row.field.kind else {
            return gpui::Empty.into_any_element();
        };
        let (min, max, step, unit) = (n.min, n.max, n.step, n.unit.as_str());
        let value = self.number(row);
        let joined = unit.starts_with(|c: char| !c.is_alphanumeric());
        let readout =
            format!("{}{}{unit}", schema::figure(value, step), if joined { "" } else { " " });
        let side = 2.0_f32.mul_add(-spacing.xxs, theme.density.row);
        let button = |up: bool| {
            let live = if up { value < max } else { value > min };
            let (icon, label) =
                if up { (IconName::Plus, "Increase") } else { (IconName::Minus, "Decrease") };
            let ink = if live { s.text_secondary } else { s.text_muted };
            div()
                .id(if up { "increase" } else { "decrease" })
                .debug_selector(move || format!("settings-{label}-{ix}").to_lowercase())
                .role(gpui::accesskit::Role::Button)
                .aria_label(label)
                .flex_none()
                .size(px(side))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(theme.radii.xs))
                .when(live, |el| {
                    el.cursor_pointer()
                        .hover(move |el| el.bg(hsla(s.overlay)))
                        .on_click(cx.listener(move |this, _ev, _window, cx| this.nudge(ix, up, cx)))
                })
                .child(crate::icons::icon(theme, icon, IconSize::Inline, hsla(ink)))
        };
        let el = well(theme)
            .id(("settings-stepper", ix))
            .debug_selector(move || format!("settings-stepper-{ix}"))
            .role(gpui::accesskit::Role::SpinButton)
            .aria_value(readout.clone())
            .aria_numeric_value(value)
            .aria_min_numeric_value(min)
            .aria_max_numeric_value(max)
            .aria_numeric_value_step(step)
            .px(px(spacing.xxs))
            .child(button(false))
            .child(
                crate::kit::tabular(div())
                    .min_w(px(theme.typography.ui_size * 4.0))
                    .flex()
                    .justify_center()
                    .text_size(px(theme.typography.ui_size))
                    .child(readout),
            )
            .child(button(true));
        self.stop(ix, row, el, cx).into_any_element()
    }

    /// The terminal's family, drawn in itself; a press opens the list of monospace families
    /// under the row.
    fn font(&self, ix: usize, row: &Row, family: SharedString, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let el = well(theme)
            .id(("settings-font", ix))
            .debug_selector(|| "settings-font".to_owned())
            .role(gpui::accesskit::Role::ComboBox)
            .aria_value(family.clone())
            .aria_expanded(self.font_open)
            .w(px(FONT_WIDTH))
            .justify_between()
            .gap(px(spacing.xs))
            .px(px(crate::kit::FIELD_INSET))
            .cursor_pointer()
            .on_click(cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                // Return on the open list picks the family the keyboard is on.
                let at = this.font_at;
                let picked = this.fonts.as_ref().and_then(|fonts| fonts.get(at)).cloned();
                match picked {
                    Some(family) if this.font_open && ev.is_keyboard() => {
                        this.pick_font(ix, &family, cx);
                    }
                    _ => this.toggle_fonts(ix, cx),
                }
            }))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(family.clone())
                    .text_size(px(theme.typography.ui_size))
                    .child(family),
            )
            .child(crate::icons::icon(
                theme,
                IconName::ChevronDown,
                IconSize::Inline,
                hsla(s.text_muted),
            ));
        self.stop(ix, row, el, cx).into_any_element()
    }

    /// The installed monospace families, each in its own face, the one in use ticked.
    fn font_list(&self, ix: usize, row: &Row, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let current = self.family(row);
        let Some(fonts) = self.fonts.as_ref() else {
            return quiet(theme, "settings-fonts-finding", FINDING_FONTS).into_any_element();
        };
        let rows = fonts.iter().enumerate().map(|(n, family)| {
            let on = *family == current;
            let at = n == self.font_at;
            let picked = family.clone();
            div()
                .id(("font", n))
                .role(gpui::accesskit::Role::ListBoxOption)
                .aria_label(family.clone())
                .aria_selected(at)
                .flex_none()
                .h(px(theme.density.row))
                .flex()
                .items_center()
                .justify_between()
                .px(px(spacing.sm))
                .rounded(px(theme.radii.sm))
                .cursor_pointer()
                .when(at, |el| el.bg(hsla(s.overlay)))
                .when(!at, |el| el.hover(move |el| el.bg(hsla(s.overlay))))
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.pick_font(ix, &picked, cx);
                }))
                .child(div().font_family(family.clone()).child(family.clone()))
                .when(on, |el| {
                    el.child(crate::icons::icon(
                        theme,
                        IconName::Check,
                        IconSize::Inline,
                        hsla(s.accent),
                    ))
                })
        });
        div()
            .id("settings-fonts")
            .debug_selector(|| "settings-fonts".to_owned())
            .role(gpui::accesskit::Role::ListBox)
            .aria_label(row.label())
            .flex_none()
            .max_h(px(2.0_f32.mul_add(spacing.xxs, FONT_ROWS * theme.density.row)))
            .overflow_y_scroll()
            .track_scroll(&self.font_scroll)
            .mb(px(spacing.sm))
            .p(px(spacing.xxs))
            .rounded(px(theme.radii.sm))
            .bg(hsla(s.raised))
            .text_size(px(theme.typography.ui_size))
            .children(rows)
            .into_any_element()
    }

    /// A field typed into: a host, a list of addresses, a colour with its swatch.
    fn field(&self, ix: usize, row: &Row, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let Some(Some(state)) = self.fields.get(ix) else {
            return gpui::Empty.into_any_element();
        };
        let swatch = match row.field.kind {
            Kind::Colour => {
                let own = match self.value(row) {
                    Value::Str(hex) => parse_hex(&hex),
                    _ => None,
                };
                own.or_else(|| theme_colour(theme, row.key())).map(|colour| {
                    div()
                        .flex_none()
                        .size(px(theme.typography.icon()))
                        .rounded(px(theme.radii.xs))
                        .border_1()
                        .border_color(hsla(s.border))
                        .bg(hsla(colour))
                })
            }
            _ => None,
        };
        well(theme)
            .id(("settings-field", ix))
            .debug_selector(move || format!("settings-field-{ix}"))
            .w(px(FIELD_WIDTH))
            .gap(px(spacing.xs))
            .px(px(crate::kit::FIELD_INSET))
            .capture_action(cx.listener(move |this, _: &MoveUp, window, cx| {
                if !this.composing(cx) {
                    this.step_from(Some(ix), false, window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
                if !this.composing(cx) {
                    this.step_from(Some(ix), true, window, cx);
                    cx.stop_propagation();
                }
            }))
            .children(swatch)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(state)
                        .appearance(false)
                        .px_0()
                        .text_size(px(theme.typography.ui_size))
                        .aria_label(row.label()),
                ),
            )
            .into_any_element()
    }
}

/// A control's well: a row tall, the raised fill, no hairline, as a field is drawn.
fn well(theme: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(theme.density.row))
        .flex()
        .items_center()
        .rounded(px(theme.radii.sm))
        .bg(hsla(theme.surfaces.raised))
}

/// A line said in passing on the page: nothing matched, the fonts are being looked for. On the
/// page's own edge, where the palette's quiet line pads itself in from the sheet's.
fn quiet(theme: &Theme, id: &'static str, text: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Status)
        .aria_label(text)
        .py(px(theme.spacing.sm))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text)
}

/// The Keyboard page's lines for one text of the file, and what its `[keys]` said that did not
/// hold.
struct KeyPage {
    text: String,
    rows: Vec<KeyRow>,
    said: Vec<String>,
}

impl KeyPage {
    fn read(text: &str) -> Self {
        let keys = slopty_settings::Settings::parse(text).settings.keys;
        let keymap = crate::keymap::current().with_keys(&keys);
        let rows = schema::key_rows(&keymap, &crate::workspace::palette_items());
        Self { text: text.to_owned(), rows, said: keymap.diagnostics().to_vec() }
    }
}

/// The command whose next chord is recorded, and what listens for it.
struct Recording {
    command: usize,
    /// Why the last key typed was not taken, until the next one.
    said: Option<&'static str>,
    /// Takes every keystroke before any binding does, while it lives.
    _keys: Subscription,
    /// Stops the recording when its control loses the keyboard.
    _blur: Option<Subscription>,
}

/// A keystroke in the palette's key syntax, its modifiers first (`cmd-shift-t`); the Fn key is
/// no part of a chord.
fn chord_text(stroke: &Keystroke) -> String {
    let m = stroke.modifiers;
    let mut out = String::new();
    for (on, word) in
        [(m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-"), (m.platform, "cmd-")]
    {
        if on {
            out.push_str(word);
        }
    }
    out.push_str(&stroke.key);
    out
}

/// Whether `key` is one of the F keys, which a terminal or a field never types.
fn function_key(key: &str) -> bool {
    key.strip_prefix('f').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What a text row's field shows for its value in `text`.
fn field_text(text: &str, row: &Row) -> String {
    match edit::read(text, row.table(), row.key()) {
        Some(Value::Str(value) | Value::Other(value)) => value,
        Some(Value::List(items)) => items.join(", "),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(on)) => on.to_string(),
        None => String::new(),
    }
}

/// `#rrggbb` or `rrggbb`.
fn parse_hex(text: &str) -> Option<Rgb> {
    let text = text.trim();
    let hex = text.strip_prefix('#').unwrap_or(text);
    (hex.len() == 6).then_some(())?;
    u32::from_str_radix(hex, 16).ok().map(Rgb::hex)
}

/// The theme's own colour that `[colors]` key stands in for, so an unset row shows it.
fn theme_colour(theme: &Theme, key: &str) -> Option<Rgb> {
    let t = &theme.terminal;
    Some(match key {
        "foreground" => t.fg,
        "background" => t.bg,
        "cursor" => t.cursor,
        "cursor_text" => t.cursor_text,
        "selection" => t.selection,
        _ => return None,
    })
}

/// The bundled face, then every installed family whose `i` is as wide as its `M`.
fn monospace_families(text_system: &gpui::TextSystem) -> Vec<SharedString> {
    let size = px(16.0);
    let mut out: Vec<SharedString> = vec![crate::fonts::MONO_FAMILY.into()];
    for name in text_system.all_font_names() {
        let ours = name == crate::fonts::MONO_FAMILY || name == crate::fonts::SYMBOLS_FAMILY;
        // A leading dot is a system family's private name (`.SF NS`), not one to choose.
        if ours || name.starts_with('.') {
            continue;
        }
        let id = text_system.resolve_font(&gpui::font(name.clone()));
        let width = |c| text_system.advance(id, size, c).ok().map(|s| f32::from(s.width));
        if let (Some(narrow), Some(wide)) = (width('i'), width('M'))
            && narrow > 0.0
            && (narrow - wide).abs() < 0.01
        {
            out.push(name.into());
        }
    }
    out
}

impl EventEmitter<SettingsFormEvent> for SettingsForm {}

impl Focusable for SettingsForm {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }
}

impl Render for SettingsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.narrow = window.viewport_size().width < px(crate::kit::Overlay::List.bounds().0);
        let shown = self.visible();
        let page = self.page(&shown, cx);
        let root = div()
            .id("settings-form")
            .debug_selector(|| "settings-form".to_owned())
            .size_full()
            .min_h_0()
            .flex();
        if self.narrow {
            root.flex_col()
                .child(
                    crate::kit::inset_x(div(), &self.theme)
                        .flex_none()
                        .py(px(self.theme.spacing.sm))
                        .child(self.search_field(cx)),
                )
                .child(div().flex_1().min_h_0().flex().child(page))
        } else {
            root.child(self.sidebar(cx)).child(page)
        }
    }
}
