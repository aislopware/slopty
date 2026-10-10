//! The one link a client keeps to the server, dialled at start and redialled forever after it
//! drops.
//!
//! A server on a different build is said so ([`ServerEvent::WrongBuild`]) and asked again only
//! after [`slopty_net::redial::WRONG_BUILD`]. One that turns this device away, whether after
//! its hello or at the handshake, is [`ServerEvent::Refused`].
//!
//! Every message it carries is handed on as a [`ServerEvent`], and verbs go up it through a
//! [`ServerCaller`] ([`ServerCaller::wake`] wakes a sleeping worker).
//!
//! The server is the control plane only, so nothing here is on a terminal's or a stream's
//! path: while this link is down the workers are still dialled directly
//! ([`crate::directory`]).

use std::collections::HashMap;
use std::sync::Arc;

use slopty_core::{ClientId, WorkerId};
use slopty_net::server::{DialError, ServerLink, connect};
use slopty_net::{HostAddr, NetError};
use slopty_proto::RequestId;
use slopty_proto::orchestration::{ErrorCode, IdempotencyKey, Outcome, Verb};
use slopty_proto::push::PushDevice;
use slopty_proto::server::{FromServer, Refusal, Role, ToServer};
use slopty_proto::thread::attention::Presence;
use tokio::sync::{Notify, mpsc, oneshot, watch};

use crate::update::UpdateNotice;

/// Server messages queued for the UI at most.
const EVENT_DEPTH: usize = 256;
/// Verbs waiting for the link at most; a caller past them waits its turn.
const CALL_DEPTH: usize = 64;
/// How long a probe after [`ServerTask::resume`] waits for the server before giving up the link.
///
/// The server is the control plane, off every terminal's and stream's path, so one fixed wait
/// is enough: a live link answers in a round trip.
pub const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(1);
/// How often a probe looks for the answer.
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(2);
/// How long after a drop a verb whose answer it lost waits for the next link to go again.
///
/// Past it, the verb answers [`ErrorCode::Interrupted`]. It is well within the server's
/// [`slopty_proto::orchestration::KEY_LIFETIME`], so the repeat still meets its key.
pub const RESEND_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// What the link says.
#[derive(Debug)]
pub enum ServerEvent {
    /// The server welcomed this client.
    Linked {
        /// Its name.
        name: String,
        /// Its number for this link ([`slopty_proto::thread::attention::Present::link`]).
        link: u64,
        /// Its build ([`slopty_proto::wire::this_build`] there), to tell an older server on the
        /// same wire.
        build: String,
    },
    /// A message from it: the directory, a worker's change, an event.
    Message(Box<FromServer>),
    /// A dial failed or the link dropped; the next attempt is on its way.
    Unlinked {
        /// Why.
        why: String,
    },
    /// The server answered on a different build: it is there, and this build cannot talk to
    /// it until one of them is updated. Asked again after [`slopty_net::redial::WRONG_BUILD`].
    WrongBuild(UpdateNotice),
    /// The server answered and turned this dialer away. The next attempt is on its way all the
    /// same: a change to the tailnet policy can let it in.
    Refused(Refusal),
}

/// The running link; dropping it closes the link and ends the redials.
#[derive(Debug)]
pub struct ServerTask {
    task: tokio::task::JoinHandle<()>,
    calls: mpsc::Sender<Call>,
    presence: Arc<watch::Sender<Option<Presence>>>,
    phone: Arc<watch::Sender<Option<Phone>>>,
    resume: Arc<Notify>,
}

/// What this client last said of itself as a phone the server may push to: its identity, and
/// the device, or none to take it back.
type Phone = (ClientId, Option<PushDevice>);

impl ServerTask {
    /// Something may have killed the link (the device slept, the path moved): a live link is
    /// probed now, and given up if the server says nothing within [`PROBE_DEADLINE`], rather
    /// than at the transport's idle timeout; a link between dials is dialled now.
    pub fn resume(&self) {
        self.resume.notify_one();
    }

    /// A handle that sends verbs up this link, for as long as it runs.
    #[must_use]
    pub fn caller(&self) -> ServerCaller {
        ServerCaller {
            calls: self.calls.clone(),
            presence: Arc::clone(&self.presence),
            phone: Arc::clone(&self.phone),
        }
    }
}

