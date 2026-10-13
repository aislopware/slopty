//! What the person wrote and has not sent, kept on this device so a quit, a crash or iOS ending
//! a suspended app loses none of it: a thread's composer and a start's first message.
//!
//! A thread's composer is also the thread's draft on its worker, so a message begun on one
//! device is there on the next (`conversation::thread::view::sharing`). A composer opens with
//! whichever was kept last, this device's words or the worker's.
//!
//! One store, one file (`drafts.json` beside the layout), replaced whole by
//! `slopty_platform::fs::replace`, so a crash mid-write leaves the one before. It holds every
//! draft by what it is about, not by the view that held it: a thread's by the thread (in its
//! terminal's tile, its own tile or neither), a start's by its machine, agent and folder (a
//! start tile is not kept across a launch, so the next start there takes the words back). A view
//! hands its words over as it goes, and a pass [`DRAFTS_PAUSE`] after the last change reads every
//! live one and writes the file; the app asks for a pass at once as it goes to the background or
//! quits. A sent message and a start that went leave nothing. What was kept more than [`KEPT_FOR`]
//! ago goes when the file is read.
//!
//! The file is the user's alone (0600 in a 0700 directory): a draft may hold a secret.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, Task};
use serde::{Deserialize, Serialize};
use slopty_client::layout::WorkerKey;
use slopty_core::WallMs;
use slopty_proto::thread::wire::Draft;
use slopty_proto::thread::{AgentId, ThreadId};

use super::WorkspaceView;

/// How long after the last change a pass writes the drafts.
pub const DRAFTS_PAUSE: Duration = Duration::from_millis(400);

/// How long a draft nobody came back to is kept.
const KEPT_FOR: Duration = Duration::from_hours(30 * 24);

/// A thread's composer, as it was left.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct ThreadDraft {
    thread: String,
    text: String,
    kept_ms: WallMs,
}

/// A start's first message, by where it starts.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct StartDraft {
    worker: String,
    agent: String,
    cwd: String,
    text: String,
    kept_ms: WallMs,
}

/// The file's contents.
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct Held {
    threads: Vec<ThreadDraft>,
    starts: Vec<StartDraft>,
}

/// The drafts, as the workspace holds them.
#[derive(Default)]
pub(super) struct Drafts {
    path: Option<PathBuf>,
    held: Held,
    /// A pass waits out [`DRAFTS_PAUSE`].
    pass: Option<Task<()>>,
}

impl Drafts {
    /// `thread`'s composer as it was left here, or as `row` (its worker's draft, kept by any
    /// device) has it: whichever was kept last, with when this device kept it when the words
    /// are its own.
    pub(super) fn thread(
        &self,
        thread: ThreadId,
        row: Option<&Draft>,
    ) -> Option<(String, Option<WallMs>)> {
        let key = thread.to_string();
        let here = self.held.threads.iter().find(|d| d.thread == key);
        match (here, row) {
            (Some(here), Some(row)) if row.at_ms > here.kept_ms => Some((row.text.clone(), None)),
            (Some(here), _) => Some((here.text.clone(), Some(here.kept_ms))),
            (None, row) => row.map(|r| (r.text.clone(), None)),
        }
    }

    /// Keep `text` as `thread`'s composer; nothing for an empty one.
    pub(super) fn set_thread(&mut self, thread: ThreadId, text: &str, now: WallMs) {
        let key = thread.to_string();
        self.held.threads.retain(|d| d.thread != key);
        if !text.trim().is_empty() {
            self.held.threads.push(ThreadDraft {
                thread: key,
                text: text.to_owned(),
                kept_ms: now,
            });
        }
    }

    /// The first message of a start of `agent` on `worker` in `cwd`, as it was left.
    pub(super) fn start(&self, worker: WorkerKey, agent: &AgentId, cwd: &str) -> Option<&str> {
        let worker = worker.to_string();
        self.held
            .starts
            .iter()
            .find(|d| d.worker == worker && d.agent == agent.0 && d.cwd == cwd)
            .map(|d| d.text.as_str())
    }

