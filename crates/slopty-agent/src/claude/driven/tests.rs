use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use slopty_proto::thread::{
    Action, ItemBody, Phase, RequestState, ThreadId, ThreadState, TurnState, Wait,
};

use super::*;

/// What Claude Code 2.1.301 wrote, driven against a canned model (`cargo xtask fixtures
/// claude-stream`): a turn that thinks and answers, one whose Bash call asks permission, a model,
/// a mode and the context asked for, a turn stopped as it streams, and `/compact`.
fn recorded(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-stream")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(path).expect("a recording");
    let mut lines: Vec<Value> =
        text.lines().map(|l| serde_json::from_str(l).expect("a JSON line")).collect();
    // The recorder asked `initialize` as `init1`; the codec under test asks it first, as its own.
    for line in &mut lines {
        if line["type"] == "control_response" && line["response"]["request_id"] == "init1" {
            line["response"]["request_id"] = json!("slopty-1");
        }
    }
    lines
}

/// An intent of a test's own, `n`.
fn intent(n: u32) -> IntentId {
    serde_json::from_value(json!(format!("aaaaaaaa-0000-4000-8000-{n:012}"))).expect("an id")
}

/// The intent whose message the recording's host sent as `line` names it (`command_uuid`).
fn sent_as(line: &Value) -> IntentId {
    serde_json::from_value(line["command_uuid"].clone()).expect("an id")
}

/// The threads as a host keeps them, from nothing but the outs.
#[derive(Default)]
struct Host {
    threads: BTreeMap<ThreadId, ThreadState>,
}

impl Host {
    fn take(&mut self, outs: Vec<Out>) {
        for out in outs {
            match out {
                Out::Begin(meta) => {
                    self.threads.insert(meta.id, ThreadState::new(*meta));
                }
                Out::Actions(thread, actions) => {
                    let state = self.threads.get_mut(&thread).expect("begun before its actions");
                    for action in &actions {
                        state.apply(action);
                    }
                }
            }
        }
    }

    fn main(&self, driven: &Driven) -> &ThreadState {
        self.threads.get(&driven.meta().id).expect("the main thread")
    }
}

const SESSION: &str = "11111111-2222-4333-8444-555555555555";

fn at(ms: u64) -> WallMs {
    WallMs::from_millis(ms.saturating_add(1_800_000_000_000))
}

fn driven() -> (Driven, Host) {
    let mut driven = Driven::new(SESSION, "/work", at(0));
    let mut host = Host::default();
    host.take(driven.drain());
    driven.initialize();
    (driven, host)
}

/// Every message Claude Code heard is the person's item once echoed, with the intent that sent
/// it; each turn ends as its result says (answered, answered after a permission Slopty gave,
/// stopped, a compaction); the permission is a request on its call whose answer is the result
/// Claude Code takes; and the meters follow the model, the mode and the context.
#[test]
fn a_recorded_session_is_a_thread() {
    let (mut driven, mut host) = driven();
    let prompts =
        ["scenario:think say hello", "scenario:touch make a file", "scenario:slow one", "/compact"];
    let mut sent = Vec::new();
    let mut answered = None;
    for line in recorded("session") {
        // Each message goes as the recording's host sent it: just before Claude Code queued it.
        if line["type"] == "command_lifecycle" && line["state"] == "queued" {
            let text = prompts[sent.len()];
            let id = sent_as(&line);
            sent.push(id);
            let n = u64::try_from(sent.len()).expect("few");
            host.take(driven.send(text, &[], id, at(10 * n)));
            let writes = driven.take_writes();
            let message: Value =
                serde_json::from_str(writes.last().expect("a line")).expect("JSON");
            assert_eq!(message["uuid"], json!(message_id(id)));
            assert_eq!(message["message"]["content"][0]["text"], json!(text));
        }
        host.take(driven.incoming(&line, at(1_000)));
        if line["type"] == "control_request" {
            let thread = host.main(&driven);
            let request = thread.requests.last().expect("a request");
            assert_eq!(request.state, RequestState::Open);
            assert_eq!(request.item.as_ref().map(|i| i.0.as_str()), Some("toolu_fake4"));
            assert_eq!(thread.status.phase, Phase::NeedsYou);
            let by = Answerer { client: None, name: "test".to_owned() };
            let id = request.id.clone();
            let done = driven.answer(&id, "allow", None, by, at(1_100)).expect("taken");
            host.take(done.outs);
            answered = driven.take_writes().pop();
        }
    }
    assert_eq!(sent.len(), 4);
    let answer: Value = serde_json::from_str(&answered.expect("an answer")).expect("JSON");
    assert_eq!(answer["response"]["request_id"], json!("00000000-0000-4000-8000-000000000041"));
    assert_eq!(answer["response"]["response"]["behavior"], json!("allow"));
    assert_eq!(answer["response"]["response"]["updatedInput"]["command"], json!("touch made.txt"));

    let thread = host.main(&driven);
    let ends: Vec<&TurnState> = thread.turns.iter().map(|t| &t.state).collect();
    assert_eq!(
        ends,
        [&TurnState::Complete, &TurnState::Complete, &TurnState::Interrupted, &TurnState::Complete],
        "{:#?}",
        thread.turns
    );
    assert!(thread.pending.is_empty(), "every message was heard: {:?}", thread.pending);
    let mine: Vec<Option<IntentId>> = thread
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::User(user) if user.command.is_none() => Some(user.intent),
            _ => None,
        })
        .collect();
    assert_eq!(mine, sent[..3].iter().copied().map(Some).collect::<Vec<_>>());
    let said: Vec<&str> = thread
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(said.contains(&"Hello there."), "{said:?}");
    assert!(
        thread.items.iter().any(|i| matches!(i.body, ItemBody::Compaction(_))),
        "the compaction is an item"
    );
    assert_eq!(thread.meters.mode.as_deref(), Some("plan"));
    assert_eq!(thread.meters.model_id.as_deref(), Some("claude-sonnet-5-5"));
    assert!(!thread.meta.models.is_empty() && !thread.commands.is_empty(), "from initialize");
    assert_eq!(thread.status.phase, Phase::Done);
    assert!(thread.status.wait.is_none(), "{:?}", thread.status);
}

