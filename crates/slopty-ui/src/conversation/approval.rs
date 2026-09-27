//! A permission prompt held for this client, and how it ended.
//!
//! The worker holds Claude Code's `PermissionRequest` while a client shows the face and sends
//! it to the followers; the card that asks owns the composer's place until the prompt settles.
//! An answer goes once: a second press while the first is on its way sends nothing. What
//! "always" grants is said before it is pressed, from the updates Claude Code suggested.

use slopty_core::ClientId;
use slopty_proto::conversation::{Grant, PermissionPrompt, Settled, Suggestion, Verdict};

/// How a prompt ended, as the line under the conversation says it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// This client answered.
    Answered(Verdict),
    /// Another client answered.
    Elsewhere(Verdict),
    /// Handed back to the TUI's own dialog: the terminal asks now.
    Released,
    /// Claude Code stopped waiting.
    Withdrawn,
}

impl Outcome {
    /// What the line says.
    #[must_use]
    pub fn text(&self, tool: &str) -> String {
        let verb = |verdict: &Verdict| match verdict {
            Verdict::Allow => format!("Allowed {tool} once"),
            Verdict::AllowAlways => format!("Always allowed {tool}"),
            Verdict::Deny { .. } => format!("Denied {tool}"),
        };
        match self {
            Self::Answered(verdict) => verb(verdict),
            Self::Elsewhere(verdict) => format!("{} on another device", verb(verdict)),
            Self::Released => format!("The terminal asks about {tool} now"),
            Self::Withdrawn => "Claude Code stopped waiting".to_owned(),
        }
    }

    /// The terminal has the question: the face offers to go there.
    #[must_use]
    pub const fn in_terminal(&self) -> bool {
        matches!(self, Self::Released)
    }
}

/// The prompts of one session as this client sees them: the one asked, and the last one that
/// settled.
#[derive(Clone, Debug, Default)]
pub struct Approvals {
    asked: Option<PermissionPrompt>,
    /// The answer sent for the prompt asked, until it settles.
    answering: Option<Verdict>,
    settled: Option<(String, Outcome)>,
}

impl Approvals {
    /// The worker asks. A newer prompt takes the card (Claude Code asks one at a time).
    pub fn asked(&mut self, prompt: PermissionPrompt) {
        self.asked = Some(prompt);
        self.answering = None;
        self.settled = None;
    }

    /// The prompt waiting on an answer here.
    #[must_use]
    pub const fn prompt(&self) -> Option<&PermissionPrompt> {
        self.asked.as_ref()
    }

    /// The answer on its way, if one was given.
    #[must_use]
    pub const fn answering(&self) -> Option<&Verdict> {
        self.answering.as_ref()
    }

    /// Answer the prompt: the request to send, the first time only.
    pub fn answer(&mut self, verdict: Verdict) -> Option<(u64, Verdict)> {
        let ask = self.asked.as_ref()?.ask;
        if self.answering.is_some() {
            return None;
        }
        self.answering = Some(verdict.clone());
        Some((ask, verdict))
    }

    /// The worker says prompt `ask` settled; `me` is this client's id there.
    pub fn settled(&mut self, ask: u64, outcome: Settled, me: Option<ClientId>) {
        let Some(prompt) = self.asked.take_if(|p| p.ask == ask) else { return };
        self.answering = None;
        let outcome = match outcome {
            Settled::Answered { verdict, by } if Some(by) == me => Outcome::Answered(verdict),
            Settled::Answered { verdict, .. } => Outcome::Elsewhere(verdict),
            Settled::Released => Outcome::Released,
            Settled::Withdrawn => Outcome::Withdrawn,
        };
        self.settled = Some((prompt.tool, outcome));
    }

    /// The last prompt that settled: its tool and how.
    #[must_use]
    pub fn last(&self) -> Option<(&str, &Outcome)> {
        self.settled.as_ref().map(|(tool, outcome)| (tool.as_str(), outcome))
    }

    /// The line about the last prompt has been read, or the agent moved on.
    pub fn clear_last(&mut self) {
        self.settled = None;
    }

    /// Following ended: whatever was asked here is the worker's to release.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Where a permission update is kept, as a person reads it.
fn destination(where_: Option<&str>) -> Option<&'static str> {
    match where_? {
        "session" => Some("for this session"),
        "localSettings" => Some("in this project, for you"),
        "projectSettings" => Some("in this project"),
        "userSettings" => Some("everywhere, for you"),
        _ => None,
    }
}

