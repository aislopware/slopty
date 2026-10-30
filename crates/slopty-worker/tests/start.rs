//! Starting an agent (`slopty_worker::thread::start`): each agent's own TUI in a terminal of its
//! own, its thread held as the start says, a session taken up again while it runs left alone, a
//! terminal that does not open leaving nothing behind and taking back only what its start did,
//! and a terminal whose agent ended started again in it. The programs are stand-ins on the
//! start's own `PATH`, the terminals a fake that keeps what it was asked to open, and the
//! observer of Claude Code the worker's own, told a session ended by its hook.

#[cfg(test)]
mod start {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_agent::claude::history::History;
    use slopty_agent::claude::{RESUME_FLAG, thread_of};
    use slopty_core::{SessionId, WallMs};
    use slopty_proto::terminal::OpenSession;
    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{
        Action, AgentId, Cursor, IntentId, Liveness, Phase, Status, ThreadId, ThreadMeta,
        ThreadState,
    };
    use slopty_worker::restore::Started;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::claude::{Claude, Place};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::start::{Starter, Terminals};
    use tokio::sync::broadcast::error::RecvError;

    /// A Claude Code session id, as Claude Code names its transcript.
    const SESSION: &str = "0198f3c2-7b1a-7c3e-9d4f-5a6b7c8d9e0f";

    /// Another.
    const OTHER: &str = "0198f3c2-7b1a-7c3e-9d4f-5a6b7c8d9e10";

