//! The bar across the top: the navigator's toggle, the active workspace's name and, quietly,
//! any worker that is down on the left; the strip indicator in the middle; the agents waiting
//! on the human, the inbox's bell, "+" and "…" on the right. Nothing else: every other action
//! is a key, the palette, or a tile's own header, and the readouts live in the status bar.

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
    AddWindow, NewAgent, NewNote, NewTerminal, OpenFile, OpenPalette, ToggleNavigator, ToggleStats,
};
use super::{MenuEntry, WorkspaceView};
use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};
use crate::icons::{IconName, IconSize, icon};
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

/// Which of the bar's menus is open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MenuKind {
    /// "+": what can be opened.
    Add,
    /// "…": everything else, the app's entries included.
    More,
    /// The bell: what needs the human and what finished.
    Inbox,
}

/// The side of a [`kit::icon_button`], which the dots keep clear of.
fn icon_button_side(theme: &slopty_theme::Theme) -> f32 {
    2.0_f32.mul_add(theme.spacing.xs, theme.typography.icon_large())
}

/// The widths across the bar that decide where the column dots go, in points.
#[derive(Clone, Copy, Debug)]
struct BarRow {
    /// The whole bar.
    width: f32,
    /// The safe area's insets, left and right.
    safe: (f32, f32),
    /// The bar's padding on the left (the traffic lights and the inset) and on the right.
    leading: f32,
    trailing: f32,
    /// Between the bar's parts.
    gap: f32,
    /// What the left part holds at its natural width: the name, the server's and the workers'
    /// status.
    left: f32,
    /// What the right part holds: the bell, "+" and "…".
    right: f32,
    /// The dots themselves.
    dots: f32,
}

/// Where the column dots' left edge goes: centred on the safe area, moved aside as far as it
/// takes to keep clear of the left and right parts, and `None` when there is no room between
/// them at all.
fn dots_at(row: BarRow) -> Option<f32> {
    let (safe_left, safe_right) = row.safe;
    let centred = (row.width - safe_left - safe_right - row.dots).mul_add(0.5, safe_left);
    let lowest = row.leading + row.left + row.gap;
    let highest = row.width - row.trailing - row.right - row.gap - row.dots;
    (lowest <= highest).then(|| centred.clamp(lowest, highest))
}

