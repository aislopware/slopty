//! The component preview, Zed's: every part of the design system on one page.
//!
//! Each is the real component in a real state, so a change to a token or a component is seen
//! everywhere it lands at once and a design review reads one picture rather than twenty.
//!
//! The work's surfaces are the views themselves over the samples in `samples`: a thread
//! mid-turn with its plan, its calls, a diff, a notice and a question, over its composer and
//! its foot; a board with a task in every lane; a turn's review; and the settings' own form.
//! Beside them stand the controls in each state they take: the buttons of every kind, the icon
//! buttons and toggles, the send at rest and armed, tick boxes, fields, a menu, the state marks,
//! the lanes' phases and pills, key caps and progress. The workspace shows it in the panes'
//! place, as it shows the settings, from the palette ("Component preview"), with a toast up
//! while it is.

mod samples;

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, Div, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use gpui_kit::component::input::{Input, InputState};
use slopty_kit::colors::hsla;
use slopty_kit::icons::{Phase, Status, Symbol, status_mark};
use slopty_kit::kit::menu::{Mark, Menu, MenuItem, MenuPanel};
use slopty_kit::kit::progress::{Bar, Progress};
use slopty_kit::kit::{self, ButtonKind};
use slopty_proto::thread::Cursor;
use slopty_proto::thread::wire::ThreadFrame;
use slopty_theme::Theme;

use crate::conversation::thread::hub::ThreadHub;
use crate::conversation::thread::view::ThreadView;
use crate::project::ProjectView;
use crate::review::ReviewView;
use crate::settings_form::SettingsForm;

/// The page's name, in the palette and to a screen reader.
pub const COMPONENT_PREVIEW: &str = "Component preview";

/// The controls' column: a settings group's width, so the form in it reads as on its page.
const CONTROLS: f32 = 360.0;
/// A line's whole width in the controls' column, inside its pads.
const WHOLE: f32 = 320.0;
/// Half of [`WHOLE`], a field's or a bar's, two to a line.
const HALF: f32 = 150.0;
/// The settings' cell: the appearance group's switches, choices and steppers.
const SETTINGS_TALL: f32 = 300.0;

/// What the page asks of the workspace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentPreviewEvent {
    /// Its close was pressed: the panes again.
    Close,
}

/// The page ([module docs](self)).
pub struct ComponentPreview {
    theme: Theme,
    thread: Entity<ThreadView>,
    review: Entity<ReviewView>,
    board: Option<Entity<ProjectView>>,
    settings: Entity<SettingsForm>,
    empty: Entity<InputState>,
    filled: Entity<InputState>,
    /// The sizes last handed to the thread and the review, which lay out for the room a tile
    /// gives them, as the workspace hands a tile's.
    handed: Rc<Cell<[(f32, f32); 2]>>,
    /// The samples' worker, which nothing links: it holds the thread and its review.
    _hub: Entity<ThreadHub>,
}

impl std::fmt::Debug for ComponentPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentPreview").finish_non_exhaustive()
    }
}

impl EventEmitter<ComponentPreviewEvent> for ComponentPreview {}

