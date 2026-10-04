//! A pi thread end to end on the worker: the thread host, the pi adapter's IO, and the
//! stand-in `pi` (`slopty-stub-pi`) on the `PATH` it is found on, replaying the recording of
//! the pinned pi with Slopty's gate (`crates/slopty-agent/tests/fixtures/pi/gate.jsonl`). The
//! stand-in fails a command it has no record of, so what the worker sent is what pi was sent.
//! Handed to pi's TUI, the stand-in runs as the TUI in a terminal the test keeps, appending the
//! recorded session to its file; it notes any time two of it wrote the session at once.

#[cfg(test)]
mod pi {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;
    use slopty_agent::handoff;
    use slopty_core::{ClientId, SessionId, WallMs};
    use slopty_proto::thread::detail::Clipped;
    use slopty_proto::thread::wire::{Intent, Outcome, Pick, Start};
    use slopty_proto::thread::{
        Action, AgentId, Answerer, Cap, Changed, Delivery, Drive, Fork, IntentId, Item, ItemBody,
        ItemId, Liveness, Phase, RequestState, ThreadId, ThreadMeta, ThreadState, ToolState, Turn,
        TurnId, TurnState, Usage, UserMessage,
    };
    use slopty_worker::thread::carry::carry;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::pi::tui::{Pending, Terminals};
    use slopty_worker::thread::pi::{self, Pi};
    use slopty_worker::thread::review::Snapshots;
    use slopty_worker::thread::{Host, Seated, schedule};
    use tokio::process::ChildStdin;
    use tokio::sync::watch;

    const WAIT: Duration = Duration::from_secs(30);

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    /// A kept terminal: its program's stdin until it is closed, and whether the program ended.
    type Running = (Option<ChildStdin>, watch::Receiver<bool>);

    /// The worker's terminals as the test keeps them: each runs its program on pipes, and is
    /// closed by closing its stdin, as a terminal's program ends when its terminal goes.
    #[derive(Default)]
    struct Kept {
        running: parking_lot::Mutex<HashMap<SessionId, Running>>,
        opened: parking_lot::Mutex<Vec<(Vec<String>, String)>>,
    }

    impl Terminals for Kept {
        fn open(
            &self,
            command: Vec<String>,
            cwd: String,
            env: Vec<(String, String)>,
        ) -> Pending<'_, Result<SessionId, String>> {
            Box::pin(async move {
                self.opened.lock().push((command.clone(), cwd.clone()));
                let mut child = tokio::process::Command::new(&command[0])
                    .args(&command[1..])
                    .current_dir(cwd)
                    .envs(env)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn()
                    .map_err(|e| e.to_string())?;
                let stdin = child.stdin.take();
                let (done, ended) = watch::channel(false);
                tokio::spawn(async move {
                    let _status = child.wait().await;
                    let _gone = done.send(true);
                });
                let session = SessionId::new();
                self.running.lock().insert(session, (stdin, ended));
                Ok(session)
            })
        }

