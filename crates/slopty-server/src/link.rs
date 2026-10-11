//! The QUIC front end: one task per link, by role.
//!
//! A worker link holds a [`Lease`] for as long as it both reads and writes; what the server sends
//! it (requests) goes through a queue to its writer. A client or agent link gets the fleet's
//! state (the directory, every terminal and the agent in it, every project), then every change, and
//! the answers to its requests, each request dispatched on a task of its own so a long `WaitFor`
//! holds up nothing behind it. A link that falls behind the changes gets the state again, which
//! replaces everything it was told before.

use std::net::IpAddr;
use std::time::Duration;

use slopty_core::SessionId;
use slopty_net::NetError;
use slopty_net::admission::Admission;
use slopty_net::framed::{FramedRecv, FramedSend};
use slopty_net::server::{AcceptedLink, ServerListener};
use slopty_proto::codec::CodecError;
use slopty_proto::orchestration::{ErrorCode, Outcome};
use slopty_proto::server::{FromServer, Role, ToServer};
use slopty_tailnet::LocalApi;
use tokio::sync::{broadcast, mpsc, watch};
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
                Role::Worker(registration) => worker(hub, link, *registration, &admission).await,
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
    admission: &Admission,
) {
    let (out, queue) = mpsc::channel(LINK_QUEUE);
    let welcome = FromServer::Welcome {
        name: hub.name().to_owned(),
        link: hub.number_link(),
        build: slopty_proto::wire::this_build(),
    };
    // First in the queue before the worker is reachable, so no request can overtake it.
    if out.try_send(welcome).is_err() {
        return;
    }
    let (id, name, remote) = (registration.worker, registration.name.clone(), link.remote);
    let bound = registration.listen.ip();
    let at = published(remote.ip(), bound, admission.local_api().as_ref()).await;
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
        () = republish(&lease, (remote.ip(), bound, at), admission, REPUBLISH_EVERY) => {}
    }
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"lease ended");
    drop(lease);
}

/// How often a worker published at loopback asks Tailscale again for this machine's address.
const REPUBLISH_EVERY: Duration = Duration::from_secs(10);

