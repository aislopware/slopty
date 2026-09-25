//! The bar along the bottom: where the focused tile runs, and how things are going.
//!
//! On the left, the focused tile's worker and the tail of its working directory. On the
//! right, the uploads in flight, what is wrong with that worker's link when something is (a
//! link that is up says nothing), the round trip to it, the frame time, and the
//! agents at work and waiting, which jumps to the next one waiting when clicked. Each readout
//! is lowercase, a number rather than chrome. A phone keeps the worker, the round trip and the
//! agents.

use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_proto::items::ItemKind;

use super::WorkspaceView;
use super::navigator::{Mode, rtt_label, worker_health};
use crate::a11y::tab_stop;
use crate::colors::hsla;
use crate::icons::{IconName, IconSize, Status, icon, status_icon};

/// The bar's height.
const STATUSBAR_H: f32 = 26.0;

/// How long a frame-time readout stands before it is worked out again: the percentile sorts
/// the probe's ring, which is not work for every frame.
const FRAME_READOUT_EVERY: Duration = Duration::from_secs(1);

/// The agents at a glance: `2 working · 1 needs you`; empty when none works or waits.
#[must_use]
fn agent_summary(working: usize, waiting: usize) -> String {
    let mut parts = Vec::new();
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    match waiting {
        0 => {}
        1 => parts.push("1 needs you".to_owned()),
        n => parts.push(format!("{n} need you")),
    }
    parts.join(" · ")
}

/// Uploads in flight at a glance: `1 upload · 42%`.
#[must_use]
fn transfers_label(count: usize, done: u64, total: u64) -> String {
    let noun = if count == 1 { "upload" } else { "uploads" };
    let percent = done.saturating_mul(100).checked_div(total).unwrap_or(0).min(100);
    format!("{count} {noun} · {percent}%")
}

impl WorkspaceView {
    /// The worker the status bar speaks for: the focused tile's, else the one a new tile
    /// would go to.
    pub(super) fn status_worker(&self) -> Option<slopty_client::layout::WorkerKey> {
        self.focused().map(|t| t.worker).or_else(|| self.context_worker())
    }

    /// Agents busy on their own, across every worker.
    pub(super) fn working_count(&self) -> usize {
        let sessions = self
            .agents
            .keys()
            .chain(self.server_agents.keys().filter(|s| !self.agents.contains_key(s)));
        sessions
            .filter(|s| self.agent_state(**s).and_then(Status::of_agent) == Some(Status::Working))
            .count()
    }

