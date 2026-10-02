//! A pi thread end to end on the worker: the thread host, the pi adapter's IO, and the
//! stand-in `pi` (`slopty-stub-pi`) on the `PATH` it is found on, replaying the recording of
//! the pinned pi with Slopty's gate (`crates/slopty-agent/tests/fixtures/pi/gate.jsonl`). The
//! stand-in fails a command it has no record of, so what the worker sent is what pi was sent.

#[cfg(test)]
mod pi {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::Value;
    use slopty_core::ClientId;
    use slopty_proto::thread::wire::{Intent, Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Answerer, Cap, Delivery, Drive, IntentId, ItemBody, Liveness, Phase, RequestState,
        ThreadId, ThreadState, ToolState, TurnState,
    };
    use slopty_worker::thread::Host;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::pi::{self, Pi};

    const WAIT: Duration = Duration::from_secs(30);

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
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
            std::os::unix::fs::symlink(bin("slopty-stub-pi"), programs.join("pi")).unwrap();
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../slopty-agent/tests/fixtures/pi/gate.jsonl")
                .canonicalize()
                .unwrap();
            let record = root.join("record.json");
            let config = serde_json::json!({ "fixture": fixture, "record": record });
            std::fs::write(programs.join("stub-pi.json"), config.to_string()).unwrap();
            let host = Host::open(&data.join("threads"), Limits::default()).unwrap();
            Self { _dir: dir, host, data, work, programs, record, me: ClientId::new() }
        }

        /// The pi threads served, and what is asked of them.
        fn serve(&self) -> (Pi, tokio::task::JoinHandle<()>) {
            let (pi, asks) = Pi::channel();
            let path = Some(self.programs.clone().into_os_string());
            (pi, pi::spawn(self.host.clone(), self.data.clone(), path, asks))
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
            let send = Intent::Send { text: text.to_owned(), delivery: Delivery::Steer };
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
        assert!(rig.host.threads().is_empty());

        let (bare, asks) = Pi::channel();
        let empty = rig.data.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let _bare =
            pi::spawn(rig.host.clone(), rig.data.clone(), Some(empty.into_os_string()), asks);
        let missing = bare.start(IntentId::new(), rig.start("Say hello.")).await;
        assert_eq!(missing, Outcome::Refused { reason: "pi is not installed".to_owned() });
        assert!(!rig.record.exists(), "no pi ran");
    }
}
