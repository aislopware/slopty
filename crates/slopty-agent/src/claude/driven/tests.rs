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
    // Taken as answered: the tests of an `initialize` left unanswered start their own.
    driven.starting = None;
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
    assert_eq!(answer["response"]["request_id"], json!("00000000-0000-4000-8000-000000000045"));
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
    assert!(!driven.tick(at(20 + ms(END_WAIT) - 1)).1, "acknowledged, its turn has a moment");
    host.take(driven.incoming(
        &json!({"type": "result", "subtype": "error_during_execution", "is_error": true}),
        at(30),
    ));
    assert!(!driven.tick(at(10_000)).1, "the turn ended");
    assert_eq!(driven.deadline(), None, "nothing waits");
}

/// A stop Claude Code acknowledges but whose turn never ends is escalated once [`END_WAIT`] is
/// up, and the turn ends stopped, not failed, when the process goes. One it refuses is
/// escalated as one it never acknowledged: the refusal is said, and the stop never rests there.
#[test]
fn a_stop_acknowledged_without_its_turn_ending_or_refused_still_lands() {
    let answer = |id: &Value, ok: bool| {
        let subtype = if ok { "success" } else { "error" };
        json!({"type": "control_response", "response": {"subtype": subtype, "request_id": id,
               "error": "busy", "response": {}}})
    };
    let (mut driven, mut host) = driven();
    begun(&mut driven, &mut host, "go on", 1);
    host.take(driven.interrupt(at(10)));
    let stop: Value = serde_json::from_str(&driven.take_writes().pop().expect("a stop")).unwrap();
    host.take(driven.incoming(&answer(&stop["request_id"], true), at(20)));
    assert_eq!(driven.deadline(), Some(at(20 + ms(END_WAIT))), "the turn's moment to end");
    assert!(!driven.tick(at(20 + ms(END_WAIT) - 1)).1);
    assert!(driven.tick(at(20 + ms(END_WAIT))).1, "acknowledged, never ended: escalated");
    host.take(driven.exited(Some("Claude Code ended: killed"), at(3_100)));
    let thread = host.main(&driven);
    let turn = thread.turns.last().expect("a turn");
    assert_eq!(turn.state, TurnState::Interrupted, "stopped, as asked");

    let (mut driven, mut host) = self::driven();
    begun(&mut driven, &mut host, "go on", 1);
    host.take(driven.interrupt(at(10)));
    let stop: Value = serde_json::from_str(&driven.take_writes().pop().expect("a stop")).unwrap();
    host.take(driven.incoming(&answer(&stop["request_id"], false), at(20)));
    let said = notices(host.main(&driven));
    assert!(said.iter().any(|(_, text)| text.contains("refused to stop")), "{said:?}");
    assert!(driven.tick(at(10 + ms(INTERRUPT_ACK))).1, "refused: escalated all the same");
}

/// An `initialize` Claude Code never answers is said once [`INIT_WAIT`] is up, and the process
/// is ended; one it answers waits on nothing.
#[test]
fn a_claude_code_that_never_answers_initialize_is_said_not_to_start() {
    let mut driven = Driven::new(SESSION, "/work", at(0));
    let mut host = Host::default();
    host.take(driven.drain());
    driven.initialize();
    assert_eq!(driven.deadline(), Some(at(ms(INIT_WAIT))));
    assert!(!driven.tick(at(ms(INIT_WAIT) - 1)).1, "not yet");
    let (outs, escalate) = driven.tick(at(ms(INIT_WAIT)));
    host.take(outs);
    assert!(escalate, "ended");
    let said = notices(host.main(&driven));
    assert!(said.iter().any(|(_, text)| text.starts_with(DID_NOT_START)), "{said:?}");
    assert!(!driven.tick(at(ms(INIT_WAIT) * 2)).1, "said once");

    let mut driven = Driven::new(SESSION, "/work", at(0));
    driven.initialize();
    let ask: Value = serde_json::from_str(&driven.take_writes().pop().expect("asked")).unwrap();
    drop(driven.incoming(
        &json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": ask["request_id"], "response": {}}}),
        at(5),
    ));
    assert!(!driven.tick(at(ms(INIT_WAIT) * 2)).1, "answered");
}

/// A message on its way when the worker restarted is read again with nothing to send again: once
/// its time is up it is held as one that may not have arrived, and it holds back no message
/// queued behind it.
#[test]
fn a_message_read_again_mid_send_is_held_in_time_and_frees_the_queue() {
    let (mut driven, mut host) = driven();
    host.take(driven.send("hello", &[], intent(1), at(0)));
    let state = host.main(&driven).clone();
    let mut again = Driven::of(&state, at(100));
    let mut host = Host::default();
    host.take(again.drain());
    host.take(again.hold(intent(2), "next", Vec::new()));
    assert!(again.busy(), "the first may still start a turn");
    assert!(again.next_queued().is_none(), "the queue waits for it");
    assert_eq!(again.deadline(), Some(at(100 + ms(DELIVERY))));
    host.take(again.tick(at(100 + ms(DELIVERY))).0);
    assert!(again.take_writes().is_empty(), "nothing to send again");
    let pending = &host.main(&again).pending;
    assert!(pending.iter().any(|p| p.intent == intent(1) && p.is_unknown()), "{pending:?}");
    assert!(!again.busy());
    assert_eq!(again.next_queued().map(|p| p.text), Some("next".to_owned()), "the queue goes on");
}

/// A stall names the last call the model made.
#[test]
fn a_stall_names_the_last_call() {
    let (mut driven, mut host) = driven();
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": "x", "state": "started"}),
        at(0),
    ));
    host.take(driven.incoming(
        &json!({"type": "assistant", "uuid": "a1", "session_id": SESSION,
                "message": {"id": "m1", "role": "assistant", "content": [{"type": "tool_use",
                "id": "toolu_1", "name": "Bash", "input": {"command": "cargo test --workspace"}}]}}),
        at(1),
    ));
    let mut now = 1;
    while now < ms(STALL) * 2 {
        now += ms(TICK);
        host.take(driven.tick(at(now)).0);
    }
    let wait = host.main(&driven).status.wait.clone().expect("stalled");
    assert_eq!(wait.text, format!("{STALLED_TEXT} · last: Bash(cargo test --workspace)"));
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
    // Said as an end of its own kind, which the chat draws as a failure.
    let said = notices(thread);
    assert!(said.iter().any(|(kind, text)| kind == ENDED && text.contains("boom")), "{said:?}");
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

