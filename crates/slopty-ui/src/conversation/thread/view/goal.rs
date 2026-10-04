//! A goal the agent works toward across turns (Codex's `/goal`): one quiet line over the
//! field, read-only. Setting one, or pausing it, stays in the agent's own TUI.
//!
//! The line says what the goal is for, where it stands when that is not plain work, and the
//! tokens spent on it, against its budget where it has one. A budget also draws a thin bar
//! under the line in the context meter's tones, warning as the budget runs out. While the goal
//! is active the agent may start a turn of its own once one ends, so the Stop button's hint
//! says that a stop may not be the end.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, relative,
};
use slopty_proto::thread::Goal;

use super::{ThreadView, context_tone, tokens};
use crate::colors::hsla;
use crate::icons::IconName;
use crate::kit;

/// What the Stop button's hint adds while a goal is active.
pub(super) const GOES_ON: &str = "Codex may go on by itself toward its goal";

/// Where `goal` stands, in words; nothing while it is simply worked on.
fn standing(goal: &Goal) -> Option<String> {
    match goal.state.as_str() {
        Goal::ACTIVE => None,
        "complete" => Some("Done".to_owned()),
        other => {
            let words = other.replace(['-', '_'], " ");
            let mut chars = words.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect())
        }
    }
}

/// The tokens spent on `goal`, against its budget where it has one: "48k of 200k tokens".
fn spent(goal: &Goal) -> String {
    match goal.token_budget {
        Some(budget) => format!("{} of {} tokens", tokens(goal.tokens_used), tokens(budget)),
        None => format!("{} tokens", tokens(goal.tokens_used)),
    }
}

/// The share of its budget `goal` spent, in percent; none without a budget.
fn budget_used(goal: &Goal) -> Option<f64> {
    let budget = goal.token_budget.filter(|b| *b > 0)?;
    #[expect(clippy::cast_precision_loss, reason = "a share on screen")]
    let pct = goal.tokens_used as f64 / budget as f64 * 100.0;
    Some(pct)
}

impl ThreadView {
    /// The goal's line, while the agent holds one.
    pub(super) fn goal_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let goal = self.state(cx)?.goal.clone()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let standing = standing(&goal);
        let spent = spent(&goal);
        let tone = match goal.state.as_str() {
            Goal::ACTIVE | "complete" => s.text_secondary,
            "paused" => s.text_muted,
            _ => s.warn,
        };
        let said = [Some(goal.objective.clone()), standing.clone(), Some(spent.clone())]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(", ");
        let worked =
            format!("Worked {} toward it", kit::duration(Duration::from_secs(goal.time_used_s)));
        let hint_theme = std::rc::Rc::new(theme.clone());
        let line = div()
            .w_full()
            .flex()
            .items_center()
            .gap(self.z(theme.spacing.xs))
            .child(self.icon(IconName::Flag, s.text_muted))
            .child(kit::fit_label("thread-goal-objective", goal.objective.clone(), theme))
            .children(
                standing
                    .map(|w| div().flex_none().text_color(hsla(tone)).child(SharedString::from(w))),
            )
            .child(
                kit::tabular(div())
                    .flex_none()
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(spent)),
            );
        let bar = budget_used(&goal).filter(|_| goal.state != "complete").map(|pct| {
            let fill = hsla(context_tone(theme, pct));
            let radius = self.z(theme.radii.xs);
            #[expect(clippy::cast_possible_truncation, reason = "a share on screen")]
            let share = (pct / 100.0).clamp(0.0, 1.0) as f32;
            div()
                .debug_selector(|| "thread-goal-bar".to_owned())
                .w_full()
                .h(self.z(theme.spacing.xxs))
                .rounded(radius)
                .bg(hsla(s.border_subtle))
                .child(
                    kit::Gliding::new("thread-goal-fill", share, move |share| {
                        div()
                            .h_full()
                            .w(relative(share))
                            .rounded(radius)
                            .bg(fill)
                            .into_any_element()
                    })
                    .fill(),
                )
        });
        Some(
            div()
                .id("thread-goal")
                .debug_selector(|| "thread-goal".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(format!("Goal: {said}")))
                .flex()
                .flex_col()
                .gap(self.z(theme.spacing.xxs))
                .text_size(self.z(theme.typography.small()))
                .text_color(hsla(s.text_secondary))
                .child(line)
                .children(bar)
                .map(kit::hint_timing)
                .tooltip(move |_window, cx| {
                    let hint = kit::Hint::new(worked.clone(), "", std::rc::Rc::clone(&hint_theme));
                    cx.new(|_| hint).into()
                })
                .into_any_element(),
        )
    }

    /// Whether the agent works toward a goal and may start a turn of its own.
    pub(super) fn goal_goes_on(&self, cx: &gpui::App) -> bool {
        self.state(cx).and_then(|st| st.goal.as_ref()).is_some_and(Goal::is_active)
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::Goal;

    use super::{budget_used, spent, standing};

    fn goal(state: &str, budget: Option<u64>) -> Goal {
        Goal {
            objective: "Make the parser fast".to_owned(),
            state: state.to_owned(),
            tokens_used: 48_000,
            token_budget: budget,
            time_used_s: 720,
            updated_ms: WallMs::ZERO,
        }
    }

    /// Plain work says nothing of where it stands; anything else says it in a word or two, as
    /// the agent names it; the tokens read against the budget where there is one.
    #[test]
    fn a_goal_says_where_it_stands_and_what_it_spent() {
        assert_eq!(standing(&goal(Goal::ACTIVE, None)), None);
        assert_eq!(standing(&goal("budget-limited", None)).as_deref(), Some("Budget limited"));
        assert_eq!(standing(&goal("complete", None)).as_deref(), Some("Done"));
        assert_eq!(spent(&goal(Goal::ACTIVE, Some(200_000))), "48k of 200k tokens");
        assert_eq!(spent(&goal(Goal::ACTIVE, None)), "48k tokens");
        assert_eq!(budget_used(&goal(Goal::ACTIVE, Some(200_000))), Some(24.0));
        assert_eq!(budget_used(&goal(Goal::ACTIVE, Some(0))), None, "no budget to spend");
    }
}
