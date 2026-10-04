//! Sending a message later, where the worker holds it until its moment ([`Cap::SCHEDULE`]): at a
//! time, or once another thread of the worker rests.
//!
//! The clock beside the send button (and "Send later…" in the palette) opens a menu over the
//! field: in half an hour, in an hour, in three, tomorrow at nine, and one line for each of the
//! worker's other threads. A pick sets when the draft goes, said in a line over the field until
//! it is sent or taken off; the send button then says Schedule. What waits says in the tray
//! when it goes or what it waits on, as a held one says why it is held.

use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div,
};
use slopty_core::WallMs;
use slopty_proto::thread::{Cap, Delivery, ThreadId};

use super::ThreadView;
use crate::colors::hsla;
use crate::conversation::figures;
use crate::icons::IconName;

/// How long a thread waited on rests, its turn ended and nothing asked, before the message
/// goes: long enough that a turn's quick follow-up is not taken for its end.
pub(super) const SETTLE_MS: u32 = 60_000;

/// Whether a message sent as `delivery` is kept on the worker until its moment or the
/// person's word, rather than given to the agent's queue.
pub(super) const fn kept(delivery: Delivery) -> bool {
    matches!(delivery, Delivery::At { .. } | Delivery::After { .. } | Delivery::Draft)
}

/// The hour "tomorrow" means.
const MORNING: u8 = 9;

/// The other threads offered to wait on, the newest first.
const THREADS: usize = 6;

/// One line of the menu: what it says, when that is, and how the message goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LaterRow {
    /// Its words.
    pub label: String,
    /// When that is, beside them.
    pub when: Option<String>,
    /// How the message goes.
    pub delivery: Delivery,
}

/// When a message held for `delivery` goes, in words for a line of its own: "14:35",
/// "Tomorrow 09:00", "When “Fix the parser” rests". `title` names a thread. Nothing for a
/// message that goes with the turn.
pub(super) fn when_words(
    delivery: Delivery,
    now: WallMs,
    title: impl Fn(ThreadId) -> Option<String>,
) -> Option<String> {
    match delivery {
        Delivery::At { at_ms } => figures::stamp(at_ms, now),
        Delivery::After { thread, .. } => Some(match title(thread) {
            Some(title) => format!("When \u{201c}{title}\u{201d} rests"),
            None => "When another thread rests".to_owned(),
        }),
        Delivery::Draft => Some("Draft".to_owned()),
        Delivery::Steer | Delivery::Queue => None,
    }
}

/// The line over the field while the draft is set to go later: "Sends at 14:35", "Sends
/// tomorrow 09:00", "Sends when “Fix the parser” rests".
pub(super) fn sends_words(
    delivery: Delivery,
    now: WallMs,
    title: impl Fn(ThreadId) -> Option<String>,
) -> Option<String> {
    let when = when_words(delivery, now, title)?;
    Some(match delivery {
        Delivery::At { at_ms } if figures::stamp(at_ms, now) == figures::clock(at_ms) => {
            format!("Sends at {when}")
        }
        _ => {
            let mut chars = when.chars();
            let first = chars.next().map(|c| c.to_lowercase().collect::<String>());
            format!("Sends {}{}", first.unwrap_or_default(), chars.as_str())
        }
    })
}

/// The times the menu offers from `now`: in half an hour, an hour, three, and tomorrow at
/// nine.
pub(super) fn times(now: WallMs) -> Vec<LaterRow> {
    let after = |d: Duration| {
        WallMs::from_millis(
            now.as_millis().saturating_add(d.as_millis().try_into().unwrap_or(u64::MAX)),
        )
    };
    let row = |label: &str, at_ms: WallMs| LaterRow {
        label: label.to_owned(),
        when: figures::stamp(at_ms, now),
        delivery: Delivery::At { at_ms },
    };
    let mut rows = vec![
        row("In 30 minutes", after(Duration::from_mins(30))),
        row("In an hour", after(Duration::from_hours(1))),
        row("In 3 hours", after(Duration::from_hours(3))),
    ];
    rows.extend(figures::tomorrow_at(MORNING, now).map(|at| row("Tomorrow morning", at)));
    rows
}

impl ThreadView {
    /// Whether this thread's agent takes a message held until its moment.
    pub(super) fn schedules(&self, cx: &App) -> bool {
        self.state(cx).is_some_and(|s| s.meta.can(Cap::SCHEDULE))
    }

    /// A thread of this worker by its title, as the table names it.
    pub(super) fn thread_title(&self, thread: ThreadId, cx: &App) -> Option<String> {
        let rows = &self.hub.read(cx).threads().rows().rows;
        rows.get(&thread).map(|r| r.title.clone()).filter(|t| !t.trim().is_empty())
    }

    /// What the menu lists: the times, then the worker's other threads to wait on.
    pub(super) fn later_rows(&self, cx: &App) -> Vec<LaterRow> {
        let mut rows = times(WallMs::now());
        let table = &self.hub.read(cx).threads().rows().rows;
        let mut others: Vec<_> =
            table.values().filter(|r| r.id != self.thread && !r.title.trim().is_empty()).collect();
        others.sort_by_key(|r| std::cmp::Reverse(r.updated_ms));
        rows.extend(others.into_iter().take(THREADS).map(|r| LaterRow {
            label: format!("When \u{201c}{}\u{201d} rests", r.title),
            when: None,
            delivery: Delivery::After { thread: r.id, settle_ms: SETTLE_MS },
        }));
        rows
    }