/// The request id of the last control request the codec wrote, and its subtype.
fn last_control(driven: &mut Driven) -> Vec<(String, String)> {
    driven
        .take_writes()
        .iter()
        .filter_map(|w| serde_json::from_str::<Value>(w).ok())
        .filter(|w| w["type"] == "control_request")
        .map(|w| {
            let id = w["request_id"].as_str().unwrap_or_default().to_owned();
            (w["request"]["subtype"].as_str().unwrap_or_default().to_owned(), id)
        })
        .collect()
}

/// The codec's request just written, as the recording names it: `named`.
fn asked(driven: &mut Driven, ids: &mut BTreeMap<String, String>, named: &str) {
    let (_, id) = last_control(driven).pop().expect("a control request");
    ids.insert(named.to_owned(), id);
}

/// The words of every notice in `thread`, by kind.
fn notices(thread: &ThreadState) -> Vec<(String, String)> {
    thread
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Notice(n) => Some((n.kind.clone(), n.text.text.clone())),
            _ => None,
        })
        .collect()
}

/// Claude Code 2.1.301's doors beside messages, as it answered them: an MCP App's page and tools
/// reach the page whole and a read it refuses says why; the permission rules are the thread's;
/// a background command stopped by its task id ends killed; a `!` command's output it takes in
/// without a turn is the person's item with its output; files put back say how many, and a
/// message with no checkpoint says why not.
#[test]
fn recorded_doors_answer_as_claude_code_did() {
    use slopty_proto::visual::{AppAnswer, AppAsk, AppBody};
    let (mut driven, mut host) = driven();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let first = driven.take_writes();
    assert!(first.iter().any(|w| w.contains("supportedDialogKinds")), "{first:?}");
    let mut queued = 0;
    let mut turn_one = None;
    for mut line in recorded("doors") {
        if line["type"] == "command_lifecycle" && line["state"] == "queued" {
            queued += 1;
            let id = sent_as(&line);
            let outs = if queued == 1 {
                driven.send("scenario:background start it", &[], id, at(10))
            } else {
                let ran = Ran {
                    command: "pwd".to_owned(),
                    stdout: "/work".to_owned(),
                    stderr: String::new(),
                };
                driven.shell(&ran, id, false, at(20))
            };
            host.take(outs);
            drop(driven.take_writes());
        }
        if line["type"] == "control_response" {
            let named = line["response"]["request_id"].as_str().unwrap_or_default().to_owned();
            match named.as_str() {
                "r1" => {
                    let ask = AppAsk::Read {
                        server: "apps".to_owned(),
                        uri: "ui://apps/echo".to_owned(),
                    };
                    driven.app(1, &ask);
                    asked(&mut driven, &mut ids, "r1");
                }
                "r2" => {
                    let ask = AppAsk::Read {
                        server: "apps".to_owned(),
                        uri: "ui://apps/none".to_owned(),
                    };
                    driven.app(2, &ask);
                    asked(&mut driven, &mut ids, "r2");
                }
                "t1" | "t2" => {
                    let (tool, arguments) = if named == "t1" {
                        ("echo", r#"{"text":"hi"}"#)
                    } else {
                        ("refresh", "{}")
                    };
                    let ask = AppAsk::Call {
                        server: "apps".to_owned(),
                        tool: tool.to_owned(),
                        arguments: arguments.to_owned(),
                    };
                    driven.app(if named == "t1" { 3 } else { 4 }, &ask);
                    asked(&mut driven, &mut ids, &named);
                }
                "b1" => {
                    driven.background(None);
                    asked(&mut driven, &mut ids, "b1");
                }
                "k1" => {
                    driven.stop_task("b00000001");
                    asked(&mut driven, &mut ids, "k1");
                }
                "w1" | "w2" => {
                    let turn = turn_one.expect("the first turn");
                    host.take(driven.rewind(turn, at(30)));
                    asked(&mut driven, &mut ids, &named);
                }
                "s1" if ids.contains_key("s1") => {
                    // Asked again while the server connected: as `init` names it connected.
                    driven.control(Control::McpStatus, &json!({ "subtype": "mcp_status" }));
                    asked(&mut driven, &mut ids, "s1");
                }
                _ => {}
            }
            if let Some(id) = ids.get(&named) {
                line["response"]["request_id"] = json!(id);
            }
        }
        host.take(driven.incoming(&line, at(1_000)));
        if line["type"] == "control_response" && line["response"]["request_id"] == "slopty-1" {
            // What `initialize` asks after it, by the recording's names.
            for (subtype, id) in last_control(&mut driven) {
                let named = match subtype.as_str() {
                    "list_permission_rules" => "l1",
                    "mcp_status" => "s1",
                    "ui_attach" => "u1",
                    "ui_panes" => "u2",
                    _ => continue,
                };
                ids.insert(named.to_owned(), id);
            }
        }
        if turn_one.is_none() {
            turn_one = host.main(&driven).turns.first().map(|t| t.id);
        }
    }
    let mut answers = driven.take_app_answers();
    answers.sort_by_key(|(n, _)| *n);
    let [(1, page), (2, missing), (3, echo), (4, refresh)] = answers.as_slice() else {
        panic!("every ask answered once: {answers:?}")
    };
    let AppAnswer::Resource { uri, mime_type, body: AppBody::Text(html), meta } = page else {
        panic!("{page:?}")
    };
    assert_eq!(
        (uri.as_str(), mime_type.as_deref()),
        ("ui://apps/echo", Some("text/html;profile=mcp-app"))
    );
    assert!(html.contains("<p id=\"echo\">") && meta.contains("csp"), "{html} {meta}");
    assert!(
        matches!(missing, AppAnswer::Refused(why) if why.contains("no resource")),
        "{missing:?}"
    );
    let AppAnswer::Called { result } = echo else { panic!("{echo:?}") };
    let result: Value = serde_json::from_str(result).expect("JSON");
    assert_eq!(result["structuredContent"], json!({ "echo": "hi", "hostUi": true }));
    assert!(matches!(refresh, AppAnswer::Called { result } if result.contains("refreshed 1")));

    let thread = host.main(&driven);
    let rules = thread.rules.as_ref().expect("the rules listed");
    assert_eq!(rules.rules.len(), 1, "{rules:?}");
    let rule = &rules.rules[0];
    assert_eq!(
        (rule.behavior.as_str(), rule.text.as_str(), rule.source.as_str(), rule.editable),
        ("allow", "Bash(echo:*)", "cliArg", false)
    );
    assert_eq!(rule.description.as_deref(), Some("Any Bash command starting with echo"));
    let task = thread.tasks.iter().find(|t| t.id == "b00000001").expect("the background command");
    assert_eq!((task.kind.as_str(), task.state.as_str()), ("shell", "killed"), "{task:?}");
    let bang = thread
        .items
        .iter()
        .find_map(|i| match &i.body {
            ItemBody::User(user) if user.command.as_deref() == Some("!") => Some(user),
            _ => None,
        })
        .expect("the `!` command, the person's");
    assert_eq!(bang.text.text, "pwd");
    assert!(thread.pending.is_empty(), "{:?}", thread.pending);
    let said = notices(thread);
    assert!(
        said.iter().any(|(k, t)| k == Notice::REWOUND && t == "Put back 0 files (+0 −0)"),
        "{said:?}"
    );
    assert!(
        said.iter().any(|(k, t)| k == Notice::INFO && t.contains("No file checkpoint found")),
        "{said:?}"
    );
    assert!(thread.turns.iter().all(|t| t.state != TurnState::Active), "{:#?}", thread.turns);
    assert_eq!(thread.status.phase, Phase::Done);
}

/// A turn that ends with a background subagent still at work says it waits on it, as the TUI's
/// footer does, and the subagent stays a live task until its notice finishes it.
#[test]
fn a_turn_left_waiting_on_its_background_agent_says_so() {
    let (mut driven, mut host) = driven();
    let stream = [
        (json!({ "type": "system", "subtype": "init", "session_id": SESSION, "model": "claude-opus-5-5" })),
        (json!({
            "type": "assistant", "uuid": "a1", "session_id": SESSION, "parent_tool_use_id": null,
            "message": { "id": "m1", "role": "assistant", "content": [{
                "type": "tool_use", "id": "t1", "name": "Agent",
                "input": { "description": "Survey", "prompt": "look", "run_in_background": true },
            }] },
        })),
        (json!({
            "type": "system", "subtype": "task_started", "session_id": SESSION, "task_id": "x1",
            "tool_use_id": "t1", "description": "Survey", "task_type": "local_agent",
            "subagent_type": "general-purpose", "is_backgrounded": true,
        })),
        (json!({
            "type": "user", "uuid": "u1", "session_id": SESSION, "parent_tool_use_id": null,
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "t1", "content": "Async agent launched",
            }] },
            "tool_use_result": { "isAsync": true, "status": "async_launched", "agentId": "x1" },
        })),
        (json!({ "type": "result", "subtype": "success", "is_error": false, "result": "ok", "session_id": SESSION })),
    ];
    for message in &stream {
        host.take(driven.incoming(message, at(100)));
    }
    let thread = host.main(&driven);
    let said = notices(thread);
    assert!(
        said.iter()
            .any(|(k, t)| k == Notice::TURN && t == "Waiting for 1 background agent to finish"),
        "{said:?}"
    );
    let task = thread.tasks.iter().find(|t| t.id == "x1").expect("the subagent's task");
    assert!(task.is_live() && task.backgrounded, "{task:?}");
    assert!(thread.turns.iter().all(|t| t.state != TurnState::Active), "{:#?}", thread.turns);

    let done = json!({
        "type": "system", "subtype": "task_notification", "session_id": SESSION, "task_id": "x1",
        "tool_use_id": "t1", "status": "completed", "summary": "Found it",
        "usage": { "total_tokens": 1200, "tool_uses": 3, "duration_ms": 4000 },
    });
    host.take(driven.incoming(&done, at(200)));
    let thread = host.main(&driven);
    let task = thread.tasks.iter().find(|t| t.id == "x1").expect("the subagent's task");
    assert_eq!(task.state, "completed", "{task:?}");
    assert_eq!(task.usage.map(|u| u.tokens), Some(1200), "{task:?}");
}

