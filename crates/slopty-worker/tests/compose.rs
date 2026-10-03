//! The composer of an observed agent on a real terminal: a session actor over an in-process
//! PTY whose program records every byte it is given (`cat`, in raw mode, into a file), with
//! the agent's status played by the test. What is judged is the bytes that arrived and the
//! thread's pending list, never the time.

#[cfg(test)]
mod compose {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use slopty_agent::observed::{Observed, Out};
    use slopty_core::{ClientId, SessionId, WallMs};
    use slopty_proto::agent::{AgentKind, AgentSource, AgentStatus, BlockReason, SessionAgent};
    use slopty_proto::input::CellMetrics;
    use slopty_proto::terminal::{TermRequest, TermSize};
    use slopty_proto::thread::wire::{Intent, Outcome};
    use slopty_proto::thread::{
        Action, Changed, Clipped, Delivery, IntentId, Item, ItemBody, ItemId, PendingState,
        ThreadId, ThreadState, Turn, TurnId, TurnState, Usage, UserMessage,
    };
    use slopty_pty::{Pty, SpawnSpec};
    use slopty_worker::orchestrate::Agents;
    use slopty_worker::session::{self, SessionHandle, SessionStart};
    use slopty_worker::thread::compose::{DRAFT, TAKEN_BACK, Terminals};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Composer, Host};
    use tokio::sync::mpsc;

    /// Long enough for any step on a loaded machine; the tests judge what arrives.
    const BOUND: Duration = Duration::from_secs(20);

    /// The one terminal, and the agent in it as the test says.
    struct Terminal {
        handle: SessionHandle,
        agent: parking_lot::Mutex<Option<AgentStatus>>,
        source: parking_lot::Mutex<AgentSource>,
    }

    impl Agents for Terminal {
        fn status(&self, _session: SessionId) -> Option<SessionAgent> {
            let status = self.agent.lock().clone()?;
            Some(SessionAgent {
                kind: AgentKind::ClaudeCode,
                status,
                source: *self.source.lock(),
                since_ms: WallMs::ZERO,
                mode: None,
            })
        }

        fn forget(&self, _session: SessionId) {}
    }

    impl Terminals for Terminal {
        fn terminal(&self, session: SessionId) -> Option<SessionHandle> {
            (session == self.handle.id()).then(|| self.handle.clone())
        }
    }

    struct Rig {
        terminal: Arc<Terminal>,
        host: Host,
        composer: Composer,
        thread: ThreadId,
        record: PathBuf,
        _child: slopty_pty::Child,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        /// A terminal recording what it is given, an observed Claude Code thread in it, and
        /// the agent at rest.
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let record = dir.path().join("record");
            std::fs::write(&record, "").unwrap();
            let size = TermSize {
                cols: 80,
                rows: 24,
                metrics: CellMetrics { cell_width: 8, cell_height: 16 },
            };
            let pty = Pty::open(size).unwrap();
            let spec = SpawnSpec {
                command: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    r#"stty raw -echo; printf ready >"$0"; exec cat >>"$0""#.to_owned(),
                    record.to_string_lossy().into_owned(),
                ],
                cwd: Some(dir.path().to_path_buf()),
                env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())],
                size,
            };
            let slopty_pty::Spawned { child, term } = pty.spawn_with(&spec, None).unwrap();
            let (tap, mut taps) = mpsc::channel(64);
            tokio::spawn(async move { while taps.recv().await.is_some() {} });
            let handle = session::spawn(SessionStart {
                id: SessionId::new(),
                master: pty.into_master(),
                term,
                checkpoint: Vec::new(),
                backlog: Vec::new(),
                tap,
                size,
                scrollback_lines: 100,
                exited: None,
                port_hints: None,
                moves: None,
                touched: None,
                restored: None,
                divide: false,
            })
            .unwrap();
            let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
            let mut observed = Observed::new("s1", "", Some(handle.id()), "/", WallMs::ZERO);
            let Some(Out::Begin(meta)) = observed.drain().into_iter().next() else { panic!() };
            let thread = meta.id;
            host.create(*meta).unwrap();
            let terminal = Arc::new(Terminal {
                handle,
                agent: parking_lot::Mutex::new(Some(AgentStatus::Idle)),
                source: parking_lot::Mutex::new(AgentSource::Hook),
            });
            let composer = Composer::new(host.clone(), Arc::<Terminal>::clone(&terminal));
            let rig = Self { terminal, host, composer, thread, record, _child: child, _dir: dir };
            // The recorder is listening once its shell has written its word and gone raw.
            rig.recorded("ready").await;
            std::fs::write(&rig.record, "").unwrap();
            rig
        }

        fn agent(&self, status: AgentStatus) {
            *self.terminal.agent.lock() = Some(status);
        }

        /// Intent `id`, acted on once as the worker acts on it.
        fn intent(&self, id: IntentId, intent: &Intent) -> Outcome {
            let composer = &self.composer;
            self.host
                .intent(self.thread, id, |state| composer.decide(state, id, intent).unwrap())
                .unwrap()
        }

        fn send(&self, text: &str, delivery: Delivery) -> IntentId {
            let id = IntentId::new();
            let send = Intent::Send { text: text.to_owned(), delivery, attachments: vec![] };
            assert_eq!(self.intent(id, &send), Outcome::Accepted);
            id
        }

        fn state(&self) -> ThreadState {
            self.host.state(self.thread).unwrap().0
        }

        /// A person at the terminal types `bytes`.
        fn person(&self, bytes: &[u8]) {
            let person = ClientId::new();
            self.terminal.handle.request(person, TermRequest::Raw(bytes.to_vec())).unwrap();
        }

        fn enter(&self) {
            let enter = slopty_worker::orchestrate::keys::parse("enter", 0).unwrap();
            let person = ClientId::new();
            self.terminal.handle.request(person, TermRequest::Key(enter)).unwrap();
        }

        /// Wait until the terminal has been given exactly `want`.
        async fn recorded(&self, want: &str) {
            wait(|| read(&self.record) == want, || format!("{:?}", read(&self.record))).await;
        }

        /// Wait until the thread satisfies `done`.
        async fn until(&self, done: impl Fn(&ThreadState) -> bool) -> ThreadState {
            wait(|| done(&self.state()), || format!("{:#?}", self.state().pending)).await;
            self.state()
        }
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Look again, at most every few milliseconds, until `done`, within [`BOUND`].
    async fn wait(done: impl Fn() -> bool, seen: impl Fn() -> String) {
        let deadline = tokio::time::Instant::now().checked_add(BOUND).unwrap();
        while !done() {
            assert!(tokio::time::Instant::now() < deadline, "never came: {}", seen());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A message goes as a paste and an Enter, and leaves the pending list. One sent while a
    /// person has a line typed and unsent is held, saying so, and nothing of it is typed;
    /// once they send their line, it goes after it. The same intent again does nothing.
    #[tokio::test]
    async fn a_message_is_typed_once_and_waits_out_a_persons_draft() {
        let rig = Rig::new().await;
        let first = rig.send("hello there", Delivery::Steer);
        rig.recorded("hello there\r").await;
        rig.until(|s| s.pending.is_empty()).await;
        // The agent's own record of the prompt is the intent's item.
        let prompt = Item {
            id: ItemId("p1".to_owned()),
            turn: TurnId(1),
            at_ms: WallMs::ZERO,
            body: ItemBody::User(UserMessage {
                text: Clipped::whole("hello there"),
                images: Vec::new(),
                command: None,
                intent: None,
            }),
        };
        rig.host.apply(rig.thread, vec![Action::ItemStarted(prompt)]);
        let item = rig.state().items.into_iter().find(|i| i.id.0 == "p1").unwrap();
        let ItemBody::User(message) = item.body else { panic!() };
        assert_eq!(message.intent, Some(first), "the client's bubble finds its item by id");

        rig.person(b"half");
        rig.send("second", Delivery::Steer);
        let held = rig.until(|s| s.pending.iter().any(|p| p.state != PendingState::Waiting)).await;
        let held = held.pending.first().unwrap();
        assert_eq!(held.state, PendingState::Held { reason: DRAFT.to_owned() });
        assert_eq!(read(&rig.record), "hello there\rhalf", "nothing typed into the draft");
        rig.enter();
        rig.recorded("hello there\rhalf\rsecond\r").await;
        rig.until(|s| s.pending.is_empty()).await;

        let again = Intent::Send {
            text: "hello there".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![],
        };
        assert_eq!(rig.intent(first, &again), Outcome::Accepted, "its first outcome");
        assert!(rig.state().pending.is_empty(), "and nothing sent again");
    }

    /// Queued messages go in the order the person puts them in, and one promoted goes at once,
    /// into the turn under way, while the rest wait for the agent to be at rest.
    #[tokio::test]
    async fn a_queued_message_moves_in_the_list_or_goes_now() {
        let rig = Rig::new().await;
        rig.agent(AgentStatus::Working);
        let q1 = rig.send("q1", Delivery::Queue);
        let q2 = rig.send("q2", Delivery::Queue);
        let q3 = rig.send("q3", Delivery::Queue);
        rig.until(|s| s.pending.len() == 3).await;
        let order = |s: &ThreadState| s.pending.iter().map(|p| p.text.clone()).collect::<Vec<_>>();

        let first = Intent::Reorder { pending: q3, before: Some(q1) };
        assert_eq!(rig.intent(IntentId::new(), &first), Outcome::Done);
        assert_eq!(order(&rig.state()), ["q3", "q1", "q2"]);
        let last = Intent::Reorder { pending: q3, before: None };
        assert_eq!(rig.intent(IntentId::new(), &last), Outcome::Done);
        assert_eq!(order(&rig.state()), ["q1", "q2", "q3"]);
        let nowhere = Intent::Reorder { pending: q1, before: Some(IntentId::new()) };
        let refused = rig.intent(IntentId::new(), &nowhere);
        assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");

        let now = Intent::Promote { pending: q2 };
        assert_eq!(rig.intent(IntentId::new(), &now), Outcome::Done);
        rig.recorded("q2\r").await;
        let left = rig.until(|s| s.pending.len() == 2).await;
        assert_eq!(order(&left), ["q1", "q3"], "the rest wait");
        let gone = rig.intent(IntentId::new(), &now);
        assert!(matches!(gone, Outcome::Refused { .. }), "{gone:?}");

        rig.agent(AgentStatus::Idle);
        rig.recorded("q2\rq1\r").await;
    }

    /// A queued message waits for the agent to be at rest, one per turn, while a steer goes
    /// into the turn under way; what waits can be edited and withdrawn.
    #[tokio::test]
    async fn queued_messages_wait_for_the_turn_to_end_one_per_turn() {
        let rig = Rig::new().await;
        rig.agent(AgentStatus::Working);
        rig.send("q1", Delivery::Queue);
        let q2 = rig.send("q2", Delivery::Queue);
        let dropped = rig.send("never", Delivery::Queue);
        rig.send("steer", Delivery::Steer);
        rig.recorded("steer\r").await;
        let waiting = rig.until(|s| s.pending.len() == 3).await;
        assert!(waiting.pending.iter().all(|p| p.state == PendingState::Waiting), "{waiting:?}");

        let edit = Intent::Edit { pending: q2, text: "q2, edited".to_owned() };
        assert_eq!(rig.intent(IntentId::new(), &edit), Outcome::Done);
        let withdraw = Intent::Withdraw { pending: dropped };
        assert_eq!(rig.intent(IntentId::new(), &withdraw), Outcome::Done);
        let gone = rig.intent(IntentId::new(), &withdraw);
        assert!(matches!(gone, Outcome::Refused { .. }), "{gone:?}");

        rig.agent(AgentStatus::Idle);
        rig.recorded("steer\rq1\r").await;
        let left = rig.until(|s| s.pending.len() == 1).await;
        assert_eq!(left.pending.first().map(|p| p.text.as_str()), Some("q2, edited"));
        // Before the agent has taken the first, a steer still goes and the next does not.
        rig.send("probe", Delivery::Steer);
        rig.recorded("steer\rq1\rprobe\r").await;
        // The first one's turn starts and ends, as the transcript tells it: the next goes.
        let turn = Turn {
            id: TurnId(1),
            input: None,
            state: TurnState::Active,
            started_ms: WallMs::ZERO,
            ended_ms: None,
            usage: Usage::default(),
            models: Vec::new(),
            changed: Changed::default(),
            before: None,
            after: None,
        };
        rig.host.apply(rig.thread, vec![Action::TurnStarted(turn)]);
        let ended = Action::TurnEnded {
            turn: TurnId(1),
            state: TurnState::Complete,
            usage: Usage::default(),
            ended_ms: WallMs::ZERO,
        };
        rig.host.apply(rig.thread, vec![ended]);
        rig.recorded("steer\rq1\rprobe\rq2, edited\r").await;
        rig.until(|s| s.pending.is_empty()).await;
    }

    /// An interrupt is Esc, only while the agent works and not over a person's draft; a model
    /// is `/model` with an id from the thread's catalogue, typed once the agent is at rest.
    #[tokio::test]
    async fn an_interrupt_is_esc_and_a_model_is_its_command() {
        let rig = Rig::new().await;
        let refused = |o: Outcome| matches!(o, Outcome::Refused { .. });
        assert!(refused(rig.intent(IntentId::new(), &Intent::Interrupt)), "nothing to stop");
        rig.agent(AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }));
        assert!(refused(rig.intent(IntentId::new(), &Intent::Interrupt)), "a person's to answer");
        rig.agent(AgentStatus::Working);
        rig.person(b"draft");
        let over = rig.intent(IntentId::new(), &Intent::Interrupt);
        assert_eq!(over, Outcome::Refused { reason: DRAFT.to_owned() });
        rig.enter();
        assert_eq!(rig.intent(IntentId::new(), &Intent::Interrupt), Outcome::Accepted);
        rig.recorded("draft\r\x1b").await;

        let unknown = Intent::SetModel { model: "gpt-9".to_owned() };
        assert!(refused(rig.intent(IntentId::new(), &unknown)), "not in the catalogue");
        let opus = Intent::SetModel { model: "opus".to_owned() };
        assert_eq!(rig.intent(IntentId::new(), &opus), Outcome::Accepted);
        rig.send("then this", Delivery::Steer);
        rig.recorded("draft\r\x1bthen this\r").await;
        rig.agent(AgentStatus::Idle);
        rig.recorded("draft\r\x1bthen this\r/model opus\r").await;
    }

    /// A message Claude Code takes back into its input before it begins (Esc right after
    /// Enter) never reaches the transcript: once the agent is at rest again by its own title,
    /// the message is back in the list, held as taken back, and not typed again, since it is
    /// in the terminal. It cannot be edited there, and can be withdrawn. A message the
    /// transcript shows is never taken as gone back.
    #[tokio::test]
    async fn a_message_claude_code_takes_back_is_held_as_taken_back() {
        let rig = Rig::new().await;
        let shown = rig.send("shown", Delivery::Steer);
        rig.recorded("shown\r").await;
        rig.until(|s| s.pending.is_empty()).await;
        let prompt = Item {
            id: ItemId("p1".to_owned()),
            turn: TurnId(1),
            at_ms: WallMs::ZERO,
            body: ItemBody::User(UserMessage {
                text: Clipped::whole("shown"),
                images: Vec::new(),
                command: None,
                intent: Some(shown),
            }),
        };
        rig.host.apply(rig.thread, vec![Action::ItemStarted(prompt)]);
        *rig.terminal.source.lock() = AgentSource::Title;
        rig.host.apply(rig.thread, vec![Action::Status(rig.state().status)]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rig.state().pending.is_empty(), "the transcript took it");

        *rig.terminal.source.lock() = AgentSource::Hook;
        let back = rig.send("never mind", Delivery::Steer);
        rig.recorded("shown\rnever mind\r").await;
        rig.until(|s| s.pending.is_empty()).await;
        rig.agent(AgentStatus::Working);
        rig.host.apply(rig.thread, vec![Action::Status(rig.state().status)]);
        rig.agent(AgentStatus::Idle);
        *rig.terminal.source.lock() = AgentSource::Title;
        rig.host.apply(rig.thread, vec![Action::Status(rig.state().status)]);
        let state = rig.until(|s| !s.pending.is_empty()).await;
        assert_eq!(state.pending[0].intent, back);
        assert_eq!(state.pending[0].text, "never mind");
        assert_eq!(state.pending[0].state, PendingState::Held { reason: TAKEN_BACK.to_owned() });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(read(&rig.record), "shown\rnever mind\r", "not typed again");
        let edit = Intent::Edit { pending: back, text: "again".to_owned() };
        assert!(matches!(rig.intent(IntentId::new(), &edit), Outcome::Refused { .. }));
        let withdraw = Intent::Withdraw { pending: back };
        assert_eq!(rig.intent(IntentId::new(), &withdraw), Outcome::Done);
        assert_eq!(rig.state().pending, []);
    }
}
