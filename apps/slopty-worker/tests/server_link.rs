//! The worker daemon with a server: it dials one (a test double on slopty-net's listener),
//! registers, answers the verbs the server forwards against real terminals in ptyd, passes on what
//! happens, and registers again when the server drops it.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::{ClientId, SessionId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::framed::{FramedRecv, FramedSend};
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::WorkerMsg;
    use slopty_proto::handshake::Hello;
    use slopty_proto::items::{ItemKind, ItemOp, ItemSync};
    use slopty_proto::orchestration::{
        ErrorCode, Input, ItemRef, Outcome, Size, TermRef, UploadPart, Verb, WaitUntil, Waited,
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
                command.env("PATH", slopty_testkit::env::path_with(first));
            }
            // As a developer's shell may hold it: an agent the worker starts must not have it,
            // or its mod goes silent.
            command.env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");
        };
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = Command::new(bin("slopty-ptyd"));
        with_path(&mut ptyd);
        let mut ptyd = ptyd
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
            let welcome = FromServer::Welcome { name: "fake".into(), link: 1 };
            tx.send(&welcome).await.unwrap();
            (Self { conn: link.conn, tx, rx: link.rx, heard: Vec::new(), next: 1 }, *registration)
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

    /// Whether `m` is a table frame with a row of a thread in terminal `session` that `pred`
    /// holds of.
    fn row_at(m: &ToServer, session: SessionId, pred: impl Fn(&ThreadRow) -> bool) -> bool {
        let ToServer::Threads(TableFrame::Snapshot { rows, .. } | TableFrame::Delta { rows, .. }) =
            m
        else {
            return false;
        };
        rows.iter().any(|r| r.terminal == Some(session) && pred(r))
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

    /// A hook fired in `session`, told to the worker as `slopty hook` tells it.
    async fn hook(dir: &Path, session: SessionId, payload: serde_json::Value) {
        let req = slopty_proto::ctl::CtlRequest::Hook { session, payload: payload.to_string() };
        let reply = ctl(dir, &req).await;
        assert!(matches!(reply, slopty_proto::ctl::CtlReply::Ok { .. }), "{reply:?}");
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

    /// Once registered, the worker publishes its thread table to the server, all of it first:
    /// a Claude Code session that starts in a terminal is a row there, naming that terminal,
    /// for the server's attention ladder.
    #[tokio::test]
    async fn the_server_hears_the_workers_threads() {
        use slopty_proto::thread::wire::TableFrame;

        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        peer.heard(|m| matches!(m, ToServer::Threads(TableFrame::Snapshot { .. }))).await;

        let Outcome::Opened(term) = peer.ask(open(reg.worker, dir.path())).await else {
            panic!("the terminal opens");
        };
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/slopty-agent/tests/fixtures/conversation/tools/transcript.jsonl");
        let transcript = dir.path().join("projects").join("s1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::copy(fixture, &transcript).unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": transcript, "cwd": dir.path(),
        });
        hook(dir.path(), term.session, start).await;
        let thread = slopty_agent::observed::thread_of("s1");
        let has_row = |m: &ToServer| match m {
            ToServer::Threads(
                TableFrame::Snapshot { rows, .. } | TableFrame::Delta { rows, .. },
            ) => rows.iter().any(|r| r.id == thread && r.terminal == Some(term.session)),
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

    /// Once registered, the worker tells the server what it has, its toolchains among it. What
    /// the server fills in itself from the registration is left to it.
    #[tokio::test]
    async fn the_server_hears_the_workers_facts() {
        use slopty_proto::project::Fact;

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
        let (mut peer, _reg) = Peer::welcome(link).await;

        peer.heard(|m| matches!(m, ToServer::Facts(_))).await;
        let facts = peer
            .heard
            .iter()
            .find_map(|m| match m {
                ToServer::Facts(facts) => Some(facts),
                _ => None,
            })
            .expect("heard above");
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

    /// Items an orchestrator puts on the workspace reach a client as they happen: a page appears
    /// with its name, is renamed and goes. A terminal's item is refused both ways,
    /// since its session owns it.
    #[tokio::test]
    async fn an_orchestrated_item_reaches_a_client_and_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let daemons = daemons(dir.path(), server.local_addr().unwrap()).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;
        let worker = reg.worker;
        let endpoint = bind_client().unwrap();
        let hello = Hello { client: ClientId::new(), name: "watcher".to_owned() };
        // The worker listens on every interface; its own machine reaches it on loopback.
        let at = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, daemons.listen.port()));
        let mut client =
            tokio::time::timeout(STEP, connect_addr(&endpoint, at, hello)).await.unwrap().unwrap();
        let mut next_items = async move || loop {
            let msg = tokio::time::timeout(STEP, client.rx.recv()).await.unwrap().unwrap();
            if let WorkerMsg::Items(sync @ ItemSync::Delta { .. }) = msg {
                return sync;
            }
        };

        let url = "http://localhost:5173/".to_owned();
        let kind = ItemKind::Browser { url };
        let opened = Verb::OpenItem { worker, kind: kind.clone(), name: Some("app".to_owned()) };
        let Outcome::Item(item) = peer.ask(opened).await else { panic!("the page opens") };
        let ItemSync::Delta { op: ItemOp::Add(shown), .. } = next_items().await else {
            panic!("the client hears the page")
        };
        assert_eq!((shown.id, &shown.kind, shown.name.as_deref()), (item.item, &kind, Some("app")));
        let Outcome::Items(listed) = peer.ask(Verb::ListItems { worker }).await else { panic!() };
        assert_eq!(listed, [shown]);

        let rename = Verb::RenameItem { item, name: Some("  docs ".to_owned()) };
        assert_eq!(peer.ask(rename).await, Outcome::Done);
        let ItemSync::Delta { op: ItemOp::Rename { id, name }, .. } = next_items().await else {
            panic!("the client hears the name")
        };
        assert_eq!((id, name.as_deref()), (item.item, Some("docs")), "trimmed as a person's is");

        assert_eq!(peer.ask(Verb::RemoveItem { item }).await, Outcome::Done);
        assert!(
            matches!(next_items().await, ItemSync::Delta { op: ItemOp::Remove(id), .. } if id == item.item)
        );
        let gone = peer.ask(Verb::RemoveItem { item }).await;
        assert!(matches!(gone, Outcome::Error { code: ErrorCode::UnknownItem, .. }), "{gone:?}");

        let Outcome::Opened(term) = peer.ask(open(worker, dir.path())).await else { panic!() };
        let ItemSync::Delta { op: ItemOp::Add(shell), .. } = next_items().await else {
            panic!("the terminal's item")
        };
        let theirs = ItemRef { worker, item: shell.id };
        let kept = peer.ask(Verb::RemoveItem { item: theirs }).await;
        assert!(matches!(kept, Outcome::Error { code: ErrorCode::Invalid, .. }), "{kept:?}");
        let session = ItemKind::Terminal { session: term.session };
        let refused = peer.ask(Verb::OpenItem { worker, kind: session, name: None }).await;
        assert!(matches!(refused, Outcome::Error { code: ErrorCode::Invalid, .. }), "{refused:?}");
        endpoint.close(0_u32.into(), b"done");
    }

    /// An agent the server starts reports its status with no hooks in anyone's settings: the
    /// worker hands `claude` the relay beside it on `--settings`, merged into the caller's own.
    /// A stand-in `claude` records what it was given in a session (and answers the worker's
    /// `--version` probe outside one), and the relay runs as Claude Code would run it from those
    /// settings.
    #[tokio::test]
    async fn a_spawned_agent_reports_through_the_relay_it_was_handed() {
        use slopty_proto::agent::AgentKind;
        use slopty_proto::thread::{AgentId, Cap};

        let dir = tempfile::tempdir().unwrap();
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        let (argv, env) = (dir.path().join("argv"), dir.path().join("env"));
        let script = format!(
            "#!/bin/sh\n\
             [ -n \"$SLOPTY_SESSION\" ] || {{ echo '2.1.283 (Claude Code)'; exit 0; }}\n\
             printf '%s\\n' \"$SLOPTY_SESSION\" \"$SLOPTY_WORKER_SOCKET\" \"$SLOPTY_MOD_SOCKET\" \
               \"$CLAUDE_CODE_ENABLE_FUNCTION_HOOKS\" \
               \"[${{CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC-unset}}]\" > '{env}'\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\"; done > '{argv}.part'\n\
             mv '{argv}.part' '{argv}'\n\
             exec sleep 60\n",
            env = env.display(),
            argv = argv.display(),
        );
        let claude = programs.join("claude");
        std::fs::write(&claude, script).unwrap();
        std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let relay = bin("slopty");
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let spawn = Verb::SpawnAgent {
            worker: reg.worker,
            agent: AgentKind::ClaudeCode,
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: None,
            args: ["--settings", r#"{"model":"haiku"}"#, "--verbose"].map(String::from).to_vec(),
            env: Vec::new(),
            size: None,
            session: None,
            permission_flags: true,
            worktree: None,
        };
        let Outcome::Opened(term) = peer.ask(spawn).await else { panic!("the agent starts") };
        let recorded = async {
            loop {
                if let Ok(bytes) = std::fs::read(&argv) {
                    return bytes;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let bytes = tokio::time::timeout(STEP, recorded).await.expect("claude ran");
        let args: Vec<String> = bytes
            .split(|b| *b == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8(word.to_vec()).unwrap())
            .collect();
        let [plugin, mcp, flag, settings, pin, conversation, rest @ ..] = args.as_slice() else {
            panic!("{args:?}")
        };
        assert_eq!((flag.as_str(), rest), ("--settings", &["--verbose".to_owned()][..]));
        assert_eq!(pin, "--session-id", "the conversation's id is chosen before it starts");
        // Any UUID parses as a session id.
        assert!(conversation.parse::<SessionId>().is_ok(), "{conversation}");
        // Slopty's tools, through the same `slopty` beside the worker.
        let mcp: serde_json::Value =
            serde_json::from_str(mcp.strip_prefix("--mcp-config=").expect(mcp)).unwrap();
        let tools = &mcp["mcpServers"]["slopty"];
        assert_eq!(tools["command"].as_str().map(PathBuf::from), Some(relay.clone()));
        assert_eq!(tools["args"], serde_json::json!(["mcp"]));
        // The mod, as the worker wrote it under its data dir, in the flag's `=` form.
        let module = plugin.strip_prefix("--plugin-dir=").map(PathBuf::from).expect(plugin);
        assert!(module.starts_with(dir.path().join("data/claude-mod")), "{module:?}");
        for (path, content) in slopty_agent::claude_mod::FILES {
            assert_eq!(std::fs::read_to_string(module.join(path)).unwrap(), content, "{path}");
        }
        let settings: serde_json::Value = serde_json::from_str(settings).unwrap();
        assert_eq!(settings["model"], "haiku", "the caller's settings are kept");
        assert!(settings.get("permissions").is_none(), "the person allowed its flags");
        for event in slopty_agent::HOOK_EVENTS {
            assert!(slopty_agent::hooks::has_relay(&settings, event), "{event}");
        }
        let entry = &settings["hooks"]["SessionStart"][0]["hooks"][0];
        assert_eq!(entry["command"].as_str().map(PathBuf::from), Some(relay));

        let env = std::fs::read_to_string(&env).unwrap();
        let [session, socket, mod_socket, hooks, traffic] = env.lines().collect::<Vec<_>>()[..]
        else {
            panic!("{env:?}")
        };
        assert_eq!(Path::new(mod_socket), dir.path().join("worker.mod.sock"));
        assert_eq!(hooks, "1", "function hooks on");
        assert_eq!(traffic, "[]", "the inherited switch that silences the mod is cleared");
        let mut hook = Command::new(entry["command"].as_str().unwrap())
            .args(entry["args"].as_array().unwrap().iter().filter_map(|a| a.as_str()))
            .env("SLOPTY_SESSION", session)
            .env("SLOPTY_WORKER_SOCKET", socket)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let payload = br#"{"hook_event_name":"SessionStart","source":"startup"}"#;
        tokio::io::AsyncWriteExt::write_all(&mut hook.stdin.take().unwrap(), payload)
            .await
            .unwrap();
        assert!(tokio::time::timeout(STEP, hook.wait()).await.unwrap().unwrap().success());
        // A hook spoke for the agent's thread: it holds its prompts from now on.
        let approvals = Cap::named(Cap::APPROVALS);
        peer.heard(|m| {
            row_at(m, term.session, |r| {
                r.agent == AgentId::named(AgentId::CLAUDE_CODE) && r.caps.contains(&approvals)
            })
        })
        .await;
    }

    /// An agent the server starts under an id it chose starts once, with its conversation's
    /// id pinned and bypass mode locked off. Nothing is typed into it while it cannot take it:
    /// its first prompt waits for its first hook (a title that looks idle is not enough),
    /// input waits out a permission prompt, and once it has exited, input is refused. Each
    /// change of its permission mode reaches the server once.
    #[tokio::test]
    async fn a_spawned_agent_is_typed_into_only_when_it_can_take_it() {
        use serde_json::json;
        use slopty_proto::agent::AgentKind;
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let _relay = bin("slopty");
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let record = dir.path().join("record.json");
        let chosen = SessionId::new();
        let env = [
            ("STUB_RECORD", record.to_string_lossy().into_owned()),
            ("STUB_HOOKS", "[]".to_owned()),
            ("STUB_TITLE", "\u{2733} Claude Code".to_owned()),
        ];
        let spawn = Verb::SpawnAgent {
            worker: reg.worker,
            agent: AgentKind::ClaudeCode,
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: Some("write the brief".to_owned()),
            args: Vec::new(),
            env: env.map(|(k, v)| (k.to_owned(), v)).to_vec(),
            size: None,
            session: Some(chosen),
            permission_flags: false,
            worktree: None,
        };
        let term = TermRef { worker: reg.worker, session: chosen };
        assert_eq!(peer.ask(spawn.clone()).await, Outcome::Opened(term));
        assert_eq!(peer.ask(spawn).await, Outcome::Opened(term), "the same start again");
        let status = ctl(dir.path(), &slopty_proto::ctl::CtlRequest::Status).await;
        let slopty_proto::ctl::CtlReply::Status { sessions, .. } = status else { panic!() };
        assert_eq!(sessions.iter().map(|s| s.id).collect::<Vec<_>>(), [chosen], "one terminal");

        let started = recorded(&record, |r| r["argv"].is_array()).await;
        let argv: Vec<&str> =
            started["argv"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
        let after = |flag: &str| argv.iter().position(|a| *a == flag).map(|at| argv[at + 1]);
        let conversation = after("--session-id").expect("pinned");
        assert!(conversation.parse::<SessionId>().is_ok(), "{conversation}");
        let settings: serde_json::Value =
            serde_json::from_str(after("--settings").expect("settings")).unwrap();
        assert_eq!(settings["permissions"]["disableBypassPermissionsMode"], "disable");

        // Its title says it is at its prompt before any hook has spoken: a dialog of its own
        // may be up, so nothing goes in.
        peer.heard(|m| row_at(m, chosen, |_| true)).await;
        let early = peer.ask(Verb::SendInput { term, input: text("hello\n") }).await;
        assert!(
            matches!(early, Outcome::Error { code: ErrorCode::AgentNotReady, .. }),
            "{early:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let quiet = recorded(&record, |_| true).await;
        assert_eq!(quiet["typed"], json!([]), "the prompt waits for a hook");

        let said = |event: &str, mode: &str| json!({ "hook_event_name": event, "session_id": conversation, "permission_mode": mode });
        hook(dir.path(), chosen, said("SessionStart", "default")).await;
        recorded(&record, |r| r["typed"] == json!(["write the brief"])).await;

        let mut asking = said("PermissionRequest", "default");
        asking["tool_name"] = json!("Bash");
        hook(dir.path(), chosen, asking).await;
        let held = peer.ask(Verb::SendInput { term, input: text("more\n") }).await;
        let Outcome::Error { code: ErrorCode::AwaitsPerson, message } = held else {
            panic!("{held:?}")
        };
        assert!(message.contains("the person's to answer"), "{message}");
        let mut ran = said("PostToolUse", "plan");
        ran["tool_name"] = json!("Bash");
        hook(dir.path(), chosen, ran).await;
        assert_eq!(peer.ask(Verb::SendInput { term, input: text("more\n") }).await, Outcome::Done);
        recorded(&record, |r| r["typed"] == json!(["write the brief", "more"])).await;

        let modes = |heard: &[ToServer]| -> Vec<String> {
            heard
                .iter()
                .filter_map(|m| match m {
                    ToServer::Report(AgentReport::PermissionMode { session, mode })
                        if *session == chosen =>
                    {
                        Some(mode.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        peer.heard(|m| matches!(m, ToServer::Report(AgentReport::PermissionMode { mode, .. }) if mode == "plan")).await;
        assert_eq!(modes(&peer.heard), ["default", "plan"], "each change once");

        let eof = Input::Keys(vec!["ctrl+d".to_owned()]);
        assert_eq!(peer.ask(Verb::SendInput { term, input: eof }).await, Outcome::Done);
        peer.heard(|m| {
            matches!(m, ToServer::SessionChanged(s) if s.id == chosen && matches!(s.state, SessionState::Exited { .. }))
        })
        .await;
        let gone = peer.ask(Verb::SendInput { term, input: text("ls\n") }).await;
        assert!(matches!(gone, Outcome::Error { code: ErrorCode::AgentExited, .. }), "{gone:?}");
    }

    /// A terminal whose command starts Claude Code, however wrapped, is an agent's from the
    /// start: nothing is typed into it before its first hook, and what its command line gives
    /// it beyond what asks the person is reported, for the server to judge.
    #[tokio::test]
    async fn claude_in_a_shell_line_is_guarded_and_judged_as_a_spawned_one() {
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let _relay = bin("slopty");
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let record = dir.path().join("record.json");
        let line = "cd . && exec claude --allowedTools 'Bash(rm:*)'";
        let opening = Verb::OpenTerminal {
            worker: reg.worker,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            command: ["/bin/sh", "-c", line].map(String::from).to_vec(),
            env: vec![
                ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
                ("STUB_HOOKS".to_owned(), "[]".to_owned()),
            ],
            name: None,
            size: None,
            session: None,
            worktree: None,
        };
        let Outcome::Opened(term) = peer.ask(opening).await else { panic!("opened") };
        let early = peer.ask(Verb::SendInput { term, input: text("1\n") }).await;
        assert!(
            matches!(early, Outcome::Error { code: ErrorCode::AgentNotReady, .. }),
            "{early:?}"
        );
        peer.heard(|m| {
            matches!(m, ToServer::Report(AgentReport::Loosened { session, found })
                if *session == term.session && found.iter().any(|f| f == "--allowedTools"))
        })
        .await;
        let quiet = recorded(&record, |_| true).await;
        assert_eq!(quiet["typed"], serde_json::json!([]), "nothing reached its TUI");
    }

    /// Reports the server sends an agent reach it through its own hooks, never its terminal:
    /// kept until its next prompt, handed over as that prompt's context by the hook its
    /// settings register, and the server told which batch arrived.
    #[tokio::test]
    async fn reports_reach_an_agent_through_its_next_prompt_s_hook() {
        use serde_json::json;
        use slopty_proto::agent::AgentKind;
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let programs = dir.path().join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let _relay = bin("slopty");
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let _daemons =
            daemons_finding(dir.path(), server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let (record, gate) = (dir.path().join("record.json"), dir.path().join("prompted"));
        let session = SessionId::new();
        let prompt = json!([{ "hook_event_name": "UserPromptSubmit", "prompt": "go on" }]);
        let env = [
            ("STUB_RECORD", record.to_string_lossy().into_owned()),
            ("STUB_LATER", prompt.to_string()),
            ("STUB_LATER_AFTER", gate.to_string_lossy().into_owned()),
        ];
        let spawn = Verb::SpawnAgent {
            worker: reg.worker,
            agent: AgentKind::ClaudeCode,
            cwd: dir.path().to_string_lossy().into_owned(),
            prompt: None,
            args: Vec::new(),
            env: env.map(|(k, v)| (k.to_owned(), v)).to_vec(),
            size: None,
            session: Some(session),
            permission_flags: false,
            worktree: None,
        };
        let term = TermRef { worker: reg.worker, session };
        assert_eq!(peer.ask(spawn).await, Outcome::Opened(term));
        let started =
            recorded(&record, |r| r["hooks"].as_array().is_some_and(|h| !h.is_empty())).await;
        assert_eq!(started["hooks"][0]["event"], "SessionStart");
        assert_eq!(started["hooks"][0]["outputs"], json!([]), "nothing waits yet");

        let context = "<slopty-reports project=\"demo\">\ntask 2: needs input\n  Which crate owns the store?\n</slopty-reports>";
        let deliver = FromServer::Deliver { session, batch: 7, context: context.to_owned() };
        peer.tx.send(&deliver).await.unwrap();
        let kept = slopty_agent::reports::dir(&dir.path().join("worker.sock"));
        let waiting = async {
            while !kept.join(format!("{session}.json")).exists() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, waiting).await.expect("the worker keeps the batch");
        let untouched = recorded(&record, |_| true).await;
        assert_eq!(untouched["typed"], json!([]), "nothing is typed into the agent");

        std::fs::write(&gate, b"").unwrap();
        let prompted =
            recorded(&record, |r| r["hooks"].as_array().is_some_and(|h| h.len() == 2)).await;
        let turn = &prompted["hooks"][1];
        assert_eq!(
            (turn["event"].as_str(), turn["fired"].as_bool()),
            (Some("UserPromptSubmit"), Some(true))
        );
        let outputs = turn["outputs"].as_array().unwrap();
        let [handed] = outputs.as_slice() else { panic!("{turn}") };
        assert_eq!(handed["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
        assert_eq!(handed["hookSpecificOutput"]["additionalContext"], context);
        peer.heard(|m| {
            matches!(m, ToServer::Report(AgentReport::Delivered { session: s, batch: 7 }) if *s == session)
        })
        .await;
        assert!(!kept.join(format!("{session}.json")).exists(), "handed over once");
    }

    /// An agent at rest that takes messages on an inbox: the stub with an inbox and a
    /// transcript, `more` in its environment. The peer, the session, the stub's record, the
    /// inbox's notes, the kept batches' directory and the daemons.
    async fn resting_agent_with_an_inbox(
        dir: &Path,
        more: &[(&str, String)],
    ) -> (Peer, SessionId, PathBuf, PathBuf, PathBuf, Daemons) {
        use slopty_proto::agent::AgentKind;

        let programs = dir.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let server =
            ServerListener::bind("127.0.0.1:0".parse().unwrap(), Admission::new(Vec::new()))
                .unwrap();
        let daemons = daemons_finding(dir, server.local_addr().unwrap(), Some(&programs)).await;
        let link = tokio::time::timeout(STEP, server.accept()).await.unwrap().unwrap();
        let (mut peer, reg) = Peer::welcome(link).await;

        let (record, posted) = (dir.join("record.json"), dir.join("inbox.jsonl"));
        let transcript = dir.join("transcript.jsonl");
        let session = SessionId::new();
        let mut env = vec![
            ("STUB_RECORD".to_owned(), record.to_string_lossy().into_owned()),
            ("STUB_INBOX".to_owned(), posted.to_string_lossy().into_owned()),
            ("STUB_TRANSCRIPT".to_owned(), transcript.to_string_lossy().into_owned()),
        ];
        env.extend(more.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        let spawn = Verb::SpawnAgent {
            worker: reg.worker,
            agent: AgentKind::ClaudeCode,
            cwd: dir.to_string_lossy().into_owned(),
            prompt: None,
            args: Vec::new(),
            env,
            size: None,
            session: Some(session),
            permission_flags: false,
            worktree: None,
        };
        let term = TermRef { worker: reg.worker, session };
        assert_eq!(peer.ask(spawn).await, Outcome::Opened(term));
        let started =
            recorded(&record, |r| r["hooks"].as_array().is_some_and(|h| !h.is_empty())).await;
        assert_eq!(started["hooks"][0]["event"], "SessionStart");
        let kept = slopty_agent::reports::dir(&dir.join("worker.sock"));
        (peer, session, record, posted, kept, daemons)
    }

    /// The inbox's notes, once there are `n`.
    async fn inbox_notes(posted: &Path, n: usize) -> Vec<serde_json::Value> {
        let looking = async {
            loop {
                let text = std::fs::read_to_string(posted).unwrap_or_default();
                let got: Vec<serde_json::Value> =
                    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
                if got.len() >= n {
                    return got;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, looking).await.expect("the inbox's notes")
    }

    /// Reports reach an agent at rest at once, through the inbox Claude Code takes messages
    /// from other processes on, never its terminal: its first hook notes the inbox, and a batch
    /// the server sends is posted there with the session's token as a user turn, marked. The
    /// turn it starts ends in a hook that finds the mark in the transcript, so it prints
    /// nothing more and tells the server the batch arrived.
    #[tokio::test]
    async fn reports_wake_an_agent_at_rest_through_its_inbox() {
        use serde_json::json;
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let (mut peer, session, record, posted, kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &[]).await;

        let context =
            "<slopty-reports project=\"demo\">\ntask 2: done\n  Merged.\n</slopty-reports>";
        let deliver = FromServer::Deliver { session, batch: 9, context: context.to_owned() };
        peer.tx.send(&deliver).await.unwrap();
        peer.heard(|m| {
            matches!(m, ToServer::Report(AgentReport::Delivered { session: s, batch: 9 }) if *s == session)
        })
        .await;
        let got = inbox_notes(&posted, 1).await;
        let [note] = got.as_slice() else { panic!("one message: {got:?}") };
        let content = note["content"].as_str().unwrap();
        assert!(
            content.starts_with("<slopty-reports delivery=\"")
                && content
                    .ends_with(" project=\"demo\">\ntask 2: done\n  Merged.\n</slopty-reports>"),
            "the batch, marked: {content}"
        );
        assert_eq!(
            (note["priority"].as_str(), note["authed"].as_bool()),
            (Some("next"), Some(true))
        );
        assert_eq!(note["turn"], json!([]), "its turn's hook found it read and printed nothing");
        assert!(!kept.join(format!("{session}.json")).exists(), "nothing left for the hooks");
        let untouched = recorded(&record, |_| true).await;
        assert_eq!(untouched["typed"], json!([]), "nothing is typed into the agent");
    }

    /// A session that holds what arrives on its inbox (`crossSessionInbound: "hold"`, or one
    /// that bypasses prompts) never reads the post, and Claude Code says nothing back. The batch
    /// is not taken as read: it waits, and the next prompt's hook hands it over and acks it.
    #[tokio::test]
    async fn reports_an_inbox_held_wait_for_the_next_hook() {
        use serde_json::json;
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("prompted");
        let transcript = dir.path().join("transcript.jsonl").to_string_lossy().into_owned();
        let prompt = json!([{
            "hook_event_name": "UserPromptSubmit",
            "prompt": "go on",
            "transcript_path": transcript,
        }]);
        let more = [
            ("STUB_INBOX_HOLD", "1".to_owned()),
            ("STUB_LATER", prompt.to_string()),
            ("STUB_LATER_AFTER", gate.to_string_lossy().into_owned()),
        ];
        let (mut peer, session, record, posted, kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &more).await;

        let context = "<slopty-reports project=\"demo\">\ntask 2: stuck\n</slopty-reports>";
        let deliver = FromServer::Deliver { session, batch: 4, context: context.to_owned() };
        peer.tx.send(&deliver).await.unwrap();
        let got = inbox_notes(&posted, 1).await;
        assert_eq!(got[0]["held"], true, "{got:?}");
        assert!(kept.join(format!("{session}.json")).exists(), "the batch still waits");

        std::fs::write(&gate, b"").unwrap();
        let prompted =
            recorded(&record, |r| r["hooks"].as_array().is_some_and(|h| h.len() == 2)).await;
        let turn = &prompted["hooks"][1];
        let [handed] = turn["outputs"].as_array().unwrap().as_slice() else { panic!("{turn}") };
        assert_eq!(handed["hookSpecificOutput"]["additionalContext"], context);
        peer.heard(|m| {
            matches!(m, ToServer::Report(AgentReport::Delivered { session: s, batch: 4 }) if *s == session)
        })
        .await;
        assert!(!kept.join(format!("{session}.json")).exists(), "handed over once");
    }

    /// An agent the worker starts inherits nothing of the environment the test runs in: not
    /// the developer's Claude Code settings or credentials, not the Slopty terminal the test
    /// may run inside, not the build's own variables. The daemons start from a clean one
    /// (`slopty_testkit::env::scrub`), and the agent from theirs.
    #[tokio::test]
    async fn an_agent_the_worker_starts_inherits_nothing_of_the_test_s_environment() {
        let dir = tempfile::tempdir().unwrap();
        let (_peer, _session, record, _posted, _kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &[]).await;
        let started = recorded(&record, |r| r["inherited"].is_object()).await;
        let inherited: std::collections::BTreeMap<String, u64> =
            serde_json::from_value(started["inherited"].clone()).unwrap();
        assert!(inherited.contains_key("STUB_RECORD"), "what the start gave it: {inherited:?}");
        let leaked = slopty_testkit::env::leaked(&inherited);
        assert!(leaked.is_empty(), "the test's own environment leaked into the agent: {leaked:?}");
    }

    /// The batch kept for `session` in `kept` once it is `batch`.
    async fn kept_batch(kept: &Path, session: SessionId, batch: u64) {
        let looking = async {
            while slopty_agent::reports::peek(kept, session).ok().flatten().map(|b| b.batch)
                != Some(batch)
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        tokio::time::timeout(STEP, looking).await.expect("the worker keeps the batch");
    }

    /// Reports go to an agent's inbox only when it could take typed input: not while it waits
    /// on a prompt that is the person's to answer, whose answer the post could otherwise land
    /// in the middle of. Those batches wait for its hooks; once the prompt is answered, the
    /// next batch is posted at once.
    #[tokio::test]
    async fn reports_are_not_posted_while_the_agent_waits_on_the_person() {
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let (mut peer, session, _record, posted, kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &[]).await;
        let asking = json!({ "hook_event_name": "PermissionRequest", "tool_name": "Bash" });
        hook(dir.path(), session, asking).await;
        let context = |n: u64| {
            format!("<slopty-reports project=\"demo\">\ntask {n}: done\n</slopty-reports>")
        };
        // The link takes one message at a time, so the batch after one is kept only once the
        // one before was judged.
        for batch in [5, 6] {
            let deliver = FromServer::Deliver { session, batch, context: context(batch) };
            peer.tx.send(&deliver).await.unwrap();
            kept_batch(&kept, session, batch).await;
        }
        let ran = json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" });
        hook(dir.path(), session, ran).await;
        let deliver = FromServer::Deliver { session, batch: 7, context: context(7) };
        peer.tx.send(&deliver).await.unwrap();
        let got = inbox_notes(&posted, 1).await;
        let [note] = got.as_slice() else { panic!("one message: {got:?}") };
        let content = note["content"].as_str().unwrap();
        assert!(content.contains("task 7: done"), "only the batch after the answer: {content}");
    }

    /// Reports do not start an agent the person just stopped: after their Esc (the transcript's
    /// interrupt record, the only word of it), what arrives is kept for the hooks and not
    /// posted, even once a hook says the agent waits at its prompt, so nothing starts a turn in
    /// nobody's name. Once the person prompts again, the next batch is posted at once.
    #[tokio::test]
    async fn reports_wait_for_the_person_after_they_stop_the_agent() {
        use serde_json::json;
        use slopty_proto::thread::Phase;

        let dir = tempfile::tempdir().unwrap();
        let (mut peer, session, _record, posted, kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &[]).await;
        let transcript = dir.path().join("transcript.jsonl");
        let prompt = json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "go",
            "transcript_path": transcript,
        });
        hook(dir.path(), session, prompt.clone()).await;
        peer.heard(|m| row_at(m, session, |r| r.status.phase == Phase::Working)).await;
        peer.heard.clear();
        let stop = json!({
            "type": "user", "uuid": "esc", "timestamp": "2026-10-04T07:00:00.000Z",
            "message": { "role": "user", "content": "[Request interrupted by user]" },
        });
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(&transcript).unwrap();
        std::io::Write::write_all(&mut file, format!("{stop}\n").as_bytes()).unwrap();
        // The thread rests once the transcript says the person stopped it.
        peer.heard(|m| row_at(m, session, |r| r.status.phase == Phase::Idle)).await;
        // A minute on, Claude Code says it waits at its prompt: a hook, but not the person.
        let waiting = json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt",
            "transcript_path": transcript,
        });
        hook(dir.path(), session, waiting).await;

        let context = |n: u64| {
            format!("<slopty-reports project=\"demo\">\ntask {n}: done\n</slopty-reports>")
        };
        // The link takes one message at a time, so the batch after one is kept only once the
        // one before was judged.
        for batch in [5, 6] {
            let deliver = FromServer::Deliver { session, batch, context: context(batch) };
            peer.tx.send(&deliver).await.unwrap();
            kept_batch(&kept, session, batch).await;
        }
        hook(dir.path(), session, prompt).await;
        let deliver = FromServer::Deliver { session, batch: 7, context: context(7) };
        peer.tx.send(&deliver).await.unwrap();
        let got = inbox_notes(&posted, 1).await;
        let [note] = got.as_slice() else { panic!("one message: {got:?}") };
        let content = note["content"].as_str().unwrap();
        assert!(content.contains("task 7: done"), "only the batch after the prompt: {content}");
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
    /// Codex task's does. Asked again under the same name it reopens that one as it is; with no
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

        let opened = peer.ask(opening(Some(&clone))).await;
        assert!(matches!(opened, Outcome::Opened(_)), "{opened:?}");
        let tree = std::fs::canonicalize(&clone).unwrap().join(".claude/worktrees/slopty-demo-1");
        assert_eq!(opened_in().await, tree.to_string_lossy());
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]), main, "from main");
        assert_eq!(git(&tree, &["branch", "--show-current"]), "worktree-slopty-demo-1");
        assert_eq!(git(&clone, &["branch", "--show-current"]), "feature", "the clone untouched");

        git(&tree, &["commit", "-q", "--allow-empty", "-m", "the task's work"]);
        let work = git(&tree, &["rev-parse", "HEAD"]);
        std::fs::remove_file(&record).unwrap();
        let again = peer.ask(opening(Some(&clone))).await;
        assert!(matches!(again, Outcome::Opened(_)), "{again:?}");
        assert_eq!(opened_in().await, tree.to_string_lossy());
        assert_eq!(git(&tree, &["rev-parse", "HEAD"]), work, "reopened as is");

        let nowhere = peer.ask(opening(None)).await;
        assert!(matches!(nowhere, Outcome::Error { code: ErrorCode::Invalid, .. }), "{nowhere:?}");
    }

    /// A kept batch is its own session's to take, by the token the worker made for it: asked
    /// for or acknowledged under another session's token, nothing is handed over or dropped.
    /// Under its own it is handed over, and dropped only once the hook says it printed it.
    #[tokio::test]
    async fn reports_are_handed_only_to_their_own_session_by_its_token() {
        use serde_json::json;
        use slopty_proto::ctl::{CtlReply, CtlRequest, ReportsAsk};
        use slopty_proto::project::AgentReport;

        let dir = tempfile::tempdir().unwrap();
        let more = [("STUB_INBOX_HOLD", "1".to_owned())];
        let (mut peer, session, _record, _posted, kept, _daemons) =
            resting_agent_with_an_inbox(dir.path(), &more).await;
        let context = "<slopty-reports project=\"demo\">\ntask 3: done\n</slopty-reports>";
        let deliver = FromServer::Deliver { session, batch: 3, context: context.to_owned() };
        peer.tx.send(&deliver).await.unwrap();
        kept_batch(&kept, session, 3).await;

        let key = slopty_agent::vouch::SessionKey::load_or_make(&dir.path().join("data")).unwrap();
        let (own, other) = (key.token(session), key.token(SessionId::new()));
        let payload = json!({ "hook_event_name": "UserPromptSubmit", "prompt": "go on" });
        let ask = |token: &str| {
            CtlRequest::Reports(ReportsAsk {
                session,
                token: token.to_owned(),
                payload: payload.to_string(),
                inbox: None,
            })
        };
        let handed =
            |token: &str| CtlRequest::ReportsHanded { session, token: token.to_owned(), batch: 3 };

        let refused = ctl(dir.path(), &ask(&other)).await;
        assert!(matches!(refused, CtlReply::Error { .. }), "{refused:?}");
        let refused = ctl(dir.path(), &handed(&other)).await;
        assert!(matches!(refused, CtlReply::Error { .. }), "{refused:?}");
        let refused = ctl(dir.path(), &handed("")).await;
        assert!(matches!(refused, CtlReply::Error { .. }), "{refused:?}");
        assert_eq!(slopty_agent::reports::peek(&kept, session).unwrap().map(|b| b.batch), Some(3));

        let CtlReply::Reports { batch: Some(3), print: Some(print) } =
            ctl(dir.path(), &ask(&own)).await
        else {
            panic!("the batch, to print")
        };
        let print: serde_json::Value = serde_json::from_str(&print).unwrap();
        assert_eq!(print["hookSpecificOutput"]["additionalContext"], context);
        assert!(kept.join(format!("{session}.json")).exists(), "kept until the hook printed it");
        assert_eq!(ctl(dir.path(), &handed(&own)).await, CtlReply::Ok { changed: true });
        peer.heard(|m| {
            matches!(m, ToServer::Report(AgentReport::Delivered { session: s, batch: 3 }) if *s == session)
        })
        .await;
        assert!(!kept.join(format!("{session}.json")).exists(), "handed over once");
        assert_eq!(ctl(dir.path(), &handed(&own)).await, CtlReply::Ok { changed: false });
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
        assert_eq!(reg.caps.os, Os::MacOs);
        assert!(reg.caps.cpus > 0 && reg.caps.memory > 0, "{:?}", reg.caps);
        let worker = reg.worker;

        let Outcome::Opened(term) = peer.ask(open(worker, dir.path())).await else {
            panic!("the terminal opens");
        };
        assert_eq!(term.worker, worker);
        // The server hears of the terminal before the answer, so a caller naming it next (a
        // project's orchestrator) finds it.
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
}