impl ComponentPreview {
    /// The page in `theme`, its views over the samples.
    pub fn new(theme: Theme, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hub = cx.new(|_| ThreadHub::new("studio".to_owned(), None));
        hub.update(cx, ThreadHub::connected);
        let state = samples::thread();
        let id = state.meta.id;
        let thread = cx.new(|cx| ThreadView::new(hub.clone(), id, theme.clone(), window, cx));
        let review = cx.new(|cx| ReviewView::new(hub.clone(), id, theme.clone(), window, cx));
        // As the worker answers: the thread, then, once the review has asked for it, the turn's
        // review.
        hub.update(cx, |hub, cx| {
            let cursor = Cursor { epoch: 1, seq: 1 };
            hub.frame(id, ThreadFrame::Snapshot { cursor, state: Box::new(state) }, cx);
        });
        hub.update(cx, |hub, cx| {
            hub.frame(id, ThreadFrame::Review(Box::new(samples::review())), cx);
        });
        let board = samples::project_id().zip(samples::board()).map(|(project, seen)| {
            cx.new(|cx| {
                let mut board = ProjectView::new(project, theme.clone(), cx);
                board.set_seen(seen, cx);
                board
            })
        });
        let settings = cx.new(|cx| {
            let mut form = SettingsForm::new("", theme.clone(), window, cx);
            form.set_aside(true, cx);
            form
        });
        let empty = cx.new(|cx| InputState::new(window, cx).placeholder("Branch name"));
        let filled = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Branch name");
            input.set_value("atlas/replayed-refresh", window, cx);
            input
        });
        Self {
            theme,
            thread,
            review,
            board,
            settings,
            empty,
            filled,
            handed: Rc::default(),
            _hub: hub,
        }
    }

    /// Draw in `theme` from now on, the views in it too.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        self.thread.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        self.review.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        if let Some(board) = &self.board {
            board.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        self.settings.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        self.theme = theme;
        cx.notify();
    }

    /// A surface under its name, in a frame on the one structural line.
    fn cell(&self, id: &'static str, name: &'static str, body: impl IntoElement) -> Div {
        let (s, spacing) = (self.theme.surfaces, self.theme.spacing);
        div()
            .debug_selector(move || format!("components-{id}"))
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .child(kit::label(&self.theme, name))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .overflow_hidden()
                    .rounded(px(self.theme.radii.md))
                    .border(kit::HAIR)
                    .border_color(hsla(s.stroke))
                    .bg(hsla(s.ground))
                    .child(body),
            )
    }

    /// `body`, handed its room as it is laid out: the room of cell `which` of [`Self::handed`],
    /// told to `hand` once it changes, after the frame.
    fn sized(
        &self,
        which: usize,
        body: impl IntoElement,
        hand: impl Fn(f32, f32, &mut App) + 'static,
    ) -> AnyElement {
        let handed = Rc::clone(&self.handed);
        let hand = Rc::new(hand);
        div()
            .relative()
            .size_full()
            .flex()
            .child(body)
            .child(
                gpui::canvas(
                    move |bounds, window, cx| {
                        let room = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                        let mut sizes = handed.get();
                        let Some(was) = sizes.get_mut(which) else { return };
                        if *was == room {
                            return;
                        }
                        *was = room;
                        handed.set(sizes);
                        let hand = Rc::clone(&hand);
                        window.defer(cx, move |_window, cx| hand(room.0, room.1, cx));
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .inset_0(),
            )
            .into_any_element()
    }

    /// One line of the controls: its name, then the controls in it.
    fn line(&self, name: &'static str, controls: Vec<AnyElement>) -> Div {
        let spacing = self.theme.spacing;
        div()
            .flex()
            .flex_col()
            .gap(px(spacing.xs))
            .child(kit::meta(div(), &self.theme).child(name))
            .child(
                div().flex().flex_wrap().items_center().gap(px(spacing.sm)).children(
                    // Each in a place of its own: a mark or a field keeps one name wherever it
                    // is drawn, so a line of them would name its parts alike.
                    controls.into_iter().enumerate().map(|(ix, control)| {
                        div().id((name, ix)).flex().items_center().child(control)
                    }),
                ),
            )
    }

    /// The controls, each in the states it takes.
    fn controls(&self, cx: &Context<Self>) -> gpui::Stateful<Div> {
        let theme = &self.theme;
        let s = theme.surfaces;
        let spacing = theme.spacing;
        let buttons = [
            ("components-primary", "Merge", ButtonKind::Primary),
            ("components-secondary", "Review", ButtonKind::Secondary),
            ("components-ghost", "Cancel", ButtonKind::Ghost),
            ("components-destructive", "Remove", ButtonKind::Destructive),
            ("components-link", "Open in editor", ButtonKind::Link),
        ]
        .into_iter()
        .map(|(id, words, kind)| kit::button(theme, id, words, kind).into_any_element())
        .collect();
        let icons = vec![
            kit::icon_button(theme, "components-icon", Symbol::Plus, "New agent")
                .into_any_element(),
            kit::icon_toggle(
                theme,
                "components-toggle-off",
                Symbol::SidebarLeft,
                "Navigator",
                false,
            )
            .into_any_element(),
            kit::icon_toggle(theme, "components-toggle-on", Symbol::SidebarLeft, "Navigator", true)
                .into_any_element(),
            kit::text_toggle(theme, "components-case-off", "Aa", "Match case", false)
                .into_any_element(),
            kit::text_toggle(theme, "components-case-on", "Aa", "Match case", true)
                .into_any_element(),
            kit::icon_button_inked(theme, "components-bell", Symbol::Bell, "Needs you", s.warn)
                .into_any_element(),
            kit::close_box(theme, "components-close", "Close").into_any_element(),
            kit::message::send_control(theme, "components-send", Symbol::Send, "Send", false)
                .into_any_element(),
            kit::message::send_control(theme, "components-send-armed", Symbol::Send, "Send", true)
                .into_any_element(),
        ];
        let ticks = vec![
            kit::tick_box(theme, false).into_any_element(),
            kit::tick_box(theme, true).into_any_element(),
            kit::key_cap(theme, "cmd-k").into_any_element(),
            kit::key_cap(theme, "cmd-shift-p").into_any_element(),
            kit::key_cap(theme, "enter").into_any_element(),
        ];
        let field = |input: &Entity<InputState>, name: &'static str| {
            div().w(px(HALF)).child(Input::new(input).aria_label(name)).into_any_element()
        };
        let fields = vec![field(&self.empty, "Empty field"), field(&self.filled, "Filled field")];
        let search = vec![
            kit::search_field(theme, false)
                .w(px(WHOLE))
                .child(kit::meta(div(), theme).child("Filter"))
                .into_any_element(),
        ];
        let states = [
            Status::Idle,
            Status::Running,
            Status::Working,
            Status::NeedsYou,
            Status::Done,
            Status::Failed,
            Status::Away,
        ]
        .into_iter()
        .map(|status| status_mark(theme, Some(status)).into_any_element())
        .collect();
        let side = px(theme.icon());
        let phases = [
            Phase::NeedsYou,
            Phase::Failed,
            Phase::Working,
            Phase::Waiting,
            Phase::UpNext,
            Phase::VERIFYING,
            Phase::ReadyToMerge,
            Phase::Merged,
        ]
        .into_iter()
        .map(|phase| phase.glyph(theme, side))
        .collect();
        let pills = [("Needs you", s.warn), ("Working", s.working), ("Failed", s.error)]
            .into_iter()
            .map(|(words, tone)| kit::pill(theme, tone).child(words).into_any_element())
            .collect();
        let progress = vec![
            div()
                .w(px(HALF))
                .child(Bar::new(theme, "components-bar", Progress::Share(0.6)).label("Upload"))
                .into_any_element(),
            div()
                .w(px(HALF))
                .child(Bar::new(theme, "components-failed", Progress::Failed(Some(0.3))))
                .into_any_element(),
        ];
        let menu = Rc::new(
            Menu::new()
                .item(
                    MenuItem::new("split", "Split right", |_, _| {})
                        .icon(Symbol::RectangleSplit3x1),
                )
                .item(MenuItem::new("rename", "Name this tile", |_, _| {}).detail("F2"))
                .item(MenuItem::new("sound", "Sound", |_, _| {}).mark(Mark::Check(true))),
        );
        let menu = MenuPanel::new("components-menu", "Menu", menu, theme, |_, _| {}).inert(true);
        div()
            .id("components-controls")
            .debug_selector(|| "components-controls".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(spacing.md))
            .p(px(spacing.md))
            .child(self.line("Buttons", buttons))
            .child(self.line("Icon buttons, toggles and send", icons))
            .child(self.line("Tick boxes and keys", ticks))
            .child(self.line("Fields", fields))
            .child(self.line("Search", search))
            .child(self.line("States", states))
            .child(self.line("Phases", phases))
            .child(self.line("Pills", pills))
            .child(self.line("Progress", progress))
            .child(self.line("Menu", vec![menu.into_any_element()]))
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.stop_propagation()))
    }
}

