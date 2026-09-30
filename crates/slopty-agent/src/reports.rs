//! Reports on their way into an agent through its hooks (`docs/decisions/projects.md`,
//! "Reports go up through hooks").
//!
//! The server sends a batch of reports for an agent's terminal to its worker, which keeps it in
//! a file per session beside its control socket ([`put`]). When the agent next starts, is
//! prompted or finishes a turn, Claude Code runs `slopty hook reports`, which takes the file
//! ([`take`]), prints it as the hook's context ([`output`]) and tells the worker it was handed
//! over ([`delivered_payload`]). Nothing is typed into a terminal.
//!
//! An agent at rest would wait for its next prompt. So the same hook also notes the session's
//! inbox, the socket Claude Code takes messages from other processes on
//! (`CLAUDE_CODE_MESSAGING_SOCKET`, [`Inbox`]), and the worker posts a batch there as soon as it
//! arrives ([`message`]): an idle agent starts a turn with it, a busy one reads it between tool
//! calls. The file stays the way in when a session has no inbox or the post fails.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slopty_core::SessionId;

use crate::HookEvent;

/// The events whose hook hands reports over: a session starting, a prompt, a turn's end.
pub const EVENTS: [HookEvent; 3] =
    [HookEvent::SessionStart, HookEvent::UserPromptSubmit, HookEvent::Stop];

/// A batch as the worker keeps it for its session.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Batch {
    /// Its number, the server's.
    pub batch: u64,
    /// What the agent reads.
    pub context: String,
}

/// Where the worker whose control socket is `ctl_socket` keeps its sessions' batches.
#[must_use]
pub fn dir(ctl_socket: &Path) -> PathBuf {
    ctl_socket.parent().map_or_else(|| PathBuf::from("reports"), |d| d.join("reports"))
}

fn file(dir: &Path, session: SessionId) -> PathBuf {
    dir.join(format!("{session}.json"))
}

/// Keep `batch` for `session`, in place of any it had: a later batch holds what an earlier one
/// did not hand over. Written whole and renamed into place, so a reader never sees half.
///
/// # Errors
/// The directory or the file cannot be written.
pub fn put(dir: &Path, session: SessionId, batch: &Batch) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let json = serde_json::to_vec(batch).map_err(io::Error::other)?;
    slopty_platform::fs::replace(&file(dir, session), &json)
}

/// Take the batch kept for `session`, if there is one: claimed by a rename first, so two hooks
/// firing at once never both hand it over.
///
/// # Errors
/// A batch is there and cannot be read.
pub fn take(dir: &Path, session: SessionId) -> io::Result<Option<Batch>> {
    let claimed = dir.join(format!("{session}.taken-{}", std::process::id()));
    match std::fs::rename(file(dir, session), &claimed) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let read = std::fs::read(&claimed);
    std::fs::remove_file(&claimed)?;
    serde_json::from_slice(&read?).map(Some).map_err(io::Error::other)
}

/// Keep `batch` for `session` unless another is kept already: a batch put back after a failed
/// post never replaces a newer one, which holds its reports too.
///
/// # Errors
/// The directory or the file cannot be written.
pub fn put_back(dir: &Path, session: SessionId, batch: &Batch) -> io::Result<()> {
    let spare = dir.join(format!("{session}.back-{}", std::process::id()));
    let json = serde_json::to_vec(batch).map_err(io::Error::other)?;
    slopty_platform::fs::replace(&spare, &json)?;
    // A link, unlike a rename, fails where a file is: the newer batch stays.
    let linked = std::fs::hard_link(&spare, file(dir, session));
    std::fs::remove_file(&spare)?;
    match linked {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

/// The variable Claude Code names a session's inbox socket in, for its hooks.
pub const SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
/// The variable Claude Code gives its hooks the session's inbox token in.
pub const TOKEN_ENV: &str = "CLAUDE_CODE_MESSAGING_TOKEN";

/// Where a Claude Code session takes messages from other processes on this machine: a Unix
/// socket, and the token that marks a message as the session's own child's.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Inbox {
    /// The socket (`CLAUDE_CODE_MESSAGING_SOCKET`).
    pub socket: PathBuf,
    /// The token (`CLAUDE_CODE_MESSAGING_TOKEN`), when the session gave one.
    pub token: Option<String>,
}

impl Inbox {
    /// The inbox a hook's environment names, if any.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let socket = std::env::var_os(SOCKET_ENV).filter(|s| !s.is_empty())?;
        let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.trim().is_empty());
        Some(Self { socket: PathBuf::from(socket), token })
    }
}

