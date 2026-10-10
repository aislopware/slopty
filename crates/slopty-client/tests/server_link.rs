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
                form: slopty_proto::server::Form::Desktop,
                cpus: 8,
                memory: 1,
                encoders: Vec::new(),
                displays: Vec::new(),
                agents: Vec::new(),
                can_capture: false,
                can_inject: false,
                virtual_displays: false,
                curtain: false,
                build: "0".to_owned(),
                lan: Vec::new(),
                wake_on_lan: None,
                writes_failing: None,
                stops_at_logout: None,
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
        link.tx
            .send(&FromServer::Welcome {
                name: "hub".to_owned(),
                link: 1,
                build: "0.1.0.commit.abc".to_owned(),
            })
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
        let ServerEvent::Linked { name, link: number, build } = next(&mut events).await else {
            panic!("not linked")
        };
        assert_eq!(number, 1, "the number the server gave the link");
        assert_eq!(build, "0.1.0.commit.abc", "the build the server said");
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

    /// The next two requests a client sent, by verb: the change's id and key, and the read's
    /// id and key.
    async fn two_requests(
        rx: &mut slopty_net::framed::FramedRecv<slopty_proto::server::ToServer>,
    ) -> [(u64, Option<slopty_proto::orchestration::IdempotencyKey>); 2] {
        use slopty_proto::orchestration::Verb;
        use slopty_proto::server::ToServer;
        let (mut change, mut read) = (None, None);
        while change.is_none() || read.is_none() {
            match tokio::time::timeout(WAIT, rx.recv()).await.unwrap().unwrap() {
                ToServer::Request { id, key, verb: Verb::TaskCreate { .. } } => {
                    change = Some((id, key));
                }
                ToServer::Request { id, key, verb: Verb::ListWorkers } => read = Some((id, key)),
                other => panic!("{other:?}"),
            }
        }
        [change.unwrap(), read.unwrap()]
    }

    /// A verb whose answer a drop lost goes again on the next link under the key it went
    /// under, so the server answers it as the first time, and its answer comes back; a read
    /// goes again too, with no key.
    #[tokio::test]
    async fn a_verb_whose_answer_a_drop_lost_goes_again_under_its_key() {
        use slopty_proto::orchestration::{Outcome, Verb};
        use slopty_proto::project::{ProjectId, TaskSpec};
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);
        let mut link = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("not linked") };
        let ServerEvent::Message(_) = next(&mut events).await else { panic!("no directory") };

        let caller = task.caller();
        let create = Verb::TaskCreate {
            project: ProjectId::new("demo").unwrap(),
            spec: Box::new(TaskSpec { title: "Once".to_owned(), ..TaskSpec::default() }),
        };
        let asked = tokio::spawn({
            let caller = caller.clone();
            async move { caller.call(create).await }
        });
        let listed = tokio::spawn(async move { caller.call(Verb::ListWorkers).await });
        let [(_, key), (_, read_key)] = two_requests(&mut link.rx).await;
        assert!(key.is_some(), "a change goes under a key");
        assert_eq!(read_key, None, "a read under none");
        link.conn.close(0_u32.into(), b"restart");
        let ServerEvent::Unlinked { .. } = next(&mut events).await else { panic!("no drop") };

        let mut again = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("no redial") };
        let [(id, key_again), (read_id, _)] = two_requests(&mut again.rx).await;
        assert_eq!(key_again, key, "the same key again");
        let reply = |id, outcome| FromServer::Reply { id, outcome };
        again.tx.send(&reply(id, Outcome::Done)).await.unwrap();
        again.tx.send(&reply(read_id, Outcome::Workers(Vec::new()))).await.unwrap();
        let answered = tokio::time::timeout(WAIT, asked).await.unwrap().unwrap();
        assert_eq!(answered, Outcome::Done, "the answer the second link brought");
        let read = tokio::time::timeout(WAIT, listed).await.unwrap().unwrap();
        assert_eq!(read, Outcome::Workers(Vec::new()));
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
            showing: Vec::new(),
            focus: None,
            listening: true,
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

    /// The phone this client is goes up when said and on each change, not again for the same,
    /// to a new link at once, and its withdrawal names the client.
    #[tokio::test]
    async fn the_phone_goes_on_each_change_and_to_each_new_link() {
        use slopty_core::ClientId;
        use slopty_proto::push::PushDevice;
        use slopty_proto::server::ToServer;
        let listener =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::default()).unwrap();
        let port = listener.local_addr().unwrap().port();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let addr = HostAddr::new("127.0.0.1", port);
        let (task, mut events) =
            spawn(&tokio::runtime::Handle::current(), endpoint, addr, role(), None);
        let caller = task.caller();
        let me = ClientId::new();
        let device = PushDevice {
            token: "0f".repeat(32),
            key: [7; 32],
            sandbox: true,
            topic: "dev.aislopware.slopty".to_owned(),
            quiet_ms: 30_000,
        };
        let heard =
            async |rx: &mut slopty_net::framed::FramedRecv<ToServer>| match tokio::time::timeout(
                WAIT,
                rx.recv(),
            )
            .await
            .unwrap()
            .unwrap()
            {
                ToServer::PushDevice { client, device } => (client, device),
                other => panic!("{other:?}"),
            };

        let mut link = welcome(&listener, Vec::new()).await;
        let ServerEvent::Linked { .. } = next(&mut events).await else { panic!("not linked") };
        caller.push_device(me, Some(device.clone()));
        assert_eq!(heard(&mut link.rx).await, (me, Some(device.clone())));
        caller.push_device(me, Some(device.clone()));
        caller.push_device(me, None);
        assert_eq!(heard(&mut link.rx).await, (me, None), "the same again sent nothing");

        caller.push_device(me, Some(device.clone()));
        assert_eq!(heard(&mut link.rx).await, (me, Some(device.clone())));
        link.conn.close(0_u32.into(), b"restart");
        while !matches!(next(&mut events).await, ServerEvent::Unlinked { .. }) {}
        let mut again = welcome(&listener, Vec::new()).await;
        assert_eq!(heard(&mut again.rx).await, (me, Some(device)), "a new link is told at once");
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
