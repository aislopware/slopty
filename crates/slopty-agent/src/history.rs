//! The person's past prompts, as each agent records them, for the worker's prompt search
//! (`slopty_worker::thread::history`).
//!
//! Sans-IO: the worker reads the files and hands their lines here.
//!
//! - **Claude Code** appends every prompt to `~/.claude/history.jsonl`, the record its own ↑ recall
//!   reads: the words as shown (`display`), when (`timestamp`, ms), the folder (`project`) and the
//!   session (`sessionId`). A long paste stands in the words as `[Pasted text #N +M lines]`, its
//!   text in `pastedContents`, or in `paste-cache/<contentHash>.txt` when it was kept apart. No
//!   transcript is read.
//! - **Codex** appends every prompt its TUI is sent to `$CODEX_HOME/history.jsonl` (`session_id`,
//!   `ts` in seconds, `text`). The folder is in the first line of the session's rollout
//!   (`session_meta`), which also says who ran it ([`codex::Kept`]). A thread another client runs
//!   on Codex's app-server (Slopty's own, an editor's) is in no history, so its prompts are read
//!   from its rollout.
//! - **pi** writes every message into the session's own file, the folder in its first line.
//!
//! A slash command (`/model opus`) is a command to the agent, not a prompt, and is left out. A
//! prompt is kept to its first [`PROMPT_BYTES`].

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use slopty_core::WallMs;

/// How much of one prompt is kept, in bytes, its pastes put back: enough for any prompt a
/// person writes, bounded for one that carried a whole log.
pub const PROMPT_BYTES: usize = 32 * 1024;

/// One of the person's prompts.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Prompt {
    /// The agent's own id for the session it went to.
    pub session: String,
    /// The folder the session ran in, when the record says.
    pub cwd: Option<String>,
    /// When it was sent, when the record says.
    pub at_ms: Option<WallMs>,
    /// What it said, cut to [`PROMPT_BYTES`].
    pub text: String,
}

