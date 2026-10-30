//! The fleet's attention ladder, and the notices it sends by where the person is
//! (`slopty_proto::thread::attention`, `docs/decisions/agents.md`).
//!
//! Each worker publishes its thread table
//! ([`slopty_proto::server::ToServer::Threads`]); the hub keeps the rows and ranks them again
//! whenever a row or a terminal moves: a subagent folds into the thread it hangs from, and the
//! rest roll up per tile, worker, agent tree (an agent with every agent it started, at any
//! depth, across workers), space (a folder on a machine) and the fleet. A ladder that differs
//! from the last goes to every link.
//!
//! A thread that hangs from no other and climbs to needing the person, fails, or comes to rest
//! from working, is a notice; an agent another agent started coming to rest is not, since its
//! parent hears of it and the person hears what the parent makes of it. It goes to
//! no client when the thread's tile is on screen where the person is, to the desks they are at when
//! they are at one, to the handhelds they hold when not, and to every client when they are at none.
//!
//! When a notice finds the person at none of their clients, it is pushed as well to every phone
//! that gave the server a device and is not listening on a live link ([`Phones`],
//! `slopty_proto::push`): sealed to the phone, through the relay or straight to APNs
//! ([`crate::push`]). Once a thread a phone was told needs the person stops needing them
//! (answered in its terminal, from whichever client), the note is taken back with a background
//! push ([`Phones::take_back`]). A take-back the push queue cannot take now is owed, and tried
//! again after [`TAKE_BACK_RETRY`] unless a newer note about the same thread replaced it. What
//! each phone shows and what is owed it are kept with the phones ([`PushKept`]), so a server
//! that restarts still takes them back. A notice every link it went to could not take is pushed
//! as well, as though the person were away. A note answers nothing: it opens its session, where
//! the person answers in the agent's terminal. One that a turn finished is taken back once the
//! person has seen the turn ([`Shown::finished`]).
//! A thread first seen already needing the person is pushed to a phone not showing it
//! ([`Told::first`]).
//!
//! A thread still needing the person [`REMIND_AFTER`] after its wait began is told again where
//! the person is now, once per wait, saying how long it has waited ([`Board::remind`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId, WallMs, WorkerId};
use slopty_proto::agent::{PARENT_FACT, SEAT_FACT};
use slopty_proto::orchestration::{TermAgent, TermRef};
use slopty_proto::push::{PushBody, PushDevice};
use slopty_proto::server::FromServer;
use slopty_proto::terminal::{ProgramState, ProgramStatus, SessionSummary};
use slopty_proto::thread::attention::{
    Ladder, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat, SpaceAt, Standing, Subject,
    ThreadAt, Via,
};
use slopty_proto::thread::wire::{TableFrame, ThreadRow};
use slopty_proto::thread::{Liveness, Phase, ThreadId, TurnId};
use tokio::sync::{Notify, broadcast, mpsc, watch};

use super::awake::{Awake, Hold, Policy as KeepAwake};
use super::{Hub, State, WeakHub};
use crate::push::{Outgoing, Sending};

/// The rows every worker published, the ladder made of them, and the clients the person may
/// be at.
#[derive(Debug, Default)]
pub(super) struct Board {
    /// Each worker's rows, by thread.
    tables: HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>,
    /// The ladder last published.
    published: Ladder,
    /// Since when each thread has been busy (working or waiting), for how long a finished
    /// one worked.
    busy: HashMap<ThreadAt, WallMs>,
    /// Every person's client link, by the server's number for it.
    seats: BTreeMap<u64, Sitting>,
    /// The last number a link was given.
    next_link: u64,
    /// Wakes [`Hub::publish_ladder`] when rows came.
    wake: Arc<Notify>,
    /// Where each thread hanging from no other was last said to stand ([`Self::rung_moves`]).
    rungs_said: HashMap<(WorkerId, ThreadId), TermAgent>,
    /// The hold on the server's machine that the seats and the tables imply.
    awake: Awake,
    /// The phones notices are pushed to when the person is at no client.
    phones: Phones,
    /// Counts each frame of rows taken in, for a wait on the agents ([`Self::rows_changed`]).
    rows: watch::Sender<u64>,
    /// The workers whose link ended and has not come back: their rows say what they last said,
    /// which nothing keeps true, so none of them holds the machine awake.
    unlinked: HashSet<WorkerId>,
    /// Each thread told again that it still needs the person, by when its wait began: once per
    /// wait ([`Self::remind`]).
    reminded: HashMap<ThreadAt, WallMs>,
    /// When the next reminder falls due, once a wake is set for it.
    remind_at: Option<WallMs>,
    /// How long a wait goes before it is told again, when not [`REMIND_AFTER`] (tests).
    remind_after: Option<Duration>,
}

/// How long a take-back the push queue could not take waits before it is tried again.
pub(crate) const TAKE_BACK_RETRY: Duration = Duration::from_secs(2);

/// How long a thread needs the person before it is told again, once per wait, as Claude
/// desktop notifies an open prompt again at 30 minutes.
pub(crate) const REMIND_AFTER: Duration = Duration::from_mins(30);

/// The phones the server may push to, and where their pushes go.
#[derive(Debug, Default)]
struct Phones {
    /// Each phone, by its client.
    devices: Devices,
    /// Where pushes go to be sealed and sent: none while pushing is off.
    out: Option<mpsc::Sender<Outgoing>>,
    /// Where the phones go to be kept ([`crate::store::PushStore`]).
    kept: Option<watch::Sender<PushKept>>,
    /// The notes each phone shows that a take-back is to take down ([`Shown`]).
    shown: Shown,
    /// What each phone is owed a take-back of, decided and not yet taken by the push queue.
    owed: BTreeMap<ClientId, HashSet<Asked>>,
    /// The notes the push queue could not take when they were made, the latest per phone and
    /// subject, oldest first: sent with the owed take-backs. Not kept across a restart, where
    /// the note would be stale. A finished turn's carries the turn ([`Shown::finished`]).
    owed_notes: Vec<(ClientId, PushBody, Option<TurnId>)>,
    /// When the owed take-backs and notes are next tried, once a try is set.
    retry_at: Option<tokio::time::Instant>,
}

/// The phones the server may push to, the notes each shows that need the person, and the
/// take-backs each is owed, as the store keeps them ([`crate::store::PushStore`]).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PushKept {
    devices: Devices,
    asked: BTreeMap<ClientId, HashSet<Asked>>,
    finished: BTreeMap<ClientId, HashSet<(ThreadAt, TurnId)>>,
    owed: BTreeMap<ClientId, HashSet<Asked>>,
}

