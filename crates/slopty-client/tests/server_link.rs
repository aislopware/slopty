//! The client's server link against a real server listener in-process, over UDP on loopback:
//! the directory arrives, a drop is reported, and the link comes back by itself.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_client::directory::{Change, Directory, ServerState};
    use slopty_client::server::{ServerEvent, spawn};
    use slopty_core::{ClientId, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::handshake::ClientKind;
    use slopty_proto::server::{FromServer, Liveness, Os, Role, WorkerCaps, WorkerInfo};
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(20);

    fn role() -> Role {
        Role::Client { client: ClientId::new(), kind: ClientKind::Tool, name: "test".to_owned() }
    }

    fn worker(id: WorkerId, liveness: Liveness) -> WorkerInfo {
        WorkerInfo {
            worker: id,
            name: "studio".to_owned(),
            address: "127.0.0.1:45550".to_owned(),
            liveness,
            caps: WorkerCaps {
                os: Os::MacOs,
                os_version: "26.5".to_owned(),
                arch: "aarch64".to_owned(),
                cpus: 8,
                memory: 1,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: false,
                can_inject: false,
                load: 0.0,
                version: "0".to_owned(),
            },
            last_seen_ms: 0,
        }
    }

    async fn next(rx: &mut mpsc::Receiver<ServerEvent>) -> ServerEvent {
        tokio::time::timeout(WAIT, rx.recv()).await.unwrap().unwrap()
    }

    async fn welcome(listener: &ServerListener, directory: Vec<WorkerInfo>) -> AcceptedLink {
        let mut link = tokio::time::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        assert!(matches!(link.role, Role::Client { .. }), "{:?}", link.role);
        link.tx
            .send(&FromServer::Welcome { protocol: PROTOCOL_VERSION, name: "hub".to_owned() })
            .await
            .unwrap();
        link.tx.send(&FromServer::Directory(directory)).await.unwrap();
        link
    }

    #[tokio::test]
    async fn the_link_takes_the_directory_and_comes_back_after_a_drop() {
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);
        let id = WorkerId::new();
        let mut dir = Directory::default();

        let mut link = welcome(&listener, vec![worker(id, Liveness::Online)]).await;
        let ServerEvent::Linked { name } = next(&mut events).await else { panic!("not linked") };
        assert_eq!(name, "hub");
        dir.set_server(ServerState::Linked { name });
        let ServerEvent::Message(msg) = next(&mut events).await else { panic!("no directory") };
        assert_eq!(dir.apply(*msg), vec![Change::Listed(id)]);

        link.tx.send(&FromServer::Worker(worker(id, Liveness::Unreachable))).await.unwrap();
        let ServerEvent::Message(msg) = next(&mut events).await else { panic!("no change") };
        assert_eq!(
            dir.apply(*msg),
            vec![Change::Liveness {
                worker: id,
                was: Liveness::Online,
                now: Liveness::Unreachable
            }]
        );

        link.conn.close(0_u32.into(), b"restart");
        let ServerEvent::Unlinked { .. } = next(&mut events).await else { panic!("no drop") };

        let _again = welcome(&listener, vec![worker(id, Liveness::Online)]).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("no redial") };
        let ServerEvent::Message(msg) = next(&mut events).await else { panic!("no directory") };
        assert!(
            matches!(dir.apply(*msg).as_slice(), [Change::Liveness { now: Liveness::Online, .. }]),
            "back online after the relink"
        );

        drop(task);
        assert!(
            tokio::time::timeout(WAIT, events.recv()).await.unwrap().is_none(),
            "dropping the task ends the link"
        );
    }

    #[tokio::test]
    async fn a_server_that_is_not_there_is_reported() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let endpoint = slopty_net::client::bind_client().unwrap();
        let (_task, mut events) = spawn(
            &tokio::runtime::Handle::current(),
            endpoint,
            HostAddr::new("127.0.0.1", port),
            role(),
            None,
        );
        let ServerEvent::Unlinked { why } = next(&mut events).await else { panic!("linked?") };
        assert!(!why.is_empty());
    }
}
