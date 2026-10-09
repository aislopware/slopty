//! Claude Code observed into the thread host, end to end on the worker's side: the daemon's
//! broadcast, its held prompts and a session's sources are played by the test (a status, a held
//! prompt, hooks heard, the mod's board), the transcripts are a real Claude Code's, recorded
//! (`slopty-agent/tests/fixtures`), written into a session directory as the agent would write
//! them, and the threads are judged from the host.

#[cfg(test)]
mod claude_threads {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_agent::conversation::{Conversation, ThreadId as ConvThread};
    use slopty_agent::live::{Batch, Board, ModEvent};
    use slopty_agent::observed::{terminal_thread, thread_of};
    use slopty_agent::status::{AgentEvent, AgentSource, AgentStatus};
    use slopty_agent::transcript::Tail;
    use slopty_core::{SessionId, WallMs};
    use slopty_proto::WorkerMsg;
    use slopty_proto::conversation::{PermissionEvent, PermissionPrompt, ToolDetail};
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_proto::thread::{Cap, ItemBody, Phase, ThreadId, ThreadState};
    use slopty_worker::conversation::Seen;
    use slopty_worker::orchestrate;
    use slopty_worker::thread::Host;
    use slopty_worker::thread::claude::{self, Sources};
    use slopty_worker::thread::log::Limits;
    use tokio::sync::{broadcast, watch};

    /// The recorded session's own id.
    const NATIVE: &str = "00000000-0000-4000-8000-000000000001";

    /// Long enough for any read on a loaded machine; the tests judge the state, not the time.
    const BOUND: Duration = Duration::from_secs(30);

    fn fixture(kind: &str, scenario: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../slopty-agent/tests/fixtures")
            .join(kind)
            .join(scenario)
    }

    struct Fake {
        /// The terminal whose agent writes the transcript.
        terminal: SessionId,
        /// The main transcript, once the agent has written one.
        main: watch::Sender<Option<PathBuf>>,
        seen: watch::Sender<Seen>,
        /// What the daemon's table holds of each agent.
        standing: parking_lot::Mutex<Vec<AgentEvent>>,
        /// The terminals closed.
        closed: parking_lot::Mutex<Vec<SessionId>>,
    }

    impl Sources for Fake {
        fn sources(&self, session: SessionId) -> orchestrate::Sources {
            let main = self.main.borrow().clone().filter(|_| session == self.terminal);
            orchestrate::Sources { main, ..Default::default() }
        }

        fn seen(&self, _session: SessionId) -> watch::Receiver<Seen> {
            self.seen.subscribe()
        }

        fn standing(&self) -> Vec<AgentEvent> {
            self.standing.lock().clone()
        }

        fn open(&self, session: SessionId) -> bool {
            !self.closed.lock().contains(&session)
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        main: PathBuf,
        terminal: SessionId,
        events: broadcast::Sender<WorkerMsg>,
        heard: broadcast::Sender<AgentEvent>,
        seen: Arc<Fake>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("projects").join(format!("{NATIVE}.jsonl"));
            std::fs::create_dir_all(main.parent().unwrap()).unwrap();
            std::fs::write(&main, "").unwrap();
            let terminal = SessionId::new();
            let seen = Arc::new(Fake {
                terminal,
                main: watch::Sender::new(Some(main.clone())),
                seen: watch::Sender::new(Seen::default()),
                standing: parking_lot::Mutex::new(Vec::new()),
                closed: parking_lot::Mutex::new(Vec::new()),
            });
            let (events, heard) = (broadcast::Sender::new(64), broadcast::Sender::new(64));
            Self { dir, main, terminal, events, heard, seen }
        }

        fn host(&self) -> Host {
            Host::open(&self.dir.path().join("threads"), Limits::default()).unwrap()
        }

        fn observe(&self, host: &Host) -> (tokio::task::JoinHandle<()>, claude::Driver) {
            let sources: Arc<dyn Sources> = Arc::<Fake>::clone(&self.seen);
            let (driver, asks) = claude::Driver::channel();
            let (events, heard) = (self.events.subscribe(), self.heard.subscribe());
            (claude::spawn(host.clone(), events, heard, sources, asks), driver)
        }

