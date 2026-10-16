//! The Changes surface: a thread's diff, the working tree's live and the branch's or the latest
//! turn's on asking, cut by the worker's own review ([`GitOp::Changes`], [`ReviewScope`]) and
//! written as the unified patch the shell reads, with each file's two sides whole for its
//! highlighting. A working tree is diffed again only when the worker says its repository moved
//! (`WorkerMsg::RepoMoved`), or the link came back.

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::FutureExt as _;
use gpui::{AsyncApp, Context, Task, WeakEntity};
use slopty_client::WorkerKey;
use slopty_proto::git::{GitDone, GitOp, GitOutcome};
use slopty_proto::thread::ThreadId;
use slopty_proto::thread::wire::{Against, FileDiff, FileKind, Review, ReviewScope};
use slopty_view_proto::{CheckoutDiff, CheckoutFileDiffText, DiffFileSummary};
use slopty_view_rpc::{RpcError, RpcReply};

use super::{ask, param, unanswered, value, watched, within};
use crate::workspace::WorkspaceView;

/// How often a diff watch looks at which chats it follows, to diff a chat's folder once it is
/// followed. A look at the workspace's own state, never a question to a worker.
const PLACES_EVERY: Duration = Duration::from_secs(1);

/// The most files a diff fetches whole when the review clipped their hunks.
const WHOLE_FILES: usize = 16;

/// How many diffs' file sides are kept for their highlighting.
const KEPT_DIFFS: usize = 32;

/// How often a review asked of a thread is looked for.
const REVIEW_POLL: Duration = Duration::from_millis(100);

/// The blobs each file of a recent diff was between, by the diff's checksum.
#[derive(Default)]
pub(super) struct Kept {
    sides: VecDeque<(String, Sides)>,
}

/// Where a diff is, and its files' blobs on the old and new side, by path.
#[derive(Clone)]
struct Sides {
    key: WorkerKey,
    cwd: String,
    blobs: HashMap<String, (Option<String>, Option<String>)>,
}

impl Kept {
    fn keep(&mut self, checksum: String, sides: Sides) {
        if self.sides.iter().any(|(sum, _)| *sum == checksum) {
            return;
        }
        if self.sides.len() >= KEPT_DIFFS {
            self.sides.pop_front();
        }
        self.sides.push_back((checksum, sides));
    }

    fn get(&self, checksum: &str) -> Option<&Sides> {
        self.sides.iter().find(|(sum, _)| sum == checksum).map(|(_, sides)| sides)
    }
}

/// A diff as the shell reads it, with the file sides to keep.
struct Written {
    diff: CheckoutDiff,
    sides: Sides,
}

/// `review` of the folder `cwd` on `key` as the shell's diff.
fn written(key: WorkerKey, cwd: &str, review: &Review) -> Written {
    let device = key.to_string();
    let mut patch = String::new();
    let mut files = Vec::with_capacity(review.files.len());
    let mut blobs = HashMap::with_capacity(review.files.len());
    let (mut additions, mut deletions) = (0_u32, 0_u32);
    let mut truncated = false;
    for file in &review.files {
        write_file(&mut patch, file);
        truncated |= file.patch.clipped_lines > 0;
        additions = additions.saturating_add(file.patch.added);
        deletions = deletions.saturating_add(file.patch.removed);
        files.push(summary(file));
        blobs.insert(file.path.clone(), (file.from.clone(), file.to.clone()));
    }
    let mut hasher = DefaultHasher::new();
    patch.hash(&mut hasher);
    review.to.as_ref().map(|tree| &tree.0).hash(&mut hasher);
    let checksum = format!("{:016x}", hasher.finish());
    let diff = CheckoutDiff {
        checkout_id: checkout_id(&device, cwd),
        device_id: device,
        cwd: cwd.to_owned(),
        patch,
        files,
        additions,
        deletions,
        truncated,
        checksum,
        updated_at: Utc::now(),
    };
    Written { diff, sides: Sides { key, cwd: cwd.to_owned(), blobs } }
}

/// The id of the checkout at `cwd` on `device`.
fn checkout_id(device: &str, cwd: &str) -> String {
    format!("{device}:{cwd}")
}

