use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::wire::{Expanded, Outcome, TableFrame};
use slopty_proto::thread::{
    Action, AgentId, Clipped, Cursor, Delivery, Drive, Item, ItemBody, ItemId, PartKey, Pending,
    PendingState, Request, RequestState, ThreadMeta, ThreadState, TurnId, UserMessage,
};

use super::cache::CACHE_THREADS;
use super::*;

fn meta(id: ThreadId) -> ThreadMeta {
    ThreadMeta {
        id,
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        agent_version: String::new(),
        native: "s1".to_owned(),
        cwd: "/w".to_owned(),
        title: "Fix the build".to_owned(),
        terminal: None,
        parent: None,
        origin: ThreadMeta::PERSON.to_owned(),
        forked_from: None,
        drive: Drive::named(Drive::OBSERVED),
        caps: Vec::new(),
        models: Vec::new(),
        facts: BTreeMap::new(),
        created_ms: WallMs::ZERO,
    }
}

fn snapshot(thread: ThreadId, seq: u64) -> ThreadFrame {
    ThreadFrame::Snapshot {
        cursor: Cursor { epoch: 7, seq },
        state: Box::new(ThreadState::new(meta(thread))),
    }
}

fn actions(first: u64, actions: Vec<Action>) -> ThreadFrame {
    let next = first.saturating_add(actions.len() as u64);
    ThreadFrame::Actions { epoch: 7, first, next, actions }
}

fn said(id: &str, intent: Option<IntentId>) -> Action {
    Action::ItemStarted(Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::User(UserMessage {
            text: Clipped::whole("hi"),
            images: Vec::new(),
            command: None,
            intent,
        }),
    })
}

fn request(id: &str) -> Action {
    Action::RequestOpened(Box::new(Request {
        id: AskId(id.to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run cargo test?".to_owned(),
        text: None,
        options: Vec::new(),
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: WallMs::ZERO,
        until_ms: None,
    }))
}

fn followed(thread: ThreadId) -> Threads {
    let mut threads = Threads::default();
    let _sent = threads.connected();
    let _follow = threads.open_thread(thread, None);
    let _took = threads.frame(thread, snapshot(thread, 0));
    threads
}

fn send(text: &str) -> Intent {
    Intent::Send { text: text.to_owned(), delivery: Delivery::Steer }
}

#[test]
fn frames_carry_a_mirror_on_and_a_gap_starts_the_stream_again_from_its_cursor() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (changed, out) = threads.frame(thread, actions(0, vec![said("u1", None)]));
    assert_eq!((changed, out.len()), (Changed::Thread, 0));
    let mirror = threads.mirror(thread).unwrap();
    assert_eq!(mirror.cursor(), Some(Cursor { epoch: 7, seq: 1 }));
    assert!(mirror.live());

    let (changed, out) = threads.frame(thread, actions(5, vec![said("u2", None)]));
    assert_eq!(changed, Changed::Nothing, "a frame that does not follow on is not applied");
    assert_eq!(
        out,
        [
            ClientMsg::Thread(ThreadRequest::Unfollow { thread }),
            ClientMsg::Thread(ThreadRequest::Follow {
                thread,
                have: Some(Cursor { epoch: 7, seq: 1 }),
                turns: SNAPSHOT_TURNS,
                max_latency_ms: MAX_LATENCY_MS,
            }),
        ],
        "the stream starts again from where the mirror stands"
    );
    assert_eq!(threads.mirror(thread).unwrap().state().unwrap().items.len(), 1);
}

#[test]
fn a_send_is_drawn_at_once_and_until_the_agent_s_record_shows_it() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (id, msg) = threads.intent(thread, send("hi"));
    assert!(msg.is_some(), "sent at once while linked");
    assert_eq!(threads.unshown(thread).count(), 1, "a pending bubble in the same frame");

    assert!(threads.done(&IntentDone { id, outcome: Outcome::Done }));
    assert_eq!(threads.unshown(thread).count(), 1, "the worker's answer alone is not the record");
    let _took = threads.frame(thread, actions(0, vec![said("u1", Some(id))]));
    assert_eq!(threads.unshown(thread).count(), 0, "the agent's own record takes over");
    assert!(threads.outbox().all().is_empty());
}