/// Why Claude Code asks is the request's, in its TUI's words: a classifier, an ask rule (one
/// that overrides auto mode said so), a hook, or its own words for anything else.
#[test]
fn why_claude_code_asks_is_said_as_its_tui_says_it() {
    let reason = |fields: Value, tool: &str, mode: Option<&str>| asks::reason(&fields, tool, mode);
    assert_eq!(
        reason(
            json!({ "decision_reason_type": "classifier", "decision_reason": "Deletes files" }),
            "Bash",
            Some("auto")
        )
        .as_deref(),
        Some("Auto mode classifier requires confirmation for this command.\nDeletes files")
    );
    assert_eq!(
        reason(
            json!({ "decision_reason_type": "rule", "matched_ask_rule": "Bash(rm:*)" }),
            "Bash",
            Some("auto")
        )
        .as_deref(),
        Some("Ask rule Bash(rm:*) overrides auto mode for this command.")
    );
    assert_eq!(
        reason(json!({ "decision_reason_type": "rule", "matched_ask_rule": "Write", "decision_reason_rule_source": "projectSettings" }), "Write", Some("default")).as_deref(),
        Some("Permission rule Write from projectSettings requires confirmation for this tool.")
    );
    assert_eq!(
        reason(json!({ "decision_reason_type": "hook", "decision_reason_hook_event": "PreToolUse:Bash", "decision_reason": "on main" }), "Bash", None).as_deref(),
        Some("Hook PreToolUse:Bash requires confirmation for this command: on main")
    );
    assert_eq!(
        reason(json!({ "decision_reason_type": "workingDir", "decision_reason": "Outside the folder" }), "Read", None).as_deref(),
        Some("Outside the folder")
    );
    assert_eq!(reason(json!({}), "Bash", None), None, "it says nothing");
}

/// Every line `driven` wrote since last asked, as JSON.
fn written(driven: &mut Driven) -> Vec<Value> {
    driven.take_writes().iter().filter_map(|w| serde_json::from_str(w).ok()).collect()
}

/// A `can_use_tool` ask of `tool` with `input`, as Claude Code sends it.
fn can_use(id: &str, tool: &str, input: &Value, suggestions: &Value) -> Value {
    json!({
        "type": "control_request", "request_id": id,
        "request": {
            "subtype": "can_use_tool", "tool_name": tool, "input": input,
            "tool_use_id": format!("tu-{id}"), "permission_suggestions": suggestions,
        },
    })
}

