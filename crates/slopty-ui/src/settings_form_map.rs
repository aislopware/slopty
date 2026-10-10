//! The settings form's map rows: names the person picks, each with a value of one kind.
//!
//! `[clipboard.workers]` maps a machine's worker id to a switch, `[worker.acp]` an agent's name to
//! its command line.
//!
//! A map keyed by machine shows each entry by the machine's name, as the server lists it, and
//! takes a machine's name in its field, writing its id: the file keeps the id so a renamed
//! machine keeps its choice, while the person reads and types names. An id the server no longer
//! lists says so, with the start of the id to tell two apart.
//!
//! The row's control is the field a new name is typed in; ↩ adds it, set to its kind's first
//! value (a switch off, an empty command line), and the keyboard moves to it. Under the row's
//! words each entry is a line: its name, the control that sets it (a switch, or a field written
//! once the hand pauses, as a row's is) and a way to take it out. The file stays the one source
//! of truth: the lines are read from it and written into it an entry at a time
//! ([`edit::write_entry`], [`edit::remove_entry`]), in the form the map is written there, and
//! the last entry out takes its table with it.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use slopty_settings::edit::{self, Value};
use slopty_settings::schema::Kind;

use super::schema::{Row, rows};
use super::{SettingsForm, well};
use crate::colors::hsla;
use crate::icons::Symbol;

/// What a map row's field says before a name is typed.
pub const ADD_ENTRY: &str = "Add by name";

/// What takes an entry out, as a screen reader names it.
pub const REMOVE_ENTRY: &str = "Remove";

/// What a machine-keyed entry says for an id the server does not list.
pub const UNLISTED_MACHINE: &str = "Machine not on the server";

/// What a machine-keyed map's field says when no machine has the name typed.
#[must_use]
pub fn no_machine_named(name: &str) -> String {
    format!("No machine is named {name}")
}

/// Whether map row `row` is keyed by a machine's worker id.
fn keyed_by_machine(row: &Row) -> bool {
    row.table() == "clipboard" && row.key() == "workers"
}

/// One entry's own parts.
pub(super) struct EntryParts {
    /// What takes the keyboard for its switch.
    handle: FocusHandle,
    /// The field its value is typed in, for a value that is typed.
    field: Option<Entity<InputState>>,
    _typed: Option<Subscription>,
}

#[cfg(test)]
impl EntryParts {
    /// What takes the keyboard for its switch.
    pub(super) const fn handle(&self) -> &FocusHandle {
        &self.handle
    }

    /// The field its value is typed in.
    pub(super) const fn field(&self) -> Option<&Entity<InputState>> {
        self.field.as_ref()
    }
}

/// The kind of value map row `row`'s entries hold, when it is a map.
fn entry_kind(row: &Row) -> Option<&Kind> {
    match &row.field.kind {
        Kind::Map(entry) => Some(entry),
        _ => None,
    }
}

/// An entry's value as its field shows it.
fn entry_text(value: &Value) -> String {
    match value {
        Value::Str(text) | Value::Other(text) => text.clone(),
        Value::List(items) => items.join(", "),
        Value::Number(n) => n.to_string(),
        Value::Bool(on) => on.to_string(),
        Value::Map(_) => String::new(),
    }
}

/// What `typed` is as an entry of `kind`, written.
fn entry_literal(kind: &Kind, typed: &str) -> String {
    match kind {
        Kind::List => edit::list(typed),
        Kind::Number(_) | Kind::Switch => typed.trim().to_owned(),
        _ => edit::quoted(typed.trim()),
    }
}

/// A new entry's value of `kind`: a switch off, a list empty, a number zero, text empty.
const fn first_value(kind: &Kind) -> &'static str {
    match kind {
        Kind::Switch => "false",
        Kind::List => "[]",
        Kind::Number(_) => "0",
        _ => "\"\"",
    }
}

impl SettingsForm {
    /// How entry `key` of map row `row` reads: a machine's name for a map keyed by machine,
    /// else the key as the file writes it.
    pub(super) fn entry_name(&self, row: &Row, key: &str) -> String {
        if !keyed_by_machine(row) {
            return key.to_owned();
        }
        let listed =
            self.machines_listed().iter().find(|m| m.of.is_some_and(|w| w.to_string() == key));
        listed.map_or_else(
            || {
                let start: String = key.chars().take(8).collect();
                format!("{UNLISTED_MACHINE} ({start})")
            },
            |m| m.name.clone(),
        )
    }

    /// The key a name typed in map row `row`'s field is written under: for a map keyed by
    /// machine, the id of the worker of that name (case aside), or why there is none.
    fn entry_key(&self, row: &Row, typed: &str) -> Result<String, String> {
        if !keyed_by_machine(row) {
            return Ok(typed.to_owned());
        }
        self.machines_listed()
            .iter()
            .find_map(|m| m.of.filter(|_| m.name.eq_ignore_ascii_case(typed)))
            .map(|w| w.to_string())
            .ok_or_else(|| no_machine_named(typed))
    }

