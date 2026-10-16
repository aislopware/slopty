//! Claude Code, driven the way Claude's own desktop app drives it: `claude -p` speaking
//! stream-json both ways, with the control protocol (`docs/decisions/agents.md`, "Each agent's
//! drive").
//!
//! - [`history`]: a session's record (its transcript, or the messages it streams, which are the
//!   same records) mapped onto the thread model; [`history::replay`] rebuilds a thread from the
//!   record alone.
//! - [`driven`]: the codec of one running Claude Code, sans-IO.
//! - [`sessions`]: where Claude Code keeps a session's record, and the sessions of a folder.
//! - [`offers`]: the models, modes and efforts a thread can be started with.
//! - [`args`] and [`ENV`]: how the worker runs it.

pub mod driven;
pub mod history;
pub mod offers;
pub mod sessions;

pub use history::{FAST, REVIEW_COMMAND, thread_of};

/// The flag that names a new session.
pub const SESSION_ID_FLAG: &str = "--session-id";

/// The flag that takes a session up again by its id.
pub const RESUME_FLAG: &str = "--resume";

/// The flag that has a resume go on in a new session, the one it resumes left as it was.
pub const FORK_SESSION_FLAG: &str = "--fork-session";

/// The flag the permission mode is given with.
pub const PERMISSION_MODE_FLAG: &str = "--permission-mode";

/// The flag the effort is given with.
pub const EFFORT_FLAG: &str = "--effort";

/// The flag the model is given with.
pub const MODEL_FLAG: &str = "--model";

/// The flags every driven Claude Code runs with.
///
/// A run that prints, reading and writing stream-json, asking about tool calls over the same
/// stream, echoing the messages it takes, and streaming what the model writes as it writes it.
pub const DRIVEN: [&str; 9] = [
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--permission-prompt-tool=stdio",
    "--replay-user-messages",
    "--include-partial-messages",
];

/// What Claude Code runs with besides the person's own environment, as the desktop app runs it.
///
/// Its output is flushed at once, MCP servers connect without holding the first turn, and its
/// questions to the person are asked over the stream.
pub const ENV: [(&str, &str); 3] = [
    ("CLAUDE_CODE_EAGER_FLUSH", "1"),
    ("MCP_CONNECTION_NONBLOCKING", "true"),
    ("CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL", "true"),
];

/// How a driven run reaches its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Session {
    /// A new session under this id.
    New(String),
    /// This session, taken up again.
    Resume(String),
    /// A new session under the second id, branched off the whole of the first.
    Fork(String, String),
}

impl Session {
    /// The id the run writes under.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::New(id) | Self::Resume(id) | Self::Fork(_, id) => id,
        }
    }
}

/// The arguments of a driven run on `session`, with the thread's own `kept` flags (a model, a
/// permission mode, an effort, a system prompt's addition, Slopty's tools) after them.
#[must_use]
pub fn args(session: &Session, kept: &[String]) -> Vec<String> {
    let mut args: Vec<String> = DRIVEN.iter().map(|&a| a.to_owned()).collect();
    match session {
        Session::New(id) => args.extend([SESSION_ID_FLAG.to_owned(), id.clone()]),
        Session::Resume(id) => args.extend([RESUME_FLAG.to_owned(), id.clone()]),
        Session::Fork(from, id) => args.extend([
            RESUME_FLAG.to_owned(),
            from.clone(),
            FORK_SESSION_FLAG.to_owned(),
            SESSION_ID_FLAG.to_owned(),
            id.clone(),
        ]),
    }
    args.extend(kept.iter().cloned());
    args
}

/// The flags a start keeps for every run of its thread: the model, the permission mode and the
/// effort it chose, each joined to its flag so no value is read as another flag.
#[must_use]
pub fn kept(model: Option<&str>, mode: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut kept = Vec::new();
    for (flag, value) in [(MODEL_FLAG, model), (PERMISSION_MODE_FLAG, mode), (EFFORT_FLAG, effort)]
    {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            kept.push(format!("{flag}={value}"));
        }
    }
    kept
}

/// Whether `id` can be a Claude Code session id: it names a file.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A model for people: an alias (`sonnet[1m]` is "Sonnet 1M") or an id (`claude-opus-5-5` is
/// "Opus 5.5").
#[must_use]
pub fn model_label(model: &str) -> String {
    let named = model.strip_prefix("claude-").map(|rest| {
        let mut words = rest.split('-');
        let family = words.next().unwrap_or_default();
        let version: Vec<&str> =
            words.take_while(|w| w.chars().all(|c| c.is_ascii_digit()) && w.len() <= 2).collect();
        (family.to_owned(), version.join("."))
    });
    let (base, version) = match named {
        Some((family, version)) => (family, version),
        None => (model.to_owned(), String::new()),
    };
    let (base, wide) = match base.strip_suffix("[1m]") {
        Some(base) => (base.to_owned(), " 1M"),
        None => (base, ""),
    };
    let mut chars = base.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    let version = if version.is_empty() { String::new() } else { format!(" {version}") };
    format!("{first}{}{version}{wide}", chars.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A driven run prints stream-json both ways, asks over the stream, and names its session;
    /// the thread's own flags come after.
    #[test]
    fn a_driven_run_speaks_stream_json() {
        let kept = kept(Some("opus"), Some("plan"), None);
        let run = args(&Session::New("s1".to_owned()), &kept);
        assert_eq!(run.first().map(String::as_str), Some("-p"));
        assert!(run.windows(2).any(|w| w == ["--input-format", "stream-json"]));
        assert!(run.contains(&"--permission-prompt-tool=stdio".to_owned()));
        assert!(run.windows(2).any(|w| w == ["--session-id", "s1"]));
        assert!(run.ends_with(&["--model=opus".to_owned(), "--permission-mode=plan".to_owned()]));
        let fork = args(&Session::Fork("a".to_owned(), "b".to_owned()), &[]);
        assert!(fork.ends_with(&words(&["--resume", "a", "--fork-session", "--session-id", "b"])));
    }

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|&w| w.to_owned()).collect()
    }

    /// Models read as people say them.
    #[test]
    fn models_are_named_for_people() {
        assert_eq!(model_label("sonnet[1m]"), "Sonnet 1M");
        assert_eq!(model_label("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(model_label("opusplan"), "Opusplan");
    }
}
