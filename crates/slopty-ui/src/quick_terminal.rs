//! The quick terminal's panel: a terminal that slides down from the top of the screen over any
//! app, and back up out of the way.
//!
//! The panel draws a terminal the workspace owns ([`TerminalView`], the one element every tile
//! draws with) or, until its shell is attached, a line saying why there is none. It never owns a
//! session: showing and hiding move the window, not the terminal, so what ran in it is there
//! the next time (`docs/decisions/ui.md`, "A quick terminal slides down from the top of the
//! screen").
//!
//! Shown, the sheet slides down from above the window's top on the sheet's pace and curve
//! ([`kit::Pace::Sheet`]); hidden, it slides back up on the settle pace, and the window is
//! ordered out once it is gone. Under Reduce Motion it appears and goes at once. The window is
//! clear where the sheet is not, so the slide shows the desktop behind it.
//!
//! Drawing it changes nothing: the window's shadow follows the slide from the frames' own
//! callbacks, and a show is timed from the window's presentation reports.

use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, App, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    div, px,
};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::kit::{self, Pace};
use crate::terminal::TerminalView;

/// The panel's key context.
pub(crate) const CTX: &str = "QuickTerminal";

/// What the panel holds.
#[derive(Clone)]
pub(crate) enum Body {
    /// The quick terminal's shell.
    Terminal(Entity<TerminalView>),
    /// No shell yet, and why: a line, and a quieter one under it.
    Waiting(SharedString, Option<SharedString>),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal(view) => f.debug_tuple("Terminal").field(&view.entity_id()).finish(),
            Self::Waiting(line, _) => f.debug_tuple("Waiting").field(line).finish(),
        }
    }
}

/// How the quick terminal sits: the settings' `[quick_terminal]`, less the chord, which is the
/// app's to register.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct QuickConfig {
    /// Its height, as a share of the screen under the pointer (0 to 1).
    pub height: f32,
    /// It slides away once another window or app takes the keyboard.
    pub autohide: bool,
    /// A show takes the keyboard. The self-test's panel does not: the machine's keyboard is
    /// the person's.
    pub keyboard: bool,
}

impl Default for QuickConfig {
    fn default() -> Self {
        Self { height: 0.4, autohide: true, keyboard: true }
    }
}

/// The root of the quick terminal's window.
pub(crate) struct QuickTerminalView {
    theme: Theme,
    body: Body,
    /// Shown, or on its way up and out.
    shown: bool,
    config: QuickConfig,
    /// Bumped at every show and hide, so each slide plays from its start.
    turn: u64,
    focus: FocusHandle,
    /// The order-out that follows the slide up.
    out: Option<Task<()>>,
    /// When the show now under way was asked for (the chord's press, or the palette's
    /// command), until the first frame on the glass after the window came in front is timed.
    asked: Option<Instant>,
    /// The window's presentation reports, while a show waits to be timed.
    timing: Option<Subscription>,
    /// The window as AppKit holds it, dressed on the first show.
    #[cfg(target_os = "macos")]
    panel: Option<slopty_platform::panel::Panel>,
    /// Until when the sheet slides in: the window's shadow follows it frame by frame until
    /// then, and never on a frame that only redraws the shell.
    #[cfg(target_os = "macos")]
    sliding: Option<Instant>,
    /// Which slide the shadow follows now, so a new show's does not run beside the last one's.
    #[cfg(target_os = "macos")]
    shadowed: u64,
    _activation: Subscription,
    /// What it follows besides its window: the workspace that owns its shell.
    follow: Vec<Subscription>,
}

impl std::fmt::Debug for QuickTerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickTerminalView")
            .field("body", &self.body)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

