//! The thread host against its real files in a temporary data directory: the log across a
//! restart, a torn tail, compaction, following from a cursor, coalescing within a latency
//! budget, a log that starts over, the table's deltas, and intents acted on once per id.

#[cfg(test)]
mod threads {
    use std::collections::BTreeMap;
    use std::path::Path;

    use slopty_core::WallMs;
    use slopty_proto::thread::wire::{Intent, Outcome, TableFrame, ThreadFrame};
    use slopty_proto::thread::{
        Action, AgentId, Cap, Changed, Clipped, Cursor, Delivery, Drive, Edge, IntentId, Item,
        ItemBody, ItemId, PartKey, Pending, PendingState, TableState, ThreadId, ThreadMeta,
        ThreadState, TreeRef, Turn, TurnId, TurnState, Usage, UserMessage,
    };
    use slopty_worker::thread::compose::TYPED_NOT_SENT;
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Follower, Host, Seated, schedule};

    fn meta() -> ThreadMeta {
        ThreadMeta {
            modes: Vec::new(),
            efforts: Vec::new(),
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
            attachments: vec![],
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

    /// Where a thread was branched from outlives the agent's own account of it: a meta that
    /// does not say, a read again from nothing, and a worker that starts again. An agent that
    /// names the thread but not the turn has the turn put back.
    #[tokio::test]
    async fn where_a_fork_came_from_outlives_a_read_again_and_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let branch = meta();
        let thread = branch.id;
        host.create(branch.clone()).unwrap();
        let from = slopty_proto::thread::Fork { thread: ThreadId::new(), turn: Some(TurnId(3)) };
        host.forked(thread, from);
        let kept = |host: &Host| {
            let (state, _) = state(host, thread);
            assert_eq!(state.meta.forked_from, Some(from));
            assert_eq!(state.meta.origin, ThreadMeta::FORK);
        };
        kept(&host);
        host.apply(thread, vec![Action::Meta(Box::new(branch.clone()))]);
        kept(&host);
        let mut named = branch.clone();
        named.forked_from = Some(slopty_proto::thread::Fork { thread: from.thread, turn: None });
        host.apply(thread, vec![Action::Meta(Box::new(named))]);
        kept(&host);
        let mut fresh = ThreadState::new(branch.clone());
        fresh.meta.forked_from = None;
        host.reset(thread, fresh).unwrap();
        kept(&host);
        drop(host);

        let host = open(dir.path());
        kept(&host);
        host.apply(thread, vec![Action::Meta(Box::new(branch))]);
        kept(&host);
    }

    /// The seat a server's task started a thread at outlives the agent's own account of the
    /// thread, a read again and a restart: the row keeps naming it, and the whole seat comes
    /// back from the thread's directory. What the worker adds for the seat (its token among it)
    /// is never kept, only given where the agent runs.
    #[tokio::test]
    async fn a_seat_outlives_a_read_again_and_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let started = meta();
        let thread = started.id;
        host.create(started.clone()).unwrap();
        let seat = slopty_core::SessionId::new();
        let seated = Seated {
            seat,
            env: vec![("SLOPTY_TASK".to_owned(), "task-1".to_owned())],
            role: Some("You review.".to_owned()),
            relay: Some("/opt/slopty/bin/slopty".to_owned()),
        };
        host.seated(thread, &seated);
        let kept = |host: &Host| {
            let (state, _) = state(host, thread);
            let fact = state.meta.facts.get(slopty_proto::project::SEAT_FACT);
            assert_eq!(fact, Some(&seat.to_string()));
            assert_eq!(host.seated_of(thread).as_ref(), Some(&seated));
            assert_eq!(host.seated_at(seat), Some(thread));
        };
        kept(&host);
        host.apply(thread, vec![Action::Meta(Box::new(started.clone()))]);
        kept(&host);
        host.reset(thread, ThreadState::new(started)).unwrap();
        kept(&host);
        drop(host);

        let host = open(dir.path());
        kept(&host);
        assert_eq!(host.env_of(&seated), seated.env, "the server's alone, until the daemon says");
        host.set_seat_env(std::sync::Arc::new(|seat, extra| {
            let mut env = extra.to_vec();
            env.push(("SLOPTY_SESSION_TOKEN".to_owned(), format!("token-for-{seat}")));
            env
        }));
        let env = host.env_of(&seated);
        assert_eq!(env[..1], seated.env);
        assert_eq!(env[1], ("SLOPTY_SESSION_TOKEN".to_owned(), format!("token-for-{seat}")));
        let file =
            std::fs::read_to_string(dir.path().join(thread.to_string()).join("seat.json")).unwrap();
        assert!(!file.contains("token"), "no token is kept: {file}");
    }

    /// An aside keeps its mark through its agent's own account of the thread, a read again and
    /// a restart, and its row names the thread it was asked beside; kept, it is an ordinary
    /// thread again; forgotten, it leaves no log behind.
    #[tokio::test]
    async fn an_aside_keeps_its_mark_until_it_is_kept_or_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let parent = ThreadId::new();
        let started = meta();
        let thread = started.id;
        host.create(started.clone()).unwrap();
        assert!(!host.aside(ThreadId::new(), Some(parent)), "no such thread");
        assert!(host.aside(thread, Some(parent)));
        let marked = |host: &Host| {
            let (state, _) = state(host, thread);
            let row = state.row(WallMs::ZERO);
            assert_eq!(row.facts.get(ThreadMeta::ASIDE_FACT), Some(&parent.to_string()));
            state.meta.aside_of()
        };
        assert_eq!(marked(&host), Some(parent));
        host.apply(thread, vec![Action::Meta(Box::new(started.clone()))]);
        assert_eq!(marked(&host), Some(parent), "the agent's account knows nothing of it");
        host.reset(thread, ThreadState::new(started.clone())).unwrap();
        assert_eq!(marked(&host), Some(parent), "read again");
        drop(host);

        let host = open(dir.path());
        assert_eq!(marked(&host), Some(parent), "after a restart");
        assert!(host.aside(thread, None));
        host.apply(thread, vec![Action::Meta(Box::new(started))]);
        assert_eq!(state(&host, thread).0.meta.aside_of(), None, "kept: an ordinary thread");
        assert!(host.aside(thread, Some(parent)));
        host.remove(thread).unwrap();
        assert!(host.state(thread).is_none());
        assert!(!dir.path().join(thread.to_string()).exists(), "no log left behind");
    }

    fn scheduled(text: &str, delivery: Delivery) -> Intent {
        Intent::Send { text: text.to_owned(), delivery, attachments: vec![] }
    }

    fn pending_of(host: &Host, thread: ThreadId) -> Vec<(String, Delivery, PendingState)> {
        let (state, _) = state(host, thread);
        state.pending.iter().map(|p| (p.text.clone(), p.delivery, p.state.clone())).collect()
    }

    /// A message the person schedules waits in the thread's pending list on the worker, once
    /// per intent, whatever list the agent's adapter tells, and outlives a restart; it is taken
    /// back or changed there, and one being sent as the worker stopped comes back held, never
    /// sent twice. A draft waits for the person's word. A message cannot be scheduled on a
    /// thread that cannot take it.
    #[tokio::test]
    async fn a_scheduled_message_waits_on_the_worker_and_outlives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut plan = meta();
        plan.caps.push(Cap::named(Cap::SCHEDULE));
        let thread = plan.id;
        host.create(plan.clone()).unwrap();
        let at = Delivery::At { at_ms: WallMs::from_millis(u64::MAX) };
        let after = Delivery::Draft;
        let (first, second) = (IntentId::new(), IntentId::new());
        let act = |id, intent: &Intent| schedule::act(&host, thread, id, intent);
        assert_eq!(act(first, &scheduled("Run the checks", at)), Some(Outcome::Accepted));
        assert_eq!(act(first, &scheduled("Run the checks", at)), Some(Outcome::Accepted), "once");
        assert_eq!(act(second, &scheduled("Review it", after)), Some(Outcome::Accepted));
        let waiting = |text: &str, delivery| (text.to_owned(), delivery, PendingState::Waiting);
        let both = vec![waiting("Run the checks", at), waiting("Review it", after)];
        assert_eq!(pending_of(&host, thread), both);
        assert!(host.is_scheduled(thread, first));
        let now =
            Intent::Send { text: "Now".to_owned(), delivery: Delivery::Steer, attachments: vec![] };
        assert_eq!(act(IntentId::new(), &now), None, "a message now is the agent's");

        // The adapter tells its own queue, which knows nothing of them: they stay.
        let queued = Pending {
            intent: IntentId::new(),
            text: "Then this".to_owned(),
            attachments: vec![],
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        };
        host.apply(thread, vec![Action::PendingSet(vec![queued])]);
        let mut three = vec![waiting("Then this", Delivery::Queue)];
        three.extend(both.iter().cloned());
        assert_eq!(pending_of(&host, thread), three);
        host.apply(thread, vec![Action::PendingSet(Vec::new())]);
        assert_eq!(pending_of(&host, thread), both);

        let edit = Intent::Edit { pending: second, text: "Review it all".to_owned() };
        assert_eq!(act(IntentId::new(), &edit), Some(Outcome::Done));
        let edited = vec![waiting("Run the checks", at), waiting("Review it all", after)];
        assert_eq!(pending_of(&host, thread), edited);
        drop(host);

        let host = open(dir.path());
        assert_eq!(pending_of(&host, thread), edited, "kept through a restart");
        let (due, next) = host.due(WallMs::now());
        assert_eq!((due, next), (Vec::new(), Some(WallMs::from_millis(u64::MAX))));
        let promote = Intent::Promote { pending: first };
        assert_eq!(schedule::act(&host, thread, IntentId::new(), &promote), Some(Outcome::Done));
        let (due, _) = host.due(WallMs::now());
        assert_eq!(due.len(), 1, "sent now: {due:?}");
        assert_eq!(due[0].intent, first);
        let send = Intent::Send {
            text: "Run the checks".to_owned(),
            attachments: vec![],
            delivery: Delivery::Queue,
        };
        assert_eq!(due[0].send, send, "queued, as the agent queues");
        assert_eq!(pending_of(&host, thread)[0].2, PendingState::Sending);
        assert_eq!(host.due(WallMs::now()).0, Vec::new(), "never twice");
        drop(host);

        // The worker stopped while it went: held, saying it may have gone.
        let host = open(dir.path());
        let cut = PendingState::Held { reason: schedule::CUT_OFF.to_owned() };
        assert_eq!(pending_of(&host, thread)[0].2, cut);
        assert_eq!(host.due(WallMs::now()).0, Vec::new(), "never sent again");
        let withdraw = Intent::Withdraw { pending: first };
        assert_eq!(schedule::act(&host, thread, IntentId::new(), &withdraw), Some(Outcome::Done));
        // A draft goes only on the person's word.
        assert_eq!(host.due(WallMs::now()).0, Vec::new());
        assert_eq!(pending_of(&host, thread), [waiting("Review it all", after)]);

        let mut plain = meta();
        plain.caps.clear();
        let other = plain.id;
        host.create(plain).unwrap();
        let cannot = schedule::act(&host, other, IntentId::new(), &scheduled("Hi", at)).unwrap();
        assert_eq!(cannot, Outcome::Unsupported { cap: Cap::named(Cap::SCHEDULE) });
    }

    /// The worker sends each scheduled message at its time, once. One its agent takes leaves
    /// the list; one it turns down stays, held with the reason.
    #[tokio::test]
    async fn the_worker_sends_a_scheduled_message_at_its_moment() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut plan = meta();
        plan.caps.push(Cap::named(Cap::SCHEDULE));
        let thread = plan.id;
        host.create(plan).unwrap();

        let (tx, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let fire: schedule::Fire = std::sync::Arc::new(move |thread, id, intent| {
            let Intent::Send { text, .. } = &intent else { panic!("{intent:?}") };
            let outcome = if text == "Turned down" {
                Outcome::Refused { reason: "The agent is gone".to_owned() }
            } else {
                Outcome::Done
            };
            tx.send((thread, id, intent)).unwrap();
            outcome
        });
        let _scheduler = schedule::spawn(host.clone(), fire);
        let soon = WallMs::from_millis(WallMs::now().as_millis() + 150);
        let (timed, down) = (IntentId::new(), IntentId::new());
        let act = |id, intent: &Intent| schedule::act(&host, thread, id, intent);
        act(timed, &scheduled("On time", Delivery::At { at_ms: soon }));
        act(down, &scheduled("Turned down", Delivery::At { at_ms: WallMs::ZERO }));

        let (_, id, _) = next(&mut sent).await;
        assert_eq!(id, schedule::sent_as(down), "the one already due first");
        let (to, id, intent) = next(&mut sent).await;
        assert!(WallMs::now() >= soon, "not before its time");
        assert_eq!((to, id), (thread, schedule::sent_as(timed)));
        assert!(matches!(intent, Intent::Send { delivery: Delivery::Queue, .. }));
        let held = PendingState::Held { reason: "The agent is gone".to_owned() };
        let left: Vec<(String, PendingState)> =
            pending_of(&host, thread).into_iter().map(|(text, _, state)| (text, state)).collect();
        assert_eq!(left, [("Turned down".to_owned(), held)], "taken ones leave; that one stays");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(sent.try_recv().is_err(), "each once");
    }

    type Sent = (ThreadId, IntentId, Intent);

    /// What the scheduler sent next.
    async fn next(sent: &mut tokio::sync::mpsc::UnboundedReceiver<Sent>) -> Sent {
        tokio::time::timeout(std::time::Duration::from_secs(10), sent.recv())
            .await
            .expect("sent in time")
            .expect("the scheduler runs")
    }
}
