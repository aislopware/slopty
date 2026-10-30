//! ptyd + worker + real client links on this machine: Claude Code as its own TUI in a Slopty
//! terminal, read as a thread (`docs/decisions/agents.md`, "TUI-first"). A start opens the
//! terminal and names it on the thread; what the TUI writes to its transcript reaches the thread
//! as the kernel reports the write; its hooks give the phase; words typed from the prompt box go
//! as a paste with the Enter apart, and wait while the agent asks the person at a dialog; a
//! `/clear` moves the terminal to the next thread; a worker started again reads the thread again
//! from the terminal ptyd kept. Claude Code is the stand-in (`slopty-stub-claude`,
//! `slopty_testkit::claude_tui`) first on the daemons' `PATH`, whose record says how it was
//! started, what it read of its terminal and which hooks it ran.
//!
//! Each step waits for what it means to arrive, on no clock of its own: a runner that held their
//! processes for 40 s only made these tests slower, and a test that never gets there is ended by
//! nextest's slow-timeout (docs/decisions/testing.md, "A test waits on the event it means").
//! Nothing here is Apple's: it runs on Linux too, wherever ptyd and the worker run.

#[cfg(test)]
mod threads {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_proto::thread::wire::{
        Expanded, IntentDone, NewWorktree, Outcome, PastSessions, Start, ThreadFrame,
        ThreadRequest, ThreadRow,
    };
    use slopty_proto::thread::{
        AgentId, ContentRef, Cursor, Drive, IntentId, ItemBody, Liveness, Phase, TableState,
        ThreadId, ThreadMeta, ThreadState, TurnState,
    };
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    /// A binary of this build (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), name)
    }

    /// The daemons, killed with the test; their pasteboard released.
    struct Daemons {
        children: Vec<Child>,
        pasteboard: String,
        addr: SocketAddr,
        dir: PathBuf,
        /// What each daemon has over its scrubbed environment.
        env: Vec<(String, std::ffi::OsString)>,
    }

    impl Daemons {
        /// The worker killed outright, as a crash or an update's kill ends it, and a new one
        /// started on the same ptyd and data: its address from then on.
        async fn restart_worker(&mut self) {
            let mut old = self.children.split_off(1);
            let ended = slopty_testkit::group::end(&mut old, Child::id, |child| {
                child.try_wait().is_ok_and(|status| status.is_some())
            });
            assert!(ended, "the old worker's group is gone");
            let env: Vec<(&str, std::ffi::OsString)> =
                self.env.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            let (worker, addr) = worker_on(&self.dir, &env, &self.pasteboard).await;
            self.children.push(worker);
            self.addr = addr;
        }
    }

    impl Drop for Daemons {
        fn drop(&mut self) {
            // Each daemon leads a process group of its own: ended whole and waited for, nothing
            // it started outlives the test holding its output (`slopty_testkit::group`).
            let ended = slopty_testkit::group::end(&mut self.children, Child::id, |child| {
                child.try_wait().is_ok_and(|status| status.is_some())
            });
            debug_assert!(ended, "a daemon's group outlived the test");
            // Off macOS the worker's clipboard is its own and goes with it.
            #[cfg(target_os = "macos")]
            slopty_input::MacBoard::named(&self.pasteboard).release();
        }
    }

    /// `program`, started from a clean environment with its home at `home`
    /// (`slopty_testkit::env::scrub`): nothing of the developer's reaches it.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

    /// ptyd and a worker on `dir`, whose home is `dir`.
    async fn daemons(dir: &Path) -> Daemons {
        daemons_with(dir, &[]).await
    }

    /// [`daemons`], each with `env` over its scrubbed environment.
    async fn daemons_with(dir: &Path, env: &[(&str, std::ffi::OsString)]) -> Daemons {
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), dir)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(ptyd.id());
        while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let pasteboard = format!("dev.aislopware.slopty.threads.{leaf}");
        let (worker, addr) = worker_on(dir, env, &pasteboard).await;
        let env = env.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
        Daemons { children: vec![ptyd, worker], pasteboard, addr, dir: dir.to_path_buf(), env }
    }

    /// A worker on `dir`'s ptyd and data, with `env` over its scrubbed environment, and the
    /// address it listens on.
    async fn worker_on(
        dir: &Path,
        env: &[(&str, std::ffi::OsString)],
        pasteboard: &str,
    ) -> (Child, SocketAddr) {
        let mut worker = scrubbed(bin("slopty-worker"), dir)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .arg("--ptyd-socket")
            .arg(dir.join("ptyd.sock"))
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0"])
            .env("SLOPTY_PASTEBOARD", pasteboard)
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(worker.id());
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        BufReader::new(stdout).read_line(&mut line).await.unwrap();
        assert!(!line.is_empty(), "the worker exited before it printed its address");
        let bound: SocketAddr = line.trim().parse().unwrap();
        let addr = if bound.ip().is_unspecified() {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()))
        } else {
            bound
        };
        (worker, addr)
    }

    /// One client: its link, and its copies of the table and of the thread it follows, kept
    /// from nothing but what the link brings.
    struct Client {
        endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        table: TableState,
        thread: Option<ThreadState>,
        cursor: Option<Cursor>,
        /// What each expand brought, in order.
        expanded: Vec<(ContentRef, Expanded)>,
    }

    impl Client {
        /// Client `id` connected to the daemons, the thread table followed.
        async fn connect(daemons: &Daemons, id: ClientId) -> Self {
            let endpoint = bind_client().unwrap();
            let hello = Hello { client: id, name: "threads".to_owned() };
            let dialing = async {
                loop {
                    match connect_addr(&endpoint, daemons.addr, hello.clone()).await {
                        Err(slopty_net::NetError::NoAnswer(_)) => {}
                        other => break other,
                    }
                }
            };
            let conn = dialing.await.unwrap();
            let mut link = WorkerLink::start(conn);
            let events = link.events().unwrap();
            let client = Self {
                endpoint,
                link,
                events,
                table: TableState::default(),
                thread: None,
                cursor: None,
                expanded: Vec::new(),
            };
            client.send(ThreadRequest::Table { have: None }).await;
            client
        }

        async fn send(&self, req: ThreadRequest) {
            self.link.send(ClientMsg::Thread(req)).await.unwrap();
        }

        /// `req` for terminal `session`.
        async fn term(&self, session: SessionId, req: TermRequest) {
            self.link.send(ClientMsg::Term { session, req }).await.unwrap();
        }

        /// Type `words` and Enter into terminal `session`, as the person does in the TUI.
        async fn typed(&self, session: SessionId, words: &str) {
            self.term(session, TermRequest::Raw(format!("{words}\r").into_bytes())).await;
        }

        /// End this client's connection, as a client that quits does.
        async fn leave(self) {
            self.endpoint.close(0_u32.into(), b"done");
            let _drained =
                tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
        }

        /// Follow `thread` from nothing, until its first state is here.
        async fn follow(&mut self, thread: ThreadId) {
            self.thread = None;
            self.cursor = None;
            self.send(ThreadRequest::Follow { thread, have: None, turns: 100, max_latency_ms: 0 })
                .await;
            self.until(|c| c.thread.is_some()).await;
        }

        /// The next event, its thread and table frames taken into the copies; a control
        /// message is handed back.
        async fn step(&mut self) -> Option<WorkerMsg> {
            let event = self.events.recv().await.expect("the link's events go on");
            match event {
                LinkEvent::Thread { frame, .. } => self.take(frame),
                LinkEvent::Control(WorkerMsg::Threads(frame)) => self.table.apply(&frame),
                LinkEvent::Control(msg) => return Some(msg),
                LinkEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
            None
        }

        fn take(&mut self, frame: ThreadFrame) {
            match frame {
                ThreadFrame::Snapshot { cursor, state } => {
                    self.thread = Some(*state);
                    self.cursor = Some(cursor);
                }
                ThreadFrame::Actions { epoch, first, next, actions } => {
                    let Some(at) = self.cursor else { return };
                    assert_eq!((epoch, first), (at.epoch, at.seq), "each frame follows on");
                    let state = self.thread.as_mut().expect("actions follow a state");
                    for action in &actions {
                        state.apply(action);
                    }
                    self.cursor = Some(Cursor { epoch, seq: next });
                }
                ThreadFrame::Expanded { content, body } => self.expanded.push((content, body)),
                _ => {}
            }
        }

        /// Take events until `done` holds of the copies.
        async fn until(&mut self, done: impl Fn(&Self) -> bool) {
            while !done(self) {
                let _control = self.step().await;
            }
        }

        /// Take events until a control message `pick` takes.
        async fn heard<T>(&mut self, mut pick: impl FnMut(WorkerMsg) -> Option<T>) -> T {
            loop {
                if let Some(found) = self.step().await.and_then(&mut pick) {
                    return found;
                }
            }
        }

        /// Take events until thread `thread`'s row is there and `holds` of it.
        async fn row_until(&mut self, thread: ThreadId, holds: impl Fn(&ThreadRow) -> bool) {
            self.until(|c| c.table.rows.get(&thread).is_some_and(&holds)).await;
        }

        fn state(&self) -> &ThreadState {
            self.thread.as_ref().expect("following")
        }
    }

    /// Where the stand-in run in terminal `terminal` keeps its record, under `root`.
    fn record_path(root: &Path, terminal: SessionId) -> PathBuf {
        root.join("records").join(format!("{terminal}.json"))
    }

    /// What the stand-in run in terminal `terminal` recorded, once `holds` of it.
    async fn record_of(
        root: &Path,
        terminal: SessionId,
        holds: impl Fn(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let at = record_path(root, terminal);
        loop {
            let seen = std::fs::read(&at)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if let Some(seen) = seen.filter(|r| holds(r)) {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The person's `claude` first on the `PATH` of daemons whose home is `root`, each with
    /// `env` besides: the stand-in, keeping its record under `root/records` by its terminal.
    async fn with_claude(root: &Path, env: &[(&str, &str)]) -> Daemons {
        with_claude_as(root, &bin("slopty-stub-claude"), env).await
    }

    /// [`with_claude`], with `program` as the `claude` on the `PATH`.
    async fn with_claude_as(root: &Path, program: &Path, env: &[(&str, &str)]) -> Daemons {
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(program, programs.join("claude")).unwrap();
        let record = root.join("records").join(concat!("{", "session", "}", ".json"));
        let mut all = vec![
            ("PATH", slopty_testkit::env::path_with(&programs)),
            ("STUB_RECORD", record.into_os_string()),
        ];
        all.extend(env.iter().map(|(k, v)| (*k, std::ffi::OsString::from(v))));
        daemons_with(root, &all).await
    }

    /// A Claude Code start in `cwd`, with `prompt` as its first message.
    fn claude_in(cwd: &Path, prompt: Option<&str>) -> Start {
        Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: cwd.to_string_lossy().into_owned(),
            drive: None,
            prompt: prompt.map(str::to_owned),
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: None,
        }
    }

    /// The terminal a start the person asked for as intent `id` opens.
    fn terminal_of(id: IntentId) -> SessionId {
        SessionId::from_uuid(*id.as_uuid())
    }

    /// Ask `a` for `start`: the intent, and how it ended.
    async fn asked(a: &mut Client, start: Start) -> (IntentId, Outcome) {
        let id = IntentId::new();
        a.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
        let outcome = a
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await;
        (id, outcome)
    }

    /// Start `start` from `a`: the thread it named, and its terminal.
    async fn started(a: &mut Client, start: Start) -> (ThreadId, SessionId) {
        let (id, outcome) = asked(a, start).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        (thread, terminal_of(id))
    }

    /// Whether the followed thread has `n` turns, the last one done.
    fn turns_done(n: usize) -> impl Fn(&Client) -> bool {
        move |c: &Client| {
            c.thread.as_ref().is_some_and(|s| {
                s.turns.len() == n && s.turns.last().is_some_and(|t| t.state == TurnState::Complete)
            })
        }
    }

    /// The texts of `state`'s items, the person's and the agent's, in order.
    fn said(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::User(user) => Some(user.text.text.clone()),
                ItemBody::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The person's prompts in `state`.
    fn prompts(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::User(user) => Some(user.text.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// The entries of the stand-in's record list `list`.
    fn listed<'a>(record: &'a serde_json::Value, list: &str) -> Vec<&'a serde_json::Value> {
        record[list].as_array().into_iter().flatten().collect()
    }

    /// The hooks of event `event` the stand-in ran for session `session`, each with how many
    /// entries ran.
    fn hooks_ran(record: &serde_json::Value, event: &str, session: &str) -> Vec<u64> {
        listed(record, "hooks")
            .into_iter()
            .filter(|h| h["event"] == event && h["session"] == session)
            .map(|h| h["ran"].as_u64().unwrap_or_default())
            .collect()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "a hook is stamped on the wall clock in microseconds, finer than `WallMs`"
    )]
    fn now_us() -> u64 {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        u64::try_from(now.as_micros()).unwrap()
    }

    /// A Claude Code start opens the person's own `claude` as a TUI in a terminal ptyd keeps,
    /// and its thread names that terminal. It runs on the session Slopty named, with Slopty's
    /// tools and hooks, the first prompt as its own argument, and nothing of the Claude Code a
    /// worker may itself run under. Its first turn reaches the thread from its transcript, and
    /// its hooks give the phase. Each record the TUI writes reaches the thread in well under the
    /// second a look takes.
    #[tokio::test]
    async fn a_claude_code_start_is_its_tui_in_a_terminal_its_thread_follows() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        // A worker run inside a Claude Code session has its markers: none reaches the agent.
        let daemons =
            with_claude(&root, &[("CLAUDECODE", "1"), ("CLAUDE_CODE_ENTRYPOINT", "cli")]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (thread, terminal) = started(&mut a, claude_in(&root, Some("Fix the login"))).await;
        a.row_until(thread, |row| row.facts.contains_key(ThreadMeta::TERMINAL_FACT)).await;
        let row = &a.table.rows[&thread];
        assert_eq!(row.terminal(), Some(terminal), "the thread names its terminal");
        assert_eq!(row.drive, Drive::named(Drive::OBSERVED));

        a.follow(thread).await;
        a.until(turns_done(1)).await;
        assert_eq!(said(a.state()), ["Fix the login", "Done: Fix the login"]);
        a.until(|c| c.state().status.phase == Phase::Done).await;

        // The stand-in notes a hook once the relay has run, which may be after the worker took it.
        let seen =
            record_of(&root, terminal, |r| listed(r, "hooks").iter().any(|h| h["event"] == "Stop"))
                .await;
        let args: Vec<&str> = listed(&seen, "args").iter().filter_map(|a| a.as_str()).collect();
        let at = args.iter().position(|a| *a == "--session-id").expect("a session id");
        let native = args[at + 1];
        assert_eq!(a.state().meta.native, native);
        assert_eq!(thread, slopty_agent::claude::thread_of(native), "named by its session");
        assert_eq!(args[args.len() - 2..], ["--", "Fix the login"], "the prompt is its own");
        assert!(args.iter().any(|a| a.starts_with("--mcp-config")), "Slopty's tools: {args:?}");
        let settings = args.iter().position(|a| *a == "--settings").expect("Slopty's hooks");
        let settings: serde_json::Value = serde_json::from_str(args[settings + 1]).unwrap();
        for event in slopty_agent::hooks::EVENTS {
            let relay = &settings["hooks"][event][0]["hooks"][0];
            assert_eq!(relay["args"], serde_json::json!(["hook"]), "{event}: {settings}");
            assert!(relay["command"].as_str().unwrap().ends_with("/slopty"), "{relay}");
        }
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref());
        assert_eq!(seen["seat"], terminal.to_string(), "it runs in its terminal");
        let env = seen["env"].as_object().unwrap();
        for marker in ["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"] {
            assert!(!env.contains_key(marker), "{marker} reached the agent");
        }
        for (event, ran) in [("SessionStart", 1), ("UserPromptSubmit", 1), ("Stop", 1)] {
            assert_eq!(hooks_ran(&seen, event, native), [ran], "{event} relayed");
        }

        // Typed in, as the person types.
        a.typed(terminal, "Second").await;
        a.until(turns_done(2)).await;
        assert_eq!(prompts(a.state()), ["Fix the login", "Second"]);

        // Each record written reaches the thread at once.
        let mut late = Vec::new();
        for (n, words) in ["Third", "Fourth", "Fifth"].into_iter().enumerate() {
            a.typed(terminal, words).await;
            a.until(|c| prompts(c.state()).last().map(String::as_str) == Some(words)).await;
            let arrived = now_us();
            // The stand-in saves its record at its hooks, so the write is waited for.
            let user = |r: &serde_json::Value| {
                listed(r, "wrote").into_iter().filter(|w| w["type"] == "user").nth(n + 2).cloned()
            };
            let seen = record_of(&root, terminal, |r| user(r).is_some()).await;
            let wrote = user(&seen).and_then(|w| w["at_us"].as_u64()).unwrap();
            late.push(Duration::from_micros(arrived.saturating_sub(wrote)));
            a.until(turns_done(n + 3)).await;
        }
        println!("a transcript record reached the client {late:?} after it was written");
        assert!(late.iter().all(|l| *l < Duration::from_millis(500)), "{late:?}");
    }

    /// The TUI's prompt box shows on its thread as Slopty's mod relays it from a Claude Code
    /// Slopty started (the stand-in runs the relay the worker wrote into the mod it loads): the
    /// words as typed; a picture pasted into it, read from Claude Code's own store under the
    /// temporary root the session has, with its bytes a press away; nothing once the box is sent,
    /// and nothing once the session ends with words left in it.
    #[tokio::test]
    async fn the_prompt_box_shows_on_its_thread_with_the_pictures_pasted_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let tmp = root.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let picture = root.join("red.png");
        std::fs::write(&picture, png()).unwrap();
        let daemons = with_claude(&root, &[("CLAUDE_CODE_TMPDIR", tmp.to_str().unwrap())]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (thread, terminal) = started(&mut a, claude_in(&root, None)).await;
        a.follow(thread).await;
        let seen = record_of(&root, terminal, |r| !listed(r, "hooks").is_empty()).await;
        let args: Vec<&str> = listed(&seen, "args").iter().filter_map(|a| a.as_str()).collect();
        let module = args.iter().find_map(|a| a.strip_prefix("--plugin-dir=")).expect("the mod");
        assert!(
            Path::new(module).starts_with(root.join("data")),
            "under the worker's data: {module}"
        );

        a.term(terminal, TermRequest::Raw(b"Fix the header".to_vec())).await;
        a.until(|c| c.state().draft.as_ref().is_some_and(|d| d.text == "Fix the header")).await;
        let text = picture.to_string_lossy().into_owned();
        a.term(terminal, TermRequest::Paste { text, confirmed: true }).await;
        let shown = "Fix the header[Image #1]";
        a.until(|c| c.state().draft.as_ref().is_some_and(|d| d.text == shown)).await;
        let draft = a.state().draft.clone().unwrap();
        let [pasted] = draft.images.as_slice() else { panic!("{draft:?}") };
        assert_eq!(pasted.n, 1);
        assert_eq!(pasted.image.media_type, "image/png");
        assert_eq!((pasted.image.width, pasted.image.height), (1, 1));
        let session = &a.state().meta.native;
        let tmpdir = Some(tmp.to_str().unwrap());
        let store = slopty_agent::claude::draft::images_dir(tmpdir, uid(&picture), &root, session);
        assert!(store.join("1.png").is_file(), "kept where Claude Code keeps it");
        let content = pasted.image.at.clone();
        a.send(ThreadRequest::Expand { thread, content: content.clone() }).await;
        a.until(|c| !c.expanded.is_empty()).await;
        assert_eq!(a.expanded, [(content, Expanded::Bytes(png()))], "its bytes, a press away");

        a.term(terminal, TermRequest::Raw(b"\r".to_vec())).await;
        a.until(|c| c.state().draft.is_none()).await;
        a.until(turns_done(1)).await;
        assert_eq!(prompts(a.state()), [shown], "the box was sent");

        a.term(terminal, TermRequest::Raw(b"half a thought".to_vec())).await;
        a.until(|c| c.state().draft.as_ref().is_some_and(|d| d.text == "half a thought")).await;
        let relayed = record_of(&root, terminal, |r| listed(r, "drafts").len() == 4).await;
        let texts: Vec<&str> =
            listed(&relayed, "drafts").iter().filter_map(|d| d["text"].as_str()).collect();
        assert_eq!(texts, ["Fix the header", shown, "", "half a thought"], "each change, once");
        a.term(terminal, TermRequest::Close).await;
        a.row_until(thread, |row| row.status.liveness != Liveness::Live).await;
        a.until(|c| c.state().draft.is_none()).await;
    }

    /// The process's user id, as Claude Code names its temporary folder: the owner of a file it
    /// made.
    fn uid(made: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(made).unwrap().uid()
    }

    /// The smallest PNG: one pixel.
    fn png() -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(b"\x08\x06\0\0\0\x1f\x15\xc4\x89");
        bytes
    }

    /// A `/clear` in the TUI moves its terminal to the next session: that session's thread
    /// begins naming the terminal, and the one before ends, resumable, naming none.
    #[tokio::test]
    async fn a_clear_moves_its_terminal_to_the_next_thread() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (first, terminal) = started(&mut a, claude_in(&root, Some("Fix the login"))).await;
        a.follow(first).await;
        a.until(turns_done(1)).await;

        a.typed(terminal, "/clear").await;
        let seen = record_of(&root, terminal, |r| listed(r, "sessions").len() == 2).await;
        let next = slopty_agent::claude::thread_of(seen["sessions"][1].as_str().unwrap());
        a.row_until(next, |row| row.terminal() == Some(terminal)).await;
        a.row_until(first, |row| {
            row.terminal().is_none() && row.status.liveness == Liveness::Exited { resumable: true }
        })
        .await;
        let naming: Vec<ThreadId> = a
            .table
            .rows
            .values()
            .filter(|r| r.terminal() == Some(terminal))
            .map(|r| r.id)
            .collect();
        assert_eq!(naming, [next], "one thread names a terminal");

        a.follow(next).await;
        a.typed(terminal, "Now the logout").await;
        a.until(turns_done(1)).await;
        assert_eq!(said(a.state()), ["Now the logout", "Done: Now the logout"]);
    }

    /// A Claude Code typed by hand into a terminal runs with none of Slopty's hooks, and its
    /// record of itself says when a dialog holds it: its thread waits on the person for the
    /// question its transcript has open, and once the person answers there the record waits no
    /// more and the turn ends.
    #[tokio::test]
    async fn a_claude_code_typed_by_hand_waits_on_the_person_as_its_record_says() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some(root.to_string_lossy().into_owned()),
            command: vec!["/bin/sh".to_owned()],
            env: vec![("PS1".to_owned(), "$ ".to_owned())],
            title: None,
            attach: false,
        };
        a.link.send(ClientMsg::OpenSession { request: 1, spec }).await.unwrap();
        let terminal = a
            .heard(|msg| match msg {
                WorkerMsg::SessionOpened { summary, .. } => Some(summary.id),
                _ => None,
            })
            .await;
        let claude = root.join("programs").join("claude");
        let typed = format!("{}\r", claude.display());
        a.term(terminal, TermRequest::Raw(typed.into_bytes())).await;
        let runs_there = |c: &Client| {
            c.table.rows.iter().find_map(|(id, row)| {
                (row.terminal() == Some(terminal) && row.status.liveness == Liveness::Live)
                    .then_some(*id)
            })
        };
        a.until(|c| runs_there(c).is_some()).await;
        let thread = runs_there(&a).expect("its thread");

        a.term(terminal, TermRequest::Raw(b"ask Which colour?\r".to_vec())).await;
        a.row_until(thread, |row| row.status.phase == Phase::NeedsYou).await;
        let wait = a.table.rows[&thread].status.wait.clone().expect("what it waits on");
        assert_eq!((wait.kind.as_str(), wait.text.as_str()), ("question", "Which colour?"));

        a.term(terminal, TermRequest::Raw(b"Red\r".to_vec())).await;
        a.row_until(thread, |row| row.status.phase == Phase::Done).await;
        let seen = record_of(&root, terminal, |r| listed(r, "prompts").len() == 2).await;
        let ran: Vec<&serde_json::Value> = listed(&seen, "hooks");
        assert!(!ran.is_empty() && ran.iter().all(|h| h["ran"] == 0), "no hooks: {ran:?}");
    }

    /// A worker started again reads every Claude Code its terminals still run: the thread is
    /// read again from the transcript, names its terminal and is live, and the session's hooks
    /// reach the new worker, so a dialog it asks is heard.
    #[tokio::test]
    async fn a_worker_started_again_reads_the_thread_again_from_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut daemons = with_claude(&root, &[]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (thread, terminal) = started(&mut a, claude_in(&root, Some("Fix the login"))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;
        a.leave().await;

        daemons.restart_worker().await;
        let mut b = Client::connect(&daemons, ClientId::new()).await;
        b.row_until(thread, |row| {
            row.terminal() == Some(terminal) && row.status.liveness == Liveness::Live
        })
        .await;
        b.follow(thread).await;
        b.until(turns_done(1)).await;
        assert_eq!(said(b.state()), ["Fix the login", "Done: Fix the login"]);

        b.typed(terminal, "ask Red or blue?").await;
        b.row_until(thread, |row| row.status.phase == Phase::NeedsYou).await;
    }

    /// A managed launcher in `claude`'s place, as an organisation installs one: the start runs
    /// it, it runs the real client beneath it as its child, and the thread is that client's
    /// session, read as any other. Nothing else is asked of it but whether it is one.
    #[tokio::test]
    async fn a_managed_launcher_s_client_is_read_as_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let artifacts = root.join(".local/share/claude-managed/artifacts");
        let client = artifacts.join(slopty_testkit::claude_tui::VERSION).join("any-platform");
        std::fs::create_dir_all(&client).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), client.join("claude")).unwrap();
        let launched = root.join("managed.jsonl");
        let log = launched.to_str().unwrap();
        let launcher = bin("slopty-stub-managed-claude");
        let daemons = with_claude_as(&root, &launcher, &[("STUB_MANAGED_LOG", log)]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (thread, terminal) = started(&mut a, claude_in(&root, Some("Fix the login"))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;
        assert_eq!(said(a.state()), ["Fix the login", "Done: Fix the login"]);

        let seen = record_of(&root, terminal, |r| r["pid"].is_u64()).await;
        let runs: Vec<serde_json::Value> = std::fs::read_to_string(&launched)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let words = |run: &serde_json::Value| -> Vec<String> {
            listed(run, "argv").iter().filter_map(|a| a.as_str()).map(str::to_owned).collect()
        };
        let starts = |w: &[String]| w.iter().any(|a| a == "--session-id");
        let start = runs.iter().position(|r| starts(&words(r)));
        let start = start.unwrap_or_else(|| panic!("the start ran the launcher: {runs:?}"));
        assert_eq!(runs[start]["client"], seen["pid"], "the session is its client's, beneath it");
        let asked = |w: &[String]| starts(w) || w == ["--managed-help"];
        assert!(runs.iter().all(|r| asked(&words(r))), "nothing else asked: {runs:?}");
    }

    /// An agent at work keeps the worker's machine awake with nobody linked, and Esc in its
    /// terminal stops the turn, as its transcript records an interrupt with no hook: the thread
    /// is stopped, and the machine let go.
    #[tokio::test]
    async fn an_agent_at_work_keeps_the_machine_awake_until_esc_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let client = ClientId::new();
        let mut a = Client::connect(&daemons, client).await;
        let (thread, terminal) = started(&mut a, claude_in(&root, Some("hold the line"))).await;
        a.row_until(thread, |row| row.status.phase == Phase::Working).await;
        a.leave().await;
        let left = tokio::time::Instant::now();
        // Linux's logind may refuse the system's hold (a CI runner's does): the hold itself is
        // asked of macOS alone, the agent that wants it of both.
        let system = |w: &slopty_proto::ctl::Awake| w.system || !cfg!(target_vendor = "apple");
        let held = awake_until(&daemons, |w| w.clients == 0 && w.agents == 1 && system(w)).await;
        let after = held.duration_since(left);
        assert!(after <= TWO_TICKS, "held {after:?} after the client left");

        let mut b = Client::connect(&daemons, client).await;
        b.term(terminal, TermRequest::Raw(vec![0x1b])).await;
        b.row_until(thread, |row| row.status.phase == Phase::Stopped).await;
        b.leave().await;
        let let_go =
            |w: &slopty_proto::ctl::Awake| (w.clients, w.agents, w.system) == (0, 0, false);
        let released = tokio::time::timeout(TWO_TICKS, awake_until(&daemons, let_go)).await;
        assert!(released.is_ok(), "{:?}", awake(&daemons).await);
        let seen = record_of(&root, terminal, |r| !listed(r, "reads").is_empty()).await;
        let native = seen["sessions"][0].as_str().unwrap();
        assert_eq!(hooks_ran(&seen, "Stop", native), Vec::<u64>::new(), "no hook for an Esc");
    }

    /// The palette's start: no first message and a folder under the worker's home spelled
    /// with `~`, as a client that knows no home writes it. Claude Code runs in the home with
    /// nothing typed, waiting for the person, and the thread is live.
    #[tokio::test]
    async fn a_claude_code_start_with_no_prompt_runs_in_the_home_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let (thread, terminal) = started(&mut a, claude_in(Path::new("~"), None)).await;
        let seen = record_of(&root, terminal, |r| !listed(r, "hooks").is_empty()).await;
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref(), "`~` is the worker's home");
        assert!(!listed(&seen, "args").iter().any(|a| *a == "--"), "no prompt: {seen}");
        a.follow(thread).await;
        let state = a.state();
        assert_eq!(state.status.liveness, Liveness::Live);
        assert!(state.turns.is_empty(), "nothing sent: {:?}", state.turns);
        assert!(listed(&seen, "prompts").is_empty(), "nothing typed");
    }

    /// What keeps the worker's machine awake, from its control socket (`slopty worker awake`).
    async fn awake(daemons: &Daemons) -> slopty_proto::ctl::Awake {
        use tokio::io::AsyncWriteExt as _;
        let socket = daemons.dir.join("worker.sock");
        let stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let mut line = serde_json::to_vec(&slopty_proto::ctl::CtlRequest::Wake).unwrap();
        line.push(b'\n');
        wr.write_all(&line).await.unwrap();
        // The worker may close first; the reply is read below either way.
        let _closed = wr.shutdown().await;
        let mut reply = String::new();
        BufReader::new(rd).read_line(&mut reply).await.unwrap();
        match serde_json::from_str(reply.trim()).unwrap() {
            slopty_proto::ctl::CtlReply::Wake(awake) => awake,
            other => panic!("not what keeps it awake: {other:?}"),
        }
    }

    /// Ask what keeps the machine awake until `holds` says it, every 50 ms: when it did.
    async fn awake_until(
        daemons: &Daemons,
        holds: impl Fn(&slopty_proto::ctl::Awake) -> bool,
    ) -> tokio::time::Instant {
        loop {
            if holds(&awake(daemons).await) {
                return tokio::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// How often the worker looks its threads over with nothing changed (`TICK` of the daemon's
    /// `threads::awake`, private to its crate), twice: the latest a sample after a change is
    /// taken, though a change of the threads' table is looked at at once.
    const TWO_TICKS: Duration = Duration::from_secs(10);

    /// A start naming a worktree opens its agent in it: the worker makes it from the clone the
    /// start's folder is in, where Claude Code would, and the agent starts there. A start in a
    /// folder that is in no repository is refused in words, and nothing opens.
    #[tokio::test]
    async fn a_start_in_a_worktree_opens_its_agent_there() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let clone = root.join("atlas");
        std::fs::create_dir_all(&clone).unwrap();
        for args in
            [&["init", "-q", "-b", "main"][..], &["commit", "-q", "--allow-empty", "-m", "c0"]]
        {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(&clone)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(done.success(), "git {args:?}");
        }
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let start = |cwd: &Path| Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: cwd.to_string_lossy().into_owned(),
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: Some(NewWorktree::named("claude-c0ffee")),
        };
        let notes = root.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let (_, refused) = asked(&mut a, start(&notes)).await;
        assert!(
            matches!(&refused, Outcome::Refused { reason } if reason.contains("no git repository")),
            "{refused:?}"
        );

        let (id, outcome) = asked(&mut a, start(&clone)).await;
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let tree = clone.join(".claude/worktrees/claude-c0ffee");
        assert!(tree.join(".git").is_file(), "a worktree of the clone");
        let seen = record_of(&root, terminal_of(id), |r| r["cwd"].is_string()).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");
    }

    /// A start outlives the link that asked for it: its worktree's setup goes on while the
    /// client is gone, and the client back on a new link, asking the same start again, takes
    /// it up and is answered with the thread, the setup run once.
    #[tokio::test]
    async fn a_start_outlives_its_link_and_the_relinked_client_takes_it_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let daemons = with_claude(&root, &[]).await;
        let clone = root.join("atlas");
        std::fs::create_dir_all(&clone).unwrap();
        // The setup says it is under way, then waits on a pipe the test writes to once the
        // link has gone.
        let go = root.join("go");
        let made = std::process::Command::new("mkfifo").arg(&go).status().unwrap();
        assert!(made.success(), "mkfifo");
        let setup = format!(
            r#"{{"scripts":{{"setup":"echo run >> \"$SLOPTY_ROOT_PATH/runs\"\necho waiting\nread line < {}"}}}}"#,
            go.display()
        );
        std::fs::write(clone.join("conductor.json"), setup).unwrap();
        std::fs::write(clone.join(".gitignore"), "runs\n").unwrap();
        for args in
            [&["init", "-q", "-b", "main"][..], &["add", "."], &["commit", "-q", "-m", "c0"]]
        {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(&clone)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(done.success(), "git {args:?}");
        }
        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: clone.to_string_lossy().into_owned(),
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: Some(NewWorktree::named("claude-5ea7ed")),
        };
        let client = ClientId::new();
        let id = IntentId::new();
        let mut a = Client::connect(&daemons, client).await;
        a.send(ThreadRequest::Start { id, start: Box::new(start.clone()) }).await;
        a.heard(|msg| match msg {
            WorkerMsg::SettingUp { id: at, setup } if at == id => {
                setup.tail.iter().any(|line| line == "waiting").then_some(())
            }
            _ => None,
        })
        .await;
        a.leave().await;

        tokio::task::spawn_blocking(move || std::fs::write(go, "go\n")).await.unwrap().unwrap();
        let mut b = Client::connect(&daemons, client).await;
        b.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
        let outcome = b
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await;
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let runs = std::fs::read_to_string(clone.join("runs")).unwrap();
        assert_eq!(runs, "run\n", "the setup ran once, through the drop");
        let tree = clone.join(".claude/worktrees/claude-5ea7ed");
        let seen = record_of(&root, terminal_of(id), |r| r["cwd"].is_string()).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");
    }

    /// A start the worker stops during (a crash, an update's kill) is taken up by the worker
    /// that starts next, with no client asking: its setup, which never finished, runs again in
    /// the worktree, the one the old worker left running ended first, and the agent opens. The
    /// client back asks the same start and is answered with the thread, nothing run a third
    /// time.
    #[tokio::test]
    async fn a_start_the_worker_restarts_during_is_finished_by_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let mut daemons = with_claude(&root, &[]).await;
        let clone = root.join("atlas");
        std::fs::create_dir_all(&clone).unwrap();
        // The setup names its group and says it is under way, then waits on a pipe the test
        // writes to once the next worker runs it again.
        let go = root.join("go");
        let made = std::process::Command::new("mkfifo").arg(&go).status().unwrap();
        assert!(made.success(), "mkfifo");
        let setup = format!(
            r#"{{"scripts":{{"setup":"echo $$ >> \"$SLOPTY_ROOT_PATH/runs\"\necho waiting\nread line < {}"}}}}"#,
            go.display()
        );
        std::fs::write(clone.join("conductor.json"), setup).unwrap();
        std::fs::write(clone.join(".gitignore"), "runs\n").unwrap();
        for args in
            [&["init", "-q", "-b", "main"][..], &["add", "."], &["commit", "-q", "-m", "c0"]]
        {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(&clone)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(done.success(), "git {args:?}");
        }
        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: clone.to_string_lossy().into_owned(),
            drive: None,
            prompt: None,
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: Some(NewWorktree::named("claude-4e57a7")),
        };
        let client = ClientId::new();
        let id = IntentId::new();
        let mut a = Client::connect(&daemons, client).await;
        a.send(ThreadRequest::Start { id, start: Box::new(start.clone()) }).await;
        a.heard(|msg| match msg {
            WorkerMsg::SettingUp { id: at, setup } if at == id => {
                setup.tail.iter().any(|line| line == "waiting").then_some(())
            }
            _ => None,
        })
        .await;
        a.leave().await;

        daemons.restart_worker().await;
        let runs = clone.join("runs");
        let groups = loop {
            let read = std::fs::read_to_string(&runs).unwrap_or_default();
            let groups: Vec<String> = read.lines().map(str::to_owned).collect();
            if groups.len() >= 2 {
                break groups;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let alive = std::process::Command::new("kill")
            .args(["-0", &groups[0]])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "the setup the old worker left running was ended first");
        tokio::task::spawn_blocking(move || std::fs::write(go, "go\n")).await.unwrap().unwrap();

        let mut b = Client::connect(&daemons, client).await;
        b.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
        let outcome = b
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await;
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let ran = std::fs::read_to_string(&runs).unwrap();
        assert_eq!(ran.lines().count(), 2, "set up again once, by the next worker: {ran}");
        let tree = clone.join(".claude/worktrees/claude-4e57a7");
        let seen = record_of(&root, terminal_of(id), |r| r["cwd"].is_string()).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");
    }

    /// A folder's past Claude Code sessions are listed from the names and times of the
    /// transcripts under the worker's home alone, the last written first, and one Slopty keeps a
    /// thread of is named by it; an agent the worker cannot list says why in words.
    #[tokio::test]
    async fn past_sessions_are_listed_per_agent_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let project = slopty_agent::claude::sessions::project_dir(&root, &work);
        std::fs::create_dir_all(&project).unwrap();
        for (name, secs) in [("older-1", 100_u64), ("newer-2", 200)] {
            let file = std::fs::File::create(project.join(format!("{name}.jsonl"))).unwrap();
            let at = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
            file.set_modified(at).unwrap();
        }
        let daemons = daemons(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let claude = AgentId::named(AgentId::CLAUDE_CODE);
        let cwd = work.to_string_lossy().into_owned();
        let asked = ThreadRequest::Sessions {
            agent: Some(claude.clone()),
            cwd: Some(cwd.clone()),
            query: String::new(),
            limit: 10,
        };
        a.send(asked).await;
        let listed = a
            .heard(|msg| match msg {
                WorkerMsg::Sessions(listed) if listed.agent.as_ref() == Some(&claude) => {
                    Some(listed)
                }
                _ => None,
            })
            .await;
        assert_eq!(listed.absent, None);
        assert_eq!(listed.cwd.as_deref(), Some(cwd.as_str()));
        let names: Vec<&str> = listed.sessions.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(names, ["newer-2", "older-1"]);
        assert_eq!(listed.sessions[0].resume, ["--resume", "newer-2"]);

        let nobody = AgentId::named("nobody");
        let asked = ThreadRequest::Sessions {
            agent: Some(nobody.clone()),
            cwd: Some(cwd),
            query: String::new(),
            limit: 10,
        };
        a.send(asked).await;
        let listed = a
            .heard(|msg| match msg {
                WorkerMsg::Sessions(listed) if listed.agent.as_ref() == Some(&nobody) => {
                    Some(listed)
                }
                _ => None,
            })
            .await;
        assert_eq!(listed.sessions, []);
        assert!(listed.absent.is_some(), "why, in words");
    }

    /// The person's past prompts are searched across Claude Code, Codex and pi from each agent's
    /// own record under the worker's home: every word must be there, the sessions come with the
    /// prompts that matched and the words that take each up again, a line that is not JSON is
    /// passed over, and a narrower question narrows the answer.
    #[tokio::test]
    async fn past_prompts_are_found_across_agents_with_the_words_that_resume_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let w = work.to_string_lossy().into_owned();
        // Claude Code: its prompt history, a paste kept apart, and the session's transcript,
        // which is looked at and never opened.
        let claude = root.join(".claude");
        std::fs::create_dir_all(claude.join("paste-cache")).unwrap();
        std::fs::write(claude.join("paste-cache/abc1.txt"), "panic in the login handler").unwrap();
        let project = slopty_agent::claude::sessions::project_dir(&root, &work);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("c-1.jsonl"), "not read\n").unwrap();
        let history = format!(
            "{}\nnot json at all\n{}\n{}\n",
            serde_json::json!({"display": "fix the flaky login test", "pastedContents": {},
                "timestamp": 1_000, "project": w, "sessionId": "c-1"}),
            serde_json::json!({"display": "why [Pasted text #1 +4 lines]",
                "pastedContents": {"1": {"id": 1, "type": "text", "contentHash": "abc1"}},
                "timestamp": 2_000, "project": w, "sessionId": "c-2"}),
            serde_json::json!({"display": "/model opus", "pastedContents": {},
                "timestamp": 3_000, "project": w, "sessionId": "c-1"}),
        );
        std::fs::write(claude.join("history.jsonl"), history).unwrap();
        // Codex: its prompt history, and the rollout that says where the thread ran.
        let codex_id = "01a1009b-ce5e-7d91-b77c-80127c630fd6";
        let day = root.join(".codex/sessions/2026/10/03");
        std::fs::create_dir_all(&day).unwrap();
        let meta = serde_json::json!({"timestamp": "2026-10-03T10:00:00.000Z",
            "type": "session_meta", "payload": {"id": codex_id, "cwd": w, "source": "cli"}});
        std::fs::write(
            day.join(format!("rollout-2026-10-03T10-00-00-{codex_id}.jsonl")),
            format!("{meta}\n"),
        )
        .unwrap();
        let codex_history = serde_json::json!({"session_id": codex_id, "ts": 5, "text": "login retries for the api"});
        std::fs::write(root.join(".codex/history.jsonl"), format!("{codex_history}\n")).unwrap();
        // pi: a session file in its folder's directory.
        let pi_dir = slopty_agent::pi::sessions::folder(&root.join(".pi/agent"), &w);
        std::fs::create_dir_all(&pi_dir).unwrap();
        let pi_file = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"type": "session", "version": 3, "id": "p-1", "cwd": w}),
            serde_json::json!({"type": "message", "id": "1", "message": {"role": "user",
                "content": [{"type": "text", "text": "Login page layout"}], "timestamp": 4_000}}),
            serde_json::json!({"type": "message", "id": "2", "message": {"role": "assistant",
                "content": [{"type": "text", "text": "the login page"}], "timestamp": 4_001}}),
        );
        std::fs::write(pi_dir.join("2026-10-03T10-00-00-000Z_p-1.jsonl"), pi_file).unwrap();

        let daemons = daemons(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let search = |agent: Option<&str>, query: &str| ThreadRequest::Sessions {
            agent: agent.map(AgentId::named),
            cwd: None,
            query: query.to_owned(),
            limit: 10,
        };

        a.send(search(None, "login")).await;
        let found = answer(&mut a, "login").await;
        assert_eq!((found.absent.as_deref(), found.cut.as_deref()), (None, None));
        let mut by: Vec<(&str, &str, &[String])> = found
            .sessions
            .iter()
            .map(|s| (s.agent.0.as_str(), s.native.as_str(), s.resume.as_slice()))
            .collect();
        by.sort_unstable();
        let resume = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        let (claude_words, codex_words, pi_words) = (
            resume(&["--resume", "c-1"]),
            resume(&["resume", codex_id]),
            resume(&["--session", "p-1"]),
        );
        let mut want = vec![
            (AgentId::CLAUDE_CODE, "c-1", claude_words.as_slice()),
            (AgentId::CLAUDE_CODE, "c-2", &[][..]),
            (AgentId::CODEX, codex_id, codex_words.as_slice()),
            (AgentId::PI, "p-1", pi_words.as_slice()),
        ];
        want.sort_unstable();
        assert_eq!(by, want, "c-2 matched by its paste; it has no transcript to take up");
        for session in &found.sessions {
            assert_eq!(session.cwd.as_deref(), Some(w.as_str()), "{}", session.native);
            let hit = session.prompts.first().unwrap();
            let marked: Vec<&str> = hit
                .spans
                .iter()
                .filter_map(|s| hit.text.get(s.start as usize..s.end as usize))
                .collect();
            assert_eq!(marked.iter().map(|m| m.to_lowercase()).collect::<Vec<_>>(), ["login"]);
        }
        let pi = found.sessions.iter().find(|s| s.native == "p-1").unwrap();
        assert_eq!(pi.prompts.len(), 1, "only the person's message is a prompt");

        a.send(search(Some(AgentId::CLAUDE_CODE), "flaky LOGIN")).await;
        let found = answer(&mut a, "flaky LOGIN").await;
        assert_eq!(found.sessions, [], "a capital asks for that case");
        a.send(search(Some(AgentId::CLAUDE_CODE), "flaky login")).await;
        let found = answer(&mut a, "flaky login").await;
        let names: Vec<&str> = found.sessions.iter().map(|s| s.native.as_str()).collect();
        assert_eq!(names, ["c-1"], "every word must be there");
        assert_eq!(found.sessions[0].title.as_deref(), Some("fix the flaky login test"));

        a.send(search(None, "opus")).await;
        assert_eq!(answer(&mut a, "opus").await.sessions, [], "a command is no prompt");
        a.send(search(Some("nobody"), "login")).await;
        let found = answer(&mut a, "login").await;
        assert!(found.sessions.is_empty() && found.absent.is_some(), "why, in words");
    }

    /// The worker's answer to a search for `query`.
    async fn answer(a: &mut Client, query: &str) -> PastSessions {
        a.heard(|msg| match msg {
            WorkerMsg::Sessions(found) if found.query == query => Some(found),
            _ => None,
        })
        .await
    }
}
