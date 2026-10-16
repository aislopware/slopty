//! The one menu Slopty's own views open (a terminal's right click, a remote window's "…"), drawn
//! as the shell's menus are: [`popover::popover_card`] holding [`popover::menu_row_nav`] rows,
//! parted by [`popover::menu_separator`], each with its shortcut as [`popover::kbd_hint`]. The
//! owner mounts the panel with [`popover::menu_at`] or [`popover::anchored_menu_below_end`], which
//! frost it and play its motion, and closes it in `on_close`; the panel says when to.
//!
//! The keyboard model is Finder's, after Ely GPUI Components (`src/menus/menu.rs`), MIT OR
//! Apache-2.0:
//! - It takes the keyboard when it opens and hands back nothing itself: the owner's close puts the
//!   keyboard where it was.
//! - Opened from the keyboard it marks its first row; from the pointer, none until the pointer
//!   moves over one.
//! - ↑ and ↓ walk the rows round, past a disabled one; Home and End go to the ends.
//! - Letters typed within [`TYPING`] of each other go to the next row they start.
//! - ↩ and Space choose the marked row on their release, armed by their own press, so the key that
//!   opened the menu never chooses in it.
//! - Esc and a press outside close it; choosing a row closes it first, then runs the row, so the
//!   row runs with the keyboard back where it was.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent,
    MouseButton, ParentElement as _, RenderOnce, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Toggled, Window, div, px,
};

use crate::popover;
use crate::theme::Theme;

/// Letters typed within this pause of each other make one prefix.
pub const TYPING: Duration = Duration::from_millis(800);

/// The least a menu is wide: the shell's context menus run 170 to 232.
pub const MIN_WIDTH: f32 = 200.0;

/// The most a menu is wide: a longer row ends in an ellipsis.
pub const MAX_WIDTH: f32 = 320.0;

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
    shortcut: Option<SharedString>,
    icon: Option<&'static str>,
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
            shortcut: None,
            icon: None,
            mark: Mark::Plain,
            disabled: false,
            run,
        }
    }

    /// A Solar icon ([`crate::icons`]) before its words.
    #[must_use]
    pub const fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The keys that do the same, shown at its end ("⌘C").
    #[must_use]
    pub fn shortcut(mut self, keys: impl Into<SharedString>) -> Self {
        self.shortcut = Some(keys.into());
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

    /// The keys shown at its end.
    #[must_use]
    pub const fn shortcut_keys(&self) -> Option<&SharedString> {
        self.shortcut.as_ref()
    }
}

/// A part of a menu: a row, or the hairline between two groups of them.
#[derive(Clone, Debug)]
enum Part {
    Row(MenuItem),
    Separator,
}

/// A menu's rows, in order, in groups.
#[derive(Clone, Debug, Default)]
pub struct Menu {
    parts: Vec<Part>,
}

impl Menu {
    /// A menu with no rows yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A row at the end, in place.
    pub fn push(&mut self, item: MenuItem) {
        self.parts.push(Part::Row(item));
    }

    /// A hairline before the next row: the end of a group. One at either end, or two in a row,
    /// draws nothing.
    pub fn separator(&mut self) {
        self.parts.push(Part::Separator);
    }

