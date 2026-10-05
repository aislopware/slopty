//! One menu for every context and overflow menu: the bar's, a terminal block's, a split
//! button's.
//!
//! After Ely GPUI Components (`src/menus/menu.rs`, `model.rs`, `src/lists/select.rs`),
//! Copyright (c) 2026 Ely GPUI Component contributors, MIT OR Apache-2.0, on Slopty's tokens.
//! [`Menu`] is the rows; [`MenuPanel`] draws them and runs the keyboard and the pointer over
//! them. The owner says where it hangs and when it is open, and closes it in `on_close`; the
//! panel says when to.
//!
//! - It takes the keyboard when it opens and hands back nothing itself: the owner's close puts the
//!   keyboard where it was.
//! - Opened from the keyboard it marks its first row; from the pointer, none until the pointer
//!   moves over one.
//! - ↑ and ↓ walk the rows round, past a disabled one; Home and End go to the ends.
//! - Letters typed within [`TYPING`] of each other go to the next row they start, Finder's and
//!   Ely's typeahead.
//! - ↩ and Space choose the marked row on their release, armed by their own press, so the key that
//!   opened the menu never chooses in it.
//! - Esc and a press outside close it; choosing a row closes it first, then runs the row, so the
//!   row runs with the keyboard back where it was.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent,
    MouseButton, ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconSize, Symbol};

/// Letters typed within this pause of each other make one prefix.
pub const TYPING: Duration = Duration::from_millis(800);

/// A menu's width at zoom 1: room for a row's words and its keys.
pub const WIDTH: f32 = 260.0;

/// What a row does when chosen.
pub type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a row shows of its state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// It only runs.
    Plain,
    /// A setting it flips, ticked while on.
    Check(bool),
    /// One of a set, ticked while chosen.
    Radio(bool),
}

/// One row of a menu.
#[derive(Clone)]
pub struct MenuItem {
    key: SharedString,
    label: SharedString,
    detail: SharedString,
    icon: Option<Symbol>,
    mark: Mark,
    disabled: bool,
    run: Run,
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuItem")
            .field("key", &self.key)
            .field("label", &self.label)
            .field("mark", &self.mark)
            .finish_non_exhaustive()
    }
}

impl MenuItem {
    /// A row saying `label` that runs `run`; its selector is the panel's then `key`.
    #[must_use]
    pub fn new(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        run: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self::with_run(key, label, Rc::new(run))
    }

    /// [`Self::new`] with a run already shared.
    #[must_use]
    pub fn with_run(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        run: Run,
    ) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            detail: SharedString::default(),
            icon: None,
            mark: Mark::Plain,
            disabled: false,
            run,
        }
    }

    /// Muted words at the row's end: its keys, or a state.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = detail.into();
        self
    }

    /// An icon before its words.
    #[must_use]
    pub const fn icon(mut self, icon: Symbol) -> Self {
        self.icon = Some(icon);
        self
    }

    /// What it shows of its state.
    #[must_use]
    pub const fn mark(mut self, mark: Mark) -> Self {
        self.mark = mark;
        self
    }

    /// Shown and passed over: nothing to do now.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// What the row says.
    #[must_use]
    pub const fn label(&self) -> &SharedString {
        &self.label
    }
}

/// A menu's entries in order.
#[derive(Clone, Debug)]
enum Entry {
    Item(MenuItem),
    Separator,
}

/// Rows, and hairlines between groups of them.
#[derive(Clone, Debug, Default)]
pub struct Menu {
    entries: Vec<Entry>,
}

impl Menu {
    /// A menu with no rows yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A row at the end.
    #[must_use]
    pub fn item(mut self, item: MenuItem) -> Self {
        self.push(item);
        self
    }

    /// A row at the end, in place.
    pub fn push(&mut self, item: MenuItem) {
        self.entries.push(Entry::Item(item));
    }

    /// A hairline before the next row, once there is a row before it.
    pub fn separate(&mut self) {
        if matches!(self.entries.last(), Some(Entry::Item(_))) {
            self.entries.push(Entry::Separator);
        }
    }

