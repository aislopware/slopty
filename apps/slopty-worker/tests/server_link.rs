//! The worker daemon with a server: it dials one (a test double on slopty-net's listener),
//! registers, answers the verbs the server forwards against real terminals in ptyd, passes on what
//! happens, and registers again when the server drops it.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::WorkerId;
    use slopty_net::admission::Admission;
    use slopty_net::framed::{FramedRecv, FramedSend};
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::PROTOCOL_VERSION;
    use slopty_proto::orchestration::{
        ErrorCode, Input, Outcome, TermRef, Verb, WaitUntil, Waited,
    };
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer};
    use slopty_proto::terminal::CloseReason;
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);
    /// From `exit` typed to the worker announcing the session's end: ptyd reaps the child and
    /// tells the worker at once, so this is slack for a loaded machine, not a wait.
    const EXIT_BOUND: Duration = Duration::from_secs(5);

    /// A sibling binary from the same build, built on demand (as `e2e.rs` does).
    fn bin(name: &str) -> PathBuf {
        let worker = PathBuf::from(env!("CARGO_BIN_EXE_slopty-worker"));
        let path = worker.with_file_name(name);
        if !path.exists() {
            let release = worker.parent().is_some_and(|dir| dir.ends_with("release"));
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut build = std::process::Command::new(cargo);
            build.args(["build", "-p", name]);
            if release {
                build.arg("--release");
            }
            assert!(build.status().expect("run cargo").success(), "build {name}");
        }
        path
    }

    /// ptyd and the worker for one test, killed with it.
    struct Daemons {
        _children: Vec<Child>,
        /// Where the worker listens for clients.
        listen: SocketAddr,
    }

    /// Start ptyd, then the worker pointed at `server`, in `dir`.
    async fn daemons(dir: &std::path::Path, server: SocketAddr) -> Daemons {
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = Command::new(bin("slopty-ptyd"))
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let ready = async {
            while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
                assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited early");
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, ready).await.expect("ptyd socket");
        let mut worker = Command::new(bin("slopty-worker"))
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
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .expect("the worker prints its address")
            .unwrap();
        let listen: SocketAddr = line.trim().parse().unwrap();
        Daemons { _children: vec![ptyd, worker], listen }
    }

    /// The server's side of one worker link: asks verbs, keeps what else the worker sent.
    struct Peer {
        conn: slopty_net::Connection,
        tx: FramedSend<FromServer>,
        rx: FramedRecv<ToServer>,
        heard: Vec<ToServer>,
        next: u64,
    }

    impl Peer {
        /// Welcome the worker that dialed; its registration.
        async fn welcome(link: AcceptedLink) -> (Self, Registration) {
            let Role::Worker(registration) = link.role else { panic!("{:?}", link.role) };
            let mut tx = link.tx;
            let welcome = FromServer::Welcome { protocol: PROTOCOL_VERSION, name: "fake".into() };
            tx.send(&welcome).await.unwrap();
            (Self { conn: link.conn, tx, rx: link.rx, heard: Vec::new(), next: 1 }, registration)
        }

        async fn send(&mut self, verb: Verb) -> u64 {
            let id = self.next;
            self.next = self.next.saturating_add(1);
            self.tx.send(&FromServer::Request { id, verb }).await.unwrap();
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
    fn open(worker: WorkerId, cwd: &std::path::Path) -> Verb {
        Verb::OpenTerminal {
            worker,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command: ["/bin/bash", "--noprofile", "--norc", "-i"].map(String::from).to_vec(),
            env: vec![
                ("PS1".to_owned(), "$ ".to_owned()),
                ("BASH_SILENCE_DEPRECATION_WARNING".to_owned(), "1".to_owned()),
            ],
            name: Some("link test".to_owned()),
        }
    }

    fn text(s: &str) -> Input {
        Input::Text(s.to_owned())
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
        assert_eq!(reg.port, daemons.listen.port(), "clients dial the port it listens on");
        assert!(reg.sessions.is_empty());
        assert_eq!(reg.caps.os, Os::MacOs);
        assert!(reg.caps.cpus > 0 && reg.caps.memory > 0, "{:?}", reg.caps);
        let worker = reg.worker;

        let Outcome::Opened(term) = peer.ask(open(worker, dir.path())).await else {
            panic!("the terminal opens");
        };
        assert_eq!(term.worker, worker);
        peer.heard(|m| matches!(m, ToServer::SessionOpened(s) if s.id == term.session)).await;

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
        assert!(screen.lines.iter().any(|l| l.text == "$ echo hi"), "{screen:?}");
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
        let write = Verb::WriteFile { worker, path: path.clone(), bytes: b"hello".to_vec() };
        assert_eq!(peer.ask(write).await, Outcome::Done);
        assert_eq!(
            peer.ask(Verb::ReadFile { worker, path }).await,
            Outcome::File(b"hello".to_vec())
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

        // A shell that exits on its own is announced as exited, once, without a verb closing
        // it.
        let Outcome::Opened(quitter) = peer.ask(open(worker, dir.path())).await else {
            panic!("the second terminal opens");
        };
        assert_eq!(
            peer.ask(Verb::SendInput { term: quitter, input: text("exit\n") }).await,
            Outcome::Done
        );
        let exited = |m: &ToServer| matches!(m, ToServer::SessionClosed { session, reason: CloseReason::Exited } if *session == quitter.session);
        tokio::time::timeout(EXIT_BOUND, peer.heard(exited))
            .await
            .expect("the exit is announced within the bound");
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
}
