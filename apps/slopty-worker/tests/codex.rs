//! ptyd + worker + real client links on this machine, beside a stand-in Codex app-server: a
//! Codex thread shared with its TUI, served over the link, and its approval answered from two
//! sides. The app-server is played by what the pinned Codex said to the client that resumed the
//! thread (`crates/slopty-agent/tests/fixtures/codex/approval.jsonl`), replayed in order on the
//! daemon's control socket; the TUI is played by that recording too. No Codex runs, and nothing
//! is typed into a terminal.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod codex {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use futures_util::{SinkExt as _, StreamExt as _};
    use serde_json::{Value, json};
    use slopty_agent::codex::shared;
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::ClientId;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::thread::wire::{Intent, IntentDone, Outcome, ThreadFrame, ThreadRequest};
    use slopty_proto::thread::{
        AskId, Cursor, IntentId, Phase, RequestState, TableState, ThreadId, ThreadState, TurnState,
    };
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::net::UnixListener;
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;

    /// Long enough for any step on a loaded machine; the test judges what arrives, not when.
    const STEP: Duration = Duration::from_secs(20);

    /// The recorded approval, which Codex numbered 0.
    const ASK: &str = "0";

    fn bin(name: &str) -> PathBuf {
        slopty_testkit::bins::bin(env!("CARGO_BIN_EXE_slopty-worker"), name)
    }

    /// The daemons, killed with the test; their pasteboard released.
    struct Daemons {
        children: Vec<Child>,
        pasteboard: String,
        addr: SocketAddr,
    }

    impl Drop for Daemons {
        fn drop(&mut self) {
            for child in &mut self.children {
                let _killed = child.start_kill();
            }
            slopty_input::MacBoard::named(&self.pasteboard).release();
        }
    }

    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

    /// ptyd and a worker on `dir`, whose home is `dir` and whose Codex home is `codex_home`.
    async fn daemons(dir: &Path, codex_home: &Path) -> Daemons {
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
        let pasteboard = format!("dev.aislopware.slopty.codex.{leaf}");
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
            .env("CODEX_HOME", codex_home)
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
        let addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, bound.port()));
        Daemons { children: vec![ptyd, worker], pasteboard, addr }
    }

    /// One recorded frame.
    struct Line {
        sent: bool,
        msg: Value,
    }

    /// What the pinned Codex and the client that resumed the thread said to each other.
    fn follower() -> Vec<Line> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/slopty-agent/tests/fixtures/codex/approval.jsonl"
        );
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["client"] == "b")
            .map(|line| Line { sent: line["dir"] == "sent", msg: line["msg"].clone() })
            .collect()
    }

    /// Who answers the approval first.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum First {
        /// The person, from Slopty: the stand-in waits for the worker's answer.
        Slopty,
        /// The Codex TUI: the stand-in settles it at once, as the recording shows the TUI's
        /// answer did.
        Tui,
    }

    /// The stand-in app-server's side of the worker's connection.
    struct Stub {
        ws: tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
        heard: mpsc::UnboundedSender<Value>,
    }

    impl Stub {
        /// The next frame the worker sends; `None` once it goes.
        async fn next(&mut self) -> Option<Value> {
            let Some(Ok(Message::Text(text))) = self.ws.next().await else { return None };
            let msg: Value = serde_json::from_str(&text).unwrap();
            self.heard.send(msg.clone()).unwrap();
            Some(msg)
        }

        async fn say(&mut self, msg: &Value) {
            self.ws.send(Message::text(msg.to_string())).await.unwrap();
        }

        /// The worker's next frame, a request of `method`, answered with `result`.
        async fn answer(&mut self, method: &str, result: Value) -> Value {
            let asked = self.next().await.unwrap();
            assert_eq!(asked["method"], method, "{asked}");
            self.say(&json!({ "id": asked["id"], "result": result })).await;
            asked
        }
    }

    /// The stand-in app-server at `listener`: it takes the worker's handshake and resume, then
    /// replays what Codex said after the resume, and when Slopty answers first waits for its
    /// answer to the approval. Every frame the worker sent is handed to `heard`.
    async fn app_server(listener: UnixListener, first: First, heard: mpsc::UnboundedSender<Value>) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stub = Stub { ws: tokio_tungstenite::accept_async(stream).await.unwrap(), heard };
        let lines = follower();
        // Where the follower sent `method`, and what it was answered.
        let recorded = |method: &str| {
            let at = lines.iter().position(|l| l.sent && l.msg["method"] == method).unwrap();
            let id = &lines[at].msg["id"];
            let answered = lines[at..].iter().position(|l| !l.sent && l.msg["id"] == *id).unwrap();
            let answered = answered.saturating_add(at);
            (answered, lines[answered].msg["result"].clone())
        };
        let init = stub.answer("initialize", recorded("initialize").1).await;
        assert_eq!(init["params"]["clientInfo"]["name"], "slopty");
        assert_eq!(stub.next().await.unwrap(), json!({ "method": "initialized" }));
        let (resumed_at, resumed) = recorded("thread/resume");
        let thread = resumed["thread"]["id"].clone();
        stub.answer("thread/loaded/list", json!({ "data": [thread], "nextCursor": null })).await;
        let resume = stub.answer("thread/resume", resumed).await;
        assert_eq!(resume["params"]["threadId"], thread);
        // What the thread has cost so far, asked as it is taken up; this account says no figure.
        let usage = stub.answer("account/usage/read", json!({ "summary": {} })).await;
        assert_eq!(usage["params"]["threadId"], thread);
        for line in lines.iter().skip(resumed_at.saturating_add(1)) {
            if !line.sent {
                stub.say(&line.msg).await;
            } else if first == First::Slopty {
                // The follower's answer to the approval: Slopty's must be the very same frame.
                assert_eq!(stub.next().await.unwrap(), line.msg, "the TUI's own answer");
            }
        }
        // Whatever the worker says from here is handed on.
        while stub.next().await.is_some() {}
    }

    /// One Slopty client of the worker, with its copies of the table and of one thread.
    struct Client {
        _endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        table: TableState,
        thread: Option<ThreadState>,
        cursor: Option<Cursor>,
    }

    impl Client {
        async fn connect(daemons: &Daemons, id: ClientId) -> Self {
            let endpoint = bind_client().unwrap();
            let hello = Hello { client: id, name: "codex".to_owned() };
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
                thread: None,
                cursor: None,
            }
        }

        async fn send(&self, req: ThreadRequest) {
            self.link.send(ClientMsg::Thread(req)).await.unwrap();
        }

        async fn step(&mut self) -> Option<WorkerMsg> {
            let event = tokio::time::timeout(STEP, self.events.recv()).await.unwrap().unwrap();
            match event {
                LinkEvent::Thread { frame, .. } => match frame {
                    ThreadFrame::Snapshot { cursor, state } => {
                        self.thread = Some(*state);
                        self.cursor = Some(cursor);
                    }
                    ThreadFrame::Actions { epoch, next, actions, .. } => {
                        let state = self.thread.as_mut().expect("actions follow a state");
                        for action in &actions {
                            state.apply(action);
                        }
                        self.cursor = Some(Cursor { epoch, seq: next });
                    }
                    _ => {}
                },
                LinkEvent::Control(WorkerMsg::Threads(frame)) => self.table.apply(&frame),
                LinkEvent::Control(msg) => return Some(msg),
                LinkEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
            None
        }

        async fn until(&mut self, done: impl Fn(&Self) -> bool) {
            while !done(self) {
                let _control = self.step().await;
            }
        }

        async fn intent(&mut self, thread: ThreadId, intent: Intent) -> Outcome {
            let id = IntentId::new();
            self.send(ThreadRequest::Intent { id, thread, intent }).await;
            loop {
                if let Some(WorkerMsg::IntentDone(IntentDone { id: done, outcome })) =
                    self.step().await
                    && done == id
                {
                    return outcome;
                }
            }
        }

        fn state(&self) -> &ThreadState {
            self.thread.as_ref().expect("following")
        }

        fn request(&self) -> Option<&slopty_proto::thread::Request> {
            self.thread.as_ref()?.requests.iter().find(|r| r.id.0 == ASK)
        }
    }

    /// Run the shared thread with `first` answering, and hand back what the worker sent the
    /// app-server and the two clients.
    async fn shared_approval(first: First) -> (Vec<Value>, Client, Client) {
        let dir = tempfile::tempdir().unwrap();
        // A socket's own path must fit 104 bytes, so the daemon's lives in a short directory
        // and `CODEX_HOME` holds a symlink to it, as Codex lays its own out.
        let short = tempfile::Builder::new().prefix("slopty-codex").tempdir_in("/tmp").unwrap();
        let socket = short.path().join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let codex_home = dir.path().join("codex-home");
        std::fs::create_dir_all(codex_home.join("app-server-control")).unwrap();
        std::os::unix::fs::symlink(
            &socket,
            codex_home.join("app-server-control/app-server-control.sock"),
        )
        .unwrap();
        let (tx, mut heard) = mpsc::unbounded_channel();
        let server = tokio::spawn(app_server(listener, first, tx));
        let daemons = daemons(dir.path(), &codex_home).await;

        let mut slopty = Client::connect(&daemons, ClientId::new()).await;
        let mut other = Client::connect(&daemons, ClientId::new()).await;
        slopty.send(ThreadRequest::Table { have: None }).await;
        let native = follower()
            .iter()
            .find_map(|l| l.msg["result"]["thread"]["id"].as_str().map(str::to_owned))
            .unwrap();
        let thread = shared::thread_of(&native);
        slopty.until(|c| c.table.rows.contains_key(&thread)).await;
        for client in [&mut slopty, &mut other] {
            client
                .send(ThreadRequest::Follow { thread, have: None, turns: 100, max_latency_ms: 0 })
                .await;
        }
        let open = |c: &Client| c.request().is_some_and(slopty_proto::thread::Request::is_open);
        let settled = |c: &Client| c.request().is_some_and(|r| !r.is_open());
        let answer = || Intent::Answer {
            ask: AskId(ASK.to_owned()),
            choice: "accept".to_owned(),
            message: None,
        };
        match first {
            First::Slopty => {
                slopty.until(open).await;
                assert_eq!(slopty.state().status.phase, Phase::NeedsYou);
                assert_eq!(slopty.intent(thread, answer()).await, Outcome::Done);
                slopty.until(settled).await;
            }
            First::Tui => slopty.until(settled).await,
        }
        // The other client answers after it was settled: no error, and nothing goes to Codex.
        other.until(settled).await;
        assert_eq!(other.intent(thread, answer()).await, Outcome::Done, "settled is no error");
        let ended = |c: &Client| {
            c.thread.as_ref().is_some_and(|s| {
                s.turns.len() == 2 && s.turns.iter().all(|t| t.state == TurnState::Complete)
            })
        };
        slopty.until(ended).await;
        other.until(ended).await;
        drop(daemons);
        let _done = tokio::time::timeout(STEP, server).await;
        let mut sent = Vec::new();
        while let Ok(msg) = heard.try_recv() {
            sent.push(msg);
        }
        (sent, slopty, other)
    }

    fn answers(sent: &[Value]) -> Vec<&Value> {
        sent.iter().filter(|m| m.get("result").is_some() || m.get("error").is_some()).collect()
    }

    /// The person approves from Slopty before the TUI does: the worker sends Codex the very
    /// answer the TUI would, both clients see it settled by Slopty, and the other client's
    /// later answer is taken as settled and sent nowhere.
    #[tokio::test]
    async fn an_approval_from_slopty_goes_as_the_tuis_own_answer() {
        let (sent, slopty, other) = shared_approval(First::Slopty).await;
        assert_eq!(answers(&sent), [&json!({ "id": 0, "result": { "decision": "accept" } })]);
        for client in [&slopty, &other] {
            let state = client.request().map(|r| r.state.clone());
            let Some(RequestState::Answered { by, choice }) = state else { panic!("{state:?}") };
            assert_eq!((by.name.as_str(), choice.as_str()), ("Slopty", "accept"));
            assert_eq!(client.state().status.phase, Phase::Done);
        }
    }

    /// The TUI approves first: Slopty's card shows it settled by Codex, and an answer from
    /// Slopty after that is taken as settled, never an error, and never sent.
    #[tokio::test]
    async fn an_approval_the_tui_gave_first_shows_as_settled() {
        let (sent, slopty, other) = shared_approval(First::Tui).await;
        assert!(answers(&sent).is_empty(), "no answer went to Codex: {sent:?}");
        for client in [&slopty, &other] {
            let state = client.request().map(|r| r.state.clone());
            let Some(RequestState::Answered { by, .. }) = state else { panic!("{state:?}") };
            assert_eq!(by.name, shared::ELSEWHERE);
        }
    }
}
