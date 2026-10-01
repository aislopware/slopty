//! The QUIC front end: one task per link, by role.
//!
//! A worker link holds a [`Lease`] for as long as it both reads and writes; what the server sends
//! it (requests) goes through a queue to its writer. A client or agent link gets the fleet's
//! state (the directory, every terminal and the agent in it, every project), then every change, and
//! the answers to its requests, each request dispatched on a task of its own so a long `WaitFor`
//! holds up nothing behind it. A link that falls behind the changes gets the state again, which
//! replaces everything it was told before.

use std::net::IpAddr;

use slopty_core::SessionId;
use slopty_net::NetError;
use slopty_net::framed::{FramedRecv, FramedSend};
use slopty_net::server::{AcceptedLink, ServerListener};
use slopty_proto::codec::CodecError;
use slopty_proto::orchestration::{ErrorCode, Outcome};
use slopty_proto::server::{FromServer, Role, ToServer};
use slopty_tailnet::LocalApi;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

use crate::hub::{Hub, Lease, Speaker};

/// Messages queued for one link before its sender waits.
const LINK_QUEUE: usize = 256;

/// Serve every link `listener` accepts until its endpoint closes.
pub async fn serve(listener: ServerListener, hub: Hub) {
    while let Some(link) = listener.accept().await {
        let (hub, admission) = (hub.clone(), listener.admission().clone());
        tokio::spawn(async move {
            match link.role.clone() {
                Role::Worker(registration) => {
                    let tailscale = admission.local_api();
                    worker(hub, link, *registration, tailscale.as_ref()).await;
                }
                Role::Client { name, .. } => client(hub, link, name, Speaker::Person).await,
                Role::Agent { name, vouch } => {
                    let proven = vouch.filter(|v| proven(&hub, &name, v.session, &v.token));
                    let speaker = proven.map_or(Speaker::Agent, |v| Speaker::Proven(v.session));
                    client(hub, link, name, speaker).await;
                }
                Role::Shell { name, session, token } => {
                    let speaker = match token {
                        Some(token) if proven(&hub, &name, session, &token) => {
                            Speaker::ProvenShell(session)
                        }
                        _ => Speaker::Shell(session),
                    };
                    client(hub, link, name, speaker).await;
                }
            }
        });
    }
}

/// Whether `token` proves the link `name` speaks from the terminal `session`; a token that does
/// not is logged, and the link speaks as one that showed none.
fn proven(hub: &Hub, name: &str, session: SessionId, token: &str) -> bool {
    let proven = hub.vouches(session, token);
    if !proven {
        tracing::warn!(name, %session, "a link's token does not prove its terminal");
    }
    proven
}

