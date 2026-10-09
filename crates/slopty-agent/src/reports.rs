//! Reports on their way into an agent through its hooks (`docs/decisions/projects.md`,
//! "Reports go up through hooks").
//!
//! The server sends a batch of reports for an agent's terminal to its worker, which keeps it in
//! a file per session beside its control socket ([`put`]). When the agent next starts, is
//! prompted or finishes a turn, Claude Code runs `slopty hook reports`, which asks the worker
//! with the session's token: the worker says what to print ([`hand_over`], [`output`]), the
//! hook prints it as its context, then says so, and only then does the worker let the batch go
//! ([`handed`]) and tell the server it was read. Nothing is typed into a terminal.
//!
//! An agent at rest would wait for its next prompt. So the same hook also notes the session's
//! inbox, the socket Claude Code takes messages from other processes on
//! (`CLAUDE_CODE_MESSAGING_SOCKET`, [`Inbox`]), and the worker posts a batch there as soon as it
//! arrives ([`message`]): an idle agent starts a turn with it, a busy one reads it between tool
//! calls.
//!
//! A post is a wake-up, never the hand-over itself. Claude Code may hold or drop what arrives on
//! the socket (`crossSessionInbound`, or a session that bypasses permission prompts holding
//! messages from one that does not), and it says nothing back. So the batch stays in its file,
//! the post is noted beside it with a mark the message carries ([`Posted`]), and the next hook
//! decides: when the mark is in the prompt or the transcript, the message reached the agent and
//! the hook only acknowledges it; when it is not, the hook hands the batch over itself
//! ([`reached`]). A report is therefore never acknowledged unread, and read twice at worst.
//!
//! The worker's word that a batch was read can be lost on its way (no server link then, or a
//! link that fell behind), and the server then sends the batch again. So the worker notes the
//! last batch each session's agent read ([`handed`], [`last_handed`]), across its own restarts
//! too, and a batch sent again under that number is only acknowledged again, never kept to be
//! read twice ([`put`]). The server never gives two batches one number.

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
/// Whether it was kept: the batch `session`'s agent read last, sent again because the word of
/// it was lost, is not, and its caller says it was read again.
///
/// # Errors
/// The directory or the file cannot be written.
pub fn put(dir: &Path, session: SessionId, batch: &Batch) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    if read_last(dir, session) == Some(batch.batch) {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let json = serde_json::to_vec(batch).map_err(io::Error::other)?;
    slopty_platform::fs::replace(&file(dir, session), &json)?;
    Ok(true)
}

/// The note of the last batch a session's agent read.
const HANDED: &str = "handed";

fn handed_file(dir: &Path, session: SessionId) -> PathBuf {
    dir.join(format!("{session}.{HANDED}"))
}

/// The last batch `session`'s agent read, as noted; none when none is or it cannot be read.
fn read_last(dir: &Path, session: SessionId) -> Option<u64> {
    std::fs::read_to_string(handed_file(dir, session)).ok()?.trim().parse().ok()
}

/// The last batch each session's agent read, to say again to a server that may not have
/// heard.
///
/// # Errors
/// The directory is there and cannot be read.
pub fn last_handed(dir: &Path) -> io::Result<Vec<(SessionId, u64)>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != HANDED) {
            continue;
        }
        let session = path.file_stem().and_then(|s| s.to_str()?.parse::<SessionId>().ok());
        if let Some(session) = session
            && let Some(batch) = read_last(dir, session)
        {
            out.push((session, batch));
        }
    }
    Ok(out)
}

/// Forget which batch `session`'s agent read last: its session ended.
///
/// # Errors
/// One is noted and cannot be removed.
pub fn forget_handed(dir: &Path, session: SessionId) -> io::Result<()> {
    remove(&handed_file(dir, session))
}