/// A plan reads as the TUI's dialog: plan mode entered from bypass permissions is marked in the
/// transcript, the plan offers "Yes, clear context and bypass permissions" first, and that one
/// verb turns the plan's call down, keeps bypass permissions (marked once), clears the context and
/// sends the plan. Plan mode itself is asked for in the TUI's words, and in a session begun in
/// bypass permissions says it will ask before almost nothing.
#[test]
fn a_plan_reads_as_the_tuis_dialog_and_its_clear_keeps_the_mode() {
    let (mut driven, mut host) = driven();
    let status = |mode: &str| json!({ "type": "system", "subtype": "status", "permissionMode": mode, "session_id": SESSION });
    host.take(driven.incoming(&status("bypassPermissions"), at(10)));
    host.take(driven.incoming(&can_use("e1", "EnterPlanMode", &json!({}), &json!([])), at(20)));
    let enter = host.main(&driven).requests.last().expect("plan mode asked").clone();
    assert_eq!(enter.title, "Enter plan mode?");
    let labels: Vec<&str> = enter.options.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, ["Yes, enter plan mode", "No, start implementing now"]);
    let by = || Answerer { client: None, name: "test".to_owned() };
    host.take(driven.answer(&enter.id, "allow", None, by(), at(30)).expect("taken").outs);
    host.take(driven.incoming(&status("plan"), at(40)));
    let suggest = json!([{ "type": "setMode", "mode": "acceptEdits", "destination": "session" }]);
    let plan = can_use("p1", "ExitPlanMode", &json!({ "plan": "1. Write it" }), &suggest);
    host.take(driven.incoming(&plan, at(50)));
    let asked = host.main(&driven).requests.last().expect("the plan").clone();
    let options: Vec<(&str, &str)> =
        asked.options.iter().map(|c| (c.id.as_str(), c.label.as_str())).collect();
    assert_eq!(
        options,
        [
            ("clear:bypassPermissions", "Yes, clear context and bypass permissions"),
            ("always", "Yes, auto-accept edits"),
            ("allow", "Yes, manually approve edits"),
            ("deny", "No, keep planning"),
        ]
    );
    let _before = driven.take_writes();
    let done = driven.answer(&asked.id, "clear:bypassPermissions", None, by(), at(60));
    host.take(done.expect("taken").outs);
    let lines = written(&mut driven);
    assert_eq!(lines[0]["response"]["response"]["behavior"], "deny");
    let mode = lines.iter().find(|l| l["request"]["subtype"] == "set_permission_mode");
    assert_eq!(mode.expect("the mode")["request"]["mode"], "bypassPermissions");
    let said: Vec<String> = lines.iter().map(ToString::to_string).collect();
    let clear = said.iter().position(|l| l.contains("\"/clear\"")).expect("the clear");
    let carried = said.iter().position(|l| l.contains("Implement the following plan")).unwrap();
    assert!(clear < carried, "{said:#?}");
    host.take(driven.incoming(&status("bypassPermissions"), at(70)));
    let marks: Vec<String> = notices(host.main(&driven))
        .into_iter()
        .filter(|(kind, _)| kind == Notice::MODE)
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        marks,
        [
            "Switched to plan mode",
            "Session was started with bypass permissions, so plan mode won't ask before most \
             edits or commands. Start a new session without bypass permissions for prompts in \
             plan mode.",
            "Switched to bypass permissions",
        ]
    );
}