impl QuickTerminalView {
    /// The panel of `window`, holding `body`, hidden until [`Self::show`].
    pub(crate) fn new(
        theme: Theme,
        body: Body,
        config: QuickConfig,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Self {
        let activation = cx.observe_window_activation(window, |this: &mut Self, window, cx| {
            if this.config.autohide && this.shown && !window.is_window_active() {
                this.hide(window, cx);
            }
        });
        Self {
            theme,
            body,
            shown: false,
            config,
            turn: 0,
            focus: cx.focus_handle(),
            out: None,
            asked: None,
            timing: None,
            #[cfg(target_os = "macos")]
            panel: None,
            #[cfg(target_os = "macos")]
            sliding: None,
            #[cfg(target_os = "macos")]
            shadowed: 0,
            _activation: activation,
            follow: Vec::new(),
        }
    }

    /// Keep `subscription` for as long as the panel lives.
    pub(crate) fn keep(&mut self, subscription: Subscription) {
        self.follow.push(subscription);
    }

    /// Whether it is shown (or sliding in).
    pub(crate) const fn is_shown(&self) -> bool {
        self.shown
    }

    /// What it holds.
    #[cfg(test)]
    pub(crate) const fn body(&self) -> &Body {
        &self.body
    }

    /// Hold `body` from now on; a new terminal takes the keyboard while the panel is shown.
    pub(crate) fn set_body(&mut self, body: Body, window: &mut Window, cx: &mut Context<Self>) {
        let same = match (&self.body, &body) {
            (Body::Terminal(a), Body::Terminal(b)) => a.entity_id() == b.entity_id(),
            (Body::Waiting(a, x), Body::Waiting(b, y)) => a == b && x == y,
            _ => false,
        };
        if same {
            return;
        }
        self.body = body;
        if self.shown {
            self.take_keyboard(window, cx);
        }
        cx.notify();
    }

    /// Follow the theme and the settings.
    pub(crate) fn configure(&mut self, theme: &Theme, config: QuickConfig, cx: &mut Context<Self>) {
        if self.theme != *theme || self.config != config {
            self.theme = theme.clone();
            self.config = config;
            cx.notify();
        }
    }

    /// Slide in along the top of the screen under the pointer and take the keyboard, leaving
    /// whichever app is in front in front. `asked` is when it was asked for, for the timing.
    pub(crate) fn show(&mut self, asked: Instant, window: &mut Window, cx: &mut Context<Self>) {
        self.out = None;
        if !self.shown {
            self.turn = self.turn.wrapping_add(1);
            self.asked = Some(asked);
            #[cfg(target_os = "macos")]
            {
                let slide = if kit::motion(cx) { Pace::Sheet.duration() } else { Duration::ZERO };
                self.sliding = Instant::now().checked_add(slide);
            }
        }
        self.shown = true;
        self.order_in(window, cx);
        self.take_keyboard(window, cx);
        cx.notify();
    }

    /// Slide up out of view; the window goes once the slide has played.
    pub(crate) fn hide(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.turn = self.turn.wrapping_add(1);
        self.asked = None;
        self.timing = None;
        let slide = if kit::motion(cx) { Pace::Settle.duration() } else { Duration::ZERO };
        self.out = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(slide).await;
            #[cfg(target_os = "macos")]
            if let Ok(Some(panel)) =
                this.update(cx, |this, _cx| if this.shown { None } else { this.panel.clone() })
            {
                panel.hide();
            }
            #[cfg(not(target_os = "macos"))]
            drop(this);
        }));
        cx.notify();
    }

    fn take_keyboard(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.body {
            Body::Terminal(view) => window.focus(&view.read(cx).focus_handle(cx), cx),
            Body::Waiting(..) => window.focus(&self.focus, cx),
        }
    }

    /// Put the window on the screen under the pointer and in front of every app. Off the Mac,
    /// and in the headless tests, GPUI's own activation stands in.
    ///
    /// AppKit tells GPUI of a new frame or the keyboard from inside the call that changes it,
    /// and GPUI drops what arrives while the app is being updated; so the panel is moved on the
    /// next turn of the main run loop, outside any update.
    fn order_in(&mut self, window: &Window, cx: &Context<Self>) {
        #[cfg(target_os = "macos")]
        if !cfg!(test) {
            let dress = self.panel.is_none();
            if dress {
                self.panel = native_panel(window);
            }
            if let Some(panel) = self.panel.clone() {
                let QuickConfig { height, keyboard, .. } = self.config;
                let handle = window.window_handle();
                cx.spawn(async move |this, cx| {
                    if dress {
                        panel.dress();
                    }
                    let _placed = panel.place(f64::from(height));
                    panel.show(keyboard);
                    let fronted = Instant::now();
                    let _gone = handle.update(cx, |_root, window, cx| {
                        let _gone = this.update(cx, |this, cx| {
                            if this.shown {
                                this.follow_slide(window, cx);
                                this.time_show(fronted, window, cx);
                                cx.notify();
                            }
                        });
                    });
                })
                .detach();
                return;
            }
        }
        window.activate_window();
        self.time_show(Instant::now(), window, cx);
    }

    /// Keep the window's shadow on the sheet while it slides in: once a frame, from the frame's
    /// own callback, until the slide has played.
    #[cfg(target_os = "macos")]
    fn follow_slide(&mut self, window: &Window, cx: &Context<Self>) {
        self.shadowed = self.turn;
        Self::trail_the_sheet(cx.entity().downgrade(), self.turn, window);
    }

    #[cfg(target_os = "macos")]
    fn trail_the_sheet(this: gpui::WeakEntity<Self>, turn: u64, window: &Window) {
        window.on_next_frame(move |window, cx| {
            let again = this
                .update(cx, |this, _cx| {
                    let (Some(until), Some(panel)) = (this.sliding, &this.panel) else {
                        return false;
                    };
                    if this.shadowed != turn {
                        return false;
                    }
                    panel.refresh_shadow();
                    if Instant::now() >= until {
                        this.sliding = None;
                        return false;
                    }
                    true
                })
                .unwrap_or(false);
            if again {
                Self::trail_the_sheet(this, turn, window);
            }
        });
    }

    /// Time the show under way from when it was asked for: to the window in front
    /// (`fronted`), to the first frame handed to the GPU after that, and to a frame on the glass,
    /// from the window's presentation reports (a report may come without the glass).
    fn time_show(&mut self, fronted: Instant, window: &Window, cx: &Context<Self>) {
        let Some(asked) = self.asked.take() else { return };
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let watched = cx.entity().downgrade();
        let mut painted = false;
        let reports = window.on_frame_presented(move |frame, _window, cx| {
            if frame.submitted_at < fronted {
                return;
            }
            if !painted {
                painted = true;
                tracing::info!(
                    target: "slopty::quick_terminal",
                    asked_to_front_ms = ms(fronted.saturating_duration_since(asked)),
                    asked_to_paint_ms = ms(frame.submitted_at.saturating_duration_since(asked)),
                    "quick terminal shown",
                );
            }
            let Some(presented) = frame.presented_at else { return };
            tracing::info!(
                target: "slopty::quick_terminal",
                asked_to_glass_ms = ms(presented.saturating_duration_since(asked)),
                "quick terminal on the glass",
            );
            let _done = watched.update(cx, |this, _| this.timing = None);
        });
        self.timing = Some(reports);
    }

    /// The sheet's body: the terminal, or the line saying why there is none.
    fn body_element(&self) -> gpui::AnyElement {
        match &self.body {
            Body::Terminal(view) => view.clone().into_any_element(),
            Body::Waiting(line, detail) => {
                let theme = &self.theme;
                let mark = kit::notice_mark(theme, crate::icons::IconName::SquareTerminal, 1.0);
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(kit::notice(theme, 1.0, mark, line.clone(), detail.clone()))
                    .into_any_element()
            }
        }
    }
}

