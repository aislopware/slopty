//! `WindowPicker`: jump to a session on the canvas, or choose a host window or display to put
//! on it.
//!
//! Shown by the canvas after a `Listing` arrives; a click picks, Escape dismisses. Sessions come
//! first, and among them the ones whose agent is waiting on the human, so a wall of terminals
//! is searched by what needs doing rather than by position.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_core::SessionId;
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_theme::Theme;

use crate::colors::{hsla, hsla_alpha};

/// What the user chose.
#[derive(Clone, Debug)]
pub enum PickerEvent {
    /// Put this on the canvas; `size` is the target's size in points.
    Pick {
        /// Target.
        target: CaptureTarget,
        /// Points.
        size: (f32, f32),
        /// Label for the item's title bar.
        title: String,
    },
    /// Reveal and focus this session's terminal.
    Jump(SessionId),
    /// Closed without choosing.
    Dismiss,
}

/// One terminal session on the canvas, as the picker lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// Session.
    pub session: SessionId,
    /// Title bar text.
    pub title: String,
    /// The agent's status line, when an agent was seen in it.
    pub status: Option<String>,
    /// The agent is waiting on the human.
    pub needs_you: bool,
}

/// The text of one picker row; `hot` paints the secondary text warm (an agent waiting on the
/// human).
struct Line {
    primary: String,
    secondary: String,
    hot: bool,
}

impl Line {
    const fn new(primary: String, secondary: String) -> Self {
        Self { primary, secondary, hot: false }
    }
}

/// A modal list of sessions, windows and displays.
pub struct WindowPicker {
    /// Already ordered by the canvas: needs-you, other agents, plain shells.
    sessions: Vec<SessionRow>,
    windows: Vec<WindowInfo>,
    displays: Vec<DisplayInfo>,
    theme: Theme,
    focus: FocusHandle,
}

impl std::fmt::Debug for WindowPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowPicker")
            .field("sessions", &self.sessions.len())
            .field("windows", &self.windows.len())
            .field("displays", &self.displays.len())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<PickerEvent> for WindowPicker {}

impl Focusable for WindowPicker {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WindowPicker {
    /// A picker over the canvas's sessions and a host listing.
    pub fn new(
        sessions: Vec<SessionRow>,
        mut windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        windows.retain(|w| w.on_screen);
        windows.sort_by(|a, b| a.app.cmp(&b.app).then_with(|| a.title.cmp(&b.title)));
        Self { sessions, windows, displays, theme, focus: cx.focus_handle() }
    }

    /// Swap the theme.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    fn key_down(_this: &mut Self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" {
            cx.emit(PickerEvent::Dismiss);
            cx.stop_propagation();
        }
    }

    /// One pickable line.
    fn row(
        &self,
        id: impl Into<gpui::ElementId>,
        line: Line,
        on_pick: PickerEvent,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let Line { primary, secondary, hot } = line;
        let secondary_color =
            if hot { theme.terminal.palette(3) } else { theme.surfaces.text_muted };
        div()
            .id(id)
            .w_full()
            .px(px(12.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .rounded(px(theme.radius * 0.6))
            .cursor_pointer()
            .hover(|s| s.bg(hsla_alpha(theme.surfaces.accent, 0.12)))
            .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_this, _ev, _w, cx| {
                cx.emit(on_pick.clone());
            }))
            .child(div().text_color(hsla(theme.surfaces.text)).child(SharedString::from(primary)))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(hsla(secondary_color))
                    .child(SharedString::from(secondary)),
            )
    }

    /// A muted heading between the picker's sections.
    fn heading(&self, text: &'static str) -> impl IntoElement {
        div()
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(2.0))
            .text_size(px(11.0))
            .text_color(hsla(self.theme.surfaces.text_muted))
            .child(text)
    }
}

impl Render for WindowPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if !self.sessions.is_empty() {
            rows.push(self.heading("Sessions").into_any_element());
        }
        for (i, s) in self.sessions.iter().enumerate() {
            let status = s.status.clone().unwrap_or_default();
            let line = Line { primary: s.title.clone(), secondary: status, hot: s.needs_you };
            rows.push(
                self.row(("session", i), line, PickerEvent::Jump(s.session), cx).into_any_element(),
            );
        }
        let has_screens = !(self.displays.is_empty() && self.windows.is_empty());
        if !self.sessions.is_empty() && has_screens {
            rows.push(self.heading("Windows").into_any_element());
        }
        for (i, d) in self.displays.iter().enumerate() {
            let event = PickerEvent::Pick {
                target: CaptureTarget::Display(d.id),
                size: (d.w, d.h),
                title: format!("display {}", d.id),
            };
            let label = format!("{}×{} @{}× {}Hz", d.w, d.h, d.scale, d.hz);
            let line = Line::new(format!("Display {}", d.id), label);
            rows.push(self.row(("display", i), line, event, cx).into_any_element());
        }
        for (i, w) in self.windows.iter().enumerate() {
            let title = if w.title.is_empty() { w.app.clone() } else { w.title.clone() };
            let event = PickerEvent::Pick {
                target: CaptureTarget::Window(w.id),
                size: (w.w, w.h),
                title: title.clone(),
            };
            let line = Line::new(w.app.clone(), title);
            rows.push(self.row(("window", i), line, event, cx).into_any_element());
        }
        let empty = rows.is_empty();

        div()
            .id("picker-backdrop")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla_alpha(theme.surfaces.canvas, 0.6))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _w, cx| {
                    cx.emit(PickerEvent::Dismiss);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("picker")
                    .w(px(560.0))
                    .max_h(px(520.0))
                    .flex()
                    .flex_col()
                    .rounded(px(theme.radius))
                    .border_1()
                    .border_color(hsla(theme.surfaces.border))
                    .bg(hsla(theme.surfaces.panel))
                    .shadow_lg()
                    .text_size(px(13.0))
                    .font_family(theme.typography.ui_family.clone())
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .child(
                        div()
                            .px(px(14.0))
                            .py(px(10.0))
                            .border_b_1()
                            .border_color(hsla(theme.surfaces.border))
                            .text_color(hsla(theme.surfaces.text))
                            .child("Jump to a session, or add a window from the host"),
                    )
                    .child(
                        div()
                            .id("picker-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .p(px(6.0))
                            .children(rows)
                            .when(empty, |el| {
                                el.child(
                                    div()
                                        .p(px(12.0))
                                        .text_color(hsla(theme.surfaces.text_muted))
                                        .child("nothing on the canvas or shareable on the host"),
                                )
                            }),
                    ),
            )
    }
}