/// Sends verbs to the server over the client's one link and hands back each answer.
///
/// A verb that changes something goes under an [`IdempotencyKey`] of its own. One whose answer
/// a drop lost goes again, under the same key, on the next link within [`RESEND_WITHIN`], so
/// the server answers it as it answered the first and nothing is done twice; past that, it
/// answers [`ErrorCode::Interrupted`]. While the link is down, a new verb answers
/// [`ErrorCode::ServerUnreachable`] at once.
#[derive(Clone, Debug)]
pub struct ServerCaller {
    calls: mpsc::Sender<Call>,
    /// Where the person is, sent on every change and again on every link.
    presence: Arc<watch::Sender<Option<Presence>>>,
    /// The phone this client is, sent on every change and again on every link.
    phone: Arc<watch::Sender<Option<Phone>>>,
}

impl ServerCaller {
    /// Send `verb` and wait for the server's answer.
    pub async fn call(&self, verb: Verb) -> Outcome {
        let (reply, answer) = oneshot::channel();
        let key = verb.changes().then(|| IdempotencyKey::from_id(uuid::Uuid::new_v4().as_u128()));
        if self.calls.send(Call { verb, key, reply, until: None }).await.is_err() {
            return unreachable("the link to the server has stopped");
        }
        answer.await.unwrap_or_else(|_dropped| interrupted())
    }

    /// Wake `worker`, which sleeps: the server, or a worker on the same LAN, sends it the
    /// magic packet. [`Outcome::WakeSent`] says which machine sent it; the worker registering
    /// again is the directory's news.
    pub async fn wake(&self, worker: WorkerId) -> Outcome {
        self.call(Verb::Wake { worker }).await
    }

    /// Say where the person is on this client. Nothing goes when it is what was said last;
    /// a new link is told it at once.
    pub fn presence(&self, presence: Presence) {
        self.presence.send_if_modified(|now| {
            let changed = now.as_ref() != Some(&presence);
            if changed {
                *now = Some(presence);
            }
            changed
        });
    }

    /// Where this client last said the person is.
    #[must_use]
    pub fn presence_said(&self) -> Option<Presence> {
        self.presence.borrow().clone()
    }

    /// Say this client, `client`, is a phone the server may push to as `device`, or, with
    /// none, no longer. Nothing goes when it is what was said last; a new link is told at once.
    pub fn push_device(&self, client: ClientId, device: Option<PushDevice>) {
        let phone = (client, device);
        self.phone.send_if_modified(|now| {
            let changed = now.as_ref() != Some(&phone);
            if changed {
                *now = Some(phone);
            }
            changed
        });
    }
}

impl ServerCaller {
    /// A caller with no link behind it: its verbs wait in the [`CallQueue`] returned, to be
    /// answered there. What a view is tested with, against no server.
    #[must_use]
    pub fn queued() -> (Self, CallQueue) {
        let (calls, queued) = mpsc::channel(CALL_DEPTH);
        let presence = Arc::new(watch::Sender::new(None));
        let phone = Arc::new(watch::Sender::new(None));
        (Self { calls, presence, phone }, CallQueue { queued })
    }
}

/// The verbs a [`ServerCaller::queued`] sent, each with the way to answer it.
#[derive(Debug)]
pub struct CallQueue {
    queued: mpsc::Receiver<Call>,
}

impl CallQueue {
    /// The next verb sent, if one waits, and where its answer goes.
    pub fn try_next(&mut self) -> Option<(Verb, oneshot::Sender<Outcome>)> {
        self.queued.try_recv().ok().map(|call| (call.verb, call.reply))
    }
}

/// A verb waiting for the link, and where its answer goes.
#[derive(Debug)]
struct Call {
    verb: Verb,
    /// The key it goes under every time, when it changes something.
    key: Option<IdempotencyKey>,
    reply: oneshot::Sender<Outcome>,
    /// Once a drop lost its answer, how long it waits for a link to go again.
    until: Option<tokio::time::Instant>,
}

impl Call {
    fn answer(self, outcome: Outcome) {
        let _gave_up = self.reply.send(outcome);
    }

    /// Whether it gave up waiting to go again by `now`.
    fn lapsed(&self, now: tokio::time::Instant) -> bool {
        self.until.is_some_and(|until| now >= until)
    }
}

fn unreachable(why: &str) -> Outcome {
    Outcome::Error { code: ErrorCode::ServerUnreachable, message: why.to_owned() }
}