        /// A hook says the status of session [`NATIVE`].
        fn status(&self, status: AgentStatus) {
            self.tracked(status, Some(NATIVE), AgentSource::Hook);
        }

        /// The tracker says `status`, heard from `source`, with the session id it knows.
        fn tracked(&self, status: AgentStatus, native: Option<&str>, source: AgentSource) {
            self.heard.send(said(self.terminal, status, native, source)).unwrap();
        }

        /// More reports of other terminals than the broadcast holds, so the observer falls
        /// behind and misses what came before them.
        fn flood(&self) {
            for _ in 0..80 {
                let other = said(SessionId::new(), AgentStatus::Idle, None, AgentSource::Process);
                self.heard.send(other).unwrap();
            }
        }

        /// The agent writes `scenario`'s transcripts, and a hook fires.
        fn write(&self, scenario: &str) {
            let from = fixture("conversation", scenario);
            std::fs::copy(from.join("transcript.jsonl"), &self.main).unwrap();
            if let Ok(agents) = std::fs::read_dir(from.join("subagents")) {
                let to = self.main.with_extension("").join("subagents");
                std::fs::create_dir_all(&to).unwrap();
                for agent in agents {
                    let agent = agent.unwrap().path();
                    std::fs::copy(&agent, to.join(agent.file_name().unwrap())).unwrap();
                }
            }
            self.heard();
        }

        /// The agent writes `transcript` as its main one, and a hook fires.
        fn write_main(&self, transcript: &str) {
            std::fs::write(&self.main, transcript).unwrap();
            self.heard();
        }

        fn heard(&self) {
            self.seen.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
        }

        /// The terminal reports its title and folder, as the daemon lists it.
        fn terminal_says(&self, title: &str, cwd: &str) {
            let summary = SessionSummary {
                id: self.terminal,
                title: title.to_owned(),
                cwd: Some(cwd.to_owned()),
                repo: None,
                branch: None,
                repo_id: None,
                changes: None,
                started_ms: WallMs::ZERO,
                cols: 80,
                rows: 24,
                state: SessionState::Running,
                viewers: 0,
                command: Vec::new(),
                progress: None,
                restored: None,
                program: Vec::new(),
            };
            self.events.send(WorkerMsg::SessionChanged(summary)).unwrap();
        }
    }

    /// What the tracker says of `session`'s agent.
    fn said(
        session: SessionId,
        status: AgentStatus,
        native: Option<&str>,
        source: AgentSource,
    ) -> AgentEvent {
        AgentEvent {
            session,
            status,
            agent_session: native.map(str::to_owned),
            detail: None,
            attention: false,
            source,
            since_ms: WallMs::from_millis(1),
            mode: None,
        }
    }

