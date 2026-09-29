//! Two ends on different builds over real UDP: whichever end reads the other's wire prefix
//! closes with `WRONG_BUILD` and its build as the reason, and neither gets as far as a message
//! it cannot decode.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use slopty_core::ClientId;
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::worker::{WorkerListener, close_code};
    use slopty_net::{ClientMsg, NetError};
    use slopty_proto::codec;
    use slopty_proto::handshake::Hello;
    use slopty_proto::wire::{BUILD, FINGERPRINT, Prefix};

    const STEP: Duration = Duration::from_secs(5);

    fn other() -> Prefix {
        Prefix { fingerprint: FINGERPRINT ^ 1, build: "0.0.9+wire.0badf00d".to_owned() }
    }

    fn hello() -> Hello {
        Hello { client: ClientId::new(), name: "old".to_owned() }
    }

    fn hello_msg() -> Vec<u8> {
        codec::encode(&ClientMsg::Hello(hello())).unwrap().to_vec()
    }

    /// The application close `conn` ended with.
    async fn close_of(conn: &noq::Connection) -> (u64, String) {
        match tokio::time::timeout(STEP, conn.closed()).await.unwrap() {
            noq::ConnectionError::ApplicationClosed(close) => {
                let reason = String::from_utf8_lossy(&close.reason).into_owned();
                (close.error_code.into_inner(), reason)
            }
            other => panic!("not an application close: {other}"),
        }
    }

    /// Dial `at` bare and write `opening` on a fresh control stream, as a build that is not
    /// this one would. The endpoint and the stream's receiving half go with the connection:
    /// dropped, they would close it and stop the worker's writes.
    async fn dial_saying(at: SocketAddr, opening: &[u8]) -> (noq::Connection, Held) {
        let endpoint = bind_client().unwrap();
        let conn = endpoint.connect(at, "worker").unwrap().await.unwrap();
        let (mut send, recv) = conn.open_bi().await.unwrap();
        send.write_all(opening).await.unwrap();
        (conn, (endpoint, recv))
    }

    type Held = (noq::Endpoint, noq::RecvStream);

    fn listener() -> (WorkerListener, SocketAddr) {
        let listener =
            WorkerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let at = listener.local_addr().unwrap();
        (listener, at)
    }

    /// A client on another build, or on one from before the prefix, is closed at its prefix
    /// with `WRONG_BUILD` and the worker's build; the worker never hands it on as a client.
    #[tokio::test]
    async fn a_worker_closes_a_client_on_another_build() {
        let (listener, at) = listener();
        let mut prefixed = other().encode().to_vec();
        prefixed.extend_from_slice(&hello_msg());
        let bare = hello_msg();
        for opening in [prefixed, bare] {
            let (conn, _held) = dial_saying(at, &opening).await;
            let (code, reason) = close_of(&conn).await;
            assert_eq!(code, u64::from(close_code::WRONG_BUILD));
            assert_eq!(reason, BUILD, "the reason is the worker's build, for the person");
        }
        let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
        assert!(accepted.is_err(), "no client on another build was handed on");
    }

    /// A worker on another build: the client's dial fails as `WrongBuild` naming it, and the
    /// client closes with `WRONG_BUILD` and its own build, without waiting for an answer.
    #[tokio::test]
    async fn a_client_closes_a_worker_on_another_build() {
        let endpoint = slopty_net::endpoint::bind("127.0.0.1:0".parse().unwrap(), true).unwrap();
        let at = endpoint.local_addr().unwrap();
        let worker = tokio::spawn(async move {
            let conn = endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, _recv) = conn.accept_bi().await.unwrap();
            send.write_all(&other().encode()).await.unwrap();
            close_of(&conn).await
        });
        let dialing = bind_client().unwrap();
        let dialled = tokio::time::timeout(STEP, connect_addr(&dialing, at, hello()));
        let err = dialled.await.unwrap().unwrap_err();
        let NetError::WrongBuild(wrong) = &err else { panic!("{err:?}") };
        assert_eq!(wrong.peer, other().build);
        let (code, reason) = tokio::time::timeout(STEP, worker).await.unwrap().unwrap();
        assert_eq!(code, u64::from(close_code::WRONG_BUILD));
        assert_eq!(reason, BUILD);
    }

    /// Two ends of this build link as before: the prefixes pass and `Hello` comes through.
    #[tokio::test]
    async fn the_same_build_links() {
        let (listener, at) = listener();
        let accepting = tokio::spawn(async move { listener.accept().await.map(|c| c.hello.name) });
        let (conn, _held) = dial_saying(at, &{
            let mut opening = Prefix::this().encode().to_vec();
            opening.extend_from_slice(&hello_msg());
            opening
        })
        .await;
        let name = tokio::time::timeout(STEP, accepting).await.unwrap().unwrap();
        assert_eq!(name.as_deref(), Some("old"));
        conn.close(0_u32.into(), b"bye");
    }
}
