//! `CommandPalette`: every action by name with its shortcut, filtered as you type; ↩ runs the
//! selected one, a click runs any, Esc dismisses.
//!
//! Shown by the canvas on ⌘⇧P over whatever has the keyboard; the action runs once the
//! palette is gone and the focus is back where it was, so a terminal's own actions (find,
//! the prompts) reach the terminal that was focused.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};

/// One line of the palette: a name, the keys that do the same, the action ↩ dispatches.
pub struct PaletteItem {
    /// What the line says (`New note`).
    pub label: String,
    /// The shortcut as the line shows it (`⌘⇧N`), empty when there is none.
    pub keys: String,
    /// What runs.
    pub action: Box<dyn Action>,
}

impl PaletteItem {
    /// An item for `action`, its keys read from `bindings` (the first binding for it).
    #[must_use]
    pub fn new(label: &str, action: Box<dyn Action>, bindings: &[KeyBinding]) -> Self {
        let keys = bindings
            .iter()
            .find(|b| b.action().partial_eq(action.as_ref()))
            .map(|b| b.keystrokes().iter().map(|k| keys_label(k.inner())).collect::<String>())
            .unwrap_or_default();
        Self { label: label.to_owned(), keys, action }
    }

    /// The line as a screen reader reads it: the label, then the keys.
    #[must_use]
    pub fn a11y_label(&self) -> String {
        if self.keys.is_empty() {
            self.label.clone()
        } else {
            format!("{} {}", self.label, self.keys)
        }
    }
}

impl Clone for PaletteItem {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            keys: self.keys.clone(),
            action: self.action.boxed_clone(),
        }
    }
}

impl std::fmt::Debug for PaletteItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteItem")
            .field("label", &self.label)
            .field("keys", &self.keys)
            .field("action", &self.action.name())
            .finish()
    }
}

/// A keystroke as the palette shows it, the same on every platform: the modifiers in the
/// menu-bar order `⌃⌥⇧⌘`, then the key,
/// a letter upper-cased, the named keys as their glyphs.
#[must_use]
pub fn keys_label(keystroke: &gpui::Keystroke) -> String {
    let m = keystroke.modifiers;
    let mut out = String::new();
    if m.control {
        out.push('⌃');
    }
    if m.alt {
        out.push('⌥');
    }
    if m.shift {
        out.push('⇧');
    }
    if m.platform {
        out.push('⌘');
    }
    match keystroke.key.as_str() {
        "up" => out.push('↑'),
        "down" => out.push('↓'),
        "left" => out.push('←'),
        "right" => out.push('→'),
        "tab" => out.push('⇥'),
        "enter" => out.push('↩'),
        "escape" => out.push('⎋'),
        "backspace" => out.push('⌫'),
        "space" => out.push('␣'),
        key => out.extend(key.chars().flat_map(char::to_uppercase)),
    }
    out
}

/// The items `query` keeps, in their order: every word of the query is found in the label,
/// case-insensitive; an empty query keeps all.
#[must_use]
pub fn filter<'a>(query: &str, items: &'a [PaletteItem]) -> Vec<&'a PaletteItem> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    items
        .iter()
        .filter(|item| {
            let label = item.label.to_lowercase();
            words.iter().all(|w| label.contains(w.as_str()))
        })
        .collect()
}

/// What the palette decided.
pub enum PaletteEvent {
    /// Run this, once the palette is gone and the focus is back.
    Run(Box<dyn Action>),
    /// Esc, or a click outside.
    Dismiss,
}

impl std::fmt::Debug for PaletteEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(action) => f.debug_tuple("Run").field(&action.name()).finish(),
            Self::Dismiss => f.write_str("Dismiss"),
        }
    }
}

/// The palette: a field and the items that match it.
pub struct CommandPalette {
    items: Vec<PaletteItem>,
    input: Entity<InputState>,
    /// Which match ↑/↓ have selected.
    selected: usize,
    theme: Theme,
    _events: Subscription,
}