    /// The rows, in order.
    fn items(&self) -> impl Iterator<Item = &MenuItem> {
        self.parts.iter().filter_map(|part| match part {
            Part::Row(item) => Some(item),
            Part::Separator => None,
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

/// A menu's panel under `id`: its rows go by `{id}-{key}`. Mount it with [`popover::menu_at`]
/// (a context menu) or [`popover::anchored_menu_below_end`] (under a button).
#[derive(IntoElement)]
pub struct MenuPanel {
    id: SharedString,
    label: SharedString,
    menu: Rc<Menu>,
    keyed: bool,
    closing: bool,
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
        on_close: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            menu,
            keyed: false,
            closing: false,
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

    /// Playing its closing motion: it no longer takes the keyboard. The closing layer is
    /// another element (`popover::menu_at` swaps the entrance for the exit), so without this
    /// the panel would take the keyboard again as it goes, from what its close gave it back to.
    #[must_use]
    pub const fn closing(mut self, closing: bool) -> Self {
        self.closing = closing;
        self
    }

    /// Closed with nothing chosen (Esc, a press outside), when that differs from closing it
    /// to run a row.
    #[must_use]
    pub fn on_dismiss(mut self, on_dismiss: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(on_dismiss));
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
        let Self { id, label, menu, keyed, closing, on_close, on_dismiss } = self;
        let theme = super::chrome::theme(cx).for_popup();
        let dismiss = on_dismiss.unwrap_or_else(|| Rc::clone(&on_close));
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
        // The keyboard moves once this frame is drawn: a panel drawn late in a frame (a view's
        // deferred menu, built as the frame is prepainted) taking it here would leave two
        // elements in one frame claiming it, the one that had it and the panel.
        if !closing && !cursor.read(cx).took {
            cursor.update(cx, |c, _| c.took = true);
            let took = focus.clone();
            window.defer(cx, move |window, cx| window.focus(&took, cx));
        }
        let marked = cursor.read(cx).at;
        // A hairline only between two groups: none leading, trailing or doubled.
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut n = 0;
        let mut parted = false;
        for part in &menu.parts {
            match part {
                Part::Separator => parted = n > 0,
                Part::Row(item) => {
                    if std::mem::take(&mut parted) {
                        rows.push(popover::menu_separator().into_any_element());
                    }
                    let marked = marked == Some(n);
                    rows.push(row(&id, n, item, marked, &menu, &cursor, &on_close, &theme));
                    n += 1;
                }
            }
        }
        let (keys_menu, keys_cursor, keys_close) =
            (Rc::clone(&menu), cursor.clone(), Rc::clone(&dismiss));
        let (up_menu, up_cursor, up_close) =
            (Rc::clone(&menu), cursor.clone(), Rc::clone(&on_close));
        let out_close = dismiss;
        let selector = id.to_string();
        popover::popover_card(&theme)
            .id(ElementId::Name(id))
            .debug_selector(move || selector)
            .role(Role::Menu)
            .aria_label(label)
            .min_w(px(MIN_WIDTH))
            .max_w(px(MAX_WIDTH))
            .flex()
            .flex_col()
            .font_family(theme.font_sans.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_ev, _w, cx| cx.stop_propagation())
            .track_focus(&focus)
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
                let (armed, at) = up_cursor.update(cx, |c, _| (std::mem::take(&mut c.armed), c.at));
                if armed {
                    cx.stop_propagation();
                    if let Some(at) = at {
                        choose(&up_menu, at, &up_close, window, cx);
                    }
                }
            })
            .on_mouse_down_out(move |_ev, window, cx| out_close(window, cx))
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

/// Row `n`: its icon, its words, its shortcut and its tick; marked, washed as the keyboard's.
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
    let selector = format!("{id}-{}", item.key);
    let (role, ticked) = match item.mark {
        Mark::Plain => (Role::MenuItem, None),
        Mark::Check(on) => (Role::MenuItemCheckBox, Some(on)),
        Mark::Radio(on) => (Role::MenuItemRadio, Some(on)),
    };
    let (hover_cursor, click_menu, click_close) =
        (cursor.clone(), Rc::clone(menu), Rc::clone(close));
    let disabled = item.disabled;
    popover::menu_row_nav(theme, false, marked && !disabled, selector.clone())
        .id(ElementId::Name(selector.clone().into()))
        .debug_selector(move || selector)
        .role(role)
        .aria_label(item.label.clone())
        .when_some(ticked, |el, on| {
            el.aria_toggled(if on { Toggled::True } else { Toggled::False })
        })
        .when(marked, |el| el.aria_active_descendant())
        // A row that cannot run now: zeron's disabled menu row.
        .when(disabled, |el| el.opacity(0.38).cursor_default())
        .on_mouse_move(move |_ev, _window, cx| {
            if !disabled && hover_cursor.read(cx).at != Some(n) {
                hover_cursor.update(cx, |c, cx| {
                    c.at = Some(n);
                    cx.notify();
                });
            }
        })
        .on_click(move |_ev, window, cx| choose(&click_menu, n, &click_close, window, cx))
        .children(item.icon.map(|glyph| {
            crate::icons::icon(glyph).size(px(16.0)).flex_none().text_color(theme.text_muted)
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(item.label.clone()),
        )
        .children(item.shortcut.as_ref().map(|keys| popover::kbd_hint(theme, keys)))
        .when_some(ticked, |el, on| {
            el.child(crate::settings::widgets::select_check(theme, on))
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

    /// The arrows walk round the ends and past a disabled row; Home and End go to the ends.
    #[test]
    fn the_arrows_walk_round_past_what_is_disabled() {
        let m = menu(&[("Copy", false), ("Paste", true), ("Find", false)]);
        assert_eq!(m.step(None, 1), Some(0), "down from nothing: the first");
        assert_eq!(m.step(Some(0), 1), Some(2), "past the disabled Paste");
        assert_eq!(m.step(Some(2), 1), Some(0), "round the end");
        assert_eq!(m.step(Some(0), -1), Some(2), "round the top");
        assert_eq!((m.end(true), m.end(false)), (Some(0), Some(2)));
    }

    /// Typed letters go to the next row they start, round to the top; more letters stay on the
    /// row while it still starts with them.
    #[test]
    fn letters_go_to_the_row_they_start() {
        let m = menu(&[("Copy", false), ("Clear", false), ("Find", false)]);
        assert_eq!(m.typed_to(None, "c"), Some(0));
        assert_eq!(m.typed_to(Some(0), "c"), Some(1), "the next C");
        assert_eq!(m.typed_to(Some(1), "c"), Some(0), "round to the top");
        assert_eq!(m.typed_to(Some(1), "cl"), Some(1), "still Clear");
        assert_eq!(m.typed_to(Some(0), "f"), Some(2));
    }

    /// Separators part groups and are not rows: they count for nothing in the keyboard's walk.
    #[test]
    fn a_separator_is_no_row() {
        let mut m = menu(&[("Copy", false)]);
        m.separator();
        m.push(MenuItem::new("find", "Find", |_, _| {}).shortcut("⌘F"));
        assert_eq!(m.len(), 2);
        assert_eq!(m.step(Some(0), 1), Some(1));
        assert_eq!(m.row(1).and_then(MenuItem::shortcut_keys).map(SharedString::as_ref), Some("⌘F"));
    }
}
