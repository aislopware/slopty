//! ptyd + worker + this crate's client core on this machine: a Claude Code session held by
//! `slopty_client::threads`, through a dropped link, a relaunch and a cut network.
//!
//! The session is started in a terminal of the worker and its thread followed; the person names
//! it through the outbox, drawn as named before the worker hears of it. The link drops, the
//! client keeps the thread and a name it could not send, and a new client core starts from that
//! cache: the thread draws before any frame comes, catches up from its cursor with only the
//! actions it missed, and the name goes under its first id and is acted on once. Claude Code is
//! the stand-in TUI (`slopty-stub-claude`) first on the daemons' `PATH`.

#![cfg(target_vendor = "apple")]

#[cfg(test)]
mod threads {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use slopty_client::threads::{Cache, Changed, Mirror, Threads};
    use slopty_client::{LinkEvent, WorkerLink};
    use slopty_core::ClientId;
    use slopty_net::client::{bind_client, connect_addr};
    use slopty_net::{ClientMsg, WorkerMsg};
    use slopty_proto::handshake::Hello;
    use slopty_proto::thread::wire::{
        Against, Intent, IntentDone, Outcome, ReviewScope, Start, ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{AgentId, Cursor, IntentId, ThreadId, ThreadState, TurnState};
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
        /// Where the threads work.
        work: PathBuf,
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

    /// `program` from a clean environment with its home at `home`, the stand-in `claude` first
    /// on its `PATH`: nothing of the developer's reaches it.
    fn scrubbed(program: impl AsRef<std::ffi::OsStr>, home: &Path) -> Command {
        let mut command = Command::new(program);
        slopty_testkit::env::scrub(command.as_std_mut(), home);
        command.env("PATH", slopty_testkit::env::path_with(&home.join("programs")));
        command
    }

    async fn daemons(dir: &Path) -> Daemons {
        let dir = std::fs::canonicalize(dir).unwrap();
        let programs = dir.join("programs");
        std::fs::create_dir_all(&programs).unwrap();
        std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let ptyd_sock = dir.join("ptyd.sock");
        let mut ptyd = scrubbed(bin("slopty-ptyd"), &dir)
            .arg("--socket")
            .arg(&ptyd_sock)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        slopty_testkit::group::ward(ptyd.id());
        let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
        while tokio::net::UnixStream::connect(&ptyd_sock).await.is_err() {
            assert!(ptyd.try_wait().unwrap().is_none(), "ptyd exited");
            assert!(tokio::time::Instant::now() < deadline, "ptyd never listened");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let pasteboard = format!("dev.aislopware.slopty.client-threads.{leaf}");
        let mut worker = scrubbed(bin("slopty-worker"), &dir)
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
        slopty_testkit::group::ward(worker.id());
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
        Daemons { children: vec![ptyd, worker], pasteboard, addr, work }
    }

    /// A Claude Code start in the daemons' work folder with `prompt`.
    fn start(daemons: &Daemons, prompt: &str) -> Box<Start> {
        Box::new(Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: daemons.work.to_string_lossy().into_owned(),
            drive: None,
            prompt: Some(prompt.to_owned()),
            model: None,
            mode: None,
            effort: None,
            args: Vec::new(),
            worktree: None,
        })
    }

    /// The person's name `title` for the thread, sent through the core.
    fn name(title: &str) -> Intent {
        Intent::Name { title: title.to_owned() }
    }

    /// One client: a link and the client core it feeds, as the app holds them.
    struct Client {
        _endpoint: slopty_net::Endpoint,
        link: WorkerLink,
        events: mpsc::Receiver<LinkEvent>,
        threads: Threads,
        /// The thread frames as they came: snapshot or the actions from a cursor.
        came: Vec<Came>,
        /// The worker's answers to this client's intents, as they came.
        done: Vec<IntentDone>,
        /// The starts this client asked for, which the core does not send.
        starts: Vec<IntentId>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Came {
        Snapshot,
        Actions { epoch: u64, first: u64 },
    }

    impl Client {
        /// Connected as `id`, with `threads` caught up from wherever they stand.
        async fn connect(daemons: &Daemons, id: ClientId, threads: Threads) -> Self {
            Self::connect_to(daemons.addr, id, threads).await
        }

        /// [`Self::connect`] at `addr`, dialled again until it answers.
        async fn connect_to(addr: SocketAddr, id: ClientId, mut threads: Threads) -> Self {
            let endpoint = bind_client().unwrap();
            let hello = Hello { client: id, name: "client-threads".to_owned() };
            let dialing = async {
                loop {
                    match connect_addr(&endpoint, addr, hello.clone()).await {
                        Err(slopty_net::NetError::NoAnswer(_)) => {}
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
            Self {
                _endpoint: endpoint,
                link,
                events,
                threads,
                came: Vec::new(),
                done: Vec::new(),
                starts: Vec::new(),
            }
        }

        async fn send_all(&self, msgs: impl IntoIterator<Item = ClientMsg>) {
            for msg in msgs {
                self.link.send(msg).await.unwrap();
            }
        }

        /// Start a Claude Code thread with `start`: its id once the worker answers.
        async fn start(&mut self, start: Box<Start>) -> ThreadId {
            let id = IntentId::new();
            self.starts.push(id);
            self.send_all([ClientMsg::Thread(ThreadRequest::Start { id, start })]).await;
            let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
            loop {
                if let Some(done) = self.done.iter().find(|d| d.id == id) {
                    let Outcome::Started { thread } = done.outcome else { panic!("{done:?}") };
                    return thread;
                }
                assert!(self.step(deadline).await.is_some(), "never started in {STEP:?}");
            }
        }

        /// The next event into the core; a control message it does not take is handed back.
        /// `None` once `deadline` passed with nothing.
        async fn step(&mut self, deadline: tokio::time::Instant) -> Option<Option<WorkerMsg>> {
            let event = tokio::time::timeout_at(deadline, self.events.recv()).await.ok()?;
            let event = event.unwrap_or_else(|| panic!("the link's events ended"));
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
                    let ours = self.threads.done(&done).is_some() || self.starts.contains(&done.id);
                    assert!(ours, "every answer is for an intent sent here");
                    self.done.push(done);
                }
                LinkEvent::Control(msg) => return Some(Some(msg)),
                LinkEvent::Disconnected(why) => panic!("disconnected: {why}"),
                _ => {}
            }
            Some(None)
        }

        /// Feed the core whatever comes for `time`.
        async fn take_for(&mut self, time: Duration) {
            let deadline = tokio::time::Instant::now().checked_add(time).unwrap();
            while self.step(deadline).await.is_some() {}
        }

        /// Feed the core until `done` holds; a failure naming `what` once [`STEP`] passes
        /// without it, however busy the link is meanwhile.
        async fn until(&mut self, what: &str, done: impl Fn(&Threads) -> bool) {
            let deadline = tokio::time::Instant::now().checked_add(STEP).unwrap();
            while !done(&self.threads) {
                assert!(self.step(deadline).await.is_some(), "never {what} in {STEP:?}");
            }
        }
    }

    fn state(threads: &Threads, thread: ThreadId) -> Option<&ThreadState> {
        threads.mirror(thread).and_then(Mirror::state)
    }

    fn named(threads: &Threads, thread: ThreadId, title: &str) -> bool {
        state(threads, thread).is_some_and(|s| s.named == title)
    }

    /// Whether the name `title` waits in the outbox, drawn over the thread before it shows it.
    fn naming(threads: &Threads, thread: ThreadId, title: &str) -> bool {
        threads
            .outbox()
            .of(thread)
            .any(|s| matches!(&s.intent, Intent::Name { title: t } if t == title))
    }

    /// How many answers `client` heard for intent `id`.
    fn answers(client: &Client, id: IntentId) -> usize {
        client.done.iter().filter(|d| d.id == id).count()
    }

    fn turn_complete(threads: &Threads, thread: ThreadId, n: usize) -> bool {
        state(threads, thread).is_some_and(|s| {
            s.turns.len() == n && s.turns.last().is_some_and(|t| t.state == TurnState::Complete)
        })
    }

    #[tokio::test]
    async fn a_thread_and_a_name_outlive_a_dropped_link_and_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let cache = Cache::new(dir.path().join("client").join("worker"));
        let me = ClientId::new();
        let mut a = Client::connect(&daemons, me, Threads::new(cache.outbox())).await;

        // A session started here in a terminal, its thread followed through its first turn.
        let thread = a.start(start(&daemons, "Say hello")).await;
        let follow = a.threads.open_thread(thread, None);
        a.send_all(follow).await;
        a.until("the first turn", |t| turn_complete(t, thread, 1)).await;

        // Named through the outbox: drawn as named before the worker hears of it.
        let (first, msg) = a.threads.intent(thread, name("Greeting"));
        assert!(naming(&a.threads, thread, "Greeting"), "drawn in the same frame");
        a.send_all(msg).await;
        a.until("the name settled", |t| {
            named(t, thread, "Greeting") && t.outbox().all().is_empty()
        })
        .await;
        assert_eq!(answers(&a, first), 1);

        // A second device follows throughout.
        let mut witness = Client::connect(&daemons, ClientId::new(), Threads::default()).await;
        let follow = witness.threads.open_thread(thread, None);
        witness.send_all(follow).await;
        // At or past where this one stands: the log may have grown since this one last heard.
        let at = a.threads.mirror(thread).and_then(Mirror::cursor).unwrap();
        witness
            .until("the witness caught up", |t| {
                t.mirror(thread)
                    .and_then(Mirror::cursor)
                    .is_some_and(|c| c.epoch == at.epoch && c.seq >= at.seq)
            })
            .await;

        // The link drops; a new name waits for the next.
        a.link.close();
        a.threads.disconnected();
        let (later, msg) = a.threads.intent(thread, name("Greeting, kept"));
        assert!(msg.is_none(), "nothing goes on a dropped link");
        let kept = a.threads.to_cache(thread).unwrap();
        cache.keep_thread(thread, &kept).unwrap();
        cache.keep_outbox(a.threads.outbox()).unwrap();
        drop(a);

        // A relaunch: the core starts from the cache, and the thread draws before any frame.
        let mut threads = Threads::new(cache.outbox());
        assert_eq!(threads.outbox().all().iter().map(|s| s.id).collect::<Vec<_>>(), [later]);
        assert!(threads.open_thread(thread, cache.thread(thread)).is_none(), "not linked yet");
        assert_eq!(state(&threads, thread), Some(&kept.state), "drawn from the cache");
        assert!(naming(&threads, thread, "Greeting, kept"), "the unsent name is drawn too");

        // Linked again: only what was missed comes, and the name goes under its first id.
        let mut b = Client::connect(&daemons, me, threads).await;
        b.until("the second name settled", |t| {
            named(t, thread, "Greeting, kept") && t.outbox().all().is_empty()
        })
        .await;
        let cursor: Cursor = kept.cursor;
        assert_eq!(
            b.came.first(),
            Some(&Came::Actions { epoch: cursor.epoch, first: cursor.seq }),
            "caught up from the cached cursor, not sent a snapshot: {:?}",
            b.came
        );
        assert!(!b.came.contains(&Came::Snapshot), "{:?}", b.came);
        assert_eq!(answers(&b, later), 1, "acted on once");
        witness.until("the witness has the name", |t| named(t, thread, "Greeting, kept")).await;

        drop(b);
        drop(witness);
        drop(daemons);
    }

    /// Six seconds without a pong, after which the app gives a worker's link up and dials
    /// again (`slopty_net::liveness::PONG_WITHIN`).
    const SILENCE_DROP: Duration = slopty_net::liveness::PONG_WITHIN;
    /// How long the network is gone: past the app's giving up and the worker's wait for a
    /// half-open link's hello (`slopty_net::listen`, 10 s), so the outage is a long one on both
    /// sides. It was 30 s, which made this test alone the length of the suite's run.
    const AWAY: Duration = Duration::from_secs(12);

    /// The app's link to its worker cut while the person names a session and asks for its
    /// review, as the app meets it: the link goes silent, the app gives it up after six seconds
    /// without a pong and dials again until the network is back. Cut both ways for 12 s, a name
    /// and a review asked into the cut go again on the next link and arrive, the name acted on
    /// once and the review without being asked again. Cut on the way back alone, a name is acted
    /// on (a second device sees it) and its reply lost; on the next link it goes again under its
    /// id and is answered as it was. The times from the network back are printed: the
    /// measurement.
    #[tokio::test]
    async fn a_name_and_a_review_cross_a_cut_link() {
        let dir = tempfile::tempdir().unwrap();
        let daemons = daemons(dir.path()).await;
        let cable = slopty_testkit::relay::Relay::start(daemons.addr).await.unwrap();
        let me = ClientId::new();
        let mut a = Client::connect_to(cable.addr(), me, Threads::default()).await;
        let thread = a.start(start(&daemons, "Say hello")).await;
        let follow = a.threads.open_thread(thread, None);
        a.send_all(follow).await;
        a.until("the first turn", |t| turn_complete(t, thread, 1)).await;
        // A second device on the worker itself, past the cut, sees what the worker does.
        let mut witness = Client::connect(&daemons, ClientId::new(), Threads::default()).await;
        let follow = witness.threads.open_thread(thread, None);
        witness.send_all(follow).await;
        witness.until("the witness has the turn", |t| turn_complete(t, thread, 1)).await;

        // Cut both ways: the name and a review go into the cut, and the app gives up.
        cable.cut();
        let cut = tokio::time::Instant::now();
        let (first, msg) = a.threads.intent(thread, name("Greeting"));
        a.send_all(msg).await;
        let scope = ReviewScope::WorkingTree(Against::Head);
        let msg = a.threads.ask_review(thread, scope.clone());
        a.send_all(msg).await;
        tokio::time::sleep(SILENCE_DROP).await;
        a.link.close();
        a.threads.disconnected();
        let threads = std::mem::take(&mut a.threads);
        drop(a);
        tokio::time::sleep_until(cut.checked_add(AWAY).unwrap()).await;
        witness.take_for(Duration::from_secs(1)).await;
        assert!(!named(&witness.threads, thread, "Greeting"), "nothing crossed the cut");
        cable.restore();
        let back = tokio::time::Instant::now();
        let mut b = Client::connect_to(cable.addr(), me, threads).await;
        let linked = back.elapsed();
        b.until("the name acted on", |t| named(t, thread, "Greeting")).await;
        let acted = back.elapsed();
        b.until("the review", |t| t.review(thread, &scope).is_some()).await;
        let reviewed = back.elapsed();
        b.until("the name settled", |t| t.outbox().all().iter().all(|s| s.id != first)).await;
        let answer = b.done.iter().find(|d| d.id == first).map(|d| d.outcome.clone());
        assert!(matches!(answer, Some(Outcome::Done)), "answered as done: {answer:?}");
        assert_eq!(answers(&b, first), 1, "acted on once");
        println!(
            "cut both ways {AWAY:?}: back to linked {linked:?}, to the name acted on \
             {acted:?}, to the review {reviewed:?}"
        );

        // Cut on the way back: the name is acted on, its reply lost, and the app gives up.
        cable.cut_back();
        let cut = tokio::time::Instant::now();
        let (second, msg) = b.threads.intent(thread, name("Greeting, renamed"));
        b.send_all(msg).await;
        witness.until("the second name acted on", |t| named(t, thread, "Greeting, renamed")).await;
        tokio::time::sleep_until(cut.checked_add(SILENCE_DROP).unwrap()).await;
        assert!(b.done.iter().all(|d| d.id != second), "its reply was lost");
        b.link.close();
        b.threads.disconnected();
        let threads = std::mem::take(&mut b.threads);
        drop(b);
        cable.restore();
        let back = tokio::time::Instant::now();
        let mut c = Client::connect_to(cable.addr(), me, threads).await;
        c.until("the second name settled", |t| t.outbox().all().iter().all(|s| s.id != second))
            .await;
        let settled = back.elapsed();
        let answer = c.done.iter().find(|d| d.id == second).map(|d| d.outcome.clone());
        assert!(matches!(answer, Some(Outcome::Done)), "answered as it was: {answer:?}");
        c.until("the name kept", |t| named(t, thread, "Greeting, renamed")).await;
        println!("reply lost: back to the name settled {settled:?}");

        drop(c);
        drop(witness);
        drop(daemons);
    }
}