/// What the model writes streams into items as it comes, and its message, when it comes,
/// settles each in place: the thread read live is the thread its records make.
#[test]
fn streamed_blocks_are_settled_by_their_messages() {
    let (mut driven, mut host) = driven();
    let mut appended = 0;
    for line in recorded("session") {
        if line["type"] == "command_lifecycle" && line["state"] == "queued" {
            let uuid = line["command_uuid"].as_str().expect("an id").to_owned();
            driven.history.sent_as(&uuid, sent_as(&line));
        }
        let outs = driven.incoming(&line, at(1_000));
        appended += outs
            .iter()
            .map(|o| match o {
                Out::Actions(_, actions) => {
                    actions.iter().filter(|a| matches!(a, Action::Append { .. })).count()
                }
                Out::Begin(_) => 0,
            })
            .sum::<usize>();
        host.take(outs);
    }
    assert!(appended > 0, "the stream was shown as it came");
    let live = host.main(&driven).items.iter().filter(|i| i.id.0.starts_with("live:")).count();
    let mut records = Driven::new(SESSION, "/work", at(0));
    let mut read = Host::default();
    read.take(records.drain());
    let messages: Vec<Value> = recorded("session")
        .into_iter()
        .filter(|l| matches!(l["type"].as_str(), Some("user" | "assistant")))
        .collect();
    read.take(records.replay(&messages, false, at(2_000)));
    let texts = |state: &ThreadState| -> Vec<String> {
        state
            .items
            .iter()
            .filter_map(|i| match &i.body {
                ItemBody::Text(t) | ItemBody::Reasoning(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(texts(host.main(&driven)), texts(read.main(&records)));
    assert_eq!(live, texts(host.main(&driven)).len(), "the live items are where they stood");
}

/// A subagent is a thread of its own, linked to the call that started it, done once its
/// notice comes; work the engine takes up alone afterwards ends as it ends.
#[test]
fn a_subagent_streams_into_its_own_thread() {
    let (mut driven, mut host) = driven();
    for line in recorded("agent") {
        host.take(driven.incoming(&line, at(1_000)));
    }
    let main = driven.meta().id;
    let sub = host
        .threads
        .values()
        .find(|t| t.meta.parent.as_ref().is_some_and(|p| p.thread == main))
        .expect("a subagent thread");
    assert!(
        sub.items
            .iter()
            .any(|i| matches!(&i.body, ItemBody::Text(t) if t.text == "Subagent found it.")),
        "{:#?}",
        sub.items
    );
    assert_eq!(sub.status.phase, Phase::Done);
    let thread = host.main(&driven);
    assert!(thread.turns.iter().all(|t| t.state != TurnState::Active), "{:#?}", thread.turns);
}

/// A message Claude Code does not acknowledge goes again under its id, once; still unheard, it
/// is held as one that may not have reached it, and nothing more goes on its own.
#[test]
fn an_unacknowledged_message_goes_again_then_is_held() {
    let (mut driven, mut host) = driven();
    drop(driven.take_writes());
    host.take(driven.send("hello", &[], intent(1), at(0)));
    let first = driven.take_writes();
    assert_eq!(first.len(), 1);
    assert_eq!(driven.deadline(), Some(at(ms(DELIVERY))));
    let (outs, escalate) = driven.tick(at(ms(DELIVERY)));
    host.take(outs);
    assert!(!escalate);
    assert_eq!(driven.take_writes(), first, "the same message, under the same id");
    let (outs, _) = driven.tick(at(2 * ms(DELIVERY)));
    host.take(outs);
    assert!(driven.take_writes().is_empty(), "not a third time");
    let pending = &host.main(&driven).pending;
    assert!(matches!(pending.as_slice(), [p] if p.is_unknown()), "{pending:?}");
    assert_eq!(driven.deadline(), None);
}

/// A stop Claude Code does not acknowledge in time is escalated: the worker ends the process.
/// One it acknowledges is not.
#[test]
fn an_unacknowledged_stop_ends_the_process() {
    let (mut driven, mut host) = driven();
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": "x", "state": "started"}),
        at(0),
    ));
    host.take(driven.interrupt(at(10)));
    let stop: Value =
        serde_json::from_str(driven.take_writes().last().expect("a stop")).expect("JSON");
    assert_eq!(stop["request"]["subtype"], json!("interrupt"));
    let wait = host.main(&driven).status.wait.clone().expect("waits on the stop");
    assert_eq!(wait.kind, Wait::STOPPING);
    assert!(!driven.tick(at(10 + ms(INTERRUPT_ACK) - 1)).1, "not yet");
    assert!(driven.tick(at(10 + ms(INTERRUPT_ACK))).1, "escalated");
    let id = stop["request_id"].clone();
    host.take(driven.incoming(
        &json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {"still_queued": []}}}),
        at(20),
    ));
    assert!(!driven.tick(at(10_000)).1, "acknowledged");
}

