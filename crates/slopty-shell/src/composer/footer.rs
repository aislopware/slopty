// slopty: Slopty's own file (not in zeron).
//! slopty: the footer under the composer, always there (the TUI's footer row): the person's
//! status line with its colours, what the agent's mods put there, the goal it works towards
//! ("/goal active (5h)"), the background counts ("3 commands · 1 monitor", which open the
//! Background tray), and the permission mode ("Bypass permissions on", amber when nothing is
//! asked), which ⇧Tab cycles in the agent's order and a click picks from a menu.
//!
//! The words are the view model's (`slopty_view_doc::dock`); what ticks (the goal's age) is
//! ticked here from the times it carries.

use gpui::AnyElement;
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
}

/// How a goal reads in the footer at `now`: "/goal active (5h)", its age's most significant
/// unit, nothing in its first second.
pub(crate) fn goal_words(goal: &GoalLine, now: i64) -> String {
    let age = u64::try_from(now.saturating_sub(goal.set_at)).unwrap_or(0);
    if age < 1_000 {
        return "/goal active".to_owned();
    }
    let words = duration_words(age);
    let first = words.split(' ').next().unwrap_or(&words);
    format!("/goal active ({first})")
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

    fn toggle_footer_menu(&mut self, menu: FooterMenu, cx: &mut Context<Self>) {
        if self.footer.menu.take_press_was_open() || self.footer.menu.as_open() == Some(&menu) {
            self.close_footer_menu(cx);
            return;
        }
        self.footer.menu.open(menu);
        cx.notify();
    }

    fn close_footer_menu(&mut self, cx: &mut Context<Self>) {
        if self.footer.menu.begin_close() {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.footer.menu);
        }
        cx.notify();
    }

    /// The footer's segments for the selected chat, or `None` with nothing to say.
    pub(super) fn render_dock_footer(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chat = self.state.read(cx).selected_chat.clone();
        let dock: Option<ChatDock> = chat.as_deref().and_then(|chat| self.dock.dock_of(chat)).cloned();
        let in_footer = dock.as_ref().is_some_and(|dock| dock.mode.is_some());
        if self.pickers.read(cx).mode_in_footer != in_footer {
            self.pickers.update(cx, |pickers, cx| {
                pickers.mode_in_footer = in_footer;
                cx.notify();
            });
        }
        let dock = dock?;
        // A switch the dock now shows is confirmed.
        if let (Some(pending), Some(chip)) = (&self.footer.pending_mode, &dock.mode)
            && *pending == chip.id
        {
            self.footer.pending_mode = None;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut segments: Vec<AnyElement> = Vec::new();
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
        if dock.background.total() > 0 || dock.background.needs_you > 0 {
            segments.push(self.background_segment(&dock, theme, cx));
        }
        if let Some(chip) = &dock.mode {
            segments.push(self.mode_segment(chip, dock.can.set_mode, theme, cx));
        }
        if segments.is_empty() {
            return None;
        }
        Some(
            div()
                .id("dock-footer")
                .flex_none()
                .min_w_0()
                .h(px(SEGMENT_HEIGHT))
                .flex()
                .items_center()
                .gap(px(4.0))
                .children(segments)
                .into_any_element(),
        )
    }

    /// "◎ /goal active (5h)": hover shows the condition, a click its card with Clear.
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
            // slopty: a goal that is set is chosen, not news: the text colour, not the accent.
            .text_color(theme.text)
            .cursor_pointer()
            .bg(if open {
                crate::theme::ink(0.05)
            } else {
                motion::hover_blend(&hover, gpui::transparent_black(), crate::theme::ink(0.05))
            })
            .on_hover(motion::hover_listener(hover))
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
            .on_click(cx.listener(|this, _, _, cx| this.toggle_footer_menu(FooterMenu::Goal, cx)))
            .child(goal_glyph(theme))
            .child(SharedString::from(words));
        if self.footer.menu.get() != Some(&FooterMenu::Goal) {
            return chip.into_any_element();
        }
        let popup = theme.for_popup();
        let mut card = crate::popover::popover_card(&popup)
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
                    crate::popover::menu_row(&popup, false, "dock-goal-clear")
                        .id("dock-goal-clear")
                        .role(Role::Button)
                        .aria_label("Clear goal")
                        .child(div().flex_1().child("Clear goal"))
                        .child(crate::popover::kbd_hint(&popup, "/goal clear"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_footer_menu(cx);
                            this.dock_call(calls::CLEAR_GOAL, serde_json::json!({}), "Couldn't clear the goal", cx);
                        })),
                ),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_footer_menu(cx)));
        chip.child(crate::popover::anchored_menu_above_end(
            "dock-goal-menu",
            card.into_any_element(),
            self.footer.menu.closing_since(),
        ))
        .into_any_element()
    }

    /// "3 commands · 1 monitor": a click opens the Background tray.
    fn background_segment(&self, dock: &ChatDock, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let needs = dock.background.needs_you;
        let mut words = if dock.background_words.is_empty() {
            dock.background.summary()
        } else {
            dock.background_words.clone()
        };
        if needs > 0 {
            words = if words.is_empty() { format!("{needs} needs you") } else { format!("{words} · {needs} needs you") };
        }
        div()
            .id("dock-background")
            .role(Role::Button)
            .aria_label(SharedString::from(words.clone()))
            .aria_description("Opens the Background tray")
            .flex_none()
            .h(px(SEGMENT_HEIGHT))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .text_size(crate::typography::ui_rems(11.0))
            .text_color(if needs > 0 { theme.warning } else { theme.text_muted })
            .cursor_pointer()
            .bg(motion::hover_blend("dock-background-hover", gpui::transparent_black(), crate::theme::ink(0.05)))
            .on_hover(motion::hover_listener("dock-background-hover"))
            .action_hint(
                SharedString::from(words.clone()),
                Keys::Action(Box::new(crate::background_tray::FocusBackgroundTray)),
                cx,
            )
            .on_click(cx.listener(|this, _, window, cx| this.focus_background_tray(window, cx)))
            .child(crate::icons::icon(crate::icons::LIST).size(px(12.0)).text_color(theme.text_faint))
            .child(SharedString::from(words))
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
        // slopty: loud only when nothing is asked; plan mode is chosen, not news (the text colour,
        // not the accent); the recommended default is quiet and wears no lock.
        let (color, wash) = match chip.tone.as_str() {
            ModeChip::DANGER => (theme.warning, Some(theme.warning.opacity(0.12))),
            ModeChip::PLAN => (theme.text, None),
            ModeChip::ACCEPT => (theme.text_muted, None),
            _ => (theme.text_faint, None),
        };
        let lock = chip.tone == ModeChip::DANGER;
        let open = self.footer.menu.get() == Some(&FooterMenu::Mode);
        let switchable = can_switch && chip.choices.len() > 1;
        let segment = div()
            .id("dock-mode")
            .role(Role::Button)
            .relative()
            .flex_none()
            .h(px(SEGMENT_HEIGHT))
            .px(px(6.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .rounded(px(Theme::CONTROL_RADIUS))
            .text_size(crate::typography::ui_rems(11.0))
            .font_weight(if wash.is_some() { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::NORMAL })
            .text_color(color)
            .when(lock, |el| el.child(crate::icons::icon(crate::icons::LOCK).size(px(12.0)).text_color(color)))
            .child(SharedString::from(words.clone()));
        let aria = SharedString::from(format!("Mode: {words}"));
        if !switchable {
            return segment.aria_label(aria).when_some(wash, |el, wash| el.bg(wash)).into_any_element();
        }
        let rest = wash.unwrap_or(gpui::transparent_black());
        let segment = segment
            .cursor_pointer()
            .bg(if open {
                crate::theme::ink(0.05)
            } else {
                motion::hover_blend("dock-mode-hover", rest, crate::theme::ink(0.05))
            })
            .on_hover(motion::hover_listener("dock-mode-hover"))
            .action_hint("Mode", Keys::Fixed("shift-tab"), cx)
            .aria_label(aria)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.footer.menu.note_trigger_press_matching(|m| *m == FooterMenu::Mode)
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| this.toggle_footer_menu(FooterMenu::Mode, cx)));
        if !open {
            return segment.into_any_element();
        }
        let popup = theme.for_popup();
        let current = self.footer.pending_mode.clone().unwrap_or_else(|| chip.id.clone());
        let mut card = crate::popover::popover_card(&popup)
            .w(px(300.0))
            .flex()
            .flex_col()
            .child(crate::popover::menu_heading(&popup, "Mode"));
        for choice in &chip.choices {
            let selected = choice.id == current;
            let id = choice.id.clone();
            let row_id = SharedString::from(format!("dock-mode-{}", choice.id));
            card = card.child(
                crate::popover::menu_row(&popup, selected, row_id.clone())
                    .id(row_id)
                    .role(Role::MenuItem)
                    .aria_label(SharedString::from(choice.label.clone()))
                    .aria_selected(selected)
                    .items_start()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.close_footer_menu(cx);
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
        let card = card.on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_footer_menu(cx)));
        segment
            .child(crate::popover::anchored_menu_above_end(
                "dock-mode-menu",
                card.into_any_element(),
                self.footer.menu.closing_since(),
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
        .border_color(theme.text)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(4.0)).rounded_full().bg(theme.text))
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
        assert_eq!(goal_words(&goal, 500), "/goal active");
        assert_eq!(goal_words(&goal, 5 * 3_600_000 + 120_000), "/goal active (5h)");
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
