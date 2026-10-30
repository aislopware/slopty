// Derived from zeron (MIT, github.com/zeronsh/zeron @ c02739d), see THIRD_PARTY_NOTICES.md
//! Session navigation — the horizontal tab strip is gone (wing 2026-08-10):
//! the activity sidebar IS the session list, and the titlebar names the
//! selected session (harness brand icon + title). A `+` new-session button
//! lives in the titlebar's left control cluster while an existing session is
//! selected. `UiSettings.open_tabs` is legacy — no longer read or written.

use super::*;

const RIGHT_PANE_HEADER_CONTROLS_MIN_WIDTH: f32 = 8.0 + 4.0 + 4.0 + 28.0;

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

pub(super) fn right_pane_expand_icon(expanded: bool) -> &'static str {
    if expanded {
        icons::COLLAPSE_ARROWS
    } else {
        icons::EXPAND_ARROWS
    }
}

struct PanelTitlebarWidths {
    surface_reveal: f32,
    files_controls: f32,
}

pub(super) struct TitlebarTrailing {
    fit: HorizontalPanelFit,
    right_pane_visible: bool,
    takeover: bool,
    row_left: f32,
    row_gap: f32,
    right_pad: f32,
    widths: PanelTitlebarWidths,
    content_width: f32,
}

/// The two fixed right-edge anchors: the explorer toggle and the pane toggle
/// (28px each) with the same 4px gap the surface strip keeps between its
/// controls, so the two never render as one fused block.
const PANEL_TOGGLE_GAP: f32 = 4.0;
const PANEL_TOGGLE_SLOTS: f32 = 28.0 * 2.0 + PANEL_TOGGLE_GAP;

fn panel_titlebar_widths(
    surfaces_visible: f32,
    files_visible: f32,
    available: f32,
    right_pad: f32,
) -> PanelTitlebarWidths {
    // Caption controls occupy the far-right panel first. Subtract their
    // clearance once across the combined header, then split it at Files.
    // The explorer and pane toggles keep their slots even when closed.
    let files_controls = (files_visible - right_pad).max(PANEL_TOGGLE_SLOTS);
    let surfaces = surfaces_visible + files_visible - right_pad - files_controls;
    PanelTitlebarWidths {
        surface_reveal: surfaces.min(available - files_controls).max(0.0),
        files_controls,
    }
}

impl Shell {
    /// The same binding navigates the focused pane. Keep the persisted action
    /// IDs so existing user keymaps and the native browser bridge still work.
    pub(super) fn cycle_navigation(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.navigation_overlay_open() {
            return;
        }
        // slopty: no pull-request pages.
        if matches!(self.route, Route::Chat)
            && self.right_pane_open(cx)
            && self.navigation_focus.in_right(window, cx)
        {
            let rows = self.right_surface_rows(cx);
            if rows.len() <= 1 {
                return;
            }
            let active = self.resolved_right_active(cx);
            let at = rows.iter().position(|(surface, ..)| *surface == active);
            let next = match (at, forward) {
                (Some(at), true) => (at + 1) % rows.len(),
                (Some(at), false) => (at + rows.len() - 1) % rows.len(),
                (None, true) => 0,
                (None, false) => rows.len() - 1,
            };
            self.activate_right_surface(rows[next].0, window, cx);
        } else {
            self.cycle_session(forward, cx);
        }
    }

    fn navigation_overlay_open(&self) -> bool {
        self.overlay_owns_keyboard() // slopty: no accounts, sync or sign-in.
            || self.delete_space_confirm.is_some()
            || self.chat_rename.is_some()
            || self.rename_space_dialog.is_some()
            // slopty: no Discard changes (ruled out).
            || self.chat_menu.get().is_some()
            // slopty: no pull-request pages.
            || self.space_menu.get().is_some()
            || self.user_menu.get().is_some()
            || self.spaces_menu.get().is_some()
            || self.right_plus.get().is_some()
            || !self.pending_file_closes.is_empty()
            || self.pending_exit.is_some()
    }

    pub(super) fn activate_right_surface(
        &mut self,
        surface: RightSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigation_focus.right_was_focused = true;
        // Establish a stable target before detaching the old editor. Read-only
        // surfaces keep it; editable surfaces claim their own focus below/on mount.
        window.focus(&self.navigation_focus.right, cx);
        self.set_right_active(surface, cx);
        self.focus_right_file_editor(surface, window, cx);
        // slopty: a diff picked by its tab takes the keyboard, as a file's editor does.
        if let RightSurface::Diff(id) = surface {
            self.focus_diff(id, cx);
        }
    }

