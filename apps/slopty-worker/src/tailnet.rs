//! How each client's packets travel, as this machine's Tailscale sees it.
//!
//! Straight, through a peer relay, or through DERP. One task reads the daemon's status while any
//! client listens, and sleeps while none does; each client on the tailnet is told its path when
//! it has one and again when it changes.
//!
//! A read, not a watch of the daemon's IPN bus: the bus carries no peer's path, and its engine
//! updates are the daemon polling itself every 2 s (`docs/decisions/workers.md`, "The tailnet
//! path is read, not watched").

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use slopty_net::admission::Admission;
use slopty_proto::WorkerMsg;
use slopty_proto::tailnet::LinkPath;
use slopty_tailnet::{Path, Status};
use tokio::sync::{Notify, mpsc, watch};

/// How often the status is read while a client listens: a path moves from DERP to direct
/// within a few seconds of traffic starting, and a status read costs the daemon a few ms.
const POLL: Duration = Duration::from_secs(2);

/// The daemon's latest status, for every client's path.
#[derive(Clone, Debug)]
pub struct Paths {
    status: Arc<watch::Sender<Option<Arc<Status>>>>,
    /// Wakes the reader when a client starts listening.
    listening: Arc<Notify>,
}

impl Paths {
    /// Read the status through the Tailscale `admission` asks while any client listens; while
    /// there is none, no client is told a path.
    pub fn spawn(admission: Admission) -> Self {
        let status = Arc::new(watch::Sender::new(None));
        let listening = Arc::new(Notify::new());
        tokio::spawn(poll(admission, Arc::clone(&status), Arc::clone(&listening)));
        Self { status, listening }
    }

    /// Tell the client at `remote` its path through `out`, each time it changes, until the
    /// client goes. A client that is not on the tailnet is told nothing.
    pub async fn report(&self, remote: SocketAddr, out: mpsc::Sender<WorkerMsg>) {
        let ip = remote.ip().to_canonical();
        if !slopty_net::admission::on_tailnet(ip) {
            return;
        }
        let status = self.status.subscribe();
        self.listening.notify_one();
        report(status, ip, out).await;
    }
}

/// Read the status every [`POLL`] while a client listens. While none does, nothing is read and
/// the task sleeps; the status it last read is dropped, and the first client to listen again
/// has it read at once rather than a poll later.
async fn poll(
    admission: Admission,
    status: Arc<watch::Sender<Option<Arc<Status>>>>,
    listening: Arc<Notify>,
) -> ! {
    loop {
        if status.receiver_count() == 0 {
            status.send_replace(None);
            listening.notified().await;
            continue;
        }
        if let Some(api) = admission.local_api() {
            match api.status().await {
                Ok(read) => {
                    status.send_replace(Some(Arc::new(read)));
                }
                Err(e) => tracing::debug!(error = %e, "tailscale status"),
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn report(
    mut status: watch::Receiver<Option<Arc<Status>>>,
    ip: IpAddr,
    out: mpsc::Sender<WorkerMsg>,
) {
    let mut told = None;
    loop {
        let path = status.borrow_and_update().as_deref().and_then(|s| link_path(s, ip));
        if let Some(path) = path
            && told.as_ref() != Some(&path)
        {
            tracing::info!(%ip, ?path, "client path");
            if out.send(WorkerMsg::Path(path.clone())).await.is_err() {
                return;
            }
            told = Some(path);
        }
        if status.changed().await.is_err() {
            return;
        }
    }
}

/// The path to the node at `ip`, `None` while none is chosen or no node has it.
fn link_path(status: &Status, ip: IpAddr) -> Option<LinkPath> {
    match status.node_at(ip)?.path() {
        Path::Direct(_) => Some(LinkPath::Direct),
        Path::PeerRelay(_) => Some(LinkPath::PeerRelay),
        Path::Derp(region) => Some(LinkPath::Derp { region }),
        Path::Idle => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(peer_cur_addr: &str, active: bool) -> Arc<Status> {
        let json = format!(
            r#"{{"BackendState":"Running","Peer":{{"k":{{"ID":"n2","HostName":"laptop",
            "DNSName":"laptop.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.4"],
            "CurAddr":"{peer_cur_addr}","Relay":"fra","Active":{active}}}}}}}"#
        );
        Arc::new(serde_json::from_str(&json).unwrap())
    }

    /// A client is told nothing while no path is chosen, then DERP, then direct once disco
    /// finds one, and nothing again for a status that says the same.
    #[tokio::test]
    async fn a_client_hears_its_path_when_it_changes_and_only_then() {
        let tx = watch::Sender::new(Some(status("", false)));
        let (out, mut heard) = mpsc::channel(8);
        let task = tokio::spawn(report(tx.subscribe(), "100.64.0.4".parse().unwrap(), out));
        let next = async |heard: &mut mpsc::Receiver<WorkerMsg>| match tokio::time::timeout(
            Duration::from_secs(5),
            heard.recv(),
        )
        .await
        {
            Ok(Some(WorkerMsg::Path(path))) => path,
            other => panic!("expected a path, got {other:?}"),
        };
        tx.send_replace(Some(status("", true)));
        assert_eq!(next(&mut heard).await, LinkPath::Derp { region: "fra".into() });
        tx.send_replace(Some(status("", true)));
        tx.send_replace(Some(status("192.168.1.20:41641", true)));
        assert_eq!(next(&mut heard).await, LinkPath::Direct, "no repeat of DERP in between");
        tx.send_replace(Some(status("192.168.1.20:41641", true)));
        drop(tx);
        task.await.unwrap();
        assert!(heard.try_recv().is_err(), "the same path is not told twice");
    }

    /// Nothing is read while no client listens; the first client to listen has the status
    /// read at once and hears its path, not a poll later.
    #[tokio::test]
    async fn the_status_is_read_when_a_client_listens_and_only_then() {
        let (api, seen) = slopty_tailnet::fake::daemon(|_path| {
            (
                200,
                r#"{"BackendState":"Running","Peer":{"k":{"ID":"n2","HostName":"laptop",
            "DNSName":"laptop.ts.net.","OS":"macOS","TailscaleIPs":["100.64.0.4"],
            "CurAddr":"192.168.1.20:41641","Relay":"fra","Active":true}}}"#
                    .to_owned(),
            )
        })
        .await
        .unwrap();
        let paths = Paths::spawn(Admission::with_tailnet(Vec::new(), Some(api)));
        tokio::time::sleep(POLL + Duration::from_millis(500)).await;
        assert!(seen.lock().is_empty(), "read with no client listening: {:?}", seen.lock());

        let (out, mut heard) = mpsc::channel(8);
        let asked = std::time::Instant::now();
        let reporting = tokio::spawn({
            let paths = paths.clone();
            async move { paths.report("100.64.0.4:5000".parse().unwrap(), out).await }
        });
        let told = tokio::time::timeout(POLL, heard.recv()).await;
        assert!(
            matches!(told, Ok(Some(WorkerMsg::Path(LinkPath::Direct)))),
            "the path, within a poll: {told:?}"
        );
        assert!(asked.elapsed() < POLL / 2, "told after {:?}", asked.elapsed());
        reporting.abort();
    }

    /// An address no node has, and one idle node, give no path.
    #[test]
    fn an_unknown_or_idle_node_has_no_path() {
        let idle = status("", false);
        assert_eq!(link_path(&idle, "100.64.0.4".parse().unwrap()), None);
        assert_eq!(link_path(&idle, "100.64.0.9".parse().unwrap()), None);
    }
}