impl PushKept {
    /// Each phone, by its client.
    #[must_use]
    pub const fn devices(&self) -> &Devices {
        &self.devices
    }
}

/// What a pushed note that needs the person is about: a thread, or a terminal's program.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
enum Asked {
    Thread(ThreadAt),
    Terminal(TermRef),
}

impl Asked {
    /// The subject a take-back names.
    const fn subject(self) -> Subject {
        match self {
            Self::Thread(at) => Subject::Thread(at),
            Self::Terminal(term) => Subject::Terminal(term),
        }
    }
}

/// The phones the server may push to, by their clients.
pub type Devices = BTreeMap<ClientId, PushDevice>;

/// The notes each phone shows that a take-back is to take down, by its client: one per thread
/// or terminal, as the push queue took them ([`Self::noted`]).
#[derive(Debug, Default)]
struct Shown {
    /// The notes about what needs the person, taken back once it no longer does
    /// ([`Phones::take_back`], [`Phones::program_answered`]).
    asked: BTreeMap<ClientId, HashSet<Asked>>,
    /// The notes about a finished turn, with the turn, taken back once the person has seen it
    /// on any device ([`ThreadRow::seen`]).
    finished: BTreeMap<ClientId, HashSet<(ThreadAt, TurnId)>>,
}

impl Shown {
    /// The push queue took `notice` for `client`'s phone, about `turn` when it says a turn
    /// finished. The phone shows one note per thread or terminal, so this one replaced whatever
    /// was up there.
    fn noted(&mut self, client: ClientId, notice: &Notice, turn: Option<TurnId>) {
        let about = asked_about(&notice.about);
        let finished = self.finished.entry(client).or_default();
        if let Asked::Thread(at) = about {
            finished.retain(|(t, _)| *t != at);
        }
        let asked = self.asked.entry(client).or_default();
        if notice.kind == NoticeKind::NeedsYou {
            asked.insert(about);
            return;
        }
        asked.remove(&about);
        if let (NoticeKind::Finished, Asked::Thread(at), Some(turn)) = (notice.kind, about, turn) {
            finished.insert((at, turn));
        }
    }

    /// Forget every phone's empty sets.
    fn tidy(&mut self) {
        self.asked.retain(|_, asked| !asked.is_empty());
        self.finished.retain(|_, finished| !finished.is_empty());
    }

    /// Forget what the phone of `client` shows.
    fn forget(&mut self, client: ClientId) {
        self.asked.remove(&client);
        self.finished.remove(&client);
    }
}

/// How a notice is pushed ([`Phones::send`]).
#[derive(Clone, Copy, Debug, Default)]
struct Pushing<'a> {
    /// The links that could not take it, so their phones do not count as listening.
    unheard: &'a [u64],
    /// Only to the phones not already showing a note about its subject that needs the person.
    new_only: bool,
    /// The turn a finished turn's note is about, to take it back once seen.
    turn: Option<TurnId>,
}

impl Phones {
    /// Hand the phones, and what they show and are owed, to the keeper when that moved.
    fn keep(&self) {
        if let Some(kept) = &self.kept {
            kept.send_if_modified(|kept| {
                let now = PushKept {
                    devices: self.devices.clone(),
                    asked: self.shown.asked.clone(),
                    finished: self.shown.finished.clone(),
                    owed: self.owed.clone(),
                };
                let moved = *kept != now;
                *kept = now;
                moved
            });
        }
    }

    /// Push `notice` to every phone not listening on a live link among `seats`, as `how` says,
    /// skipping the phones already showing it when only new ones are to hear. A finished turn
    /// shorter than a phone's quiet time is not pushed to it, as that phone would not post it. A
    /// link among those `how` names unheard could not take the notice, so its phone is not counted
    /// as listening.
    fn send(&mut self, seats: &BTreeMap<u64, Sitting>, notice: &Notice, how: Pushing<'_>) {
        let Some(out) = &self.out else { return };
        let about = asked_about(&notice.about);
        for (client, device) in &self.devices {
            let shown =
                |about: Asked| self.shown.asked.get(client).is_some_and(|a| a.contains(&about));
            if how.new_only && shown(about) {
                continue;
            }
            let listening = seats.iter().any(|(link, s)| {
                s.client == Some(*client)
                    && s.presence.as_ref().is_none_or(|p| p.listening)
                    && !how.unheard.contains(link)
            });
            let short = notice.kind == NoticeKind::Finished
                && notice.worked_ms.is_some_and(|ms| ms < device.quiet_ms);
            if listening || short {
                continue;
            }
            // A note answers nothing: it opens its session.
            let body = PushBody { notice: notice.clone() };
            // A note waiting for room, or a take-back owed, about the same subject is older
            // than this one, which replaces it on the phone.
            self.owed_notes
                .retain(|(c, owed, _)| *c != *client || owed.notice.about != notice.about);
            if let Some(owed) = self.owed.get_mut(client) {
                owed.remove(&about);
            }
            let push =
                Outgoing { client: *client, device: device.clone(), what: Sending::Note(body) };
            match out.try_send(push) {
                Ok(()) => self.shown.noted(*client, notice, how.turn),
                Err(mpsc::error::TrySendError::Full(push)) => {
                    tracing::debug!(%client, "a push found its queue full; owed");
                    let Sending::Note(body) = push.what else { continue };
                    self.owed_notes.push((*client, body, how.turn));
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::debug!(%client, "a push found its queue gone");
                }
            }
        }
        self.shown.tidy();
        self.owed.retain(|_, owed| !owed.is_empty());
        self.keep();
    }

    /// Take back from each phone what `answered` says no longer needs the person, among what
    /// it was pushed: one background push per [`slopty_push::apns::MAX_TAKE_BACK`]. What the
    /// queue cannot take now stays owed ([`Self::send_owed`]).
    fn take_back_where(&mut self, answered: impl Fn(&Asked) -> bool) {
        if self.out.is_none() {
            self.shown = Shown::default();
            self.owed.clear();
            self.keep();
            return;
        }
        let devices = &self.devices;
        self.owed.retain(|client, _| devices.contains_key(client));
        self.shown.finished.retain(|client, _| devices.contains_key(client));
        // A note still waiting for room about what no longer needs the person goes unsent.
        self.owed_notes.retain(|(client, body, _)| {
            devices.contains_key(client) && !answered(&asked_about(&body.notice.about))
        });
        let owed = &mut self.owed;
        self.shown.asked.retain(|client, asked| {
            if !devices.contains_key(client) {
                return false;
            }
            let gone: Vec<Asked> = asked.iter().copied().filter(|a| answered(a)).collect();
            for about in gone {
                asked.remove(&about);
                owed.entry(*client).or_default().insert(about);
            }
            !asked.is_empty()
        });
        self.send_owed();
    }