    pub(super) fn restore_right_focus_after_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.right_pane_open(cx) {
            self.activate_right_surface(self.resolved_right_active(cx), window, cx);
        } else {
            self.navigation_focus.right_was_focused = false;
            self.focus_session_terminal(window, cx); // slopty: the session
        }
    }

    /// slopty: navigation gives the session's terminal the keyboard once it renders.
    pub(super) fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.session_terminal.update(cx, |panel, cx| panel.request_focus(cx));
    }

    /// slopty: the session's terminal takes the keyboard now (the dock closing, the pane
    /// closing), and keeps it once its view renders.
    pub(super) fn focus_session_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.session_terminal.read(cx).focus_handle(), cx);
        self.focus_composer(cx);
    }

    /// slopty: landing on a session (opening it, the boot's landing, back and forward) gives
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
    // slopty: once, at the first synced frame, chat or no chat: a chat arriving later (from the
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
            self.focus_composer_on_landing(cx); // slopty: not on touch
            self.state
                .update(cx, |s, cx| s.select_chat(Some(first), cx));
        }
    }

    /// Open a session from the sidebar: select it, the main area follows.
    pub(crate) fn open_chat(&mut self, chat_id: String, cx: &mut Context<Self>) {
        self.command_palette = None;
        // slopty: voice removed.
        self.set_route(Route::Chat, cx);
        self.focus_composer_on_landing(cx); // slopty: not on touch
        self.nav.push(NavEntry::Chat(chat_id.clone()));
        self.state
            .update(cx, |s, cx| s.select_chat(Some(chat_id), cx));
        cx.notify();
    }

    /// slopty: `+` in the titlebar, the sidebar's and ⌘N: New session…, which starts an agent's
    /// TUI or a terminal in a project at once (`new_session`). `project` (a project's own `+`)
    /// is offered first. The open session stays until the new one opens.
    pub(super) fn open_new_session(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        self.set_route(Route::Chat, cx);
        self.open_new_session_picker(project, cx);
    }

    /// The trailing titlebar strip follows the columns beneath it.
    pub(super) fn titlebar_trailing_layout(&self, on_canvas: bool, cx: &App) -> TitlebarTrailing {
        let sidebar_now = self.sidebar_now();

        // Same glide as the old strip: content starts at the inset card's
        // left edge while the sidebar is open, and slides toward the control
        // cluster as it collapses.
        let content_left =
            (sidebar_now + Theme::SPACE_LG).max(self.title_bar_content_start());

        // Trailing titlebar section. With the changes pane open this is the
        // PANE'S HEADER — a strip exactly as wide as the pane carrying its
        // controls (scope dropdown, ref selector, fold-all from the Changes
        // entity; expand + close shell-side). It lives up here because the
        // titlebar overlay owns this band's hit-testing: controls mounted in
        // the pane itself would sit under the drag region and never see a
        // click. Closed, it is just the stable open/close toggle. Hidden on
        // the new-session canvas (user request) — nothing to diff yet.
        let fit = self.horizontal_fit();
        let right_pane_visible = !on_canvas
            && ((fit.right && (self.right_pane_open(cx) || self.tween_active(self.right_tween)))
                || self.fit_exit_width(1).is_some());
        let takeover = right_pane_visible
            && (self.right_pane_expanded || self.tween_active(self.main_takeover_tween));
        // In takeover the title hides and the strip owns the whole band, so
        // the row's left inset pulls back to the sidebar seam — the title
        // inset would push the scope dropdown off the pane's own left gutter
        // (user report: misaligned dead space). With the sidebar COLLAPSED
        // the seam is the window edge, where the traffic lights + nav
        // cluster overlay lives — the strip must still clear it, but only
        // just: `title_bar_content_start` carries the identity-group margin the
        // strip doesn't want (it brings its own 8px pad), and doubling up
        // read as a hole after the `+` (user report).
        let row_left = if takeover {
            // The surface tabs must LEFT-ALIGN with the pane's own rows (the
            // diff options and stats strip carry an 8px box gutter off the
            // seam — user report: rows started at different insets). The
            // strip's width is capped to `avail`, which subtracts the row's
            // 8px child gap — pulling row_left 8 LEFT of the seam cancels
            // that, so the uncapped strip starts exactly at the seam and its
            // own 8px pad lands the first chip on the pane gutter. The
            // window-control cluster still wins while the sidebar is
            // collapsed (the chips clear it instead of underlapping).
            let cluster_end =
                self.title_bar_content_start() - TITLEBAR_IDENTITY_GAP - 14.0;
            (sidebar_now - 8.0).max(cluster_end)
        } else {
            content_left
        };
        let row_gap = 8.0;
        let files_width = self.files_visible_width(cx);
        // slopty: room at the edge for the outermost control's press on a touch screen (#23).
        let right_pad = self
            .titlebar_right_pad(TITLEBAR_ACTION_EDGE_INSET)
            .max(crate::surface_chrome::touch_reach(28.0));
        // The title row's gaps are outside the fixed-width panel controls.
        let gap_budget = if takeover { row_gap } else { row_gap * 3.0 };
        // Match the surface's visible width, including its resize bounce,
        // so the header follows the pane seam as the sidebar moves.
        let right_visible = self.right_visible_width(cx);
        let widths = panel_titlebar_widths(
            right_visible,
            files_width,
            self.viewport_width - row_left - right_pad - gap_budget,
            right_pad,
        );
        // Match the pane's stable inner width during open/close. Reflowing
        // the scroller on every masked frame clamps its scroll offset and
        // makes the tabs jump when the close is reversed.
        let content_width = panel_titlebar_widths(
            self.right_content_width(self.right_target(cx)),
            files_width,
            self.viewport_width - row_left - right_pad - gap_budget,
            right_pad,
        )
        .surface_reveal
        .max(widths.surface_reveal);
        TitlebarTrailing {
            fit,
            right_pane_visible,
            takeover,
            row_left,
            row_gap,
            right_pad,
            widths,
            content_width,
        }
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
        let TitlebarTrailing {
            fit,
            right_pane_visible,
            takeover,
            row_left,
            row_gap,
            right_pad,
            widths,
            content_width,
        } = self.titlebar_trailing_layout(on_canvas, cx);
        // Inked for the ground under it: the terminal's over the session, the window's own over
        // the canvas.
        let ink = if on_canvas { theme.clone() } else { ink_for(self.shown_ground(cx), &theme) };
        let identity = (!takeover && !on_canvas)
            .then(|| self.render_titlebar_identity(&ink, window.is_window_active(), cx));

        let trailing: Option<gpui::AnyElement> = if on_canvas {
            None
        } else {
            let mut controls = div()
                .id("right-titlebar-controls")
                .debug_selector(|| "right-titlebar-controls".into())
                .flex_none()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                // The transcript extends under this band (it fades beneath
                // the tab strip); a wheel over the tabs must scroll the
                // strip, never the surface behind it.
                .on_scroll(|_, _, cx| cx.stop_propagation());
            if right_pane_visible {
                // The right pane's SURFACE TABS (t3 RightPanelTabs) — the diff
                // options that used to live here moved into the pane's own
                // second row; expand stays in this band (user request).
                let show_pane_controls =
                    widths.surface_reveal >= RIGHT_PANE_HEADER_CONTROLS_MIN_WIDTH;
                // slopty: the expand button's name, said and shown.
                let expand_label = if self.right_pane_expanded {
                    "Restore side pane"
                } else {
                    "Expand side pane"
                };
                // The toggle is the fixed right-edge anchor, like the left
                // sidebar control. Only the tabs + expand section reveals to
                // its left; including the toggle in this animated width
                // compressed both icons into the same clipped box at open.
                controls = controls.child(
                    div()
                        .w(px(widths.surface_reveal))
                        .h_full()
                        .flex_none()
                        .relative()
                        .overflow_hidden()
                        .child(
                            div()
                                .debug_selector(|| "right-titlebar-content".into())
                                .absolute()
                                .right_0()
                                .top_0()
                                .w(px(content_width))
                                .h_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(4.0))
                                .overflow_hidden()
                                // 8 + the trigger's own 8px pad = the pane's 16px
                                // text gutter. The 4px right padding is the stable
                                // gap before the fixed toggle.
                                .when(show_pane_controls, |el| {
                                    el.pl(px(8.0))
                                        .pr(px(4.0))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .h_full()
                                                .overflow_hidden()
                                                .child(self.render_right_tab_strip(cx)),
                                        )
                                        .child(
                                            header_icon_button(
                                                "expand-changes",
                                                right_pane_expand_icon(self.right_pane_expanded),
                                                false,
                                                &theme,
                                                cx.listener(|this, _, _, cx| {
                                                    this.toggle_right_pane_expand(cx)
                                                }),
                                            )
                                            // slopty: named, with its key, for the eye and a
                                            // screen reader.
                                            .action_hint(
                                                expand_label,
                                                Keys::Action(Box::new(
                                                    chrome_keys::ToggleExpandPane,
                                                )),
                                                cx,
                                            ),
                                        )
                                }),
                        ),
                );
            }
            let files_panel_label = if self.files_panel_open(cx) && fit.files {
                "Hide files panel"
            } else {
                "Show files panel"
            };
            // The explorer slot sits over the explorer column and carries the
            // two fixed right-edge anchors — the explorer toggle and,
            // outermost, the pane toggle — which stay mounted at one position
            // while the surface tabs reveal to their left. The explorer's own
            // search and visibility controls live in its secondary header.
            Some(
                controls
                    .child(
                        div()
                            .w(px(widths.files_controls))
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(PANEL_TOGGLE_GAP))
                            // No hairline here: the explorer column below is
                            // padded down by the titlebar height and its left
                            // border already runs through this band, so a
                            // second one on the slot stacked on the same
                            // pixels and read lighter than the seam beneath
                            // it (user report).
                            .child(
                                header_icon_button(
                                    "toggle-files-panel",
                                    icons::FOLDER_TREE,
                                    self.files_panel_open(cx) && fit.files,
                                    &theme,
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_files_panel(window, cx)
                                    }),
                                )
                                .action_hint(files_panel_label, Keys::Action(Box::new(ToggleFiles)), cx),
                            )
                            .child(header_icon_button_with(
                                "toggle-changes",
                                icons::sidebar_glyph(
                                    motion::state_t(
                                        "toggle-changes",
                                        self.right_pane_open(cx) && fit.right,
                                        motion::GLYPH_STATE,
                                        self.reduced_motion.get(),
                                        cx.frame_time(), // slopty: the frame's moment
                                    ),
                                    true,
                                    16.0,
                                    theme.text_muted,
                                ),
                                false,
                                &theme,
                                cx.listener(|this, _, _, cx| this.toggle_right_pane(cx)),
                            )
                            // slopty: what it does now, and its key.
                            .action_hint(
                                if self.right_pane_open(cx) && fit.right {
                                    "Hide side pane"
                                } else {
                                    "Show side pane"
                                },
                                Keys::Action(Box::new(ToggleChanges)),
                                cx,
                            )),
                    )
                    .into_any_element(),
            )
        };

        // slopty: the inspector's toggle, and the chat's folder in the person's editor
        // (`open_in`), until the summoned views take their place. A plain terminal has nothing
        // to inspect.
        let running_lanes = {
            let state = self.state.read(cx);
            let now = crate::clock::utc(cx);
            state.selected_chat.as_deref().map_or(0, |chat| state.running_subagents_for(chat, now))
        };
        let inspects = crate::inspector::inspects(self.state.read(cx), &self.active_chat);
        let session_info_toggle = (!takeover && !on_canvas && inspects).then(|| {
            let open = self.inspector_shown(cx);
            let label = if open { "Hide inspector" } else { "Show inspector" };
            header_icon_button_with(
                "toggle-inspector",
                icons::corner_card_glyph(16.0, ink.text_muted),
                open,
                &ink,
                cx.listener(|this, _, window, cx| this.toggle_inspector(window, cx)),
            )
            .action_hint(label, Keys::Action(Box::new(chrome_keys::ToggleInspector)), cx)
            .flex_none()
            // The session's running subagents: the button counts them, open or closed, for as
            // long as they run.
            .when(running_lanes > 0, |button| {
                crate::running_pill::mark_titlebar_button(button, running_lanes, &ink)
            })
        });
        let open_in = (!takeover && !on_canvas).then(|| self.render_open_in_button(cx)).flatten();
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
            .pt(px(super::titlebar_top_pad())) // slopty: a finger's presses inside the window
            .gap(px(row_gap))
            .pl(px(row_left))
            .pr(px(right_pad))
            .children(identity)
            .child(div().flex_1())
            .children(open_in)
            .children(session_info_toggle)
            .child(actions)
            .children(trailing);
        let bar = div().h(px(Theme::TITLEBAR_HEIGHT)).flex_none().child(inner);
        self.titlebar_drag_region("chat-titlebar", bar, cx)
            .into_any_element()
    }

    /// The session's identity in the titlebar row: its program's icon, its title (whole in a
    /// tooltip when cut, rising in over the old when the agent gives another), its machine and
    /// branch in secondary text, then its state's mark, the same glyph as its row's in the
    /// list. `ink` is the theme inked for the ground under the row; the title dims to secondary
    /// while the window is not `key`. A right-click opens the session's menu under the row.
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
        // slopty: a title the agent gives rises in over the old (Amp's), on the house's roll.
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
        div()
            .id("titlebar-identity")
            .debug_selector(|| "titlebar-identity".into())
            .min_w_0()
            .overflow_hidden()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(13.0))
            .child(icon(glyph).size(px(14.0)).flex_none().text_color(tint.unwrap_or(title_ink)))
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
            // slopty: a cut title shows whole in a tooltip.
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
fn machine_and_branch(machine: Option<&str>, branch: Option<&str>) -> Option<String> {
    let (machine, branch) =
        (machine.filter(|words| !words.is_empty()), branch.filter(|words| !words.is_empty()));
    match (machine, branch) {
        (Some(machine), Some(branch)) => Some(format!("{machine} \u{b7} {branch}")),
        (Some(one), None) | (None, Some(one)) => Some(one.to_owned()),
        (None, None) => None,
    }
}