fn interrupted() -> Outcome {
    Outcome::Error {
        code: ErrorCode::Interrupted,
        message: "the link to the server dropped before the answer came".to_owned(),
    }
}

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Dial the server at `addr` as `role` on `endpoint` and keep dialling after every drop, on
/// `runtime`. `first` is a link already open (the one that proved the address), used before
/// any dial.
#[must_use]
pub fn spawn(
    runtime: &tokio::runtime::Handle,
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    role: Role,
    first: Option<ServerLink>,
) -> (ServerTask, mpsc::Receiver<ServerEvent>) {
    let (tx, rx) = mpsc::channel(EVENT_DEPTH);
    let (calls, queued) = mpsc::channel(CALL_DEPTH);
    let presence = Arc::new(watch::Sender::new(None));
    let said = presence.subscribe();
    let phone = Arc::new(watch::Sender::new(None));
    let resume = Arc::new(Notify::new());
    let up =
        Up { calls: queued, presence: said, phone: phone.subscribe(), resume: Arc::clone(&resume) };
    let task = runtime.spawn(run(endpoint, addr, role, first, tx, up));
    (ServerTask { task, calls, presence, phone, resume }, rx)
}

async fn run(
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    role: Role,
    mut first: Option<ServerLink>,
    tx: mpsc::Sender<ServerEvent>,
    mut up: Up,
) {
    let mut redial = slopty_net::redial::Redial::default();
    // The verbs whose answers a drop lost, oldest first, to go again on the next link.
    let mut carried: Vec<Call> = Vec::new();
    loop {
        let dialled = match first.take() {
            Some(link) => Ok(link),
            None => connect(&endpoint, &addr, role.clone()).await,
        };
        let mut another_build = false;
        let event = match dialled {
            Ok(link) => {
                redial.linked(std::time::Instant::now());
                let Some(why) = pump(link, &tx, &mut up, &mut carried).await else { return };
                ServerEvent::Unlinked { why }
            }
            Err(DialError::Refused(why)) => ServerEvent::Refused(why),
            // A tailnet node the policy grants no role at all is closed at the handshake,
            // before any hello: the same refusal, said sooner.
            Err(DialError::Net(NetError::NotGranted)) => ServerEvent::Refused(Refusal::NotGranted),
            Err(DialError::Net(NetError::WrongBuild(wrong))) => {
                another_build = true;
                ServerEvent::WrongBuild(UpdateNotice::server(addr.host(), &wrong))
            }
            Err(DialError::Net(e)) => ServerEvent::Unlinked { why: e.to_string() },
        };
        tracing::debug!(server = %addr, ?event, "server link down");
        let why = match &event {
            ServerEvent::Unlinked { why } => why.clone(),
            ServerEvent::Refused(refusal) => refusal.text().to_owned(),
            ServerEvent::WrongBuild(notice) => notice.to_string(),
            ServerEvent::Linked { .. } | ServerEvent::Message(_) => String::new(),
        };
        if tx.send(event).await.is_err() {
            return;
        }
        // Until the next dial there is no server to send a verb to. One on another build is
        // asked again only after a while: it changes only when someone updates it.
        let wait = if another_build {
            slopty_net::redial::WRONG_BUILD
        } else {
            redial.next(std::time::Instant::now())
        };
        let wait = tokio::time::sleep(wait);
        tokio::pin!(wait);
        loop {
            let due = carried.iter().filter_map(|c| c.until).min();
            tokio::select! {
                () = &mut wait => break,
                () = up.resume.notified() => break,
                () = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)),
                    if due.is_some() => give_up(&mut carried),
                Some(call) = up.calls.recv() => call.answer(unreachable(&why)),
            }
        }
    }
}

/// What goes up the link from this client: verbs, where the person is, and the phone it is;
/// and the word to probe it ([`ServerTask::resume`]).
#[derive(Debug)]
struct Up {
    calls: mpsc::Receiver<Call>,
    presence: watch::Receiver<Option<Presence>>,
    phone: watch::Receiver<Option<Phone>>,
    resume: Arc<Notify>,
}

/// Answer [`ErrorCode::Interrupted`] to the carried verbs that gave up waiting for a link.
fn give_up(carried: &mut Vec<Call>) {
    let now = tokio::time::Instant::now();
    for call in carried.extract_if(.., |c| c.lapsed(now)) {
        call.answer(interrupted());
    }
}

