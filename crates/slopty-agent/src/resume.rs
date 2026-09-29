//! Bringing a Claude Code conversation back after a reboot, with `claude --resume <id>`.
//!
//! The worker keeps, for each terminal whose agent it follows, the conversation that agent
//! holds, the directory it runs in (Claude Code looks the conversation up under it) and the
//! flags it was started with that shape the session: the model, the permission mode, the tools
//! and directories it may use. Nothing else of the command line is kept. The prompt was sent
//! already, and `--settings`, `--mcp-config`, `--agents` and the system prompts can carry
//! tokens, so none of them is ever written down.

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
        ["--resume".to_owned(), self.session.clone()].into_iter().chain(self.args.clone()).collect()
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
    /// It is a `--print` run, not a conversation in the terminal.
    pub print: bool,
}

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
            out.relay |= value.is_some_and(registers_relay);
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
    out
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

/// Whether `id` can be a Claude Code session id: it is typed into a shell and names a file.
pub(crate) fn is_session_id(id: &str) -> bool {
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
fn registers_relay(value: &str) -> bool {
    serde_json::from_str::<Value>(value)
        .is_ok_and(|doc| crate::hooks::has_relay(&doc, HookEvent::SessionStart))
}

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
        };
        assert_eq!(
            resume.transcript(Path::new("/h")),
            Path::new("/h/.claude/projects/-nowhere-w/abc.jsonl")
        );
        let named = Resume { transcript: Some("/t/abc.jsonl".to_owned()), ..resume.clone() };
        assert_eq!(named.transcript(Path::new("/h")), Path::new("/t/abc.jsonl"));
        assert_eq!(resume.args(), words("--resume abc --model x"));
    }
}
