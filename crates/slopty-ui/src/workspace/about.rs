//! Slopty's mark, live, and the About panel it leads.
//!
//! The mark is a prompt on the aislopware grid: nine circles of one size, the chevron `>` lit
//! at (column, row) (0,0), (1,1) and (0,2), the cursor at (2,2), the rest unlit (set back to
//! [`Theme::brand_unlit`]), all in [`slopty_theme::BRAND`] (`docs/decisions/brand.md`). In the
//! app the cursor is the live element: it blinks at the terminal caret's cadence
//! ([`Motion::blink`]) while a worker is reachable, holds steady under Reduce Motion, and sits
//! at the unlit level while no worker is. It is a view of its own, so a blink draws the mark
//! and nothing round it; its clock runs only while it is drawn.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, div, px,
};
use slopty_theme::{Motion, Theme, Typography};

use super::WorkspaceView;
use super::actions::About;
use crate::colors::{hsla, hsla_alpha};
use crate::kit;

/// The lit dots of the chevron, as (column, row).
const CHEVRON: [(usize, usize); 3] = [(0, 0), (1, 1), (0, 2)];
/// The cursor's dot, on the baseline after the chevron.
const CURSOR: (usize, usize) = (2, 2);

/// The widest the About panel grows.
const ABOUT_W: f32 = 320.0;

/// How large a mark is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkSize {
    /// Above the empty workspace's hint: 8 pt dots.
    Page,
    /// Leading the About panel: 16 pt dots.
    Panel,
}

impl MarkSize {
    /// A dot's side and the gap between two, from the spacing scale: a dot twice its gap, as the
    /// mark's art has them.
    const fn dot_and_gap(self, theme: &Theme) -> (f32, f32) {
        let s = theme.spacing;
        match self {
            Self::Page => (s.sm, s.xs),
            Self::Panel => (s.lg, s.sm),
        }
    }
}

/// The mark as a view: nine dots and a cursor with a clock.
pub struct Mark {
    theme: Theme,
    size: MarkSize,
    /// The prefix of its debug selectors: `<name>-mark`, `<name>-cursor`.
    name: &'static str,
    /// A worker is reachable: the cursor is lit (and blinks).
    lit: bool,
    /// The blink's phase: the cursor shows.
    on: bool,
    /// Drawn since the clock last ticked: a mark no longer drawn stops its clock.
    drawn: bool,
    blink: Option<Task<()>>,
}

impl Mark {
    /// A mark of `size`, lit while `lit`.
    #[must_use]
    pub const fn new(theme: Theme, size: MarkSize, name: &'static str, lit: bool) -> Self {
        Self { theme, size, name, lit, on: true, drawn: false, blink: None }
    }

    /// Whether a worker is reachable.
    pub fn set_lit(&mut self, lit: bool, cx: &mut Context<Self>) {
        if self.lit != lit {
            self.lit = lit;
            self.on = true;
            cx.notify();
        }
    }

    /// The theme changed: the unlit level follows the content.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Whether the cursor shows lit now.
    #[must_use]
    pub const fn cursor_lit(&self) -> bool {
        self.lit && self.on
    }

    /// Whether its clock runs.
    #[cfg(test)]
    #[must_use]
    pub const fn blinking(&self) -> bool {
        self.blink.is_some()
    }

    /// Start the clock, when the cursor is lit, the system lets things move, and it is not
    /// running.
    fn blink_if_due(&mut self, cx: &Context<Self>) {
        if !self.lit || self.blink.is_some() || !kit::motion(cx) {
            return;
        }
        self.blink = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Motion::DEFAULT.blink).await;
                if !this.update(cx, Self::tick).unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Half a blink passed: flip the phase. False ends the clock, the cursor steady: the mark
    /// was not drawn since, no worker is reachable, or motion is off.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.drawn || !self.lit || !kit::motion(cx) {
            self.blink = None;
            if !self.on {
                self.on = true;
                cx.notify();
            }
            return false;
        }
        self.drawn = false;
        self.on = !self.on;
        cx.notify();
        true
    }
}

impl Render for Mark {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drawn = true;
        self.blink_if_due(cx);
        let brand = self.theme.surfaces.brand;
        let (lit, unlit) = (hsla(brand), hsla_alpha(brand, self.theme.brand_unlit()));
        let (side, gap) = self.size.dot_and_gap(&self.theme);
        let cursor = self.cursor_lit();
        let name = self.name;
        let rows = (0..3).map(|row| {
            div().flex().gap(px(gap)).children((0..3).map(move |column| {
                let at = (column, row);
                let on = if at == CURSOR { cursor } else { CHEVRON.contains(&at) };
                div()
                    .flex_none()
                    .size(px(side))
                    .rounded_full()
                    .bg(if on { lit } else { unlit })
                    .when(at == CURSOR, |el| el.debug_selector(move || format!("{name}-cursor")))
            }))
        });
        div()
            .debug_selector(move || format!("{name}-mark"))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(gap))
            .children(rows)
    }
}

