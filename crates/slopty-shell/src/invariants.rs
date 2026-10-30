//! What must hold of the shell's state whatever path its data took, checked as the
//! app runs.
//!
//! The person meets bugs where the worker, the view model and the shell meet: a message shown
//! twice, an open chat unread. Each such state is named here as a breach of what the person must
//! see, and the shell's state is checked against them every [`EVERY`]. A breach counts once it
//! has stood for [`GRACE`], since a turn passes through each briefly. One is logged once, with what
//! makes it up (`slopty::invariant`), and under `SLOPTY_INVARIANTS=panic` (the e2e harness's) the
//! app stops on it, so an e2e run that meets one fails where it happened instead of in a golden
//! later.
#![allow(
    missing_docs,
    clippy::arithmetic_side_effects,
    clippy::cloned_ref_to_slice_refs,
    clippy::needless_pass_by_ref_mut,
    reason = "shell lint migration: slice A"
)]

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{App, Entity};

use crate::state::AppState;

/// How often the state is checked.
pub const EVERY: Duration = Duration::from_secs(2);

/// How long a breach stands before it counts.
pub const GRACE: Duration = Duration::from_secs(6);

/// Set to `panic` to stop the app on a breach.
pub const ENV: &str = "SLOPTY_INVARIANTS";

/// Something the person would see wrong.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Breach {
    /// The open chat holds two entries under one id.
    TwiceInTranscript { chat: String, id: String },
    /// The open chat, in the active window, reads as unread.
    OpenChatUnread { chat: String },
}

/// Whether `state`, as the window is `active`, breaches anything.
#[must_use]
pub fn check(state: &AppState, active: bool) -> Vec<Breach> {
    let mut breaches = Vec::new();
    let Some(chat) = state.selected_chat.clone() else { return breaches };
    let transcript = &state.transcript;
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for entry in transcript {
        let seen = ids.entry(entry.id.as_str()).or_default();
        *seen = seen.saturating_add(1);
        if *seen == 2 {
            breaches.push(Breach::TwiceInTranscript { chat: chat.clone(), id: entry.id.clone() });
        }
    }
    if active && state.selected_chat_row().is_some_and(slopty_view_proto::Chat::unseen) {
        breaches.push(Breach::OpenChatUnread { chat });
    }
    breaches
}

/// The breaches standing, by when each was first seen; and those already told.
#[derive(Debug, Default)]
pub struct Watch {
    since: HashMap<Breach, DateTime<Utc>>,
    told: Vec<Breach>,
}

impl Watch {
    /// The breaches of `now` that have stood for [`GRACE`] and were not told yet; those gone are
    /// forgotten, so one that comes back is told again.
    pub fn standing(&mut self, breaches: Vec<Breach>, now: DateTime<Utc>) -> Vec<Breach> {
        self.since.retain(|breach, _| breaches.contains(breach));
        self.told.retain(|breach| breaches.contains(breach));
        let grace = chrono::TimeDelta::from_std(GRACE).unwrap_or_default();
        let mut due = Vec::new();
        for breach in breaches {
            let since = *self.since.entry(breach.clone()).or_insert(now);
            if now.signed_duration_since(since) >= grace && !self.told.contains(&breach) {
                self.told.push(breach.clone());
                due.push(breach);
            }
        }
        due
    }
}

/// Check `state` every [`EVERY`] for as long as it lives, and tell what stands.
pub fn watch(state: &Entity<AppState>, cx: &mut App) {
    let state = state.downgrade();
    let stop = std::env::var(ENV).is_ok_and(|mode| mode == "panic");
    tracing::info!(target: "slopty::invariant", stop, "the shell's invariants are watched");
    cx.spawn(async move |cx| {
        let mut watch = Watch::default();
        loop {
            cx.background_executor().timer(EVERY).await;
            let Ok(due) = state.update(cx, |state, cx| {
                let now = crate::clock::utc(cx);
                let active = state.window_active;
                watch.standing(check(state, active), now)
            }) else {
                return;
            };
            for breach in due {
                tracing::error!(target: "slopty::invariant", ?breach, "the shell shows something wrong");
                assert!(!stop, "invariant breached: {breach:?}");
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000 + seconds, 0).expect("a time")
    }

    /// A breach counts once it has stood for the grace, is told once, and is told again only
    /// after it went away and came back.
    #[test]
    fn a_breach_is_told_once_it_stands() {
        let breach = Breach::OpenChatUnread { chat: "c".to_owned() };
        let mut watch = Watch::default();
        assert!(watch.standing(vec![breach.clone()], at(0)).is_empty(), "just seen");
        assert!(watch.standing(vec![breach.clone()], at(5)).is_empty(), "in its grace");
        assert_eq!(watch.standing(vec![breach.clone()], at(6)), [breach.clone()], "stood");
        assert!(watch.standing(vec![breach.clone()], at(9)).is_empty(), "told already");
        assert!(watch.standing(Vec::new(), at(10)).is_empty(), "gone");
        assert!(watch.standing(vec![breach.clone()], at(11)).is_empty(), "back: its grace again");
        assert_eq!(watch.standing(vec![breach.clone()], at(17)), [breach], "and told");
    }
}
