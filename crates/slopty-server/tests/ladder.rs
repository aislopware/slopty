//! The attention ladder over real links: a worker publishes its thread table, the server ranks
//! it and every client hears the ladder; a person's client says where they are, and a notice
//! goes only where it is wanted. One process, real UDP.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::{ServerLink, connect};
    use slopty_proto::orchestration::TermRef;
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer, WorkerCaps};
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_proto::thread::attention::{
        Ladder, Notice, NoticeKind, Presence, Present, Rung, Seat, ThreadAt,
    };
    use slopty_proto::thread::wire::{RequestCard, TableFrame, ThreadRow};
    use slopty_proto::thread::{
        AgentId, AskId, Changed, Cursor, Drive, Liveness, Meters, Phase, Status, ThreadId,
    };
    use slopty_server::{Config, Server};

    const PATIENCE: Duration = Duration::from_secs(10);

    fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: "claude".to_owned(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            agent: None,
            progress: None,
            restored: None,
            repo_id: None,
        }
    }

    fn worker_role(worker: WorkerId, sessions: Vec<SessionSummary>) -> Role {
        Role::Worker(Box::new(Registration {
            worker,
            name: "fake-worker".to_owned(),
            listen: std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45999)),
            caps: WorkerCaps::bare(Os::MacOs),
            sessions,
            session_key: [7; 32],
        }))
    }

    async fn dial(server: &Server, role: Role) -> ServerLink {
        let endpoint = bind_client().unwrap();
        let addr = HostAddr::from(server.quic_addr());
        tokio::time::timeout(PATIENCE, connect(&endpoint, &addr, role)).await.unwrap().unwrap()
    }

    fn row(id: ThreadId, phase: Phase, since: u64, terminal: SessionId) -> ThreadRow {
        ThreadRow {
            id,
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: "Rank the fleet".to_owned(),
            status: Status {
                phase,
                wait: None,
                liveness: Liveness::Live,
                since_ms: WallMs::from_millis(since),
            },
            requests: Vec::new(),
            last_line: None,
            doing: None,
            changed: Changed::default(),
            terminal: Some(terminal),
            parent: None,
            drive: Drive::named(Drive::OBSERVED),
            caps: Vec::new(),
            facts: BTreeMap::new(),
            to_review: false,
            meters: Meters::default(),
            updated_ms: WallMs::from_millis(since),
            cwd: None,
            repo: None,
            repo_id: None,
        }
    }

    /// A link's side of the server, with every notice it was sent kept.
    struct Listener {
        link: ServerLink,
        notices: Vec<Notice>,
    }

    impl Listener {
        async fn dial(server: &Server, role: Role) -> Self {
            Self { link: dial(server, role).await, notices: Vec::new() }
        }

        async fn send(&mut self, msg: ToServer) {
            self.link.tx.send(&msg).await.unwrap();
        }

        /// The first message `pick` takes; what comes before it is passed over, but notices,
        /// which are kept.
        async fn heard<T>(&mut self, mut pick: impl FnMut(&FromServer) -> Option<T>) -> T {
            loop {
                let msg =
                    tokio::time::timeout(PATIENCE, self.link.rx.recv()).await.unwrap().unwrap();
                if let Some(found) = pick(&msg) {
                    return found;
                }
                if let FromServer::Notice(notice) = msg {
                    self.notices.push(*notice);
                }
            }
        }

        async fn ladder(&mut self, done: impl Fn(&Ladder) -> bool) -> Ladder {
            self.heard(|msg| match msg {
                FromServer::Ladder(ladder) if done(ladder) => Some((**ladder).clone()),
                _ => None,
            })
            .await
        }

        async fn notice(&mut self) -> Notice {
            if !self.notices.is_empty() {
                return self.notices.remove(0);
            }
            self.heard(|msg| match msg {
                FromServer::Notice(notice) => Some((**notice).clone()),
                _ => None,
            })
            .await
        }

        async fn present(&mut self, done: impl Fn(&[Present]) -> bool) -> Vec<Present> {
            self.heard(|msg| match msg {
                FromServer::Present(list) if done(list) => Some(list.clone()),
                _ => None,
            })
            .await
        }
    }

    /// A worker's rows reach every client as the fleet's ladder, and a client that connects
    /// later has it in its first state. A person at their desk with the thread's tile on screen
    /// is not told it needs them; once the tile is off screen they are, and the agent's link
    /// (no person's) never is.
    #[tokio::test]
    async fn rows_become_the_ladder_and_a_notice_goes_where_the_person_is() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            mcp: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            admission: Admission::with_tailnet(Vec::new(), None),
        })
        .await
        .unwrap();
        let (worker, shell) = (WorkerId::new(), SessionId::new());
        let mut desk = Listener::dial(&server, Role::Client { name: "mac".to_owned() }).await;
        let agent_role = Role::Agent { name: "mcp".to_owned(), vouch: None };
        let mut agent = Listener::dial(&server, agent_role).await;
        let mut link = dial(&server, worker_role(worker, vec![summary(shell)])).await;
        let tile = TermRef { worker, session: shell };
        let presence = |showing: Vec<TermRef>| Presence {
            seat: Seat::Desk,
            active: true,
            workspace: Some("slopty".to_owned()),
            showing,
            focus: None,
        };
        desk.send(ToServer::Presence(presence(vec![tile]))).await;
        let present = desk.present(|list| !list.is_empty()).await;
        assert_eq!(present.len(), 1);
        assert_eq!(present[0].link, desk.link.link, "listed under the number its Welcome gave");
        assert_eq!(
            (present[0].name.as_str(), &present[0].presence),
            ("mac", &presence(vec![tile]))
        );

        let thread = ThreadId::new();
        let at = ThreadAt { worker, thread };
        let rows = vec![row(thread, Phase::Working, 1_000, shell)];
        let snapshot = TableFrame::Snapshot { cursor: Cursor::default(), rows };
        link.tx.send(&ToServer::Threads(snapshot)).await.unwrap();
        let working = desk.ladder(|l| l.rung(at) == Some(Rung::Working)).await;
        assert_eq!(working.tile(tile).map(|(_, s)| s.rung), Some(Rung::Working));
        assert_eq!(working.fleet.counts.working, 1);

        let mut asking = row(thread, Phase::NeedsYou, 2_000, shell);
        asking.requests.push(RequestCard {
            id: AskId("1".to_owned()),
            item: None,
            kind: "permission".to_owned(),
            title: "Run cargo test?".to_owned(),
            options: Vec::new(),
            opened_ms: WallMs::from_millis(2_000),
        });
        let delta = |rows| TableFrame::Delta { cursor: Cursor::default(), rows, removed: vec![] };
        link.tx.send(&ToServer::Threads(delta(vec![asking.clone()]))).await.unwrap();
        desk.ladder(|l| l.rung(at) == Some(Rung::NeedsYou)).await;

        // Off screen now: the next time it needs the person, the desk is told.
        desk.send(ToServer::Presence(presence(Vec::new()))).await;
        desk.present(|list| list.iter().all(|p| p.presence.showing.is_empty())).await;
        let working = row(thread, Phase::Working, 3_000, shell);
        link.tx.send(&ToServer::Threads(delta(vec![working]))).await.unwrap();
        desk.ladder(|l| l.rung(at) == Some(Rung::Working)).await;
        assert!(desk.notices.is_empty(), "none while its tile was on screen: {:?}", desk.notices);
        link.tx.send(&ToServer::Threads(delta(vec![asking]))).await.unwrap();
        let expected = Notice {
            kind: NoticeKind::NeedsYou,
            thread: at,
            tile: Some(shell),
            title: "Rank the fleet".to_owned(),
            text: "Run cargo test?".to_owned(),
            worked_ms: None,
            via: None,
        };
        assert_eq!(desk.notice().await, expected);

        let mut late = Listener::dial(&server, Role::Client { name: "phone".to_owned() }).await;
        let first = late.ladder(|_| true).await;
        assert_eq!(first.rung(at), Some(Rung::NeedsYou), "a late client's state has the ladder");
        agent.ladder(|l| l.rung(at) == Some(Rung::NeedsYou)).await;
        assert!(agent.notices.is_empty(), "an agent's link gets no notice: {:?}", agent.notices);
        assert!(late.notices.is_empty(), "nor a client that came after");
        server.shutdown().await;
    }
}