    /// Hand the push queue every take-back owed that it takes; the rest stay owed. Whether any
    /// is still owed.
    fn send_owed(&mut self) -> bool {
        if let Some(out) = &self.out {
            let devices = &self.devices;
            let mut waiting = std::mem::take(&mut self.owed_notes).into_iter();
            for (client, body, turn) in waiting.by_ref() {
                let Some(device) = devices.get(&client) else { continue };
                let notice = body.notice.clone();
                let push = Outgoing { client, device: device.clone(), what: Sending::Note(body) };
                match out.try_send(push) {
                    Ok(()) => self.shown.noted(client, &notice, turn),
                    Err(mpsc::error::TrySendError::Full(push)) => {
                        if let Sending::Note(body) = push.what {
                            self.owed_notes.push((client, body, turn));
                        }
                        break;
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            self.owed_notes.extend(waiting);
            for (client, owed) in &mut self.owed {
                let Some(device) = devices.get(client) else {
                    owed.clear();
                    continue;
                };
                let all: Vec<Asked> = owed.iter().copied().collect();
                for chunk in all.chunks(slopty_push::apns::MAX_TAKE_BACK) {
                    let what = Sending::TakeBack(chunk.iter().map(|a| a.subject()).collect());
                    let push = Outgoing { client: *client, device: device.clone(), what };
                    if out.try_send(push).is_err() {
                        tracing::debug!(%client, "a take-back found its queue full; owed");
                        break;
                    }
                    for about in chunk {
                        owed.remove(about);
                    }
                }
            }
        }
        self.owed.retain(|_, owed| !owed.is_empty());
        self.shown.tidy();
        self.keep();
        !self.owed.is_empty() || !self.owed_notes.is_empty()
    }

    /// Take back each phone's pushed note about `term`'s program: it no longer waits on the
    /// person, or the terminal is gone.
    fn program_answered(&mut self, term: TermRef) {
        self.take_back_where(|a| *a == Asked::Terminal(term));
    }

    /// Take back each phone's pushed asks whose threads no longer need the person in `ladder`
    /// (answered at another client or in the terminal, or ended), and its notes of a finished
    /// turn the person has since seen on any device, or whose thread ended. A thread on a
    /// worker that is not linked now is left until it is, since nobody can tell. A phone that
    /// is listening again, or forgotten, takes back its own on coming to the front
    /// ([`Self::listening`]).
    fn take_back(
        &mut self,
        ladder: &Ladder,
        tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>,
    ) {
        let read = |at: &ThreadAt, turn: TurnId| {
            tables.get(&at.worker).is_some_and(|t| t.get(&at.thread).is_none_or(|r| r.seen >= turn))
        };
        let owed = &mut self.owed;
        for (client, finished) in &mut self.shown.finished {
            finished.retain(|(at, turn)| {
                let seen = read(at, *turn);
                if seen {
                    owed.entry(*client).or_default().insert(Asked::Thread(*at));
                }
                !seen
            });
        }
        self.take_back_where(|asked| {
            let Asked::Thread(at) = asked else { return false };
            if !tables.contains_key(&at.worker) {
                return false;
            }
            let found = ladder.threads.binary_search_by_key(at, |r| r.at).ok();
            found.and_then(|i| ladder.threads.get(i)).is_none_or(|r| r.rung != Rung::NeedsYou)
        });
    }

    /// The phone of `client` listens on its link again: back in front, it takes back its own
    /// notes, so none is left for a push to.
    fn listening(&mut self, client: ClientId) {
        self.shown.forget(client);
        self.owed.remove(&client);
        self.owed_notes.retain(|(c, ..)| *c != client);
        self.keep();
    }
}

/// What a note about `subject` puts up on a phone that a take-back can take down: one per
/// thread or terminal.
const fn asked_about(subject: &Subject) -> Asked {
    match subject {
        Subject::Thread(at) => Asked::Thread(*at),
        Subject::Terminal(term) => Asked::Terminal(*term),
    }
}

impl Board {
    /// Owed take-backs are tried again after [`TAKE_BACK_RETRY`]: a ranking pass sends what
    /// the queue takes, and sets the next try while any is still owed.
    fn retry_owed(&mut self) {
        let now = tokio::time::Instant::now();
        if self.phones.retry_at.is_some_and(|at| at > now) {
            return;
        }
        self.phones.retry_at = None;
        if !self.phones.send_owed() {
            return;
        }
        let at = now.checked_add(TAKE_BACK_RETRY).unwrap_or(now);
        self.phones.retry_at = Some(at);
        let wake = Arc::clone(&self.wake);
        drop(tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            wake.notify_one();
        }));
    }
}

impl Board {
    /// The threads that have needed the person for [`REMIND_AFTER`] since their wait began, told
    /// so again, once per wait, where the person is now: "Still needs you after 30m". A wait
    /// first seen already that old is no news, as its need was not ([`Told::first`]). A wake is
    /// set for the next one due.
    fn remind(&mut self, ladder: &Ladder, now: WallMs) -> Vec<Notice> {
        let waits: HashMap<ThreadAt, WallMs> = ladder
            .threads
            .iter()
            .filter(|r| r.rung == Rung::NeedsYou)
            .map(|r| (r.at, r.since_ms))
            .collect();
        self.reminded.retain(|at, since| waits.get(at) == Some(since));
        let seen: HashSet<ThreadAt> = self
            .published
            .threads
            .iter()
            .filter(|r| r.rung == Rung::NeedsYou)
            .map(|r| r.at)
            .collect();
        let after = self.remind_after.unwrap_or(REMIND_AFTER);
        let after = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        let mut told = Vec::new();
        let mut next: Option<u64> = None;
        for (at, since) in waits {
            if self.reminded.contains_key(&at) {
                continue;
            }
            let due = since.as_millis().saturating_add(after);
            if due > now.as_millis() {
                next = Some(next.map_or(due, |n| n.min(due)));
                continue;
            }
            self.reminded.insert(at, since);
            if !seen.contains(&at) {
                continue;
            }
            let Some(mut notice) = notice_of(self, at, (NoticeKind::NeedsYou, None)) else {
                continue;
            };
            let waited = now.as_millis().saturating_sub(since.as_millis());
            notice.text = still_words(waited, &notice.text);
            told.push(notice);
        }
        if let Some(due) = next {
            self.wake_at(WallMs::from_millis(due), now);
        }
        told
    }

