//! The fleet's attention ladder, and the notices it sends by where the person is
//! (`slopty_proto::thread::attention`, `docs/decisions/agents.md`).
//!
//! Each worker publishes its thread table
//! ([`slopty_proto::server::ToServer::Threads`]); the hub keeps the rows and ranks them again
//! whenever a row, a terminal or a project moves: a subagent folds into the thread it hangs from,
//! and the rest roll up per tile, worker, project node, project and the fleet. A ladder that
//! differs from the last goes to every link.
//!
//! A thread that hangs from no other and climbs to needing the person, fails, or comes to rest
//! from working, is a notice; a project task's agent coming to rest is not, since its work
//! comes to the person when it is ready to merge and its orchestrator hears of the rest. It goes to
//! no client when the thread's tile is on screen where the person is, to the desks they are at when
//! they are at one, to the handhelds they hold when not, and to every client when they are at none.
//!
//! When a notice finds the person at none of their clients, it is pushed as well to every phone
//! that gave the server a device and is not listening on a live link ([`Phones`],
//! `slopty_proto::push`): sealed to the phone, through the relay or straight to APNs
//! ([`crate::push`]). Once a thread a phone was pushed an ask about stops needing the person
//! (answered at another client, or in its terminal), the note is taken back with a background
//! push ([`Phones::take_back`]). A take-back the push queue cannot take now is owed, and tried
//! again after [`TAKE_BACK_RETRY`] unless a newer note about the same thread replaced it. What
//! each phone shows and what is owed it are kept with the phones ([`PushKept`]), so a server
//! that restarts still takes them back. A notice every link it went to could not take is pushed
//! as well, as though the person were away. A note that a thread needs the person follows the
//! request its buttons answer, pushed again quietly when that moves ([`Phones::follow`]); one
//! that a turn finished is taken back once the person has seen the turn ([`Shown::finished`]).
//! A thread first seen already needing the person is pushed to a phone not showing it
//! ([`Told::first`]).
//!
//! A project's change that holds its work up is a notice too ([`tell_project`]): its pull
//! request (as its thread's row names it) failing a check, asked to change or conflicting, its
//! verifier or a step for it failing (a rebase that conflicts among them), or the push after its
//! merge not going. It is one notice per timeline entry, about the project, routed by the
//! orchestrator's terminal as a thread's is by its own.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use slopty_agent::status::{AgentStatus, BlockReason};
use slopty_core::{ClientId, SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::{TermAgent, TermRef};
use slopty_proto::project::{
    AgentReport, Merge, Moment, SEAT_FACT, StepKind, StepState, Task, TaskId, TaskStep,
    TimelineEntry,
};
use slopty_proto::push::{PushBody, PushDevice};
use slopty_proto::server::FromServer;
use slopty_proto::terminal::{ProgramState, ProgramStatus, SessionSummary};
use slopty_proto::thread::attention::{
    Ladder, NodeAt, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat, Standing, Subject,
    ThreadAt, Via,
};
use slopty_proto::thread::wire::{PullSeen, TableFrame, ThreadRow};
use slopty_proto::thread::{AgentId, AskId, Liveness, Phase, Request, ThreadId, TurnId, Wait};
use tokio::sync::{Notify, broadcast, mpsc, watch};

use super::awake::{Awake, Hold, Policy as KeepAwake};
use super::{Hub, State, WeakHub};
use crate::project::{Kept, Projects};
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
    /// What the agent at each seat was last said to be doing, by its seat: the terminal its
    /// TUI runs in, or the seat a task's thread was started at ([`Self::seat_moves`]).
    seat_said: HashMap<TermRef, AgentStatus>,
    /// Where each thread hanging from no other was last said to stand ([`Self::rung_moves`]).
    rungs_said: HashMap<(WorkerId, ThreadId), TermAgent>,
    /// Every task's thread a table has shown, by its worker: one gone from it since ended.
    seen: HashSet<(WorkerId, ThreadId)>,
    /// Each subagent thread last told to the projects as a native ([`Self::native_moves`]),
    /// with the seat it was told under and whether it had stopped.
    natives_said: HashMap<(WorkerId, ThreadId), (SessionId, bool)>,
    /// Each Codex thread's approval policy and sandbox last said ([`Self::codex_moves`]).
    codex_said: HashMap<(WorkerId, ThreadId), CodexSettings>,
    /// The hold on the server's machine that the seats and the tables imply.
    awake: Awake,
    /// The phones notices are pushed to when the person is at no client.
    phones: Phones,
    /// Each project whose work turned ready to merge and is still to be told, with the
    /// timeline entry that made the latest ready ([`Hub::ready_soon`]).
    ready_due: HashMap<slopty_proto::project::ProjectId, u64>,
}

/// A Codex thread's approval policy and sandbox, as its row says them.
pub(super) type CodexSettings = (Option<String>, Option<String>);

/// How long a take-back the push queue could not take waits before it is tried again.
pub(crate) const TAKE_BACK_RETRY: Duration = Duration::from_secs(2);

/// The phones the server may push to, and where their pushes go.
#[derive(Debug, Default)]
struct Phones {
    /// Each phone, by its client.
    devices: Devices,
    /// Where pushes go to be sealed and sent: none while pushing is off.
    out: Option<mpsc::Sender<Outgoing>>,
    /// Where the phones go to be kept ([`crate::store::PushStore`]).
    kept: Option<watch::Sender<PushKept>>,
    /// [`Self::answerable`], as every worker's link sends it: a word each link takes the
    /// latest of, so none is dropped behind a full queue.
    said: watch::Sender<bool>,
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
    /// The request each of [`Self::asked`] answers with its buttons, as last pushed, so a
    /// request that opens or goes while the thread still needs the person moves them quietly
    /// ([`Phones::follow`]). Not kept: a server started again moves each note once.
    asks: HashMap<(ClientId, Asked), Option<AskId>>,
    /// The notes about a finished turn, with the turn, taken back once the person has seen it
    /// on any device ([`ThreadRow::seen`]).
    finished: BTreeMap<ClientId, HashSet<(ThreadAt, TurnId)>>,
}