#[test]
fn a_send_the_worker_holds_is_its_pending_row_not_a_bubble() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (id, _msg) = threads.intent(thread, send("after this"));
    let held = Pending {
        intent: id,
        text: "after this".to_owned(),
        delivery: Delivery::Queue,
        state: PendingState::Waiting,
    };
    let _took = threads.frame(thread, actions(0, vec![Action::PendingSet(vec![held])]));
    assert_eq!(threads.unshown(thread).count(), 0, "drawn once, as the worker holds it");
}

#[test]
fn an_answer_flips_its_card_at_once_and_a_refusal_puts_it_back() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let _took = threads.frame(thread, actions(0, vec![request("p1")]));
    let ask = AskId("p1".to_owned());
    let answer = Intent::Answer { ask: ask.clone(), choice: "allow".to_owned(), message: None };
    let (id, _msg) = threads.intent(thread, answer);
    assert!(threads.answering(thread, &ask).is_some(), "flipped in the same frame");
    threads.done(&IntentDone { id, outcome: Outcome::Done });
    assert!(threads.answering(thread, &ask).is_some(), "until the thread says it is answered");
    let resolved = Action::RequestResolved { id: ask.clone(), state: RequestState::Released };
    let _took = threads.frame(thread, actions(1, vec![resolved]));
    assert!(threads.answering(thread, &ask).is_none());

    let _took = threads.frame(thread, actions(2, vec![request("p2")]));
    let ask = AskId("p2".to_owned());
    let again = Intent::Answer { ask: ask.clone(), choice: "allow".to_owned(), message: None };
    let (id, _msg) = threads.intent(thread, again);
    let refused = Outcome::Refused { reason: "Already answered".to_owned() };
    threads.done(&IntentDone { id, outcome: refused });
    assert!(threads.answering(thread, &ask).is_none(), "a refused answer is not drawn as one");
    assert!(threads.outbox().all().is_empty());
}

#[test]
fn a_failed_send_keeps_its_words_until_dismissed() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (id, _msg) = threads.intent(thread, send("hello"));
    let cap = slopty_proto::thread::Cap::named("steer");
    threads.done(&IntentDone { id, outcome: Outcome::Unsupported { cap } });
    let failed: Vec<&Sent> = threads.unshown(thread).collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].failure().as_deref(), Some("The agent cannot steer"));
    assert!(threads.dismiss(id).is_some());
    assert_eq!(threads.unshown(thread).count(), 0);
}

