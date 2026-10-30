//! A worker up for weeks: each thread opened goes back to its head once it has rested
//! (`Host::rest`), so the worker holds the threads in use, not every thread it ever opened. The
//! bytes a thread opened holds, and what resting gives back, are counted by
//! `slopty_testkit::alloc` on the test's own thread, so they are exact on any machine.

#[cfg(test)]
mod rest {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use slopty_core::WallMs;
    use slopty_proto::thread::{
        Action, AgentId, Changed, Clipped, Cursor, Drive, Item, ItemBody, ItemId, PartKey,
        ThreadId, ThreadMeta, Turn, TurnId, TurnState, Usage,
    };
    use slopty_testkit::alloc::{self, Counting};
    use slopty_worker::thread::Host;
    use slopty_worker::thread::log::{Catchup, Limits};

    #[global_allocator]
    static ALLOC: Counting = Counting;

    /// Threads written, then opened again.
    const THREADS: usize = 40;
    /// Turns in each, every one with an answer of about 2 KB.
    const TURNS: u32 = 20;
    /// The threads but the one a follower watches.
    const UNFOLLOWED: usize = THREADS - 1;

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

    /// Turn `n`, its answer `words` and its end.
    fn answered(n: u32, words: &str) -> Vec<Action> {
        let id = ItemId(format!("a{n}"));
        vec![
            turn(n),
            Action::ItemStarted(Item {
                id: id.clone(),
                turn: TurnId(n),
                at_ms: WallMs::ZERO,
                body: ItemBody::Text(Clipped::default()),
            }),
            Action::Append { item: id, part: PartKey::Body, text: words.to_owned() },
            ended(n),
        ]
    }

    fn open(dir: &std::path::Path) -> Host {
        Host::open(dir, Limits::default()).expect("the host opens")
    }

    /// A thread opened holds its whole state and its tail; rested, only its head, as at the
    /// start. Nothing rests while it is used within the idle time or followed, and one rested
    /// comes back the same: its state, and its cursor, which a follower from before goes on
    /// from with only what it missed. Prints the bytes for `docs/MEASUREMENTS.md`.
    #[test]
    fn a_thread_at_rest_goes_back_to_its_head_and_comes_back_the_same() {
        assert!(alloc::installed(), "the counting allocator is this binary's");
        let dir = tempfile::tempdir().unwrap();
        let answer = "The build failed on a missing import, now fixed. ".repeat(40);
        {
            let host = open(dir.path());
            for _ in 0..THREADS {
                let meta = meta();
                let thread = meta.id;
                host.create(meta).unwrap();
                for n in 1..=TURNS {
                    host.apply(thread, answered(n, &answer));
                }
            }
        }

        let host = open(dir.path());
        assert_eq!(host.opened(), 0, "nothing opened at the start");
        let threads = host.threads();
        let (watched, kept) = (threads[0], threads[1]);
        let (state, cursor) = host.state(kept).expect("held");
        let before = alloc::live();
        for thread in &threads {
            drop(host.state(*thread));
        }
        let opened = alloc::live().saturating_sub(before);
        assert_eq!(host.opened(), THREADS);
        let follower = host.watch(watched).expect("held");

        assert_eq!(host.rest(Duration::from_hours(1)), 0, "all used within the hour");
        assert_eq!(host.rest(Duration::ZERO), UNFOLLOWED, "every one but the followed");
        assert_eq!(host.opened(), 1);
        let rested = alloc::live().saturating_sub(before);
        let per_thread = opened.checked_div(i64::try_from(UNFOLLOWED).unwrap()).unwrap();
        eprintln!(
            "rest: {THREADS} threads x {TURNS} turns of ~2 KB opened hold {} KiB each; after \
             resting, {} KiB held of {} KiB",
            per_thread / 1024,
            rested / 1024,
            opened / 1024
        );
        assert!(
            rested.saturating_mul(10) < opened,
            "resting gives back most of it: {rested} of {opened}"
        );
        drop(follower);
        assert_eq!(host.rest(Duration::ZERO), 1, "unfollowed, it rests too");

        assert_eq!(host.state(kept), Some((state, cursor)), "back the same");
        let next = host.apply(kept, vec![turn(TURNS + 1)]).expect("held");
        assert_eq!(
            (next.epoch, next.seq),
            (cursor.epoch, cursor.seq.saturating_add(1)),
            "the cursor goes on"
        );
        host.rest(Duration::ZERO);
        let follow = host.follow(kept, Some(cursor), 1).expect("held");
        let Catchup::Actions { from, actions } = follow.catchup else {
            panic!("a follower from before rest is sent what it missed")
        };
        assert_eq!((from, actions.len()), (cursor, 1));
    }

    /// A rest opens nothing: a thread not used since the start stays at its head, and its
    /// first use reads it whole.
    #[test]
    fn a_thread_never_used_is_not_opened_by_a_rest() {
        let dir = tempfile::tempdir().unwrap();
        let thread = {
            let host = open(dir.path());
            let meta = meta();
            let thread = meta.id;
            host.create(meta).unwrap();
            host.apply(thread, answered(1, "done"));
            thread
        };
        let host = open(dir.path());
        assert_eq!(host.rest(Duration::ZERO), 0, "nothing opened, nothing to rest");
        assert_eq!(host.opened(), 0);
        let Some((_, Cursor { seq, .. })) = host.state(thread) else { panic!("held") };
        assert_eq!(seq, 4);
    }
}