/// The batch kept for `session`, left where it is.
///
/// # Errors
/// A batch is there and cannot be read.
pub fn peek(dir: &Path, session: SessionId) -> io::Result<Option<Batch>> {
    match std::fs::read(file(dir, session)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
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

/// A batch posted to its session's inbox: its number, and the mark the message carried.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Posted {
    /// The batch.
    pub batch: u64,
    /// A mark made for this one post, which only the message holds.
    pub mark: String,
}

impl Posted {
    /// A post of `batch` under a fresh mark.
    #[must_use]
    pub fn new(batch: u64) -> Self {
        Self { batch, mark: uuid::Uuid::new_v4().simple().to_string() }
    }
}

fn posted_file(dir: &Path, session: SessionId) -> PathBuf {
    dir.join(format!("{session}.posted"))
}

/// Note that `posted` went to `session`'s inbox, beside its batch in `dir`.
///
/// # Errors
/// The file cannot be written.
pub fn note_posted(dir: &Path, session: SessionId, posted: &Posted) -> io::Result<()> {
    let json = serde_json::to_vec(posted).map_err(io::Error::other)?;
    slopty_platform::fs::replace(&posted_file(dir, session), &json)
}

/// `session`'s agent read batch `batch`: it is no longer kept, nor the note of its post, and it
/// is noted as the last read ([`put`]).
///
/// Whether it was still kept; a later batch kept in its place stays. Its caller runs one of
/// these, [`put`] and [`hand_over`] at a time.
///
/// # Errors
/// The batch or its note is there and cannot be read, written or removed.
pub fn handed(dir: &Path, session: SessionId, batch: u64) -> io::Result<bool> {
    if peek_posted(dir, session).is_some_and(|p| p.batch == batch) {
        remove(&posted_file(dir, session))?;
    }
    if peek(dir, session)?.is_none_or(|kept| kept.batch != batch) {
        return Ok(false);
    }
    slopty_platform::fs::replace(&handed_file(dir, session), batch.to_string().as_bytes())?;
    remove(&file(dir, session))?;
    Ok(true)
}

fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The note of `session`'s last post, left in place; none when it cannot be read.
fn peek_posted(dir: &Path, session: SessionId) -> Option<Posted> {
    serde_json::from_slice(&std::fs::read(posted_file(dir, session)).ok()?).ok()
}

/// How much of a transcript's end is read for a post's mark.
///
/// A message the agent took sits near the end by the next hook; one further back than this is
/// handed over again, which costs a repeat and never a loss.
pub const TRANSCRIPT_TAIL: u64 = 4 * 1024 * 1024;

/// Whether the message that carried `mark` reached the agent.
///
/// It did when the mark is in the hook's `prompt` (the message started the turn), or in the
/// last [`TRANSCRIPT_TAIL`] bytes of its `transcript`. The mark is hex, so JSON never escapes it.
#[must_use]
pub fn reached(mark: &str, prompt: Option<&str>, transcript: Option<&Path>) -> bool {
    if mark.is_empty() {
        return false;
    }
    if prompt.is_some_and(|p| p.contains(mark)) {
        return true;
    }
    transcript.is_some_and(|path| tail_holds(path, mark.as_bytes()).unwrap_or(false))
}

fn tail_holds(path: &Path, needle: &[u8]) -> io::Result<bool> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL)))?;
    let mut tail = Vec::new();
    file.take(TRANSCRIPT_TAIL).read_to_end(&mut tail)?;
    Ok(tail.windows(needle.len()).any(|w| w == needle))
}

/// How the server's reports block opens.
const OPEN: &str = "<slopty-reports";