impl Focusable for QuickTerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for QuickTerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let s = &theme.surfaces;
        let tall = f32::from(window.viewport_size().height);
        let (shown, turn) = (self.shown, self.turn);
        let sheet = div()
            .id("quick-sheet")
            .debug_selector(|| "quick-sheet".to_owned())
            .absolute()
            .left_0()
            .w_full()
            .h(px(tall))
            .flex()
            .flex_col()
            .bg(hsla(theme.content()))
            .border_b_1()
            .border_color(hsla(s.border))
            .rounded_b(px(theme.radii.lg))
            .pb(px(theme.radii.lg))
            .overflow_hidden()
            .child(div().flex_1().min_h_0().w_full().child(self.body_element()));
        let sheet = if kit::motion(cx) {
            let pace = if shown { Pace::Sheet } else { Pace::Settle };
            sheet
                .with_animation(("quick-slide", turn), pace.animation(), move |el, t| {
                    let down = if shown { t } else { 1.0 - t };
                    el.top(px(-tall * (1.0 - down)))
                })
                .into_any_element()
        } else {
            sheet.top(px(if shown { 0.0 } else { -tall })).into_any_element()
        };
        div()
            .id("quick-terminal")
            .key_context(CTX)
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .overflow_hidden()
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text))
            // Esc in the terminal is the program's; with no terminal holding the keyboard it
            // puts the panel away.
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.focus.is_focused(window) {
                    this.hide(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(sheet)
    }
}

/// The AppKit window `window` draws into.
#[cfg(target_os = "macos")]
fn native_panel(window: &Window) -> Option<slopty_platform::panel::Panel> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::AppKit(handle) => slopty_platform::panel::Panel::of_view(handle.ns_view),
        _ => None,
    }
}
