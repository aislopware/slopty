//! What a group of tiles adds up to where their own rows are out of sight: a folded worker in
//! the navigator, a workspace's tab in the title bar. One mark in one fixed slot, the most
//! urgent first: the warn mark and how many wait on the human, else the working mark, else
//! the unseen dot. The slot keeps its width whether it holds a mark or nothing, so what sits
//! beside it never moves.
//!
//! Also the navigator's second line: the words it is made of and when its age starts.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::accesskit::Role;
use gpui::{
    Div, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{Status, status_mark};

/// What a group of tiles adds up to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Rollup {
    /// Tiles whose agent waits on the human.
    pub needs_you: usize,
    /// Tiles at work.
    pub working: usize,
    /// Tiles with news the human has not looked at.
    pub unseen: usize,
}

impl Rollup {
    /// Count one tile: its status and whether it has news not yet seen.
    pub const fn add(&mut self, status: Option<Status>, unseen: bool) {
        match status {
            Some(Status::NeedsYou) => self.needs_you = self.needs_you.saturating_add(1),
            Some(Status::Working) => self.working = self.working.saturating_add(1),
            _ if unseen => self.unseen = self.unseen.saturating_add(1),
            _ => {}
        }
    }

    /// The one thing the slot shows: what waits on the human outranks what works, which
    /// outranks what finished unseen.
    pub const fn shown(self) -> Option<Shown> {
        if self.needs_you > 0 {
            Some(Shown::NeedsYou(self.needs_you))
        } else if self.working > 0 {
            Some(Shown::Working)
        } else if self.unseen > 0 {
            Some(Shown::Unseen)
        } else {
            None
        }
    }

    /// What the slot says to a screen reader, to follow a label after a comma.
    pub fn words(self) -> Option<String> {
        self.shown().map(|shown| match shown {
            Shown::NeedsYou(n) => format!("{n} {}", if n == 1 { "needs you" } else { "need you" }),
            Shown::Working => "working".to_owned(),
            Shown::Unseen => "unseen".to_owned(),
        })
    }
}

/// What a rollup's slot shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shown {
    /// The warn mark, and the count beside it past one.
    NeedsYou(usize),
    /// The working mark.
    Working,
    /// The accent dot.
    Unseen,
}

/// The slot's width: a mark and a count of two figures beside it.
pub(super) fn slot_width(theme: &Theme) -> f32 {
    theme.typography.icon_large() + theme.spacing.md
}

/// The rollup's slot, `selector` naming it for tests, its content on its right edge.
pub(super) fn rollup_slot(theme: &Theme, selector: String, rollup: Rollup) -> Div {
    let s = &theme.surfaces;
    let shown = rollup.shown();
    let slot = div()
        .flex_none()
        .w(px(slot_width(theme)))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(theme.spacing.xxs));
    match shown {
        None => slot,
        Some(Shown::NeedsYou(n)) => slot
            .debug_selector(move || selector)
            .children((n > 1).then(|| {
                crate::kit::tabular(div())
                    .flex_none()
                    .text_size(px(theme.typography.caption()))
                    .text_color(hsla(s.warn))
                    .child(SharedString::from(n.to_string()))
            }))
            .child(status_mark(theme, Some(Status::NeedsYou), 1.0)),
        Some(Shown::Working) => slot.debug_selector(move || selector).child(status_mark(
            theme,
            Some(Status::Working),
            1.0,
        )),
        Some(Shown::Unseen) => slot.debug_selector(move || selector).child(
            div()
                .flex_none()
                .size(px(theme.typography.icon_large()))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("unseen")
                        .role(Role::Image)
                        .aria_label("Unseen")
                        .size(px(theme.spacing.xs + theme.spacing.xxs))
                        .rounded_full()
                        .bg(hsla(s.accent)),
                ),
        ),
    }
}

/// How long ago `started_ms` (Unix milliseconds) was at `now`; `None` for a start nobody gave
/// (zero), and a clock that disagrees about which came first counts as just now.
pub(super) fn age_at(started_ms: u64, now: SystemTime) -> Option<Duration> {
    (started_ms > 0).then(|| {
        let now = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        now.saturating_sub(Duration::from_millis(started_ms))
    })
}

/// The navigator's second line: the parts that say something, joined by a bullet.
pub(super) fn meta_line<'a>(parts: impl IntoIterator<Item = Option<&'a str>>) -> String {
    parts
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{2022} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slot shows the most urgent thing only: one waiting outranks any number at work,
    /// work outranks news not seen, and a group at rest shows nothing.
    #[test]
    fn a_rollup_shows_what_waits_then_what_works_then_what_is_unseen() {
        let mut r = Rollup::default();
        assert_eq!(r.shown(), None);
        r.add(Some(Status::Idle), false);
        r.add(None, false);
        assert_eq!(r.shown(), None, "at rest");
        r.add(Some(Status::Done), true);
        assert_eq!(r.shown(), Some(Shown::Unseen));
        r.add(Some(Status::Working), false);
        r.add(Some(Status::Working), false);
        assert_eq!(r.shown(), Some(Shown::Working));
        r.add(Some(Status::NeedsYou), false);
        assert_eq!(r.shown(), Some(Shown::NeedsYou(1)));
        assert_eq!(r.words().as_deref(), Some("1 needs you"));
        r.add(Some(Status::NeedsYou), false);
        assert_eq!(r.words().as_deref(), Some("2 need you"));
        assert_eq!(r, Rollup { needs_you: 2, working: 2, unseen: 1 });
    }

    /// Something at work or waiting is not also news: it counts once, as what it is doing.
    #[test]
    fn a_tile_counts_once() {
        let mut r = Rollup::default();
        r.add(Some(Status::Working), true);
        r.add(Some(Status::NeedsYou), true);
        assert_eq!(r, Rollup { needs_you: 1, working: 1, unseen: 0 });
    }

    /// An age runs from the session's start on the wall clock; no start is no age, and a start
    /// ahead of this clock is now.
    #[test]
    fn an_age_runs_from_the_start() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        assert_eq!(age_at(0, now), None);
        assert_eq!(age_at(400_000, now), Some(Duration::from_secs(600)));
        assert_eq!(age_at(2_000_000, now), Some(Duration::ZERO));
    }

    #[test]
    fn the_second_line_skips_what_says_nothing() {
        assert_eq!(
            meta_line([Some("oss/slopty"), None, Some(" "), Some("main")]),
            "oss/slopty • main"
        );
        assert_eq!(meta_line([None, None]), "");
    }
}
