//! The app's link to the server failing and coming back: a real server in this process, and the
//! client core's own server link (`slopty_client::server`) dialling it over QUIC on loopback
//! through a relay the test cuts (`slopty_testkit::relay`), so packets stop and nothing says so,
//! as on a train. What the person sees while the server is out of reach, what waits and what is
//! refused, and how soon the board is current again once the network is back.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use slopty_client::server::{ServerEvent, spawn};
    use slopty_net::HostAddr;
    use slopty_proto::orchestration::{ErrorCode, Happening, HubEvent, Outcome, Verb};
    use slopty_proto::project::{
        Autonomy, LimitsChange, ProjectId, TaskChange, TaskId, TaskSpec, TaskState,
    };
    use slopty_proto::server::{FromServer, Role};
    use slopty_server::{Config, Server};
    use slopty_testkit::relay::Relay;
    use tokio::sync::mpsc;

    /// Long enough for any step on a loaded runner; the test judges what arrives, and the
    /// times it prints are the measurement.
    const STEP: Duration = Duration::from_secs(60);
    /// How long the app is off the network.
    const AWAY: Duration = Duration::from_secs(30);

    async fn server(dir: &std::path::Path) -> Server {
        Server::start(Config {
            name: "links-test".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.to_path_buf(),
            admission: slopty_net::admission::Admission::default(),
            push: slopty_server::PushConfig::Off,
        })
        .await
        .unwrap()
    }

    /// The board as the app keeps it: each task's state, from the last snapshot and every
    /// change since, and whether the link is up.
    #[derive(Debug, Default)]
    struct Board {
        linked: bool,
        tasks: HashMap<(ProjectId, TaskId), TaskState>,
    }

    impl Board {
        fn apply(&mut self, event: ServerEvent) {
            match event {
                ServerEvent::Linked { .. } => self.linked = true,
                ServerEvent::Unlinked { .. } => self.linked = false,
                ServerEvent::Message(msg) => match *msg {
                    FromServer::Projects(part) => {
                        if part.first {
                            self.tasks.clear();
                        }
                        for status in part.projects {
                            let id = status.project.id.clone();
                            for card in status.tasks {
                                self.tasks.insert((id.clone(), card.id), card.state);
                            }
                        }
                    }
                    FromServer::Event(HubEvent { what: Happening::Project(update), .. }) => {
                        if let Some(card) = update.task {
                            self.tasks.insert((update.project.clone(), card.id), card.state);
                        }
                    }
                    _ => {}
                },
                ServerEvent::WrongBuild(_) | ServerEvent::Refused(_) => {}
            }
        }

        /// Take events until `done` holds; when it did.
        async fn until(
            &mut self,
            events: &mut mpsc::Receiver<ServerEvent>,
            what: &str,
            done: impl Fn(&Self) -> bool,
        ) -> Instant {
            let waited = tokio::time::timeout(STEP, async {
                while !done(self) {
                    let event = events.recv().await.unwrap_or_else(|| panic!("the link ended"));
                    self.apply(event);
                }
            })
            .await;
            waited.unwrap_or_else(|_| panic!("never {what} in {STEP:?}"));
            Instant::now()
        }
    }

    fn task(title: &str) -> Verb {
        let spec = TaskSpec { title: title.to_owned(), ..TaskSpec::default() };
        Verb::TaskCreate { project: ProjectId::new("trip").unwrap(), spec: Box::new(spec) }
    }

    /// The app off the network for 30 s while the agents go on. The server's link is said lost
    /// within its lease (5 s), and the board keeps showing what it last knew. A verb made as the
    /// network went goes again under its key on the next link, and is done once; one asked once
    /// the link is known lost is refused at once, rather than left waiting. Back on the network,
    /// the link comes back by itself and the board is current with the agents' work at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_app_off_the_network_keeps_its_board_and_catches_up_once_back() {
        let dir = tempfile::tempdir().unwrap();
        let server = server(dir.path()).await;
        let hub = server.hub().clone();
        let project = ProjectId::new("trip").unwrap();
        let made = hub
            .dispatch(Verb::ProjectCreate {
                project: project.clone(),
                title: "Trip".to_owned(),
                goal: None,
                autonomy: Autonomy::Ask,
                repo: dir.path().to_string_lossy().into_owned(),
                target: "main".to_owned(),
                verifier: None,
                push: false,
                orchestrator: None,
                limits: LimitsChange::default(),
                metadata: None,
            })
            .await;
        assert!(matches!(made, Outcome::Project(_)), "{made:?}");
        let Outcome::Task(working) = hub.dispatch(task("Work on")).await else { panic!("a task") };

        let relay = Relay::start(server.quic_addr()).await.unwrap();
        let endpoint = slopty_net::client::bind_client().unwrap();
        let role = Role::Client { name: "phone".to_owned() };
        let addr = HostAddr::from(relay.addr());
        let runtime = tokio::runtime::Handle::current();
        let (link, mut events) = spawn(&runtime, endpoint, addr, role, None);
        let caller = link.caller();
        let mut board = Board::default();
        let key = (project.clone(), working.id);
        board.until(&mut events, "the board", |b| b.linked && b.tasks.contains_key(&key)).await;

        // Off the network. A verb made just then is on its way when the link goes.
        relay.cut();
        let cut = Instant::now();
        let in_flight = tokio::spawn({
            let caller = caller.clone();
            async move { caller.call(task("Made as the network went")).await }
        });
        let said = board.until(&mut events, "the link said lost", |b| !b.linked).await;
        let lost_after = said.duration_since(cut);

        // Known lost: a verb is refused at once, saying so.
        let asked = Instant::now();
        let refused = caller.call(task("Made while away")).await;
        let refused_in = asked.elapsed();
        assert!(
            matches!(&refused, Outcome::Error { code: ErrorCode::ServerUnreachable, .. }),
            "{refused:?}"
        );

        // The agents go on meanwhile: the work they report reaches the server.
        let change = TaskChange { state: Some(TaskState::Done), ..TaskChange::default() };
        let verb =
            Verb::TaskUpdate { project: project.clone(), task: working.id, change: change.into() };
        assert!(matches!(hub.dispatch(verb).await, Outcome::Task(_)));
        assert_eq!(
            board.tasks.get(&key),
            Some(&TaskState::Planned),
            "the board shows what it knew"
        );

        let back = cut.checked_add(AWAY).unwrap();
        tokio::time::sleep_until(back.into()).await;
        relay.restore();
        let restored = Instant::now();
        let linked = board.until(&mut events, "linked again", |b| b.linked).await;
        let current = board
            .until(&mut events, "the board current", |b| {
                b.tasks.get(&key) == Some(&TaskState::Done)
            })
            .await;

        // The verb that was on its way is done, once.
        let outcome = tokio::time::timeout(STEP, in_flight).await.unwrap().unwrap();
        assert!(matches!(outcome, Outcome::Task(_)), "{outcome:?}");
        let verb = Verb::ProjectStatus { project: project.clone(), since: None, timeout_ms: 0 };
        let Outcome::Project(status) = hub.dispatch(verb).await else { panic!("the project") };
        let made = status.tasks.iter().filter(|t| t.title == "Made as the network went").count();
        assert_eq!(made, 1, "done once: {:?}", status.tasks);
        assert!(status.tasks.iter().all(|t| t.title != "Made while away"));

        println!(
            "off the network {AWAY:?}: said lost after {lost_after:?}; refused in {refused_in:?}; \
             back: linked after {:?}, board current after {:?}; {} datagrams dropped",
            linked.duration_since(restored),
            current.duration_since(restored),
            relay.dropped()
        );
        assert!(lost_after < Duration::from_secs(10), "{lost_after:?}");
        assert!(refused_in < Duration::from_secs(1), "{refused_in:?}");
        drop(link);
        server.shutdown().await;
    }
}
