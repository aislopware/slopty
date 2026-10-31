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
        Action, AgentId, Changed, Clipped, Cursor, Drive, IntentId, Item, ItemBody, ItemId,
        PartKey, TableState, ThreadId, ThreadMeta, ThreadState, Turn, TurnId, TurnState, Usage,
    };
    use slopty_worker::thread::log::Limits;
    use slopty_worker::thread::{Follower, Host, Seated};

    fn meta() -> ThreadMeta {
        ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: "2.1.286".to_owned(),
            native: "s1".to_owned(),
            cwd: "/work".to_owned(),
            title: "Fix the build".to_owned(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::DRIVEN),
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
            mode: None,
            context: None,
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
            let started = tokio::time::Instant::now();
            let host = open(dir.path());
            times.push(started.elapsed());
            let thread = host.threads()[THREADS / 2];
            let started = tokio::time::Instant::now();
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
                (Outcome::Done, vec![turn(1)])
            });
            assert_eq!(outcome, Some(Outcome::Done));
        }
        assert_eq!((acted, state(&host, thread).0.turns.len()), (1, 1));
        drop(host);
        let host = open(dir.path());
        let again = host.intent(thread, send, |_| panic!("acted on twice"));
        assert_eq!(again, Some(Outcome::Done));
        assert_eq!(host.start(id, || panic!("started twice")), Outcome::Started { thread });
    }

    /// The person's own marks on a thread (how far they have seen it, their name for it)
    /// outlive the thread being read again from the agent's session, after a worker restart
    /// too; what the agent's TUI held (its prompt box, its queue) does not, as its reader tells
    /// it again.
    #[tokio::test]
    async fn the_persons_marks_outlive_a_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let host = open(dir.path());
        let meta = meta();
        let thread = meta.id;
        host.create(meta.clone()).unwrap();
        let draft = slopty_proto::thread::Draft {
            text: "and then".to_owned(),
            images: Vec::new(),
            at_ms: WallMs::ZERO,
        };
        host.apply(
            thread,
            vec![
                turn(1),
                Action::Seen(TurnId(1)),
                Action::NameSet("Greetings".to_owned()),
                Action::DraftSet(Some(draft)),
                Action::QueuedSet(vec!["next".to_owned()]),
            ],
        );
        drop(host);

        let host = open(dir.path());
        host.reset(thread, ThreadState::new(meta)).unwrap();
        host.apply(thread, vec![turn(1)]);
        let (again, _) = state(&host, thread);
        assert_eq!(again.named, "Greetings", "the person's name for it stays");
        assert_eq!(again.seen, TurnId(1), "and how far they saw it");
        assert_eq!(
            (again.draft, again.queued),
            (None, Vec::<String>::new()),
            "the TUI's are its own"
        );
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
        };
        host.seated(thread, &seated);
        let kept = |host: &Host| {
            let (state, _) = state(host, thread);
            let fact = state.meta.facts.get(slopty_proto::agent::SEAT_FACT);
            assert_eq!(fact, Some(&seat.to_string()));
            assert_eq!(state.meta.facts.get(PARENT_FACT), Some(&parent.to_string()));
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
        let file =
            std::fs::read_to_string(dir.path().join(thread.to_string()).join("seat.json")).unwrap();
        assert!(!file.contains("token"), "no token is kept: {file}");
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
}

