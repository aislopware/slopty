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
        Expanded, Intent, IntentDone, Outcome, TableFrame, ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{Cursor, IntentId, RequestState, TableState, ThreadId, ThreadState};
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
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), dir)
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
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some(cwd.to_string_lossy().into_owned()),
            command: vec!["/bin/sh".to_owned()],
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
}
