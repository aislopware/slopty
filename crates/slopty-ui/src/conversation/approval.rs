//! A permission prompt held for this client, and how it ended.
//!
//! The worker holds Claude Code's `PermissionRequest` while a client shows the face and sends
//! it to the followers; the card that asks owns the composer's place until the prompt settles.
//! An answer goes once: a second press while the first is on its way sends nothing. What
//! "always" grants is said before it is pressed, from the updates Claude Code suggested.

use slopty_core::ClientId;
use slopty_proto::conversation::{
    Grant, PermissionPrompt, Settled, Suggestion, ToolDetail, Verdict,
};

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

/// A piece of the line that says what "Always allow" grants: words, or a rule or command as
/// Claude Code writes it, set in the mono face.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Said {
    /// Prose.
    Words(String),
    /// A command, rule or path, as typed.
    Code(String),
}

impl Said {
    /// The text, whichever face it is set in.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Words(text) | Self::Code(text) => text,
        }
    }
}

/// What "Always allow" grants, as one sentence from the updates Claude Code suggested.
///
/// "Always allow stops asking for `npm test` commands in this project, for you". Empty when
/// nothing is suggested, and the button is not offered then.
#[must_use]
pub fn always_line(suggestions: &[Suggestion]) -> Vec<Said> {
    fn words(out: &mut Vec<Said>, text: &str) {
        match out.last_mut() {
            Some(Said::Words(last)) => last.push_str(text),
            _ => out.push(Said::Words(text.to_owned())),
        }
    }
    let mut out: Vec<Said> = Vec::new();
    let kept = suggestions.first().map(|first| &first.destination);
    let shared = suggestions.iter().all(|s| Some(&s.destination) == kept);
    for (i, suggestion) in suggestions.iter().enumerate() {
        words(&mut out, if i == 0 { "Always allow " } else { " and " });
        match &suggestion.grant {
            Grant::Rules { behavior, rules } => {
                if behavior == "allow" {
                    words(&mut out, "stops asking for ");
                } else {
                    words(&mut out, &format!("sets {behavior} for "));
                }
                for (j, rule) in rules.iter().enumerate() {
                    if j > 0 {
                        words(&mut out, ", ");
                    }
                    match bash_rule(rule) {
                        Some((command, true)) => {
                            out.push(Said::Code(command.to_owned()));
                            words(&mut out, " commands");
                        }
                        Some((command, false)) => out.push(Said::Code(command.to_owned())),
                        None => out.push(Said::Code(rule.clone())),
                    }
                }
            }
            Grant::Mode { mode } => {
                words(&mut out, &format!("switches to {} mode", mode_label(mode)));
            }
            Grant::Directories { directories } => {
                words(&mut out, "lets Claude work in ");
                out.push(Said::Code(directories.join(", ")));
            }
            Grant::Other { kind } => words(&mut out, kind),
        }
        if !shared && let Some(place) = destination(suggestion.destination.as_deref()) {
            words(&mut out, &format!(" {place}"));
        }
    }
    // Where every grant is kept alike, it is said once, at the end.
    if shared
        && let Some(place) =
            suggestions.first().and_then(|first| destination(first.destination.as_deref()))
    {
        words(&mut out, &format!(" {place}"));
    }
    out
}

/// A `Bash(…)` rule's command, and whether it covers every command that starts with it
/// (`Bash(npm test:*)`).
fn bash_rule(rule: &str) -> Option<(&str, bool)> {
    let inner = rule.strip_prefix("Bash(")?.strip_suffix(')')?;
    Some(match inner.strip_suffix(":*").or_else(|| inner.strip_suffix(" *")) {
        Some(prefix) => (prefix, true),
        None => (inner, false),
    })
}

/// What a held prompt asks, as a statement rather than a question: "Claude wants to run a
/// command", "Claude wants to edit view.rs".
#[must_use]
pub fn statement(prompt: &PermissionPrompt) -> String {
    let file = |path: &str| super::tools::file_name(path).to_owned();
    let what = match &prompt.detail {
        ToolDetail::Bash(_) => "run a command".to_owned(),
        ToolDetail::Edit(edit) => format!("edit {}", file(&edit.path)),
        ToolDetail::Write(write) => format!("write {}", file(&write.path)),
        ToolDetail::Read(read) => format!("read {}", file(&read.path)),
        ToolDetail::WebFetch(fetch) => {
            let host = fetch.url.split("://").nth(1).and_then(|r| r.split('/').next());
            format!("fetch {}", host.unwrap_or(&fetch.url))
        }
        ToolDetail::WebSearch(_) => "search the web".to_owned(),
        ToolDetail::Agent(_) => "start a subagent".to_owned(),
        ToolDetail::Plan { .. } => "leave plan mode".to_owned(),
        ToolDetail::Mcp(mcp) => format!("use {} from {}", mcp.tool, mcp.server),
        _ => format!("use {}", prompt.tool),
    };
    format!("Claude wants to {what}")
}

/// A permission mode's name as Claude Code's own footer says it; the default mode, which that
/// footer leaves unsaid, by what it does.
#[must_use]
pub fn mode_label(mode: &str) -> &str {
    match mode {
        "acceptEdits" => "Accept edits",
        "plan" => "Plan",
        "bypassPermissions" => "Bypass permissions",
        "default" => "Asks permission",
        "dontAsk" => "Don't ask",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{SessionId, WallMs};
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
            asked_ms: WallMs::from_millis(1),
            until_ms: WallMs::from_millis(2),
        }
    }

    /// A held prompt says what Claude wants as a statement, naming the file or the host.
    #[test]
    fn a_prompt_is_a_statement() {
        let mut asked = prompt(1);
        assert_eq!(statement(&asked), "Claude wants to use Bash");
        asked.detail = crate::conversation::fixtures::bash_prompt(SessionId::new(), 1).detail;
        assert_eq!(statement(&asked), "Claude wants to run a command");
        asked.detail = ToolDetail::WebFetch(slopty_proto::conversation::WebFetchDetail {
            url: "https://docs.rs/gpui/latest".to_owned(),
            prompt: None,
            code: None,
            bytes: None,
        });
        assert_eq!(statement(&asked), "Claude wants to fetch docs.rs");
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

    /// What "always" grants reads as one sentence: the command a rule covers in the mono
    /// face, the mode by its name, and where each is kept.
    #[test]
    fn always_says_what_it_grants() {
        let suggestions = vec![
            Suggestion {
                grant: Grant::Rules {
                    behavior: "allow".to_owned(),
                    rules: vec!["Bash(npm test:*)".to_owned(), "Read(/etc/**)".to_owned()],
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
        let said: String = always_line(&suggestions)
            .iter()
            .map(|s| match s {
                Said::Words(w) => w.clone(),
                Said::Code(c) => format!("`{c}`"),
            })
            .collect();
        assert_eq!(
            said,
            "Always allow stops asking for `npm test` commands, `Read(/etc/**)` in this project, \
             for you and switches to Accept edits mode for this session and lets Claude work in \
             `/work`"
        );
        let kept_alike = [
            suggestions[1].clone(),
            Suggestion { destination: Some("session".to_owned()), ..suggestions[2].clone() },
        ];
        let said: String = always_line(&kept_alike).iter().map(Said::text).collect();
        assert_eq!(
            said,
            "Always allow switches to Accept edits mode and lets Claude work in /work for this \
             session",
            "where both are kept, said once"
        );
        assert!(always_line(&[]).is_empty());
        assert_eq!(mode_label("default"), "Asks permission", "a behaviour, not a key");
    }
}