    /// Rank again at `due`, unless a ranking is already set for no later.
    fn wake_at(&mut self, due: WallMs, now: WallMs) {
        if self.remind_at.is_some_and(|at| at > now && at <= due) {
            return;
        }
        self.remind_at = Some(due);
        let wait = Duration::from_millis(due.as_millis().saturating_sub(now.as_millis()));
        let wake = Arc::clone(&self.wake);
        drop(tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            wake.notify_one();
        }));
    }
}

/// A reminder's words: how long the thread has waited on the person, in whole minutes and
/// hours, then what it asks.
fn still_words(waited_ms: u64, asks: &str) -> String {
    let minutes = waited_ms / 60_000;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let waited = match (hours, minutes) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    };
    if asks.is_empty() {
        format!("Still needs you after {waited}")
    } else {
        format!("Still needs you after {waited} · {asks}")
    }
}

/// A person's client link.
#[derive(Debug)]
struct Sitting {
    name: String,
    /// Where its notices go: the link's own queue.
    tx: mpsc::Sender<FromServer>,
    /// Where the person is on it, once it said.
    presence: Option<Presence>,
    /// The client, once it said it is a phone the server may push to.
    client: Option<ClientId>,
    /// Whether the server can push to that phone ([`FromServer::Pushable`]), once it said
    /// one: a word the link takes the latest of, so none is dropped behind a full queue.
    pushable: watch::Sender<Option<bool>>,
}

impl Board {
    /// Hold the machine through `hold` from now on, under `policy`.
    pub(super) fn keep_awake(&mut self, hold: Box<dyn Hold>, policy: KeepAwake) {
        let (linked, working) = self.linked_and_working();
        self.awake.keep(hold, policy, linked, working);
    }

    /// Hold the machine under `policy` from now on.
    pub(super) fn set_keep_awake(&mut self, policy: KeepAwake) {
        let (linked, working) = self.linked_and_working();
        self.awake.set_policy(policy, linked, working);
    }

    /// Whether a person's client is linked, and whether an agent is at work on any worker
    /// linked now: a thread whose agent is there working, or waiting on its own background
    /// work. A worker out of reach may have stopped mid-turn, its rows left as they were.
    fn linked_and_working(&self) -> (bool, bool) {
        let working = self
            .tables
            .iter()
            .filter(|(worker, _)| !self.unlinked.contains(worker))
            .flat_map(|(_, table)| table.values())
            .any(|r| there(r) && matches!(r.status.phase, Phase::Working | Phase::Waiting));
        (!self.seats.is_empty(), working)
    }

    /// `worker`'s link came (`linked`) or ended: only a linked worker's agents hold the machine.
    pub(super) fn linked(&mut self, worker: WorkerId, linked: bool) {
        let moved =
            if linked { self.unlinked.remove(&worker) } else { self.unlinked.insert(worker) };
        if moved {
            self.settle_awake();
        }
    }

    /// Hold the machine as the seats and the tables imply now.
    fn settle_awake(&mut self) {
        let (linked, working) = self.linked_and_working();
        self.awake.settle(linked, working);
    }

    /// Take in a worker's table frame. A snapshot replaces what it published before.
    pub(super) fn take(&mut self, worker: WorkerId, frame: TableFrame) {
        let table = self.tables.entry(worker).or_default();
        match frame {
            TableFrame::Snapshot { rows, .. } => {
                *table = rows.into_iter().map(|r| (r.id, r)).collect();
            }
            TableFrame::Delta { rows, removed, .. } => {
                for gone in removed {
                    table.remove(&gone);
                }
                table.extend(rows.into_iter().map(|r| (r.id, r)));
            }
        }
        self.wake.notify_one();
        self.rows.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Says each frame of rows taken in.
    pub(super) fn rows_changed(&self) -> watch::Receiver<u64> {
        self.rows.subscribe()
    }

    /// Every thread on every worker, with the worker it is on.
    pub(super) fn rows(&self) -> impl Iterator<Item = (WorkerId, &ThreadRow)> {
        self.tables.iter().flat_map(|(worker, table)| table.values().map(|r| (*worker, r)))
    }

    /// Every thread on every worker that hangs from no other there: the agents, without the
    /// subagents their harness runs inside them.
    pub(super) fn agent_rows(&self) -> impl Iterator<Item = (WorkerId, &ThreadRow)> {
        self.tables.iter().flat_map(|(worker, table)| {
            table.values().filter(|r| root_of(table, r) == r.id).map(|r| (*worker, r))
        })
    }

    /// The row of `thread`, with the worker that holds it.
    pub(super) fn row(&self, thread: ThreadId) -> Option<(WorkerId, &ThreadRow)> {
        self.tables.iter().find_map(|(worker, table)| Some((*worker, table.get(&thread)?)))
    }

    /// The thread seated at `term` (its TUI runs there, or an agent's thread was started there)
    /// and hanging from no other: one whose agent is there before one whose agent ended, then
    /// the latest to change. A session that ended later than the one running now (cut short as
    /// a worker restarted) does not hide the agent at the terminal.
    fn thread_in(&self, term: TermRef) -> Option<&ThreadRow> {
        let table = self.tables.get(&term.worker)?;
        table
            .values()
            .filter(|r| seat_of(r) == Some(term.session) && root_of(table, r) == r.id)
            .max_by_key(|r| (there(r), r.updated_ms, r.id))
    }

    /// The thread whose agent runs or is seated at `term`, hanging from no other.
    pub(super) fn thread_at(&self, term: TermRef) -> Option<ThreadId> {
        self.thread_in(term).map(|r| r.id)
    }

    /// The worker whose table holds `thread`.
    pub(super) fn worker_of(&self, thread: ThreadId) -> Option<WorkerId> {
        self.tables.iter().find(|(_, table)| table.contains_key(&thread)).map(|(w, _)| *w)
    }

    /// The seat `session` of a thread on any worker, as its row's [`SEAT_FACT`] says.
    pub(super) fn seat(&self, session: SessionId) -> Option<TermRef> {
        self.tables.iter().find_map(|(worker, table)| {
            table
                .values()
                .any(|r| seat_of(r) == Some(session))
                .then_some(TermRef { worker: *worker, session })
        })
    }

    /// The agent at work or seated at `term`, as its thread's row says; `None` with none there
    /// or once its agent ended.
    pub(super) fn agent_at(&self, term: TermRef) -> Option<TermAgent> {
        self.thread_in(term).filter(|r| there(r)).map(TermAgent::of)
    }

    /// Each thread on `worker` hanging from no other whose rung, phase or ask moved since last
    /// asked, with the terminal it is seated at: what [`Happening::Rung`] tells. One gone from
    /// the table is forgotten.
    ///
    /// [`Happening::Rung`]: slopty_proto::orchestration::Happening::Rung
    pub(super) fn rung_moves(&mut self, worker: WorkerId) -> Vec<(Option<SessionId>, TermAgent)> {
        let Some(table) = self.tables.get(&worker) else { return Vec::new() };
        let roots: Vec<&ThreadRow> = table.values().filter(|r| root_of(table, r) == r.id).collect();
        self.rungs_said.retain(|(w, id), _| *w != worker || roots.iter().any(|r| r.id == *id));
        let mut moves = Vec::new();
        for row in roots {
            let now = TermAgent::of(row);
            let said = self.rungs_said.get(&(worker, row.id));
            let moved = said.is_none_or(|was| {
                (was.rung, was.phase, &was.wait) != (now.rung, now.phase, &now.wait)
            });
            if moved {
                self.rungs_said.insert((worker, row.id), now.clone());
                moves.push((seat_of(row), now));
            }
        }
        moves
    }

    /// Where the person is on every client that said.
    fn present(&self) -> Vec<Present> {
        self.seats
            .iter()
            .filter_map(|(link, s)| {
                let presence = s.presence.clone()?;
                Some(Present { link: *link, name: s.name.clone(), presence })
            })
            .collect()
    }
}

/// A person's client link seated on the hub, for as long as the link lives.
#[derive(Debug)]
pub struct Seated {
    hub: WeakHub,
    link: u64,
    pushable: watch::Receiver<Option<bool>>,
}

impl Seated {
    /// The server's number for the link.
    #[must_use]
    pub const fn link(&self) -> u64 {
        self.link
    }

