//! The worker daemon with a server: it dials one (a test double on slopty-net's listener),
//! registers, answers the verbs the server forwards against real terminals in ptyd, passes on what
//! happens, and registers again when the server drops it.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::{SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::framed::{FramedRecv, FramedSend};
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::orchestration::{
        ErrorCode, Input, Outcome, Size, TermRef, UploadPart, Verb, WaitUntil, Waited,
    };
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer};
    use slopty_proto::terminal::{CloseReason, SessionState};
    use slopty_proto::thread::wire::{TableFrame, ThreadRow};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);
    /// From `exit` typed to the worker announcing the exit: ptyd reaps the child and
    /// tells the worker at once, so this is slack for a loaded machine, not a wait.
    const EXIT_BOUND: Duration = Duration::from_secs(5);

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), name)
    }

    /// ptyd and the worker for one test, killed with it.
    struct Daemons {
        _children: Vec<Child>,
        /// Where the worker listens for clients.
        listen: SocketAddr,
    }

    /// Start ptyd, then the worker pointed at `server`, in `dir`.
    async fn daemons(dir: &Path, server: SocketAddr) -> Daemons {
        daemons_finding(dir, server, None).await
    }

    /// As [`daemons`], with `programs` searched first for a command's program.
    async fn daemons_finding(dir: &Path, server: SocketAddr, programs: Option<&Path>) -> Daemons {
        let home = dir.join("home");
        let with_path = |command: &mut Command| {
            slopty_testkit::env::scrub(command.as_std_mut(), &home);
            if let Some(first) = programs {
                command
                    .env("PATH", slopty_testkit::env::path_with(first))
                    .env("STUB_RECORD", dir.join("record.json"));
            }
        };
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = Command::new(bin("slopty-ptyd"));
        with_path(&mut ptyd);
        let mut ptyd = ptyd
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(ptyd.id());
        let ready = async {
            while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
                assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited early");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, ready).await.expect("ptyd socket");
        let mut worker = Command::new(bin("slopty-worker"));
        with_path(&mut worker);
        let mut worker = worker
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0", "--server", &server.to_string()])
            .env("SLOPTY_WORKER_NAME", "link-test")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(worker.id());
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("the worker prints its address")
            .unwrap();
        let listen: SocketAddr = line.trim().parse().unwrap();
        Daemons { _children: vec![ptyd, worker], listen }
    }

    /// The server's side of one worker link: asks verbs, keeps what else the worker sent, and
    /// answers its pings as a server does, on tasks of their own, so a test that reads nothing
    /// for a while still holds the link (`slopty_net::liveness`).
    struct Peer {
        conn: slopty_net::Connection,
        tx: PeerTx,
        rx: PeerRx,
        heard: Vec<ToServer>,
        next: u64,
        /// Cleared, the worker's pings go unanswered while everything else still flows.
        answering: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// When a ping was last answered: the worker's silence counts from there.
        answered: tokio::sync::watch::Receiver<Option<std::time::Instant>>,
    }

    /// What the test sends the worker, beside the pongs.
    struct PeerTx(tokio::sync::mpsc::UnboundedSender<FromServer>);

    impl PeerTx {
        /// Awaited as the link's own send was.
        fn send(&self, msg: &FromServer) -> std::future::Ready<Result<(), String>> {
            std::future::ready(self.0.send(msg.clone()).map_err(|e| e.to_string()))
        }
    }

    /// What the worker sent but its pings.
    struct PeerRx(tokio::sync::mpsc::UnboundedReceiver<Result<ToServer, String>>);

    impl PeerRx {
        async fn recv(&mut self) -> Result<ToServer, String> {
            self.0.recv().await.unwrap_or_else(|| Err("the link ended".to_owned()))
        }
    }

    impl Peer {
        /// Welcome the worker that dialed; its registration.
        async fn welcome(link: AcceptedLink) -> (Self, Registration) {
            let Role::Worker(registration) = link.role else { panic!("{:?}", link.role) };
            let (mut tx, mut rx): (FramedSend<FromServer>, FramedRecv<ToServer>) =
                (link.tx, link.rx);
            let welcome =
                FromServer::Welcome { name: "fake".into(), link: 1, build: String::new() };
            tx.send(&welcome).await.unwrap();
            let (to_worker, mut outgoing) = tokio::sync::mpsc::unbounded_channel();
            let (incoming, from_worker) = tokio::sync::mpsc::unbounded_channel();
            let pongs = to_worker.clone();
            let answering = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let answers = std::sync::Arc::clone(&answering);
            let (last_answer, answered) = tokio::sync::watch::channel(None);
            tokio::spawn(async move {
                while let Some(msg) = outgoing.recv().await {
                    if tx.send(&msg).await.is_err() {
                        break;
                    }
                }
            });
            tokio::spawn(async move {
                loop {
                    let msg = rx.recv().await.map_err(|e| e.to_string());
                    let ended = msg.is_err();
                    match msg {
                        Ok(ToServer::Ping { sent_at }) => {
                            if answers.load(std::sync::atomic::Ordering::SeqCst) {
                                // Noted before it goes, so the worker hears it no sooner.
                                last_answer.send_replace(Some(std::time::Instant::now()));
                                let _gone = pongs.send(FromServer::Pong { sent_at });
                            }
                        }
                        msg => {
                            if incoming.send(msg).is_err() || ended {
                                break;
                            }
                        }
                    }
                }
            });
            let (tx, rx) = (PeerTx(to_worker), PeerRx(from_worker));
            let peer =
                Self { conn: link.conn, tx, rx, heard: Vec::new(), next: 1, answering, answered };
            (peer, *registration)
        }

        async fn send(&mut self, verb: Verb) -> u64 {
            let id = self.next;
            self.next = self.next.saturating_add(1);
            self.tx.send(&FromServer::Request { id, key: None, verb }).await.unwrap();
            id
        }

        /// The reply to request `id`; whatever else arrives meanwhile is kept.
        async fn reply(&mut self, id: u64) -> Outcome {
            loop {
                let msg = tokio::time::timeout(STEP, self.rx.recv()).await.unwrap().unwrap();
                match msg {
                    ToServer::Reply { id: got, outcome } if got == id => return outcome,
                    other => self.heard.push(other),
                }
            }
        }

        async fn ask(&mut self, verb: Verb) -> Outcome {
            let id = self.send(verb).await;
            self.reply(id).await
        }

        /// Wait until the worker has sent something `pred` accepts.
        async fn heard(&mut self, pred: impl Fn(&ToServer) -> bool) {
            while !self.heard.iter().any(&pred) {
                let msg = tokio::time::timeout(STEP, self.rx.recv()).await.unwrap().unwrap();
                self.heard.push(msg);
            }
        }
    }

    /// A quiet interactive bash in `cwd`.
    fn open(worker: WorkerId, cwd: &Path) -> Verb {
        Verb::OpenTerminal {
            worker,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command: ["/bin/bash", "--noprofile", "--norc", "-i"].map(String::from).to_vec(),
            env: vec![
                ("PS1".to_owned(), "$ ".to_owned()),
                ("BASH_SILENCE_DEPRECATION_WARNING".to_owned(), "1".to_owned()),
            ],
            name: Some("link test".to_owned()),
            size: None,
            session: None,
            worktree: None,
        }
    }

    fn text(s: &str) -> Input {
        Input::Text(s.to_owned())
    }

    /// Ask the worker in `dir` over its control socket, as the CLI does.
    async fn ctl(dir: &Path, req: &slopty_proto::ctl::CtlRequest) -> slopty_proto::ctl::CtlReply {
        use tokio::io::AsyncWriteExt as _;
        let mut stream = tokio::net::UnixStream::connect(dir.join("worker.sock")).await.unwrap();
        let mut line = serde_json::to_vec(req).unwrap();
        line.push(b'\n');
        stream.write_all(&line).await.unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await.unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    /// The stub agent's record once `pred` holds for it.
    async fn recorded(at: &Path, pred: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let looking = async {
            loop {
                let record = std::fs::read(at).ok().and_then(|b| serde_json::from_slice(&b).ok());
                if let Some(record) = record.filter(|r| pred(r)) {
                    return record;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, looking).await.expect("the stub agent's record")
    }

    /// A terminal's summary follows it: the server hears the directory, repository and branch
    /// the shell reported, again when a command checks out another branch, and always the start
    /// ptyd stamped when it spawned the shell.
    #[tokio::test]
    async fn the_server_hears_where_a_terminal_is_and_since_when() {
        let dir = tempfile::tempdir().unwrap();
        let repo = std::fs::canonicalize(dir.path()).unwrap().join("project");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let unix_ms = || {
            let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
            u64::try_from(since.unwrap().as_millis()).unwrap()
        };

        let before = unix_ms();
        let Outcome::Opened(term) = peer.ask(open(reg.worker, &repo)).await else {
            panic!("the terminal opens");
        };
        let after = unix_ms();
        let root = repo.to_string_lossy().into_owned();
        let on = |branch: &'static str| {
            let root = root.clone();
            move |m: &ToServer| {
                matches!(m, ToServer::SessionChanged(s) if s.id == term.session
                    && s.cwd.as_deref() == Some(root.as_str())
                    && s.repo.as_deref() == Some(root.as_str())
                    && s.branch.as_deref() == Some(branch))
            }
        };
        peer.heard(on("main")).await;

        let switch = text("printf 'ref: refs/heads/feature/rows\\n' > .git/HEAD\n");
        assert_eq!(peer.ask(Verb::SendInput { term, input: switch }).await, Outcome::Done);
        peer.heard(on("feature/rows")).await;

        let starts: Vec<u64> = peer
            .heard
            .iter()
            .filter_map(|m| match m {
                ToServer::SessionChanged(s) if s.id == term.session => {
                    Some(s.started_ms.as_millis())
                }
                _ => None,
            })
            .collect();
        assert!(starts.len() >= 3, "opened, then moved twice: {starts:?}");
        assert!(
            starts.iter().all(|at| (before..=after).contains(at)),
            "{before}..={after}: {starts:?}"
        );
    }

    /// Another device edits this worker's `[worker]` through the server: the edit lands in the
    /// worker's own settings file, which the worker reads as it reads its own. An edit outside
    /// `[worker]` is refused and writes nothing.
    #[tokio::test]
    async fn a_worker_setting_edited_from_afar_lands_in_the_file() {
        use slopty_proto::settings::SettingEdit;
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let file = dir.path().join("data").join("settings.toml");

        let outside = SettingEdit {
            table: "server".to_owned(),
            key: "listen".to_owned(),
            entry: None,
            literal: Some("4".to_owned()),
        };
        let refused = peer.ask(Verb::Settings { of: Some(reg.worker), edits: vec![outside] }).await;
        assert!(matches!(&refused, Outcome::Error { code: ErrorCode::Invalid, .. }), "{refused:?}");
        assert!(!file.exists(), "nothing written");

        let attached = SettingEdit {
            table: "worker".to_owned(),
            key: "keep_awake".to_owned(),
            entry: None,
            literal: Some("\"attached\"".to_owned()),
        };
        let Outcome::Settings(read) =
            peer.ask(Verb::Settings { of: Some(reg.worker), edits: vec![attached] }).await
        else {
            panic!("the settings are answered");
        };
        assert_eq!(read.path, file.to_string_lossy());
        assert_eq!(read.tables, ["worker", "network"]);
        assert!(read.problems.is_empty(), "{:?}", read.problems);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), read.text);
        let own = slopty_settings::Settings::load(&file).settings.worker.keep_awake;
        assert_eq!(own, slopty_settings::KeepAwake::Attached);
    }

    /// A worker no service manager keeps alive (one a test or a person started by hand) would
    /// not come back from a restart, so it refuses one and goes on serving.
    #[tokio::test]
    async fn a_worker_started_by_hand_refuses_a_restart_and_stays() {
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let refused = peer.ask(Verb::RestartWorker { worker: reg.worker }).await;
        assert!(
            matches!(&refused, Outcome::Error { code: ErrorCode::Unsupported, message }
                if message.contains("start it again where it runs")),
            "{refused:?}"
        );
        let still = peer.ask(Verb::Settings { of: Some(reg.worker), edits: vec![] }).await;
        assert!(matches!(still, Outcome::Settings(_)), "still serving: {still:?}");
    }

    /// The doctor names the worker as the server lists it and says how its server answers:
    /// not linked while the server has not welcomed it, linked once it has, and dialling again,
    /// with why, once the server drops it.
    #[tokio::test]
    async fn the_doctor_names_the_worker_and_whether_its_server_answers() {
        use slopty_proto::ctl::{CtlReply, CtlRequest, Health, LinkState};

        async fn doctor(dir: &Path) -> Health {
            let CtlReply::Doctor(health) = ctl(dir, &CtlRequest::Doctor).await else {
                panic!("a doctor's answer");
            };
            *health
        }
        async fn until(dir: &Path, what: &str, pred: impl Fn(&LinkState) -> bool) -> Health {
            let waiting = async {
                loop {
                    let health = doctor(dir).await;
                    if health.server.as_ref().is_some_and(|s| pred(&s.link)) {
                        return health;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            };
            tokio::time::timeout(STEP, waiting).await.unwrap_or_else(|_| panic!("{what}"))
        }

        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let at = server.local_addr().unwrap();
        let _daemons = daemons(dir.path(), at).await;
        let before = doctor(dir.path()).await;
        let said = before.server.expect("a server is set");
        assert_eq!(said.address, at.to_string());
        assert_ne!(said.link, LinkState::Linked, "not welcomed yet");

        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (peer, reg) = Peer::welcome(link).await;
        let linked = until(dir.path(), "linked", |l| *l == LinkState::Linked).await;
        assert_eq!(linked.worker, reg.worker, "the worker the server lists");

        peer.conn.close(0_u32.into(), b"server restarting");
        let dropped =
            until(dir.path(), "dialling again", |l| matches!(l, LinkState::Redialling { .. }))
                .await;
        let said = dropped.server.map(|s| s.link);
        assert!(
            matches!(&said, Some(LinkState::Redialling { why }) if !why.is_empty()),
            "it says why: {said:?}"
        );
    }

    /// A server whose control stream stops answering while its connection still holds (QUIC's
    /// keep-alives answered, as a stalled reader leaves it) is given up by the worker within six
    /// seconds of its last answer and dialled again: the lease follows the stream, not the
    /// connection under it.
    #[tokio::test]
    async fn a_server_that_stops_answering_its_pings_is_dialled_again() {
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, first) = Peer::welcome(link).await;
        // Answered for a while: the link holds past the bar.
        let held = slopty_net::liveness::PONG_WITHIN.saturating_add(Duration::from_secs(1));
        let again = tokio::time::timeout(held, server.accept()).await;
        assert!(again.is_err(), "a server that answers keeps its link");
        let term = TermRef { worker: first.worker, session: SessionId::new() };
        let read = peer.ask(Verb::ReadScreen { term }).await;
        assert!(
            matches!(read, Outcome::Error { code: ErrorCode::UnknownTerminal, .. }),
            "the worker still answers: {read:?}"
        );

        peer.answering.store(false, std::sync::atomic::Ordering::SeqCst);
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        // Counted from the last answer, as the worker counts its silence: a ping on its way
        // when the answers stopped, or one a loaded runner read late, goes unanswered, so the
        // last answer may be well over a ping before the mute.
        let last = peer.answered.borrow().expect("pings were answered");
        let took = last.elapsed();
        let (_peer, second) = Peer::welcome(link).await;
        assert_eq!(second.worker, first.worker, "the same worker registers again");
        println!("MEASURE a server that stopped answering, dialled again after {took:?}");
        let within = slopty_net::liveness::PONG_WITHIN;
        assert!(took >= within, "given up only past its last answer's six seconds: {took:?}");
        assert!(took < within.saturating_add(Duration::from_secs(3)), "{took:?}");
        drop(peer);
    }

    /// A probe the worker is running when it is killed outright dies with it: its warden kills
    /// the probe's process group, which no kill of the worker reaches.
    #[tokio::test]
    async fn a_probe_running_when_the_worker_is_killed_dies_with_it() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        // A `claude` that hangs on the first thing it is asked, as one stuck on the network.
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let pids = dir.path().join("probe.pid");
        let claude = programs.join("claude");
        let script = format!("#!/bin/sh\necho $$ >> '{}'\nexec /bin/sleep 600\n", pids.display());
        std::fs::write(&claude, script).unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;

        let probe = tokio::time::timeout(STEP, async {
            loop {
                let said = std::fs::read_to_string(&pids).unwrap_or_default();
                if let Some(pid) = said.lines().next().and_then(|l| l.trim().parse::<u32>().ok()) {
                    break pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the worker asks its agents what they are");
        // Killed outright, as a crash or a test's teardown ends it. The probe leads a process
        // group of its own, as every command the worker runs for itself does.
        drop(daemons);
        let gone = tokio::time::timeout(STEP, async {
            while !slopty_testkit::group::gone(probe) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if gone.is_err() {
            slopty_testkit::group::kill(probe);
        }
        assert!(gone.is_ok(), "the probe outlived the worker");
    }

    /// Once registered, the worker tells the server only the clones it has: its toolchains and
    /// the rest are gathered when the server first asks, and answered then. What the server
    /// fills in itself from the registration is left to it.
    #[tokio::test]
    async fn the_worker_gathers_its_facts_once_the_server_asks() {
        use slopty_proto::server::Fact;

        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        // The cargo running this test, found where the worker looks for toolchains.
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let cargo = std::env::var_os("CARGO").expect("run under cargo");
        std::os::unix::fs::symlink(cargo, programs.join("cargo")).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, registration) = Peer::welcome(link).await;

        peer.heard(|m| matches!(m, ToServer::Facts(_))).await;
        let unasked = peer
            .heard
            .iter()
            .find_map(|m| match m {
                ToServer::Facts(facts) => Some(facts),
                _ => None,
            })
            .expect("heard above");
        assert!(unasked.keys().eq(["repos"]), "nothing probed before the ask: {unasked:?}");

        let worker = registration.worker;
        let asked = peer.ask(Verb::WorkerFacts { worker: Some(worker) }).await;
        let Outcome::Facts(said) = asked else { panic!("{asked:?}") };
        let [own] = said.as_slice() else { panic!("{said:?}") };
        assert_eq!(own.worker, worker);
        let facts = &own.facts;
        let Some(Fact::Map(toolchains)) = facts.get("toolchains") else { panic!("{facts:?}") };
        assert!(
            matches!(toolchains.get("cargo"), Some(Fact::Text(v)) if !v.is_empty()),
            "{facts:?}"
        );
        for filled in ["name", "worker", "os", "arch", "cpus", "memory_mb", "load", "online"] {
            assert!(!facts.contains_key(filled), "{filled} is the server's: {facts:?}");
        }
    }

    /// The largest read fits in one message on the link with its envelope, a file larger than
    /// one read is refused whole and read in parts, and the link goes on answering.
    #[tokio::test]
    async fn the_largest_read_fits_in_one_message_and_a_larger_file_is_read_in_parts() {
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let worker = reg.worker;

        let big = dir.path().join("big.bin");
        let cap = slopty_worker::orchestrate::MAX_FILE_BYTES;
        let size = cap.saturating_add(5);
        std::fs::File::create(&big).unwrap().set_len(size).unwrap();
        let path = big.to_string_lossy().into_owned();
        let read = |offset, length| Verb::ReadFile { worker, path: path.clone(), offset, length };
        let whole = peer.ask(read(0, None)).await;
        let Outcome::Error { code: ErrorCode::Failed, message } = whole else {
            panic!("{whole:?}")
        };
        assert!(message.contains("offset and length"), "{message}");
        let first = peer.ask(read(0, Some(u64::MAX))).await;
        let Outcome::File { bytes, offset: 0, size: told } = first else { panic!("{first:?}") };
        assert_eq!((bytes.len() as u64, told), (cap, size), "a whole read's worth arrived");
        let rest = peer.ask(read(cap, None)).await;
        let Outcome::File { bytes, .. } = rest else { panic!("{rest:?}") };
        assert_eq!(bytes.len(), 5);

        let ports = peer.ask(Verb::ListPorts { worker }).await;
        assert!(matches!(ports, Outcome::Ports(_)), "the link still answers: {ports:?}");
    }

    /// `git -C dir args…` with nobody's config: what it printed.
    fn git(dir: &Path, args: &[&str]) -> String {
        let ran = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(ran.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&ran.stderr));
        String::from_utf8_lossy(&ran.stdout).trim().to_owned()
    }

    /// A terminal asked to open in a worktree opens in one the worker made from the clone its
    /// cwd names, at the base asked for and not at the branch the clone has checked out, as a
    /// Codex agent's does. Asked again under the same name it reopens that one as it is; with no
    /// cwd there is no clone to make it from, and it is refused.
    #[tokio::test]
    async fn a_terminal_opens_in_a_worktree_made_from_its_base() {
        use slopty_proto::thread::wire::NewWorktree;

        let dir = tempfile::tempdir().unwrap();
        let clone = dir.path().join("demo");
        std::fs::create_dir_all(&clone).unwrap();
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "target"]);
        let main = git(&clone, &["rev-parse", "main"]);
        git(&clone, &["switch", "-q", "-c", "feature"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "the person's own work"]);
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let record = dir.path().join("where.txt");
        let opening = |cwd: Option<&Path>| Verb::OpenTerminal {
            worker: reg.worker,
            cwd: cwd.map(|c| c.to_string_lossy().into_owned()),
            command: ["/bin/sh", "-c", "pwd -P > \"$WHERE\".part && mv \"$WHERE\".part \"$WHERE\""]
                .map(String::from)
                .to_vec(),
            env: vec![("WHERE".to_owned(), record.to_string_lossy().into_owned())],
            name: None,
            size: None,
            session: None,
            worktree: Some(NewWorktree {
                name: "slopty-demo-1".to_owned(),
                base: Some("main".to_owned()),
                merge_base: None,
                pull: None,
                setup: true,
            }),
        };
        let opened_in = async || {
            let looking = async {
                loop {
                    if let Ok(text) = std::fs::read_to_string(&record) {
                        return text.trim().to_owned();
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            };
            tokio::time::timeout(STEP, looking).await.expect("the terminal says where it is")
        };

        let tree = std::fs::canonicalize(&clone).unwrap().join(".claude/worktrees/slopty-demo-1");
        let made_in = |outcome: Outcome| {
            let Outcome::OpenedIn { worktree, .. } = outcome else {
                panic!("opened in its worktree: {outcome:?}")
            };
            assert_eq!(worktree.name, "slopty-demo-1");
            assert_eq!(Path::new(&worktree.path), tree, "the worktree it says it opened in");
            assert_eq!(worktree.branch.as_deref(), Some("worktree-slopty-demo-1"));
        };
        made_in(peer.ask(opening(Some(&clone))).await);
        assert_eq!(opened_in().await, tree.to_string_lossy());
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]), main, "from main");
        assert_eq!(git(&tree, &["branch", "--show-current"]), "worktree-slopty-demo-1");
        assert_eq!(git(&clone, &["branch", "--show-current"]), "feature", "the clone untouched");

        git(&tree, &["commit", "-q", "--allow-empty", "-m", "the task's work"]);
        let work = git(&tree, &["rev-parse", "HEAD"]);
        std::fs::remove_file(&record).unwrap();
        made_in(peer.ask(opening(Some(&clone))).await);
        assert_eq!(opened_in().await, tree.to_string_lossy());
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]), work, "reopened as is");

        let nowhere = peer.ask(opening(None)).await;
        assert!(matches!(nowhere, Outcome::Error { code: ErrorCode::Invalid, .. }), "{nowhere:?}");
    }

    #[tokio::test]
    async fn a_worker_registers_answers_forwarded_verbs_and_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let daemons = daemons(dir.path(), server.local_addr().unwrap()).await;

        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        assert_eq!(reg.name, "link-test");
        assert_eq!(reg.listen, daemons.listen, "where its clients' listener is bound");
        assert_eq!(reg.sessions, []);
        let host = if cfg!(target_os = "linux") { Os::Linux } else { Os::MacOs };
        assert_eq!(reg.caps.os, host, "the host's own");
        assert!(reg.caps.cpus > 0 && reg.caps.memory > 0, "{:?}", reg.caps);
        let worker = reg.worker;

        let Outcome::Opened(term) = peer.ask(open(worker, dir.path())).await else {
            panic!("the terminal opens");
        };
        assert_eq!(term.worker, worker);
        // The server hears of the terminal before the answer, so a caller naming it next (a
        // agent that asked for it) finds it.
        let announced =
            |m: &ToServer| matches!(m, ToServer::SessionChanged(s) if s.id == term.session);
        assert!(peer.heard.iter().any(announced), "before the answer: {:?}", peer.heard);

        // Resized with no client showing it; the server hears the new size before the answer.
        let size = Size { cols: 100, rows: 30 };
        assert_eq!(peer.ask(Verb::ResizeTerminal { term, size }).await, Outcome::Done);
        let resized = |m: &ToServer| matches!(m, ToServer::SessionChanged(s) if s.id == term.session && (s.cols, s.rows) == (100, 30));
        assert!(peer.heard.iter().any(resized), "{:?}", peer.heard);
        let tiny = Size { cols: 1, rows: 1 };
        let refused = peer.ask(Verb::ResizeTerminal { term, size: tiny }).await;
        assert!(matches!(refused, Outcome::Error { code: ErrorCode::Invalid, .. }), "{refused:?}");

        // Typed, then waited for in a separate request: the output is found however soon it
        // came.
        let typed = peer.ask(Verb::SendInput { term, input: text("echo hi\n") }).await;
        assert_eq!(typed, Outcome::Done);
        let until = WaitUntil::Output("^hi$".to_owned());
        let waited = peer.ask(Verb::WaitFor { term, until, timeout_ms: 20_000 }).await;
        let Outcome::Waited(Waited::Met { line: Some(hi) }) = waited else { panic!("{waited:?}") };
        let done =
            peer.ask(Verb::WaitFor { term, until: WaitUntil::CommandDone, timeout_ms: 20_000 });
        assert_eq!(done.await, Outcome::Waited(Waited::Met { line: None }));
        let read = peer.ask(Verb::ReadOutput { term, since: None, max_lines: 100 }).await;
        let Outcome::Output { lines, .. } = read else { panic!("{read:?}") };
        assert!(lines.iter().any(|l| l.index == hi.index && l.text == "hi"), "{lines:?}");
        let listed = peer.ask(Verb::ListCommands { term, since: None }).await;
        let Outcome::Commands(commands) = listed else { panic!("{listed:?}") };
        assert!(commands.iter().any(|c| c.line == "echo hi" && c.exit == Some(0)), "{commands:?}");

        // A long wait holds up nothing behind it.
        let never = WaitUntil::Output("never printed".to_owned());
        let slow = peer.send(Verb::WaitFor { term, until: never, timeout_ms: 3_000 }).await;
        let screen = peer.ask(Verb::ReadScreen { term }).await;
        let Outcome::Screen(screen) = screen else { panic!("{screen:?}") };
        // Ends with: Debian's bash reads /etc/bash.bashrc, whose PS1 replaces the test's, even
        // under `--norc` once the shell integration's `--rcfile` stands in for it.
        assert!(screen.lines.iter().any(|l| l.text.ends_with("$ echo hi")), "{screen:?}");
        assert_eq!(peer.reply(slow).await, Outcome::Waited(Waited::TimedOut));

        let bad = peer.ask(Verb::SendInput { term, input: Input::Keys(vec!["ctrl+nope".into()]) });
        let Outcome::Error { code: ErrorCode::Invalid, message } = bad.await else {
            panic!("a bad key is invalid");
        };
        assert!(message.contains("[mods+]key"), "{message}");

        // A listener in the terminal's process tree.
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let nc = text(&format!("nc -l 127.0.0.1 {port}\n"));
        assert_eq!(peer.ask(Verb::SendInput { term, input: nc }).await, Outcome::Done);
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        loop {
            let Outcome::Ports(ports) = peer.ask(Verb::ListPorts { worker }).await else {
                panic!("ports");
            };
            if let Some(found) = ports.iter().find(|p| p.number == port) {
                assert_eq!((found.process.as_str(), found.session), ("nc", Some(term.session)));
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "nc never listened: {ports:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let interrupt = Input::Keys(vec!["ctrl+c".to_owned()]);
        assert_eq!(peer.ask(Verb::SendInput { term, input: interrupt }).await, Outcome::Done);

        let path = dir.path().join("note.txt").to_string_lossy().into_owned();
        let upload = slopty_core::XferId::new();
        let step = |part| Verb::Upload { worker, path: path.clone(), upload, part };
        let bytes = UploadPart::Bytes { offset: 0, bytes: b"hello".to_vec() };
        assert_eq!(peer.ask(step(bytes)).await, Outcome::Done);
        let digest = *blake3::hash(b"hello").as_bytes();
        let finish = UploadPart::Finish { size: 5, digest, mode: None };
        assert_eq!(peer.ask(step(finish)).await, Outcome::Done);
        assert_eq!(
            peer.ask(Verb::ReadFile { worker, path, offset: 1, length: Some(3) }).await,
            Outcome::File { bytes: b"ell".to_vec(), offset: 1, size: 5 }
        );

        let elsewhere = TermRef { worker: WorkerId::new(), session: term.session };
        let wrong = peer.ask(Verb::ReadScreen { term: elsewhere }).await;
        assert!(
            matches!(wrong, Outcome::Error { code: ErrorCode::UnknownWorker, .. }),
            "{wrong:?}"
        );
        let theirs = peer.ask(Verb::ListWorkers).await;
        assert!(matches!(theirs, Outcome::Error { code: ErrorCode::Invalid, .. }), "{theirs:?}");

        // The server drops the link: the worker dials again and registers what it runs.
        peer.conn.close(0_u32.into(), b"server restarting");
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, again) = Peer::welcome(link).await;
        assert_eq!(again.worker, worker);
        assert!(again.sessions.iter().any(|s| s.id == term.session), "{:?}", again.sessions);

        // A shell that exits on its own is announced as exited and stays, its last screen still
        // readable, until a verb closes it.
        let mut sized = open(worker, dir.path());
        if let Verb::OpenTerminal { size, .. } = &mut sized {
            *size = Some(Size { cols: 90, rows: 20 });
        }
        let Outcome::Opened(quitter) = peer.ask(sized).await else {
            panic!("the second terminal opens");
        };
        peer.heard(|m| {
            matches!(m, ToServer::SessionChanged(s) if s.id == quitter.session && (s.cols, s.rows) == (90, 20))
        })
        .await;
        assert_eq!(
            peer.ask(Verb::SendInput { term: quitter, input: text("exit\n") }).await,
            Outcome::Done
        );
        let exited = |m: &ToServer| matches!(m, ToServer::SessionChanged(s) if s.id == quitter.session && matches!(s.state, SessionState::Exited { .. }));
        tokio::time::timeout(EXIT_BOUND, peer.heard(exited))
            .await
            .expect("the exit is announced within the bound");
        let kept = peer.ask(Verb::ReadScreen { term: quitter }).await;
        assert!(matches!(kept, Outcome::Screen(_)), "the last screen is kept: {kept:?}");
        assert_eq!(peer.ask(Verb::Close { term: quitter }).await, Outcome::Done);
        peer.heard(|m| {
            matches!(m, ToServer::SessionClosed { session, reason: CloseReason::Requested } if *session == quitter.session)
        })
        .await;
        let gone = peer.ask(Verb::ReadScreen { term: quitter }).await;
        assert!(
            matches!(gone, Outcome::Error { code: ErrorCode::UnknownTerminal, .. }),
            "{gone:?}"
        );

        assert_eq!(peer.ask(Verb::Close { term }).await, Outcome::Done);
        peer.heard(|m| {
            matches!(m, ToServer::SessionClosed { session, reason: CloseReason::Requested } if *session == term.session)
        })
        .await;
        let closes = |id| {
            peer.heard
                .iter()
                .filter(|m| matches!(m, ToServer::SessionClosed { session, .. } if *session == id))
                .count()
        };
        assert_eq!((closes(quitter.session), closes(term.session)), (1, 1), "{:?}", peer.heard);
        let gone = peer.ask(Verb::ReadScreen { term }).await;
        assert!(
            matches!(gone, Outcome::Error { code: ErrorCode::UnknownTerminal, .. }),
            "{gone:?}"
        );
    }

    /// The stand-in `claude` first on the `PATH` of daemons in `dir` linked to `server`, its
    /// record's path.
    async fn with_claude(dir: &Path, server: SocketAddr) -> (Daemons, PathBuf) {
        let programs = dir.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        (daemons_finding(dir, server, Some(&programs)).await, dir.join("record.json"))
    }

    /// The server starts a Claude Code thread at `seat` in `cwd`, told `prompt` first: the thread
    /// it names.
    async fn seated_claude(
        peer: &mut Peer,
        worker: WorkerId,
        cwd: &Path,
        seat: SessionId,
        prompt: &str,
    ) -> slopty_proto::thread::ThreadId {
        use slopty_proto::thread::AgentId;
        use slopty_proto::thread::wire::Start;

        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: cwd.to_string_lossy().into_owned(),
            drive: None,
            prompt: Some(prompt.to_owned()),
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: None,
        };
        let verb = Verb::StartThread {
            worker,
            start: Box::new(start),
            seat,
            facts: Vec::new(),
            system: None,
        };
        let Outcome::ThreadStarted { thread, .. } = peer.ask(verb).await else {
            panic!("the thread starts")
        };
        thread
    }

    /// Once registered, the worker publishes its thread table to the server, all of it first:
    /// a thread the server starts at a seat is a row there, naming its seat, for the server's
    /// attention ladder.
    #[tokio::test]
    async fn the_server_hears_the_workers_threads() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let (_daemons, _record) = with_claude(&root, server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        peer.heard(|m| matches!(m, ToServer::Threads(TableFrame::Snapshot { .. }))).await;

        let seat = SessionId::new();
        let thread = seated_claude(&mut peer, reg.worker, &root, seat, "Say hello.").await;
        let seated = seat.to_string();
        let has_row = |m: &ToServer| match m {
            ToServer::Threads(
                TableFrame::Snapshot { rows, .. } | TableFrame::Delta { rows, .. },
            ) => rows.iter().any(|r: &ThreadRow| {
                r.id == thread && r.facts.get(slopty_proto::agent::SEAT_FACT) == Some(&seated)
            }),
            _ => false,
        };
        peer.heard(has_row).await;
        let first = peer.heard.iter().position(|m| matches!(m, ToServer::Threads(_)));
        let snapshot = first.and_then(|at| peer.heard.get(at));
        assert!(
            matches!(snapshot, Some(ToServer::Threads(TableFrame::Snapshot { .. }))),
            "the whole table first: {snapshot:?}"
        );
    }

    /// An agent the worker starts inherits nothing of the environment the test runs in: not
    /// the developer's Claude Code settings or credentials, not the Slopty terminal the test
    /// may run inside, not the build's own variables. The daemons start from a clean one
    /// (`slopty_testkit::env::scrub`), and the agent's terminal from theirs, with the Slopty
    /// session it runs in.
    #[tokio::test]
    async fn an_agent_the_worker_starts_inherits_nothing_of_the_test_s_environment() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let (_daemons, record) = with_claude(&root, server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let _thread =
            seated_claude(&mut peer, reg.worker, &root, SessionId::new(), "Say hello.").await;
        let started = recorded(&record, |r| r["env"].is_object()).await;
        let inherited: std::collections::BTreeMap<String, u64> =
            serde_json::from_value(started["env"].clone()).unwrap();
        assert!(inherited.contains_key(slopty_proto::ctl::SESSION_ENV), "its terminal's own");
        let leaked = slopty_testkit::env::leaked(&inherited);
        assert!(leaked.is_empty(), "the test's own environment leaked into the agent: {leaked:?}");
    }

    /// Whether `msg` is a table frame with `thread` at `phase`.
    fn at_phase(
        msg: &ToServer,
        thread: slopty_proto::thread::ThreadId,
        phase: slopty_proto::thread::Phase,
    ) -> bool {
        match msg {
            ToServer::Threads(
                TableFrame::Snapshot { rows, .. } | TableFrame::Delta { rows, .. },
            ) => rows.iter().any(|r: &ThreadRow| r.id == thread && r.status.phase == phase),
            _ => false,
        }
    }

    /// What the stand-in read from its terminal, in order, each read's bytes and when.
    fn reads(record: &serde_json::Value) -> Vec<(String, u64)> {
        let reads = record["reads"].as_array().map(Vec::as_slice).unwrap_or_default();
        reads
            .iter()
            .map(|r| {
                let bytes = r["bytes"].as_str().unwrap_or_default().to_owned();
                (bytes, r["at_us"].as_u64().unwrap_or_default())
            })
            .collect()
    }

    /// What an orchestrating agent tells another (`agent_tell`) goes into the agent's TUI as one
    /// message: its words as one paste, bracketed as the TUI asked, two lines and all, then its
    /// Enter as a write of its own the wait after them, since an Ink TUI reads an Enter that
    /// comes with a paste as part of the paste.
    #[tokio::test]
    async fn an_agents_words_go_in_as_one_paste_and_an_enter_of_their_own() {
        use slopty_proto::thread::Phase;

        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let (_daemons, record) = with_claude(&root, server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let thread =
            seated_claude(&mut peer, reg.worker, &root, SessionId::new(), "Say hello.").await;
        peer.heard(|m| at_phase(m, thread, Phase::Done)).await;

        let tell = Verb::AgentTell { agent: thread, text: "Second\nline".to_owned() };
        assert_eq!(peer.ask(tell).await, Outcome::Done);
        let prompts = |r: &serde_json::Value| r["prompts"].as_array().map_or(0, Vec::len);
        let seen = recorded(&record, |r| prompts(r) == 2).await;
        assert_eq!(seen["prompts"][1]["text"], "Second\nline", "one message");
        let reads = reads(&seen);
        let start = reads.iter().position(|(b, _)| b.contains("\u{1b}[200~")).expect("a paste");
        let end = reads.iter().position(|(b, _)| b.contains("\u{1b}[201~")).expect("its end");
        let pasted: String = reads[start..=end].iter().map(|(b, _)| b.as_str()).collect();
        assert_eq!(
            pasted.replace('\r', "\n"),
            "\u{1b}[200~Second\nline\u{1b}[201~",
            "the words as one paste, bracketed as the TUI asked"
        );
        let (enter, entered_us) = &reads[end + 1];
        assert_eq!(enter, "\r", "the Enter is a write of its own");
        // Timed where the TUI reads, so with the way's jitter (ptyd, the pty, the reader's turn
        // to run): the wait is proved, its length is the worker's own clock.
        let gap = entered_us - reads[end].1;
        let wait = u64::try_from(slopty_worker::session::SUBMIT_ENTER.as_micros()).unwrap();
        assert!(gap >= wait * 3 / 4, "the Enter came {gap} µs after the paste");
    }

    /// While an agent asks the person at a dialog, what another agent tells it is refused, with
    /// why, and nothing reaches its terminal: the dialog would take the words as its answer.
    #[tokio::test]
    async fn an_agents_words_wait_while_the_agent_asks_the_person() {
        use slopty_proto::thread::Phase;

        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let (_daemons, record) = with_claude(&root, server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let seat = SessionId::new();
        let thread = seated_claude(&mut peer, reg.worker, &root, seat, "ask Which colour?").await;
        peer.heard(|m| at_phase(m, thread, Phase::NeedsYou)).await;

        let tell = Verb::AgentTell { agent: thread, text: "Blue, and more".to_owned() };
        let refused = peer.ask(tell).await;
        let Outcome::Error { code: ErrorCode::Conflict, message } = refused else {
            panic!("not refused: {refused:?}")
        };
        assert!(message.contains("answer it there"), "why, in words: {message}");
        let seen =
            recorded(&record, |r| r["prompts"].as_array().is_some_and(|p| !p.is_empty())).await;
        let typed: String = reads(&seen).into_iter().map(|(b, _)| b).collect();
        assert!(!typed.contains("Blue"), "nothing typed: {typed:?}");
    }
}
