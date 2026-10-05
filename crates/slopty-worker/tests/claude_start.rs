//! A Claude Code thread started from a client, on the worker's side: the person's `claude` is
//! played by `slopty-stub-claude` on a `PATH` of the test's own, the terminals by the test (each
//! open is kept, nothing runs in it), the daemon's broadcast and a session's sources by the test,
//! and the transcript by a real Claude Code's, recorded (`slopty-agent/tests/fixtures`). The
//! thread is judged from the host.

#[cfg(test)]
mod claude_start {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_agent::observed::thread_of;
    use slopty_core::SessionId;
    use slopty_proto::WorkerMsg;
    use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus};
    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Drive, Fork, IntentId, ItemBody, Liveness, ThreadId, ThreadMeta, ThreadState,
        TurnId,
    };
    use slopty_worker::conversation::Seen;
    use slopty_worker::orchestrate;
    use slopty_worker::thread::claude::{self, Sources};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::terminals::{Pending, Terminals};
    use slopty_worker::thread::{Host, Seated};
    use tokio::sync::{broadcast, watch};

    /// Long enough for any step on a loaded machine; the tests judge the state, not the time.
    const BOUND: Duration = Duration::from_secs(30);

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    /// A terminal the start opened: its command, folder and variables.
    type Opened = (Vec<String>, String, Vec<(String, String)>, SessionId);

    /// The worker's terminals as the test keeps them: every open is noted, and nothing runs.
    #[derive(Default)]
    struct Kept {
        opened: parking_lot::Mutex<Vec<Opened>>,
    }

    impl Terminals for Kept {
        fn open(
            &self,
            command: Vec<String>,
            cwd: String,
            env: Vec<(String, String)>,
        ) -> Pending<'_, Result<SessionId, String>> {
            let session = SessionId::new();
            self.opened.lock().push((command, cwd, env, session));
            Box::pin(std::future::ready(Ok(session)))
        }

        fn open_at(
            &self,
            seat: SessionId,
            command: Vec<String>,
            cwd: String,
            env: Vec<(String, String)>,
        ) -> Pending<'_, Result<SessionId, String>> {
            self.opened.lock().push((command, cwd, env, seat));
            Box::pin(std::future::ready(Ok(seat)))
        }

        fn exited(&self, _session: SessionId) -> Pending<'static, ()> {
            Box::pin(std::future::pending())
        }

        fn close(&self, _session: SessionId) -> Pending<'_, ()> {
            Box::pin(std::future::ready(()))
        }
    }

    /// The session's sources: the transcript, once the agent has written one.
    struct Fake {
        main: watch::Sender<Option<PathBuf>>,
        seen: watch::Sender<Seen>,
    }

    impl Sources for Fake {
        fn sources(&self, _session: SessionId) -> orchestrate::Sources {
            orchestrate::Sources { main: self.main.borrow().clone(), ..Default::default() }
        }

        fn seen(&self, _session: SessionId) -> watch::Receiver<Seen> {
            self.seen.subscribe()
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        work: PathBuf,
        /// A `PATH` holding the stand-in as `claude`.
        programs: PathBuf,
        host: Host,
        terminals: Arc<Kept>,
        sources: Arc<Fake>,
        starter: claude::start::Starter,
        events: broadcast::Sender<WorkerMsg>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = std::fs::canonicalize(dir.path()).unwrap();
            let work = root.join("work");
            let programs = root.join("programs");
            std::fs::create_dir_all(&work).unwrap();
            std::fs::create_dir_all(&programs).unwrap();
            std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude")).unwrap();
            let host = Host::open(&root.join("threads"), Limits::default()).unwrap();
            let sources = Arc::new(Fake {
                main: watch::Sender::new(None),
                seen: watch::Sender::new(Seen::default()),
            });
            let events = broadcast::Sender::new(64);
            let (driver, asks) = claude::Driver::channel();
            let observed: Arc<dyn Sources> = Arc::<Fake>::clone(&sources);
            drop(claude::spawn(host.clone(), events.subscribe(), observed, asks));
            let terminals = Arc::new(Kept::default());
            let (starter, asks) = claude::start::Starter::channel();
            let opened: Arc<dyn Terminals> = Arc::<Kept>::clone(&terminals);
            let path = Some(programs.clone().into_os_string());
            drop(claude::start::spawn(host.clone(), driver, opened, path, asks));
            Self { dir, work, programs, host, terminals, sources, starter, events }
        }

        fn start(&self, prompt: Option<&str>) -> Start {
            Start {
                agent: AgentId::named(AgentId::CLAUDE_CODE),
                cwd: self.work.to_string_lossy().into_owned(),
                drive: None,
                prompt: prompt.map(str::to_owned),
                model: Some("opus".to_owned()),
                args: Vec::new(),
                worktree: None,
            }
        }

        fn opened(&self) -> Vec<Opened> {
            self.terminals.opened.lock().clone()
        }

        /// The agent writes `scenario`'s transcript, and a hook fires.
        fn write(&self, scenario: &str) {
            let from = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../slopty-agent/tests/fixtures/conversation")
                .join(scenario)
                .join("transcript.jsonl");
            let main = self.dir.path().join("transcript.jsonl");
            std::fs::copy(from, &main).unwrap();
            self.sources.main.send_replace(Some(main));
            self.sources.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
        }
    }

    /// The first message of `scenario`'s transcript, as the person typed it.
    fn first_message(scenario: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../slopty-agent/tests/fixtures/conversation")
            .join(scenario)
            .join("transcript.jsonl");
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|entry| entry["type"] == "user")
            .and_then(|entry| entry["message"]["content"].as_str().map(str::to_owned))
            .unwrap()
    }

    /// Wait until `thread` in `host` satisfies `done`.
    async fn until(
        host: &Host,
        thread: ThreadId,
        done: impl Fn(&ThreadState) -> bool,
    ) -> ThreadState {
        let mut table = host.table_watch();
        let mut feed = None;
        let waited = tokio::time::timeout(BOUND, async {
            loop {
                if feed.is_none() {
                    feed = host.follow(thread, None, 1).map(|f| f.feed);
                }
                if let Some((state, _)) = host.state(thread)
                    && done(&state)
                {
                    return state;
                }
                match feed.as_mut() {
                    Some(feed) => tokio::select! {
                        _ = table.changed() => {}
                        _ = feed.recv() => {}
                    },
                    None => table.changed().await.unwrap(),
                }
            }
        });
        waited.await.expect("the thread came to the state awaited")
    }

    fn users(state: &ThreadState) -> Vec<(String, Option<IntentId>)> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::User(m) => Some((m.text.text.clone(), m.intent)),
                _ => None,
            })
            .collect()
    }

    /// A start opens the person's own `claude`, found on their `PATH`, in the thread's folder,
    /// on a session id chosen for it, with the model and the first message on Claude Code's own
    /// command line; the thread is named by that id and begun at once with the terminal, and the
    /// first message, once the transcript shows it, carries the start's intent. A repeat of the
    /// intent starts nothing.
    #[tokio::test]
    async fn a_start_opens_the_persons_claude_and_its_thread_begins_at_once() {
        let rig = Rig::new();
        let prompt = first_message("edit");
        let id = IntentId::new();
        let outcome = rig.starter.start(id, rig.start(Some(&prompt))).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };

        let opened = rig.opened();
        assert_eq!(opened.len(), 1);
        let (command, cwd, env, terminal) = &opened[0];
        assert_eq!(Path::new(&command[0]), rig.programs.join("claude"), "the person's own");
        let native = command[2].clone();
        let want = ["--session-id", native.as_str(), "--model=opus", "--", prompt.as_str()];
        assert_eq!(command[1..], want, "as Claude Code's command line takes them");
        assert_eq!(thread, thread_of(&native), "named by the session id it was given");
        assert_eq!(*cwd, rig.work.to_string_lossy());
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str());
        assert_eq!(path, rig.programs.to_str(), "with the PATH it was found on");

        let (state, _) = rig.host.state(thread).expect("begun before the agent spoke");
        assert_eq!(state.meta.native, native);
        assert_eq!(state.meta.terminal, Some(*terminal));
        assert_eq!(state.meta.cwd, rig.work.to_string_lossy());
        assert!(state.meta.agent.is(AgentId::CLAUDE_CODE));
        assert!(state.meta.drive.is(Drive::OBSERVED));

        let again = rig.starter.start(id, rig.start(Some(&prompt))).await;
        assert_eq!(again, outcome, "started once");
        assert_eq!(rig.opened().len(), 1, "one terminal");

        rig.write("edit");
        let state = until(&rig.host, thread, |s| !users(s).is_empty()).await;
        assert_eq!(users(&state)[0], (prompt, Some(id)), "the first message, as the start sent it");
    }

    /// A server task's thread opens Claude Code in a terminal under the seat, with the seat's
    /// variables beside the `PATH` and its role added to the system prompt; the first message
    /// goes as written. Its row names the seat, kept through what the transcript says, and a
    /// start repeated at the seat opens nothing.
    #[tokio::test]
    async fn a_seated_start_opens_claude_under_the_seat_with_its_role() {
        let rig = Rig::new();
        let prompt = first_message("edit");
        let seat = SessionId::new();
        let variable = ("SLOPTY_SESSION".to_owned(), seat.to_string());
        let seated = Seated {
            seat,
            env: vec![variable.clone()],
            role: Some("You review.".to_owned()),
            relay: None,
        };
        let outcome = rig.starter.start_seated(rig.start(Some(&prompt)), seated.clone()).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let again = rig.starter.start_seated(rig.start(Some(&prompt)), seated).await;
        assert_eq!(again, outcome, "one thread a seat");

        let opened = rig.opened();
        assert_eq!(opened.len(), 1, "one terminal");
        let (command, _, env, terminal) = &opened[0];
        assert_eq!(*terminal, seat, "the terminal is the seat");
        assert_eq!(command[1], "--append-system-prompt=You review.");
        assert_eq!(command[2..4], ["--session-id".to_owned(), thread_native(&rig, thread)]);
        assert_eq!(command.last(), Some(&prompt), "the first message as written");
        assert!(env.contains(&variable), "{env:?}");
        assert_eq!(rig.host.seated_at(seat), Some(thread));

        rig.write("edit");
        let state = until(&rig.host, thread, |s| !users(s).is_empty()).await;
        let fact = state.meta.facts.get(slopty_proto::project::SEAT_FACT);
        assert_eq!(fact, Some(&seat.to_string()), "kept through the transcript");

        // Gone, its session is taken up again where it was started: under the seat, with the
        // seat's variables and its role.
        let native = thread_native(&rig, thread);
        gone(&rig, seat, &native);
        until(&rig.host, thread, |s| matches!(s.status.liveness, Liveness::Exited { .. })).await;
        let mut resume = rig.start(None);
        resume.args = vec!["--resume".to_owned(), native.clone()];
        let seated = rig.host.seated_of(thread).expect("the seat is kept");
        let woken = rig.starter.start_at(IntentId::new(), resume, seated).await;
        assert_eq!(woken, Outcome::Started { thread });
        let opened = rig.opened();
        assert_eq!(opened.len(), 2);
        let (command, _, env, terminal) = &opened[1];
        assert_eq!(*terminal, seat, "under the seat again");
        assert_eq!(command[1], "--append-system-prompt=You review.");
        assert_eq!(command[2..4], ["--resume".to_owned(), native]);
        assert!(env.contains(&variable), "{env:?}");
    }

    /// The Claude Code session `thread` is of.
    fn thread_native(rig: &Rig, thread: ThreadId) -> String {
        rig.host.state(thread).map(|(state, _)| state.meta.native).unwrap_or_default()
    }

    /// Claude Code in `terminal` says it is gone from session `native`, as the worker's tracker
    /// says when its process ends.
    fn gone(rig: &Rig, terminal: SessionId, native: &str) {
        let event = AgentEvent {
            session: terminal,
            kind: AgentKind::ClaudeCode,
            status: AgentStatus::None,
            agent_session: Some(native.to_owned()),
            detail: None,
            attention: false,
            source: AgentSource::Hook,
            since_ms: slopty_core::WallMs::ZERO,
            mode: None,
        };
        rig.events.send(WorkerMsg::Agent(event)).unwrap();
    }

    /// An exited thread is taken up again by a start of its own session: the person's `claude`
    /// opens in a new terminal with `--resume <id>` and the model it last ran, and the same
    /// thread goes on there, named by the same id and the new terminal. While a Claude Code
    /// still runs that session, a resume is refused and opens nothing: a session has one
    /// writer. A resume of what is no session id is refused too.
    #[tokio::test]
    async fn an_exited_thread_is_resumed_on_its_own_session_in_a_new_terminal() {
        let rig = Rig::new();
        let Outcome::Started { thread } = rig.starter.start(IntentId::new(), rig.start(None)).await
        else {
            panic!("started");
        };
        let (_, _, _, first) = rig.opened()[0].clone();
        let (state, _) = rig.host.state(thread).unwrap();
        let native = state.meta.native.clone();
        let mut resume = rig.start(None);
        resume.args = vec!["--resume".to_owned(), native.clone()];
        let busy = rig.starter.start(IntentId::new(), resume.clone()).await;
        let want = Outcome::Refused { reason: "Claude Code runs this session already".to_owned() };
        assert_eq!(busy, want, "one writer");
        assert_eq!(rig.opened().len(), 1, "nothing opened");

        gone(&rig, first, &native);
        until(&rig.host, thread, |s| matches!(s.status.liveness, Liveness::Exited { .. })).await;
        let outcome = rig.starter.start(IntentId::new(), resume).await;
        assert_eq!(outcome, Outcome::Started { thread }, "the same thread goes on");
        let opened = rig.opened();
        assert_eq!(opened.len(), 2);
        let (command, cwd, _, second) = &opened[1];
        assert_eq!(command[1..], ["--resume", native.as_str(), "--model=opus"]);
        assert_eq!(*cwd, rig.work.to_string_lossy());
        let state = until(&rig.host, thread, |s| s.meta.terminal == Some(*second)).await;
        assert_eq!(state.meta.native, native, "under the same id");

        let mut bogus = rig.start(None);
        bogus.args = vec!["--resume".to_owned(), "$(rm -rf ~)".to_owned()];
        let outcome = rig.starter.start(IntentId::new(), bogus).await;
        assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
        assert_eq!(rig.opened().len(), 2);
    }

    /// A start may ask for a permission mode to begin in, plan mode among them: the person's
    /// `claude` opens with `--permission-mode <mode>`, on a new session or one taken up again.
    #[tokio::test]
    async fn a_start_in_plan_mode_opens_claude_planning() {
        let rig = Rig::new();
        let mut plan = rig.start(None);
        plan.args = vec!["--permission-mode".to_owned(), "plan".to_owned()];
        let Outcome::Started { thread } = rig.starter.start(IntentId::new(), plan).await else {
            panic!("started");
        };
        let opened = rig.opened();
        let (command, ..) = &opened[0];
        assert_eq!(command[1..3], ["--permission-mode", "plan"]);
        assert_eq!(command[3..5], ["--session-id".to_owned(), thread_native(&rig, thread)]);
    }

    /// What a start cannot be is refused in words, and opens nothing: arguments of a client's
    /// other than a resume and a permission mode (one that skips every permission among them),
    /// another way to drive it, a folder that is not here, and a worker with no `claude`.
    #[tokio::test]
    async fn a_start_claude_code_cannot_take_is_refused_and_opens_nothing() {
        let rig = Rig::new();
        let mut args = rig.start(None);
        args.args = vec!["--dangerously-skip-permissions".to_owned()];
        let mut bypass = rig.start(None);
        bypass.args = vec!["--permission-mode".to_owned(), "bypassPermissions".to_owned()];
        let mut driven = rig.start(None);
        driven.drive = Some(Drive::named(Drive::DRIVEN));
        let mut nowhere = rig.start(None);
        nowhere.cwd = rig.work.join("missing").to_string_lossy().into_owned();
        for start in [args, bypass, driven, nowhere] {
            let outcome = rig.starter.start(IntentId::new(), start).await;
            assert!(matches!(outcome, Outcome::Refused { .. }), "{outcome:?}");
        }
        assert_eq!(rig.opened(), []);

        std::fs::remove_file(rig.programs.join("claude")).unwrap();
        let id = IntentId::new();
        let outcome = rig.starter.start(id, rig.start(None)).await;
        let want = Outcome::Refused { reason: "Claude Code is not installed".to_owned() };
        assert_eq!(outcome, want);
        assert_eq!(rig.starter.start(id, rig.start(None)).await, want, "refused once");
        assert_eq!(rig.opened(), []);
    }

    /// A fork opens the person's `claude` in a new terminal, resuming the thread's conversation
    /// into a new one of an id chosen for it (`--fork-session`), and the new thread, named by
    /// that id, says which thread and turn it came from. Before the first message there is no
    /// conversation to fork, and a fork from an earlier turn is refused: Claude Code forks whole
    /// conversations.
    #[tokio::test]
    async fn a_fork_resumes_the_conversation_into_a_new_thread() {
        let rig = Rig::new();
        let Outcome::Started { thread } = rig.starter.start(IntentId::new(), rig.start(None)).await
        else {
            panic!("started");
        };
        let early = rig.starter.fork(thread, IntentId::new(), None).await;
        let want = Outcome::Refused { reason: "There is nothing to fork yet".to_owned() };
        assert_eq!(early, want);
        rig.write("edit");
        let state = until(&rig.host, thread, |s| !s.turns.is_empty()).await;
        let last = state.last_turn().map(|t| t.id);
        let earlier = Some(TurnId(last.unwrap().0 + 9));
        let refused = rig.starter.fork(thread, IntentId::new(), earlier).await;
        assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");
        assert_eq!(rig.opened().len(), 1, "nothing opened");

        let fork = IntentId::new();
        let Outcome::Started { thread: branch } = rig.starter.fork(thread, fork, last).await else {
            panic!("not forked");
        };
        assert_eq!(rig.starter.fork(thread, fork, last).await, Outcome::Started { thread: branch });
        let opened = rig.opened();
        assert_eq!(opened.len(), 2, "one terminal for the fork");
        let (command, cwd, ..) = &opened[1];
        let new = command[5].clone();
        let native = state.meta.native.as_str();
        assert_eq!(command[1..], ["--resume", native, "--fork-session", "--session-id", &new]);
        assert_eq!(*cwd, rig.work.to_string_lossy());
        assert_eq!(branch, thread_of(&new));
        let (forked, _) = rig.host.state(branch).unwrap();
        assert_eq!(forked.meta.forked_from, Some(Fork { thread, turn: last }));
        assert_eq!(forked.meta.origin, ThreadMeta::FORK);
    }
}
