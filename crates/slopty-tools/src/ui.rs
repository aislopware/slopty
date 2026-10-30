//! The person's Slopty window, driven by an agent: the `ui_*` tools, and the arguments they and
//! `slopty ui` share.
//!
//! An agent reads what the window shows and changes it with open verbs: a shell, a file, a page
//! or the changes in a pane beside the session, a region shown or hidden, a chat put on screen,
//! the session's panes at even shares. The ask goes to the window of the
//! person in front of the agent's session through its worker ([`crate::Dispatch::window`]),
//! never to a window on another person's machines. The window does it without taking the
//! keyboard, refuses what would move it away from a draft being typed, and marks each change
//! with a notice naming the agent (`docs/decisions/agents.md`, "Agents drive the person's window
//! through open verbs").
//!
//! Every answer is JSON: what the ask did (`did`), the pane it opened (`made`), and the window
//! after it (`window`: its `page`, the `chat` on screen, its `regions`, its `tabs`, and whether
//! the person is `typing`); or `{"error": {"code", "message"}}`, `code` one of `no_window`,
//! `unknown`, `busy`, `elsewhere`, `failed`.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};
use slopty_proto::handoff::{UiAct, UiChange, UiDone, UiError, UiFailure, UiTarget};

use crate::{Dispatch, ToolError};

/// The tools' names, in the order a model meets them.
pub(crate) const NAMES: [&str; 6] =
    ["ui_state", "ui_terminal", "ui_open", "ui_region", "ui_chat", "ui_split"];

pub(crate) const STATE_DESCRIPTION: &str = "\
What the person's Slopty window shows: the `page` (`chat` or `settings/...`), the `chat` on screen \
(`id`, `title`, `machine`), its `regions` (`sidebar`, `files`, `changes`, `inspector`, \
`terminal`; each `shown`, and `expanded` when its pane is zoomed over the session), its `tabs`, \
the panes beside the session (`id`, `kind`, `title`, `region`, `selected` when it has the \
keyboard, a shell's session `terminal`, a file's or page's `target`), and whether the person is \
`typing` a draft. Every ui_* tool answers with this as `window`.";

pub(crate) const TERMINAL_DESCRIPTION: &str = "\
Open a shell in a pane beside the chat on screen in the person's window, in `folder` (your \
working folder when omitted), and type `command` once it is up. A command that acts as root, \
destroys, or spans lines waits at the prompt for the person to read and run (`ran`: false). \
Returns the pane as `made`, its session id as `made.terminal`. The pane never takes the \
keyboard.";

pub(crate) const OPEN_DESCRIPTION: &str = "\
Show one thing in a pane beside the chat on screen: a `file` on this machine (relative to \
your working folder; `line` to start at, or `path:line`), a web page `url`, or the chat's \
uncommitted changes (`diff`: true). A page on this machine's localhost opens at the port that \
forwards it to the person. The chat on screen must run on this machine for a file; ui_chat puts \
yours there.";

pub(crate) const REGION_DESCRIPTION: &str = "\
Show, hide or toggle one region of the person's window (expand shows, collapse hides): \
`sidebar`, `files`, `changes` (the chat's uncommitted changes), `inspector` (the session's \
trace) or `terminal` (shown, a new shell beside the session; the person closes their own \
shells). A region the person's keyboard is in is not hidden.";

pub(crate) const CHAT_DESCRIPTION: &str = "\
Put a chat on screen in the person's window: `chat` by id, id prefix or title; your own chat \
when omitted. Refused while the person types a draft or is in Settings.";

pub(crate) const SPLIT_DESCRIPTION: &str = "\
Lay the session's panes at even shares, first showing a `file`, `url` or the `diff` beside it \
when given, as ui_open does.";

/// `ui_terminal`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalArgs {
    /// The folder it starts in, relative to your working folder; your working folder when
    /// omitted.
    folder: Option<String>,
    /// A command line to type once its shell is up.
    command: Option<String>,
}

