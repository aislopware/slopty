//! A screen an agent drives: the view watches it, and the person's input goes nowhere, until
//! they take control.
//!
//! The workspace names the driver of a tile it opened from a thread ([`ScreenView::set_driver`]).
//! While the agent drives, a pill at the foot of the picture says so and offers
//! [`TAKE_CONTROL`]; the pointer drawn is the worker's, and no move, click, key or scroll leaves
//! this device, so a stray hand cannot fight the agent for the screen. Taking control is the
//! person's word: the agent's turn under way stops through its own door ([`Driver::take`]),
//! and from then the stream is the person's like any other, until they hand it back.

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::{
    App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_proto::thread::AgentId;

use super::ScreenView;
use crate::colors::hsla;
use crate::kit;

/// The pill's button that gives the person the screen.
pub const TAKE_CONTROL: &str = "Take control";
/// The pill's button that gives the screen back to the agent to drive, the person watching.
pub const HAND_BACK: &str = "Hand back";
/// What the pill says while the person has the screen.
pub const IN_CONTROL: &str = "You have control";

/// The agent that drives a stream's screen, as its thread says.
#[derive(Clone)]
pub struct Driver {
    /// The agent.
    pub agent: AgentId,
    /// Its name for people.
    pub name: String,
    /// Whether its turn is under way.
    pub working: bool,
    /// Stop the agent's turn under way, when one is: the person takes control.
    pub take: Rc<dyn Fn(&mut App)>,
}

impl Driver {
    /// Whether `other` says the same of the agent; the stop is the same door whatever it is.
    #[must_use]
    pub fn same(&self, other: &Self) -> bool {
        self.agent == other.agent && self.name == other.name && self.working == other.working
    }
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Driver")
            .field("agent", &self.agent)
            .field("name", &self.name)
            .field("working", &self.working)
            .finish_non_exhaustive()
    }
}

impl ScreenView {
    /// The agent that drives this screen, or `None` for a screen of the person's own. A new
    /// driver starts watched; the same one keeps whoever has control.
    pub fn set_driver(&mut self, driver: Option<Driver>, cx: &mut Context<Self>) {
        let same_agent = match (&self.driver, &driver) {
            (Some(was), Some(now)) => was.agent == now.agent,
            (None, None) => true,
            _ => false,
        };
        if !same_agent {
            self.control = false;
        }
        self.driver = driver;
        cx.notify();
    }

    /// The agent that drives this screen, as last named.
    #[must_use]
    pub const fn driver(&self) -> Option<&Driver> {
        self.driver.as_ref()
    }

    /// Whether the agent drives and the person only watches: no input leaves this device.
    #[must_use]
    pub const fn watching(&self) -> bool {
        self.driver.is_some() && !self.control
    }

    /// The person takes the screen: the agent's turn stops, and their input goes to it.
    pub fn take_control(&mut self, cx: &mut Context<Self>) {
        if let Some(driver) = &self.driver {
            let take = Rc::clone(&driver.take);
            take(cx);
        }
        self.control = true;
        cx.notify();
    }

    /// The person gives the screen back: they watch again.
    pub fn hand_back(&mut self, cx: &mut Context<Self>) {
        self.control = false;
        cx.notify();
    }

    /// At the foot of the picture, while an agent drives it: who drives, and the way to take or
    /// give back control. It floats over the picture, lifted, as the zoom's readout does.
    pub(super) fn driver_pill(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let driver = self.driver.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let (words, button) = if self.control {
            (IN_CONTROL.to_owned(), HAND_BACK)
        } else {
            (format!("{} is driving", driver.name), TAKE_CONTROL)
        };
        let control = self.control;
        let act = kit::button(theme, "screen-driver-act", button, kit::ButtonKind::Secondary)
            .debug_selector(|| "screen-driver-act".to_owned())
            .on_click(cx.listener(move |this, _ev, _w, cx| {
                if control {
                    this.hand_back(cx);
                } else {
                    this.take_control(cx);
                }
            }));
        let pill = kit::elevate(kit::pill_frame(theme), theme)
            .id("screen-driver")
            .debug_selector(|| "screen-driver".to_owned())
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .pl(px(theme.spacing.md))
            .font_family(theme.typography.ui_family.clone())
            .text_size(px(theme.typography.small()))
            .text_color(hsla(s.text_secondary))
            // A press on the pill is the pill's: it never reaches the picture under it.
            .on_mouse_down(gpui::MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
            .child(
                div()
                    .id("screen-driver-words")
                    .role(Role::Status)
                    .aria_label(SharedString::from(words.clone()))
                    .child(SharedString::from(words)),
            )
            .child(act);
        Some(
            div()
                .absolute()
                .bottom(px(theme.spacing.md))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(pill)
                .into_any_element(),
        )
    }
}
