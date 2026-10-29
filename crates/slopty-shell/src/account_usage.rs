// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! slopty: a plan-usage window's meter, as Settings shows an agent's windows (the composer
//! footer's rings went with the composer, TUI-first: the agent's TUI shows its own usage).
// slopty: no agent accounts: Slopty never touches a credential. The windows are read-only, as
// the chat's agent reports them.
use chrono::{DateTime, Utc};
use gpui::{AnyElement, Hsla, SharedString, div, prelude::*, px};
use slopty_view_proto::AgentUsageWindow;

use crate::theme::Theme;

// slopty: zeron's usage meters (`settings/accounts.rs`), kept read-only here, coloured at the
// context ring's thresholds.
/// A usage fraction's warning step.
pub const USAGE_WARN_FRACTION: f32 = 0.75;
/// A usage fraction's critical step.
pub const USAGE_CRITICAL_FRACTION: f32 = 0.90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLevel {
    Normal,
    Warn,
    Critical,
}

/// Threshold classification of a usage fraction. Pure.
pub fn usage_level(fraction: f32) -> UsageLevel {
    if fraction >= USAGE_CRITICAL_FRACTION {
        UsageLevel::Critical
    } else if fraction >= USAGE_WARN_FRACTION {
        UsageLevel::Warn
    } else {
        UsageLevel::Normal
    }
}

pub fn usage_color(level: UsageLevel, theme: &Theme) -> Hsla {
    match level {
        UsageLevel::Normal => theme.accent,
        UsageLevel::Warn => theme.warning,
        UsageLevel::Critical => theme.danger,
    }
}

/// Compact absolute reset moment: a local clock time ("3:45 PM") within ~22 h, a short
/// weekday ("Mon") within a week, else month and day ("Sep 14"). Pure given `now`.
pub fn format_reset(resets_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    use chrono::Local;
    let at = resets_at?;
    let local = at.with_timezone(&Local);
    Some(if at.signed_duration_since(now).num_hours() < 22 {
        format!("resets {}", local.format(crate::transcript::clock_format()))
    } else if at.signed_duration_since(now).num_hours() < 24 * 7 {
        format!("resets {}", local.format("%a"))
    } else {
        format!("resets {}", local.format("%b %-d"))
    })
}

const USAGE_LABEL_WIDTH: f32 = 52.0;
const USAGE_BAR_WIDTH: f32 = 120.0;
const USAGE_PERCENT_WIDTH: f32 = 34.0;
const USAGE_RESET_WIDTH: f32 = 96.0;

/// One meter line: label, a short bar, percent, then when it resets.
pub(crate) fn render_usage_meter(
    window: &AgentUsageWindow,
    now: DateTime<Utc>,
    theme: &Theme,
) -> AnyElement {
    let fraction = window.used_fraction.clamp(0.0, 1.0);
    let level = usage_level(fraction);
    let fill = usage_color(level, theme).opacity(match level {
        UsageLevel::Normal => 0.8,
        _ => 0.9,
    });
    let percent = format!("{}%", (fraction * 100.0).round() as u32);
    let reset = format_reset(window.resets_at, now).unwrap_or_default();
    div()
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .text_size(crate::typography::ui_rems(11.5))
        .child(
            div()
                .w(px(USAGE_LABEL_WIDTH))
                .flex_none()
                .truncate()
                .text_color(theme.text_muted)
                .child(SharedString::from(window.label.clone())),
        )
        .child(
            div()
                .w(px(USAGE_BAR_WIDTH))
                .flex_none()
                .h(px(4.0))
                .rounded_full()
                .overflow_hidden()
                .bg(theme.wash(0.08))
                .when(fraction > 0.0, |el| {
                    el.child(
                        div()
                            .h_full()
                            // A 1.5% floor keeps tiny non-zero usage visible.
                            .w(gpui::relative(fraction.max(0.015)))
                            .rounded_full()
                            .bg(fill),
                    )
                }),
        )
        .child(
            div()
                .w(px(USAGE_PERCENT_WIDTH))
                .flex_none()
                .text_right()
                .text_color(match level {
                    UsageLevel::Normal => theme.text_muted,
                    _ => usage_color(level, theme),
                })
                .child(SharedString::from(percent)),
        )
        .child(
            div()
                .w(px(USAGE_RESET_WIDTH))
                .flex_none()
                .truncate()
                .text_color(theme.text_faint)
                .child(SharedString::from(reset)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_turns_at_the_context_ring_s_thresholds() {
        assert_eq!(usage_level(0.74), UsageLevel::Normal);
        assert_eq!(usage_level(0.75), UsageLevel::Warn);
        assert_eq!(usage_level(0.9), UsageLevel::Critical);
        let theme = Theme::dark();
        assert_eq!(usage_color(UsageLevel::Normal, &theme), theme.accent);
        assert_eq!(usage_color(UsageLevel::Warn, &theme), theme.warning);
    }

    #[test]
    fn a_reset_reads_as_a_time_a_day_or_a_date() {
        let now = Utc::now();
        assert_eq!(format_reset(None, now), None);
        let soon = format_reset(Some(now + chrono::TimeDelta::hours(2)), now).unwrap();
        // On the system's clock.
        let at = (now + chrono::TimeDelta::hours(2)).with_timezone(&chrono::Local);
        assert_eq!(soon, format!("resets {}", at.format(crate::transcript::clock_format())));
        let later = format_reset(Some(now + chrono::TimeDelta::days(3)), now).unwrap();
        assert_eq!(later.len(), "resets Mon".len(), "{later}");
    }
}
