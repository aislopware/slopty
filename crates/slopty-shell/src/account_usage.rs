// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The composer footer's ring cluster. The plan-usage ring shows how much of
//! the plan's rate limit the session's agent has used, beside the context
//! ring. Clicking it opens a card with each window's meter and reset.
// slopty: no agent accounts: Slopty never touches a credential. The windows are read-only, as
// the chat's agent reports them (`AppState::usage_limits`).
use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, Context, Entity, Hsla, IntoElement, Render, SharedString, Subscription, Window,
    div, prelude::*, px,
};
use slopty_view_proto::AgentUsageWindow;

use crate::popover;
use crate::state::AppState;
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
        // slopty: use at a normal rate is quiet, as the context ring beside it; only nearing a
        // limit takes a hue.
        UsageLevel::Normal => theme.text_muted,
        UsageLevel::Warn => theme.warning,
        UsageLevel::Critical => theme.danger,
    }
}

/// The binding limit: the most-used window. Pure.
pub fn used_fraction(windows: &[AgentUsageWindow]) -> Option<f32> {
    windows
        .iter()
        .map(|window| window.used_fraction.clamp(0.0, 1.0))
        .reduce(f32::max)
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

/// The plan-usage ring's card: every window's meter, read-only.
fn usage_card(windows: &[AgentUsageWindow], theme: &Theme) -> gpui::Div {
    let now = Utc::now();
    popover::popover_card(theme)
        .flex()
        .flex_col()
        .child(popover::menu_heading(theme, "Plan usage"))
        .children(windows.iter().map(|window| render_usage_meter(window, now, theme)))
        .child(div().h(px(4.0)))
}

// slopty: no agent accounts: Slopty never touches a credential.
/// The footer's rings.
pub struct AccountUsage {
    state: Entity<AppState>,
    popup: popover::Popup<FooterCard>,
    // slopty: the agent's command catalog (the composer's `ListCommands` params), and whether
    // it lists `compact`, which the context card offers.
    catalog: Option<serde_json::Value>,
    compactable: std::collections::HashMap<String, bool>,
    catalog_task: Option<gpui::Task<()>>,
    // slopty: the chat's context by category, from its dock (`composer/dock.rs`).
    breakdown: Option<slopty_view_doc::dock::ContextBreakdown>,
    _state: Subscription,
}

// slopty: the context card's Compact row was pressed: the composer sends `/compact` through
// the agent's own command door.
/// Compact the conversation, through the agent's own `/compact`.
pub struct CompactRequested;

impl gpui::EventEmitter<CompactRequested> for AccountUsage {}

impl AccountUsage {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        Self {
            _state: cx.observe(&state, |_, _, cx| cx.notify()),
            state,
            popup: popover::Popup::default(),
            catalog: None,
            compactable: std::collections::HashMap::new(),
            catalog_task: None,
            breakdown: None,
        }
    }

    // slopty: the composer hands the chat's breakdown over each render; cheap.
    /// The chat's context by category, when its agent counted it.
    pub fn set_breakdown(&mut self, breakdown: Option<slopty_view_doc::dock::ContextBreakdown>, cx: &mut Context<Self>) {
        if self.breakdown != breakdown {
            self.breakdown = breakdown;
            cx.notify();
        }
    }

    // slopty: the composer names the catalog its `/` menu reads; cheap to call every render.
    /// Point at the agent whose commands decide the Compact row.
    pub fn set_catalog(&mut self, params: Option<serde_json::Value>) {
        self.catalog = params;
    }

    /// Whether the agent lists `compact`, as far as known.
    fn compactable(&self) -> bool {
        self.catalog
            .as_ref()
            .and_then(|params| self.compactable.get(&params.to_string()))
            .copied()
            .unwrap_or(false)
    }

    /// Ask the agent's commands once per catalog, when the context card opens.
    fn load_commands(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.catalog.clone() else { return };
        let key = params.to_string();
        if self.compactable.contains_key(&key) {
            return;
        }
        let Some(engine) = self.state.read(cx).engine().cloned() else { return };
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let listed = engine
                .client()
                .call(slopty_view_rpc::methods::LIST_COMMANDS, params)
                .await
                .ok()
                .and_then(|value| {
                    serde_json::from_value::<Vec<slopty_view_proto::SlashCommand>>(value).ok()
                });
            let Some(commands) = listed else { return };
            let compact = commands.iter().any(|c| c.name.trim_start_matches('/') == "compact");
            this.update(cx, |usage, cx| {
                usage.compactable.insert(key, compact);
                cx.notify();
            })
            .ok();
        }));
    }

    /// The context card, with Compact where the agent lists it.
    fn context_card(
        &self,
        context: Option<slopty_view_proto::ContextUsage>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let theme = Theme::of(cx).for_popup();
        let card = crate::context_usage::card(context, self.breakdown.as_ref(), &theme);
        if !self.compactable() {
            return card;
        }
        card.child(
            div().px(px(4.0)).pb(px(4.0)).child(
                popover::menu_row(&theme, false, "context-usage-compact")
                    .id("context-usage-compact")
                    .role(gpui::Role::Button)
                    .aria_label("Compact now")
                    .child(div().flex_1().child(SharedString::from("Compact now")))
                    .child(popover::kbd_hint(&theme, "/compact"))
                    .on_click(cx.listener(|usage, _, _, cx| {
                        usage.dismiss(cx);
                        cx.emit(CompactRequested);
                    })),
            ),
        )
    }

    // slopty: no agent accounts: Slopty never touches a credential.
    /// A trigger click: open this ring's popover, or close it when the
    /// press found it open (the card's mouse-down-out already began that
    /// close — see `Popup::note_trigger_press`).
    fn toggle(&mut self, card: FooterCard, cx: &mut Context<Self>) {
        if self.popup.take_press_was_open() || self.popup.as_open() == Some(&card) {
            self.dismiss(cx);
            return;
        }
        // slopty: no agent accounts: Slopty never touches a credential.
        if card == FooterCard::Context {
            self.load_commands(cx); // slopty: the card offers Compact where the agent has it.
        }
        self.popup.open(card);
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.popup.begin_close() {
            popover::reap_popup(cx, |usage: &mut Self| &mut usage.popup);
        }
        cx.notify();
    }

    /// A footer ring that opens `card` above itself, right-aligned like the
    /// model picker.
    fn trigger(
        &self,
        chip: gpui::Stateful<gpui::Div>,
        card: FooterCard,
        content: impl FnOnce(&Self, &mut Context<Self>) -> gpui::Div,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let chip = chip
            .relative()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |usage, _, _, _| {
                    usage
                        .popup
                        .note_trigger_press_matching(|open| *open == card);
                }),
            )
            .on_click(cx.listener(move |usage, _, _, cx| usage.toggle(card, cx)));
        if self.popup.get() != Some(&card) {
            return chip.into_any_element();
        }
        let content = content(self, cx)
            .on_mouse_down_out(cx.listener(|usage, _, _, cx| usage.dismiss(cx)))
            .into_any_element();
        chip.child(popover::anchored_menu_above_end(
            card.id(),
            content,
            self.popup.closing_since(),
        ))
        .into_any_element()
    }

    // slopty: no agent accounts: Slopty never touches a credential.
}

