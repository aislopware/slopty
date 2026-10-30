//! Claude Code, the person's own `claude` TUI in a Slopty terminal (`docs/decisions/agents.md`,
//! "TUI-first").
//!
//! - [`Tui`]: how the worker starts it, on a session Slopty names.
//! - [`sessions`]: the live record that names a terminal's session, where Claude Code keeps a
//!   session's transcript, and the sessions of a folder.
//! - [`history`]: a session's transcript mapped onto the thread model; [`history::replay`] rebuilds
//!   a thread from the transcript alone.
//! - [`draft`]: the TUI's prompt box, read live through Slopty's mod, with the pictures pasted into
//!   it.

pub mod draft;
pub mod history;
pub mod sessions;

pub use history::thread_of;

/// The flag that names a new session.
pub const SESSION_ID_FLAG: &str = "--session-id";

/// The flag that takes a session up again by its id.
pub const RESUME_FLAG: &str = "--resume";

/// The flag the permission mode is given with.
pub const PERMISSION_MODE_FLAG: &str = "--permission-mode";

/// The flag the effort is given with.
pub const EFFORT_FLAG: &str = "--effort";

/// The flag the model is given with.
pub const MODEL_FLAG: &str = "--model";

/// The flag a TUI's addition to its system prompt is given with.
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";

/// How a TUI Slopty starts reaches its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Begin {
    /// A new session under this id.
    New(String),
    /// This session, taken up again.
    Resume(String),
}

impl Begin {
    /// The session's id.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::New(id) | Self::Resume(id) => id,
        }
    }
}

/// A Claude Code TUI Slopty starts in a terminal: the person's own `claude`, run as they would
/// run it, on a session Slopty names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tui {
    /// The session it runs.
    pub session: Begin,
    /// The model, only when the start named one.
    pub model: Option<String>,
    /// The permission mode, only when named.
    pub mode: Option<String>,
    /// The effort, only when named.
    pub effort: Option<String>,
    /// What its system prompt gets besides (a seated agent's role).
    pub system: Option<String>,
    /// Its first prompt, submitted as it opens.
    pub prompt: Option<String>,
}

impl Tui {
    /// Its arguments: the session, then each choice joined to its flag so no value is read as
    /// another flag, then the prompt after `--` so a prompt that begins with a dash stays one.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args = match &self.session {
            Begin::New(id) => vec![SESSION_ID_FLAG.to_owned(), id.clone()],
            Begin::Resume(id) => vec![RESUME_FLAG.to_owned(), id.clone()],
        };
        let given = |value: &Option<String>| {
            value.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
        };
        for (flag, value) in [
            (MODEL_FLAG, &self.model),
            (PERMISSION_MODE_FLAG, &self.mode),
            (EFFORT_FLAG, &self.effort),
            (APPEND_SYSTEM_PROMPT_FLAG, &self.system),
        ] {
            if let Some(value) = given(value) {
                args.push(format!("{flag}={value}"));
            }
        }
        if let Some(prompt) = self.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            args.extend(["--".to_owned(), prompt.to_owned()]);
        }
        args
    }
}

/// Whether `id` can be a Claude Code session id: it names a file.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TUI runs on the session Slopty names, with only the choices the start made, and its
    /// first prompt after `--` whatever it begins with.
    #[test]
    fn a_tui_runs_the_session_with_the_choices_made() {
        let mut tui = Tui {
            session: Begin::New("s1".to_owned()),
            model: Some("opus".to_owned()),
            mode: None,
            effort: Some(" ".to_owned()),
            system: Some("You review".to_owned()),
            prompt: Some("--help me".to_owned()),
        };
        assert_eq!(
            tui.args(),
            words(&[
                "--session-id",
                "s1",
                "--model=opus",
                "--append-system-prompt=You review",
                "--",
                "--help me"
            ])
        );
        tui.session = Begin::Resume("s0".to_owned());
        tui.prompt = None;
        tui.model = None;
        tui.system = None;
        assert_eq!(tui.args(), words(&["--resume", "s0"]));
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|&w| w.to_owned()).collect()
    }
}