    /// The rows, in order.
    fn items(&self) -> impl Iterator<Item = &MenuItem> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Item(item) => Some(item),
            Entry::Separator => None,
        })
    }

    /// How many rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items().count()
    }

    /// Whether it has no row.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items().next().is_none()
    }

    /// The row a step of `by` from `from` lands on, round the ends, past the disabled ones;
    /// from nothing, the first (or the last, going up).
    #[must_use]
    pub fn step(&self, from: Option<usize>, by: isize) -> Option<usize> {
        let open: Vec<bool> = self.items().map(|i| !i.disabled).collect();
        let count = open.len();
        let mut at = match from {
            Some(at) => at,
            None if by > 0 => count.checked_sub(1)?,
            None => 0,
        };
        for _ in 0..count {
            at = if by > 0 {
                at.saturating_add(1).checked_rem(count).unwrap_or(0)
            } else {
                at.checked_sub(1).unwrap_or_else(|| count.saturating_sub(1))
            };
            if open.get(at).copied().unwrap_or(false) {
                return Some(at);
            }
        }
        None
    }

    /// The first open row (`first`) or the last.
    fn end(&self, first: bool) -> Option<usize> {
        let open: Vec<bool> = self.items().map(|i| !i.disabled).collect();
        if first { open.iter().position(|o| *o) } else { open.iter().rposition(|o| *o) }
    }

    /// The row `typed` goes to: one letter moves past `at` to the next row starting with it,
    /// round to the top; more letters stay on `at` while it still starts with them. Disabled
    /// rows are passed over.
    #[must_use]
    pub fn typed_to(&self, at: Option<usize>, typed: &str) -> Option<usize> {
        let rows: Vec<&MenuItem> = self.items().collect();
        let count = rows.len();
        let start = at.unwrap_or_else(|| count.saturating_sub(1));
        let from = if typed.chars().count() == 1 { start.saturating_add(1) } else { start };
        (0..count).map(|step| from.saturating_add(step).checked_rem(count).unwrap_or(0)).find(
            |ix| {
                rows.get(*ix)
                    .is_some_and(|row| !row.disabled && row.label.to_lowercase().starts_with(typed))
            },
        )
    }

    fn row(&self, at: usize) -> Option<&MenuItem> {
        self.items().nth(at)
    }
}

/// The open panel's own state: the keyboard's holder, the marked row and the typing.
struct Cursor {
    focus: FocusHandle,
    took: bool,
    at: Option<usize>,
    armed: bool,
    typed: String,
    typed_at: Option<Instant>,
}

/// A menu's panel under `id`: its rows go by `{id}-{key}` and its hairlines by
/// `{id}-separator-{n}`, `n` the row after it.
#[derive(IntoElement)]
pub struct MenuPanel {
    id: SharedString,
    label: SharedString,
    menu: Rc<Menu>,
    theme: Theme,
    keyed: bool,
    inert: bool,
    head: Vec<gpui::AnyElement>,
    on_close: Run,
    on_dismiss: Option<Run>,
}

impl std::fmt::Debug for MenuPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuPanel").field("id", &self.id).finish_non_exhaustive()
    }
}

impl MenuPanel {
    /// The panel of `menu` under `id`, named `label`; `on_close` closes it.
    #[must_use]
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        menu: Rc<Menu>,
        theme: &Theme,
        on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            menu,
            theme: theme.clone(),
            keyed: false,
            inert: false,
            head: Vec::new(),
            on_close: Rc::new(on_close),
            on_dismiss: None,
        }
    }

    /// Opened from the keyboard: its first row starts marked.
    #[must_use]
    pub const fn keyed(mut self, keyed: bool) -> Self {
        self.keyed = keyed;
        self
    }

    /// Closed with nothing chosen (Esc, a press outside), when that differs from closing it
    /// to run a row.
    #[must_use]
    pub fn on_dismiss(mut self, on_dismiss: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(on_dismiss));
        self
    }

    /// What it shows above its rows, which is not chosen: a machine's facts.
    #[must_use]
    pub fn head(mut self, head: Vec<gpui::AnyElement>) -> Self {
        self.head = head;
        self
    }

    /// Drawn on its way out: it takes no keyboard and no press.
    #[must_use]
    pub const fn inert(mut self, inert: bool) -> Self {
        self.inert = inert;
        self
    }
}

