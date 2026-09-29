//! The client against a worker and a server on another build, over UDP on loopback: each is
//! said so with what updates it, and neither is redialled on the fast backoff.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use slopty_client::server::{ServerEvent, spawn};
    use slopty_client::update::UpdateNotice;
    use slopty_core::ClientId;
    use slopty_net::HostAddr;
    use slopty_proto::handshake::Hello;
    use slopty_proto::server::Role;
    use slopty_proto::wire::{FINGERPRINT, Prefix};

    const WAIT: Duration = Duration::from_secs(10);

    /// Longer than the first three redials of the fast backoff together (0.25 + 0.5 + 1 s).
    const NO_REDIAL: Duration = Duration::from_millis(2500);

    const OTHER: &str = "0.0.9+wire.0badf00d";

    /// A peer on another build at a loopback port: it answers every control stream with its
    /// prefix and counts the connections that reach it.
    fn another_build(lease: bool) -> (HostAddr, Arc<AtomicUsize>) {
        let local = "127.0.0.1:0".parse().unwrap();
        let endpoint = if lease {
            slopty_net::endpoint::bind_lease(local).unwrap()
        } else {
            slopty_net::endpoint::bind(local, true).unwrap()
        };
        let port = endpoint.local_addr().unwrap().port();
        let dials = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&dials);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let conn = incoming.await.unwrap();
                    let Ok((mut send, _recv)) = conn.accept_bi().await else { return };
                    let prefix = Prefix { fingerprint: FINGERPRINT ^ 1, build: OTHER.to_owned() };
                    let _sent = send.write_all(&prefix.encode()).await;
                    conn.closed().await;
                });
            }
        });
        (HostAddr::new("127.0.0.1", port), dials)
    }

    /// A dial to a worker on another build fails as the notice that names both builds and
    /// the deploy that updates it: the state the app shows on that worker.
    #[tokio::test]
    async fn a_worker_on_another_build_needs_an_update() {
        let (addr, _dials) = another_build(false);
        let endpoint = slopty_net::client::bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "test".to_owned() };
        let dialled =
            tokio::time::timeout(WAIT, slopty_net::client::connect(&endpoint, &addr, hello));
        let err = dialled.await.unwrap().unwrap_err();
        let notice = UpdateNotice::for_worker_dial(addr.host(), &err).expect("a wrong build");
        assert_eq!(notice.peer, OTHER);
        assert_eq!(notice.title(), "This worker runs a different build");
        assert_eq!(notice.command(), "slopty worker deploy 127.0.0.1 --update");
    }

    /// The server link says a server on another build once, with its notice, and does not dial
    /// it again on the fast backoff.
    #[tokio::test]
    async fn a_server_on_another_build_is_said_once_and_not_redialled() {
        let (addr, dials) = another_build(true);
        let endpoint = slopty_net::client::bind_client().unwrap();
        let role = Role::Client { name: "test".to_owned() };
        let (_task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role, None);

        let event = tokio::time::timeout(WAIT, events.recv()).await.unwrap().unwrap();
        let ServerEvent::Unlinked { why } = event else { panic!("{event:?}") };
        assert!(why.starts_with("127.0.0.1 runs a different build. It runs 0.0.9"), "{why}");
        assert!(why.ends_with("run `slopty server install` on 127.0.0.1"), "{why}");

        let more = tokio::time::timeout(NO_REDIAL, events.recv()).await;
        assert!(more.is_err(), "nothing more to say: {more:?}");
        assert_eq!(dials.load(Ordering::SeqCst), 1, "dialled once, not on the fast backoff");
    }
}
