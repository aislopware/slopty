//! Verbs done once per [`IdempotencyKey`] (`docs/decisions/topology.md`).
//!
//! The worker does the verbs, so it keeps the table: a repeat under a key answers what the
//! first did, whether the server restarted or the link to it dropped in between. The first
//! runs on a task of its own, so it finishes and its answer is kept even when the caller that
//! asked is gone; a repeat that arrives meanwhile waits for that answer.
//!
//! The table holds at most `capacity` keys, each for [`KEY_LIFETIME`] after its answer.
//! When full, the key answered longest ago goes first. A key still running is never dropped:
//! when every one is, a new key is refused rather than left unguarded.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use slopty_proto::codec;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, KEY_LIFETIME, Outcome, Verb};
use tokio::sync::watch;
use tokio::time::Instant;

/// Keys a worker keeps. Each costs its key, a digest and a small answer; a worker driven hard
/// by agents does a verb or two a second, a few thousand in [`KEY_LIFETIME`].
pub const KEYS_KEPT: usize = 4096;

/// Keyed verbs and their answers. Cheap to clone; every clone shares the table.
#[derive(Clone, Debug)]
pub struct Ledger {
    table: Arc<Mutex<Table>>,
    capacity: usize,
    lifetime: Duration,
}

#[derive(Debug, Default)]
struct Table {
    slots: HashMap<IdempotencyKey, Slot>,
}

#[derive(Debug)]
struct Slot {
    /// The verb's arguments, hashed: a repeat must match them.
    digest: blake3::Hash,
    /// `None` until the verb is answered.
    answer: watch::Receiver<Option<Outcome>>,
    /// When the answer came.
    answered: Option<Instant>,
}

impl Slot {
    /// Past its lifetime, or its task ended without an answer.
    fn lapsed(&self, now: Instant, lifetime: Duration) -> bool {
        match self.answered {
            Some(at) => now.duration_since(at) >= lifetime,
            None => self.answer.has_changed().is_err(),
        }
    }
}

/// What a key finds in the table.
enum Claim {
    /// Nobody has it: do the verb.
    Run(watch::Sender<Option<Outcome>>, watch::Receiver<Option<Outcome>>),
    /// The same verb has it: its answer, now or when it comes.
    Wait(watch::Receiver<Option<Outcome>>),
    /// Another verb has it.
    Reused,
    /// Every slot is taken by a verb still running.
    Full,
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new(KEYS_KEPT, KEY_LIFETIME)
    }
}

impl Ledger {
    /// A table of at most `capacity` keys, each kept `lifetime` after its answer.
    #[must_use]
    pub fn new(capacity: usize, lifetime: Duration) -> Self {
        Self { table: Arc::default(), capacity: capacity.max(1), lifetime }
    }

    /// `verb`'s answer under `key`: `effect`'s, run on a task of its own, the first time; the
    /// answer it gave (or will give) every time after, without running `effect`.
    pub async fn run<F>(&self, key: IdempotencyKey, verb: &Verb, effect: F) -> Outcome
    where
        F: Future<Output = Outcome> + Send + 'static,
    {
        let digest = match codec::encode_body(verb) {
            Ok(body) => blake3::hash(&body),
            Err(e) => return failed(ErrorCode::Invalid, &e.to_string()),
        };
        match self.claim(key.clone(), digest) {
            Claim::Run(tx, rx) => {
                let ledger = self.clone();
                tokio::spawn(async move {
                    let outcome = effect.await;
                    ledger.answered(&key);
                    tx.send_replace(Some(outcome));
                });
                answer(rx).await
            }
            Claim::Wait(rx) => answer(rx).await,
            Claim::Reused => key.reused(),
            Claim::Full => failed(
                ErrorCode::Failed,
                "too many keyed verbs are running on this worker; try again shortly",
            ),
        }
    }

    fn claim(&self, key: IdempotencyKey, digest: blake3::Hash) -> Claim {
        let now = Instant::now();
        let mut table = self.table.lock();
        if let Some(slot) = table.slots.get(&key)
            && !slot.lapsed(now, self.lifetime)
        {
            return if slot.digest == digest {
                Claim::Wait(slot.answer.clone())
            } else {
                Claim::Reused
            };
        }
        if table.slots.len() >= self.capacity && !table.make_room(now, self.lifetime) {
            return Claim::Full;
        }
        let (tx, rx) = watch::channel(None);
        table.slots.insert(key, Slot { digest, answer: rx.clone(), answered: None });
        drop(table);
        Claim::Run(tx, rx)
    }

    fn answered(&self, key: &IdempotencyKey) {
        if let Some(slot) = self.table.lock().slots.get_mut(key) {
            slot.answered = Some(Instant::now());
        }
    }

    /// Keys in the table, lapsed ones included until room is made.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.table.lock().slots.len()
    }
}

impl Table {
    /// Drop every lapsed slot, else the one answered longest ago; false when all still run.
    fn make_room(&mut self, now: Instant, lifetime: Duration) -> bool {
        let before = self.slots.len();
        self.slots.retain(|_, slot| !slot.lapsed(now, lifetime));
        if self.slots.len() < before {
            return true;
        }
        let oldest = self
            .slots
            .iter()
            .filter_map(|(key, slot)| slot.answered.map(|at| (at, key)))
            .min_by_key(|(at, _)| *at)
            .map(|(_, key)| key.clone());
        oldest.is_some_and(|key| self.slots.remove(&key).is_some())
    }
}