    /// Whether the server can push to the phone this client said it is, as that moves: `None`
    /// until it said one ([`FromServer::Pushable`]).
    #[must_use]
    pub fn pushable(&self) -> watch::Receiver<Option<bool>> {
        self.pushable.clone()
    }
}

impl Drop for Seated {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else { return };
        let mut state = hub.inner.state.lock();
        let gone = state.board.seats.remove(&self.link);
        if gone.as_ref().is_some_and(|s| at_desk(s.presence.as_ref())) {
            let state = &mut *state;
            left(&mut state.board);
        }
        if gone.is_some_and(|s| s.presence.is_some()) {
            hub.announce(FromServer::Present(state.board.present()));
        }
        state.board.settle_awake();
        drop(state);
    }
}

impl Hub {
    /// A number for a new link, its own among every link this server has had.
    #[must_use]
    pub fn number_link(&self) -> u64 {
        let mut state = self.inner.state.lock();
        state.board.next_link = state.board.next_link.wrapping_add(1);
        state.board.next_link
    }

    /// Seat the person's client link `link` ([`Self::number_link`]), named `name`, whose
    /// notices go on `tx`.
    #[must_use]
    pub fn seat(&self, link: u64, name: String, tx: mpsc::Sender<FromServer>) -> Seated {
        let (pushable, word) = watch::channel(None);
        let mut state = self.inner.state.lock();
        let sitting = Sitting { name, tx, presence: None, client: None, pushable };
        state.board.seats.insert(link, sitting);
        state.board.settle_awake();
        drop(state);
        Seated { hub: self.downgrade(), link, pushable: word }
    }

    /// Where the person is on the client of `link`.
    pub fn presence(&self, link: u64, presence: Presence) {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let board = &mut state.board;
        let Some(seat) = board.seats.get_mut(&link) else { return };
        if seat.presence.as_ref() == Some(&presence) {
            return;
        }
        if let Some(client) = seat.client.filter(|_| presence.listening) {
            board.phones.listening(client);
        }
        let leaves = at_desk(seat.presence.as_ref()) && !presence.active;
        seat.presence = Some(presence);
        if leaves {
            left(board);
        }
        self.announce(FromServer::Present(board.present()));
        drop(guard);
    }

    /// The phone the client `client` is, as it said on `link`: one the server may push to,
    /// or, with no `device`, no longer. A device token another client gave before is that
    /// client's no more: the phone was set up again.
    pub fn push_device(&self, link: u64, client: ClientId, device: Option<PushDevice>) {
        let mut state = self.inner.state.lock();
        let board = &mut state.board;
        if let Some(seat) = board.seats.get_mut(&link) {
            seat.client = Some(client);
        }
        let phones = &mut board.phones;
        let before = phones.devices.clone();
        match device {
            Some(device) if slopty_push::apns::is_token(&device.token) => {
                phones.devices.retain(|c, d| *c == client || d.token != device.token);
                phones.devices.insert(client, device);
            }
            Some(_) => {
                tracing::debug!(%client, "ignored a phone whose token is no device token");
            }
            None => {
                phones.devices.remove(&client);
            }
        }
        if phones.devices != before {
            phones.keep();
        }
        say_pushes(&state);
        drop(state);
    }

    /// Forget the phone of `client` while its token is still `token`: APNs said it is gone.
    pub fn forget_device(&self, client: ClientId, token: &str) {
        let mut state = self.inner.state.lock();
        let phones = &mut state.board.phones;
        if phones.devices.get(&client).is_some_and(|d| d.token == token) {
            phones.devices.remove(&client);
            phones.keep();
        }
        say_pushes(&state);
        drop(state);
    }

    /// Take `kept`, the phones a store kept with what they show and are owed, and say every
    /// change to them on the returned receiver from now on, for the store to keep. What is
    /// owed is tried again at the next ranking.
    pub fn keep_phones(&self, kept: PushKept) -> watch::Receiver<PushKept> {
        let mut state = self.inner.state.lock();
        let phones = &mut state.board.phones;
        let PushKept { devices, asked, finished, owed } = kept.clone();
        (phones.devices, phones.owed) = (devices, owed);
        phones.shown = Shown { asked, finished };
        let (sender, changes) = watch::channel(kept);
        phones.kept = Some(sender);
        say_pushes(&state);
        state.board.wake.notify_one();
        drop(state);
        changes
    }

    /// The phones the server may push to, by their clients.
    #[must_use]
    pub fn devices(&self) -> Devices {
        self.inner.state.lock().board.phones.devices.clone()
    }

    /// Push notices to phones through `out` from now on; with none, push none. The queue
    /// before it closes, so what sent from it ends.
    pub fn push_to(&self, out: Option<mpsc::Sender<Outgoing>>) {
        let mut state = self.inner.state.lock();
        state.board.phones.out = out;
        say_pushes(&state);
        drop(state);
    }

