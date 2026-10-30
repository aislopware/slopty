// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Session navigation: the sidebar is the session list, and the titlebar row names the session
//! shown (its program's icon, its title, its machine and branch, its mark), with the palette and a
//! new session at its trailing end.

use super::*;

/// The chat one step from `selected` in the sidebar `order`, wrapping at both
/// ends. Pure.
///
/// With nothing selected — the new-session canvas — cycling enters the list at
/// the end it would have wrapped to: the first row going forward, the last
/// going back. A selection that has since left the list (archived from another
/// device mid-cycle) is treated the same way rather than dead-ending.
pub(super) fn cycle_target(
    order: &[String],
    selected: Option<&str>,
    forward: bool,
) -> Option<String> {
    if order.is_empty() {
        return None;
    }
    let at = selected.and_then(|id| order.iter().position(|c| c == id));
    let next = match (at, forward) {
        (Some(at), true) => (at + 1) % order.len(),
        (Some(at), false) => (at + order.len() - 1) % order.len(),
        (None, true) => 0,
        (None, false) => order.len() - 1,
    };
    Some(order[next].clone())
}

impl Shell {
    /// ⌃⇥ and ⌃⇧⇥: the next or previous session in the list, quiet under an overlay that holds
    /// the keyboard.
    pub(super) fn cycle_navigation(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.navigation_overlay_open() {
            self.cycle_session(forward, cx);
        }
    }

    fn navigation_overlay_open(&self) -> bool {
        self.overlay_owns_keyboard()
            || self.delete_space_confirm.is_some()
            || self.chat_rename.is_some()
            || self.rename_space_dialog.is_some()
            || self.chat_menu.get().is_some()
            || self.space_menu.get().is_some()
            || self.user_menu.get().is_some()
            || self.spaces_menu.get().is_some()
            || !self.pending_file_closes.is_empty()
            || self.pending_exit.is_some()
    }