/// Choose row `at`: close the menu, then run it.
fn choose(menu: &Menu, at: usize, close: &Run, window: &mut Window, cx: &mut App) {
    let Some(row) = menu.row(at).filter(|row| !row.disabled) else { return };
    tracing::debug!(row = %row.label, "menu");
    let run = Rc::clone(&row.run);
    close(window, cx);
    run(window, cx);
}

impl RenderOnce for MenuPanel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, label, menu, theme, keyed, inert, head, on_close, on_dismiss } = self;
        let dismiss = on_dismiss.unwrap_or_else(|| Rc::clone(&on_close));
        let s = theme.surfaces;
        let cursor =
            window.use_keyed_state(ElementId::Name(format!("{id}-{label}-cursor").into()), cx, {
                let menu = Rc::clone(&menu);
                move |_window, cx| Cursor {
                    focus: cx.focus_handle(),
                    took: false,
                    at: if keyed { menu.end(true) } else { None },
                    armed: false,
                    typed: String::new(),
                    typed_at: None,
                }
            });
        let focus = cursor.read(cx).focus.clone();
        if !inert && !cursor.read(cx).took {
            cursor.update(cx, |c, _| c.took = true);
            window.focus(&focus, cx);
        }
        let marked = cursor.read(cx).at;
        let mut rows = head;
        rows.reserve(menu.entries.len());
        let mut n = 0_usize;
        for entry in &menu.entries {
            match entry {
                Entry::Separator => {
                    let selector = format!("{id}-separator-{n}");
                    rows.push(
                        div()
                            .debug_selector(move || selector)
                            .flex_none()
                            .child(super::list_rule(&theme))
                            .into_any_element(),
                    );
                }
                Entry::Item(item) => {
                    rows.push(row(
                        &id,
                        n,
                        item,
                        marked == Some(n),
                        &menu,
                        &cursor,
                        &on_close,
                        &theme,
                    ));
                    n = n.saturating_add(1);
                }
            }
        }
        let (keys_menu, keys_cursor, keys_close) =
            (Rc::clone(&menu), cursor.clone(), Rc::clone(&dismiss));
        let (up_menu, up_cursor, up_close) =
            (Rc::clone(&menu), cursor.clone(), Rc::clone(&on_close));
        let out_close = dismiss;
        let selector = id.to_string();
        super::elevate(div(), &theme)
            .id(ElementId::Name(id))
            .debug_selector(move || selector)
            .role(Role::Menu)
            .aria_label(label)
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .p(px(super::sheet_pad(&theme)))
            .rounded(px(theme.radii.lg))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .when(!inert, |el| {
                el.track_focus(&focus)
                    .on_key_down(move |ev: &KeyDownEvent, window, cx| {
                        if key_down(&keys_menu, &keys_cursor, ev, &keys_close, window, cx) {
                            cx.stop_propagation();
                        }
                    })
                    .on_key_up(move |ev: &KeyUpEvent, window, cx| {
                        let key = ev.keystroke.key.as_str();
                        if !matches!(key, "enter" | "space") || ev.keystroke.modifiers.modified() {
                            return;
                        }
                        let (armed, at) =
                            up_cursor.update(cx, |c, _| (std::mem::take(&mut c.armed), c.at));
                        if armed {
                            cx.stop_propagation();
                            if let Some(at) = at {
                                choose(&up_menu, at, &up_close, window, cx);
                            }
                        }
                    })
                    .on_mouse_down_out(move |_ev, window, cx| out_close(window, cx))
            })
            .children(rows)
    }
}

