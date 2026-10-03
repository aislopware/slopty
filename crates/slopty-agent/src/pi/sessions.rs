//! pi's own sessions: where they are, what each is called, and the words that take one up
//! again or branch a new one off it.
//!
//! pi keeps one file per session in a directory per folder
//! (`<pi dir>/sessions/--<folder>--/<time>_<id>.jsonl`).
//!
//! Sans-IO but for [`agent_dir`]: the worker lists the directory and reads each file's head and
//! tail; this names what it finds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use slopty_core::WallMs;
use slopty_proto::thread::wire::PastSession;

use super::rpc::{Message, UserContent};
use crate::driven::title_of;

/// The flag that takes pi session `<id>` up again, as the person's own pi spells it.
pub const SESSION_FLAG: &str = "--session";

/// The flag that branches a new session off session `<id>`; with `--session-id` it names the new
/// one.
pub const FORK_FLAG: &str = "--fork";

/// How much of a session's file is read from each end to name it: its first message is near
/// the head, and a name given late is near the tail.
pub const READ: u64 = 64 * 1024;

/// pi's own directory: `PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
#[must_use]
pub fn agent_dir() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi").join("agent"))
    })
}

/// The directory under pi's `agent` directory that holds the sessions of folder `cwd`.
#[must_use]
pub fn folder(agent: &Path, cwd: &str) -> PathBuf {
    let name = format!("--{}--", cwd.trim_start_matches('/').replace(['/', '\\', ':'], "-"));
    agent.join("sessions").join(name)
}

/// The session id a session file's `name` carries (`<time>_<id>.jsonl`).
#[must_use]
pub fn id_of(name: &str) -> Option<&str> {
    let (_, id) = name.strip_suffix(".jsonl")?.split_once('_')?;
    valid(id).then_some(id)
}

/// Whether `id` is a session id pi takes: letters, digits, `.`, `_` and `-`, beginning and
/// ending with a letter or digit.
#[must_use]
pub fn valid(id: &str) -> bool {
    let ends = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    ends(id.chars().next())
        && ends(id.chars().next_back())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The arguments of a start that takes pi session `id` up again.
#[must_use]
pub fn resume_args(id: &str) -> Vec<String> {
    vec![SESSION_FLAG.to_owned(), id.to_owned()]
}

/// The session a start's `args` take up again and the flags after it, when they begin with
/// [`resume_args`].
#[must_use]
pub fn resumed(args: &[String]) -> Option<(&str, &[String])> {
    match args {
        [flag, id, rest @ ..] if flag == SESSION_FLAG && valid(id) => Some((id, rest)),
        _ => None,
    }
}

/// The flags that branch the new session off session `from`, for its first run only.
#[must_use]
pub fn fork_args(from: &str) -> [String; 2] {
    [FORK_FLAG.to_owned(), from.to_owned()]
}

/// What the session whose file begins with `head` and ends with `tail` is called.
///
/// It is the last name given it (`session_info`), else its first message's first line. Each
/// end may be cut; a line cut short is passed over.
#[must_use]
pub fn title(head: &str, tail: &str) -> Option<String> {
    let named = |text: &str| {
        text.lines()
            .rev()
            .filter(|line| line.contains("\"session_info\""))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("session_info"))
            .filter_map(|entry| entry.get("name").and_then(Value::as_str).map(str::to_owned))
            .find(|name| !name.trim().is_empty())
    };
    let first = || {
        head.lines()
            .filter(|line| line.contains("\"user\""))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|mut entry| entry.get_mut("message").map(Value::take))
            .filter_map(|message| serde_json::from_value::<Message>(message).ok())
            .find_map(|message| match message {
                Message::User { content } => Some(content),
                _ => None,
            })
            .map(|content: UserContent| content.text())
            .filter(|text| !text.trim().is_empty())
    };
    named(tail).or_else(|| named(head)).or_else(first).map(|text| title_of(&text))
}

/// Session `id` in folder `cwd`, called `title`, last written at `updated`, as a past session.
#[must_use]
pub fn past(id: &str, cwd: &str, title: Option<String>, updated: Option<WallMs>) -> PastSession {
    PastSession {
        agent: slopty_proto::thread::AgentId::named(slopty_proto::thread::AgentId::PI),
        native: id.to_owned(),
        cwd: Some(cwd.to_owned()),
        prompts: Vec::new(),
        title,
        updated_ms: updated,
        thread: None,
        resume: resume_args(id),
        facts: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is what follows the time in a file's name, and only an id pi takes is one.
    #[test]
    fn a_files_name_carries_its_session() {
        assert_eq!(id_of("2026-10-03T10-00-00-000Z_0b1c-2d.jsonl"), Some("0b1c-2d"));
        assert_eq!(id_of("2026-10-03T10-00-00-000Z_a_b.jsonl"), Some("a_b"));
        for name in ["notes.jsonl", "2026_x.json", "2026_-x.jsonl", "2026_x-.jsonl", "2026_.jsonl"]
        {
            assert_eq!(id_of(name), None, "{name}");
        }
        assert_eq!(folder(Path::new("/p"), "/a/b c"), Path::new("/p/sessions/--a-b c--"));
    }

    /// A start takes a session up again by pi's own flag, with the thread's flags after it;
    /// anything else is no resume.
    #[test]
    fn resume_words_round_trip() {
        let args = [resume_args("s-1"), vec!["--model".into(), "m".into()]].concat();
        assert_eq!(resumed(&args), Some(("s-1", &args[2..])));
        assert_eq!(resumed(&resume_args("s-1")), Some(("s-1", &[][..])));
        assert_eq!(resumed(&["--session".into(), "-x".into()]), None);
        assert_eq!(resumed(&["--model".into(), "m".into()]), None);
        assert_eq!(fork_args("s-1"), ["--fork", "s-1"]);
    }

    /// The last name given wins, wherever it is; without one the first message names it, its
    /// first line only; a line cut by the read is passed over.
    #[test]
    fn a_session_is_called_by_its_name_else_its_first_message() {
        let head = concat!(
            r#"{"type":"session","version":3,"id":"s","cwd":"/a"}"#,
            "\n",
            r#"{"type":"message","id":"1","message":{"role":"user","content":"Fix the login\nand more","timestamp":1}}"#,
            "\n",
            r#"{"type":"message","id":"2","message":{"role":"user","content":[{"type":"text","text":"Second"}],"timestamp":2}}"#,
            "\n",
            r#"{"type":"session_info","id":"3","name":"Cut sho"#,
        );
        assert_eq!(title(head, "").as_deref(), Some("Fix the login"));
        let tail = concat!(
            r#"ssion_info","name":"lost"}"#,
            "\n",
            r#"{"type":"session_info","id":"8","name":"Old name"}"#,
            "\n",
            r#"{"type":"session_info","id":"9","name":"Auth refactor"}"#,
            "\n",
        );
        assert_eq!(title(head, tail).as_deref(), Some("Auth refactor"));
        assert_eq!(title(r#"{"type":"session","id":"s"}"#, ""), None);
    }
}