    /// The ladder as last published.
    #[must_use]
    pub fn ladder(&self) -> Ladder {
        self.inner.state.lock().board.published.clone()
    }

    /// Where the person is on every client that said.
    #[must_use]
    pub fn present(&self) -> Vec<Present> {
        self.inner.state.lock().board.present()
    }

    /// Rank the fleet again whenever rows come or anything else moves, and publish what
    /// changed, until the hub goes.
    pub async fn publish_ladder(hub: WeakHub) {
        let Some((wake, mut events)) = hub.upgrade().map(|h| {
            let wake = Arc::clone(&h.inner.state.lock().board.wake);
            (wake, h.subscribe())
        }) else {
            return;
        };
        loop {
            tokio::select! {
                () = wake.notified() => {}
                event = events.recv() => match event {
                    Ok(FromServer::Load { .. } | FromServer::Ladder(_) | FromServer::Present(_)) => {
                        continue;
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
            // Whatever else came meanwhile is ranked in the same pass.
            while !matches!(
                events.try_recv(),
                Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)
            ) {}
            let Some(hub) = hub.upgrade() else { return };
            hub.rank_ladder();
        }
    }

    /// Rank every thread now, and publish the ladder and its notices when it moved.
    pub(crate) fn rank_ladder(&self) {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        let live = &state.workers;
        state.board.tables.retain(|worker, _| live.contains_key(worker));
        state.board.unlinked.retain(|worker| live.contains_key(worker));
        state.board.settle_awake();
        let ladder = ladder(&state.board.tables);
        let board = &mut state.board;
        board.phones.take_back(&ladder, &board.tables);
        board.retry_owed();
        for notice in board.remind(&ladder, WallMs::now()) {
            tell_pushing(board, &notice, Pushing::default());
        }
        if ladder == board.published {
            return;
        }
        let notices = moved(board, &ladder);
        self.announce(FromServer::Ladder(Box::new(ladder.clone())));
        state.board.published = ladder;
        for Told { notice, turn, first } in notices {
            let board = &mut state.board;
            let how = Pushing { turn, ..Pushing::default() };
            if !first {
                tell_pushing(board, &notice, how);
            } else if route(board, &notice).away {
                board.phones.send(&board.seats, &notice, Pushing { new_only: true, ..how });
            }
        }
        drop(guard);
    }
}

/// Tell each client that said it is a phone whether the server can push to it, once that moved
/// ([`FromServer::Pushable`]).
fn say_pushes(state: &State) {
    let phones = &state.board.phones;
    for seat in state.board.seats.values() {
        let Some(client) = seat.client else { continue };
        let now = Some(phones.out.is_some() && phones.devices.contains_key(&client));
        seat.pushable.send_if_modified(|was| std::mem::replace(was, now) != now);
    }
}

/// The record by which `summary`'s program waits on the person (`OSC 7501`), if one does.
pub(super) fn program_blocked(summary: &SessionSummary) -> Option<&ProgramStatus> {
    summary.program.iter().find(|r| r.state == ProgramState::Blocked)
}

/// A terminal's program records moved: `was` they had one waiting on the person, `now` the
/// summary as it stands (none once the terminal closed).
///
/// One that comes to wait, in a terminal no agent's thread is seated at (the thread speaks for
/// it), is pushed to the phones when the person is at none of their clients. A linked client
/// posts a program's records itself, so no link is told. One that stops waiting, or closes, is
/// taken back from the phones it was pushed to.
pub(super) fn program_moved(
    board: &mut Board,
    term: TermRef,
    was: bool,
    now: Option<&SessionSummary>,
) {
    let Some((summary, record)) = now.and_then(|s| Some((s, program_blocked(s)?))) else {
        board.phones.program_answered(term);
        board.retry_owed();
        return;
    };
    if was || board.thread_in(term).is_some() {
        return;
    }
    let title = [&record.title, &record.app, &summary.title]
        .into_iter()
        .map(|t| t.trim())
        .find(|t| !t.is_empty())
        .unwrap_or_default();
    let text = match (record.message.trim(), record.need.as_deref()) {
        ("", Some(ProgramStatus::PERMISSION)) => "Waits for your approval",
        ("", Some(ProgramStatus::QUESTION)) => "Waits for your answer",
        ("", Some(ProgramStatus::AUTH)) => "Waits for you to sign in",
        ("", _) => "Waits for you",
        (message, _) => message,
    };
    let notice = Notice {
        kind: NoticeKind::NeedsYou,
        about: Subject::Terminal(term),
        title: title.to_owned(),
        text: text.to_owned(),
        worked_ms: None,
        via: None,
    };
    if route(board, &notice).away {
        board.phones.send(&board.seats, &notice, Pushing::default());
    }
}

/// `worker` is linked again, with programs waiting on the person in the terminals `waiting`:
/// a program pushed while it was away that waits no more, or whose terminal went, is taken
/// back. A new wait that came while it was away is no news, as a thread first seen is not.
pub(super) fn programs_back(board: &mut Board, worker: WorkerId, waiting: &[SessionId]) {
    board.phones.take_back_where(|asked| {
        matches!(asked, Asked::Terminal(t) if t.worker == worker && !waiting.contains(&t.session))
    });
    board.retry_owed();
}

/// Send `notice` to the links [`route`] picks among the board's seats, and push it to the
/// phones when it finds the person at none of them, as `how` says: a finished turn's note to
/// take back once the turn is seen.
fn tell_pushing(board: &mut Board, notice: &Notice, how: Pushing<'_>) {
    let reach = route(board, notice);
    let mut unheard = Vec::new();
    for link in &reach.links {
        let Some(seat) = board.seats.get(link) else { continue };
        if seat.tx.try_send(FromServer::Notice(Box::new(notice.clone()))).is_err() {
            tracing::debug!(link, "a notice found its link full or gone");
            unheard.push(*link);
        }
    }
    // A notice no link it went to could take is pushed, as though the person were away.
    let lost = !reach.links.is_empty() && unheard.len() == reach.links.len();
    if reach.away || lost {
        board.phones.send(&board.seats, notice, Pushing { unheard: &unheard, ..how });
    }
}

/// A thread that hangs from no other, standing as high as its subagents.
struct Root<'a> {
    ranked: Ranked,
    row: &'a ThreadRow,
}

/// Where `row`'s thread sits: the seat it was started at, as its row's [`SEAT_FACT`] says.
pub(super) fn seat_of(row: &ThreadRow) -> Option<SessionId> {
    row.facts.get(SEAT_FACT)?.parse().ok()
}

/// The agent that started `row`'s, as its row's [`PARENT_FACT`] says.
pub(super) fn parent_of(row: &ThreadRow) -> Option<ThreadId> {
    row.facts.get(PARENT_FACT)?.parse().ok()
}

/// Whether `row`'s thread is still to be had: its process has not ended.
pub(super) const fn there(row: &ThreadRow) -> bool {
    !matches!(row.status.liveness, Liveness::Exited { .. })
}

/// The thread `row` hangs from in `table`, through every parent there; itself when none.
fn root_of(table: &BTreeMap<ThreadId, ThreadRow>, row: &ThreadRow) -> ThreadId {
    let mut at = row.id;
    // A parent chain longer than the table loops: it ends where it started over.
    for _ in 0..table.len() {
        match table.get(&at).and_then(|r| r.parent.as_ref()).map(|p| p.thread) {
            Some(parent) if table.contains_key(&parent) => at = parent,
            _ => break,
        }
    }
    at
}

/// Where `family`, hanging from `root`, stands together, and the row that puts it there: the
/// root's own when it stands highest, else the subagent there longest.
///
/// A subagent that needs the person always lifts its root. One that failed reads as working
/// while anyone in the family still works or waits, since its parent may well carry on
/// without it, and lifts the root to failed only once the whole family is at rest.
fn source<'a>(root: &'a ThreadRow, family: &[&'a ThreadRow]) -> (Rung, &'a ThreadRow) {
    let busy = std::iter::once(root)
        .chain(family.iter().copied())
        .any(|r| matches!(Rung::of(r), Rung::Working | Rung::Waiting));
    let rung = |row: &ThreadRow| match Rung::of(row) {
        Rung::Failed if busy && row.id != root.id => Rung::Working,
        rung => rung,
    };
    // From the root's own rung: no default stands in, since asleep stands below idle.
    let top = family.iter().map(|r| rung(r)).fold(rung(root), Ord::max);
    if rung(root) == top {
        return (top, root);
    }
    let from = family
        .iter()
        .copied()
        .filter(|r| rung(r) == top)
        .min_by_key(|r| (r.status.since_ms, r.id))
        .unwrap_or(root);
    (top, from)
}

/// Every thread of `table` that hangs from no other, with its subagents.
fn families(table: &BTreeMap<ThreadId, ThreadRow>) -> BTreeMap<ThreadId, Vec<&ThreadRow>> {
    let mut families: BTreeMap<ThreadId, Vec<&ThreadRow>> = BTreeMap::new();
    for row in table.values() {
        let root = root_of(table, row);
        let family = families.entry(root).or_default();
        if root != row.id {
            family.push(row);
        }
    }
    families
}

/// Every worker's threads that hang from no other, each standing as high as its subagents.
fn roots(tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>) -> Vec<Root<'_>> {
    let mut roots: Vec<Root<'_>> = tables
        .iter()
        .flat_map(|(worker, table)| {
            families(table).into_iter().filter_map(|(root, family)| {
                let row = table.get(&root)?;
                let (rung, from) = source(row, &family);
                let at = ThreadAt { worker: *worker, thread: root };
                let ranked = Ranked { at, rung, since_ms: from.status.since_ms };
                Some(Root { ranked, row })
            })
        })
        .collect();
    roots.sort_by_key(|r| r.ranked.at);
    roots
}

