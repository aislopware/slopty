//! Bringing a Claude Code conversation back after a reboot, with `claude --resume <id>`.
//!
//! The worker keeps, for each terminal whose agent it follows, the conversation that agent
//! holds, the directory it runs in (Claude Code looks the conversation up under it) and the
//! flags it was started with that shape the session: the model, the permission mode, the tools
//! and directories it may use. Nothing else of the command line is kept. The prompt was sent
//! already, and `--settings`, `--mcp-config`, `--agents` and the system prompts can carry
//! tokens, so none of them is ever written down. Nor is `--worktree`: Claude Code records the
//! worktree a session runs in and enters it again on a resume, where the flag would make another.
//!
//! What Slopty itself put on the command line is noted instead, to be given afresh: its hook
//! relay, its tools, and the lock on the mode that asks no permission. One system prompt is
//! kept, the one appended to an agent started with Slopty's tools: that is the role the server
//! wrote for a project's agent, and a resumed one without it would not know its task.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::HookEvent;

/// A conversation to resume.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Resume {
    /// Claude Code's session id, what `--resume` takes.
    pub session: String,
    /// The directory the agent runs in.
    pub cwd: String,
    /// The conversation's transcript, when a hook or the lookup named it.
    pub transcript: Option<String>,
    /// The flags kept from its command line, the permission mode it was last in folded in.
    pub args: Vec<String>,
    /// It was started with Slopty's hook relay on its `--settings`, which is not kept: the
    /// resumed one is given the relay afresh.
    pub relay: bool,
    /// It was started with Slopty's tools on its `--mcp-config`, given afresh.
    pub mcp: bool,
    /// Its `--settings` locked it out of the mode that asks no permission, locked afresh.
    pub locked: bool,
    /// The system prompt appended to it, kept when it was started with Slopty's tools (the
    /// server's role for a project's agent). One that carries the pointer to Slopty's CLI keeps
    /// the pointer alone ([`crate::hooks::POINTER`]): a prompt the person appended is never kept.
    pub role: Option<String>,
}

impl Resume {
    /// Where its transcript is: the file a hook named, else where Claude Code writes one for
    /// this directory and id.
    #[must_use]
    pub fn transcript(&self, home: &Path) -> PathBuf {
        self.transcript.as_ref().map_or_else(
            || {
                crate::discover::project_dir(home, Path::new(&self.cwd))
                    .join(format!("{}.jsonl", self.session))
            },
            PathBuf::from,
        )
    }

    /// The arguments that resume it: `--resume <id>` and the flags kept.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        [RESUME_FLAG.to_owned(), self.session.clone()]
            .into_iter()
            .chain(self.args.clone())
            .collect()
    }
}

/// What a terminal says about an agent to bring back after a reboot.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Resumable {
    /// None runs there, or the one that did was ended by the person.
    No,
    /// One runs, but which conversation it holds is not known yet: what was kept stands.
    Unknown,
    /// This conversation.
    Yes(Resume),
}

/// What of a command line a resume keeps.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Invocation {
    /// The flags kept, in their order.
    pub args: Vec<String>,
    /// Its `--settings` registered Slopty's relay.
    pub relay: bool,
    /// Its `--mcp-config` served Slopty's tools.
    pub mcp: bool,
    /// Its `--settings` locked out the mode that asks no permission.
    pub locked: bool,
    /// Its appended system prompt, when [`Resume::role`] keeps it.
    pub role: Option<String>,
    /// It is a `--print` run, not a conversation in the terminal.
    pub print: bool,
}

/// The flag a system prompt is appended with.
const APPEND_SYSTEM_PROMPT: &str = "--append-system-prompt";

/// Flags kept that take no value.
const SWITCHES: [&str; 7] = [
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--chrome",
    "--no-chrome",
    "--ide",
    "--verbose",
    "--brief",
];

/// Flags kept with the one value they take.
const VALUED: [&str; 10] = [
    "--model",
    "--permission-mode",
    "--agent",
    "--effort",
    "--fallback-model",
    "--name",
    "-n",
    "--setting-sources",
    "--autocompact",
    "--plugin-dir",
];

/// Flags kept with the list of values that follow them.
const LISTS: [&str; 6] = [
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--tools",
];