/// What "always" grants, one line per update: "Bash(npm test:*) in this project".
#[must_use]
pub fn grants(suggestions: &[Suggestion]) -> Vec<String> {
    suggestions
        .iter()
        .map(|s| {
            let what = match &s.grant {
                Grant::Rules { behavior, rules } if behavior == "allow" => rules.join(", "),
                Grant::Rules { behavior, rules } => format!("{behavior} {}", rules.join(", ")),
                Grant::Mode { mode } => format!("{} mode", mode_label(mode)),
                Grant::Directories { directories } => {
                    format!("Work in {}", directories.join(", "))
                }
                Grant::Other { kind } => kind.clone(),
            };
            match destination(s.destination.as_deref()) {
                Some(place) => format!("{what} {place}"),
                None => what,
            }
        })
        .collect()
}

/// A permission mode's name as Claude Code's own footer says it.
#[must_use]
pub fn mode_label(mode: &str) -> &str {
    match mode {
        "acceptEdits" => "Accept edits",
        "plan" => "Plan",
        "bypassPermissions" => "Bypass permissions",
        "default" => "Default",
        "dontAsk" => "Don't ask",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::SessionId;
    use slopty_proto::conversation::{Clipped, ToolDetail};

    use super::*;

    fn prompt(ask: u64) -> PermissionPrompt {
        PermissionPrompt {
            session: SessionId::new(),
            ask,
            tool: "Bash".to_owned(),
            detail: ToolDetail::Other {
                input: Clipped { text: "{}".to_owned(), lines: 1, chars: 2, full: None },
            },
            suggestions: Vec::new(),
            mode: Some("default".to_owned()),
            asked_ms: 1,
            until_ms: 2,
        }
    }

    /// An answer goes once: pressing again while it is on its way sends nothing.
    #[test]
    fn an_answer_goes_once() {
        let mut approvals = Approvals::default();
        assert_eq!(approvals.answer(Verdict::Allow), None, "nothing asked");
        approvals.asked(prompt(7));
        assert_eq!(approvals.answer(Verdict::Allow), Some((7, Verdict::Allow)));
        assert_eq!(approvals.answer(Verdict::AllowAlways), None, "already answering");
        assert_eq!(approvals.answering(), Some(&Verdict::Allow));
    }

    /// A prompt settles into the line that says how: this client's answer, another's, the
    /// terminal taking it back, Claude Code giving up. A settle for another prompt is not
    /// this one's.
    #[test]
    fn a_prompt_settles_into_how_it_ended() {
        let me = ClientId::new();
        let mut approvals = Approvals::default();
        approvals.asked(prompt(7));
        approvals.settled(8, Settled::Released, Some(me));
        assert!(approvals.prompt().is_some(), "another prompt's settle");
        approvals.settled(7, Settled::Answered { verdict: Verdict::Allow, by: me }, Some(me));
        assert!(approvals.prompt().is_none());
        let (tool, outcome) = approvals.last().unwrap();
        assert_eq!(outcome.text(tool), "Allowed Bash once");

        approvals.asked(prompt(9));
        assert!(approvals.last().is_none(), "a new prompt clears the old line");
        let other = ClientId::new();
        approvals.settled(
            9,
            Settled::Answered { verdict: Verdict::AllowAlways, by: other },
            Some(me),
        );
        let (tool, outcome) = approvals.last().unwrap();
        assert_eq!(outcome.text(tool), "Always allowed Bash on another device");

        approvals.asked(prompt(10));
        approvals.settled(10, Settled::Released, Some(me));
        let (tool, outcome) = approvals.last().unwrap();
        assert!(outcome.in_terminal());
        assert_eq!(outcome.text(tool), "The terminal asks about Bash now");
    }

    /// What "always" grants reads as the rule and where it is kept.
    #[test]
    fn always_says_what_it_grants() {
        let suggestions = vec![
            Suggestion {
                grant: Grant::Rules {
                    behavior: "allow".to_owned(),
                    rules: vec!["Bash(npm test:*)".to_owned()],
                },
                destination: Some("localSettings".to_owned()),
            },
            Suggestion {
                grant: Grant::Mode { mode: "acceptEdits".to_owned() },
                destination: Some("session".to_owned()),
            },
            Suggestion {
                grant: Grant::Directories { directories: vec!["/work".to_owned()] },
                destination: None,
            },
        ];
        assert_eq!(
            grants(&suggestions),
            [
                "Bash(npm test:*) in this project, for you",
                "Accept edits mode for this session",
                "Work in /work",
            ]
        );
    }
}