    /// The frame time's readout, worked out at most once a [`FRAME_READOUT_EVERY`]; nothing
    /// before the app's probe has timed a frame.
    fn frame_readout(&mut self, cx: &gpui::App) -> Option<SharedString> {
        let now = Instant::now();
        let fresh = self
            .frame_text
            .as_ref()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) < FRAME_READOUT_EVERY);
        if !fresh {
            let text = crate::frames::stats(cx)
                .filter(|s| s.frames > 0)
                .map(|s| format!("frame {:.1} ms", s.draw_p50.as_secs_f64() * 1e3).into());
            self.frame_text = Some((now, text));
        }
        self.frame_text.as_ref().and_then(|(_, text)| text.clone())
    }

    /// The bar, or nothing before the first worker.
    pub(super) fn render_statusbar(
        &mut self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.workers.is_empty() {
            return None;
        }
        let phone = matches!(self.nav.drawn, Some(Mode::Drawer))
            || f32::from(window.viewport_size().width) < self.layout.config().phone_below;
        let frame = if phone { None } else { self.frame_readout(cx) };
        let theme = &self.theme;
        let s = &theme.surfaces;
        let spacing = theme.spacing;
        let safe = window.insets().effective();
        let readout = |selector: &'static str, text: SharedString| {
            div()
                .id(selector)
                .debug_selector(move || selector.to_owned())
                .role(Role::Status)
                .aria_label(text.clone())
                .flex_none()
                .whitespace_nowrap()
                .child(text)
        };

        let worker = self.status_worker();
        let name =
            worker.and_then(|k| self.workers.get(&k)).map(|w| SharedString::from(w.name.clone()));
        let cwd = (!phone)
            .then(|| self.focused().and_then(|t| self.item(t)))
            .flatten()
            .filter(|item| matches!(item.kind, ItemKind::Terminal { .. } | ItemKind::File { .. }))
            .and_then(|item| self.cwd_of(item))
            .map(|cwd| SharedString::from(super::tile::cwd_tail(&cwd)));
        let left = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(spacing.sm))
            .children(name.map(|n| readout("status-worker", n).text_color(hsla(s.text_secondary))))
            .children(cwd.map(|c| readout("status-cwd", c)));

        let transfers = (!phone && !self.uploads.is_empty()).then(|| {
            let (done, total) = self.uploads.values().fold((0_u64, 0_u64), |(d, t), u| {
                (d.saturating_add(u.done), t.saturating_add(u.total))
            });
            let text = transfers_label(self.uploads.len(), done, total);
            readout("status-transfers", text.into())
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .child(icon(theme, IconName::Upload, IconSize::Inline, hsla(s.text_muted)))
        });
        let link = worker.and_then(|k| self.workers.get(&k));
        let rtt = link
            .and_then(|w| w.rtt.filter(|_| w.status.is_up()))
            .map(|rtt| readout("status-rtt", format!("rtt {}", rtt_label(rtt)).into()));
        // The link says something only when it is not up.
        let health = link.and_then(|w| worker_health(&w.status)).map(|(mark, word)| {
            let tone = hsla(mark.tone(theme));
            readout("status-link", word.into())
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .text_color(tone)
                .child(status_icon(theme, mark, px(theme.typography.icon()), tone))
        });
        let frame = frame.map(|text| readout("status-frame", text));
        let (working, waiting) = (self.working_count(), self.needs_you_count());
        let agents = (working > 0 || waiting > 0).then(|| {
            let text: SharedString = agent_summary(working, waiting).into();
            let tone = if waiting > 0 { s.warn } else { s.text_secondary };
            let pill = readout("status-agents", text)
                .role(Role::Button)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .px(px(spacing.xs))
                .rounded(px(theme.radii.xs))
                .text_color(hsla(tone))
                .cursor_pointer()
                .hover(move |el| el.bg(hsla(s.raised)))
                .child(status_icon(
                    theme,
                    if waiting > 0 { Status::NeedsYou } else { Status::Working },
                    px(theme.typography.icon()),
                    hsla(tone),
                ));
            tab_stop(pill, s.accent).on_click(cx.listener(|this, _ev, window, cx| {
                this.next_attention(&super::actions::NextAttention, window, cx);
            }))
        });
        let right = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(spacing.md))
            .children(transfers)
            .children(health)
            .children(rtt)
            .children(frame)
            .children(agents);
        Some(
            div()
                .id("statusbar")
                .debug_selector(|| "statusbar".to_owned())
                .role(Role::Group)
                .aria_label("Status")
                .flex_none()
                .h(px(STATUSBAR_H))
                .w_full()
                .flex()
                .items_center()
                .gap(px(spacing.md))
                .pl(px(spacing.md) + safe.left)
                .pr(px(spacing.md) + safe.right)
                .bg(hsla(s.panel))
                .border_t_1()
                .border_color(hsla(s.border))
                .font_family(theme.typography.ui_family.clone())
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.text_muted))
                .child(left)
                .child(right)
                .when(phone, |el| el.gap(px(spacing.sm)))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readouts_say_what_they_count() {
        assert_eq!(agent_summary(2, 1), "2 working · 1 needs you");
        assert_eq!(agent_summary(0, 3), "3 need you");
        assert_eq!(agent_summary(1, 0), "1 working");
        assert_eq!(agent_summary(0, 0), "");
        assert_eq!(transfers_label(1, 42, 100), "1 upload · 42%");
        assert_eq!(transfers_label(2, 0, 0), "2 uploads · 0%");
    }
}
