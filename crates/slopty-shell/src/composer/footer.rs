// slopty: Slopty's own file (not in zeron).
//! slopty: the footer under the composer, always there (the TUI's footer row): the person's
//! status line with its colours, what the agent's mods put there, the goal it works towards
//! ("Goal · 5h"), and the permission mode ("Bypass permissions on", amber when nothing
//! is asked), which ⇧Tab cycles in the agent's order and a click picks from a menu. The
//! background counts are the Background tray's, right above it, so the footer does not repeat
//! them.
//!
//! The words are the view model's (`slopty_view_doc::dock`); what ticks (the goal's age) is
//! ticked here from the times it carries.

use std::time::Instant;

use gpui::{AnyElement, KeyDownEvent, KeyUpEvent};
use slopty_view_doc::calls;
use slopty_view_doc::dock::{ChatDock, GoalLine, ModeChip, StatusSpan};
use slopty_view_doc::work::duration_words;

use super::*;

/// The tallest a status line grows in the footer before it truncates.
const STATUS_MAX_WIDTH: f32 = 280.0;
const SEGMENT_HEIGHT: f32 = 24.0;

/// Which footer menu is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FooterMenu {
    Mode,
    Goal,
}

/// What the footer keeps between frames.
#[derive(Default)]
pub(crate) struct FooterState {
    pub menu: crate::popover::Popup<FooterMenu>,
    /// The mode asked for and not yet confirmed by the dock.
    pub pending_mode: Option<String>,
    /// The open mode menu's keyboard.
    pub(crate) keys: MenuKeys,
}

/// slopty: an open footer menu's keyboard, as the shared menu's (`live::menu`): ↑ ↓ walk the
/// rows round, Home and End go to the ends, letters go to the row they start, ↩ and Space
/// choose on their release, Esc closes. Opened from the keyboard, the current row starts marked.
#[derive(Default)]
pub(crate) struct MenuKeys {
    focus: Option<FocusHandle>,
    /// The marked row.
    pub(crate) at: Option<usize>,
    armed: bool,
    typed: String,
    typed_at: Option<Instant>,
}

impl MenuKeys {
    /// Start over for a menu just opened: the row `at` marked (from the keyboard), or none.
    fn reset(&mut self, at: Option<usize>) {
        self.at = at;
        self.armed = false;
        self.typed.clear();
        self.typed_at = None;
    }

    /// A key down on the open menu of `menu`: what it does.
    pub(crate) fn key(&mut self, menu: &crate::live::menu::Menu, key: &str, typed: Option<&str>, held: &gpui::Modifiers, now: Instant) -> MenuKey {
        let to = match key {
            "escape" => return MenuKey::Close,
            "enter" | "space" if !held.modified() => {
                self.armed = true;
                return MenuKey::Held;
            }
            "down" => menu.step(self.at, 1),
            "up" => menu.step(self.at, -1),
            "home" => menu.step(None, 1),
            "end" => menu.step(None, -1),
            _ => {
                let letter = typed
                    .filter(|t| !t.trim().is_empty())
                    .filter(|_| !held.platform && !held.control && !held.alt && !held.function);
                let Some(letter) = letter else { return MenuKey::Ignored };
                if self.typed_at.is_none_or(|then| now.saturating_duration_since(then) > crate::live::menu::TYPING) {
                    self.typed.clear();
                }
                self.typed.push_str(&letter.to_lowercase());
                self.typed_at = Some(now);
                menu.typed_to(self.at, &self.typed)
            }
        };
        if let Some(to) = to {
            self.at = Some(to);
        }
        MenuKey::Held
    }

    /// ↩ or Space released: the row to choose, when its press was on the menu.
    pub(crate) fn release(&mut self, key: &str, held: &gpui::Modifiers) -> Option<usize> {
        if !matches!(key, "enter" | "space") || held.modified() {
            return None;
        }
        std::mem::take(&mut self.armed).then_some(self.at).flatten()
    }
}

/// What a key did on an open footer menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    /// The menu's: nothing else takes it.
    Held,
    /// Esc: the menu closes with nothing chosen.
    Close,
    /// Not the menu's.
    Ignored,
}