/// Where the worker whose control socket is `ctl_socket` keeps its sessions' inboxes. Apart
/// from the batches, which a worker starting again forgets: its agents' inboxes outlive it.
#[must_use]
pub fn inboxes(ctl_socket: &Path) -> PathBuf {
    ctl_socket.parent().map_or_else(|| PathBuf::from("inboxes"), |d| d.join("inboxes"))
}

/// Note `inbox` as `session`'s, in place of any it had.
///
/// # Errors
/// The directory or the file cannot be written.
pub fn keep_inbox(dir: &Path, session: SessionId, inbox: &Inbox) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let json = serde_json::to_vec(inbox).map_err(io::Error::other)?;
    slopty_platform::fs::replace(&file(dir, session), &json)
}

/// The inbox noted for `session`, if any.
///
/// # Errors
/// One is noted and cannot be read.
pub fn inbox(dir: &Path, session: SessionId) -> io::Result<Option<Inbox>> {
    match std::fs::read(file(dir, session)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Forget `session`'s inbox: its session ended, or its socket is gone.
///
/// # Errors
/// One is noted and cannot be removed.
pub fn forget_inbox(dir: &Path, session: SessionId) -> io::Result<()> {
    match std::fs::remove_file(file(dir, session)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What the worker writes to an [`Inbox`] to hand `context` over, one JSON document a line.
///
/// The session's token comes first when it gave one, then the message as a user turn Claude
/// Code reads at its next chance (`priority: "next"`): at once when idle, between tool calls
/// when busy. The shape is Claude Code's own (v2.1.224 and later); the stub claude speaks it,
/// and the version is pinned with it.
#[must_use]
pub fn message(inbox: &Inbox, context: &str) -> String {
    let mut lines = String::new();
    if let Some(token) = &inbox.token {
        lines.push_str(&json!({ "type": "auth", "token": token }).to_string());
        lines.push('\n');
    }
    let user = json!({
        "type": "user",
        "message": { "role": "user", "content": context },
        "priority": "next",
    });
    lines.push_str(&user.to_string());
    lines.push('\n');
    lines
}

/// Forget every session's batch: a worker starting again is sent again what is outstanding.
///
/// # Errors
/// The directory is there and cannot be removed.
pub fn clear(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What `slopty hook reports` prints for `event` to hand `context` over.
///
/// Context added to the session's start or to the prompt, or a turn's end held with the reports
/// as what to do next; none for an event that hands nothing over.
#[must_use]
pub fn output(event: HookEvent, context: &str) -> Option<Value> {
    match event {
        HookEvent::SessionStart | HookEvent::UserPromptSubmit => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": event.as_str(),
                "additionalContext": context,
            }
        })),
        HookEvent::Stop => Some(json!({ "decision": "block", "reason": context })),
        _ => None,
    }
}

/// The hook `slopty hook reports` posts once it handed `batch` over, which the worker reports
/// to the server ([`crate::Hook::report`]).
#[must_use]
pub fn delivered_payload(batch: u64) -> String {
    json!({ "hook_event_name": HookEvent::Delivered, "batch": batch }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch kept for a session is taken once, the latest in place of the earlier; another
    /// session's stays; nothing kept is nothing taken.
    #[test]
    fn a_batch_is_kept_per_session_and_taken_once() {
        let root = tempfile::tempdir().expect("dir");
        let dir = dir(&root.path().join("worker.sock"));
        let (a, b) = (SessionId::new(), SessionId::new());
        assert_eq!(take(&dir, a).expect("read"), None);
        let first = Batch { batch: 1, context: "one".to_owned() };
        let second = Batch { batch: 2, context: "one\ntwo".to_owned() };
        put(&dir, a, &first).expect("put");
        put(&dir, a, &second).expect("put");
        put(&dir, b, &first).expect("put");
        assert_eq!(take(&dir, a).expect("read"), Some(second));
        assert_eq!(take(&dir, a).expect("read"), None, "taken once");
        assert_eq!(take(&dir, b).expect("read"), Some(first));
        clear(&dir).expect("clear");
        assert!(!dir.exists());
        clear(&dir).expect("nothing to clear");
    }

    /// A batch put back after a failed post waits for the hooks, but never over a newer one.
    #[test]
    fn a_batch_put_back_never_replaces_a_newer_one() {
        let root = tempfile::tempdir().expect("dir");
        let dir = dir(&root.path().join("worker.sock"));
        let session = SessionId::new();
        let (older, newer) = (
            Batch { batch: 1, context: "one".to_owned() },
            Batch { batch: 2, context: "one\ntwo".to_owned() },
        );
        put(&dir, session, &older).expect("put");
        let taken = take(&dir, session).expect("read").expect("kept");
        put(&dir, session, &newer).expect("a newer batch meanwhile");
        put_back(&dir, session, &taken).expect("put back");
        assert_eq!(take(&dir, session).expect("read"), Some(newer), "the newer stays");
        put_back(&dir, session, &taken).expect("put back");
        assert_eq!(take(&dir, session).expect("read"), Some(taken), "back for the hooks");
        let left: Vec<_> = std::fs::read_dir(&dir).expect("dir").collect();
        assert!(left.is_empty(), "no spare left behind: {left:?}");
    }

    /// An inbox is noted per session and read back; a message is the token's line, when there
    /// is a token, then the reports as a user turn read at the next chance.
    #[test]
    fn an_inbox_is_kept_per_session_and_written_to_as_claude_code_reads() {
        let root = tempfile::tempdir().expect("dir");
        let dir = inboxes(&root.path().join("worker.sock"));
        let session = SessionId::new();
        assert_eq!(inbox(&dir, session).expect("read"), None);
        let noted = Inbox { socket: PathBuf::from("/tmp/cc.sock"), token: Some("t0k".to_owned()) };
        keep_inbox(&dir, session, &noted).expect("keep");
        assert_eq!(inbox(&dir, session).expect("read"), Some(noted.clone()));
        let text = message(&noted, "task 2: done");
        let lines: Vec<Value> =
            text.lines().map(|l| serde_json::from_str(l).expect("json")).collect();
        assert_eq!(lines[0], json!({ "type": "auth", "token": "t0k" }));
        assert_eq!(lines[1]["type"], "user");
        assert_eq!(lines[1]["message"]["content"], "task 2: done");
        assert_eq!(lines[1]["priority"], "next");
        let bare = Inbox { token: None, ..noted };
        assert_eq!(message(&bare, "x").lines().count(), 1, "no token, no auth line");
        forget_inbox(&dir, session).expect("forget");
        assert_eq!(inbox(&dir, session).expect("read"), None);
        forget_inbox(&dir, session).expect("nothing to forget");
    }

    /// A start and a prompt take the reports as context; a turn's end is held with them as
    /// what to do next; any other event hands nothing over.
    #[test]
    fn reports_are_handed_over_as_each_event_takes_them() {
        let said = output(HookEvent::UserPromptSubmit, "task 2: done").expect("output");
        assert_eq!(said["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert_eq!(said["hookSpecificOutput"]["additionalContext"], "task 2: done");
        let start = output(HookEvent::SessionStart, "r").expect("output");
        assert_eq!(start["hookSpecificOutput"]["hookEventName"], "SessionStart");
        let stop = output(HookEvent::Stop, "task 2: done").expect("output");
        assert_eq!(
            (stop["decision"].as_str(), stop["reason"].as_str()),
            (Some("block"), Some("task 2: done"))
        );
        assert_eq!(output(HookEvent::PreToolUse, "r"), None);
        let hook = crate::Hook::parse(&delivered_payload(7)).expect("a hook");
        assert_eq!((hook.event, hook.batch), (HookEvent::Delivered, Some(7)));
    }
}
