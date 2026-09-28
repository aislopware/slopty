//! An agent's conversation as orchestration reads it, and the permission prompts it answers
//! (`docs/decisions/topology.md`, "An orchestrating agent reads and answers another").
//!
//! The conversation is read from the agent's transcripts by the decoder the conversation face
//! uses (`slopty_agent::conversation::Transcripts`), so a page holds the entries a face would
//! show. The prompts are the daemon's held ones (`crate::conversation::Holds`), which a verb
//! reaches as [`crate::conversation::ORCHESTRATION`] through [`Conversations`].

use std::path::PathBuf;

use slopty_agent::conversation::Transcripts;
use slopty_core::SessionId;
use slopty_proto::conversation::{Meters, PermissionPrompt, ThreadId, Verdict};
use slopty_proto::orchestration::{ConversationPage, ErrorCode, ThreadInfo};

use super::{Failure, MAX_FILE_BYTES};

/// Most entries one page returns, whatever it asks.
pub const MAX_CONVERSATION_ENTRIES: u32 = 500;

/// Most encoded bytes of entries one page carries: the page is one reply frame, and this
/// leaves half of it for the rest, as a file read does.
const PAGE_BYTES: u64 = MAX_FILE_BYTES;

/// Where a session's conversation is written, and what the daemon heard of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sources {
    /// The transcript the agent writes now; `None` before it wrote one.
    pub main: Option<PathBuf>,
    /// The subagent transcripts the hooks named.
    pub subagents: Vec<PathBuf>,
    /// The status line's latest meters.
    pub meters: Option<Meters>,
}

/// The daemon's followed conversations and held prompts, as orchestration reaches them.
pub trait Conversations: Send + Sync {
    /// Orchestration follows `session` from now on (following twice is following once);
    /// the prompts held for the session now, oldest first.
    fn follow(&self, session: SessionId) -> Vec<PermissionPrompt>;
    /// Where the session's conversation is.
    fn sources(&self, session: SessionId) -> Sources;
    /// Orchestration answers prompt `ask` of `session`. `false` when nothing is held under
    /// that id for orchestration there: answered already, released to the TUI, withdrawn, or
    /// held before orchestration followed and for another session.
    fn answer(&self, session: SessionId, ask: u64, verdict: Verdict) -> bool;
    /// The session ended: orchestration stops following it.
    fn forget(&self, session: SessionId);
}