/// A key on the open panel: whether it was the menu's.
fn key_down(
    menu: &Menu,
    cursor: &gpui::Entity<Cursor>,
    ev: &KeyDownEvent,
    dismiss: &Run,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let stroke = &ev.keystroke;
    let held = &stroke.modifiers;
    let at = cursor.read(cx).at;
    let to = match stroke.key.as_str() {
        "escape" => {
            dismiss(window, cx);
            return true;
        }
        "enter" | "space" if !held.modified() => {
            cursor.update(cx, |c, _| c.armed = true);
            return true;
        }
        "down" => menu.step(at, 1),
        "up" => menu.step(at, -1),
        "home" => menu.end(true),
        "end" => menu.end(false),
        _ => {
            let letter = stroke
                .key_char
                .as_deref()
                .filter(|typed| !typed.trim().is_empty())
                .filter(|_| !held.platform && !held.control && !held.alt && !held.function);
            let Some(letter) = letter else { return false };
            let now = cx.background_executor().now();
            let typed = cursor.update(cx, |c, _| {
                if c.typed_at.is_none_or(|then| now.saturating_duration_since(then) > TYPING) {
                    c.typed.clear();
                }
                c.typed.push_str(&letter.to_lowercase());
                c.typed_at = Some(now);
                c.typed.clone()
            });
            menu.typed_to(at, &typed)
        }
    };
    if let Some(to) = to {
        cursor.update(cx, |c, cx| {
            c.at = Some(to);
            cx.notify();
        });
    }
    true
}

