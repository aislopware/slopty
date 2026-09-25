//! The bar across the top: the active workspace's name and, quietly, any worker that is down
//! on the left; the strip indicator in the middle; the link's round trip, the agents waiting
//! on the human, "+" and "…" on the right. Nothing else: every other action is a key, the
//! palette, or a tile's own header.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement as _, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px,
};
use slopty_client::layout::WorkerKey;
use slopty_theme::alpha;

use super::actions::{
    AddWindow, NewAgent, NewNote, NewTerminal, OpenFile, OpenPalette, ToggleStats,
};
use super::{MenuEntry, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::kit;

/// The bar's height, under the safe area.
pub const TITLEBAR_H: f32 = 38.0;

/// Room for the traffic lights at the left of the bar.
#[cfg(target_os = "macos")]
const LEADING_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: f32 = 12.0;

/// Keyboard hints where there is a keyboard with a ⌘ key.
const SHORTCUT_HINTS: bool = cfg!(target_os = "macos");

/// The column marks: a dot's diameter and the room it takes, at most this many dots at full
/// size before they shrink to fit.
const DOT: f32 = 6.0;
const DOTS_AT_FULL_SIZE: usize = 12;

/// The side of the square "+" and "…" buttons, and the stroke their glyphs are drawn with.
const ICON_BUTTON: f32 = 24.0;
const ICON_STROKE: f32 = 1.5;

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    /// "+": what can be opened.
    Add,
    /// "…": everything else, the app's entries included.
    More,
}

impl WorkspaceView {
    /// Whether the bar shows `key`'s round trip: the worker of the focused tile does.
    pub(super) fn rtt_shown(&self, key: WorkerKey) -> bool {
        self.context_worker() == Some(key)
    }

    /// The active workspace's name: the one given, else its place.
    #[must_use]
    pub fn workspace_name(&self) -> String {
        self.workspace_name_at(self.layout.active_workspace())
    }

    /// Workspace `ix`'s name: the one given, else its place.
    pub(super) fn workspace_name_at(&self, ix: usize) -> String {
        self.layout
            .workspaces()
            .get(ix)
            .and_then(|ws| ws.name().map(str::to_owned))
            .unwrap_or_else(|| format!("Workspace {}", ix.saturating_add(1)))
    }

    fn toggle_menu(&mut self, which: MenuKind, cx: &mut Context<Self>) {
        self.menu = if self.menu == Some(which) { None } else { Some(which) };
        cx.notify();
    }