/// The mode menu's rows as the shared menu's, for its keyboard.
fn mode_menu(chip: &ModeChip) -> crate::live::menu::Menu {
    let mut menu = crate::live::menu::Menu::new();
    for choice in &chip.choices {
        menu.push(crate::live::menu::MenuItem::new(choice.id.clone(), choice.label.clone(), |_, _| {}));
    }
    menu
}

/// How a goal reads in the footer at `now`: "Goal · 5h", its age's most significant
/// unit, nothing in its first second.
pub(crate) fn goal_words(goal: &GoalLine, now: i64) -> String {
    let age = u64::try_from(now.saturating_sub(goal.set_at)).unwrap_or(0);
    if age < 1_000 {
        return "Goal".to_owned();
    }
    let words = duration_words(age);
    let first = words.split(' ').next().unwrap_or(&words);
    // slopty: a plain noun and its age, never the TUI's command syntax.
    format!("Goal · {first}")
}

/// A goal's details, for its hover and its card: the condition, how far it has come, the last
/// check.
pub(crate) fn goal_details(goal: &GoalLine, now: i64) -> Vec<String> {
    let age = duration_words(u64::try_from(now.saturating_sub(goal.set_at)).unwrap_or(0));
    let mut stats = vec![format!("running {age}"), format!("{} turn{}", goal.iterations, if goal.iterations == 1 { "" } else { "s" })];
    if let Some(tokens) = goal.tokens {
        stats.push(format!("{} tokens", slopty_view_doc::tokens_short(tokens)));
    }
    let mut lines = vec![format!("Goal: {}", goal.condition), stats.join(" · ")];
    if let Some(reason) = goal.last_reason.as_ref().filter(|r| !r.trim().is_empty()) {
        lines.push(format!("Last check: {reason}"));
    }
    lines
}

/// The mode after `current` in `chip`'s choices, wrapping: what ⇧Tab switches to.
pub(crate) fn next_mode(chip: &ModeChip, current: &str) -> Option<String> {
    let choices = &chip.choices;
    if choices.len() < 2 {
        return None;
    }
    let at = choices.iter().position(|choice| choice.id == current).unwrap_or(choices.len() - 1);
    Some(choices[(at + 1) % choices.len()].id.clone())
}

/// What the mode chip says: its words when the mode says something, else its name.
fn mode_words(chip: &ModeChip) -> String {
    if chip.words.trim().is_empty() { chip.label.clone() } else { chip.words.clone() }
}

/// slopty: a footer chip's press, a button: on a touch screen it takes a finger's 44 pt around
/// what it draws, its margins giving the room back, so the row lays out as drawn and the wash
/// and the ring (`focus_ring_within` with the reach) stay on the chip (#23). The reach.
fn finger_segment(id: &'static str) -> (gpui::Stateful<gpui::Div>, f32) {
    let reach = crate::surface_chrome::touch_reach(SEGMENT_HEIGHT);
    let press = div()
        .id(id)
        .role(Role::Button)
        .flex_none()
        .flex()
        .p(px(reach))
        .m(px(-reach))
        .text_size(crate::typography::ui_rems(11.0));
    (press, reach)
}

/// What a footer chip draws: its height, its inset and its corners, for its wash.
fn segment_drawn() -> gpui::Div {
    div()
        .flex_none()
        .h(px(SEGMENT_HEIGHT))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(Theme::CONTROL_RADIUS))
}

/// The footer's row of `segments`.
fn footer_row(segments: Vec<AnyElement>) -> AnyElement {
    div()
        .id("dock-footer")
        .flex_none()
        .min_w_0()
        .h(px(SEGMENT_HEIGHT))
        .flex()
        .items_center()
        .gap(px(4.0))
        .children(segments)
        .into_any_element()
}

