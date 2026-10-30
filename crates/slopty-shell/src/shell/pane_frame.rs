//! A session's view as panes, on the shell's side (`docs/decisions/layout.md`, "One session per
//! view, splits on demand" and "No permanent right column"): the tree each view holds
//! ([`super::panes`]), the shells opened beside a session, the keys that split, zoom, walk and
//! close, the views summoned beside the terminal, and each pane drawn with its header.

use gpui::{Hsla, size};

use super::pane_area::PaneArea;
use super::panes::{Direction, Grid, Laid, Pane, Panes};
use super::*;

/// A pane's header while its view holds more than one pane: the program's icon, its title, and
/// where it runs, with zoom and close on the focused or hovered pane.
const PANE_HEADER: f32 = 28.0;

/// A pane header's height: [`PANE_HEADER`], and on a touch screen a finger's row, so its
/// controls' presses stay inside it.
fn pane_header_height() -> f32 {
    crate::surface_chrome::touch_row_height(PANE_HEADER)
}

/// The narrowest window that splits: an iPad 1024 pt or wider; a Mac always, an iPhone never.
const SPLITS_FROM: f32 = 1024.0;

/// What a summoned view takes of the session's room when it docks beside it, at most and at
/// least: the terminal keeps the larger part.
const VIEW_SHARE: (f32, f32) = (0.25, 0.5);

/// The divider's colour over `ground`: the terminal's background darkened 8 % on a light one and
/// 40 % on a dark one, as Ghostty derives it, so the line never fights the program's theme.
pub(super) fn divider_over(ground: Hsla) -> Hsla {
    let by = if crate::window_appearance::of_ground(ground).is_light() { 0.08 } else { 0.4 };
    Hsla { l: ground.l * (1.0 - by), ..ground }
}

impl Shell {
    /// Whether this window splits ([`SPLITS_FROM`]).
    pub(super) fn splits_here(&self) -> bool {
        !touch_screen() || self.viewport_width >= SPLITS_FROM
    }

    /// The view's panes as they stand.
    pub(super) fn panes_now(&self, cx: &App) -> Panes {
        self.panes.get(&self.panel_key(cx)).cloned().unwrap_or_default()
    }

    pub(super) fn panes_mut(&mut self, cx: &App) -> &mut Panes {
        let key = self.panel_key(cx);
        self.panes.entry(key).or_default()
    }

    /// The view laid out where it was drawn last.
    pub(super) fn laid_panes(&self, cx: &App) -> Laid {
        let at = self.pane_area_at.get().unwrap_or_default();
        self.panes_now(cx).layout(at, self.pane_grid)
    }

