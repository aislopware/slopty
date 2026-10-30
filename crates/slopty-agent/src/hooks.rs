//! Claude Code's hooks, as Slopty hears them (`docs/decisions/agents.md`, "TUI-first").
//!
//! A Claude Code Slopty starts gets one `--settings` that registers `slopty hook` for each of
//! [`EVENTS`] ([`with_relay`]), merged into whatever settings the caller passed: Claude Code
//! keeps only the last `--settings` it is given, so the caller's own is read, gets the relay, and
//! becomes the one left. The person's own settings files are never written. Every entry is
//! asynchronous, so Claude Code never waits on Slopty, and a permission dialog is only heard,
//! never answered: the TUI shows its own.
//!
//! The relay hands each payload to the worker, which reads the session's status from it
//! ([`Hook::signal`]). Slopty's own mod relays the TUI's prompt box the same way, under an event of
//! its own ([`DRAFT_EVENT`], `crate::claude::draft`). Unknown events and fields are passed over,
//! so a newer Claude Code's additions cost nothing.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The events the relay is registered for: what the status needs, and nothing else.
pub const EVENTS: [&str; 15] = [
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
];

/// How long Claude Code lets a relay run, in seconds: the relay itself gives up long before.
pub const TIMEOUT_S: u32 = 5;

/// The relay's subcommand: `<slopty> hook`.
pub const RELAY_ARG: &str = "hook";

/// The flag Claude Code takes settings for one run with.
pub const SETTINGS_FLAG: &str = "--settings";

/// The event Slopty's mod relays the TUI's prompt box under: none of Claude Code's own.
pub const DRAFT_EVENT: &str = "PromptDraft";

/// `claude` arguments that also relay [`EVENTS`] to `<relay> hook`, for that run alone.
///
/// The caller's own `--settings` (JSON, or a file relative to `cwd`) is read and gets the relay,
/// and becomes the one `--settings`, first; one that cannot be read leaves `args` as they are,
/// for Claude Code to report. A relay already registered is not registered twice.
#[must_use]
pub fn with_relay(args: Vec<String>, relay: &str, cwd: &Path) -> Vec<String> {
    let mut rest = Vec::with_capacity(args.len());
    let mut given = None;
    let mut words = args.iter().cloned();
    while let Some(word) = words.next() {
        if word == "--" {
            rest.push(word);
            rest.extend(words.by_ref());
        } else if word == SETTINGS_FLAG
            && let Some(value) = words.next()
        {
            given = Some(value);
        } else if let Some(value) = word.strip_prefix("--settings=") {
            given = Some(value.to_owned());
        } else {
            rest.push(word);
        }
    }
    let doc = match given {
        None => Some(json!({})),
        Some(value) => settings_value(&value, cwd),
    };
    let Some(mut doc) = doc else { return args };
    install(&mut doc, relay);
    [SETTINGS_FLAG.to_owned(), doc.to_string()].into_iter().chain(rest).collect()
}

/// A `--settings` value as Claude Code reads it: JSON when it opens an object, else a file.
fn settings_value(value: &str, cwd: &Path) -> Option<Value> {
    let text = if value.trim_start().starts_with('{') {
        value.to_owned()
    } else {
        std::fs::read_to_string(cwd.join(value)).ok()?
    };
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// The relay's entry, as Claude Code's settings spell a command hook.
fn entry(relay: &str) -> Value {
    json!({
        "type": "command",
        "command": relay,
        "args": [RELAY_ARG],
        "async": true,
        "timeout": TIMEOUT_S,
    })
}

/// Whether a hook entry is a relay: any `slopty` binary with `args: ["hook"]`.
fn is_relay(entry: &Value) -> bool {
    let command = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let args = entry.get("args").and_then(Value::as_array).map(Vec::as_slice);
    matches!(args, Some([word]) if word == RELAY_ARG)
        && Path::new(command).file_name().is_some_and(|n| n == "slopty")
}

/// Register the relay at `relay` for every event in `doc`, in place of any relay there.
fn install(doc: &mut Value, relay: &str) {
    let Some(root) = doc.as_object_mut() else { return };
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let Some(hooks) = hooks.as_object_mut() else { return };
    for event in EVENTS {
        let groups = hooks.entry(event).or_insert_with(|| Value::Array(Vec::new()));
        if !groups.is_array() {
            *groups = Value::Array(Vec::new());
        }
        let Some(groups) = groups.as_array_mut() else { continue };
        for group in groups.iter_mut() {
            if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                entries.retain(|e| !is_relay(e));
            }
        }
        groups.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|e| !e.is_empty()));
        groups.push(json!({ "hooks": [entry(relay)] }));
    }
}