/// `ui_open`, and what `ui_split` shows first.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenArgs {
    /// A file on this machine, relative to your working folder; `path:line` names a line.
    file: Option<String>,
    /// The line of `file` to start at, from 1.
    line: Option<u32>,
    /// A web page's `http` or `https` address.
    url: Option<String>,
    /// The chat's uncommitted changes.
    #[serde(default)]
    diff: bool,
}

/// `ui_region`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegionArgs {
    /// `sidebar`, `files`, `changes`, `inspector` or `terminal`.
    region: String,
    /// What becomes of it.
    change: ChangeArg,
}

/// What becomes of a region.
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChangeArg {
    /// On screen.
    Show,
    /// Off screen.
    Hide,
    /// The other of the two.
    Toggle,
    /// As large as it goes: shown.
    Expand,
    /// Back out of the way: hidden.
    Collapse,
}

/// `ui_chat`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChatArgs {
    /// The chat's id, a prefix of it, or its title; your own when omitted.
    chat: Option<String>,
}

impl From<ChangeArg> for UiChange {
    fn from(change: ChangeArg) -> Self {
        match change {
            ChangeArg::Show => Self::Show,
            ChangeArg::Hide => Self::Hide,
            ChangeArg::Toggle => Self::Toggle,
            ChangeArg::Expand => Self::Expand,
            ChangeArg::Collapse => Self::Collapse,
        }
    }
}

fn unknown(message: impl Into<String>) -> UiFailure {
    UiFailure::new(UiError::Unknown, message)
}

/// A web page an agent names: an `http` or `https` address with a host.
///
/// # Errors
///
/// [`UiError::Unknown`] for anything else.
pub fn page(url: &str) -> Result<UiTarget, UiFailure> {
    match slopty_proto::handoff::page(url) {
        Some(page) => Ok(UiTarget::Page { url: page.url }),
        None => Err(unknown(format!("{url} is not an http or https address with a host"))),
    }
}

/// A file an agent names, relative to `cwd`: one that is there, at `line` or at the line a
/// `path:line` ending names.
///
/// # Errors
///
/// [`UiError::Unknown`] for no file there, or a folder.
pub fn file(given: &str, line: Option<u32>, cwd: &Path) -> Result<UiTarget, UiFailure> {
    let at = |given: &str| cwd.join(given);
    let (path, line) = match given.rsplit_once(':') {
        Some((head, tail)) if !at(given).exists() && !head.is_empty() => {
            match tail.parse::<u32>() {
                Ok(named) => (at(head), line.or(Some(named))),
                Err(_) => (at(given), line),
            }
        }
        _ => (at(given), line),
    };
    let path = std::path::absolute(&path).unwrap_or(path);
    if path.is_dir() {
        return Err(unknown(format!("{} is a folder; a pane shows a file", path.display())));
    }
    if !path.is_file() {
        return Err(unknown(format!("no file at {}", path.display())));
    }
    Ok(UiTarget::File { path: path.to_string_lossy().into_owned(), line })
}

/// A file or a page, as one word names it: an address with a scheme is a page, anything else
/// a file relative to `cwd`.
///
/// # Errors
///
/// As [`page()`] and [`file()`].
pub fn target(given: &str, line: Option<u32>, cwd: &Path) -> Result<UiTarget, UiFailure> {
    if given.contains("://") { page(given) } else { file(given, line, cwd) }
}

/// The folder a shell starts in, relative to `cwd`; `cwd` itself when none is named.
///
/// # Errors
///
/// [`UiError::Unknown`] for no folder there.
pub fn folder(given: Option<&str>, cwd: &Path) -> Result<String, UiFailure> {
    let path = given.map_or_else(|| cwd.to_path_buf(), |given| cwd.join(given));
    let path = std::path::absolute(&path).unwrap_or(path);
    if !path.is_dir() {
        return Err(unknown(format!("no folder at {}", path.display())));
    }
    Ok(path.to_string_lossy().into_owned())
}

