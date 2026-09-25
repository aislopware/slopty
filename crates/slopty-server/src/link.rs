//! The QUIC front end: one task per link, by role.
//!
//! A worker link holds a [`Lease`] for as long as it both reads and writes; what the server sends
//! it (requests) goes through a queue to its writer. A client or agent link gets the fleet's
//! state (the directory, then every terminal and the agent in it), then every change, and the
//! answers to its requests, each request dispatched on a task of its own so a long `WaitFor`
//! holds up nothing behind it. A link that falls behind the changes gets the state again, with
//! what it was told that no longer holds taken back.

use std::collections::{HashMap, HashSet};

use slopty_core::{SessionId, WorkerId};
use slopty_net::NetError;
use slopty_net::framed::{FramedRecv, FramedSend};
use slopty_net::server::{AcceptedLink, ServerListener};
use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus};
use slopty_proto::codec::CodecError;
use slopty_proto::orchestration::{ErrorCode, Outcome};
use slopty_proto::server::{Event, FromServer, Role, ToServer};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

use crate::hub::{Hub, Lease};

/// Messages queued for one link before its sender waits.
const LINK_QUEUE: usize = 256;

/// Serve every link `listener` accepts until its endpoint closes.
pub async fn serve(listener: ServerListener, hub: Hub) {
    while let Some(link) = listener.accept().await {
        let hub = hub.clone();
        tokio::spawn(async move {
            match link.role.clone() {
                Role::Worker(registration) => worker(hub, link, registration).await,
                Role::Client { name, .. } | Role::Agent { name } => client(hub, link, name).await,
            }
        });
    }
}

async fn worker(hub: Hub, link: AcceptedLink, registration: slopty_proto::server::Registration) {
    let (out, queue) = mpsc::channel(LINK_QUEUE);
    let welcome = FromServer::Welcome { name: hub.name().to_owned() };
    // First in the queue before the worker is reachable, so no request can overtake it.
    if out.try_send(welcome).is_err() {
        return;
    }
    let (id, name, remote) = (registration.worker, registration.name.clone(), link.remote);
    let lease = match hub.register(registration, remote.ip(), out) {
        Ok(lease) => lease,
        Err(why) => {
            tracing::info!(worker = %id, %name, %remote, ?why, "worker refused");
            link.refuse(why).await;
            return;
        }
    };
    tracing::info!(worker = %id, %name, %remote, "worker online");
    let AcceptedLink { conn, tx, mut rx, .. } = link;
    // Either half failing ends the lease: a worker the server cannot write to answers nothing.
    tokio::select! {
        () = read_worker(&lease, &mut rx) => {}
        () = write_worker(&lease, tx, queue) => {}
    }
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"lease ended");
    drop(lease);
}

async fn read_worker(lease: &Lease, rx: &mut FramedRecv<ToServer>) {
    loop {
        match rx.recv().await {
            Ok(msg) => lease.handle(msg),
            Err(e) => {
                tracing::info!(worker = %lease.worker(), error = %e, "worker link ended");
                return;
            }
        }
    }
}

/// Send the worker what is queued until the link fails. A request too large for one message
/// is answered here with an error: nothing of it was written.
async fn write_worker(
    lease: &Lease,
    mut tx: FramedSend<FromServer>,
    mut queue: mpsc::Receiver<FromServer>,
) {
    while let Some(msg) = queue.recv().await {
        let failed = match tx.send(&msg).await {
            Ok(()) => continue,
            Err(NetError::Codec(CodecError::TooLarge { len, max })) => match msg {
                FromServer::Request { id, .. } => {
                    tracing::warn!(worker = %lease.worker(), id, len, max, "a request too large for the link");
                    lease.answer(id, too_large(ErrorCode::Invalid, len, max));
                    continue;
                }
                _ => NetError::Codec(CodecError::TooLarge { len, max }),
            },
            Err(e) => e,
        };
        tracing::info!(worker = %lease.worker(), error = %failed, "worker link write failed");
        return;
    }
}

/// Send `msg`; a reply too large for one message goes as an error in its place.
async fn send_to_client(tx: &mut FramedSend<FromServer>, msg: FromServer) -> Result<(), NetError> {
    match tx.send(&msg).await {
        Err(NetError::Codec(CodecError::TooLarge { len, max })) => match msg {
            FromServer::Reply { id, .. } => {
                tracing::warn!(id, len, max, "a reply too large for the link");
                tx.send(&FromServer::Reply { id, outcome: too_large(ErrorCode::Failed, len, max) })
                    .await
            }
            _ => Err(NetError::Codec(CodecError::TooLarge { len, max })),
        },
        sent => sent,
    }
}