    /// How a terminal was asked for.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Way {
        Open,
        Again,
    }

    /// One terminal asked for.
    #[derive(Clone, Debug)]
    struct Asked {
        way: Way,
        id: SessionId,
        req: OpenSession,
        started: Started,
    }

    /// What happens elsewhere while the next terminal asked for opens.
    struct During(Box<dyn FnOnce() + Send>);

    impl std::fmt::Debug for During {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("During")
        }
    }

    /// Terminals that keep what they were asked to open, run `during` while the next one opens,
    /// and refuse with `refuse` when it is set.
    #[derive(Debug, Default)]
    struct Fake {
        asked: parking_lot::Mutex<Vec<Asked>>,
        refuse: parking_lot::Mutex<Option<String>>,
        during: parking_lot::Mutex<Option<During>>,
    }

    impl Fake {
        fn took(
            &self,
            way: Way,
            id: SessionId,
            req: OpenSession,
            started: Started,
        ) -> Result<(), String> {
            let during = self.during.lock().take();
            if let Some(During(during)) = during {
                during();
            }
            let refused = self.refuse.lock().clone();
            if let Some(why) = refused {
                return Err(why);
            }
            self.asked.lock().push(Asked { way, id, req, started });
            Ok(())
        }

        fn asked(&self) -> Vec<Asked> {
            self.asked.lock().clone()
        }
    }

    impl Terminals for Fake {
        fn open(
            &self,
            id: SessionId,
            req: OpenSession,
            started: Started,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async move { self.took(Way::Open, id, req, started) })
        }

        fn again(
            &self,
            id: SessionId,
            req: OpenSession,
            started: Started,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>> {
            Box::pin(async move { self.took(Way::Again, id, req, started) })
        }

        fn env(&self, id: SessionId) -> Vec<(String, String)> {
            vec![("SLOPTY_SESSION".to_owned(), id.to_string())]
        }
    }

    /// A starter on a host of its own, its programs stand-ins in `bin`, its Claude Codes
    /// observed by `claude`.
    struct Rig {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        /// The person's home, where Claude Code keeps its records.
        home: PathBuf,
        host: Host,
        claude: Claude,
        fake: Arc<Fake>,
        starter: Starter,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        // As the programs are found: macOS's temporary folder is behind a link.
        let bin = std::fs::canonicalize(dir.path()).unwrap().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for (program, version) in
            [("claude", "2.1.314 (Claude Code)"), ("codex", "codex-cli 0.50.0"), ("pi", "0.40.0")]
        {
            let path = bin.join(program);
            std::fs::write(&path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
            let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(&path, mode).unwrap();
        }
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let place = || Place {
            home: dir.path().join("home"),
            path: Some(bin.clone().into_os_string()),
            data: dir.path().join("data"),
        };
        let (claude, _watching) = Claude::spawn(host.clone(), place());
        let fake = Arc::new(Fake::default());
        let shared = Arc::clone(&fake);
        let terminals: Arc<dyn Terminals> = shared;
        let starter =
            Starter::new(host.clone(), claude.clone(), place(), terminals, "studio".to_owned());
        let home = dir.path().join("home");
        Rig { _dir: dir, bin, home, host, claude, fake, starter }
    }

    fn start(agent: &str, args: &[&str]) -> Start {
        Start {
            agent: AgentId::named(agent),
            cwd: "/work".to_owned(),
            drive: None,
            prompt: Some("Fix it".to_owned()),
            model: None,
            mode: None,
            effort: None,
            args: args.iter().map(|&arg| arg.to_owned()).collect(),
            worktree: None,
        }
    }

    /// The terminal a start for intent `id` opens when no seat names one.
    fn terminal_of(id: IntentId) -> SessionId {
        SessionId::from_uuid(*id.as_uuid())
    }

    fn started(outcome: Outcome) -> ThreadId {
        match outcome {
            Outcome::Started { thread } => thread,
            other => panic!("not started: {other:?}"),
        }
    }

    /// `thread`'s agent ended, as its status says; nothing else of it is told.
    fn ended(host: &Host, thread: ThreadId) {
        let status = Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Exited { resumable: true },
            since_ms: WallMs::now(),
        };
        host.apply(thread, vec![Action::Status(status)]);
    }

    /// Thread `thread` once `now` holds of it, looked at again on each change its feed tells.
    async fn shown(
        rig: &Rig,
        thread: ThreadId,
        now: impl Fn(&ThreadState, Cursor) -> bool,
    ) -> ThreadState {
        let mut feed = rig.host.watch(thread).expect("its thread is held");
        let looks = async {
            loop {
                let (state, cursor) = rig.host.state(thread).expect("its thread is held");
                if now(&state, cursor) {
                    return state;
                }
                match feed.recv().await {
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => panic!("its thread went"),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(30), looks).await.expect("its thread came to it")
    }

    fn exited(state: &ThreadState) -> bool {
        matches!(state.status.liveness, Liveness::Exited { .. })
    }

    /// Claude Code's session [`SESSION`] in `terminal` ends, as its `SessionEnd` hook tells the
    /// observer the start told of it.
    fn session_ends(rig: &Rig, terminal: SessionId) {
        let hook = serde_json::json!({
            "hook_event_name": "SessionEnd",
            "session_id": SESSION,
            "reason": "prompt_input_exit",
        });
        let at_us = WallMs::now().as_millis().saturating_mul(1_000);
        rig.claude.hook(terminal, at_us, hook.to_string());
    }

    /// [`session_ends`], back once its thread `thread` shows it: exited and naming no terminal,
    /// the session no longer followed.
    async fn session_ended(rig: &Rig, terminal: SessionId, thread: ThreadId) {
        session_ends(rig, terminal);
        shown(rig, thread, |state, _| exited(state) && state.meta.terminal().is_none()).await;
    }

    /// The thread of Claude Code's session `native`, held as it ended in `/before`, still
    /// naming `terminal` when one is given.
    fn held(rig: &Rig, native: &str, terminal: Option<SessionId>) -> ThreadId {
        let mut meta = History::new(native, "2.1.314", "/before", WallMs::now()).meta().clone();
        if let Some(terminal) = terminal {
            meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), terminal.to_string());
        }
        let thread = meta.id;
        rig.host.create(meta).unwrap();
        ended(&rig.host, thread);
        thread
    }

    fn program(asked: &Asked) -> &Path {
        Path::new(asked.req.command.first().map_or("", String::as_str))
    }

    /// Claude Code, Codex and pi each start in their own TUI, the program found on the start's
    /// `PATH`, in a new terminal named for the intent and titled for the agent, in the start's
    /// folder. The thread names that terminal; the terminal keeps the start without its first
    /// prompt. A start repeated for the same intent answers the same and opens nothing more.
    #[tokio::test]
    async fn each_agent_starts_in_its_own_tui_in_a_terminal_of_its_own() {
        let rig = rig();
        let agents = [
            (AgentId::CLAUDE_CODE, "claude", "Claude Code"),
            (AgentId::CODEX, "codex", "Codex"),
            (AgentId::PI, "pi", "pi"),
        ];
        let mut intents = Vec::new();
        for (n, (agent, name, title)) in agents.into_iter().enumerate() {
            let id = IntentId::new();
            intents.push(id);
            let asked = start(agent, &[]);
            let thread = started(rig.starter.start(id, &asked, None).await);
            let opened = rig.fake.asked();
            assert_eq!(opened.len(), n.saturating_add(1), "{agent}: one terminal each");
            let last = opened.last().unwrap();
            assert_eq!((last.way, last.id), (Way::Open, terminal_of(id)), "{agent}");
            assert_eq!(program(last), rig.bin.join(name), "{agent}: {:?}", last.req.command);
            assert_eq!(last.req.title.as_deref(), Some(title), "{agent}");
            assert_eq!(last.req.cwd.as_deref(), Some("/work"), "{agent}");
            let path = ("PATH".to_owned(), rig.bin.to_string_lossy().into_owned());
            assert!(last.req.env.contains(&path), "{agent}: {:?}", last.req.env);
            assert_eq!(last.started.thread, thread, "{agent}");
            assert!(last.started.start.agent.is(agent), "{agent}");
            assert_eq!(last.started.start.prompt, None, "{agent}: what was said is not kept");
            let (meta, status) = rig.host.meta(thread).expect("its thread is held");
            assert_eq!(meta.terminal(), Some(terminal_of(id)), "{agent}");
            assert_eq!(status.liveness, Liveness::Live, "{agent}");

            let again = rig.starter.start(id, &asked, None).await;
            assert_eq!(again, Outcome::Started { thread }, "{agent}: once per intent");
            assert_eq!(rig.fake.asked().len(), n.saturating_add(1), "{agent}: nothing more opened");
        }
        let claude = rig.fake.asked()[0].req.command.clone();
        let session = intents[0].to_string();
        assert_eq!(
            claude[1..],
            ["--session-id", session.as_str(), "--", "Fix it"],
            "its own session"
        );
    }

    /// A start that takes up a Claude Code session whose terminal still runs it answers with
    /// its thread and opens nothing; once that program ended, the same start opens a terminal
    /// that resumes it, and the thread names the new terminal.
    #[tokio::test]
    async fn a_session_taken_up_while_it_runs_opens_nothing() {
        let rig = rig();
        let resume = start(AgentId::CLAUDE_CODE, &[RESUME_FLAG, SESSION]);
        let first = IntentId::new();
        let thread = started(rig.starter.start(first, &resume, None).await);
        assert_eq!(thread, thread_of(SESSION), "the session's own thread");
        let opened = rig.fake.asked();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].req.command[1..3], [RESUME_FLAG.to_owned(), SESSION.to_owned()]);

        let running = started(rig.starter.start(IntentId::new(), &resume, None).await);
        assert_eq!(running, thread);
        assert_eq!(rig.fake.asked().len(), 1, "it runs: nothing opened");

        session_ended(&rig, terminal_of(first), thread).await;
        let later = IntentId::new();
        assert_eq!(started(rig.starter.start(later, &resume, None).await), thread);
        let opened = rig.fake.asked();
        assert_eq!(opened.len(), 2, "it ended: opened again");
        assert_eq!(opened[1].id, terminal_of(later));
        let (meta, _status) = rig.host.meta(thread).unwrap();
        assert_eq!(meta.terminal(), Some(terminal_of(later)), "the thread names the new one");
    }

    /// A terminal that does not open refuses the start with its reason and leaves nothing: a
    /// thread made for the start is gone, and one that was held already stays without the
    /// terminal it was given.
    #[tokio::test]
    async fn a_start_whose_terminal_does_not_open_leaves_nothing_behind() {
        let rig = rig();
        *rig.fake.refuse.lock() = Some("no room".to_owned());
        for agent in [AgentId::CLAUDE_CODE, AgentId::CODEX] {
            let id = IntentId::new();
            let outcome = rig.starter.start(id, &start(agent, &[]), None).await;
            assert_eq!(outcome, Outcome::Refused { reason: "no room".to_owned() }, "{agent}");
            let thread = if agent == AgentId::CLAUDE_CODE {
                thread_of(&id.to_string())
            } else {
                ThreadId::from_uuid(*id.as_uuid())
            };
            assert!(rig.host.meta(thread).is_none(), "{agent}: its thread is gone");
        }

        *rig.fake.refuse.lock() = None;
        let resume = start(AgentId::CLAUDE_CODE, &[RESUME_FLAG, SESSION]);
        let first = IntentId::new();
        let thread = started(rig.starter.start(first, &resume, None).await);
        session_ended(&rig, terminal_of(first), thread).await;
        *rig.fake.refuse.lock() = Some("no room".to_owned());
        let outcome = rig.starter.start(IntentId::new(), &resume, None).await;
        assert_eq!(outcome, Outcome::Refused { reason: "no room".to_owned() });
        let (meta, _status) = rig.host.meta(thread).expect("the held thread stays");
        assert_eq!(meta.terminal(), None, "without the terminal that did not open");
    }

    /// A start whose terminal does not open takes back what it did and only that, whenever
    /// another writes the thread. A held thread names again the terminal it named before, in
    /// its folder before: a start of the session just before this one, whose agent its observer
    /// has not found there yet. A terminal named while this one was opening stays: the observer
    /// found the session running there.
    #[tokio::test]
    async fn a_refused_start_takes_back_only_what_it_did() {
        let rig = rig();
        *rig.fake.refuse.lock() = Some("no room".to_owned());
        let refused = Outcome::Refused { reason: "no room".to_owned() };

        let before = SessionId::new();
        let thread = held(&rig, SESSION, Some(before));
        let resume = start(AgentId::CLAUDE_CODE, &[RESUME_FLAG, SESSION]);
        assert_eq!(rig.starter.start(IntentId::new(), &resume, None).await, refused);
        let (meta, _status) = rig.host.meta(thread).expect("the held thread stays");
        assert_eq!(meta.terminal(), Some(before), "the terminal it named before");
        assert_eq!(meta.cwd, "/before", "its folder before");

        let thread = held(&rig, OTHER, None);
        let found = SessionId::new();
        let host = rig.host.clone();
        let observed = move || {
            host.update(thread, |state| {
                let mut meta = state.meta.clone();
                meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), found.to_string());
                (vec![Action::Meta(Box::new(meta))], ())
            });
        };
        *rig.fake.during.lock() = Some(During(Box::new(observed)));
        let resume = start(AgentId::CLAUDE_CODE, &[RESUME_FLAG, OTHER]);
        assert_eq!(rig.starter.start(IntentId::new(), &resume, None).await, refused);
        let (meta, _status) = rig.host.meta(thread).expect("the held thread stays");
        assert_eq!(meta.terminal(), Some(found), "the terminal its session was found in");
    }

    /// The observer of a session Slopty started changes of its thread only what the session's
    /// records say: the terminal, folder, origin and facts the worker gave the thread after the
    /// observer read it stay through its word of the title, its read of the transcript from the
    /// start again, and its end, which takes back only the terminal it named itself.
    #[tokio::test]
    async fn the_observer_changes_only_what_the_records_say() {
        let rig = rig();
        let thread = held(&rig, SESSION, None);
        let resume = start(AgentId::CLAUDE_CODE, &[RESUME_FLAG, SESSION]);
        let first = IntentId::new();
        assert_eq!(started(rig.starter.start(first, &resume, None).await), thread);
        // The observer read the thread by the time it says the session runs.
        shown(&rig, thread, |state, _| !exited(state)).await;
        let moved = SessionId::new();
        rig.host.update(thread, |state| {
            let mut meta = state.meta.clone();
            "/moved".clone_into(&mut meta.cwd);
            ThreadMeta::ORCHESTRATED.clone_into(&mut meta.origin);
            meta.facts.insert("kept".to_owned(), "yes".to_owned());
            meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), moved.to_string());
            (vec![Action::Meta(Box::new(meta))], ())
        });
        let given = |state: &ThreadState| {
            let meta = &state.meta;
            let kept = meta.facts.get("kept").cloned();
            (meta.cwd.clone(), meta.origin.clone(), kept, meta.terminal())
        };
        let origin = ThreadMeta::ORCHESTRATED.to_owned();
        let kept = ("/moved".to_owned(), origin, Some("yes".to_owned()), Some(moved));

        let transcript =
            slopty_agent::claude::sessions::transcript(&rig.home, Path::new("/work"), SESSION);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let prompt = serde_json::json!({
            "type": "user",
            "uuid": "00000000-0000-4000-8000-000000000003",
            "parentUuid": null,
            "sessionId": SESSION,
            "cwd": "/work",
            "isSidechain": false,
            "userType": "external",
            "timestamp": "2026-10-30T07:38:06.461Z",
            "version": "2.1.314",
            "message": {"role": "user", "content": "Fix the build"},
        });
        std::fs::write(&transcript, format!("{prompt}\n")).unwrap();
        let titled = shown(&rig, thread, |state, _| !state.meta.title.is_empty()).await;
        assert_eq!(given(&titled), kept, "through its word of the title");

        let epoch = rig.host.state(thread).unwrap().1.epoch;
        std::fs::write(&transcript, "").unwrap();
        let again = shown(&rig, thread, |_, cursor| cursor.epoch != epoch).await;
        assert_eq!(given(&again), kept, "through its read from the start again");

        session_ends(&rig, terminal_of(first));
        let gone = shown(&rig, thread, |state, _| exited(state)).await;
        assert_eq!(given(&gone), kept, "through its end, the terminal not the one it named");
    }

    /// A terminal whose agent ended starts it again in place: Claude Code resumes its own
    /// session, pi its own, and a Codex that Slopty began opens Codex's own list of the
    /// folder's sessions; each with the choices the start made and told nothing. A thread that
    /// is gone is said so.
    #[tokio::test]
    async fn a_terminal_whose_agent_ended_starts_it_again_in_place() {
        let rig = rig();
        for agent in [AgentId::CLAUDE_CODE, AgentId::PI, AgentId::CODEX] {
            let id = IntentId::new();
            let mut asked = start(agent, &[]);
            asked.model = Some("opus".to_owned());
            let thread = started(rig.starter.start(id, &asked, None).await);
            let keeps = rig.fake.asked().last().unwrap().started.clone();
            ended(&rig.host, thread);
            rig.starter.again(terminal_of(id), keeps).await.unwrap();
            let again = rig.fake.asked().last().unwrap().clone();
            assert_eq!((again.way, again.id), (Way::Again, terminal_of(id)), "{agent}");
            let (meta, _status) = rig.host.meta(thread).unwrap();
            let command = &again.req.command[1..];
            let told = command.iter().any(|arg| arg.contains("Fix it"));
            assert!(!told, "{agent}: told nothing: {command:?}");
            let resumed = match agent {
                AgentId::CLAUDE_CODE => vec![RESUME_FLAG.to_owned(), meta.native.clone()],
                AgentId::PI => slopty_agent::pi::sessions::resume_args(&meta.native),
                _ => vec![slopty_agent::codex::RESUME.to_owned()],
            };
            assert_eq!(command[..resumed.len()], resumed[..], "{agent}: {command:?}");
            let model = command.iter().any(|arg| arg.contains("opus"));
            assert!(model, "{agent}: the start's choice: {command:?}");
            assert_eq!(meta.terminal(), Some(terminal_of(id)), "{agent}");
        }

        let gone = Started { thread: ThreadId::new(), start: start(AgentId::CLAUDE_CODE, &[]) };
        let refused = rig.starter.again(SessionId::new(), gone).await;
        assert_eq!(refused, Err("its thread is gone".to_owned()));
    }
}
