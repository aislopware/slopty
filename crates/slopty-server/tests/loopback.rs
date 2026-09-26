//! A server, a worker and a client in one process over real UDP: the worker registers and
//! answers a forwarded verb, the client lists and asks, and the worker's drop reaches the client
//! as a directory change and as the answer to what it was waiting on.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_core::{SessionId, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::{DialError, ServerLink, connect};
    use slopty_proto::orchestration::{ErrorCode, Line, Outcome, Screen, TermRef, Verb};
    use slopty_proto::server::{
        FromServer, Liveness, Os, Refusal, Registration, Role, ToServer, WorkerCaps,
    };
    use slopty_server::{Config, Server};

    const PATIENCE: Duration = Duration::from_secs(10);

    fn caps() -> WorkerCaps {
        WorkerCaps {
            os: Os::MacOs,
            os_version: "26.5".to_owned(),
            arch: "aarch64".to_owned(),
            cpus: 8,
            memory: 16 << 30,
            encoders: Vec::new(),
            displays: Vec::new(),
            agents: Vec::new(),
            can_capture: false,
            can_inject: false,
            load: 0.1,
            version: "0".to_owned(),
        }
    }

    fn worker_role(worker: WorkerId) -> Role {
        Role::Worker(Box::new(Registration {
            worker,
            name: "fake-worker".to_owned(),
            port: 45999,
            caps: caps(),
            sessions: Vec::new(),
        }))
    }

    fn client_role() -> Role {
        Role::Client { name: "test".to_owned() }
    }

    fn screen() -> Screen {
        Screen {
            lines: vec![Line { index: 0, text: "$ ready".to_owned() }],
            cursor: (0, 7),
            title: "zsh".to_owned(),
            cwd: Some("/tmp".to_owned()),
            alternate: false,
        }
    }

    pub async fn start(dir: &std::path::Path) -> Server {
        Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.to_path_buf(),
            admission: Admission::default(),
        })
        .await
        .unwrap()
    }

    async fn dial(server: &Server, role: Role) -> Result<ServerLink, DialError> {
        let endpoint = bind_client().unwrap();
        let addr = HostAddr::from(server.quic_addr());
        tokio::time::timeout(PATIENCE, connect(&endpoint, &addr, role)).await.unwrap()
    }

    async fn next(link: &mut ServerLink) -> FromServer {
        tokio::time::timeout(PATIENCE, link.rx.recv()).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn a_verb_goes_to_the_worker_and_its_drop_reaches_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let server = start(dir.path()).await;
        let id = WorkerId::new();
        let mut worker = dial(&server, worker_role(id)).await.unwrap();
        assert_eq!(worker.name, "test-server");

        // A second live link with the same id is refused.
        let twin = dial(&server, worker_role(id)).await.unwrap_err();
        assert!(matches!(twin, DialError::Refused(Refusal::DuplicateWorker)), "{twin:?}");

        let mut client = dial(&server, client_role()).await.unwrap();
        let FromServer::Directory(directory) = next(&mut client).await else {
            panic!("the directory comes first")
        };
        assert_eq!(directory.len(), 1);
        assert_eq!(directory[0].worker, id);
        assert_eq!(directory[0].liveness, Liveness::Online);
        assert_eq!(directory[0].address, "127.0.0.1:45999", "the seen IP, the registered port");

        client.tx.send(&ToServer::Request { id: 1, verb: Verb::ListWorkers }).await.unwrap();
        let FromServer::Reply { id: 1, outcome: Outcome::Workers(listed) } =
            next(&mut client).await
        else {
            panic!("list_workers")
        };
        assert_eq!(listed, directory);

        // Forwarded: the worker answers under the server's id; the client hears its own.
        let term = TermRef { worker: id, session: SessionId::new() };
        client
            .tx
            .send(&ToServer::Request { id: 2, verb: Verb::ReadScreen { term } })
            .await
            .unwrap();
        let FromServer::Request { id: forwarded, verb } = next(&mut worker).await else {
            panic!("the worker gets the verb")
        };
        assert_eq!(verb, Verb::ReadScreen { term });
        worker
            .tx
            .send(&ToServer::Reply { id: forwarded, outcome: Outcome::Screen(screen()) })
            .await
            .unwrap();
        let reply = next(&mut client).await;
        assert_eq!(reply, FromServer::Reply { id: 2, outcome: Outcome::Screen(screen()) });

        // The worker drops with a request pending on it.
        client
            .tx
            .send(&ToServer::Request { id: 3, verb: Verb::ReadScreen { term } })
            .await
            .unwrap();
        let FromServer::Request { .. } = next(&mut worker).await else {
            panic!("the worker gets it")
        };
        worker.close();
        let (mut unreachable, mut failed) = (false, false);
        while !(unreachable && failed) {
            match next(&mut client).await {
                FromServer::Worker(info) if info.worker == id => {
                    assert_eq!(info.liveness, Liveness::Unreachable);
                    unreachable = true;
                }
                FromServer::Reply { id: 3, outcome } => {
                    assert!(
                        matches!(
                            outcome,
                            Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }
                        ),
                        "{outcome:?}"
                    );
                    failed = true;
                }
                other => panic!("unexpected {other:?}"),
            }
        }

        // Asked now, the worker is known but not there.
        client
            .tx
            .send(&ToServer::Request { id: 4, verb: Verb::ReadScreen { term } })
            .await
            .unwrap();
        let FromServer::Reply { id: 4, outcome } = next(&mut client).await else { panic!("reply") };
        assert!(matches!(outcome, Outcome::Error { code: ErrorCode::WorkerUnreachable, .. }));

        // It comes back under the same id.
        let _back = dial(&server, worker_role(id)).await.unwrap();
        let FromServer::Worker(info) = next(&mut client).await else { panic!("back online") };
        assert_eq!(info.liveness, Liveness::Online);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn a_worker_that_goes_silent_turns_unreachable_after_the_idle_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let server = start(dir.path()).await;
        let id = WorkerId::new();
        // The worker's end over a relay that can be cut, so it vanishes without closing.
        let relay = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let upstream = server.quic_addr();
        let cut = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        tokio::spawn({
            let cut = std::sync::Arc::clone(&cut);
            async move {
                let out = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let mut peer = None;
                let (mut a, mut b) = (vec![0_u8; 2048], vec![0_u8; 2048]);
                loop {
                    tokio::select! {
                        Ok((n, from)) = relay.recv_from(&mut a) => {
                            peer = Some(from);
                            if !cut.load(std::sync::atomic::Ordering::Relaxed) {
                                let _sent = out.send_to(&a[..n], upstream).await;
                            }
                        }
                        Ok((n, _)) = out.recv_from(&mut b) => {
                            if let (Some(p), false) = (peer, cut.load(std::sync::atomic::Ordering::Relaxed)) {
                                let _sent = relay.send_to(&b[..n], p).await;
                            }
                        }
                    }
                }
            }
        });
        let endpoint = bind_client().unwrap();
        let _worker =
            connect(&endpoint, &HostAddr::from(relay_addr), worker_role(id)).await.unwrap();
        let mut client = dial(&server, client_role()).await.unwrap();
        let _directory = next(&mut client).await;

        cut.store(true, std::sync::atomic::Ordering::Relaxed);
        let started = std::time::Instant::now();
        let FromServer::Worker(info) = next(&mut client).await else { panic!("a change") };
        let took = started.elapsed();
        assert_eq!(info.liveness, Liveness::Unreachable);
        let lease = slopty_net::endpoint::LEASE_IDLE_TIMEOUT;
        assert!(took >= lease.checked_sub(Duration::from_secs(1)).unwrap(), "{took:?}");
        assert!(took < lease.checked_add(Duration::from_secs(2)).unwrap(), "{took:?}");
        server.shutdown().await;
    }
}