/// The ladder of `tables`: each agent's tree, by the parent its row names across workers,
/// and each space, by the clone or folder its agent works in.
fn ladder(tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>) -> Ladder {
    let roots = roots(tables);
    let mut tiles: BTreeMap<(WorkerId, SessionId), Standing> = BTreeMap::new();
    let mut workers: BTreeMap<WorkerId, Standing> = BTreeMap::new();
    let mut spaces: BTreeMap<SpaceAt, Standing> = BTreeMap::new();
    let mut fleet = Standing::default();
    let at_of: HashMap<ThreadId, ThreadAt> =
        roots.iter().map(|r| (r.ranked.at.thread, r.ranked.at)).collect();
    let parents: HashMap<ThreadId, ThreadId> =
        roots.iter().filter_map(|r| Some((r.ranked.at.thread, parent_of(r.row)?))).collect();
    let mut trees: BTreeMap<ThreadAt, Standing> = BTreeMap::new();
    for root in &roots {
        let ranked = &root.ranked;
        fleet.add(ranked);
        workers.entry(ranked.at.worker).or_default().add(ranked);
        if let Some(session) = seat_of(root.row) {
            tiles.entry((ranked.at.worker, session)).or_default().add(ranked);
        }
        if let Some(folder) = root.row.repo.as_ref().or(root.row.cwd.as_ref()) {
            let space = SpaceAt { worker: ranked.at.worker, folder: folder.clone() };
            spaces.entry(space).or_default().add(ranked);
        }
        // Each agent above it, at any depth: a chain longer than the fleet loops.
        let mut at = ranked.at.thread;
        for _ in 0..roots.len() {
            let Some(parent) = parents.get(&at).copied() else { break };
            let Some(parent_at) = at_of.get(&parent).copied() else { break };
            trees.entry(parent_at).or_default().add(ranked);
            if parent == ranked.at.thread {
                break;
            }
            at = parent;
        }
    }
    // A tree holds the agent at its top too.
    for root in &roots {
        if let Some(tree) = trees.get_mut(&root.ranked.at) {
            tree.add(&root.ranked);
        }
    }
    Ladder {
        threads: roots.iter().map(|r| r.ranked).collect(),
        tiles: tiles
            .into_iter()
            .map(|((worker, session), s)| (TermRef { worker, session }, s))
            .collect(),
        workers: workers.into_iter().collect(),
        trees: trees.into_iter().collect(),
        spaces: spaces.into_iter().collect(),
        fleet,
    }
}

/// A notice the ladder made ([`moved`]).
struct Told {
    /// The notice.
    notice: Notice,
    /// The turn it says finished.
    turn: Option<TurnId>,
    /// It is about a thread first seen already needing the person: after the server started,
    /// or its worker linked again. The clients that held it hold it still, so no link is told;
    /// only a phone not already showing it is pushed, when the person is at no client.
    first: bool,
}