/// The flag that starts the session without asking for any permission.
const SKIP_PERMISSIONS: &str = "--dangerously-skip-permissions";
/// The flag that only lets the person switch to that mode.
const ALLOW_SKIP_PERMISSIONS: &str = "--allow-dangerously-skip-permissions";
/// The flag the permission mode is given with.
const PERMISSION_MODE: &str = "--permission-mode";
/// The modes `--permission-mode` takes (`claude --help`, 2.1.283). A hook's `default` is
/// none of them: a session in it is started without the flag.
const PERMISSION_MODES: [&str; 6] =
    ["acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan"];

/// What of `args` (Claude Code's own arguments, [`crate::detect::agent_args`]) a resume keeps.
///
/// Anything not listed above is left out with whatever it takes, the prompt among them. The
/// plugin directory of Slopty's own mod is left out too: the shell's `claude` function or the
/// worker adds the one installed now. So is a value that is not [`typeable`]: the resume may be
/// typed at a prompt, where it would drive the line editor.
pub fn invocation(args: &[String]) -> Invocation {
    let mut out = Invocation::default();
    let mut role = None;
    let mut words = args.iter().peekable();
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value)),
            _ => (word.as_str(), None),
        };
        if flag == "-p" || flag == "--print" {
            out.print = true;
        } else if flag == "--settings" {
            let value = inline.or_else(|| words.next().map(String::as_str));
            let doc = value.and_then(|v| serde_json::from_str::<Value>(v).ok());
            out.relay |=
                doc.as_ref().is_some_and(|d| crate::hooks::has_relay(d, HookEvent::SessionStart));
            out.locked |= doc.as_ref().is_some_and(crate::hooks::locks_bypass);
        } else if flag == crate::hooks::MCP_CONFIG_FLAG {
            let mut configs: Vec<&str> = inline.into_iter().collect();
            if inline.is_none() {
                while let Some(value) = words.next_if(|w| !w.starts_with('-')) {
                    configs.push(value);
                }
            }
            out.mcp |= configs.iter().any(|c| serves_slopty(c));
        } else if flag == APPEND_SYSTEM_PROMPT {
            role = inline.map(str::to_owned).or_else(|| words.next().cloned());
        } else if SWITCHES.contains(&flag) {
            if inline.is_none() {
                out.args.push(word.clone());
            }
        } else if VALUED.contains(&flag) {
            let Some(value) = inline.or_else(|| words.next().map(String::as_str)) else {
                continue;
            };
            if (flag == crate::claude_mod::PLUGIN_DIR_FLAG && is_mod_dir(value)) || !typeable(value)
            {
                continue;
            }
            out.args.extend([flag.to_owned(), value.to_owned()]);
        } else if LISTS.contains(&flag) {
            let mut values: Vec<String> = inline.map(str::to_owned).into_iter().collect();
            if inline.is_none() {
                while let Some(value) = words.next_if(|w| !w.starts_with('-')) {
                    values.push(value.clone());
                }
            }
            values.retain(|value| typeable(value));
            if !values.is_empty() {
                out.args.push(flag.to_owned());
                out.args.extend(values);
            }
        }
    }
    out.role = if out.mcp {
        role
    } else {
        role.filter(|role| role.contains(crate::hooks::POINTER))
            .map(|_| crate::hooks::POINTER.to_owned())
    };
    out
}

/// Whether an `--mcp-config` document serves Slopty's tools (`hooks::mcp_config`).
fn serves_slopty(value: &str) -> bool {
    serde_json::from_str::<Value>(value).is_ok_and(|doc| {
        let args = doc
            .get("mcpServers")
            .and_then(|servers| servers.get(crate::hooks::MCP_SERVER_NAME))
            .and_then(|server| server.get("args"))
            .and_then(Value::as_array);
        args.is_some_and(|a| matches!(a.as_slice(), [m] if m == "mcp"))
    })
}