/// Which footer ring's popover is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FooterCard {
    // slopty: the plan's windows, read-only (no accounts: Slopty never touches a credential).
    Usage,
    Context,
}

impl FooterCard {
    fn id(self) -> &'static str {
        match self {
            FooterCard::Usage => "account-usage-menu",
            FooterCard::Context => "context-usage-menu",
        }
    }
}

/// The footer's ring cluster: context occupancy, opening its popover on click. // slopty: no agent accounts: Slopty never touches a credential.
impl Render for AccountUsage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let mut context = self.state.read(cx).context_usage;
        // slopty: the agent's own count, when it made one, is the ring's.
        if let Some(breakdown) = self.breakdown.as_ref().filter(|b| b.window > 0) {
            context = Some(slopty_view_proto::ContextUsage {
                tokens: Some(breakdown.tokens),
                window: Some(breakdown.window),
            });
        }
        let urgent = self.breakdown.as_ref().is_some_and(|b| b.urgent);
        // slopty: the plan-usage ring reads the agent's own windows; no accounts.
        let windows = self.state.read(cx).usage_limits.clone();
        let usage = used_fraction(&windows).map(|fraction| {
            let level = usage_level(fraction);
            let chip = crate::context_usage::ring_chip(
                "account-usage",
                fraction,
                usage_color(level, &theme),
                match level {
                    UsageLevel::Normal => theme.text_muted,
                    _ => usage_color(level, &theme),
                },
                format!("{}%", (fraction * 100.0).round() as u32),
                self.popup.get() == Some(&FooterCard::Usage),
                &theme,
            );
            self.trigger(
                chip,
                FooterCard::Usage,
                move |_, cx| usage_card(&windows, &Theme::of(cx).for_popup()),
                cx,
            )
        });
        let context = crate::context_usage::has_window(context).then(|| {
            let mut chip = crate::context_usage::chip(
                context,
                self.popup.get() == Some(&FooterCard::Context),
                &theme,
            );
            if urgent {
                chip = chip.text_color(theme.danger); // slopty: nearly full, nothing to compact it.
            }
            self.trigger(
                chip,
                FooterCard::Context,
                move |usage, cx| usage.context_card(context, cx),
                cx,
            )
        });
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .children(usage)
            .children(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(label: &str, used: f32) -> AgentUsageWindow {
        AgentUsageWindow { label: label.into(), used_fraction: used, resets_at: None }
    }

    #[test]
    fn ring_shows_the_most_used_window() {
        assert_eq!(used_fraction(&[]), None);
        assert_eq!(used_fraction(&[window("5h", 0.12), window("7d", 0.64)]), Some(0.64));
        assert_eq!(used_fraction(&[window("5h", 1.4)]), Some(1.0));
    }

    #[test]
    fn the_ring_turns_at_the_context_ring_s_thresholds() {
        assert_eq!(usage_level(0.74), UsageLevel::Normal);
        assert_eq!(usage_level(0.75), UsageLevel::Warn);
        assert_eq!(usage_level(0.9), UsageLevel::Critical);
        // slopty: normal use is quiet, as the context ring is.
        let theme = Theme::dark();
        assert_eq!(usage_color(UsageLevel::Normal, &theme), theme.text_muted);
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
