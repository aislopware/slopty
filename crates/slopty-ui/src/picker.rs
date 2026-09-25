//! `WindowPicker`: jump to a session on the canvas, or choose a worker window or display to put
//! on it.
//!
//! Shown by the canvas after a `Listing` arrives; a click picks, Escape dismisses. Sessions come
//! first, and among them the ones whose agent is waiting on the human, so a wall of terminals
//! is searched by what needs doing rather than by position. A field at the top filters the
//! rows by every word typed, ↑/↓ choose one and ↩ picks it, as the palette does.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use slopty_core::SessionId;
use slopty_proto::screen::{CaptureTarget, DisplayInfo, WindowInfo};
use slopty_theme::Theme;

use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{self, IconName, IconSize, Status};
use crate::palette::{icon_slot, section_heading};

/// What the field says before anything is typed.
pub(crate) const FILTER_PLACEHOLDER: &str = "Type to filter";

/// The picker's empty states: nothing to offer at all, and nothing left after the query.
pub(crate) const NOTHING_TO_JUMP_TO: &str = "Nothing on the canvas or shareable on the worker";
pub(crate) const NOTHING_MATCHES: &str = "Nothing matches";

/// The row that stands for the worker's windows until its listing arrives.
pub(crate) const LOADING_WINDOWS: &str = "Asking the worker for its windows…";

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
    /// How the agent is doing, for the row's status mark; `None` for a plain shell.
    pub mark: Option<Status>,
    /// The worker it runs on, named only when more than one is known.
    pub worker: Option<String>,
}

/// The text of one picker row; `hot` paints the secondary text warm (an agent waiting on the
/// human).
#[derive(Clone)]
struct Line {
    icon: IconName,
    primary: String,
    secondary: String,
    hot: bool,
    mark: Option<Status>,
    worker: Option<String>,
}

impl Line {
    const fn new(icon: IconName, primary: String, secondary: String) -> Self {
        Self { icon, primary, secondary, hot: false, mark: None, worker: None }
    }
}

/// Which list a row belongs to; the headings sit between them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Sessions,
    Displays,
    Windows,
}

impl Section {
    const fn heading(self) -> &'static str {
        match self {
            Self::Sessions => "Sessions",
            Self::Displays => "Displays",
            Self::Windows => "Windows",
        }
    }
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
    /// The worker's listing is still on its way: a row says so where its windows will be.
    loading: bool,
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
            .field("loading", &self.loading)
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

/// The windows worth offering, in the order they are listed: on screen only (a minimised one,
/// or one on another Space, captures nothing), by app then title.
fn offered(mut windows: Vec<WindowInfo>) -> Vec<WindowInfo> {
    windows.retain(|w| w.on_screen);
    windows.sort_by(|a, b| a.app.cmp(&b.app).then_with(|| a.title.cmp(&b.title)));
    windows
}

impl WindowPicker {
    /// A picker over the canvas's sessions and a worker listing.
    pub fn new(
        sessions: Vec<SessionRow>,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        theme: Theme,
        cx: &Context<Self>,
    ) -> Self {
        Self {
            sessions,
            windows: offered(windows),
            displays,
            loading: false,
            theme,
            focus: cx.focus_handle(),
            input: None,
            events: None,
            query: String::new(),
            selected: 0,
        }
    }

    /// A picker over the canvas's sessions, shown at once while the worker is asked for its
    /// windows; [`Self::set_listing`] fills them in.
    pub fn loading(sessions: Vec<SessionRow>, theme: Theme, cx: &Context<Self>) -> Self {
        Self { loading: true, ..Self::new(sessions, Vec::new(), Vec::new(), theme, cx) }
    }

    /// Whether the worker's listing is still on its way.
    #[must_use]
    pub const fn is_loading(&self) -> bool {
        self.loading
    }

    /// The worker's listing arrived: its displays and windows join the sessions, and the row
    /// that stood for them goes. The choice stays on the row it was on.
    pub fn set_listing(
        &mut self,
        windows: Vec<WindowInfo>,
        displays: Vec<DisplayInfo>,
        cx: &mut Context<Self>,
    ) {
        self.windows = offered(windows);
        self.displays = displays;
        self.loading = false;
        cx.notify();
    }