/// `args` with the permission mode the agent was last in (`mode`, from its hooks) in place of
/// the one it was started with. A session started skipping every permission and switched out
/// of that mode keeps the right to switch back, not the mode.
pub(crate) fn with_mode(args: Vec<String>, mode: Option<&str>) -> Vec<String> {
    let Some(mode) = mode.filter(|m| *m == "default" || PERMISSION_MODES.contains(m)) else {
        return args;
    };
    let mut out = Vec::with_capacity(args.len().saturating_add(2));
    let mut skipped = false;
    let mut words = args.into_iter();
    while let Some(word) = words.next() {
        if word == PERMISSION_MODE {
            let _mode = words.next();
        } else if word == SKIP_PERMISSIONS {
            skipped = true;
        } else {
            out.push(word);
        }
    }
    if skipped && !out.iter().any(|w| w == ALLOW_SKIP_PERMISSIONS) {
        out.push(ALLOW_SKIP_PERMISSIONS.to_owned());
    }
    if mode != "default" {
        out.extend([PERMISSION_MODE.to_owned(), mode.to_owned()]);
    }
    out
}

/// The flag that starts a conversation under an id the caller chose.
pub const SESSION_ID_FLAG: &str = "--session-id";

/// Flags that pick the conversation themselves: an id of their own, or one to resume. Claude
/// Code refuses `--session-id` beside a resume unless it forks.
const PICKS_CONVERSATION: [&str; 6] =
    [SESSION_ID_FLAG, RESUME_FLAG, "-r", "--continue", "-c", "--from-pr"];

/// `args` starting a conversation whose id is known before its first hook, and that id.
///
/// A fresh id is pinned with `--session-id`, unless `args` already pick the conversation:
/// then they are returned as they are, with no id.
///
/// Only the id is known this early. Claude Code writes the transcript when the first prompt
/// is sent, so until then there is nothing under the id to read or `--resume`, and
/// [`invocation`] never keeps the flag: a resume names the conversation with `--resume`.
#[must_use]
pub fn with_session_id(args: Vec<String>) -> (Vec<String>, Option<String>) {
    let picked = args.iter().take_while(|word| *word != "--").any(|word| {
        let flag = word.split_once('=').map_or(word.as_str(), |(flag, _value)| flag);
        PICKS_CONVERSATION.contains(&flag)
    });
    if picked {
        return (args, None);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let pinned = [SESSION_ID_FLAG.to_owned(), id.clone()].into_iter().chain(args).collect();
    (pinned, Some(id))
}

/// The arguments that start Claude Code on a conversation of a fresh id, and that id.
///
/// The id is chosen here, so the thread is named before the first hook. They are as Claude
/// Code's CLI takes them: `claude --session-id <uuid> [--model=<model>] [-- <prompt>]`, `model`
/// when one is named and `prompt` as the first message. The prompt follows `--`, so words of it
/// that look like flags are still the person's words, and the model is joined to its flag for
/// the same reason.
#[must_use]
pub fn started(model: Option<&str>, prompt: Option<&str>) -> (Vec<String>, String) {
    let session = uuid::Uuid::new_v4().to_string();
    let mut args = vec![SESSION_ID_FLAG.to_owned(), session.clone()];
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push(format!("--model={model}"));
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.extend(["--".to_owned(), prompt.to_owned()]);
    }
    (args, session)
}

/// The flag that takes a conversation up again by its id.
pub const RESUME_FLAG: &str = "--resume";

/// The arguments that take Claude Code's conversation `session` up again from a client's start.
///
/// They read `claude --resume <id> [--model=<model>] [-- <prompt>]`, as [`started`] spells the
/// rest; `None` when `session` cannot be a session id.
///
/// Claude Code goes on under the same id unless asked to fork, so the thread it was is the
/// one it comes back as. Only the model is given back: the flags the person started it with are
/// theirs to give again, and what Slopty adds to any `claude` it opens is added afresh.
#[must_use]
pub fn resumed(session: &str, model: Option<&str>, prompt: Option<&str>) -> Option<Vec<String>> {
    if !is_session_id(session) {
        return None;
    }
    let mut args = vec![RESUME_FLAG.to_owned(), session.to_owned()];
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push(format!("--model={model}"));
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.extend(["--".to_owned(), prompt.to_owned()]);
    }
    Some(args)
}

/// The flag that has a resume go on in a new conversation, the one it resumes left as it was.
pub const FORK_SESSION_FLAG: &str = "--fork-session";

