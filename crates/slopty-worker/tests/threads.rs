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

    /// What opening the host costs a worker that kept many threads: 2,000 threads of 20 turns,
    /// each with an answer of about 2 KB, written once, then opened again and timed. Run by
    /// hand for `docs/MEASUREMENTS.md` (readiness-w 10-11 rank 7):
    /// `cargo nextest run -p slopty-worker --release --run-ignored only -E
    /// 'test(host_open_cost)' --no-capture`.
    #[test]
    #[ignore = "a measurement, run by hand"]
    fn host_open_cost() {
        const THREADS: usize = 2_000;
        const TURNS: u32 = 20;
        let dir = tempfile::tempdir().unwrap();
        let answer = "The build failed on a missing import, now fixed. ".repeat(40);
        {
            let host = open(dir.path());
            for _ in 0..THREADS {
                let meta = meta();
                let thread = meta.id;
                host.create(meta).unwrap();
                let actions = (1..=TURNS).flat_map(|n| {
                    let id = format!("a{n}");
                    [turn(n), text(&id, n), append(&id, &answer)]
                });
                host.apply(thread, actions.collect());
            }
        }
        let bytes = walk(dir.path());
        let (mut times, mut firsts) = (Vec::new(), Vec::new());
        for _ in 0..5 {
            let started = std::time::Instant::now();
            let host = open(dir.path());
            times.push(started.elapsed());
            let thread = host.threads()[THREADS / 2];
            let started = std::time::Instant::now();
            assert_eq!(
                state(&host, thread).0.turns.len(),
                usize::try_from(TURNS).unwrap(),
                "read whole"
            );
            firsts.push(started.elapsed());
            drop(host);
        }
        times.sort_unstable();
        firsts.sort_unstable();
        eprintln!(
            "host_open_cost: {THREADS} threads x {TURNS} turns, {} MiB on disk, open median {} ms, \
             worst {} ms; a thread's first use median {} us, worst {} us",
            bytes >> 20,
            times[times.len() / 2].as_millis(),
            times[times.len() - 1].as_millis(),
            firsts[firsts.len() / 2].as_micros(),
            firsts[firsts.len() - 1].as_micros()
        );
    }

    /// A worker's start reads each thread's head alone: the table has every row as it was, and
    /// no log is opened until its thread is used. A look over every thread sees each by its
    /// head, items left on the disk, and a search reads the items without opening the log. A
    /// thread that changed after its head was written (it was working when the worker
    /// stopped) is read whole at the start and its head written, so the start after that reads
    /// the head alone; and a thread let go goes from the disk whether opened or not.
    #[test]
    fn a_start_reads_the_heads_and_opens_a_log_on_first_use() {
        use slopty_proto::thread::{Phase, Status};
        let dir = tempfile::tempdir().unwrap();
        let metas = [meta(), meta(), meta()];
        let [rests, works, gone] = metas.each_ref().map(|m| m.id);
        let before = {
            let host = open(dir.path());
            for meta in metas {
                host.create(meta).unwrap();
            }
            for thread in [rests, works, gone] {
                host.apply(thread, vec![turn(1), text("a1", 1), append("a1", "the answer")]);
            }
            let working = Status { phase: Phase::Working, ..Status::default() };
            host.apply(works, vec![Action::Status(working), turn(2), text("a2", 2)]);
            host.apply(works, vec![append("a2", "half an answer")]);
            [rests, works].map(|thread| state(&host, thread).0)
        };

        let host = open(dir.path());
        assert_eq!(host.opened(), 0, "no log is opened at the start");
        let TableFrame::Snapshot { rows, .. } = host.table(None) else { panic!("a snapshot") };
        assert_eq!(rows.len(), 3, "every thread in the table");
        let working = rows.iter().find(|r| r.id == works).expect("its row");
        assert_eq!(working.status.phase, Phase::Working, "as its log left it, read whole");
        let looked = host.visit(|s| Some((s.meta.id, s.turns.len(), s.items.len())));
        assert!(looked.contains(&(rests, 1, 0)), "by its head, the items left: {looked:?}");
        let said = host.visit_whole(|s| {
            s.items.iter().any(|i| i.id == ItemId("a2".to_owned())).then_some(s.meta.id)
        });
        assert_eq!(said, [works], "a search reads the items");
        assert_eq!(host.opened(), 0, "and opens nothing");
        assert_eq!(state(&host, rests).0, before[0], "a first use reads it whole");
        assert_eq!(state(&host, works).0, before[1]);
        assert_eq!(host.opened(), 2);
        host.remove(gone).unwrap();
        assert!(!dir.path().join(gone.to_string()).exists(), "let go unopened, gone from disk");
        drop(host);

        let host = open(dir.path());
        let TableFrame::Snapshot { rows, .. } = host.table(None) else { panic!("a snapshot") };
        assert_eq!(rows.len(), 2);
        assert_eq!(host.opened(), 0, "every head current now");
        assert_eq!(state(&host, works).0, before[1], "the working thread as it was");
    }

    /// The bytes of every file under `dir`.
    fn walk(dir: &Path) -> u64 {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() { walk(&entry.path()) } else { meta.len() }
            })
            .sum()
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

    /// A follower cut short while it gathers within its budget, as a stream's `select!` cuts
    /// it when a review or a page comes, keeps the batch it took: the next call sends it, though
    /// the thread says nothing more. It used to be lost until the thread's next change, so a
    /// prompt answered just then stayed open on the client.
    #[tokio::test(start_paused = true)]
    async fn a_follower_cut_short_while_it_gathers_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        host.apply(thread, vec![turn(1)]);
        let mut follower = Follower::new(host.clone(), thread, None, 10, 16);
        let mut client = Mirror::default();
        client.take(follower.next().await.unwrap());
        host.apply(thread, vec![text("a", 1), append("a", "the last word")]);
        let cut = std::time::Duration::from_millis(5);
        assert!(
            tokio::time::timeout(cut, follower.next()).await.is_err(),
            "cut short inside its 16 ms budget"
        );
        let quiet = std::time::Duration::from_secs(1);
        let frame = tokio::time::timeout(quiet, follower.next())
            .await
            .expect("the batch it took goes, with nothing more said");
        client.take(frame.unwrap());
        assert_eq!(client.state, Some(state(&host, thread).0));
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
    /// from the agent's session, after a worker restart too. A message that was being sent
    /// waits to go again: its delivery is named for it, so ptyd types it only if it never took
    /// it.
    #[tokio::test]
    async fn what_the_worker_owns_outlives_a_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta.clone()).unwrap();
        let (sent, waiting, sending) = (IntentId::new(), IntentId::new(), IntentId::new());
        host.typed(thread, sent, "hello there", "hello there");
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
        assert_eq!(
            states,
            [(waiting, PendingState::Waiting), (sending, PendingState::Waiting)],
            "the one being sent goes again, under its own delivery"
        );
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

    /// The seat an agent's thread was started at outlives the agent's own account of the
    /// thread, a read again and a restart: the row keeps naming it and its parent, and the whole
    /// seat comes back from the thread's directory. What the worker adds for the seat (its token
    /// among it) is never kept, only given where the agent runs.
    #[tokio::test]
    async fn a_seat_outlives_a_read_again_and_a_restart() {
        use slopty_proto::agent::PARENT_FACT;
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let started = meta();
        let thread = started.id;
        host.create(started.clone()).unwrap();
        let seat = slopty_core::SessionId::new();
        let parent = ThreadId::new();
        let seated = Seated {
            seat,
            facts: vec![(PARENT_FACT.to_owned(), parent.to_string())],
            system: Some("You review.".to_owned()),
            relay: Some("/opt/slopty/bin/slopty".to_owned()),
        };
        host.seated(thread, &seated);
        let kept = |host: &Host| {
            let (state, _) = state(host, thread);
            let fact = state.meta.facts.get(slopty_proto::agent::SEAT_FACT);
            assert_eq!(fact, Some(&seat.to_string()));
            assert_eq!(state.meta.facts.get(PARENT_FACT), Some(&parent.to_string()));
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
        assert!(host.env_of(&seated).is_empty(), "nothing until the daemon says");
        host.set_seat_env(std::sync::Arc::new(|seat| {
            vec![("SLOPTY_SESSION_TOKEN".to_owned(), format!("token-for-{seat}"))]
        }));
        let env = host.env_of(&seated);
        assert_eq!(env, [("SLOPTY_SESSION_TOKEN".to_owned(), format!("token-for-{seat}"))]);
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

    /// A start's first message an earlier worker left on its way is never left "being sent"
    /// for good: the next worker takes it off the list when the thread shows the agent took
    /// it, and else holds it, saying it may have gone, so the person can withdraw it or send it
    /// again. A message the composer was typing is left to go again on its own delivery.
    #[test]
    fn a_starts_first_message_cut_off_by_a_restart_is_settled() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let sending = |intent, text: &str| Pending {
            intent,
            text: text.to_owned(),
            attachments: vec![],
            delivery: Delivery::Queue,
            state: PendingState::Sending,
        };
        let begin = |intent| {
            let meta = meta();
            let thread = meta.id;
            host.create(meta).unwrap();
            host.record_start(intent, Outcome::Started { thread });
            host.first_message(thread, sending(intent, "hello"));
            thread
        };
        let (took, lost, typing) = (IntentId::new(), IntentId::new(), IntentId::new());
        let taken = begin(took);
        let prompt = Item {
            id: ItemId("p1".to_owned()),
            turn: TurnId(1),
            at_ms: WallMs::ZERO,
            body: ItemBody::User(UserMessage {
                text: Clipped::whole("hello"),
                images: Vec::new(),
                command: None,
                intent: Some(took),
            }),
        };
        host.apply(taken, vec![turn(1), Action::ItemStarted(prompt)]);
        let left = begin(lost);
        let both = vec![sending(lost, "hello"), sending(typing, "next")];
        host.apply(left, vec![Action::PendingSet(both)]);
        drop(host);

        let host = open(dir.path());
        assert_eq!(pending_of(&host, taken), [], "taken: gone from the list");
        let cut = PendingState::Held { reason: schedule::CUT_OFF.to_owned() };
        assert_eq!(
            pending_of(&host, left),
            [
                ("hello".to_owned(), Delivery::Queue, cut),
                ("next".to_owned(), Delivery::Queue, PendingState::Sending),
            ],
            "the first held; the composer's left to go again"
        );
        // The agent's transcript read again shows it, knowing nothing of the intent: it is
        // the held one's, which leaves the list.
        host.reset(left, ThreadState::new(state(&host, left).0.meta)).unwrap();
        host.apply(left, vec![turn(1), self::prompt("p1", 1, "hello")]);
        assert_eq!(intent_of(&state(&host, left).0, "p1"), Some(lost), "the item is the intent's");
        assert_eq!(
            pending_of(&host, left),
            [("next".to_owned(), Delivery::Queue, PendingState::Waiting)],
            "taken: gone from the list"
        );
    }

    /// A thread whose log no longer reads (an older build wrote it) goes with its directory, so
    /// it is not read and warned of at every start; what is not a thread's stays.
    #[test]
    fn a_log_that_no_longer_reads_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        drop(host);
        let kept = dir.path().join(thread.to_string());
        let _no_head = std::fs::remove_file(kept.join("head"));
        std::fs::write(kept.join("snapshot"), [0xff_u8; 64]).unwrap();

        let host = open(dir.path());
        assert!(!host.holds(thread), "not held");
        assert!(!kept.exists(), "its directory went");
        assert!(dir.path().join("starts").is_file(), "the starts stay");
    }

    /// The call a thread hangs off is the thread's for good: an adapter that tells its metadata
    /// again, or reads it again from nothing, before it knows the parent (a subagent followed
    /// before the thread that started it, after a restart) does not unlink it.
    #[test]
    fn a_threads_parent_outlives_an_adapter_that_does_not_know_it() {
        use slopty_proto::thread::Link;
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let link = Link { thread: ThreadId::new(), item: ItemId("call".to_owned()) };
        let mut child = meta();
        child.parent = Some(link.clone());
        child.origin = ThreadMeta::SUBAGENT.to_owned();
        let thread = child.id;
        host.create(child.clone()).unwrap();
        let unknown = ThreadMeta { parent: None, title: "Renamed".to_owned(), ..child };
        host.apply(thread, vec![Action::Meta(Box::new(unknown.clone()))]);
        let (state, _) = state(&host, thread);
        assert_eq!((state.meta.title.as_str(), state.meta.parent), ("Renamed", Some(link.clone())));
        drop(host);

        let host = open(dir.path());
        host.reset(thread, ThreadState::new(unknown)).unwrap();
        let (again, _) = self::state(&host, thread);
        assert_eq!(again.meta.parent, Some(link), "read again from nothing");
    }

    /// The agent's copy of a message typed into its terminal is the message's where it is not
    /// what was typed word for word: the head of a long one, which Claude Code keeps clipped; a
    /// command, which it keeps as its name and arguments; and one sent with files, whose paths
    /// were typed after the words, of which Claude Code kept a picture as a placeholder, or as a
    /// picture apart. Another message's copy is not.
    #[test]
    fn a_typed_message_is_known_in_the_agents_copy() {
        use slopty_proto::thread::ContentRef;
        use slopty_proto::thread::detail::Clip;
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta).unwrap();
        let user = |id: &str, text: Clipped, command: Option<&str>| {
            Action::ItemStarted(Item {
                id: ItemId(id.to_owned()),
                turn: TurnId(1),
                at_ms: WallMs::ZERO,
                body: ItemBody::User(UserMessage {
                    text,
                    images: vec![],
                    command: command.map(str::to_owned),
                    intent: None,
                }),
            })
        };
        let long = (0..2_000).fold(String::new(), |mut long, n| {
            std::fmt::Write::write_fmt(&mut long, format_args!("line {n} of a long paste\n"))
                .unwrap();
            long
        });
        assert!(long.chars().count() > 32_000, "longer than an agent keeps");
        let prose = Clip { lines: 400, chars: 32_000 };
        let head = Clipped::head(&long, prose, Some(ContentRef("p1".to_owned())));
        assert!(head.is_clipped());
        let paths = ["/drop/a/shot.png", "/drop/b/notes.txt"];
        let with_files = slopty_agent::attach::with_paths("Look at this", paths);
        let (cut, command, picture, apart, other) =
            (IntentId::new(), IntentId::new(), IntentId::new(), IntentId::new(), IntentId::new());
        host.typed(thread, cut, &long, &long);
        host.typed(thread, command, "/model opus", "/model opus");
        host.typed(thread, picture, "Look at this", &with_files);
        host.typed(thread, apart, "Look at this", &with_files);
        host.typed(thread, other, "Something else", "Something else");
        host.apply(
            thread,
            vec![
                turn(1),
                user("p1", head, None),
                user("p2", Clipped::whole("opus"), Some("/model")),
                user("p3", Clipped::whole("Look at this [Image #1] /drop/b/notes.txt"), None),
                user("p4", Clipped::whole("Look at this"), None),
                user("p5", Clipped::whole("Look at that"), None),
            ],
        );
        let (state, _) = state(&host, thread);
        let intents: Vec<Option<IntentId>> =
            ["p1", "p2", "p3", "p4", "p5"].iter().map(|id| intent_of(&state, id)).collect();
        assert_eq!(intents, [Some(cut), Some(command), Some(picture), Some(apart), None]);
    }

    /// A message the person schedules waits in the thread's pending list on the worker, once
    /// per intent, whatever list the agent's adapter tells, and outlives a restart; it is taken
    /// back or changed there, and one being sent as the worker stopped comes back held, never
    /// sent twice. A message cannot be scheduled on a thread that cannot take it.
    #[tokio::test]
    async fn a_scheduled_message_waits_on_the_worker_and_outlives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let mut plan = meta();
        plan.caps.push(Cap::named(Cap::SCHEDULE));
        let thread = plan.id;
        host.create(plan.clone()).unwrap();
        let at = Delivery::At { at_ms: WallMs::from_millis(u64::MAX) };
        let after = at;
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

        let edit = Intent::Edit {
            pending: second,
            text: "Review it all".to_owned(),
            attachments: Vec::new(),
        };
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
        // One whose moment has not come waits for it.
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

    /// A start is kept from when it is taken up until its outcome is recorded, across a
    /// restart: as asked, then marked once its agent opens, and gone with its outcome, which a
    /// repeat of the id gets back. One already answered is not kept again.
    #[test]
    fn a_start_under_way_is_kept_until_its_outcome() {
        use slopty_proto::thread::wire::{NewWorktree, Start};
        use slopty_worker::thread::host::UnderWay;
        let dir = tempfile::tempdir().unwrap();
        let start = Start {
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            cwd: "/work".to_owned(),
            drive: None,
            prompt: Some("Fix the build".to_owned()),
            model: None,
            mode: None,
            effort: None,
            attachments: Vec::new(),
            args: Vec::new(),
            worktree: Some(NewWorktree::named("fix")),
        };
        let (id, other) = (IntentId::new(), IntentId::new());
        open(dir.path()).start_under_way(id, &start);
        let kept = |begun| vec![UnderWay { id, start: start.clone(), begun }];
        assert_eq!(open(dir.path()).starts_under_way(), kept(false), "across a restart");

        let host = open(dir.path());
        host.start_under_way(id, &start);
        assert_eq!(host.starts_under_way(), kept(false), "once");
        host.start_begins(id);
        assert_eq!(open(dir.path()).starts_under_way(), kept(true), "its agent opening");

        let host = open(dir.path());
        let thread = ThreadId::new();
        assert_eq!(host.record_start(id, Outcome::Started { thread }), Outcome::Started { thread });
        assert!(host.starts_under_way().is_empty(), "settled");
        host.start_under_way(id, &start);
        assert!(open(dir.path()).starts_under_way().is_empty(), "an answered start is not kept");
        assert_eq!(host.started(id), Some(Outcome::Started { thread }));
        host.start_under_way(other, &start);
        let refused = Outcome::Refused { reason: "no".to_owned() };
        assert_eq!(host.record_start(other, refused.clone()), refused);
        assert!(open(dir.path()).starts_under_way().is_empty(), "a refusal settles it too");
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
