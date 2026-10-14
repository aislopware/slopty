//! Slopty's lists as the shell's rows: a worker as a device, a folder on it as a space, a thread as
//! a chat with its live status as a session.

use chrono::{DateTime, Utc};
use slopty_core::WallMs;
use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{Phase, ThreadId, ThreadMeta};
use slopty_view_proto::{Chat, Session, SessionStatus, Space};

/// A wall time as the shell's.
#[must_use]
pub fn time(at: WallMs) -> DateTime<Utc> {
    i64::try_from(at.as_millis()).ok().and_then(DateTime::from_timestamp_millis).unwrap_or_default()
}

/// The space of the folder `path` on `device`: one id for one folder on one machine.
#[must_use]
pub fn space_id(device: &str, path: &str) -> String {
    format!("{device}:{path}")
}

/// The folder `path` on `device` as a space.
#[must_use]
pub fn space(device: &str, path: &str, git: bool, created: DateTime<Utc>) -> Space {
    Space {
        id: space_id(device, path),
        device_id: device.to_owned(),
        path: path.to_owned(),
        name: None,
        git_detected: git,
        git_checked_at: None,
        checkout_id: None,
        repository_id: None,
        created_at: created,
    }
}

/// The folder a thread's row places it in: its repository's root, else where it works.
#[must_use]
pub fn folder(row: &ThreadRow) -> Option<&str> {
    row.repo.as_deref().or(row.cwd.as_deref()).filter(|path| !path.is_empty())
}

/// The thread that started this one: the row's link to it, else the parent its agent was
/// started under ([`slopty_proto::agent::PARENT_FACT`]), else the thread it was asked aside of
/// ([`ThreadMeta::ASIDE_FACT`]).
#[must_use]
pub fn parent(row: &ThreadRow) -> Option<ThreadId> {
    row.parent.as_ref().map(|link| link.thread).or_else(|| {
        [slopty_proto::agent::PARENT_FACT, ThreadMeta::ASIDE_FACT]
            .iter()
            .find_map(|fact| row.facts.get(*fact).and_then(|thread| thread.parse().ok()))
    })
}

/// A thread's row on `device` as a chat.
#[must_use]
pub fn chat(row: &ThreadRow, device: &str) -> Chat {
    let updated = time(row.updated_ms);
    Chat {
        id: row.id.to_string(),
        device_id: device.to_owned(),
        title: Some(row.title.clone()).filter(|title| !title.is_empty()),
        archived: false,
        cwd: row.cwd.clone(),
        branch: row.facts.get("branch").cloned(),
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: row.last_line.clone(),
        last_message_at: row.ended.map(|ended| time(ended.at_ms)).or(Some(updated)),
        created_at: updated,
        harness_session_id: Some(row.native.clone()).filter(|native| !native.is_empty()),
        harness_session_cwd: None,
        space_id: folder(row).map(|path| space_id(device, path)),
        last_seen_at: row
            .ended
            .filter(|ended| ended.turn <= row.seen)
            .map(|ended| time(ended.at_ms)),
        room_gen: None,
        parent_chat_id: parent(row).map(|thread| thread.to_string()),
    }
}

/// The pull request a thread's row shows, as the shell's summary of it, on branch `branch`.
#[must_use]
pub fn pull(
    row: &ThreadRow,
    branch: Option<&str>,
) -> Option<slopty_view_proto::ChangeRequestSummary> {
    use slopty_proto::thread::wire::PullStands;
    use slopty_view_proto::ChangeRequestState;
    let pull = row.pull.as_ref()?;
    let state = match pull.stands {
        PullStands::Merged => ChangeRequestState::Merged,
        PullStands::Closed => ChangeRequestState::Closed,
        _ => ChangeRequestState::Open,
    };
    Some(slopty_view_proto::ChangeRequestSummary {
        provider: match pull.forge {
            slopty_proto::git::Forge::GitHub => "github",
            slopty_proto::git::Forge::GitLab => "gitlab",
        }
        .to_owned(),
        number: u64::from(pull.number),
        title: pull.title.clone(),
        url: pull.url.clone(),
        state,
        base_ref: pull.base.clone(),
        head_ref: branch.unwrap_or_default().to_owned(),
    })
}

/// A thread's live status as a session, as of `now`, with the subagents of it that run.
#[must_use]
pub fn session(row: &ThreadRow, device: &str, running: u32, now: DateTime<Utc>) -> Session {
    // An agent waiting on its own background work or a wakeup is at its prompt: the person may
    // write to it, and there is no turn for a stop to end. It works again when one wakes it.
    let status = match row.status.phase {
        Phase::Working => SessionStatus::Working,
        Phase::NeedsYou => SessionStatus::AwaitingInput,
        Phase::Failed => SessionStatus::Errored,
        Phase::Waiting | Phase::Idle | Phase::Done | Phase::Stopped => SessionStatus::Idle,
    };
    Session {
        last_completed_turn: row
            .ended
            .filter(|ended| ended.answered)
            .map(|ended| ended.turn.0.to_string()),
        chat_id: row.id.to_string(),
        device_id: device.to_owned(),
        status,
        started_at: (status == SessionStatus::Working).then(|| time(row.status.since_ms)),
        updated_at: now,
        running_subagents: running,
    }
}

/// Whether a thread's row says it is at work.
#[must_use]
pub const fn running(row: &ThreadRow) -> bool {
    matches!(row.status.phase, Phase::Working | Phase::Waiting)
}

/// The subagents of `parent` that run, among `rows`.
#[must_use]
pub fn running_children<'a>(parent: ThreadId, rows: impl Iterator<Item = &'a ThreadRow>) -> u32 {
    let count =
        rows.filter(|row| self::parent(row) == Some(parent)).filter(|row| running(row)).count();
    u32::try_from(count).unwrap_or(u32::MAX)
}
