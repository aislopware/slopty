//! An ACP thread end to end on the worker: the thread host, the ACP adapter's IO, and the
//! stand-in agent (`slopty-stub-acp`) on the `PATH` it is found on under a registry agent's
//! program name, replaying an ACP session (`crates/slopty-agent/tests/fixtures/acp/`). The
//! stand-in answers a message it has no record of with an error and notes it, so what the
//! worker sent is what the agent was sent.

#[cfg(test)]
mod acp {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::Value;
    use slopty_core::{ClientId, SessionId};
    use slopty_proto::thread::wire::{Intent, Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Answerer, Cap, Delivery, Drive, Fork, IntentId, ItemBody, Liveness, Pending,
        Phase, RequestState, ThreadId, ThreadMeta, ThreadState, ToolState, TurnState,
    };
    use slopty_worker::thread::acp::{self, Acp};
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Host, Seated};

    const WAIT: Duration = Duration::from_secs(30);

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../slopty-agent/tests/fixtures/acp")
            .join(name)
            .canonicalize()
            .unwrap()
    }

    /// A worker's threads, a project to work in, and the stand-in as `opencode` alone on a
    /// `PATH` of its own: the registry's `opencode`, started as `opencode acp`.
    struct Rig {
        _dir: tempfile::TempDir,
        host: Host,
        work: PathBuf,
        programs: PathBuf,
        record: PathBuf,
        me: ClientId,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let (data, work, programs) = (root.join("data"), root.join("work"), root.join("bin"));
            for made in [&data, &work, &programs] {
                std::fs::create_dir_all(made).unwrap();
            }
            std::os::unix::fs::symlink(bin("slopty-stub-acp"), programs.join("opencode")).unwrap();
            let host = Host::open(&data.join("threads"), Limits::default()).unwrap();
            let rig = Self {
                _dir: dir,
                host,
                work,
                programs,
                record: root.join("record.json"),
                me: ClientId::new(),
            };
            rig.replay("turns.jsonl");
            rig
        }

        /// The stand-in started from now on replays fixture `name`.
        fn replay(&self, name: &str) {
            let config = serde_json::json!({ "fixture": fixture(name), "record": self.record });
            std::fs::write(self.programs.join("stub-acp.json"), config.to_string()).unwrap();
        }

        /// The ACP threads served, and what is asked of them.
        fn serve(&self) -> (Acp, tokio::task::JoinHandle<()>) {
            let (acp, asks) = Acp::channel();
            let path = Some(self.programs.clone().into_os_string());
            let own: acp::Own = Arc::new(BTreeMap::new);
            (acp, acp::spawn(self.host.clone(), path, own, asks))
        }

        fn start(&self, agent: &str, prompt: &str) -> Box<Start> {
            Box::new(Start {
                agent: AgentId::named(agent),
                cwd: self.work.to_string_lossy().into_owned(),
                drive: None,
                prompt: Some(prompt.to_owned()),
                model: None,
                mode: None,
                effort: None,
                attachments: Vec::new(),
                args: Vec::new(),
                worktree: None,
            })
        }

        fn by(&self) -> Answerer {
            Answerer { client: Some(self.me), name: "Slopty".to_owned() }
        }

        /// Intent `intent` on `thread`, decided as the daemon decides it: once per id.
        fn intent(&self, acp: &Acp, thread: ThreadId, intent: &Intent) -> (IntentId, Outcome) {
            let id = IntentId::new();
            let decided = self.host.intent(thread, id, |state| {
                if !state.meta.can(intent.needs()) {
                    return (Outcome::Unsupported { cap: Cap::named(intent.needs()) }, Vec::new());
                }
                (acp.decide(state, id, intent, self.by()), Vec::new())
            });
            (id, decided.unwrap())
        }

        fn send(&self, acp: &Acp, thread: ThreadId, text: &str) -> IntentId {
            let send = Intent::Send {
                text: text.to_owned(),
                delivery: Delivery::Queue,
                attachments: vec![],
            };
            let (id, outcome) = self.intent(acp, thread, &send);
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
            waited.unwrap_or_else(|_| {
                let record = std::fs::read_to_string(&self.record).unwrap_or_default();
                panic!("{what}: {:#?}\nthe stand-in: {record}", self.host.state(thread))
            })
        }

        /// What the stand-in that ran last was given and heard.
        fn record(&self) -> Value {
            serde_json::from_slice(&std::fs::read(&self.record).unwrap()).unwrap()
        }

        /// The stand-in's record once `done` holds of it: it writes what it heard after it
        /// answers.
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

    fn texts(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Text(text) => Some(text.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A message sent by interrupt to an agent with no steer of its own goes first in its queue
    /// and stops the turn under way, on the person's word: the stopped turn ends as the agent
    /// ends it, and the message goes as the next turn, as the person's, before what waited.
    #[tokio::test]
    async fn a_message_sent_by_interrupt_stops_the_turn_and_goes_next() {
        let rig = Rig::new();
        // The recording's first turn, its third call's turn stopped, then a turn on the words
        // sent by interrupt, which the agent answers as it answered the first.
        let lines = lines("turns.jsonl");
        let resent = lines[4..13].iter().map(|l| l.replace("Say hello.", "Say hello instead."));
        let composed: Vec<String> =
            lines[..13].iter().chain(&lines[35..44]).cloned().chain(resent).collect();
        rig.replay_lines(&composed);
        let (acp, _served) = rig.serve();
        let outcome = acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let act = |id, intent: &Intent| -> Outcome {
            let decide = |s: &ThreadState| (acp.decide(s, id, intent, rig.by()), Vec::new());
            rig.host.intent(thread, id, decide).unwrap()
        };
        let now = |text: &str| Intent::Send {
            text: text.to_owned(),
            delivery: Delivery::Interrupt,
            attachments: vec![],
        };
        assert_eq!(now("Say hello instead.").needs(), Cap::INTERRUPT);

        rig.send(&acp, thread, "Remove it again.");
        rig.until(thread, "the call's ask", asking(1)).await;
        let id = IntentId::new();
        assert_eq!(act(id, &now("Say hello instead.")), Outcome::Done);
        assert_eq!(act(id, &now("Say hello instead.")), Outcome::Done, "once");
        let state =
            rig.until(thread, "the message's turn", turn_ended(3, TurnState::Complete)).await;
        assert_eq!(state.turns[1].state, TurnState::Interrupted, "the turn under way stopped");
        assert_eq!(state.requests[0].state, RequestState::Withdrawn);
        assert!(state.pending.is_empty(), "it went");
        let (text, intent) = users(&state).pop().unwrap();
        assert_eq!((text.as_str(), intent), ("Say hello instead.", Some(id)), "as the person's");
        let record = rig.record();
        assert_eq!(record["unexpected"], serde_json::json!([]), "the agent was sent what it was");
        let cancels = record["heard"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["method"] == "session/cancel")
            .count();
        assert_eq!(cancels, 1, "stopped once");
    }

    /// "Send now" on a message held for an agent with no steer of its own goes as one sent by
    /// interrupt does: the turn under way stops, and the held message goes next, under the
    /// intent that queued it, as the person's.
    #[tokio::test]
    async fn a_held_message_sent_now_stops_the_turn_and_goes_next() {
        let rig = Rig::new();
        let lines = lines("turns.jsonl");
        let resent = lines[4..13].iter().map(|l| l.replace("Say hello.", "Say hello instead."));
        let composed: Vec<String> =
            lines[..13].iter().chain(&lines[35..44]).cloned().chain(resent).collect();
        rig.replay_lines(&composed);
        let (acp, _served) = rig.serve();
        let outcome = acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let act = |id, intent: &Intent| -> Outcome {
            let decide = |s: &ThreadState| (acp.decide(s, id, intent, rig.by()), Vec::new());
            rig.host.intent(thread, id, decide).unwrap()
        };
        let promote = |pending| Intent::Promote { pending };
        assert_eq!(promote(IntentId::new()).needs(), Cap::QUEUE);

        rig.send(&acp, thread, "Remove it again.");
        rig.until(thread, "the call's ask", asking(1)).await;
        let queued = IntentId::new();
        let held = Intent::Send {
            text: "Say hello instead.".to_owned(),
            delivery: Delivery::Queue,
            attachments: vec![],
        };
        assert_eq!(act(queued, &held), Outcome::Done);
        rig.until(thread, "the message held", |s: &ThreadState| s.pending.len() == 1).await;
        assert_eq!(act(IntentId::new(), &promote(queued)), Outcome::Done);
        let state =
            rig.until(thread, "the message's turn", turn_ended(3, TurnState::Complete)).await;
        assert_eq!(state.turns[1].state, TurnState::Interrupted, "the turn under way stopped");
        assert!(state.pending.is_empty(), "it went");
        let (text, intent) = users(&state).pop().unwrap();
        assert_eq!((text.as_str(), intent), ("Say hello instead.", Some(queued)), "as queued");
        assert_eq!(rig.record()["unexpected"], serde_json::json!([]));
    }

    /// The person's stop holds what is queued: the turn is cancelled, the message queued says it
    /// waits on the stop, and no prompt follows once the cancelled turn ends. Their next message
    /// lets it go first, and theirs after it, each as its own turn.
    #[tokio::test]
    async fn a_stop_holds_the_queue_until_the_person_sends_again() {
        let rig = Rig::new();
        // The recording's first turn, its third call's turn stopped, then two turns the agent
        // answers as it answered the first.
        let lines = lines("turns.jsonl");
        let again = |text: &str| {
            lines[4..13].iter().map(|l| l.replace("Say hello.", text)).collect::<Vec<_>>()
        };
        let composed: Vec<String> = lines[..13]
            .iter()
            .chain(&lines[35..44])
            .cloned()
            .chain(again("Say hello later."))
            .chain(again("Say hello again."))
            .collect();
        rig.replay_lines(&composed);
        let (acp, _served) = rig.serve();
        let outcome = acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        rig.send(&acp, thread, "Remove it again.");
        rig.until(thread, "the call's ask", asking(1)).await;
        let later = rig.send(&acp, thread, "Say hello later.");
        rig.until(thread, "it waits", |s| s.pending.len() == 1).await;

        let (_, stopped) = rig.intent(&acp, thread, &Intent::Interrupt);
        assert_eq!(stopped, Outcome::Done);
        let state = rig.until(thread, "stopped", turn_ended(2, TurnState::Interrupted)).await;
        assert!(state.pending.iter().all(Pending::stopped), "held: {:?}", state.pending);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let state = rig.host.state(thread).unwrap().0;
        assert_eq!((state.turns.len(), state.pending.len()), (2, 1), "nothing went on its own");

        let then = rig.send(&acp, thread, "Say hello again.");
        let state = rig.until(thread, "both went", turn_ended(4, TurnState::Complete)).await;
        assert!(state.pending.is_empty(), "both went");
        let users = users(&state);
        assert_eq!(
            users[2..],
            [
                ("Say hello later.".to_owned(), Some(later)),
                ("Say hello again.".to_owned(), Some(then))
            ],
            "the held one first, then theirs"
        );
        assert_eq!(rig.record()["unexpected"], serde_json::json!([]));
    }

    /// A thread started on an ACP agent runs the person's program for it, as the registry
    /// starts it, in the thread's folder; its first turn streams the agent's answer. Each call
    /// the agent asks about is a request on the thread with the answers the agent offers: an
    /// answer it does not offer goes nowhere, a message sent while a turn waits goes once the
    /// turn ends, and an allow, a reject and an interrupt each end the call as the agent ended
    /// it.
    #[tokio::test]
    async fn an_acp_thread_runs_turns_and_asks_before_it_acts() {
        let rig = Rig::new();
        let (acp, _served) = rig.serve();
        let id = IntentId::new();
        let outcome = acp.start(id, rig.start("acp:opencode", "Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let again = acp.start(id, rig.start("acp:opencode", "Say hello.")).await;
        assert_eq!(again, outcome, "started once");
        assert_eq!(rig.host.threads(), [thread]);

        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(state.meta.agent, slopty_agent::acp::agent_id("opencode"));
        assert_eq!(state.meta.agent_version, "1.18.34", "as the agent said");
        assert_eq!(state.meta.native, "ses_00000000000000000000000001", "the agent's session");
        assert_eq!(state.meta.title, "Say hello.");
        assert!(state.meta.drive.is(Drive::DRIVEN));
        for cap in [Cap::APPROVALS, Cap::INTERRUPT, Cap::QUEUE, Cap::SET_MODE, Cap::SET_MODEL] {
            assert!(state.meta.can(cap), "{cap}");
        }
        assert!(!state.meta.can(Cap::STEER), "ACP takes no message while a turn runs");
        assert_eq!(users(&state), [("Say hello.".to_owned(), Some(id))]);
        assert_eq!(texts(&state), ["Hello, there."]);
        assert_eq!(state.meters.mode.as_deref(), Some("build"));
        assert_eq!(state.meters.context_tokens, Some(12));
        assert_eq!(state.status.phase, Phase::Done);
        let record = rig.record();
        assert_eq!(record["argv"], serde_json::json!(["acp"]), "as the registry starts it");
        assert_eq!(record["cwd"], rig.work.to_string_lossy().as_ref());
        let opened = &record["heard"][1];
        assert_eq!(opened["method"], "session/new");
        assert_eq!(opened["params"]["cwd"], rig.work.to_string_lossy().as_ref());
        let steer = Intent::Send {
            text: "Go on.".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![],
        };
        let (_, steered) = rig.intent(&acp, thread, &steer);
        assert_eq!(steered, Outcome::Unsupported { cap: Cap::named(Cap::STEER) });

        rig.send(&acp, thread, "Make a file called made-by-acp.");
        let state = rig.until(thread, "the first ask", asking(1)).await;
        assert_eq!(state.status.phase, Phase::NeedsYou);
        let request = &state.requests[0];
        assert_eq!(request.title, "/work/made-by-acp?", "the call, as the agent titles it");
        let offered: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(offered, ["once", "always", "reject"], "what the agent offers, no more");
        let ask = request.id.clone();
        let queued = rig.send(&acp, thread, "Remove it.");
        let state = rig.until(thread, "the message waits", |s| s.pending.len() == 1).await;
        assert_eq!(state.pending[0].intent, queued);
        let now = Intent::Promote { pending: queued };
        let promoted = rig.intent(&acp, thread, &now).1;
        assert_eq!(promoted, Outcome::Unsupported { cap: Cap::named(Cap::STEER) }, "no steer");
        let answer = |choice: &str| Intent::Answer {
            ask: ask.clone(),
            choice: choice.to_owned(),
            message: None,
        };
        let (_, maybe) = rig.intent(&acp, thread, &answer("allow"));
        assert!(matches!(maybe, Outcome::Refused { .. }), "only what the agent offers: {maybe:?}");
        let release = Intent::Release { ask: ask.clone() };
        let (_, released) = rig.intent(&acp, thread, &release);
        assert!(matches!(released, Outcome::Refused { .. }), "no prompt to give it to");
        assert_eq!(rig.intent(&acp, thread, &answer("once")).1, Outcome::Done);

        // The allowed turn ends, and the message that waited goes as the next.
        let state = rig.until(thread, "the queued message's ask", asking(2)).await;
        assert_eq!(state.turns[1].state, TurnState::Complete);
        assert_eq!(
            state.requests[0].state,
            RequestState::Answered { by: rig.by(), choice: "once".to_owned() }
        );
        assert!(state.pending.is_empty(), "the message went");
        assert_eq!(rig.intent(&acp, thread, &answer("reject")).1, Outcome::Done, "settled");
        let ask = state.requests[1].id.clone();
        let reject = Intent::Answer { ask, choice: "reject".to_owned(), message: None };
        assert_eq!(rig.intent(&acp, thread, &reject).1, Outcome::Done);
        rig.until(thread, "the rejected turn", turn_ended(3, TurnState::Complete)).await;

        rig.send(&acp, thread, "Remove it again.");
        rig.until(thread, "the third ask", asking(3)).await;
        assert_eq!(rig.intent(&acp, thread, &Intent::Interrupt).1, Outcome::Done);
        let state =
            rig.until(thread, "the stopped turn", turn_ended(4, TurnState::Interrupted)).await;
        assert_eq!(state.status.phase, Phase::Stopped);
        let want = [
            ("toolu_1".to_owned(), ToolState::Completed),
            ("toolu_2".to_owned(), ToolState::Rejected),
            ("toolu_3".to_owned(), ToolState::Cancelled),
        ];
        assert_eq!(tools(&state), want);
        assert_eq!(state.requests[2].state, RequestState::Withdrawn);
        let users: Vec<Option<IntentId>> = users(&state).into_iter().map(|(_, i)| i).collect();
        assert_eq!(users[2], Some(queued), "the waiting message, as its intent sent it");
        let (_, late) = rig.intent(&acp, thread, &Intent::Interrupt);
        assert!(matches!(late, Outcome::Refused { .. }), "nothing to stop: {late:?}");

        let record = rig.record();
        assert_eq!(record["unexpected"], serde_json::json!([]), "the agent was sent what it was");
        let heard = record["heard"].as_array().unwrap();
        let refused: Vec<&Value> = heard.iter().filter_map(|m| m.get("error")).collect();
        assert_eq!(refused.len(), 1, "the agent's own write to the file system, refused");
        assert_eq!(refused[0]["code"], -32_601);
        let answers: Vec<&Value> = heard
            .iter()
            .filter(|m| m.get("method").is_none() && m.get("error").is_none())
            .map(|m| &m["result"]["outcome"])
            .collect();
        let want = [
            serde_json::json!({"outcome": "selected", "optionId": "once"}),
            serde_json::json!({"outcome": "selected", "optionId": "reject"}),
            serde_json::json!({"outcome": "cancelled"}),
        ];
        assert_eq!(answers, want.iter().collect::<Vec<_>>());
    }

    /// A thread whose agent is gone is exited, and resumable since the agent loads sessions; the
    /// next message starts the agent again, loads the session, reads the thread again from what
    /// the agent replays (keeping the intent of a message it knew), and then sends the message.
    #[tokio::test]
    async fn an_acp_agent_that_is_gone_is_taken_up_again_by_loading_its_session() {
        let rig = Rig::new();
        let (acp, served) = rig.serve();
        let id = IntentId::new();
        let Outcome::Started { thread } =
            acp.start(id, rig.start("acp:opencode", "Say hello.")).await
        else {
            panic!("not started");
        };
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;

        served.abort();
        let exited = |s: &ThreadState| s.status.liveness == (Liveness::Exited { resumable: true });
        rig.until(thread, "the agent ends with the worker", exited).await;
        rig.replay("load.jsonl");
        let (acp, _served) = rig.serve();
        let (_, stop) = rig.intent(&acp, thread, &Intent::Interrupt);
        assert!(matches!(stop, Outcome::Refused { .. }), "only a message wakes it: {stop:?}");

        let again = rig.send(&acp, thread, "Say hello again.");
        let state =
            rig.until(thread, "the turn after the load", turn_ended(5, TurnState::Complete)).await;
        assert_eq!(state.status.liveness, Liveness::Live);
        assert_eq!(
            users(&state),
            [
                ("Say hello.".to_owned(), Some(id)),
                ("Make a file called made-by-acp.".to_owned(), None),
                ("Remove it.".to_owned(), None),
                ("Remove it again.".to_owned(), None),
                ("Say hello again.".to_owned(), Some(again)),
            ],
            "read again from the agent's record, the intent of the message it knew kept, and \
             the turns it had that the thread had not seen taken in"
        );
        assert_eq!(
            texts(&state),
            ["Hello, there.", "I will write it.", "Made it.", "Hello again."]
        );
        let record = rig.record();
        assert_eq!(record["heard"][1]["method"], "session/load", "the session, loaded");
        assert_eq!(record["heard"][1]["params"]["sessionId"], "ses_00000000000000000000000001");
        assert_eq!(record["unexpected"], serde_json::json!([]));
    }

    /// An agent that asks to be signed in is left as it is: the thread says so in words and
    /// is exited, the first message is not sent to an agent started over and over, and since
    /// the agent named no session there is nothing to take up again.
    #[tokio::test]
    async fn an_agent_that_asks_to_be_signed_in_ends_its_thread_with_the_reason() {
        let rig = Rig::new();
        rig.replay("auth.jsonl");
        let (acp, _served) = rig.serve();
        let Outcome::Started { thread } =
            acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await
        else {
            panic!("not started");
        };
        let exited = |s: &ThreadState| s.status.liveness == (Liveness::Exited { resumable: false });
        let state = rig.until(thread, "the agent is left as it is", exited).await;
        let notices = |state: &ThreadState| -> Vec<String> {
            state
                .items
                .iter()
                .filter_map(|i| match &i.body {
                    ItemBody::Notice(notice) => Some(notice.text.text.clone()),
                    _ => None,
                })
                .collect()
        };
        let said = notices(&state);
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains("Slopty signs no agent in"), "{said:?}");
        assert!(users(&state).is_empty(), "the message never went");
        // An agent started again for the held message would be a second start, and a notice.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let (state, _) = rig.host.state(thread).unwrap();
        assert_eq!(notices(&state).len(), 1, "started once");
        let record = rig.record();
        assert_eq!(record["starts"], 1);
        let heard: Vec<&Value> =
            record["heard"].as_array().unwrap().iter().map(|m| &m["method"]).collect();
        assert_eq!(heard, ["initialize", "session/new"]);
        let send =
            Intent::Send { text: "Hi.".to_owned(), delivery: Delivery::Queue, attachments: vec![] };
        let (_, again) = rig.intent(&acp, thread, &send);
        assert!(matches!(again, Outcome::Refused { .. }), "no session to take up: {again:?}");
    }

    /// What cannot start is refused once, with the reason: an agent the registry does not know,
    /// arguments from a start, a folder that is not there.
    #[tokio::test]
    async fn a_start_an_acp_agent_cannot_take_is_refused() {
        let rig = Rig::new();
        let (acp, _served) = rig.serve();
        let refused = |outcome: &Outcome, why: &str| match outcome {
            Outcome::Refused { reason } => assert!(reason.contains(why), "{reason}"),
            other => panic!("{other:?}"),
        };
        let id = IntentId::new();
        let unknown = acp.start(id, rig.start("acp:nobody", "Hi.")).await;
        refused(&unknown, "No ACP agent nobody");
        assert_eq!(acp.start(id, rig.start("acp:opencode", "Hi.")).await, unknown, "once");
        let mut loose = rig.start("acp:opencode", "Hi.");
        loose.args = vec!["--yolo".to_owned()];
        refused(&acp.start(IntentId::new(), loose).await, "no arguments");
        let mut nowhere = rig.start("acp:opencode", "Hi.");
        nowhere.cwd = "/nowhere/at/all".to_owned();
        refused(&acp.start(IntentId::new(), nowhere).await, "no folder");
        refused(&acp.start(IntentId::new(), rig.start("acp:gemini", "Hi.")).await, "installed");
        assert_eq!(rig.host.threads(), []);
    }

    /// The lines of fixture `name`.
    fn lines(name: &str) -> Vec<String> {
        std::fs::read_to_string(fixture(name)).unwrap().lines().map(str::to_owned).collect()
    }

    impl Rig {
        /// The stand-in started from now on replays `lines`, a recording the test wrote.
        fn replay_lines(&self, lines: &[String]) {
            let written = self.record.with_file_name("written.jsonl");
            std::fs::write(&written, lines.join("\n")).unwrap();
            let config = serde_json::json!({ "fixture": written, "record": self.record });
            std::fs::write(self.programs.join("stub-acp.json"), config.to_string()).unwrap();
        }
    }

    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    /// Sent with files to an agent that takes pictures, a message goes as the person's words, a
    /// picture as an image block with its bytes and where it is, and any other file as a link to
    /// it; the words in the thread stay as they were written.
    #[tokio::test]
    async fn a_picture_goes_as_an_image_and_a_file_as_a_link() {
        let rig = Rig::new();
        let shot = rig.work.join("shot.png");
        std::fs::write(&shot, PNG).unwrap();
        let notes = rig.work.join("my notes.md");
        std::fs::write(&notes, "# notes").unwrap();
        let uri = |p: &Path| format!("file://{}", p.to_string_lossy().replace(' ', "%20"));
        let session = "ses_00000000000000000000000001";
        let prompt = serde_json::json!({"id": 3, "jsonrpc": "2.0", "method": "session/prompt",
        "params": {"sessionId": session, "prompt": [
            {"type": "text", "text": "Look."},
            {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png", "uri": uri(&shot)},
            {"type": "resource_link", "name": "my notes.md", "uri": uri(&notes)},
        ]}});
        let ended =
            r#"{"dir":"out","msg":{"id":3,"jsonrpc":"2.0","result":{"stopReason":"end_turn"}}}"#;
        let turns = lines("turns.jsonl");
        let mut recording = turns[..4].to_vec();
        recording.push(serde_json::json!({"dir": "in", "msg": prompt}).to_string());
        recording.push(ended.to_owned());
        rig.replay_lines(&recording);
        let (acp, _served) = rig.serve();
        let mut start = rig.start("acp:opencode", "");
        start.prompt = None;
        let Outcome::Started { thread } = acp.start(IntentId::new(), start).await else {
            panic!("not started");
        };
        rig.until(thread, "the session made", |s| !s.meta.native.is_empty()).await;
        let send = Intent::Send {
            text: "Look.".to_owned(),
            delivery: Delivery::Queue,
            attachments: [&shot, &notes].map(|p| p.to_string_lossy().into_owned()).to_vec(),
        };
        let (id, outcome) = rig.intent(&acp, thread, &send);
        assert_eq!(outcome, Outcome::Done);
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(users(&state), [("Look.".to_owned(), Some(id))]);
        let record = rig.record();
        assert_eq!(record["heard"][2]["params"]["prompt"], prompt["params"]["prompt"]);
        assert_eq!(record["unexpected"], serde_json::json!([]));
    }

    /// A server task's thread on an ACP agent is given Slopty's tools as an MCP server of its
    /// session, `slopty mcp` with the seat's variables, and its role ahead of the first message,
    /// since ACP has no system prompt of its own. Its row names the seat, kept whatever the
    /// agent says of the thread, and a start repeated at the seat answers with the same thread.
    #[tokio::test]
    async fn a_seated_acp_thread_gets_the_seat_its_tools_and_its_role() {
        let rig = Rig::new();
        let session = "ses_00000000000000000000000001";
        let first = "You review.\n\nSay hello.";
        let prompt = serde_json::json!({"dir": "in", "msg": {"id": 3, "jsonrpc": "2.0",
            "method": "session/prompt", "params": {"sessionId": session,
                "prompt": [{"type": "text", "text": first}]}}});
        let ended =
            r#"{"dir":"out","msg":{"id":3,"jsonrpc":"2.0","result":{"stopReason":"end_turn"}}}"#;
        let mut recording = lines("turns.jsonl")[..4].to_vec();
        recording.extend([prompt.to_string(), ended.to_owned()]);
        rig.replay_lines(&recording);
        let (acp, _served) = rig.serve();
        let seat = SessionId::new();
        let relay = "/opt/slopty/bin/slopty";
        let seated = Seated {
            seat,
            env: vec![
                ("SLOPTY_SESSION".to_owned(), seat.to_string()),
                ("SLOPTY_TASK".to_owned(), "task-1".to_owned()),
            ],
            role: Some("You review.".to_owned()),
            relay: Some(relay.to_owned()),
        };
        let outcome =
            acp.start_seated(rig.start("acp:opencode", "Say hello."), seated.clone()).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let again = acp.start_seated(rig.start("acp:opencode", "Say hello."), seated).await;
        assert_eq!(again, outcome, "one thread a seat");
        assert_eq!(rig.host.threads(), [thread]);
        assert_eq!(rig.host.seated_at(seat), Some(thread));

        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let fact = state.meta.facts.get(slopty_proto::project::SEAT_FACT);
        assert_eq!(fact, Some(&seat.to_string()), "kept over what the agent says of the thread");
        let record = rig.record_once(|r| r["heard"][2]["method"] == "session/prompt").await;
        assert_eq!(record["unexpected"], serde_json::json!([]), "{record:#}");
        let opened = &record["heard"][1];
        assert_eq!(opened["method"], "session/new");
        let tools = serde_json::json!([{"name": "slopty", "command": relay, "args": ["mcp"],
            "env": [{"name": "SLOPTY_SESSION", "value": seat.to_string()},
                {"name": "SLOPTY_TASK", "value": "task-1"}]}]);
        assert_eq!(opened["params"]["mcpServers"], tools);
        assert_eq!(record["heard"][2]["params"]["prompt"][0]["text"], first, "the role first");
    }

    /// A fork runs the agent afresh, which forks the thread's session (`session/fork`) and then
    /// loads the fork, replaying its history into the new thread; the new thread says which
    /// thread and turn it came from. The agent's sessions in a folder are listed by a run of
    /// its own (`session/list`) with the words that take each up again, and one taken up again
    /// is the thread already kept of it.
    #[tokio::test]
    async fn a_fork_and_a_listing_go_to_the_agent() {
        let rig = Rig::new();
        let (acp, _served) = rig.serve();
        let Outcome::Started { thread } =
            acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await
        else {
            panic!("not started");
        };
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.meta.can(Cap::FORK), "the agent forks: {:?}", state.meta.caps);
        let last = state.turns[0].id;

        let (from, to) = ("ses_00000000000000000000000001", "ses_fork");
        let turns = lines("turns.jsonl");
        let mut recording = turns[..2].to_vec();
        let ask = serde_json::json!({"dir": "in", "msg": {"id": 2, "jsonrpc": "2.0",
            "method": "session/fork", "params": {"sessionId": from, "cwd": "/work"}}});
        let forked = serde_json::json!({"dir": "out", "msg": {"id": 2, "jsonrpc": "2.0",
            "result": {"sessionId": to}}});
        recording.extend([ask.to_string(), forked.to_string()]);
        recording.extend(lines("load.jsonl")[2..].iter().map(|l| l.replace(from, to)));
        rig.replay_lines(&recording);
        let fork = IntentId::new();
        let Outcome::Started { thread: branch } = acp.fork(thread, fork, Some(last)).await else {
            panic!("not forked");
        };
        assert_eq!(acp.fork(thread, fork, Some(last)).await, Outcome::Started { thread: branch });
        let state = rig.until(branch, "the fork loaded", |s| s.turns.len() == 4).await;
        assert_eq!(state.meta.native, to);
        assert_eq!(state.meta.forked_from, Some(Fork { thread, turn: Some(last) }));
        assert_eq!(state.meta.origin, ThreadMeta::FORK);
        let record = rig.record();
        assert_eq!(record["heard"][1]["params"]["sessionId"], from);
        assert_eq!(record["heard"][2]["method"], "session/load");
        assert_eq!(record["unexpected"], serde_json::json!([]));

        let mut recording = turns[..2].to_vec();
        let ask = serde_json::json!({"dir": "in", "msg": {"id": 2, "jsonrpc": "2.0",
            "method": "session/list", "params": {"cwd": "/work"}}});
        let listed = serde_json::json!({"dir": "out", "msg": {"id": 2, "jsonrpc": "2.0",
            "result": {"sessions": [{"sessionId": from, "cwd": "/work", "title": "Greetings",
                "updatedAt": "2026-10-03T10:00:00Z"}], "nextCursor": null}}});
        recording.extend([ask.to_string(), listed.to_string()]);
        rig.replay_lines(&recording);
        let agent = AgentId::named("acp:opencode");
        let cwd = rig.work.to_string_lossy().into_owned();
        let past = acp.sessions(agent, cwd, 10).await.unwrap();
        assert_eq!(past.len(), 1);
        assert_eq!(past[0].native, from);
        assert_eq!(past[0].title.as_deref(), Some("Greetings"));
        assert!(past[0].updated_ms.is_some());
        assert_eq!(past[0].resume, ["resume", from]);
        assert_eq!(rig.record()["heard"][1]["params"]["cwd"], rig.work.to_string_lossy().as_ref());

        let mut again = rig.start("acp:opencode", "");
        again.prompt = None;
        again.args = past[0].resume.clone();
        assert_eq!(acp.start(IntentId::new(), again).await, Outcome::Started { thread });
    }

    /// The thought-level option the agent offers, beside its model and mode, as a select
    /// (`thought_level`), and the session's options once it is set to `current`.
    fn thought(current: &str) -> Value {
        serde_json::json!({"category": "thought_level", "currentValue": current,
            "id": "effort", "name": "Thinking", "type": "select", "options": [
                {"value": "low", "name": "Low", "description": "Answers quickly"},
                {"value": "high", "name": "High", "description": "Thinks longer"}]})
    }

    /// An agent that offers a thought level (an ACP config option of that category) lets the
    /// thread's effort be set among its values: the switch goes as `session/set_config_option`,
    /// and the meters say what the agent then holds. A value it does not offer is refused before
    /// anything goes.
    #[tokio::test]
    async fn an_acp_threads_effort_is_set_through_its_thought_level_option() {
        let rig = Rig::new();
        let turns = lines("turns.jsonl");
        let mut recording = turns[..3].to_vec();
        let mut opened: Value = serde_json::from_str(&turns[3]).unwrap();
        let options = opened["msg"]["result"]["configOptions"].as_array_mut().unwrap();
        options.push(thought("low"));
        let after: Vec<Value> = options
            .iter()
            .map(|o| if o["id"] == "effort" { thought("high") } else { o.clone() })
            .collect();
        recording.push(opened.to_string());
        let answered =
            turns.iter().position(|l| l.contains("\"stopReason\"")).expect("the first turn's end");
        recording.extend(turns[4..=answered].iter().cloned());
        let ask = serde_json::json!({"dir": "in", "msg": {"id": 4, "jsonrpc": "2.0",
            "method": "session/set_config_option", "params": {
                "sessionId": "ses_00000000000000000000000001", "configId": "effort",
                "value": "high"}}});
        let set = serde_json::json!({"dir": "out", "msg": {"id": 4, "jsonrpc": "2.0",
            "result": {"configOptions": after}}});
        recording.extend([ask.to_string(), set.to_string()]);
        rig.replay_lines(&recording);
        let (acp, _served) = rig.serve();
        let outcome = acp.start(IntentId::new(), rig.start("acp:opencode", "Say hello.")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.meta.can(Cap::SET_EFFORT), "{:?}", state.meta.caps);
        let efforts: Vec<(&str, &str, Option<&str>)> = state
            .meta
            .efforts
            .iter()
            .map(|e| (e.id.as_str(), e.label.as_str(), e.description.as_deref()))
            .collect();
        assert_eq!(
            efforts,
            [("low", "Low", Some("Answers quickly")), ("high", "High", Some("Thinks longer"))]
        );
        assert_eq!(state.meters.effort.as_deref(), Some("Low"));

        let max = Intent::SetEffort { effort: "max".to_owned() };
        assert!(matches!(rig.intent(&acp, thread, &max).1, Outcome::Refused { .. }));
        let high = Intent::SetEffort { effort: "high".to_owned() };
        assert_eq!(rig.intent(&acp, thread, &high).1, Outcome::Done);
        rig.until(thread, "the agent's level", |s| s.meters.effort.as_deref() == Some("High"))
            .await;
        // The stand-in writes its record after what it answers.
        let record = rig
            .record_once(|r| {
                r["heard"]
                    .as_array()
                    .is_some_and(|h| h.iter().any(|m| m["method"] == "session/set_config_option"))
            })
            .await;
        assert_eq!(record["unexpected"], serde_json::json!([]), "{record:#}");
        let sent = record["heard"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["method"] == "session/set_config_option")
            .unwrap();
        assert_eq!(sent["params"]["configId"], "effort");
        assert_eq!(sent["params"]["value"], "high");
    }
}