    /// Navigation gives the session's terminal the keyboard once it renders.
    pub(super) fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.session_terminal.update(cx, |panel, cx| panel.request_focus(cx));
    }

    /// The session's terminal takes the keyboard now (Esc from a view, a pane closing),
    /// and keeps it once its view renders.
    pub(super) fn focus_session_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.session_terminal.read(cx).focus_handle(), cx);
        self.focus_composer(cx);
    }

    /// Landing on a session (opening it, the boot's landing, back and forward) gives
    /// its terminal the keyboard, but not on a touch screen, where it would raise the software
    /// keyboard over the session: there it comes when the person taps it
    /// ([`super::touch_screen`]).
    pub(super) fn focus_composer_on_landing(&mut self, cx: &mut Context<Self>) {
        if !super::touch_screen() {
            self.focus_composer(cx);
        }
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab: step through the sidebar's Sessions list in
    /// the order it is drawn. Selection is immediate (no MRU overlay held open
    /// on the modifier) — one press, one session.
    ///
    /// Works from Settings too, landing back in chat like a jump; the
    /// shortcut recorder intercepts these keys while it records. Quiet under
    /// a keyboard-owning overlay: gpui dispatches a matched binding before any
    /// `on_key_down`, so a cycle under the add-space palette would strand the
    /// overlay over a session the user never picked.
    pub(super) fn cycle_session(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.overlay_owns_keyboard() {
            return;
        }
        // The same list `render_active_rows` draws and the jump shortcuts
        // count — one function, so neither can drift from the screen.
        let order = self.sidebar_visible_order(cx);
        let selected = self.state.read(cx).selected_chat.clone();
        if let Some(target) = cycle_target(&order, selected.as_deref(), forward) {
            self.open_chat(target, cx);
        }
    }

    /// Boot landing: the most recently active visible chat once the first
    /// chats frame has synced (manual selection wins; no chats → the
    /// new-session canvas shows).
    // Once, at the first synced frame, chat or no chat: a chat arriving later (from the
    // CLI or another device) never opens by itself.
    pub(super) fn boot_select_chat(&mut self, cx: &mut Context<Self>) {
        let first = {
            let state = self.state.read(cx);
            if !state.chats_synced || state.auto_selected {
                return;
            }
            state
                .selected_chat
                .is_none()
                .then(|| state.overview_chats(crate::clock::utc(cx)).first().map(|(_, c)| c.id.clone()))
                .flatten()
        };
        self.state.update(cx, |s, _| s.auto_selected = true);
        if let Some(first) = first {
            self.focus_composer_on_landing(cx); // not on touch
            self.state
                .update(cx, |s, cx| s.select_chat(Some(first), cx));
        }
    }

    /// Open a session from the sidebar: select it, the main area follows.
    pub(crate) fn open_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.command_palette = None;
        self.set_route(Route::Chat, cx);
        self.focus_composer_on_landing(cx); // not on touch
        self.nav.push(NavEntry::Chat(chat_id.clone()));
        self.state
            .update(cx, |s, cx| s.select_chat(Some(chat_id), cx));
        cx.notify();
    }

    /// `+` in the titlebar, the sidebar's and ⌘N: New session…, which starts an agent's
    /// TUI or a terminal in a project at once (`new_session`). `project` (a project's own `+`)
    /// is offered first. The open session stays until the new one opens.
    pub(super) fn open_new_session(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.set_route(Route::Chat, cx);
        self.open_new_session_picker(project, cx);
    }

    /// Where the row's content starts: at the inset card's left edge while the sidebar is
    /// open, gliding toward the window's controls as it closes.
    fn titlebar_row_left(&self) -> f32 {
        (self.sidebar_now() + Theme::SPACE_LG).max(self.title_bar_content_start())
    }

    /// The titlebar row over a session (`docs/decisions/layout.md`, "One row of chrome"): the
    /// session's identity (its program's icon, its title, then its machine and branch in
    /// secondary text, and its state's mark), and at the trailing end the palette and a new
    /// session. The row stands on the shown terminal's own ground (the column under it paints
    /// it, [`Shell::shown_ground`]), its words inked for that ground, and the title dims to
    /// secondary while the window is not the key one. Over the new-session canvas it names
    /// nothing; it keeps its height, drag region and buttons.
    pub(super) fn render_session_title_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx).clone();
        let on_canvas = self.state.read(cx).selected_chat_row().is_none();
        let row_left = self.titlebar_row_left();
        let row_gap = 8.0;
        // Room at the edge for the outermost control's press on a touch screen (#23).
        let right_pad = TITLEBAR_ACTION_EDGE_INSET.max(crate::surface_chrome::touch_reach(24.0));
        // Inked for the ground under it: the terminal's over the session, the window's own over
        // the canvas.
        let ink = if on_canvas { theme.clone() } else { ink_for(self.shown_ground(cx), &theme) };
        // The picker hangs under the identity (`picker`).
        let identity = super::picker::identity_anchor(
            (!on_canvas).then(|| self.render_titlebar_identity(&ink, window.is_window_active(), cx)),
            window,
        );
        // At the trailing end: the inbox's eye, the palette, then a new session, which over the
        // new-session canvas would open the canvas it is on.
        let eye = self.render_inbox_button(&ink, cx);
        let actions = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(TITLEBAR_CONTROL_GAP))
            .child(eye)
            .child(
                window_control_button(
                    "titlebar-palette",
                    icons::COMMAND,
                    &ink,
                    cx.listener(|this, _, window, cx| this.toggle_command_palette(window, cx)),
                )
                .action_hint("Command palette", Keys::Action(Box::new(ToggleCommandPalette)), cx),
            )
            .children((!on_canvas).then(|| {
                window_control_button(
                    "titlebar-new-session",
                    icons::PLUS,
                    &ink,
                    cx.listener(|this, _, _, cx| this.open_new_session(None, cx)),
                )
                .action_hint("New session", Keys::Action(Box::new(NewSession)), cx)
            }));
        let inner = div()
            .size_full()
            .flex()
            .items_center()
            .pt(px(super::titlebar_top_pad())) // a finger's presses inside the window
            .gap(px(row_gap))
            .pl(px(row_left))
            .pr(px(right_pad))
            .children(identity)
            .child(div().flex_1())
            .child(actions);
        let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
        self.titlebar_drag_region("chat-titlebar", bar, cx)
            .into_any_element()
    }

    /// The session's identity in the titlebar row: its program's icon, its title (whole in a
    /// tooltip when cut, rising in over the old when the agent gives another), its machine and
    /// branch in secondary text, then its state's mark, the same glyph as its row's in the
    /// list. `ink` is the theme inked for the ground under the row; the title dims to secondary
    /// while the window is not `key`. A press opens the session picker under it, as ⌘K does
    /// (Rex's); a right-click opens the session's menu under the row.
    fn render_titlebar_identity(
        &mut self,
        ink: &Theme,
        key: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = crate::clock::utc(cx);
        let (chat_id, title, harness, machine_branch, mark) = {
            let state = self.state.read(cx);
            let Some(chat) = state.selected_chat_row() else {
                return Empty.into_any_element();
            };
            let title = transcript::single_line(chat.title.as_deref().unwrap_or("New session"));
            let machine_branch =
                machine_and_branch(state.device_name(&chat.device_id), chat.branch.as_deref());
            let mark = super::marks::Mark::of(
                state.display_status_for(chat, now),
                state.session_for(&chat.id),
                now,
            );
            (
                chat.id.clone(),
                SharedString::from(title),
                chat.config.as_ref().map(|config| config.harness),
                machine_branch,
                mark,
            )
        };
        // A title the agent gives rises in over the old (Amp's), on the house's roll.
        let rolling_title = crate::roll_text::rolling(
            format!("titlebar-title-{chat_id}"),
            title.clone(),
            crate::motion::reduced_motion(cx),
            crate::clock::now(cx),
        );
        // An agent's mark in its brand's colour; a plain terminal's the terminal glyph.
        let (glyph, tint) = harness
            .map_or((icons::TERMINAL, None), crate::pickers::harness_brand_icon);
        let title_ink = if key { ink.text } else { ink.text_muted };
        let ground = self.shown_ground(cx);
        let mark = super::marks::mark_element(
            SharedString::from(format!("titlebar-mark-{chat_id}")),
            "titlebar-mark".to_owned(),
            &mark,
            crate::loaders::status_mark::MARK_SMALL,
            ground,
        );
        let menu_chat = chat_id.clone();
        let spoken = title.clone();
        div()
            .id("titlebar-identity")
            .debug_selector(|| "titlebar-identity".into())
            // A screen reader hears the session's title, its machine and branch after, as a
            // button: a press opens the session picker.
            .role(gpui::Role::Button)
            .aria_label(spoken)
            .when_some(machine_branch.clone(), |el, words| el.aria_description(words))
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(13.0))
            .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint.unwrap_or(title_ink)))
            // Out of the drag strip, as the row's buttons are (`window_control_button`).
            .occlude()
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle_picker(super::picker::Mode::Sessions, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.chat_menu.open(super::ChatMenuState::for_row(
                        menu_chat.clone(),
                        gpui::point(event.position.x, px(Theme::TITLEBAR_HEIGHT)),
                    ));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            // A cut title shows whole in a tooltip.
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(title_ink)
                    .child(match rolling_title {
                        Some(rolling) => div()
                            .debug_selector(|| "chat-titlebar-title".into())
                            .min_w_0()
                            .overflow_hidden()
                            .child(rolling)
                            .into_any_element(),
                        None => crate::settings::widgets::truncate_with_tip(
                            "chat-titlebar-title",
                            title,
                        )
                        .into_any_element(),
                    }),
            )
            .children(machine_branch.map(|words| {
                div()
                    .debug_selector(|| "titlebar-machine-branch".into())
                    .flex_none()
                    .max_w(px(240.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(ink.text_muted)
                    .child(SharedString::from(words))
            }))
            .children(mark)
            .into_any_element()
    }

    /// The ground the session on screen stands on: its terminal's program's background (OSC 11,
    /// else the theme's), which the titlebar row above it wears too; the theme's session ground
    /// until its terminal's view says.
    pub(super) fn shown_ground(&self, cx: &App) -> gpui::Hsla {
        self.session_terminal
            .read(cx)
            .shown_ground(cx)
            .unwrap_or_else(|| Theme::of(cx).session_ground())
    }
}

