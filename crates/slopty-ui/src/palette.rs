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
use slopty_core::SessionId;
use slopty_theme::{Theme, alpha};

use crate::colors::{hsla, hsla_alpha};

/// What a line does when it is chosen.
pub enum PaletteRun {
    /// Dispatch this from the element that had the keyboard.
    Action(Box<dyn Action>),
    /// Reveal and focus this session's terminal on the canvas.
    Session(SessionId),
    /// Reveal this item (a file card, a note) on the canvas.
    Item(slopty_core::ItemId),
    /// Follow another client's viewport on the canvas.
    Follow(slopty_core::ClientId),
    /// Open a file card for the path the field holds (relative to the active shell, `~` the
    /// host's home), landing on `line`.
    OpenFile {
        /// As typed, with any `:line` suffix removed.
        path: String,
        /// The `:line` suffix, 1-based.
        line: Option<u32>,
    },
}

impl Clone for PaletteRun {
    fn clone(&self) -> Self {
        match self {
            Self::Action(action) => Self::Action(action.boxed_clone()),
            Self::Session(session) => Self::Session(*session),
            Self::Item(item) => Self::Item(*item),
            Self::Follow(client) => Self::Follow(*client),
            Self::OpenFile { path, line } => Self::OpenFile { path: path.clone(), line: *line },
        }
    }
}

impl std::fmt::Debug for PaletteRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Action(action) => f.debug_tuple("Action").field(&action.name()).finish(),
            Self::Session(session) => f.debug_tuple("Session").field(session).finish(),
            Self::Item(item) => f.debug_tuple("Item").field(item).finish(),
            Self::Follow(client) => f.debug_tuple("Follow").field(client).finish(),
            Self::OpenFile { path, line } => {
                f.debug_struct("OpenFile").field("path", path).field("line", line).finish()
            }
        }
    }
}

/// One line of the palette: a name, what the right side says (the keys that do the same, or
/// a session's status), and what ↩ does.
pub struct PaletteItem {
    /// What the line says (`New note`, `Go to shell`).
    pub label: String,
    /// The right side, muted: the shortcut (`⌘⇧N`) or a session's status; empty for none.
    pub keys: String,
    /// What runs.
    pub run: PaletteRun,
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
        Self { label: label.to_owned(), keys, run: PaletteRun::Action(action) }
    }

    /// A line that goes to a session on the canvas: `Go to <title>`, its status on the right.
    #[must_use]
    pub fn session(title: &str, status: &str, session: SessionId) -> Self {
        Self {
            label: format!("Go to {title}"),
            keys: status.to_owned(),
            run: PaletteRun::Session(session),
        }
    }

    /// `Go to <title>` for an item on the canvas, `what` ("file") on the right.
    #[must_use]
    pub fn item(title: &str, what: &str, item: slopty_core::ItemId) -> Self {
        Self { label: format!("Go to {title}"), keys: what.to_owned(), run: PaletteRun::Item(item) }
    }

    /// `Follow <name>` for another client on the canvas, what device it is on the right.
    #[must_use]
    pub fn looker(name: &str, device: &str, client: slopty_core::ClientId) -> Self {
        Self {
            label: format!("Follow {name}"),
            keys: device.to_owned(),
            run: PaletteRun::Follow(client),
        }
    }

    /// `Open <relative>` for a file the host found under `root`, `file` on the right.
    #[must_use]
    pub fn found_file(root: &str, relative: &str) -> Self {
        Self {
            label: format!("Open {relative}"),
            keys: "file".to_owned(),
            run: PaletteRun::OpenFile {
                path: format!("{}/{relative}", root.trim_end_matches('/')),
                line: None,
            },
        }
    }

    /// `Open <path>` for a path typed into the field, `line N` or `file` on the right.
    #[must_use]
    pub fn open_file(path: &str, line: Option<u32>) -> Self {
        Self {
            label: format!("Open {path}"),
            keys: line.map_or_else(|| "file".to_owned(), |n| format!("line {n}")),
            run: PaletteRun::OpenFile { path: path.to_owned(), line },
        }
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
        Self { label: self.label.clone(), keys: self.keys.clone(), run: self.run.clone() }
    }
}