/// Whether text over `ground` reads as on a light one, as Ghostty picks a transparent
/// titlebar's appearance from the terminal's background: its luma past half.
fn ground_is_light(ground: gpui::Hsla) -> bool {
    let rgb = ground.to_rgb();
    0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b > 0.5
}

/// The theme inked for words over `ground`: the window's own when the ground is of its
/// appearance, else the other appearance's, so a dark program's titlebar row in a light window
/// keeps its title readable (`docs/decisions/layout.md`, "One row of chrome").
pub(super) fn ink_for(ground: gpui::Hsla, theme: &Theme) -> Theme {
    let light = ground_is_light(ground);
    if light == !theme.appearance.is_dark() {
        theme.clone()
    } else {
        let appearance = if light {
            crate::theme::Appearance::Light
        } else {
            crate::theme::Appearance::Dark
        };
        Theme::for_appearance(appearance)
    }
}

#[cfg(test)]
mod panel_titlebar_tests {
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

    #[test]
    fn tabs_align_with_the_panel_for_each_caption_layout_and_files_width() {
        let viewport = 1400.0;
        for right_pad in [6.0, 40.0, 92.0, 114.0] {
            for files in [0.0, 10.0, 28.0, 100.0, 220.0, 286.0, 440.0] {
                let widths = panel_titlebar_widths(520.0, files, 1100.0, right_pad);
                let controls_left =
                    viewport - right_pad - widths.files_controls - widths.surface_reveal;
                assert_eq!(
                    controls_left,
                    viewport - files - 520.0,
                    "caption clearance {right_pad}, Files width {files}"
                );
                assert!(widths.files_controls >= PANEL_TOGGLE_SLOTS);
                if files >= right_pad + PANEL_TOGGLE_SLOTS {
                    assert_eq!(
                        viewport - right_pad - widths.files_controls,
                        viewport - files
                    );
                }
            }
        }
    }