impl Render for ComponentPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let close = kit::button(&theme, "components-done", "Done", ButtonKind::Ghost)
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ComponentPreviewEvent::Close)));
        let head = div()
            .flex_none()
            .h(px(theme.density.item))
            .flex()
            .items_center()
            // Done stands by the title: the window's notices take the trailing end up here.
            .gap(px(spacing.sm))
            .child(kit::title(&theme, COMPONENT_PREVIEW))
            .child(close);
        let board: AnyElement = match &self.board {
            Some(board) => board.clone().into_any_element(),
            None => gpui::Empty.into_any_element(),
        };
        let shown = self.thread.clone();
        let thread = self.sized(0, self.thread.clone(), move |width, _height, cx| {
            shown.update(cx, |v, cx| v.set_layout(width, cx));
        });
        let shown = self.review.clone();
        let review = self.sized(1, self.review.clone(), move |width, height, cx| {
            shown.update(cx, |v, cx| v.set_layout(width, height, cx));
        });
        let work = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .gap(px(spacing.lg))
            .child(self.cell("thread", "Thread", thread).flex_1())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.lg))
                    .child(self.cell("board", "Board", board).flex_1())
                    .child(self.cell("review", "Review", review).flex_1()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(CONTROLS))
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(spacing.lg))
                    .child(
                        self.cell("settings", "Settings", self.settings.clone())
                            .flex_none()
                            .h(px(SETTINGS_TALL)),
                    )
                    .child(self.cell("kit", "Controls", self.controls(cx)).flex_1()),
            );
        div()
            .id("components")
            .debug_selector(|| "components".to_owned())
            .role(gpui::accesskit::Role::Group)
            .aria_label(SharedString::from(COMPONENT_PREVIEW))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(spacing.sm))
            .px(px(spacing.lg))
            .pb(px(spacing.lg))
            .bg(hsla(s.ground))
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.ui_size))
            .text_color(hsla(s.text))
            .child(head)
            .child(work)
    }
}
