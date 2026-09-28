//! The bookkeeping of every work of the process, whatever the system has said of it so far.
//! Pure: the iOS half does what each answer says to the system's task.

use std::collections::BTreeMap;

use super::Expired;

/// Every work of the process, by key.
#[derive(Debug)]
pub(super) struct Ledger<T> {
    next: u64,
    works: BTreeMap<u64, Entry<T>>,
}

struct Entry<T> {
    state: State<T>,
    done: u64,
    total: u64,
    expired: Expired,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Entry<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("state", &self.state)
            .field("done", &self.done)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

/// Where a work stands with the system.
#[derive(Debug)]
enum State<T> {
    /// Submitted; the system has not started it yet.
    Waiting,
    /// Started: the system's task, shown in its progress UI.
    Running(T),
    /// Ended here before the system started it, which completes it as soon as it does.
    Ended(bool),
}

/// What to do with a task the system just started.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Launched<T> {
    /// Show this progress and keep it.
    Run { task: T, done: u64, total: u64 },
    /// Complete it at once: the work is already over, or was never this process's.
    Finish { task: T, success: bool },
}

/// What to do when a work ends here.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ended<T> {
    /// Complete the system's task.
    Complete { task: T, success: bool },
    /// Withdraw the request the system has not started.
    Withdraw,
    /// Nothing: the system already ended it, or it was never submitted.
    Nothing,
}

impl<T: Clone> Ledger<T> {
    pub(super) const fn new() -> Self {
        Self { next: 0, works: BTreeMap::new() }
    }

    /// A new work, waiting on the system; its key, never 0.
    pub(super) fn begin(&mut self, expired: Expired) -> u64 {
        self.next = self.next.saturating_add(1);
        let entry = Entry { state: State::Waiting, done: 0, total: 0, expired };
        self.works.insert(self.next, entry);
        self.next
    }

    /// The system refused the request: the work goes on only while the app is on screen.
    pub(super) fn refused(&mut self, key: u64) {
        self.works.remove(&key);
    }

    /// The system started `key` as `task`.
    pub(super) fn launched(&mut self, key: u64, task: T) -> Launched<T> {
        let Some(entry) = self.works.get_mut(&key) else {
            return Launched::Finish { task, success: false };
        };
        match entry.state {
            State::Waiting => {
                entry.state = State::Running(task.clone());
                Launched::Run { task, done: entry.done, total: entry.total }
            }
            State::Ended(success) => {
                self.works.remove(&key);
                Launched::Finish { task, success }
            }
            State::Running(_) => Launched::Finish { task, success: false },
        }
    }

    /// `key` is `done` of `total` through; the task to show it on, once there is one.
    pub(super) fn progress(&mut self, key: u64, done: u64, total: u64) -> Option<T> {
        let entry = self.works.get_mut(&key)?;
        entry.done = done;
        entry.total = total;
        match &entry.state {
            State::Running(task) => Some(task.clone()),
            State::Waiting | State::Ended(_) => None,
        }
    }

    /// `key` ended here.
    pub(super) fn end(&mut self, key: u64, success: bool) -> Ended<T> {
        let Some(entry) = self.works.get_mut(&key) else { return Ended::Nothing };
        match &entry.state {
            State::Waiting => {
                entry.state = State::Ended(success);
                Ended::Withdraw
            }
            State::Running(task) => {
                let task = task.clone();
                self.works.remove(&key);
                Ended::Complete { task, success }
            }
            State::Ended(_) => Ended::Nothing,
        }
    }

    /// The system is taking `key` back (the person cancelled it, or its time is up): the task
    /// to complete as failed, and whom to tell. `None` when it is no running work here.
    pub(super) fn expired(&mut self, key: u64) -> Option<(T, Expired)> {
        if !matches!(self.works.get(&key)?.state, State::Running(_)) {
            return None;
        }
        let entry = self.works.remove(&key)?;
        match entry.state {
            State::Running(task) => Some((task, entry.expired)),
            State::Waiting | State::Ended(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn counting() -> (Expired, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let expired: Expired = Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        (expired, calls)
    }

    /// Progress told before the system starts the work is what its task shows first, later
    /// progress goes to the task, and the end completes it and forgets the work.
    #[test]
    fn a_work_runs_shows_its_progress_and_completes() {
        let mut ledger = Ledger::new();
        let (expired, calls) = counting();
        let key = ledger.begin(expired);
        assert_ne!(key, 0, "0 is the key of a work never submitted");
        assert_eq!(ledger.progress(key, 10, 100), None, "no task to show it on yet");
        assert_eq!(
            ledger.launched(key, "task"),
            Launched::Run { task: "task", done: 10, total: 100 }
        );
        assert_eq!(ledger.progress(key, 50, 100), Some("task"));
        assert_eq!(ledger.end(key, true), Ended::Complete { task: "task", success: true });
        assert_eq!(ledger.end(key, false), Ended::Nothing, "only the first end counts");
        assert!(ledger.works.is_empty(), "{:?}", ledger.works);
        assert_eq!(calls.load(Ordering::Relaxed), 0, "never expired");
    }

    /// A work that ends before the system starts it withdraws the request; if the start races
    /// the withdrawal, the task is completed at once with the work's outcome.
    #[test]
    fn a_work_over_before_the_system_starts_it_completes_at_once() {
        let mut ledger = Ledger::new();
        let key = ledger.begin(counting().0);
        assert_eq!(ledger.end(key, true), Ended::Withdraw);
        assert_eq!(ledger.launched(key, 7), Launched::Finish { task: 7, success: true });
        assert!(ledger.works.is_empty(), "{:?}", ledger.works);
        assert_eq!(ledger.launched(99, 8), Launched::Finish { task: 8, success: false }, "unknown");
    }

    /// The person cancelling from the Live Activity: the transfer is told once, the task is
    /// completed as failed, and the transfer's own end afterwards does nothing more.
    #[test]
    fn an_expired_work_tells_the_transfer_once() {
        let mut ledger = Ledger::new();
        let (expired, calls) = counting();
        let key = ledger.begin(expired);
        assert!(ledger.expired(key).is_none(), "the system expires only what it started");
        let _run = ledger.launched(key, "task");
        let (task, tell) = ledger.expired(key).unwrap();
        assert_eq!(task, "task");
        tell();
        assert!(ledger.expired(key).is_none(), "once");
        assert_eq!(ledger.end(key, false), Ended::Nothing);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    /// A refused request is forgotten: its progress and end reach nothing.
    #[test]
    fn a_refused_work_reaches_nothing() {
        let mut ledger = Ledger::<u8>::new();
        let key = ledger.begin(counting().0);
        ledger.refused(key);
        assert_eq!(ledger.progress(key, 1, 2), None);
        assert_eq!(ledger.end(key, true), Ended::Nothing);
    }
}
