//! ptyd + worker + real client links on this machine: a driven Claude Code thread served over
//! the link. The table and the thread are followed from a cursor, the link drops and comes back,
//! and only what it missed is sent; an intent sent again after the drop is answered as the first
//! time and acts on nothing. The worker restarts in the middle of a turn and the turn goes on.
//! Claude Code is the stand-in (`slopty-stub-claude`) first on the daemons' `PATH`, whose record
//! says how it was started and what it heard.
//!
//! Each step waits for what it means to arrive, on no clock of its own: a runner that held their
//! processes for 40 s only made these tests slower, and a test that never gets there is ended by
//! nextest's slow-timeout (docs/decisions/testing.md, "A test waits on the event it means").

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod threads {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::ClientId;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::thread::wire::{
        Expanded, Intent, IntentDone, NewWorktree, Outcome, PastSessions, Start, TableFrame,
        ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        AgentId, Cap, Cursor, IntentId, ItemBody, RequestState, TableState, ThreadId, ThreadState,
        TurnState,
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

    /// A frame as it arrived, for judging what was sent.
    #[derive(Debug, PartialEq, Eq)]
    enum Got {
        Snapshot(Cursor),
        Page { turns: usize, older: bool },
        Expanded(Expanded),
        Review(Box<slopty_proto::thread::wire::Review>),
        Actions { epoch: u64, first: u64, next: u64 },
        App(u64, slopty_proto::visual::AppAnswer),
    }

    /// One client: its link, and its copies of the table and of one thread, kept from nothing
    /// but what the link brings.
    struct Client {
        endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        table: TableState,
        tables: Vec<TableFrame>,
        thread: Option<ThreadState>,
        cursor: Option<Cursor>,
        got: Vec<Got>,
    }

    impl Client {
        /// Client `id` connected to the daemons.
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
            Self {
                endpoint,
                link,
                events,
                table: TableState::default(),
                tables: Vec::new(),
                thread: None,
                cursor: None,
                got: Vec::new(),
            }
        }

        async fn send(&self, req: ThreadRequest) {
            self.link.send(ClientMsg::Thread(req)).await.unwrap();
        }

        /// End this client's connection, as a client that quits does.
        async fn leave(self) {
            self.endpoint.close(0_u32.into(), b"done");
            let _drained =
                tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
        }

        /// Follow `thread` from where this copy stands.
        async fn follow(&self, thread: ThreadId) {
            let have = self.cursor;
            self.send(ThreadRequest::Follow { thread, have, turns: 100, max_latency_ms: 0 }).await;
        }

        /// The next event, its thread and table frames taken into the copies; a control
        /// message is handed back.
        async fn step(&mut self) -> Option<WorkerMsg> {
            let event = self.events.recv().await.expect("the link's events go on");
            match event {
                LinkEvent::Thread { frame, .. } => self.take(frame),
                LinkEvent::Control(WorkerMsg::Threads(frame)) => {
                    self.table.apply(&frame);
                    self.tables.push(frame);
                }
                LinkEvent::Control(msg) => return Some(msg),
                LinkEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
            None
        }

        fn take(&mut self, frame: ThreadFrame) {
            match frame {
                ThreadFrame::Snapshot { cursor, state } => {
                    self.got.push(Got::Snapshot(cursor));
                    self.thread = Some(*state);
                    self.cursor = Some(cursor);
                }
                ThreadFrame::Actions { epoch, first, next, actions } => {
                    self.got.push(Got::Actions { epoch, first, next });
                    let at = self.cursor.expect("actions follow a cursor");
                    assert_eq!((epoch, first), (at.epoch, at.seq), "each frame follows on");
                    let state = self.thread.as_mut().expect("actions follow a state");
                    for action in &actions {
                        state.apply(action);
                    }
                    self.cursor = Some(Cursor { epoch, seq: next });
                }
                ThreadFrame::Page(page) => {
                    self.got.push(Got::Page { turns: page.turns.len(), older: page.older });
                }
                ThreadFrame::Expanded { body, .. } => self.got.push(Got::Expanded(body)),
                ThreadFrame::Review(review) => self.got.push(Got::Review(review)),
                ThreadFrame::App { id, answer } => self.got.push(Got::App(id, answer)),
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

        async fn intent(&mut self, id: IntentId, thread: ThreadId, intent: Intent) -> Outcome {
            self.send(ThreadRequest::Intent { id, thread, intent }).await;
            self.heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await
        }

        fn state(&self) -> &ThreadState {
            self.thread.as_ref().expect("following")
        }
    }

    /// What the stand-in recorded of how it was started, once it has.
    async fn recorded(record: &Path) -> serde_json::Value {
        loop {
            if let Some(seen) =
                std::fs::read(record).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// The person's `claude` and the stand-in's record of how it started, first on the
    /// `PATH` of a worker whose home is `root`.
    async fn with_claude(root: &Path) -> (Daemons, PathBuf) {
        with_claude_and(root, &[]).await
    }

    /// [`with_claude`], the stand-in's knobs `knobs` (`slopty_testkit::claude_stream`) in the
    /// daemons' environment.
    async fn with_claude_and(root: &Path, knobs: &[(&str, &str)]) -> (Daemons, PathBuf) {
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let record = root.join("record.json");
        let mut env = vec![
            ("PATH", slopty_testkit::env::path_with(&programs)),
            ("STUB_RECORD", record.clone().into_os_string()),
        ];
        env.extend(knobs.iter().map(|(k, v)| (*k, std::ffi::OsString::from(v))));
        (daemons_with(root, &env).await, record)
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
            attachments: Vec::new(),
            args: Vec::new(),
            options: std::collections::BTreeMap::new(),
            worktree: None,
        }
    }

    /// Start `start` from `a`: the thread it named.
    async fn started(a: &mut Client, start: Start) -> ThreadId {
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
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        thread
    }

    /// The lines of `kind` (a message's type, or a control request's subtype) the stand-in
    /// heard, by its record.
    fn heard_lines(record: &serde_json::Value, kind: &str) -> Vec<serde_json::Value> {
        record["received"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["type"] == kind || l["request"]["subtype"] == kind)
            .cloned()
            .collect()
    }

    fn turns_done(n: usize) -> impl Fn(&Client) -> bool {
        move |c: &Client| {
            c.thread.as_ref().is_some_and(|s| {
                s.turns.len() == n && s.turns.last().is_some_and(|t| t.state == TurnState::Complete)
            })
        }
    }

    fn asking(c: &Client) -> bool {
        c.thread.as_ref().is_some_and(|s| s.open_requests().count() == 1)
    }

    /// A driven Claude Code thread is found in the table and followed over a real client link;
    /// the permission it asks is answered by an intent. The link drops while the agent goes on
    /// (a second client, following throughout, sees it all land), and comes back as the same
    /// client: the table and the thread resume from its cursors with only what it missed, and
    /// the intent sent again is answered as before and acts on nothing, where a new one for the
    /// same request finds it answered.
    #[tokio::test]
    async fn a_thread_resumes_from_its_cursor_and_an_intent_is_had_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let me = ClientId::new();
        let mut a = Client::connect(&daemons, me).await;
        let thread = started(&mut a, claude_in(&root, Some("bash touch made"))).await;
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        assert!(matches!(a.tables.first(), Some(TableFrame::Snapshot { .. })), "{:?}", a.tables);
        a.follow(thread).await;
        a.until(asking).await;
        assert!(matches!(a.got.first(), Some(Got::Snapshot(_))), "a first follow is a snapshot");

        let request = a.state().open_requests().next().unwrap().id.clone();
        let answer =
            Intent::Answer { ask: request.clone(), choice: "allow".to_owned(), message: None };
        let once = IntentId::new();
        assert_eq!(a.intent(once, thread, answer.clone()).await, Outcome::Done);
        a.until(turns_done(1)).await;
        assert!(
            a.state()
                .requests
                .iter()
                .any(|r| r.id == request && matches!(r.state, RequestState::Answered { .. })),
            "{:?}",
            a.state().requests
        );

        // A witness follows throughout; the link drops, and the agent goes on.
        let mut witness = Client::connect(&daemons, ClientId::new()).await;
        witness.follow(thread).await;
        // As far as `a` got, or further: a lease's stamp may land after `a` last read.
        let reached = a.cursor.unwrap();
        witness
            .until(|c| {
                c.cursor.is_some_and(|at| at.epoch == reached.epoch && at.seq >= reached.seq)
            })
            .await;
        let (cursor, table, kept) = (a.cursor.unwrap(), a.table.clone(), a.thread.clone());
        a.link.close();
        drop(a);
        let send = Intent::Send {
            text: "Say more.".to_owned(),
            delivery: slopty_proto::thread::Delivery::Steer,
            attachments: vec![],
        };
        assert_eq!(witness.intent(IntentId::new(), thread, send).await, Outcome::Done);
        witness.until(turns_done(2)).await;
        let landed = witness.cursor.unwrap();
        assert_eq!(landed.epoch, cursor.epoch, "the log went on, not over");

        // Back as the same client, from its cursors: only what it missed comes.
        let mut b = Client::connect(&daemons, me).await;
        (b.table, b.thread, b.cursor) = (table.clone(), kept, Some(cursor));
        b.send(ThreadRequest::Table { have: Some(table.cursor) }).await;
        b.until(|c| !c.tables.is_empty()).await;
        assert!(matches!(b.tables.first(), Some(TableFrame::Delta { .. })), "{:?}", b.tables);
        b.follow(thread).await;
        b.until(|c| c.cursor.is_some_and(|at| at.seq >= landed.seq)).await;
        assert!(!b.got.iter().any(|g| matches!(g, Got::Snapshot(_))), "{:?}", b.got);
        let resumed = b.got.first().unwrap();
        assert!(
            matches!(resumed, Got::Actions { epoch, first, .. } if *epoch == cursor.epoch && *first == cursor.seq),
            "resumed {resumed:?}, not from {cursor:?}"
        );
        b.until(turns_done(2)).await;
        assert!(b.table.rows.contains_key(&thread));

        // The intent again, from before the drop: its first outcome, and nothing done; a new
        // one for the same request finds it answered.
        assert_eq!(b.intent(once, thread, answer.clone()).await, Outcome::Done);
        assert_eq!(b.intent(IntentId::new(), thread, answer).await, Outcome::Done);
        let seen = recorded(&record).await;
        assert_eq!(heard_lines(&seen, "control_response").len(), 1, "acted on once");

        // Pages and expansions come on the thread's stream, between its frames.
        let last = b.state().last_turn().map(|t| t.id).unwrap();
        b.send(ThreadRequest::Page { thread, before: last, turns: 1 }).await;
        b.until(|c| c.got.iter().any(|g| matches!(g, Got::Page { .. }))).await;
        let content = slopty_proto::thread::ContentRef("nothing of this thread".to_owned());
        b.send(ThreadRequest::Expand { thread, content }).await;
        b.until(|c| c.got.contains(&Got::Expanded(Expanded::Gone))).await;
    }

    /// The worker restarts while Claude Code is in the middle of a turn: Claude Code goes on in
    /// ptyd's keeping, the next worker takes it up, reads the thread again, then what Claude
    /// Code wrote while no worker held it, and the turn ends as Claude Code ended it, run by
    /// the same process from start to end. A client back on the new worker sees it whole.
    #[tokio::test]
    async fn a_turn_survives_the_worker_restarting_mid_turn() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (mut daemons, record) = with_claude(&root).await;
        let me = ClientId::new();
        let mut a = Client::connect(&daemons, me).await;
        // Twenty deltas 200 ms apart: four seconds of a turn for the restart to land in.
        let thread = started(&mut a, claude_in(&root, Some("slow 20"))).await;
        a.follow(thread).await;
        a.until(|c| {
            c.thread.as_ref().is_some_and(|s| {
                s.turns.last().is_some_and(|t| t.state == TurnState::Active) && !s.items.is_empty()
            })
        })
        .await;
        let pid = recorded(&record).await["pid"].clone();
        a.link.close();
        drop(a);

        let restarted = std::time::Instant::now();
        daemons.restart_worker().await;
        let mut b = Client::connect(&daemons, me).await;
        b.follow(thread).await;
        b.until(turns_done(1)).await;
        println!("the turn ended {:?} after the restart began", restarted.elapsed());
        let seen = recorded(&record).await;
        assert_eq!(seen["pid"], pid, "the same Claude Code, start to end");
        assert_eq!(heard_lines(&seen, "initialize").len(), 2, "asked again by the next worker");
        assert_eq!(heard_lines(&seen, "user").len(), 1, "the message went once");
        let state = b.state();
        assert_eq!(state.turns.len(), 1, "one turn, read again whole");
        assert_eq!(state.status.liveness, slopty_proto::thread::Liveness::Live);
        let said: String = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(said, ["Slowly"; 20].join(" "), "what it said while no worker held it too");
    }

    /// A review by the agent, on the person's word, goes to Claude Code as their turn: its own
    /// `/code-review` over the change's range, sent as any command of theirs is. The thread can
    /// review because Claude Code lists `code-review`; the range is two commits the worker made
    /// of the trees the review showed.
    #[tokio::test]
    async fn a_review_by_claude_code_is_its_own_code_review_over_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let repo = root.join("repo");
        let git = |args: &[&str]| {
            let out =
                std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        let from = git(&["write-tree"]);
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        let to = git(&["write-tree"]);

        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&repo, Some("Say hello."))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;
        a.until(|c| c.thread.as_ref().is_some_and(|t| t.meta.can(Cap::REVIEW))).await;

        let review = Intent::Review {
            from: slopty_proto::thread::TreeRef(from.clone()),
            to: slopty_proto::thread::TreeRef(to.clone()),
        };
        assert_eq!(a.intent(IntentId::new(), thread, review).await, Outcome::Done);
        let (base, head) = (
            git(&["rev-parse", &format!("refs/slopty/threads/{thread}/review-base")]),
            git(&["rev-parse", &format!("refs/slopty/threads/{thread}/review-head")]),
        );
        let sent = format!("/code-review {base}...{head}");
        a.until(turns_done(2)).await;
        let seen = recorded(&record).await;
        let texts: Vec<String> = heard_lines(&seen, "user")
            .iter()
            .map(|l| match &l["message"]["content"] {
                serde_json::Value::String(text) => text.clone(),
                blocks => blocks[0]["text"].as_str().unwrap_or_default().to_owned(),
            })
            .collect();
        assert_eq!(texts, ["Say hello.".to_owned(), sent], "the review, as the person's turn");
        assert_eq!(
            (
                git(&["rev-parse", &format!("{base}^{{tree}}")]),
                git(&["rev-parse", &format!("{head}^{{tree}}")])
            ),
            (from, to)
        );
    }

    /// A Claude Code thread started from a client runs the person's own `claude` (the
    /// stand-in, first on the worker's `PATH`) driven over stream-json, on a session id chosen
    /// for it, the first message its first user message. The start is answered with the thread
    /// named by that id, which a client follows at once, and Claude Code's answer lands there.
    #[tokio::test]
    async fn a_claude_code_start_runs_claude_driven_and_names_its_thread() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&root, Some("Say hello."))).await;

        let seen = recorded(&record).await;
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
        let at = argv.iter().position(|a| *a == "--session-id").expect("a session id");
        let native = argv[at + 1];
        assert_eq!(thread, slopty_agent::claude::thread_of(native), "named by its session id");
        assert!(argv.contains(&"--input-format") && argv.contains(&"stream-json"), "{argv:?}");
        assert!(!argv.contains(&"--settings"), "no hooks: {argv:?}");
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref());

        a.follow(thread).await;
        a.until(turns_done(1)).await;
        assert_eq!(a.state().meta.native, native);
        assert!(a.state().meta.can(Cap::APPROVALS));
    }

    /// A start naming a worktree opens its agent in it: the worker makes it from the clone the
    /// start's folder is in, where Claude Code would, and the agent starts there. A start in a
    /// folder that is in no repository is refused in words, and nothing opens. Closed for good
    /// (a run the person did not keep), the thread goes and its worktree with it, the work in
    /// it not committed and all.
    #[tokio::test]
    async fn a_start_in_a_worktree_opens_its_agent_there_and_a_close_takes_it_away() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
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
            attachments: Vec::new(),
            args: Vec::new(),
            options: std::collections::BTreeMap::new(),
            worktree: Some(NewWorktree::named("claude-c0ffee")),
        };
        let answer = async |a: &mut Client, start: Start| {
            let id = IntentId::new();
            a.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
            a.heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await
        };

        let notes = root.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let refused = answer(&mut a, start(&notes)).await;
        assert!(
            matches!(&refused, Outcome::Refused { reason } if reason.contains("no git repository")),
            "{refused:?}"
        );

        let outcome = answer(&mut a, start(&clone)).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let tree = clone.join(".claude/worktrees/claude-c0ffee");
        assert!(tree.join(".git").is_file(), "a worktree of the clone");
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");

        std::fs::write(tree.join("draft.txt"), "half done").unwrap();
        let id = IntentId::new();
        a.send(ThreadRequest::Intent { id, thread, intent: Intent::Discard }).await;
        let closed = a
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await;
        assert_eq!(closed, Outcome::Done);
        assert!(!tree.exists(), "its worktree went with it");
        assert!(clone.join(".git").is_dir(), "the clone stays");
    }

    /// A start outlives the link that asked for it: its worktree's setup goes on while the
    /// client is gone, and the client back on a new link, asking the same start again, takes
    /// it up and is answered with the thread, the setup run once.
    #[tokio::test]
    async fn a_start_outlives_its_link_and_the_relinked_client_takes_it_up() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
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
            attachments: Vec::new(),
            args: Vec::new(),
            options: std::collections::BTreeMap::new(),
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
        let seen = recorded(&record).await;
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
        let (mut daemons, record) = with_claude(&root).await;
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
            attachments: Vec::new(),
            args: Vec::new(),
            options: std::collections::BTreeMap::new(),
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
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");
    }

    /// The palette's start: no first message and a folder under the worker's home spelled
    /// with `~`, as a client that knows no home writes it. Claude Code runs in the home with
    /// nothing sent, waiting for the person, and the thread is live.
    #[tokio::test]
    async fn a_claude_code_start_with_no_prompt_runs_in_the_home_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(Path::new("~"), None)).await;
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref(), "`~` is the worker's home");
        a.follow(thread).await;
        a.until(|c| c.thread.is_some()).await;
        let state = a.state();
        assert_eq!(state.status.liveness, slopty_proto::thread::Liveness::Live);
        assert!(state.turns.is_empty(), "nothing sent: {:?}", state.turns);
        let heard = heard_lines(&recorded(&record).await, "user");
        assert!(heard.is_empty(), "nothing sent: {heard:?}");
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
            let at = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
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

    /// An ACP agent's picture reaches the client by its digest: the stand-in replays `opencode`
    /// reading a PNG (`crates/slopty-agent/tests/fixtures/acp/media.jsonl`), the call carries
    /// the picture, and the client's `Expand` of it brings back the very bytes. A page's ask
    /// of that thread's agent, which has no door to its MCP servers, is refused in words.
    #[tokio::test]
    async fn an_acp_agents_picture_comes_back_whole_by_its_digest() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (programs, work) = (root.join("programs"), root.join("work"));
        for made in [&programs, &work] {
            std::fs::create_dir_all(made).unwrap();
        }
        std::os::unix::fs::symlink(bin("slopty-stub-acp"), programs.join("opencode")).unwrap();
        // The recording's prompt carried the picture; this one sends the words alone, so the
        // stand-in is given the recording with its prompt as this one goes.
        let recorded = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/slopty-agent/tests/fixtures/acp/media.jsonl"),
        )
        .unwrap();
        let mut fixture = String::new();
        for line in recorded.lines() {
            let mut line: serde_json::Value = serde_json::from_str(line).unwrap();
            if line["msg"]["method"] == "session/prompt" {
                line["msg"]["params"]["prompt"] =
                    serde_json::json!([{ "type": "text", "text": "Look at dot.png." }]);
            }
            fixture.push_str(&line.to_string());
            fixture.push('\n');
        }
        let fixture_at = root.join("media.jsonl");
        std::fs::write(&fixture_at, fixture).unwrap();
        let config =
            serde_json::json!({ "fixture": fixture_at, "record": root.join("record.json") });
        std::fs::write(programs.join("stub-acp.json"), config.to_string()).unwrap();
        let env = [("PATH", slopty_testkit::env::path_with(&programs))];
        let daemons = daemons_with(&root, &env).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;

        let mut start = claude_in(&work, Some("Look at dot.png."));
        start.agent = AgentId::named("acp:opencode");
        let thread = started(&mut a, start).await;
        a.follow(thread).await;
        let pictured = |c: &Client| {
            c.thread.as_ref().and_then(|s| {
                s.items.iter().find_map(|i| match &i.body {
                    ItemBody::Tool(call) if !call.images.is_empty() => Some(call.images.clone()),
                    _ => None,
                })
            })
        };
        a.until(|c| pictured(c).is_some()).await;
        let images = pictured(&a).unwrap();
        assert_eq!(images.len(), 1, "{images:?}");
        let image = &images[0];
        a.send(ThreadRequest::Expand { thread, content: image.at.clone() }).await;
        a.until(|c| c.got.iter().any(|g| matches!(g, Got::Expanded(_)))).await;
        let bytes = a
            .got
            .iter()
            .find_map(|g| match g {
                Got::Expanded(Expanded::Bytes(bytes)) => Some(bytes.clone()),
                _ => None,
            })
            .expect("the picture's bytes");
        assert_eq!(blake3::hash(&bytes).to_hex().to_string(), image.digest);
        assert_eq!(u64::try_from(bytes.len()).unwrap(), image.bytes);
        let kept = root.join("data/media").join(&image.digest);
        assert!(kept.is_file(), "kept in the worker's data, by its digest");

        let ask = slopty_proto::visual::AppAsk::Read {
            server: "apps".to_owned(),
            uri: "ui://apps/echo".to_owned(),
        };
        a.send(ThreadRequest::App { thread, id: 7, ask }).await;
        a.until(|c| c.got.iter().any(|g| matches!(g, Got::App(7, _)))).await;
        let refused = a.got.iter().any(|g| {
            matches!(g, Got::App(7, slopty_proto::visual::AppAnswer::Refused(why)) if !why.is_empty())
        });
        assert!(refused, "{:?}", a.got.last());
    }

    /// A page of a Claude Code thread asks its MCP App through Claude Code, which holds the
    /// connection: the app's page is read (`mcp_read_resource`) and its tool called for the page
    /// alone (`mcp_call` with `result_not_for_model`), each answer whole; a page Claude Code's
    /// server does not have is refused with why, and a URI that is no app's page never reaches
    /// Claude Code.
    #[tokio::test]
    async fn a_claude_code_page_asks_its_app_through_claude_code() {
        use slopty_proto::visual::{AppAnswer, AppAsk, AppBody};
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let log = root.join("apps.log");
        let servers = serde_json::json!({ "mcpServers": { "apps": {
            "command": bin("slopty-stub-mcp-apps"), "args": ["--log", log],
        } } });
        std::fs::write(work.join(".mcp.json"), servers.to_string()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&work, Some("Say hello."))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;

        let asks = [
            (1, AppAsk::Read { server: "apps".to_owned(), uri: "ui://apps/echo".to_owned() }),
            (
                2,
                AppAsk::Call {
                    server: "apps".to_owned(),
                    tool: "echo".to_owned(),
                    arguments: r#"{"text":"hi"}"#.to_owned(),
                },
            ),
            (3, AppAsk::Read { server: "apps".to_owned(), uri: "ui://apps/none".to_owned() }),
            (4, AppAsk::Read { server: "apps".to_owned(), uri: "https://example.com/".to_owned() }),
        ];
        for (id, ask) in asks {
            a.send(ThreadRequest::App { thread, id, ask }).await;
        }
        a.until(|c| {
            (1..=4).all(|n| c.got.iter().any(|g| matches!(g, Got::App(id, _) if *id == n)))
        })
        .await;
        let answer = |n: u64| {
            a.got
                .iter()
                .find_map(|g| match g {
                    Got::App(id, answer) if *id == n => Some(answer.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let AppAnswer::Resource { uri, mime_type, body: AppBody::Text(html), .. } = answer(1)
        else {
            panic!("{:?}", answer(1))
        };
        assert_eq!(uri, "ui://apps/echo");
        assert_eq!(mime_type.as_deref(), Some("text/html;profile=mcp-app"));
        assert!(html.contains("echo"), "{html}");
        let AppAnswer::Called { result } = answer(2) else { panic!("{:?}", answer(2)) };
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["structuredContent"]["echo"], "hi", "{result}");
        assert!(matches!(answer(3), AppAnswer::Refused(why) if why.contains("no resource")));
        assert!(matches!(answer(4), AppAnswer::Refused(why) if why.contains("ui://")));

        let seen = recorded(&record).await;
        let calls = heard_lines(&seen, "mcp_call");
        assert_eq!(calls.len(), 1, "{calls:#?}");
        assert_eq!(calls[0]["request"]["tool"], "mcp__apps__echo");
        assert_eq!(calls[0]["request"]["result_not_for_model"], true);
        let reads = heard_lines(&seen, "mcp_read_resource");
        assert_eq!(reads.len(), 2, "the page that is no app's never went: {reads:#?}");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(!logged.contains("example.com"), "{logged}");
    }

    /// A rewind of the files can be undone: the worker takes the folder before Claude Code puts
    /// the files back (`rewind_files`), so the file a later turn wrote, gone with the rewind,
    /// comes back with the undo. An undo once done, or of a tree that is not the last rewind's,
    /// is refused.
    #[tokio::test]
    async fn a_rewind_of_the_files_is_undone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        for args in
            [&["init", "-q", "-b", "main"][..], &["commit", "-q", "--allow-empty", "-m", "c0"]]
        {
            let done = std::process::Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .status()
                .unwrap();
            assert!(done.success(), "git {args:?}");
        }
        let (daemons, _record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&work, Some("write a.txt one"))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;
        let send = Intent::Send {
            text: "write b.txt two".to_owned(),
            delivery: slopty_proto::thread::Delivery::Steer,
            attachments: Vec::new(),
        };
        assert_eq!(a.intent(IntentId::new(), thread, send).await, Outcome::Done);
        a.until(turns_done(2)).await;
        let (a_txt, b_txt) = (work.join("a.txt"), work.join("b.txt"));
        assert_eq!(std::fs::read_to_string(&b_txt).unwrap(), "two\n");
        let second = a.state().turns[1].id;

        let rewind = Intent::Rewind { turn: second };
        assert_eq!(a.intent(IntentId::new(), thread, rewind).await, Outcome::Done);
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.rewound.is_some())).await;
        let rewound = a.state().rewound.clone().unwrap();
        assert_eq!(rewound.turn, second);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while b_txt.exists() {
            assert!(std::time::Instant::now() < deadline, "b.txt put back");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(std::fs::read_to_string(&a_txt).unwrap(), "one\n", "turn 1's file stays");

        let stale = Intent::Unrewind { tree: slopty_proto::thread::TreeRef("0".repeat(40)) };
        assert!(matches!(a.intent(IntentId::new(), thread, stale).await, Outcome::Refused { .. }));
        let undo = Intent::Unrewind { tree: rewound.tree.clone() };
        assert_eq!(a.intent(IntentId::new(), thread, undo.clone()).await, Outcome::Done);
        assert_eq!(std::fs::read_to_string(&b_txt).unwrap(), "two\n", "back with the undo");
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.rewound.is_none())).await;
        let again = a.intent(IntentId::new(), thread, undo).await;
        assert!(matches!(again, Outcome::Refused { .. }), "{again:?}");
    }

    /// A rewind of the code and the conversation is undone in a folder outside git, as the
    /// dock's "Undo rewind" does it: the rewind and the fork go at once, the worker keeps the file
    /// the rewind takes away as Claude Code names it first (`rewind_files` with `dry_run`), and the
    /// undo, sent with the branch's discard, writes it back.
    #[tokio::test]
    async fn a_branched_rewind_outside_git_is_undone() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        // One record per process: the branch's Claude Code is one of its own.
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let records = root.join("records");
        std::fs::create_dir_all(&records).unwrap();
        let env = [
            ("PATH", slopty_testkit::env::path_with(&programs)),
            ("STUB_RECORD", records.join(concat!("{", "pid", "}.json")).into_os_string()),
        ];
        let daemons = daemons_with(&root, &env).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&work, Some("write notes.txt first words"))).await;
        a.follow(thread).await;
        a.until(turns_done(1)).await;
        let notes = work.join("notes.txt");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first words\n");
        let first = a.state().turns[0].id;

        // Both at once, as `slopty.Rewind` sends them.
        let (rewind, fork) = (IntentId::new(), IntentId::new());
        a.send(ThreadRequest::Intent {
            id: rewind,
            thread,
            intent: Intent::Rewind { turn: first },
        })
        .await;
        let after = Some(slopty_proto::thread::TurnId(first.0.saturating_sub(1)));
        a.send(ThreadRequest::Intent { id: fork, thread, intent: Intent::Fork { after } }).await;
        let dones = both_done(&mut a, [rewind, fork]).await;
        assert_eq!(dones[&rewind], Outcome::Done);
        let Outcome::Started { thread: branch } = dones[&fork] else { panic!("{dones:?}") };
        let kept = a.until(|c| c.thread.as_ref().is_some_and(|s| s.rewound.is_some()));
        tokio::time::timeout(Duration::from_secs(30), kept)
            .await
            .expect("the files the rewind takes back were kept, outside git too");
        let rewound = a.state().rewound.clone().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while notes.exists() {
            assert!(std::time::Instant::now() < deadline, "notes.txt taken back");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // The undo and the branch's delete at once, as `slopty.UndoRewind` sends them.
        let (undo, discard) = (IntentId::new(), IntentId::new());
        let unrewind = Intent::Unrewind { tree: rewound.tree.clone() };
        a.send(ThreadRequest::Intent { id: undo, thread, intent: unrewind }).await;
        a.send(ThreadRequest::Intent { id: discard, thread: branch, intent: Intent::Discard })
            .await;
        let dones = both_done(&mut a, [undo, discard]).await;
        assert_eq!(dones[&undo], Outcome::Done, "{dones:?}");
        assert_eq!(dones[&discard], Outcome::Done, "{dones:?}");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), "first words\n", "back with the undo");
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.rewound.is_none())).await;

        let dry: Vec<Vec<bool>> = std::fs::read_dir(&records)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .map(|path| {
                let seen: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                heard_lines(&seen, "rewind_files")
                    .iter()
                    .map(|l| l["request"]["dry_run"].as_bool().unwrap_or(false))
                    .collect()
            })
            .filter(|dry: &Vec<bool>| !dry.is_empty())
            .collect();
        assert_eq!(dry, [[true, false]], "asked what it would put back, then put it back");
    }

    /// The outcomes of intents `ids`, once each is done.
    async fn both_done(
        a: &mut Client,
        ids: [IntentId; 2],
    ) -> std::collections::HashMap<IntentId, Outcome> {
        let mut dones = std::collections::HashMap::new();
        a.heard(|msg| {
            if let WorkerMsg::IntentDone(IntentDone { id, outcome }) = msg
                && ids.contains(&id)
            {
                dones.insert(id, outcome);
            }
            (dones.len() == ids.len()).then_some(())
        })
        .await;
        dones
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
    ) -> std::time::Instant {
        loop {
            if holds(&awake(daemons).await) {
                return std::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A driven turn keeps the worker's machine awake with nobody linked, as the thread host
    /// holds it (no terminal shows it): from within one sampling tick of the client leaving,
    /// for as long as the turn runs, and no longer.
    #[tokio::test]
    async fn a_driven_turn_keeps_the_machine_awake_with_nobody_linked() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, _record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        // Twenty-five deltas 200 ms apart: five seconds of a turn, held from its start and let
        // go at its end, each a change of the thread's table the worker looks over at once.
        let thread = started(&mut a, claude_in(&root, Some("slow 25"))).await;
        a.follow(thread).await;
        a.until(|c| c.thread.as_ref().is_some_and(|s| !s.items.is_empty())).await;
        a.leave().await;
        let left = std::time::Instant::now();

        let held = awake_until(&daemons, |w| w.clients == 0 && w.agents == 1 && w.system).await;
        let after = held.duration_since(left);
        assert!(after <= TWO_TICKS, "held {after:?} after the client left");
        let released = awake_until(&daemons, |w| w.agents == 0 && !w.system).await;
        println!(
            "held {:?} after the client left, released {:?} after",
            held.duration_since(left),
            released.duration_since(left)
        );
    }

    /// A turn whose agent died holds nothing: the thread's process is gone, whatever its phase
    /// last said.
    #[tokio::test]
    async fn a_turn_whose_agent_died_lets_the_machine_sleep() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, _record) = with_claude_and(&root, &[("STUB_CRASH_MID_TURN", "1")]).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let thread = started(&mut a, claude_in(&root, Some("slow 75"))).await;
        a.follow(thread).await;
        a.until(|c| {
            c.thread.as_ref().is_some_and(|s| {
                matches!(s.status.liveness, slopty_proto::thread::Liveness::Exited { .. })
            })
        })
        .await;
        a.leave().await;
        // Let go within two ticks of the exit, at the latest: the exit changes the thread's table,
        // which the worker looks over at once.
        let let_go =
            |w: &slopty_proto::ctl::Awake| (w.clients, w.agents, w.system) == (0, 0, false);
        let released = tokio::time::timeout(TWO_TICKS, awake_until(&daemons, let_go)).await;
        let now = awake(&daemons).await;
        assert!(released.is_ok(), "{now:?}");
    }

    /// How often the worker looks its threads over with nothing changed (`TICK` of the daemon's
    /// `threads::awake`, private to its crate), twice: the latest a sample after a change is
    /// taken, though a change of the threads' table is looked at at once.
    const TWO_TICKS: Duration = Duration::from_secs(10);
}