impl std::fmt::Debug for PaletteItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaletteItem")
            .field("label", &self.label)
            .field("keys", &self.keys)
            .field("run", &self.run)
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

/// The path a query spells, when it is one.
///
/// A single word starting with `/`, `~/`, `./` or `../`, or holding a `/` with no empty
/// segment — with an optional `:line` suffix (`src/main.rs:12`) split off. A word with no
/// `/` is a command, never a file, so `note` keeps matching `New note`.
#[must_use]
pub fn path_query(query: &str) -> Option<(String, Option<u32>)> {
    let word = query.trim();
    if word.is_empty() || word.contains(char::is_whitespace) || !word.contains('/') {
        return None;
    }
    let looks_like_a_path = word.starts_with('/')
        || word.starts_with("~/")
        || word.starts_with("./")
        || word.starts_with("../")
        || word.split('/').all(|part| !part.is_empty() || word.ends_with('/'));
    if !looks_like_a_path {
        return None;
    }
    let (path, line) = match word.rsplit_once(':') {
        Some((path, digits)) if !path.is_empty() && !digits.is_empty() => {
            match digits.parse::<u32>() {
                Ok(line) => (path, Some(line)),
                Err(_) => (word, None),
            }
        }
        _ => (word, None),
    };
    Some((path.to_owned(), line))
}