/// The colour of one of xterm's 256 colours, the first 16 from the terminal theme.
pub(crate) fn ansi_color(index: u8, theme: &Theme) -> gpui::Hsla {
    const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
    let rgb = match index {
        0..=15 => return theme.terminal.ansi[usize::from(index)],
        16..=231 => {
            let n = u32::from(index - 16);
            (LEVELS[(n / 36) as usize] << 16) | (LEVELS[(n / 6 % 6) as usize] << 8) | LEVELS[(n % 6) as usize]
        }
        _ => {
            let grey = 8 + 10 * u32::from(index - 232);
            (grey << 16) | (grey << 8) | grey
        }
    };
    gpui::rgb(rgb).into()
}

/// A status span's colour (`ansi:{n}` or `#rrggbb`), or `None` for the footer's own.
pub(crate) fn span_color(spec: Option<&str>, theme: &Theme) -> Option<gpui::Hsla> {
    let spec = spec?.trim();
    if let Some(n) = spec.strip_prefix("ansi:") {
        return n.parse::<u8>().ok().map(|n| ansi_color(n, theme));
    }
    let hex = spec.strip_prefix('#')?;
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten().map(|rgb| gpui::rgb(rgb).into())
}

impl Composer {
    /// ⇧Tab: the next permission mode, in the agent's order (`slopty.SetMode`). Whether there
    /// was one to switch to.
    pub(super) fn cycle_mode(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(chat) = self.dock.chat.clone() else { return false };
        let Some(dock) = self.dock.dock_of(&chat) else { return false };
        let Some(chip) = dock.mode.as_ref().filter(|_| dock.can.set_mode) else { return false };
        let current = self.footer.pending_mode.clone().unwrap_or_else(|| chip.id.clone());
        let Some(next) = next_mode(chip, &current) else { return false };
        self.set_mode(next, cx);
        true
    }

    /// Switch the chat's permission mode to `mode`.
    /// The footer shows it at once; a switch that fails goes back to the mode the agent is in,
    /// and says why.
    pub(crate) fn set_mode(&mut self, mode: String, cx: &mut Context<Self>) {
        self.footer.pending_mode = Some(mode.clone());
        self.dock_call_or(calls::SET_MODE, serde_json::json!({ "mode": mode }), "Couldn't switch the mode", |this| {
            this.footer.pending_mode = None;
        }, cx);
        cx.notify();
    }

    /// Open or close `menu`; opened from the keyboard (`keyed`), its current row starts marked,
    /// and it takes the keyboard either way.
    pub(super) fn toggle_footer_menu(&mut self, menu: FooterMenu, keyed: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.footer.menu.take_press_was_open() || self.footer.menu.as_open() == Some(&menu) {
            self.close_footer_menu(window, cx);
            return;
        }
        self.footer.menu.open(menu);
        // slopty: the menu takes the keyboard, its current row marked when opened from it.
        let current = self.dock.chat.as_deref().and_then(|chat| self.dock.dock_of(chat)).and_then(|dock| {
            let chip = dock.mode.as_ref()?;
            let id = self.footer.pending_mode.as_deref().unwrap_or(&chip.id);
            chip.choices.iter().position(|choice| choice.id == id)
        });
        self.footer.keys.reset(if keyed { current.or(Some(0)) } else { None });
        let focus = self.footer.keys.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        window.defer(cx, move |window, cx| window.focus(&focus, cx));
        cx.notify();
    }

