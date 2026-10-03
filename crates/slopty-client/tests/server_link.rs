//! The client's server link against a real server listener in-process, over UDP on loopback:
//! the directory arrives, a drop is reported, and the link comes back by itself; a refusal is
//! reported by name and the link keeps dialling.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_client::directory::{Change, Directory, ServerState};
    use slopty_client::server::{ServerEvent, spawn};
    use slopty_core::WorkerId;
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::server::{FromServer, Liveness, Os, Refusal, Role, WorkerCaps, WorkerInfo};
    use tokio::sync::mpsc;

    const WAIT: Duration = Duration::from_secs(20);

    fn role() -> Role {
        Role::Client { name: "test".to_owned() }
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
                virtual_displays: false,
                version: "0".to_owned(),
                lan: Vec::new(),
                wake_on_lan: None,
                writes_failing: None,
            },
            load: 0.0,
            last_seen_ms: slopty_core::WallMs::ZERO,
        }
    }

    async fn next(rx: &mut mpsc::Receiver<ServerEvent>) -> ServerEvent {
        tokio::time::timeout(WAIT, rx.recv()).await.unwrap().unwrap()
    }

    async fn welcome(listener: &ServerListener, directory: Vec<WorkerInfo>) -> AcceptedLink {
        let mut link = tokio::time::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
        assert!(matches!(link.role, Role::Client { .. }), "{:?}", link.role);
        link.tx.send(&FromServer::Welcome { name: "hub".to_owned(), link: 1 }).await.unwrap();
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
        let ServerEvent::Linked { name, link: number } = next(&mut events).await else {
            panic!("not linked")
        };
        assert_eq!(number, 1, "the number the server gave the link");
        assert_eq!(name, "hub");
        dir.set_server(ServerState::Linked { name, link: number });
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

    /// The port stays bound to a socket that never answers: freed, another test's server
    /// running alongside could take it and let the client in.
    #[tokio::test]
    async fn a_server_that_is_not_there_is_reported() {
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let (_task, mut events) = spawn(
            &tokio::runtime::Handle::current(),
            endpoint,
            HostAddr::new("127.0.0.1", port),
            role(),
            None,
        );
        let ServerEvent::Unlinked { why } = next(&mut events).await else { panic!("linked?") };
        assert_ne!(why, "");
    }

    /// A server that turns the client away (the tailnet policy grants it no client role) is
    /// reported as that refusal, not as unreachable, and dialled again: a policy change can let
    /// it in.
    #[tokio::test]
    async fn a_refusal_is_reported_by_name_and_the_link_keeps_dialling() {
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (_task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);

        for _ in 0..2 {
            let link = tokio::time::timeout(WAIT, listener.accept()).await.unwrap().unwrap();
            tokio::spawn(link.refuse(Refusal::NotGranted));
            let event = next(&mut events).await;
            assert!(matches!(event, ServerEvent::Refused(Refusal::NotGranted)), "{event:?}");
        }
        assert_eq!(Refusal::NotGranted.text(), "Not granted by the tailnet policy");

        let _granted = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("not let in") };
    }

    /// The next message a client sent, which must say where the person is.
    async fn said(
        rx: &mut slopty_net::framed::FramedRecv<slopty_proto::server::ToServer>,
    ) -> slopty_proto::thread::attention::Presence {
        match tokio::time::timeout(WAIT, rx.recv()).await.unwrap().unwrap() {
            slopty_proto::server::ToServer::Presence(p) => p,
            other => panic!("{other:?}"),
        }
    }

    /// Where the person is goes up on each change, not again for the same, and to a new link
    /// at once.
    #[tokio::test]
    async fn presence_goes_on_each_change_and_to_each_new_link() {
        use slopty_proto::thread::attention::{Presence, Seat};
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);
        let caller = task.caller();
        let at = |active| Presence {
            seat: Seat::Desk,
            active,
            workspace: None,
            showing: Vec::new(),
            focus: None,
        };

        let mut link = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("not linked") };
        caller.presence(at(true));
        assert_eq!(said(&mut link.rx).await, at(true));
        caller.presence(at(true));
        caller.presence(at(false));
        assert_eq!(said(&mut link.rx).await, at(false), "the same again sent nothing");

        link.conn.close(0_u32.into(), b"restart");
        while !matches!(next(&mut events).await, ServerEvent::Unlinked { .. }) {}
        let mut again = welcome(&listener, Vec::new()).await;
        assert_eq!(said(&mut again.rx).await, at(false), "a new link is told at once");
        assert_eq!(caller.presence_said(), Some(at(false)));
    }

    /// A UDP relay in front of the server at `port` that can go deaf both ways, as a path that
    /// died while the device slept does: nothing is closed, nothing more arrives. Returns its
    /// port and the switch.
    async fn relay(port: u16) -> (u16, std::sync::Arc<std::sync::atomic::AtomicBool>) {
        use std::sync::atomic::Ordering;
        let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let front = tokio::net::UdpSocket::bind(loopback).await.unwrap();
        let behind = tokio::net::UdpSocket::bind(loopback).await.unwrap();
        behind.connect(("127.0.0.1", port)).await.unwrap();
        let at = front.local_addr().unwrap().port();
        let deaf = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let muted = std::sync::Arc::clone(&deaf);
        tokio::spawn(async move {
            let mut client = None;
            let (mut up, mut down) = ([0_u8; 2048], [0_u8; 2048]);
            loop {
                tokio::select! {
                    Ok((n, from)) = front.recv_from(&mut up) => {
                        client = Some(from);
                        if !muted.load(Ordering::Relaxed) {
                            let _sent = behind.send(&up[..n]).await;
                        }
                    }
                    Ok(n) = behind.recv(&mut down) => {
                        if let Some(client) = client
                            && !muted.load(Ordering::Relaxed)
                        {
                            let _sent = front.send_to(&down[..n], client).await;
                        }
                    }
                }
            }
        });
        (at, deaf)
    }

    /// A resume probes the server link: one the server still answers is kept, one whose path
    /// died is given up within the probe's deadline (not the transport's 45 s idle timeout),
    /// and the link comes back once the path does.
    #[tokio::test]
    async fn a_resume_keeps_a_live_link_and_gives_up_a_dead_one_at_once() {
        use std::sync::atomic::Ordering;
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let (port, deaf) = relay(listener.local_addr().unwrap().port()).await;
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);
        let _link = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("not linked") };
        let ServerEvent::Message(_) = next(&mut events).await else { panic!("no directory") };

        task.resume();
        let quiet = slopty_client::server::PROBE_DEADLINE.saturating_mul(2);
        let heard = tokio::time::timeout(quiet, events.recv()).await;
        assert!(heard.is_err(), "a live link is kept: {heard:?}");

        deaf.store(true, Ordering::Relaxed);
        let asked = tokio::time::Instant::now();
        task.resume();
        let ServerEvent::Unlinked { why } = next(&mut events).await else { panic!("kept") };
        let gave_up = asked.elapsed();
        assert_eq!(why, "the server did not answer after a resume");
        assert!(
            gave_up < slopty_client::server::PROBE_DEADLINE + Duration::from_millis(500),
            "{gave_up:?}"
        );

        deaf.store(false, Ordering::Relaxed);
        let _again = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("no redial") };
    }
}
