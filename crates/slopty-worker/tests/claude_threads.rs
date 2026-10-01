//! Claude Code observed into the thread host, end to end on the worker's side: the daemon's
//! broadcast and a session's sources are played by the test (a status, a held prompt, hooks
//! heard, the mod's board), the transcripts are a real Claude Code's, recorded
//! (`slopty-agent/tests/fixtures`), written into a session directory as the agent would write
//! them, and the threads are judged from the host.

#[cfg(test)]
mod claude_threads {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use slopty_agent::conversation::{Conversation, ThreadId as ConvThread};
    use slopty_agent::live::{Batch, Board, ModEvent};
    use slopty_agent::observed::{subagent_of, thread_of};
    use slopty_agent::transcript::Tail;
    use slopty_core::{SessionId, WallMs};
    use slopty_proto::WorkerMsg;
    use slopty_proto::agent::{AgentEvent, AgentKind, AgentSource, AgentStatus};
    use slopty_proto::conversation::{PermissionEvent, PermissionPrompt, ToolDetail};
    use slopty_proto::thread::{Phase, ThreadId, ThreadState};
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
        main: PathBuf,
        seen: watch::Sender<Seen>,
    }

    impl Sources for Fake {
        fn sources(&self, _session: SessionId) -> orchestrate::Sources {
            orchestrate::Sources { main: Some(self.main.clone()), ..Default::default() }
        }

        fn seen(&self, _session: SessionId) -> watch::Receiver<Seen> {
            self.seen.subscribe()
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        main: PathBuf,
        terminal: SessionId,
        events: broadcast::Sender<WorkerMsg>,
        seen: Arc<Fake>,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let main = dir.path().join("projects").join(format!("{NATIVE}.jsonl"));
            std::fs::create_dir_all(main.parent().unwrap()).unwrap();
            std::fs::write(&main, "").unwrap();
            let seen =
                Arc::new(Fake { main: main.clone(), seen: watch::Sender::new(Seen::default()) });
            Self { dir, main, terminal: SessionId::new(), events: broadcast::Sender::new(64), seen }
        }

        fn host(&self) -> Host {
            Host::open(&self.dir.path().join("threads"), Limits::default()).unwrap()
        }

        fn observe(&self, host: &Host) -> tokio::task::JoinHandle<()> {
            let sources: Arc<dyn Sources> = Arc::<Fake>::clone(&self.seen);
            claude::spawn(host.clone(), self.events.subscribe(), sources)
        }

        fn status(&self, status: AgentStatus) {
            let event = AgentEvent {
                session: self.terminal,
                kind: AgentKind::ClaudeCode,
                status,
                agent_session: Some(NATIVE.to_owned()),
                detail: None,
                attention: false,
                source: AgentSource::Hook,
                since_ms: WallMs::from_millis(1),
                mode: None,
            };
            self.events.send(WorkerMsg::Agent(event)).unwrap();
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
            self.seen.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
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
        let observer = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        rig.write("tools");
        let want = entries("tools");
        let state = until(&host, thread, |s| ids(s) == want).await;
        assert_eq!(state.status.phase, Phase::Working);
        assert_eq!(state.meta.terminal, Some(rig.terminal));
        assert!(!state.meta.title.is_empty());
        let child = state
            .items
            .iter()
            .find_map(|i| match &i.body {
                slopty_proto::thread::ItemBody::Tool(call) => call.child,
                _ => None,
            })
            .expect("an Agent call names its thread");
        let sub = until(&host, child, |s| !s.items.is_empty()).await;
        assert_eq!(sub.meta.parent.map(|p| p.thread), Some(thread));
        assert_eq!(child, subagent_of(thread, &sub.meta.native));

        let prompt = PermissionPrompt {
            session: rig.terminal,
            ask: 3,
            tool: "Bash".to_owned(),
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
        };
        rig.events.send(WorkerMsg::Permission(PermissionEvent::Asked(Box::new(prompt)))).unwrap();
        rig.status(AgentStatus::Blocked(slopty_proto::agent::BlockReason::Permission {
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

    /// Where the mod is heard, what the model writes shows as items before the transcript has
    /// it, and goes once the transcript settles it.
    #[tokio::test]
    async fn the_mods_blocks_stream_and_settle() {
        let rig = Rig::new();
        let host = rig.host();
        let _observer = rig.observe(&host);
        let thread = thread_of(NATIVE);
        rig.status(AgentStatus::Working);
        until(&host, thread, |_| true).await;
        let recorded = fixture("mod", "bash");
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
        until(&host, thread, |s| live(s) > 0).await;
        // Written now, not when it was recorded: an entry stamped long before its block was
        // first shown is an older one, so the stamps go.
        let mut transcript = String::new();
        for line in std::fs::read_to_string(recorded.join("transcript.jsonl")).unwrap().lines() {
            let mut record: serde_json::Value = serde_json::from_str(line).unwrap();
            record.as_object_mut().unwrap().remove("timestamp");
            transcript.push_str(&record.to_string());
            transcript.push('\n');
        }
        std::fs::write(&rig.main, transcript).unwrap();
        rig.seen.seen.send_modify(|seen| seen.hooks = seen.hooks.wrapping_add(1));
        let state = until(&host, thread, |s| live(s) == 0 && !s.items.is_empty()).await;
        assert!(state.items.iter().all(|i| !i.id.0.starts_with("live:")));
    }
}