/// The About panel while it is open.
#[derive(Clone, PartialEq)]
pub(super) struct AboutPanel {
    mark: Entity<Mark>,
    focus: FocusHandle,
}

impl AboutPanel {
    /// The panel's mark.
    #[cfg(test)]
    pub(super) const fn mark(&self) -> &Entity<Mark> {
        &self.mark
    }
}

impl WorkspaceView {
    /// A worker's link is up: the marks' cursors are lit.
    pub(super) fn reachable(&self) -> bool {
        self.workers.values().any(|w| w.link.is_some())
    }

    /// The marks follow whether a worker is reachable.
    pub(super) fn light_marks(&self, cx: &mut Context<Self>) {
        let lit = self.reachable();
        let marks = std::iter::once(&self.empty_mark).chain(self.about.as_ref().map(|a| &a.mark));
        for mark in marks {
            if mark.read(cx).lit != lit {
                mark.update(cx, |m, cx| m.set_lit(lit, cx));
            }
        }
    }

    /// The marks take a new theme.
    pub(super) fn theme_marks(&self, cx: &mut Context<Self>) {
        let theme = self.theme.clone();
        let marks = std::iter::once(&self.empty_mark).chain(self.about.as_ref().map(|a| &a.mark));
        for mark in marks {
            mark.update(cx, |m, cx| m.set_theme(theme.clone(), cx));
        }
    }

    /// The palette's "About Slopty": the panel, with the keyboard in it.
    pub fn about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        let (theme, lit) = (self.theme.clone(), self.reachable());
        let mark = cx.new(|_| Mark::new(theme, MarkSize::Panel, "about", lit));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.about = Some(AboutPanel { mark, focus });
        cx.notify();
    }

    /// Close the About panel and give the keyboard back to the workspace.
    pub(super) fn close_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(gone) = self.about.take() {
            if self.chrome_moves(cx) {
                self.keep_leaving(
                    gone,
                    kit::Pace::Exit.duration(),
                    |this| &mut this.about_leaving,
                    cx,
                );
            }
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Whether the About panel is open.
    #[must_use]
    pub const fn about_open(&self) -> bool {
        self.about.is_some()
    }

    /// The About panel over the window: the mark, the name, and the version and build on one
    /// quiet line. Esc or a click beside it closes it, and it fades out where it stands.
    ///
    /// The palette summons it, so it arrives by fading in where it stands, with no travel.
    pub(super) fn render_about(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let (about, leaving) = match (&self.about, &self.about_leaving) {
            (Some(open), _) => (open, false),
            (None, Some(gone)) => (gone, true),
            (None, None) => return None,
        };
        let theme = &self.theme;
        let (s, spacing) = (theme.surfaces, theme.spacing);
        let (version, build) = crate::settings_form::schema::about();
        let said = format!("Version {version}");
        let panel = kit::dialog(theme, kit::Overlay::List)
            .id("about")
            .debug_selector(|| "about".to_owned())
            .track_focus(&about.focus)
            .role(gpui::accesskit::Role::Dialog)
            .aria_label("About Slopty")
            .aria_description(SharedString::from(format!("{said}, {build}")))
            .max_w(px(ABOUT_W))
            .items_center()
            .p(px(spacing.xl))
            .gap(px(spacing.md))
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_about(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(about.mark.clone())
            .child(
                div()
                    .pt(px(spacing.xs))
                    .text_size(px(theme.typography.title()))
                    .font_weight(gpui::FontWeight(Typography::STRONG_WEIGHT))
                    .text_color(hsla(s.text))
                    .child(kit::APP_NAME),
            )
            .child(
                kit::tabular(kit::meta(div(), theme))
                    .flex()
                    .items_center()
                    .gap(px(spacing.xs))
                    .child(said)
                    .child(kit::separator(theme))
                    .child(build),
            );
        // The dim comes and goes with it ([`kit::Presence`]); leaving, for its moment it still
        // holds the pointer, as it did.
        let root = kit::backdrop(theme, window).id("about-backdrop").occlude();
        let root = if leaving {
            root.child(panel.debug_selector(|| "about-leaving".to_owned()))
        } else {
            root.on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    this.close_about(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(panel)
        };
        Some(kit::presence(root, "about-presence", !leaving).into_any_element())
    }
}
