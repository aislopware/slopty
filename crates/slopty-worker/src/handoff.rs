//! Which client a shell's web pages and edits go to, and whether a page opens or is offered.
//!
//! A program in a session opens a page (`BROWSER`, the `open` shim) or asks for an editor
//! (`EDITOR`). Only clients that said they take that kind of handoff (`HandoffCaps`) are asked,
//! best first ([`Handoffs::candidates`]):
//!
//! 1. a client focused on the session's tile, the latest to focus it first;
//! 2. the client that last typed into it;
//! 3. any client focused on a tile of this worker, then any other, the most recently active first.
//!
//! **A page opens only on a person's say** ([`Handoffs::open_as`]). It opens unasked only on a
//! client that typed into the session within [`TYPED_RECENTLY`] (the Enter that started a
//! login), only for an address that is not `Wary`, and at most [`AUTO_OPENS`] times in
//! [`AUTO_OPEN_SPAN`]; otherwise it is offered, a notice naming the host. Any program on the
//! worker can ask, so nothing else may drive the person's browser.
//!
//! One asked client that refuses, or does not answer in time, is withdrawn and passed over for
//! the next; one that answers late still counts (its page is open), and the others are
//! withdrawn then. A waiting edit lives as long as the program that asked: its client may drop
//! and come back, and is asked again under the same number.
//!
//! The same focus says whether a person is looking at a session: while any client is focused on
//! it, its `CLAUDE_CLIENT_PRESENCE_FILE` exists, and Claude Code sends no Remote Control push for
//! it ([`presence_file`]).
//!
//! This is the bookkeeping only; the daemon runs the asks (`slopty-worker`'s `handoff`).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use slopty_core::{ClientId, SessionId};
use slopty_proto::WorkerMsg;
use slopty_proto::handoff::{
    EditFile, HandoffCaps, HandoffEvent, HandoffId, HandoffReply, OfferReason, Page,
};

use crate::clip::Link;

/// How long after a keystroke into a session a page it asks for may open unasked on the client
/// that typed.
///
/// The Enter that starts a login (`gh auth login`, Claude Code's `/login`) comes a
/// moment before its page, and a command that starts a runtime first (`az login`, `gcloud auth
/// login`) takes a few seconds more; a page asked for later is a program acting on its own.
pub const TYPED_RECENTLY: Duration = Duration::from_secs(8);

/// At most this many pages open unasked in [`AUTO_OPEN_SPAN`]; the rest are offered.
pub const AUTO_OPENS: usize = 3;

/// The span [`AUTO_OPENS`] counts over.
pub const AUTO_OPEN_SPAN: Duration = Duration::from_secs(10);

/// Withdrawals kept for one client that is away, at most; the oldest goes first.
const OWED_MAX: usize = 32;

/// Where a message to one client goes: its connection's control queue, without waiting.
pub type Sink = Arc<dyn Fn(WorkerMsg) + Send + Sync>;

/// Which kind of handoff a client is asked to take.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Need {
    /// A web page.
    Open,
    /// A file.
    Edit,
}

impl Need {
    const fn met_by(self, caps: HandoffCaps) -> bool {
        match self {
            Self::Open => caps.open,
            Self::Edit => caps.edit,
        }
    }
}

/// What an ask in flight hears, and from which client.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Heard {
    /// A client asked answered.
    Reply(ClientId, HandoffReply),
    /// A client asked disconnected.
    Gone(ClientId),
    /// The client holding a waiting edit connected again, and was asked again.
    Back(ClientId),
}

/// Where an ask in flight hears from its clients.
pub type Waiter = tokio::sync::mpsc::UnboundedSender<Heard>;

/// Why no client can be asked ([`Handoffs::candidates`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nobody {
    /// No client is connected.
    Connected,
    /// Clients are connected; none takes this kind of handoff.
    Capable,
}

/// A connected client.
struct Client {
    link: Link,
    name: String,
    sink: Sink,
    caps: HandoffCaps,
    /// When it last joined, focused or typed: the order among clients that have nothing to do
    /// with the session.
    active: u64,
}

/// One handoff in flight.
struct Asked {
    waiter: Waiter,
    /// Every client asked so far, in order; each may still answer.
    clients: Vec<ClientId>,
    /// Those already told to forget it.
    withdrawn: Vec<ClientId>,
    /// A waiting edit and the client that took it, asked again when that client comes back.
    edit: Option<(ClientId, EditFile)>,
}