    /// Map row `row`'s entries in the file, in its order.
    pub(super) fn entries_of(&self, row: &Row) -> Vec<(String, Value)> {
        edit::entries(self.text_for(row), row.table(), row.key())
    }

    /// Make the parts of every map entry the file holds and drop those of the ones it no longer
    /// does; a typed entry the file changed under shows what the file says, unless it is being
    /// typed in.
    pub(super) fn sync_entries(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut seen = Vec::new();
        for (ix, row) in rows().iter().enumerate() {
            let Some(kind) = entry_kind(row) else { continue };
            for (name, value) in self.entries_of(row) {
                let key = (ix, name);
                let text = entry_text(&value);
                if let Some(parts) = self.entries.get(&key) {
                    let typing = self.entry_typed.contains(&key);
                    if let Some(field) = parts.field.as_ref().filter(|_| !typing)
                        && field.read(cx).value().as_ref() != text
                    {
                        field.update(cx, |state, cx| state.set_value(text, window, cx));
                    }
                } else {
                    let parts = Self::entry_parts(&key, kind, text, window, cx);
                    self.entries.insert(key.clone(), parts);
                }
                seen.push(key);
            }
        }
        self.entries.retain(|key, _| seen.contains(key));
        self.entry_typed.retain(|key| seen.contains(key));
    }

