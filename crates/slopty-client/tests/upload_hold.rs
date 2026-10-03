//! An upload held on its link (`WorkerLink::hold_uploads`) sends no byte of its file until it is
//! released, then all of them; a held upload that is cancelled stops rather than waiting on.
//! The e2e build holds uploads so a drawn upload is at the same point on every machine.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use slopty_client::WorkerLink;
    use slopty_core::{ClientId, SessionId, WorkerId, XferId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect};
    use slopty_net::streams::{self, RawRecv, Uni};
    use slopty_net::worker::{AcceptedClient, WorkerListener};
    use slopty_net::{ClientMsg, HostAddr, WorkerMsg};
    use slopty_proto::handshake::{Hello, HelloAck};
    use slopty_proto::transfer::{Dest, XferMsg};

    const WAIT: Duration = Duration::from_secs(30);
    /// Long enough for an upload that is not held to have sent its first chunk many times over.
    const QUIET: Duration = Duration::from_millis(300);
    const SIZE: usize = 1 << 20;

    /// A link to a worker played by the test, uploads held, and the worker's side of it.
    async fn held_link() -> (WorkerLink, AcceptedClient) {
        let listener =
            WorkerListener::bind(slopty_net::endpoint::any(0), Admission::default()).unwrap();
        let worker_at: HostAddr =
            SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port())).into();
        let accepted = tokio::spawn(async move {
            let mut client = listener.accept().await.unwrap();
            let ack = HelloAck {
                settings: String::new(),
                worker: WorkerId::new(),
                name: "worker".to_owned(),
                home: String::new(),
                caps: slopty_proto::server::WorkerCaps::bare(slopty_proto::server::Os::MacOs),
                load: 0.0,
                sessions: Vec::new(),
            };
            client.tx.send(&WorkerMsg::HelloAck(ack)).await.unwrap();
            client
        });
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let conn = connect(&endpoint, &worker_at, hello).await.unwrap();
        let link = WorkerLink::start(conn);
        link.hold_uploads(true);
        let client = tokio::time::timeout(WAIT, accepted).await.unwrap().unwrap();
        (link, client)
    }

    /// `SIZE` bytes in a file of `dir`, sent up as `xfer`; the file's bulk stream, once open.
    async fn upload(
        link: &WorkerLink,
        client: &mut AcceptedClient,
        dir: &tempfile::TempDir,
        xfer: XferId,
    ) -> RawRecv {
        let path = dir.path().join(format!("{xfer}.bin"));
        std::fs::write(&path, vec![7_u8; SIZE]).unwrap();
        link.remote().upload(xfer, vec![path], Dest::SessionCwd(SessionId::new()), false);
        loop {
            let msg = tokio::time::timeout(WAIT, client.rx.recv()).await.unwrap().unwrap();
            if matches!(msg, ClientMsg::Xfer(XferMsg::Begin { .. })) {
                break;
            }
        }
        let accepted = tokio::time::timeout(WAIT, streams::accept_uni(&client.conn)).await;
        let Uni::Bulk { rx, .. } = accepted.unwrap().unwrap() else { panic!("a bulk stream") };
        rx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_upload_sends_nothing_until_it_is_released() {
        let (link, mut client) = held_link().await;
        let dir = tempfile::tempdir().unwrap();
        let mut rx = upload(&link, &mut client, &dir, XferId::new()).await;
        let early = tokio::time::timeout(QUIET, rx.chunk(SIZE)).await;
        let sent = early.map(|chunk| chunk.map(|bytes| bytes.map(|b| b.len())));
        assert!(sent.is_err(), "a held upload sent bytes: {sent:?}");

        link.hold_uploads(false);
        let mut got = 0_usize;
        while let Some(chunk) = tokio::time::timeout(WAIT, rx.chunk(SIZE)).await.unwrap().unwrap() {
            got = got.saturating_add(chunk.len());
        }
        assert_eq!(got, SIZE, "released, the whole file arrives");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_upload_that_is_cancelled_stops() {
        let (link, mut client) = held_link().await;
        let dir = tempfile::tempdir().unwrap();
        let xfer = XferId::new();
        let mut rx = upload(&link, &mut client, &dir, xfer).await;
        link.remote().cancel(xfer);
        let ended = tokio::time::timeout(WAIT, rx.chunk(SIZE)).await;
        let ended = ended.expect("the held stream ends once cancelled");
        assert!(ended.is_err(), "reset, not finished: {ended:?}");
    }
}