/// A turn that shows no progress for [`STALL`] is said to have stalled, and moves on with the
/// next word; a gap between looks longer than [`WAKE_GAP`] is the machine waking, not a stall.
#[test]
fn a_stall_is_said_but_a_wake_is_not() {
    let (mut driven, mut host) = driven();
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": "x", "state": "started"}),
        at(0),
    ));
    let tick = ms(TICK);
    let mut now = 0;
    // The machine sleeps for an hour: one late look, which is no stall.
    now += 60 * 60 * 1_000;
    host.take(driven.tick(at(now)).0);
    assert!(host.main(&driven).status.wait.is_none(), "a wake is not a stall");
    let until = now + ms(STALL) * 2;
    while now < until {
        now += tick;
        host.take(driven.tick(at(now)).0);
    }
    let wait = host.main(&driven).status.wait.clone().expect("stalled");
    assert_eq!(wait.kind, Wait::STALLED);
    host.take(driven.incoming(&json!({"type": "keep_alive"}), at(now + 1)));
    assert!(host.main(&driven).status.wait.is_none(), "it moved");
}

/// A usage limit holds every message until it resets, then lets them go in their order.
#[test]
fn the_limit_holds_messages_until_it_resets() {
    let (mut driven, mut host) = driven();
    drop(driven.take_writes());
    let resets = 1_800_000_100_u64;
    host.take(driven.incoming(
        &json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "resetsAt": resets, "rateLimitType": "five_hour"}}),
        at(0),
    ));
    host.take(driven.send("later", &[], intent(1), at(10)));
    assert!(driven.take_writes().is_empty(), "held");
    let thread = host.main(&driven);
    assert_eq!(thread.status.wait.as_ref().map(|w| w.kind.as_str()), Some(Wait::LIMIT));
    assert!(thread.pending.iter().all(|p| !p.is_waiting()), "{:?}", thread.pending);
    let reset = WallMs::from_millis(resets * 1_000);
    assert_eq!(driven.deadline(), Some(WallMs::from_millis(reset.as_millis() + ms(LIMIT_SLACK))));
    host.take(driven.tick(WallMs::from_millis(reset.as_millis() + ms(LIMIT_SLACK))).0);
    let next = driven.next_queued().expect("let go");
    assert_eq!(next.text, "later");
    assert!(host.main(&driven).status.wait.is_none());
}