    #[test]
    fn expanded_tabs_clear_the_left_controls_without_reserving_captions_twice() {
        for right_pad in [6.0, 92.0, 114.0] {
            let viewport = 1400.0;
            let files = 286.0;
            // With the sidebar open, the header starts exactly at its seam.
            // With it collapsed, leave room for the window/nav controls.
            for (sidebar, row_left) in [(256.0, 248.0), (0.0, 180.0)] {
                let widths = panel_titlebar_widths(
                    viewport - sidebar - files,
                    files,
                    viewport - row_left - right_pad - 8.0,
                    right_pad,
                );
                let controls_left =
                    viewport - right_pad - widths.files_controls - widths.surface_reveal;
                assert_eq!(controls_left, sidebar.max(row_left + 8.0));
            }
        }
    }

    #[test]
    fn narrow_panels_and_tight_headers_keep_nonnegative_reveal_widths() {
        // A narrow surface and Files share a 520px header.
        let widths = panel_titlebar_widths(234.0, 286.0, 600.0, 92.0);
        assert_eq!(
            1000.0 - 92.0 - widths.files_controls - widths.surface_reveal,
            480.0
        );
        for available in [-20.0, 0.0, 28.0, 56.0, 100.0] {
            let widths = panel_titlebar_widths(0.0, 0.0, available, 114.0);
            assert_eq!(widths.surface_reveal, 0.0);
            assert_eq!(widths.files_controls, PANEL_TOGGLE_SLOTS);
        }
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