/// Whether `text` is a slash command to the agent rather than a prompt: one line whose first
/// word is `/` and a name (`/model opus`, `/clear`). A path (`/Users/x/a is broken`) is a prompt.
#[must_use]
pub fn is_command(text: &str) -> bool {
    let text = text.trim();
    let Some(name) = text.split_whitespace().next().and_then(|w| w.strip_prefix('/')) else {
        return false;
    };
    !name.is_empty()
        && !text.contains('\n')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

/// `text` cut to at most `bytes`, on a character's boundary.
#[must_use]
pub fn clip(mut text: String, bytes: usize) -> String {
    if text.len() > bytes {
        let mut end = bytes;
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text.truncate(end);
    }
    text
}

/// A prompt worth keeping: not empty, not a command; cut to [`PROMPT_BYTES`].
fn kept(session: &str, cwd: Option<String>, at_ms: Option<WallMs>, text: &str) -> Option<Prompt> {
    let text = text.trim();
    if session.is_empty() || text.is_empty() || is_command(text) {
        return None;
    }
    let text = clip(text.to_owned(), PROMPT_BYTES);
    Some(Prompt { session: session.to_owned(), cwd, at_ms, text })
}

/// Claude Code's prompt history.
pub mod claude {
    use super::{BTreeMap, Deserialize, Prompt, WallMs, kept};

    /// Its file, under Claude Code's directory (`~/.claude`).
    pub const HISTORY: &str = "history.jsonl";

    /// The directory, under Claude Code's, that holds a paste kept apart, as
    /// `<contentHash>.txt`.
    pub const PASTES: &str = "paste-cache";

    #[derive(Deserialize)]
    struct Line {
        display: String,
        #[serde(rename = "pastedContents", default)]
        pasted: BTreeMap<String, Paste>,
        timestamp: Option<u64>,
        project: Option<String>,
        #[serde(rename = "sessionId")]
        session: Option<String>,
    }

    #[derive(Deserialize)]
    struct Paste {
        #[serde(rename = "type")]
        kind: Option<String>,
        content: Option<String>,
        #[serde(rename = "contentHash")]
        hash: Option<String>,
    }

    /// Whether `hash` can name a paste: hex digits only, so it names a file in the paste
    /// directory and nothing outside it.
    #[must_use]
    pub fn is_paste_hash(hash: &str) -> bool {
        !hash.is_empty() && hash.len() <= 128 && hash.chars().all(|c| c.is_ascii_hexdigit())
    }

    /// The prompt one line of the history records, its pastes put back.
    ///
    /// A paste's text comes from the line, or from `kept_apart` by its hash
    /// ([`is_paste_hash`]); a paste that cannot be had keeps its marker. `None` for a line that is
    /// cut, not JSON, names no session, or records a command.
    pub fn prompt(
        line: &str,
        kept_apart: &mut dyn FnMut(&str) -> Option<String>,
    ) -> Option<Prompt> {
        let line: Line = serde_json::from_str(line).ok()?;
        let session = line.session?;
        let text = expand(&line.display, &line.pasted, kept_apart);
        kept(&session, line.project, line.timestamp.map(WallMs::from_millis), &text)
    }

    /// `display` with each `[Pasted text #N …]` marker replaced by paste `N`'s text.
    fn expand(
        display: &str,
        pasted: &BTreeMap<String, Paste>,
        kept_apart: &mut dyn FnMut(&str) -> Option<String>,
    ) -> String {
        const MARK: &str = "[Pasted text #";
        let mut out = String::with_capacity(display.len());
        let mut rest = display;
        while let Some(at) = rest.find(MARK) {
            let (before, marker) = rest.split_at(at);
            out.push_str(before);
            let Some(close) = marker.find(']') else {
                rest = marker;
                break;
            };
            let (whole, after) = marker.split_at(close.saturating_add(1));
            let number = whole
                .get(MARK.len()..)
                .unwrap_or_default()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap_or_default();
            let text = pasted
                .get(number)
                .filter(|paste| paste.kind.as_deref().is_none_or(|kind| kind == "text"))
                .and_then(|paste| {
                    paste.content.clone().or_else(|| {
                        paste
                            .hash
                            .as_deref()
                            .filter(|h| is_paste_hash(h))
                            .and_then(&mut *kept_apart)
                    })
                });
            out.push_str(text.as_deref().unwrap_or(whole));
            rest = after;
        }
        out.push_str(rest);
        out
    }
}

/// Codex's prompt history and rollouts.
pub mod codex {
    use super::{Prompt, Value, WallMs, kept};

    /// Its prompt history, under Codex's directory (`$CODEX_HOME`, else `~/.codex`).
    pub const HISTORY: &str = "history.jsonl";

    /// The directories, under Codex's, that hold its rollouts, by day
    /// (`sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`).
    pub const SESSIONS: [&str; 2] = ["sessions", "archived_sessions"];

    /// The prompt one line of the history records, its folder not known. `None` for a line
    /// that is cut, not JSON, names no session, or records a command.
    #[must_use]
    pub fn history(line: &str) -> Option<Prompt> {
        let line: Value = serde_json::from_str(line).ok()?;
        let session = line.get("session_id")?.as_str()?;
        let text = line.get("text")?.as_str()?.to_owned();
        let at = line.get("ts").and_then(Value::as_u64).map(|s| s.saturating_mul(1_000));
        kept(session, None, at.map(WallMs::from_millis), &text)
    }

    /// The thread a rollout's file is of: its name's last 36 characters before `.jsonl`.
    #[must_use]
    pub fn id_of(name: &str) -> Option<&str> {
        let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
        let id = stem.get(stem.len().checked_sub(36)?..)?;
        let uuid = id.len() == 36
            && id.char_indices().all(|(at, c)| match at {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            });
        uuid.then_some(id)
    }

    /// Where a thread's prompts are kept, by who ran it.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Kept {
        /// Codex's TUI ran it: its prompts are in the history.
        History,
        /// Another client of the app-server ran it: its prompts are in its rollout only.
        Rollout,
        /// Codex ran it for someone else's prompt (`codex exec` from a script or another agent,
        /// a subagent of a thread): none of its prompts is the person's.
        Nobody,
    }

    /// What the first line of a rollout says of its thread.
    #[derive(Clone, PartialEq, Eq, Debug)]
    pub struct Meta {
        /// The thread.
        pub id: String,
        /// The folder it ran in.
        pub cwd: Option<String>,
        /// Where its prompts are.
        pub kept: Kept,
    }

    /// The thread a rollout's first line (`session_meta`) describes.
    #[must_use]
    pub fn meta(line: &str) -> Option<Meta> {
        let line: Value = serde_json::from_str(line).ok()?;
        if line.get("type")?.as_str()? != "session_meta" {
            return None;
        }
        let payload = line.get("payload")?;
        let id = payload.get("id")?.as_str()?.to_owned();
        let cwd = payload.get("cwd").and_then(Value::as_str).map(str::to_owned);
        let kept = match payload.get("source") {
            Some(Value::String(source)) if source == "cli" => Kept::History,
            Some(Value::String(source)) if source == "exec" => Kept::Nobody,
            Some(Value::Object(source)) if source.contains_key("subagent") => Kept::Nobody,
            _ => Kept::Rollout,
        };
        Some(Meta { id, cwd, kept })
    }

    /// How a rollout records a prompt.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Said {
        /// As the event Codex sends its clients (`event_msg` `user_message`): the words the
        /// person sent, and nothing Codex added.
        Event,
        /// As an item of the model's input (`response_item`, a `user` message), which also
        /// carries what Codex puts before a thread's first turn.
        Item,
    }

    /// The prompt one line of thread `meta`'s rollout records, and how. A rollout that has
    /// [`Said::Event`]s is read by those alone, since each turn has both.
    #[must_use]
    pub fn rollout(line: &str, meta: &Meta) -> Option<(Prompt, Said)> {
        // Most of a rollout is the model's output; only a prompt's line is parsed.
        if !line.contains("\"user") {
            return None;
        }
        let line: Value = serde_json::from_str(line).ok()?;
        let at =
            line.get("timestamp").and_then(Value::as_str).and_then(crate::conversation::parse_ms);
        let payload = line.get("payload")?;
        let (text, said) = match (line.get("type")?.as_str()?, payload.get("type")?.as_str()?) {
            ("event_msg", "user_message") => {
                (payload.get("message")?.as_str()?.to_owned(), Said::Event)
            }
            ("response_item", "message") if payload.get("role")?.as_str()? == "user" => {
                let texts: Vec<&str> = payload
                    .get("content")?
                    .as_array()?
                    .iter()
                    .filter(|part| {
                        matches!(
                            part.get("type").and_then(Value::as_str),
                            Some("input_text" | "text")
                        )
                    })
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect();
                let text = texts.join("\n");
                if added(&text) {
                    return None;
                }
                (text, Said::Item)
            }
            _ => return None,
        };
        Some((kept(&meta.id, meta.cwd.clone(), at, &text)?, said))
    }

    /// Whether a `user` input item is what Codex puts before a thread's first turn rather than
    /// the person's words: the folder's `AGENTS.md`, or a tagged block of its own
    /// (`<environment_context>`, `<user_instructions>`).
    fn added(text: &str) -> bool {
        let text = text.trim_start();
        text.starts_with("# AGENTS.md instructions")
            || text.strip_prefix('<').is_some_and(|tag| {
                tag.split(['>', ' ']).next().is_some_and(|name| {
                    !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                })
            })
    }
}