impl Shown {
    /// The push queue took `notice` for `client`'s phone, its buttons answering `ask`, about
    /// `turn` when it says a turn finished. The phone shows one note per thread or terminal, so
    /// this one replaced whatever was up there.
    fn noted(
        &mut self,
        client: ClientId,
        notice: &Notice,
        ask: Option<&AskId>,
        turn: Option<TurnId>,
    ) {
        let Some(about) = asked_about(&notice.about) else { return };
        let finished = self.finished.entry(client).or_default();
        if let Asked::Thread(at) = about {
            finished.retain(|(t, _)| *t != at);
        }
        let asked = self.asked.entry(client).or_default();
        if notice.kind == NoticeKind::NeedsYou {
            asked.insert(about);
            self.asks.insert((client, about), ask.cloned());
            return;
        }
        asked.remove(&about);
        self.asks.remove(&(client, about));
        if let (NoticeKind::Finished, Asked::Thread(at), Some(turn)) = (notice.kind, about, turn) {
            finished.insert((at, turn));
        }
    }

    /// Forget every phone's empty sets, and the requests of notes no longer up.
    fn tidy(&mut self) {
        self.asked.retain(|_, asked| !asked.is_empty());
        self.finished.retain(|_, finished| !finished.is_empty());
        let asked = &self.asked;
        self.asks.retain(|(client, about), _| asked.get(client).is_some_and(|a| a.contains(about)));
    }

    /// Forget what the phone of `client` shows.
    fn forget(&mut self, client: ClientId) {
        self.asked.remove(&client);
        self.finished.remove(&client);
        self.asks.retain(|(c, _), _| *c != client);
    }
}

/// How a notice is pushed ([`Phones::send`]).
#[derive(Clone, Copy, Debug, Default)]
struct Pushing<'a> {
    /// The request its note's buttons answer.
    ask: Option<&'a Ask>,
    /// The links that could not take it, so their phones do not count as listening.
    unheard: &'a [u64],
    /// Only to the phones not already showing a note about its subject that needs the person.
    new_only: bool,
    /// The task a ready-to-merge note's Merge merges.
    merges: Option<TaskId>,
    /// The turn a finished turn's note is about, to take it back once seen.
    turn: Option<TurnId>,
}

impl Phones {
    /// Whether a pocketed phone can answer a yes or no: pushing is set up and a phone is known.
    fn answerable(&self) -> bool {
        self.out.is_some() && !self.devices.is_empty()
    }

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