/// A hook's payload, the fields Slopty reads of it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct Hook {
    /// The event (`hook_event_name`), as Claude Code names it.
    #[serde(default, rename = "hook_event_name")]
    pub event: String,
    /// Claude Code's session id.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The session's main transcript.
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// Where the agent works.
    #[serde(default)]
    pub cwd: Option<String>,
    /// The tool, on tool events.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// The tool's input, on tool events.
    #[serde(default)]
    pub tool_input: Option<Value>,
    /// What a `Notification` is about: `permission_prompt`, `idle_prompt`,
    /// `elicitation_dialog`, …
    #[serde(default)]
    pub notification_type: Option<String>,
    /// A `Notification`'s words.
    #[serde(default)]
    pub message: Option<String>,
    /// How a session began (`startup`, `resume`, `clear`, `compact`).
    #[serde(default)]
    pub source: Option<String>,
    /// Why a session ended (`clear`, `resume`, `logout`, `prompt_input_exit`, `other`).
    #[serde(default)]
    pub reason: Option<String>,
    /// What a `StopFailure` failed on.
    #[serde(default)]
    pub error: Option<String>,
    /// The prompt whose turn the hook is of: every hook from a prompt up to the next names it,
    /// as the transcript's records of the turn do (`promptId`), so a `Stop` says which turn it
    /// ended ([`crate::claude::history::History::turn_ended`]). None before the first prompt.
    #[serde(default)]
    pub prompt_id: Option<String>,
    /// The prompt: the one sent, on `UserPromptSubmit`; what the box holds, on a
    /// [`DRAFT_EVENT`].
    #[serde(default)]
    pub prompt: Option<String>,
    /// The temporary root Claude Code keeps its files under (its `CLAUDE_CODE_TMPDIR`), on a
    /// [`DRAFT_EVENT`] whose TUI has one set.
    #[serde(default)]
    pub tmpdir: Option<String>,
}

/// What a hook says of its session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// The session began (`SessionStart`): at rest, waiting for a prompt.
    Began,
    /// It works on a turn.
    Working,
    /// It waits on the person at a dialog: a permission, a question, an MCP server's form.
    Asks {
        /// What kind (`slopty_proto::thread::Wait`'s kinds: `permission`, `question`, `plan`,
        /// `input`).
        kind: &'static str,
        /// What it asks, in a line.
        text: String,
    },
    /// The turn ended and it is at rest.
    Done,
    /// The turn ended on an error.
    Failed(Option<String>),
    /// Its context is being compacted (`true`), or that is over.
    Compacting(bool),
    /// The session ended (`/clear`, `/resume`, `/exit`, a logout).
    Ended,
    /// The TUI's prompt box holds `text` now ([`DRAFT_EVENT`]): nothing for the status. Its pasted
    /// pictures are kept under the temporary root `tmpdir`, Claude Code's own default when
    /// `None`.
    Draft {
        /// What the box holds.
        text: String,
        /// The temporary root, when the TUI moved it.
        tmpdir: Option<String>,
    },
    /// Nothing for the status (a subagent's start or stop: the transcript tells those).
    Nothing,
}