    /// Every row in its order, before the filter: sessions, then the worker's displays and
    /// windows.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            let status = s.status.clone().unwrap_or_default();
            // A session an agent was seen in is an agent's; the rest are shells.
            let icon = if s.status.is_some() { IconName::Bot } else { IconName::SquareTerminal };
            rows.push(Row {
                id: ("session", i),
                section: Section::Sessions,
                line: Line {
                    icon,
                    primary: s.title.clone(),
                    secondary: status,
                    hot: s.needs_you,
                    mark: s.mark,
                    worker: s.worker.clone(),
                },
                on_pick: PickerEvent::Jump(s.session),
            });
        }
        for (i, d) in self.displays.iter().enumerate() {
            rows.push(Row {
                id: ("display", i),
                section: Section::Displays,
                line: Line::new(
                    IconName::Monitor,
                    format!("Display {}", d.id),
                    format!("{}×{} @{}× {}Hz", d.w, d.h, d.scale, d.hz),
                ),
                on_pick: PickerEvent::Pick {
                    target: CaptureTarget::Display(d.id),
                    size: (d.w, d.h),
                    title: format!("Display {}", d.id),
                },
            });
        }
        for (i, w) in self.windows.iter().enumerate() {
            let title = if w.title.is_empty() { w.app.clone() } else { w.title.clone() };
            rows.push(Row {
                id: ("window", i),
                section: Section::Windows,
                line: Line::new(IconName::AppWindow, w.app.clone(), title.clone()),
                on_pick: PickerEvent::Pick {
                    target: CaptureTarget::Window(w.id),
                    size: (w.w, w.h),
                    title,
                },
            });
        }
        rows
    }

    /// The rows the field lets through: every word typed is in the row's text.
    fn visible(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| {
                let line = &row.line;
                let worker = line.worker.as_deref().unwrap_or_default();
                matches(&self.query, &format!("{} {} {worker}", line.primary, line.secondary))
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
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(FILTER_PLACEHOLDER));
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
        let s = &theme.surfaces;
        let Line { icon, primary, secondary, hot, mark, worker } = line;
        let secondary_color = if hot { s.warn } else { s.text_muted };
        let icon_ink = if chosen { s.text } else { s.text_muted };
        let (raised, overlay) = (s.raised, s.overlay);
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
            .when(chosen, |el| el.bg(hsla(overlay)))
            .when(!chosen, |el| el.hover(move |st| st.bg(hsla(raised))))
            .active(move |st| st.bg(hsla(overlay)))
            .child(icon_slot(theme, icon, hsla(icon_ink)))
            .child(
                div()
                    .flex_none()
                    .max_w_1_2()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(s.text))
                    .child(SharedString::from(primary)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(hsla(secondary_color))
                    .child(SharedString::from(secondary)),
            )
            .children(worker.map(|worker| {
                div().flex_none().text_color(hsla(s.text_muted)).child(SharedString::from(worker))
            }))
            .children(mark.map(|mark| icons::status_mark(theme, Some(mark))));
        tab_stop(row, s.accent).on_click(cx.listener(move |_this, _ev, _w, cx| {
            cx.emit(on_pick.clone());
        }))
    }

    /// Where the worker's windows will be, until its listing arrives: not a row to pick.
    fn loading_row(&self) -> impl IntoElement {
        let theme = &self.theme;
        let muted = hsla(theme.surfaces.text_muted);
        div()
            .id("picker-loading")
            .debug_selector(|| "picker-loading".to_owned())
            .role(gpui::accesskit::Role::Status)
            .aria_label(LOADING_WINDOWS)
            .w_full()
            .px(px(theme.spacing.md))
            .py(px(theme.spacing.sm))
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .text_color(muted)
            .child(icon_slot(theme, IconName::LoaderCircle, muted))
            .child(LOADING_WINDOWS)
    }

    /// A muted heading over one of the picker's sections.
    fn heading(&self, section: Section) -> impl IntoElement {
        let name = match section {
            Section::Sessions => "picker-heading-sessions",
            Section::Displays => "picker-heading-displays",
            Section::Windows => "picker-heading-windows",
        };
        section_heading(&self.theme, name.into(), section.heading())
            .debug_selector(|| name.to_owned())
    }

    /// Nothing to list: a muted icon over what is missing, in the middle of the list.
    fn empty_state(&self) -> impl IntoElement {
        let theme = &self.theme;
        let muted = hsla(theme.surfaces.text_muted);
        let (icon, text) = if self.query.is_empty() {
            (IconName::AppWindow, NOTHING_TO_JUMP_TO)
        } else {
            (IconName::Search, NOTHING_MATCHES)
        };
        div()
            .id("picker-empty")
            .debug_selector(|| "picker-empty".to_owned())
            .role(gpui::accesskit::Role::Status)
            .aria_label(text)
            .w_full()
            .py(px(theme.spacing.xl))
            .px(px(theme.spacing.md))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(theme.spacing.sm))
            .text_color(muted)
            .child(icons::icon(theme, icon, IconSize::Large, muted))
            .child(text)
    }
}

