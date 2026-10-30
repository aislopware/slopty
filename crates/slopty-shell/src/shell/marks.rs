//! The one state vocabulary every surface wears (`docs/decisions/layout.md`, "One state
//! vocabulary on every surface"). A session stands in one of five states, said by one mark in
//! one hue on the sidebar row, the titlebar identity, a pane header, the picker, the inbox, a
//! notification and the Dock badge:
//!
//! - **at work**: a blue spinner. The agent works, or waits on work of its own (a background
//!   command, a subagent it left running), which needs nothing from the person;
//! - **needs you**: amber, with the need in words from the agent's records ("Needs approval",
//!   "Has a question", "Plan ready for review");
//! - **failed**: red, until the person looks;
//! - **done and not yet seen**: a green ✓, and the title in the heavier weight until the person
//!   looks;
//! - **at rest**: no mark.
//!
//! The states come through the view model (`slopty_view_proto`'s `ChatIndicator`, `Session` and
//! `SessionWait`), which Slopty's adapters for Claude Code, Codex and pi feed, with a program's
//! OSC 7501 record as the fallback. Seen is one state across the person's devices: a thread's
//! seen mark (`ThreadRow::seen`) is the last turn the person saw on any of them, the chat carries
//! it as `last_seen_at`, and a finished or failed turn past it is what calls. Looking at a
//! session moves it, from whichever device looked (`AppState::mark_chat_seen`, `Intent::Seen`).

use chrono::{DateTime, Utc};
use gpui::{AnyElement, Hsla, SharedString, div, prelude::*};
use slopty_view_proto::{ChatIndicator, Session, SessionStatus, SessionWait};

use crate::loaders::status_mark::{MarkState, StatusMark};
use crate::state::{SESSION_STALE_MS, running_subagents};
use crate::theme::Theme;

/// What a session's mark says: one of the five states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mark {
    /// The agent works, or waits on its own work.
    AtWork,
    /// It waits on the person, for what the words say.
    NeedsYou(SharedString),
    /// Its run failed, and the person has not looked since.
    Failed,
    /// It finished, and the person has not looked since.
    DoneUnseen,
    /// Nothing to say.
    AtRest,
}

impl Mark {
    /// The mark of a session whose display status is `status` (seen already folded in,
    /// `slopty_view_proto::chat_indicator`) and whose live session is `session`, by `now`. A
    /// session whose machine stopped saying (past the staleness window) says nothing of its own
    /// work or need.
    pub(crate) fn of(status: ChatIndicator, session: Option<&Session>, now: DateTime<Utc>) -> Self {
        let session = live(session, now);
        match status {
            ChatIndicator::Working => Self::AtWork,
            ChatIndicator::AwaitingInput => {
                Self::NeedsYou(need(session.and_then(|session| session.waiting_on.as_ref())))
            }
            ChatIndicator::Errored => Self::Failed,
            ChatIndicator::Completed | ChatIndicator::Idle if works_on_its_own(session, now) => {
                Self::AtWork
            }
            ChatIndicator::Completed => Self::DoneUnseen,
            ChatIndicator::Idle => Self::AtRest,
        }
    }

    /// Whether the session's title takes the heavier weight: a result nobody has seen.
    pub(crate) fn heavy(&self) -> bool {
        *self == Self::DoneUnseen
    }

    /// The state in a word, as a row's status and a screen reader say it; the need itself is
    /// the [`Self::NeedsYou`] words.
    pub(crate) fn word(&self) -> Option<&'static str> {
        match self {
            Self::AtWork => Some("Working"),
            Self::NeedsYou(_) => Some("Needs you"),
            Self::Failed => Some("Failed"),
            Self::DoneUnseen => Some("Done"),
            Self::AtRest => None,
        }
    }

    /// What the mark draws, if anything.
    pub(crate) fn drawn(&self) -> Option<MarkState> {
        match self {
            Self::AtWork => Some(MarkState::Working),
            Self::NeedsYou(_) => Some(MarkState::Waiting),
            Self::Failed => Some(MarkState::Failed),
            Self::DoneUnseen => Some(MarkState::Done),
            Self::AtRest => None,
        }
    }

    /// Its hue, from the one table ([`hue`]); none at rest.
    pub(crate) fn hue(&self, theme: &Theme) -> Option<Hsla> {
        self.drawn().map(|state| hue(state, theme))
    }
}