/// What the worker writes to an [`Inbox`] to hand `context` over, one JSON document a line.
///
/// The session's token comes first when it gave one, then the message as a user turn Claude
/// Code reads at its next chance (`priority: "next"`): at once when idle, between tool calls
/// when busy. The message carries `mark` ([`Posted`]) in its reports block's opening tag, or
/// on a line of its own after a context that has none. The shape is Claude Code's own
/// (v2.1.224 and later); the stub claude speaks it, and the version is pinned with it.
#[must_use]
pub fn message(inbox: &Inbox, context: &str, mark: &str) -> String {
    let mut lines = String::new();
    if let Some(token) = &inbox.token {
        lines.push_str(&json!({ "type": "auth", "token": token }).to_string());
        lines.push('\n');
    }
    let content = match context.strip_prefix(OPEN) {
        Some(rest) => format!("{OPEN} delivery=\"{mark}\"{rest}"),
        None => format!("{context}\n(delivery {mark})"),
    };
    let user = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "priority": "next",
    });
    lines.push_str(&user.to_string());
    lines.push('\n');
    lines
}

/// Forget every session's batch and post: a worker starting again is sent again what is
/// outstanding. Which batch each agent read last stays, so one of those sent again is known.
///
/// # Errors
/// The directory is there and cannot be read, or a file in it cannot be removed.
pub fn clear(dir: &Path) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != HANDED) {
            remove(&path)?;
        }
    }
    Ok(())
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

/// What the hook for `event` does with the batch kept for `session` in `dir`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Handed {
    /// The batch, to acknowledge.
    pub batch: u64,
    /// What to print to hand it over; none when its post reached the agent already.
    pub print: Option<Value>,
}

/// What a hook's payload says of the turn it fires in.
#[derive(Clone, Copy, Debug, Default)]
pub struct Turn<'a> {
    /// The prompt that started it, for `UserPromptSubmit`.
    pub prompt: Option<&'a str>,
    /// The conversation's transcript.
    pub transcript: Option<&'a Path>,
    /// A `Stop` hook held this turn's end already (`stop_hook_active`).
    pub held: bool,
}