/// A file of a review as the shell's summary row.
fn summary(file: &FileDiff) -> DiffFileSummary {
    let status = match (&file.from, &file.to) {
        (None, Some(_)) => "added",
        (Some(_), None) => "deleted",
        _ if file.old_path.is_some() => "renamed",
        _ => "modified",
    };
    DiffFileSummary {
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        status: status.to_owned(),
        additions: file.patch.added,
        deletions: file.patch.removed,
        binary: !file.is_text(),
    }
}

/// A file of a review, as `git diff` writes it.
fn write_file(patch: &mut String, file: &FileDiff) {
    use std::fmt::Write as _;
    let old = file.old_path.as_deref().unwrap_or(&file.path);
    writeln!(patch, "diff --git a/{old} b/{}", file.path).unwrap_or_default();
    let mode = |side: fn(&slopty_proto::thread::wire::Modes) -> u32| {
        file.modes
            .as_ref()
            .map_or_else(|| "100644".to_owned(), |modes| format!("{:o}", side(modes)))
    };
    if file.from.is_none() {
        writeln!(patch, "new file mode {}", mode(|m| m.to)).unwrap_or_default();
    } else if file.to.is_none() {
        writeln!(patch, "deleted file mode {}", mode(|m| m.from)).unwrap_or_default();
    } else if let Some(modes) = file.modes {
        writeln!(patch, "old mode {:o}\nnew mode {:o}", modes.from, modes.to).unwrap_or_default();
    }
    if let Some(old_path) = &file.old_path {
        writeln!(patch, "rename from {old_path}\nrename to {}", file.path).unwrap_or_default();
    }
    let old_name = if file.from.is_some() { format!("a/{old}") } else { "/dev/null".to_owned() };
    let new_name =
        if file.to.is_some() { format!("b/{}", file.path) } else { "/dev/null".to_owned() };
    if !matches!(file.kind, FileKind::Text) {
        writeln!(patch, "Binary files {old_name} and {new_name} differ").unwrap_or_default();
        return;
    }
    if file.patch.hunks.is_empty() {
        return;
    }
    writeln!(patch, "--- {old_name}\n+++ {new_name}").unwrap_or_default();
    for hunk in &file.patch.hunks {
        write!(
            patch,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        )
        .unwrap_or_default();
        if let Some(heading) = &hunk.heading {
            write!(patch, " {heading}").unwrap_or_default();
        }
        patch.push('\n');
        for line in &hunk.lines {
            patch.push_str(line);
            patch.push('\n');
        }
    }
}

/// The diff a git op answered, or why there is none.
fn review_of(outcome: GitOutcome) -> Result<Review, RpcError> {
    match outcome {
        GitOutcome::Done(GitDone::Changes(review)) => Ok(*review),
        GitOutcome::Done(_) => Err(RpcError::Failed("the machine answered something else".into())),
        GitOutcome::Refused { why } | GitOutcome::Unavailable { why, .. } => {
            Err(RpcError::Failed(why))
        }
        GitOutcome::Failed { said } => Err(RpcError::Failed(said)),
    }
}

/// Ask `op` of `cwd`'s repository on `key`, fetch whole the clipped files, and write the diff.
async fn diff_of(
    this: &WeakEntity<WorkspaceView>,
    cx: &mut AsyncApp,
    key: WorkerKey,
    cwd: String,
    op: GitOp,
) -> Result<CheckoutDiff, RpcError> {
    let ignore_whitespace = matches!(
        op,
        GitOp::Changes { ignore_whitespace: true, .. }
            | GitOp::Turn { ignore_whitespace: true, .. }
    );
    let repo = cwd.clone();
    let mut review = review_of(ask(this, cx, |v| v.surface_git(key, repo, op)).await?)?;
    whole(this, cx, key, &cwd, &mut review, ignore_whitespace).await;
    finish(this, cx, key, &cwd, &review)
}

/// Write `review` and keep its sides.
fn finish(
    this: &WeakEntity<WorkspaceView>,
    cx: &mut AsyncApp,
    key: WorkerKey,
    cwd: &str,
    review: &Review,
) -> Result<CheckoutDiff, RpcError> {
    let Written { diff, sides } = written(key, cwd, review);
    let checksum = diff.checksum.clone();
    this.update(cx, |view, _| view.view_model.surfaces.changes.keep(checksum, sides))
        .map_err(super::gone)?;
    Ok(diff)
}