/// A page of the conversation `sources` names.
///
/// `thread`'s entries from `since` (its last `max` when absent), at most `max` and
/// [`MAX_CONVERSATION_ENTRIES`] of them and no more than a frame holds, with the thread's tasks,
/// every thread, and the meters. Nothing is held on the page: the caller adds the prompts.
///
/// Reads the files: call it on the blocking pool.
///
/// # Errors
///
/// [`ErrorCode::Invalid`] for a subagent thread the conversation does not have.
pub fn read_page(
    sources: &Sources,
    thread: &ThreadId,
    since: Option<u32>,
    max: u32,
) -> Result<ConversationPage, Failure> {
    let mut transcripts = Transcripts::default();
    if let Some(main) = &sources.main {
        transcripts.read(main, &sources.subagents);
    }
    let conversation = transcripts.conversation();
    let count = |id: &ThreadId| u32::try_from(conversation.entries(id).len()).unwrap_or(u32::MAX);
    let snapshot = conversation.snapshot();
    let mut threads: Vec<ThreadInfo> = snapshot
        .iter()
        .map(|t| ThreadInfo { id: t.id.clone(), origin: t.origin.clone(), entries: count(&t.id) })
        .collect();
    if !threads.iter().any(|t| t.id == ThreadId::Main) {
        threads.insert(0, ThreadInfo { id: ThreadId::Main, origin: None, entries: 0 });
    }
    if !threads.iter().any(|t| t.id == *thread) {
        let name = match thread {
            ThreadId::Agent(name) => name.as_str(),
            ThreadId::Main => "main",
        };
        return Err(Failure::new(
            ErrorCode::Invalid,
            format!("this conversation has no subagent thread {name:?}"),
        ));
    }
    let all = conversation.entries(thread);
    let total = count(thread);
    let max = max.min(MAX_CONVERSATION_ENTRIES);
    let start = since.unwrap_or_else(|| total.saturating_sub(max)).min(total);
    let mut entries = Vec::new();
    let mut bytes = 0_u64;
    let skip = usize::try_from(start).unwrap_or(usize::MAX);
    let take = usize::try_from(max).unwrap_or(usize::MAX);
    for entry in all.iter().skip(skip).take(take) {
        let size = slopty_proto::codec::encode_body(entry)
            .map_or(u64::MAX, |b| u64::try_from(b.len()).unwrap_or(u64::MAX));
        bytes = bytes.saturating_add(size);
        // One entry always goes, however large: the decoder clipped its texts.
        if bytes > PAGE_BYTES && !entries.is_empty() {
            break;
        }
        entries.push(entry.clone());
    }
    let next = start.saturating_add(u32::try_from(entries.len()).unwrap_or(u32::MAX));
    Ok(ConversationPage {
        threads,
        thread: thread.clone(),
        entries,
        start,
        next,
        total,
        tasks: conversation.tasks(thread).to_vec(),
        meters: sources.meters.clone(),
        held: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

    use slopty_proto::conversation::Body;

    use super::*;

    fn prompt(n: u32, parent: Option<u32>) -> String {
        let record = serde_json::json!({
            "type": "user", "uuid": format!("u{n}"), "parentUuid": parent.map(|p| format!("u{p}")),
            "timestamp": "2026-09-28T03:15:25.849Z",
            "message": { "role": "user", "content": format!("prompt {n}") },
        });
        format!("{record}\n")
    }

    fn transcript(path: &Path, prompts: u32) {
        let mut file = std::fs::File::create(path).expect("create");
        for n in 0..prompts {
            file.write_all(prompt(n, n.checked_sub(1)).as_bytes()).expect("write");
        }
    }

    fn words(page: &ConversationPage) -> Vec<String> {
        page.entries
            .iter()
            .map(|e| match &e.body {
                Body::Prompt(p) => p.text.text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// A page starts where it is asked to, or at the last `max` entries, and says where the
    /// next one starts and how many there are; a page past the end is empty.
    #[test]
    fn a_page_is_read_from_its_start_or_the_end() {
        let dir = tempfile::tempdir().expect("dir");
        let main = dir.path().join("s.jsonl");
        transcript(&main, 5);
        let meters = Some(Meters { model: Some("Opus".to_owned()), ..Meters::default() });
        let sources = Sources { main: Some(main), subagents: Vec::new(), meters: meters.clone() };
        let tail = read_page(&sources, &ThreadId::Main, None, 2).expect("page");
        assert_eq!(words(&tail), ["prompt 3", "prompt 4"]);
        assert_eq!((tail.start, tail.next, tail.total), (3, 5, 5));
        assert_eq!(tail.meters, meters);
        assert_eq!(tail.threads, [ThreadInfo { id: ThreadId::Main, origin: None, entries: 5 }]);
        let head = read_page(&sources, &ThreadId::Main, Some(0), 2).expect("page");
        assert_eq!((words(&head), head.next), (vec!["prompt 0".into(), "prompt 1".into()], 2));
        let past = read_page(&sources, &ThreadId::Main, Some(9), 2).expect("page");
        assert_eq!((past.entries.len(), past.start, past.next), (0, 5, 5));
        let capped = read_page(&sources, &ThreadId::Main, Some(0), u32::MAX).expect("page");
        assert_eq!(capped.entries.len(), 5, "a large max is capped, not refused");
    }

    /// Before the agent wrote a transcript the conversation is empty, not an error; a subagent
    /// thread the conversation lacks is.
    #[test]
    fn no_transcript_is_an_empty_conversation_and_a_missing_thread_is_refused() {
        let empty = read_page(&Sources::default(), &ThreadId::Main, None, 50).expect("page");
        assert_eq!((empty.total, empty.entries.len(), empty.threads.len()), (0, 0, 1));
        let missing = read_page(&Sources::default(), &ThreadId::Agent("a9".to_owned()), None, 5);
        let failure = missing.expect_err("no such thread");
        assert_eq!(failure.code, ErrorCode::Invalid);
        assert!(failure.message.contains("a9"), "{failure:?}");
    }
}