/// The arguments that branch a new conversation off Claude Code's conversation `session`, and
/// the new one's id.
///
/// They read `claude --resume <id> --fork-session --session-id <uuid>`. The new id is
/// chosen here, as [`started`] chooses one, so the thread is named before the first hook; `None`
/// when `session` cannot be a session id.
#[must_use]
pub fn forked(session: &str) -> Option<(Vec<String>, String)> {
    if !is_session_id(session) {
        return None;
    }
    let new = uuid::Uuid::new_v4().to_string();
    let args = vec![
        RESUME_FLAG.to_owned(),
        session.to_owned(),
        FORK_SESSION_FLAG.to_owned(),
        SESSION_ID_FLAG.to_owned(),
        new.clone(),
    ];
    Some((args, new))
}

/// Whether `id` can be a Claude Code session id: it is typed into a shell and names a file.
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Whether a `SessionEnd` with this `reason` was the person ending the conversation (`/exit`,
/// ⌃D, `/logout`). `clear` and `resume` go on in a new conversation; `other` is also what a
/// signal gives, as in a reboot, which is exactly when the conversation should come back.
pub(crate) fn ended_by_the_person(reason: Option<&str>) -> bool {
    matches!(reason, Some("prompt_input_exit" | "logout"))
}

/// Whether `word` can be typed at a prompt as it is.
///
/// A control character cannot: the line editor acts on it however the word is quoted (a `\r`
/// runs the line early, a `\x03` cancels it, an escape starts a binding).
pub fn typeable(word: &str) -> bool {
    !word.chars().any(char::is_control)
}

/// Whether a `--settings` value is JSON that registers Slopty's relay. A file is not read: an
/// agent started with one reports through whatever it says, and so does the resumed one.
/// Whether a plugin directory is Slopty's mod (`<data dir>/claude-mod/<digest>`).
fn is_mod_dir(dir: &str) -> bool {
    Path::new(dir).parent().and_then(Path::file_name).is_some_and(|name| name == "claude-mod")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// A start names its conversation, its model joined to the flag, and the person's first
    /// message after `--`, so words that look like flags stay words; an empty one is no message.
    #[test]
    fn a_start_names_its_conversation_and_its_first_message() {
        let (args, id) = started(None, None);
        assert_eq!(args, words(&format!("--session-id {id}")));
        assert_eq!(uuid::Uuid::parse_str(&id).map(|u| u.get_version_num()), Ok(4));
        let (args, id) = started(Some("opus"), Some("--help me"));
        assert_eq!(args, [SESSION_ID_FLAG, &id, "--model=opus", "--", "--help me"]);
        let (args, id) = started(Some(" "), Some("  "));
        assert_eq!(args, words(&format!("--session-id {id}")));
        let (args, _) = started(Some("opus"), Some("go"));
        let (pinned, none) = with_session_id(args);
        assert!(none.is_none(), "the conversation is picked already: {pinned:?}");
        assert_eq!(invocation(&pinned).args, ["--model", "opus"], "a resume keeps the model alone");
    }

    /// The flags that shape the session are kept in their order, each with what it takes; the
    /// prompt, the flags that pick another conversation and every flag that may carry a secret
    /// are left out with their values, and a `--print` run is marked.
    #[test]
    fn only_the_flags_that_shape_the_session_are_kept() {
        let args = words(
            "--model opus --settings /tmp/s.json --add-dir ../a ../b --append-system-prompt \
             hush-hush --mcp-config /tmp/m.json -c --effort=high --session-id x --plugin-dir \
             /d/claude-mod/0123 --plugin-dir=/mine --verbose --debug-file=/tmp/d fix-it",
        );
        let kept = invocation(&args);
        assert_eq!(
            kept.args,
            words("--model opus --add-dir ../a ../b --effort high --plugin-dir /mine --verbose")
        );
        assert!(!kept.relay && !kept.print);
        assert!(invocation(&words("-p hello")).print);
        assert_eq!(
            invocation(&words("--model opus -- --model sonnet")).args,
            words("--model opus")
        );
    }

    /// A session started in a worktree of its own is resumed without `--worktree`, which would
    /// make another: Claude Code records the worktree in the session and enters it again on a
    /// resume. Its `--tmux` goes with it, whatever the spelling.
    #[test]
    fn a_worktree_is_entered_again_by_claude_code_not_made_again() {
        for line in [
            "--worktree --model opus",
            "--worktree fix-login --model opus",
            "-w fix-login --tmux --model opus",
            "--worktree=fix-login --tmux=classic --model opus",
        ] {
            assert_eq!(invocation(&words(line)).args, words("--model opus"), "{line}");
        }
    }

    /// A kept flag whose value holds a control character is left out with it, a list keeping
    /// its other values: typed at a prompt, a `\r` would run the line early, a `\x03` cancel
    /// it and an escape start a key binding, whatever the quoting.
    #[test]
    fn a_value_with_a_control_character_is_not_kept() {
        let args: Vec<String> = [
            "--name",
            "fix\r rm -rf ~",
            "--model",
            "opus",
            "--add-dir",
            "../a",
            "../b\u{1b}[A",
            "../c",
            "--plugin-dir=/p\u{3}",
            "--add-dir",
            "\n",
            "--effort",
            "high",
        ]
        .map(str::to_owned)
        .into();
        assert_eq!(invocation(&args).args, words("--model opus --add-dir ../a ../c --effort high"));
        assert!(typeable("opus 5 'quoted' ~/dir"));
        assert!(!typeable("a\tb"));
    }

    /// Slopty's relay on an inline `--settings` is noted so the resumed agent gets it again;
    /// the settings themselves are never kept.
    #[test]
    fn a_relay_on_the_settings_is_noted_not_kept() {
        let mut doc = serde_json::json!({"env": {"TOKEN": "hush-hush"}});
        crate::hooks::install(&mut doc, "/Applications/Slopty.app/Contents/MacOS/slopty");
        let args = vec!["--settings".to_owned(), doc.to_string(), "--model".to_owned(), "x".into()];
        let kept = invocation(&args);
        assert!(kept.relay);
        assert_eq!(kept.args, words("--model x"));
        assert!(!invocation(&words("--settings={}")).relay);
    }

    /// What Slopty put on a command line is noted, never kept: its tools, the lock on the mode
    /// that asks nothing. The role appended to an agent with Slopty's tools is kept; a system
    /// prompt on any other is not, as it may carry anything, and one joined with the pointer to
    /// Slopty's CLI keeps the pointer alone.
    #[test]
    fn slopty_s_own_wiring_is_noted_and_its_role_kept() {
        let dir = Path::new("/nowhere");
        let none = crate::managed::ManagedSettings::default();
        let ours = crate::hooks::with_mcp_under(words("--model x"), "/bin/slopty", &none);
        let ours = crate::hooks::without_bypass(ours, dir);
        let mut args = ours;
        args.push("--append-system-prompt=You work on task 3.\nReport with task_report.".into());
        let kept = invocation(&args);
        assert!(kept.mcp && kept.locked && !kept.relay, "{kept:?}");
        assert_eq!(kept.role.as_deref(), Some("You work on task 3.\nReport with task_report."));
        assert_eq!(kept.args, words("--model x"), "no document is kept");

        let theirs = words("--mcp-config {\"mcpServers\":{\"db\":{\"command\":\"pg\"}}}");
        let mut theirs = theirs;
        theirs.extend(["--append-system-prompt".to_owned(), "token=hush".to_owned()]);
        let kept = invocation(&theirs);
        assert!(!kept.mcp && !kept.locked && kept.role.is_none(), "{kept:?}");

        let pointed = format!("token=hush\n\n{}", crate::hooks::POINTER);
        let kept = invocation(&["--append-system-prompt".to_owned(), pointed]);
        assert_eq!(kept.role.as_deref(), Some(crate::hooks::POINTER), "the person's words go");
    }

    /// The mode the hooks last reported replaces the one the agent was started with; `default`
    /// drops the flag, and leaving the skip-everything mode keeps only the right to go back.
    #[test]
    fn the_last_permission_mode_wins() {
        let started = words("--model x --permission-mode plan --dangerously-skip-permissions");
        assert_eq!(
            with_mode(started.clone(), Some("acceptEdits")),
            words("--model x --allow-dangerously-skip-permissions --permission-mode acceptEdits")
        );
        assert_eq!(
            with_mode(started.clone(), Some("default")),
            words("--model x --allow-dangerously-skip-permissions")
        );
        assert_eq!(with_mode(started.clone(), None), started, "not reported");
        assert_eq!(with_mode(started.clone(), Some("yolo")), started, "not a mode");
    }

    /// A run is pinned to a fresh id unless its arguments pick the conversation already, and
    /// its resume names the conversation once, with `--resume`: the pinned flag is not kept.
    #[test]
    fn a_run_is_pinned_to_a_conversation_id_once() {
        let (pinned, id) = with_session_id(words("--model opus fix-it"));
        let id = id.expect("pinned");
        assert!(uuid::Uuid::parse_str(&id).is_ok(), "{id}");
        assert_eq!(pinned, words(&format!("--session-id {id} --model opus fix-it")));
        for picked in [
            "--session-id 4f1c7a52-9d0e-4b8a-a1a3-0c5f3e0b8d11",
            "--session-id=4f1c7a52-9d0e-4b8a-a1a3-0c5f3e0b8d11",
            "--resume abc",
            "-r abc",
            "--continue",
            "-c",
        ] {
            assert_eq!(with_session_id(words(picked)), (words(picked), None), "{picked}");
        }
        let (after, id) = with_session_id(words("-- -c"));
        assert!(id.is_some() && after.ends_with(&words("-- -c")), "a prompt after `--`");

        let kept = invocation(&pinned);
        assert_eq!(kept.args, words("--model opus"), "the pinned id is not kept");
        let resume = Resume {
            session: id.unwrap_or_default(),
            cwd: "/w".to_owned(),
            transcript: None,
            args: kept.args,
            relay: false,
            mcp: false,
            locked: false,
            role: None,
        };
        let args = resume.args();
        assert_eq!(args.iter().filter(|a| a.starts_with("--resume")).count(), 1);
        assert!(!args.iter().any(|a| a.starts_with(SESSION_ID_FLAG)), "{args:?}");
    }

    /// Only a person's own exit ends what comes back; a new conversation or a signal does not.
    #[test]
    fn only_the_person_ends_a_conversation_for_good() {
        assert!(ended_by_the_person(Some("prompt_input_exit")));
        assert!(ended_by_the_person(Some("logout")));
        for reason in [Some("other"), Some("clear"), Some("resume"), None] {
            assert!(!ended_by_the_person(reason), "{reason:?}");
        }
        assert!(is_session_id("4f1c7a52-9d0e-4b8a-a1a3-0c5f3e0b8d11"));
        assert!(!is_session_id("x; rm -rf ~"));
        assert!(!is_session_id(""));
    }

    /// Without a named transcript, the file is where Claude Code writes one for the directory.
    #[test]
    fn the_transcript_is_named_or_found_under_the_project() {
        let resume = Resume {
            session: "abc".to_owned(),
            cwd: "/nowhere/w".to_owned(),
            transcript: None,
            args: words("--model x"),
            relay: false,
            mcp: false,
            locked: false,
            role: None,
        };
        assert_eq!(
            resume.transcript(Path::new("/h")),
            Path::new("/h/.claude/projects/-nowhere-w/abc.jsonl")
        );
        let named = Resume { transcript: Some("/t/abc.jsonl".to_owned()), ..resume.clone() };
        assert_eq!(named.transcript(Path::new("/h")), Path::new("/t/abc.jsonl"));
        assert_eq!(resume.args(), words("--resume abc --model x"));
    }

    /// A fork resumes the conversation into a new one whose id is chosen here, and names no
    /// conversation that cannot be one.
    #[test]
    fn a_fork_resumes_into_a_new_conversation() {
        let (args, new) = forked("abc-123").expect("a session id");
        assert_eq!(args, ["--resume", "abc-123", "--fork-session", "--session-id", new.as_str()]);
        assert_ne!(new, "abc-123");
        assert!(is_session_id(&new));
        assert_eq!(forked("a b"), None);
        assert_eq!(forked(""), None);
    }
}