async fn worker(
    hub: Hub,
    link: AcceptedLink,
    registration: slopty_proto::server::Registration,
    tailscale: Option<&LocalApi>,
) {
    let (out, queue) = mpsc::channel(LINK_QUEUE);
    let welcome = FromServer::Welcome { name: hub.name().to_owned() };
    // First in the queue before the worker is reachable, so no request can overtake it.
    if out.try_send(welcome).is_err() {
        return;
    }
    let (id, name, remote) = (registration.worker, registration.name.clone(), link.remote);
    let at = published(remote.ip(), registration.listen.ip(), tailscale).await;
    let lease = match hub.register(registration, at, out) {
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

/// Where clients reach a worker that dialed in from `ip` and listens on `bound`.
///
/// A worker bound to one address is reachable there alone; one bound to loopback only from this
/// machine, at the loopback address it dialed from. A worker on every interface of the server's
/// own machine dials over loopback, which no other machine can use, so it is published at this
/// machine's tailnet address when Tailscale gives one.
async fn published(ip: IpAddr, bound: IpAddr, tailscale: Option<&LocalApi>) -> IpAddr {
    if bound.is_loopback() {
        return ip;
    }
    if !bound.is_unspecified() {
        return bound;
    }
    let Some(api) = tailscale.filter(|_| ip.is_loopback()) else { return ip };
    match api.status().await {
        Ok(status) => status.me.as_ref().and_then(slopty_tailnet::Node::ipv4).unwrap_or(ip),
        Err(e) => {
            tracing::warn!(error = %e, "tailscale did not say this machine's address");
            ip
        }
    }
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

async fn client(hub: Hub, link: AcceptedLink, name: String, speaker: Speaker) {
    let AcceptedLink { conn, remote, mut tx, rx, .. } = link;
    tracing::info!(%name, %remote, "client connected");
    // Subscribed before the state is read, so no change falls between the two.
    let mut changes = hub.subscribe();
    let welcome = FromServer::Welcome { name: hub.name().to_owned() };
    if tx.send(&welcome).await.is_err() {
        return;
    }
    if tell(&mut tx, state(&hub)).await.is_err() {
        return;
    }
    let (out, mut replies) = mpsc::channel(LINK_QUEUE);
    let mut reader = tokio::spawn(read_requests(hub.clone(), rx, out, speaker));
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
                    state(&hub)
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        if let Err(e) = tell(&mut tx, msgs).await {
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
    speaker: Speaker,
) -> NetError {
    // Dropped (returning or aborted) with the link, which aborts every request still running.
    let mut requests = JoinSet::new();
    loop {
        match rx.recv().await {
            Ok(ToServer::Request { id, key, verb }) => {
                while requests.try_join_next().is_some() {}
                let (hub, out) = (hub.clone(), out.clone());
                requests.spawn(async move {
                    let outcome = hub.dispatch_as(speaker, key, verb).await;
                    let _gone = out.send(FromServer::Reply { id, outcome }).await;
                });
            }
            Ok(_other) => tracing::debug!("ignored a message a client does not send"),
            Err(e) => return e,
        }
    }
}

/// Send `msgs` in order.
async fn tell(tx: &mut FramedSend<FromServer>, msgs: Vec<FromServer>) -> Result<(), NetError> {
    for msg in msgs {
        send_to_client(tx, msg).await?;
    }
    Ok(())
}

/// The fleet's state: the directory, then every terminal and the agent in it, then every
/// project.
fn state(hub: &Hub) -> Vec<FromServer> {
    let mut snapshot = hub.state();
    let parts = Hub::project_parts(&mut snapshot);
    let mut msgs =
        vec![FromServer::Directory(snapshot.directory), FromServer::Terminals(snapshot.terminals)];
    msgs.extend(parts.into_iter().map(|part| FromServer::Projects(Box::new(part))));
    msgs
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::connect;
    use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent};
    use slopty_proto::orchestration::{EventFilter, Happening, HubEvent, Verb};
    use slopty_proto::terminal::CloseReason;

    use super::*;
    use crate::WAIT_CAP_MS;
    use crate::hub::tests::{registration, summary};

    /// A worker on every interface of the server's own machine dials over loopback and is
    /// published at the machine's tailnet address; any other worker at the address it dialed
    /// from, without asking Tailscale. With no Tailscale, or one that fails, loopback stays. A
    /// worker bound to one address is published there, and one bound to loopback keeps the
    /// loopback address it dialed from: nothing listens for it on the tailnet.
    #[tokio::test]
    async fn a_worker_on_the_servers_machine_is_published_at_its_tailnet_address() {
        let (api, seen) = slopty_tailnet::fake::daemon(|_| {
            let me = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac",
                "DNSName":"mac.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.3","fd7a::3"]}}"#;
            (200, me.to_owned())
        })
        .await
        .unwrap();
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let (tailnet, every) = (ip("100.64.0.3"), ip("::"));
        assert_eq!(published(ip("127.0.0.1"), every, Some(&api)).await, tailnet);
        assert_eq!(published(ip("::1"), every, Some(&api)).await, tailnet);
        let asked = seen.lock().len();
        assert_eq!(published(ip("100.64.0.9"), every, Some(&api)).await, ip("100.64.0.9"));
        assert_eq!(published(ip("::1"), ip("127.0.0.1"), Some(&api)).await, ip("::1"));
        assert_eq!(published(ip("::1"), ip("10.0.0.5"), Some(&api)).await, ip("10.0.0.5"));
        assert_eq!(seen.lock().len(), asked, "only a worker on every interface asks");
        assert_eq!(published(ip("127.0.0.1"), every, None).await, ip("127.0.0.1"));
        let (broken, _) =
            slopty_tailnet::fake::daemon(|_| (500, "stuck".to_owned())).await.unwrap();
        assert_eq!(published(ip("127.0.0.1"), every, Some(&broken)).await, ip("127.0.0.1"));
    }

    fn report(session: SessionId, status: AgentStatus) -> ToServer {
        let agent = SessionAgent {
            kind: AgentKind::ClaudeCode,
            status,
            source: AgentSource::Hook,
            since_ms: WallMs::ZERO,
            mode: None,
        };
        ToServer::Agent(agent.quiet_event(session))
    }

    /// What a client shows of the agents the server reports, as the app keeps it: the status
    /// of each session with one, all of them replaced by a snapshot of the terminals, taken
    /// off when the agent leaves or the terminal closes.
    fn apply(shown: &mut HashMap<SessionId, AgentStatus>, msgs: Vec<FromServer>) {
        for msg in msgs {
            match msg {
                FromServer::Terminals(terminals) => {
                    shown.clear();
                    shown.extend(
                        terminals.into_iter().filter_map(|(_, s)| Some((s.id, s.agent?.status))),
                    );
                }
                FromServer::Event(HubEvent { what: Happening::Agent { event, .. }, .. })
                    if event.status == AgentStatus::None =>
                {
                    shown.remove(&event.session);
                }
                FromServer::Event(HubEvent { what: Happening::Agent { event, .. }, .. }) => {
                    shown.insert(event.session, event.status);
                }
                FromServer::Event(HubEvent { what: Happening::SessionClosed { term }, .. }) => {
                    shown.remove(&term.session);
                }
                _other => {}
            }
        }
    }

    /// A client gets every agent's status when it connects, and after it fell behind the
    /// changes the state again replaces all it was shown: an agent that left, a terminal that
    /// closed, one that opened, a status that moved. A client that kept up reads the same from
    /// the pushed events, which are the log's own.
    #[tokio::test]
    async fn a_client_gets_the_agents_on_connect_and_again_after_it_lagged() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let mut pushed = hub.subscribe();
        let worker = WorkerId::new();
        let [left, closed, moved, opened] = [(); 4].map(|()| SessionId::new());
        let (tx, _rx) = mpsc::channel(8);
        let listed = vec![summary(left), summary(closed), summary(moved)];
        let lease =
            hub.register(registration(worker, listed), IpAddr::from([100, 64, 0, 7]), tx).unwrap();
        let blocked = AgentStatus::Blocked(BlockReason::Question);
        for session in [left, closed, moved] {
            lease.handle(report(session, blocked.clone()));
        }

        let mut shown = HashMap::new();
        let first = state(&hub);
        assert!(matches!(first.first(), Some(FromServer::Directory(list)) if list.len() == 1));
        apply(&mut shown, first);
        let all_blocked: HashMap<_, _> =
            [left, closed, moved].into_iter().map(|s| (s, blocked.clone())).collect();
        assert_eq!(shown, all_blocked, "on connect");
        let mut kept_up = shown.clone();
        while pushed.try_recv().is_ok() {}
        let cursor = read_log(&hub, None).await.1;

        // Changes the lagging client never hears.
        lease.handle(report(left, AgentStatus::None));
        lease.handle(ToServer::SessionClosed { session: closed, reason: CloseReason::Exited });
        lease.handle(report(moved, AgentStatus::Working));
        lease.handle(ToServer::SessionChanged(summary(opened)));
        lease.handle(report(opened, blocked.clone()));

        apply(&mut shown, state(&hub));
        let now: HashMap<_, _> = [(moved, AgentStatus::Working), (opened, blocked)].into();
        assert_eq!(shown, now, "after the lag");

        let mut events = Vec::new();
        while let Ok(msg) = pushed.try_recv() {
            events.push(msg);
        }
        let pushed_events: Vec<HubEvent> = events
            .iter()
            .filter_map(|m| match m {
                FromServer::Event(e) => Some(e.clone()),
                _other => None,
            })
            .collect();
        assert_eq!(pushed_events.len(), 5, "every change pushed once: {pushed_events:?}");
        assert_eq!(read_log(&hub, Some(cursor)).await.0, pushed_events, "the log's own events");
        apply(&mut kept_up, events);
        assert_eq!(kept_up, now, "the pushed events say the same");
    }

    async fn read_log(hub: &Hub, since: Option<u64>) -> (Vec<HubEvent>, u64) {
        match hub.dispatch(Verb::Events { since, timeout_ms: 0, filter: EventFilter::All }).await {
            Outcome::Events { events, next, .. } => (events, next),
            other => panic!("not events: {other:?}"),
        }
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
        let role = Role::Agent { name: "test".to_owned(), vouch: None };
        let mut link = connect(&endpoint, &at, role).await.unwrap();
        let verb = Verb::Events { since: None, timeout_ms: WAIT_CAP_MS, filter: EventFilter::All };
        link.tx.send(&ToServer::Request { id: 1, key: None, verb }).await.unwrap();
        until("the wait started", || hub.events_waiting() == 1).await;

        link.close();
        until("the wait ended with the link", || hub.events_waiting() == 0).await;
        serving.abort();
    }
}