    /// Open a shell beside the focused pane, toward `toward` or where there is most room
    /// (⌘D, ⇧⌘D, ⌘⏎), on the session's machine and in its folder, and give it the keyboard.
    /// Over the new-session canvas it opens beside the canvas, on the project's machine, so a
    /// shell needs no session first.
    pub(super) fn split_pane(&mut self, toward: Option<Direction>, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Chat) || !self.splits_here() {
            return;
        }
        let toward = toward.unwrap_or_else(|| {
            let laid = self.laid_panes(cx);
            self.panes_now(cx).roomiest(&laid)
        });
        let shell = cx.new(|cx| TerminalPanel::new_pane(self.state.clone(), cx));
        self.add_pane_shell(shell, toward, cx);
    }

    /// Put `shell` in a pane beside the focused one, toward `toward`, with the keyboard once it
    /// draws.
    pub(super) fn add_pane_shell(
        &mut self,
        shell: Entity<TerminalPanel>,
        toward: Direction,
        cx: &mut Context<Self>,
    ) {
        self.pane_seq += 1;
        let pane = Pane::Shell(self.pane_seq);
        self.pane_shells.insert(self.pane_seq, shell);
        self.panes_mut(cx).split(toward, pane);
        self.pane_keyboard_pending = true;
        cx.notify();
    }

    /// A pane for `shell`, beside the focused pane of the view of `key` (a session's panes), where
    /// there is most room on screen, the keyboard staying where it is: a project action's run,
    /// or an agent's shell. Its number.
    pub(super) fn add_shell_beside(
        &mut self,
        key: &str,
        shell: Entity<TerminalPanel>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let toward = if key == self.panel_key(cx) {
            let laid = self.laid_panes(cx);
            self.panes_now(cx).roomiest(&laid)
        } else {
            Direction::Right
        };
        self.pane_seq += 1;
        self.pane_shells.insert(self.pane_seq, shell);
        let panes = self.panes.entry(key.to_owned()).or_default();
        let had = panes.focused();
        panes.split(toward, Pane::Shell(self.pane_seq));
        panes.focus(had);
        cx.notify();
        self.pane_seq
    }

    /// Summon `view` by its key: docked at the right of the session, or focused where it is, or,
    /// when it has the keyboard already, the keyboard back to the session's terminal.
    pub(super) fn summon_view(&mut self, view: RightSurface, cx: &mut Context<Self>) {
        if self.active_chat.is_empty() {
            return;
        }
        let share = self.view_share();
        let panes = self.panes_mut(cx);
        panes.summon(Pane::View(view), share);
        let focused = panes.focused();
        if focused == Pane::View(view) {
            self.wake_view(view, cx);
        }
        // The keyboard follows once the pane draws: one opened now is not laid out yet.
        self.pane_keyboard_pending = true;
        cx.notify();
    }

    /// What a summoned view takes of the session's room: the person's last width for a side
    /// view, as a share of the view's room, within [`VIEW_SHARE`].
    pub(super) fn view_share(&self) -> f32 {
        let room =
            self.pane_area_at.get().map_or(self.viewport_width, |at| f32::from(at.size.width));
        (self.settings.right_pane_width / room.max(1.0)).clamp(VIEW_SHARE.0, VIEW_SHARE.1)
    }

    /// Give `pane` the keyboard: it becomes the view's focused pane, and the keyboard lands in it.
    pub(super) fn focus_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        // A pane a zoom brings on screen is not laid out yet: the keyboard follows as it draws.
        if self.panes_mut(cx).focus(pane) {
            self.pane_keyboard_pending = true;
            cx.notify();
        }
    }

    /// The keyboard into `pane`, laid out: the session's terminal, a shell's, or the view's own
    /// ([`Self::focus_view`]).
    fn put_keyboard_in(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        match pane {
            Pane::Session => self.focus_session_terminal(window, cx),
            Pane::Shell(n) => {
                if let Some(shell) = self.pane_shells.get(&n).cloned() {
                    window.focus(&shell.read(cx).focus_handle(), cx);
                }
            }
            Pane::View(view) => self.focus_view(view, window, cx),
        }
    }

    /// Whether the keyboard is in `pane`.
    pub(super) fn pane_holds_keyboard(&self, pane: Pane, window: &Window, cx: &App) -> bool {
        match pane {
            Pane::Session => {
                self.session_terminal.read(cx).focus_handle().contains_focused(window, cx)
            }
            Pane::Shell(n) => self
                .pane_shells
                .get(&n)
                .is_some_and(|shell| shell.read(cx).focus_handle().contains_focused(window, cx)),
            Pane::View(view) => self
                .view_handles
                .get(&view)
                .is_some_and(|handle| handle.contains_focused(window, cx)),
        }
    }

    /// Move the keyboard to the pane beside the focused one toward `toward` (⌥⌘ arrows).
    pub(super) fn focus_pane_toward(&mut self, toward: Direction, cx: &mut Context<Self>) {
        let laid = self.laid_panes(cx);
        if let Some(pane) = self.panes_now(cx).neighbour(&laid, toward) {
            self.focus_pane(pane, cx);
        }
    }

    /// Move the keyboard to the next or previous pane in reading order (⌘] ⌘[).
    pub(super) fn cycle_pane(&mut self, forward: bool, cx: &mut Context<Self>) {
        let panes = self.panes_now(cx);
        if panes.len() > 1 {
            self.focus_pane(panes.cycle(forward), cx);
        }
    }

    /// Fill the view with the focused pane, or put the view back (⇧⌘⏎).
    pub(super) fn toggle_pane_zoom(&mut self, cx: &mut Context<Self>) {
        self.panes_mut(cx).toggle_zoom();
        cx.notify();
    }

    /// Close `pane`: a shell's terminal closes with it, a view as [`Self::close_view`] closes
    /// it; its sibling takes its room and the keyboard. The session's own pane does not close.
    pub(super) fn close_pane(&mut self, pane: Pane, cx: &mut Context<Self>) -> bool {
        if !self.panes_now(cx).contains(pane) {
            return false;
        }
        match pane {
            Pane::Shell(n) => {
                self.panes_mut(cx).close(pane);
                if let Some(shell) = self.pane_shells.remove(&n) {
                    shell.update(cx, |shell, cx| shell.close_shown(cx));
                }
                self.pane_keyboard_pending = true;
                cx.notify();
            }
            Pane::View(view) => self.close_view(view, cx),
            Pane::Session => return false,
        }
        true
    }

    /// ⌘W: close the focused pane, when it is not the session's own. Whether it closed one.
    pub(crate) fn close_focused_pane(&mut self, cx: &mut Context<Self>) -> bool {
        let focused = self.panes_now(cx).focused();
        focused != Pane::Session && self.close_pane(focused, cx)
    }

    /// The pane under a press becomes the focused one, the press itself going on to what it
    /// landed on; once it has, the keyboard follows into the pane unless what the press landed
    /// on took it (a header, a view's margin).
    fn note_pane_press(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let panes = self.panes_mut(cx);
        if panes.focused() != pane && panes.focus(pane) {
            cx.notify();
            cx.defer_in(window, move |this, window, cx| {
                if this.panes_now(cx).focused() == pane
                    && !this.pane_holds_keyboard(pane, window, cx)
                {
                    this.put_keyboard_in(pane, window, cx);
                }
            });
        }
    }

    /// Whether `pane`'s program needs the person, so an unfocused pane of it wears the ring.
    fn pane_needs_you(&self, pane: Pane, cx: &App) -> Option<Hsla> {
        if pane != Pane::Session {
            return None;
        }
        let state = self.state.read(cx);
        let chat = state.selected_chat_row()?;
        let now = crate::clock::utc(cx);
        let mark =
            marks::Mark::of(state.display_status_for(chat, now), state.session_for(&chat.id), now);
        matches!(mark, marks::Mark::NeedsYou(_)).then(|| mark.hue(Theme::of(cx))).flatten()
    }

    /// The keyboard follows a click or a Tab into another pane; a change the shell made (a
    /// switch, a summon, a close) hands it to the pane focused, once this frame lays it out. On
    /// a phone the keyboard never moves the views: a tap on the terminal over the inspector's
    /// sheet leaves the sheet up.
    pub(super) fn settle_pane_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_keyboard_pending {
            self.pane_keyboard_pending = false;
            let focused = self.panes_now(cx).focused();
            cx.defer_in(window, move |this, window, cx| this.put_keyboard_in(focused, window, cx));
        } else if !self.compact()
            && let Some(holder) = self
                .panes_now(cx)
                .panes()
                .into_iter()
                .find(|pane| self.pane_holds_keyboard(*pane, window, cx))
        {
            self.panes_mut(cx).focus(holder);
        }
    }

    /// The view's panes, drawn: each at its rectangle, with a header while there is more than
    /// one, the terminals without the keyboard dimmed, and the dividers between them.
    pub(super) fn render_panes(
        &mut self,
        session: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.settle_pane_keyboard(window, cx);
        // A phone shows the session alone: its views are pages and a sheet over it (`compact`),
        // and nothing glides beside it.
        if self.compact() {
            self.session_terminal.update(cx, |panel, cx| panel.hold_fit(gpui::Size::default(), cx));
            return session;
        }
        let panes = self.panes_now(cx);
        let many = panes.len() > 1 && panes.zoomed().is_none();
        let focused = panes.focused();
        let header = if many { pane_header_height() } else { 0.0 };
        self.pane_grid = crate::live::grid(window, cx).map(|(cell, padding)| Grid {
            cell,
            frame: size(padding.width, padding.height + px(header)),
        });
        let ground = self.shown_ground(cx);
        let shell = cx.weak_entity();
        let equalize = cx.weak_entity();
        let mut area =
            PaneArea::new("panes", panes.clone(), self.pane_grid, self.pane_area_at.clone())
                .divider(divider_over(ground))
                .on_ratio(move |path, ratio, _, cx| {
                    let _gone = shell.update(cx, |this, cx| {
                        this.panes_mut(cx).set_ratio(path, ratio);
                        cx.notify();
                    });
                })
                .on_equalize(move |_, cx| {
                    let _gone = equalize.update(cx, |this, cx| {
                        this.panes_mut(cx).equalize();
                        cx.notify();
                    });
                });
        // A glide of the sidebar beside the panes reflows each terminal in them once
        // (`glide_hold`).
        let (session_terminal, pane_shells) =
            (self.session_terminal.clone(), self.pane_shells.clone());
        area = area.glide(self.glide_hold(), move |pane, by, cx| {
            let terminal = match pane {
                Pane::Session => Some(session_terminal.clone()),
                Pane::Shell(n) => pane_shells.get(&n).cloned(),
                Pane::View(_) => None,
            };
            if let Some(terminal) = terminal {
                terminal.update(cx, |panel, cx| panel.hold_fit(size(by, Pixels::ZERO), cx));
            }
        });
        let mut session = Some(session);
        for pane in panes.panes() {
            let dimmed = many && pane != focused;
            let content = match pane {
                Pane::Session => {
                    self.session_terminal.update(cx, |panel, cx| panel.set_dimmed(dimmed, cx));
                    session.take().unwrap_or_else(|| Empty.into_any_element())
                }
                Pane::Shell(n) => match self.pane_shells.get(&n).cloned() {
                    Some(shell) => {
                        shell.update(cx, |panel, cx| panel.set_dimmed(dimmed, cx));
                        shell.into_any_element()
                    }
                    None => Empty.into_any_element(),
                },
                Pane::View(view) => self.view_pane(view, window, cx),
            };
            if dimmed && let Some(hue) = self.pane_needs_you(pane, cx) {
                area = area.ring(pane, hue);
            }
            let header = many.then(|| self.render_pane_header(pane, pane == focused, ground, cx));
            // A screen reader hears each pane as a group named by what it shows.
            let (_, _, title, _) = self.pane_identity(pane, cx);
            area = area.pane(
                pane,
                div()
                    .id(SharedString::from(format!("pane-{}", pane_name(pane))))
                    .debug_selector(move || format!("pane-{}", pane_name(pane)))
                    .role(gpui::Role::Group)
                    .aria_label(title)
                    .size_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                        this.note_pane_press(pane, window, cx);
                    }))
                    .children(header)
                    .child(div().flex_1().min_h_0().child(content)),
            );
        }
        area.into_any_element()
    }

    /// A pane's header: its program's icon, its title, and where it runs, quiet; zoom and close
    /// on the focused pane and the one under the pointer.
    fn render_pane_header(
        &mut self,
        pane: Pane,
        focused: bool,
        ground: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ink = tabs::ink_for(ground, Theme::of(cx));
        let (glyph, tint, title, place) = self.pane_identity(pane, cx);
        let name = pane_name(pane);
        let group = SharedString::from(format!("pane-header-{name}"));
        let zoomed = self.panes_now(cx).zoomed().is_some();
        // On a touch screen each control takes a finger's press around the 24 it draws, the
        // header as tall as the press and the two presses side by side, not over each other.
        let reach = crate::surface_chrome::touch_reach(24.0);
        let shell = cx.weak_entity();
        let zoom = window_control_button(
            format!("pane-zoom-{name}"),
            if zoomed { icons::WINDOW_RESTORE } else { icons::WINDOW_MAXIMIZE },
            &ink,
            move |_, _, cx| {
                let _gone = shell.update(cx, |this, cx| {
                    this.focus_pane(pane, cx);
                    this.toggle_pane_zoom(cx);
                });
            },
        )
        .action_hint(
            if zoomed { "Put the panes back" } else { "Fill the view" },
            Keys::Action(Box::new(ZoomPane)),
            cx,
        );
        let shell = cx.weak_entity();
        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.0 + 2.0 * reach))
            .when(!focused, |el| el.invisible().group_hover(group.clone(), |style| style.visible()))
            .child(zoom)
            // Named by what it closes, so a screen reader tells one pane's from another's.
            .when(pane != Pane::Session, |el| {
                el.child(
                    window_control_button(
                        format!("pane-close-{name}"),
                        icons::CLOSE,
                        &ink,
                        move |_, _, cx| {
                            let _gone = shell.update(cx, |this, cx| this.close_pane(pane, cx));
                        },
                    )
                    .action_hint(
                        format!("Close {title}"),
                        Keys::Action(Box::new(crate::app_menus::CloseTab)),
                        cx,
                    ),
                )
            });
        let title_ink = if focused { ink.text } else { ink.text_muted };
        let leading = match self.pane_favicon(pane, cx) {
            // A page's own icon, named so a screen reader tells it from the default globe.
            Some(favicon) => div()
                .id(SharedString::from(format!("pane-favicon-{name}")))
                .flex_none()
                .role(gpui::Role::Image)
                .aria_label("Page icon")
                .child(gpui::img(favicon).size(px(13.0)))
                .into_any_element(),
            None => icon(glyph)
                .size(px(13.0))
                .flex_none()
                .text_color(tint.unwrap_or(title_ink))
                .into_any_element(),
        };
        div()
            .id(SharedString::from(format!("pane-header-{name}")))
            .debug_selector(move || format!("pane-header-{name}"))
            .group(group)
            .h(px(pane_header_height()))
            .flex_none()
            .w_full()
            .px(px(8.0_f32.max(reach)))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .text_size(crate::typography::ui_rems(12.0))
            .child(leading)
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.0)
                    .truncate()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(title_ink)
                    .child(title),
            )
            .children(place.map(|place| {
                div().min_w_0().flex_shrink(1.0).truncate().text_color(ink.text_muted).child(place)
            }))
            .child(div().flex_1())
            .child(controls)
            .into_any_element()
    }

    /// The icon a browser pane's page gives itself, which its header wears for the globe.
    fn pane_favicon(&self, pane: Pane, cx: &App) -> Option<std::sync::Arc<gpui::Image>> {
        let Pane::View(RightSurface::Browser(id)) = pane else { return None };
        self.browsers.get(&id)?.read(cx).favicon.clone()
    }

    /// What a pane's header names: its icon and the icon's tint, its title, and where it runs.
    fn pane_identity(
        &self,
        pane: Pane,
        cx: &App,
    ) -> (&'static str, Option<Hsla>, SharedString, Option<SharedString>) {
        let state = self.state.read(cx);
        let chat = state.selected_chat_row();
        let place = chat.and_then(|chat| {
            tabs::machine_and_branch(state.device_name(&chat.device_id), chat.branch.as_deref())
                .map(SharedString::from)
        });
        match pane {
            Pane::Session => {
                let harness =
                    chat.and_then(|chat| chat.config.as_ref().map(|config| config.harness));
                let (glyph, tint) =
                    harness.map_or((icons::TERMINAL, None), crate::pickers::harness_brand_icon);
                let title = chat.and_then(|chat| chat.title.as_deref()).unwrap_or("New session");
                (glyph, tint, transcript::single_line(title).into(), place)
            }
            Pane::Shell(n) => {
                let title = self
                    .pane_shells
                    .get(&n)
                    .and_then(|shell| shell.read(cx).shown_title(cx))
                    .unwrap_or_else(|| "Shell".into());
                (icons::TERMINAL, None, title, place)
            }
            Pane::View(view) => {
                let (glyph, title) = self.view_identity(view, cx);
                (glyph, None, title, None)
            }
        }
    }
}

/// A pane's name in ids and selectors.
fn pane_name(pane: Pane) -> String {
    match pane {
        Pane::Session => "session".to_owned(),
        Pane::Shell(n) => format!("shell-{n}"),
        Pane::View(view) => format!("{view:?}").to_lowercase().replace('(', "-").replace(')', ""),
    }
}
