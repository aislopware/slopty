//! Slopty's lists as the shell's rows: a worker as a device, a folder on it as a space, a thread as
//! a chat with its live status as a session.

use std::borrow::Cow;
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use slopty_core::WallMs;
use slopty_proto::thread::wire::ThreadRow;
use slopty_proto::thread::{Phase, ThreadId, ThreadMeta};
use slopty_view_proto::{Chat, Session, SessionStatus, SessionWait, Space};

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

/// The repository a thread's row names, as the shell groups projects by it: its first commit,
/// which every clone and worktree shares, else its normalized origin.
#[must_use]
pub fn repository_id(row: &ThreadRow) -> Option<String> {
    let repo = row.repo_id.as_ref()?;
    repo.root.as_deref().map(|root| format!("commit:{root}")).or_else(|| repo.origin.clone())
}

/// Where Slopty's own worktrees of a clone go: `<clone>/.claude/worktrees/<name>`.
const WORKTREES: &str = "/.claude/worktrees/";

/// `path` without a trailing slash, `~` spelled out under `home`.
fn plain(path: &str, home: Option<&str>) -> String {
    let path = path.trim_end_matches('/');
    match (path.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.trim_end_matches('/'))
        }
        _ => path.to_owned(),
    }
}

/// The project folder a chat working in `cwd` belongs to, among the folders the person `added`.
///
/// `repo` is its repository's root and `home` its machine's home; each added folder comes by
/// the path it was added at and where it really is (its links resolved, as the worker names
/// the folders threads work in):
///
/// - the deepest added folder it works in, or whose repository it is, by either path;
/// - else, in one of Slopty's worktrees of a clone, the clone's own folder;
/// - else its repository's root, else where it works;
/// - and none for a chat at home or in a scratch folder of its own
///   ([`slopty_view_proto::projectless`]), which has no project.
#[must_use]
pub fn place(
    cwd: Option<&str>,
    repo: Option<&str>,
    home: Option<&str>,
    added: &[(&str, &str)],
) -> Option<String> {
    let cwd = cwd.or(repo).filter(|path| !path.is_empty()).map(|path| plain(path, home))?;
    let repo = repo.filter(|path| !path.is_empty()).map(|path| plain(path, home));
    let under = |path: &str, root: &str| {
        let root = root.trim_end_matches('/');
        path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
    };
    let holds = |root: &str| under(&cwd, root) || repo.as_deref().is_some_and(|r| under(r, root));
    let deepest = added
        .iter()
        .filter_map(|&(path, real)| {
            let depth = [path, real]
                .into_iter()
                .filter(|root| holds(root))
                .map(|root| root.trim_end_matches('/').len())
                .max()?;
            Some((depth, path))
        })
        .max_by_key(|&(depth, _)| depth);
    if let Some((_, path)) = deepest {
        return Some(path.trim_end_matches('/').to_owned());
    }
    let scratch = plain(slopty_view_proto::projectless::SCRATCH_ROOT, home);
    if under(&cwd, &scratch) {
        return None;
    }
    let folder = repo.unwrap_or(cwd);
    if let Some((clone, _)) = folder.split_once(WORKTREES) {
        return Some(clone.to_owned());
    }
    let at_home = folder == "~" || home.is_some_and(|home| folder == home.trim_end_matches('/'));
    (!at_home).then_some(folder)
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

/// Whether another agent started the thread: a subagent of a call, or an agent started by
/// another; not a side chat the person asked aside.
#[must_use]
pub fn subagent(row: &ThreadRow) -> bool {
    row.parent.is_some() || row.facts.contains_key(slopty_proto::agent::PARENT_FACT)
}

/// A thread's row on `device` as a chat.
#[must_use]
pub fn chat(row: &ThreadRow, device: &str) -> Chat {
    let updated = time(row.updated_ms);
    // When it began; a row that cannot say goes by when it last moved.
    let created = if row.created_ms.is_zero() { updated } else { time(row.created_ms) };
    Chat {
        id: row.id.to_string(),
        device_id: device.to_owned(),
        // A title is plain words everywhere it shows: a prompt's Markdown reads as its words.
        title: Some(slopty_kit::markdown::plain_line(&row.title)).filter(|title| !title.is_empty()),
        archived: false,
        cwd: row.cwd.clone(),
        branch: row.facts.get("branch").cloned(),
        checkout_id: None,
        source_context: None,
        config: None,
        last_message_preview: row.last_line.clone(),
        // Only a turn that ended is a message to read: an agent that has answered nothing yet
        // has nothing unread.
        last_message_at: row.ended.map(|ended| time(ended.at_ms)),
        created_at: created,
        harness_session_id: Some(row.native.clone()).filter(|native| !native.is_empty()),
        harness_session_cwd: None,
        space_id: folder(row).map(|path| space_id(device, path)),
        last_seen_at: row
            .ended
            .filter(|ended| ended.turn <= row.seen)
            .map(|ended| time(ended.at_ms)),
        room_gen: None,
        parent_chat_id: parent(row).map(|thread| thread.to_string()),
        subagent: subagent(row),
        in_parent: row.parent.is_some(),
        // What the person last kept writing to it, on any device.
        draft: row.draft.as_ref().map(|draft| draft.text.clone()).filter(|text| !text.is_empty()),
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

/// A plan's rate window as the usage ring names it: Claude Code's five-hour window is the
/// session's, its seven-day ones the week's (the model it is for after), as zeron names them.
fn window_label(name: &str) -> String {
    let name = name.trim();
    let (base, rest) = if let Some(rest) = name.strip_prefix("five-hour") {
        ("Session", rest)
    } else if let Some(rest) = name.strip_prefix("seven-day") {
        ("Week", rest)
    } else if let Some(rest) = name.strip_prefix("month") {
        ("Month", rest)
    } else {
        let mut chars = name.chars();
        let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
        return format!("{first}{}", chars.as_str()).replace(['-', '_'], " ");
    };
    let rest = rest.trim_start_matches(['-', '_', ' ']);
    if rest.is_empty() {
        return base.to_owned();
    }
    let mut chars = rest.chars();
    let first = chars.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    format!("{base} · {first}{}", chars.as_str())
}

/// A plan's rate windows as the shell's usage ring takes them, the most used first.
#[must_use]
pub fn limits(limits: &[slopty_proto::thread::Limit]) -> Vec<slopty_view_proto::AgentUsageWindow> {
    let mut windows: Vec<_> = limits
        .iter()
        .map(|limit| slopty_view_proto::AgentUsageWindow {
            label: window_label(&limit.name),
            used_fraction: f32::from(u16::try_from(limit.used_bp.min(10_000)).unwrap_or(10_000))
                / 10_000.0,
            resets_at: limit.resets_ms.map(time),
        })
        .collect();
    windows.sort_by(|a, b| b.used_fraction.total_cmp(&a.used_fraction));
    windows
}

/// The longest a session's reason line is, in characters as seen.
const SUMMARY_MAX: usize = 60;

/// What a thread's row says it waits on, as the sidebar's reason line names it: an approval, a
/// question, a plan, its context being compacted, or the row's own open kind.
#[must_use]
pub fn waiting_on(row: &ThreadRow) -> Option<SessionWait> {
    use slopty_proto::thread::Wait;
    let wait = row.status.wait.as_ref()?;
    let kind = match wait.kind.as_str() {
        "permission" => SessionWait::APPROVAL,
        "question" => SessionWait::QUESTION,
        "plan" => SessionWait::PLAN,
        Wait::COMPACTING => SessionWait::COMPACTING,
        other => other,
    };
    let summary = super::text::line(&wait.text, SUMMARY_MAX);
    Some(SessionWait { kind: kind.to_owned(), summary: (!summary.is_empty()).then_some(summary) })
}

/// A thread's live status as a session, with the subagents of it that run.
///
/// A turn under way stands as of `leased`: when this client, by its own clock, last heard the
/// row's lease ([`ThreadRow::lease_ms`]) move, the worker's word that its agent lives. So the
/// shell lets one whose agent went silent go stale, and a worker whose clock is off by any
/// amount moves nothing. A row with no lease stands as of `heard`, when this client last heard
/// its worker.
#[must_use]
pub fn session(
    row: &ThreadRow,
    device: &str,
    running: u32,
    heard: DateTime<Utc>,
    leased: Option<DateTime<Utc>>,
) -> Session {
    let status = match row.status.phase {
        Phase::Working => SessionStatus::Working,
        Phase::NeedsYou => SessionStatus::AwaitingInput,
        Phase::Failed => SessionStatus::Errored,
        // At its prompt, waiting on its own background work or a wakeup: the person may write
        // to it, and there is no turn for a stop to end.
        Phase::Waiting => SessionStatus::Waiting,
        Phase::Done => SessionStatus::Done,
        Phase::Stopped => SessionStatus::Stopped,
        Phase::Idle => SessionStatus::Idle,
    };
    let live = matches!(status, SessionStatus::Working | SessionStatus::AwaitingInput);
    let updated_at = match leased {
        Some(leased) if live => leased,
        _ => heard,
    };
    Session {
        last_completed_turn: row
            .ended
            .filter(|ended| ended.answered)
            .map(|ended| ended.turn.0.to_string()),
        chat_id: row.id.to_string(),
        device_id: device.to_owned(),
        status,
        // A turn's time runs from when it began, through its approvals and waits.
        started_at: (status == SessionStatus::Working)
            .then(|| time(row.began_ms.unwrap_or(row.status.since_ms))),
        updated_at,
        running_subagents: running,
        status_since: (!row.status.since_ms.is_zero()).then(|| time(row.status.since_ms)),
        waiting_on: waiting_on(row),
    }
}

/// Whether a thread's row says it is at work.
#[must_use]
pub const fn running(row: &ThreadRow) -> bool {
    matches!(row.status.phase, Phase::Working | Phase::Waiting)
}

/// Whether `row` is a subagent hung off a call of an agent that has exited (`exited`): it ran
/// inside that agent, so it ran no further, whatever its own row last said.
#[must_use]
pub fn outlived(row: &ThreadRow, exited: &BTreeSet<ThreadId>) -> bool {
    row.parent.as_ref().is_some_and(|link| exited.contains(&link.thread))
}

/// `row` as it stands: a subagent its agent [`outlived`] stopped with it.
#[must_use]
pub fn standing<'a>(row: &'a ThreadRow, exited: &BTreeSet<ThreadId>) -> Cow<'a, ThreadRow> {
    if running(row) && outlived(row, exited) {
        let mut stopped = row.clone();
        stopped.status.phase = Phase::Stopped;
        Cow::Owned(stopped)
    } else {
        Cow::Borrowed(row)
    }
}

/// The subagents of `parent` that run, among `rows`, none of an agent that has exited
/// (`exited`).
#[must_use]
pub fn running_children<'a>(
    parent: ThreadId,
    rows: impl Iterator<Item = &'a ThreadRow>,
    exited: &BTreeSet<ThreadId>,
) -> u32 {
    let count = rows
        .filter(|row| self::parent(row) == Some(parent))
        .filter(|row| running(row) && !outlived(row, exited))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}