    /// Keep `text` as the first message of a start of `agent` on `worker` in `cwd`; an empty
    /// one lets it go.
    pub(super) fn set_start(
        &mut self,
        (worker, agent, cwd): (WorkerKey, &AgentId, &str),
        text: &str,
        now: WallMs,
    ) {
        let worker = worker.to_string();
        self.held.starts.retain(|d| !(d.worker == worker && d.agent == agent.0 && d.cwd == cwd));
        if !text.trim().is_empty() {
            self.held.starts.push(StartDraft {
                worker,
                agent: agent.0.clone(),
                cwd: cwd.to_owned(),
                text: text.to_owned(),
                kept_ms: now,
            });
        }
    }

    /// Read what `path` holds, all but what is older than [`KEPT_FOR`]; nothing when it is
    /// absent or unreadable, which only loses what it held.
    fn read(path: &Path, now: WallMs) -> Held {
        let Ok(bytes) = std::fs::read(path) else { return Held::default() };
        let mut held: Held = match serde_json::from_slice(&bytes) {
            Ok(held) => held,
            Err(e) => {
                tracing::warn!(%e, path = %path.display(), "drafts unreadable: set aside");
                return Held::default();
            }
        };
        let fresh =
            |kept: WallMs| u128::from(now.millis_since(kept)) < KEPT_FOR.as_millis() || kept > now;
        held.threads.retain(|d| fresh(d.kept_ms));
        held.starts.retain(|d| fresh(d.kept_ms));
        held
    }
}

