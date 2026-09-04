//! Host and client in one process: pair with a ticket, reconnect on trust alone, reject strangers.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_core::{ClientId, HostId, SessionId};
    use slopty_net::client::{HandshakeError, bind_client, connect, connect_with_ticket};
    use slopty_net::host::{HostListener, open_session_stream};
    use slopty_net::pairing::TrustStore;
    use slopty_net::{HostMsg, Reach, SecretKey};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::{Caps, ClientKind, Hello, HelloAck, Rejection};
    use slopty_proto::terminal::TermEvent;

    fn hello(client: ClientId) -> Hello {
        Hello {
            protocol: PROTOCOL_VERSION,
            client,
            kind: ClientKind::Tool,
            name: "test".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            pair_token: None,
        }
    }

    fn ack() -> HelloAck {
        HelloAck {
            protocol: PROTOCOL_VERSION,
            host: HostId::nil(),
            name: "host".to_owned(),
            app_version: "0".to_owned(),
            caps: Caps::empty(),
            sessions: Vec::new(),
        }
    }

    async fn host(reach: Reach) -> (HostListener, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::open(&dir.path().join("trust.json")).unwrap();
        let listener = HostListener::bind(store, reach).await.unwrap();
        (listener, dir)
    }

    /// Relays off on both ends: the ticket carries only IP addresses, the dial lands on a
    /// direct path, and nothing waits on a relay handshake.
    #[tokio::test]
    async fn direct_only_pairs_without_a_relay() {
        let (listener, _dir) = host(Reach::DirectOnly).await;
        tokio::time::timeout(Duration::from_secs(5), listener.online()).await.unwrap();
        let ticket = listener.pair_ticket().await;
        assert!(ticket.addr.addrs.iter().all(|a| !a.is_relay()), "{:?}", ticket.addr);
        assert!(ticket.addr.addrs.iter().any(|a| !a.is_relay()), "{:?}", ticket.addr);

        let host_task = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut first = listener.accept().await.unwrap();
                first.tx.send(&HostMsg::HelloAck(ack())).await.unwrap();
                first.conn.closed().await;
            })
        };
        let client_ep = bind_client(SecretKey::generate(), Reach::DirectOnly).await.unwrap();
        let conn = tokio::time::timeout(
            Duration::from_secs(10),
            connect_with_ticket(&client_ep, Reach::DirectOnly, &ticket, hello(ClientId::new())),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(slopty_net::endpoint::relayed(&conn.conn), Some(false));
        conn.conn.close(0_u32.into(), b"done");
        tokio::time::timeout(Duration::from_secs(10), host_task).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn pair_then_reconnect_then_reject_stranger() {
        let (listener, _dir) = host(Reach::Anywhere).await;
        let ticket = listener.pair_ticket().await;
        let client_id = ClientId::new();
        let client_secret = SecretKey::generate();
        let client_ep = bind_client(client_secret.clone(), Reach::Anywhere).await.unwrap();

        // Host side: accept, ack, open one session stream, send an event.
        let session = SessionId::new();
        let host_task = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let mut first = listener.accept().await.unwrap();
                assert!(first.newly_paired, "ticket redeems on first contact");
                first.tx.send(&HostMsg::HelloAck(ack())).await.unwrap();
                let mut stream = open_session_stream(&first.conn, session).await.unwrap();
                stream.send(&TermEvent::Bell).await.unwrap();
                stream.finish().unwrap();
                first.conn.closed().await;

                let mut second = listener.accept().await.unwrap();
                assert!(!second.newly_paired, "trusted key needs no token");
                assert_eq!(second.remote, first.remote);
                second.tx.send(&HostMsg::HelloAck(ack())).await.unwrap();
                second.conn.closed().await;
            })
        };

        let conn = connect_with_ticket(&client_ep, Reach::Anywhere, &ticket, hello(client_id))
            .await
            .unwrap();
        assert_eq!(conn.ack.name, "host");
        let (header, mut events) = conn.accept_session_stream().await.unwrap();
        assert_eq!(header.session, session);
        assert_eq!(events.recv().await.unwrap(), TermEvent::Bell);
        conn.conn.close(0_u32.into(), b"done");

        // Reconnect without a token.
        let again = connect(&client_ep, Reach::Anywhere, ticket.addr.clone(), hello(client_id))
            .await
            .unwrap();
        assert_eq!(again.ack.name, "host");
        again.conn.close(0_u32.into(), b"done");
        tokio::time::timeout(Duration::from_secs(10), host_task).await.unwrap().unwrap();

        // A stranger with no token is refused; the token cannot be redeemed twice. The accept loop
        // swallows rejections, so it only returns if a stranger somehow gets through.
        let reject_loop = {
            let listener = listener.clone();
            tokio::spawn(async move {
                let intruder = listener.accept().await;
                assert!(intruder.is_none(), "stranger accepted");
            })
        };
        let stranger = bind_client(SecretKey::generate(), Reach::Anywhere).await.unwrap();
        let err = connect(&stranger, Reach::Anywhere, ticket.addr.clone(), hello(ClientId::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, HandshakeError::Rejected(Rejection::NotPaired)), "{err:?}");
        let err = connect_with_ticket(&stranger, Reach::Anywhere, &ticket, hello(ClientId::new()))
            .await
            .unwrap_err();
        assert!(matches!(err, HandshakeError::Rejected(Rejection::NotPaired)), "{err:?}");
        assert!(listener.store().lock().await.is_paired(&client_secret.public()));
        reject_loop.abort();
    }
}
