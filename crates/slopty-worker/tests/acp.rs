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
    use slopty_core::ClientId;
    use slopty_proto::thread::wire::{Intent, Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Answerer, Cap, Delivery, Drive, IntentId, ItemBody, Liveness, Phase, RequestState,
        ThreadId, ThreadState, ToolState, TurnState,
    };
    use slopty_worker::thread::Host;
    use slopty_worker::thread::acp::{self, Acp};
    use slopty_worker::thread::log::Limits;

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
                args: Vec::new(),
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
            let send = Intent::Send { text: text.to_owned(), delivery: Delivery::Queue };
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
        let steer = Intent::Send { text: "Go on.".to_owned(), delivery: Delivery::Steer };
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
        let send = Intent::Send { text: "Hi.".to_owned(), delivery: Delivery::Queue };
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
}