impl WorkspaceView {
    /// Whether `key`'s round trip is on screen: the status bar shows the focused tile's
    /// worker's, and the navigator every worker's.
    pub(super) fn rtt_shown(&self, key: WorkerKey) -> bool {
        self.nav.drawn.is_some() || self.status_worker() == Some(key)
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
        // Nothing to open, no column to mark and no one to point at before the first worker:
        // the bar keeps only "…", where settings and the ways to add one live.
        let has_workers = !self.workers.is_empty();

        // Left: the navigator's toggle, where the human is, and which workers are not there
        // with them.
        let toggle = has_workers.then(|| {
            let hint_theme = Rc::new(theme.clone());
            kit::icon_button(theme, "navigator-toggle", IconName::PanelLeft, "Navigator")
                .when(SHORTCUT_HINTS, |el| {
                    el.tooltip(move |_window, cx| {
                        let keys =
                            crate::palette::keys_for(&ToggleNavigator, &super::key_bindings());
                        cx.new(|_| kit::Hint::new("Navigator", keys, Rc::clone(&hint_theme))).into()
                    })
                })
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_navigator(&ToggleNavigator, window, cx);
                }))
        });
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
            .flex()
            .items_center()
            .gap(px(spacing.xs))
            .px(px(spacing.xs))
            .py(px(spacing.xxs))
            .rounded(px(radii.sm))
            .hover(move |el| el.bg(hsla(s.raised)))
            .active(move |el| el.bg(hsla(s.overlay)))
            .cursor_pointer()
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(theme.typography.ui_size))
                    .font_weight(gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(SharedString::from(self.workspace_name())),
            )
            .child(icon(theme, IconName::ChevronDown, IconSize::Inline, hsla(s.text_muted)));
        let name = tab_stop(name, s.accent).on_click(cx.listener(|this, _ev, _w, cx| {
            this.tick();
            this.layout.toggle_overview();
            cx.notify();
        }));

        // Right: the inbox, "+" and "…". Who needs you is counted on the bell and named in the
        // status bar, which also goes to them.
        let total = self.needs_you_count();
        let unread = self.inbox_count();
        let bell = has_workers.then(|| {
            let tone = if total > 0 { s.warn } else { s.accent };
            let badge = (unread > 0).then(|| {
                let count = SharedString::from(unread.to_string());
                let side = theme.typography.caption() + spacing.xs;
                div()
                    .id("bell-count")
                    .debug_selector(|| "bell-count".to_owned())
                    .role(Role::Status)
                    .aria_label(SharedString::from(format!("{unread} new")))
                    .absolute()
                    .top(px(-spacing.xxs))
                    .right(px(-spacing.xxs))
                    .h(px(side))
                    .min_w(px(side))
                    .px(px(spacing.xxs))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(hsla(tone))
                    .text_color(hsla(s.accent_fg))
                    .text_size(px(theme.typography.caption()))
                    .child(count)
            });
            kit::icon_button(theme, "bell", IconName::Bell, "Inbox")
                .relative()
                .children(badge)
                .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::Inbox, cx)))
        });
        let add = has_workers.then(|| {
            kit::icon_button(theme, "add", IconName::Plus, "Open")
                .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::Add, cx)))
        });
        let more = kit::icon_button(theme, "more", IconName::Ellipsis, "More")
            .on_click(cx.listener(|this, _ev, _w, cx| this.toggle_menu(MenuKind::More, cx)));
        let buttons = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.xxs))
            .children(bell)
            .children(add)
            .child(more);
        // The column dots sit on the middle of the safe area, not on the middle of what the
        // traffic lights leave (the bar's leading inset is 78 pt and its trailing one 12, so a
        // dot row between two equal flexible sides stood 33 pt right of it), and give way to
        // the toggle, the name and the buttons rather than cover them.
        let (leading, trailing) =
            (LEADING_INSET + f32::from(safe.left), spacing.md + f32::from(safe.right));
        let indicator = self.render_indicator(cx).and_then(|(marks, dots)| {
            let text = |text: &str, size: f32, weight: gpui::FontWeight| {
                text_width(window, &theme.typography.ui_family, text, size, weight)
            };
            let status = |status: &str| {
                spacing.xs.mul_add(2.0, text(status, small, gpui::FontWeight::NORMAL))
            };
            let side = icon_button_side(theme);
            let mut left: Vec<f32> = Vec::new();
            if has_workers {
                let strong = gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT);
                left.push(side);
                let name = text(&self.workspace_name(), theme.typography.ui_size, strong);
                left.push(spacing.xs.mul_add(3.0, name) + theme.typography.icon());
            }
            left.extend(self.server_status.as_deref().map(status));
            left.extend(
                self.workers
                    .values()
                    .filter(|w| !w.status.is_up())
                    .map(|w| status(&format!("{} {}", w.name, w.status.text()))),
            );
            let count: f32 = if has_workers { 3.0 } else { 1.0 };
            let right = count.mul_add(side, (count - 1.0) * spacing.xxs);
            let x = dots_at(BarRow {
                width: self.width(window),
                safe: (f32::from(safe.left), f32::from(safe.right)),
                leading,
                trailing,
                gap: spacing.md,
                left: spaced(&left, spacing.md),
                right,
                dots,
            })?;
            Some(
                div()
                    .absolute()
                    .top(safe.top)
                    .bottom_0()
                    .left(px(x))
                    .flex()
                    .items_center()
                    .child(marks),
            )
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
            .pl(px(leading))
            .pr(px(trailing))
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
                    .children(toggle)
                    .when(has_workers, |el| el.child(name))
                    .when_some(server, gpui::ParentElement::child)
                    .children(down),
            )
            .children(indicator)
            .child(div().flex_1().flex().items_center().justify_end().child(buttons))
            .into_any_element()
    }

    /// One dot per column of the active workspace: the focused column's in the text colour,
    /// those in view a step quieter, the rest faint; a click on one goes to that column.
    /// Nothing for a workspace of one column, where there is nowhere to go.
    ///
    /// Dots, not a scaled map of the strip: a track with the view bracketed and the active
    /// column filled read as a progress bar, the loudest thing in the bar saying the least.
    fn render_indicator(&self, cx: &Context<Self>) -> Option<(gpui::AnyElement, f32)> {
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
        #[expect(clippy::cast_precision_loss, reason = "a count of columns is small")]
        let width = count as f32 * theme.spacing.xxs.mul_add(2.0, size);
        let row = div()
            .id("indicator")
            .debug_selector(|| "indicator".to_owned())
            .role(Role::Group)
            .aria_label("Columns")
            .flex_none()
            .flex()
            .items_center()
            .children(marks)
            .into_any_element();
        Some((row, width))
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
            MenuKind::Inbox => Vec::new(),
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
        let panel = if which == MenuKind::Inbox {
            self.render_inbox(cx)
        } else {
            self.menu_panel(rows, cx)
        };
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

    /// The "+" or "…" menu's panel around its rows.
    fn menu_panel(&self, rows: Vec<gpui::AnyElement>, _cx: &Context<Self>) -> gpui::AnyElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        div()
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
            .children(rows)
            .into_any_element()
    }
}