#[test]
fn intents_wait_out_a_dropped_link_and_go_again_under_their_ids() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (answered, _msg) = threads.intent(thread, send("one"));
    threads.done(&IntentDone { id: answered, outcome: Outcome::Accepted });
    let (lost, _msg) = threads.intent(thread, send("two"));
    threads.disconnected();
    assert!(!threads.mirror(thread).unwrap().live(), "drawn as last known");
    let (offline, msg) = threads.intent(thread, send("three"));
    assert!(msg.is_none(), "nothing goes while the link is down");

    let out = threads.connected();
    let resent: Vec<IntentId> = out
        .iter()
        .filter_map(|m| match m {
            ClientMsg::Thread(ThreadRequest::Intent { id, .. }) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(resent, [lost, offline], "the unanswered ones, oldest first, under their ids");
    assert!(
        out.iter().any(|m| matches!(
            m,
            ClientMsg::Thread(ThreadRequest::Follow {
                have: Some(Cursor { epoch: 7, seq: 0 }),
                ..
            })
        )),
        "the thread is followed again from its cursor: {out:?}"
    );
    assert!(
        matches!(out.first(), Some(ClientMsg::Thread(ThreadRequest::Table { have: None }))),
        "no table was heard yet, so all of it"
    );
}

#[test]
fn the_table_resumes_from_its_cursor_and_settles_a_list_s_answers() {
    let thread = ThreadId::new();
    let mut threads = Threads::default();
    let _out = threads.connected();
    let mut state = ThreadState::new(meta(thread));
    state.apply(&request("p1"));
    let row = state.row(WallMs::ZERO);
    threads.table(&TableFrame::Snapshot { cursor: Cursor { epoch: 3, seq: 9 }, rows: vec![row] });
    let ask = AskId("p1".to_owned());
    let answer = Intent::Answer { ask: ask.clone(), choice: "deny".to_owned(), message: None };
    let (id, _msg) = threads.intent(thread, answer);
    threads.done(&IntentDone { id, outcome: Outcome::Done });
    assert!(threads.answering(thread, &ask).is_some(), "the row still holds it open");
    let mut answered = ThreadState::new(meta(thread));
    answered.apply(&request("p1"));
    answered.apply(&Action::RequestResolved { id: ask, state: RequestState::Withdrawn });
    let delta = TableFrame::Delta {
        cursor: Cursor { epoch: 3, seq: 10 },
        rows: vec![answered.row(WallMs::ZERO)],
        removed: Vec::new(),
    };
    threads.table(&delta);
    assert!(threads.outbox().all().is_empty(), "settled by the row");

    threads.disconnected();
    let out = threads.connected();
    assert!(matches!(
        out.first(),
        Some(ClientMsg::Thread(ThreadRequest::Table { have: Some(Cursor { epoch: 3, seq: 10 }) }))
    ));
}

#[test]
fn a_cached_thread_is_drawn_before_any_frame_and_caught_up_from_its_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().join("w1"));
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let _took = threads.frame(thread, actions(0, vec![said("u1", None)]));
    let (kept, unfollow) = threads.close_thread(thread);
    assert!(unfollow.is_some());
    cache.keep_thread(thread, &kept.unwrap()).unwrap();

    let mut later = Threads::default();
    let follow = later.open_thread(thread, cache.thread(thread));
    assert!(follow.is_none(), "not linked yet");
    let state = later.mirror(thread).and_then(Mirror::state).unwrap();
    assert_eq!(state.items.len(), 1, "drawn from the cache");
    let out = later.connected();
    assert!(out.iter().any(|m| matches!(
        m,
        ClientMsg::Thread(ThreadRequest::Follow { have: Some(Cursor { epoch: 7, seq: 1 }), .. })
    )));
    let _took = later.frame(thread, actions(1, vec![said("u2", None)]));
    assert_eq!(later.mirror(thread).and_then(Mirror::state).unwrap().items.len(), 2);

    std::fs::write(dir.path().join("w1").join(format!("{thread}.thread")), b"torn").unwrap();
    assert!(cache.thread(thread).is_none(), "an unreadable file is dropped, not trusted");
    assert!(!dir.path().join("w1").join(format!("{thread}.thread")).exists());
}

#[test]
fn the_cache_keeps_the_outbox_and_only_the_most_recent_threads() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().join("w1"));
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let _sent = threads.intent(thread, send("kept"));
    assert!(threads.take_outbox_changed());
    cache.keep_outbox(threads.outbox()).unwrap();
    assert_eq!(&cache.outbox(), threads.outbox(), "an intent outlives a relaunch");
    let mode = std::fs::metadata(dir.path().join("w1/outbox")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "the user's alone");

    let state = ThreadState::new(meta(thread));
    let ids: Vec<ThreadId> =
        std::iter::repeat_with(ThreadId::new).take(CACHE_THREADS + 1).collect();
    for (n, id) in ids.iter().enumerate() {
        let cached = Cached { cursor: Cursor { epoch: 1, seq: n as u64 }, state: state.clone() };
        cache.keep_thread(*id, &cached).unwrap();
        // Distinct times, so the oldest is the first written.
        let file = std::fs::File::open(dir.path().join(format!("w1/{id}.thread"))).unwrap();
        let when = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(n as u64 + 1);
        file.set_modified(when).unwrap();
    }
    let _again = cache.keep_thread(ids[1], &Cached { cursor: Cursor::default(), state });
    assert!(cache.thread(ids[0]).is_none(), "the least recently written went");
    assert!(cache.thread(ids[CACHE_THREADS]).is_some());
}

