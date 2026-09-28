//! The worker daemon with a server: it dials one (a test double on slopty-net's listener),
//! registers, answers the verbs the server forwards against real terminals in ptyd, passes on what
//! happens, and registers again when the server drops it.

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_core::{ClientId, WorkerId};
    use slopty_net::admission::Admission;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::framed::{FramedRecv, FramedSend};
    use slopty_net::server::{AcceptedLink, ServerListener};
    use slopty_proto::WorkerMsg;
    use slopty_proto::handshake::Hello;
    use slopty_proto::items::{ItemKind, ItemOp, ItemSync};
    use slopty_proto::orchestration::{
        ErrorCode, Input, ItemRef, Outcome, Size, TermRef, Verb, WaitUntil, Waited,
    };
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer};
    use slopty_proto::terminal::{CloseReason, SessionState};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};

    const STEP: Duration = Duration::from_secs(20);
    /// From `exit` typed to the worker announcing the exit: ptyd reaps the child and
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
            let package = if name == "slopty" { "slopty-cli" } else { name };
            build.args(["build", "-p", package, "--bin", name]);
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
    async fn daemons(dir: &Path, server: SocketAddr) -> Daemons {
        daemons_finding(dir, server, None).await
    }

    /// As [`daemons`], with `programs` searched first for a command's program.
    async fn daemons_finding(dir: &Path, server: SocketAddr, programs: Option<&Path>) -> Daemons {
        let path = programs.map(|first| {
            let rest = std::env::var_os("PATH").unwrap_or_default();
            std::env::join_paths(
                std::iter::once(first.to_owned()).chain(std::env::split_paths(&rest)),
            )
            .unwrap()
        });
        let with_path = |command: &mut Command| {
            if let Some(path) = &path {
                command.env("PATH", path);
            }
            // Inherited, as from a developer's shell: an agent the worker starts must not have
            // it, or its mod goes silent.
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
            let welcome = FromServer::Welcome { name: "fake".into() };
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
        }
    }

    fn text(s: &str) -> Input {
        Input::Text(s.to_owned())
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
    /// with its name, is renamed, is pointed at and goes. A terminal's item is refused both ways,
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
            if let WorkerMsg::Items(sync @ (ItemSync::Delta { .. } | ItemSync::Pointed { .. })) =
                msg
            {
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

        assert_eq!(peer.ask(Verb::PointAt { item }).await, Outcome::Done);
        let ItemSync::Pointed { item: at, .. } = next_items().await else { panic!("pointed") };
        assert_eq!(at, item.item);

        assert_eq!(peer.ask(Verb::RemoveItem { item }).await, Outcome::Done);
        assert!(
            matches!(next_items().await, ItemSync::Delta { op: ItemOp::Remove(id), .. } if id == item.item)
        );
        let gone = peer.ask(Verb::PointAt { item }).await;
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
        use slopty_proto::agent::{AgentKind, AgentSource};

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
        let [plugin, flag, settings, rest @ ..] = args.as_slice() else { panic!("{args:?}") };
        assert_eq!((flag.as_str(), rest), ("--settings", &["--verbose".to_owned()][..]));
        // The mod, as the worker wrote it under its data dir, in the flag's `=` form.
        let module = plugin.strip_prefix("--plugin-dir=").map(PathBuf::from).expect(plugin);
        assert!(module.starts_with(dir.path().join("data/claude-mod")), "{module:?}");
        for (path, content) in slopty_agent::claude_mod::FILES {
            assert_eq!(std::fs::read_to_string(module.join(path)).unwrap(), content, "{path}");
        }
        let settings: serde_json::Value = serde_json::from_str(settings).unwrap();
        assert_eq!(settings["model"], "haiku", "the caller's settings are kept");
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
        peer.heard(|m| {
            matches!(m, ToServer::Agent(ev) if ev.session == term.session
                && ev.kind == AgentKind::ClaudeCode && ev.source == AgentSource::Hook)
        })
        .await;
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
        assert!(reg.sessions.is_empty());
        assert_eq!(reg.caps.os, Os::MacOs);
        assert!(reg.caps.cpus > 0 && reg.caps.memory > 0, "{:?}", reg.caps);
        let worker = reg.worker;

        let Outcome::Opened(term) = peer.ask(open(worker, dir.path())).await else {
            panic!("the terminal opens");
        };
        assert_eq!(term.worker, worker);
        peer.heard(|m| matches!(m, ToServer::SessionChanged(s) if s.id == term.session)).await;

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
        let write = Verb::WriteFile { worker, path: path.clone(), bytes: b"hello".to_vec() };
        assert_eq!(peer.ask(write).await, Outcome::Done);
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