    /// The menu of when to send open or shut; open, the keyboard walks it from its first row.
    pub(super) fn toggle_later(&mut self, cx: &mut Context<Self>) {
        let open = !self.composing.later_open();
        self.close_chip_menus();
        self.composing.open_later(open);
        cx.notify();
    }

    /// Send the draft as `delivery` sets, or as ↵ does again.
    pub(super) fn send_later(&mut self, delivery: Option<Delivery>, cx: &mut Context<Self>) {
        self.composing.set_later(delivery);
        cx.notify();
    }

    /// How the draft goes when it is sent, where it is set to go later.
    pub(super) const fn later(&self) -> Option<Delivery> {
        self.composing.later()
    }

    /// One line of the menu: its words, and when that is at the right.
    pub(super) fn later_row(&self, ix: usize, row: &LaterRow, cx: &Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let s = theme.surfaces;
        let said = row
            .when
            .as_ref()
            .map_or_else(|| row.label.clone(), |when| format!("{}, {when}", row.label));
        self.menu_row(ix, said, cx)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(self.z(theme.typography.small()))
                    .child(SharedString::from(row.label.clone())),
            )
            .children(row.when.clone().map(|when| {
                crate::kit::tabular(div())
                    .flex_none()
                    .text_size(self.z(theme.typography.small()))
                    .text_color(hsla(s.text_muted))
                    .child(SharedString::from(when))
            }))
            .into_any_element()
    }

    /// The line over the field while the draft is set to go later, and the way to take that
    /// off.
    pub(super) fn later_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let delivery = self.composing.later()?;
        let words = sends_words(delivery, WallMs::now(), |t| self.thread_title(t, cx))?;
        let theme = &self.theme;
        let s = theme.surfaces;
        Some(
            div()
                .id("thread-later")
                .debug_selector(|| "thread-later".to_owned())
                .role(Role::Status)
                .aria_label(SharedString::from(words.clone()))
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
                        .child(SharedString::from(words)),
                )
                .child(
                    self.icon_button("thread-later-clear", IconName::X, "Clear the schedule")
                        .on_click(cx.listener(|this, _ev, _w, cx| this.send_later(None, cx))),
                )
                .into_any_element(),
        )
    }

    /// The clock beside the send button that opens the menu of when to send, where the agent
    /// takes a message held until its moment.
    pub(super) fn later_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if !self.schedules(cx) || self.composing.editing() {
            return None;
        }
        let s = self.theme.surfaces;
        let open = self.composing.later_open();
        Some(
            self.icon_button("thread-later-open", IconName::Clock, "Send later")
                .aria_expanded(open)
                .when(open, |el| el.bg(hsla(s.selected)))
                .on_click(cx.listener(|this, _ev, window, cx| {
                    this.toggle_later(cx);
                    this.composer.update(cx, |c, cx| c.focus(window, cx));
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{Delivery, ThreadId};

    use super::{SETTLE_MS, sends_words, times, when_words};
    use crate::conversation::figures;

    /// What waits says when it goes, or what it waits on, by its thread's title.
    #[test]
    fn a_held_message_says_when_it_goes() {
        let now = WallMs::now();
        let soon = WallMs::from_millis(now.as_millis() + 60_000);
        let at = Delivery::At { at_ms: soon };
        let clock = figures::stamp(soon, now).expect("a time");
        assert_eq!(when_words(at, now, |_| None), Some(clock));
        let other = ThreadId::new();
        let after = Delivery::After { thread: other, settle_ms: SETTLE_MS };
        let named = |t: ThreadId| (t == other).then(|| "Fix the parser".to_owned());
        assert_eq!(
            when_words(after, now, named).as_deref(),
            Some("When \u{201c}Fix the parser\u{201d} rests")
        );
        assert_eq!(when_words(after, now, |_| None).as_deref(), Some("When another thread rests"));
        assert_eq!(when_words(Delivery::Queue, now, |_| None), None);
        assert_eq!(
            sends_words(after, now, named).as_deref(),
            Some("Sends when \u{201c}Fix the parser\u{201d} rests")
        );
        let tomorrow = figures::tomorrow_at(9, now).expect("a time");
        assert_eq!(
            sends_words(Delivery::At { at_ms: tomorrow }, now, |_| None).as_deref(),
            Some("Sends tomorrow 09:00")
        );
    }

    /// The menu's times run forward from now, each said as the tray will say it.
    #[test]
    fn the_times_run_forward_from_now() {
        let now = WallMs::now();
        let rows = times(now);
        let at: Vec<WallMs> = rows
            .iter()
            .filter_map(|r| match r.delivery {
                Delivery::At { at_ms } => Some(at_ms),
                _ => None,
            })
            .collect();
        assert_eq!(at.len(), 4);
        assert!(at.windows(2).all(|w| w[0] < w[1]) || at[3] > now, "{at:?}");
        assert!(at.iter().all(|a| *a > now));
        assert_eq!(rows[0].label, "In 30 minutes");
        assert!(rows.iter().all(|r| r.when.is_some()));
    }
}
