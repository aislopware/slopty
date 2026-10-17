//! Claude Code driven on the worker, end to end: the thread host, the adapter's IO, a real
//! `slopty-ptyd` holding each Claude Code, and the stand-in `claude` (`slopty-stub-claude`,
//! `slopty_testkit::claude_stream`) alone on the `PATH` the adapter is given. The stand-in speaks
//! stream-json with Claude Code's control protocol and fails as a knob in ptyd's environment
//! says; what it was started with and every line it heard is its record.
//!
//! One test per behaviour of the drive: a message delivered and a permission answered by its id,
//! a stop not acknowledged in time escalated, a crash mid-turn, an end by a signal said by its
//! kind, a message never acknowledged held as unknown, a torn line passed over, a permission
//! outstanding across the adapter's restart (ptyd keeps Claude Code), the usage limit's hold, a
//! thread taken up again by its next message, the lease stamped while the process lives and not
//! after, a managed launcher's arguments handed on, a request tried again kept to one row, and a
//! turn Claude Code really took played through the drive, its subagent and its permissions with
//! it.

#[cfg(test)]
mod claude {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use slopty_core::{ClientId, WallMs};
    use slopty_proto::thread::wire::{Intent, Outcome, Start};
    use slopty_proto::thread::{
        AgentId, Answerer, Delivery, IntentId, Liveness, ThreadId, ThreadState, TurnState, Wait,
    };
    use slopty_worker::thread::Host;
    use slopty_worker::thread::claude::custody::Custody;
    use slopty_worker::thread::claude::{self, Claude, Place};
    use slopty_worker::thread::log::Limits;
    use tokio::process::{Child, Command};

    const WAIT: Duration = Duration::from_secs(30);