/// `parts` laid side by side with `gap` between them.
fn spaced(parts: &[f32], gap: f32) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a handful of parts")]
    let gaps = parts.len().saturating_sub(1) as f32;
    gaps.mul_add(gap, parts.iter().sum())
}

/// How wide `text` is set in `family` at `size` and `weight`.
fn text_width(
    window: &Window,
    family: &str,
    text: &str,
    size: f32,
    weight: gpui::FontWeight,
) -> f32 {
    let font = gpui::Font { weight, ..gpui::font(SharedString::from(family.to_owned())) };
    let run = gpui::TextRun {
        len: text.len(),
        font,
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(text.to_owned()),
        px(size),
        &[run],
        None,
    );
    f32::from(line.width)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Mac bar 1200 wide: the traffic lights' 78 on the left, 12 on the right, a short name
    /// and "+" and "…".
    const MAC: BarRow = BarRow {
        width: 1200.0,
        safe: (0.0, 0.0),
        leading: 78.0,
        trailing: 12.0,
        gap: 12.0,
        left: 90.0,
        right: 60.0,
        dots: 50.0,
    };

    fn near(a: Option<f32>, b: f32) {
        assert!(a.is_some_and(|a| (a - b).abs() < 0.01), "{a:?} vs {b}");
    }

    /// The dots sit on the middle of the safe area, not of the window and not of what the bar's
    /// paddings leave: a phone on its side with the notch on the left centres them right of the
    /// window's middle.
    #[test]
    fn the_dots_centre_on_the_safe_area() {
        near(dots_at(MAC), 600.0 - 25.0);
        let phone = BarRow {
            width: 844.0,
            safe: (59.0, 0.0),
            leading: 12.0 + 59.0,
            trailing: 12.0,
            left: 80.0,
            ..MAC
        };
        near(dots_at(phone), 59.0 + (844.0 - 59.0) / 2.0 - 25.0);
        let both = BarRow { safe: (59.0, 59.0), trailing: 12.0 + 59.0, ..phone };
        near(dots_at(both), 422.0 - 25.0);
    }

    /// The dots never cover the name nor the right side: a name reaching past where they would
    /// sit moves them right, a wide right side moves them left, and with no room between the
    /// two they are not drawn.
    #[test]
    fn the_dots_give_way_to_the_name_and_the_buttons() {
        let long = BarRow { width: 700.0, left: 260.0, ..MAC };
        near(dots_at(long), 78.0 + 260.0 + 12.0);
        let busy = BarRow { width: 700.0, right: 330.0, ..MAC };
        near(dots_at(busy), 700.0 - 12.0 - 330.0 - 12.0 - 50.0);
        assert_eq!(dots_at(BarRow { width: 450.0, left: 260.0, ..MAC }), None);
        assert_eq!(dots_at(BarRow { width: 390.0, leading: 12.0, left: 240.0, ..MAC }), None);
    }
}