/// The batch kept for `session` for a hook firing for `event` in `turn`, and what to print for
/// it ([`Handed`]).
///
/// It stays kept until the hook says it printed it ([`handed`]), so a hook that dies before is
/// followed by another that hands it over.
///
/// None when nothing waits, it cannot be read, or the event takes no reports. A turn a `Stop`
/// hook held already is let end: reports that keep coming would otherwise never let the agent
/// rest, so they wait for its next prompt or its inbox. One its inbox brought is acknowledged
/// there all the same.
#[must_use]
pub fn hand_over(
    dir: &Path,
    session: SessionId,
    event: HookEvent,
    turn: Turn<'_>,
) -> Option<Handed> {
    if !EVENTS.contains(&event) {
        return None;
    }
    let Turn { prompt, transcript, held } = turn;
    let reached_by = |batch: &Batch, posted: Option<&Posted>| {
        posted.is_some_and(|p| p.batch == batch.batch && reached(&p.mark, prompt, transcript))
    };
    let batch = peek(dir, session).ok()??;
    let read = reached_by(&batch, peek_posted(dir, session).as_ref());
    if event == HookEvent::Stop && held && !read {
        return None;
    }
    let print = if read { None } else { output(event, &batch.context) };
    Some(Handed { batch: batch.batch, print })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch kept for a session stays until its agent read it, the latest in place of the
    /// earlier; word that an earlier one was read leaves the latest; another session's stays.
    #[test]
    fn a_batch_is_kept_per_session_until_it_is_read() {
        let root = tempfile::tempdir().expect("dir");
        let dir = dir(&root.path().join("worker.sock"));
        let (a, b) = (SessionId::new(), SessionId::new());
        assert_eq!(peek(&dir, a).expect("read"), None);
        let first = Batch { batch: 1, context: "one".to_owned() };
        let second = Batch { batch: 2, context: "one\ntwo".to_owned() };
        put(&dir, a, &first).expect("put");
        put(&dir, a, &second).expect("put");
        put(&dir, b, &first).expect("put");
        assert_eq!(peek(&dir, a).expect("read"), Some(second.clone()));
        assert!(!handed(&dir, a, 1).expect("handed"), "an earlier batch's word");
        assert_eq!(peek(&dir, a).expect("read"), Some(second), "leaves the latest");
        assert!(handed(&dir, a, 2).expect("handed"));
        assert_eq!(peek(&dir, a).expect("read"), None, "read once");
        assert_eq!(peek(&dir, b).expect("read"), Some(first));
        clear(&dir).expect("clear");
        assert_eq!(peek(&dir, b).expect("read"), None, "cleared");
        clear(&dir).expect("nothing to clear");
    }

    /// A batch sent again because the word that it was read was lost is not kept to be read
    /// twice, across the worker's restart too; every session's last read is there to say
    /// again, and goes with its session.
    #[test]
    fn a_batch_read_already_is_not_kept_again() {
        let root = tempfile::tempdir().expect("dir");
        let dir = dir(&root.path().join("worker.sock"));
        let (a, b) = (SessionId::new(), SessionId::new());
        assert_eq!(last_handed(&dir).expect("read"), Vec::new(), "no directory yet");
        let first = Batch { batch: 7, context: "one".to_owned() };
        assert!(put(&dir, a, &first).expect("put"));
        assert!(handed(&dir, a, 7).expect("handed"));
        assert!(!put(&dir, a, &first).expect("put"), "sent again: read already");
        assert_eq!(peek(&dir, a).expect("read"), None);
        assert!(put(&dir, b, &first).expect("put"), "another session's");
        clear(&dir).expect("clear");
        assert!(!put(&dir, a, &first).expect("put"), "known after a restart");
        assert_eq!(last_handed(&dir).expect("read"), [(a, 7)]);
        let next = Batch { batch: 8, context: "one\ntwo".to_owned() };
        assert!(put(&dir, a, &next).expect("put"), "a new batch is kept");
        forget_handed(&dir, a).expect("forget");
        assert_eq!(last_handed(&dir).expect("read"), Vec::new());
        forget_handed(&dir, a).expect("nothing to forget");
    }

    /// A batch whose post reached the agent (its mark in the prompt or the transcript) is only
    /// acknowledged by the next hook; one whose post was held, dropped or never made is
    /// handed over by it. Either way it stays until read, and goes with its note.
    #[test]
    fn a_hook_hands_over_only_what_the_post_did_not() {
        fn on(t: &Path) -> Turn<'_> {
            Turn { transcript: Some(t), ..Turn::default() }
        }

        fn prompted(p: &str) -> Turn<'_> {
            Turn { prompt: Some(p), ..Turn::default() }
        }

        let root = tempfile::tempdir().expect("dir");
        let dir = dir(&root.path().join("worker.sock"));
        let session = SessionId::new();
        let transcript = root.path().join("t.jsonl");
        let batch = Batch {
            batch: 3,
            context: "<slopty-reports project=\"p\">\nr\n</slopty-reports>".to_owned(),
        };
        let stop = HookEvent::Stop;

        // Never posted: the hook hands it over.
        put(&dir, session, &batch).expect("put");
        let handed = hand_over(&dir, session, stop, Turn::default()).expect("taken");
        assert_eq!(handed.batch, 3);
        assert_eq!(handed.print.expect("printed")["reason"].as_str(), Some(batch.context.as_str()));
        let again = hand_over(&dir, session, stop, Turn::default()).expect("again");
        assert_eq!(again.batch, 3, "a hook that died before saying so is followed by another");
        assert!(super::handed(&dir, session, 3).expect("handed"));
        assert!(hand_over(&dir, session, stop, Turn::default()).is_none(), "read once");

        // Each batch read has a number of its own: one read already is not kept again.
        let batch = Batch { batch: 5, ..batch };
        // Posted, and the message is in the transcript: acknowledged, not printed again.
        put(&dir, session, &batch).expect("put");
        let posted = Posted::new(5);
        assert_eq!(peek(&dir, session).expect("read"), Some(batch.clone()), "a peek leaves it");
        note_posted(&dir, session, &posted).expect("note");
        let inbox = Inbox { socket: PathBuf::from("/s"), token: None };
        let sent: Value =
            serde_json::from_str(message(&inbox, &batch.context, &posted.mark).trim())
                .expect("json");
        let line = json!({ "type": "user", "message": sent["message"] }).to_string();
        std::fs::write(&transcript, format!("{{\"type\":\"summary\"}}\n{line}\n")).expect("write");
        let handed = hand_over(&dir, session, stop, on(&transcript)).expect("taken");
        assert_eq!((handed.batch, handed.print), (5, None));
        assert!(super::handed(&dir, session, 5).expect("handed"));
        assert_eq!(peek_posted(&dir, session), None, "the note went with it");

        // Posted and held: the mark is nowhere, so the hook hands it over.
        let batch = Batch { batch: 6, ..batch };
        put(&dir, session, &batch).expect("put");
        note_posted(&dir, session, &Posted::new(6)).expect("note");
        let handed = hand_over(&dir, session, stop, on(&transcript)).expect("taken");
        assert!(handed.print.is_some(), "held by Claude Code, so handed over here");

        // The message started the turn: its mark is in the prompt.
        put(&dir, session, &batch).expect("put");
        let posted = Posted::new(6);
        note_posted(&dir, session, &posted).expect("note");
        let prompt = message(&inbox, &batch.context, &posted.mark);
        let handed = hand_over(&dir, session, HookEvent::UserPromptSubmit, prompted(&prompt))
            .expect("taken");
        assert_eq!(handed.print, None);

        // A note of an older batch's post says nothing of a newer batch.
        put(&dir, session, &Batch { batch: 7, ..batch.clone() }).expect("put");
        note_posted(&dir, session, &posted).expect("note");
        let handed = hand_over(&dir, session, HookEvent::UserPromptSubmit, prompted(&prompt))
            .expect("taken");
        assert_eq!(handed.batch, 7);
        assert!(handed.print.is_some());

        // An event that takes no reports leaves the batch.
        put(&dir, session, &batch).expect("put");
        assert!(hand_over(&dir, session, HookEvent::PreToolUse, Turn::default()).is_none());
        assert!(peek(&dir, session).expect("read").is_some());
        // A turn a `Stop` hook held already ends: the batch waits for the next.
        let held = Turn { held: true, ..on(&transcript) };
        assert!(hand_over(&dir, session, stop, held).is_none());
        assert!(peek(&dir, session).expect("read").is_some(), "still waiting");
        // One the inbox brought is acknowledged there all the same, and nothing is printed.
        let posted = Posted::new(6);
        note_posted(&dir, session, &posted).expect("note");
        let sent: Value =
            serde_json::from_str(message(&inbox, &batch.context, &posted.mark).trim())
                .expect("json");
        let line = json!({ "type": "user", "message": sent["message"] }).to_string();
        std::fs::write(&transcript, format!("{line}\n")).expect("write");
        let handed = hand_over(&dir, session, stop, held).expect("acknowledged");
        assert_eq!((handed.batch, handed.print), (6, None));
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
        let text = message(&noted, "task 2: done", "m1");
        let lines: Vec<Value> =
            text.lines().map(|l| serde_json::from_str(l).expect("json")).collect();
        assert_eq!(lines[0], json!({ "type": "auth", "token": "t0k" }));
        assert_eq!(lines[1]["type"], "user");
        assert_eq!(lines[1]["message"]["content"], "task 2: done\n(delivery m1)");
        assert_eq!(lines[1]["priority"], "next");
        let block = "<slopty-reports project=\"p\">\nr\n</slopty-reports>";
        let marked: Value =
            serde_json::from_str(message(&noted, block, "m2").lines().nth(1).expect("user"))
                .expect("json");
        assert_eq!(
            marked["message"]["content"],
            "<slopty-reports delivery=\"m2\" project=\"p\">\nr\n</slopty-reports>",
            "the mark rides in the block's opening tag"
        );
        let bare = Inbox { token: None, ..noted };
        assert_eq!(message(&bare, "x", "m").lines().count(), 1, "no token, no auth line");
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
    }
}
