//! The QUIC front end: one task per link, by role.
//!
//! A worker link holds a [`Lease`] for as long as it reads; what the server sends it (requests)
//! goes through a queue to a writer task. A client or agent link gets the directory, then every
//! change, and the answers to its requests, each request dispatched on a task of its own so a
//! long `WaitFor` holds up nothing behind it.

use slopty_net::framed::FramedSend;
use slopty_net::server::{AcceptedLink, ServerListener};
use slopty_proto::PROTOCOL_VERSION;
use slopty_proto::server::{FromServer, Role, ToServer};
use tokio::sync::{broadcast, mpsc};

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
    let welcome = FromServer::Welcome { protocol: PROTOCOL_VERSION, name: hub.name().to_owned() };
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
    let writer = tokio::spawn(write(tx, queue));
    read_worker(&lease, &mut rx).await;
    writer.abort();
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"lease ended");
    drop(lease);
}

async fn read_worker(lease: &Lease, rx: &mut slopty_net::framed::FramedRecv<ToServer>) {
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

async fn write(mut tx: FramedSend<FromServer>, mut queue: mpsc::Receiver<FromServer>) {
    while let Some(msg) = queue.recv().await {
        if tx.send(&msg).await.is_err() {
            return;
        }
    }
}

async fn client(hub: Hub, link: AcceptedLink, name: String) {
    let AcceptedLink { conn, remote, mut tx, mut rx, .. } = link;
    tracing::info!(%name, %remote, "client connected");
    // Subscribed before the directory is read, so no change falls between the two.
    let mut changes = hub.subscribe();
    let welcome = FromServer::Welcome { protocol: PROTOCOL_VERSION, name: hub.name().to_owned() };
    if tx.send(&welcome).await.is_err()
        || tx.send(&FromServer::Directory(hub.directory())).await.is_err()
    {
        return;
    }
    let (out, mut replies) = mpsc::channel(LINK_QUEUE);
    let mut reader = tokio::spawn({
        let hub = hub.clone();
        async move {
            loop {
                match rx.recv().await {
                    Ok(ToServer::Request { id, verb }) => {
                        let (hub, out) = (hub.clone(), out.clone());
                        tokio::spawn(async move {
                            let outcome = hub.dispatch(verb).await;
                            let _gone = out.send(FromServer::Reply { id, outcome }).await;
                        });
                    }
                    Ok(_other) => tracing::debug!("ignored a message a client does not send"),
                    Err(e) => return e,
                }
            }
        }
    });
    loop {
        let msg = tokio::select! {
            ended = &mut reader => {
                let why = ended.map_or_else(|e| e.to_string(), |e| e.to_string());
                tracing::info!(%name, %remote, error = %why, "client link ended");
                break;
            }
            Some(reply) = replies.recv() => reply,
            change = changes.recv() => match change {
                Ok(change) => change,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::debug!(%name, missed, "client lagged; sending the directory again");
                    FromServer::Directory(hub.directory())
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
        };
        if tx.send(&msg).await.is_err() {
            reader.abort();
            break;
        }
    }
    conn.close(slopty_net::worker::close_code::NORMAL.into(), b"bye");
}