/// Connected clients, what each takes, focuses and typed into, and the handoffs in flight.
pub struct Handoffs {
    clients: HashMap<ClientId, Client>,
    /// Per session, the clients focused on its tile, the latest last.
    focused: HashMap<SessionId, Vec<ClientId>>,
    /// Per session, the client that last typed into it, and when.
    typed: HashMap<SessionId, (ClientId, Instant)>,
    asked: HashMap<HandoffId, Asked>,
    /// When the pages opened unasked lately were opened.
    opened: VecDeque<Instant>,
    /// Withdrawals for clients that were away when their handoff ended, sent when they return.
    owed: HashMap<ClientId, Vec<HandoffId>>,
    next_id: HandoffId,
    clock: u64,
}

impl std::fmt::Debug for Handoffs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handoffs")
            .field("clients", &self.clients.len())
            .field("focused", &self.focused)
            .field("asked", &self.asked.len())
            .finish_non_exhaustive()
    }
}

impl Default for Handoffs {
    fn default() -> Self {
        Self::numbered_from(first_id())
    }
}

/// Where a run's handoff numbers start: the wall clock in microseconds, so a restarted worker
/// never reuses a number a client may still hold from its last run.
fn first_id() -> HandoffId {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

impl Handoffs {
    /// No clients yet; handoffs numbered from after `first`.
    #[must_use]
    pub fn numbered_from(first: HandoffId) -> Self {
        Self {
            clients: HashMap::new(),
            focused: HashMap::new(),
            typed: HashMap::new(),
            asked: HashMap::new(),
            opened: VecDeque::new(),
            owed: HashMap::new(),
            next_id: first,
            clock: 0,
        }
    }

    /// A number for a new handoff.
    pub const fn next_id(&mut self) -> HandoffId {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    const fn tick(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// `client` connected on `link` (replacing an older connection of the same client). It
    /// takes nothing until it says ([`Self::caps`]), and is focused on nothing: a client that
    /// reconnects says its focus again, so the sessions only its old connection was focused on
    /// come back, for their presence files to go. It is asked again for the waiting edits it
    /// holds, whose asks hear it is [`Heard::Back`].
    pub fn join(
        &mut self,
        client: ClientId,
        link: Link,
        name: String,
        sink: Sink,
    ) -> Vec<SessionId> {
        let active = self.tick();
        for id in self.owed.remove(&client).unwrap_or_default() {
            sink(WorkerMsg::Handoff(HandoffEvent::Withdrawn { id }));
        }
        for asked in self.asked.values() {
            if let Some((holder, edit)) = &asked.edit
                && *holder == client
            {
                sink(WorkerMsg::Handoff(HandoffEvent::Edit(edit.clone())));
                let _gone = asked.waiter.send(Heard::Back(client));
            }
        }
        self.clients
            .insert(client, Client { link, name, sink, caps: HandoffCaps::default(), active });
        self.unfocus(client)
    }

    /// `client` said which handoffs it takes.
    pub fn caps(&mut self, client: ClientId, caps: HandoffCaps) {
        if let Some(c) = self.clients.get_mut(&client) {
            c.caps = caps;
        }
    }

    /// `client`'s connection on `link` ended. Nothing, when a newer connection of the same
    /// client already joined. Its asks hear [`Heard::Gone`]; the sessions it was the last to
    /// focus come back, for their presence files to go.
    pub fn leave(&mut self, client: ClientId, link: Link) -> Vec<SessionId> {
        if self.clients.get(&client).is_none_or(|c| c.link != link) {
            return Vec::new();
        }
        self.clients.remove(&client);
        for asked in self.asked.values().filter(|a| a.clients.contains(&client)) {
            let _gone = asked.waiter.send(Heard::Gone(client));
        }
        self.typed.retain(|_session, (typist, _at)| *typist != client);
        self.unfocus(client)
    }

    /// Drop `client`'s focus everywhere: the sessions nobody is focused on now.
    fn unfocus(&mut self, client: ClientId) -> Vec<SessionId> {
        let mut unwatched = Vec::new();
        self.focused.retain(|session, clients| {
            let had = clients.contains(&client);
            clients.retain(|c| *c != client);
            if had && clients.is_empty() {
                unwatched.push(*session);
            }
            !clients.is_empty()
        });
        unwatched
    }

    /// `client` focused on `session`'s tile, or let go of it. `Some` with whether anyone is
    /// focused on the session now, when that changed.
    pub fn focus(&mut self, session: SessionId, client: ClientId, focused: bool) -> Option<bool> {
        let active = self.tick();
        if let Some(c) = self.clients.get_mut(&client) {
            c.active = active;
        }
        let clients = self.focused.entry(session).or_default();
        let before = !clients.is_empty();
        clients.retain(|c| *c != client);
        if focused {
            clients.push(client);
        }
        let now = !clients.is_empty();
        if !now {
            self.focused.remove(&session);
        }
        (before != now).then_some(now)
    }

    /// `client` typed into `session` at `now`.
    pub fn typed(&mut self, session: SessionId, client: ClientId, now: Instant) {
        let active = self.tick();
        if let Some(c) = self.clients.get_mut(&client) {
            c.active = active;
        }
        self.typed.insert(session, (client, now));
    }

    /// Whether anyone is focused on `session`.
    #[must_use]
    pub fn watched(&self, session: SessionId) -> bool {
        self.focused.contains_key(&session)
    }

    /// The session is gone: nobody focuses or types into it, and whether anyone was focused
    /// on it (its presence file to go).
    pub fn forget(&mut self, session: SessionId) -> bool {
        self.typed.remove(&session);
        self.focused.remove(&session).is_some()
    }

    /// Connected clients that take `need`, to ask for a handoff from `session`, best first; or
    /// why there are none.
    ///
    /// # Errors
    ///
    /// [`Nobody`] when no client can be asked.
    pub fn candidates(
        &self,
        session: Option<SessionId>,
        need: Need,
    ) -> Result<Vec<ClientId>, Nobody> {
        if self.clients.is_empty() {
            return Err(Nobody::Connected);
        }
        let takes =
            |client: &ClientId| self.clients.get(client).is_some_and(|c| need.met_by(c.caps));
        let mut out: Vec<ClientId> = Vec::new();
        let mut push = |client: ClientId| {
            if takes(&client) && !out.contains(&client) {
                out.push(client);
            }
        };
        if let Some(session) = session {
            self.focused.get(&session).into_iter().flatten().rev().copied().for_each(&mut push);
            self.typed.get(&session).map(|(c, _at)| *c).into_iter().for_each(&mut push);
        }
        let focusing = |client: &ClientId| self.focused.values().any(|cs| cs.contains(client));
        let mut rest: Vec<(&ClientId, &Client)> = self.clients.iter().collect();
        rest.sort_by_key(|(id, c)| (!focusing(id), std::cmp::Reverse(c.active)));
        rest.into_iter().map(|(id, _c)| *id).for_each(push);
        if out.is_empty() { Err(Nobody::Capable) } else { Ok(out) }
    }

    /// Whether `page`, asked for from `session`, opens on `client` or is offered, and why: it
    /// opens only for a client that typed into the session within [`TYPED_RECENTLY`] of `now`,
    /// for an address that is not `Wary`, and while fewer than [`AUTO_OPENS`] opened in the
    /// last [`AUTO_OPEN_SPAN`]. An opening is counted.
    pub fn open_as(
        &mut self,
        page: &Page,
        session: Option<SessionId>,
        client: ClientId,
        now: Instant,
    ) -> Option<OfferReason> {
        if let Some(wary) = page.wary {
            return Some(OfferReason::Wary(wary));
        }
        let typed = session.and_then(|s| self.typed.get(&s)).is_some_and(|(typist, at)| {
            *typist == client && now.saturating_duration_since(*at) <= TYPED_RECENTLY
        });
        if !typed {
            return Some(OfferReason::NotTyped);
        }
        while self
            .opened
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= AUTO_OPEN_SPAN)
        {
            self.opened.pop_front();
        }
        if self.opened.len() >= AUTO_OPENS {
            return Some(OfferReason::Busy);
        }
        self.opened.push_back(now);
        None
    }

    /// The name of a connected client.
    #[must_use]
    pub fn name(&self, client: ClientId) -> Option<&str> {
        self.clients.get(&client).map(|c| c.name.as_str())
    }

    /// Ask `client` for handoff `id` with `event`, its answers (and every other client's asked
    /// for `id`) to `waiter`. `false` when the client is not connected.
    pub fn ask(
        &mut self,
        client: ClientId,
        id: HandoffId,
        event: HandoffEvent,
        waiter: &Waiter,
    ) -> bool {
        let Some(c) = self.clients.get(&client) else { return false };
        (c.sink)(WorkerMsg::Handoff(event));
        let asked = self.asked.entry(id).or_insert_with(|| Asked {
            waiter: waiter.clone(),
            clients: Vec::new(),
            withdrawn: Vec::new(),
            edit: None,
        });
        asked.clients.push(client);
        true
    }

    /// Tell `client` to forget handoff `id`, which went on to another client; an answer it
    /// still sends counts.
    pub fn withdraw(&mut self, id: HandoffId, client: ClientId) {
        let Some(asked) = self.asked.get_mut(&id) else { return };
        if asked.withdrawn.contains(&client) {
            return;
        }
        asked.withdrawn.push(client);
        if let Some(c) = self.clients.get(&client) {
            (c.sink)(WorkerMsg::Handoff(HandoffEvent::Withdrawn { id }));
        }
    }

    /// `holder` took waiting edit `edit`: it is asked again when it reconnects, and the other
    /// clients asked are withdrawn.
    pub fn hold(&mut self, holder: ClientId, edit: EditFile) {
        let id = edit.id;
        let others: Vec<ClientId> = self
            .asked
            .get(&id)
            .map(|a| a.clients.iter().copied().filter(|c| *c != holder).collect())
            .unwrap_or_default();
        for other in others {
            self.withdraw(id, other);
        }
        if let Some(asked) = self.asked.get_mut(&id) {
            asked.edit = Some((holder, edit));
        }
    }

    /// Handoff `id` is over: every client asked that did not take it (`taker`) and was not
    /// told yet is told to forget it, one that is away when it returns (a waiting edit given
    /// up or lost while its client was gone stops waiting there too).
    pub fn finish(&mut self, id: HandoffId, taker: Option<ClientId>) {
        let Some(asked) = self.asked.remove(&id) else { return };
        for client in asked.clients {
            if Some(client) == taker || asked.withdrawn.contains(&client) {
                continue;
            }
            if let Some(c) = self.clients.get(&client) {
                (c.sink)(WorkerMsg::Handoff(HandoffEvent::Withdrawn { id }));
            } else {
                let owed = self.owed.entry(client).or_default();
                if owed.len() >= OWED_MAX {
                    owed.remove(0);
                }
                owed.push(id);
            }
        }
    }

    /// Whether a waiting edit shows `path`: its saves go into the file in place, since the
    /// program waiting on it may hold it open (`slopty_worker::file::Rewrite::InPlace`).
    #[must_use]
    pub fn editing(&self, path: &str) -> bool {
        self.asked.values().any(|a| a.edit.as_ref().is_some_and(|(_holder, e)| e.path == path))
    }

    /// Forget every session not in `live` (it exited, or ptyd lost it): the ones someone was
    /// focused on come back, for their presence files to go.
    pub fn retain(&mut self, live: &[SessionId]) -> Vec<SessionId> {
        self.typed.retain(|session, _typist| live.contains(session));
        let gone: Vec<SessionId> =
            self.focused.keys().filter(|s| !live.contains(s)).copied().collect();
        for session in &gone {
            self.focused.remove(session);
        }
        gone
    }

    /// `client` answered. An answer to a handoff it was not asked is dropped.
    pub fn replied(&self, client: ClientId, reply: HandoffReply) {
        if let Some(asked) = self.asked.get(&reply.id()).filter(|a| a.clients.contains(&client)) {
            let _gone = asked.waiter.send(Heard::Reply(client, reply));
        }
    }
}

/// The presence file of `session` under `dir` (`CLAUDE_CLIENT_PRESENCE_FILE`): Claude Code
/// sends no Remote Control push while it exists
/// (<https://code.claude.com/docs/en/env-vars>).
#[must_use]
pub fn presence_file(dir: &Path, session: SessionId) -> PathBuf {
    dir.join(session.to_string())
}

/// Make or remove `session`'s presence file under `dir`. A file already there, or already gone,
/// is what was wanted.
///
/// # Errors
///
/// When the file cannot be made or removed.
pub fn mark_presence(dir: &Path, session: SessionId, present: bool) -> std::io::Result<()> {
    let file = presence_file(dir, session);
    let done = if present {
        std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&file, b""))
    } else {
        std::fs::remove_file(&file)
    };
    match done {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !present => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use slopty_proto::handoff::{EditOutcome, page};

    use super::*;

    type Log = Arc<Mutex<Vec<(u8, WorkerMsg)>>>;

    const BOTH: HandoffCaps = HandoffCaps { open: true, edit: true };

    /// A client whose messages are kept.
    fn client(log: &Log, n: u8) -> Sink {
        let log = Arc::clone(log);
        Arc::new(move |msg| log.lock().push((n, msg)))
    }

    /// `ids` connected in order, each taking `caps`.
    fn joined(log: &Log, ids: &[ClientId], caps: HandoffCaps) -> Handoffs {
        let mut h = Handoffs::numbered_from(0);
        for (nth, id) in ids.iter().enumerate() {
            let n = u8::try_from(nth).unwrap();
            h.join(*id, nth, format!("c{nth}"), client(log, n));
            h.caps(*id, caps);
        }
        h
    }

    fn withdrawn(id: HandoffId) -> WorkerMsg {
        WorkerMsg::Handoff(HandoffEvent::Withdrawn { id })
    }

    /// The client focused on the session goes first, the latest to focus first; then the one
    /// that typed into it; then clients focused elsewhere, then the rest by recent activity.
    #[test]
    fn the_client_in_front_of_the_session_is_asked_first() {
        let log = Arc::default();
        let (a, b, c, d) = (ClientId::new(), ClientId::new(), ClientId::new(), ClientId::new());
        let (shell, other) = (SessionId::new(), SessionId::new());
        let mut offers = joined(&log, &[a, b, c, d], BOTH);
        let open = |h: &Handoffs, s| h.candidates(s, Need::Open).unwrap();
        assert_eq!(open(&offers, Some(shell)), [d, c, b, a], "by when each joined, newest first");
        offers.typed(shell, a, Instant::now());
        offers.focus(other, b, true);
        assert_eq!(open(&offers, Some(shell)), [a, b, d, c]);
        offers.focus(shell, c, true);
        offers.focus(shell, d, true);
        assert_eq!(open(&offers, Some(shell)), [d, c, a, b]);
        assert_eq!(open(&offers, None), [d, c, b, a], "focused anywhere, latest first");
        offers.leave(d, 3);
        assert_eq!(open(&offers, Some(shell)), [c, a, b]);
    }

    /// Only a client that said it takes a kind of handoff is asked for it; with none, the
    /// reason says whether anyone is connected at all.
    #[test]
    fn only_a_client_that_takes_it_is_asked() {
        let log = Arc::default();
        let (pages, files, silent) = (ClientId::new(), ClientId::new(), ClientId::new());
        let mut h = Handoffs::numbered_from(0);
        assert_eq!(h.candidates(None, Need::Open), Err(Nobody::Connected));
        h.join(silent, 1, "cli".into(), client(&log, 1));
        assert_eq!(h.candidates(None, Need::Open), Err(Nobody::Capable), "never said");
        h.join(pages, 2, "phone".into(), client(&log, 2));
        h.caps(pages, HandoffCaps { open: true, edit: false });
        h.join(files, 3, "mac".into(), client(&log, 3));
        h.caps(files, HandoffCaps { open: false, edit: true });
        assert_eq!(h.candidates(None, Need::Open), Ok(vec![pages]));
        assert_eq!(h.candidates(None, Need::Edit), Ok(vec![files]));
        h.caps(files, HandoffCaps::default());
        assert_eq!(h.candidates(None, Need::Edit), Err(Nobody::Capable), "said it stopped");
    }

    /// A page opens only on the client that typed into its session a moment before, never for
    /// an address to be wary of, and not more than a few times in a burst; otherwise it is
    /// offered, with the reason.
    #[test]
    fn a_page_opens_only_right_after_its_client_typed() {
        let log = Arc::default();
        let (a, b) = (ClientId::new(), ClientId::new());
        let (shell, other) = (SessionId::new(), SessionId::new());
        let mut h = joined(&log, &[a, b], BOTH);
        let web = page("https://github.com/login/device").unwrap();
        let t0 = Instant::now();
        assert_eq!(h.open_as(&web, Some(shell), a, t0), Some(OfferReason::NotTyped));
        assert_eq!(h.open_as(&web, None, a, t0), Some(OfferReason::NotTyped), "no session");
        h.typed(shell, a, t0);
        let soon = t0.checked_add(Duration::from_secs(2)).unwrap();
        assert_eq!(h.open_as(&web, Some(shell), b, soon), Some(OfferReason::NotTyped), "b did not");
        assert_eq!(h.open_as(&web, Some(other), a, soon), Some(OfferReason::NotTyped));
        let local = page("http://127.0.0.1:3000/").unwrap();
        assert_eq!(
            h.open_as(&local, Some(shell), a, soon),
            Some(OfferReason::Wary(slopty_proto::handoff::Wary::Loopback))
        );
        for _ in 0..AUTO_OPENS {
            assert_eq!(h.open_as(&web, Some(shell), a, soon), None);
        }
        assert_eq!(h.open_as(&web, Some(shell), a, soon), Some(OfferReason::Busy));
        let late = t0.checked_add(TYPED_RECENTLY + Duration::from_secs(1)).unwrap();
        assert_eq!(h.open_as(&web, Some(shell), a, late), Some(OfferReason::NotTyped));
        h.typed(shell, a, late);
        let later = late.checked_add(AUTO_OPEN_SPAN).unwrap();
        h.typed(shell, a, later);
        assert_eq!(h.open_as(&web, Some(shell), a, later), None, "the burst is over");
    }

    /// Presence follows focus across clients: made with the first, gone with the last, gone
    /// when the last focused client disconnects, and gone when it reconnects (a new connection
    /// says its focus again).
    #[test]
    fn presence_is_there_while_any_client_is_focused() {
        let log = Arc::default();
        let (a, b) = (ClientId::new(), ClientId::new());
        let session = SessionId::new();
        let mut h = joined(&log, &[a, b], BOTH);
        assert_eq!(h.focus(session, a, true), Some(true));
        assert_eq!(h.focus(session, b, true), None);
        assert_eq!(h.focus(session, a, false), None);
        assert!(h.watched(session));
        assert_eq!(h.leave(b, 1), [session]);
        assert!(!h.watched(session));
        assert_eq!(h.focus(session, a, false), None, "nobody was focused");
        h.focus(session, a, true);
        assert_eq!(
            h.join(a, 5, "a".into(), client(&log, 5)),
            [session],
            "back, focused on nothing"
        );
        assert!(!h.watched(session));
        h.focus(session, a, true);
        assert!(h.forget(session), "a closed session takes its presence along");
        assert!(!h.watched(session));
    }

    /// A stale connection's end, after the same client came back, changes nothing.
    #[test]
    fn an_old_connection_ending_does_not_drop_the_new_one() {
        let log = Arc::default();
        let a = ClientId::new();
        let session = SessionId::new();
        let mut h = joined(&log, &[a], BOTH);
        h.join(a, 2, "a".into(), client(&log, 2));
        h.caps(a, BOTH);
        h.focus(session, a, true);
        assert_eq!(h.leave(a, 0), []);
        assert_eq!(h.candidates(Some(session), Need::Edit), Ok(vec![a]));
    }

    /// A client passed over after no answer is withdrawn; its late answer still counts, and
    /// then the client asked after it is withdrawn instead. Answers name their client, and one
    /// from a client never asked is dropped.
    #[test]
    fn a_late_answer_counts_and_the_others_are_withdrawn() {
        let log: Log = Arc::default();
        let (a, b, c) = (ClientId::new(), ClientId::new(), ClientId::new());
        let mut h = joined(&log, &[a, b, c], BOTH);
        let (waiter, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let id = h.next_id();
        let ask = HandoffEvent::Withdrawn { id: 0 };
        assert!(h.ask(a, id, ask.clone(), &waiter));
        h.withdraw(id, a);
        assert!(h.ask(b, id, ask.clone(), &waiter));
        h.replied(c, HandoffReply::Taken { id });
        h.replied(a, HandoffReply::Taken { id });
        assert_eq!(heard.try_recv().ok(), Some(Heard::Reply(a, HandoffReply::Taken { id })));
        assert!(heard.try_recv().is_err(), "c was never asked");
        h.finish(id, Some(a));
        let sent: Vec<(u8, WorkerMsg)> = log.lock().clone();
        assert_eq!(
            sent,
            [
                (0, WorkerMsg::Handoff(ask.clone())),
                (0, withdrawn(id)),
                (1, WorkerMsg::Handoff(ask)),
                (1, withdrawn(id)),
            ]
        );
    }

    /// A waiting edit hears its client go and come back, and is asked again under its number;
    /// the other clients asked for it are withdrawn once one took it.
    #[test]
    fn a_waiting_edit_is_asked_again_when_its_client_returns() {
        let log: Log = Arc::default();
        let (a, b) = (ClientId::new(), ClientId::new());
        let mut h = joined(&log, &[a, b], BOTH);
        let (waiter, mut heard) = tokio::sync::mpsc::unbounded_channel();
        let id = h.next_id();
        let edit = EditFile {
            id,
            session: None,
            path: "/r/.git/COMMIT_EDITMSG".into(),
            line: None,
            wait: true,
        };
        let ask = WorkerMsg::Handoff(HandoffEvent::Edit(edit.clone()));
        assert!(h.ask(b, id, HandoffEvent::Edit(edit.clone()), &waiter));
        h.withdraw(id, b);
        assert!(h.ask(a, id, HandoffEvent::Edit(edit.clone()), &waiter));
        h.hold(a, edit);
        h.leave(a, 0);
        assert_eq!(heard.try_recv().ok(), Some(Heard::Gone(a)));
        h.join(a, 3, "a".into(), client(&log, 3));
        assert_eq!(heard.try_recv().ok(), Some(Heard::Back(a)));
        assert!(h.editing("/r/.git/COMMIT_EDITMSG"), "its saves go in place");
        assert!(!h.editing("/r/other"));
        let done = HandoffReply::Edited { id, outcome: EditOutcome::Done };
        h.replied(a, done);
        assert_eq!(heard.try_recv().ok(), Some(Heard::Reply(a, done)));
        h.finish(id, Some(a));
        assert!(!h.editing("/r/.git/COMMIT_EDITMSG"));
        let sent: Vec<(u8, WorkerMsg)> = log.lock().clone();
        assert_eq!(sent, [(1, ask.clone()), (1, withdrawn(id)), (0, ask.clone()), (3, ask)]);
    }

    /// A waiting edit that ended while its client was away (lost, or given up) is withdrawn
    /// when the client returns, so its tile stops waiting on nothing.
    #[test]
    fn an_edit_ended_while_its_client_was_away_is_withdrawn_on_return() {
        let log: Log = Arc::default();
        let a = ClientId::new();
        let mut h = joined(&log, &[a], BOTH);
        let (waiter, _heard) = tokio::sync::mpsc::unbounded_channel();
        let id = h.next_id();
        let edit = EditFile { id, session: None, path: "/f".into(), line: None, wait: true };
        h.ask(a, id, HandoffEvent::Edit(edit.clone()), &waiter);
        h.hold(a, edit);
        h.leave(a, 0);
        h.finish(id, None);
        h.join(a, 4, "a".into(), client(&log, 4));
        assert_eq!(log.lock().last(), Some(&(4, withdrawn(id))));
        h.join(a, 5, "a".into(), client(&log, 5));
        assert_eq!(log.lock().last(), Some(&(4, withdrawn(id))), "told once");
    }

    /// Sessions that are gone are forgotten, and the watched ones come back for their
    /// presence files.
    #[test]
    fn a_gone_session_is_forgotten() {
        let log: Log = Arc::default();
        let a = ClientId::new();
        let (kept, gone) = (SessionId::new(), SessionId::new());
        let mut h = joined(&log, &[a], BOTH);
        h.focus(kept, a, true);
        h.focus(gone, a, true);
        assert_eq!(h.retain(&[kept]), [gone]);
        assert!(h.watched(kept) && !h.watched(gone));
    }

    /// A run numbers its handoffs from the clock, so a restarted worker does not reuse one.
    #[test]
    fn a_new_run_numbers_past_the_last() {
        let (mut first, mut second) = (Handoffs::default(), Handoffs::default());
        let before = first.next_id();
        assert!(second.next_id() >= before);
        assert!(before > 1_000_000_000_000_000, "microseconds since 1970");
    }

    /// The presence file is made and removed, and removing one that is gone is no error.
    #[test]
    fn a_presence_file_comes_and_goes() {
        let dir = tempfile::tempdir().unwrap();
        let presence = dir.path().join("presence");
        let session = SessionId::new();
        mark_presence(&presence, session, true).unwrap();
        assert!(presence_file(&presence, session).is_file());
        mark_presence(&presence, session, false).unwrap();
        assert!(!presence_file(&presence, session).exists());
        mark_presence(&presence, session, false).unwrap();
    }
}
