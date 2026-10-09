//! Starts that outlive the connection that asked for them (`docs/decisions/agents.md`, "A start
//! outlives its link").
//!
//! A start (`ThreadRequest::Start`) may set a worktree up for minutes before its agent runs. It
//! runs on the daemon's runtime, keyed by its intent id, not on the asking connection's tasks:
//! a Mac that sleeps or a link that blips no longer kills it halfway. A connection only follows
//! it ([`follow`]): what its setup says, then its outcome. The same start asked again, as a
//! client does when its link comes back, follows the one under way, or gets the outcome of one
//! finished in the last [`KEPT_AFTER`]; a thread begun is answered from the host's own record
//! of starts after that.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use slopty_net::WorkerMsg;
use slopty_proto::thread::IntentId;
use slopty_proto::thread::wire::{IntentDone, Outcome, Setup};
use tokio::sync::{mpsc, watch};

/// How long a finished start's outcome waits for a client that asked for it and lost its link.
pub const KEPT_AFTER: Duration = Duration::from_mins(10);

/// What a start under way says: its setup's latest word, and its outcome once it has one.
#[derive(Clone, Debug)]
pub struct Followed {
    setup: watch::Receiver<Option<Setup>>,
    done: watch::Receiver<Option<Outcome>>,
}

/// The starts under way on this daemon, and those finished in the last [`KEPT_AFTER`], by
/// intent. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct Starts(Arc<parking_lot::Mutex<HashMap<IntentId, Followed>>>);

impl Starts {
    /// The start of intent `id`: the one under way or lately finished, else a new one, `run`
    /// on the daemon's runtime with where to say its setup.
    pub fn take_up<F>(
        &self,
        id: IntentId,
        run: impl FnOnce(watch::Sender<Option<Setup>>) -> F,
    ) -> Followed
    where
        F: Future<Output = Outcome> + Send + 'static,
    {
        let mut starts = self.0.lock();
        if let Some(followed) = starts.get(&id) {
            return followed.clone();
        }
        let (setup, setup_rx) = watch::channel(None);
        let (done, done_rx) = watch::channel(None);
        let followed = Followed { setup: setup_rx, done: done_rx };
        starts.insert(id, followed.clone());
        drop(starts);
        let running = run(setup);
        let all = Arc::clone(&self.0);
        drop(tokio::spawn(async move {
            let outcome = running.await;
            done.send_replace(Some(outcome));
            tokio::time::sleep(KEPT_AFTER).await;
            all.lock().remove(&id);
        }));
        followed
    }
}

/// Tell `out` what start `id` says as it goes: its setup's words (the latest at once, each one
/// after; one lost to a full link is drawn over by the next), then its outcome.
pub async fn follow(id: IntentId, followed: Followed, out: mpsc::Sender<WorkerMsg>) {
    let Followed { mut setup, mut done } = followed;
    setup.mark_changed();
    done.mark_changed();
    // The setup's sender goes with the start: what it said last is told before the outcome.
    let mut saying = true;
    loop {
        tokio::select! {
            biased;
            changed = setup.changed(), if saying => {
                if changed.is_err() {
                    saying = false;
                    continue;
                }
                let said = setup.borrow_and_update().clone();
                if let Some(said) = said {
                    let _full = out.try_send(WorkerMsg::SettingUp { id, setup: said });
                }
            }
            changed = done.changed() => {
                let outcome = match changed {
                    Ok(()) => done.borrow_and_update().clone(),
                    Err(_) => Some(super::refused("the start stopped before it finished".to_owned())),
                };
                if let Some(outcome) = outcome {
                    let _gone = out.send(WorkerMsg::IntentDone(IntentDone { id, outcome })).await;
                    return;
                }
            }
        }
    }
}