impl Hook {
    /// The payload `json`, or `None` when it is not one.
    #[must_use]
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// What it says of the session's status.
    #[must_use]
    pub fn signal(&self) -> Signal {
        let tool = self.tool_name.as_deref().unwrap_or_default();
        let input = self.tool_input.clone().unwrap_or(Value::Null);
        let asks =
            |kind| Signal::Asks { kind, text: crate::claude::history::asks_for(tool, &input) };
        match self.event.as_str() {
            "SessionStart" => Signal::Began,
            "SessionEnd" => Signal::Ended,
            "UserPromptSubmit" | "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                Signal::Working
            }
            "PreToolUse" | "PermissionRequest" if tool == QUESTION_TOOL => asks(QUESTION),
            "PreToolUse" => Signal::Working,
            "PermissionRequest" if tool == PLAN_TOOL => asks(PLAN),
            "PermissionRequest" => asks(PERMISSION),
            "Notification" => self.notified(),
            "Stop" => Signal::Done,
            "StopFailure" => Signal::Failed(self.error.clone().filter(|e| !e.trim().is_empty())),
            DRAFT_EVENT => Signal::Draft {
                text: self.prompt.clone().unwrap_or_default(),
                tmpdir: self.tmpdir.clone().filter(|t| !t.trim().is_empty()),
            },
            "PreCompact" => Signal::Compacting(true),
            "PostCompact" => Signal::Compacting(false),
            _ => Signal::Nothing,
        }
    }

    /// What a `Notification` says: a dialog is up, or the TUI waits for the next prompt.
    fn notified(&self) -> Signal {
        let line = self.message.as_deref().map(first_line).unwrap_or_default();
        match self.notification_type.as_deref() {
            Some("permission_prompt") => Signal::Asks { kind: PERMISSION, text: line },
            Some("agent_needs_input") => Signal::Asks { kind: QUESTION, text: line },
            Some("elicitation_dialog" | "elicitation_url_dialog") => {
                Signal::Asks { kind: INPUT, text: line }
            }
            Some("elicitation_complete" | "elicitation_response") => Signal::Working,
            Some("idle_prompt") => Signal::Done,
            _ => Signal::Nothing,
        }
    }
}

/// Claude Code's tool that asks the person a question in the TUI.
const QUESTION_TOOL: &str = "AskUserQuestion";

/// Claude Code's tool that asks the person to approve a plan.
const PLAN_TOOL: &str = "ExitPlanMode";

/// The wait kinds a dialog is said by (`slopty_proto::thread::Wait`).
pub(crate) const PERMISSION: &str = "permission";
pub(crate) const QUESTION: &str = "question";
const PLAN: &str = "plan";
pub(crate) const INPUT: &str = "input";

