//! The app's link to the server failing and coming back: a real server in this process, and the
//! client core's own server link (`slopty_client::server`) dialling it over QUIC, both on
//! `slopty_shape::sim`'s network on tokio's paused clock, which a blackout cuts so packets stop
//! and nothing says so, as on a train. What the person sees while the server is out of reach,
//! what waits and what is refused, and how soon the board is current again once the network is
//! back. Half a minute away passes in milliseconds, and each time is exact.

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use slopty_client::server::{ServerEvent, spawn};
    use slopty_core::WorkerId;
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::server::ServerListener;
    use slopty_proto::orchestration::{ErrorCode, Outcome, Verb};
    use slopty_proto::server::{FromServer, Os, Registration, Role, WorkerCaps};
    use slopty_server::{Config, Server};
    use slopty_shape::sim::{Faults, Net};
    use tokio::sync::mpsc;
    use tokio::time::Instant;

    /// A bound on each step, on the simulated clock: the test judges what arrives.
    const STEP: Duration = Duration::from_secs(60);
    /// How long the app is off the network.
    const AWAY: Duration = Duration::from_secs(30);
    /// The run's draws: the network's, the server's and the client's.
    const SEED: u64 = 0x5106_7e57;
    /// Where the server answers on the simulated network.
    const SERVER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 45560);
    /// Where the app dials from.
    const PHONE: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)), 50000);

    /// The server on `net`, letting in loopback alone: no tailnet is asked.
    async fn server(net: &Net, dir: &std::path::Path) -> Server {
        let socket = Box::new(net.bind(SERVER).unwrap());
        let endpoint = slopty_net::endpoint::bind_lease_on(socket, Some(SEED)).unwrap();
        let admission = Admission::with_tailnet(Vec::new(), None);
        let config = Config {
            name: "links-test".to_owned(),
            quic: SERVER,
            data_dir: dir.to_path_buf(),
            admission: admission.clone(),
            push: slopty_server::PushConfig::Off,
        };
        Server::start_on(config, ServerListener::on(endpoint, admission)).await.unwrap()
    }

    /// A machine that once registered and is off now: what the person forgets.
    fn machine(server: &Server, name: &str) -> WorkerId {
        let worker = WorkerId::new();
        let caps = WorkerCaps {
            os: Os::Linux,
            os_version: "6.15".to_owned(),
            arch: "x86_64".to_owned(),
            form: slopty_proto::server::Form::Desktop,
            cpus: 8,
            memory: 16 << 30,
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
        };
        let registration = Registration {
            worker,
            name: name.to_owned(),
            listen: SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45550)),
            caps,
            sessions: Vec::new(),
            session_key: [7; 32],
        };
        let (tx, _rx) = mpsc::channel(8);
        let lease = server.hub().register(registration, [127, 0, 0, 1].into(), tx).unwrap();
        drop(lease);
        worker
    }

    /// The board as the app keeps it: the machines it knows, from the last directory and every
    /// change since, and whether the link is up.
    #[derive(Debug, Default)]
    struct Board {
        linked: bool,
        machines: HashSet<WorkerId>,
    }

    impl Board {
        fn apply(&mut self, event: ServerEvent) {
            match event {
                ServerEvent::Linked { .. } => self.linked = true,
                ServerEvent::Unlinked { .. } => self.linked = false,
                ServerEvent::Message(msg) => match *msg {
                    FromServer::Directory(all) => {
                        self.machines = all.iter().map(|w| w.worker).collect();
                    }
                    FromServer::Worker(info) => {
                        self.machines.insert(info.worker);
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

    /// The app off the network for 30 s while the fleet changes. The server's link is said lost
    /// within its lease (5 s), and the board keeps showing what it last knew. A verb made as the
    /// network went goes again under its key on the next link, and is done once; one asked once
    /// the link is known lost is refused at once, rather than left waiting. Back on the network,
    /// the link comes back by itself and the board is current at once.
    #[tokio::test(start_paused = true)]
    async fn the_app_off_the_network_keeps_its_board_and_catches_up_once_back() {
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new(Faults::CLEAR, SEED);
        let server = server(&net, dir.path()).await;
        let hub = server.hub().clone();
        let [went, away, elsewhere] = ["went", "away", "elsewhere"].map(|n| machine(&server, n));

        let socket = Box::new(net.bind(PHONE).unwrap());
        let endpoint =
            slopty_net::endpoint::bind_on(socket, false, Some(SEED.rotate_left(32))).unwrap();
        let role = Role::Client { name: "phone".to_owned() };
        let addr = HostAddr::from(server.quic_addr());
        let runtime = tokio::runtime::Handle::current();
        let (link, mut events) = spawn(&runtime, endpoint, addr, role, None);
        let caller = link.caller();
        let mut board = Board::default();
        board.until(&mut events, "the board", |b| b.linked && b.machines.len() == 3).await;

        // Off the network. A verb made just then is on its way when the link goes.
        net.blackout(AWAY);
        let cut = Instant::now();
        let in_flight = tokio::spawn({
            let caller = caller.clone();
            async move { caller.call(Verb::ForgetWorker { worker: went }).await }
        });
        let said = board.until(&mut events, "the link said lost", |b| !b.linked).await;
        let lost_after = said.duration_since(cut);

        // Known lost: a verb is refused at once, saying so.
        let asked = Instant::now();
        let refused = caller.call(Verb::ForgetWorker { worker: away }).await;
        let refused_in = asked.elapsed();
        assert!(
            matches!(&refused, Outcome::Error { code: ErrorCode::ServerUnreachable, .. }),
            "{refused:?}"
        );

        // The fleet changes meanwhile, from another device.
        assert_eq!(hub.dispatch(Verb::ForgetWorker { worker: elsewhere }).await, Outcome::Done);
        assert!(board.machines.contains(&elsewhere), "the board shows what it knew");

        let back = cut.checked_add(AWAY).unwrap();
        tokio::time::sleep_until(back).await;
        let restored = Instant::now();
        let linked = board.until(&mut events, "linked again", |b| b.linked).await;
        let current = board
            .until(&mut events, "the board current", |b| b.machines == HashSet::from([away]))
            .await;

        // The verb that was on its way is done, once: its repeat answers as the first did.
        let outcome = tokio::time::timeout(STEP, in_flight).await.unwrap().unwrap();
        assert_eq!(outcome, Outcome::Done);
        let Outcome::Workers(left) = hub.dispatch(Verb::ListWorkers).await else {
            panic!("the machines")
        };
        assert_eq!(left.iter().map(|w| w.worker).collect::<Vec<_>>(), [away], "{left:?}");

        let (linked_after, current_after) =
            (linked.duration_since(restored), current.duration_since(restored));
        println!(
            "off the network {AWAY:?}: said lost after {lost_after:?}; refused in {refused_in:?}; \
             back: linked after {linked_after:?}, board current after {current_after:?}; {} \
             packets blacked out",
            net.counts().blacked_out
        );
        // The lease's idle timeout, counted from the verb that went into the cut: the first
        // packet that asked for an answer after the last one heard.
        let lease = slopty_net::endpoint::LEASE_IDLE_TIMEOUT;
        assert_eq!(lost_after, lease, "said lost at the lease's idle timeout");
        assert_eq!(refused_in, Duration::ZERO, "refused at once");
        // A dial on its way when the network came back is answered, or given up at its own
        // idle timeout; the next is due within the redials' longest wait.
        let redial = slopty_net::redial::MAX;
        assert!(linked_after <= lease.saturating_add(redial), "{linked_after:?}");
        assert!(
            current_after.saturating_sub(linked_after) < Duration::from_millis(50),
            "the board is current as the link comes back: {current_after:?}"
        );
        drop(link);
        server.shutdown().await;
    }
}