/// Fetch whole the files whose hunks the review clipped, a few at most, whitespace counting or
/// not as the review was asked.
async fn whole(
    this: &WeakEntity<WorkspaceView>,
    cx: &mut AsyncApp,
    key: WorkerKey,
    cwd: &str,
    review: &mut Review,
    ignore_whitespace: bool,
) {
    let clipped = review.files.iter_mut().filter(|file| file.patch.clipped_lines > 0);
    for file in clipped.take(WHOLE_FILES) {
        let (from, to) = (file.from.clone(), file.to.clone());
        let op = GitOp::FileDiff { from, to, ignore_whitespace };
        let repo = cwd.to_owned();
        if let Ok(GitOutcome::Done(GitDone::FileDiff { patch, .. })) =
            ask(this, cx, |v| v.surface_git(key, repo, op)).await
        {
            file.patch = *patch;
        }
    }
}

/// The turn of `state` whose message the transcript shows as the user entry `entry`: the id the
/// shell gave the message where it sent it (`named`, by its intent), else the item's own.
fn turn_of_entry(
    state: &slopty_proto::thread::ThreadState,
    entry: &str,
    named: &HashMap<slopty_proto::thread::IntentId, String>,
) -> Option<slopty_proto::thread::TurnId> {
    state
        .items
        .iter()
        .find(|item| {
            let slopty_proto::thread::ItemBody::User(message) = &item.body else { return false };
            let shown = message.intent.and_then(|intent| named.get(&intent));
            shown.map_or(item.id.0.as_str(), String::as_str) == entry
        })
        .map(|item| item.turn)
}

/// One blob's text, `None` when it is not text or could not be read.
async fn blob_text(
    this: &WeakEntity<WorkspaceView>,
    cx: &mut AsyncApp,
    key: WorkerKey,
    cwd: &str,
    blob: Option<String>,
) -> (Option<String>, bool) {
    let Some(blob) = blob else { return (None, false) };
    let repo = cwd.to_owned();
    match ask(this, cx, |v| v.surface_git(key, repo, GitOp::Blob { blob })).await {
        Ok(GitOutcome::Done(GitDone::Blob { bytes, .. })) => match String::from_utf8(bytes) {
            Ok(text) => (Some(text), false),
            Err(_) => (None, true),
        },
        _ => (None, false),
    }
}