/// Row `n`: its icon, its words, its detail and its tick; marked, washed.
#[expect(clippy::too_many_arguments, reason = "a row is drawn from the panel's parts")]
fn row(
    id: &SharedString,
    n: usize,
    item: &MenuItem,
    marked: bool,
    menu: &Rc<Menu>,
    cursor: &gpui::Entity<Cursor>,
    close: &Run,
    theme: &Theme,
) -> gpui::AnyElement {
    let s = theme.surfaces;
    let selector = format!("{id}-{}", item.key);
    let (role, ticked) = match item.mark {
        Mark::Plain => (Role::MenuItem, None),
        Mark::Check(on) => (Role::MenuItemCheckBox, Some(on)),
        Mark::Radio(on) => (Role::MenuItemRadio, Some(on)),
    };
    let ink = if item.disabled { s.text_muted } else { s.text };
    let (hover_cursor, click_menu, click_close) =
        (cursor.clone(), Rc::clone(menu), Rc::clone(close));
    let disabled = item.disabled;
    super::sheet_row(theme, super::Row::One)
        .id(ElementId::Name(selector.clone().into()))
        .debug_selector(move || selector)
        .role(role)
        .aria_label(item.label.clone())
        .when_some(ticked, |el, on| {
            el.aria_toggled(if on { Toggled::True } else { Toggled::False })
        })
        .gap(px(theme.spacing.md))
        .when(!disabled, gpui::Styled::cursor_pointer)
        .when(marked, |el| el.bg(hsla(s.hover)).aria_active_descendant())
        .on_mouse_move(move |_ev, _window, cx| {
            if !disabled && hover_cursor.read(cx).at != Some(n) {
                hover_cursor.update(cx, |c, cx| {
                    c.at = Some(n);
                    cx.notify();
                });
            }
        })
        .on_click(move |_ev, window, cx| choose(&click_menu, n, &click_close, window, cx))
        .children(item.icon.map(|icon| {
            crate::icons::icon(theme, icon, IconSize::Inline, hsla(s.text_muted)).flex_none()
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(theme.typography.ui_size))
                .text_color(hsla(ink))
                .child(item.label.clone()),
        )
        .when(!item.detail.is_empty(), |el| {
            el.child(
                div()
                    .flex_none()
                    .text_size(px(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(item.detail.clone()),
            )
        })
        .when(ticked == Some(true), |el| {
            el.child(
                crate::icons::icon(
                    theme,
                    Symbol::Checkmark,
                    IconSize::Inline,
                    hsla(s.text_secondary),
                )
                .flex_none(),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(rows: &[(&str, bool)]) -> Menu {
        let mut menu = Menu::new();
        for (label, disabled) in rows {
            menu.push(MenuItem::new(*label, *label, |_, _| {}).disabled(*disabled));
        }
        menu
    }

    /// The arrows go round the ends and pass over a disabled row; from nothing, ↓ starts at
    /// the top and ↑ at the bottom.
    #[test]
    fn the_arrows_go_round_past_a_disabled_row() {
        let m = menu(&[("Copy", false), ("Cut", true), ("Paste", false)]);
        assert_eq!(m.step(None, 1), Some(0));
        assert_eq!(m.step(None, -1), Some(2));
        assert_eq!(m.step(Some(0), 1), Some(2), "past the disabled row");
        assert_eq!(m.step(Some(2), 1), Some(0), "round the end");
        assert_eq!(m.step(Some(0), -1), Some(2), "round the top");
        assert_eq!(menu(&[("Cut", true)]).step(None, 1), None);
        assert_eq!(Menu::new().step(None, 1), None);
    }

    /// A letter goes to the next row it starts, round; more letters stay while they fit.
    #[test]
    fn letters_go_to_the_row_they_start() {
        let m = menu(&[("Copy", false), ("Clear", false), ("Cut", true), ("Paste", false)]);
        assert_eq!(m.typed_to(None, "c"), Some(0));
        assert_eq!(m.typed_to(Some(0), "c"), Some(1), "the next one");
        assert_eq!(m.typed_to(Some(1), "c"), Some(0), "round, past the disabled one");
        assert_eq!(m.typed_to(Some(1), "cl"), Some(1), "more letters stay");
        assert_eq!(m.typed_to(Some(0), "p"), Some(3));
        assert_eq!(m.typed_to(Some(0), "x"), None);
    }

    use std::cell::RefCell;

    use gpui::{Context, Entity, Keystroke, Modifiers, Render, TestAppContext, VisualTestContext};

    /// What happened, in order: "closed", "dismissed", or the row run.
    type Log = Rc<RefCell<Vec<String>>>;

    /// A view holding a menu of Copy, Cut (disabled) and Paste, open while `open`.
    struct Host {
        open: bool,
        keyed: bool,
        log: Log,
        focus: FocusHandle,
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::default();
            let mut menu = Menu::new();
            for (label, disabled) in [("Copy", false), ("Cut", true), ("Paste", false)] {
                let log = Rc::clone(&self.log);
                menu.push(
                    MenuItem::new(label.to_lowercase(), label, move |_, _| {
                        log.borrow_mut().push(label.to_owned());
                    })
                    .disabled(disabled),
                );
            }
            let shut = |this: Entity<Self>, word: &'static str| {
                move |_: &mut Window, cx: &mut App| {
                    this.update(cx, |host, cx| {
                        host.open = false;
                        host.log.borrow_mut().push(word.to_owned());
                        cx.notify();
                    });
                }
            };
            let this = cx.entity();
            div().track_focus(&self.focus).size_full().when(self.open, |el| {
                el.child(
                    MenuPanel::new(
                        "menu",
                        "Edit",
                        Rc::new(menu),
                        &theme,
                        shut(this.clone(), "closed"),
                    )
                    .on_dismiss(shut(this, "dismissed"))
                    .keyed(self.keyed),
                )
            })
        }
    }

    fn host(cx: &mut TestAppContext, keyed: bool) -> (Entity<Host>, Log, &mut VisualTestContext) {
        let log = Log::default();
        let (view, cx) = cx.add_window_view({
            let log = Rc::clone(&log);
            move |_window, cx| Host { open: true, keyed, log, focus: cx.focus_handle() }
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        (view, log, cx)
    }

    /// The row marked now, by its words: the one a screen reader is on.
    fn marked(cx: &mut VisualTestContext) -> Option<String> {
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Menu", Some("Edit"))), "the menu is open: {tree:#?}");
        let on = tree.into_iter().find(|n| n.focused)?;
        (on.role == "MenuItem").then(|| on.label.unwrap_or_default())
    }

    fn said(log: &Log) -> Vec<String> {
        log.borrow().clone()
    }

    /// A key let go.
    fn release(cx: &mut VisualTestContext, key: &str) {
        let keystroke = Keystroke::parse(key).expect("a keystroke");
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
    }

    /// Opened from the keyboard, the panel holds the keyboard on its first row; the arrows walk
    /// round past the disabled row, and a letter goes to the row it starts.
    #[gpui::test]
    fn the_keyboard_walks_the_rows(cx: &mut TestAppContext) {
        let (_view, _log, cx) = host(cx, true);
        assert_eq!(marked(cx).as_deref(), Some("Copy"), "the first row, from the keyboard");
        cx.simulate_keystrokes("down");
        assert_eq!(marked(cx).as_deref(), Some("Paste"), "past Cut, disabled");
        cx.simulate_keystrokes("down");
        assert_eq!(marked(cx).as_deref(), Some("Copy"), "round the end");
        cx.simulate_keystrokes("up");
        assert_eq!(marked(cx).as_deref(), Some("Paste"), "round the top");
        cx.simulate_keystrokes("home");
        assert_eq!(marked(cx).as_deref(), Some("Copy"));
        cx.simulate_keystrokes("p");
        assert_eq!(marked(cx).as_deref(), Some("Paste"), "the row p starts");
    }

    /// Opened by the pointer, nothing is marked until a key or the pointer marks a row.
    #[gpui::test]
    fn the_pointer_opens_it_unmarked(cx: &mut TestAppContext) {
        let (_view, _log, cx) = host(cx, false);
        assert_eq!(marked(cx), None);
        cx.simulate_keystrokes("down");
        assert_eq!(marked(cx).as_deref(), Some("Copy"));
    }

    /// ↩ chooses on its release: the menu closes, then the row runs. A release with no press
    /// first, the tail of the key that opened the menu, chooses nothing.
    #[gpui::test]
    fn enter_chooses_on_its_release_after_closing(cx: &mut TestAppContext) {
        let (view, log, cx) = host(cx, true);
        release(cx, "enter");
        assert!(view.read_with(cx, |h, _| h.open), "a release alone chooses nothing");
        cx.simulate_keystrokes("down enter");
        assert!(view.read_with(cx, |h, _| h.open), "the press only arms");
        assert_eq!(said(&log), Vec::<String>::new());
        release(cx, "enter");
        assert!(!view.read_with(cx, |h, _| h.open));
        assert_eq!(said(&log), ["closed", "Paste"], "closed, then run");
    }

    /// Esc closes it with nothing chosen, through the dismissal.
    #[gpui::test]
    fn escape_dismisses_it(cx: &mut TestAppContext) {
        let (view, log, cx) = host(cx, true);
        cx.simulate_keystrokes("escape");
        assert!(!view.read_with(cx, |h, _| h.open));
        assert_eq!(said(&log), ["dismissed"]);
    }

    /// A click on a row closes the menu and runs the row; one on a disabled row does nothing,
    /// and a press outside dismisses it.
    #[gpui::test]
    fn a_click_runs_a_row_and_one_outside_dismisses(cx: &mut TestAppContext) {
        let (view, log, cx) = host(cx, false);
        let cut = cx.debug_bounds("menu-cut").expect("the Cut row").center();
        cx.simulate_click(cut, Modifiers::none());
        assert!(view.read_with(cx, |h, _| h.open), "Cut is disabled");
        let copy = cx.debug_bounds("menu-copy").expect("the Copy row").center();
        cx.simulate_click(copy, Modifiers::none());
        assert_eq!(said(&log), ["closed", "Copy"]);
        view.update(cx, |h, cx| {
            h.open = true;
            cx.notify();
        });
        cx.run_until_parked();
        let panel = cx.debug_bounds("menu").expect("the panel");
        let outside = panel.bottom_right() + gpui::point(px(40.0), px(40.0));
        cx.simulate_click(outside, Modifiers::none());
        assert!(!view.read_with(cx, |h, _| h.open));
        assert_eq!(said(&log), ["closed", "Copy", "dismissed"]);
    }
}
