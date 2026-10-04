//! A message the worker holds until its moment ([`Cap::SCHEDULE`]), and the one way the person
//! sets one: "Continue at 14:35" on a thread a usage limit stopped.
//!
//! A general "Send later…" menu (times, or once another thread rests) went: nobody reached for
//! it, and the one moment worth waiting for is a limit's reset, which the agent names
//! (`TurnState::Failed::until_ms`). The thread says so over the field, and one press sends the
//! draft, or "Continue" when nothing is typed, to go at that moment. The tray says when it
//! goes.

use gpui::accesskit::Role;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use slopty_core::WallMs;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{Cap, Delivery, ThreadState, TurnState};

use super::ThreadView;
use crate::colors::hsla;
use crate::conversation::figures;
use crate::icons::IconName;
use crate::kit::ButtonKind;

/// What goes at a limit's reset when nothing is typed.
const CONTINUE: &str = "Continue";

/// When a message held for `delivery` goes, in words for a line of its own: "14:35",
/// "Tomorrow 09:00". Nothing for a message that goes with the turn.
pub(super) fn when_words(delivery: Delivery, now: WallMs) -> Option<String> {
    match delivery {
        Delivery::At { at_ms } => figures::stamp(at_ms, now),
        Delivery::Steer | Delivery::Queue | Delivery::Interrupt => None,
    }
}

/// When the usage limit that stopped the thread's last turn lifts, while that is still ahead.
pub(super) fn limit_lifts(state: &ThreadState, now: WallMs) -> Option<WallMs> {
    match &state.turns.last()?.state {
        TurnState::Failed { until_ms: Some(at), .. } if *at > now => Some(*at),
        _ => None,
    }
}

impl ThreadView {
    /// Send the draft, or "Continue" when nothing is typed, to go when the limit lifts.
    fn continue_at(&mut self, at_ms: WallMs, window: &mut Window, cx: &mut Context<Self>) {
        let delivery = Delivery::At { at_ms };
        if self.draft(cx).trim().is_empty() {
            let text = CONTINUE.to_owned();
            let _id = self.intent(Intent::Send { text, delivery, attachments: Vec::new() }, cx);
        } else {
            self.submit(delivery, window, cx);
        }
    }

    /// Over the field while a usage limit holds the thread and the agent takes a message kept
    /// for later: when it lifts, and "Continue at 14:35".
    pub(super) fn limit_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let state = self.state(cx)?;
        if !state.meta.can(Cap::SCHEDULE) || self.composing.editing() {
            return None;
        }
        let now = WallMs::now();
        let at = limit_lifts(state, now)?;
        // One waiting already says when the thread goes on.
        if state.pending.iter().any(|p| matches!(p.delivery, Delivery::At { .. })) {
            return None;
        }
        let when = figures::stamp(at, now)?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let label = format!("Continue at {when}");
        Some(
            div()
                .id("thread-limit")
                .debug_selector(|| "thread-limit".to_owned())
                .role(Role::Status)
                .flex()
                .items_center()
                .gap(self.z(theme.spacing.xs))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(self.icon(IconName::Clock, s.text_muted))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(SharedString::from(format!("The usage limit lifts at {when}"))),
                )
                .child(self.button("thread-continue-at", label, ButtonKind::Secondary).on_click(
                    cx.listener(move |this, _ev, window, cx| this.continue_at(at, window, cx)),
                ))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{Delivery, TurnState};

    use super::{limit_lifts, when_words};
    use crate::conversation::figures;
    use crate::conversation::thread::fixtures;

    /// What waits says when it goes.
    #[test]
    fn a_held_message_says_when_it_goes() {
        let now = WallMs::now();
        let soon = WallMs::from_millis(now.as_millis() + 60_000);
        let clock = figures::stamp(soon, now).expect("a time");
        assert_eq!(when_words(Delivery::At { at_ms: soon }, now), Some(clock));
        assert_eq!(when_words(Delivery::Queue, now), None);
    }

    /// A limit lifts at the time the agent names for its last turn, while that is ahead.
    #[test]
    fn a_limit_lifts_when_its_turn_says() {
        let now = WallMs::from_millis(10_000);
        let mut state = fixtures::empty();
        let turn = |until_ms| slopty_proto::thread::Turn {
            id: slopty_proto::thread::TurnId(1),
            input: None,
            state: TurnState::Failed { error: "limit".to_owned(), until_ms },
            started_ms: WallMs::ZERO,
            ended_ms: None,
            usage: slopty_proto::thread::Usage::default(),
            models: Vec::new(),
            changed: slopty_proto::thread::Changed::default(),
            before: None,
            after: None,
        };
        state.turns = vec![turn(Some(WallMs::from_millis(70_000)))];
        assert_eq!(limit_lifts(&state, now), Some(WallMs::from_millis(70_000)));
        state.turns = vec![turn(Some(WallMs::from_millis(5_000)))];
        assert_eq!(limit_lifts(&state, now), None, "already lifted");
        state.turns = vec![turn(None)];
        assert_eq!(limit_lifts(&state, now), None);
    }
}