impl std::fmt::Debug for CommandPalette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandPalette")
            .field("items", &self.items.len())
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl CommandPalette {
    /// Over `items`, the field empty and focused by whoever shows it.
    pub fn new(
        items: Vec<PaletteItem>,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command"));
        let events = cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::Change => {
                this.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.run(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self { items, input, selected: 0, theme, _events: events }
    }

    /// The items matching the field, in order.
    #[must_use]
    pub fn matches(&self, cx: &App) -> Vec<&PaletteItem> {
        filter(&self.input.read(cx).value(), &self.items)
    }

    /// The selected match's index, clamped to the matches.
    fn selected(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    fn run(&self, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let at = self.selected(matches.len());
        if let Some(item) = matches.get(at) {
            let action = item.action.boxed_clone();
            cx.emit(PaletteEvent::Run(action));
        }
    }

    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let count = i64::try_from(self.matches(cx).len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(self.selected(usize::try_from(count).unwrap_or(0))).unwrap_or(0);
        self.selected = usize::try_from(at.saturating_add(delta).rem_euclid(count)).unwrap_or(0);
        cx.notify();
    }

    fn row(
        &self,
        ix: usize,
        item: &PaletteItem,
        chosen: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let action = item.action.boxed_clone();
        let (raised, overlay) = (s.raised, s.overlay);
        div()
            .id(ElementId::NamedInteger("palette-item".into(), u64::try_from(ix).unwrap_or(0)))
            .debug_selector(move || format!("palette-item-{ix}"))
            .role(gpui::accesskit::Role::ListBoxOption)
            .aria_label(SharedString::from(item.a11y_label()))
            .w_full()
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.xs))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .when(chosen, |el| el.bg(hsla_alpha(s.accent, alpha::TINT_STRONG)))
            .hover(move |st| st.bg(hsla(raised)))
            .active(move |st| st.bg(hsla(overlay)))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_this, _ev, _window, cx| {
                cx.emit(PaletteEvent::Run(action.boxed_clone()));
            }))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(item.label.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(item.keys.clone())),
            )
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let s = theme.surfaces.clone();
        let matches: Vec<PaletteItem> = self.matches(cx).into_iter().cloned().collect();
        let chosen = self.selected(matches.len());
        let rows: Vec<gpui::AnyElement> = matches
            .iter()
            .enumerate()
            .map(|(ix, item)| self.row(ix, item, ix == chosen, cx).into_any_element())
            .collect();
        let empty = rows.is_empty();
        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(theme.spacing.xl * 2.0))
            .bg(hsla_alpha(s.canvas, alpha::SCRIM))
            .capture_action(cx.listener(|this, _: &MoveUp, _window, cx| this.step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _window, cx| this.step(1, cx)))
            .capture_action(cx.listener(|_this, _: &Escape, _window, cx| {
                cx.emit(PaletteEvent::Dismiss);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _window, cx| {
                    cx.emit(PaletteEvent::Dismiss);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("palette")
                    .debug_selector(|| "palette".to_owned())
                    .role(gpui::accesskit::Role::Dialog)
                    .aria_label("Commands")
                    .w(px(520.0))
                    .max_h(px(440.0))
                    .flex()
                    .flex_col()
                    .rounded(px(theme.radii.md))
                    .border_1()
                    .border_color(hsla(s.border))
                    .bg(hsla(s.panel))
                    .shadow_md()
                    .text_size(px(theme.typography.ui_size))
                    .font_family(theme.typography.ui_family.clone())
                    .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
                    .child(
                        div()
                            .px(px(theme.spacing.md))
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(s.border))
                            .child(Input::new(&self.input).aria_label("Command")),
                    )
                    .child(
                        div()
                            .id("palette-list")
                            .role(gpui::accesskit::Role::ListBox)
                            .aria_label("Commands")
                            .flex_1()
                            .overflow_y_scroll()
                            .p(px(theme.spacing.xs))
                            .children(rows)
                            .when(empty, |el| {
                                el.child(
                                    div()
                                        .p(px(theme.spacing.md))
                                        .text_color(hsla(s.text_muted))
                                        .child("no command matches"),
                                )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::Keystroke;

    use super::*;

    #[test]
    fn keys_read_as_glyphs_and_the_filter_takes_every_word() {
        let label = |k: &str| keys_label(&Keystroke::parse(k).unwrap());
        assert_eq!(label("cmd-shift-n"), "⇧⌘N", "Apple's order: ⌃⌥⇧⌘");
        assert_eq!(label("cmd-up"), "⌘↑");
        assert_eq!(label("ctrl-tab"), "⌃⇥");
        assert_eq!(label("cmd-alt-r"), "⌥⌘R");
        assert_eq!(label("cmd-="), "⌘=");
        let items = vec![
            PaletteItem {
                label: "New note".to_owned(),
                keys: String::new(),
                action: Box::new(MoveUp),
            },
            PaletteItem {
                label: "Zoom in".to_owned(),
                keys: String::new(),
                action: Box::new(MoveUp),
            },
            PaletteItem {
                label: "Zoom to item".to_owned(),
                keys: String::new(),
                action: Box::new(MoveUp),
            },
        ];
        let labels =
            |q: &str| filter(q, &items).iter().map(|i| i.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(""), ["New note", "Zoom in", "Zoom to item"]);
        assert_eq!(labels("zoom"), ["Zoom in", "Zoom to item"]);
        assert_eq!(labels("item zo"), ["Zoom to item"], "every word, any order, any case");
        assert!(labels("nothing").is_empty());
    }
}
