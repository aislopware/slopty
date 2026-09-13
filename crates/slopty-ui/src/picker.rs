//! `WindowPicker`: jump to a session on the canvas, or choose a host window or display to put
//! on it.
//!
//! Shown by the canvas after a `Listing` arrives; a click picks, Escape dismisses. Sessions come
//! first, and among them the ones whose agent is waiting on the human, so a wall of terminals
//! is searched by what needs doing rather than by position. A field at the top filters the
//! rows by every word typed, ↑/↓ choose one and ↩ picks it, as the palette does.
//!
//! The same modal lists past Claude Code conversations the host found on disk
//! (`AgentSessions`), for resuming one as a driven agent.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
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
#[derive(Clone)]
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

/// Which list a row belongs to; the headings sit between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Sessions,
    Screens,
    Agents,
    Everywhere,
}

/// One pickable row, before the filter.
#[derive(Clone)]
struct Row {
    /// The kind and the index in its own list: the row's element id and debug selector, kept
    /// through the filter so a hidden row does not renumber the rest.
    id: (&'static str, usize),
    section: Section,
    line: Line,
    on_pick: PickerEvent,
}

/// Whether `text` holds every word of `query`, in any order and any case; an empty query
/// matches everything.
#[must_use]
pub fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    query.split_whitespace().all(|word| text.contains(&word.to_lowercase()))
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
    /// The filter field, made on the first frame (it needs the window) and given the keyboard
    /// then, when the picker has it.
    input: Option<Entity<InputState>>,
    /// The field's events, held while the field is.
    events: Option<Subscription>,
    /// What the field says: every word of it must be in a row for the row to show.
    query: String,
    /// Which visible row ↑/↓ have chosen; ↩ picks it.
    selected: usize,
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
    /// The field once it exists, so the keyboard lands in it; the backdrop before.
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.input
            .as_ref()
            .map_or_else(|| self.focus.clone(), |input| input.read(cx).focus_handle(cx))
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
            input: None,
            events: None,
            query: String::new(),
            selected: 0,
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
            input: None,
            events: None,
            query: String::new(),
            selected: 0,
        }
    }

    /// Every row in its order, before the filter: sessions, then the host's displays and
    /// windows, then the conversations, then the "Every directory" row of a one-directory list.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let status = s.status.clone().unwrap_or_default();
            rows.push(Row {
                id: ("session", i),
                section: Section::Sessions,
                line: Line { primary: s.title.clone(), secondary: status, hot: s.needs_you },
                on_pick: PickerEvent::Jump(s.session),
            });
        }
        for (i, d) in self.displays.iter().enumerate() {
            rows.push(Row {
                id: ("display", i),
                section: Section::Screens,
                line: Line::new(
                    format!("Display {}", d.id),
                    format!("{}×{} @{}× {}Hz", d.w, d.h, d.scale, d.hz),
                ),
                on_pick: PickerEvent::Pick {
                    target: CaptureTarget::Display(d.id),
                    size: (d.w, d.h),
                    title: format!("display {}", d.id),
                },
            });
        }
        for (i, w) in self.windows.iter().enumerate() {
            let title = if w.title.is_empty() { w.app.clone() } else { w.title.clone() };
            rows.push(Row {
                id: ("window", i),
                section: Section::Screens,
                line: Line::new(w.app.clone(), title.clone()),
                on_pick: PickerEvent::Pick {
                    target: CaptureTarget::Window(w.id),
                    size: (w.w, w.h),
                    title,
                },
            });
        }
        for (i, a) in self.agents.iter().enumerate() {
            let title = if a.title.is_empty() { a.id.clone() } else { a.title.clone() };
            rows.push(Row {
                id: ("agent", i),
                section: Section::Agents,
                line: Line::new(title, format!("{} · {}", ago(a.modified_ms), a.cwd)),
                on_pick: PickerEvent::Resume(a.clone()),
            });
        }
        if self.resume && self.scope.is_some() {
            // A list for one directory offers the whole host; the answer replaces the picker.
            rows.push(Row {
                id: ("everywhere", 0),
                section: Section::Everywhere,
                line: Line::new(
                    "Every directory".to_owned(),
                    "the conversations of every project on the host".to_owned(),
                ),
                on_pick: PickerEvent::Everywhere,
            });
        }
        rows
    }

    /// The rows the field lets through: every word typed is in the row's text. The "Every
    /// directory" row is a way out, not a match, and always shows.
    fn visible(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| {
                row.section == Section::Everywhere
                    || matches(&self.query, &format!("{} {}", row.line.primary, row.line.secondary))
            })
            .collect()
    }

    /// The chosen row's index, clamped to the visible rows.
    fn selected(&self, count: usize) -> usize {
        self.selected.min(count.saturating_sub(1))
    }

    /// ↑/↓: the choice moves, wrapping.
    fn step(&mut self, delta: i64, cx: &mut Context<Self>) {
        let count = i64::try_from(self.visible().len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let at = i64::try_from(self.selected(usize::try_from(count).unwrap_or(0))).unwrap_or(0);
        self.selected = usize::try_from(at.saturating_add(delta).rem_euclid(count)).unwrap_or(0);
        cx.notify();
    }

    /// ↩: the chosen row is picked.
    fn pick(&self, cx: &mut Context<Self>) {
        let rows = self.visible();
        if let Some(row) = rows.get(self.selected(rows.len())) {
            cx.emit(row.on_pick.clone());
        }
    }

    /// The first frame makes the field (it needs the window); the keyboard, when the picker
    /// holds it on the backdrop, moves into the field.
    fn ensure_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type to filter"));
        self.events = Some(cx.subscribe(&input, |this, input, event, cx| match event {
            InputEvent::Change => {
                this.query = input.read(cx).value().to_string();
                this.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.pick(cx),
            InputEvent::Focus | InputEvent::Blur => {}
        }));
        if self.focus.is_focused(window) {
            window.focus(&input.read(cx).focus_handle(cx), cx);
        }
        self.input = Some(input);
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

    /// One pickable line; `chosen` is the one ↩ would pick.
    fn row(
        &self,
        id: (&'static str, usize),
        line: Line,
        on_pick: PickerEvent,
        chosen: bool,
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
            .when(chosen, |el| el.bg(hsla_alpha(theme.surfaces.accent, alpha::TINT_STRONG)))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_field(window, cx);
        let theme = self.theme.clone();
        let visible = self.visible();
        let chosen = self.selected(visible.len());
        let has_sessions = visible.iter().any(|r| r.section == Section::Sessions);
        let has_screens = visible.iter().any(|r| r.section == Section::Screens);
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut section = None;
        for (ix, row) in visible.into_iter().enumerate() {
            if section != Some(row.section) {
                section = Some(row.section);
                match row.section {
                    Section::Sessions => rows.push(self.heading("Sessions").into_any_element()),
                    Section::Screens if has_sessions && has_screens => {
                        rows.push(self.heading("Windows").into_any_element());
                    }
                    Section::Screens | Section::Agents | Section::Everywhere => {}
                }
            }
            let Row { id, line, on_pick, .. } = row;
            rows.push(self.row(id, line, on_pick, ix == chosen, cx).into_any_element());
        }
        let empty = rows.is_empty();
        let (title, nothing): (&'static str, &'static str) =
            match (self.resume, self.query.is_empty()) {
                (true, true) => ("Resume a conversation", "no conversation on the host"),
                (true, false) => ("Resume a conversation", "no conversation matches"),
                (false, true) => (
                    "Jump to a session, or add a window from the host",
                    "nothing on the canvas or shareable on the host",
                ),
                (false, false) => {
                    ("Jump to a session, or add a window from the host", "nothing matches")
                }
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
            .capture_action(cx.listener(|this, _: &MoveUp, _window, cx| this.step(-1, cx)))
            .capture_action(cx.listener(|this, _: &MoveDown, _window, cx| this.step(1, cx)))
            .capture_action(cx.listener(|_this, _: &Escape, _window, cx| {
                cx.emit(PickerEvent::Dismiss);
            }))
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
                    .children(self.input.as_ref().map(|input| {
                        div()
                            .px(px(theme.spacing.md))
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(theme.surfaces.border))
                            .child(Input::new(input).aria_label("Filter"))
                    }))
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

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn the_filter_takes_every_word_in_any_order_and_case() {
        assert!(matches("", "anything at all"));
        assert!(matches("build", "fix the build, 2 d ago · /w/slopty"));
        assert!(matches("SLOPTY fix", "fix the build, 2 d ago · /w/slopty"));
        assert!(!matches("fix tests", "fix the build, 2 d ago · /w/slopty"));
    }
}