    /// Close the open footer menu; the keyboard goes back to the composer when the menu had it.
    fn close_footer_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.footer.menu.begin_close(crate::clock::now(cx)) {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.footer.menu);
        }
        if self.footer.keys.focus.as_ref().is_some_and(|focus| focus.is_focused(window)) {
            window.focus(&self.input.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// The open footer menu's rows as the shared menu's, for its keyboard.
    fn open_footer_menu(&self) -> Option<crate::live::menu::Menu> {
        match self.footer.menu.as_open()? {
            FooterMenu::Mode => {
                let chip = self.dock.chat.as_deref().and_then(|chat| self.dock.dock_of(chat))?.mode.as_ref()?;
                Some(mode_menu(chip))
            }
            FooterMenu::Goal => {
                let mut menu = crate::live::menu::Menu::new();
                menu.push(crate::live::menu::MenuItem::new("clear", "Clear goal", |_, _| {}));
                Some(menu)
            }
        }
    }

    /// A key on the open footer menu.
    fn footer_menu_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.open_footer_menu() else { return };
        let now = cx.background_executor().now();
        let stroke = &ev.keystroke;
        // A letter's own key stands for its character where the event carries none.
        let typed = stroke.key_char.as_deref().or_else(|| (stroke.key.chars().count() == 1).then_some(stroke.key.as_str()));
        match self.footer.keys.key(&menu, &stroke.key, typed, &stroke.modifiers, now) {
            MenuKey::Held => cx.stop_propagation(),
            MenuKey::Close => {
                cx.stop_propagation();
                self.close_footer_menu(window, cx);
            }
            MenuKey::Ignored => {}
        }
        cx.notify();
    }

    /// ↩ or Space released on the open footer menu: choose the marked row.
    fn footer_menu_key_up(&mut self, ev: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.footer.keys.release(&ev.keystroke.key, &ev.keystroke.modifiers) else { return };
        cx.stop_propagation();
        match self.footer.menu.as_open().copied() {
            Some(FooterMenu::Mode) => {
                let id = self
                    .dock
                    .chat
                    .as_deref()
                    .and_then(|chat| self.dock.dock_of(chat))
                    .and_then(|dock| dock.mode.as_ref()?.choices.get(at).map(|choice| choice.id.clone()));
                self.close_footer_menu(window, cx);
                if let Some(id) = id {
                    self.set_mode(id, cx);
                }
            }
            Some(FooterMenu::Goal) => {
                self.close_footer_menu(window, cx);
                self.dock_call(calls::CLEAR_GOAL, serde_json::json!({}), "Couldn't clear the goal", cx);
            }
            None => {}
        }
    }

    /// The open footer menu's card takes the keyboard: its focus and its keys.
    fn keyed_card(&self, card: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let card = match self.footer.keys.focus.as_ref() {
            Some(focus) => card.track_focus(focus),
            None => card,
        };
        card.on_key_down(cx.listener(Self::footer_menu_key)).on_key_up(cx.listener(Self::footer_menu_key_up))
    }

    /// The footer's segments for the selected chat, or `None` with nothing to say.
    pub(super) fn render_dock_footer(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = self.state.read(cx).selected_chat.clone();
        let dock: Option<ChatDock> = chat.as_deref().and_then(|chat| self.dock.dock_of(chat)).cloned();
        // slopty: a chat's mode is said in its footer, the agent's own while it runs, else the
        // one its next run takes; only the new chat's canvas, which has no footer, keeps it in
        // the composer.
        let in_footer = chat.is_some();
        if self.pickers.read(cx).mode_in_footer != in_footer {
            self.pickers.update(cx, |pickers, cx| {
                pickers.mode_in_footer = in_footer;
                cx.notify();
            });
        }
        let picked = dock
            .as_ref()
            .is_none_or(|dock| dock.mode.is_none())
            .then(|| self.pickers.read(cx).mode_label(cx))
            .flatten()
            .filter(|_| in_footer);
        let Some(dock) = dock else {
            return picked.map(|label| footer_row(vec![self.picked_mode_segment(label, theme, cx)]));
        };
        // A switch the dock now shows is confirmed.
        if let (Some(pending), Some(chip)) = (&self.footer.pending_mode, &dock.mode)
            && *pending == chip.id
        {
            self.footer.pending_mode = None;
        }
        let now = crate::clock::utc(cx).timestamp_millis();
        let mut segments: Vec<AnyElement> = Vec::new();
        // slopty: where ↑ has walked to among the chat's prompts.
        if self.extras.history.is_some() {
            let total = super::history::prompts(&self.state.read(cx).transcript).len();
            if let Some(words) = super::history::history_words(self.extras.history.as_ref(), &self.current_key, total) {
                segments.push(
                    div()
                        .id("dock-history")
                        .role(Role::Status)
                        .aria_label(SharedString::from(words.clone()))
                        .flex_none()
                        .px(px(4.0))
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_faint)
                        .font_features(crate::typography::tabular())
                        .child(SharedString::from(words))
                        .into_any_element(),
                );
            }
        }
        if !dock.status_line.is_empty() {
            segments.push(status_line(&dock.status_line, theme));
        }
        for surface in dock.surfaces.iter().filter(|s| s.kind == slopty_view_doc::ModSurface::STATUS) {
            let words = surface.text.clone().or(surface.title.clone()).unwrap_or_default();
            if words.trim().is_empty() {
                continue;
            }
            segments.push(
                div()
                    .id(SharedString::from(format!("dock-mod-status-{}", surface.id)))
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("{}: {words}", surface.plugin)))
                    .flex_none()
                    .max_w(px(200.0))
                    .truncate()
                    .px(px(4.0))
                    .text_size(crate::typography::ui_rems(11.0))
                    .text_color(theme.text_faint)
                    .tooltip(crate::settings::widgets::text_tooltip(surface.plugin.clone()))
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                    .child(SharedString::from(words))
                    .into_any_element(),
            );
        }
        if let Some(goal) = &dock.goal {
            segments.push(self.goal_segment(goal, now, theme, cx));
        }
        if let Some(chip) = &dock.mode {
            segments.push(self.mode_segment(chip, dock.can.set_mode, theme, cx));
        } else if let Some(label) = picked {
            segments.push(self.picked_mode_segment(label, theme, cx));
        }
        if segments.is_empty() {
            return None;
        }
        Some(footer_row(segments))
    }

    /// The mode the chat's next run takes, where its agent says none of its own: quiet, as the
    /// recommended default reads; a click opens the model menu at its Mode choices.
    fn picked_mode_segment(&self, label: SharedString, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        // slopty: on a touch screen the chip takes a finger's 44 pt around what it draws (#23).
        let (segment, reach) = finger_segment("dock-picked-mode");
        segment
            .aria_label(SharedString::from(format!("Mode: {label}")))
            .text_color(theme.text_faint)
            .cursor_pointer()
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring_within(el, theme, reach))
            .hover_fade("dock-picked-mode-hover")
            .on_click(cx.listener(|this, _, window, cx| {
                this.pickers.update(cx, |pickers, cx| pickers.open_mode_menu(window, cx));
            }))
            .child(
                segment_drawn()
                    .id("dock-picked-mode-drawn")
                    .bg(motion::hover_blend("dock-picked-mode-hover", gpui::transparent_black(), crate::theme::ink(0.05)))
                    .active(|s| s.bg(crate::theme::ink(0.09)))
                    .child(label),
            )
            .into_any_element()
    }

    /// "◎ Goal · 5h": hover shows the condition, a click its card with Clear.
    fn goal_segment(&self, goal: &GoalLine, now: i64, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let words = goal_words(goal, now);
        let details = goal_details(goal, now).join("\n");
        let open = self.footer.menu.get() == Some(&FooterMenu::Goal);
        let hover: SharedString = "dock-goal-hover".into();
        let chip = div()
            .id("dock-goal")
            .role(Role::Button)
            .aria_label(SharedString::from(words.clone()))
            .aria_description(SharedString::from(details.clone()))
            .relative()
            .flex_none()
            .h(px(SEGMENT_HEIGHT))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .text_size(crate::typography::ui_rems(11.0))
            // slopty: its age's digits keep one width as it ticks.
            .font_features(crate::typography::tabular())
            .text_color(theme.accent)
            .cursor_pointer()
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring(el, theme))
            .bg(if open {
                crate::theme::ink(0.05)
            } else {
                motion::hover_blend(&hover, gpui::transparent_black(), crate::theme::ink(0.05))
            })
            .hover_fade(hover)
            .active(|s| s.bg(crate::theme::ink(0.09))) // slopty: a press shows
            .when(!open, |el| {
                el.tooltip(crate::settings::widgets::text_tooltip(details))
                    .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.footer.menu.note_trigger_press_matching(|m| *m == FooterMenu::Goal)
                }),
            )
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                this.toggle_footer_menu(FooterMenu::Goal, ev.is_keyboard(), window, cx)
            }))
            .child(goal_glyph(theme))
            .child(SharedString::from(words));
        if self.footer.menu.get() != Some(&FooterMenu::Goal) {
            return chip.into_any_element();
        }
        let popup = theme.for_popup();
        let mut card = self
            .keyed_card(crate::popover::popover_card(&popup), cx)
            .id("dock-goal-card")
            .role(Role::Menu)
            .aria_label("Goal")
            .w(px(300.0))
            .flex()
            .flex_col()
            .child(crate::popover::menu_heading(&popup, "Goal"));
        for (ix, line) in goal_details(goal, now).into_iter().enumerate() {
            card = card.child(
                div()
                    .px(px(8.0))
                    .pb(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(crate::typography::ui_rems(17.0))
                    .text_color(if ix == 0 { popup.text } else { popup.text_muted })
                    .child(SharedString::from(line)),
            );
        }
        let card = card
            .child(
                div().px(px(4.0)).pb(px(4.0)).pt(px(2.0)).child(
                    crate::popover::menu_row_nav(&popup, false, self.footer.keys.at == Some(0), "dock-goal-clear")
                        .id("dock-goal-clear")
                        .role(Role::Button)
                        .aria_label("Clear goal")
                        .when(self.footer.keys.at == Some(0), |el| el.aria_active_descendant())
                        .on_mouse_move(cx.listener(|this, _, _, cx| {
                            if this.footer.keys.at != Some(0) {
                                this.footer.keys.at = Some(0);
                                cx.notify();
                            }
                        }))
                        .child(div().flex_1().child("Clear goal"))
                        .child(crate::popover::kbd_hint(&popup, "/goal clear"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.close_footer_menu(window, cx);
                            this.dock_call(calls::CLEAR_GOAL, serde_json::json!({}), "Couldn't clear the goal", cx);
                        })),
                ),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_footer_menu(window, cx)));
        chip.child(crate::popover::anchored_menu_above_end(
            "dock-goal-menu",
            card.into_any_element(),
            self.footer.menu.closing_at(crate::clock::now(cx)),
        ))
        .into_any_element()
    }

    /// The permission mode: its words, quiet for the recommended default, amber when nothing is
    /// asked; a click opens its choices.
    fn mode_segment(&self, chip: &ModeChip, can_switch: bool, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let pending = self
            .footer
            .pending_mode
            .as_ref()
            .and_then(|id| chip.choices.iter().find(|choice| choice.id == *id));
        let words = match pending {
            Some(choice) => format!("{}…", choice.label),
            None => mode_words(chip),
        };
        // slopty: loud only when nothing is asked; the recommended default is quiet and wears no
        // lock.
        let (color, wash) = match chip.tone.as_str() {
            ModeChip::DANGER => (theme.warning, Some(theme.warning.opacity(0.12))),
            ModeChip::PLAN => (theme.accent, None),
            ModeChip::ACCEPT => (theme.text_muted, None),
            _ => (theme.text_faint, None),
        };
        let lock = chip.tone == ModeChip::DANGER;
        let open = self.footer.menu.get() == Some(&FooterMenu::Mode);
        let switchable = can_switch && chip.choices.len() > 1;
        // slopty: on a touch screen the chip takes a finger's 44 pt around what it draws (#23).
        let (segment, reach) = finger_segment("dock-mode");
        let drawn = segment_drawn()
            .gap(px(5.0))
            .when(lock, |el| el.child(crate::icons::icon(crate::icons::LOCK).size(px(12.0)).text_color(color)))
            .child(SharedString::from(words.clone()));
        let segment = segment
            .relative()
            .font_weight(if wash.is_some() { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
            .text_color(color);
        let aria = SharedString::from(format!("Mode: {words}"));
        if !switchable {
            return segment
                .aria_label(aria)
                .child(drawn.when_some(wash, |el, wash| el.bg(wash)))
                .into_any_element();
        }
        let rest = wash.unwrap_or(gpui::transparent_black());
        let segment = segment
            .cursor_pointer()
            // slopty: a tab stop with the shared ring; ↩ or Space opens the menu keyed.
            .tab_index(0)
            .map(|el| crate::surface_chrome::focus_ring_within(el, theme, reach))
            .aria_expanded(open)
            .child(
                drawn
                    .id("dock-mode-drawn")
                    .bg(if open {
                        crate::theme::ink(0.05)
                    } else {
                        motion::hover_blend("dock-mode-hover", rest, crate::theme::ink(0.05))
                    })
                    // slopty: a press shows, a step deeper than the hover.
                    .active(|s| s.bg(crate::theme::ink(0.09))),
            )
            .hover_fade("dock-mode-hover")
            .action_hint("Mode", Keys::Fixed("shift-tab"), cx)
            .aria_label(aria)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.footer.menu.note_trigger_press_matching(|m| *m == FooterMenu::Mode)
                }),
            )
            .on_click(cx.listener(|this, ev: &gpui::ClickEvent, window, cx| {
                this.toggle_footer_menu(FooterMenu::Mode, ev.is_keyboard(), window, cx)
            }));
        if !open {
            return segment.into_any_element();
        }
        let popup = theme.for_popup();
        let current = self.footer.pending_mode.clone().unwrap_or_else(|| chip.id.clone());
        let mut card = self
            .keyed_card(crate::popover::popover_card(&popup), cx)
            .id("dock-mode-card")
            .role(Role::Menu)
            .aria_label("Mode")
            .w(px(300.0))
            .flex()
            .flex_col()
            .child(crate::popover::menu_heading(&popup, "Mode"));
        for (ix, choice) in chip.choices.iter().enumerate() {
            let selected = choice.id == current;
            let marked = self.footer.keys.at == Some(ix);
            let id = choice.id.clone();
            let row_id = SharedString::from(format!("dock-mode-{}", choice.id));
            card = card.child(
                // slopty: the keyboard's row washed as the shared menu's.
                crate::popover::menu_row_nav(&popup, selected, marked, row_id.clone())
                    .id(row_id)
                    .role(Role::MenuItemRadio)
                    .aria_label(SharedString::from(choice.label.clone()))
                    .aria_toggled(if selected { gpui::Toggled::True } else { gpui::Toggled::False })
                    .when(marked, |el| el.aria_active_descendant())
                    .items_start()
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.footer.keys.at != Some(ix) {
                            this.footer.keys.at = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_footer_menu(window, cx);
                        this.set_mode(id.clone(), cx);
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(SharedString::from(choice.label.clone()))
                                    // slopty: the agent's own default, which it recommends.
                                    .children(
                                        crate::settings::widgets::Standing::of(choice.recommended, choice.recommended)
                                            .map(|standing| crate::settings::widgets::standing_note(&popup, standing).ml_0()),
                                    ),
                            )
                            .when_some(choice.description.clone(), |el, description| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(11.5))
                                        .line_height(crate::typography::ui_rems(15.0))
                                        .text_color(popup.text_muted)
                                        .child(SharedString::from(description)),
                                )
                            }),
                    )
                    .when(selected, |el| {
                        el.child(crate::icons::icon(crate::icons::CHECK).size(px(12.0)).text_color(popup.text_muted))
                    }),
            );
        }
        let card = card.child(
            div()
                .px(px(8.0))
                .py(px(4.0))
                .flex()
                .justify_end()
                .child(crate::popover::kbd_hint(
                    &popup,
                    &format!("{} cycles", crate::keys::combo_text("shift-tab")),
                )),
        );
        let card = card.on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_footer_menu(window, cx)));
        segment
            .child(crate::popover::anchored_menu_above_end(
                "dock-mode-menu",
                card.into_any_element(),
                self.footer.menu.closing_at(crate::clock::now(cx)),
            ))
            .into_any_element()
    }
}

