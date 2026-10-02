//! ptyd + worker + real client links on this machine: an observed Claude Code session's thread
//! served over the link. The table and the thread are followed from a cursor, the link drops
//! and comes back, and only what it missed is sent; an intent sent again after the drop is
//! answered as the first time and acts on nothing. The agent is played by its captured
//! transcript, written here, and by `slopty hook` run as Claude Code runs it, as the test's own
//! child; nothing is typed into the shell.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod threads {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_agent::conversation::{Conversation, ThreadId as ConvThread};
    use slopty_agent::transcript::Tail;
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_proto::thread::wire::{
        Expanded, Intent, IntentDone, Outcome, Start, TableFrame, ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        AgentId, Cap, Cursor, IntentId, Request, RequestState, TableState, ThreadId, ThreadState,
    };
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    /// Long enough for any step on a loaded machine; the test judges what arrives, not when.
    const STEP: Duration = Duration::from_secs(20);

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
    }

    impl Drop for Daemons {
        fn drop(&mut self) {
            for child in &mut self.children {
                let _killed = child.start_kill();
            }
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
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
            assert!(tokio::time::Instant::now() < deadline, "ptyd never listened");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let pasteboard = format!("dev.aislopware.slopty.threads.{leaf}");
        let mut worker = scrubbed(bin("slopty-worker"), dir)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .arg("--ptyd-socket")
            .arg(&ptyd_sock)
            .arg("--ctl-socket")
            .arg(dir.join("worker.sock"))
            .arg("--data-dir")
            .arg(dir.join("data"))
            .args(["--print-addr", "--port", "0"])
            .env("SLOPTY_PASTEBOARD", &pasteboard)
            .env("SLOPTY_DROP_DIR", dir.join("drop"))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut line = String::new();
        let stdout = worker.stdout.take().unwrap();
        tokio::time::timeout(STEP, BufReader::new(stdout).read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let bound: SocketAddr = line.trim().parse().unwrap();
        let addr = if bound.ip().is_unspecified() {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()))
        } else {
            bound
        };
        Daemons { children: vec![ptyd, worker], pasteboard, addr, dir: dir.to_path_buf() }
    }

    /// The fixture transcripts and hooks `slopty-agent` pins its decoder with.
    fn fixture(scenario: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/slopty-agent/tests/fixtures/conversation")
            .join(scenario)
    }

    /// Run `slopty hook` as Claude Code would, in `session`, with `payload` on stdin.
    fn relay(dir: &Path, session: SessionId, payload: &serde_json::Value) -> Child {
        use tokio::io::AsyncWriteExt as _;
        let mut child = scrubbed(bin("slopty"), dir)
            .arg("--data-dir")
            .arg(dir.join("data"))
            .arg("hook")
            .env("SLOPTY_SESSION", session.to_string())
            .env("SLOPTY_WORKER_SOCKET", dir.join("worker.sock"))
            .env("CLAUDE_CONFIG_DIR", dir.join("claude-config"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let bytes = payload.to_string().into_bytes();
        tokio::spawn(async move {
            let _written = stdin.write_all(&bytes).await;
        });
        child
    }

    /// What a relay printed for Claude Code, once it exits successfully.
    async fn printed(child: Child) -> String {
        let out = tokio::time::timeout(STEP, child.wait_with_output()).await.unwrap().unwrap();
        assert!(out.status.success(), "the relay exits 0: {:?}", out.status);
        String::from_utf8(out.stdout).unwrap()
    }

    /// The ids of the entries the decoder reads from the main transcript at `path`.
    fn entries(path: &Path) -> Vec<String> {
        let mut conversation = Conversation::default();
        conversation.read(&mut Tail::default(), path).unwrap();
        conversation.entries(&ConvThread::Main).iter().map(|e| e.id.clone()).collect()
    }

    fn ids(state: &ThreadState) -> Vec<String> {
        state.items.iter().map(|i| i.id.0.clone()).collect()
    }

    /// A frame as it arrived, for judging what was sent.
    #[derive(Debug, PartialEq, Eq)]
    enum Got {
        Snapshot(Cursor),
        Page { turns: usize, older: bool },
        Expanded(Expanded),
        Review(Box<slopty_proto::thread::wire::Review>),
        Actions { epoch: u64, first: u64, next: u64 },
    }

    /// One client: its link, and its copies of the table and of one thread, kept from nothing
    /// but what the link brings.
    struct Client {
        _endpoint: slopty_net::Endpoint,
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
                        Err(slopty_net::NetError::Connect(why)) if why.ends_with("no answer") => {}
                        other => break other,
                    }
                }
            };
            let conn = tokio::time::timeout(STEP, dialing).await.unwrap().unwrap();
            let mut link = WorkerLink::start(conn);
            let events = link.events().unwrap();
            Self {
                _endpoint: endpoint,
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

        /// Follow `thread` from where this copy stands.
        async fn follow(&self, thread: ThreadId) {
            let have = self.cursor;
            self.send(ThreadRequest::Follow { thread, have, turns: 100, max_latency_ms: 0 }).await;
        }

        /// The next event, its thread and table frames taken into the copies; a control
        /// message is handed back.
        async fn step(&mut self) -> Option<WorkerMsg> {
            let event = tokio::time::timeout(STEP, self.events.recv()).await.unwrap().unwrap();
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

    /// Open `/bin/sh` in `cwd`, not attached.
    async fn open_shell(client: &mut Client, cwd: &Path) -> SessionId {
        open(client, cwd, vec!["/bin/sh".to_owned()]).await
    }

    /// Open `command` in `cwd`, not attached.
    async fn open(client: &mut Client, cwd: &Path, command: Vec<String>) -> SessionId {
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command,
            env: vec![("PS1".to_owned(), "$ ".to_owned())],
            title: None,
            attach: false,
        };
        client.link.send(ClientMsg::OpenSession { request: 1, spec }).await.unwrap();
        client
            .heard(|msg| match msg {
                WorkerMsg::SessionOpened { summary, .. } => Some(summary.id),
                _ => None,
            })
            .await
    }

    /// A Claude Code session's thread is found in the table and followed over a real client
    /// link; a held prompt is answered by an intent. The link drops while the agent goes on
    /// (a second client, following throughout, sees it all land), and comes back as the same
    /// client: the table and the thread resume from its cursors with only what it missed, and
    /// the intent sent again is answered as before and acts on nothing, where a new one for
    /// the same prompt finds it gone.
    #[tokio::test]
    async fn a_thread_resumes_from_its_cursor_and_an_intent_is_had_once() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let me = ClientId::new();
        let mut a = Client::connect(&daemons, me).await;
        let session = open_shell(&mut a, dir.path()).await;

        // The captured `tools` session, its first half written now and the rest later.
        let main = daemons.dir.join("projects").join("s1.jsonl");
        std::fs::create_dir_all(main.parent().unwrap()).unwrap();
        let captured = std::fs::read_to_string(fixture("tools").join("transcript.jsonl")).unwrap();
        let lines: Vec<&str> = captured.lines().collect();
        let (first, rest) = lines.split_at(lines.len() / 2);
        std::fs::write(&main, format!("{}\n", first.join("\n"))).unwrap();
        let transcript = main.to_string_lossy().into_owned();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": transcript, "cwd": dir.path(),
        });
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");

        let thread = slopty_agent::observed::thread_of("s1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        assert!(matches!(a.tables.first(), Some(TableFrame::Snapshot { .. })), "{:?}", a.tables);
        a.follow(thread).await;
        let half = entries(&main);
        a.until(|c| c.thread.as_ref().is_some_and(|s| ids(s) == half)).await;
        assert!(matches!(a.got.first(), Some(Got::Snapshot(_))), "a first follow is a snapshot");

        // A prompt, as the `permission` capture's first one, held for the follower and
        // answered by an intent.
        let hooks = std::fs::read_to_string(fixture("permission").join("hooks.jsonl")).unwrap();
        let mut ask: serde_json::Value = hooks
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .map(|l| l["input"].clone())
            .find(|input| input["hook_event_name"] == "PermissionRequest")
            .unwrap();
        ask["transcript_path"] = transcript.clone().into();
        ask["session_id"] = "s1".into();
        let held = relay(dir.path(), session, &ask);
        a.until(|c| c.state().open_requests().count() == 1).await;
        let request = a.state().open_requests().next().unwrap().id.clone();
        let answer =
            Intent::Answer { ask: request.clone(), choice: "allow".to_owned(), message: None };
        let once = IntentId::new();
        assert_eq!(a.intent(once, thread, answer.clone()).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        assert_eq!(output["hookSpecificOutput"]["decision"]["behavior"], "allow");
        a.until(|c| {
            c.state()
                .requests
                .iter()
                .any(|r| r.id == request && matches!(r.state, RequestState::Answered { .. }))
        })
        .await;

        // A witness follows throughout; the link drops, and the agent goes on.
        let mut witness = Client::connect(&daemons, ClientId::new()).await;
        witness.follow(thread).await;
        witness.until(|c| c.cursor == a.cursor).await;
        let (cursor, table, kept) = (a.cursor.unwrap(), a.table.clone(), a.thread.clone());
        a.link.close();
        drop(a);
        let mut file = std::fs::OpenOptions::new().append(true).open(&main).unwrap();
        std::io::Write::write_all(&mut file, format!("{}\n", rest.join("\n")).as_bytes()).unwrap();
        let whole = entries(&main);
        assert!(whole.len() > half.len(), "the rest has entries of its own");
        witness.until(|c| c.thread.as_ref().is_some_and(|s| ids(s) == whole)).await;
        let landed = witness.cursor.unwrap();
        assert_eq!(landed.epoch, cursor.epoch, "the log went on, not over");

        // Back as the same client, from its cursors: only what it missed comes.
        let mut b = Client::connect(&daemons, me).await;
        (b.table, b.thread, b.cursor) = (table.clone(), kept, Some(cursor));
        b.send(ThreadRequest::Table { have: Some(table.cursor) }).await;
        b.until(|c| !c.tables.is_empty()).await;
        assert!(matches!(b.tables.first(), Some(TableFrame::Delta { .. })), "{:?}", b.tables);
        b.follow(thread).await;
        b.until(|c| c.cursor == Some(landed)).await;
        assert!(!b.got.iter().any(|g| matches!(g, Got::Snapshot(_))), "{:?}", b.got);
        let resumed = b.got.first().unwrap();
        let from = Got::Actions { epoch: cursor.epoch, first: cursor.seq, next: landed.seq };
        assert!(
            matches!(resumed, Got::Actions { epoch, first, .. } if *epoch == cursor.epoch && *first == cursor.seq),
            "resumed {resumed:?}, not from {from:?}"
        );
        assert_eq!(b.state(), witness.state(), "the copy came out as the worker's");
        assert!(b.table.rows.contains_key(&thread));

        // The intent again, from before the drop: its first outcome, and nothing done; a new
        // one for the same prompt finds it answered.
        assert_eq!(b.intent(once, thread, answer.clone()).await, Outcome::Done);
        let again = b.intent(IntentId::new(), thread, answer).await;
        assert!(matches!(again, Outcome::Refused { .. }), "{again:?}");

        // Pages and expansions come on the thread's stream, between its frames.
        let last = b.state().last_turn().map(|t| t.id).unwrap();
        b.send(ThreadRequest::Page { thread, before: last, turns: 1 }).await;
        b.until(|c| c.got.iter().any(|g| matches!(g, Got::Page { .. }))).await;
        let content = slopty_proto::thread::ContentRef("nothing of this thread".to_owned());
        b.send(ThreadRequest::Expand { thread, content }).await;
        b.until(|c| c.got.contains(&Got::Expanded(Expanded::Gone))).await;

        b.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
        drop(file);
    }

    /// A message sent to a thread is typed by the worker into its agent's terminal, as a paste
    /// and an Enter, and its pending entry goes once it has. The terminal's program records
    /// every byte it is given (`cat`, in raw mode); the agent's hooks are `slopty hook` run as
    /// Claude Code runs it.
    #[tokio::test]
    async fn a_message_sent_to_a_thread_is_typed_into_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let record = dir.path().join("record");
        let script = r#"stty raw -echo; printf ready >"$0"; exec cat >>"$0""#;
        let command = ["/bin/sh", "-c", script, &record.to_string_lossy()].map(str::to_owned);
        let session = open(&mut a, dir.path(), command.to_vec()).await;
        let read = || std::fs::read_to_string(&record).unwrap_or_default();
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        let recorded = async |want: &str| {
            while read() != want {
                assert!(tokio::time::Instant::now() < deadline, "{:?}", read());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        recorded("ready").await;
        std::fs::write(&record, "").unwrap();

        let main = dir.path().join("s2.jsonl");
        std::fs::write(&main, "").unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s2",
            "transcript_path": main, "cwd": dir.path(),
        });
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");
        let thread = slopty_agent::observed::thread_of("s2");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        a.follow(thread).await;
        a.until(|c| c.thread.is_some()).await;

        let id = IntentId::new();
        let send = Intent::Send {
            text: "hello from the face".to_owned(),
            delivery: slopty_proto::thread::Delivery::Steer,
        };
        assert_eq!(a.intent(id, thread, send).await, Outcome::Accepted);
        recorded("hello from the face\r").await;
        a.until(|c| c.state().pending.is_empty()).await;

        // A review comes on the thread's stream; this folder is not in git, so it says so,
        // and nothing can be kept.
        let scope = slopty_proto::thread::wire::ReviewScope::Kept;
        a.send(ThreadRequest::Review { thread, scope }).await;
        a.until(|c| c.got.iter().any(|g| matches!(g, Got::Review(_)))).await;
        let absent = a.got.iter().find_map(|g| match g {
            Got::Review(review) => review.absent.as_deref(),
            _ => None,
        });
        assert_eq!(absent, Some(slopty_worker::thread::review::NOT_IN_GIT));
        let pick = slopty_proto::thread::wire::Pick {
            path: "a.txt".to_owned(),
            from: None,
            stamp: None,
            hunks: Vec::new(),
        };
        let kept = a.intent(IntentId::new(), thread, Intent::Keep(pick)).await;
        assert!(matches!(kept, Outcome::Refused { .. }), "{kept:?}");

        a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// The first `PermissionRequest` of the `permission` capture, as Claude Code session
    /// `native` would send it with its transcript at `transcript`.
    fn first_ask(native: &str, transcript: &Path) -> serde_json::Value {
        let hooks = std::fs::read_to_string(fixture("permission").join("hooks.jsonl")).unwrap();
        let mut ask: serde_json::Value = hooks
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .map(|l| l["input"].clone())
            .find(|input| input["hook_event_name"] == "PermissionRequest")
            .unwrap();
        ask["transcript_path"] = transcript.to_string_lossy().into_owned().into();
        ask["session_id"] = native.into();
        ask
    }

    /// The row of the thread observed in terminal `terminal`, when there is one.
    fn row_in(
        client: &Client,
        terminal: SessionId,
    ) -> Option<&slopty_proto::thread::wire::ThreadRow> {
        client.table.rows.values().find(|r| r.terminal == Some(terminal))
    }

    /// A Claude Code whose first word is a permission prompt, with no session start before it
    /// and its transcript still empty, has a thread from that hook: the prompt is a request on
    /// it, in the table and in the thread. The thread fills from the transcript once it is
    /// written, and the request is answered by an intent.
    #[tokio::test]
    async fn a_first_hook_that_asks_opens_a_thread_with_its_request() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        a.send(ThreadRequest::Approvals { on: true }).await;
        let session = open_shell(&mut a, dir.path()).await;
        let main = dir.path().join("p1.jsonl");
        std::fs::write(&main, "").unwrap();

        let held = relay(dir.path(), session, &first_ask("p1", &main));
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| row_in(c, session).is_some_and(|r| r.requests.len() == 1)).await;
        let row = row_in(&a, session).unwrap();
        let thread = row.id;
        assert_eq!(thread, slopty_agent::observed::thread_of("p1"), "the session's own thread");
        assert_eq!(row.requests[0].kind, Request::APPROVAL);

        a.follow(thread).await;
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.open_requests().count() == 1)).await;
        let captured = std::fs::read_to_string(fixture("tools").join("transcript.jsonl")).unwrap();
        std::fs::write(&main, captured).unwrap();
        let whole = entries(&main);
        assert!(!whole.is_empty());
        a.until(|c| c.thread.as_ref().is_some_and(|s| ids(s) == whole)).await;
        assert_eq!(a.state().open_requests().count(), 1, "the request outlives the read");

        let request = a.state().open_requests().next().unwrap().id.clone();
        let answer = Intent::Answer { ask: request, choice: "allow".to_owned(), message: None };
        assert_eq!(a.intent(IntentId::new(), thread, answer).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        assert_eq!(output["hookSpecificOutput"]["decision"]["behavior"], "allow");
        a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// Two terminals whose Claude Codes name one session id at once keep a thread each: the
    /// first the session's own, the second one of its own. A prompt in the second is a request
    /// on the second's thread alone, and neither thread moves to the other terminal.
    #[tokio::test]
    async fn two_terminals_on_one_session_id_keep_a_thread_each() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        a.send(ThreadRequest::Approvals { on: true }).await;
        let first = open_shell(&mut a, dir.path()).await;
        let second = open_shell(&mut a, dir.path()).await;
        let main = dir.path().join("e2e.jsonl");
        let captured = std::fs::read_to_string(fixture("tools").join("transcript.jsonl")).unwrap();
        std::fs::write(&main, captured).unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "e2e",
            "transcript_path": main, "cwd": dir.path(),
        });
        a.send(ThreadRequest::Table { have: None }).await;
        assert_eq!(printed(relay(dir.path(), first, &start)).await, "");
        a.until(|c| row_in(c, first).is_some()).await;
        assert_eq!(printed(relay(dir.path(), second, &start)).await, "");
        a.until(|c| row_in(c, first).is_some() && row_in(c, second).is_some()).await;
        let mine = row_in(&a, first).unwrap().id;
        let beside = row_in(&a, second).unwrap().id;
        assert_eq!(mine, slopty_agent::observed::thread_of("e2e"));
        assert_eq!(beside, slopty_agent::observed::thread_in("e2e", second));

        let held = relay(dir.path(), second, &first_ask("e2e", &main));
        a.until(|c| c.table.rows.get(&beside).is_some_and(|r| r.requests.len() == 1)).await;
        let rows = &a.table.rows;
        assert!(rows[&mine].requests.is_empty(), "the first terminal's thread asks nothing");
        assert_eq!(rows[&mine].terminal, Some(first), "it stays with its terminal");
        assert_eq!(rows[&beside].terminal, Some(second));
        let ask = rows[&beside].requests[0].id.clone();
        let deny = Intent::Answer { ask, choice: "deny".to_owned(), message: None };
        assert_eq!(a.intent(IntentId::new(), beside, deny).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        assert_eq!(output["hookSpecificOutput"]["decision"]["behavior"], "deny");
        for session in [first, second] {
            a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
        }
    }

    /// What the stand-in recorded of how it was started, once it has.
    async fn recorded(record: &Path) -> serde_json::Value {
        tokio::time::timeout(STEP, async {
            loop {
                if let Some(seen) =
                    std::fs::read(record).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
                {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the stand-in started")
    }

    /// The person's `claude` and the stand-in's record of how it started, first on the
    /// `PATH` of a worker whose home is `root`.
    async fn with_claude(root: &Path) -> (Daemons, PathBuf) {
        let programs = root.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let record = root.join("record.json");
        let env = [
            ("PATH", slopty_testkit::env::path_with(&programs)),
            ("STUB_RECORD", record.clone().into_os_string()),
        ];
        (daemons_with(root, &env).await, record)
    }

    /// A Claude Code thread started from a client opens the person's own `claude` (the
    /// stand-in, first on the worker's `PATH`) in one of the worker's terminals, wired as the
    /// worker wires any `claude` it opens, on a session id chosen for it and with the first
    /// message on its own command line. The start is answered with the thread named by that id,
    /// which a client follows at once, its terminal named, and the agent's first hook is heard
    /// in it.
    #[tokio::test]
    async fn a_claude_code_start_opens_claude_in_a_terminal_and_names_its_thread() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let id = IntentId::new();
        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: root.to_string_lossy().into_owned(),
            drive: None,
            prompt: Some("Say hello.".to_owned()),
            model: None,
            args: Vec::new(),
        };
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

        let seen = recorded(&record).await;
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
        let at = argv.iter().position(|a| *a == "--session-id").expect("a session id");
        let native = argv[at + 1];
        assert_eq!(thread, slopty_agent::observed::thread_of(native), "named by its session id");
        assert_eq!(argv[argv.len() - 2..], ["--", "Say hello."], "its own initial prompt");
        assert!(argv.contains(&"--settings"), "the hook relay, as the worker wires claude");
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref());

        // The stand-in's first hook, through the relay, is heard in the thread.
        a.follow(thread).await;
        let hooked = |s: &ThreadState| s.meta.terminal.is_some() && s.meta.can(Cap::APPROVALS);
        a.until(|c| c.thread.as_ref().is_some_and(hooked)).await;
        assert_eq!(a.state().meta.native, native);
    }

    /// The palette's start: no first message and a folder under the worker's home spelled
    /// with `~`, as a client that knows no home writes it. Claude Code opens in the home with
    /// nothing on its command line after its own flags, waiting for the person.
    #[tokio::test]
    async fn a_claude_code_start_with_no_prompt_opens_in_the_home_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (daemons, record) = with_claude(&root).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let id = IntentId::new();
        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: "~".to_owned(),
            drive: None,
            prompt: None,
            model: None,
            args: Vec::new(),
        };
        a.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
        let outcome = a
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                _ => None,
            })
            .await;
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref(), "`~` is the worker's home");
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
        assert!(!argv.contains(&"--"), "no first message: {argv:?}");
    }

    /// A Claude Code session works where its hooks say, whatever folder its terminal reports:
    /// its thread's folder is the agent's own, known before any transcript is, and the slash
    /// commands the composer offers are those Claude Code takes there.
    #[tokio::test]
    async fn a_session_works_where_its_hooks_say() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let work = std::fs::canonicalize(dir.path()).unwrap().join("work");
        std::fs::create_dir_all(&work).unwrap();
        // Claude Code has written nothing to its transcript yet.
        let transcript = daemons.dir.join("projects").join("h1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "").unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "h1",
            "transcript_path": transcript, "cwd": work,
        });
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");
        let thread = slopty_agent::observed::thread_of("h1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        a.follow(thread).await;
        let cwd = work.to_string_lossy().into_owned();
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.meta.cwd == cwd)).await;
        // The commands Claude Code takes there, its own among them.
        a.until(|c| {
            c.thread.as_ref().is_some_and(|s| s.commands.iter().any(|c| c.name == "compact"))
        })
        .await;
    }

    /// A question Claude Code asks while nobody follows its thread is not held, so it is asked
    /// in its own terminal; the thread still shows it as a request answered only there, and
    /// taking it to the terminal is no error.
    #[tokio::test]
    async fn a_question_asked_in_the_terminal_is_shown_on_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let transcript = daemons.dir.join("projects").join("q1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "").unwrap();
        let hook = |event: &str, more: serde_json::Value| {
            let mut payload = serde_json::json!({
                "hook_event_name": event, "session_id": "q1",
                "transcript_path": transcript, "cwd": dir.path(),
            });
            payload.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
            payload
        };
        let start = hook("SessionStart", serde_json::json!({ "source": "startup" }));
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");
        let asked = "Where should the key live?";
        let question = hook(
            "PermissionRequest",
            serde_json::json!({
                "permission_mode": "default", "tool_name": "AskUserQuestion",
                "tool_input": { "questions": [{
                    "question": asked, "header": "Key", "multiSelect": false,
                    "options": [
                        { "label": "Memory", "description": "In the request" },
                        { "label": "Store", "description": "In the session store" }
                    ]
                }] },
            }),
        );
        let _passed = printed(relay(dir.path(), session, &question)).await;

        let thread = slopty_agent::observed::thread_of("q1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        a.follow(thread).await;
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.open_requests().count() == 1)).await;
        let request = a.state().open_requests().next().unwrap().clone();
        assert_eq!((request.kind.as_str(), request.title.as_str()), (Request::QUESTION, asked));
        assert!(request.options.is_empty() && request.questions.is_empty(), "answered there");
        let release = Intent::Release { ask: request.id };
        assert_eq!(a.intent(IntentId::new(), thread, release).await, Outcome::Done);
    }
}