/// Claude Code's sessions as the observer follows them, from what a real Claude Code wrote
/// (`slopty-agent`'s `tests/fixtures`: each scenario's transcript, the hook payloads it ran
/// beside it and the order they came in; what Slopty's mod relayed of its prompt box).
///
/// The status a session settles on, and its turns, are the same whether each hook comes once or
/// twice, and whether its transcript is on disk before the hooks or only after them. A turn's
/// state is not held: `Stop` ends the turn open when it is heard, so with the whole transcript
/// read first it ends the last one (`compact`'s `/compact`), and with none read it ends none.
/// Replayed in the order it was recorded, a session shows each moment as it came, heard by its
/// hooks or by Claude Code's record of itself alone; a thread ends once its terminal no longer
/// runs its agent; the prompt box shows as the mod relays it.
#[cfg(test)]
mod hooks {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::Value;
    use slopty_core::{SessionId, WallMs};
    use slopty_proto::thread::{Status, ThreadId, ThreadState, TurnId, TurnState};
    use slopty_worker::thread::Host;
    use slopty_worker::thread::claude::{Claude, Foreground, Place, is_claude};
    use slopty_worker::thread::log::Limits;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../slopty-agent/tests/fixtures/conversation")
    }

    fn lines(path: &Path) -> Vec<Value> {
        let text = std::fs::read_to_string(path).expect("a fixture");
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// How the hooks reach the worker.
    #[derive(Clone, Copy, Debug)]
    struct Arrival {
        /// Each hook comes twice, at the same instant, as a relay run again would bring it.
        twice: bool,
        /// The transcript is written only once every hook was heard.
        transcript_after: bool,
    }

    /// A turn as it settled: which, how it ended, and when.
    type Settled = (TurnId, TurnState, Option<WallMs>);

    /// What the thread of scenario `name` settles on when its hooks arrive as `arrival` says:
    /// its status, and its turns.
    async fn settled(name: &str, arrival: Arrival) -> (Status, Vec<Settled>) {
        let scenario = fixtures().join(name);
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let place = Place { home: home.clone(), path: None, data: dir.path().join("data") };
        let (claude, watching) = Claude::spawn(host.clone(), place);
        let records = lines(&scenario.join("transcript.jsonl"));
        let session = records
            .iter()
            .find_map(|r| r.get("sessionId").and_then(Value::as_str))
            .expect("a session id")
            .to_owned();
        let main = slopty_agent::claude::sessions::transcript(&home, Path::new("/work"), &session);
        let plant = || {
            std::fs::create_dir_all(main.parent().unwrap()).unwrap();
            std::fs::copy(scenario.join("transcript.jsonl"), &main).unwrap();
            let agents = scenario.join("subagents");
            if agents.is_dir() {
                let into = slopty_agent::conversation::subagents_dir(&main);
                std::fs::create_dir_all(&into).unwrap();
                for agent in std::fs::read_dir(&agents).unwrap() {
                    let agent = agent.unwrap().path();
                    std::fs::copy(&agent, into.join(agent.file_name().unwrap())).unwrap();
                }
            }
        };
        if !arrival.transcript_after {
            plant();
        }
        let terminal = SessionId::new();
        let hooks = lines(&scenario.join("hooks.jsonl"));
        // The session's end would leave every status the same; what is held is the live one.
        let live = hooks.iter().filter(|h| h["input"]["hook_event_name"] != "SessionEnd");
        for (n, hook) in (0_u64..).zip(live) {
            let mut payload = hook["input"].clone();
            payload["transcript_path"] = Value::from(main.to_string_lossy().into_owned());
            let at_us = 1_800_000_000_000_000_u64.saturating_add(n.saturating_mul(1_000));
            for _ in 0..if arrival.twice { 2 } else { 1 } {
                claude.hook(terminal, at_us, payload.to_string());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if arrival.transcript_after {
            tokio::time::sleep(Duration::from_secs(2)).await;
            plant();
        }
        // The follower looks at its files every second as well as on each event.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let thread = slopty_agent::claude::thread_of(&session);
        let (state, _cursor): (ThreadState, _) = host.state(thread).expect("the thread");
        drop(claude);
        watching.abort();
        let turns = state.turns.iter().map(|t| (t.id, t.state.clone(), t.ended_ms)).collect();
        (state.status, turns)
    }

    #[tokio::test(start_paused = true)]
    async fn the_hooks_settle_on_one_status_however_they_arrive() {
        let mut names: Vec<String> = std::fs::read_dir(fixtures())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert!(names.len() >= 9, "{names:?}");
        for name in &names {
            let once = Arrival { twice: false, transcript_after: false };
            let base = settled(name, once).await;
            for arrival in [
                Arrival { twice: true, transcript_after: false },
                Arrival { twice: false, transcript_after: true },
                Arrival { twice: true, transcript_after: true },
            ] {
                assert_eq!(settled(name, arrival).await, base, "{name}: {arrival:?}");
            }
        }
    }

    /// The moment RFC 3339 UTC stamp `stamp` (`2026-10-30T07:38:52.606Z`) names, in ms since the
    /// epoch.
    #[expect(clippy::arithmetic_side_effects, reason = "a recording's stamps, far inside u64")]
    fn ms_of(stamp: &str) -> u64 {
        let number = |part: &str| part.parse::<u64>().unwrap();
        let parts = |text: &str, by: char| {
            <[u64; 3]>::try_from(text.split(by).map(number).collect::<Vec<_>>()).unwrap()
        };
        let (date, time) = stamp.strip_suffix('Z').unwrap().split_once('T').unwrap();
        let [year, month, day] = parts(date, '-');
        // Days from the civil date (Howard Hinnant's `days_from_civil`), the year from March.
        let (year, month) = if month <= 2 { (year - 1, month + 9) } else { (year, month - 3) };
        let days = 365 * year + year / 4 - year / 100 + year / 400 + (153 * month + 2) / 5 + day
            - 1
            - 719_468;
        let (clock, millis) = time.split_once('.').unwrap_or((time, "0"));
        let [hour, minute, second] = parts(clock, ':');
        ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + number(millis)
    }

    /// How many lines arrival `step` wrote, and when, in ms into the recording.
    fn step_of(step: &Value) -> (usize, u64) {
        let lines = usize::try_from(step["lines"].as_u64().unwrap()).unwrap();
        (lines, step["ms"].as_u64().unwrap())
    }

    /// When the recording of `scenario` began, in ms since the epoch: as late as its transcript
    /// allows, each line stamped no later than it was seen written (`steps`, its arrivals).
    fn began(scenario: &Path, steps: &[Value]) -> u64 {
        let text = std::fs::read_to_string(scenario.join("transcript.jsonl")).unwrap();
        let mut lines = text.lines();
        let mut began = 0;
        for step in steps.iter().filter(|s| s["file"] == "transcript.jsonl") {
            let (count, ms) = step_of(step);
            for line in lines.by_ref().take(count) {
                let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
                if let Some(stamp) = record["timestamp"].as_str() {
                    began = began.max(ms_of(stamp).saturating_sub(ms));
                }
            }
        }
        began
    }

    /// A recording's moments, from its start (`base`, ms since the epoch) to its end (`span`
    /// later): a time shown after the start, `before` it, or `now` for the test's own clock.
    #[derive(Clone, Copy)]
    struct Recorded {
        base: u64,
        span: u64,
    }

    impl Recorded {
        fn at(self, ms: WallMs) -> String {
            match ms.as_millis().checked_sub(self.base) {
                None => "before".to_owned(),
                Some(after) if after > self.span => "now".to_owned(),
                Some(after) => format!("+{after}"),
            }
        }

        /// What thread `thread` shows: its phase, what it waits for and since when, each turn's
        /// state and end, and its title.
        fn shown(self, host: &Host, thread: ThreadId) -> String {
            let Some((state, _)) = host.state(thread) else { return "none".to_owned() };
            let status = &state.status;
            let wait =
                status.wait.as_ref().map_or(String::new(), |w| format!(" {}: {}", w.kind, w.text));
            let turns: Vec<String> = state
                .turns
                .iter()
                .map(|t| {
                    let ended = t.ended_ms.map_or(String::new(), |e| format!(" {}", self.at(e)));
                    format!("{:?}{ended}", t.state)
                })
                .collect();
            let (phase, liveness, since) =
                (status.phase, status.liveness, self.at(status.since_ms));
            format!(
                "{phase:?} {liveness:?}{wait} {since} | {} | {}",
                turns.join(", "),
                state.meta.title
            )
        }
    }

    /// How a recording's session is heard.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Heard {
        /// By its hooks as they came, beside the record Claude Code keeps of itself.
        Hooks,
        /// By the record alone, as a session typed by hand that runs no hook.
        Record,
    }

    /// What Claude Code's record of itself says while hook `event` is run: at work, waiting on
    /// the person at a dialog, or at rest; `None` for its end, which leaves the record as it was.
    /// A dialog is answered by the next hook, or by its call's result where none comes first (a
    /// refusal).
    fn record_status(event: &str) -> Option<&'static str> {
        match event {
            "SessionStart" | "Stop" => Some("idle"),
            "PermissionRequest" => Some("waiting"),
            "SessionEnd" => None,
            _ => Some("busy"),
        }
    }

    /// No process has this id (macOS's run to 99 999): the record of a Claude Code that is gone.
    const GONE: u32 = i32::MAX.unsigned_abs();

    /// A recorded session replayed under `home`, its recording begun at `base`.
    struct Replay {
        name: String,
        scenario: PathBuf,
        home: PathBuf,
        session: String,
        main: PathBuf,
        base: u64,
    }

    impl Replay {
        fn new(name: &str, home: &Path) -> Self {
            let scenario = fixtures().join(name);
            let records = lines(&scenario.join("transcript.jsonl"));
            let session = records
                .iter()
                .find_map(|r| r.get("sessionId").and_then(Value::as_str))
                .expect("a session id")
                .to_owned();
            let main =
                slopty_agent::claude::sessions::transcript(home, Path::new("/work"), &session);
            let base = began(&scenario, &lines(&scenario.join("arrivals.jsonl")));
            let home = home.to_path_buf();
            Self { name: name.to_owned(), scenario, home, session, main, base }
        }

        /// Claude Code's record of itself (`~/.claude/sessions/<pid>.json`) says `status` since
        /// `ms` into the recording.
        fn says(&self, status: &str, ms: u64) {
            use slopty_agent::claude::sessions;
            let live = serde_json::json!({
                "pid": GONE, "sessionId": self.session, "cwd": "/work", "kind": "interactive",
                "startedAt": self.base, "version": "2.1.314", "name": self.name,
                "status": status, "waitingFor": (status == "waiting").then_some("permission prompt"),
                "statusUpdatedAt": self.base.saturating_add(ms),
            });
            let dir = sessions::config_dir(&self.home).join(sessions::LIVE_DIR);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{GONE}.json")), live.to_string()).unwrap();
        }

        /// Hook `hook` as the relay brings it, at `ms` into the recording.
        fn hook(&self, claude: &Claude, terminal: SessionId, hook: &Value, ms: u64) {
            let mut payload = hook["input"].clone();
            payload["transcript_path"] = Value::from(self.main.to_string_lossy().into_owned());
            let at_us = self.base.saturating_add(ms).saturating_mul(1_000);
            claude.hook(terminal, at_us, payload.to_string());
        }

        /// `lines` written at the end of recorded file `file`: the transcript, or a subagent's.
        fn write(&self, file: &str, lines: &[&str]) {
            let into = match file.strip_prefix("subagents/") {
                Some(agent) => slopty_agent::conversation::subagents_dir(&self.main).join(agent),
                None => self.main.clone(),
            };
            std::fs::create_dir_all(into.parent().unwrap()).unwrap();
            let mut out =
                std::fs::OpenOptions::new().create(true).append(true).open(&into).unwrap();
            std::io::Write::write_all(&mut out, lines.concat().as_bytes()).unwrap();
        }
    }

    /// Scenario `name` replayed in the order Claude Code made it (`arrivals.jsonl`), heard as
    /// `heard` says: each hook as it came, each file's lines as they were written, Claude Code's
    /// record of itself saying what each hook says, and a tick after each step, as the daemon's,
    /// every step read before the next. Claude Code is in the foreground until its end's step,
    /// and gone after. What the thread shows after each step, a moment shown once however many
    /// steps leave it.
    async fn replayed(name: &str, heard: Heard) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let replay = Replay::new(name, &dir.path().join("home"));
        let place = Place { home: replay.home.clone(), path: None, data: dir.path().join("data") };
        let (claude, watching) = Claude::spawn(host.clone(), place);
        let thread = slopty_agent::claude::thread_of(&replay.session);
        let hooks = lines(&replay.scenario.join("hooks.jsonl"));
        let steps = lines(&replay.scenario.join("arrivals.jsonl"));
        let last = steps.last().map_or(0, |s| step_of(s).1);
        let recorded = Recorded { base: replay.base, span: last.saturating_add(60_000) };
        let terminal = SessionId::new();
        let there =
            || vec![(terminal, Foreground::Claude { pid: GONE, cwd: Some("/work".into()) })];
        let mut written: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut waiting = false;
        let mut moments: Vec<String> = Vec::new();
        for step in &steps {
            let file = step["file"].as_str().unwrap();
            let (count, ms) = step_of(step);
            let from = written.get(file).copied().unwrap_or(0);
            written.insert(file.to_owned(), from.saturating_add(count));
            if file == "hooks.jsonl" {
                for hook in hooks.iter().skip(from).take(count) {
                    let event = hook["input"]["hook_event_name"].as_str().unwrap();
                    if let Some(status) = record_status(event) {
                        replay.says(status, ms);
                        waiting = status == "waiting";
                    }
                    if heard == Heard::Hooks {
                        replay.hook(&claude, terminal, hook, ms);
                    }
                }
            } else {
                let text = std::fs::read_to_string(replay.scenario.join(file)).unwrap();
                let mut gained: Vec<&str> =
                    text.split_inclusive('\n').skip(from).take(count).collect();
                let answer = gained.iter().position(|l| l.contains("\"tool_result\""));
                let answered = answer.filter(|_| waiting).map(|at| gained.split_off(at));
                replay.write(file, &gained);
                if let Some(rest) = answered {
                    replay.says("busy", ms);
                    waiting = false;
                    replay.write(file, &rest);
                }
            }
            claude.tick(there());
            // Past the follower's look at its files, so this step is read before the next.
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            keep(&mut moments, recorded.shown(&host, thread));
        }
        if heard == Heard::Hooks {
            // A hook relayed late, after the end: one older than the last heard is passed over.
            let prompt = hooks.iter().find(|h| h["input"]["hook_event_name"] == "UserPromptSubmit");
            replay.hook(&claude, terminal, prompt.unwrap(), 0);
        }
        claude.tick(vec![(terminal, Foreground::Other)]);
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        keep(&mut moments, recorded.shown(&host, thread));
        drop(claude);
        watching.abort();
        moments
    }

    /// `now` kept after `moments`, unless the last is the same.
    fn keep(moments: &mut Vec<String>, now: String) {
        if moments.last() != Some(&now) {
            moments.push(now);
        }
    }

    /// The recorded sessions by name, sorted.
    fn scenarios() -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(fixtures())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Each moment a recorded session shows as it is replayed, heard by its hooks or by its
    /// record alone: a dialog said of the call it is about (by the record once the transcript
    /// wrote the call, at the dialog's time), a turn its `Stop` ends and its records then
    /// stamp, an interruption the transcript tells, a compaction shown while it runs (since it
    /// began, whatever the phase beneath it says meanwhile) with the session begun again after
    /// it, the end at its hook's moment or once its process is gone, and a hook relayed late
    /// passed over. Each moment names the event its time is that of, a hook's arrival or a
    /// record's stamp or write, and [`Times`] reads when that was from the recording's own files,
    /// so a recapture of the same session moves no expectation. Every recording's moments are
    /// compared at once.
    #[tokio::test(start_paused = true)]
    async fn each_recorded_session_shows_each_moment_as_it_came() {
        const PROMPT: &str = "UserPromptSubmit";
        const ASKED: &str = "PermissionRequest";
        const BEFORE: &str = "PreToolUse";
        const AFTER: &str = "PostToolUse";
        // Claude Code's record of itself says each status as its hook comes (`record_status`),
        // so heard by the record alone a dialog is still said at its hook's moment.
        let p = Times::of("permission");
        let at =
            |what: &str, since: u64, turns: &str| format!("{what} +{since} | {turns} | permission");
        let asks = |what: &str| format!("NeedsYou Live permission: {what}");
        // The turn ends at its stop summary's stamp.
        let ended = format!("Complete +{}", p.stamp("system/stop_hook_summary", 1));
        let permission_hooks = vec![
            "Idle Live now |  | permission".to_owned(),
            at("Working Live", p.hook(PROMPT, 1), ""),
            at("Working Live", p.hook(PROMPT, 1), "Active"),
            at(&asks("Run touch refused.txt"), p.hook(ASKED, 1), "Active"),
            // The refused dialog is answered by the next call's hook.
            at("Working Live", p.hook(BEFORE, 2), "Active"),
            at(&asks("Run touch allowed.txt"), p.hook(ASKED, 2), "Active"),
            at("Working Live", p.hook(AFTER, 1), "Active"),
            at(&asks("Edit /work/notes.txt"), p.hook(ASKED, 3), "Active"),
            at("Working Live", p.hook(AFTER, 3), "Active"),
            at(&asks("Write /work/todo.txt"), p.hook(ASKED, 4), "Active"),
            at("Working Live", p.hook(AFTER, 4), "Active"),
            at("Working Live", p.hook(AFTER, 4), &ended),
            at("Done Live", p.hook("Stop", 1), &ended),
            at("Done Exited { resumable: true }", p.hook("SessionEnd", 1), &ended),
        ];
        let permission_record = vec![
            "Idle Live now |  | permission".to_owned(),
            at("Working Live", p.stamp("user", 1), "Active"),
            at(&asks("Wants permission"), p.hook(ASKED, 1), "Active"),
            // The refused dialog is answered by its call's result, as it is written.
            at("Working Live", p.written("user:tool_result", 1), "Active"),
            at(&asks("Wants permission"), p.hook(ASKED, 2), "Active"),
            at(&asks("Run touch allowed.txt"), p.hook(ASKED, 2), "Active"),
            at("Working Live", p.hook(AFTER, 1), "Active"),
            at(&asks("Wants permission"), p.hook(ASKED, 3), "Active"),
            at("Working Live", p.hook(AFTER, 3), "Active"),
            at(&asks("Wants permission"), p.hook(ASKED, 4), "Active"),
            at("Working Live", p.hook(AFTER, 4), "Active"),
            at("Done Live", p.stamp("system/stop_hook_summary", 1), &ended),
            format!("Done Exited {{ resumable: true }} now | {ended} | permission"),
        ];

        let i = Times::of("interrupt");
        let at =
            |what: &str, since: u64, turns: &str| format!("{what} +{since} | {turns} | interrupt");
        // The turn ends at the record that tells the interruption.
        let interrupted = format!("Interrupted +{}", i.stamp("user:text", 1));
        let interrupt_hooks = vec![
            "Idle Live now |  | interrupt".to_owned(),
            at("Working Live", i.hook(PROMPT, 1), ""),
            at("Working Live", i.hook(PROMPT, 1), "Active"),
            at("Stopped Live", i.stamp("user:text", 1), &interrupted),
            at("Stopped Exited { resumable: true }", i.hook("SessionEnd", 1), &interrupted),
        ];
        let interrupt_record = vec![
            "Idle Live now |  | interrupt".to_owned(),
            at("Working Live", i.stamp("user", 1), "Active"),
            at("Stopped Live", i.stamp("user:text", 1), &interrupted),
            format!("Stopped Exited {{ resumable: true }} now | {interrupted} | interrupt"),
        ];

        let c = Times::of("compact");
        let at =
            |what: &str, since: u64, turns: &str| format!("{what} +{since} | {turns} | compact");
        // The answer's turn ends at its stop summary's stamp, each command's at its output's.
        let answered = format!("Complete +{}", c.stamp("system/stop_hook_summary", 1));
        let renamed = format!("{answered}, Complete +{}", c.stamp("system/local_command", 1));
        let compacted = format!("{renamed}, Complete +{}", c.stamp("user", LAST));
        let compact_hooks = vec![
            "Idle Live now |  | compact".to_owned(),
            at("Working Live", c.hook(PROMPT, 1), ""),
            at("Working Live", c.hook(PROMPT, 1), "Active"),
            at("Working Live", c.hook(PROMPT, 1), &answered),
            at("Done Live", c.hook("Stop", 1), &answered),
            at("Done Live", c.hook("Stop", 1), &renamed),
            at(
                "Working Live compacting: Compacting the conversation",
                c.hook("PreCompact", 1),
                &renamed,
            ),
            // Begun again after the compaction.
            at("Idle Live", c.hook("SessionStart", 2), &renamed),
            at("Idle Live", c.hook("SessionStart", 2), &compacted),
            at("Idle Exited { resumable: true }", c.hook("SessionEnd", 1), &compacted),
        ];

        let pinned = [
            ("permission", Heard::Hooks, permission_hooks),
            ("permission", Heard::Record, permission_record),
            ("interrupt", Heard::Hooks, interrupt_hooks),
            ("interrupt", Heard::Record, interrupt_record),
            ("compact", Heard::Hooks, compact_hooks),
        ];
        let mut shown = Vec::new();
        for (name, heard, _) in &pinned {
            shown.push((*name, *heard, replayed(name, *heard).await));
        }
        assert_eq!(shown, pinned, "each recording's moments, heard each way");
    }

    /// Which of a kind of record [`Times`] reads: the last.
    const LAST: usize = 0;

    /// When a recording's events came, in ms into it (as [`Recorded`] shows a time), read from
    /// its own files alone: each hook when it arrived, each record of the main transcript by its
    /// own stamp and when it was written.
    struct Times {
        /// Each hook's event and when it came, in order.
        hooks: Vec<(String, u64)>,
        /// Each record's kind ([`kind`]), its stamp (none before the recording began), and when
        /// it was written, in order.
        records: Vec<(String, Option<u64>, u64)>,
    }

    impl Times {
        /// The times of recording `name`, its start as [`Replay`] takes it.
        fn of(name: &str) -> Self {
            let scenario = fixtures().join(name);
            let steps = lines(&scenario.join("arrivals.jsonl"));
            let base = began(&scenario, &steps);
            let hooks = lines(&scenario.join("hooks.jsonl"));
            let text = std::fs::read_to_string(scenario.join("transcript.jsonl")).unwrap();
            let (mut unread_hooks, mut unread_lines) = (hooks.iter(), text.lines());
            let mut times = Self { hooks: Vec::new(), records: Vec::new() };
            for step in &steps {
                let (count, ms) = step_of(step);
                if step["file"] == "hooks.jsonl" {
                    for hook in unread_hooks.by_ref().take(count) {
                        let event = hook["input"]["hook_event_name"].as_str().unwrap();
                        times.hooks.push((event.to_owned(), ms));
                    }
                } else if step["file"] == "transcript.jsonl" {
                    for line in unread_lines.by_ref().take(count) {
                        let Ok(record) = serde_json::from_str::<Value>(line) else { continue };
                        let stamp =
                            record["timestamp"].as_str().and_then(|at| ms_of(at).checked_sub(base));
                        times.records.push((kind(&record), stamp, ms));
                    }
                }
            }
            times
        }

        /// When the `n`th hook of `event` (from 1) came.
        fn hook(&self, event: &str, n: usize) -> u64 {
            let mut of = self.hooks.iter().filter(|(e, _)| e == event);
            let found = of.nth(n.checked_sub(1).expect("counted from 1"));
            found.unwrap_or_else(|| panic!("no hook {event} #{n}")).1
        }

        /// The `n`th record of `kind` (from 1, or [`LAST`]): its stamp, and when it was written.
        fn record(&self, kind: &str, n: usize) -> (Option<u64>, u64) {
            let mut of = self.records.iter().filter(|(k, ..)| k == kind);
            let found = match n.checked_sub(1) {
                Some(index) => of.nth(index),
                None => of.next_back(),
            };
            let (_, stamp, written) = found.unwrap_or_else(|| panic!("no record {kind} #{n}"));
            (*stamp, *written)
        }

        /// The stamp of the `n`th record of `kind`.
        fn stamp(&self, kind: &str, n: usize) -> u64 {
            self.record(kind, n).0.unwrap_or_else(|| panic!("{kind} #{n} stamped in the recording"))
        }

        /// When the `n`th record of `kind` was written.
        fn written(&self, kind: &str, n: usize) -> u64 {
            self.record(kind, n).1
        }
    }

    /// A record's kind: its type, `/` its subtype, `:` its content's first block's type
    /// (`system/stop_hook_summary`, `user:tool_result`; `user` for words typed in).
    fn kind(record: &Value) -> String {
        let mut kind = record["type"].as_str().unwrap_or_default().to_owned();
        if let Some(subtype) = record["subtype"].as_str() {
            kind = format!("{kind}/{subtype}");
        }
        if let Some(block) = record.pointer("/message/content/0/type").and_then(Value::as_str) {
            kind = format!("{kind}:{block}");
        }
        kind
    }

    /// However a recorded session is heard, by its hooks or by its record alone, its turns end
    /// the same, each moment bears the name its record gives it, and it ends exited and
    /// resumable in the phase it rested in; a rest that ends a turn is shown from that turn's
    /// end, by its hook's clock or its record's, never from an earlier turn's (a background
    /// task's notice is a turn of its own).
    #[tokio::test(start_paused = true)]
    async fn a_session_ends_the_same_heard_by_its_hooks_or_its_record() {
        let names = scenarios();
        assert!(names.len() >= 9, "{names:?}");
        let turns = |moments: &[String]| {
            moments.last().and_then(|m| m.split(" | ").nth(1)).map(str::to_owned)
        };
        for name in &names {
            let hooks = replayed(name, Heard::Hooks).await;
            let record = replayed(name, Heard::Record).await;
            assert_eq!(turns(&hooks), turns(&record), "{name}: {hooks:#?} {record:#?}");
            for moments in [&hooks, &record] {
                let named = format!(" | {name}");
                assert!(moments.iter().all(|m| m.ends_with(&named)), "{name}: {moments:#?}");
                let [.., rested, exited] = moments.as_slice() else { panic!("{moments:#?}") };
                let phase = |m: &str| m.split(' ').next().map(str::to_owned);
                assert_eq!(phase(rested), phase(exited), "{name}: {moments:#?}");
                assert!(rested.contains(" Live "), "{name}: {moments:#?}");
                let ms =
                    |at: Option<&str>| at.and_then(|at| at.strip_prefix('+')?.parse::<u64>().ok());
                let mut parts = rested.split(" | ");
                let (status, turns) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let ends = ["Done ", "Failed ", "Stopped "].iter().any(|p| status.starts_with(p));
                let ended: Vec<_> = turns.split(", ").map(|t| ms(t.split(' ').nth(1))).collect();
                if let (true, [.., Some(earlier), _]) = (ends, ended.as_slice()) {
                    let since = ms(status.rsplit(' ').next());
                    assert!(since.is_some_and(|since| since > *earlier), "{name}: {moments:#?}");
                }
                assert!(exited.contains(" Exited { resumable: true } "), "{name}: {moments:#?}");
            }
        }
    }

    /// A thread whose terminal no longer runs its agent ends, exited and resumable, its work
    /// stopped: one an earlier worker left naming a terminal once the worker has had its
    /// terminals back (four ticks), one Slopty began once its launcher has had time to start
    /// Claude Code (eight more), and another agent's row as soon as its terminal is gone. A
    /// session followed, a row, and a thread that names no terminal are left alone meanwhile.
    #[tokio::test(start_paused = true)]
    async fn a_thread_ends_once_its_terminal_no_longer_runs_its_agent() {
        use slopty_proto::thread::{Action, AgentId, Drive, Liveness, Phase, ThreadMeta};
        let dir = tempfile::tempdir().unwrap();
        let threads = dir.path().join("threads");
        let home = dir.path().join("home");
        let place = || Place { home: home.clone(), path: None, data: dir.path().join("data") };
        let earlier = Replay::new("permission", &home);
        {
            // An earlier worker heard a session begin, and stopped.
            let host = Host::open(&threads, Limits::default()).unwrap();
            let (claude, watching) = Claude::spawn(host.clone(), place());
            let began = &lines(&earlier.scenario.join("hooks.jsonl"))[0];
            earlier.hook(&claude, SessionId::new(), began, 0);
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            drop(claude);
            watching.await.unwrap();
            tokio::time::sleep(Duration::from_millis(1_100)).await;
        }
        let host = Host::open(&threads, Limits::default()).unwrap();
        let left = slopty_agent::claude::thread_of(&earlier.session);
        // Its TUI held a prompt queued, which goes with it.
        host.apply(left, vec![Action::QueuedSet(vec!["next".to_owned()])]).unwrap();
        let (claude, watching) = Claude::spawn(host.clone(), place());
        let launching = SessionId::new();
        let native = "00000000-0000-4000-8000-0000000000b9";
        claude.begun(launching, native.to_owned(), "/work".to_owned(), "2.1.314".to_owned());
        let begun = slopty_agent::claude::thread_of(native);
        // Another agent's threads: a row in a terminal of its own, and one naming none.
        let at_work = |terminal: Option<SessionId>| {
            let mut facts = std::collections::BTreeMap::new();
            if let Some(terminal) = terminal {
                facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), terminal.to_string());
            }
            let meta = ThreadMeta {
                id: ThreadId::new(),
                agent: AgentId::named(AgentId::CODEX),
                agent_version: "0.200.0".to_owned(),
                native: ThreadId::new().to_string(),
                cwd: "/work".to_owned(),
                title: "Elsewhere".to_owned(),
                parent: None,
                origin: ThreadMeta::PERSON.to_owned(),
                drive: Drive::named(Drive::OBSERVED),
                facts,
                created_ms: WallMs::from_millis(1),
            };
            let id = meta.id;
            host.create(meta).unwrap();
            let since_ms = WallMs::from_millis(2);
            let working =
                Status { phase: Phase::Working, wait: None, liveness: Liveness::Live, since_ms };
            host.apply(id, vec![Action::Status(working)]).unwrap();
            id
        };
        let row_terminal = SessionId::new();
        let row = at_work(Some(row_terminal));
        let loose = at_work(None);
        claude.row(row_terminal, row);
        let status = |thread| host.state(thread).map(|(s, _)| (s.status.phase, s.status.liveness));
        let live = |phase| Some((phase, Liveness::Live));
        let exited = |phase| Some((phase, Liveness::Exited { resumable: true }));
        // After so many ticks: the earlier worker's thread, the one Slopty began, and the row.
        let mut ticks = 0_u32;
        for (until, row_there, now) in [
            (3, true, [live(Phase::Idle), live(Phase::Idle), live(Phase::Working)]),
            (4, true, [exited(Phase::Idle), live(Phase::Idle), live(Phase::Working)]),
            (8, true, [exited(Phase::Idle), live(Phase::Idle), live(Phase::Working)]),
            (9, false, [exited(Phase::Idle), exited(Phase::Idle), exited(Phase::Stopped)]),
        ] {
            let seen = if row_there { vec![(row_terminal, Foreground::Other)] } else { vec![] };
            for _ in ticks..until {
                claude.tick(seen.clone());
                tokio::time::sleep(Duration::from_millis(1_100)).await;
            }
            ticks = until;
            assert_eq!([status(left), status(begun), status(row)], now, "after {ticks} ticks");
            assert_eq!(status(loose), live(Phase::Working), "after {ticks} ticks");
        }
        for thread in [left, begun, row] {
            let named = host.state(thread).and_then(|(s, _)| s.meta.terminal());
            assert_eq!(named, None, "{thread} names no terminal once ended");
        }
        let queued = host.state(left).map(|(s, _)| s.queued);
        assert_eq!(queued, Some(Vec::new()), "its queue went with its TUI");
        let claude_code = |thread| host.state(thread).is_some_and(|(s, _)| is_claude(&s));
        assert!(claude_code(left) && claude_code(begun), "Claude Code's");
        assert!(!claude_code(row) && !claude_code(loose), "another agent's");
        drop(claude);
        watching.abort();
    }

    /// A picture as Claude Code stores one pasted into its box: a PNG, two pixels wide.
    fn picture() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0; 6]).unwrap();
        writer.finish().unwrap();
        bytes
    }

    /// The prompt box of a Claude Code Slopty started shows in its thread as Slopty's mod relays
    /// it (`slopty-agent`'s `tests/fixtures/claude-mod/drafts.jsonl`, what the mod relayed of a
    /// real one's box): each draft once heard, nothing for an empty box, and a picture pasted
    /// into it once Claude Code has stored it, looked for again a moment later when it had not
    /// yet, and read from its store while the box shows it. A draft of another session, or one
    /// relayed after a later one, is passed over; the box goes with the session.
    #[tokio::test(start_paused = true)]
    async fn the_prompt_box_shows_as_the_mod_relays_it() {
        use slopty_agent::claude::draft;
        use slopty_proto::thread::wire::Expanded;
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let home = dir.path().join("home");
        let place = Place { home: home.clone(), path: None, data: dir.path().join("data") };
        let (claude, watching) = Claude::spawn(host.clone(), place);
        let tmp = dir.path().join("tmp").to_string_lossy().into_owned();
        let uid = rustix::process::getuid().as_raw();
        let store = draft::images_dir(Some(&tmp), uid, Path::new("/work"), "session-1");
        let terminal = SessionId::new();
        let main =
            slopty_agent::claude::sessions::transcript(&home, Path::new("/work"), "session-1");
        let began = serde_json::json!({
            "hook_event_name": "SessionStart", "session_id": "session-1", "source": "startup",
            "cwd": "/work", "transcript_path": main.to_string_lossy(),
        });
        let mut at_us = 1_800_000_000_000_000_u64;
        let mut heard = |payload: &Value| {
            at_us = at_us.saturating_add(1_000_000);
            claude.hook(terminal, at_us, payload.to_string());
            at_us
        };
        heard(&began);
        let thread = slopty_agent::claude::thread_of("session-1");
        let shown = || {
            host.state(thread).and_then(|(s, _)| s.draft).map_or_else(
                || "none".to_owned(),
                |d| format!("{:?} {:?}", d.text, d.images.iter().map(|i| i.n).collect::<Vec<_>>()),
            )
        };
        let expanded = || claude.expand(thread, draft::image_ref("2.png"));
        let look = || tokio::time::sleep(Duration::from_millis(1_100));
        let mut moments = Vec::new();
        let relays = lines(&fixtures().join("../claude-mod/drafts.jsonl"));
        for relay in &relays {
            let mut payload = relay["payload"].clone();
            payload["tmpdir"] = Value::from(tmp.clone());
            let at = heard(&payload);
            look().await;
            moments.push(format!("{}: {}", relay["act"].as_str().unwrap(), shown()));
            if relay["act"] == "image" {
                // Not stored yet: looked for again a moment later, and found once it is.
                look().await;
                moments.push(format!("looked again: {}", shown()));
                assert_eq!(expanded().await, Expanded::Gone, "no picture before it is stored");
                std::fs::create_dir_all(&store).unwrap();
                std::fs::write(store.join("2.png"), picture()).unwrap();
                look().await;
                moments.push(format!("stored: {}", shown()));
                assert_eq!(
                    expanded().await,
                    Expanded::Bytes(picture()),
                    "its bytes, from its store"
                );
                let mut elsewhere = payload.clone();
                elsewhere["session_id"] = Value::from("session-2");
                elsewhere["prompt"] = Value::from("another session's");
                heard(&elsewhere);
                let mut late = payload.clone();
                late["prompt"] = Value::from("hell");
                claude.hook(terminal, at.saturating_sub(1), late.to_string());
                look().await;
                moments.push(format!("passed over: {}", shown()));
            }
            if relay["act"] == "clear" {
                assert_eq!(expanded().await, Expanded::Gone, "nothing read once the box is clear");
            }
        }
        // The session ends while its box holds a picture: the box goes, and the picture's store.
        let recalled = relays.iter().find(|r| r["act"] == "recall").unwrap();
        let mut payload = recalled["payload"].clone();
        payload["tmpdir"] = Value::from(tmp.clone());
        heard(&payload);
        look().await;
        assert_eq!(expanded().await, Expanded::Bytes(picture()), "shown again");
        heard(&serde_json::json!({ "hook_event_name": "SessionEnd", "session_id": "session-1" }));
        look().await;
        moments.push(format!("ended: {}", shown()));
        assert_eq!(expanded().await, Expanded::Gone, "nothing read once the session ended");
        let pasted = r#""hell one\ntwo[Pasted text #1 +5 lines][Image #2]""#;
        assert_eq!(
            moments,
            [
                r#"typed: "hello" []"#.to_owned(),
                r#"backspace: "hell" []"#.to_owned(),
                r#"short paste: "hell one\ntwo" []"#.to_owned(),
                r#"long paste: "hell one\ntwo[Pasted text #1 +5 lines]" []"#.to_owned(),
                format!("image: {pasted} []"),
                format!("looked again: {pasted} []"),
                format!("stored: {pasted} [2]"),
                format!("passed over: {pasted} [2]"),
                "clear: none".to_owned(),
                format!("recall: {pasted} [2]"),
                "back: none".to_owned(),
                r#"submit: "scenario" []"#.to_owned(),
                "submit: none".to_owned(),
                r#"pull back: "queued" []"#.to_owned(),
                "queue again: none".to_owned(),
                r#"ask: "scenario" []"#.to_owned(),
                "ask: none".to_owned(),
                "ended: none".to_owned(),
            ]
        );
        drop(claude);
        watching.abort();
    }

    /// Each hook speaks for the session it names. A session whose first hook is no start is
    /// followed by the files the hook names, and word of its start after keeps it as it is, in
    /// the version it names. Another session's hooks change nothing in the terminal, its end
    /// included. Two hooks at one instant are both taken, a draft's as well. A new session's
    /// start (as after `/clear`) is followed in place of the last, in the same Claude Code's
    /// version, and its end ends it; a session taken up again runs in its new version.
    #[tokio::test(start_paused = true)]
    async fn each_hook_speaks_for_the_session_it_names() {
        use slopty_proto::thread::{Liveness, Phase};
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let place = Place { home: home.clone(), path: None, data: dir.path().join("data") };
        let (claude, watching) = Claude::spawn(host.clone(), place);
        let (first, second) = (SessionId::new(), SessionId::new());
        let at = |n: u64| 1_800_000_000_000_000_u64.saturating_add(n.saturating_mul(1_000));
        let hook = |terminal, n: u64, event: &str, session: &str, extra: Value| {
            let main =
                slopty_agent::claude::sessions::transcript(&home, Path::new("/work"), session);
            let mut payload = serde_json::json!({
                "hook_event_name": event, "session_id": session, "cwd": "/work",
                "transcript_path": main.to_string_lossy(),
            });
            if let (Some(payload), Value::Object(extra)) = (payload.as_object_mut(), extra) {
                payload.extend(extra);
            }
            claude.hook(terminal, at(n), payload.to_string());
        };
        let settle = || tokio::time::sleep(Duration::from_secs(2));
        let state = |session: &str| {
            host.state(slopty_agent::claude::thread_of(session)).map(|(state, _)| state)
        };
        let status = |session: &str| state(session).map(|s| (s.status.phase, s.status.liveness));
        let live = |phase| Some((phase, Liveness::Live));
        let ended = |session: &str| {
            state(session).map(|s| s.status.liveness) == Some(Liveness::Exited { resumable: true })
        };
        let version = |session: &str| state(session).map(|s| s.meta.agent_version);

        hook(first, 1, "UserPromptSubmit", "session-a", Value::Null);
        settle().await;
        assert_eq!(status("session-a"), live(Phase::Working), "followed from its first hook");
        let word = |terminal, session: &str, version: &str| {
            claude.begun(terminal, session.to_owned(), "/work".to_owned(), version.to_owned());
        };
        word(first, "session-a", "2.1.400");
        settle().await;
        assert_eq!(status("session-a"), live(Phase::Working), "kept, not begun again");
        assert_eq!(version("session-a").as_deref(), Some("2.1.400"), "the version it names");

        hook(first, 2, "Stop", "session-b", Value::Null);
        hook(first, 3, "SessionEnd", "session-b", Value::Null);
        settle().await;
        assert_eq!(status("session-a"), live(Phase::Working), "another session's hooks");
        assert!(state("session-b").is_none(), "a session not followed here");

        hook(first, 4, "Stop", "session-a", Value::Null);
        hook(first, 4, "UserPromptSubmit", "session-a", Value::Null);
        let draft = |text: &str| serde_json::json!({ "prompt": text });
        hook(first, 5, slopty_agent::hooks::DRAFT_EVENT, "session-a", draft("one"));
        hook(first, 5, slopty_agent::hooks::DRAFT_EVENT, "session-a", draft("two"));
        settle().await;
        assert_eq!(status("session-a"), live(Phase::Working), "a hook at the same instant");
        let drafted = state("session-a").and_then(|s| s.draft).map(|d| d.text);
        assert_eq!(drafted.as_deref(), Some("two"), "a draft at the same instant");

        hook(first, 6, "SessionStart", "session-b", serde_json::json!({ "source": "clear" }));
        settle().await;
        assert_eq!(status("session-b"), live(Phase::Idle), "the new session, followed");
        assert!(ended("session-a"), "the last, ended: {:?}", status("session-a"));
        assert_eq!(version("session-b").as_deref(), Some("2.1.400"));
        hook(first, 7, "SessionEnd", "session-b", Value::Null);
        settle().await;
        assert!(ended("session-b"), "its end: {:?}", status("session-b"));
        // Taken up again by a newer Claude Code: its thread runs in that one's version.
        word(first, "session-a", "2.1.402");
        settle().await;
        assert_eq!(status("session-a"), live(Phase::Idle), "taken up again");
        assert_eq!(version("session-a").as_deref(), Some("2.1.402"));

        // Word of a start first: the next session there runs in the version it named.
        word(second, "session-c", "2.1.401");
        settle().await;
        hook(second, 8, "SessionStart", "session-d", serde_json::json!({ "source": "clear" }));
        settle().await;
        assert_eq!(version("session-d").as_deref(), Some("2.1.401"));
        drop(claude);
        watching.abort();
    }

    /// A session Slopty started whose Claude Code never shows in the foreground (a shell is
    /// there instead, and no process was ever seen) is taken to have ended only once a launcher
    /// has had its ticks to start it.
    #[tokio::test(start_paused = true)]
    async fn a_session_whose_agent_never_shows_ends_after_its_start_ticks() {
        use slopty_proto::thread::Liveness;
        let dir = tempfile::tempdir().unwrap();
        let host = Host::open(&dir.path().join("threads"), Limits::default()).unwrap();
        let home = dir.path().join("home");
        let place = Place { home, path: None, data: dir.path().join("data") };
        let (claude, watching) = Claude::spawn(host.clone(), place);
        let terminal = SessionId::new();
        let native = "00000000-0000-4000-8000-0000000000c3";
        claude.begun(terminal, native.to_owned(), "/work".to_owned(), "2.1.314".to_owned());
        let thread = slopty_agent::claude::thread_of(native);
        let liveness = || host.state(thread).map(|(s, _)| s.status.liveness);
        for ticks in 1..=9 {
            claude.tick(vec![(terminal, Foreground::Other)]);
            tokio::time::sleep(Duration::from_millis(1_100)).await;
            let now =
                if ticks <= 8 { Liveness::Live } else { Liveness::Exited { resumable: true } };
            assert_eq!(liveness(), Some(now), "after {ticks} ticks");
        }
        drop(claude);
        watching.abort();
    }
}