#[test]
fn blobs_keep_the_most_recent_within_their_budget() {
    let mut blobs = Blobs::with_budget(10);
    let (a, b, c) =
        (ContentRef("a".to_owned()), ContentRef("b".to_owned()), ContentRef("c".to_owned()));
    blobs.put(a.clone(), Expanded::Text("aaaa".to_owned()));
    blobs.put(b.clone(), Expanded::Text("bbbb".to_owned()));
    assert!(blobs.get(&a).is_some(), "a is now the most recent");
    blobs.put(c.clone(), Expanded::Bytes(vec![0; 4]));
    assert!(blobs.get(&b).is_none(), "b, least recent, went to make room");
    assert!(blobs.get(&a).is_some() && blobs.get(&c).is_some());
    assert_eq!(blobs.bytes(), 8);
    blobs.put(ContentRef("huge".to_owned()), Expanded::Text("x".repeat(11)));
    assert_eq!(blobs.bytes(), 8, "one past the whole budget is not kept");
}

#[test]
fn an_expansion_comes_once_and_is_held() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let content = ContentRef("t1:out".to_owned());
    assert!(threads.expand(thread, &content).is_some(), "asked of the worker");
    assert!(threads.expand(thread, &content).is_none(), "asked once while on its way");
    assert!(threads.expanded(&content).is_none());
    let frame = ThreadFrame::Expanded { content: content.clone(), body: Expanded::Gone };
    assert_eq!(threads.frame(thread, frame).0, Changed::Expanded(content.clone()));
    assert_eq!(threads.expanded(&content).as_deref(), Some(&Expanded::Gone));
    assert!(threads.expand(thread, &content).is_none(), "held, so not asked again");
}

#[test]
fn an_item_s_revision_moves_with_it_and_with_a_fresh_snapshot() {
    let thread = ThreadId::new();
    let mut threads = followed(thread);
    let (u1, u2) = (ItemId("u1".to_owned()), ItemId("u2".to_owned()));
    let _took = threads.frame(thread, actions(0, vec![said("u1", None), said("u2", None)]));
    let rev = |t: &Threads, item: &ItemId| t.mirror(thread).unwrap().rev(item);
    let (first, other) = (rev(&threads, &u1), rev(&threads, &u2));
    let grow = Action::Append { item: u1.clone(), part: PartKey::Body, text: " there".to_owned() };
    let _took = threads.frame(thread, actions(2, vec![grow]));
    assert_ne!(rev(&threads, &u1), first, "the item that grew");
    assert_eq!(rev(&threads, &u2), other, "the item that did not");
    let _took = threads.frame(thread, snapshot(thread, 9));
    assert_ne!(rev(&threads, &u2), other, "a snapshot replaces every item");
}

#[test]
fn a_thread_s_title_is_its_state_s_else_its_row_s() {
    let (titled, untitled, unknown) = (ThreadId::new(), ThreadId::new(), ThreadId::new());
    let mut threads = followed(titled);
    let mut bare = meta(untitled);
    bare.title = String::new();
    let rows = vec![
        ThreadState::new(meta(titled)).row(WallMs::ZERO),
        ThreadState::new(bare).row(WallMs::ZERO),
    ];
    threads.table(&TableFrame::Snapshot { cursor: Cursor { epoch: 3, seq: 1 }, rows });
    assert_eq!(threads.title(titled), Some("Fix the build"), "followed, from its state");
    assert_eq!(threads.title(untitled), None, "a blank title is none");
    let mut row = ThreadState::new(meta(unknown)).row(WallMs::ZERO);
    row.title = "From the table".to_owned();
    let delta = TableFrame::Delta {
        cursor: Cursor { epoch: 3, seq: 2 },
        rows: vec![row],
        removed: Vec::new(),
    };
    threads.table(&delta);
    assert_eq!(threads.title(unknown), Some("From the table"), "unfollowed, from its row");
}