impl Render for WindowPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_field(window, cx);
        let theme = self.theme.clone();
        let visible = self.visible();
        let chosen = self.selected(visible.len());
        // The listing still on its way stands where the windows will be.
        let mut sections: Vec<Section> = visible.iter().map(|r| r.section).collect();
        if self.loading {
            sections.push(Section::Windows);
        }
        sections.dedup();
        // A heading only where there are two groups to tell apart.
        let grouped = sections.len() > 1;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut section = None;
        for (ix, row) in visible.into_iter().enumerate() {
            if grouped && section != Some(row.section) {
                section = Some(row.section);
                rows.push(self.heading(row.section).into_any_element());
            }
            let Row { id, line, on_pick, .. } = row;
            rows.push(self.row(id, line, on_pick, ix == chosen, cx).into_any_element());
        }
        if self.loading {
            if grouped && section != Some(Section::Windows) {
                rows.push(self.heading(Section::Windows).into_any_element());
            }
            rows.push(self.loading_row().into_any_element());
        }
        let empty = rows.is_empty();
        let title = "Jump to a session, or add a window from the worker";

        crate::kit::backdrop(&theme, window)
            .id("picker-backdrop")
            .track_focus(&self.focus)
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
                crate::kit::dialog(&theme, crate::kit::Overlay::List)
                    .id("picker")
                    .debug_selector(|| "picker".to_owned())
                    .role(gpui::accesskit::Role::Dialog)
                    .aria_label(title)
                    // The title, the field and the rows' text share one left edge: the list's
                    // inset plus a row's.
                    .child(
                        div()
                            .px(px(theme.spacing.lg))
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(theme.surfaces.border))
                            .text_color(hsla(theme.surfaces.text))
                            .font_weight(gpui::FontWeight(slopty_theme::Typography::STRONG_WEIGHT))
                            .child(title),
                    )
                    .children(self.input.as_ref().map(|input| {
                        div()
                            .px(px(theme.spacing.lg))
                            .py(px(theme.spacing.sm))
                            .border_b_1()
                            .border_color(hsla(theme.surfaces.border))
                            .child(Input::new(input).appearance(false).px_0().aria_label("Filter"))
                    }))
                    .child(
                        div()
                            .id("picker-list")
                            .flex_1()
                            .overflow_y_scroll()
                            .p(px(theme.spacing.xs))
                            .children(rows)
                            .when(empty, |el| el.child(self.empty_state())),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use slopty_core::WindowId;

    use super::*;

    #[test]
    fn the_filter_takes_every_word_in_any_order_and_case() {
        assert!(matches("", "anything at all"));
        assert!(matches("build", "fix the build, 2 d ago · /w/slopty"));
        assert!(matches("SLOPTY fix", "fix the build, 2 d ago · /w/slopty"));
        assert!(!matches("fix tests", "fix the build, 2 d ago · /w/slopty"));
    }

    fn window(id: u32, app: &str, title: &str, on_screen: bool) -> WindowInfo {
        WindowInfo {
            id: WindowId(id),
            app: app.to_owned(),
            bundle_id: None,
            title: title.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: 800.0,
            display: 1,
            on_screen,
        }
    }

    /// The listing is sessions first (in the canvas's order), then the worker's displays and
    /// its on-screen windows by app then title, an untitled window under its app's name; a
    /// window off screen (minimised, another Space) is not offered. The filter keeps the rows
    /// whose text holds every word; ↑/↓ wrap over what is visible and ↩ emits the chosen
    /// row's pick, with the ids untouched by the filter.
    #[gpui::test]
    fn sessions_then_screens_in_order_filtered_and_stepped(cx: &mut gpui::TestAppContext) {
        let (s1, s2) = (SessionId::new(), SessionId::new());
        let sessions = vec![
            SessionRow {
                session: s1,
                title: "fix the build".to_owned(),
                status: Some("waiting on you".to_owned()),
                needs_you: true,
                mark: Some(Status::NeedsYou),
                worker: None,
            },
            SessionRow {
                session: s2,
                title: "zsh".to_owned(),
                status: None,
                needs_you: false,
                mark: None,
                worker: None,
            },
        ];
        let windows = vec![
            window(30, "Xcode", "slopty.xcodeproj", true),
            window(20, "Safari", "Rust docs", true),
            window(21, "Safari", "", true),
            window(40, "Music", "Music", false),
        ];
        let displays = vec![DisplayInfo { id: 2, w: 1728.0, h: 1117.0, scale: 2.0, hz: 120.0 }];
        let picker =
            cx.new(|cx| WindowPicker::new(sessions, windows, displays, Theme::default(), cx));
        let picked: Rc<RefCell<Vec<PickerEvent>>> = Rc::default();
        let sink = Rc::clone(&picked);
        cx.update(|cx| {
            cx.subscribe(&picker, move |_, ev: &PickerEvent, _| sink.borrow_mut().push(ev.clone()))
                .detach();
        });

        let lines = |p: &WindowPicker| {
            p.visible()
                .into_iter()
                .map(|r| (r.id, r.line.primary, r.line.secondary, r.line.hot))
                .collect::<Vec<_>>()
        };
        picker.read_with(cx, |p, _| {
            assert_eq!(
                lines(p),
                vec![
                    (("session", 0), "fix the build".to_owned(), "waiting on you".to_owned(), true),
                    (("session", 1), "zsh".to_owned(), String::new(), false),
                    (
                        ("display", 0),
                        "Display 2".to_owned(),
                        "1728×1117 @2× 120Hz".to_owned(),
                        false
                    ),
                    (("window", 0), "Safari".to_owned(), "Safari".to_owned(), false),
                    (("window", 1), "Safari".to_owned(), "Rust docs".to_owned(), false),
                    (("window", 2), "Xcode".to_owned(), "slopty.xcodeproj".to_owned(), false),
                ]
            );
        });

        // ↑ from the top wraps to the last row; ↩ picks it: the Xcode window at its size.
        picker.update(cx, |p, cx| {
            p.step(-1, cx);
            p.pick(cx);
        });
        match picked.borrow_mut().pop() {
            Some(PickerEvent::Pick {
                target: CaptureTarget::Window(WindowId(30)),
                size,
                title,
            }) => {
                assert_eq!((size, title.as_str()), ((1200.0, 800.0), "slopty.xcodeproj"));
            }
            other => panic!("expected the Xcode window, got {other:?}"),
        }

        // The filter narrows to the rows holding every word and keeps their ids; the choice
        // is clamped to what is left, and ↓ from the last visible row wraps to the first.
        picker.update(cx, |p, cx| {
            p.query = "safari docs".to_owned();
            assert_eq!(p.visible().iter().map(|r| r.id).collect::<Vec<_>>(), vec![("window", 1)]);
            p.query = "safari".to_owned();
            p.selected = 0;
            p.step(1, cx);
            p.step(1, cx);
            p.pick(cx);
        });
        match picked.borrow_mut().pop() {
            Some(PickerEvent::Pick {
                target: CaptureTarget::Window(WindowId(21)), title, ..
            }) => {
                assert_eq!(title, "Safari", "an untitled window is named after its app");
            }
            other => panic!("expected the untitled Safari window, got {other:?}"),
        }
        // A session row jumps; a display row picks the display at its point size.
        picker.update(cx, |p, cx| {
            p.query = String::new();
            p.selected = 0;
            p.pick(cx);
            p.selected = 2;
            p.pick(cx);
            p.query = "nothing like this".to_owned();
            p.step(1, cx);
            p.pick(cx);
        });
        let events = picked.borrow();
        assert_eq!(events.len(), 2, "no row, no pick: {events:?}");
        assert!(matches!(&events[0], PickerEvent::Jump(s) if *s == s1), "{events:?}");
        assert!(
            matches!(
                &events[1],
                PickerEvent::Pick { target: CaptureTarget::Display(2), size, title }
                    if *size == (1728.0, 1117.0) && title == "Display 2"
            ),
            "{events:?}"
        );
    }

    fn shell(title: &str) -> SessionRow {
        SessionRow {
            session: SessionId::new(),
            title: title.to_owned(),
            status: None,
            needs_you: false,
            mark: None,
            worker: None,
        }
    }

    /// The headings and states of the picker's last frame, in reading order.
    fn outline(cx: &mut gpui::VisualTestContext) -> Vec<String> {
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter()
            .filter(|n| matches!(n.role.as_str(), "Heading" | "Status"))
            .filter_map(|n| n.label.map(|l| format!("{} {l}", n.role)))
            .collect()
    }

    /// ⌘O shows the picker before the worker answers: the sessions, and a row standing where
    /// the windows will be, which a query that leaves no session does not turn into "Nothing
    /// matches". The listing replaces that row with the displays and windows under their own
    /// headings. With nothing at all to offer the list says so, and a query that leaves nothing
    /// says that instead; a lone group wears no heading.
    #[gpui::test]
    fn the_picker_waits_for_the_listing_and_says_when_nothing_is_left(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let (picker, cx) = cx.add_window_view(|_window, cx| {
            WindowPicker::loading(vec![shell("zsh")], Theme::default(), cx)
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        let loading = format!("Status {LOADING_WINDOWS}");
        assert_eq!(outline(cx), ["Heading Sessions", "Heading Windows", loading.as_str()]);
        assert!(cx.debug_bounds("picker-session-0").is_some(), "the sessions are there at once");

        let picked: Rc<RefCell<Vec<PickerEvent>>> = Rc::default();
        let sink = Rc::clone(&picked);
        cx.update(|_window, cx| {
            cx.subscribe(&picker, move |_, ev: &PickerEvent, _| sink.borrow_mut().push(ev.clone()))
                .detach();
        });
        picker.update(cx, |p, cx| {
            p.query = "safari".to_owned();
            p.pick(cx);
            cx.notify();
        });
        assert!(picked.borrow().is_empty(), "the waiting row is not a pick");
        assert_eq!(outline(cx), [loading.as_str()], "still asking, not empty");

        picker.update(cx, |p, cx| {
            p.query.clear();
            let displays = vec![DisplayInfo { id: 1, w: 1728.0, h: 1117.0, scale: 2.0, hz: 120.0 }];
            p.set_listing(vec![window(20, "Safari", "Rust docs", true)], displays, cx);
        });
        assert!(!picker.read_with(cx, |p, _| p.is_loading()));
        assert_eq!(outline(cx), ["Heading Sessions", "Heading Displays", "Heading Windows"]);
        assert!(cx.debug_bounds("picker-window-0").is_some());

        picker.update(cx, |p, cx| {
            p.query = "safari".to_owned();
            cx.notify();
        });
        assert!(outline(cx).is_empty(), "one group left: no heading");

        picker.update(cx, |p, cx| {
            p.query = "nothing like this".to_owned();
            cx.notify();
        });
        assert_eq!(outline(cx), [format!("Status {NOTHING_MATCHES}")]);

        picker.update(cx, |p, cx| {
            p.query.clear();
            p.sessions.clear();
            p.set_listing(Vec::new(), Vec::new(), cx);
        });
        assert_eq!(outline(cx), [format!("Status {NOTHING_TO_JUMP_TO}")]);
    }
}