/// The identity's secondary words: the session's machine and its branch, "Studio · fix-lexer",
/// or whichever of the two is known.
pub(super) fn machine_and_branch(machine: Option<&str>, branch: Option<&str>) -> Option<String> {
    let (machine, branch) =
        (machine.filter(|words| !words.is_empty()), branch.filter(|words| !words.is_empty()));
    match (machine, branch) {
        (Some(machine), Some(branch)) => Some(format!("{machine} \u{b7} {branch}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_owned()),
        (None, None) => None,
    }
}

/// The theme inked for words over `ground`, read as the window's appearance is
/// (`window_appearance::of_ground`): the theme's own when the ground is of its appearance, else
/// the other appearance's, so a dark program's titlebar row in a light window
/// keeps its title readable (`docs/decisions/layout.md`, "One row of chrome").
pub(super) fn ink_for(ground: gpui::Hsla, theme: &Theme) -> Theme {
    let appearance = crate::window_appearance::of_ground(ground);
    if appearance == theme.appearance { theme.clone() } else { Theme::for_appearance(appearance) }
}

#[cfg(test)]
mod titlebar_tests {
    use super::*;

    /// The identity's secondary words say the machine and the branch, or whichever is known.
    #[test]
    fn the_identity_says_its_machine_and_branch() {
        assert_eq!(
            machine_and_branch(Some("Studio"), Some("fix-lexer")).as_deref(),
            Some("Studio \u{b7} fix-lexer")
        );
        assert_eq!(machine_and_branch(Some("Studio"), None).as_deref(), Some("Studio"));
        assert_eq!(machine_and_branch(None, Some("main")).as_deref(), Some("main"));
        assert_eq!(machine_and_branch(Some(""), Some("")), None, "nothing known, nothing said");
    }

    /// The row is inked for the ground it stands on, as Ghostty's transparent titlebar picks
    /// its appearance: a dark program's ground in a light window gets the dark ink, a light
    /// one in a dark window the light ink, and a ground of the window's own appearance its own.
    #[test]
    fn the_row_is_inked_for_the_ground_it_stands_on() {
        let (light, dark) = (
            Theme::for_appearance(crate::theme::Appearance::Light),
            Theme::for_appearance(crate::theme::Appearance::Dark),
        );
        // Solarized's two backgrounds: base03 #002b36 and base3 #fdf6e3.
        let (base03, base3) = (gpui::rgb(0x002b36).into(), gpui::rgb(0xfdf6e3).into());
        assert!(ink_for(base03, &light).appearance.is_dark(), "dark ink on a dark program");
        assert!(!ink_for(base3, &dark).appearance.is_dark(), "light ink on a light program");
        assert_eq!(ink_for(base3, &light).text, light.text, "the window's own");
        assert_eq!(ink_for(base03, &dark).text, dark.text, "the window's own");
        assert!(
            crate::theme::contrast_ratio(ink_for(base03, &light).text, base03) >= 4.5,
            "the title reads on the dark ground"
        );
    }
}

#[cfg(test)]
mod cycle_tests {
    use super::*;

    fn order(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn steps_forward_and_back_through_the_list() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("a"), true).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), true).as_deref(), Some("c"));
        assert_eq!(cycle_target(&list, Some("c"), false).as_deref(), Some("b"));
        assert_eq!(cycle_target(&list, Some("b"), false).as_deref(), Some("a"));
    }

    #[test]
    fn wraps_at_both_ends() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, Some("c"), true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, Some("a"), false).as_deref(), Some("c"));
    }

    #[test]
    fn a_single_session_cycles_to_itself() {
        // Not a no-op by accident: with one row both directions must resolve,
        // so the shortcut never looks broken by dead-ending on `None`.
        let list = order(&["only"]);
        assert_eq!(
            cycle_target(&list, Some("only"), true).as_deref(),
            Some("only")
        );
        assert_eq!(
            cycle_target(&list, Some("only"), false).as_deref(),
            Some("only")
        );
    }

    #[test]
    fn no_selection_enters_the_list_from_the_matching_end() {
        let list = order(&["a", "b", "c"]);
        assert_eq!(cycle_target(&list, None, true).as_deref(), Some("a"));
        assert_eq!(cycle_target(&list, None, false).as_deref(), Some("c"));
        assert_eq!(
            cycle_target(&list, Some("gone"), true).as_deref(),
            Some("a")
        );
        assert_eq!(
            cycle_target(&list, Some("gone"), false).as_deref(),
            Some("c")
        );
    }

    #[test]
    fn an_empty_list_has_nothing_to_select() {
        assert_eq!(cycle_target(&[], None, true), None);
        assert_eq!(cycle_target(&[], Some("a"), true), None);
    }

    // Cycling walks the rows the sidebar is drawing, not every chat: that
    // guarantee is structural now — `cycle_session` reads the same
    // `AppState::sidebar_chats` the sidebar and the jump shortcuts read, and
    // `jump_slots_count_the_rows_the_sidebar_draws` (state.rs) covers the
    // space-filter behaviour for all of them.
}