/// The answer `rx` carries, once it does.
async fn answer(mut rx: watch::Receiver<Option<Outcome>>) -> Outcome {
    match rx.wait_for(Option::is_some).await {
        Ok(answer) => answer.clone().unwrap_or_else(lost),
        Err(_ended) => lost(),
    }
}

fn lost() -> Outcome {
    failed(ErrorCode::Failed, "the verb's first attempt ended without an answer")
}

fn failed(code: ErrorCode, message: &str) -> Outcome {
    Outcome::Error { code, message: message.to_owned() }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use slopty_core::{SessionId, WorkerId};
    use slopty_proto::orchestration::{Input, TermRef};
    use tokio::sync::oneshot;

    use super::*;

    fn key(name: &str) -> IdempotencyKey {
        IdempotencyKey::new(name).unwrap()
    }

    fn typing(text: &str) -> Verb {
        let term = TermRef { worker: WorkerId::nil(), session: SessionId::nil() };
        Verb::SendInput { term, input: Input::Text(text.to_owned()) }
    }

    /// Counts the times it ran, and answers `Done`.
    fn counted(runs: &Arc<AtomicU32>) -> impl Future<Output = Outcome> + Send + 'static {
        let runs = Arc::clone(runs);
        async move {
            runs.fetch_add(1, Ordering::SeqCst);
            Outcome::Done
        }
    }

    #[tokio::test]
    async fn a_repeat_answers_the_first_outcome_without_running_again() {
        let (ledger, runs) = (Ledger::default(), Arc::new(AtomicU32::new(0)));
        let verb = typing("make\n");
        assert_eq!(ledger.run(key("a"), &verb, counted(&runs)).await, Outcome::Done);
        assert_eq!(ledger.run(key("a"), &verb, counted(&runs)).await, Outcome::Done);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        ledger.run(key("b"), &verb, counted(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "another key runs it again");
    }

    #[tokio::test]
    async fn a_repeat_while_the_first_runs_waits_for_its_answer() {
        let (ledger, runs) = (Ledger::default(), Arc::new(AtomicU32::new(0)));
        let verb = typing("make\n");
        let (release, released) = oneshot::channel::<()>();
        let slow = {
            let runs = Arc::clone(&runs);
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                released.await.unwrap();
                Outcome::Waited(slopty_proto::orchestration::Waited::TimedOut)
            }
        };
        let first = tokio::spawn({
            let (ledger, verb) = (ledger.clone(), verb.clone());
            async move { ledger.run(key("a"), &verb, slow).await }
        });
        tokio::task::yield_now().await;
        // The caller that asked first goes away; the verb runs on.
        first.abort();
        let second = tokio::spawn({
            let (ledger, verb, runs) = (ledger.clone(), verb.clone(), Arc::clone(&runs));
            async move { ledger.run(key("a"), &verb, counted(&runs)).await }
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished(), "waits for the first");
        release.send(()).unwrap();
        let waited = second.await.unwrap();
        assert_eq!(waited, Outcome::Waited(slopty_proto::orchestration::Waited::TimedOut));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_key_reused_with_other_arguments_is_invalid() {
        let (ledger, runs) = (Ledger::default(), Arc::new(AtomicU32::new(0)));
        ledger.run(key("a"), &typing("make\n"), counted(&runs)).await;
        let reused = ledger.run(key("a"), &typing("rm -rf /\n"), counted(&runs)).await;
        assert!(matches!(reused, Outcome::Error { code: ErrorCode::Invalid, .. }), "{reused:?}");
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_key_lapses_its_lifetime_after_the_answer() {
        let (ledger, runs) = (Ledger::default(), Arc::new(AtomicU32::new(0)));
        let verb = typing("make\n");
        ledger.run(key("a"), &verb, counted(&runs)).await;
        tokio::time::advance(KEY_LIFETIME.saturating_sub(Duration::from_secs(1))).await;
        ledger.run(key("a"), &verb, counted(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 1, "still kept");
        tokio::time::advance(Duration::from_secs(1)).await;
        ledger.run(key("a"), &verb, counted(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2, "lapsed, so it runs again");
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_table_drops_the_oldest_answer_and_never_a_running_verb() {
        let (ledger, runs) = (Ledger::new(2, KEY_LIFETIME), Arc::new(AtomicU32::new(0)));
        let verb = typing("make\n");
        for name in ["a", "b", "c"] {
            ledger.run(key(name), &verb, counted(&runs)).await;
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        assert_eq!(ledger.len(), 2);
        ledger.run(key("c"), &verb, counted(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 3, "c is kept");
        ledger.run(key("a"), &verb, counted(&runs)).await;
        assert_eq!(runs.load(Ordering::SeqCst), 4, "a went first");

        let ledger = Ledger::new(1, KEY_LIFETIME);
        let _running = tokio::spawn({
            let (ledger, verb) = (ledger.clone(), verb.clone());
            async move { ledger.run(key("slow"), &verb, std::future::pending()).await }
        });
        tokio::task::yield_now().await;
        let full = ledger.run(key("next"), &verb, counted(&runs)).await;
        assert!(matches!(full, Outcome::Error { code: ErrorCode::Failed, .. }), "{full:?}");
    }
}
