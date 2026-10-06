//! ptyd + worker + real client links on this machine: an observed Claude Code session's thread
//! served over the link. The table and the thread are followed from a cursor, the link drops
//! and comes back, and only what it missed is sent; an intent sent again after the drop is
//! answered as the first time and acts on nothing. The agent is played by its captured
//! transcript, written here, and by `slopty hook` run as Claude Code runs it, as the test's own
//! child; nothing is typed into the shell.
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

    use slopty_agent::conversation::{Conversation, ThreadId as ConvThread};
    use slopty_agent::transcript::Tail;
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_proto::thread::wire::{
        Expanded, Intent, IntentDone, NewWorktree, Outcome, PastSessions, Start, TableFrame,
        ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        AgentId, AskId, Cap, Cursor, IntentId, Request, RequestState, TableState, ThreadId,
        ThreadState,
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
        while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
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
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
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
        let out = child.wait_with_output().await.unwrap();
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
                        Err(slopty_net::NetError::Connect(why)) if why.ends_with("no answer") => {}
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

        /// Keep the thread table from now on, where every thread's requests show: this client
        /// answers a yes or no whoever follows the thread.
        async fn answer_requests(&self) {
            self.send(ThreadRequest::Table { have: None }).await;
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
        let recorded = async |want: &str| {
            let mut seen = read();
            while seen != want {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let now = read();
                if now != seen {
                    eprintln!("recorded {now:?}, waiting for {want:?}");
                }
                seen = now;
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
            attachments: vec![],
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

    /// A review by the agent, on the person's word, goes to Claude Code as their turn: its own
    /// `/code-review` over the change's range, typed as any command of theirs is. The thread
    /// can review because Claude Code lists `code-review`, here a project command of the
    /// folder's; the range is two commits the worker made of the trees the review showed.
    #[tokio::test]
    async fn a_review_by_claude_code_is_its_own_code_review_over_the_range() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git = |args: &[&str]| {
            let out =
                std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        std::fs::create_dir_all(repo.join(".claude/commands")).unwrap();
        std::fs::write(repo.join(".claude/commands/code-review.md"), "Review the diff\n").unwrap();
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        let from = git(&["write-tree"]);
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        let to = git(&["write-tree"]);

        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let record = dir.path().join("record");
        let script = r#"stty raw -echo; printf ready >"$0"; exec cat >>"$0""#;
        let command = ["/bin/sh", "-c", script, &record.to_string_lossy()].map(str::to_owned);
        let session = open(&mut a, &repo, command.to_vec()).await;
        let read = || std::fs::read_to_string(&record).unwrap_or_default();
        while read() != "ready" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::write(&record, "").unwrap();
        let main = dir.path().join("s3.jsonl");
        std::fs::write(&main, "").unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s3",
            "transcript_path": main, "cwd": repo,
        });
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");
        let thread = slopty_agent::observed::thread_of("s3");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        a.follow(thread).await;
        a.until(|c| c.thread.as_ref().is_some_and(|t| t.meta.can(Cap::REVIEW))).await;

        let review = Intent::Review {
            from: slopty_proto::thread::TreeRef(from.clone()),
            to: slopty_proto::thread::TreeRef(to.clone()),
        };
        assert_eq!(a.intent(IntentId::new(), thread, review).await, Outcome::Accepted);
        let (base, head) = (
            git(&["rev-parse", &format!("refs/slopty/threads/{thread}/review-base")]),
            git(&["rev-parse", &format!("refs/slopty/threads/{thread}/review-head")]),
        );
        let typed = format!("/code-review {base}...{head}\r");
        while read() != typed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            (
                git(&["rev-parse", &format!("{base}^{{tree}}")]),
                git(&["rev-parse", &format!("{head}^{{tree}}")])
            ),
            (from, to)
        );
        a.until(|c| c.state().pending.is_empty()).await;
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
        a.answer_requests().await;
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
        assert_ne!(whole, Vec::<String>::new());
        a.until(|c| c.thread.as_ref().is_some_and(|s| ids(s) == whole)).await;
        assert_eq!(a.state().open_requests().count(), 1, "the request outlives the read");

        let request = a.state().open_requests().next().unwrap().id.clone();
        let answer = Intent::Answer { ask: request, choice: "allow".to_owned(), message: None };
        assert_eq!(a.intent(IntentId::new(), thread, answer).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        assert_eq!(output["hookSpecificOutput"]["decision"]["behavior"], "allow");
        a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// Two clients follow one Claude Code thread and both answer requests. The second denies
    /// a permission prompt with a word for the model: Claude Code is told to deny with it, both
    /// clients see the request answered by the second with that choice, and the first's
    /// answer after it moves nothing.
    #[tokio::test]
    async fn a_denial_from_one_of_two_clients_settles_the_request_for_both() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let second = ClientId::new();
        let mut b = Client::connect(&daemons, second).await;
        a.answer_requests().await;
        b.answer_requests().await;
        let session = open_shell(&mut a, dir.path()).await;
        let main = dir.path().join("p1.jsonl");
        std::fs::write(&main, "").unwrap();

        let held = relay(dir.path(), session, &first_ask("p1", &main));
        let thread = slopty_agent::observed::thread_of("p1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| row_in(c, session).is_some_and(|r| r.requests.len() == 1)).await;
        a.follow(thread).await;
        b.follow(thread).await;
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.open_requests().count() == 1)).await;
        b.until(|c| c.thread.as_ref().is_some_and(|s| s.open_requests().count() == 1)).await;

        let request = b.state().open_requests().next().unwrap().id.clone();
        let deny = Intent::Answer {
            ask: request.clone(),
            choice: "deny".to_owned(),
            message: Some("Not on main".to_owned()),
        };
        assert_eq!(b.intent(IntentId::new(), thread, deny).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        let decision = &output["hookSpecificOutput"]["decision"];
        assert_eq!(decision["behavior"], "deny", "{output}");
        assert_eq!(decision["message"], "Not on main", "{output}");
        let denied_by_b = |c: &Client| {
            c.thread.as_ref().is_some_and(|s| {
                s.requests.iter().any(|r| {
                    r.id == request
                        && matches!(&r.state, RequestState::Answered { by, choice }
                            if by.client == Some(second) && choice == "deny")
                })
            })
        };
        b.until(denied_by_b).await;
        a.until(denied_by_b).await;

        let allow =
            Intent::Answer { ask: request.clone(), choice: "allow".to_owned(), message: None };
        let late = a.intent(IntentId::new(), thread, allow).await;
        assert!(matches!(late, Outcome::Refused { .. }), "{late:?}");
        assert!(denied_by_b(&a), "the late answer moves nothing");
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
        a.answer_requests().await;
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

    /// A held prompt's request on the thread, once there is one open.
    ///
    /// A loaded machine can open the block's "asked in the terminal" card first, when the hook
    /// comes after [`slopty_agent::observed::ASK_GRACE`]; the held prompt then withdraws it. So
    /// the wait is for the one open request to be the held one.
    /// The one prompt the thread holds open, past `answered`: an answer's outcome can come back
    /// before the frame that closes its request, so the request just answered is no new one.
    async fn asked(client: &mut Client, answered: &[&Request]) -> Request {
        let fresh = |r: &Request| {
            !r.id.0.starts_with("terminal-") && answered.iter().all(|done| done.id != r.id)
        };
        client
            .until(|c| {
                c.thread.as_ref().is_some_and(|s| {
                    s.open_requests().count() == 1 && s.open_requests().all(&fresh)
                })
            })
            .await;
        client.state().open_requests().next().unwrap().clone()
    }

    /// A prompt is held while a client follows the session's thread, shown there as a request,
    /// and nobody else's: answered "always" from the thread, Claude Code is told to allow with
    /// what the suggestions grant, and a second answer finds nothing. One whose relay goes away
    /// is withdrawn, and one the last follower leaves goes back to the TUI undecided. With
    /// nobody following and nobody keeping the table, the relay is let go at once. The
    /// follower keeps no table: a second client finds the thread for it.
    #[tokio::test]
    async fn a_prompt_is_held_while_its_thread_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let main = dir.path().join("s1.jsonl");
        let captured = std::fs::read_to_string(fixture("tools").join("transcript.jsonl")).unwrap();
        std::fs::write(&main, captured).unwrap();
        let ask = first_ask("s1", &main);

        let started = std::time::Instant::now();
        assert_eq!(printed(relay(dir.path(), session, &ask)).await, "", "nobody answers");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

        let thread = slopty_agent::observed::thread_of("s1");
        let mut finder = Client::connect(&daemons, ClientId::new()).await;
        finder.send(ThreadRequest::Table { have: None }).await;
        finder.until(|c| c.table.rows.contains_key(&thread)).await;
        finder.leave().await;
        a.follow(thread).await;
        a.until(|c| c.thread.is_some()).await;

        let held = relay(dir.path(), session, &ask);
        let first = asked(&mut a, &[]).await;
        let always =
            Intent::Answer { ask: first.id.clone(), choice: "always".to_owned(), message: None };
        assert_eq!(a.intent(IntentId::new(), thread, always.clone()).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        let decision = &output["hookSpecificOutput"]["decision"];
        assert_eq!(decision["behavior"], "allow", "{output}");
        assert_eq!(decision["updatedPermissions"], ask["permission_suggestions"], "{output}");
        let again = a.intent(IntentId::new(), thread, always).await;
        assert!(matches!(again, Outcome::Refused { .. }), "{again:?}");

        let mut gone = relay(dir.path(), session, &ask);
        let request = asked(&mut a, &[&first]).await;
        gone.start_kill().unwrap();
        let withdrawn = |c: &Client| {
            c.state()
                .requests
                .iter()
                .any(|r| r.id == request.id && r.state == RequestState::Withdrawn)
        };
        a.until(withdrawn).await;

        let released = relay(dir.path(), session, &ask);
        asked(&mut a, &[&first, &request]).await;
        a.send(ThreadRequest::Unfollow { thread }).await;
        assert_eq!(printed(released).await, "", "no decision: the TUI's dialog");
        a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// A client that keeps the thread table, following nothing, answers a yes or no from its
    /// row, and the relay prints its answer; a question is not held for it. A hold for it ends
    /// a second before the relay's wait would, undecided; it can hand a held prompt to the TUI
    /// at once; and once it leaves, what only it could answer goes back to the TUI and the
    /// next prompt is let go at once. The bounded hold is a control-socket request, as the
    /// relay's.
    #[tokio::test]
    async fn the_tables_holder_answers_without_following_and_the_tui_asks_otherwise() {
        use slopty_proto::ctl::{CtlReply, CtlRequest, Decision, PermissionAnswer, PermissionAsk};
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        a.answer_requests().await;
        let session = open_shell(&mut a, dir.path()).await;
        let main = dir.path().join("s1.jsonl");
        std::fs::write(&main, "").unwrap();
        let ask = first_ask("s1", &main);
        let thread = slopty_agent::observed::thread_of("s1");
        // The request on the thread's row that is not one of `done`, once one is open.
        let fresh = |c: &Client, done: &[AskId]| {
            let row = c.table.rows.get(&thread)?;
            row.requests.iter().map(|r| r.id.clone()).find(|id| !done.contains(id))
        };
        let mut done = Vec::new();

        let held = relay(dir.path(), session, &ask);
        a.until(|c| fresh(c, &done).is_some()).await;
        let request = fresh(&a, &done).unwrap();
        done.push(request.clone());
        let allow = Intent::Answer { ask: request, choice: "allow".to_owned(), message: None };
        assert_eq!(a.intent(IntentId::new(), thread, allow).await, Outcome::Done);
        let output: serde_json::Value = serde_json::from_str(&printed(held).await).unwrap();
        assert_eq!(output["hookSpecificOutput"]["decision"]["behavior"], "allow");

        let mut question = ask.clone();
        question["tool_name"] = "AskUserQuestion".into();
        let started = std::time::Instant::now();
        assert_eq!(printed(relay(dir.path(), session, &question)).await, "");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());

        let bounded = CtlRequest::Permission(PermissionAsk {
            session,
            payload: ask.to_string(),
            wait_ms: 2_500,
        });
        let started = std::time::Instant::now();
        let sock = dir.path().join("worker.sock");
        let reply = tokio::spawn(async move { ctl(&sock, &bounded).await });
        a.until(|c| fresh(c, &done).is_some()).await;
        done.push(fresh(&a, &done).unwrap());
        let reply = reply.await.unwrap();
        assert_eq!(reply, CtlReply::Permission(PermissionAnswer { decision: Decision::Pass }));
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(1_400), "{waited:?}");

        let handed = relay(dir.path(), session, &ask);
        a.until(|c| fresh(c, &done).is_some()).await;
        let request = fresh(&a, &done).unwrap();
        done.push(request.clone());
        let release = Intent::Release { ask: request };
        assert_eq!(a.intent(IntentId::new(), thread, release).await, Outcome::Done);
        assert_eq!(printed(handed).await, "", "no decision: the TUI's dialog");

        // The terminal stays open: its end would release the prompt by itself.
        let orphaned = relay(dir.path(), session, &ask);
        a.until(|c| fresh(c, &done).is_some()).await;
        a.leave().await;
        assert_eq!(printed(orphaned).await, "", "nobody is left to answer");
        let started = std::time::Instant::now();
        assert_eq!(printed(relay(dir.path(), session, &ask)).await, "", "nobody answers now");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    /// How soon a hook's status reaches a client in its thread's row. 200 hooks, a prompt and
    /// a stop in turn, each played to the control socket from a task of its own while the
    /// client reads; each time is from the hook's sending to the client's receipt.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "a measurement: cargo test -p slopty-workerd --release --test threads \
                a_hooks_status_reaches_a_client_in_its_row -- --ignored --nocapture"]
    async fn a_hooks_status_reaches_a_client_in_its_row() {
        use slopty_proto::ctl::CtlRequest;
        use slopty_proto::thread::Phase;

        const HOOKS: usize = 200;
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let transcript = dir.path().join("projects").join("m1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "").unwrap();
        let sock = dir.path().join("worker.sock");
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "m1",
            "transcript_path": transcript, "cwd": dir.path(),
        });
        ctl(&sock, &CtlRequest::Hook { session, payload: start.to_string() }).await;
        let thread = slopty_agent::observed::thread_of("m1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;

        let mut times = Vec::new();
        for i in 0..HOOKS {
            let (name, phase) = if i % 2 == 0 {
                ("UserPromptSubmit", Phase::Working)
            } else {
                ("Stop", Phase::Done)
            };
            let payload =
                serde_json::json!({"hook_event_name": name, "session_id": "m1", "prompt": "go"});
            let sent = std::time::Instant::now();
            let sock = sock.clone();
            let played = tokio::spawn(async move {
                ctl(&sock, &CtlRequest::Hook { session, payload: payload.to_string() }).await
            });
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while a.table.rows.get(&thread).is_none_or(|r| r.status.phase != phase) {
                let _msg = tokio::time::timeout_at(deadline, a.step()).await.unwrap_or_else(|_| {
                    let row = a.table.rows.get(&thread).map(|r| r.status.phase);
                    panic!("hook {i} ({name}): the row is at {row:?}")
                });
            }
            times.push(sent.elapsed());
            played.await.unwrap();
        }
        times.sort_unstable();
        // The nearest rank: `per_mille` thousandths of the way along the sorted times.
        let at = |per_mille: usize| {
            let i = (times.len() - 1).saturating_mul(per_mille).div_ceil(1000);
            times[i].as_secs_f64() * 1000.0
        };
        println!(
            "thread row: p50 {:.2} / p90 {:.2} / p99 {:.2} / max {:.2} ms over {}",
            at(500),
            at(900),
            at(990),
            at(1000),
            times.len()
        );
    }

    /// One request to the worker's control socket, as the relay and the CLI write it.
    async fn ctl(
        sock: &Path,
        request: &slopty_proto::ctl::CtlRequest,
    ) -> slopty_proto::ctl::CtlReply {
        use tokio::io::AsyncWriteExt as _;
        let mut stream = tokio::net::UnixStream::connect(sock).await.unwrap();
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        stream.write_all(&line).await.unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).await.unwrap();
        serde_json::from_str(reply.trim()).unwrap()
    }

    /// Where the agent runs Slopty's mod, a follower of its thread sees what the model writes
    /// before the transcript has it: nothing until a hello from a verified Claude Code, then
    /// each block as it grows, gone once the transcript's entry for it has come. The events are
    /// the official build's own (`cargo xtask fixtures claude-mod`), posted to the mod socket.
    #[tokio::test]
    async fn a_trusted_mod_streams_live_blocks_that_the_transcript_settles() {
        use slopty_proto::thread::{ItemBody, ToolState};
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let recorded = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/slopty-agent/tests/fixtures/mod/bash");
        let main = dir.path().join("projects/s1.jsonl");
        std::fs::create_dir_all(main.parent().unwrap()).unwrap();
        std::fs::write(&main, "").unwrap();
        let start = serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": main, "cwd": dir.path(),
        });
        assert_eq!(printed(relay(dir.path(), session, &start)).await, "");
        let thread = slopty_agent::observed::thread_of("s1");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.contains_key(&thread)).await;
        a.follow(thread).await;
        a.until(|c| c.thread.is_some()).await;

        let socket = dir.path().join("worker.mod.sock");
        let batches: Vec<serde_json::Value> =
            std::fs::read_to_string(recorded.join("events.jsonl"))
                .unwrap()
                .lines()
                .map(|line| {
                    let mut batch: serde_json::Value = serde_json::from_str(line).unwrap();
                    batch["session"] = serde_json::Value::String(session.to_string());
                    batch
                })
                .collect();
        let has = |batch: &serde_json::Value, kind: &str| {
            batch["events"].as_array().unwrap().iter().any(|e| e["kind"] == kind)
        };
        let stops: Vec<usize> =
            batches.iter().enumerate().filter(|(_, b)| has(b, "stop")).map(|(i, _)| i).collect();
        let [first_stop, second_stop] = stops[..] else { panic!("two steps: {stops:?}") };
        let bye = batches.iter().position(|b| has(b, "bye")).unwrap();
        let hello = batches.iter().position(|b| has(b, "hello")).unwrap();
        let first_text = batches.iter().position(|b| has(b, "text")).unwrap();

        // A piece before any hello, and after a hello from a Claude Code the mod was not
        // verified against: neither shows.
        assert_eq!(post(&socket, &batches[first_text]).await, 204);
        let mut unknown = batches[hello].clone();
        unknown["events"][0]["claude"] = "0.0.1".into();
        assert_eq!(post(&socket, &unknown).await, 204);
        assert_eq!(post(&socket, &batches[first_text]).await, 204);
        let mut elsewhere = batches[first_text].clone();
        elsewhere["session"] = "00000000-0000-4000-8000-000000000000".into();
        assert_eq!(post(&socket, &elsewhere).await, 404, "no such session here");

        // What the model is writing: its answers, and the calls whose input still streams.
        let texts = |s: &ThreadState| -> Vec<String> {
            s.items
                .iter()
                .filter(|i| i.id.0.starts_with("live:"))
                .filter_map(|i| match &i.body {
                    ItemBody::Text(text) => Some(text.text.clone()),
                    _ => None,
                })
                .collect()
        };
        let streaming = |s: &ThreadState| -> Vec<String> {
            s.items
                .iter()
                .filter_map(|i| match &i.body {
                    ItemBody::Tool(call) if call.state == ToolState::Streaming => {
                        Some(call.input.text.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        let live = |s: &ThreadState| texts(s).len() + streaming(s).len();

        // Written now, not when it was recorded: an entry stamped long before the follower saw
        // the block is an older one, so the stamps go.
        let transcript: Vec<String> = std::fs::read_to_string(recorded.join("transcript.jsonl"))
            .unwrap()
            .lines()
            .map(|line| {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                record.as_object_mut().unwrap().remove("timestamp");
                record.to_string()
            })
            .collect();
        let append = |lines: &[String]| {
            let mut file = std::fs::OpenOptions::new().append(true).open(&main).unwrap();
            std::io::Write::write_all(&mut file, format!("{}\n", lines.join("\n")).as_bytes())
                .unwrap();
            std::time::Instant::now()
        };
        // Claude Code writes the prompt before the model answers it.
        let prompt = transcript.iter().position(|l| l.contains(r#""type":"user""#)).unwrap();
        let (asked, rest) = transcript.split_at(prompt + 1);
        let _written = append(asked);
        let prompted =
            |c: &Client| c.state().items.iter().any(|i| matches!(i.body, ItemBody::User(_)));
        a.until(prompted).await;

        // The first step, as the model writes it: the answer, then the Bash call's input.
        for batch in &batches[..first_stop] {
            assert_eq!(post(&socket, batch).await, 204);
        }
        let first_step = |c: &Client| {
            let s = c.state();
            texts(s) == ["Let me run it."]
                && streaming(s) == [r#"{"command": "echo hi", "description": "Say hi"}"#]
        };
        a.until(first_step).await;
        let shown = &a.state().items;
        assert_eq!(shown.len(), 3, "the prompt, and nothing more of the transcript yet");
        assert!(matches!(shown[0].body, ItemBody::User(_)), "the blocks follow their prompt");

        // The step stops and the transcript gets the answer and the call: both settle.
        assert_eq!(post(&socket, &batches[first_stop]).await, 204);
        let result = rest.iter().position(|l| l.contains(r#""type":"tool_result""#)).unwrap();
        let (before, after) = rest.split_at(result);
        // Settled by the entries, well before the grace that clears a block nothing settles.
        let soon = slopty_agent::live::SETTLE_GRACE / 2;
        let written = append(before);
        let answered = |s: &ThreadState| {
            s.items
                .iter()
                .any(|i| !i.id.0.starts_with("live:") && matches!(i.body, ItemBody::Text(_)))
        };
        a.until(|c| live(c.state()) == 0 && answered(c.state())).await;
        assert!(written.elapsed() < soon, "settled in {:?}", written.elapsed());

        // The second step streams, and settles when the transcript has the rest.
        for batch in &batches[first_stop + 1..second_stop] {
            assert_eq!(post(&socket, batch).await, 204);
        }
        a.until(|c| texts(c.state()).iter().any(|t| t == "Done: the command said hi.")).await;
        for batch in &batches[second_stop..bye] {
            assert_eq!(post(&socket, batch).await, 204);
        }
        let written = append(after);
        a.until(|c| live(c.state()) == 0).await;
        assert!(written.elapsed() < soon, "settled in {:?}", written.elapsed());
        let whole = entries(&main);
        // Every entry the transcript has, and nothing the mod wrote beside them.
        let settled = |c: &Client| {
            let mut now = ids(c.state());
            let mut want = whole.clone();
            now.sort();
            want.sort();
            now == want
        };
        a.until(settled).await;
        a.until(|c| c.state().meters.context_window.is_some()).await;
        assert_eq!(a.state().meters.context_window, Some(200_000), "the mod's measure");
        assert_eq!(post(&socket, &batches[bye]).await, 204);

        a.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
    }

    /// Post one batch to the mod socket as the mod does; the answer's status.
    async fn post(socket: &Path, batch: &serde_json::Value) -> u16 {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let body = batch.to_string();
        let head = format!(
            "POST /v1/events HTTP/1.1\r\nhost: slopty\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        answer.split_whitespace().nth(1).and_then(|code| code.parse().ok()).unwrap_or(0)
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
            worktree: None,
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

    /// A start naming a worktree opens its agent in it: the worker makes it from the clone the
    /// start's folder is in, where Claude Code would, and the agent starts there. A start in a
    /// folder that is in no repository is refused in words, and nothing opens.
    #[tokio::test]
    async fn a_start_in_a_worktree_opens_its_agent_there() {
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
            args: Vec::new(),
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
        assert!(matches!(outcome, Outcome::Started { .. }), "{outcome:?}");
        let tree = clone.join(".claude/worktrees/claude-c0ffee");
        assert!(tree.join(".git").is_file(), "a worktree of the clone");
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], tree.to_string_lossy().as_ref(), "the agent works in it");
    }

    /// The palette's start: no first message and a folder under the worker's home spelled
    /// with `~`, as a client that knows no home writes it. Claude Code opens in the home with
    /// nothing on its command line after its own flags, waiting for the person. The terminal it
    /// runs in is news for the client, as one it opened would be: the client shows the agent
    /// there.
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
            worktree: None,
        };
        a.send(ThreadRequest::Start { id, start: Box::new(start) }).await;
        let mut told = Vec::new();
        let outcome = a
            .heard(|msg| match msg {
                WorkerMsg::IntentDone(IntentDone { id: done, outcome }) if done == id => {
                    Some(outcome)
                }
                WorkerMsg::SessionChanged(summary) => {
                    told.push(summary);
                    None
                }
                _ => None,
            })
            .await;
        let Outcome::Started { thread } = outcome else { panic!("started: {outcome:?}") };
        let seen = recorded(&record).await;
        assert_eq!(seen["cwd"], root.to_string_lossy().as_ref(), "`~` is the worker's home");
        let argv: Vec<&str> =
            seen["argv"].as_array().unwrap().iter().filter_map(|a| a.as_str()).collect();
        assert!(!argv.contains(&"--"), "no first message: {argv:?}");
        a.send(ThreadRequest::Table { have: None }).await;
        a.until(|c| c.table.rows.get(&thread).is_some_and(|r| r.terminal.is_some())).await;
        let terminal = a.table.rows[&thread].terminal.expect("the table names its terminal");
        let first = told.iter().find(|s| s.id == terminal).cloned();
        let summary = if let Some(summary) = first {
            summary
        } else {
            let told = a.heard(|msg| match msg {
                WorkerMsg::SessionChanged(summary) if summary.id == terminal => Some(summary),
                _ => None,
            });
            tokio::time::timeout(Duration::from_secs(10), told)
                .await
                .expect("the client is told of the agent's terminal")
        };
        assert_eq!(summary.state, slopty_proto::terminal::SessionState::Running, "{summary:?}");
    }

    /// A Claude Code session works where its hooks say, whatever folder its terminal reports:
    /// its thread's folder is the agent's own, known before any transcript is, and the slash
    /// commands the composer offers are the project's own there (with no mod heard, nothing of
    /// Claude Code's own list is known).
    #[tokio::test]
    async fn a_session_works_where_its_hooks_say() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let mut a = Client::connect(&daemons, ClientId::new()).await;
        let session = open_shell(&mut a, dir.path()).await;
        let work = std::fs::canonicalize(dir.path()).unwrap().join("work");
        std::fs::create_dir_all(work.join(".claude/commands")).unwrap();
        std::fs::write(work.join(".claude/commands/ship.md"), "Ship the build\n").unwrap();
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
        // The project's own command there.
        a.until(|c| c.thread.as_ref().is_some_and(|s| s.commands.iter().any(|c| c.name == "ship")))
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

    /// A folder's past Claude Code sessions are listed from the names and times of the
    /// transcripts under the worker's home alone, the last written first, and one Slopty keeps a
    /// thread of is named by it; an agent the worker cannot list says why in words.
    #[tokio::test]
    async fn past_sessions_are_listed_per_agent_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let project = slopty_agent::discover::project_dir(&root, &work);
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
        let project = slopty_agent::discover::project_dir(&root, &work);
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
