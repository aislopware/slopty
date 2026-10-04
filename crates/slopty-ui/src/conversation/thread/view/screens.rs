//! The screen the agent drives, offered in the composer's toolbar.
//!
//! It is the window or display the agent's tools last acted on ([`AgentScreen`], named by the
//! worker), which opens beside the thread as a stream the person watches and can take control
//! of.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_proto::thread::AgentScreen;

use super::{ThreadView, ThreadViewEvent};
use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit;

/// The widest a screen's name stands in the toolbar, in points at zoom 1.
const WORDS_MAX: f32 = 160.0;

/// The words a screen goes by in the toolbar: the window's title alone (the device, the page),
/// else its whole label.
#[must_use]
pub fn short(screen: &AgentScreen) -> &str {
    screen.label.split_once(" \u{2014} ").map_or(screen.label.as_str(), |(_app, title)| title)
}

/// A screen's mark: a phone for a simulator, a globe for a browser, a monitor for a display.
#[must_use]
pub const fn mark(kind: &str) -> IconName {
    match kind.as_bytes() {
        b"simulator" => IconName::Smartphone,
        b"browser" => IconName::Globe,
        b"desktop" => IconName::Monitor,
        _ => IconName::AppWindow,
    }
}

impl ThreadView {
    /// The screen the agent drove last, while the worker still offers it.
    pub(super) fn agent_screen(&self, cx: &gpui::App) -> Option<AgentScreen> {
        self.state(cx).and_then(|s| s.screens.first().cloned())
    }

    /// Open the screen the agent drove last beside the thread.
    pub(super) fn watch_screen(&self, cx: &mut Context<Self>) {
        if let Some(screen) = self.agent_screen(cx) {
            cx.emit(ThreadViewEvent::Watch { thread: self.thread, screen });
        }
    }

    /// In the toolbar's facts: the screen the agent drives, which opens it beside the thread.
    /// Its mark and its name where the thread is wide, the name no wider than [`WORDS_MAX`];
    /// its mark alone where it is narrow, the name in a hint, so the checkout keeps its room.
    pub(super) fn screen_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let screen = self.agent_screen(cx)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let words = short(&screen).to_owned();
        let label = format!("Watch {words}");
        let chip = div()
            .id("thread-screen")
            .debug_selector(|| "thread-screen".to_owned())
            .role(Role::Button)
            .aria_label(SharedString::from(label.clone()))
            .flex_none()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .px(self.z(theme.spacing.xs))
            .rounded(self.z(theme.radii.xs))
            .cursor_pointer()
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text_secondary)))
            .active(move |el| el.bg(hsla(s.pressed)))
            .child(
                crate::icons::icon(theme, mark(&screen.kind), IconSize::Inline, hsla(s.text_muted))
                    .size(self.z(theme.typography.small())),
            )
            .on_click(cx.listener(|this, _ev, _w, cx| this.watch_screen(cx)));
        let chip = if self.width >= super::WIDE {
            chip.child(div().min_w_0().max_w(self.z(WORDS_MAX)).child(kit::fit_label(
                "thread-screen-words",
                words,
                theme,
            )))
        } else {
            let hint_theme = theme.clone();
            kit::hint_timing(chip).tooltip(move |_window, cx| {
                let theme = std::rc::Rc::new(hint_theme.clone());
                cx.new(|_| kit::Hint::new(label.clone(), "", theme)).into()
            })
        };
        Some(crate::a11y::tab_stop(chip, s.accent).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{WallMs, WindowId};
    use slopty_proto::screen::CaptureTarget;
    use slopty_proto::thread::AgentScreen;

    use super::{mark, short};
    use crate::icons::IconName;

    #[test]
    fn a_screen_goes_by_its_windows_title() {
        let screen = |kind: &str, label: &str| AgentScreen {
            target: CaptureTarget::Window(WindowId(1)),
            kind: kind.to_owned(),
            label: label.to_owned(),
            used_ms: WallMs::ZERO,
        };
        let sim = screen(AgentScreen::SIMULATOR, "Simulator \u{2014} iPhone 17 Pro");
        assert_eq!(short(&sim), "iPhone 17 Pro");
        assert_eq!(short(&screen(AgentScreen::DESKTOP, "Desktop")), "Desktop");
        assert_eq!(mark(&sim.kind), IconName::Smartphone);
        assert_eq!(mark("anything else"), IconName::AppWindow);
    }
}