/// Keep a worker published where other machines reach it, for as long as its lease lives.
///
/// One that dialed over loopback and listens everywhere is published at this machine's tailnet
/// address, but only once Tailscale gives one: until then it is published at loopback, which
/// no other machine can use. So while it is, Tailscale is asked again `every` so often, and the
/// address it gives once it is up is published in its place. An address once published stays
/// when Tailscale stops answering, rather than flap back to loopback. Never returns for any
/// other worker.
#[expect(
    clippy::infinite_loop,
    reason = "it watches for as long as the lease lives; the link's select ends it with the lease"
)]
async fn republish(
    lease: &Lease,
    (ip, bound, at): (IpAddr, IpAddr, IpAddr),
    admission: &Admission,
    every: Duration,
) {
    if !(ip.is_loopback() && bound.is_unspecified()) {
        return std::future::pending().await;
    }
    let mut at = at;
    loop {
        tokio::time::sleep(every).await;
        let now = published(ip, bound, admission.local_api().as_ref()).await;
        if now != at && !now.is_loopback() {
            lease.republish(now);
            at = now;
        }
    }
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

/// Send the worker what is queued, and whether a pocketed phone can answer as that moves, until
/// the link fails. A request too large for one message is answered here with an error: nothing
/// of it was written.
async fn write_worker(
    lease: &Lease,
    mut tx: FramedSend<FromServer>,
    mut queue: mpsc::Receiver<FromServer>,
) {
    let mut pushes = lease.pushes();
    // Told after its welcome when a phone can answer already.
    pushes.mark_changed();
    let mut told = None;
    while let Some(msg) = next_for_worker(&mut queue, &mut pushes, &mut told).await {
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

/// What goes to a worker next: its welcome, the first queued, before anything; then the latest
/// word on whether a pocketed phone can answer whenever it moved from what the worker was
/// `told`, ahead of what is queued. That word never waits in the queue, so a full queue can
/// neither drop nor hold it back, and the worker hears the last of it. `None` once the queue
/// is closed.
async fn next_for_worker(
    queue: &mut mpsc::Receiver<FromServer>,
    pushes: &mut watch::Receiver<bool>,
    told: &mut Option<bool>,
) -> Option<FromServer> {
    let Some(was) = *told else {
        let welcome = queue.recv().await;
        // A worker starts out holding nothing for a phone.
        *told = Some(false);
        return welcome;
    };
    let mut hub = true;
    loop {
        tokio::select! {
            biased;
            changed = pushes.changed(), if hub => {
                if changed.is_err() {
                    hub = false;
                    continue;
                }
                let now = *pushes.borrow_and_update();
                if now != was {
                    *told = Some(now);
                    return Some(FromServer::Pushes(now));
                }
            }
            msg = queue.recv() => return msg,
        }
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
    let (out, mut replies) = mpsc::channel(LINK_QUEUE);
    let number = hub.number_link();
    // Only a person's client is where a person is, and gets notices.
    let seated = (speaker == Speaker::Person).then(|| hub.seat(number, name.clone(), out.clone()));
    let seat = seated.as_ref().map(crate::hub::Seated::link);
    let mut pushable = seated.as_ref().map(crate::hub::Seated::pushable);
    let welcome = FromServer::Welcome {
        name: hub.name().to_owned(),
        link: number,
        build: slopty_proto::wire::this_build(),
    };
    if tx.send(&welcome).await.is_err() {
        return;
    }
    if tell(&mut tx, state(&hub)).await.is_err() {
        return;
    }
    let mut reader = tokio::spawn(read_requests(hub.clone(), rx, out, speaker, seat));
    loop {
        let msgs = tokio::select! {
            ended = &mut reader => {
                let why = ended.map_or_else(|e| e.to_string(), |e| e.to_string());
                tracing::info!(%name, %remote, error = %why, "client link ended");
                break;
            }
            Some(reply) = replies.recv() => vec![reply],
            now = next_pushable(&mut pushable) => vec![FromServer::Pushable(now)],
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
    drop(seated);
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"bye");
}

/// The next word on whether the server can push to the phone a client said it is, once it
/// moved: never for a client that is no person's, before it said a phone, or once the hub is
/// gone. A watch, so the word never waits behind a full queue and the client hears the last.
async fn next_pushable(word: &mut Option<watch::Receiver<Option<bool>>>) -> bool {
    loop {
        let Some(rx) = word.as_mut() else { return std::future::pending().await };
        if rx.changed().await.is_err() {
            *word = None;
            continue;
        }
        let now = *rx.borrow_and_update();
        if let Some(now) = now {
            return now;
        }
    }
}

/// Dispatch each request the client sends until its link ends, and return why it ended.
async fn read_requests(
    hub: Hub,
    mut rx: FramedRecv<ToServer>,
    out: mpsc::Sender<FromServer>,
    speaker: Speaker,
    seat: Option<u64>,
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
            Ok(ToServer::Presence(presence)) => {
                if let Some(link) = seat {
                    hub.presence(link, presence);
                } else {
                    tracing::debug!("ignored where the person is, from no person's client");
                }
            }
            Ok(ToServer::PushDevice { client, device }) => {
                if let Some(link) = seat {
                    hub.push_device(link, client, device);
                } else {
                    tracing::debug!("ignored a phone, from no person's client");
                }
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

/// The fleet's state: the directory, then every project, the attention ladder, which says
/// where every agent's thread stands, and where the person is.
fn state(hub: &Hub) -> Vec<FromServer> {
    let mut snapshot = hub.state();
    let parts = Hub::project_parts(&mut snapshot);
    let mut msgs = vec![FromServer::Directory(snapshot.directory)];
    msgs.extend(parts.into_iter().map(|part| FromServer::Projects(Box::new(part))));
    msgs.push(FromServer::Ladder(Box::new(hub.ladder())));
    msgs.push(FromServer::Present(hub.present()));
    msgs
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use slopty_core::{SessionId, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::connect;
    use slopty_proto::orchestration::{EventFilter, Happening, HubEvent, Verb};
    use slopty_proto::terminal::CloseReason;
    use slopty_proto::thread::attention::Rung;
    use slopty_proto::thread::{Phase, ThreadId};

    use super::*;
    use crate::WAIT_CAP_MS;
    use crate::hub::tests::{registration, summary, table};

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

    /// Tailscale coming up after a worker on the server's own machine registered, published
    /// at loopback for want of a tailnet address, republishes it at the machine's tailnet
    /// address, and every client hears it; a worker that dialed from elsewhere is never asked
    /// about again.
    #[tokio::test]
    async fn tailscale_coming_up_republishes_a_worker_published_at_loopback() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static UP: AtomicBool = AtomicBool::new(false);
        let (api, seen) = slopty_tailnet::fake::daemon(|_| {
            if !UP.load(Ordering::SeqCst) {
                return (503, "starting".to_owned());
            }
            let me = r#"{"BackendState":"Running","Self":{"ID":"n1","HostName":"mac",
                "DNSName":"mac.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.3"]}}"#;
            (200, me.to_owned())
        })
        .await
        .unwrap();
        let admission = Admission::with_tailnet(Vec::new(), Some(api));
        let hub = Hub::new("server".to_owned(), Vec::new());
        let worker = WorkerId::new();
        let (tx, _rx) = mpsc::channel(8);
        let registration = registration(worker, Vec::new());
        let (loopback, every) = (IpAddr::from([127, 0, 0, 1]), registration.listen.ip());
        let at = published(loopback, every, admission.local_api().as_ref()).await;
        assert_eq!(at, loopback, "no tailnet address yet");
        let lease = hub.register(registration, at, tx).unwrap();
        let mut heard = hub.subscribe();
        let address = || hub.directory()[0].address.clone();
        let watching =
            republish(&lease, (loopback, every, at), &admission, Duration::from_millis(10));
        let republished = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            assert_eq!(address(), "127.0.0.1:45550", "Tailscale is not up");
            UP.store(true, Ordering::SeqCst);
            while address() != "100.64.0.3:45550" {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::select! {
            () = watching => unreachable!("it watches for as long as the lease"),
            done = tokio::time::timeout(Duration::from_secs(10), republished) => {
                done.expect("republished once Tailscale is up");
            }
        }
        let told = std::iter::from_fn(|| heard.try_recv().ok()).any(
            |msg| matches!(msg, FromServer::Worker(info) if info.address == "100.64.0.3:45550"),
        );
        assert!(told, "every client hears it");

        let asked = seen.lock().len();
        let elsewhere = republish(
            &lease,
            (IpAddr::from([100, 64, 0, 9]), every, at),
            &admission,
            Duration::from_millis(1),
        );
        assert!(tokio::time::timeout(Duration::from_millis(50), elsewhere).await.is_err());
        assert_eq!(seen.lock().len(), asked, "a worker that dialed from elsewhere asks nothing");
    }

    /// What a client shows of the agents, as the app keeps it: each terminal's agent's phase,
    /// all of them replaced by the ladder of a state, moved by the pushed rungs, and taken off
    /// when the terminal closes.
    fn apply(shown: &mut HashMap<SessionId, Rung>, msgs: Vec<FromServer>) {
        for msg in msgs {
            match msg {
                FromServer::Ladder(ladder) => {
                    shown.clear();
                    shown.extend(ladder.tiles.iter().map(|(term, s)| (term.session, s.rung)));
                }
                FromServer::Event(HubEvent {
                    what: Happening::Rung { terminal: Some(session), agent, .. },
                    ..
                }) => {
                    shown.insert(session, agent.rung);
                }
                FromServer::Event(HubEvent { what: Happening::SessionClosed { term }, .. }) => {
                    shown.remove(&term.session);
                }
                _other => {}
            }
        }
    }

    /// A client reads every agent from the ladder in the state it gets when it connects, and
    /// after it fell behind the changes the state again replaces all it was shown: a terminal
    /// that closed, one that opened, an agent that moved. A client that kept up reads the same
    /// from the pushed rungs, which are the log's own.
    #[tokio::test]
    async fn a_client_gets_the_agents_on_connect_and_again_after_it_lagged() {
        let hub = Hub::new("server".to_owned(), Vec::new());
        let mut pushed = hub.subscribe();
        let worker = WorkerId::new();
        let [closed, moved, opened] = [(); 3].map(|()| SessionId::new());
        let [on_closed, on_moved, on_opened] = [(); 3].map(|()| ThreadId::new());
        let (tx, _rx) = mpsc::channel(8);
        let listed = vec![summary(closed), summary(moved)];
        let lease =
            hub.register(registration(worker, listed), IpAddr::from([100, 64, 0, 7]), tx).unwrap();
        let asking = Phase::NeedsYou;
        lease.handle(table(&[(on_closed, closed, asking), (on_moved, moved, asking)]));
        hub.rank_ladder();

        let mut shown = HashMap::new();
        let first = state(&hub);
        assert!(matches!(first.first(), Some(FromServer::Directory(list)) if list.len() == 1));
        apply(&mut shown, first);
        let all_asking: HashMap<_, _> = [(closed, Rung::NeedsYou), (moved, Rung::NeedsYou)].into();
        assert_eq!(shown, all_asking, "on connect");
        let mut kept_up = shown.clone();
        while pushed.try_recv().is_ok() {}
        let cursor = read_log(&hub, None).await.1;

        // Changes the lagging client never hears.
        lease.handle(ToServer::SessionClosed { session: closed, reason: CloseReason::Exited });
        lease.handle(ToServer::SessionChanged(summary(opened)));
        lease.handle(table(&[(on_moved, moved, Phase::Working), (on_opened, opened, asking)]));
        hub.rank_ladder();

        apply(&mut shown, state(&hub));
        let now: HashMap<_, _> = [(moved, Rung::Working), (opened, Rung::NeedsYou)].into();
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
        assert_eq!(pushed_events.len(), 4, "every change pushed once: {pushed_events:?}");
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

    /// The word on whether a pocketed phone can answer reaches a worker whose link's queue is
    /// full: it never waits in the queue, so it is neither dropped nor held back. The welcome
    /// still goes first; the word goes next, ahead of what was queued, which then goes in its
    /// order; and a word that moved back to what the worker was told is not sent at all.
    #[tokio::test]
    async fn a_worker_hears_the_pushes_word_past_a_full_queue() {
        let (out, mut queue) = mpsc::channel(LINK_QUEUE);
        let (said, mut pushes) = watch::channel(false);
        pushes.mark_changed();
        let numbered = |n: usize| FromServer::Welcome {
            name: format!("queued {n}"),
            link: 1,
            build: String::new(),
        };
        let welcome =
            FromServer::Welcome { name: "server".to_owned(), link: 1, build: String::new() };
        out.try_send(welcome.clone()).unwrap();
        let mut queued = Vec::new();
        while out.try_send(numbered(queued.len())).is_ok() {
            queued.push(Some(numbered(queued.len())));
        }
        assert_eq!(queued.len(), LINK_QUEUE - 1, "the queue is full");
        said.send_replace(true);

        let mut told = None;
        let next = async |queue: &mut mpsc::Receiver<FromServer>, pushes: &mut _, told: &mut _| {
            tokio::time::timeout(Duration::from_secs(5), next_for_worker(queue, pushes, told))
                .await
                .unwrap()
        };
        assert_eq!(next(&mut queue, &mut pushes, &mut told).await, Some(welcome), "first");
        let word = next(&mut queue, &mut pushes, &mut told).await;
        assert_eq!(word, Some(FromServer::Pushes(true)), "the word, ahead of the queue");
        let mut sent = Vec::new();
        for _ in 0..queued.len() {
            sent.push(next(&mut queue, &mut pushes, &mut told).await);
        }
        assert_eq!(sent, queued, "what was queued, in its order");

        said.send_replace(false);
        said.send_replace(true);
        drop(out);
        assert_eq!(next(&mut queue, &mut pushes, &mut told).await, None, "back as it was told");
    }
}