/// The hue table: at work blue, needs you amber, failed red, done green. On light each takes
/// the tone of itself that clears 3:1 against the surfaces a mark sits on
/// ([`Theme::spinner_ink`]); never a grey.
pub(crate) fn hue(state: MarkState, theme: &Theme) -> Hsla {
    let tint = match state {
        MarkState::Working => at_work_blue(theme),
        MarkState::Waiting => theme.warning,
        MarkState::Failed => theme.danger,
        MarkState::Done => theme.success,
    };
    theme.spinner_ink(tint)
}

/// At work's blue, whatever the accent: an amber or green accent would otherwise read as needs
/// you or done. The Blue accent's own tone in each appearance.
fn at_work_blue(theme: &Theme) -> Hsla {
    match theme.appearance {
        crate::theme::Appearance::Dark => crate::theme::oklch(0.70, 0.17, 255.0),
        crate::theme::Appearance::Light => crate::theme::oklch(0.47, 0.21, 255.0),
    }
}

/// `mark` drawn `size` square as the mark `id` (it morphs from the state it last drew there),
/// the done tick cut in `ground`, the colour beneath it, and found by the tests as `selector`;
/// nothing at rest.
pub(crate) fn mark_element(
    id: SharedString,
    selector: String,
    mark: &Mark,
    size: f32,
    ground: Hsla,
) -> Option<AnyElement> {
    let state = mark.drawn()?;
    Some(
        div()
            .debug_selector(move || selector)
            .flex_none()
            .child(StatusMark::new(id, state).size(size).ground(ground))
            .into_any_element(),
    )
}

/// `session`, unless it has gone quiet past the staleness window (its machine stopped saying).
fn live(session: Option<&Session>, now: DateTime<Utc>) -> Option<&Session> {
    session.filter(|session| {
        now.signed_duration_since(session.updated_at).num_milliseconds() <= SESSION_STALE_MS
    })
}

/// What a session waiting on the person waits for, in the agent's records' words.
pub(crate) fn need(wait: Option<&SessionWait>) -> SharedString {
    let Some(wait) = wait else { return "Needs input".into() };
    let summary = wait
        .summary
        .as_deref()
        .map(crate::transcript::single_line)
        .filter(|summary| !summary.trim().is_empty());
    match wait.kind.as_str() {
        SessionWait::QUESTION => "Has a question".into(),
        SessionWait::PLAN => "Plan ready for review".into(),
        SessionWait::APPROVAL => match summary {
            Some(summary) => format!("Needs approval \u{b7} {summary}").into(),
            None => "Needs approval".into(),
        },
        _ => summary.map_or_else(|| "Needs input".into(), SharedString::from),
    }
}

