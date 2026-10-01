//! ptyd + worker + this crate's client core on this machine: an observed Claude Code session's
//! thread held by `slopty_client::threads`, through a dropped link and a relaunch.
//!
//! The thread is opened and followed; a held prompt is answered through the outbox, drawn as
//! answered before the worker hears of it. The link drops, the client keeps the thread and an
//! answer it could not send, and a new client core starts from that cache: the thread draws
//! before any frame comes, catches up from its cursor with only the actions it missed, and the
//! answer goes under its first id and is acted on (a second device, following throughout, keeps
//! the prompt held meanwhile). The agent is played by its captured
//! transcript and by `slopty hook` run as Claude Code runs it, as the test's own child; nothing
//! is typed into the shell.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod threads {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_client::threads::{Cache, Changed, Mirror, Threads};
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::{ClientId, SessionId};
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};
    use slopty_proto::thread::wire::{Intent, ThreadFrame};
    use slopty_proto::thread::{AskId, Cursor, RequestState, ThreadId, ThreadState};
    use tokio::io::{AsyncBufReadExt as _, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::mpsc;

    /// Long enough for any step on a loaded machine; the test judges what arrives, not when.
    const STEP: Duration = Duration::from_secs(20);

    /// A binary of this build, found from the profile directory: this package has none of its
    /// own to name (`slopty_testkit::bins`).
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-client-tests").to_string_lossy(), name)
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

    /// `program` from a clean environment with its home at `home`: nothing of the developer's
    /// reaches it.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command
    }

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
        let pasteboard = format!("dev.aislopware.slopty.client-threads.{leaf}");
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
        Daemons { children: vec![ptyd, worker], pasteboard, addr }
    }

    fn fixture(scenario: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../slopty-agent/tests/fixtures/conversation")
            .join(scenario)
    }

    /// `slopty hook` as Claude Code runs it, in `session`, with `payload` on stdin.
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

    async fn printed(child: Child) -> serde_json::Value {
        let out = tokio::time::timeout(STEP, child.wait_with_output()).await.unwrap().unwrap();
        assert!(out.status.success(), "the relay exits 0: {:?}", out.status);
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// One client: a link and the client core it feeds, as the app holds them.
    struct Client {
        _endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        threads: Threads,
        /// The thread frames as they came: snapshot or the actions from a cursor.
        came: Vec<Came>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Came {
        Snapshot,
        Actions { epoch: u64, first: u64 },
    }

    impl Client {
        /// Connected as `id`, with `threads` caught up from wherever they stand.
        async fn connect(daemons: &Daemons, id: ClientId, mut threads: Threads) -> Self {
            let endpoint = bind_client().unwrap();
            let hello = Hello { client: id, name: "client-threads".to_owned() };
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
            for msg in threads.connected() {
                link.send(msg).await.unwrap();
            }
            Self { _endpoint: endpoint, link, events, threads, came: Vec::new() }
        }

        async fn send_all(&self, msgs: impl IntoIterator<Item = ClientMsg>) {
            for msg in msgs {
                self.link.send(msg).await.unwrap();
            }
        }

        /// The next event into the core; a control message it does not take is handed back.
        async fn step(&mut self) -> Option<WorkerMsg> {
            let event = tokio::time::timeout(STEP, self.events.recv()).await.unwrap().unwrap();
            match event {
                LinkEvent::Thread { thread, frame } => {
                    self.came.push(match &frame {
                        ThreadFrame::Actions { epoch, first, .. } => {
                            Came::Actions { epoch: *epoch, first: *first }
                        }
                        _ => Came::Snapshot,
                    });
                    let (_changed, out): (Changed, _) = self.threads.frame(thread, frame);
                    self.send_all(out).await;
                }
                LinkEvent::Control(WorkerMsg::Threads(frame)) => self.threads.table(&frame),
                LinkEvent::Control(WorkerMsg::IntentDone(done)) => {
                    assert!(self.threads.done(&done), "every answer is for an intent sent here");
                }
                LinkEvent::Control(msg) => return Some(msg),
                LinkEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
            None
        }

        async fn until(&mut self, done: impl Fn(&Threads) -> bool) {
            while !done(&self.threads) {
                let _control = self.step().await;
            }
        }

        async fn heard<T>(&mut self, mut pick: impl FnMut(WorkerMsg) -> Option<T>) -> T {
            loop {
                if let Some(found) = self.step().await.and_then(&mut pick) {
                    return found;
                }
            }
        }
    }

    fn state(threads: &Threads, thread: ThreadId) -> Option<&ThreadState> {
        threads.mirror(thread).and_then(Mirror::state)
    }

    fn open_ask(threads: &Threads, thread: ThreadId, not: Option<&AskId>) -> Option<AskId> {
        let state = state(threads, thread)?;
        state.open_requests().map(|r| r.id.clone()).find(|id| Some(id) != not)
    }

    fn answered(threads: &Threads, thread: ThreadId, ask: &AskId) -> bool {
        state(threads, thread).is_some_and(|s| {
            s.requests
                .iter()
                .any(|r| r.id == *ask && matches!(r.state, RequestState::Answered { .. }))
        })
    }

    #[tokio::test]
    async fn a_thread_and_an_answer_outlive_a_dropped_link_and_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let cache = Cache::new(dir.path().join("client").join("worker"));
        let me = ClientId::new();
        let mut a = Client::connect(&daemons, me, Threads::new(cache.outbox())).await;
        let spec = OpenSession {
            size: TermSize { cols: 80, rows: 24, ..TermSize::default() },
            cwd: Some(dir.path().to_string_lossy().into_owned()),
            command: vec!["/bin/sh".to_owned()],
            env: vec![("PS1".to_owned(), "$ ".to_owned())],
            title: None,
            attach: false,
        };
        a.link.send(ClientMsg::OpenSession { request: 1, spec }).await.unwrap();
        let session = a
            .heard(|msg| match msg {
                WorkerMsg::SessionOpened { summary, .. } => Some(summary.id),
                _ => None,
            })
            .await;

        // The captured `permission` session: its transcript and its hooks, prompts and all.
        let main = dir.path().join("projects").join("s1.jsonl");
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
        let _started = relay(dir.path(), session, &start).wait_with_output().await.unwrap();
        let hooks = std::fs::read_to_string(fixture("permission").join("hooks.jsonl")).unwrap();
        let mut prompts = hooks
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .map(|l| l["input"].clone())
            .filter(|input| input["hook_event_name"] == "PermissionRequest")
            .map(|mut ask| {
                ask["transcript_path"] = transcript.clone().into();
                ask["session_id"] = "s1".into();
                ask
            });

        // The thread shows in the table and is opened; a prompt is held for this follower.
        let thread = slopty_agent::observed::thread_of("s1");
        a.until(|t| t.rows().rows.contains_key(&thread)).await;
        let follow = a.threads.open_thread(thread, None);
        a.send_all(follow).await;
        a.until(|t| state(t, thread).is_some()).await;
        let held = relay(dir.path(), session, &prompts.next().unwrap());
        a.until(|t| open_ask(t, thread, None).is_some()).await;
        let ask = open_ask(&a.threads, thread, None).unwrap();

        // Answered through the outbox: drawn as answered before the worker hears of it.
        let allow = |ask: &AskId| Intent::Answer {
            ask: ask.clone(),
            choice: "allow".to_owned(),
            message: None,
        };
        let (_id, msg) = a.threads.intent(thread, allow(&ask));
        assert!(a.threads.answering(thread, &ask).is_some(), "flipped in the same frame");
        a.send_all(msg).await;
        assert_eq!(printed(held).await["hookSpecificOutput"]["decision"]["behavior"], "allow");
        a.until(|t| answered(t, thread, &ask) && t.outbox().all().is_empty()).await;

        // A second device follows throughout, so the next prompt is held while this one is
        // away, as a phone keeps it held.
        let mut witness = Client::connect(&daemons, ClientId::new(), Threads::default()).await;
        let follow = witness.threads.open_thread(thread, None);
        witness.send_all(follow).await;
        let at = a.threads.mirror(thread).and_then(Mirror::cursor);
        witness.until(|t| t.mirror(thread).and_then(Mirror::cursor) == at).await;

        // A second prompt; the link drops before its answer can go, and the agent goes on.
        let second = relay(dir.path(), session, &prompts.next().unwrap());
        a.until(|t| open_ask(t, thread, Some(&ask)).is_some()).await;
        let next = open_ask(&a.threads, thread, Some(&ask)).unwrap();
        a.link.close();
        a.threads.disconnected();
        let (later, msg) = a.threads.intent(thread, allow(&next));
        assert!(msg.is_none(), "nothing goes on a dropped link");
        let kept = a.threads.to_cache(thread).unwrap();
        cache.keep_thread(thread, &kept).unwrap();
        cache.keep_outbox(a.threads.outbox()).unwrap();
        drop(a);
        let mut file = std::fs::OpenOptions::new().append(true).open(&main).unwrap();
        std::io::Write::write_all(&mut file, format!("{}\n", rest.join("\n")).as_bytes()).unwrap();

        // A relaunch: the core starts from the cache, and the thread draws before any frame.
        let mut threads = Threads::new(cache.outbox());
        assert_eq!(threads.outbox().unanswered().map(|s| s.id).collect::<Vec<_>>(), [later]);
        assert!(threads.open_thread(thread, cache.thread(thread)).is_none(), "not linked yet");
        assert_eq!(state(&threads, thread), Some(&kept.state), "drawn from the cache");
        assert!(threads.answering(thread, &next).is_some(), "the unsent answer is drawn too");

        // Linked again: only what was missed comes, and the answer goes under its first id.
        let mut b = Client::connect(&daemons, me, threads).await;
        assert_eq!(printed(second).await["hookSpecificOutput"]["decision"]["behavior"], "allow");
        b.until(|t| answered(t, thread, &next) && t.outbox().all().is_empty()).await;
        let cursor: Cursor = kept.cursor;
        assert_eq!(
            b.came.first(),
            Some(&Came::Actions { epoch: cursor.epoch, first: cursor.seq }),
            "caught up from the cached cursor, not sent a snapshot: {:?}",
            b.came
        );
        assert!(!b.came.contains(&Came::Snapshot), "{:?}", b.came);
        let grown = kept.state.items.len();
        b.until(|t| state(t, thread).is_some_and(|s| s.items.len() > grown)).await;

        b.link.send(ClientMsg::Term { session, req: TermRequest::Close }).await.unwrap();
        drop(file);
    }
}