/// An ask's outcome as JSON: what it did, or `{"error": {"code", "message"}}`.
#[must_use]
pub fn json(outcome: &Result<UiDone, UiFailure>) -> Value {
    match outcome {
        Ok(done) => serde_json::to_value(done).unwrap_or(Value::Null),
        Err(failure) => json!({ "error": failure }),
    }
}

impl OpenArgs {
    /// The one thing it names; `None` when it names nothing.
    fn target(self, cwd: &Path) -> Result<Option<UiTarget>, UiFailure> {
        match (self.file, self.url, self.diff) {
            (None, None, false) => Ok(None),
            (Some(file), None, false) => self::file(&file, self.line, cwd).map(Some),
            (None, Some(url), false) => page(&url).map(Some),
            (None, None, true) => Ok(Some(UiTarget::Diff)),
            _ => Err(unknown("name one of `file`, `url` and `diff`")),
        }
    }
}

fn args<T: serde::de::DeserializeOwned>(
    arguments: serde_json::Map<String, Value>,
) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|e| ToolError::invalid(format!("bad arguments: {e}")))
}

/// The act a `ui_*` tool's arguments ask for, paths read against `cwd`.
fn act(
    name: &str,
    arguments: serde_json::Map<String, Value>,
    cwd: &Path,
) -> Result<Result<UiAct, UiFailure>, ToolError> {
    Ok(match name {
        "ui_state" => {
            if let Some(key) = arguments.keys().next() {
                return Err(ToolError::invalid(format!("bad arguments: unknown field `{key}`")));
            }
            Ok(UiAct::State)
        }
        "ui_terminal" => {
            let a: TerminalArgs = args(arguments)?;
            folder(a.folder.as_deref(), cwd)
                .map(|folder| UiAct::Terminal { folder: Some(folder), command: a.command })
        }
        "ui_open" => {
            let a: OpenArgs = args(arguments)?;
            a.target(cwd).and_then(|target| {
                target
                    .map(UiAct::Open)
                    .ok_or_else(|| unknown("name a `file`, a `url` or the `diff`"))
            })
        }
        "ui_region" => {
            let a: RegionArgs = args(arguments)?;
            Ok(UiAct::Region { region: a.region, change: a.change.into() })
        }
        "ui_chat" => Ok(UiAct::Chat { chat: args::<ChatArgs>(arguments)?.chat }),
        "ui_split" => args::<OpenArgs>(arguments)?.target(cwd).map(UiAct::Split),
        other => return Err(ToolError::invalid(format!("no tool is called {other}"))),
    })
}

/// Run the `ui_*` tool `name` on `dispatch`, paths read against `cwd`: what the window did, or
/// why it did nothing, each as its JSON.
///
/// # Errors
///
/// Arguments that do not fit the tool.
pub(crate) async fn call<D: Dispatch>(
    dispatch: &D,
    name: &str,
    arguments: serde_json::Map<String, Value>,
    cwd: &Path,
) -> Result<Result<Value, Value>, ToolError> {
    let outcome = match act(name, arguments, cwd)? {
        Ok(act) => dispatch.window(act).await,
        Err(failure) => Err(failure),
    };
    let value = json(&outcome);
    Ok(if outcome.is_ok() { Ok(value) } else { Err(value) })
}

#[cfg(test)]
mod tests {
    use slopty_proto::orchestration::ErrorCode;

    use super::*;

    fn object(value: Value) -> serde_json::Map<String, Value> {
        let Value::Object(map) = value else { panic!("an object") };
        map
    }