/// Hand on everything the link carries and send up every verb until it ends, the ones
/// `carried` from the last link first; the reason, or `None` once nobody listens. The verbs
/// whose answers it ends without are left in `carried`, to go again on the next.
async fn pump(
    link: ServerLink,
    tx: &mpsc::Sender<ServerEvent>,
    up: &mut Up,
    carried: &mut Vec<Call>,
) -> Option<String> {
    let Up { calls, presence, phone, resume } = up;
    tracing::debug!(server = %link.remote, name = %link.name, "server linked");
    let ServerLink { conn, name, link, build, tx: mut up, mut rx, .. } = link;
    let close = || conn.close(slopty_net::worker::close_code::NORMAL.into(), b"bye");
    if tx.send(ServerEvent::Linked { name, link, build }).await.is_err() {
        close();
        return None;
    }
    // A new link knows nothing of where the person is, nor of the phone: it is told at once.
    presence.mark_changed();
    phone.mark_changed();
    let (mut said_open, mut phone_open) = (true, true);
    let mut pending: HashMap<RequestId, Call> = HashMap::new();
    let mut next: RequestId = 0;
    // A probe on its way: what had arrived when its PING went, and when it gives up.
    let mut probe: Option<(u64, tokio::time::Instant)> = None;
    give_up(carried);
    let mut failed = None;
    for call in carried.drain(..) {
        if failed.is_some() {
            next = next.wrapping_add(1);
            pending.insert(next, call);
        } else if let Err(e) = send(&mut up, &mut pending, &mut next, call).await {
            failed = Some(e);
        }
    }
    let why = loop {
        if let Some(why) = failed.take() {
            break why;
        }
        tokio::select! {
            () = resume.notified() => {
                if probe.is_none() {
                    let before = slopty_net::endpoint::received_datagrams(&conn);
                    probe = tokio::time::Instant::now()
                        .checked_add(PROBE_DEADLINE)
                        .map(|until| (before, until));
                    slopty_net::endpoint::ping(&conn);
                }
            }
            () = tokio::time::sleep(PROBE_POLL), if probe.is_some() => {
                let Some((before, until)) = probe else { continue };
                if slopty_net::endpoint::received_datagrams(&conn) > before {
                    probe = None;
                } else if tokio::time::Instant::now() >= until {
                    close();
                    break "the server did not answer after a resume".to_owned();
                }
            }
            msg = rx.recv() => match msg {
                Ok(FromServer::Reply { id, outcome }) => {
                    if let Some(call) = pending.remove(&id) {
                        call.answer(outcome);
                    }
                }
                Ok(msg) => {
                    if tx.send(ServerEvent::Message(Box::new(msg))).await.is_err() {
                        close();
                        return None;
                    }
                }
                Err(e) => break e.to_string(),
            },
            changed = presence.changed(), if said_open => {
                if changed.is_err() {
                    said_open = false;
                    continue;
                }
                let said = presence.borrow_and_update().clone();
                if let Some(said) = said
                    && let Err(e) = up.send(&ToServer::Presence(said)).await
                {
                    break e.to_string();
                }
            }
            changed = phone.changed(), if phone_open => {
                if changed.is_err() {
                    phone_open = false;
                    continue;
                }
                let said = phone.borrow_and_update().clone();
                if let Some((client, device)) = said
                    && let Err(e) = up.send(&ToServer::PushDevice { client, device }).await
                {
                    break e.to_string();
                }
            }
            Some(call) = calls.recv() => {
                if let Err(e) = send(&mut up, &mut pending, &mut next, call).await {
                    break e;
                }
            }
        }
    };
    // In the order they were sent, each waiting from the first drop that lost its answer.
    let mut lost: Vec<_> = pending.into_iter().collect();
    lost.sort_unstable_by_key(|(id, _)| *id);
    let until = tokio::time::Instant::now().checked_add(RESEND_WITHIN);
    carried.extend(lost.into_iter().map(|(_, mut call)| {
        call.until = call.until.or(until);
        call
    }));
    Some(why)
}

/// Send `call` up the link as the request after `next`, and hold it in `pending` for its
/// answer: one that may have gone though the send failed is held all the same, so it goes
/// again on the next link under its key.
async fn send(
    up: &mut slopty_net::framed::FramedSend<ToServer>,
    pending: &mut HashMap<RequestId, Call>,
    next: &mut RequestId,
    call: Call,
) -> Result<(), String> {
    *next = next.wrapping_add(1);
    let request = ToServer::Request { id: *next, key: call.key.clone(), verb: call.verb.clone() };
    let sent = up.send(&request).await;
    pending.insert(*next, call);
    sent.map_err(|e| e.to_string())
}