    /// The bar. `safe_top` is the notch's inset on a phone, zero on a Mac.
    pub(super) fn render_titlebar(&self, window: &Window, cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let (spacing, radii) = (theme.spacing, theme.radii);
        let safe = window.insets().effective();
        let small = theme.typography.small();
        let hint_theme = Rc::new(theme.clone());
        // A square button around a glyph drawn from quads, so "+" and "…" sit on the button's
        // centre in any font rather than on a text baseline.
        let button =
            |id: &'static str, glyph: gpui::AnyElement, label: &'static str, hint: &'static str| {
                let hint_theme = Rc::clone(&hint_theme);
                let pill = div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .role(Role::Button)
                    .aria_label(label)
                    .flex_none()
                    .size(px(ICON_BUTTON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(radii.sm))
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .active(move |el| el.bg(hsla(s.overlay)))
                    .cursor_pointer()
                    .child(glyph)
                    .when(SHORTCUT_HINTS && !hint.is_empty(), |el| {
                        el.tooltip(move |_window, cx| {
                            cx.new(|_| kit::Hint::new(label, hint, Rc::clone(&hint_theme))).into()
                        })
                    });
                tab_stop(pill, s.accent)
            };
        let ink = hsla(s.text_secondary);
        let plus = div()
            .relative()
            .size(px(spacing.md))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px((spacing.md - ICON_STROKE) / 2.0))
                    .w_full()
                    .h(px(ICON_STROKE))
                    .rounded_full()
                    .bg(ink),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px((spacing.md - ICON_STROKE) / 2.0))
                    .h_full()
                    .w(px(ICON_STROKE))
                    .rounded_full()
                    .bg(ink),
            )
            .into_any_element();
        let dot = || div().size(px(ICON_STROKE * 2.0)).rounded_full().bg(ink);
        let ellipsis = div()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .child(dot())
            .child(dot())
            .child(dot())
            .into_any_element();
        // Nothing to open, no column to mark and no one to point at before the first worker:
        // the bar keeps only "…", where settings and the ways to add one live.
        let has_workers = !self.workers.is_empty();

        // Left: where the human is, and which workers are not there with them.
        let down: Vec<gpui::AnyElement> = self
            .workers
            .iter()
            .filter(|(_, w)| !w.status.is_up())
            .map(|(key, w)| {
                let key = *key;
                div()
                    .id(SharedString::from(format!("down-{key}")))
                    .debug_selector(move || format!("down-{key}"))
                    .role(Role::Status)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .text_size(px(small))
                    .text_color(hsla(s.text_muted))
                    .aria_label(SharedString::from(format!("{}, {}", w.name, w.status.text())))
                    .child(div().size(px(spacing.xs)).rounded_full().bg(hsla(s.warn)))
                    .child(SharedString::from(format!("{} {}", w.name, w.status.text())))
                    .into_any_element()
            })
            .collect();
        let server = self.server_status.clone().map(|text| {
            div()
                .id("server-status")
                .debug_selector(|| "server-status".to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_size(px(small))
                .text_color(hsla(s.text_muted))
                .child(div().size(px(spacing.xs)).rounded_full().bg(hsla(s.warn)))
                .child(text)
        });
        let name = div()
            .id("workspace-name")
            .debug_selector(|| "workspace-name".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(format!(
                "{}, show every workspace",
                self.workspace_name()
            )))
            .flex_shrink(1.0)
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(px(theme.typography.ui_size))
            .font_weight(gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT))
            .text_color(hsla(s.text))
            .cursor_pointer()
            .child(SharedString::from(self.workspace_name()))
            .on_click(cx.listener(|this, _ev, _w, cx| {
                this.tick();
                this.layout.toggle_overview();
                cx.notify();
            }));

        // Right: the round trip to the focused tile's worker, the agents waiting, "+" and "…".
        let rtt = self
            .context_worker()
            .and_then(|key| self.workers.get(&key)?.rtt)
            .map(|d| format!("{:.1} ms", d.as_secs_f64() * 1e3));
        let total = self.needs_you_count();
        let needs_you = (total > 0).then(|| {
            let warm = s.warn;
            let text =
                if total == 1 { "1 needs you".to_owned() } else { format!("{total} need you") };
            let pill = div()
                .id("needs-you")
                .debug_selector(|| "needs-you".to_owned())
                .role(Role::Button)
                .aria_label(SharedString::from(text.clone()))
                .flex_none()
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(radii.xs))
                .text_size(px(small))
                .text_color(hsla(warm))
                .bg(hsla_alpha(warm, alpha::FAINT))
                .hover(move |el| el.bg(hsla_alpha(warm, alpha::TINT)))
                .cursor_pointer()
                .child(SharedString::from(text));
            tab_stop(pill, s.accent).on_click(cx.listener(|this, _ev, window, cx| {
                this.next_attention(&super::actions::NextAttention, window, cx);
            }))
        });
        let add = has_workers.then(|| {
            button("add", plus, "Open", "").on_click(cx.listener(|this, _ev, _w, cx| {
                this.toggle_menu(MenuKind::Add, cx);
            }))
        });
        let more =
            button("more", ellipsis, "More", "").on_click(cx.listener(|this, _ev, _w, cx| {
                this.toggle_menu(MenuKind::More, cx);
            }));
        // The readouts, then the buttons: a number set right against "+" read as its label.
        let readouts = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .when_some(rtt, |el, rtt| {
                el.child(
                    div()
                        .id("rtt")
                        .debug_selector(|| "rtt".to_owned())
                        .flex_none()
                        .text_size(px(small))
                        .text_color(hsla(s.text_muted))
                        .child(SharedString::from(rtt)),
                )
            })
            .when_some(needs_you, gpui::ParentElement::child);
        let buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .when_some(add, gpui::ParentElement::child)
            .child(more);
        // The column dots sit on the window's centre, not on the centre of what the traffic
        // lights leave: the bar's leading inset is 78 pt and its trailing one 12, so a dot row
        // between two equal flexible sides stood 33 pt right of the middle.
        let indicator = self.render_indicator(cx).map(|marks| {
            div()
                .absolute()
                .top(safe.top)
                .bottom_0()
                .left_0()
                .right_0()
                .flex()
                .items_center()
                .justify_center()
                .child(marks)
        });
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".to_owned())
            .relative()
            .h(px(TITLEBAR_H) + safe.top)
            .pt(safe.top)
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .pl(px(LEADING_INSET) + safe.left)
            .pr(px(spacing.md) + safe.right)
            .bg(hsla(s.canvas))
            .border_b_1()
            .border_color(hsla(s.border))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(spacing.md))
                    .when(has_workers, |el| el.child(name))
                    .when_some(server, gpui::ParentElement::child)
                    .children(down),
            )
            .children(indicator)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(spacing.md))
                    .child(readouts)
                    .child(buttons),
            )
            .into_any_element()
    }

    /// One dot per column of the active workspace: the focused column's in the text colour,
    /// those in view a step quieter, the rest faint; a click on one goes to that column.
    /// Nothing for a workspace of one column, where there is nowhere to go.
    ///
    /// Dots, not a scaled map of the strip: a track with the view bracketed and the active
    /// column filled read as a progress bar, the loudest thing in the bar saying the least.
    fn render_indicator(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let strip = self.layout.frame().strip;
        let count = strip.columns.len();
        if count < 2 {
            return None;
        }
        let (view_x, view_w) = strip.view;
        let size = if count > DOTS_AT_FULL_SIZE { DOT - theme.spacing.xxs } else { DOT };
        let marks: Vec<gpui::AnyElement> = strip
            .columns
            .iter()
            .enumerate()
            .map(|(i, (x, w))| {
                let active = strip.active == Some(i);
                let in_view = x + w > view_x + 1.0 && *x < view_x + view_w - 1.0;
                let ink = if active {
                    hsla(s.text)
                } else if in_view {
                    hsla(s.text_muted)
                } else {
                    hsla_alpha(s.text_muted, alpha::PRESSED)
                };
                div()
                    .id(("column", i))
                    .debug_selector(move || format!("column-{i}"))
                    .role(Role::Button)
                    .aria_label(SharedString::from(format!("Column {}", i.saturating_add(1))))
                    .flex_none()
                    .py(px(theme.spacing.sm))
                    .px(px(theme.spacing.xxs))
                    .cursor_pointer()
                    .child(div().size(px(size)).rounded_full().bg(ink))
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _ev, _w, cx| {
                        this.tick();
                        this.layout.strip_jump(i);
                        this.after_focus_moved(cx);
                        this.layout_touched(cx);
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .id("indicator")
                .debug_selector(|| "indicator".to_owned())
                .role(Role::Group)
                .aria_label("Columns")
                .flex_none()
                .flex()
                .items_center()
                .children(marks)
                .into_any_element(),
        )
    }

    /// The open menu, anchored under its button at the right of the bar.
    pub(super) fn render_menu(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        // Rows run on the workspace itself: with nothing focused, a dispatched action would
        // never reach its handlers.
        type Run = fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>);
        let which = self.menu?;
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let entity = cx.entity().downgrade();
        let bindings = super::key_bindings();
        // The row's keys come from the binding table, spelled as the palette spells them; a
        // phone's menu shows none, as its bar shows no hints.
        let action = |label: &'static str, bound: &dyn gpui::Action, run: Run| {
            let entity = entity.clone();
            let detail = if SHORTCUT_HINTS {
                crate::palette::keys_for(bound, &bindings)
            } else {
                String::new()
            };
            MenuEntry {
                label: label.into(),
                detail: detail.into(),
                run: Rc::new(move |window, cx| {
                    let _gone = entity.update(cx, |this, cx| run(this, window, cx));
                }),
            }
        };
        let entries: Vec<MenuEntry> = match which {
            MenuKind::Add => vec![
                action("Shell", &NewTerminal, |this, w, cx| this.new_terminal(&NewTerminal, w, cx)),
                action("Agent", &NewAgent, |this, w, cx| this.new_agent(&NewAgent, w, cx)),
                action("Note", &NewNote, |this, w, cx| this.new_note(&NewNote, w, cx)),
                action("Window", &AddWindow, |this, w, cx| this.add_window(&AddWindow, w, cx)),
                action("File", &OpenFile, |this, w, cx| this.open_file_palette(&OpenFile, w, cx)),
            ],
            MenuKind::More => {
                let mut entries = vec![
                    action("Command palette", &OpenPalette, |this, w, cx| {
                        this.open_palette(&OpenPalette, w, cx);
                    }),
                    action("Overview", &super::actions::ToggleOverview, |this, _w, cx| {
                        this.tick();
                        this.layout.toggle_overview();
                        cx.notify();
                    }),
                    action("Stream stats", &ToggleStats, |this, w, cx| {
                        this.toggle_stats(&ToggleStats, w, cx);
                    }),
                ];
                entries.extend(self.more_entries.iter().cloned());
                entries
            }
        };
        let rows: Vec<gpui::AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let run = Rc::clone(&entry.run);
                let entity = entity.clone();
                let row = div()
                    .id(("menu-row", i))
                    .debug_selector({
                        let label = entry.label.clone();
                        move || format!("menu-{label}")
                    })
                    .role(Role::MenuItem)
                    .aria_label(entry.label.clone())
                    .flex()
                    .items_center()
                    .gap(px(spacing.md))
                    .px(px(spacing.md))
                    .py(px(spacing.xs))
                    .cursor_pointer()
                    .hover(move |el| el.bg(hsla(s.raised)))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(theme.typography.ui_size))
                            .text_color(hsla(s.text))
                            .child(entry.label.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(theme.typography.small()))
                            .text_color(hsla(s.text_muted))
                            .child(entry.detail.clone()),
                    );
                tab_stop(row, s.accent)
                    .on_click(move |_ev, window, cx| {
                        // The menu closes first, so the entry runs with the focus back where
                        // it was.
                        let _closed = entity.update(cx, |this, cx| {
                            this.menu = None;
                            cx.notify();
                        });
                        run(window, cx);
                    })
                    .into_any_element()
            })
            .collect();
        let panel = div()
            .id("menu")
            .debug_selector(|| "menu".to_owned())
            .role(Role::Menu)
            .occlude()
            .w(px(260.0))
            .flex()
            .flex_col()
            .py(px(spacing.xs))
            .rounded(px(theme.radii.md))
            .bg(hsla(s.panel))
            .border_1()
            .border_color(hsla(s.border))
            .shadow_sm()
            .font_family(theme.typography.ui_family.clone())
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .children(rows);
        // A click anywhere else closes it.
        Some(
            div()
                .id("menu-away")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev, _w, cx| {
                        this.menu = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(TITLEBAR_H) + safe.top)
                        .right(px(spacing.md) + safe.right)
                        .child(panel),
                )
                .into_any_element(),
        )
    }
}
