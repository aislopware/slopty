//! `WindowPicker`: jump to a session on the canvas, or choose a host window or display to put
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
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};

/// What the field says before anything is typed.
pub(crate) const FILTER_PLACEHOLDER: &str = "Type to filter";

/// The picker's empty states: nothing to offer at all, and nothing left after the query.
pub(crate) const NOTHING_TO_JUMP_TO: &str = "Nothing on the canvas or shareable on the host";
pub(crate) const NOTHING_MATCHES: &str = "Nothing matches";

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
            theme,
            focus: cx.focus_handle(),
            input: None,
            events: None,
            query: String::new(),
            selected: 0,
        }
    }

    /// Every row in its order, before the filter: sessions, then the host's displays and
    /// windows.
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
                    title: format!("Display {}", d.id),
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
        rows
    }

    /// The rows the field lets through: every word typed is in the row's text.
    fn visible(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| {
                matches(&self.query, &format!("{} {}", row.line.primary, row.line.secondary))
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
            .when(chosen, |el| el.bg(hsla_alpha(theme.surfaces.accent, alpha::TINT)))
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
                    Section::Screens => {}
                }
            }
            let Row { id, line, on_pick, .. } = row;
            rows.push(self.row(id, line, on_pick, ix == chosen, cx).into_any_element());
        }
        let empty = rows.is_empty();
        let title = "Jump to a session, or add a window from the host";
        let nothing = if self.query.is_empty() { NOTHING_TO_JUMP_TO } else { NOTHING_MATCHES };

        crate::kit::backdrop(&theme)
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
                            .child(Input::new(input).appearance(false).aria_label("Filter"))
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

    /// The listing is sessions first (in the canvas's order), then the host's displays and
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
            },
            SessionRow { session: s2, title: "zsh".to_owned(), status: None, needs_you: false },
        ];
        let windows = vec![
            window(30, "Xcode", "slopty.xcodeproj", true),
            window(20, "Safari", "Rust docs", true),
            window(21, "Safari", "", true),
            window(40, "Music", "Music", false),
        ];
        let displays =
            vec![DisplayInfo { id: 2, w: 1728.0, h: 1117.0, scale: 2.0, hz: 120.0, hdr: true }];
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
}
