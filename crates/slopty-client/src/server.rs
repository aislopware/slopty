//! The one link a client keeps to the server, dialled at start and redialled forever after it
//! drops.
//!
//! A server on a different build is said so ([`UpdateNotice`]) and asked again only after
//! [`slopty_net::redial::WRONG_BUILD`].
//!
//! Every message it carries is handed on as a [`ServerEvent`], and verbs go up it through a
//! [`ServerCaller`] ([`ServerCaller::wake`] wakes a sleeping worker).
//!
//! The server is the control plane only, so nothing here is on a terminal's or a stream's
//! path: while this link is down the workers are still dialled directly
//! ([`crate::directory`]).

use std::collections::HashMap;

use slopty_core::WorkerId;
use slopty_net::server::{DialError, ServerLink, connect};
use slopty_net::{HostAddr, NetError};
use slopty_proto::RequestId;
use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
use slopty_proto::server::{FromServer, Refusal, Role, ToServer};
use tokio::sync::{mpsc, oneshot};

use crate::update::UpdateNotice;

/// Server messages queued for the UI at most.
const EVENT_DEPTH: usize = 256;
/// Verbs waiting for the link at most; a caller past them waits its turn.
const CALL_DEPTH: usize = 64;

/// What the link says.
#[derive(Debug)]
pub enum ServerEvent {
    /// The server welcomed this client.
    Linked {
        /// Its name.
        name: String,
    },
    /// A message from it: the directory, a worker's change, an event.
    Message(Box<FromServer>),
    /// A dial failed or the link dropped; the next attempt is on its way. A server on a
    /// different build is one of these, `why` being its [`UpdateNotice`].
    Unlinked {
        /// Why.
        why: String,
    },
    /// The server answered and turned this dialer away. The next attempt is on its way all the
    /// same: a change to the tailnet policy can let it in.
    Refused(Refusal),
}

/// The running link; dropping it closes the link and ends the redials.
#[derive(Debug)]
pub struct ServerTask {
    task: tokio::task::JoinHandle<()>,
    calls: mpsc::Sender<Call>,
}

impl ServerTask {
    /// A handle that sends verbs up this link, for as long as it runs.
    #[must_use]
    pub fn caller(&self) -> ServerCaller {
        ServerCaller { calls: self.calls.clone() }
    }
}

/// Sends verbs to the server over the client's one link and hands back each answer.
///
/// A verb goes once: one that changes something is not sent again after a drop, and answers
/// [`ErrorCode::Interrupted`] instead. While the link is down, a verb answers
/// [`ErrorCode::ServerUnreachable`] at once.
#[derive(Clone, Debug)]
pub struct ServerCaller {
    calls: mpsc::Sender<Call>,
}

impl ServerCaller {
    /// Send `verb` and wait for the server's answer.
    pub async fn call(&self, verb: Verb) -> Outcome {
        let (reply, answer) = oneshot::channel();
        if self.calls.send(Call { verb, reply }).await.is_err() {
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
}

impl ServerCaller {
    /// A caller with no link behind it: its verbs wait in the [`CallQueue`] returned, to be
    /// answered there. What a view is tested with, against no server.
    #[must_use]
    pub fn queued() -> (Self, CallQueue) {
        let (calls, queued) = mpsc::channel(CALL_DEPTH);
        (Self { calls }, CallQueue { queued })
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
    reply: oneshot::Sender<Outcome>,
}

impl Call {
    fn answer(self, outcome: Outcome) {
        let _gave_up = self.reply.send(outcome);
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
    let task = runtime.spawn(run(endpoint, addr, role, first, tx, queued));
    (ServerTask { task, calls }, rx)
}

async fn run(
    endpoint: slopty_net::Endpoint,
    addr: HostAddr,
    role: Role,
    mut first: Option<ServerLink>,
    tx: mpsc::Sender<ServerEvent>,
    mut calls: mpsc::Receiver<Call>,
) {
    let mut redial = slopty_net::redial::Redial::default();
    loop {
        let dialled = match first.take() {
            Some(link) => Ok(link),
            None => connect(&endpoint, &addr, role.clone()).await,
        };
        let mut another_build = false;
        let event = match dialled {
            Ok(link) => {
                redial.linked(std::time::Instant::now());
                let Some(why) = pump(link, &tx, &mut calls).await else { return };
                ServerEvent::Unlinked { why }
            }
            Err(DialError::Refused(why)) => ServerEvent::Refused(why),
            Err(DialError::Net(NetError::WrongBuild(wrong))) => {
                another_build = true;
                let notice = UpdateNotice::server(addr.host(), &wrong);
                ServerEvent::Unlinked { why: notice.to_string() }
            }
            Err(DialError::Net(e)) => ServerEvent::Unlinked { why: e.to_string() },
        };
        tracing::debug!(server = %addr, ?event, "server link down");
        let why = match &event {
            ServerEvent::Unlinked { why } => why.clone(),
            ServerEvent::Refused(refusal) => refusal.text().to_owned(),
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
            tokio::select! {
                () = &mut wait => break,
                Some(call) = calls.recv() => call.answer(unreachable(&why)),
            }
        }
    }
}

/// Hand on everything the link carries and send up every verb until it ends; the reason, or
/// `None` once nobody listens.
async fn pump(
    link: ServerLink,
    tx: &mpsc::Sender<ServerEvent>,
    calls: &mut mpsc::Receiver<Call>,
) -> Option<String> {
    tracing::debug!(server = %link.remote, name = %link.name, "server linked");
    let ServerLink { conn, name, tx: mut up, mut rx, .. } = link;
    let close = || conn.close(slopty_net::worker::close_code::NORMAL.into(), b"bye");
    if tx.send(ServerEvent::Linked { name }).await.is_err() {
        close();
        return None;
    }
    let mut pending: HashMap<RequestId, Call> = HashMap::new();
    let mut next: RequestId = 0;
    let why = loop {
        tokio::select! {
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
            Some(call) = calls.recv() => {
                next = next.wrapping_add(1);
                let request = ToServer::Request { id: next, key: None, verb: call.verb.clone() };
                if let Err(e) = up.send(&request).await {
                    call.answer(interrupted());
                    break e.to_string();
                }
                pending.insert(next, call);
            }
        }
    };
    for (_, call) in pending {
        call.answer(interrupted());
    }
    Some(why)
}