/// Whether the agent, at rest, waits on its own work (a background command, a wakeup, a
/// subagent it left running), not on the person. A session past the staleness window says no.
pub(crate) fn works_on_its_own(session: Option<&Session>, now: DateTime<Utc>) -> bool {
    live(session, now).is_some_and(|session| {
        session.status == SessionStatus::Waiting
            || (session.status != SessionStatus::Working
                && session.status != SessionStatus::AwaitingInput
                && running_subagents(Some(session), now) > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(status: SessionStatus, wait: Option<(&str, Option<&str>)>, at: DateTime<Utc>) -> Session {
        Session {
            last_completed_turn: None,
            chat_id: "c".into(),
            device_id: "d".into(),
            status,
            started_at: Some(at),
            updated_at: at,
            running_subagents: 0,
            status_since: Some(at),
            waiting_on: wait.map(|(kind, summary)| SessionWait {
                kind: kind.into(),
                summary: summary.map(str::to_owned),
            }),
        }
    }

    /// Each display status reads as one of the five states, with the need in the agent's words;
    /// an agent's own background work is at work, and a machine gone quiet says nothing of it.
    #[test]
    fn every_status_reads_as_one_of_five_states() {
        let now = Utc::now();
        let mark = |status, session: Option<&Session>| Mark::of(status, session, now);
        assert_eq!(mark(ChatIndicator::Working, None), Mark::AtWork);
        let waits = [
            ((SessionWait::QUESTION, None), "Has a question"),
            ((SessionWait::APPROVAL, Some("run cargo test")), "Needs approval \u{b7} run cargo test"),
            ((SessionWait::APPROVAL, Some("  ")), "Needs approval"),
            ((SessionWait::PLAN, None), "Plan ready for review"),
            (("sign-in", Some("Log in to GitHub")), "Log in to GitHub"),
        ];
        for (wait, words) in waits {
            let asking = session(SessionStatus::AwaitingInput, Some(wait), now);
            let need = mark(ChatIndicator::AwaitingInput, Some(&asking));
            assert_eq!(need, Mark::NeedsYou(words.into()));
            assert_eq!(need.word(), Some("Needs you"));
        }
        assert_eq!(mark(ChatIndicator::AwaitingInput, None), Mark::NeedsYou("Needs input".into()));
        assert_eq!(mark(ChatIndicator::Errored, None), Mark::Failed);
        assert_eq!(mark(ChatIndicator::Completed, None), Mark::DoneUnseen);
        assert_eq!(mark(ChatIndicator::Idle, None), Mark::AtRest);
        let background = session(SessionStatus::Waiting, None, now);
        assert_eq!(mark(ChatIndicator::Completed, Some(&background)), Mark::AtWork);
        assert_eq!(mark(ChatIndicator::Idle, Some(&background)), Mark::AtWork);
        let mut subagent = session(SessionStatus::Done, None, now);
        subagent.running_subagents = 1;
        assert_eq!(mark(ChatIndicator::Idle, Some(&subagent)), Mark::AtWork);
        // A failure calls even while a subagent it left runs on.
        assert_eq!(mark(ChatIndicator::Errored, Some(&subagent)), Mark::Failed);
        let quiet = session(SessionStatus::Waiting, None, now - chrono::Duration::minutes(10));
        assert_eq!(mark(ChatIndicator::Idle, Some(&quiet)), Mark::AtRest);
        let words: Vec<_> = [Mark::AtWork, Mark::Failed, Mark::DoneUnseen, Mark::AtRest]
            .iter()
            .map(|mark| (mark.word(), mark.heavy(), mark.drawn()))
            .collect();
        assert_eq!(
            words,
            [
                (Some("Working"), false, Some(MarkState::Working)),
                (Some("Failed"), false, Some(MarkState::Failed)),
                (Some("Done"), true, Some(MarkState::Done)),
                (None, false, None),
            ]
        );
    }

    /// One hue per state, at work blue whatever the accent and none at rest, each a hue and
    /// never a grey, in both appearances.
    #[test]
    fn one_hue_per_state() {
        for mut theme in [Theme::dark(), Theme::light()] {
            let hues: Vec<Hsla> = [Mark::AtWork, Mark::NeedsYou("x".into()), Mark::Failed, Mark::DoneUnseen]
                .iter()
                .map(|mark| mark.hue(&theme).unwrap())
                .collect();
            for (i, a) in hues.iter().enumerate() {
                assert!(a.s > 0.3, "{a:?} is a hue, never a grey");
                for b in &hues[i + 1..] {
                    assert!((a.h - b.h).abs() > 0.03, "{a:?} and {b:?} read apart");
                }
            }
            let blue = hues[0].h * 360.0;
            assert!((200.0..260.0).contains(&blue), "at work is blue, not {blue}");
            assert_eq!(Mark::AtRest.hue(&theme), None);
            // An accent of another hue leaves at work blue.
            theme.accent = theme.warning;
            let still = Mark::AtWork.hue(&theme).unwrap();
            assert!((still.h - hues[0].h).abs() < 0.01, "{still:?} is still {:?}", hues[0]);
        }
    }
}