/// A command's card is the TUI's ("Bash command", the command and what it is for), and "Yes, and
/// tell Claude what to do next" is one answer: the call goes ahead, then the words go in.
#[test]
fn an_allow_with_words_goes_ahead_then_says_them() {
    let (mut driven, mut host) = driven();
    let input = json!({ "command": "cargo test", "description": "Run the tests" });
    host.take(driven.incoming(&can_use("b1", "Bash", &input, &json!([])), at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    assert_eq!(asked.title, "Bash command");
    assert_eq!(asked.text.as_ref().map(|t| t.text.as_str()), Some("cargo test\n\nRun the tests"));
    let _before = driven.take_writes();
    let by = Answerer { client: None, name: "test".to_owned() };
    let done = driven.answer(&asked.id, "allow", Some("then commit"), by, at(20));
    host.take(done.expect("taken").outs);
    let lines = written(&mut driven);
    assert_eq!(lines[0]["response"]["response"]["behavior"], "allow");
    assert_eq!(lines[1]["type"], "user");
    assert_eq!(lines[1]["message"]["content"][0]["text"], "then commit");
    // A deny's words stay its reason, never a message.
    host.take(
        driven.incoming(&can_use("b2", "Bash", &json!({ "command": "ls" }), &json!([])), at(30)),
    );
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    let by = Answerer { client: None, name: "test".to_owned() };
    host.take(driven.answer(&asked.id, "deny", Some("no"), by, at(40)).expect("taken").outs);
    assert!(written(&mut driven).iter().all(|l| l["type"] != "user"));
}

/// A plan approved with words is approved as edited, as the TUI's Ctrl+G does: the plan in the
/// answer's input is the person's, and no message follows.
#[test]
fn a_plan_approved_with_words_is_the_edited_plan() {
    let (mut driven, mut host) = driven();
    let input = json!({ "plan": "1. Do it" });
    host.take(driven.incoming(&can_use("p1", "ExitPlanMode", &input, &json!([])), at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    let _before = driven.take_writes();
    let by = Answerer { client: None, name: "test".to_owned() };
    let done = driven.answer(&asked.id, "allow", Some("1. Do it\n2. Test it"), by, at(20));
    host.take(done.expect("taken").outs);
    let lines = written(&mut driven);
    assert_eq!(lines[0]["response"]["response"]["behavior"], "allow");
    assert_eq!(lines[0]["response"]["response"]["updatedInput"]["plan"], "1. Do it\n2. Test it");
    assert!(lines.iter().all(|l| l["type"] != "user"), "{lines:?}");
}

/// "Chat about this" answers an agent's questions with none of their answers: the call is
/// turned down with the TUI's words that the person wants to talk them over, naming each.
#[test]
fn chat_about_this_talks_the_questions_over() {
    let (mut driven, mut host) = driven();
    let input = json!({ "questions": [{ "question": "Which database?", "header": "DB",
        "multiSelect": false, "options": [{ "label": "Postgres", "description": "" }] }] });
    host.take(driven.incoming(&can_use("q1", "AskUserQuestion", &input, &json!([])), at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    assert!(asked.options.iter().any(|c| c.label == "Chat about this"), "{:?}", asked.options);
    let _before = driven.take_writes();
    let by = Answerer { client: None, name: "test".to_owned() };
    host.take(driven.answer(&asked.id, "clarify", None, by, at(20)).expect("taken").outs);
    let lines = written(&mut driven);
    let response = &lines[0]["response"]["response"];
    assert_eq!(response["behavior"], "deny");
    let message = response["message"].as_str().unwrap_or_default();
    assert!(message.starts_with("The user wants to clarify these questions."), "{message}");
    assert!(message.contains("- \"Which database?\"\n  (No answer provided)"), "{message}");
}

/// An answer to a question whose options have previews carries what the TUI's dialog adds:
/// the picked option's preview and the person's notes, by the question's text.
#[test]
fn an_answer_carries_its_preview_and_notes() {
    let (mut driven, mut host) = driven();
    let input = json!({ "questions": [{ "question": "Which layout?", "header": "Layout",
        "multiSelect": false, "options": [
            { "label": "Split", "description": "", "preview": "| a | b |" },
            { "label": "Stack", "description": "", "preview": "a\nb" }] }] });
    host.take(driven.incoming(&can_use("q1", "AskUserQuestion", &input, &json!([])), at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    let _before = driven.take_writes();
    let by = Answerer { client: None, name: "test".to_owned() };
    let choice = json!([{ "question": "Which layout?", "answer": "Split" }]).to_string();
    let notes = json!({ "Which layout?": "keep the gutter" }).to_string();
    host.take(driven.answer(&asked.id, &choice, Some(&notes), by, at(20)).expect("taken").outs);
    let lines = written(&mut driven);
    let input = &lines[0]["response"]["response"]["updatedInput"];
    assert_eq!(input["answers"]["Which layout?"], "Split");
    assert_eq!(
        input["annotations"]["Which layout?"],
        json!({ "preview": "| a | b |", "notes": "keep the gutter" })
    );
}

/// Fast mode says how it stands (cooling down after its limit, back on), a model switch that
/// failed says why in the TUI's words, and whether ultracode applies is a fact of the thread.
#[test]
fn the_model_says_how_it_stands() {
    let (mut driven, mut host) = driven();
    let result = |fast: &str| {
        json!({ "type": "result", "subtype": "success", "is_error": false, "result": "ok",
                "session_id": SESSION, "fast_mode_state": fast })
    };
    for (n, fast) in ["on", "cooldown", "on"].iter().enumerate() {
        let n = u64::try_from(n).expect("few");
        host.take(driven.incoming(&result(fast), at(100 * n)));
    }
    let thread = host.main(&driven);
    let notes: Vec<&str> = thread.notes.iter().map(|n| n.text.as_str()).collect();
    assert!(notes.contains(&"Fast limit reset · now using fast mode"), "{notes:?}");
    assert_eq!(thread.meta.facts.get(FAST_FACT).map(String::as_str), Some("on"));
    drop(driven.take_writes());
    driven.set_model("haiku");
    let (_, id) = last_control(&mut driven).pop().expect("the switch");
    let refused = json!({ "type": "control_response",
        "response": { "subtype": "error", "request_id": id, "error": "not allowed" } });
    host.take(driven.incoming(&refused, at(500)));
    let said = notices(host.main(&driven));
    assert!(said.iter().any(|(_, t)| t == "Model switch failed: not allowed"), "{said:?}");
    driven.ask_settings();
    let (_, id) = last_control(&mut driven).pop().expect("the settings");
    let settings = json!({ "type": "control_response", "response": { "subtype": "success",
        "request_id": id, "response": {
            "applied": { "ultracode": true, "ultracodeAvailable": true },
            "errors": [{ "file": "/w/.claude/settings.json", "path": "", "message": "Invalid JSON" }] } } });
    host.take(driven.incoming(&settings, at(600)));
    assert_eq!(host.main(&driven).meta.facts.get(ULTRACODE_FACT).map(String::as_str), Some("on"));
    let notes: Vec<&str> = host.main(&driven).notes.iter().map(|n| n.text.as_str()).collect();
    assert!(notes.contains(&"/w/.claude/settings.json was skipped: Invalid JSON"), "{notes:?}");
}

/// The rows the TUI derives around a turn: what it leaves running ("Waiting for 2 background
/// agents to finish · 1 background task still running"), background agents stopped together by
/// name, a goal not yet met with its reason, paused on the usage limit, and cleared by the person
/// with no "achieved".
#[test]
fn a_turns_leftovers_stops_and_goal_say_what_the_tui_says() {
    let (mut driven, mut host) = driven();
    let started = |id: &str, kind: &str, what: &str| {
        json!({ "type": "system", "subtype": "task_started", "session_id": SESSION, "task_id": id,
                "description": what, "task_type": kind, "is_backgrounded": true })
    };
    for message in [
        started("x1", "local_agent", "Survey"),
        started("x2", "local_agent", "Review"),
        started("s1", "local_bash", "npm run dev"),
        json!({ "type": "result", "subtype": "success", "is_error": false, "result": "ok", "session_id": SESSION }),
        json!({ "type": "system", "subtype": "agents_killed", "session_id": SESSION }),
    ] {
        host.take(driven.incoming(&message, at(100)));
    }
    let said = notices(host.main(&driven));
    assert!(
        said.contains(&(
            Notice::TURN.to_owned(),
            "Waiting for 2 background agents to finish · 1 background task still running"
                .to_owned()
        )),
        "{said:?}"
    );
    assert!(
        said.contains(&(
            Notice::STOPPED.to_owned(),
            "2 background agents were stopped: \"Survey\", \"Review\"".to_owned()
        )),
        "{said:?}"
    );
    let goal = |iterations: u64, reason: Option<&str>| {
        json!({ "type": "active_goal", "value": { "condition": "tests pass", "set_at": 1,
                "iterations": iterations, "tokens_at_start": 0, "last_reason": reason } })
    };
    host.take(driven.incoming(&goal(0, None), at(200)));
    host.take(driven.incoming(&goal(1, Some("2 tests fail")), at(300)));
    let limited = json!({ "type": "rate_limit_event", "session_id": SESSION, "rate_limit_info": {
        "status": "rejected", "resetsAt": 1_900_000_000, "rateLimitType": "five_hour" } });
    host.take(driven.incoming(&limited, at(400)));
    host.take(driven.send("/goal clear", &[], intent(9), at(500)));
    host.take(driven.incoming(&json!({ "type": "active_goal", "value": null }), at(600)));
    let goals: Vec<String> = notices(host.main(&driven))
        .into_iter()
        .filter(|(kind, _)| kind == Notice::GOAL)
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        goals,
        [
            "Goal not yet met… continuing\nReason: 2 tests fail",
            "Goal paused · usage limit reached · continues automatically when it resets",
        ]
    );
}

/// A request whose "always allow" Claude Code holds back (`suppress_always_allow_rule`) offers
/// none, though it suggests rules; one that does not, offers it.
#[test]
fn a_held_back_always_allow_is_not_offered() {
    let (mut driven, mut host) = driven();
    let rule = json!([{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "ls" }],
                        "behavior": "allow", "destination": "localSettings" }]);
    host.take(driven.incoming(&can_use("a1", "Bash", &json!({ "command": "ls" }), &rule), at(10)));
    let ids = |host: &Host, driven: &Driven| -> Vec<String> {
        let asked = host.main(driven).requests.last().expect("the ask");
        asked.options.iter().map(|c| c.id.clone()).collect()
    };
    assert!(ids(&host, &driven).contains(&"always".to_owned()));
    let mut held = can_use("a2", "Bash", &json!({ "command": "ls" }), &rule);
    held["request"]["suppress_always_allow_rule"] = json!(true);
    host.take(driven.incoming(&held, at(20)));
    assert!(!ids(&host, &driven).contains(&"always".to_owned()), "{:?}", ids(&host, &driven));
}

/// A request Claude Code says only the person may answer (`requires_user_interaction`) is
/// held as one only the person at its card answers, with no rule to allow its like again; one
/// that does not say so is not.
#[test]
fn a_request_needing_the_person_is_held_for_the_card() {
    let (mut driven, mut host) = driven();
    let rule = json!([{ "type": "addRules", "rules": [{ "toolName": "Bash", "ruleContent": "rm" }],
                        "behavior": "allow", "destination": "localSettings" }]);
    let input = json!({ "command": "rm -rf build" });
    host.take(driven.incoming(&can_use("a1", "Bash", &input, &rule), at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    assert!(!asked.needs_presence);
    let mut present = can_use("a2", "Bash", &input, &rule);
    present["request"]["requires_user_interaction"] = json!(true);
    host.take(driven.incoming(&present, at(20)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    assert!(asked.needs_presence, "{asked:?}");
    assert!(asked.options.iter().all(|c| c.id != "always"), "{:?}", asked.options);
}

/// A write the person cannot review says so, as the TUI's card does: an existing file that is
/// not text offers no "always allow" (one-time only), a large one says it is not previewed, and
/// a symlink whose target leaves the folder names the target; a plain new file says nothing.
#[test]
fn a_write_says_what_is_on_disk_where_it_lands() {
    let dir = tempfile::tempdir().expect("a scratch folder");
    let binary = dir.path().join("blob.bin");
    std::fs::write(&binary, [0_u8, 159, 146, 150, 7]).expect("the binary file");
    let large = dir.path().join("large.txt");
    std::fs::write(&large, "x".repeat(300_000)).expect("the large file");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&large, &link).expect("the symlink");
    let rule = json!([{ "type": "addRules", "rules": [{ "toolName": "Write" }],
                        "behavior": "allow", "destination": "localSettings" }]);
    let (mut driven, mut host) = driven();
    let mut ask = |id: &str, path: &Path, at_ms: u64| {
        let input = json!({ "file_path": path.display().to_string(), "content": "new" });
        host.take(driven.incoming(&can_use(id, "Write", &input, &rule), at(at_ms)));
        let asked = host.main(&driven).requests.last().expect("the ask").clone();
        let text = asked.text.map(|t| t.text).unwrap_or_default();
        (text, asked.options.iter().any(|c| c.id == "always"))
    };
    let (text, always) = ask("a1", &binary, 10);
    assert!(text.contains("cannot be shown in full") && text.contains("one-time only"), "{text}");
    assert!(!always, "an unreviewable overwrite offers no rule");
    let (text, always) = ask("a2", &large, 20);
    assert!(text.contains("Existing file is too large to preview"), "{text}");
    assert!(always);
    let (text, _) = ask("a3", &link, 30);
    assert!(text.contains("(outside working directory) via a symlink"), "{text}");
    let (text, always) = ask("a4", &dir.path().join("new.txt"), 40);
    assert!(!text.contains("preview") && !text.contains("symlink"), "{text}");
    assert!(always);
}

/// A turn begun with `text` and echoed, for a result to end.
fn begun(driven: &mut Driven, host: &mut Host, text: &str, n: u32) {
    host.take(driven.send(text, &[], intent(n), at(0)));
    let uuid = message_id(intent(n));
    host.take(driven.incoming(
        &json!({"type": "user", "uuid": uuid, "isReplay": true, "session_id": SESSION,
                "timestamp": "2027-01-15T08:00:00.000Z",
                "message": {"role": "user", "content": [{"type": "text", "text": text}]}}),
        at(1),
    ));
    host.take(driven.incoming(
        &json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}),
        at(1),
    ));
}

/// The last turn's error, when it failed.
fn failed_with(thread: &ThreadState) -> Option<String> {
    match &thread.turns.last()?.state {
        TurnState::Failed { error, .. } => Some(error.clone()),
        _ => None,
    }
}

/// A turn Claude Code would not start says why in words before what it printed, one that
/// filled the context says so, and a call a hook deferred says the turn stopped there.
#[test]
fn a_turn_that_cannot_go_on_says_why() {
    let (mut driven, mut host) = driven();
    begun(&mut driven, &mut host, "hi", 1);
    host.take(driven.incoming(
        &json!({ "type": "result", "subtype": "error_during_execution", "is_error": true,
                 "session_id": SESSION, "startup_failure_reason": "provider_not_allowed",
                 "errors": ["provider blocked"] }),
        at(10),
    ));
    let error = failed_with(host.main(&driven)).unwrap_or_default();
    assert!(error.starts_with(STARTUP_REFUSED), "{error}");
    assert!(error.contains("do not allow this API provider") && error.contains("provider blocked"));
    begun(&mut driven, &mut host, "more", 2);
    host.take(driven.incoming(
        &json!({ "type": "result", "subtype": "success", "is_error": true, "session_id": SESSION,
                 "terminal_reason": "prompt_too_long", "result": "Prompt is too long" }),
        at(20),
    ));
    let error = failed_with(host.main(&driven)).unwrap_or_default();
    assert!(error.starts_with(CONTEXT_FULL) && error.ends_with("Prompt is too long"), "{error}");
    begun(&mut driven, &mut host, "go", 3);
    host.take(driven.incoming(
        &json!({ "type": "result", "subtype": "success", "is_error": false, "session_id": SESSION,
                 "result": "", "deferred_tool_use": { "id": "t1", "name": "Bash", "input": {} } }),
        at(30),
    ));
    let said = notices(host.main(&driven));
    assert!(said.iter().any(|(k, t)| k == Notice::HOOK && t.starts_with("A hook deferred Bash")));
}

/// A hook that allowed a call says so; a hook that said nothing past success stays quiet; a
/// message from another session held back says so.
#[test]
fn a_hooks_allow_and_a_held_message_say_so() {
    let (mut driven, mut host) = driven();
    let hook = |stdout: &str| {
        json!({ "type": "system", "subtype": "hook_response", "hook_id": "h", "session_id": SESSION,
                "hook_name": "PreToolUse:Bash", "hook_event": "PreToolUse", "output": "",
                "stdout": stdout, "stderr": "", "exit_code": 0, "outcome": "success" })
    };
    let allow = json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse",
                                                "permissionDecision": "allow" } });
    host.take(driven.incoming(&hook(&allow.to_string()), at(10)));
    host.take(driven.incoming(&hook(""), at(20)));
    host.take(driven.incoming(
        &json!({ "type": "system", "subtype": "peer_message_hold", "session_id": SESSION,
                 "from_name": "reviewer", "cause": "permission mode mismatch" }),
        at(30),
    ));
    let said = notices(host.main(&driven));
    assert_eq!(said[0], (Notice::ALLOWED.to_owned(), "Allowed by PreToolUse hook".to_owned()));
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].1.contains("reviewer"), "{said:?}");
}

/// What `init` says went wrong (a plugin that did not load, a server config skipped) is said
/// once, and the servers' standing is a fact of the thread.
#[test]
fn init_says_what_did_not_load_once() {
    let (mut driven, mut host) = driven();
    let init = json!({ "type": "system", "subtype": "init", "session_id": SESSION,
        "plugin_errors": [{ "plugin": "lint", "message": "bad manifest" }],
        "mcp_server_errors": [{ "name": "db", "message": "no command" }],
        "mcp_servers": [{ "name": "github", "status": "connected" },
                        { "name": "linear", "status": "failed" }] });
    host.take(driven.incoming(&init, at(10)));
    host.take(driven.incoming(&init, at(20)));
    let thread = host.main(&driven);
    let notes: Vec<&str> = thread.notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(notes.iter().filter(|n| n.contains("Failed to load plugin lint")).count(), 1);
    assert!(notes.iter().any(|n| n.starts_with("MCP server \"db\" was skipped")), "{notes:?}");
    let servers = thread.meta.facts.get("mcp-servers").map(String::as_str);
    assert_eq!(servers, Some("1 connected, 1 failed"));
}

/// A compaction that failed says why, in the TUI's short words for its known causes.
#[test]
fn a_failed_compaction_says_why() {
    let (mut driven, mut host) = driven();
    let status = |error: &str| {
        json!({ "type": "system", "subtype": "status", "status": null, "session_id": SESSION,
                "compact_result": "failed", "compact_error": error })
    };
    host.take(driven.incoming(
        &status("Compaction failed · conversation could not be reduced below the context limit"),
        at(10),
    ));
    host.take(driven.incoming(&status("Error during compaction: boom"), at(20)));
    let said: Vec<String> = notices(host.main(&driven)).into_iter().map(|(_, t)| t).collect();
    assert_eq!(
        said,
        [
            "Couldn't compact · conversation too long · /clear to start fresh",
            "Couldn't compact · Error during compaction: boom",
        ]
    );
}

/// An MCP server that fails or wants authentication is a passing note in the TUI's words; one
/// that stays as it was says nothing again.
#[test]
fn an_mcp_server_that_drops_says_so() {
    let (mut driven, mut host) = driven();
    let init = |github: &str, linear: &str| {
        json!({ "type": "system", "subtype": "init", "session_id": SESSION, "mcp_servers": [
            { "name": "github", "status": github }, { "name": "linear", "status": linear } ] })
    };
    host.take(driven.incoming(&init("connected", "needs-auth"), at(10)));
    host.take(driven.incoming(&init("failed", "needs-auth"), at(20)));
    host.take(driven.incoming(&init("failed", "needs-auth"), at(30)));
    let notes: Vec<&str> = host.main(&driven).notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(
        notes,
        ["MCP server \"linear\" needs authentication", "MCP server \"github\" disconnected"]
    );
}

/// A request that got no answer at all is tried again saying how long it waited.
#[test]
fn a_request_with_no_answer_says_how_long_it_waited() {
    let (mut driven, mut host) = driven();
    let retry = json!({ "type": "system", "subtype": "api_retry", "session_id": SESSION,
        "attempt": 2, "max_retries": 10, "retry_delay_ms": 5000,
        "no_response": { "waited_ms": 90_000, "retry_wait_ms": 120_000 } });
    host.take(driven.incoming(&retry, at(10)));
    let said = notices(host.main(&driven));
    assert!(said.iter().any(|(_, t)| t == "No response from the API after 1m 30s"), "{said:?}");
}

/// A usage limit nearly used up says so once, and drawing on usage credits says so when it
/// begins, as the TUI's notifications do.
#[test]
fn a_limit_nearly_used_and_credits_drawn_say_so() {
    let (mut driven, mut host) = driven();
    let event = |status: &str, credits: bool| {
        json!({ "type": "rate_limit_event", "session_id": SESSION, "rate_limit_info": {
            "status": status, "rateLimitType": "seven_day", "utilization": 0.8,
            "resetsAt": 1_900_000_000, "isUsingOverage": credits } })
    };
    host.take(driven.incoming(&event("allowed_warning", false), at(10)));
    host.take(driven.incoming(&event("allowed_warning", true), at(20)));
    let notes: Vec<&str> = host.main(&driven).notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(notes, ["You've used 80% of your weekly limit", "Now using usage credits"]);
}

/// An ask a plugin's run raised names the plugin under why it asks, as the TUI's dialog does.
#[test]
fn a_plugins_ask_names_the_plugin() {
    let (mut driven, mut host) = driven();
    let mut ask = can_use("p1", "Bash", &json!({ "command": "ls" }), &json!([]));
    ask["request"]["plugin"] = json!("deploy");
    host.take(driven.incoming(&ask, at(10)));
    let asked = host.main(&driven).requests.last().expect("the ask").clone();
    assert_eq!(asked.reason.as_deref(), Some("Plugin deploy"));
}

/// A side question goes as Claude Code's own `side_question`, with the exchanges before it, and
/// shows at once; its answer takes its place, and one refused, unanswered, made up in place of the
/// model's, or left when Claude Code ends says why there is none.
#[test]
fn a_side_question_is_asked_beside_and_answered() {
    let (mut driven, mut host) = driven();
    drop(driven.take_writes());
    let earlier = [("Which file?".to_owned(), "codec.rs".to_owned())];
    host.take(driven.side(intent(1), "Why that one?", &earlier, at(10)));
    let sent = written(&mut driven);
    let [request] = sent.as_slice() else { panic!("one request: {sent:?}") };
    assert_eq!(
        request["request"],
        json!({ "subtype": "side_question", "question": "Why that one?",
                "history": [{ "question": "Which file?", "response": "codec.rs" }] })
    );
    let side = &host.main(&driven).side;
    assert!(matches!(side.as_slice(), [s] if s.pending() && s.question == "Why that one?"));
    let answer = |id: &Value, said: Value| {
        json!({ "type": "control_response",
                "response": { "subtype": "success", "request_id": id, "response": said } })
    };
    host.take(driven.incoming(
        &answer(
            &request["request_id"],
            json!({
        "response": "It holds the framing.", "synthetic": false }),
        ),
        at(20),
    ));
    let side = &host.main(&driven).side;
    assert_eq!(side[0].answer.as_deref(), Some("It holds the framing."));
    assert_eq!(side[0].intent, intent(1));
    let ask = |n: u32, driven: &mut Driven, host: &mut Host| {
        host.take(driven.side(intent(n), "And then?", &[], at(30)));
        let (_, id) = last_control(driven).pop().expect("the side question");
        Value::String(id)
    };
    let id = ask(2, &mut driven, &mut host);
    let refused = json!({ "type": "control_response",
        "response": { "subtype": "error", "request_id": id, "error": "not now" } });
    host.take(driven.incoming(&refused, at(40)));
    let id = ask(3, &mut driven, &mut host);
    host.take(
        driven.incoming(&answer(&id, json!({ "response": null, "synthetic": false })), at(41)),
    );
    let id = ask(4, &mut driven, &mut host);
    let made_up = json!({ "response": "API Error: overloaded", "synthetic": true });
    host.take(driven.incoming(&answer(&id, made_up), at(42)));
    ask(5, &mut driven, &mut host);
    host.take(driven.exited(None, at(50)));
    let failed: Vec<(Option<&str>, Option<&str>)> = host
        .main(&driven)
        .side
        .iter()
        .map(|s| (s.answer.as_deref(), s.failed.as_deref()))
        .collect();
    assert_eq!(
        failed,
        [
            (Some("It holds the framing."), None),
            (None, Some("not now")),
            (None, Some("Claude Code gave no answer")),
            (None, Some("API Error: overloaded")),
            (None, Some("Claude Code ended")),
        ]
    );
    assert!(
        notices(host.main(&driven)).iter().all(|(_, t)| !t.contains("not now")),
        "said only on the card"
    );
}

/// What `driven` asked `subtype` with last, and its id; `None` when it asked nothing so.
fn asked_for(driven: &mut Driven, subtype: &str) -> Option<(String, Value)> {
    driven
        .take_writes()
        .iter()
        .filter_map(|w| serde_json::from_str::<Value>(w).ok())
        .filter(|w| w["type"] == "control_request" && w["request"]["subtype"] == subtype)
        .map(|w| (w["request_id"].as_str().unwrap_or_default().to_owned(), w["request"].clone()))
        .next_back()
}

fn success(id: &str, response: &Value) -> Value {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": id, "response": response },
    })
}