    /// Push `notice` to every phone not listening on a live link among `seats`, as `how` says:
    /// with the request its note's buttons answer, skipping the phones already showing it when
    /// only new ones are to hear. A finished turn shorter than a phone's quiet time is not
    /// pushed to it, as that phone would not post it. A link among those `how` names unheard
    /// could not take the notice, so its phone is not counted as listening.
    fn send(&mut self, seats: &BTreeMap<u64, Sitting>, notice: &Notice, how: Pushing<'_>) {
        let Some(out) = &self.out else { return };
        let about = asked_about(&notice.about);
        for (client, device) in &self.devices {
            let shown =
                |about: Asked| self.shown.asked.get(client).is_some_and(|a| a.contains(&about));
            if how.new_only && about.is_some_and(shown) {
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
            let body = PushBody {
                notice: notice.clone(),
                ask: how.ask.map(|a| a.id.clone()),
                picks: how.ask.map(|a| a.picks.clone()).unwrap_or_default(),
                quiet: false,
                merges: how.merges,
            };
            // A note waiting for room, or a take-back owed, about the same subject is older
            // than this one, which replaces it on the phone.
            self.owed_notes
                .retain(|(c, owed, _)| *c != *client || owed.notice.about != notice.about);
            if let Some(about) = about
                && let Some(owed) = self.owed.get_mut(client)
            {
                owed.remove(&about);
            }
            let ask = body.ask.clone();
            let push =
                Outgoing { client: *client, device: device.clone(), what: Sending::Note(body) };
            match out.try_send(push) {
                Ok(()) => self.shown.noted(*client, notice, ask.as_ref(), how.turn),
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

    /// The threads whose notes each phone shows as needing the person, for [`Self::follow`].
    fn asking(&self) -> Vec<(ClientId, ThreadAt)> {
        let threads = self.shown.asked.iter().flat_map(|(client, asked)| {
            asked.iter().filter_map(|a| match a {
                Asked::Thread(at) => Some((*client, *at)),
                Asked::Terminal(_) => None,
            })
        });
        threads.collect()
    }

    /// The phone of `client` shows a note that `notice`'s thread needs the person: when the
    /// request its buttons answer is no longer `ask` (one opened after the note went, or the
    /// one it showed went), the note is pushed again quietly, `ask` its buttons' now, to
    /// replace it under the same collapse id with no sound and no banner.
    fn follow(&mut self, client: ClientId, notice: &Notice, ask: Option<&Ask>) {
        let Some(about) = asked_about(&notice.about) else { return };
        let now = ask.map(|a| a.id.clone());
        if self.shown.asks.get(&(client, about)) == Some(&now) {
            return;
        }
        let (Some(out), Some(device)) = (&self.out, self.devices.get(&client)) else { return };
        let body = PushBody {
            notice: notice.clone(),
            ask: now.clone(),
            picks: ask.map(|a| a.picks.clone()).unwrap_or_default(),
            quiet: true,
            merges: None,
        };
        self.owed_notes.retain(|(c, owed, _)| *c != client || owed.notice.about != notice.about);
        let push = Outgoing { client, device: device.clone(), what: Sending::Note(body) };
        match out.try_send(push) {
            Ok(()) => self.shown.noted(client, notice, now.as_ref(), None),
            Err(mpsc::error::TrySendError::Full(push)) => {
                tracing::debug!(%client, "a quiet push found its queue full; owed");
                if let Sending::Note(body) = push.what {
                    self.owed_notes.push((client, body, None));
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
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
            devices.contains_key(client)
                && asked_about(&body.notice.about).is_none_or(|about| !answered(&about))
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
                let (notice, ask) = (body.notice.clone(), body.ask.clone());
                let push = Outgoing { client, device: device.clone(), what: Sending::Note(body) };
                match out.try_send(push) {
                    Ok(()) => self.shown.noted(client, &notice, ask.as_ref(), turn),
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
/// thread or terminal; a project's note has none.
const fn asked_about(subject: &Subject) -> Option<Asked> {
    match subject {
        Subject::Thread(at) => Some(Asked::Thread(*at)),
        Subject::Terminal(term) => Some(Asked::Terminal(*term)),
        Subject::Project { .. } => None,
    }
}

impl Board {
    /// `project`'s work turned ready at the timeline's `entry`: whether it is the first since
    /// the person was last told, so a tell is to be set ([`Hub::ready_soon`]).
    pub(super) fn ready_due(
        &mut self,
        project: &slopty_proto::project::ProjectId,
        entry: u64,
    ) -> bool {
        self.ready_due.insert(project.clone(), entry).is_none()
    }

    /// The entry `project`'s ready work is to be told at, now, once.
    pub(super) fn ready_now(&mut self, project: &slopty_proto::project::ProjectId) -> Option<u64> {
        self.ready_due.remove(project)
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

    /// Whether a person's client is linked, and whether an agent is at work on any worker: a
    /// thread whose agent is there working, or waiting on its own background work.
    fn linked_and_working(&self) -> (bool, bool) {
        let working = self
            .tables
            .values()
            .flat_map(BTreeMap::values)
            .any(|r| there(r) && matches!(r.status.phase, Phase::Working | Phase::Waiting));
        (!self.seats.is_empty(), working)
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
        self.seen.extend(table.values().filter(|r| seat_fact(r).is_some()).map(|r| (worker, r.id)));
    }

    /// The thread seated at `term` (its TUI runs there, or a task's thread was started there)
    /// and hanging from no other, the latest to change when there were several.
    fn thread_in(&self, term: TermRef) -> Option<&ThreadRow> {
        let table = self.tables.get(&term.worker)?;
        table
            .values()
            .filter(|r| seat_of(r) == Some(term.session) && root_of(table, r) == r.id)
            .max_by_key(|r| (r.updated_ms, r.id))
    }

    /// The thread whose agent ran or is seated at `term`, there or exited: its id, its agent
    /// and the folder it worked in.
    pub(super) fn ran_at(&self, term: TermRef) -> Option<(ThreadId, AgentId, Option<String>)> {
        self.thread_in(term).map(|r| (r.id, r.agent.clone(), r.cwd.clone()))
    }

    /// The thread whose agent runs or is seated at `term`, hanging from no other.
    pub(super) fn thread_at(&self, term: TermRef) -> Option<ThreadId> {
        self.thread_in(term).map(|r| r.id)
    }

    /// The worker whose table holds `thread`.
    pub(super) fn worker_of(&self, thread: ThreadId) -> Option<WorkerId> {
        self.tables.iter().find(|(_, table)| table.contains_key(&thread)).map(|(w, _)| *w)
    }

    /// The seat `session` of a task's thread on any worker, as its row's [`SEAT_FACT`] says.
    pub(super) fn seat(&self, session: SessionId) -> Option<TermRef> {
        self.tables.iter().find_map(|(worker, table)| {
            table
                .values()
                .any(|r| seat_fact(r) == Some(session))
                .then_some(TermRef { worker: *worker, session })
        })
    }

    /// The thread a task's start seated at `term`, as its row's [`SEAT_FACT`] says.
    pub(super) fn seated_thread(&self, term: TermRef) -> Option<ThreadId> {
        let table = self.tables.get(&term.worker)?;
        table.values().find(|r| seat_fact(r) == Some(term.session)).map(|r| r.id)
    }

    /// Every task's thread with no terminal of its own whose agent runs, by its seat: it
    /// counts as a live terminal does. One asleep does not, since its agent ended.
    pub(super) fn live_seats(&self) -> Vec<TermRef> {
        self.tables
            .iter()
            .flat_map(|(worker, table)| {
                table
                    .values()
                    .filter(|r| r.terminal.is_none() && there(r))
                    .filter_map(|r| Some(TermRef { worker: *worker, session: seat_fact(r)? }))
            })
            .collect()
    }

    /// Whether the thread `thread` on `worker` is in its table with its agent there; `None`
    /// when the worker has published no table.
    pub(super) fn thread_there(&self, worker: WorkerId, thread: ThreadId) -> Option<bool> {
        let table = self.tables.get(&worker)?;
        Some(table.get(&thread).is_some_and(there))
    }

    /// Whether a table of `worker`'s has shown the task's thread `thread`.
    pub(super) fn seen(&self, worker: WorkerId, thread: ThreadId) -> bool {
        self.seen.contains(&(worker, thread))
    }

    /// What the agent seated at `term` is doing, as an agent's status reads; `None` with no
    /// thread there, and [`AgentStatus::None`] once its agent ended.
    pub(super) fn seat_status(&self, term: TermRef) -> Option<AgentStatus> {
        self.thread_in(term).map(status_of)
    }

    /// The agent at work or seated at `term`, as its thread's row says; `None` with none there
    /// or once its agent ended.
    pub(super) fn agent_at(&self, term: TermRef) -> Option<TermAgent> {
        self.thread_in(term).filter(|r| there(r)).map(TermAgent::of)
    }

    /// Each seat on `worker` whose agent's status moved since last asked, with its status now:
    /// a terminal its TUI runs in, or the seat a task's thread was started at. A seat whose
    /// thread left the table says [`AgentStatus::None`] once, and is forgotten.
    pub(super) fn seat_moves(&mut self, worker: WorkerId) -> Vec<(TermRef, AgentStatus)> {
        let mut now: HashMap<TermRef, (WallMs, ThreadId, AgentStatus)> = HashMap::new();
        if let Some(table) = self.tables.get(&worker) {
            for r in table.values().filter(|r| root_of(table, r) == r.id) {
                let Some(session) = seat_of(r) else { continue };
                let latest = (r.updated_ms, r.id, status_of(r));
                let term = TermRef { worker, session };
                // Several threads at one seat: the latest to change speaks for it.
                if now.get(&term).is_none_or(|(at, id, _)| (*at, *id) < (latest.0, latest.1)) {
                    now.insert(term, latest);
                }
            }
        }
        let mut moves = Vec::new();
        self.seat_said.retain(|term, said| {
            if term.worker != worker || now.contains_key(term) {
                return true;
            }
            if *said != AgentStatus::None {
                moves.push((*term, AgentStatus::None));
            }
            false
        });
        let mut now: Vec<_> = now.into_iter().collect();
        now.sort_by_key(|(term, _)| (term.worker, term.session));
        for (term, (_, _, status)) in now {
            if self.seat_said.insert(term, status.clone()).as_ref() != Some(&status) {
                moves.push((term, status));
            }
        }
        moves
    }

    /// What each seat's thread on `worker` says of its branch's pull request, the latest to
    /// change speaking for a seat several threads share: what a task's card follows.
    pub(super) fn pulls(&self, worker: WorkerId) -> Vec<(SessionId, Option<PullSeen>)> {
        let Some(table) = self.tables.get(&worker) else { return Vec::new() };
        let mut latest: HashMap<SessionId, &ThreadRow> = HashMap::new();
        for r in table.values().filter(|r| root_of(table, r) == r.id) {
            let Some(seat) = seat_of(r) else { continue };
            let later = |was: &&ThreadRow| (was.updated_ms, was.id) < (r.updated_ms, r.id);
            if latest.get(&seat).is_none_or(later) {
                latest.insert(seat, r);
            }
        }
        latest.into_iter().map(|(seat, r)| (seat, r.pull.clone())).collect()
    }

    /// Each thread on `worker` hanging from no other whose rung, phase or ask moved since last
    /// asked, with the terminal its TUI runs in: what [`Happening::Rung`] tells. One gone from
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
                (was.rung, was.phase, &was.asks, was.wait.as_ref().map(|w| &w.kind))
                    != (now.rung, now.phase, &now.asks, now.wait.as_ref().map(|w| &w.kind))
            });
            if moved {
                self.rungs_said.insert((worker, row.id), now.clone());
                moves.push((row.terminal, now));
            }
        }
        moves
    }

    /// Each Codex thread on `worker` at a seat, hanging from no other, whose approval policy
    /// (its mode) or sandbox (its fact) moved since last asked, with that seat: what holds a
    /// task's Codex to asking (`Hub::codex_settings`). One gone from the table is forgotten.
    pub(super) fn codex_moves(&mut self, worker: WorkerId) -> Vec<(SessionId, CodexSettings)> {
        let Some(table) = self.tables.get(&worker) else { return Vec::new() };
        let codex: Vec<(&ThreadRow, SessionId)> = table
            .values()
            .filter(|r| r.agent.is(AgentId::CODEX) && root_of(table, r) == r.id)
            .filter_map(|r| Some((r, seat_of(r)?)))
            .collect();
        self.codex_said.retain(|(w, id), _| *w != worker || codex.iter().any(|(r, _)| r.id == *id));
        let mut moves = Vec::new();
        for (row, seat) in codex {
            let now = (row.meters.mode.clone(), row.facts.get("sandbox").cloned());
            if self.codex_said.get(&(worker, row.id)) != Some(&now) {
                self.codex_said.insert((worker, row.id), now.clone());
                moves.push((seat, now));
            }
        }
        moves
    }

    /// Each subagent thread on `worker` that started or stopped since last asked, as the
    /// report a hook would make of it under the seat its family runs at: the natives of
    /// every agent but Claude Code, whose own hooks report its subagents. A subagent gone
    /// from the table stopped.
    pub(super) fn native_moves(&mut self, worker: WorkerId) -> Vec<AgentReport> {
        let mut reports = Vec::new();
        let table = self.tables.get(&worker);
        let now: Vec<(ThreadId, SessionId, &ThreadRow)> = table
            .map(|table| {
                table
                    .values()
                    .filter_map(|row| {
                        let root = table.get(&root_of(table, row)).filter(|r| r.id != row.id)?;
                        let hooked = root.agent == AgentId::named(AgentId::CLAUDE_CODE);
                        Some((row.id, seat_of(root).filter(|_| !hooked)?, row))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (id, session, row) in &now {
            let at_work =
                matches!(row.status.phase, Phase::Working | Phase::Waiting | Phase::NeedsYou);
            let stopped = !there(row) || !at_work;
            let said = self.natives_said.insert((worker, *id), (*session, stopped));
            let agent = id.to_string();
            if said.is_none() {
                let kind = Some(row.title.trim()).filter(|t| !t.is_empty());
                let kind = kind.map_or_else(|| row.agent.0.clone(), str::to_owned);
                let session = *session;
                reports.push(AgentReport::SubagentStarted { session, agent: agent.clone(), kind });
            }
            if stopped && said.is_none_or(|(_, was)| !was) {
                let last = row.last_line.clone().filter(|l| !l.trim().is_empty());
                let session = *session;
                reports.push(AgentReport::SubagentStopped {
                    session,
                    agent,
                    transcript: None,
                    last,
                });
            }
        }
        self.natives_said.retain(|(w, id), (session, stopped)| {
            if *w != worker || now.iter().any(|(t, ..)| t == id) {
                return true;
            }
            if !*stopped {
                let (session, agent) = (*session, id.to_string());
                reports.push(AgentReport::SubagentStopped {
                    session,
                    agent,
                    transcript: None,
                    last: None,
                });
            }
            false
        });
        reports
    }

    /// The last line the agent in `term` wrote, as its thread's row says.
    pub(super) fn last_words(&self, term: TermRef) -> Option<String> {
        self.thread_in(term)?.last_line.clone().filter(|l| !l.trim().is_empty())
    }

    /// What the agent in `term` asks the person, as the first open request on its thread's
    /// row names it.
    pub(super) fn asking(&self, term: TermRef) -> Option<String> {
        let row = self.thread_in(term)?;
        row.requests.first().map(|r| r.title.clone()).filter(|t| !t.trim().is_empty())
    }

    /// Whether any thread seated at `term`, or under one there, has a request open.
    pub(super) fn asks(&self, term: TermRef) -> bool {
        self.tables.get(&term.worker).is_some_and(|table| {
            table.values().any(|r| {
                !r.requests.is_empty()
                    && table.get(&root_of(table, r)).and_then(seat_of) == Some(term.session)
            })
        })
    }

    /// What the agent seated at `term` waits on, when that is only commands it left running
    /// ([`Wait::COMMAND`]): the wait's words.
    pub(super) fn left_running(&self, term: TermRef) -> Option<String> {
        let wait = self.thread_in(term)?.status.wait.as_ref()?;
        (wait.kind == Wait::COMMAND).then(|| wait.text.clone())
    }

    /// Whether `term`'s tile is on screen, or has the keyboard, on any client.
    pub(super) fn shown(&self, term: TermRef) -> bool {
        self.seats
            .values()
            .filter_map(|s| s.presence.as_ref())
            .any(|p| p.showing.contains(&term) || p.focus == Some(term))
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
}

impl Seated {
    /// The server's number for the link.
    #[must_use]
    pub const fn link(&self) -> u64 {
        self.link
    }
}

impl Drop for Seated {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else { return };
        let mut state = hub.inner.state.lock();
        let gone = state.board.seats.remove(&self.link);
        if gone.as_ref().is_some_and(|s| at_desk(s.presence.as_ref())) {
            let state = &mut *state;
            left(&mut state.board, &state.projects);
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
        let mut state = self.inner.state.lock();
        state.board.seats.insert(link, Sitting { name, tx, presence: None, client: None });
        state.board.settle_awake();
        drop(state);
        Seated { hub: self.downgrade(), link }
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
            left(board, &state.projects);
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
        phones.shown = Shown { asked, finished, asks: HashMap::new() };
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
        state.board.settle_awake();
        let ladder = ladder(&state.board.tables, &state.projects);
        let board = &mut state.board;
        board.phones.take_back(&ladder, &board.tables);
        follow_asks(board, &state.projects);
        board.retry_owed();
        if ladder == board.published {
            return;
        }
        let notices = moved(board, &ladder, &state.projects);
        self.announce(FromServer::Ladder(Box::new(ladder.clone())));
        state.board.published = ladder;
        for Told { notice, ask, turn, first } in notices {
            let board = &mut state.board;
            let how = Pushing { ask: ask.as_ref(), turn, ..Pushing::default() };
            if !first {
                tell_pushing(board, &notice, how);
            } else if route(&board.seats, &notice).away {
                board.phones.send(&board.seats, &notice, Pushing { new_only: true, ..how });
            }
        }
        drop(guard);
    }
}

/// Tell every worker linked whether a pocketed phone can answer, once that moved: while one
/// can, a worker holds for it any prompt nobody follows ([`FromServer::Pushes`]).
fn say_pushes(state: &State) {
    let now = state.board.phones.answerable();
    state.board.phones.said.send_if_modified(|said| std::mem::replace(said, now) != now);
}

impl Board {
    /// Whether a pocketed phone can answer, as it moves: each worker's link sends its latest
    /// ([`FromServer::Pushes`]), once after the welcome when one can, then on every change.
    pub(super) fn pushes(&self) -> watch::Receiver<bool> {
        self.phones.said.subscribe()
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
        tile: Some(term),
        title: title.to_owned(),
        text: text.to_owned(),
        worked_ms: None,
        via: None,
    };
    if route(&board.seats, &notice).away {
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

/// A project's change that holds its work up goes to the person as a notice, where they are.
pub(super) fn tell_project(state: &mut State, kept: &Kept) {
    if let Some(notice) = project_notice(&state.projects, kept) {
        tell(&mut state.board, &notice, None);
    }
}

/// A project's orchestrator said its goal is met, at the timeline's `entry`: the person hears
/// it once, where they are, its summary's first line the words ([`NoticeKind::GoalDone`]).
pub(super) fn tell_goal_met(
    state: &mut State,
    project: &slopty_proto::project::ProjectId,
    entry: u64,
) {
    let Ok(record) = state.projects.project(project) else { return };
    let summary = record.progress.as_ref().map_or("", |p| p.summary.as_str());
    let first = summary.lines().next().unwrap_or_default().trim();
    let notice = Notice {
        kind: NoticeKind::GoalDone,
        about: Subject::Project { project: project.clone(), entry },
        tile: record.orchestrator,
        title: record.title.clone(),
        text: format!("Goal met: {first}"),
        worked_ms: None,
        via: None,
    };
    tell(&mut state.board, &notice, None);
}

/// The request a pushed note's buttons answer, and the labels of the answers it offers as
/// buttons of their own: a small question's options; none for a yes or no, which Allow and Deny
/// answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Ask {
    /// The request.
    id: AskId,
    /// Its buttons' labels ([`slopty_proto::thread::wire::RequestCard::buttons`]).
    picks: Vec<String>,
}

/// Send `notice` to the links [`route`] picks among the board's seats, and push it to the
/// phones when it finds the person at none of them, with `ask`, the request its note's buttons
/// answer.
fn tell(board: &mut Board, notice: &Notice, ask: Option<&Ask>) {
    tell_pushing(board, notice, Pushing { ask, ..Pushing::default() });
}

/// [`tell`], pushed to the phones as `how` says: a ready-to-merge note's Merge merging a task,
/// or a finished turn's note to take back once the turn is seen.
fn tell_pushing(board: &mut Board, notice: &Notice, how: Pushing<'_>) {
    let reach = route(&board.seats, notice);
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

/// How long a project's work turning ready waits before the person hears of it, so tasks
/// verified close together make one note ([`tell_ready`]).
pub(super) const READY_SETTLE: Duration = Duration::from_secs(20);

/// The person hears how many of `project`'s tasks wait on their merge, in one notice about the
/// project at `entry`, the timeline entry that made the latest ready; its note's Merge merges
/// the one ready longest ([`NoticeKind::ReadyToMerge`]). None ready, nothing is said.
pub(super) fn tell_ready(
    state: &mut State,
    project: &slopty_proto::project::ProjectId,
    entry: u64,
) {
    let Ok(record) = state.projects.project(project) else { return };
    let (tile, title) = (record.orchestrator, record.title.clone());
    let Ok(tasks) = state.projects.tasks(project) else { return };
    let ready: Vec<&Task> = tasks.iter().filter(|t| ready_to_merge(t)).collect();
    let Some(oldest) = ready.iter().min_by_key(|t| (t.updated_ms, t.id)).map(|t| t.id) else {
        return;
    };
    let text = match ready.as_slice() {
        [one] if one.title.trim().is_empty() => format!("#{} is ready to merge", one.id),
        [one] => format!("#{} {} is ready to merge", one.id, one.title.trim()),
        many => format!("{} ready to merge", many.len()),
    };
    let notice = Notice {
        kind: NoticeKind::ReadyToMerge,
        about: Subject::Project { project: project.clone(), entry },
        tile,
        title,
        text,
        worked_ms: None,
        via: None,
    };
    tell_pushing(&mut state.board, &notice, Pushing { merges: Some(oldest), ..Pushing::default() });
}

/// Whether `task`'s work waits on the person's merge: done, writing, and in no merge yet.
pub(super) fn ready_to_merge(task: &Task) -> bool {
    task.state == slopty_proto::project::TaskState::Done && !task.read_only && task.merge.is_none()
}

/// The notice `kept` makes: a timeline entry that holds a task's work up, named by the task, and
/// said to wait on the person once the task's give-backs are spent.
fn project_notice(projects: &Projects, kept: &Kept) -> Option<Notice> {
    let entry = kept.entry.as_ref()?;
    let project = projects.project(&kept.project).ok()?;
    let task = kept.task.as_ref().filter(|t| Some(t.id) == entry.task);
    let words = held_up(entry, task, &project.target)?;
    let text = match (entry.task, task.map(|t| t.title.trim()).filter(|t| !t.is_empty())) {
        (Some(id), Some(title)) => format!("#{id} {title}: {words}"),
        (Some(id), None) => format!("#{id}: {words}"),
        (None, _) => words,
    };
    let text = if task.is_some_and(|t| t.give_backs.held) {
        format!("{text}. It waits on you")
    } else {
        text
    };
    Some(Notice {
        kind: NoticeKind::Project,
        about: Subject::Project { project: kept.project.clone(), entry: entry.seq },
        tile: project.orchestrator,
        title: project.title.clone(),
        text,
        worked_ms: None,
        via: None,
    })
}

/// What `entry` says when it holds `task`'s work up, the project's work merging into `target`;
/// `None` for the rest, which the board shows where it is looked at.
fn held_up(entry: &TimelineEntry, task: Option<&Task>, target: &str) -> Option<String> {
    let first = |text: &str| text.lines().next().unwrap_or_default().trim().to_owned();
    Some(match &entry.what {
        Moment::Pull(pull) if pull.stands.needs_you() => {
            format!("its {} {}", pull.forge.noun(), pull.line())
        }
        Moment::Verified(run) if !run.passed => "its verifier failed".to_owned(),
        Moment::Step(TaskStep { kind, state: StepState::Failed { why }, .. }) => match kind {
            StepKind::Rebase => format!("its work conflicts with {target}: {}", first(why)),
            StepKind::Clone => format!("the clone it needs failed: {}", first(why)),
            StepKind::Send => format!("its clone was not sent its start: {}", first(why)),
            StepKind::Home => format!("its branch did not come home: {}", first(why)),
            StepKind::Verify => format!("its verifier did not run: {}", first(why)),
            StepKind::Merge => format!("its merge stopped: {}", first(why)),
        },
        Moment::Step(TaskStep { kind: StepKind::Merge, state: StepState::Done { .. }, .. }) => {
            match task.and_then(|t| t.merge.as_ref()) {
                Some(Merge::Merged { target, push_failed: Some(why), .. }) => {
                    format!("merged into {target}, but the push to origin failed: {}", first(why))
                }
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// A thread that hangs from no other, standing as high as its subagents.
struct Root<'a> {
    ranked: Ranked,
    row: &'a ThreadRow,
}

/// The thread `row` hangs from in `table`, through every parent there; itself when none.
/// Where `row`'s thread sits, as the projects know its agent: the terminal its TUI runs in,
/// else the seat a task's thread was started at ([`SEAT_FACT`]).
fn seat_of(row: &ThreadRow) -> Option<SessionId> {
    row.terminal.or_else(|| seat_fact(row))
}

/// The seat a task's thread was started at, as its row's [`SEAT_FACT`] says.
fn seat_fact(row: &ThreadRow) -> Option<SessionId> {
    row.facts.get(SEAT_FACT)?.parse().ok()
}

/// Whether `row`'s thread is still to be had: its process has not ended.
const fn there(row: &ThreadRow) -> bool {
    !matches!(row.status.liveness, Liveness::Exited { .. })
}

/// `row`'s phase as an agent's status reads, what the projects follow: a request open is a
/// block on the person, by what it asks, and an agent that ended is none.
fn status_of(row: &ThreadRow) -> AgentStatus {
    if !there(row) {
        return AgentStatus::None;
    }
    let limited = row.status.wait.as_ref().is_some_and(|w| w.kind == Wait::LIMIT);
    match row.status.phase {
        Phase::Working => AgentStatus::Working,
        Phase::Waiting => AgentStatus::Waiting { tasks: 1, crons: 0 },
        Phase::NeedsYou => AgentStatus::Blocked(match row.requests.first() {
            Some(r) if r.kind == Request::APPROVAL => {
                BlockReason::Permission { tool: r.title.clone() }
            }
            Some(r) if r.kind == Request::ELICITATION => BlockReason::Elicitation,
            _ => BlockReason::Question,
        }),
        Phase::Done => AgentStatus::Done,
        Phase::Failed if limited => {
            AgentStatus::Failed { error: AgentStatus::RATE_LIMIT.to_owned(), until_ms: None }
        }
        // The row names no other error; the turn it ended is said by the outcome notice.
        Phase::Failed => AgentStatus::Failed { error: "unknown".to_owned(), until_ms: None },
        Phase::Idle | Phase::Stopped => AgentStatus::Idle,
    }
}

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
                let ranked =
                    Ranked { at, rung, since_ms: from.status.since_ms, terminal: row.terminal };
                Some(Root { ranked, row })
            })
        })
        .collect();
    roots.sort_by_key(|r| r.ranked.at);
    roots
}

/// The project node `term` works under: its task, or the project's orchestrator.
fn node_of(projects: &Projects, term: TermRef) -> Option<NodeAt> {
    projects.working_on(term).map(|(project, task)| NodeAt { project, task })
}

/// The ladder of `tables`, with the project nodes `projects` puts their terminals under.
fn ladder(
    tables: &HashMap<WorkerId, BTreeMap<ThreadId, ThreadRow>>,
    projects: &Projects,
) -> Ladder {
    let roots = roots(tables);
    let mut tiles: BTreeMap<(WorkerId, SessionId), Standing> = BTreeMap::new();
    let mut workers: BTreeMap<WorkerId, Standing> = BTreeMap::new();
    let mut nodes: BTreeMap<NodeAt, Standing> = BTreeMap::new();
    let mut by_project: BTreeMap<slopty_proto::project::ProjectId, Standing> = BTreeMap::new();
    let mut fleet = Standing::default();
    for root in &roots {
        let ranked = &root.ranked;
        fleet.add(ranked);
        workers.entry(ranked.at.worker).or_default().add(ranked);
        if let Some(session) = root.row.terminal {
            tiles.entry((ranked.at.worker, session)).or_default().add(ranked);
        }
        let Some(session) = seat_of(root.row) else { continue };
        let term = TermRef { worker: ranked.at.worker, session };
        if let Some(node) = node_of(projects, term) {
            by_project.entry(node.project.clone()).or_default().add(ranked);
            nodes.entry(node).or_default().add(ranked);
        }
    }
    Ladder {
        threads: roots.iter().map(|r| r.ranked).collect(),
        tiles: tiles
            .into_iter()
            .map(|((worker, session), s)| (TermRef { worker, session }, s))
            .collect(),
        workers: workers.into_iter().collect(),
        nodes: nodes.into_iter().collect(),
        projects: by_project.into_iter().collect(),
        fleet,
    }
}

/// A notice the ladder made ([`moved`]).
struct Told {
    /// The notice.
    notice: Notice,
    /// The request its note's buttons answer.
    ask: Option<Ask>,
    /// The turn it says finished.
    turn: Option<TurnId>,
    /// It is about a thread first seen already needing the person: after the server started,
    /// or its worker linked again. The clients that held it hold it still, so no link is told;
    /// only a phone not already showing it is pushed, when the person is at no client.
    first: bool,
}

/// The notices `ladder` makes against the one `board` published, keeping how long each
/// thread has been busy, each with the request its note's buttons answer: a thread's own
/// first, when it needs the person for a plain yes or no ([`RequestCard::answerable`]). A
/// thread first seen needing the person counts too ([`Told::first`]). A project task's agent
/// that finished says nothing: what it made reaches the person as work ready to merge, and its
/// orchestrator hears of the rest.
///
/// [`RequestCard::answerable`]: slopty_proto::thread::wire::RequestCard::answerable
fn moved(board: &mut Board, ladder: &Ladder, projects: &Projects) -> Vec<Told> {
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
        let rest = matches!(now.rung, Rung::ToReview | Rung::Idle | Rung::Failed);
        let kind = match (was, now.rung) {
            (None, Rung::NeedsYou) => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::NeedsYou) if was != Rung::NeedsYou => Some(NoticeKind::NeedsYou),
            (Some(was), Rung::Failed) if was != Rung::Failed => Some(NoticeKind::Failed),
            (Some(was), Rung::ToReview | Rung::Idle) if busy(was) => Some(NoticeKind::Finished),
            _ => None,
        };
        let worked_ms = if rest {
            let since = board.busy.remove(&now.at);
            since.map(|since| now.since_ms.as_millis().saturating_sub(since.as_millis()))
        } else {
            None
        };
        let Some(kind) = kind else { continue };
        let finished = kind == NoticeKind::Finished;
        let worked_ms = finished.then_some(worked_ms).flatten();
        let Some((notice, ask)) = notice_of(board, projects, now.at, (kind, worked_ms)) else {
            continue;
        };
        let row = board.tables.get(&now.at.worker).and_then(|t| t.get(&now.at.thread));
        let turn = row.and_then(|r| r.ended.as_ref()).map(|e| e.turn).filter(|_| finished);
        notices.push(Told { notice, ask, turn, first: was.is_none() });
    }
    let standing: Vec<ThreadAt> = ladder.threads.iter().map(|r| r.at).collect();
    board.busy.retain(|at, _| standing.binary_search(at).is_ok());
    notices
}

/// The notice of `kind` about the thread at `at`, with how long it worked when it finished,
/// and the request its note's buttons answer; `None` for a thread no table holds, and for
/// what a project task's agent says through its project instead.
fn notice_of(
    board: &Board,
    projects: &Projects,
    at: ThreadAt,
    (kind, worked_ms): (NoticeKind, Option<u64>),
) -> Option<(Notice, Option<Ask>)> {
    let table = board.tables.get(&at.worker)?;
    let row = table.get(&at.thread)?;
    let task_agent = || {
        let term = seat_of(row).map(|session| TermRef { worker: at.worker, session });
        term.and_then(|t| projects.working_on(t)).is_some_and(|(_, task)| task.is_some())
    };
    // A task's pull request is the project's to tell of ([`tell_project`]), once.
    let by_pull = kind == NoticeKind::NeedsYou
        && row.requests.is_empty()
        && row.status.phase != Phase::NeedsYou;
    if (kind == NoticeKind::Finished || by_pull) && task_agent() {
        return None;
    }
    let family: Vec<&ThreadRow> =
        table.values().filter(|r| r.id != row.id && root_of(table, r) == row.id).collect();
    let (_, from) = source(row, &family);
    let via = (from.id != row.id).then(|| Via { thread: from.id, title: from.title.clone() });
    let ask = row
        .requests
        .first()
        .filter(|_| kind == NoticeKind::NeedsYou && via.is_none())
        .filter(|r| r.answerable() || !r.buttons.is_empty())
        .map(|r| Ask {
            id: r.id.clone(),
            picks: r.buttons.iter().map(|b| b.label.clone()).collect(),
        });
    let notice = Notice {
        kind,
        about: Subject::Thread(at),
        tile: row.terminal.map(|session| TermRef { worker: at.worker, session }),
        title: row.title.clone(),
        text: text(kind, from),
        worked_ms,
        via,
    };
    Some((notice, ask))
}

/// Each note a phone shows that its thread needs the person follows the request the thread asks
/// now ([`Phones::follow`]), for a thread that needed them at the last ranking too; one that
/// came to need them since is told anew ([`moved`]).
fn follow_asks(board: &mut Board, projects: &Projects) {
    for (client, at) in board.phones.asking() {
        let threads = &board.published.threads;
        let before = threads.binary_search_by_key(&at, |r| r.at).ok().and_then(|i| threads.get(i));
        if before.is_none_or(|r| r.rung != Rung::NeedsYou) {
            continue;
        }
        let Some((notice, ask)) = notice_of(board, projects, at, (NoticeKind::NeedsYou, None))
        else {
            continue;
        };
        board.phones.follow(client, &notice, ask.as_ref());
    }
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
fn left(board: &mut Board, projects: &Projects) {
    let waiting: Vec<ThreadAt> =
        board.published.threads.iter().filter(|r| r.rung == Rung::NeedsYou).map(|r| r.at).collect();
    for at in waiting {
        let Some((notice, ask)) = notice_of(board, projects, at, (NoticeKind::NeedsYou, None))
        else {
            continue;
        };
        if route(&board.seats, &notice).away {
            let how = Pushing { ask: ask.as_ref(), new_only: true, ..Pushing::default() };
            board.phones.send(&board.seats, &notice, how);
        }
    }
}

/// What a notice of `kind` says of `row`, in a line.
fn text(kind: NoticeKind, row: &ThreadRow) -> String {
    let wait = row.status.wait.as_ref().map(|w| w.text.clone()).filter(|t| !t.is_empty());
    match kind {
        NoticeKind::NeedsYou => row
            .requests
            .first()
            .map(|r| r.title.clone())
            .or(wait)
            .or_else(|| row.pull.as_ref().filter(|p| p.stands.needs_you()).map(PullSeen::line))
            .unwrap_or_default(),
        NoticeKind::Failed
        | NoticeKind::Finished
        | NoticeKind::Project
        | NoticeKind::ReadyToMerge
        | NoticeKind::GoalDone => wait.or_else(|| row.last_line.clone()).unwrap_or_default(),
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

/// Where `notice` goes among `seats`.
fn route(seats: &BTreeMap<u64, Sitting>, notice: &Notice) -> Reach {
    let tile = notice.tile;
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
