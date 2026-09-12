//! `WindowPicker`: jump to a session on the canvas, or choose a host window or display to put
//! on it.
//!
//! Shown by the canvas after a `Listing` arrives; a click picks, Escape dismisses. Sessions come
//! first, and among them the ones whose agent is waiting on the human, so a wall of terminals
//! is searched by what needs doing rather than by position.
//!
//! The same modal lists past Claude Code conversations the host found on disk
//! (`AgentSessions`), for resuming one as a driven agent.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_core::SessionId;
use slopty_proto::agent::AgentSessionInfo;
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
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
    /// Resume a past Claude Code conversation as a driven agent.
    Resume(AgentSessionInfo),
    /// List the conversations of every directory on the host, not just this one's.
    Everywhere,
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
    /// Past conversations to resume (the picker's resume form), newest first.
    agents: Vec<AgentSessionInfo>,
    /// The picker is the resume form: only `agents` are listed, under that title.
    resume: bool,
    /// The resume form lists one directory (`Some`) or every directory on the host.
    scope: Option<String>,
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
        Self {
            sessions,
            windows,
            displays,
            agents: Vec::new(),
            resume: false,
            scope: None,
            theme,
            focus: cx.focus_handle(),
        }
    }

    /// The resume form: the conversations the host has on disk for a directory (`scope`),
    /// or for every directory on the host when `None`.
    pub fn resume(
        agents: Vec<AgentSessionInfo>,
        scope: Option<String>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            sessions: Vec::new(),
            windows: Vec::new(),
            displays: Vec::new(),
            agents,
            resume: true,
            scope,
            theme,
            focus: cx.focus_handle(),
        }
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
        id: (&'static str, usize),
        line: Line,
        on_pick: PickerEvent,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let Line { primary, secondary, hot } = line;
        let secondary_color = if hot { theme.surfaces.warn } else { theme.surfaces.text_muted };
        let (raised, overlay) = (theme.surfaces.raised, theme.surfaces.overlay);
        let label =
            if secondary.is_empty() { primary.clone() } else { format!("{primary}, {secondary}") };
        let row = div()
            .id(id)
            .debug_selector(move || format!("picker-{}-{}", id.0, id.1))
            .role(gpui::accesskit::Role::Button)
            .aria_label(SharedString::from(label))
            .w_full()
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .rounded(px(theme.radii.sm))
            .cursor_pointer()
            .hover(move |s| s.bg(hsla(raised)))
            .active(move |s| s.bg(hsla(overlay)))
            .child(div().text_color(hsla(theme.surfaces.text)).child(SharedString::from(primary)))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(hsla(secondary_color))
                    .child(SharedString::from(secondary)),
            );
        tab_stop(row, theme.surfaces.accent).on_click(cx.listener(move |_this, _ev, _w, cx| {
            cx.emit(on_pick.clone());
        }))
    }

    /// A muted heading between the picker's sections.
    fn heading(&self, text: &'static str) -> impl IntoElement {
        div()
            .px(px(self.theme.spacing.md))
            .pt(px(self.theme.spacing.sm))
            .pb(px(self.theme.spacing.xxs))
            .text_size(px(self.theme.typography.caption()))
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
        for (i, a) in self.agents.iter().enumerate() {
            let title = if a.title.is_empty() { a.id.clone() } else { a.title.clone() };
            let line = Line::new(title, format!("{} · {}", ago(a.modified_ms), a.cwd));
            rows.push(
                self.row(("agent", i), line, PickerEvent::Resume(a.clone()), cx).into_any_element(),
            );
        }
        let empty = rows.is_empty();
        if self.resume && self.scope.is_some() {
            // A list for one directory offers the whole host; the answer replaces the picker.
            let line = Line::new(
                "Every directory".to_owned(),
                "the conversations of every project on the host".to_owned(),
            );
            rows.push(
                self.row(("everywhere", 0), line, PickerEvent::Everywhere, cx).into_any_element(),
            );
        }
        let (title, nothing): (&'static str, &'static str) = if self.resume {
            ("Resume a conversation", "no conversation on the host")
        } else {
            (
                "Jump to a session, or add a window from the host",
                "nothing on the canvas or shareable on the host",
            )
        };

        div()
            .id("picker-backdrop")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla_alpha(theme.surfaces.canvas, alpha::SCRIM))
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
                    .role(gpui::accesskit::Role::Dialog)
                    .aria_label(title)
                    .w(px(560.0))
                    .max_h(px(520.0))
                    .flex()
                    .flex_col()
                    .rounded(px(theme.radii.md))
                    .border_1()
                    .border_color(hsla(theme.surfaces.border))
                    .bg(hsla(theme.surfaces.panel))
                    .shadow_sm()
                    .text_size(px(theme.typography.ui_size))
                    .font_family(theme.typography.ui_family.clone())
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .child(
                        div()
                            .px(px(theme.spacing.md))
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(theme.surfaces.border))
                            .text_color(hsla(theme.surfaces.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .id("picker-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .p(px(theme.spacing.xs))
                            .children(rows)
                            .when(empty, |el| {
                                el.child(
                                    div()
                                        .p(px(theme.spacing.md))
                                        .text_color(hsla(theme.surfaces.text_muted))
                                        .child(nothing),
                                )
                            }),
                    ),
            )
    }
}

/// How long ago a transcript was written, in the coarsest unit that is not zero.
#[must_use]
pub fn ago(modified_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    let secs = now.saturating_sub(modified_ms) / 1000;
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}