/// Write `bytes` to `path`, whole or not at all, the user's alone.
fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let new = !path.exists();
    slopty_platform::fs::replace(path, bytes)?;
    if new {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

impl WorkspaceView {
    /// Keep the drafts in `path`, and take back what it holds.
    pub fn set_drafts_path(&mut self, path: PathBuf, cx: &Context<Self>) {
        self.drafts.held = Drafts::read(&path, slopty_kit::clock::now(cx));
        self.drafts.path = Some(path);
    }

    /// A draft changed: a pass writes them all once [`DRAFTS_PAUSE`] has gone by with no
    /// pass already waiting.
    pub(super) fn drafts_changed(&mut self, cx: &Context<Self>) {
        if self.drafts.path.is_none() || self.drafts.pass.is_some() {
            return;
        }
        self.drafts.pass = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFTS_PAUSE).await;
            let _gone = this.update(cx, |this, cx| {
                this.drafts.pass = None;
                this.keep_drafts(false, cx);
            });
        }));
    }

    /// Write every draft now, as the app goes to the background or quits: on this thread,
    /// so it is on the disk before the app can be ended.
    pub fn keep_drafts_now(&mut self, cx: &mut Context<Self>) {
        self.drafts.pass = None;
        self.keep_drafts(true, cx);
        // The worker holds the words for the next device: they go now, not after the pause.
        let views: Vec<_> = self
            .thread_faces()
            .map(|(_, v)| v.clone())
            .chain(self.thread_items().map(|(_, v)| v.clone()))
            .collect();
        for view in views {
            view.update(cx, crate::conversation::thread::ThreadView::share_draft);
        }
    }

    /// `thread`'s words to open its composer with, on `key`'s worker: this device's, or its
    /// worker's draft where that was kept since.
    pub(super) fn opening_draft(
        &self,
        key: WorkerKey,
        thread: ThreadId,
        cx: &Context<Self>,
    ) -> Option<(String, Option<WallMs>)> {
        let row = self.held_hub(key).and_then(|hub| {
            hub.read(cx).threads().rows().rows.get(&thread).and_then(|r| r.draft.clone())
        });
        self.drafts.thread(thread, row.as_ref())
    }

    /// Read every live draft into the store and write it, `now` on this thread, else off it.
    fn keep_drafts(&mut self, now: bool, cx: &Context<Self>) {
        self.gather_drafts(cx);
        let Some(path) = self.drafts.path.clone() else { return };
        let bytes = match serde_json::to_vec(&self.drafts.held) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(%e, "drafts not serialised");
                return;
            }
        };
        let written = move || {
            if let Err(e) = write(&path, &bytes) {
                tracing::warn!(%e, path = %path.display(), "drafts not kept");
            }
        };
        if now {
            written();
        } else {
            cx.background_executor().spawn(async move { written() }).detach();
        }
    }

    /// Read what every live composer holds into the store.
    fn gather_drafts(&mut self, cx: &Context<Self>) {
        let now = slopty_kit::clock::now(cx);
        let mut threads: Vec<(ThreadId, String)> = Vec::new();
        for view in self.thread_faces().map(|(_, v)| v).chain(self.thread_items().map(|(_, v)| v)) {
            let view = view.read(cx);
            threads.push((view.thread(), view.draft(cx)));
        }
        // Two views of one thread: what either holds stands over an empty composer.
        threads.sort_by_key(|(_, text)| !text.trim().is_empty());
        for (thread, text) in threads {
            self.drafts.set_thread(thread, &text, now);
        }
        for (worker, agent, cwd, text) in self.starting.drafts(cx) {
            self.drafts.set_start((worker, &agent, &cwd), &text, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::{AgentId, ThreadId};

    use super::{Draft, Drafts, Held, KEPT_FOR, write};

    /// A thread's and a start's drafts go to the file and come back; an empty one leaves
    /// nothing, and one kept too long ago is let go as the file is read.
    #[test]
    fn drafts_come_back_and_old_ones_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.json");
        let now = WallMs::from_millis(KEPT_FOR.as_millis().try_into().unwrap());
        let thread = ThreadId::new();
        let worker = slopty_client::layout::WorkerKey::new(7);
        let agent = AgentId::named(AgentId::CLAUDE_CODE);
        let mut drafts = Drafts::default();
        drafts.set_thread(thread, "Half a thought", now);
        drafts.set_start((worker, &agent, "~/code"), "Fix the build", now);
        drafts.set_thread(ThreadId::new(), "  ", now);
        assert_eq!(drafts.held.threads.len(), 1, "an empty composer leaves nothing");
        write(&path, &serde_json::to_vec(&drafts.held).unwrap()).unwrap();

        let back = Drafts { held: Drafts::read(&path, now), ..Drafts::default() };
        assert_eq!(back.thread(thread, None).map(|(t, _)| t).as_deref(), Some("Half a thought"));
        let newer = Draft {
            text: "From the phone".to_owned(),
            at_ms: WallMs::from_millis(now.as_millis() + 1),
        };
        assert_eq!(
            back.thread(thread, Some(&newer)).map(|(t, _)| t).as_deref(),
            Some("From the phone"),
            "kept since"
        );
        let older = Draft { text: "Old".to_owned(), at_ms: WallMs::ZERO };
        assert_eq!(
            back.thread(thread, Some(&older)).map(|(t, _)| t).as_deref(),
            Some("Half a thought")
        );
        assert_eq!(
            back.thread(ThreadId::new(), Some(&older)).map(|(t, _)| t).as_deref(),
            Some("Old"),
            "none here"
        );
        assert_eq!(back.start(worker, &agent, "~/code"), Some("Fix the build"));
        let later = WallMs::from_millis(now.as_millis().saturating_mul(2).saturating_add(1));
        assert_eq!(Drafts::read(&path, later), Held::default(), "kept too long ago");
        let mode = std::fs::metadata(&path).map(|m| {
            use std::os::unix::fs::PermissionsExt as _;
            m.permissions().mode() & 0o777
        });
        assert_eq!(mode.unwrap(), 0o600, "the user's alone");
    }
}