/// The first line of `text`, trimmed.
fn first_line(text: &str) -> String {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|&a| a.to_owned()).collect()
    }

    /// A run gets one `--settings` that relays every event asynchronously, merged into the
    /// caller's own, which keeps its other hooks and settings; the prompt after `--` stays a
    /// prompt, and a second wiring registers the relay once.
    #[test]
    fn the_relay_rides_one_settings_merged_with_the_callers() {
        let theirs = r#"{"model":"opus","hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#;
        let args = with_relay(
            words(&["--settings", theirs, "--model=sonnet", "--", "--fix it"]),
            "/opt/Slopty/slopty",
            Path::new("/"),
        );
        assert_eq!(args.iter().filter(|a| *a == SETTINGS_FLAG).count(), 1, "{args:?}");
        assert_eq!(&args[2..], words(&["--model=sonnet", "--", "--fix it"]));
        let doc: Value = serde_json::from_str(&args[1]).expect("json");
        assert_eq!(doc["model"], "opus", "the caller's own settings stay");
        for event in EVENTS {
            let groups = doc["hooks"][event].as_array().expect(event);
            let relays: Vec<&Value> = groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().unwrap())
                .filter(|e| is_relay(e))
                .collect();
            assert_eq!(relays.len(), 1, "{event}");
            assert_eq!(relays[0]["async"], true, "{event} never holds Claude Code");
            assert_eq!(relays[0]["args"], json!(["hook"]));
        }
        let stop = doc["hooks"]["Stop"].as_array().unwrap();
        assert!(stop.iter().any(|g| g["hooks"][0]["command"] == "say done"), "theirs stays");

        let again = with_relay(args, "/opt/Slopty/slopty", Path::new("/"));
        let doc: Value = serde_json::from_str(&again[1]).expect("json");
        let count = doc["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["hooks"].as_array().unwrap())
            .filter(|e| is_relay(e))
            .count();
        assert_eq!(count, 1, "wired once");
        assert_eq!(
            with_relay(words(&["--settings", "missing.json"]), "/s/slopty", Path::new("/nowhere")),
            words(&["--settings", "missing.json"]),
            "settings it cannot read are left for Claude Code to report"
        );
    }

    /// Each hook says what the status needs: a prompt and a tool at work, a dialog the person
    /// answers (with what it asks), the turn's end, its failure, compaction and the session's
    /// end; a subagent's events and an unknown one say nothing.
    #[test]
    fn hooks_say_the_status() {
        let hook = |json: Value| Hook::parse(&json.to_string()).expect("a payload").signal();
        assert_eq!(
            hook(json!({"hook_event_name": "SessionStart", "source": "clear"})),
            Signal::Began
        );
        assert_eq!(
            hook(json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi"})),
            Signal::Working
        );
        assert_eq!(
            hook(
                json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "ls"}})
            ),
            Signal::Working
        );
        assert_eq!(
            hook(
                json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "cargo test"}})
            ),
            Signal::Asks { kind: "permission", text: "Run cargo test".to_owned() }
        );
        let question = json!({"questions": [{"question": "Which one?"}]});
        assert_eq!(
            hook(
                json!({"hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "tool_input": question})
            ),
            Signal::Asks { kind: "question", text: "Which one?".to_owned() }
        );
        assert_eq!(
            hook(
                json!({"hook_event_name": "Notification", "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash\n"})
            ),
            Signal::Asks {
                kind: "permission",
                text: "Claude needs your permission to use Bash".to_owned()
            }
        );
        assert_eq!(
            hook(json!({"hook_event_name": "Notification", "notification_type": "idle_prompt"})),
            Signal::Done
        );
        assert_eq!(
            hook(json!({"hook_event_name": "PostToolUse", "tool_name": "Bash"})),
            Signal::Working
        );
        assert_eq!(hook(json!({"hook_event_name": "Stop"})), Signal::Done);
        assert_eq!(
            hook(json!({"hook_event_name": "StopFailure", "error": "rate_limit"})),
            Signal::Failed(Some("rate_limit".to_owned()))
        );
        assert_eq!(hook(json!({"hook_event_name": "PreCompact"})), Signal::Compacting(true));
        assert_eq!(
            hook(json!({"hook_event_name": "SessionEnd", "reason": "clear"})),
            Signal::Ended
        );
        assert_eq!(hook(json!({"hook_event_name": "SubagentStop"})), Signal::Nothing);
        assert_eq!(
            hook(
                json!({"hook_event_name": "PromptDraft", "session_id": "s", "prompt": "see [Image #2]", "tmpdir": "/Volumes/Fast/tmp"})
            ),
            Signal::Draft {
                text: "see [Image #2]".to_owned(),
                tmpdir: Some("/Volumes/Fast/tmp".to_owned())
            }
        );
        assert_eq!(
            hook(json!({"hook_event_name": "PromptDraft", "session_id": "s", "tmpdir": ""})),
            Signal::Draft { text: String::new(), tmpdir: None },
            "a box with nothing in it, under Claude Code's own temporary root"
        );
        assert_eq!(hook(json!({"hook_event_name": "SomethingNew", "x": 1})), Signal::Nothing);
        assert_eq!(Hook::parse("not json"), None);
    }
}