impl WorkspaceView {
    /// The folders whose working trees a diff watch on `device` follows: those of the chats
    /// the interface follows there, each once, in order.
    fn diff_places(&self, device: Option<WorkerKey>, cx: &gpui::App) -> Vec<(WorkerKey, String)> {
        let mut places: Vec<(WorkerKey, String)> = Vec::new();
        for (key, thread) in self.view_model.followed.values() {
            if device.is_some_and(|device| device != *key) {
                continue;
            }
            let cwd = self.held_hub(*key).and_then(|hub| {
                hub.read(cx).threads().rows().rows.get(thread).and_then(|row| row.cwd.clone())
            });
            if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
                places.push((*key, cwd));
            }
        }
        places.sort();
        places.dedup();
        places
    }

    /// `WatchCheckoutDiffs`: the working trees of the chats followed on the device named,
    /// diffed against `HEAD` now, again when the worker says one's repository moved and when a
    /// chat is followed, each list sent when it changed.
    pub(super) fn watch_diffs(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let device = self.surface_device(params);
        let (tx, reply, mut gone) = watched();
        let id = self.view_model.surfaces.next_watch();
        let bell = Rc::new(tokio::sync::Notify::new());
        cx.spawn(async move |this, cx| {
            let mut last: Option<Vec<(String, String)>> = None;
            let mut followed: Option<Vec<(WorkerKey, String)>> = None;
            'watch: loop {
                let Ok(places) = this.update(cx, |view, cx| view.diff_places(device, cx)) else {
                    return;
                };
                if followed.as_ref() != Some(&places) {
                    let follow = |view: &mut Self| view.follow_repos(id, &bell, &places);
                    if this.update(cx, |view, _| follow(view)).is_err() {
                        return;
                    }
                    followed = Some(places.clone());
                }
                let mut diffs = Vec::with_capacity(places.len());
                let mut unanswered = false;
                for (key, cwd) in places {
                    let op = GitOp::Changes { against: Against::Head, ignore_whitespace: false };
                    // The shell letting the watch go ends it at once, a diff still out or not.
                    let mut asked = std::pin::pin!(diff_of(&this, cx, key, cwd.clone(), op).fuse());
                    let answer = futures::select_biased! {
                        _ = gone => break 'watch,
                        answer = asked => answer,
                    };
                    match answer {
                        Ok(diff) => diffs.push(diff),
                        Err(RpcError::Closed) => return,
                        Err(error) if super::is_unanswered(&error) => unanswered = true,
                        Err(error) => tracing::debug!(%error, %cwd, "a working tree's diff failed"),
                    }
                }
                let sums: Vec<_> =
                    diffs.iter().map(|d| (d.checkout_id.clone(), d.checksum.clone())).collect();
                // A worker that did not answer, its link down: what the shell shows stands.
                if !unanswered && last.as_ref() != Some(&sums) {
                    last = Some(sums);
                    let sent =
                        serde_json::to_value(&diffs).is_ok_and(|f| tx.unbounded_send(f).is_ok());
                    if !sent {
                        break;
                    }
                }
                loop {
                    let look = cx.background_executor().timer(PLACES_EVERY);
                    futures::select_biased! {
                        _ = gone => break 'watch,
                        () = bell.notified().fuse() => continue 'watch,
                        () = look.fuse() => {}
                    }
                    let Ok(now) = this.update(cx, |view, cx| view.diff_places(device, cx)) else {
                        return;
                    };
                    if followed.as_ref() != Some(&now) {
                        continue 'watch;
                    }
                }
            }
            let _closing = this.update(cx, |view, _| view.unfollow_repos(id));
        })
        .detach();
        Task::ready(Ok(reply))
    }

    /// `GetCheckoutDiff`: the branch's changes against a base, or the latest turn's.
    pub(super) fn checkout_diff(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let chat = param(params, "chatId").map(str::to_owned);
        let place = chat.as_deref().and_then(|chat| self.chat_place(chat, cx));
        let Some(key) = self.surface_device(params).or_else(|| place.as_ref().map(|(key, _)| *key))
        else {
            return Task::ready(Err(RpcError::BadParams("no machine for this diff".into())));
        };
        let Some(cwd) =
            param(params, "cwd").map(str::to_owned).or_else(|| place.map(|(_, cwd)| cwd))
        else {
            return Task::ready(Err(RpcError::BadParams("cwd is required".into())));
        };
        let mode = param(params, "mode").unwrap_or("workingTree").to_owned();
        let base = param(params, "baseRef").map(str::to_owned);
        // `git diff -w`: the worker reads lines with their spaces taken out.
        let ignore_whitespace =
            params.get("ignoreWhitespace").and_then(serde_json::Value::as_bool).unwrap_or(false);
        if mode == "turn" {
            let thread = chat.as_deref().and_then(|chat| self.chat_thread_in(chat, cx));
            let Some((key, thread)) = thread else {
                return Task::ready(Err(RpcError::Failed("this chat has no turns yet".into())));
            };
            // The turn the person's message `turn` began (a user entry's id), else the latest.
            let entry = param(params, "turn").map(str::to_owned);
            return self.turn_diff(key, thread, cwd, (entry, ignore_whitespace), cx);
        }
        let against = match (mode.as_str(), base) {
            ("workingTree", _) => Against::Head,
            ("branch", Some(base)) => Against::Branch(base),
            ("branch", None) => Against::Base,
            _ => return Task::ready(Err(RpcError::Failed(format!("no {mode} diff in Slopty")))),
        };
        cx.spawn(async move |this, cx| {
            let op = GitOp::Changes { against, ignore_whitespace };
            let diff = diff_of(&this, cx, key, cwd, op).await?;
            value(&diff)
        })
    }

    /// One turn's changes in `thread`: the turn the person's message `entry` (a user entry's id)
    /// began, else the latest. Between the snapshots the worker took of it, whitespace counting
    /// or not; a turn with no snapshot is asked of the thread's own review, which says why.
    fn turn_diff(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        cwd: String,
        (entry, ignore_whitespace): (Option<String>, bool),
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let hub = self.thread_hub(key, cx);
        let named = &self.view_model.named;
        let picked = hub
            .read(cx)
            .threads()
            .mirror(thread)
            .and_then(slopty_client::threads::Mirror::state)
            .and_then(|state| {
                let turn = match &entry {
                    Some(entry) => turn_of_entry(state, entry, named)?,
                    None => state.turns.last()?.id,
                };
                let turn = state.turns.iter().find(|t| t.id == turn)?;
                Some((turn.id, turn.before.clone(), turn.after.clone()))
            });
        let Some((turn, before, after)) = picked else {
            let why = if entry.is_some() {
                "that turn is not in this chat"
            } else {
                "this chat has no turns yet"
            };
            return Task::ready(Err(RpcError::Failed(why.into())));
        };
        if let Some(before) = before {
            let op = GitOp::Turn { turn, before, after, ignore_whitespace };
            return cx.spawn(async move |this, cx| value(&diff_of(&this, cx, key, cwd, op).await?));
        }
        let scope = ReviewScope::Turn(turn);
        let before = hub.read(cx).review(thread, &scope).cloned();
        hub.update(cx, |hub, cx| hub.ask_review(thread, scope.clone(), cx));
        cx.spawn(async move |this, cx| {
            let deadline = std::time::Instant::now().checked_add(super::ANSWER_WITHIN);
            loop {
                let now = hub.read_with(cx, |hub, _| hub.review(thread, &scope).cloned());
                if let Some(review) =
                    now.filter(|now| before.as_ref().is_none_or(|before| !Arc::ptr_eq(before, now)))
                {
                    let mut review = Review::clone(&review);
                    whole(&this, cx, key, &cwd, &mut review, false).await;
                    return value(&finish(&this, cx, key, &cwd, &review)?);
                }
                if deadline.is_none_or(|deadline| std::time::Instant::now() > deadline) {
                    return Err(unanswered());
                }
                cx.background_executor().timer(REVIEW_POLL).await;
            }
        })
    }

    /// `GetCheckoutFileDiffText`: a diffed file's two sides whole, by the blobs its diff named.
    pub(super) fn file_diff_text(
        &self,
        params: serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let request: slopty_view_proto::GetCheckoutFileDiffTextRequest =
            match serde_json::from_value(params) {
                Ok(request) => request,
                Err(e) => return Task::ready(Err(RpcError::BadParams(e.to_string()))),
            };
        let kept = self.view_model.surfaces.changes.get(&request.diff_checksum).cloned();
        let Some(sides) = kept else {
            return Task::ready(value(&CheckoutFileDiffText {
                diff_checksum: request.diff_checksum,
                old_text: None,
                new_text: None,
                old_content_hash: None,
                new_content_hash: None,
                binary: false,
                truncated: false,
                stale: true,
            }));
        };
        let (from, to) = sides.blobs.get(&request.path).cloned().unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let (old_text, old_binary) =
                blob_text(&this, cx, sides.key, &sides.cwd, from.clone()).await;
            let (new_text, new_binary) =
                blob_text(&this, cx, sides.key, &sides.cwd, to.clone()).await;
            value(&CheckoutFileDiffText {
                diff_checksum: request.diff_checksum,
                old_text,
                new_text,
                old_content_hash: from,
                new_content_hash: to,
                binary: old_binary || new_binary,
                truncated: false,
                stale: false,
            })
        })
    }

    /// `ListBranches`: the repository's branches, its default first.
    pub(super) fn list_branches(
        &mut self,
        params: &serde_json::Value,
        cx: &Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let (Some(key), Some(repo)) =
            (self.surface_device(params), param(params, "repoPath").map(str::to_owned))
        else {
            return Task::ready(Err(RpcError::BadParams("a machine and repoPath".into())));
        };
        let rx = self.surface_git(key, repo, GitOp::Branches);
        cx.spawn(async move |_, cx| match within(cx, rx).await? {
            GitOutcome::Done(GitDone::Branches(branches)) => {
                let mut names: Vec<String> = branches.default.into_iter().collect();
                for branch in branches.list {
                    if !names.contains(&branch.name) {
                        names.push(branch.name);
                    }
                }
                value(&names)
            }
            other => review_of(other).map(|_| RpcReply::Value(serde_json::json!([]))),
        })
    }
}

#[cfg(test)]
mod tests;