        fn exited(&self, session: SessionId) -> Pending<'static, ()> {
            let ended = self.running.lock().get(&session).map(|(_, ended)| ended.clone());
            Box::pin(async move {
                let Some(mut ended) = ended else { return };
                let _ended = ended.wait_for(|done| *done).await;
            })
        }

        fn close(&self, session: SessionId) -> Pending<'_, ()> {
            Box::pin(async move {
                let stdin =
                    self.running.lock().get_mut(&session).and_then(|(stdin, _)| stdin.take());
                drop(stdin);
                self.exited(session).await;
            })
        }
    }

    /// A worker's threads and data, a project to work in, and the stand-in as `pi` alone on a
    /// `PATH` of its own.
    struct Rig {
        _dir: tempfile::TempDir,
        host: Host,
        data: PathBuf,
        work: PathBuf,
        programs: PathBuf,
        record: PathBuf,
        tui_record: PathBuf,
        session_file: PathBuf,
        terminals: Arc<Kept>,
        me: ClientId,
    }

    impl Rig {
        fn new() -> Self {
            Self::with_tui(false)
        }

        /// A rig whose stand-in TUI exits once it has written, when `exits`.
        fn with_tui(exits: bool) -> Self {
            Self::replaying(exits, str::to_owned)
        }

        /// A rig whose stand-in replays what `fixture` makes of the gate's recording.
        fn replaying(exits: bool, replayed: impl FnOnce(&str) -> String) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let (data, work, programs) = (root.join("data"), root.join("work"), root.join("bin"));
            for made in [&data, &work, &programs] {
                std::fs::create_dir_all(made).unwrap();
            }
            std::os::unix::fs::symlink(bin("slopty-stub-pi"), programs.join("pi")).unwrap();
            let gate = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../slopty-agent/tests/fixtures/pi/gate.jsonl");
            let fixture = root.join("fixture.jsonl");
            std::fs::write(&fixture, replayed(&std::fs::read_to_string(gate).unwrap())).unwrap();
            let record = root.join("record.json");
            let tui_record = root.join("tui-record.json");
            let session_file = root.join("session.jsonl");
            let config = serde_json::json!({
                "fixture": fixture,
                "record": record,
                "tui_record": tui_record,
                "session_file": session_file,
                "writer_lock": root.join("writer.lock"),
                "tui_exits": exits,
            });
            std::fs::write(programs.join("stub-pi.json"), config.to_string()).unwrap();
            let host = Host::open(&data.join("threads"), Limits::default()).unwrap();
            Self {
                _dir: dir,
                host,
                data,
                work,
                programs,
                record,
                tui_record,
                session_file,
                terminals: Arc::default(),
                me: ClientId::new(),
            }
        }

        /// The pi threads served, and what is asked of them.
        fn serve(&self) -> (Pi, tokio::task::JoinHandle<()>) {
            let (pi, asks) = Pi::channel();
            let path = Some(self.programs.clone().into_os_string());
            let terminals: Arc<dyn Terminals> = Arc::<Kept>::clone(&self.terminals);
            (pi, pi::spawn(self.host.clone(), self.data.clone(), path, terminals, asks))
        }

        fn start(&self, prompt: &str) -> Box<Start> {
            Box::new(Start {
                agent: AgentId::named(AgentId::PI),
                cwd: self.work.to_string_lossy().into_owned(),
                drive: None,
                prompt: Some(prompt.to_owned()),
                model: None,
                args: vec!["--offline".to_owned()],
            })
        }

        fn by(&self) -> Answerer {
            Answerer { client: Some(self.me), name: "Slopty".to_owned() }
        }

        /// Intent `intent` on `thread`, decided as the daemon decides it: once per id.
        fn intent(&self, pi: &Pi, thread: ThreadId, intent: &Intent) -> (IntentId, Outcome) {
            let id = IntentId::new();
            let decided = self.host.intent(thread, id, |state| {
                if !state.meta.can(intent.needs()) {
                    return (Outcome::Unsupported { cap: Cap::named(intent.needs()) }, Vec::new());
                }
                (pi.decide(state, id, intent, self.by()), Vec::new())
            });
            (id, decided.unwrap())
        }

        fn send(&self, pi: &Pi, thread: ThreadId, text: &str) -> IntentId {
            let send = Intent::Send {
                text: text.to_owned(),
                delivery: Delivery::Steer,
                attachments: vec![],
            };
            let (id, outcome) = self.intent(pi, thread, &send);
            assert_eq!(outcome, Outcome::Done, "{text}");
            id
        }

        /// `thread` once `done` holds of it.
        async fn until(
            &self,
            thread: ThreadId,
            what: &str,
            done: impl Fn(&ThreadState) -> bool,
        ) -> ThreadState {
            let mut feed = self.host.watch(thread).unwrap();
            let waited = tokio::time::timeout(WAIT, async {
                loop {
                    let (state, _) = self.host.state(thread).unwrap();
                    if done(&state) {
                        return state;
                    }
                    match feed.recv().await {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            panic!("{what}: the thread is gone")
                        }
                    }
                }
            })
            .await;
            waited.unwrap_or_else(|_| panic!("{what}: {:#?}", self.host.state(thread)))
        }

        /// What the stand-in that ran last was given and heard.
        fn record(&self) -> Value {
            serde_json::from_slice(&std::fs::read(&self.record).unwrap()).unwrap()
        }

        /// The driven stand-in's record once `done` holds of it: a new one is written as it
        /// starts.
        async fn record_once(&self, done: impl Fn(&Value) -> bool) -> Value {
            let waited = tokio::time::timeout(WAIT, async {
                loop {
                    let read = std::fs::read(&self.record).ok();
                    if let Some(record) = read.and_then(|r| serde_json::from_slice(&r).ok())
                        && done(&record)
                    {
                        return record;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            waited.unwrap_or_else(|_| panic!("the record: {:#}", self.record()))
        }

        /// What the stand-in that ran last as the TUI was given.
        fn tui_record(&self) -> Value {
            serde_json::from_slice(&std::fs::read(&self.tui_record).unwrap()).unwrap()
        }
    }

    fn turn_ended(n: usize, state: TurnState) -> impl Fn(&ThreadState) -> bool {
        move |s: &ThreadState| {
            s.turns.len() == n && s.turns.last().is_some_and(|t| t.state == state)
        }
    }

    fn asking(n: usize) -> impl Fn(&ThreadState) -> bool {
        move |s: &ThreadState| s.requests.len() == n && s.open_requests().count() == 1
    }

    fn tools(state: &ThreadState) -> Vec<(String, ToolState)> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Tool(call) => Some((i.id.0.clone(), call.state.clone())),
                _ => None,
            })
            .collect()
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

    /// A thread started on pi runs the person's `pi` on the `PATH` in RPC mode with the gate,
    /// on a session named by the start; each call waits at the gate on the person, an answer
    /// pi's gate does not offer goes nowhere, and allow, deny with a reason, and an interrupt
    /// at the gate each end the call as pi ended it.
    #[tokio::test]
    async fn a_pi_thread_asks_through_the_gate_and_ends_as_answered() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let id = IntentId::new();
        let outcome = pi.start(id, rig.start("Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        assert_eq!(pi.start(id, rig.start("Say hello.")).await, outcome, "started once");
        assert_eq!(rig.host.threads(), [thread]);

        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(state.meta.agent_version, "1.0.0", "as the program said");
        assert_eq!(state.meta.native, id.to_string(), "the session the start names");
        assert!(state.meta.drive.is(Drive::DRIVEN));
        assert!(state.meta.can(Cap::APPROVALS) && state.meta.can(Cap::STEER));
        assert_eq!(users(&state), [("Say hello.".to_owned(), Some(id))]);
        let record = rig.record();
        let gate = rig
            .data
            .join("pi-gate")
            .join(slopty_agent::pi::digest())
            .join("gate.ts")
            .to_string_lossy()
            .into_owned();
        let session = id.to_string();
        let want = ["--mode", "rpc", "--extension", &gate, "--session-id", &session, "--offline"];
        assert_eq!(record["argv"], serde_json::json!(want));
        assert_eq!(record["cwd"], rig.work.to_string_lossy().as_ref());
        assert_eq!(record["gate"], true, "the extension it loads is Slopty's gate");

        rig.send(&pi, thread, "Make a file called made-by-pi.");
        let state = rig.until(thread, "the first ask", asking(1)).await;
        assert_eq!(state.status.phase, Phase::NeedsYou);
        let ask = state.requests[0].id.clone();
        assert_eq!(state.requests[0].title, "Run touch made-by-pi?");
        rig.send(&pi, thread, "Also say done.");
        let answer = |choice: &str, message: Option<&str>| Intent::Answer {
            ask: ask.clone(),
            choice: choice.to_owned(),
            message: message.map(str::to_owned),
        };
        let (_, maybe) = rig.intent(&pi, thread, &answer("maybe", None));
        assert!(matches!(maybe, Outcome::Refused { .. }), "only what the gate offers: {maybe:?}");
        let release = Intent::Release { ask: ask.clone() };
        let (_, released) = rig.intent(&pi, thread, &release);
        assert!(matches!(released, Outcome::Refused { .. }), "no prompt to give it to");
        assert_eq!(rig.intent(&pi, thread, &answer("allow", None)).1, Outcome::Done);
        let state = rig.until(thread, "the allowed turn", turn_ended(2, TurnState::Complete)).await;
        assert_eq!(
            state.requests[0].state,
            RequestState::Answered { by: rig.by(), choice: "allow".to_owned() }
        );
        assert_eq!(rig.intent(&pi, thread, &answer("deny", None)).1, Outcome::Done, "settled");

        rig.send(&pi, thread, "Remove it.");
        let state = rig.until(thread, "the second ask", asking(2)).await;
        let ask = state.requests[1].id.clone();
        let deny = Intent::Answer {
            ask,
            choice: "deny".to_owned(),
            message: Some("Keep the file.".into()),
        };
        assert_eq!(rig.intent(&pi, thread, &deny).1, Outcome::Done);
        rig.until(thread, "the denied turn", turn_ended(3, TurnState::Complete)).await;

        rig.send(&pi, thread, "Remove it again.");
        rig.until(thread, "the third ask", asking(3)).await;
        assert_eq!(rig.intent(&pi, thread, &Intent::Interrupt).1, Outcome::Done);
        let state =
            rig.until(thread, "the stopped turn", turn_ended(4, TurnState::Interrupted)).await;
        assert_eq!(state.status.phase, Phase::Stopped);
        let want = [
            ("toolu_pi1".to_owned(), ToolState::Completed),
            ("toolu_pi2".to_owned(), ToolState::Rejected),
            ("toolu_pi3".to_owned(), ToolState::Cancelled),
        ];
        assert_eq!(tools(&state), want);
        assert_eq!(state.requests[2].state, RequestState::Withdrawn);
        let (_, late) = rig.intent(&pi, thread, &Intent::Interrupt);
        assert!(matches!(late, Outcome::Refused { .. }), "nothing to stop: {late:?}");

        let record = rig.record();
        assert_eq!(record["unexpected"], serde_json::json!([]), "pi was sent what it was sent");
        let answers: Vec<&Value> = record["heard"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "extension_ui_response")
            .map(|c| &c["value"])
            .collect();
        assert_eq!(answers, ["allow", "deny\nKeep the file."]);
    }

    /// A server task's thread runs pi with its role added to the system prompt, kept with the
    /// thread's own flags so a pi started again for it has it too; the first message goes as
    /// written. Its row names the seat, kept whatever pi says of the thread, and a start repeated
    /// at the seat answers with the thread the first one started.
    #[tokio::test]
    async fn a_seated_pi_thread_gets_the_seat_and_its_role() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let seat = SessionId::new();
        let seated = Seated {
            seat,
            env: vec![("SLOPTY_SESSION".to_owned(), seat.to_string())],
            role: Some("You review.".to_owned()),
            relay: Some("/opt/slopty/bin/slopty".to_owned()),
        };
        let outcome = pi.start_seated(rig.start("Say hello."), seated.clone()).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        assert_eq!(pi.start_seated(rig.start("Say hello."), seated).await, outcome, "once");
        assert_eq!(rig.host.threads(), [thread]);
        assert_eq!(rig.host.seated_at(seat), Some(thread));

        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let fact = state.meta.facts.get(slopty_proto::project::SEAT_FACT);
        assert_eq!(fact, Some(&seat.to_string()), "kept over what pi says of the thread");
        assert_eq!(users(&state)[0].0, "Say hello.", "the role went apart");
        let record = rig.record();
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert_eq!(argv[6..], ["--offline", "--append-system-prompt", "You review."]);
        assert_eq!(record["unexpected"], serde_json::json!([]), "{record:#}");
    }

    /// A thread whose pi is gone is exited and resumable; the next message starts pi again on
    /// the same session, reads the thread again from the session's own record, keeps the intent
    /// of a message it knew, and then sends the message.
    #[tokio::test]
    async fn a_pi_that_is_gone_is_taken_up_again_from_its_session() {
        let rig = Rig::new();
        let (pi, served) = rig.serve();
        let id = IntentId::new();
        let Outcome::Started { thread } = pi.start(id, rig.start("Say hello.")).await else {
            panic!("not started");
        };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;

        served.abort();
        let exited = |s: &ThreadState| s.status.liveness == (Liveness::Exited { resumable: true });
        rig.until(thread, "pi ends with the worker", exited).await;
        let (pi, _served) = rig.serve();
        let (_, compact) = rig.intent(&pi, thread, &Intent::Compact);
        assert!(matches!(compact, Outcome::Refused { .. }), "only a message wakes it: {compact:?}");

        let again = rig.send(&pi, thread, "Say hello.");
        let state = rig
            .until(thread, "the turn after the record", turn_ended(5, TurnState::Complete))
            .await;
        assert_eq!(state.status.liveness, Liveness::Live);
        assert_eq!(state.turns[3].state, TurnState::Interrupted, "as the session left it");
        let users = users(&state);
        assert_eq!(users.len(), 6, "{users:?}");
        assert_eq!(users.first(), Some(&("Say hello.".to_owned(), Some(id))), "its intent kept");
        assert_eq!(users.last(), Some(&("Say hello.".to_owned(), Some(again))));
        let record = rig.record();
        assert_eq!(record["heard"][0]["type"], "get_entries", "the record first");
        assert_eq!(record["argv"][5], id.to_string(), "the same session");
        assert_eq!(record["unexpected"], serde_json::json!([]));
    }

    /// A message scheduled for a time waits on the worker, not in pi; at its time it goes to pi
    /// as the person's message, and leaves the pending list.
    #[tokio::test]
    async fn a_message_scheduled_for_a_time_goes_to_pi_at_its_time() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let Outcome::Started { thread } = pi.start(IntentId::new(), rig.start("Say hello.")).await
        else {
            panic!("not started");
        };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;

        // As the daemon sends it: the person's message, decided once per id.
        let (sending, host, by) = (pi.clone(), rig.host.clone(), rig.by());
        let fire: schedule::Fire = Arc::new(move |thread, id, intent| {
            let decide = |s: &ThreadState| (sending.decide(s, id, &intent, by.clone()), vec![]);
            host.intent(thread, id, decide)
                .unwrap_or_else(|| Outcome::Refused { reason: "gone".into() })
        });
        let _scheduler = schedule::spawn(rig.host.clone(), fire);
        let later = IntentId::new();
        let at_ms = WallMs::from_millis(WallMs::now().as_millis() + 300);
        let send = Intent::Send {
            text: "Make a file called made-by-pi.".to_owned(),
            delivery: Delivery::At { at_ms },
            attachments: vec![],
        };
        assert_eq!(schedule::act(&rig.host, thread, later, &send), Some(Outcome::Accepted));
        let state = rig.until(thread, "it waits on the worker", |s| s.pending.len() == 1).await;
        assert!(state.pending[0].delivery.is_scheduled());
        assert_eq!(state.turns.len(), 1, "not before its time");

        let state = rig.until(thread, "the scheduled message's ask", asking(1)).await;
        assert!(WallMs::now().as_millis() >= at_ms.as_millis(), "at its time");
        assert!(state.pending.is_empty(), "it went");
        let users = users(&state);
        let sent = users.last().unwrap();
        assert_eq!(sent.0, "Make a file called made-by-pi.");
        assert_eq!(sent.1, Some(schedule::sent_as(later)), "as the person's message");
    }

    /// A Claude Code thread goes on in pi: pi starts with nothing sent, once per intent, and the
    /// new thread says where it came from. Its first message, the old thread's portable account,
    /// waits on the worker as a draft; the person changes it and sends it, and it goes to pi as
    /// their message. A thread with nothing in it, or whose agent cannot, is not gone on from.
    #[tokio::test]
    async fn a_claude_code_thread_goes_on_in_pi_from_a_draft_the_person_sends() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let meta = ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: String::new(),
            native: "claude-session".to_owned(),
            cwd: rig.work.to_string_lossy().into_owned(),
            title: "Fix the parser".to_owned(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::OBSERVED),
            caps: vec![Cap::named(Cap::CONTINUE)],
            models: Vec::new(),
            modes: Vec::new(),
            efforts: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        };
        let from = meta.id;
        rig.host.create(meta.clone()).unwrap();
        let pi_agent = AgentId::named(AgentId::PI);
        let not_started = |_, _| std::future::ready(Outcome::Refused { reason: "started".into() });
        let empty = carry(&rig.host, from, IntentId::new(), pi_agent.clone(), not_started).await;
        assert!(matches!(empty, Outcome::Refused { .. }), "nothing to go on from: {empty:?}");

        let said = |n: &str, body| {
            let item =
                Item { id: ItemId(n.to_owned()), turn: TurnId(1), at_ms: WallMs::ZERO, body };
            Action::ItemStarted(item)
        };
        let asked = UserMessage {
            text: Clipped::whole("Fix the parser."),
            images: Vec::new(),
            command: None,
            intent: None,
        };
        rig.host.apply(
            from,
            vec![
                Action::TurnStarted(Turn {
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
                }),
                said("u1", ItemBody::User(asked)),
                said("a1", ItemBody::Text(Clipped::whole("Fixed it."))),
                Action::TurnEnded {
                    turn: TurnId(1),
                    state: TurnState::Complete,
                    usage: Usage::default(),
                    ended_ms: WallMs::ZERO,
                },
            ],
        );
        let mut unable = meta;
        unable.id = ThreadId::new();
        unable.caps.clear();
        rig.host.create(unable.clone()).unwrap();
        let refused = carry(&rig.host, unable.id, IntentId::new(), pi_agent.clone(), not_started);
        assert_eq!(refused.await, Outcome::Unsupported { cap: Cap::named(Cap::CONTINUE) });

        let id = IntentId::new();
        let begin = |id, mut start: Start| {
            start.args.push("--offline".to_owned());
            pi.start(id, Box::new(start))
        };
        let Outcome::Started { thread } = carry(&rig.host, from, id, pi_agent.clone(), begin).await
        else {
            panic!("not gone on");
        };
        let again = carry(&rig.host, from, id, pi_agent, not_started).await;
        assert_eq!(again, Outcome::Started { thread }, "once");
        let state = rig.until(thread, "the draft waits", |s| s.pending.len() == 1).await;
        assert_eq!(state.meta.agent, AgentId::named(AgentId::PI));
        assert_eq!(state.meta.forked_from, Some(Fork { thread: from, turn: Some(TurnId(1)) }));
        assert!(state.turns.is_empty(), "nothing sent");
        let draft = state.pending[0].clone();
        assert_eq!((draft.intent, draft.delivery), (schedule::drafted_as(id), Delivery::Draft));
        let (old, _) = rig.host.state(from).unwrap();
        assert_eq!(draft.text, handoff::render(&old, handoff::BUDGET));
        assert!(draft.text.contains("Fix the parser.") && draft.text.contains("Fixed it."));

        let (sending, host, by) = (pi.clone(), rig.host.clone(), rig.by());
        let fire: schedule::Fire = Arc::new(move |thread, id, intent| {
            let decide = |s: &ThreadState| (sending.decide(s, id, &intent, by.clone()), vec![]);
            host.intent(thread, id, decide)
                .unwrap_or_else(|| Outcome::Refused { reason: "gone".into() })
        });
        let _scheduler = schedule::spawn(rig.host.clone(), fire);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(rig.host.state(thread).unwrap().0.pending.len(), 1, "never on its own");
        let edit = Intent::Edit { pending: draft.intent, text: "Say hello.".to_owned() };
        assert_eq!(schedule::act(&rig.host, thread, IntentId::new(), &edit), Some(Outcome::Done));
        let send = Intent::Promote { pending: draft.intent };
        assert_eq!(schedule::act(&rig.host, thread, IntentId::new(), &send), Some(Outcome::Done));
        let state = rig.until(thread, "the draft's turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.pending.is_empty(), "it went");
        let sent = users(&state).pop().unwrap();
        assert_eq!(sent, ("Say hello.".to_owned(), Some(schedule::sent_as(draft.intent))));
        let record =
            rig.record_once(|r| r["heard"].as_array().is_some_and(|h| !h.is_empty())).await;
        assert_eq!(record["unexpected"], serde_json::json!([]));
    }

    /// The worker's turn snapshots are every adapter's: a pi thread in a git folder takes a
    /// change back by the worker's revert, as an observed one does, checked against the blob
    /// the review showed.
    #[tokio::test]
    async fn a_pi_thread_puts_a_change_back_through_the_workers_snapshots() {
        let rig = Rig::new();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&rig.work)
                .args(args)
                .envs([("GIT_AUTHOR_NAME", "t"), ("GIT_AUTHOR_EMAIL", "t@t")])
                .envs([("GIT_COMMITTER_NAME", "t"), ("GIT_COMMITTER_EMAIL", "t@t")])
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        std::fs::write(rig.work.join("a.txt"), "first\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "a"]);
        let from = git(&["rev-parse", "HEAD:a.txt"]);
        let (pi, _served) = rig.serve();
        let Outcome::Started { thread } = pi.start(IntentId::new(), rig.start("Say hello.")).await
        else {
            panic!("not started");
        };
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.meta.can(Cap::SNAPSHOTS), "{:?}", state.meta.caps);

        std::fs::write(rig.work.join("a.txt"), "changed\n").unwrap();
        let stamp = git(&["hash-object", "a.txt"]);
        let snapshots =
            Snapshots::new(rig.host.clone(), &rig.data.join("snapshots"), Some("git".into()));
        let pick =
            Pick { path: "a.txt".to_owned(), from: Some(from), stamp: Some(stamp), hunks: vec![] };
        let id = IntentId::new();
        assert_eq!(snapshots.pick(thread, id, &Intent::Revert(pick)).await, Some(Outcome::Done));
        assert_eq!(std::fs::read_to_string(rig.work.join("a.txt")).unwrap(), "first\n");
    }

    /// The worker going while the gate asks closes pi's stdin and ends pi with no answer sent,
    /// so the call never runs; the thread says so: the ask withdrawn, the call cancelled, the
    /// turn stopped.
    #[tokio::test]
    async fn a_call_at_the_gate_never_runs_once_the_worker_goes() {
        let rig = Rig::new();
        let (pi, served) = rig.serve();
        let Outcome::Started { thread } = pi.start(IntentId::new(), rig.start("Say hello.")).await
        else {
            panic!("not started");
        };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        rig.send(&pi, thread, "Make a file called made-by-pi.");
        rig.until(thread, "the ask", asking(1)).await;

        served.abort();
        let exited = |s: &ThreadState| s.status.liveness == (Liveness::Exited { resumable: true });
        let state = rig.until(thread, "pi ends with the worker", exited).await;
        assert_eq!(state.requests[0].state, RequestState::Withdrawn);
        assert_eq!(tools(&state), [("toolu_pi1".to_owned(), ToolState::Cancelled)]);
        assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
        assert_eq!(state.status.phase, Phase::Stopped);
        let record = rig.record();
        let answered = record["heard"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "extension_ui_response");
        assert!(!answered, "nothing answered the gate");
    }

    fn held_by_tui(s: &ThreadState) -> bool {
        s.meta.terminal.is_some() && s.meta.drive.is(Drive::OBSERVED)
    }

    fn held_by_slopty(s: &ThreadState) -> bool {
        s.meta.terminal.is_none() && s.meta.drive.is(Drive::DRIVEN)
    }

    /// On the person's word the session goes to pi's own TUI once pi rests, in a terminal on
    /// the same session with the thread's flags, and the thread follows what the TUI writes; it
    /// takes nothing else while the TUI holds it. A worker that starts again follows the TUI
    /// again. Taken back once the TUI rests, the terminal closes and pi is driven again on the
    /// same session with the same flags, read again from the session. At no time do two write
    /// the session.
    #[tokio::test]
    async fn a_session_goes_to_pis_tui_and_comes_back() {
        let rig = Rig::new();
        let (pi, served) = rig.serve();
        let id = IntentId::new();
        let Outcome::Started { thread } = pi.start(id, rig.start("Say hello.")).await else {
            panic!("not started");
        };
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(
            state.meta.facts.get("session-file").map(String::as_str),
            Some(rig.session_file.to_string_lossy().as_ref()),
            "where pi said it keeps the session"
        );
        assert!(state.meta.can(Cap::HANDOFF));
        let (_, back) = rig.intent(&pi, thread, &Intent::TakeBack);
        assert!(matches!(back, Outcome::Refused { .. }), "Slopty holds it: {back:?}");

        assert_eq!(rig.intent(&pi, thread, &Intent::Handoff).1, Outcome::Accepted);
        let state = rig
            .until(thread, "the TUI's four turns", |s| {
                held_by_tui(s) && turn_ended(4, TurnState::Interrupted)(s)
            })
            .await;
        assert_eq!(state.status.liveness, Liveness::Live);
        assert_eq!(state.meta.caps, [Cap::named(Cap::HANDOFF)], "it can only be taken back");
        let opened = rig.terminals.opened.lock().clone();
        let program = rig.programs.join("pi").to_string_lossy().into_owned();
        let session = id.to_string();
        let want =
            vec![program, "--session-id".to_owned(), session.clone(), "--offline".to_owned()];
        assert_eq!(opened, [(want.clone(), rig.work.to_string_lossy().into_owned())]);
        assert_eq!(rig.tui_record()["argv"], serde_json::json!(&want[1..]));
        let send = Intent::Send {
            text: "Hello?".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![],
        };
        assert!(!matches!(rig.intent(&pi, thread, &send).1, Outcome::Done), "the TUI's now");
        let (_, again) = rig.intent(&pi, thread, &Intent::Handoff);
        assert!(matches!(again, Outcome::Refused { .. }), "{again:?}");

        served.abort();
        let (pi, _served) = rig.serve();
        rig.until(thread, "followed again", held_by_tui).await;
        assert_eq!(rig.terminals.opened.lock().len(), 1, "the same TUI");

        assert_eq!(rig.intent(&pi, thread, &Intent::TakeBack).1, Outcome::Accepted);
        let live = |s: &ThreadState| held_by_slopty(s) && s.status.liveness == Liveness::Live;
        let state = rig.until(thread, "driven again", live).await;
        assert!(state.meta.can(Cap::STEER));
        let record = rig.record_once(|r| r["heard"][0]["type"] == "get_entries").await;
        let argv = record["argv"].as_array().unwrap();
        assert_eq!(argv[5], session.as_str(), "the same session");
        assert_eq!(argv.last().unwrap(), "--offline", "with the flags it was started with");

        let again = rig.send(&pi, thread, "Say hello.");
        let state =
            rig.until(thread, "a turn driven again", turn_ended(5, TurnState::Complete)).await;
        assert_eq!(users(&state).last(), Some(&("Say hello.".to_owned(), Some(again))));
        assert_eq!(rig.record()["clash"], false, "one writer");
        assert_eq!(rig.tui_record()["clash"], false, "one writer");
        assert_eq!(rig.record()["unexpected"], serde_json::json!([]));
    }

    /// A TUI the person ends gives the session back: Slopty holds it, pi exited, and the next
    /// message drives pi again from the session.
    #[tokio::test]
    async fn a_tui_the_person_ends_gives_the_session_back() {
        let rig = Rig::with_tui(true);
        let (pi, _served) = rig.serve();
        let Outcome::Started { thread } = pi.start(IntentId::new(), rig.start("Say hello.")).await
        else {
            panic!("not started");
        };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(rig.intent(&pi, thread, &Intent::Handoff).1, Outcome::Accepted);
        let given_back = |s: &ThreadState| {
            held_by_slopty(s)
                && s.status.liveness == (Liveness::Exited { resumable: true })
                && s.turns.len() == 4
        };
        let state = rig.until(thread, "given back with what the TUI wrote", given_back).await;
        assert_eq!(state.turns[3].state, TurnState::Interrupted);
        let again = rig.send(&pi, thread, "Say hello.");
        let state = rig.until(thread, "driven again", turn_ended(5, TurnState::Complete)).await;
        assert_eq!(users(&state).last(), Some(&("Say hello.".to_owned(), Some(again))));
        assert_eq!(rig.record()["clash"], false, "one writer");
    }

    /// A start pi cannot take is refused once, with why: a flag that would loosen the gate, a
    /// folder that is not there, pi not installed.
    #[tokio::test]
    async fn a_start_pi_cannot_take_is_refused() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let mut loose = rig.start("Say hello.");
        loose.args = vec!["--extension".to_owned(), "/tmp/other.ts".to_owned()];
        let id = IntentId::new();
        let refused = pi.start(id, loose.clone()).await;
        assert!(matches!(&refused, Outcome::Refused { reason } if reason.contains("--extension")));
        loose.args.clear();
        assert_eq!(pi.start(id, loose).await, refused, "refused once");
        let mut nowhere = rig.start("Say hello.");
        nowhere.cwd = rig.work.join("gone").to_string_lossy().into_owned();
        assert!(matches!(pi.start(IntentId::new(), nowhere).await, Outcome::Refused { .. }));
        assert_eq!(rig.host.threads(), []);

        let (bare, asks) = Pi::channel();
        let empty = rig.data.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let terminals: Arc<dyn Terminals> = Arc::<Kept>::clone(&rig.terminals);
        let path = Some(empty.into_os_string());
        let _bare = pi::spawn(rig.host.clone(), rig.data.clone(), path, terminals, asks);
        let missing = bare.start(IntentId::new(), rig.start("Say hello.")).await;
        assert_eq!(missing, Outcome::Refused { reason: "pi is not installed".to_owned() });
        assert!(!rig.record.exists(), "no pi ran");
    }

    /// The gate's recording through its first turn, then pi's answers to a thinking level set
    /// to `high` (its response and `thinking_level_changed`) and to the levels the model
    /// supports, as pi's RPC mode writes them (`docs/rpc-commands.md` of pi 1.0.0).
    fn thinking(gate: &str) -> String {
        let mut kept = Vec::new();
        for line in gate.lines() {
            kept.push(line.to_owned());
            if line.contains("\"agent_settled\"") {
                break;
            }
        }
        let steps = [
            serde_json::json!({"dir": "in", "msg": {"id": "think", "type": "set_thinking_level",
                "level": "high"}}),
            serde_json::json!({"dir": "out", "msg": {"id": "think", "type": "response",
                "command": "set_thinking_level", "success": true}}),
            serde_json::json!({"dir": "out", "msg": {"type": "thinking_level_changed",
                "level": "high"}}),
            serde_json::json!({"dir": "in", "msg": {"id": "levels",
                "type": "get_available_thinking_levels"}}),
            serde_json::json!({"dir": "out", "msg": {"id": "levels", "type": "response",
                "command": "get_available_thinking_levels", "success": true,
                "data": {"levels": ["off", "minimal", "low", "medium", "high", "xhigh"]}}}),
        ];
        kept.extend(steps.iter().map(Value::to_string));
        kept.join("\n")
    }

    /// A pi thread offers the thinking levels pi says the model supports, and an effort set
    /// from them goes to pi as its own `set_thinking_level`; the meters say the level pi took.
    /// A level pi does not offer is refused before anything goes.
    #[tokio::test]
    async fn a_pi_threads_effort_is_set_among_the_levels_pi_offers() {
        let rig = Rig::replaying(false, thinking);
        let (pi, _served) = rig.serve();
        let outcome = pi.start(IntentId::new(), rig.start("Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let state = rig
            .until(thread, "the levels", |s| {
                !s.meta.efforts.is_empty() && turn_ended(1, TurnState::Complete)(s)
            })
            .await;
        let levels: Vec<(&str, &str)> =
            state.meta.efforts.iter().map(|e| (e.id.as_str(), e.label.as_str())).collect();
        let want = [
            ("off", "Off"),
            ("minimal", "Minimal"),
            ("low", "Low"),
            ("medium", "Medium"),
            ("high", "High"),
            ("xhigh", "Extra high"),
        ];
        assert_eq!(levels, want);
        assert!(state.meta.can(Cap::SET_EFFORT));

        let max = Intent::SetEffort { effort: "max".to_owned() };
        let (_, refused) = rig.intent(&pi, thread, &max);
        assert!(matches!(refused, Outcome::Refused { .. }), "not offered: {refused:?}");
        let high = Intent::SetEffort { effort: "high".to_owned() };
        assert_eq!(rig.intent(&pi, thread, &high).1, Outcome::Done);
        rig.until(thread, "the level pi took", |s| s.meters.effort.as_deref() == Some("high"))
            .await;
        // The stand-in writes its record after what it answers.
        let record = rig
            .record_once(|r| {
                r["heard"].as_array().is_some_and(|h| h.iter().any(|c| c["level"] == "high"))
            })
            .await;
        assert_eq!(record["unexpected"], serde_json::json!([]), "{record:#}");
        let set: Vec<&Value> = record["heard"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["type"] == "set_thinking_level")
            .collect();
        assert_eq!(set.len(), 1, "once: {record:#}");
        assert_eq!(set[0]["level"], "high");
    }

    const PNG: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];

    /// A picture sent with a message goes to pi whole, beside the person's words, which stay
    /// as they were written; a file that is not here is refused before anything goes.
    #[tokio::test]
    async fn a_picture_goes_to_pi_with_the_message() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let mut start = rig.start("");
        start.prompt = None;
        let Outcome::Started { thread } = pi.start(IntentId::new(), start).await else {
            panic!("not started");
        };
        let shot = rig.work.join("shot.dat");
        std::fs::write(&shot, PNG).unwrap();
        let send = |paths: Vec<String>| Intent::Send {
            text: "Say hello.".to_owned(),
            delivery: Delivery::Steer,
            attachments: paths,
        };
        let gone = rig.work.join("gone.png").to_string_lossy().into_owned();
        let (_, refused) = rig.intent(&pi, thread, &send(vec![gone]));
        assert!(matches!(refused, Outcome::Refused { .. }), "{refused:?}");
        let (id, sent) = rig.intent(&pi, thread, &send(vec![shot.to_string_lossy().into()]));
        assert_eq!(sent, Outcome::Done);
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(users(&state), [("Say hello.".to_owned(), Some(id))]);
        // The stand-in writes what it heard after it answers, so the turn can end first.
        let prompted = |r: &Value| {
            r["heard"].as_array().is_some_and(|h| h.iter().any(|c| c["type"] == "prompt"))
        };
        let record = rig.record_once(prompted).await;
        let prompt =
            record["heard"].as_array().unwrap().iter().find(|c| c["type"] == "prompt").unwrap();
        let picture = slopty_agent::attach::Attached::Picture {
            path: String::new(),
            media_type: "image/png",
            bytes: PNG.to_vec(),
        };
        let data = picture.base64();
        assert_eq!(
            prompt["images"],
            serde_json::json!([{"type": "image", "data": data, "mimeType": "image/png"}])
        );
        assert_eq!(record["unexpected"], serde_json::json!([]));
    }

    /// A fork runs pi on a new session, named by the fork's intent, that copies the whole of
    /// the thread's (`--fork`) with the thread's own flags, and reads the new thread from the
    /// copy; the new thread says which thread and turn it came from, and keeps saying so once
    /// read again. A fork from an earlier turn is refused: pi copies whole sessions.
    #[tokio::test]
    async fn a_fork_copies_the_whole_session_into_a_new_thread() {
        let rig = Rig::new();
        let (pi, _served) = rig.serve();
        let id = IntentId::new();
        let Outcome::Started { thread } = pi.start(id, rig.start("Say hello.")).await else {
            panic!("not started");
        };
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.meta.can(Cap::FORK));
        let last = state.turns[0].id;
        let early = pi.fork(thread, IntentId::new(), Some(TurnId(last.0 + 7))).await;
        assert!(matches!(early, Outcome::Refused { .. }), "{early:?}");

        let fork = IntentId::new();
        let Outcome::Started { thread: forked } = pi.fork(thread, fork, Some(last)).await else {
            panic!("not forked");
        };
        assert_ne!(forked, thread);
        assert_eq!(pi.fork(thread, fork, Some(last)).await, Outcome::Started { thread: forked });
        let record = rig.record_once(|r| r["heard"][0]["type"] == "get_entries").await;
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        let (new, old) = (fork.to_string(), id.to_string());
        assert_eq!(argv[4..], ["--session-id", &new, "--offline", "--fork", &old]);
        let state = rig.until(forked, "the copy read", |s| s.turns.len() == 4).await;
        let from = Fork { thread, turn: Some(last) };
        assert_eq!(state.meta.forked_from, Some(from));
        assert_eq!(state.meta.origin, ThreadMeta::FORK);
        assert_eq!(state.meta.native, new);
        assert_eq!(slopty_agent::pi::driven::args_of(&state.meta), ["--offline"], "no --fork kept");
    }

    /// pi's sessions in a folder are listed from its session directory, the last written
    /// first, each called by its name or first message; one taken up again (`--session`) runs
    /// pi on that session, read from its entries first, and a second start finds the same
    /// thread.
    #[tokio::test]
    async fn a_past_session_is_listed_and_taken_up_again() {
        let rig = Rig::new();
        let agent = rig.data.join("pi-agent");
        let cwd = rig.work.to_string_lossy().into_owned();
        assert_eq!(pi::sessions(&agent, &cwd, 10).await, Ok(Vec::new()), "no folder yet");
        let folder = slopty_agent::pi::sessions::folder(&agent, &cwd);
        std::fs::create_dir_all(&folder).unwrap();
        let first = concat!(
            r#"{"type":"session","version":3,"id":"s-1","cwd":"/w"}"#,
            "\n",
            r#"{"type":"message","id":"1","message":{"role":"user","content":"Fix the login","timestamp":1}}"#,
            "\n",
        );
        let at = |secs: u64| std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        for (name, words, secs) in [
            ("2026-10-01T10-00-00-000Z_s-1.jsonl", first, 100),
            ("2026-10-02T10-00-00-000Z_s-2.jsonl", "{}\n", 200),
        ] {
            let file = folder.join(name);
            std::fs::write(&file, words).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(at(secs))
                .unwrap();
        }
        std::fs::write(folder.join("notes.txt"), "not a session").unwrap();
        let listed = pi::sessions(&agent, &cwd, 10).await.unwrap();
        let names: Vec<_> =
            listed.iter().map(|s| (s.native.as_str(), s.title.as_deref())).collect();
        assert_eq!(names, [("s-2", None), ("s-1", Some("Fix the login"))]);
        assert_eq!(pi::sessions(&agent, &cwd, 1).await.unwrap().len(), 1);

        let (pi, _served) = rig.serve();
        let mut start = rig.start("");
        start.prompt = None;
        start.args = [listed[1].resume.clone(), vec!["--offline".to_owned()]].concat();
        let Outcome::Started { thread } = pi.start(IntentId::new(), start.clone()).await else {
            panic!("not started");
        };
        assert_eq!(thread, slopty_agent::pi::driven::thread_of("s-1"));
        let record = rig.record_once(|r| r["heard"][0]["type"] == "get_entries").await;
        assert_eq!(record["argv"][5], "s-1");
        assert_eq!(record["argv"][6], "--offline");
        let state = rig.until(thread, "read from its entries", |s| s.turns.len() == 4).await;
        assert_eq!(state.meta.native, "s-1");
        assert_eq!(pi.start(IntentId::new(), start).await, Outcome::Started { thread });
        assert_eq!(rig.host.threads(), [thread]);
    }
}
