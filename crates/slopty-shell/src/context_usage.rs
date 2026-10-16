// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Context occupancy is read from the replicated chat snapshot, never local CLI state.
use crate::theme::Theme;
use gpui::{IntoElement, PathBuilder, SharedString, canvas, div, point, prelude::*, px};
use slopty_view_doc::dock::{ContextBreakdown, ContextSlice, TurnTokens};
use slopty_view_proto::ContextUsage;

/// The context ring's trigger chip; the footer ([`crate::account_usage`])
/// opens [`card`] from it on click.
pub fn chip(usage: Option<ContextUsage>, open: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let fraction = usage.and_then(ContextUsage::fraction);
    let color = match fraction {
        Some(f) if f >= 0.9 => theme.danger,
        Some(f) if f >= 0.75 => theme.warning,
        Some(_) => theme.text_muted,
        None => theme.text_faint,
    };
    let label = fraction
        .map(|f| format!("{:.0}%", f * 100.0))
        .unwrap_or_else(|| "—".into());
    ring_chip(
        "context-usage",
        fraction.unwrap_or(0.0) as f32,
        color,
        color,
        label,
        open,
        theme,
    )
}

/// One footer ring indicator: ring + percent, identical geometry for every
/// ring so they sit side by side as equals. `arc` colours the ring's fill,
/// `text` the label; `open` holds the hover wash while its popover is up.
pub(crate) fn ring_chip(
    id: &'static str,
    fraction: f32,
    arc: gpui::Hsla,
    text: gpui::Hsla,
    label: String,
    open: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    // slopty: each ring names itself to the accessibility tree ("Plan usage 64%").
    let name = if id == "account-usage" { "Plan usage" } else { "Context" };
    div()
        .id(id)
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(format!("{name} {label}")))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.0))
        .h(px(24.0))
        .px(px(6.0))
        .rounded(px(6.0))
        .text_size(px(11.0))
        .text_color(text)
        .cursor_pointer()
        .when(open, |s| s.bg(crate::theme::ink(0.05)))
        .hover(|s| s.bg(crate::theme::ink(0.05)))
        .child(ring(fraction, arc, theme))
        .child(SharedString::from(label))
}