    /// `name` from this build (`slopty_testkit::bins`), found from the profile directory: this
    /// package has no binary of its own to name.
    fn bin(name: &str) -> PathBuf {
        let exe = std::env::current_exe().unwrap();
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).parent().unwrap();
        let profile = exe.ancestors().find(|dir| dir.parent() == Some(target)).unwrap();
        slopty_testkit::bins::bin(&profile.join("slopty-worker-tests").to_string_lossy(), name)
    }

    /// A worker's threads and data, a project, ptyd with the stand-in's knobs in its
    /// environment, and the stand-in as `claude` alone on a `PATH` of its own.
    struct Rig {
        _dir: tempfile::TempDir,
        host: Host,
        data: PathBuf,
        work: PathBuf,
        home: PathBuf,
        programs: PathBuf,
        record: PathBuf,
        ptyd: Child,
        socket: PathBuf,
        me: ClientId,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            // ptyd leads a group of its own: ended whole, nothing it started outlives the test.
            let ended = slopty_testkit::group::end(
                std::slice::from_mut(&mut self.ptyd),
                Child::id,
                |child| child.try_wait().is_ok_and(|status| status.is_some()),
            );
            debug_assert!(ended, "ptyd's group outlived the test");
        }
    }

    impl Rig {
        /// A rig whose stand-in runs with `knobs` (`slopty_testkit::claude_stream`).
        async fn new(knobs: &[(&str, &str)]) -> Self {
            Self::with(knobs, |programs, _home| {
                std::os::unix::fs::symlink(bin("slopty-stub-claude"), programs.join("claude"))
                    .unwrap();
            })
            .await
        }

        /// A rig whose `claude` `install` puts in the programs directory, under the home.
        async fn with(knobs: &[(&str, &str)], install: impl FnOnce(&Path, &Path)) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let (data, work, home, programs) =
                (root.join("data"), root.join("work"), root.join("home"), root.join("bin"));
            for made in [&data, &work, &home, &programs] {
                std::fs::create_dir_all(made).unwrap();
            }
            install(&programs, &home);
            let record = root.join("record.json");
            let socket = root.join("ptyd.sock");
            let mut command = Command::new(bin("slopty-ptyd"));
            // A clean environment: nothing of the developer's reaches the Claude Code it runs.
            slopty_testkit::env::scrub(command.as_std_mut(), &home);
            let ptyd = command
                .env("STUB_RECORD", &record)
                .envs(knobs.iter().copied())
                .arg("--socket")
                .arg(&socket)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            slopty_testkit::group::ward(ptyd.id());
            tokio::time::timeout(WAIT, async {
                while tokio::net::UnixStream::connect(&socket).await.is_err() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("ptyd listens");
            let host = Host::open(&data.join("threads"), Limits::default()).unwrap();
            Self {
                _dir: dir,
                host,
                data,
                work,
                home,
                programs,
                record,
                ptyd,
                socket,
                me: ClientId::new(),
            }
        }

        /// The driven Claude Code threads served, as a worker serves them.
        fn serve(&self) -> (Claude, tokio::task::JoinHandle<()>) {
            let (claude, asks) = Claude::channel();
            let place = Place {
                data_dir: self.data.clone(),
                ptyd: self.socket.clone(),
                path: Some(self.programs.clone().into_os_string()),
                home: self.home.clone(),
                machine: "rig".to_owned(),
            };
            (claude, claude::spawn(self.host.clone(), place, asks))
        }

        fn start(&self, prompt: &str) -> Box<Start> {
            Box::new(Start {
                agent: AgentId::named(AgentId::CLAUDE_CODE),
                cwd: self.work.to_string_lossy().into_owned(),
                drive: None,
                prompt: Some(prompt.to_owned()),
                model: None,
                mode: None,
                effort: None,
                attachments: Vec::new(),
                args: Vec::new(),
                options: std::collections::BTreeMap::new(),
                worktree: None,
            })
        }

        async fn started(&self, claude: &Claude, prompt: &str) -> ThreadId {
            let outcome = claude.start(IntentId::new(), self.start(prompt)).await;
            let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
            thread
        }

        fn by(&self) -> Answerer {
            Answerer { client: Some(self.me), name: "Slopty".to_owned() }
        }

        /// Intent `intent` on `thread`, decided as the daemon decides it: once per id.
        fn intent(&self, claude: &Claude, thread: ThreadId, intent: &Intent) -> Outcome {
            let id = IntentId::new();
            let decided = self.host.intent(thread, id, |state| {
                if intent.needs().is_some_and(|needs| !state.meta.can(needs)) {
                    return (intent.unsupported(), Vec::new());
                }
                (claude.decide(state, id, intent, self.by()), Vec::new())
            });
            decided.unwrap()
        }

        fn send(&self, claude: &Claude, thread: ThreadId, text: &str) {
            let send = Intent::Send {
                text: text.to_owned(),
                delivery: Delivery::Steer,
                attachments: vec![],
            };
            assert_eq!(self.intent(claude, thread, &send), Outcome::Done, "{text}");
        }

        fn answer(&self, claude: &Claude, thread: ThreadId, choice: &str) -> Outcome {
            let (state, _) = self.host.state(thread).unwrap();
            let ask = state.open_requests().next().expect("an open request").id.clone();
            let answer = Intent::Answer { ask, choice: choice.to_owned(), message: None };
            self.intent(claude, thread, &answer)
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
                    // A stamp of time alone (a lease) changes nothing a test waits on, so the
                    // feed is not the only wake.
                    let _woke = tokio::time::timeout(Duration::from_millis(200), feed.recv()).await;
                }
            })
            .await;
            waited.unwrap_or_else(|_| panic!("{what}: {:#?}", self.host.state(thread)))
        }

        /// What the stand-in that ran last was started with and heard.
        fn record(&self) -> Value {
            std::fs::read(&self.record)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or(Value::Null)
        }

        /// The stand-in's record once `done` holds of it.
        async fn record_once(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
            let waited = tokio::time::timeout(WAIT, async {
                loop {
                    let record = self.record();
                    if done(&record) {
                        return record;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            waited.unwrap_or_else(|_| panic!("{what}: {:#}", self.record()))
        }
    }

    /// The control requests and user messages the stand-in heard, by their kind.
    fn heard<'a>(record: &'a Value, kind: &str) -> Vec<&'a Value> {
        record["received"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter(|l| l["type"] == kind || l["request"]["subtype"] == kind)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn turn_ended(n: usize, state: TurnState) -> impl Fn(&ThreadState) -> bool {
        move |s: &ThreadState| {
            s.turns.len() == n && s.turns.last().is_some_and(|t| t.state == state)
        }
    }

    fn asking(s: &ThreadState) -> bool {
        s.open_requests().count() == 1
    }

    fn exited(s: &ThreadState) -> bool {
        matches!(s.status.liveness, Liveness::Exited { resumable: true })
    }

    /// The stand-in's process, by the pid it recorded, ended at once.
    fn kill(record: &Value) {
        signal(record, "-KILL");
    }

    /// The stand-in's process, by the pid it recorded, sent `signal` (`-TERM`).
    fn signal(record: &Value, signal: &str) {
        let pid = record["pid"].as_u64().expect("a pid").to_string();
        let sent = std::process::Command::new("kill").args([signal, &pid]).status().unwrap();
        assert!(sent.success(), "kill {signal} {pid}");
    }

    /// A thread's Claude Code runs as the desktop app runs it, named by the start's id; the
    /// first message goes as a user message under the start's intent and is the person's once
    /// Claude Code echoes it; a tool call is asked about by its request's id and the answer
    /// goes back under that id, the call then run.
    #[tokio::test]
    async fn a_message_is_delivered_and_a_permission_answered_by_its_id() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let id = IntentId::new();
        let outcome = claude.start(id, rig.start("bash touch made")).await;
        let Outcome::Started { thread } = outcome else { panic!("{outcome:?}") };
        assert_eq!(claude.start(id, rig.start("bash touch made")).await, outcome, "once");

        let state = rig.until(thread, "the ask", asking).await;
        assert_eq!(state.meta.native, id.to_string(), "named by the start");
        let why = state.open_requests().next().unwrap().reason.clone();
        assert_eq!(
            why.as_deref(),
            Some("Ask rule Bash overrides auto mode for this command."),
            "why it asks, in the TUI's words, in the mode its terminal starts in"
        );
        let record = rig.record();
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for flag in [
            "-p",
            "--verbose",
            "--replay-user-messages",
            "--permission-prompt-tool=stdio",
            "--session-id",
            "--permission-mode=auto",
        ] {
            assert!(argv.contains(&flag), "{flag} in {argv:?}");
        }
        assert_eq!(record["cwd"], rig.work.to_string_lossy().as_ref());
        for (name, value) in slopty_agent::claude::ENV {
            assert!(record["inherited"].get(name).is_some(), "{name}, as the desktop app sets it");
            if name.starts_with("CLAUDE_") {
                assert_eq!(record["env"][name], value, "{name}");
            }
        }
        let sent = heard(&record, "user");
        assert_eq!(sent.len(), 1, "sent once: {sent:#?}");
        assert_eq!(sent[0]["uuid"], id.to_string(), "under the start's intent");
        assert!(state.pending.is_empty(), "echoed, so delivered: {:?}", state.pending);

        assert_eq!(rig.answer(&claude, thread, "allow"), Outcome::Done);
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        assert!(state.open_requests().next().is_none());
        let edits = state.items.iter().find_map(|i| match &i.body {
            slopty_proto::thread::ItemBody::Tool(call) => match &call.detail {
                Some(slopty_proto::thread::detail::ToolDetail::Exec(e)) => e.edits.clone(),
                _ => None,
            },
            _ => None,
        });
        let edits = edits.expect("what the command changed");
        let made = &edits.files[0];
        assert!(made.path.ends_with("/made") && made.created, "{made:?}");
        assert_eq!((made.patch.added, made.patch.removed, edits.moves_tree), (1, 0, false));
        let record = rig.record();
        let answers = heard(&record, "control_response");
        let asked = record["received"].as_array().unwrap().len();
        assert_eq!(answers.len(), 1, "answered once, of {asked} lines");
        assert_eq!(answers[0]["response"]["response"]["behavior"], "allow");
    }

    /// A stop Claude Code does not acknowledge within its time is escalated: the worker ends
    /// the process, the turn is cut as interrupted, and the thread goes on by its next message.
    #[tokio::test]
    async fn a_stop_not_acknowledged_in_time_ends_the_process() {
        let rig = Rig::new(&[("STUB_SLOW_INTERRUPT_MS", "20000")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "slow 100").await;
        rig.until(thread, "the turn under way", |s| {
            s.turns.last().is_some_and(|t| t.state == TurnState::Active)
        })
        .await;
        let asked = Instant::now();
        assert_eq!(rig.intent(&claude, thread, &Intent::Interrupt), Outcome::Done);
        let state = rig.until(thread, "the process ended", exited).await;
        let took = asked.elapsed();
        println!("a stop not acknowledged ended Claude Code after {took:?}");
        assert!(
            took >= slopty_agent::claude::driven::INTERRUPT_ACK,
            "given its time first: {took:?}"
        );
        assert!(took < Duration::from_secs(10), "not left to finish: {took:?}");
        assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
    }

    /// Stop `thread` once its turn is under way, and the thread once its process ended, with
    /// how long that took from the stop.
    async fn stopped(rig: &Rig, claude: &Claude, thread: ThreadId) -> (ThreadState, Duration) {
        rig.until(thread, "the turn under way", |s| {
            s.turns.last().is_some_and(|t| t.state == TurnState::Active)
        })
        .await;
        let asked = Instant::now();
        assert_eq!(rig.intent(claude, thread, &Intent::Interrupt), Outcome::Done);
        let state = rig.until(thread, "the process ended", exited).await;
        (state, asked.elapsed())
    }

    /// Orca's fault "an ack without ending": Claude Code acknowledges the stop and goes on with
    /// the turn. The stop still lands: once its turn has had [`END_WAIT`] to end, the process is
    /// ended and the turn reads stopped.
    ///
    /// [`END_WAIT`]: slopty_agent::claude::driven::END_WAIT
    #[tokio::test]
    async fn a_stop_acknowledged_whose_turn_goes_on_still_ends_it() {
        let rig = Rig::new(&[("STUB_ACK_WITHOUT_ENDING", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "slow 100").await;
        let (state, took) = stopped(&rig, &claude, thread).await;
        println!("an acknowledged stop whose turn went on ended Claude Code after {took:?}");
        assert!(took >= slopty_agent::claude::driven::END_WAIT, "given its time first: {took:?}");
        assert!(took < Duration::from_secs(12), "not left to finish: {took:?}");
        assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
    }

    /// A Claude Code that stops reading what it is sent does not hold its thread: a message it
    /// never takes waits with the writer, and a stop still lands, by ending the process.
    #[tokio::test]
    async fn a_claude_code_that_stops_reading_still_stops() {
        let rig = Rig::new(&[("STUB_DEAF", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "slow 300").await;
        rig.until(thread, "the turn under way", |s| {
            s.turns.last().is_some_and(|t| t.state == TurnState::Active)
        })
        .await;
        rig.send(&claude, thread, &"x".repeat(4 << 20));
        let (state, took) = stopped(&rig, &claude, thread).await;
        println!("a Claude Code that stopped reading was ended {took:?} after its stop");
        assert!(took < Duration::from_secs(10), "not held by its input: {took:?}");
        assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
    }

    /// Orca's fault "ignores SIGTERM": a Claude Code that neither acknowledges the stop nor
    /// ends on `SIGTERM` is still ended, by ptyd's `SIGKILL` after its grace, and the turn reads
    /// stopped.
    #[tokio::test]
    async fn a_claude_code_that_ignores_sigterm_is_still_ended() {
        let rig =
            Rig::new(&[("STUB_IGNORE_SIGTERM", "1"), ("STUB_SLOW_INTERRUPT_MS", "60000")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "slow 300").await;
        let (state, took) = stopped(&rig, &claude, thread).await;
        println!("a Claude Code ignoring SIGTERM was ended {took:?} after its stop");
        assert!(took < Duration::from_secs(20), "ended, not left running: {took:?}");
        assert_eq!(state.turns.last().map(|t| t.state.clone()), Some(TurnState::Interrupted));
    }

    /// Orca's fault "a grandchild holding stdout": Claude Code dies mid-turn while a process it
    /// left behind keeps its output open. The thread does not wait on that output: it reads
    /// exited with why within moments of the death.
    #[tokio::test]
    async fn a_grandchild_holding_its_output_does_not_hold_the_thread() {
        let rig = Rig::new(&[("STUB_GRANDCHILD", "40"), ("STUB_CRASH_MID_TURN", "1")]).await;
        let (claude, _served) = rig.serve();
        let began = Instant::now();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the crash", exited).await;
        let took = began.elapsed();
        println!("a crash with its output held read exited after {took:?}");
        assert!(took < Duration::from_secs(15), "not held by the grandchild: {took:?}");
        assert!(
            matches!(state.turns.last().map(|t| &t.state), Some(TurnState::Failed { .. })),
            "{:?}",
            state.turns
        );
    }

    /// A stop that finds no turn running is done, not refused: it crossed the turn's end, or
    /// came twice.
    #[tokio::test]
    async fn a_stop_with_no_turn_running_is_done() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(rig.intent(&claude, thread, &Intent::Interrupt), Outcome::Done);
        let state = rig.host.state(thread).unwrap().0;
        assert_eq!(state.turns.len(), 1, "nothing was stopped: {:?}", state.turns);
    }

    /// A Claude Code that never answers `initialize` is said not to have started once
    /// [`INIT_WAIT`] is up, and ended: the thread reads exited, not working for good.
    ///
    /// [`INIT_WAIT`]: slopty_agent::claude::driven::INIT_WAIT
    #[tokio::test]
    async fn a_claude_code_that_never_answers_initialize_is_said_not_to_start() {
        let rig = Rig::new(&[("STUB_NEVER_ANSWER_INIT", "1")]).await;
        let (claude, _served) = rig.serve();
        let began = Instant::now();
        let thread = rig.started(&claude, "Say hello.").await;
        let bound = slopty_agent::claude::driven::INIT_WAIT.saturating_add(WAIT);
        let state = tokio::time::timeout(bound, async {
            loop {
                let (state, _) = rig.host.state(thread).unwrap();
                if exited(&state) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .expect("ended");
        println!("a Claude Code that never started read exited after {:?}", began.elapsed());
        let said = state.items.iter().any(|i| {
            matches!(&i.body, slopty_proto::thread::ItemBody::Notice(n)
                if n.text.text.starts_with(slopty_agent::claude::driven::DID_NOT_START))
        });
        assert!(said, "{:#?}", state.items);
    }

    /// Claude Code gone mid-turn fails the turn with why, from its standard error, and leaves
    /// the thread exited and resumable.
    #[tokio::test]
    async fn a_crash_mid_turn_fails_the_turn_with_why() {
        let rig = Rig::new(&[("STUB_CRASH_MID_TURN", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the crash", exited).await;
        let Some(TurnState::Failed { error, .. }) = state.turns.last().map(|t| t.state.clone())
        else {
            panic!("{:?}", state.turns)
        };
        assert!(error.contains("crashed mid-turn"), "{error}");
    }

    /// Claude Code ended mid-turn by a signal from outside says what kind of end it was, in
    /// plain words with the one thing to do, as a failure of its own kind
    /// ([`ENDED`](slopty_agent::claude::driven::ENDED)) and as the turn's error, not a bare
    /// "Claude Code ended". (A crash's signal is told the same way, by
    /// `ending::tests::each_kind_of_end_is_told_apart`; none is raised here, so the machine
    /// files no crash report.)
    #[tokio::test]
    async fn an_end_by_a_signal_says_what_kind_it_was() {
        for (sent, said) in [("-KILL", "killed (SIGKILL)"), ("-TERM", "outside Slopty (SIGTERM)")] {
            let rig = Rig::new(&[]).await;
            let (claude, _served) = rig.serve();
            let thread = rig.started(&claude, "slow 300").await;
            let active =
                |s: &ThreadState| s.turns.last().is_some_and(|t| t.state == TurnState::Active);
            rig.until(thread, "the turn", active).await;
            signal(&rig.record(), sent);
            let state = rig.until(thread, "the end", exited).await;
            let Some(TurnState::Failed { error, .. }) = state.turns.last().map(|t| t.state.clone())
            else {
                panic!("{sent}: {:?}", state.turns)
            };
            assert!(error.contains(said), "{sent}: {error}");
            assert!(error.ends_with("Send a message to start it again."), "{sent}: {error}");
            let notice = state.items.iter().find_map(|i| match &i.body {
                slopty_proto::thread::ItemBody::Notice(n)
                    if n.kind == slopty_agent::claude::driven::ENDED =>
                {
                    Some(n.text.text.clone())
                }
                _ => None,
            });
            assert_eq!(notice.as_deref(), Some(error.as_str()), "{sent}: said once, as the error");
        }
    }

    /// A message Claude Code never acknowledges is on its way until Claude Code ends; then it
    /// is held as one that may not have reached it, never shown as delivered.
    #[tokio::test]
    async fn a_message_never_acknowledged_is_held_as_unknown() {
        let rig = Rig::new(&[("STUB_NO_ECHO", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "slow 100").await;
        let state = rig.until(thread, "on its way", |s| !s.pending.is_empty()).await;
        assert!(state.pending.iter().all(|p| !p.is_unknown()), "{:?}", state.pending);
        rig.record_once("heard", |r| !heard(r, "user").is_empty()).await;
        assert_eq!(rig.intent(&claude, thread, &Intent::End), Outcome::Done);
        let state = rig.until(thread, "ended", exited).await;
        let [held] = state.pending.as_slice() else { panic!("{:?}", state.pending) };
        assert!(held.is_unknown(), "{held:?}");
    }

    /// A line that is no JSON, and one cut short, are passed over: the thread goes on.
    #[tokio::test]
    async fn a_torn_line_is_passed_over() {
        let rig = Rig::new(&[("STUB_MALFORMED", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        assert_eq!(state.status.liveness, Liveness::Live);
    }

    /// A Claude Code that cannot start says why on the thread, which is left exited.
    #[tokio::test]
    async fn a_claude_code_that_cannot_start_says_why() {
        let rig = Rig::new(&[("STUB_EXIT_ON_START", "2")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "ended", exited).await;
        let said = state.items.iter().any(|i| {
            matches!(&i.body, slopty_proto::thread::ItemBody::Notice(n)
                if n.text.text.contains("this install is broken"))
        });
        assert!(said, "{:#?}", state.items);
    }

    /// The adapter goes while Claude Code asks a permission (the worker restarted): ptyd keeps
    /// Claude Code running, the next adapter takes it up, asks it again what it waits on, and
    /// the person's answer reaches the same process, which finishes the turn.
    #[tokio::test]
    async fn a_permission_outstanding_survives_the_adapter_going() {
        let rig = Rig::new(&[]).await;
        let (claude, served) = rig.serve();
        let thread = rig.started(&claude, "bash touch made").await;
        rig.until(thread, "the ask", asking).await;
        let pid = rig.record()["pid"].clone();

        served.abort();
        drop(served.await);
        drop(claude);
        // Let go of by the adapter that went: ptyd holds it, attached to no one.
        let custody = Custody::new(rig.socket.clone());
        let free = tokio::time::timeout(WAIT, async {
            loop {
                let held = custody.sessions().await.unwrap();
                if held.len() == 1 && !held[0].attached {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        });
        free.await.expect("ptyd keeps Claude Code, let go");
        drop(custody);

        let (claude, _served) = rig.serve();
        let record = rig.record_once("asked again", |r| heard(r, "initialize").len() == 2).await;
        assert_eq!(record["pid"], pid, "the same Claude Code");
        let state = rig.until(thread, "the ask again", asking).await;
        assert_eq!(state.status.liveness, Liveness::Live);
        assert_eq!(rig.answer(&claude, thread, "allow"), Outcome::Done);
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        let record = rig.record();
        assert_eq!(record["pid"], pid, "finished by the same Claude Code");
        assert_eq!(heard(&record, "control_response").len(), 1, "answered once");
        assert_eq!(state.turns.len(), 1, "one turn, read again whole");
    }

    /// The plan's usage limit holds what is sent until it resets, then it goes.
    #[tokio::test]
    async fn the_usage_limit_holds_messages_until_it_resets() {
        let rig = Rig::new(&[("STUB_LIMIT_RESET_S", "2")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "limit").await;
        let limited =
            |s: &ThreadState| s.status.wait.as_ref().is_some_and(|w| w.kind == Wait::LIMIT);
        // The limit is said before the turn's result ends it.
        let ended = |s: &ThreadState| {
            limited(s) && s.turns.last().is_some_and(|t| t.state != TurnState::Active)
        };
        let state = rig.until(thread, "limited", ended).await;
        let Some(TurnState::Failed { until_ms: Some(resets), .. }) =
            state.turns.last().map(|t| t.state.clone())
        else {
            panic!("{:?}", state.turns)
        };
        rig.send(&claude, thread, "Say hi.");
        let state = rig.until(thread, "held", |s| !s.pending.is_empty()).await;
        assert!(limited(&state));
        assert_eq!(heard(&rig.record(), "user").len(), 1, "held on the worker");
        let state = rig.until(thread, "let go", turn_ended(2, TurnState::Complete)).await;
        let now = WallMs::now();
        assert!(now >= resets, "not before the reset: {now:?} < {resets:?}");
        assert!(state.status.wait.is_none(), "{:?}", state.status.wait);
        assert_eq!(heard(&rig.record(), "user").len(), 2);
    }

    /// A thread whose Claude Code ended goes on by its next message: Claude Code is started
    /// again on the session (`--resume`), and the turn runs there.
    #[tokio::test]
    async fn an_ended_thread_goes_on_by_its_next_message() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let native = state.meta.native.clone();
        let first = rig.record()["pid"].clone();
        assert_eq!(rig.intent(&claude, thread, &Intent::End), Outcome::Done);
        rig.until(thread, "ended", exited).await;

        rig.send(&claude, thread, "Say more.");
        let state = rig.until(thread, "the second turn", turn_ended(2, TurnState::Complete)).await;
        assert_eq!(state.status.liveness, Liveness::Live);
        let record = rig.record();
        assert_ne!(record["pid"], first, "started again");
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        let at = argv.iter().position(|a| *a == "--resume").expect("resumed");
        assert_eq!(argv[at + 1], native);
        assert_eq!(state.turns.len(), 2, "the first turn read again, the second after it");
    }

    /// Claude Code's own record of session `id` run in the rig's folder, as its TUI writes it:
    /// one prompt and its answer.
    fn seed(rig: &Rig, id: &str, prompt: &str, answer: &str) {
        let path = slopty_agent::claude::sessions::transcript(&rig.home, &rig.work, id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let cwd = rig.work.to_string_lossy();
        let common = |uuid: &str, parent: Value| {
            serde_json::json!({
                "uuid": uuid, "parentUuid": parent, "sessionId": id, "cwd": cwd,
                "timestamp": "2026-10-15T10:00:00.000Z", "isSidechain": false,
                "userType": "external", "entrypoint": "cli", "version": "2.1.301",
                "gitBranch": "main",
            })
        };
        let mut user = common("u0", Value::Null);
        user["type"] = "user".into();
        user["message"] = serde_json::json!({ "role": "user", "content": prompt });
        let mut said = common("a0", "u0".into());
        said["type"] = "assistant".into();
        said["requestId"] = "req_0".into();
        said["message"] = serde_json::json!({
            "id": "msg_0", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
            "content": [{ "type": "text", "text": answer }], "stop_reason": "end_turn",
            "stop_sequence": null, "usage": { "input_tokens": 12, "output_tokens": 9 },
        });
        std::fs::write(&path, format!("{user}\n{said}\n")).unwrap();
    }

    /// A start that takes session `id` up again (`--resume <id>`), with `prompt` first.
    fn resume(rig: &Rig, id: &str, prompt: Option<&str>) -> Box<Start> {
        let mut start = rig.start("");
        start.prompt = prompt.map(str::to_owned);
        start.args = vec!["--resume".to_owned(), id.to_owned()];
        start
    }

    /// Whether the stand-in was started with `--resume id`.
    fn resumed_on(record: &Value, id: &str) -> bool {
        let argv: Vec<&str> =
            record["argv"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        argv.windows(2).any(|w| w == ["--resume", id])
    }

    /// A session the TUI left, taken up with no first message, is read from its transcript and
    /// rests with nothing started: Claude Code starts on the session with the first message.
    #[tokio::test]
    async fn a_resumed_session_starts_claude_code_with_its_first_message() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let id = "5a1c9a52-3b7e-4c1d-9a0e-2f5b8c7d1e40";
        seed(&rig, id, "Explain the parser", "The parser reads tokens one by one.");
        let outcome = claude.start(IntentId::new(), resume(&rig, id, None)).await;
        let thread = slopty_agent::claude::thread_of(id);
        assert_eq!(outcome, Outcome::Started { thread });
        let state = rig
            .until(thread, "read from its transcript", |s| s.turns.len() == 1 && exited(s))
            .await;
        assert!(notices(&state).is_empty(), "nothing to say: {:?}", notices(&state));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(rig.record(), Value::Null, "nothing started before the first message");

        rig.send(&claude, thread, "Keep going.");
        let state = rig.until(thread, "the turn", turn_ended(2, TurnState::Complete)).await;
        assert_eq!(state.status.liveness, Liveness::Live);
        let record = rig.record();
        assert!(resumed_on(&record, id), "on the same session: {:#}", record["argv"]);
        assert_eq!(heard(&record, "user").len(), 1, "the first message, once");
    }

    /// A session a live Claude Code in a terminal holds (its `~/.claude/sessions/<pid>.json`) is
    /// not taken: a start with a first message is refused in plain words and makes nothing; one
    /// without opens it read from its transcript, saying where it runs, starts nothing and
    /// refuses a message with the same words. Once that Claude Code ends the thread says so,
    /// and the next message takes the session up.
    #[tokio::test]
    async fn a_session_held_elsewhere_is_shown_and_not_taken() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let id = "9d2e7a10-5c4b-4e8f-a1b2-c3d4e5f60718";
        seed(&rig, id, "Profile the startup path", "Startup loads fonts.");
        let mut terminal = std::process::Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let live = slopty_agent::claude::sessions::config_dir(&rig.home)
            .join(slopty_agent::claude::sessions::LIVE_DIR);
        std::fs::create_dir_all(&live).unwrap();
        let record = serde_json::json!({
            "pid": terminal.id(), "sessionId": id, "cwd": rig.work, "kind": "interactive",
        });
        std::fs::write(live.join(format!("{}.json", terminal.id())), record.to_string()).unwrap();
        let words =
            "This session is running in another terminal on rig. Close it there to continue here.";
        let thread = slopty_agent::claude::thread_of(id);

        let outcome = claude.start(IntentId::new(), resume(&rig, id, Some("Carry on."))).await;
        assert_eq!(outcome, Outcome::Refused { reason: words.to_owned() });
        assert!(rig.host.state(thread).is_none(), "nothing made");

        let outcome = claude.start(IntentId::new(), resume(&rig, id, None)).await;
        assert_eq!(outcome, Outcome::Started { thread });
        let state = rig
            .until(thread, "why it is not taken", |s| {
                s.turns.len() == 1 && exited(s) && notices(s).iter().any(|n| n == words)
            })
            .await;
        assert_eq!(state.turns.len(), 1, "its conversation, read");
        let send = Intent::Send {
            text: "Carry on.".to_owned(),
            delivery: Delivery::Steer,
            attachments: vec![],
        };
        assert_eq!(rig.intent(&claude, thread, &send), Outcome::Refused { reason: words.into() });
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(rig.record(), Value::Null, "no second Claude Code on the session");

        terminal.kill().unwrap();
        terminal.wait().unwrap();
        rig.until(thread, "let go", |s| {
            notices(s).iter().any(|n| n.starts_with("The other Claude Code on this session ended"))
        })
        .await;
        rig.send(&claude, thread, "Carry on.");
        rig.until(thread, "the turn", turn_ended(2, TurnState::Complete)).await;
        assert!(resumed_on(&rig.record(), id), "taken up: {:#}", rig.record()["argv"]);
    }

    /// A thread whose transcript cannot be read is kept as it was, not read again from nothing:
    /// its turns stay, the message that would take it up waits in its queue, and it says why.
    #[tokio::test]
    async fn a_transcript_that_cannot_be_read_keeps_the_thread() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let transcript =
            slopty_agent::claude::sessions::transcript(&rig.home, &rig.work, &state.meta.native);
        assert_eq!(rig.intent(&claude, thread, &Intent::End), Outcome::Done);
        rig.until(thread, "ended", exited).await;
        let first = rig.record()["pid"].clone();
        // A folder where the transcript was: there, and no file to read.
        std::fs::remove_file(&transcript).unwrap();
        std::fs::create_dir_all(&transcript).unwrap();

        rig.send(&claude, thread, "Say more.");
        let state = rig
            .until(thread, "why it was kept", |s| {
                notices(s).iter().any(|n| n.starts_with("Its transcript could not be read"))
            })
            .await;
        assert_eq!(state.turns.len(), 1, "its turn kept: {:?}", state.turns);
        assert!(
            state.pending.iter().any(|p| p.text == "Say more."),
            "the message waits: {:?}",
            state.pending
        );
        assert_eq!(rig.record()["pid"], first, "nothing started");
    }

    /// A live thread is stamped alive within 15 s while its Claude Code runs, idle or not,
    /// and never once the process is gone.
    #[tokio::test]
    async fn a_live_thread_is_leased_and_not_after_its_process_ends() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let first = state.lease_ms.expect("stamped as it started");
        let state = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let (state, _) = rig.host.state(thread).unwrap();
                if state.lease_ms.is_some_and(|at| at > first) {
                    return state;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("stamped again within 15 s while idle");
        let again = state.lease_ms.unwrap();
        println!("stamped {} ms apart", again.as_millis() - first.as_millis());

        kill(&rig.record());
        let state = rig.until(thread, "gone", exited).await;
        let last = state.lease_ms;
        tokio::time::sleep(slopty_worker::thread::host::LEASE_EVERY + Duration::from_secs(2)).await;
        let (state, _) = rig.host.state(thread).unwrap();
        assert_eq!(state.lease_ms, last, "no stamp after the process ended");
        assert_ne!(state.status.liveness, Liveness::Live);
    }

    /// A managed launcher in `claude`'s place takes the drive's arguments and hands them on to
    /// the client it starts, and the thread runs on that client.
    #[tokio::test]
    async fn a_managed_launcher_hands_the_arguments_on() {
        let rig = Rig::with(&[], |programs, home| {
            std::os::unix::fs::symlink(bin("slopty-stub-managed-claude"), programs.join("claude"))
                .unwrap();
            let client = home.join(".local/share/claude-managed/artifacts/2.1.301/stand-in");
            std::fs::create_dir_all(&client).unwrap();
            std::os::unix::fs::symlink(bin("slopty-stub-claude"), client.join("claude")).unwrap();
        })
        .await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        // The client's own record: started by the launcher, with what the drive gave it.
        let record = rig.record();
        assert!(
            record["inherited"].get("SCC_MANAGED_ARTIFACT").is_some(),
            "started by the launcher: {:#}",
            record["inherited"]
        );
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for flag in ["-p", "--input-format", "stream-json", "--permission-prompt-tool=stdio"] {
            assert!(argv.contains(&flag), "{flag} handed on: {argv:?}");
        }
        let at = argv.iter().position(|a| *a == "--session-id").expect("a session id");
        assert_eq!(argv[at + 1], state.meta.native);
    }

    /// A Claude Code older than the floor is not started: the start is refused with the words
    /// that ask the person to update it, and nothing runs.
    #[tokio::test]
    async fn a_claude_code_older_than_the_floor_asks_to_be_updated() {
        let rig = Rig::with(&[], |programs, _home| {
            let script = programs.join("claude");
            std::fs::write(&script, "#!/bin/sh\necho '2.0.14 (Claude Code)'\n").unwrap();
            std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        })
        .await;
        let (claude, _served) = rig.serve();
        let outcome = claude.start(IntentId::new(), rig.start("Say hello.")).await;
        let Outcome::Refused { reason } = outcome else { panic!("{outcome:?}") };
        assert!(reason.starts_with("Update Claude Code"), "{reason}");
        assert!(reason.contains(slopty_agent::claude::MIN_VERSION), "{reason}");
        assert!(reason.ends_with("it is 2.0.14"), "{reason}");
        assert_eq!(rig.record(), Value::Null, "nothing started");
    }

    /// The tool calls a recorded transcript's main conversation makes.
    fn recorded_calls(fixture: &str) -> usize {
        let dir = slopty_testkit::replay::fixture(fixture);
        let text = std::fs::read_to_string(dir.join("transcript.jsonl")).unwrap();
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|record| record["type"] == "assistant")
            .filter_map(|record| record["message"]["content"].as_array().cloned())
            .flatten()
            .filter(|part| part["type"] == "tool_use")
            .count()
    }

    /// A turn Claude Code really took, played over the stream by the stand-in, comes through
    /// the drive whole: every tool call of the recording is a call of the thread, its subagent a
    /// thread of its own, and the turn ends complete.
    #[tokio::test]
    async fn a_recorded_turn_plays_through_the_drive_with_its_subagent() {
        let rig = Rig::new(&[("STUB_REPLAY", "tools")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Survey the files").await;
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        let calls = state
            .items
            .iter()
            .filter(|item| matches!(item.body, slopty_proto::thread::ItemBody::Tool(_)))
            .count();
        assert_eq!(calls, recorded_calls("tools"), "{:#?}", state.items);
        assert_eq!(rig.host.threads().len(), 2, "the thread and its subagent's");
    }

    /// Each permission a recorded turn asked for holds the played turn until it is answered,
    /// one at a time, and the turn ends once the last is.
    #[tokio::test]
    async fn a_recorded_turn_waits_on_each_permission_it_asked() {
        let rig = Rig::new(&[("STUB_REPLAY", "permission")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Touch three files").await;
        let mut answered = Vec::new();
        for n in 1..=3 {
            let new = |s: &ThreadState| s.open_requests().any(|r| !answered.contains(&r.id));
            rig.until(thread, &format!("ask {n}"), new).await;
            let (state, _) = rig.host.state(thread).unwrap();
            answered.extend(state.open_requests().map(|r| r.id.clone()));
            assert_eq!(rig.answer(&claude, thread, "allow"), Outcome::Done, "ask {n}");
        }
        rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        let answers = heard(&rig.record(), "control_response").len();
        assert_eq!(answers, 3, "each answered once");
    }

    /// A request Claude Code tries again three times and then gets through is one row of the
    /// thread: made at the first attempt, updated in place by the two after it, and gone once
    /// the turn streams again.
    #[tokio::test]
    async fn a_request_tried_again_is_one_row_updated_then_gone() {
        use slopty_proto::thread::{Action, ItemBody, Notice};
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "retry 3").await;
        let mut feed = rig.host.watch(thread).unwrap();
        let state = rig.until(thread, "the turn", turn_ended(1, TurnState::Complete)).await;
        let retrying = |body: &ItemBody| matches!(body, ItemBody::Notice(n) if n.kind == Notice::API_ERROR && n.retry.is_some());
        let (mut made, mut updated, mut gone) = (Vec::new(), Vec::new(), Vec::new());
        while let Ok(batch) = feed.try_recv() {
            for action in &batch.actions {
                match action {
                    Action::ItemStarted(item) if retrying(&item.body) => made.push(item.id.clone()),
                    Action::ItemUpdated(item) if retrying(&item.body) => {
                        updated.push(item.id.clone());
                    }
                    Action::ItemRemoved { item } => gone.push(item.clone()),
                    _ => {}
                }
            }
        }
        assert_eq!(made.len(), 1, "one row: {made:?}");
        assert_eq!(updated, vec![made[0].clone(); 2], "updated in place");
        assert_eq!(gone, made, "gone once it went through");
        let left = state
            .items
            .iter()
            .filter(|i| matches!(&i.body, ItemBody::Notice(n) if n.kind == Notice::API_ERROR))
            .count();
        assert_eq!(left, 0, "{:#?}", state.items);
    }

    /// The words of every notice `state` holds.
    fn notices(state: &ThreadState) -> Vec<String> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                slopty_proto::thread::ItemBody::Notice(n) => Some(n.text.text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Whether `intent` was refused, before Claude Code heard of it.
    fn refused(outcome: &Outcome) -> bool {
        matches!(outcome, Outcome::Refused { reason } if !reason.is_empty())
    }

    /// A background command is the thread's task under Claude Code's task id, and stopping it
    /// names that id to Claude Code (`stop_task`), which ends it killed; a task that is not
    /// running is refused before Claude Code hears of it.
    #[tokio::test]
    async fn a_background_command_is_stopped_by_its_task_id() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "background sleep 30").await;
        let task = "b00000001";
        let running = move |s: &ThreadState| s.tasks.iter().any(|t| t.id == task && t.is_live());
        let state = rig
            .until(thread, "the command running in the background", |s| {
                turn_ended(1, TurnState::Complete)(s) && running(s)
            })
            .await;
        let found = state.tasks.iter().find(|t| t.id == task).unwrap();
        assert_eq!((found.kind.as_str(), found.backgrounded), ("shell", true), "{found:?}");
        assert_eq!(found.item.as_ref().map(|i| i.0.as_str()), Some("toolu_bg1"));

        let nothing = Intent::StopTask { task: "b404".to_owned() };
        assert!(refused(&rig.intent(&claude, thread, &nothing)));
        let stop = Intent::StopTask { task: task.to_owned() };
        assert_eq!(rig.intent(&claude, thread, &stop), Outcome::Done);
        let state = rig
            .until(thread, "the command stopped", |s| {
                s.tasks.iter().any(|t| t.id == task && t.state == "killed")
            })
            .await;
        let record = rig.record();
        let stops = heard(&record, "stop_task");
        assert_eq!(stops.len(), 1, "{stops:#?}");
        assert_eq!(stops[0]["request"]["task_id"], task);
        assert!(state.turns.iter().all(|t| t.state != TurnState::Active), "{:#?}", state.turns);
    }

    /// A call that blocks the turn says how long it has run as it runs, and moving it to the
    /// background (`background_tasks`) lets the turn end while it goes on as a task; a call the
    /// thread does not hold is refused.
    #[tokio::test]
    async fn a_blocking_call_goes_to_the_background_and_the_turn_ends() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "wait 20").await;
        let call = slopty_proto::thread::ItemId("toolu_w1".to_owned());
        let noted = call.clone();
        let state = rig
            .until(thread, "the call's progress", move |s| {
                s.calls.get(&noted).is_some_and(|n| n.elapsed_ms.is_some())
            })
            .await;
        assert_eq!(state.status.phase, slopty_proto::thread::Phase::Working, "{:?}", state.status);
        let elsewhere = Intent::Background { item: Some(slopty_proto::thread::ItemId("x".into())) };
        assert!(refused(&rig.intent(&claude, thread, &elsewhere)));
        let moved = Intent::Background { item: None };
        assert_eq!(rig.intent(&claude, thread, &moved), Outcome::Done);
        let state = rig
            .until(thread, "the turn past the call", |s| {
                turn_ended(1, TurnState::Complete)(s)
                    && s.tasks.iter().any(|t| t.item.as_ref() == Some(&call) && t.is_live())
            })
            .await;
        let said = state.items.iter().any(|i| {
            matches!(&i.body, slopty_proto::thread::ItemBody::Text(t)
                if t.text == "Done: it went on in the background.")
        });
        assert!(said, "{:#?}", state.items);
        assert_eq!(heard(&rig.record(), "background_tasks").len(), 1);
    }

    /// A side question (`/btw`) goes to Claude Code as its own `side_question` while the turn
    /// works on, carrying the exchanges answered before it; its answer is the thread's, the
    /// turn untouched. One Claude Code refuses says why, and an empty one is refused at once.
    #[tokio::test]
    async fn a_side_question_is_answered_beside_the_turn() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "wait 20").await;
        let call = slopty_proto::thread::ItemId("toolu_w1".to_owned());
        rig.until(thread, "the call's progress", move |s| {
            s.calls.get(&call).is_some_and(|n| n.elapsed_ms.is_some())
        })
        .await;
        let ask = |question: &str| Intent::Side { question: question.to_owned() };
        assert!(refused(&rig.intent(&claude, thread, &ask("  "))));
        let answered = |n: usize| {
            move |s: &ThreadState| s.side.len() == n && s.side.iter().all(|x| !x.pending())
        };
        assert_eq!(rig.intent(&claude, thread, &ask("Which file?")), Outcome::Done);
        rig.until(thread, "the first answer", answered(1)).await;
        assert_eq!(rig.intent(&claude, thread, &ask("Why that one?")), Outcome::Done);
        assert_eq!(rig.intent(&claude, thread, &ask("refuse")), Outcome::Done);
        let state = rig.until(thread, "the other two", answered(3)).await;
        let said: Vec<(Option<&str>, Option<&str>)> =
            state.side.iter().map(|x| (x.answer.as_deref(), x.failed.as_deref())).collect();
        assert_eq!(
            said,
            [
                (Some("Aside: Which file? (0 before)"), None),
                (Some("Aside: Why that one? (1 before)"), None),
                (None, Some("Side questions are not available")),
            ]
        );
        assert_eq!(state.status.phase, slopty_proto::thread::Phase::Working, "{:?}", state.status);
        let record = rig.record();
        let asked = heard(&record, "side_question");
        assert_eq!(asked[1]["request"]["history"][0]["response"], "Aside: Which file? (0 before)");
        assert!(heard(&record, "interrupt").is_empty(), "the turn is never stopped");
    }

    /// Work no call started is a task row of its own; what Claude Code says beside the
    /// conversation is the thread's: a notification, the goal, the rows it derives (memories,
    /// a permission retried, a fallback model, a hook that failed, long instructions) and the
    /// prompt it suggests next.
    #[tokio::test]
    async fn what_claude_code_says_beside_the_conversation_is_the_threads() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "monitor").await;
        let state =
            rig.until(thread, "the monitor's turn", turn_ended(1, TurnState::Complete)).await;
        let row = state
            .items
            .iter()
            .find_map(|i| match &i.body {
                slopty_proto::thread::ItemBody::Task(note) if note.task == "m00000001" => {
                    Some(note.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("the monitor's own row: {:#?}", state.items));
        assert_eq!((row.call.as_ref(), row.status.as_str()), (None, "completed"), "{row:?}");
        assert!(row.summary.contains("stream ended"), "{row:?}");

        rig.send(&claude, thread, "signals");
        let state = rig
            .until(thread, "the signals", |s| {
                turn_ended(2, TurnState::Complete)(s) && s.suggestion.is_some()
            })
            .await;
        assert_eq!(state.suggestion.as_deref(), Some("Run the tests"));
        assert_eq!(state.goal.as_ref().map(|g| g.condition.as_str()), Some("the tests pass"));
        let note = state.notes.iter().find(|n| n.key == "stub-heads-up").expect("the notification");
        assert_eq!(
            (note.text.as_str(), note.priority.as_str()),
            ("Heads up from the stand-in", "high")
        );
        assert!(note.until_ms.is_some(), "it passes");
        let said = notices(&state);
        for row in [
            "Saved 2 memories",
            "Allowed npm test",
            "Switched to claude-sonnet-5-5",
            "PreToolUse hook failed: lint failed",
            "The instruction files are long: 50000 characters, over 40000",
        ] {
            assert!(said.iter().any(|s| s == row), "{row} in {said:#?}");
        }
    }

    /// A dialog Claude Code raises is a request answered on its id with the option the person
    /// took, and an MCP server's form is answered with what they filled in; Claude Code takes
    /// each answer as given.
    #[tokio::test]
    async fn a_dialog_and_a_form_are_answered_on_their_ids() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "dialog goal_proposal").await;
        let state = rig.until(thread, "the dialog", asking).await;
        let request = state.open_requests().next().unwrap();
        assert_eq!(request.kind, "dialog:goal_proposal");
        let ids: Vec<&str> = request.options.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, ["yes", "no"], "the payload's own options");
        assert!(refused(&rig.answer(&claude, thread, "maybe")), "an answer it does not offer");
        assert_eq!(rig.answer(&claude, thread, "yes"), Outcome::Done);
        let state =
            rig.until(thread, "the dialog's turn", turn_ended(1, TurnState::Complete)).await;
        let texts = |s: &ThreadState| -> Vec<String> {
            s.items
                .iter()
                .filter_map(|i| match &i.body {
                    slopty_proto::thread::ItemBody::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(texts(&state).contains(&r#"Dialog goal_proposal: completed "yes""#.to_owned()));

        rig.send(&claude, thread, "elicit");
        let state = rig.until(thread, "the form", asking).await;
        let form = state.open_requests().next().unwrap();
        assert_eq!(form.kind, slopty_proto::thread::Request::ELICITATION);
        assert!(form.schema_json.as_deref().is_some_and(|s| s.contains("ticket")), "{form:?}");
        assert_eq!(rig.answer(&claude, thread, r#"{"ticket":"T-1"}"#), Outcome::Done);
        let state = rig.until(thread, "the form's turn", turn_ended(2, TurnState::Complete)).await;
        assert!(
            texts(&state).contains(&r#"Elicitation: accept {"ticket":"T-1"}"#.to_owned()),
            "{:?}",
            texts(&state)
        );
        let record = rig.record();
        let answers = heard(&record, "control_response");
        assert_eq!(answers.len(), 2, "{answers:#?}");
        assert_eq!(answers[0]["response"]["response"]["behavior"], "completed");
        assert_eq!(answers[1]["response"]["response"]["action"], "accept");
    }

    /// A `!` command runs in the thread's folder and its output reaches Claude Code as the
    /// person's message, answered; with `respondToBashCommands` off it is taken in without a
    /// turn (`shouldQuery: false`), the command and its output still the thread's.
    #[tokio::test]
    async fn a_bang_commands_output_reaches_claude_code() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "hello").await;
        rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        assert!(refused(&rig.intent(&claude, thread, &Intent::Shell { command: " ".to_owned() })));
        let pwd = Intent::Shell { command: "pwd".to_owned() };
        assert_eq!(rig.intent(&claude, thread, &pwd), Outcome::Done);
        rig.until(thread, "the answered command", turn_ended(2, TurnState::Complete)).await;
        let record = rig.record();
        let sent = heard(&record, "user");
        let last = sent.last().unwrap();
        let output = last["message"]["content"][1]["text"].as_str().unwrap();
        let work = rig.work.to_string_lossy();
        assert!(output.contains(&format!("<bash-stdout>{work}")), "{output}");
        assert!(last.get("shouldQuery").is_none(), "answered: {last}");

        let project = rig.work.join(".claude");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("settings.json"), r#"{"respondToBashCommands":false}"#)
            .unwrap();
        let echo = Intent::Shell { command: "echo taken in".to_owned() };
        assert_eq!(rig.intent(&claude, thread, &echo), Outcome::Done);
        let record = rig
            .record_once("the command taken in", |r| {
                r["received"]
                    .as_array()
                    .is_some_and(|l| l.iter().any(|l| l["shouldQuery"] == false))
            })
            .await;
        let quiet = heard(&record, "user").into_iter().find(|l| l["shouldQuery"] == false).unwrap();
        assert!(quiet.to_string().contains("taken in"), "{quiet}");
        let state = rig
            .until(thread, "the command, the person's", |s| {
                s.items.iter().any(|i| {
                    matches!(&i.body, slopty_proto::thread::ItemBody::User(u)
                        if u.command.as_deref() == Some("!") && u.text.text == "echo taken in")
                }) && s.status.phase != slopty_proto::thread::Phase::Working
            })
            .await;
        assert!(state.pending.is_empty(), "{:?}", state.pending);
        assert!(
            !state.items.iter().any(|i| i.id.0.starts_with("bang-")),
            "the running command's own row is gone once it ran"
        );
    }

    /// Putting the files back to before a turn goes through Claude Code's checkpoints
    /// (`rewind_files`) and says what it put back; a fork through the first turn shares that
    /// turn alone (`--resume-session-at`), and a turn that is not there is refused.
    #[tokio::test]
    async fn files_go_back_before_a_turn_and_a_fork_shares_the_turns_before() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "write a.txt one").await;
        let file = rig.work.join("a.txt");
        rig.until(thread, "the first write", turn_ended(1, TurnState::Complete)).await;
        rig.send(&claude, thread, "write a.txt two");
        let state = rig.until(thread, "the second write", turn_ended(2, TurnState::Complete)).await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two\n");
        let (first, second) = (state.turns[0].id, state.turns[1].id);

        let gone = Intent::Rewind { turn: slopty_proto::thread::TurnId(99) };
        assert!(refused(&rig.intent(&claude, thread, &gone)));
        assert_eq!(rig.intent(&claude, thread, &Intent::Rewind { turn: second }), Outcome::Done);
        rig.until(thread, "the second put back", |s| {
            notices(s).iter().any(|n| n == "Put back 1 file (+1 −1)")
        })
        .await;
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\n");
        let record = rig.record();
        let rewinds = heard(&record, "rewind_files");
        let prompt = state.turns[1].input.as_ref().unwrap().0.clone();
        assert_eq!(rewinds[0]["request"]["user_message_id"], prompt.as_str());

        let forked = claude.fork(thread, IntentId::new(), Some(first)).await;
        let Outcome::Started { thread: fork } = forked else { panic!("{forked:?}") };
        let fork_state = rig.host.state(fork).unwrap().0;
        assert_eq!(
            fork_state.meta.forked_from.as_ref().map(|f| (f.thread, f.turn)),
            Some((thread, Some(first)))
        );
        let record = rig
            .record_once("the fork's Claude Code", |r| {
                r["argv"].as_array().is_some_and(|a| a.iter().any(|w| w == "--resume-session-at"))
            })
            .await;
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(argv.contains(&"--fork-session"), "{argv:?}");
        let transcript = slopty_agent::claude::sessions::transcript(
            &rig.home,
            &rig.work,
            &fork_state.meta.native,
        );
        let kept = std::fs::read_to_string(&transcript).unwrap();
        assert!(kept.contains("write a.txt one") && !kept.contains("write a.txt two"), "{kept}");
    }

    /// A rewind of the whole conversation branches before the first turn: the client names it
    /// as the turn before the first message's, and the new thread shares nothing, a session of
    /// its own in the folder, not one resumed.
    #[tokio::test]
    async fn a_fork_before_the_first_turn_shares_nothing() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "Say hello.").await;
        let state = rig.until(thread, "the first turn", turn_ended(1, TurnState::Complete)).await;
        let before = slopty_proto::thread::TurnId(state.turns[0].id.0.saturating_sub(1));
        let forked = claude.fork(thread, IntentId::new(), Some(before)).await;
        let Outcome::Started { thread: fork } = forked else { panic!("{forked:?}") };
        let fork_state = rig.host.state(fork).unwrap().0;
        assert!(fork_state.turns.is_empty(), "{:?}", fork_state.turns);
        assert_eq!(
            fork_state.meta.forked_from.as_ref().map(|f| (f.thread, f.turn)),
            Some((thread, Some(slopty_proto::thread::TurnId::BEFORE)))
        );
        let native = fork_state.meta.native.clone();
        let record = rig
            .record_once("the fork's Claude Code", |r| {
                r["argv"].as_array().is_some_and(|a| a.iter().any(|w| w == native.as_str()))
            })
            .await;
        let argv: Vec<&str> =
            record["argv"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(!argv.contains(&"--resume") && !argv.contains(&"--fork-session"), "{argv:?}");
    }

    /// The person's status line command runs on the thread as Claude Code's TUI runs it, its
    /// words the thread's.
    #[tokio::test]
    async fn the_persons_status_line_is_the_threads() {
        let rig = Rig::new(&[]).await;
        let claude_dir = rig.home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let line = serde_json::json!({ "statusLine": {
            "type": "command",
            "command": "cat > /dev/null; printf 'in %s' \"$(basename \"$PWD\")\"",
        } });
        std::fs::write(claude_dir.join("settings.json"), line.to_string()).unwrap();
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "hello").await;
        let state = rig
            .until(thread, "the status line", |s| s.status_line.as_deref() == Some("in work"))
            .await;
        assert_eq!(state.status_line.as_deref(), Some("in work"));
    }

    /// A permission rule is written into its scope's file, every other setting kept, and the
    /// rules Claude Code then lists are the thread's; removed, it is gone from both. A scope
    /// Claude Code does not write is refused.
    #[tokio::test]
    async fn a_permission_rule_goes_into_its_file_and_back_as_listed() {
        use slopty_proto::thread::{Rule, RuleChange};
        let rig = Rig::new(&[]).await;
        let local = rig.work.join(".claude/settings.local.json");
        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, r#"{"env":{"A":"1"}}"#).unwrap();
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "hello").await;
        let state = rig.until(thread, "the rules listed", |s| s.rules.is_some()).await;
        assert_eq!(state.rules.as_ref().unwrap().rules, Vec::new());
        assert_eq!(state.surfaces, Vec::new(), "no mod draws here");
        let rule = "Bash(cargo test:*)";
        let change = |add: bool, scope: &str| {
            Intent::Rule(RuleChange {
                add,
                behavior: Rule::ALLOW.to_owned(),
                rule: rule.to_owned(),
                scope: scope.to_owned(),
            })
        };
        assert!(refused(&rig.intent(&claude, thread, &change(true, "policySettings"))));
        assert_eq!(rig.intent(&claude, thread, &change(true, Rule::LOCAL)), Outcome::Done);
        let state = rig
            .until(thread, "the rule listed", |s| {
                s.rules.as_ref().is_some_and(|r| r.rules.iter().any(|r| r.text == rule))
            })
            .await;
        let listed = state.rules.unwrap().rules.into_iter().find(|r| r.text == rule).unwrap();
        assert_eq!((listed.source.as_str(), listed.behavior.as_str()), (Rule::LOCAL, Rule::ALLOW));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&local).unwrap()).unwrap();
        assert_eq!(doc["env"]["A"], "1", "the rest kept");
        assert_eq!(rig.intent(&claude, thread, &change(false, Rule::LOCAL)), Outcome::Done);
        rig.until(thread, "the rule gone", |s| {
            s.rules.as_ref().is_some_and(|r| !r.rules.iter().any(|r| r.text == rule))
        })
        .await;
    }

    /// A mod's pane and band are the thread's surfaces, each element the person can act in
    /// named; pressing its button reaches the mod (`ui_press`), whose toast and status come
    /// and whose pane is drawn again; closing the pane takes it away. A surface that is not
    /// there is refused.
    #[tokio::test]
    async fn a_mods_surfaces_are_drawn_and_acted_in() {
        use slopty_proto::thread::{Surface, SurfaceEvent};
        let rig = Rig::new(&[("STUB_UI", "1")]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "hello").await;
        let has = |s: &ThreadState, id: &str| s.surfaces.iter().any(|x| x.id == id);
        let state = rig
            .until(thread, "the pane and the band", |s| has(s, "deploy") && has(s, "above-prompt"))
            .await;
        let pane = state.surfaces.iter().find(|s| s.id == "deploy").unwrap();
        assert_eq!((pane.kind.as_str(), pane.title.as_deref()), (Surface::PANE, Some("Deploy")));
        assert!(pane.tree.contains(r#""id":"ship:1""#), "{}", pane.tree);
        let act = |surface: &str, element: &str, event: SurfaceEvent| Intent::Surface {
            surface: surface.to_owned(),
            element: element.to_owned(),
            event,
        };
        assert!(refused(&rig.intent(&claude, thread, &act("nope", "", SurfaceEvent::Press))));
        assert_eq!(
            rig.intent(&claude, thread, &act("deploy", "ship:1", SurfaceEvent::Press)),
            Outcome::Done
        );
        let state = rig
            .until(thread, "the mod's answer", |s| {
                s.surfaces
                    .iter()
                    .any(|x| x.kind == Surface::TOAST && x.text.as_deref() == Some("Shipped"))
                    && s.surfaces.iter().any(|x| x.id == "status:ship")
                    && s.surfaces
                        .iter()
                        .any(|x| x.id == "deploy" && x.tree.contains("Deploy: shipped"))
            })
            .await;
        let status = state.surfaces.iter().find(|s| s.id == "status:ship").unwrap();
        assert_eq!(status.text.as_deref(), Some("shipped"));
        let record = rig.record();
        let presses = heard(&record, "ui_press");
        assert_eq!((presses.len(), &presses[0]["request"]["handle"]), (1, &serde_json::json!(1)));
        assert_eq!(
            rig.intent(&claude, thread, &act("deploy", "", SurfaceEvent::Close)),
            Outcome::Done
        );
        rig.until(thread, "the pane gone", |s| !has(s, "deploy")).await;
    }

    /// Approving a plan with the context cleared is one verb: the plan's ask is denied as
    /// Claude Code's own "clear context" does, the mode becomes accept-edits, the context is
    /// cleared (`/clear`) and the plan goes as the next message.
    #[tokio::test]
    async fn a_plan_approved_with_the_context_cleared_runs_from_a_clear_context() {
        let rig = Rig::new(&[]).await;
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "plan").await;
        let state = rig.until(thread, "the plan", asking).await;
        let plan = state.open_requests().next().unwrap();
        assert!(plan.options.iter().any(|o| o.id == "clear"), "{:?}", plan.options);
        assert_eq!(rig.answer(&claude, thread, "clear"), Outcome::Done);
        let record = rig
            .record_once("the plan sent after the clear", |r| {
                heard(r, "user")
                    .iter()
                    .any(|l| l.to_string().contains("Implement the following plan"))
            })
            .await;
        let words: Vec<String> = heard(&record, "user").iter().map(ToString::to_string).collect();
        let clear = words.iter().position(|w| w.contains("\"/clear\"")).expect("the clear");
        let carried =
            words.iter().position(|w| w.contains("Implement the following plan")).unwrap();
        assert!(clear < carried, "{words:#?}");
        assert!(words[carried].contains("1. Write the file"), "{}", words[carried]);
        let modes = heard(&record, "set_permission_mode");
        assert_eq!(modes.last().unwrap()["request"]["mode"], "acceptEdits");
        let answer = heard(&record, "control_response");
        assert_eq!(answer[0]["response"]["response"]["behavior"], "deny");
        assert_eq!(answer[0]["response"]["response"]["interrupt"], true);
        rig.until(thread, "the plan carried out", |s| {
            s.items.iter().any(|i| {
                matches!(&i.body, slopty_proto::thread::ItemBody::Text(t)
                if t.text.starts_with("Done: Implement the following plan"))
            })
        })
        .await;
    }

    /// A stand-in for `slopty mcp` answering one `show_visual` as its check does: a script that
    /// says `broken` is turned down with its line, any other is shown.
    const CHECKER: &str = r#"#!/bin/sh
read -r line
case "$line" in
*broken*) echo '{"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text","text":"line 1: broken is not defined"}]}}' ;;
*) echo '{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"{\"shown\":\"Hello\"}"}]}}' ;;
esac
"#;

    /// Slopty's own `show_visual` called by Claude Code is the thread's visual once the call
    /// came back checked; one the check turned down stays a call, with why. The stand-in calls
    /// it through the folder's `slopty` server (`STUB_TURNS`), here a script that checks a
    /// visual as `slopty mcp` does and turns down a script that says `broken`.
    #[tokio::test]
    async fn a_visual_claude_code_shows_is_the_threads_once_checked() {
        let script = r#"ui.text("hello")"#;
        let turns = serde_json::json!({
            "show it": [{ "name": "show_visual", "arguments": { "title": "Hello", "script": script } }],
            "show broken": [{ "name": "show_visual", "arguments": { "title": "Bad", "script": "broken" } }],
        });
        let turns = turns.to_string();
        let rig = Rig::new(&[("STUB_TURNS", turns.as_str())]).await;
        let server = rig.programs.join("slopty-mcp-check");
        std::fs::write(&server, CHECKER).unwrap();
        std::fs::set_permissions(&server, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let config =
            serde_json::json!({ "mcpServers": { "slopty": { "command": server, "args": [] } } });
        std::fs::write(rig.work.join(".mcp.json"), config.to_string()).unwrap();
        let (claude, _served) = rig.serve();
        let thread = rig.started(&claude, "show it").await;
        let state = rig.until(thread, "the visual", turn_ended(1, TurnState::Complete)).await;
        let visual = state.items.iter().find_map(|i| match &i.body {
            slopty_proto::thread::ItemBody::Tool(call) => match &call.detail {
                Some(slopty_proto::thread::detail::ToolDetail::Visual(v)) => Some(v.clone()),
                _ => None,
            },
            _ => None,
        });
        let visual = visual.unwrap_or_else(|| panic!("the visual: {:#?}", state.items));
        assert_eq!((visual.title.as_str(), visual.script.text.as_str()), ("Hello", script));

        rig.send(&claude, thread, "show broken");
        let state = rig.until(thread, "the turned down", turn_ended(2, TurnState::Complete)).await;
        let visuals = state
            .items
            .iter()
            .filter(|i| {
                matches!(&i.body, slopty_proto::thread::ItemBody::Tool(call)
                if matches!(call.detail, Some(slopty_proto::thread::detail::ToolDetail::Visual(_))))
            })
            .count();
        assert_eq!(visuals, 1, "the turned-down one is no visual: {:#?}", state.items);
    }
}