/// The error in place of a message of `len` bytes, over the `max` one message carries.
fn too_large(code: ErrorCode, len: usize, max: usize) -> Outcome {
    Outcome::Error {
        code,
        message: format!("the message is {len} bytes, more than the {max} one message carries"),
    }
}

async fn client(hub: Hub, link: AcceptedLink, name: String) {
    let AcceptedLink { conn, remote, mut tx, rx, .. } = link;
    tracing::info!(%name, %remote, "client connected");
    // Subscribed before the state is read, so no change falls between the two.
    let mut changes = hub.subscribe();
    let mut told = Told::default();
    let welcome = FromServer::Welcome { name: hub.name().to_owned() };
    if tx.send(&welcome).await.is_err() {
        return;
    }
    let state = told.resync(&hub);
    if tell(&mut tx, &mut told, state).await.is_err() {
        return;
    }
    let (out, mut replies) = mpsc::channel(LINK_QUEUE);
    let mut reader = tokio::spawn(read_requests(hub.clone(), rx, out));
    loop {
        let msgs = tokio::select! {
            ended = &mut reader => {
                let why = ended.map_or_else(|e| e.to_string(), |e| e.to_string());
                tracing::info!(%name, %remote, error = %why, "client link ended");
                break;
            }
            Some(reply) = replies.recv() => vec![reply],
            change = changes.recv() => match change {
                Ok(change) => vec![change],
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(%name, missed, "client lagged; sending the state again");
                    told.resync(&hub)
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        if let Err(e) = tell(&mut tx, &mut told, msgs).await {
            tracing::info!(%name, %remote, error = %e, "client link write failed");
            break;
        }
    }
    // Takes the link's requests with it: a `WaitFor` or `Events` nobody will read the answer to
    // stops here, not at its timeout minutes later.
    reader.abort();
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"bye");
}

/// Dispatch each request the client sends until its link ends, and return why it ended.
async fn read_requests(
    hub: Hub,
    mut rx: FramedRecv<ToServer>,
    out: mpsc::Sender<FromServer>,
) -> NetError {
    // Dropped (returning or aborted) with the link, which aborts every request still running.
    let mut requests = JoinSet::new();
    loop {
        match rx.recv().await {
            Ok(ToServer::Request { id, verb }) => {
                while requests.try_join_next().is_some() {}
                let (hub, out) = (hub.clone(), out.clone());
                requests.spawn(async move {
                    let outcome = hub.dispatch(verb).await;
                    let _gone = out.send(FromServer::Reply { id, outcome }).await;
                });
            }
            Ok(_other) => tracing::debug!("ignored a message a client does not send"),
            Err(e) => return e,
        }
    }
}

/// Send `msgs` in order, noting each as told.
async fn tell(
    tx: &mut FramedSend<FromServer>,
    told: &mut Told,
    msgs: Vec<FromServer>,
) -> Result<(), NetError> {
    for msg in msgs {
        told.note(&msg);
        send_to_client(tx, msg).await?;
    }
    Ok(())
}

/// The agents a link's client was last told of, so the state sent again after it lagged can
/// take back each one that ended or left its terminal in the changes it never heard.
#[derive(Debug, Default)]
struct Told {
    agents: HashMap<(WorkerId, SessionId), (AgentKind, AgentSource)>,
}

impl Told {
    fn note(&mut self, msg: &FromServer) {
        match msg {
            FromServer::Event(Event::Agent { worker, event }) => {
                let key = (*worker, event.session);
                if event.status == AgentStatus::None {
                    self.agents.remove(&key);
                } else {
                    self.agents.insert(key, (event.kind, event.source));
                }
            }
            FromServer::Event(Event::SessionClosed { worker, session }) => {
                self.agents.remove(&(*worker, *session));
            }
            _other => {}
        }
    }

    /// The state from the registry: the directory, then each terminal and its agent, then the
    /// end of every agent this client was told of that the registry no longer has. Quiet: a
    /// state sent again raises no attention of its own.
    fn resync(&self, hub: &Hub) -> Vec<FromServer> {
        let (directory, terminals) = hub.state();
        let open: HashSet<(WorkerId, SessionId)> =
            terminals.iter().map(|(worker, s)| (*worker, s.id)).collect();
        let mut msgs = vec![FromServer::Directory(directory)];
        let mut agents = HashSet::new();
        for (worker, summary) in terminals {
            let (session, agent) = (summary.id, summary.agent.clone());
            msgs.push(FromServer::Event(Event::SessionOpened { worker, summary }));
            if let Some(agent) = agent {
                agents.insert((worker, session));
                let event = quiet(session, agent.kind, agent.status, agent.source);
                msgs.push(FromServer::Event(Event::Agent { worker, event }));
            }
        }
        for (&(worker, session), &(kind, source)) in &self.agents {
            if agents.contains(&(worker, session)) {
                continue;
            }
            msgs.push(FromServer::Event(if open.contains(&(worker, session)) {
                Event::Agent { worker, event: quiet(session, kind, AgentStatus::None, source) }
            } else {
                Event::SessionClosed { worker, session }
            }));
        }
        msgs
    }
}

const fn quiet(
    session: SessionId,
    kind: AgentKind,
    status: AgentStatus,
    source: AgentSource,
) -> AgentEvent {
    AgentEvent {
        session,
        kind,
        status,
        agent_session: None,
        detail: None,
        attention: false,
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::connect;
    use slopty_proto::agent::BlockReason;
    use slopty_proto::orchestration::{EventFilter, Verb};
    use slopty_proto::terminal::CloseReason;

    use super::*;
    use crate::WAIT_CAP_MS;
    use crate::hub::tests::{registration, summary};

    fn report(session: SessionId, status: AgentStatus) -> ToServer {
        ToServer::Agent(quiet(session, AgentKind::ClaudeCode, status, AgentSource::Hook))
    }

    /// What a client shows of the agents the server reports, as the app keeps it: the status
    /// of each session with one, taken off when the agent leaves or the terminal closes.
    fn apply(shown: &mut HashMap<SessionId, AgentStatus>, told: &mut Told, msgs: Vec<FromServer>) {
        for msg in msgs {
            told.note(&msg);
            match msg {
                FromServer::Event(Event::Agent { event, .. })
                    if event.status == AgentStatus::None =>
                {
                    shown.remove(&event.session);
                }
                FromServer::Event(Event::Agent { event, .. }) => {
                    shown.insert(event.session, event.status);
                }
                FromServer::Event(Event::SessionClosed { session, .. }) => {
                    shown.remove(&session);
                }
                _other => {}
            }
        }
    }

    /// A client gets every agent's status when it connects, and after it fell behind the
    /// changes it gets them again with what it no longer should show taken back: an agent that
    /// left, a terminal that closed, one that opened, a status that moved.
    #[tokio::test]
    async fn a_client_gets_the_agents_on_connect_and_again_after_it_lagged() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let [left, closed, moved, opened] = [(); 4].map(|()| SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let listed = vec![summary(left), summary(closed), summary(moved)];
        let lease = hub
            .register(registration(worker, listed), std::net::IpAddr::from([100, 64, 0, 7]), tx)
            .unwrap();
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        for session in [left, closed, moved] {
            lease.handle(report(session, blocked.clone()));
        }

        let (mut shown, mut told) = (HashMap::new(), Told::default());
        let state = told.resync(&hub);
        assert!(matches!(state.first(), Some(FromServer::Directory(list)) if list.len() == 1));
        apply(&mut shown, &mut told, state);
        let all_blocked: HashMap<_, _> =
            [left, closed, moved].into_iter().map(|s| (s, blocked.clone())).collect();
        assert_eq!(shown, all_blocked, "on connect");

        // Changes the lagging client never hears.
        lease.handle(report(left, AgentStatus::None));
        lease.handle(ToServer::SessionClosed { session: closed, reason: CloseReason::Exited });
        lease.handle(report(moved, AgentStatus::Working));
        lease.handle(ToServer::SessionOpened(summary(opened)));
        lease.handle(report(opened, blocked.clone()));

        let state = told.resync(&hub);
        apply(&mut shown, &mut told, state);
        let now: HashMap<_, _> = [(moved, AgentStatus::Working), (opened, blocked)].into();
        assert_eq!(shown, now, "after the lag");
        assert!(
            told.resync(&hub)
                .iter()
                .all(|m| !matches!(m, FromServer::Event(Event::SessionClosed { .. }))),
            "nothing left to take back"
        );
    }

    async fn until(what: &str, done: impl Fn() -> bool) {
        let waited = tokio::time::timeout(Duration::from_secs(10), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(waited.is_ok(), "{what}");
    }

    /// A client that leaves with an `Events` wait running takes the wait with it, rather than
    /// leaving it to run out its minutes on the server.
    #[tokio::test]
    async fn a_client_that_leaves_takes_its_requests_with_it() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let at = HostAddr::from(listener.local_addr().unwrap());
        let serving = tokio::spawn(serve(listener, hub.clone()));
        let endpoint = bind_client().unwrap();
        let role = Role::Agent { name: "test".to_owned() };
        let mut link = connect(&endpoint, &at, role).await.unwrap();
        let verb = Verb::Events { since: None, timeout_ms: WAIT_CAP_MS, filter: EventFilter::All };
        link.tx.send(&ToServer::Request { id: 1, verb }).await.unwrap();
        until("the wait started", || hub.events_waiting() == 1).await;

        link.close();
        until("the wait ended with the link", || hub.events_waiting() == 0).await;
        serving.abort();
    }
}