/// pi's session files.
pub mod pi {
    use super::{Prompt, Value, WallMs, kept};
    use crate::pi::rpc::Message;

    /// The session and folder a session file's first line (`session`) names.
    #[must_use]
    pub fn header(line: &str) -> Option<(String, String)> {
        let line: Value = serde_json::from_str(line).ok()?;
        if line.get("type")?.as_str()? != "session" {
            return None;
        }
        Some((line.get("id")?.as_str()?.to_owned(), line.get("cwd")?.as_str()?.to_owned()))
    }

    /// The prompt one line of session `session`'s file, in folder `cwd`, records: a message
    /// whose role is the person's.
    #[must_use]
    pub fn prompt(line: &str, session: &str, cwd: &str) -> Option<Prompt> {
        // Most of a session is the model's and the tools' output; only a prompt's line is parsed.
        if !line.contains("\"user\"") {
            return None;
        }
        let mut line: Value = serde_json::from_str(line).ok()?;
        if line.get("type")?.as_str()? != "message" {
            return None;
        }
        let message = line.get_mut("message").map(Value::take)?;
        let at = message.get("timestamp").and_then(Value::as_u64).map(WallMs::from_millis).or_else(
            || {
                line.get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(crate::conversation::parse_ms)
            },
        );
        let Message::User { content } = serde_json::from_value::<Message>(message).ok()? else {
            return None;
        };
        kept(session, Some(cwd.to_owned()), at, &content.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_paste(_: &str) -> Option<String> {
        None
    }

    /// A command is a slash and a name on one line; a path, a prompt that begins with one on
    /// several lines, or a bare slash is a prompt.
    #[test]
    fn a_slash_command_is_no_prompt() {
        for command in ["/clear", "/model opus", "  /compact keep the plan ", "/mcp:tool x"] {
            assert!(is_command(command), "{command}");
        }
        for prompt in ["/Users/x/a.rs is broken", "/explain\nthis", "/", "fix /etc", "hi"] {
            assert!(!is_command(prompt), "{prompt}");
        }
        assert_eq!(clip("añb".to_owned(), 2), "a", "cut on a character's boundary");
    }

    /// A Claude Code line gives its words, folder, session and time; its pastes are put back
    /// from the line or from the paste kept apart, and one that cannot be had keeps its marker.
    #[test]
    fn claude_codes_history_gives_prompts_with_their_pastes() {
        let line = r#"{"display":"look [Pasted text #1 +3 lines] and [Pasted text #2 +9 lines] and [Pasted text #3]","pastedContents":{"1":{"id":1,"type":"text","content":"inline log"},"2":{"id":2,"type":"text","contentHash":"ab12"},"3":{"id":3,"type":"text","contentHash":"../etc"}},"timestamp":1779780784751,"project":"/w/app","sessionId":"5f0c"}"#;
        let mut asked = Vec::new();
        let mut apart = |hash: &str| {
            asked.push(hash.to_owned());
            (hash == "ab12").then(|| "kept apart".to_owned())
        };
        let prompt = claude::prompt(line, &mut apart).expect("a prompt");
        assert_eq!(prompt.text, "look inline log and kept apart and [Pasted text #3]");
        assert_eq!(asked, ["ab12"], "a hash that could name another file is never asked for");
        assert_eq!(prompt.session, "5f0c");
        assert_eq!(prompt.cwd.as_deref(), Some("/w/app"));
        assert_eq!(prompt.at_ms, Some(WallMs::from_millis(1_779_780_784_751)));

        let command = r#"{"display":"/model opus","pastedContents":{},"timestamp":1,"project":"/w","sessionId":"s"}"#;
        assert_eq!(claude::prompt(command, &mut no_paste), None);
        let sessionless = r#"{"display":"hi","pastedContents":{},"timestamp":1,"project":"/w"}"#;
        assert_eq!(claude::prompt(sessionless, &mut no_paste), None);
        let cut = r#"{"display":"hi","pastedContents":{},"timest"#;
        assert_eq!(claude::prompt(cut, &mut no_paste), None, "a line cut short is skipped");
        let long = format!(
            r#"{{"display":"{}","pastedContents":{{}},"timestamp":1,"project":"/w","sessionId":"s"}}"#,
            "x".repeat(PROMPT_BYTES * 2)
        );
        let long = claude::prompt(&long, &mut no_paste).expect("a long prompt");
        assert_eq!(long.text.len(), PROMPT_BYTES, "a prompt is kept to its first bytes");
    }

    /// Codex's history gives prompts in seconds; a rollout's first line says where its prompts
    /// are, and its prompts are read from events, or from items without what Codex added.
    #[test]
    fn codex_gives_prompts_from_its_history_and_rollouts() {
        let line = r#"{"session_id":"019a","ts":1780559712,"text":"add a retry"}"#;
        let prompt = codex::history(line).expect("a prompt");
        assert_eq!((prompt.session.as_str(), prompt.cwd), ("019a", None));
        assert_eq!(prompt.at_ms, Some(WallMs::from_millis(1_780_559_712_000)));
        assert_eq!(codex::history(r#"{"session_id":"019a","ts":1,"text":"/status"}"#), None);

        let id = "01a1009b-ce5e-7d91-b77c-80127c630fd6";
        let name = format!("rollout-2026-10-03T14-12-53-{id}.jsonl");
        assert_eq!(codex::id_of(&name), Some(id));
        assert_eq!(codex::id_of("rollout-short.jsonl"), None);
        assert_eq!(codex::id_of(&format!("other-{id}.jsonl")), None);

        let first = |source: &str| {
            format!(
                r#"{{"timestamp":"2026-10-03T10:00:00.000Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/w/api","source":{source}}}}}"#
            )
        };
        let kept = |source: &str| codex::meta(&first(source)).map(|m| m.kept);
        assert_eq!(kept(r#""cli""#), Some(codex::Kept::History));
        assert_eq!(kept(r#""exec""#), Some(codex::Kept::Nobody));
        assert_eq!(kept(r#"{"subagent":"review"}"#), Some(codex::Kept::Nobody));
        assert_eq!(kept(r#""vscode""#), Some(codex::Kept::Rollout));
        let meta = codex::meta(&first(r#""vscode""#)).expect("meta");
        assert_eq!(meta.cwd.as_deref(), Some("/w/api"));

        let event = r#"{"timestamp":"2026-10-03T10:00:01.000Z","type":"event_msg","payload":{"type":"user_message","message":"make it faster","images":[]}}"#;
        let (prompt, said) = codex::rollout(event, &meta).expect("an event");
        assert_eq!((prompt.text.as_str(), said), ("make it faster", codex::Said::Event));
        assert_eq!(prompt.session, id);
        assert_eq!(prompt.at_ms, Some(WallMs::from_millis(1_791_021_601_000)));
        let item = r#"{"timestamp":"2026-10-03T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"make it faster"}]}}"#;
        let (_, said) = codex::rollout(item, &meta).expect("an item");
        assert_eq!(said, codex::Said::Item);
        for added in [
            "<environment_context>\n<cwd>/w</cwd>\n</environment_context>",
            "# AGENTS.md instructions for /w\n\nbe nice",
            "<user_instructions>x</user_instructions>",
        ] {
            let item = serde_json::json!({
                "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": added}]},
            });
            assert_eq!(codex::rollout(&item.to_string(), &meta), None, "{added}");
        }
        let model = r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"user"}]}}"#;
        assert_eq!(codex::rollout(model, &meta), None);
    }

    /// pi's first line names the session and folder; its person's messages are prompts, its
    /// model's and tools' are not.
    #[test]
    fn pi_gives_prompts_from_its_session_files() {
        let head = r#"{"type":"session","version":3,"id":"01a0","timestamp":"2026-09-06T03:13:41.286Z","cwd":"/w/site"}"#;
        assert_eq!(pi::header(head), Some(("01a0".to_owned(), "/w/site".to_owned())));
        let user = r#"{"type":"message","id":"1","parentId":null,"timestamp":"2026-09-06T03:13:42.000Z","message":{"role":"user","content":[{"type":"text","text":"tidy the css"}],"timestamp":1788664422000}}"#;
        let prompt = pi::prompt(user, "01a0", "/w/site").expect("a prompt");
        assert_eq!(prompt.text, "tidy the css");
        assert_eq!(prompt.cwd.as_deref(), Some("/w/site"));
        assert_eq!(prompt.at_ms, Some(WallMs::from_millis(1_788_664_422_000)));
        let tool = r#"{"type":"message","id":"2","message":{"role":"toolResult","toolCallId":"t","toolName":"bash","content":[{"type":"text","text":"\"user\""}],"isError":false,"timestamp":1}}"#;
        assert_eq!(pi::prompt(tool, "01a0", "/w/site"), None);
        assert_eq!(pi::prompt(r#"{"type":"message","message":{"role":"user""#, "s", "/w"), None);
    }
}