/// ◎: a ring around a dot, the TUI's goal glyph.
fn goal_glyph(theme: &Theme) -> AnyElement {
    div()
        .flex_none()
        .size(px(10.0))
        .rounded_full()
        .border_1()
        .border_color(theme.accent)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(4.0)).rounded_full().bg(theme.accent))
        .into_any_element()
}

/// The person's status line in its own colours: one run of style each, truncated.
fn status_line(spans: &[StatusSpan], theme: &Theme) -> AnyElement {
    let text: String = spans.iter().map(|span| span.text.as_str()).collect();
    let text = text.replace('\n', " · ");
    let mut row = div()
        .id("dock-status-line")
        .role(Role::Status)
        .aria_label(SharedString::from(text.clone()))
        .flex_none()
        .max_w(px(STATUS_MAX_WIDTH))
        .overflow_hidden()
        .whitespace_nowrap()
        .px(px(4.0))
        .flex()
        .items_center()
        .text_size(crate::typography::ui_rems(11.0))
        .tooltip(crate::settings::widgets::text_tooltip(text))
        .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY);
    for span in spans {
        let words = span.text.replace('\n', " · ");
        if words.is_empty() {
            continue;
        }
        let fg = span_color(span.fg.as_deref(), theme).unwrap_or(theme.text_faint);
        let fg = if span.dim { fg.opacity(0.6) } else { fg };
        let bg = span_color(span.bg.as_deref(), theme);
        row = row.child(
            div()
                .flex_none()
                .text_color(fg)
                .when_some(bg, |el, bg| el.bg(bg))
                .when(span.bold, |el| el.font_weight(gpui::FontWeight::BOLD))
                .when(span.italic, |el| el.italic())
                .when(span.underline, |el| el.underline())
                .child(SharedString::from(words)),
        );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slopty_view_doc::dock::ModeChoice;

    fn chip(id: &str) -> ModeChip {
        ModeChip {
            id: id.into(),
            label: id.into(),
            words: String::new(),
            tone: ModeChip::QUIET.into(),
            choices: ["default", "acceptEdits", "plan", "bypassPermissions"]
                .iter()
                .map(|id| ModeChoice { id: (*id).into(), label: (*id).into(), description: None, recommended: false })
                .collect(),
        }
    }

    /// ⇧Tab walks the agent's modes in its order and wraps.
    #[test]
    fn the_next_mode_follows_the_agents_order_and_wraps() {
        assert_eq!(next_mode(&chip("default"), "default").as_deref(), Some("acceptEdits"));
        assert_eq!(next_mode(&chip("default"), "plan").as_deref(), Some("bypassPermissions"));
        assert_eq!(next_mode(&chip("default"), "bypassPermissions").as_deref(), Some("default"));
        let mut lone = chip("default");
        lone.choices.truncate(1);
        assert_eq!(next_mode(&lone, "default"), None);
    }

    /// A goal says how long it has been active in its most significant unit.
    #[test]
    fn a_goal_reads_its_age() {
        let goal = GoalLine { condition: "tests pass".into(), set_at: 0, iterations: 3, tokens: Some(41_000), last_reason: Some("2 failing".into()) };
        assert_eq!(goal_words(&goal, 500), "Goal");
        assert_eq!(goal_words(&goal, 5 * 3_600_000 + 120_000), "Goal · 5h");
        assert_eq!(
            goal_details(&goal, 90_000),
            ["Goal: tests pass", "running 1m 30s · 3 turns · 41k tokens", "Last check: 2 failing"]
        );
    }

    /// The status line's colours: a palette index, a cube colour, a grey, a hex, none.
    #[test]
    fn status_colours_read_ansi_and_hex() {
        let theme = Theme::dark();
        assert_eq!(span_color(Some("ansi:1"), &theme), Some(theme.terminal.ansi[1]));
        assert_eq!(span_color(Some("ansi:196"), &theme), Some(gpui::rgb(0xff0000).into()));
        assert_eq!(span_color(Some("ansi:244"), &theme), Some(gpui::rgb(0x808080).into()));
        assert_eq!(span_color(Some("#00ff00"), &theme), Some(gpui::rgb(0x00ff00).into()));
        assert_eq!(span_color(Some("blue"), &theme), None);
        assert_eq!(span_color(None, &theme), None);
    }
}
