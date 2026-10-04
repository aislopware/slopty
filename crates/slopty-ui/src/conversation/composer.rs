//! What the composer types into the agent's terminal.
//!
//! The composer never talks to Claude Code any other way than a person at the keyboard does:
//! it types into the same PTY the TUI reads. A message goes as a paste (bracketed, since
//! Claude Code asks for it, so a multi-line message arrives whole and nothing in it is read as
//! a key) and then Enter. A slash command or a `!` shell line goes as typed text, the way the
//! TUI's own completion and bash mode expect it. Enter waits [`SUBMIT_PAUSE`] after the text:
//! Claude Code's input reads a paste and an Enter that arrive in one read as one paste and
//! swallows the Enter (the worker's orchestration found it, `orchestrate::SUBMIT_PAUSE`).
//!
//! What it attaches goes as [`super::attach`] has it: by its landed paths after the text.

use std::time::Duration;

pub use super::attach::{Attach, Attachment, Attachments, Target, picture_name, with_paths};

/// Between the text and the Enter that submits it.
pub const SUBMIT_PAUSE: Duration = Duration::from_millis(200);

/// One thing the composer does to the terminal, in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    /// Text as keys typed (the terminal's raw input).
    Type(String),
    /// Text as a paste.
    Paste(String),
    /// Wait.
    Pause(Duration),
    /// A named key, as the terminal's key path names it (`enter`, `escape`).
    Key(&'static str),
}

/// Whether `text` is a line Claude Code reads as a command: a slash command or a `!` shell
/// line, on one line.
#[must_use]
pub fn is_command(text: &str) -> bool {
    text.starts_with(['/', '!']) && !text.contains('\n')
}

/// The steps that send `text` as a message with the attachments landed at `paths`.
///
/// Nothing for blank text with nothing attached. Whether it is typed as a command is the
/// text's call: the attachments' paths start with `/` and must not make a message a command.
#[must_use]
pub fn submission(text: &str, paths: &[String]) -> Vec<Step> {
    let text = text.trim_end();
    let text = text.trim_start_matches(['\n', '\r']);
    if text.trim().is_empty() && paths.is_empty() {
        return Vec::new();
    }
    let message = with_paths(text, paths);
    let body = if is_command(text) { Step::Type(message) } else { Step::Paste(message) };
    vec![body, Step::Pause(SUBMIT_PAUSE), Step::Key("enter")]
}

/// The step that stops the agent's turn: Esc, as a person would press it.
#[must_use]
pub const fn interrupt() -> Step {
    Step::Key("escape")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message is pasted whole, then Enter after the pause; trailing blank lines go.
    #[test]
    fn a_message_is_pasted_then_entered() {
        assert_eq!(
            submission("Fix the build\n\nthen run the tests\n\n", &[]),
            [
                Step::Paste("Fix the build\n\nthen run the tests".to_owned()),
                Step::Pause(SUBMIT_PAUSE),
                Step::Key("enter"),
            ]
        );
    }

    /// A slash command and a shell line are typed, as the TUI reads them from the keyboard.
    #[test]
    fn a_command_is_typed() {
        assert_eq!(
            submission("/compact keep the plan", &[]),
            [
                Step::Type("/compact keep the plan".to_owned()),
                Step::Pause(SUBMIT_PAUSE),
                Step::Key("enter"),
            ]
        );
        assert_eq!(
            submission("!git status", &[]).first(),
            Some(&Step::Type("!git status".to_owned()))
        );
        assert_eq!(
            submission("/not\na command", &[]).first(),
            Some(&Step::Paste("/not\na command".to_owned())),
            "two lines are a message"
        );
    }

    /// The attachments' paths go after the text; a leading slash never makes them a command,
    /// and a command takes them as its argument.
    #[test]
    fn a_path_goes_with_the_message_and_never_makes_it_a_command() {
        let path = "/Users/me/.slopty/drop/x/pasted-image.png".to_owned();
        let paths = std::slice::from_ref(&path);
        assert_eq!(submission("", paths).first(), Some(&Step::Paste(path.clone())));
        assert_eq!(
            submission("/review", paths).first(),
            Some(&Step::Type(format!("/review {path}")))
        );
    }

    /// Blank text sends nothing; Esc is the key a person presses to stop.
    #[test]
    fn blank_sends_nothing_and_esc_stops() {
        assert_eq!(submission("  \n\n ", &[]), Vec::<Step>::new());
        assert_eq!(interrupt(), Step::Key("escape"));
    }
}
