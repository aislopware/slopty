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
//! A picture pasted into the composer, or a file dropped on the face, is attached the way
//! Claude Code takes one from a terminal: by its path in the prompt. The file goes up to a fresh
//! directory of the worker's drop directory (`Dest::Attachment`), never the session's working
//! tree, where a screenshot would show in `git status` and could be committed. A chip in the
//! composer shows it while it uploads ([`Attachments`]); once it has landed, its path is typed
//! at the cursor ([`typed_paths`]).

use std::time::Duration;

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

/// The steps that send `text` as a message; nothing for text that is only blank.
#[must_use]
pub fn submission(text: &str) -> Vec<Step> {
    let text = text.trim_end();
    let text = text.trim_start_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let body =
        if is_command(text) { Step::Type(text.to_owned()) } else { Step::Paste(text.to_owned()) };
    vec![body, Step::Pause(SUBMIT_PAUSE), Step::Key("enter")]
}

/// The step that stops the agent's turn: Esc, as a person would press it.
#[must_use]
pub const fn interrupt() -> Step {
    Step::Key("escape")
}

/// The name a pasted picture lands under, in a directory of its own on the worker.
#[must_use]
pub fn picture_name(extension: &str) -> String {
    format!("pasted-image.{extension}")
}

/// What is typed at the cursor for `paths` that landed: each path as a terminal drop types it
/// (quoted where a shell would need it), then a space, with a space first when the cursor
/// follows a word.
#[must_use]
pub fn typed_paths(before: Option<char>, paths: &[String]) -> String {
    let typed = slopty_client::xfer::paste_paths(paths);
    if before.is_some_and(|c| !c.is_whitespace()) { format!(" {typed}") } else { typed }
}

/// One attachment on its way to the worker: its chip in the composer.
#[derive(Clone, PartialEq, Debug)]
pub struct Attachment {
    /// Names it to the workspace, which reports its progress and its landing.
    pub id: u64,
    /// The file's name.
    pub name: String,
    /// How far along, 0 to 1.
    pub fraction: f32,
}

impl Attachment {
    /// What the chip says after the name: `↑ 42%`.
    #[must_use]
    pub fn progress(&self) -> String {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0 to 100")]
        let percent = (self.fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
        format!("\u{2191} {percent}%")
    }
}

/// The attachments still uploading, in the order they were attached.
#[derive(Debug, Default)]
pub struct Attachments {
    next: u64,
    chips: Vec<Attachment>,
}

impl Attachments {
    /// A file called `name` starts to upload: its chip shows, and its id is returned.
    pub fn add(&mut self, name: String) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.chips.push(Attachment { id: self.next, name, fraction: 0.0 });
        self.next
    }

    /// Attachment `id` is `fraction` of the way up. Whether its chip changed.
    pub fn progress(&mut self, id: u64, fraction: f32) -> bool {
        let Some(chip) = self.chips.iter_mut().find(|c| c.id == id) else { return false };
        let changed = chip.progress() != Attachment { fraction, ..chip.clone() }.progress();
        chip.fraction = fraction;
        changed
    }

    /// Attachment `id` is done, landed or not: its chip goes. Whether it was here.
    pub fn end(&mut self, id: u64) -> bool {
        let before = self.chips.len();
        self.chips.retain(|c| c.id != id);
        self.chips.len() != before
    }

    /// The chips on show.
    #[must_use]
    pub fn chips(&self) -> &[Attachment] {
        &self.chips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message is pasted whole, then Enter after the pause; trailing blank lines go.
    #[test]
    fn a_message_is_pasted_then_entered() {
        assert_eq!(
            submission("Fix the build\n\nthen run the tests\n\n"),
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
            submission("/compact keep the plan"),
            [
                Step::Type("/compact keep the plan".to_owned()),
                Step::Pause(SUBMIT_PAUSE),
                Step::Key("enter"),
            ]
        );
        assert_eq!(submission("!git status").first(), Some(&Step::Type("!git status".to_owned())));
        assert_eq!(
            submission("/not\na command").first(),
            Some(&Step::Paste("/not\na command".to_owned())),
            "two lines are a message"
        );
    }

    /// An attachment shows a chip from the paste until it lands; its path is then typed at the
    /// cursor as a terminal drop types it, apart from a word before it.
    #[test]
    fn an_attachment_is_a_chip_until_its_path_is_typed() {
        let mut attached = Attachments::default();
        let shot = attached.add(picture_name("png"));
        let other = attached.add("notes.txt".to_owned());
        assert_eq!(attached.chips()[0].name, "pasted-image.png");
        assert_eq!(attached.chips()[0].progress(), "\u{2191} 0%");
        assert!(attached.progress(shot, 0.42));
        assert!(!attached.progress(shot, 0.421), "the chip says the same");
        assert_eq!(attached.chips()[0].progress(), "\u{2191} 42%");
        assert!(attached.end(shot) && !attached.end(shot));
        assert_eq!(attached.chips().iter().map(|c| c.id).collect::<Vec<_>>(), [other]);

        let path = "/Users/me/.slopty/drop/x/pasted-image.png".to_owned();
        assert_eq!(typed_paths(None, std::slice::from_ref(&path)), format!("{path} "));
        assert_eq!(typed_paths(Some(' '), std::slice::from_ref(&path)), format!("{path} "));
        assert_eq!(typed_paths(Some('t'), std::slice::from_ref(&path)), format!(" {path} "));
        assert_eq!(
            typed_paths(None, &["/tmp/Screen Shot.png".to_owned()]),
            "'/tmp/Screen Shot.png' ",
            "quoted as a drop on a shell is"
        );
    }

    /// Blank text sends nothing; Esc is the key a person presses to stop.
    #[test]
    fn blank_sends_nothing_and_esc_stops() {
        assert!(submission("  \n\n ").is_empty());
        assert_eq!(interrupt(), Step::Key("escape"));
    }
}