    /// Wait until `thread` in `host` satisfies `done`, waking on each change of the table and
    /// each batch the thread applies.
    async fn until(
        host: &Host,
        thread: ThreadId,
        done: impl Fn(&ThreadState) -> bool,
    ) -> ThreadState {
        let mut table = host.table_watch();
        let mut feed = None;
        let waited = tokio::time::timeout(BOUND, async {
            loop {
                // Subscribed before looking, so a change between the two still wakes it.
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

    /// Wait until `host` no longer holds `thread`.
    async fn gone(host: &Host, thread: ThreadId) {
        let mut table = host.table_watch();
        let waited = tokio::time::timeout(BOUND, async {
            while host.holds(thread) {
                table.changed().await.unwrap();
            }
        });
        waited.await.expect("the thread went");
    }

    /// The entries the decoder reads from `scenario`'s main transcript.
    fn entries(scenario: &str) -> Vec<String> {
        let mut conversation = Conversation::default();
        let path = fixture("conversation", scenario).join("transcript.jsonl");
        conversation.read(&mut Tail::default(), &path).unwrap();
        conversation.entries(&ConvThread::Main).iter().map(|e| e.id.clone()).collect()
    }

    fn ids(state: &ThreadState) -> Vec<String> {
        state.items.iter().map(|i| i.id.0.clone()).collect()
    }

    /// A Claude Code session becomes a thread named by its own id, with every entry of its
    /// transcript, its status, its held prompts, and a linked thread per subagent; a worker that
    /// restarts reads it again into the same thread under a new epoch.
    #[tokio::test]
    async fn a_session_becomes_a_thread_and_comes_back_after_a_restart() {
        let rig = Rig::new();
        let host = rig.host();
        let (observer, driver) = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        rig.write("tools");
        let want = entries("tools");
        let state = until(&host, thread, |s| ids(s) == want).await;
        assert_eq!(state.status.phase, Phase::Working);
        assert_eq!(state.meta.terminal, Some(rig.terminal));
        assert_ne!(state.meta.title, "");
        let child = state
            .items
            .iter()
            .find_map(|i| match &i.body {
                ItemBody::Tool(call) => call.child,
                _ => None,
            })
            .expect("an Agent call names its thread");
        let sub = until(&host, child, |s| !s.items.is_empty()).await;
        assert_eq!(sub.meta.parent.map(|p| p.thread), Some(thread));
        assert_eq!(child, thread.subagent(&sub.meta.native));

        let prompt = PermissionPrompt {
            session: rig.terminal,
            ask: 3,
            tool: "Bash".to_owned(),
            call: None,
            detail: ToolDetail::Other {
                input: slopty_proto::conversation::Clipped::head(
                    "{}",
                    slopty_proto::conversation::Cap { lines: 1, chars: 10 },
                    None,
                ),
            },
            suggestions: vec![],
            mode: None,
            asked_ms: WallMs::from_millis(1),
            until_ms: WallMs::from_millis(2),
            declined: None,
        };
        driver.permission(PermissionEvent::Asked(Box::new(prompt)));
        rig.status(AgentStatus::Blocked(slopty_agent::status::BlockReason::Permission {
            tool: "Bash".to_owned(),
        }));
        let state = until(&host, thread, |s| {
            s.open_requests().count() == 1 && s.status.phase == Phase::NeedsYou
        })
        .await;
        let row = state.row(WallMs::ZERO);
        assert_eq!(row.requests.len(), 1);
        let before = host.state(thread).unwrap().1;

        observer.abort();
        drop(host);
        let host = rig.host();
        let _observer = rig.observe(&host);
        rig.status(AgentStatus::Idle);
        let state = until(&host, thread, |s| ids(s) == want && s.status.phase == Phase::Idle).await;
        let after = host.state(thread).unwrap().1;
        assert_ne!(after.epoch, before.epoch, "read again under a new epoch");
        assert_eq!(state.open_requests().count(), 0, "a prompt is not carried over a restart");
    }

    /// A text the thread carries clipped is read whole from the transcript through the driver;
    /// a reference nothing answers for is gone. The `tools` capture's first tool result is
    /// made long enough to clip.
    #[tokio::test]
    async fn a_clipped_text_expands_from_the_transcript() {
        use slopty_proto::thread::wire::Expanded;
        use slopty_proto::thread::{Clipped, ContentRef, ItemBody};
        let rig = Rig::new();
        let host = rig.host();
        let (_observer, driver) = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        let captured = fixture("conversation", "tools").join("transcript.jsonl");
        let long = (0..200).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let mut lengthened = false;
        let records: Vec<String> = std::fs::read_to_string(captured)
            .unwrap()
            .lines()
            .map(|line| {
                let Ok(mut record) = serde_json::from_str::<serde_json::Value>(line) else {
                    return line.to_owned();
                };
                if let Some(blocks) = record["message"]["content"].as_array_mut()
                    && let Some(result) = blocks.iter_mut().find(|b| b["type"] == "tool_result")
                    && !lengthened
                {
                    result["content"] = long.clone().into();
                    lengthened = true;
                }
                record.to_string()
            })
            .collect();
        assert!(lengthened, "the capture has a tool result");
        rig.write_main(&format!("{}\n", records.join("\n")));
        let want = entries("tools");
        let state = until(&host, thread, |s| ids(s) == want).await;
        let clipped: Vec<&Clipped> = state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Tool(call) => Some(std::iter::once(&call.input).chain(&call.output)),
                _ => None,
            })
            .flatten()
            .filter(|c| c.is_clipped())
            .collect();
        let clip = clipped.first().expect("the fixture clips a tool's text");
        let content = clip.full.clone().expect("a clipped text says where its whole is");
        let Expanded::Text(whole) = driver.expand(rig.terminal, content).await else {
            panic!("the whole text");
        };
        assert_eq!(whole.trim_end(), long, "the whole of it");
        assert!(clip.text.len() < whole.len());
        let unknown = ContentRef("not a reference".to_owned());
        assert_eq!(driver.expand(rig.terminal, unknown).await, Expanded::Gone);
        assert_eq!(
            driver.expand(SessionId::new(), clip.full.clone().unwrap()).await,
            Expanded::Gone,
            "no Claude Code observed in that terminal"
        );
    }

    /// Where the mod is heard, what the model writes shows as items, after the prompt, before
    /// the transcript has it, and goes once the transcript settles it.
    #[tokio::test]
    async fn the_mods_blocks_stream_and_settle() {
        let rig = Rig::new();
        let host = rig.host();
        let _observer = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        until(&host, thread, |_| true).await;
        let recorded = fixture("mod", "bash");
        // Written now, not when it was recorded: an entry stamped long before its block was
        // first shown is an older one, so the stamps go.
        let records: Vec<String> = std::fs::read_to_string(recorded.join("transcript.jsonl"))
            .unwrap()
            .lines()
            .map(|line| {
                let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
                record.as_object_mut().unwrap().remove("timestamp");
                format!("{record}\n")
            })
            .collect();
        // The prompt reaches the transcript first, as Claude Code writes it before the model
        // answers.
        let prompt = records.iter().position(|r| r.contains(r#""type":"user""#)).unwrap();
        std::fs::write(&rig.main, records[..=prompt].concat()).unwrap();
        rig.seen.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
        let asked = |s: &ThreadState| s.items.iter().any(|i| matches!(i.body, ItemBody::User(_)));
        until(&host, thread, asked).await;
        let text = std::fs::read_to_string(recorded.join("events.jsonl")).unwrap();
        let mut board = Board::default();
        let now = Instant::now();
        for line in text.lines() {
            let batch: Batch = serde_json::from_str(line).unwrap();
            for event in batch.decoded() {
                if !matches!(
                    event,
                    ModEvent::Bye | ModEvent::Stop(_) | ModEvent::TurnComplete { .. }
                ) {
                    board.apply(&event, now);
                }
            }
        }
        rig.seen.seen.send_modify(|seen| seen.live = board);
        let live = |s: &ThreadState| s.items.iter().filter(|i| i.id.0.starts_with("live:")).count();
        let state = until(&host, thread, |s| live(s) > 0).await;
        assert!(matches!(state.items[0].body, ItemBody::User(_)), "{:?}", state.items);
        std::fs::write(&rig.main, records.concat()).unwrap();
        rig.seen.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
        let state = until(&host, thread, |s| live(s) == 0 && !s.items.is_empty()).await;
        assert!(state.items.iter().all(|i| !i.id.0.starts_with("live:")));
    }

    /// A Claude Code started by hand, idle at its prompt with no session id yet, has a thread
    /// named by its terminal, without approvals while no hook has spoken. When the id comes,
    /// the session's own thread takes over, declares approvals for the hook heard, and the
    /// provisional one is removed, not left behind; one whose agent goes first is removed too.
    #[tokio::test]
    async fn a_claude_code_before_its_id_has_its_terminals_thread_until_the_id_comes() {
        let approvals = Cap::named(Cap::APPROVALS);
        let rig = Rig::new();
        rig.seen.main.send_replace(None);
        let host = rig.host();
        let _observer = rig.observe(&host);
        let provisional = terminal_thread(rig.terminal);
        rig.tracked(AgentStatus::Idle, None, AgentSource::Process);
        let state = until(&host, provisional, |s| s.status.phase == Phase::Idle).await;
        assert_eq!(state.meta.native, "");
        assert_eq!(state.meta.terminal, Some(rig.terminal));
        assert!(!state.meta.caps.contains(&approvals), "no hook has spoken");

        rig.seen.main.send_replace(Some(rig.main.clone()));
        rig.status(AgentStatus::Working);
        let thread = thread_of(NATIVE);
        let state = until(&host, thread, |s| {
            s.status.phase == Phase::Working && s.meta.caps.contains(&approvals)
        })
        .await;
        assert_eq!(state.meta.native, NATIVE);
        gone(&host, provisional).await;

        let other = Rig::new();
        other.seen.main.send_replace(None);
        let host = other.host();
        let _observer = other.observe(&host);
        let provisional = terminal_thread(other.terminal);
        other.tracked(AgentStatus::Idle, None, AgentSource::Process);
        until(&host, provisional, |_| true).await;
        other.tracked(AgentStatus::None, None, AgentSource::Process);
        gone(&host, provisional).await;
    }

    /// A session's last status lost when the observer fell behind the daemon's reports is read
    /// again from the table, so its thread does not stay working; a terminal whose close was
    /// lost the same way is let go, its provisional thread with it.
    #[tokio::test]
    async fn what_a_lag_loses_is_read_again() {
        let rig = Rig::new();
        let host = rig.host();
        let _observer = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        until(&host, thread, |s| s.status.phase == Phase::Working).await;
        let idle = said(rig.terminal, AgentStatus::Idle, Some(NATIVE), AgentSource::Hook);
        rig.seen.standing.lock().push(idle);
        rig.status(AgentStatus::Idle);
        rig.flood();
        until(&host, thread, |s| s.status.phase == Phase::Idle).await;

        let other = Rig::new();
        other.seen.main.send_replace(None);
        let host = other.host();
        let _observer = other.observe(&host);
        let provisional = terminal_thread(other.terminal);
        other.tracked(AgentStatus::Idle, None, AgentSource::Process);
        until(&host, provisional, |_| true).await;
        other.seen.closed.lock().push(other.terminal);
        let reason = slopty_proto::terminal::CloseReason::Exited;
        other.events.send(WorkerMsg::SessionClosed { session: other.terminal, reason }).unwrap();
        for _ in 0..80 {
            other.events.send(WorkerMsg::Load(0.0)).unwrap();
        }
        gone(&host, provisional).await;
    }

    /// A thread works where the agent's hooks say: a hook's folder outranks the terminal's
    /// from the first hook that names it, even one naming the folder the terminal already
    /// reported, so a later word from the terminal moves nothing.
    #[tokio::test]
    async fn a_hooks_folder_outranks_the_terminals_from_the_first_hook() {
        let rig = Rig::new();
        rig.seen.main.send_replace(None);
        let host = rig.host();
        let _observer = rig.observe(&host);
        let provisional = terminal_thread(rig.terminal);
        rig.terminal_says("\u{2733} One", "/work");
        rig.tracked(AgentStatus::Idle, None, AgentSource::Process);
        let state = until(&host, provisional, |s| s.meta.title == "One").await;
        assert_eq!(state.meta.cwd, "/work", "the terminal's folder, while no hook said one");

        rig.seen.seen.send_modify(|seen| {
            seen.cwd = Some("/work".to_owned());
            seen.hooks = seen.hooks.wrapping_add(1);
        });
        let approvals = Cap::named(Cap::APPROVALS);
        until(&host, provisional, |s| s.meta.caps.contains(&approvals)).await;
        rig.terminal_says("\u{2733} Two", "/elsewhere");
        rig.terminal_says("\u{2733} Three", "/elsewhere");
        let state = until(&host, provisional, |s| s.meta.title == "Three").await;
        assert_eq!(state.meta.cwd, "/work", "the hook's folder stands");

        rig.seen.seen.send_modify(|seen| seen.cwd = Some("/work/sub".to_owned()));
        until(&host, provisional, |s| s.meta.cwd == "/work/sub").await;
    }

    /// The status line's meters heard before the session's thread begins are its first: a
    /// window said once, before Claude Code named its session, is not lost until the next.
    #[tokio::test]
    async fn meters_heard_before_the_thread_begins_are_its_own() {
        let rig = Rig::new();
        let host = rig.host();
        let _observer = rig.observe(&host);
        let meters = slopty_proto::conversation::Meters {
            context_window: Some(1_000_000),
            ..slopty_proto::conversation::Meters::default()
        };
        rig.seen.seen.send_modify(|seen| seen.meters = Some(meters));
        rig.status(AgentStatus::Working);
        let state = until(&host, thread_of(NATIVE), |s| s.meters.context_window.is_some()).await;
        assert_eq!(state.meters.context_window, Some(1_000_000));
    }
}
