//! Starting an agent (`slopty_worker::thread::start`): each agent's own TUI in a terminal of its
//! own, its thread held as the start says, a session taken up again while it runs left alone, a
//! terminal that does not open leaving nothing behind, and a terminal whose agent ended started
//! again in it. The programs are stand-ins on the start's own `PATH`, and the terminals a fake
//! that keeps what it was asked to open.

#[cfg(test)]
mod start {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::Arc;

    use slopty_agent::claude::{RESUME_FLAG, thread_of};
    use slopty_core::{SessionId, WallMs};
    use slopty_proto::terminal::OpenSession;
    use slopty_proto::thread::wire::{Outcome, Start};
    use slopty_proto::thread::{Action, AgentId, IntentId, Liveness, Phase, Status, ThreadId};
    use slopty_worker::restore::Started;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::claude::{Claude, Place};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::start::{Starter, Terminals};

    /// A Claude Code session id, as Claude Code names its transcript.
    const SESSION: &str = "0198f3c2-7b1a-7c3e-9d4f-5a6b7c8d9e0f";

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

    /// Terminals that keep what they were asked to open, and refuse with `refuse` when it is
    /// set.
    #[derive(Debug, Default)]
    struct Fake {
        asked: parking_lot::Mutex<Vec<Asked>>,
        refuse: parking_lot::Mutex<Option<String>>,
    }

    impl Fake {
        fn took(
            &self,
            way: Way,
            id: SessionId,
            req: OpenSession,
            started: Started,
        ) -> Result<(), String> {
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

    /// A starter on a host of its own, its programs stand-ins in `bin`.
    struct Rig {
        _dir: tempfile::TempDir,
        bin: PathBuf,
        host: Host,
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
        let starter = Starter::new(host.clone(), claude, place(), terminals, "studio".to_owned());
        Rig { _dir: dir, bin, host, fake, starter }
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

    fn ended(host: &Host, thread: ThreadId) {
        let status = Status {
            phase: Phase::Idle,
            wait: None,
            liveness: Liveness::Exited { resumable: true },
            since_ms: WallMs::now(),
        };
        host.apply(thread, vec![Action::Status(status)]);
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

        ended(&rig.host, thread);
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
        let thread = started(rig.starter.start(IntentId::new(), &resume, None).await);
        ended(&rig.host, thread);
        *rig.fake.refuse.lock() = Some("no room".to_owned());
        let outcome = rig.starter.start(IntentId::new(), &resume, None).await;
        assert_eq!(outcome, Outcome::Refused { reason: "no room".to_owned() });
        let (meta, _status) = rig.host.meta(thread).expect("the held thread stays");
        assert_eq!(meta.terminal(), None, "without the terminal that did not open");
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
