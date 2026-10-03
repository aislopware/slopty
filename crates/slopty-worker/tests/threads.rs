//! The thread host against its real files in a temporary data directory: the log across a
//! restart, a torn tail, compaction, following from a cursor, coalescing within a latency
//! budget, a log that starts over, the table's deltas, and intents acted on once per id.

#[cfg(test)]
mod threads {
    use std::collections::BTreeMap;
    use std::path::Path;

    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{Outcome, TableFrame, ThreadFrame};
    use slopty_proto::thread::{
        Action, AgentId, Cap, Changed, Clipped, Cursor, Delivery, Drive, Edge, IntentId, Item,
        ItemBody, ItemId, PartKey, Pending, PendingState, TableState, ThreadId, ThreadMeta,
        ThreadState, TreeRef, Turn, TurnId, TurnState, Usage, UserMessage,
    };
    use slopty_worker::thread::compose::TYPED_NOT_SENT;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Follower, Host};

    fn meta() -> ThreadMeta {
        ThreadMeta {
            modes: Vec::new(),
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: "2.1.286".to_owned(),
            native: "s1".to_owned(),
            cwd: "/work".to_owned(),
            title: "Fix the build".to_owned(),
            terminal: None,
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            forked_from: None,
            drive: Drive::named(Drive::OBSERVED),
            caps: vec![Cap::named(Cap::QUEUE)],
            models: Vec::new(),
            facts: BTreeMap::new(),
            created_ms: WallMs::from_millis(1),
        }
    }

    fn turn(n: u32) -> Action {
        Action::TurnStarted(Turn {
            id: TurnId(n),
            input: None,
            state: TurnState::Active,
            started_ms: WallMs::from_millis(u64::from(n)),
            ended_ms: None,
            usage: Usage::default(),
            models: vec![],
            changed: Changed::default(),
            before: None,
            after: None,
        })
    }

    fn ended(n: u32) -> Action {
        Action::TurnEnded {
            turn: TurnId(n),
            state: TurnState::Complete,
            usage: Usage::default(),
            ended_ms: WallMs::from_millis(u64::from(n)),
        }
    }

    fn text(id: &str, n: u32) -> Action {
        Action::ItemStarted(Item {
            id: ItemId(id.to_owned()),
            turn: TurnId(n),
            at_ms: WallMs::ZERO,
            body: ItemBody::Text(Clipped::default()),
        })
    }

    fn append(id: &str, words: &str) -> Action {
        Action::Append { item: ItemId(id.to_owned()), part: PartKey::Body, text: words.to_owned() }
    }

    /// A client's copy of a thread: what the frames it was sent made of it.
    #[derive(Default)]
    struct Mirror {
        state: Option<ThreadState>,
        cursor: Option<Cursor>,
        snapshots: usize,
    }

    impl Mirror {
        fn take(&mut self, frame: ThreadFrame) {
            match frame {
                ThreadFrame::Snapshot { cursor, state } => {
                    self.state = Some(*state);
                    self.cursor = Some(cursor);
                    self.snapshots = self.snapshots.saturating_add(1);
                }
                ThreadFrame::Actions { epoch, first, next, actions } => {
                    let cursor = self.cursor.expect("a snapshot first");
                    assert_eq!((cursor.epoch, cursor.seq), (epoch, first), "actions follow on");
                    let state = self.state.as_mut().expect("a state");
                    for action in &actions {
                        state.apply(action);
                    }
                    self.cursor = Some(Cursor { epoch, seq: next });
                }
                other => panic!("{other:?}"),
            }
        }
    }

    fn open(dir: &Path) -> Host {
        Host::open(dir, Limits::default()).expect("the host opens")
    }

    fn state(host: &Host, thread: ThreadId) -> (ThreadState, Cursor) {
        host.state(thread).expect("the thread is held")
    }

    /// The log outlives the worker: the state comes back whole, the epoch stays, and a cursor
    /// from before the restart is sent only what it missed.
    #[tokio::test]
    async fn a_thread_comes_back_after_a_restart_and_a_cursor_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        host.apply(thread, vec![turn(1), text("a", 1), append("a", "Good ")]);
        let mut client = Mirror::default();
        let mut follower = Follower::new(host.clone(), thread, None, 10, 0);
        client.take(follower.next().await.unwrap());
        assert_eq!(client.snapshots, 1);
        let held = client.cursor.unwrap();
        host.apply(thread, vec![append("a", "day"), ended(1)]);
        let before = state(&host, thread);
        drop((follower, host));

        let host = open(dir.path());
        assert_eq!(state(&host, thread), before, "the same state at the same cursor");
        let mut follower = Follower::new(host.clone(), thread, Some(held), 10, 0);
        let frame = follower.next().await.unwrap();
        let ThreadFrame::Actions { actions, .. } = &frame else { panic!("{frame:?}") };
        assert_eq!(actions, &[append("a", "day"), ended(1)], "only what it missed");
        client.take(frame);
        assert_eq!(client.state.as_ref(), Some(&before.0));
        assert_eq!(client.snapshots, 1);
    }

    /// A tail file cut mid-action loses only that action, and the log goes on from there.
    #[tokio::test]
    async fn a_torn_tail_loses_only_its_torn_action() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        host.apply(thread, vec![turn(1), text("a", 1)]);
        let whole = state(&host, thread);
        host.apply(thread, vec![append("a", "lost")]);
        drop(host);
        let tail = dir.path().join(thread.to_string()).join("tail");
        let len = std::fs::metadata(&tail).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&tail).unwrap().set_len(len - 2).unwrap();

        let host = open(dir.path());
        assert_eq!(state(&host, thread), whole);
        host.apply(thread, vec![append("a", "kept")]);
        drop(host);
        let host = open(dir.path());
        let (state, cursor) = state(&host, thread);
        assert_eq!(cursor.seq, whole.1.seq + 1);
        let ItemBody::Text(text) = &state.items[0].body else { panic!() };
        assert_eq!(text.text, "kept");
    }

    /// Past its bound, a turn's end writes a new snapshot and empties the tail file, and
    /// nothing is lost to a reader or to a follower within the kept tail.
    #[tokio::test]
    async fn a_turns_end_compacts_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits { tail_actions: 64, tail_bytes: 1 << 20, compact_after: 8 };
        let host = Host::open(dir.path(), limits).unwrap();
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        host.apply(thread, vec![turn(1), text("a", 1)]);
        let held = state(&host, thread).1;
        for n in 0..10 {
            host.apply(thread, vec![append("a", &n.to_string())]);
        }
        host.apply(thread, vec![ended(1)]);
        let tail = dir.path().join(thread.to_string()).join("tail");
        let small = std::fs::metadata(&tail).unwrap().len();
        assert!(small < 64, "the tail file started over: {small} bytes");
        let mut client = Mirror::default();
        let mut follower = Follower::new(host.clone(), thread, None, 10, 0);
        client.take(follower.next().await.unwrap());
        let mut resumed = Follower::new(host.clone(), thread, Some(held), 10, 0);
        let frame = resumed.next().await.unwrap();
        assert!(matches!(frame, ThreadFrame::Actions { first, next, .. } if next - first == 11));
        let before = state(&host, thread);
        drop((follower, resumed, host));
        assert_eq!(state(&Host::open(dir.path(), limits).unwrap(), thread), before);
    }

    /// A cursor older than the kept tail, of another epoch, or none, gets a snapshot; the
    /// snapshot carries the last turns, and the rest comes by page.
    #[tokio::test]
    async fn a_cursor_the_tail_cannot_serve_gets_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits { tail_actions: 4, ..Limits::default() };
        let host = Host::open(dir.path(), limits).unwrap();
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        for n in 1..=5 {
            host.apply(thread, vec![turn(n), text(&format!("i{n}"), n)]);
        }
        let (whole, cursor) = state(&host, thread);
        for have in [
            None,
            Some(Cursor { epoch: cursor.epoch, seq: 1 }),
            Some(Cursor { epoch: 7, seq: cursor.seq }),
        ] {
            let mut follower = Follower::new(host.clone(), thread, have, 2, 0);
            let mut client = Mirror::default();
            client.take(follower.next().await.unwrap());
            assert_eq!(client.snapshots, 1, "{have:?}");
            let mut copy = client.state.unwrap();
            assert_eq!(copy.turns.len(), 2);
            while copy.older {
                let first = copy.turns[0].id;
                copy.prepend(&host.page(thread, first, 2).unwrap());
            }
            assert_eq!(copy, whole);
        }
    }

    /// Within its latency budget a follower sends what came as one frame, appends merged, and
    /// the client ends where the worker is; with no budget each batch goes alone.
    #[tokio::test(start_paused = true)]
    async fn a_budget_gathers_batches_into_one_frame() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        host.apply(thread, vec![turn(1), text("a", 1)]);
        let mut gathered = Follower::new(host.clone(), thread, None, 10, 16);
        let mut slow = Mirror::default();
        slow.take(gathered.next().await.unwrap());
        let words = ["Sun", "day", " is", " here"];
        let eager = {
            let host = host.clone();
            tokio::spawn(async move {
                let mut follower = Follower::new(host, thread, None, 10, 0);
                let mut fast = Mirror::default();
                fast.take(follower.next().await.unwrap());
                let mut frames = 0;
                while fast.cursor.map(|c| c.seq) != Some(2 + 4) {
                    fast.take(follower.next().await.unwrap());
                    frames += 1;
                }
                (fast, frames)
            })
        };
        tokio::task::yield_now().await;
        let feeder = {
            let host = host.clone();
            tokio::spawn(async move {
                for word in words {
                    host.apply(thread, vec![append("a", word)]);
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            })
        };
        let frame = gathered.next().await.unwrap();
        let ThreadFrame::Actions { first, next, actions, .. } = &frame else { panic!("{frame:?}") };
        assert_eq!((next - first, actions.as_slice()), (4, &[append("a", "Sunday is here")][..]));
        slow.take(frame);
        feeder.await.unwrap();
        let (fast, frames) = eager.await.unwrap();
        assert_eq!(frames, words.len(), "no budget: a frame per batch");
        let now = state(&host, thread).0;
        assert_eq!((slow.state.as_ref(), fast.state.as_ref()), (Some(&now), Some(&now)));
    }

    /// A log that starts over (a resync from the agent's own session) sends every follower a
    /// snapshot of the new epoch.
    #[tokio::test]
    async fn a_reset_sends_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta.clone()).unwrap();
        host.apply(thread, vec![turn(1)]);
        let mut follower = Follower::new(host.clone(), thread, None, 10, 0);
        let mut client = Mirror::default();
        client.take(follower.next().await.unwrap());
        let old = client.cursor.unwrap();
        let fresh = ThreadState::new(meta);
        let cursor = host.reset(thread, fresh.clone()).unwrap().unwrap();
        assert_ne!(cursor.epoch, old.epoch);
        client.take(follower.next().await.unwrap());
        assert_eq!((client.snapshots, client.state, client.cursor), (2, Some(fresh), Some(cursor)));
    }

    /// The table sends only the rows that changed since a cursor, and the threads gone; a
    /// cursor from another run gets every row.
    #[tokio::test]
    async fn the_table_sends_deltas_from_a_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let (one, two) = (meta(), meta());
        let (a, b) = (one.id, two.id);
        host.create(one).unwrap();
        host.create(two).unwrap();
        let mut mirror = TableState::default();
        mirror.apply(&host.table(None));
        let held = mirror.cursor;
        host.apply(a, vec![turn(1), text("x", 1), append("x", "done")]);
        let TableFrame::Delta { rows, removed, .. } = host.table(Some(held)) else { panic!() };
        assert_eq!((rows.len(), rows[0].id, removed.len()), (1, a, 0));
        assert_eq!(rows[0].last_line.as_deref(), Some("done"));
        host.remove(b).unwrap();
        let delta = host.table(Some(held));
        mirror.apply(&delta);
        assert_eq!(mirror.rows.keys().copied().collect::<Vec<_>>(), [a]);
        assert!(!dir.path().join(b.to_string()).exists(), "its log is gone");
        drop(host);
        let host = open(dir.path());
        let TableFrame::Snapshot { rows, .. } = host.table(Some(mirror.cursor)) else {
            panic!("a new run's table starts with every row")
        };
        assert_eq!(rows.len(), 1);
    }

    /// A thread's row says the repository its directory is in, by its root and its origin, as
    /// a shell's summary does; a directory in none says none.
    #[tokio::test]
    async fn a_thread_row_names_its_repository_once_known() {
        let dir = tempfile::tempdir().unwrap();
        let clone = tempfile::tempdir().unwrap();
        let git = clone.path().join(".git");
        std::fs::create_dir_all(&git).unwrap();
        let remote = "[remote \"origin\"]\n\turl = git@github.com:aislopware/slopty.git\n";
        std::fs::write(git.join("config"), remote).unwrap();
        let crates = clone.path().join("crates");
        std::fs::create_dir_all(&crates).unwrap();
        let host = open(dir.path());
        let inside = ThreadMeta { cwd: crates.to_string_lossy().into_owned(), ..meta() };
        let outside = ThreadMeta { cwd: dir.path().to_string_lossy().into_owned(), ..meta() };
        let (a, b) = (inside.id, outside.id);
        host.create(inside).unwrap();
        host.create(outside).unwrap();
        let TableFrame::Snapshot { rows, .. } = host.table(None) else { panic!("a snapshot") };
        let row = |id: ThreadId| rows.iter().find(|r| r.id == id).expect("its row");
        let root = std::fs::canonicalize(clone.path()).unwrap();
        assert_eq!(row(a).repo.as_deref(), Some(root.to_string_lossy().as_ref()));
        let origin = row(a).repo_id.as_ref().and_then(|id| id.origin.as_deref());
        assert_eq!(origin, Some("github.com/aislopware/slopty"));
        assert_eq!(row(a).cwd.as_deref(), Some(crates.to_string_lossy().as_ref()));
        assert_eq!((row(b).repo.as_ref(), row(b).repo_id.as_ref()), (None, None));
    }

    /// An intent is acted on once per id, across a restart too; a start is too.
    #[tokio::test]
    async fn an_intent_is_acted_on_once() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let id = IntentId::new();
        let mut started = 0;
        let mut start = || {
            started += 1;
            Ok::<_, String>(meta())
        };
        let Outcome::Started { thread } = host.start(id, &mut start) else { panic!() };
        assert_eq!(host.start(id, &mut start), Outcome::Started { thread });
        assert_eq!(started, 1);
        let send = IntentId::new();
        let mut acted = 0;
        for _ in 0..3 {
            let outcome = host.intent(thread, send, |_| {
                acted += 1;
                (Outcome::Accepted, vec![turn(1)])
            });
            assert_eq!(outcome, Some(Outcome::Accepted));
        }
        assert_eq!((acted, state(&host, thread).0.turns.len()), (1, 1));
        drop(host);
        let host = open(dir.path());
        let again = host.intent(thread, send, |_| panic!("acted on twice"));
        assert_eq!(again, Some(Outcome::Accepted));
        assert_eq!(host.start(id, || panic!("started twice")), Outcome::Started { thread });
    }

    fn prompt(id: &str, n: u32, words: &str) -> Action {
        Action::ItemStarted(Item {
            id: ItemId(id.to_owned()),
            turn: TurnId(n),
            at_ms: WallMs::ZERO,
            body: ItemBody::User(UserMessage {
                text: Clipped::whole(words),
                images: vec![],
                command: None,
                intent: None,
            }),
        })
    }

    fn intent_of(state: &ThreadState, id: &str) -> Option<IntentId> {
        state.items.iter().find(|i| i.id.0 == id).and_then(|i| match &i.body {
            ItemBody::User(message) => message.intent,
            _ => None,
        })
    }

    /// What the worker adds to a thread and its agent never tells (the intent a message came
    /// from, each turn's snapshots, what waits to be sent) outlives the thread being read again
    /// from the agent's session, after a worker restart too. A message that was being typed is
    /// not typed again.
    #[tokio::test]
    async fn what_the_worker_owns_outlives_a_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta.clone()).unwrap();
        let (sent, waiting, sending) = (IntentId::new(), IntentId::new(), IntentId::new());
        host.typed(thread, sent, "hello there");
        let tree = TreeRef("4b825dc6".to_owned());
        let pending = |intent, state| Pending {
            intent,
            text: "later".to_owned(),
            delivery: Delivery::Queue,
            state,
        };
        host.apply(
            thread,
            vec![
                turn(1),
                prompt("p1", 1, "  hello there\n"),
                Action::Snapshot { turn: TurnId(1), edge: Edge::Before, tree: tree.clone() },
                Action::PendingSet(vec![
                    pending(waiting, PendingState::Waiting),
                    pending(sending, PendingState::Sending),
                ]),
            ],
        );
        assert_eq!(
            intent_of(&state(&host, thread).0, "p1"),
            Some(sent),
            "the item is the intent's"
        );
        drop(host);

        let host = open(dir.path());
        host.reset(thread, ThreadState::new(meta)).unwrap();
        host.apply(thread, vec![turn(1), prompt("p1", 1, "hello there")]);
        let (again, _) = state(&host, thread);
        assert_eq!(intent_of(&again, "p1"), Some(sent));
        assert_eq!(again.turns.first().and_then(|t| t.before.clone()), Some(tree));
        let states: Vec<_> = again.pending.iter().map(|p| (p.intent, p.state.clone())).collect();
        let typed = PendingState::Held { reason: TYPED_NOT_SENT.to_owned() };
        assert_eq!(states, [(waiting, PendingState::Waiting), (sending, typed)]);
    }
}