/// The notices `ladder` makes against the one `board` published, keeping how long each
/// thread has been busy. A thread first seen needing the person counts too ([`Told::first`]).
/// An agent another started that finished says nothing: its parent hears of it.
fn moved(board: &mut Board, ladder: &Ladder) -> Vec<Told> {
    let before: HashMap<ThreadAt, Rung> =
        board.published.threads.iter().map(|r| (r.at, r.rung)).collect();
    let busy = |rung: Rung| matches!(rung, Rung::Working | Rung::Waiting);
    let mut notices = Vec::new();
    for now in &ladder.threads {
        let was = before.get(&now.at).copied();
        if busy(now.rung) {
            board.busy.entry(now.at).or_insert(now.since_ms);
        }
        // A wait on the person is part of the work; coming to rest or failing ends it.
        let rest = matches!(now.rung, Rung::Idle | Rung::Failed);
        let kind = match (was, now.rung) {
            (None, Rung::NeedsYou) => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::NeedsYou) if was != Rung::NeedsYou => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::Failed) if was != Rung::Failed => Some(NoticeKind::Failed),
            (Some(was), Rung::Idle) if busy(was) => Some(NoticeKind::Finished),
            _ => None,
        };
        let since = if rest { board.busy.remove(&now.at) } else { None };
        let worked_ms =
            since.map(|since| now.since_ms.as_millis().saturating_sub(since.as_millis()));
        let Some(kind) = kind else { continue };
        let finished = kind == NoticeKind::Finished;
        // An agent that ended with its work under way (it quit, or its worker cut it short as
        // it restarted, to read it again) finished nothing: only a turn it answered since it
        // got busy is a finish.
        let row = board.tables.get(&now.at.worker).and_then(|t| t.get(&now.at.thread));
        if finished && row.is_some_and(|r| !there(r) && !answered_since(r, since)) {
            continue;
        }
        let worked_ms = finished.then_some(worked_ms).flatten();
        let Some(notice) = notice_of(board, now.at, (kind, worked_ms)) else {
            continue;
        };
        let turn = row.and_then(|r| r.ended.as_ref()).map(|e| e.turn).filter(|_| finished);
        notices.push(Told { notice, turn, first: was.is_none() });
    }
    let standing: Vec<ThreadAt> = ladder.threads.iter().map(|r| r.at).collect();
    board.busy.retain(|at, _| standing.binary_search(at).is_ok());
    notices
}

/// Whether `row`'s latest turn ended answered, at or after `since` when that is known.
fn answered_since(row: &ThreadRow, since: Option<WallMs>) -> bool {
    row.ended.is_some_and(|e| e.answered && since.is_none_or(|since| e.at_ms >= since))
}

/// The notice of `kind` about the thread at `at`, with how long it worked when it finished;
/// `None` for a thread no table holds, and for the finish of an agent another started, which
/// its parent hears of instead.
fn notice_of(
    board: &Board,
    at: ThreadAt,
    (kind, worked_ms): (NoticeKind, Option<u64>),
) -> Option<Notice> {
    let table = board.tables.get(&at.worker)?;
    let row = table.get(&at.thread)?;
    if kind == NoticeKind::Finished && parent_of(row).is_some() {
        return None;
    }
    let family: Vec<&ThreadRow> =
        table.values().filter(|r| r.id != row.id && root_of(table, r) == row.id).collect();
    let (_, from) = source(row, &family);
    let via = (from.id != row.id).then(|| Via { thread: from.id, title: from.title.clone() });
    Some(Notice {
        kind,
        about: Subject::Thread(at),
        title: row.title.clone(),
        text: text(kind, from),
        worked_ms,
        via,
    })
}

/// Whether `presence` has the person at a desk: one whose notices stay there when they leave.
fn at_desk(presence: Option<&Presence>) -> bool {
    presence.is_some_and(|p| p.active && p.seat == Seat::Desk)
}

/// The person left the desk they were at ([`at_desk`]): each thread that still needs them is
/// pushed now, when they are at no client, to each phone that does not show it yet, as it would
/// have been had they been away when it came. A notice told at the desk stays on it; one told
/// on the phone in their hand goes with them, so leaving that pushes nothing. The clients
/// already hold it, so no link is told it again, and a phone already showing it, by the kept
/// push state, is not pushed twice.
fn left(board: &mut Board) {
    let waiting: Vec<ThreadAt> =
        board.published.threads.iter().filter(|r| r.rung == Rung::NeedsYou).map(|r| r.at).collect();
    for at in waiting {
        let Some(notice) = notice_of(board, at, (NoticeKind::NeedsYou, None)) else {
            continue;
        };
        if route(board, &notice).away {
            let how = Pushing { new_only: true, ..Pushing::default() };
            board.phones.send(&board.seats, &notice, how);
        }
    }
}

/// What a notice of `kind` says of `row`, in a line.
fn text(kind: NoticeKind, row: &ThreadRow) -> String {
    let wait = row.status.wait.as_ref().map(|w| w.text.clone()).filter(|t| !t.is_empty());
    match kind {
        NoticeKind::NeedsYou => wait.unwrap_or_default(),
        NoticeKind::Failed | NoticeKind::Finished => {
            wait.or_else(|| row.last_line.clone()).unwrap_or_default()
        }
    }
}

/// Where a notice goes.
#[derive(Debug, PartialEq, Eq)]
struct Reach {
    /// The links it is sent on.
    links: Vec<u64>,
    /// Whether it finds the person at none of them: its tile shown nowhere and no client in
    /// use, so it goes to every link and the phones are pushed.
    away: bool,
}

/// Where `notice` goes among the board's seats: nowhere while what it is about is on a screen,
/// a terminal or the seat its thread was started at.
fn route(board: &Board, notice: &Notice) -> Reach {
    let seats = &board.seats;
    let tile = match notice.about {
        Subject::Terminal(term) => Some(term),
        Subject::Thread(at) => board
            .tables
            .get(&at.worker)
            .and_then(|t| t.get(&at.thread))
            .and_then(seat_of)
            .map(|session| TermRef { worker: at.worker, session }),
    };
    let at = |seat: Option<Seat>| {
        seats
            .iter()
            .filter(|(_, s)| {
                s.presence.as_ref().is_some_and(|p| p.active && seat.is_none_or(|k| p.seat == k))
            })
            .map(|(link, _)| *link)
            .collect::<Vec<_>>()
    };
    let shown = seats
        .values()
        .filter_map(|s| s.presence.as_ref())
        .any(|p| p.active && tile.is_some_and(|t| p.showing.contains(&t) || p.focus == Some(t)));
    if shown {
        return Reach { links: Vec::new(), away: false };
    }
    let desks = at(Some(Seat::Desk));
    if !desks.is_empty() {
        return Reach { links: desks, away: false };
    }
    let held = at(Some(Seat::Handheld));
    if !held.is_empty() {
        return Reach { links: held, away: false };
    }
    Reach { links: seats.keys().copied().collect(), away: true }
}

#[cfg(test)]
pub(super) mod tests;