/// The person's name for a chat in Slopty goes to Claude Code's session as theirs
/// (`rename_session` from the host, naming the session), once; Claude Code saying it back
/// leaves the thread's own title alone, while a name given in Claude Code is the chat's title.
/// An empty name goes nowhere.
#[test]
fn a_name_given_in_slopty_reaches_the_session_and_one_given_in_claude_code_is_the_title() {
    let (mut driven, mut host) = driven();
    drop(driven.take_writes());
    host.take(driven.send("Fix the **lexer**: it drops tabs", &[], intent(1), at(1)));
    assert!(asked_for(&mut driven, "generate_session_title").is_none(), "no title is asked for");
    driven.rename(" Lexer ");
    let (id, request) = asked_for(&mut driven, "rename_session").expect("renamed");
    assert_eq!(request["title"], "Lexer");
    assert_eq!(request["source"], "host", "the person's own rename");
    assert_eq!(request["session_id"], SESSION);
    host.take(driven.incoming(&success(&id, &json!({})), at(2)));
    driven.rename("Lexer");
    assert!(asked_for(&mut driven, "rename_session").is_none(), "the session holds it already");
    let said = |title: &str| json!({ "type": "system", "subtype": "session_title_changed", "title": title });
    let before = host.main(&driven).meta.title.clone();
    host.take(driven.incoming(&said("Lexer"), at(3)));
    assert_eq!(host.main(&driven).meta.title, before, "the person's name stays their mark");
    host.take(driven.incoming(&said("Tabs"), at(4)));
    assert_eq!(host.main(&driven).meta.title, "Tabs", "named in Claude Code");
    driven.rename("");
    assert!(asked_for(&mut driven, "rename_session").is_none(), "Claude Code takes no empty name");
}

