// slopty: Slopty's own module (not in zeron).
//! slopty: what must hold of the shell's state whatever path its data took, checked as the
//! app runs.
//!
//! The person meets bugs where the worker, the view model and the shell meet: a message shown
//! twice, an echo left after its turn, an open chat unread. Each such state is named here as a
//! breach of what the person must see, and the shell's state is checked against them every
//! [`EVERY`]. A breach counts once it has stood for [`GRACE`], since a send or a turn passes
//! through each briefly. One is logged once, with what makes it up (`slopty::invariant`), and
//! under `SLOPTY_INVARIANTS=panic` (the e2e harness's) the app stops on it, so an e2e run that
//! meets one fails where it happened instead of in a golden later.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{App, Entity};
use slopty_view_doc::{MessagePart, MessageRole, SessionMessageEntry};
use slopty_view_proto::view::{Indicator, effective_indicator};

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
    /// A message the shell sent shows as its echo and in the transcript under another id: the
    /// greyed copy below the real message.
    EchoBesideItsMessage { chat: String, echo: String, entry: String },
    /// An echo still stands while the chat's agent works on nothing and nothing is on its way.
    EchoLeft { chat: String, echo: String },
    /// A message is both a queue row and in the transcript.
    QueuedAndSent { chat: String, id: String },
    /// The open chat, in the active window, reads as unread.
    OpenChatUnread { chat: String },
}

/// Whether `state`, as the window is `active`, breaches anything, at `now`.
#[must_use]
pub fn check(state: &AppState, active: bool, now: DateTime<Utc>) -> Vec<Breach> {
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
    let settled = state.transcript_replayed
        && state.pending_send_started(&chat, now).is_none()
        && effective_indicator(state.session_for(&chat), now) == Indicator::None;
    // Once nothing works and nothing is on its way, no echo is left to give way: one beside the
    // same words in the transcript is its message under another id. Times are not compared: an
    // echo's is this device's, an entry's its worker's.
    for echo in state.pending_echoes().iter().filter(|_| settled) {
        let words = words(echo);
        let twin = transcript.iter().rev().find(|entry| {
            entry.role == MessageRole::User && !words.is_empty() && self::words(entry) == words
        });
        breaches.push(match twin {
            Some(entry) => Breach::EchoBesideItsMessage {
                chat: chat.clone(),
                echo: echo.id.clone(),
                entry: entry.id.clone(),
            },
            None => Breach::EchoLeft { chat: chat.clone(), echo: echo.id.clone() },
        });
    }
    for queued in &state.queue {
        if transcript.iter().any(|entry| entry.id == queued.id) {
            breaches.push(Breach::QueuedAndSent { chat: chat.clone(), id: queued.id.clone() });
        }
    }
    if active && state.selected_chat_row().is_some_and(slopty_view_proto::Chat::unseen) {
        breaches.push(Breach::OpenChatUnread { chat });
    }
    breaches
}

/// A message's words, its text parts joined.
fn words(entry: &SessionMessageEntry) -> String {
    let parts = entry.parts.iter().filter_map(|part| match part {
        MessagePart::Text { text, .. } => Some(text.trim()),
        _ => None,
    });
    parts.collect::<Vec<_>>().join("\n")
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
                watch.standing(check(state, active, now), now)
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

    fn said(id: &str, words: &str) -> SessionMessageEntry {
        SessionMessageEntry {
            id: id.into(),
            role: MessageRole::User,
            parts: vec![MessagePart::Text { id: format!("{id}-text"), text: words.into() }],
            created_at: 0,
            device_id: "dev".into(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        }
    }

    /// The greyed copy below a sent message is a breach: its echo under one id beside the message
    /// under another, once nothing is on its way. Under one id, the echo gives way and nothing is
    /// wrong; while the send is on its way, the echo is what shows it.
    #[test]
    fn an_echo_beside_its_message_is_a_breach() {
        let now = at(0);
        let mut state = AppState::new();
        state.selected_chat = Some("c".to_owned());
        state.push_echo("c", said("shell-id", "Hello there"));
        state.begin_pending_send("c", "shell-id", now);
        state.apply_transcript(vec![said("agent-id", "Hello there")]);
        assert!(check(&state, true, now).is_empty(), "on its way");
        state.end_pending_send("c", "shell-id");
        let breach = Breach::EchoBesideItsMessage {
            chat: "c".to_owned(),
            echo: "shell-id".to_owned(),
            entry: "agent-id".to_owned(),
        };
        assert_eq!(check(&state, true, now), [breach]);

        let mut state = AppState::new();
        state.selected_chat = Some("c".to_owned());
        state.push_echo("c", said("one-id", "Hello there"));
        state.apply_transcript(vec![said("one-id", "Hello there")]);
        assert!(check(&state, true, now).is_empty(), "one id: the echo gave way");
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