/// Claude Code gone mid-turn: the turn fails with why, what it asked is withdrawn, what was on
/// its way may not have reached it, and the thread is exited and goes on by its next message.
#[test]
fn a_claude_code_gone_mid_turn_cuts_its_turn() {
    let (mut driven, mut host) = driven();
    host.take(driven.send("hi", &[], intent(1), at(0)));
    let uuid = message_id(intent(1));
    host.take(driven.incoming(
        &json!({"type": "user", "uuid": uuid, "isReplay": true, "session_id": SESSION, "timestamp": "2027-01-15T08:00:00.000Z", "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}}),
        at(1),
    ));
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}),
        at(1),
    ));
    host.take(driven.send("and more", &[], intent(2), at(2)));
    host.take(driven.exited(Some("Claude Code ended: boom"), at(3)));
    let thread = host.main(&driven);
    assert!(
        matches!(&thread.turns.last().expect("a turn").state, TurnState::Failed { error, .. } if error.contains("boom")),
        "{:?}",
        thread.turns
    );
    assert!(matches!(thread.pending.as_slice(), [p] if p.is_unknown()), "{:?}", thread.pending);
    assert_eq!(thread.status.liveness, Liveness::Exited { resumable: true });
}

/// A line that is no JSON says nothing, and what comes after it is heard.
#[test]
fn a_torn_line_is_passed_over() {
    let (mut driven, mut host) = driven();
    assert_eq!(driven.line(b"{\"type\":\"res", at(0)).len(), 0, "a torn line");
    assert_eq!(driven.line(b"not json at all", at(0)).len(), 0, "no JSON");
    host.take(
        driven.line(br#"{"type":"command_lifecycle","command_uuid":"x","state":"started"}"#, at(1)),
    );
    assert_eq!(host.main(&driven).status.phase, Phase::Working);
}

/// The rows of `thread` that say a request is being tried again, and the API errors left.
fn retries(thread: &ThreadState) -> (Vec<slopty_proto::thread::Retry>, usize) {
    let notices = thread.items.iter().filter_map(|item| match &item.body {
        ItemBody::Notice(notice) if notice.kind == Notice::API_ERROR => Some(notice.retry),
        _ => None,
    });
    let (trying, left): (Vec<_>, Vec<_>) = notices.partition(Option::is_some);
    (trying.into_iter().flatten().collect(), left.len())
}

/// A turn whose request Claude Code tries again keeps one row for it, each attempt updating it
/// with the next attempt and its wait. It goes once the turn streams again, and when the
/// attempts run out it is left as the turn's error.
#[test]
fn a_request_tried_again_is_one_row_until_it_goes_through_or_gives_up() {
    let (mut driven, mut host) = driven();
    let retry = |attempt: u64| {
        json!({
            "type": "system", "subtype": "api_retry", "attempt": attempt, "max_retries": 10,
            "retry_delay_ms": 500 * attempt, "error_status": 529,
            "error": { "type": "overloaded_error", "message": "Overloaded" },
            "session_id": SESSION,
        })
    };
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": "x", "state": "started"}),
        at(0),
    ));
    for attempt in 1..=3 {
        host.take(driven.incoming(&retry(attempt), at(attempt)));
    }
    let (trying, left) = retries(host.main(&driven));
    let want = slopty_proto::thread::Retry { attempt: 3, max: Some(10), in_ms: Some(1500) };
    assert_eq!((trying.as_slice(), left), (&[want][..], 0), "one row, the last attempt's");
    host.take(driven.incoming(
        &json!({"type": "assistant", "parent_tool_use_id": null, "session_id": SESSION, "uuid": "a1", "timestamp": "2027-01-15T08:00:00.000Z", "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "Back."}]}}),
        at(4),
    ));
    assert_eq!(retries(host.main(&driven)), (Vec::new(), 0), "gone once it went through");

    host.take(driven.incoming(&retry(1), at(5)));
    host.take(driven.incoming(&retry(2), at(6)));
    host.take(driven.incoming(
        &json!({"type": "result", "subtype": "success", "is_error": true, "result": "Overloaded", "session_id": SESSION}),
        at(7),
    ));
    let thread = host.main(&driven);
    assert_eq!(retries(thread), (Vec::new(), 1), "left as the error: {:#?}", thread.items);
}