/// A session read again takes its `custom-title` over its `ai-title`, else its first line; a
/// name the person gave the chat in Slopty while it did not run reaches the session at start.
#[test]
fn a_session_read_again_takes_its_custom_title_over_its_ai_title() {
    let mut driven = Driven::new(SESSION, "/work", at(0));
    let mut host = Host::default();
    host.take(driven.drain());
    let prompt = json!({
        "type": "user", "uuid": "u1", "sessionId": SESSION, "timestamp": "2026-10-18T10:00:00Z",
        "message": { "role": "user", "content": "Chart:\nsales by month" },
    });
    let ai = json!({ "type": "ai-title", "aiTitle": "Monthly sales chart", "sessionId": SESSION });
    let custom = json!({ "type": "custom-title", "customTitle": "Sales", "sessionId": SESSION });
    host.take(driven.replay(&[prompt.clone(), ai.clone()], false, at(1)));
    assert_eq!(host.main(&driven).meta.title, "Monthly sales chart");
    let mut again = Driven::new(SESSION, "/work", at(0));
    let mut other = Host::default();
    other.take(again.drain());
    other.take(again.replay(&[prompt.clone(), custom, ai], false, at(1)));
    assert_eq!(other.main(&again).meta.title, "Sales");
    let mut plain = Driven::new(SESSION, "/work", at(0));
    let mut third = Host::default();
    third.take(plain.drain());
    third.take(plain.replay(&[prompt], false, at(1)));
    assert_eq!(third.main(&plain).meta.title, "Chart: sales by month");
    // A name the person gave the chat while Claude Code was not running reaches it at start.
    let mut state = third.main(&plain).clone();
    state.named = "Sales chart".to_owned();
    let mut named = Driven::of(&state, at(3));
    drop(named.take_writes());
    named.after_initialize();
    let (_, request) = asked_for(&mut named, "rename_session").expect("sent at start");
    assert_eq!(request["title"], "Sales chart");
}