    /// Each tool's arguments become the act it names, paths absolute against the working
    /// folder; one naming no file there, two things at once, or nothing is answered as unknown;
    /// a field no tool has is a bad argument.
    #[test]
    fn each_tool_reads_its_arguments() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let cwd = &dir.path().canonicalize()?;
        std::fs::create_dir_all(cwd.join("src"))?;
        std::fs::write(cwd.join("src/main.rs"), "fn main() {}\n")?;
        let main = cwd.join("src/main.rs").to_string_lossy().into_owned();
        let read = |name: &str, value: Value| act(name, object(value), cwd);
        assert_eq!(read("ui_state", json!({})), Ok(Ok(UiAct::State)));
        assert_eq!(
            read("ui_terminal", json!({ "folder": "src", "command": "ls" })),
            Ok(Ok(UiAct::Terminal {
                folder: Some(cwd.join("src").to_string_lossy().into_owned()),
                command: Some("ls".to_owned()),
            }))
        );
        let opened = |path: &str, line| UiAct::Open(UiTarget::File { path: path.to_owned(), line });
        assert_eq!(read("ui_open", json!({ "file": "src/main.rs" })), Ok(Ok(opened(&main, None))));
        assert_eq!(
            read("ui_open", json!({ "file": "src/main.rs:7" })),
            Ok(Ok(opened(&main, Some(7))))
        );
        assert_eq!(
            read("ui_open", json!({ "url": "https://example.com" })),
            Ok(Ok(UiAct::Open(UiTarget::Page { url: "https://example.com/".to_owned() })))
        );
        assert_eq!(read("ui_split", json!({})), Ok(Ok(UiAct::Split(None))));
        assert_eq!(
            read("ui_split", json!({ "diff": true })),
            Ok(Ok(UiAct::Split(Some(UiTarget::Diff))))
        );
        assert_eq!(
            read("ui_region", json!({ "region": "inspector", "change": "expand" })),
            Ok(Ok(UiAct::Region { region: "inspector".to_owned(), change: UiChange::Expand }))
        );
        assert_eq!(read("ui_chat", json!({})), Ok(Ok(UiAct::Chat { chat: None })));
        let code =
            |value: Value, name: &str| read(name, value).map(|act| act.err().map(|f| f.code));
        assert_eq!(code(json!({ "file": "gone.rs" }), "ui_open"), Ok(Some(UiError::Unknown)));
        assert_eq!(code(json!({ "file": "src" }), "ui_open"), Ok(Some(UiError::Unknown)));
        assert_eq!(code(json!({}), "ui_open"), Ok(Some(UiError::Unknown)));
        assert_eq!(
            code(json!({ "url": "x", "diff": true }), "ui_open"),
            Ok(Some(UiError::Unknown))
        );
        assert_eq!(
            code(json!({ "url": "file:///etc/hosts" }), "ui_open"),
            Ok(Some(UiError::Unknown))
        );
        assert_eq!(code(json!({ "folder": "nowhere" }), "ui_terminal"), Ok(Some(UiError::Unknown)));
        let invalid = |name: &str, value: Value| read(name, value).map_err(|e| e.code);
        assert_eq!(invalid("ui_state", json!({ "x": 1 })), Err(ErrorCode::Invalid));
        let shrink = json!({ "region": "inspector", "change": "shrink" });
        assert_eq!(invalid("ui_region", shrink), Err(ErrorCode::Invalid));
        Ok(())
    }

    /// A word with a scheme is a page; anything else a file.
    #[test]
    fn a_word_is_a_page_or_a_file() {
        let cwd = Path::new("/nowhere");
        assert!(matches!(target("http://localhost:5173/", None, cwd), Ok(UiTarget::Page { .. })));
        let file = target("README.md", None, cwd).map_err(|f| f.message);
        assert_eq!(file, Err("no file at /nowhere/README.md".to_owned()));
    }

    /// A failure is an error object naming its code; what was done is itself.
    #[test]
    fn an_outcome_is_json() {
        let failed = Err(UiFailure::new(UiError::Busy, "the person is typing"));
        assert_eq!(
            json(&failed),
            json!({ "error": { "code": "busy", "message": "the person is typing" } })
        );
        let done = Ok(UiDone { did: Some("showed the sidebar".to_owned()), ..UiDone::default() });
        assert_eq!(json(&done)["did"], "showed the sidebar");
        assert_eq!(json(&done)["window"]["typing"], false);
    }
}