/// The query worth asking the host's files for: one word of two characters or more that is
/// not a path already spelled from its root (`/…`, `~…`, `.…`).
#[must_use]
pub fn files_query(query: &str) -> Option<&str> {
    let word = query.trim();
    let rooted = word.starts_with('/') || word.starts_with('~') || word.starts_with('.');
    (word.chars().count() >= 2 && !word.contains(char::is_whitespace) && !rooted).then_some(word)
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
#[derive(Debug)]
pub enum PaletteEvent {
    /// The field changed; the canvas asks the host for the files it names.
    Changed(String),
    /// Run this, once the palette is gone and the focus is back.
    Run(PaletteRun),
    /// Esc, or a click outside.
    Dismiss,
}

/// The palette: a field and the items that match it.
pub struct CommandPalette {
    items: Vec<PaletteItem>,
    /// `Open <path>` when the field spells a path; recomputed on every change.
    path_item: Option<PaletteItem>,
    /// `Open <path>` for the files the host found for the field's text; dropped on a change.
    found: Vec<PaletteItem>,
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
                let text = this.input.read(cx).value().to_string();
                this.path_item =
                    path_query(&text).map(|(path, line)| PaletteItem::open_file(&path, line));
                this.found.clear();
                cx.emit(PaletteEvent::Changed(text));
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.run(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            items,
            path_item: None,
            found: Vec::new(),
            input,
            selected: 0,
            theme,
            _events: events,
        }
    }

    /// The host found `paths` under `root` for `query`: they are `Open <path>` lines after
    /// the commands, while the field still says `query`. Directories are left out (a card
    /// shows a file).
    pub fn set_found(&mut self, root: &str, query: &str, paths: &[String], cx: &mut Context<Self>) {
        if self.input.read(cx).value().trim() != query {
            return;
        }
        self.found = paths
            .iter()
            .filter(|p| !p.ends_with('/'))
            .map(|p| PaletteItem::found_file(root, p))
            .collect();
        cx.notify();
    }

    /// The items matching the field, in order: a path typed into it as `Open <path>` first,
    /// the commands the text matches, then the files the host found for it.
    #[must_use]
    pub fn matches(&self, cx: &App) -> Vec<&PaletteItem> {
        let mut out: Vec<&PaletteItem> = self.path_item.iter().collect();
        out.extend(filter(&self.input.read(cx).value(), &self.items));
        out.extend(&self.found);
        out
    }

    /// The selected match's index, clamped to the matches.
    fn selected(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    fn run(&self, cx: &mut Context<Self>) {
        let matches = self.matches(cx);
        let at = self.selected(matches.len());
        if let Some(item) = matches.get(at) {
            let run = item.run.clone();
            cx.emit(PaletteEvent::Run(run));
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
        let run = item.run.clone();
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
                cx.emit(PaletteEvent::Run(run.clone()));
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
        let s = theme.surfaces;
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
    fn a_path_in_the_field_is_told_from_a_command() {
        let q = |s: &str| path_query(s);
        assert_eq!(q("/w/lib.rs"), Some(("/w/lib.rs".to_owned(), None)));
        assert_eq!(q(" /w/lib.rs:12 "), Some(("/w/lib.rs".to_owned(), Some(12))));
        assert_eq!(q("~/notes.md"), Some(("~/notes.md".to_owned(), None)));
        assert_eq!(q("./a"), Some(("./a".to_owned(), None)));
        assert_eq!(q("src/main.rs:7"), Some(("src/main.rs".to_owned(), Some(7))));
        assert_eq!(q("src/main.rs:x"), Some(("src/main.rs:x".to_owned(), None)), "not a line");
        assert_eq!(q("a//b"), None, "an empty segment is no path");
        assert_eq!(q("note"), None, "no slash: a command");
        assert_eq!(q("go to shell"), None, "words: a command");
        assert_eq!(q(""), None);
        let item = PaletteItem::open_file("/w/lib.rs", Some(3));
        assert_eq!((item.label.as_str(), item.keys.as_str()), ("Open /w/lib.rs", "line 3"));
        assert_eq!(PaletteItem::open_file("/w", None).keys, "file");

        // What is asked of the host's files: a word, not a rooted path, not one letter.
        assert_eq!(files_query("main"), Some("main"));
        assert_eq!(files_query(" src/ma "), Some("src/ma"));
        assert_eq!(files_query("m"), None, "one letter matches everything");
        assert_eq!(files_query("/w/lib.rs"), None, "spelled from the root already");
        assert_eq!(files_query("~/x"), None);
        assert_eq!(files_query("./x"), None);
        assert_eq!(files_query("go to"), None);
        let found = PaletteItem::found_file("/tmp/work/", "src/main.rs");
        assert_eq!(found.label, "Open src/main.rs");
        assert!(
            matches!(&found.run, PaletteRun::OpenFile { path, line: None } if path == "/tmp/work/src/main.rs"),
            "{found:?}"
        );
    }

    #[test]
    fn keys_read_as_glyphs_and_the_filter_takes_every_word() {
        let label = |k: &str| keys_label(&Keystroke::parse(k).unwrap());
        assert_eq!(label("cmd-shift-n"), "⇧⌘N", "Apple's order: ⌃⌥⇧⌘");
        assert_eq!(label("cmd-up"), "⌘↑");
        assert_eq!(label("ctrl-tab"), "⌃⇥");
        assert_eq!(label("cmd-alt-r"), "⌥⌘R");
        assert_eq!(label("cmd-="), "⌘=");
        let item = |label: &str| PaletteItem::new(label, Box::new(MoveUp), &[]);
        let items = vec![item("New note"), item("Zoom in"), item("Zoom to item")];
        let labels =
            |q: &str| filter(q, &items).iter().map(|i| i.label.as_str()).collect::<Vec<_>>();
        assert_eq!(labels(""), ["New note", "Zoom in", "Zoom to item"]);
        assert_eq!(labels("zoom"), ["Zoom in", "Zoom to item"]);
        assert_eq!(labels("item zo"), ["Zoom to item"], "every word, any order, any case");
        assert!(labels("nothing").is_empty());
    }
}