/// The footer's 16px progress ring: a faint full track under a `fraction` arc
/// starting at twelve o'clock. Shared with the account usage indicator.
pub(crate) fn ring(fraction: f32, color: gpui::Hsla, theme: &Theme) -> impl IntoElement {
    let track = theme.text_faint.opacity(0.25);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let mut arc = |fraction: f32, color| {
                if fraction <= 0.0 {
                    return;
                }
                let steps = (64.0 * fraction).ceil().max(2.0) as usize;
                let mut path = PathBuilder::stroke(px(1.8));
                for i in 0..=steps {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                    let p = point(
                        center.x + px(6.0 * angle.cos()),
                        center.y + px(6.0 * angle.sin()),
                    );
                    if i == 0 {
                        path.move_to(p);
                    } else {
                        path.line_to(p);
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            arc(1.0, track);
            arc(fraction.clamp(0.0, 1.0), color);
        },
    )
    .size(px(16.0))
}

fn with_separators(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// whether the indicator has anything to measure against: harnesses that
/// never report a window (antigravity) get no indicator at all, rather than a
/// permanently empty ring.
pub fn has_window(usage: Option<ContextUsage>) -> bool {
    usage
        .and_then(|usage| usage.window)
        .is_some_and(|window| window > 0)
}

fn details(usage: Option<ContextUsage>) -> String {
    match usage.unwrap_or_default() {
        ContextUsage {
            tokens: Some(tokens),
            window: Some(window),
        } if window > 0 => {
            format!(
                "{} / {} tokens\n{} tokens remaining",
                with_separators(tokens),
                with_separators(window),
                with_separators(window.saturating_sub(tokens))
            )
        }
        ContextUsage {
            tokens: Some(tokens),
            ..
        } => format!(
            "{} tokens used\nContext limit not reported",
            with_separators(tokens)
        ),
        ContextUsage {
            window: Some(window),
            ..
        } if window > 0 => format!(
            "{} token capacity\nWaiting for context usage",
            with_separators(window)
        ),
        _ => "Context usage not reported by this harness yet".into(),
    }
}

/// The context ring's popover content. // slopty: with the agent's breakdown when it counted it.
pub fn card(usage: Option<ContextUsage>, breakdown: Option<&ContextBreakdown>, theme: &Theme) -> gpui::Div {
    let card = crate::popover::popover_card(theme)
        .flex()
        .flex_col()
        .child(crate::popover::menu_heading(theme, "Context window"));
    let Some(breakdown) = breakdown.filter(|b| b.window > 0) else {
        return card.child(
            div()
                .px(px(8.0))
                .pb(px(6.0))
                .text_size(px(12.0))
                .line_height(px(19.0))
                .whitespace_nowrap()
                .text_color(theme.text_muted)
                .child(SharedString::from(details(usage))),
        );
    };
    breakdown_card(card, breakdown, theme)
}

// slopty: the context by category (the agent's `get_context_usage`), what the TUI warns of as it
// fills, and what the last turn took (`.research/tui-presentation-2026-10-16.md` §1.11).
/// The card's body from the agent's breakdown: the totals, a bar by category, a row each, the
/// warning, the last turn's tokens.
fn breakdown_card(card: gpui::Div, breakdown: &ContextBreakdown, theme: &Theme) -> gpui::Div {
    let palette = slice_palette(theme);
    let used: Vec<(usize, &ContextSlice)> =
        breakdown.categories.iter().enumerate().filter(|(_, slice)| !slice.free).collect();
    let totals = format!(
        "{} / {} tokens · {}%",
        with_separators(breakdown.tokens),
        with_separators(breakdown.window),
        breakdown.used_percent
    );
    let mut card = card.child(
        div()
            .id("context-card-totals")
            .role(gpui::Role::Status)
            .aria_label(SharedString::from(totals.clone()))
            .px(px(8.0))
            .pb(px(6.0))
            .text_size(px(12.0))
            .text_color(theme.text_muted)
            .child(SharedString::from(totals)),
    );
    if !breakdown.categories.is_empty() {
        let bar = div()
            .mx(px(8.0))
            .mb(px(6.0))
            .h(px(6.0))
            .rounded_full()
            .overflow_hidden()
            .flex()
            .bg(theme.wash(0.08))
            .children(used.iter().map(|(ix, slice)| {
                div()
                    .h_full()
                    .w(gpui::relative((slice.share_bp as f32 / 10_000.0).clamp(0.0, 1.0)))
                    .bg(palette[ix % palette.len()])
            }));
        card = card.child(bar);
        for (ix, slice) in breakdown.categories.iter().enumerate() {
            let color = if slice.free { theme.wash(0.16) } else { palette[ix % palette.len()] };
            let percent = format!("{:.1}%", slice.share_bp as f32 / 100.0);
            let label = format!("{} · {} tokens · {percent}", slice.name, with_separators(slice.tokens));
            card = card.child(
                div()
                    .id(SharedString::from(format!("context-slice-{ix}")))
                    .role(gpui::Role::ListItem)
                    .aria_label(SharedString::from(label))
                    .h(px(20.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_size(crate::typography::ui_rems(11.5))
                    .child(div().flex_none().size(px(7.0)).rounded(px(2.0)).bg(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if slice.free { theme.text_faint } else { theme.text_muted })
                            .child(SharedString::from(slice.name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(compact_tokens(slice.tokens))),
                    )
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_none()
                            .text_right()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(percent)),
                    ),
            );
        }
    }
    if let Some(words) = breakdown.words.clone() {
        card = card.child(
            div()
                .id("context-card-warning")
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(words.clone()))
                .px(px(8.0))
                .pt(px(4.0))
                .pb(px(2.0))
                .text_size(px(11.5))
                .text_color(if breakdown.urgent { theme.danger } else { theme.warning })
                .child(SharedString::from(words)),
        );
    } else if let Some(at) = breakdown.auto_compact_percent {
        card = card.child(
            div()
                .px(px(8.0))
                .pt(px(4.0))
                .text_size(px(11.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(format!("Compacts on its own at {at}%"))),
        );
    }
    if let Some(turn) = breakdown.last_turn {
        let words = turn_words(&turn);
        card = card.child(
            div()
                .id("context-card-last-turn")
                .role(gpui::Role::Status)
                .aria_label(SharedString::from(format!("Last turn: {words}")))
                .px(px(8.0))
                .pt(px(4.0))
                .pb(px(4.0))
                .text_size(px(11.5))
                .text_color(theme.text_faint)
                .child(SharedString::from(format!("Last turn · {words}"))),
        );
    }
    card.pb(px(2.0))
}

/// A turn's tokens by kind: "1.2k in · 840 out · 41k cached · 2k cache write".
pub fn turn_words(turn: &TurnTokens) -> String {
    let mut parts = vec![format!("{} in", compact_tokens(turn.input)), format!("{} out", compact_tokens(turn.output))];
    if turn.cache_read > 0 {
        parts.push(format!("{} cached", compact_tokens(turn.cache_read)));
    }
    if turn.cache_write > 0 {
        parts.push(format!("{} cache write", compact_tokens(turn.cache_write)));
    }
    parts.join(" · ")
}

fn compact_tokens(tokens: u64) -> String {
    slopty_view_doc::tokens_short(tokens)
}

/// The colours the categories take in turn: the theme's own, never ad hoc.
fn slice_palette(theme: &Theme) -> [gpui::Hsla; 6] {
    [theme.accent, theme.success, theme.warning, theme.text_muted, theme.accent.opacity(0.55), theme.text_faint]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indicator_needs_a_reported_window() {
        assert!(!has_window(None));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: None,
        })));
        assert!(!has_window(Some(ContextUsage {
            tokens: Some(1_200),
            window: Some(0),
        })));
        assert!(has_window(Some(ContextUsage {
            tokens: None,
            window: Some(200_000),
        })));
    }

    #[test]
    fn missing_usage_is_distinct_from_zero_and_overflow() {
        assert!(details(None).contains("not reported"));
        let turn = TurnTokens { input: 1_200, output: 840, cache_read: 41_000, cache_write: 0 };
        assert_eq!(turn_words(&turn), "1.2k in · 840 out · 41k cached");
        assert!(
            details(Some(ContextUsage {
                tokens: Some(0),
                window: Some(200)
            }))
            .contains("200 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(250),
                window: Some(200)
            }))
            .contains("0 tokens remaining")
        );
        assert!(
            details(Some(ContextUsage {
                tokens: Some(10),
                window: Some(0)
            }))
            .contains("limit not reported")
        );
    }

    #[test]
    fn token_counts_are_grouped_by_thousands() {
        assert_eq!(with_separators(0), "0");
        assert_eq!(with_separators(999), "999");
        assert_eq!(with_separators(5417), "5,417");
        assert_eq!(with_separators(1_048_576), "1,048,576");
        assert_eq!(
            details(Some(ContextUsage {
                tokens: Some(5417),
                window: Some(1_048_576)
            })),
            "5,417 / 1,048,576 tokens\n1,043,159 tokens remaining"
        );
    }
}