    /// The parts of entry `key`, of `kind`, showing `text`.
    fn entry_parts(
        key: &(usize, String),
        kind: &Kind,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> EntryParts {
        let handle = cx.focus_handle().tab_index(0).tab_stop(true);
        if matches!(kind, Kind::Switch) {
            return EntryParts { handle, field: None, _typed: None };
        }
        let field = cx.new(|cx| InputState::new(window, cx).default_value(text));
        let typed = key.clone();
        let sub = cx.subscribe(&field, move |this, _field, event, cx| {
            if matches!(event, InputEvent::Change) {
                if !this.entry_typed.contains(&typed) {
                    this.entry_typed.push(typed.clone());
                }
                if let Some(error) = this.errors.get_mut(typed.0) {
                    *error = None;
                }
                this.apply_later(cx);
            }
        });
        EntryParts { handle, field: Some(field), _typed: Some(sub) }
    }

    /// Write `literal` as map row `ix`'s entry `name`, when the map takes it; otherwise the row
    /// says why and the text stays as it was.
    fn write_entry(&mut self, ix: usize, name: &str, literal: &str) -> bool {
        let Some(row) = rows().get(ix) else { return false };
        let checked = row.field.check_entry(name, literal);
        if let Some(error) = self.errors.get_mut(ix) {
            *error = checked.as_ref().err().cloned();
        }
        if checked.is_ok() {
            self.edit_text(row, Some(name), Some(literal), |text| {
                edit::write_entry(text, row.table(), row.key(), name, literal)
            });
        }
        checked.is_ok()
    }

    /// Write what was typed in the entries' fields since the last check.
    pub(super) fn check_typed_entries(&mut self, cx: &App) {
        for (ix, name) in std::mem::take(&mut self.entry_typed) {
            let Some(kind) = rows().get(ix).and_then(entry_kind) else { continue };
            let key = (ix, name);
            let Some(field) = self.entries.get(&key).and_then(|p| p.field.as_ref()) else {
                continue;
            };
            let typed = field.read(cx).value().to_string();
            let _written = self.write_entry(ix, &key.1, &entry_literal(kind, &typed));
        }
    }

    /// ↩ in map row `ix`'s field: the name typed is added, and the keyboard goes to it. A name
    /// the map holds already takes the keyboard to its line instead.
    pub(super) fn add_entry(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        let Some(kind) = entry_kind(row) else { return };
        let Some(Some(adding)) = self.fields.get(ix).cloned() else { return };
        let typed = adding.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        let name = match self.entry_key(row, &typed) {
            Ok(name) => name,
            Err(why) => {
                if let Some(error) = self.errors.get_mut(ix) {
                    *error = Some(why);
                }
                cx.notify();
                return;
            }
        };
        let held = self.entries_of(row).iter().any(|(held, _)| *held == name);
        if !held {
            if !self.write_entry(ix, &name, first_value(kind)) {
                cx.notify();
                return;
            }
            self.apply(cx);
            self.sync_entries(window, cx);
        }
        adding.update(cx, |state, cx| state.set_value("", window, cx));
        self.focus_entry(&(ix, name), window, cx);
    }

    /// Give entry `key` the keyboard: its field, else its switch.
    fn focus_entry(&self, key: &(usize, String), window: &mut Window, cx: &mut Context<Self>) {
        let Some(parts) = self.entries.get(key) else { return };
        match &parts.field {
            Some(field) => field.update(cx, |state, cx| state.focus(window, cx)),
            None => window.focus(&parts.handle, cx),
        }
        cx.notify();
    }

    /// Take map row `ix`'s entry `name` out; the keyboard goes back to the row's field.
    fn remove_entry(&mut self, ix: usize, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        self.check_typed(cx);
        self.edit_text(row, Some(name), None, |text| {
            edit::remove_entry(text, row.table(), row.key(), name)
        });
        self.apply(cx);
        self.sync_entries(window, cx);
        if let Some(Some(adding)) = self.fields.get(ix) {
            adding.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    /// Turn map row `ix`'s switch entry `name`.
    fn toggle_entry(&mut self, ix: usize, name: &str, cx: &mut Context<Self>) {
        let Some(row) = rows().get(ix) else { return };
        let on = self
            .entries_of(row)
            .into_iter()
            .any(|(held, value)| held == name && value == Value::Bool(true));
        self.entry_moved = Some(((ix, name.to_owned()), self.turns));
        self.turns = self.turns.wrapping_add(1);
        if self.write_entry(ix, name, if on { "false" } else { "true" }) {
            self.apply(cx);
        }
        cx.notify();
    }

    /// Map row `ix`'s entries, a line each under its words; `None` for a row that is no map or
    /// a map with nothing in it.
    pub(super) fn map_entries(
        &self,
        ix: usize,
        row: &Row,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let kind = entry_kind(row)?;
        let entries = self.entries_of(row);
        if entries.is_empty() {
            return None;
        }
        let lines: Vec<AnyElement> = entries
            .into_iter()
            .filter_map(|(key, value)| {
                let shown = self.entry_name(row, &key);
                self.entry_line(ix, kind, (key, shown), &value, cx)
            })
            .collect();
        Some(
            div()
                .id(("settings-entries", ix))
                .debug_selector(move || format!("settings-entries-{ix}"))
                .role(gpui::accesskit::Role::List)
                .aria_label(row.label())
                .flex()
                .flex_col()
                .pt(px(self.theme.spacing.xs))
                .children(lines)
                .into_any_element(),
        )
    }

    /// One entry's line: its name as it reads (`shown`), the control that sets it, and the way
    /// to take it out. `name` is the key the file writes it under.
    fn entry_line(
        &self,
        ix: usize,
        kind: &Kind,
        (name, shown): (String, String),
        value: &Value,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let key = (ix, name);
        let parts = self.entries.get(&key)?;
        let (_, name) = key.clone();
        let slug = format!("settings-entry-{ix}-{name}");
        let label = SharedString::from(shown.clone());
        let control = if let Some(field) = &parts.field {
            well(theme)
                .flex_1()
                .min_w_0()
                .px(px(crate::kit::FIELD_INSET))
                .child(
                    Input::new(field)
                        .appearance(false)
                        .px_0()
                        .text_size(px(theme.typography.ui_size))
                        .aria_label(label.clone()),
                )
                .into_any_element()
        } else {
            let on = *value == Value::Bool(true);
            let turned = self
                .entry_moved
                .as_ref()
                .and_then(|(moved, turn)| (*moved == key).then_some(*turn));
            let toggled = name.clone();
            let el = self
                .switch_el(format!("{slug}-switch"), format!("{slug}-knob"), on, turned, cx)
                .track_focus(&parts.handle)
                .aria_label(label.clone())
                .on_click(cx.listener(move |this, _ev, _window, cx| {
                    this.toggle_entry(ix, &toggled, cx);
                }));
            crate::a11y::tab_stop(el, s.focus).into_any_element()
        };
        let gone = name;
        let remove =
            crate::kit::icon_button(theme, format!("{slug}-remove"), Symbol::Xmark, REMOVE_ENTRY)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    this.remove_entry(ix, &gone, window, cx);
                }));
        let switched = matches!(kind, Kind::Switch);
        Some(
            crate::kit::row(theme, crate::kit::Row::One)
                .id(SharedString::from(slug.clone()))
                .debug_selector(move || slug)
                .role(gpui::accesskit::Role::ListItem)
                .aria_label(label)
                .pl_0()
                .child(
                    crate::kit::typed(div(), theme.roles().chrome)
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text))
                        .when(switched, gpui::Styled::flex_1)
                        .child(shown),
                )
                .child(control)
                .child(remove)
                .gap(px(spacing.sm))
                .into_any_element(),
        )
    }
}

/// The entries' parts, by map row and name.
pub(super) type Entries = HashMap<(usize, String), EntryParts>;
