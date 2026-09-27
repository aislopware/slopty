use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;
use slopty_proto::conversation::{Entry, Live, LiveId, LiveKind, Meters, ThreadId};

use super::*;
use crate::claude_mod;
use crate::conversation::Transcripts;

const SCENARIOS: [&str; 3] = ["bash", "think", "agent"];

fn fixture(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mod").join(scenario)
}

/// The scenario's events as the worker takes them, batch by batch.
fn batches(scenario: &str) -> Vec<Batch> {
    let text = std::fs::read_to_string(fixture(scenario).join("events.jsonl")).expect("events");
    text.lines().map(|line| serde_json::from_str(line).expect("a batch")).collect()
}

fn events(scenario: &str) -> Vec<(Value, ModEvent)> {
    batches(scenario)
        .into_iter()
        .flat_map(|batch| {
            let decoded = batch.decoded();
            batch.events.into_iter().zip(decoded)
        })
        .collect()
}

fn id(turn: &str, step: u32, block: u32) -> LiveId {
    LiveId { turn: turn.to_owned(), step, block }
}

fn at(turn: &str, step: u32, block: u32) -> At {
    At { step: Step { turn: turn.to_owned(), index: step, agent: None }, block }
}

fn text(at: At, text: &str) -> ModEvent {
    ModEvent::Text { at, text: text.to_owned() }
}

fn stop(turn: &str, step: u32) -> ModEvent {
    ModEvent::Stop(Step { turn: turn.to_owned(), index: step, agent: None })
}

fn upsert(thread: ThreadId, id: &str, body: Body) -> Change {
    Change::Upsert { thread, entry: Entry { id: id.to_owned(), at_ms: 0, body } }
}

fn plain(text: &str) -> Clipped {
    Clipped { text: text.to_owned(), lines: 1, chars: 1, full: None }
}

/// Every event the recorded mod sent decodes; only the ones the worker has no use for (the
/// turn's start, tool calls running) are `Other`, and each batch names the session it ran in.
#[test]
fn the_recorded_events_decode() {
    for scenario in SCENARIOS {
        let sessions: BTreeSet<Option<String>> =
            batches(scenario).into_iter().map(|b| b.session).collect();
        assert_eq!(sessions.len(), 1, "{scenario}: {sessions:?}");
        assert!(sessions.first().is_some_and(Option::is_some), "{scenario}: a session");
        for (raw, event) in events(scenario) {
            let kind = raw["kind"].as_str().expect("a kind");
            let unused = matches!(kind, "turn.start" | "tool.start" | "tool.end");
            assert_eq!(event == ModEvent::Other, unused, "{scenario}: {raw}");
        }
    }
    let events = events("bash");
    let Some((_, ModEvent::Hello(hello))) = events.first() else { panic!("{events:?}") };
    assert_eq!((hello.protocol, hello.claude.as_str()), (MOD_PROTOCOL, "2.1.283"));
}

/// The fixtures are the mod as it is embedded, recorded on a Claude Code the gate trusts: a
/// change to the mod, or a version added to the list, needs a new recording
/// (`cargo xtask fixtures claude-mod`).
#[test]
fn the_recording_is_of_this_mod_on_a_trusted_version() {
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("").join("recorded.json")).expect("recorded.json"),
    )
    .expect("json");
    for (path, embedded) in claude_mod::FILES {
        let copy = std::fs::read_to_string(fixture("plugin").join(path)).expect("the copy");
        assert_eq!(copy, embedded, "{path} changed since the recording");
    }
    assert_eq!(
        recorded["claude"].as_str().map(|v| vec![v]),
        Some(MOD_CLAUDE_VERSIONS.to_vec()),
        "every trusted version, and only those, was recorded"
    );
    for scenario in SCENARIOS {
        let [(_, ModEvent::Hello(hello)), ..] = &*events(scenario) else { panic!("{scenario}") };
        assert_eq!(gate(hello), Ok(()), "{scenario}");
    }
}

/// Only this protocol on a verified Claude Code is trusted.
#[test]
fn the_gate_names_what_it_refuses() {
    let hello =
        |protocol, claude: &str| Hello { protocol, claude: claude.to_owned(), session_id: None };
    assert_eq!(gate(&hello(MOD_PROTOCOL, MOD_CLAUDE_VERSIONS[0])), Ok(()));
    assert_eq!(gate(&hello(MOD_PROTOCOL + 1, MOD_CLAUDE_VERSIONS[0])), Err(Refusal::Protocol(2)));
    assert_eq!(gate(&hello(MOD_PROTOCOL, "2.1.284")), Err(Refusal::Version("2.1.284".to_owned())));
    let decoded = ModEvent::decode(&json!({"kind": "hello", "protocol": "one", "claude": "x"}));
    assert_eq!(decoded, ModEvent::Other, "a hello of another shape is no hello");
}

/// Replayed whole, the recorded events leave the blocks the model wrote, and reading the
/// transcripts the same runs wrote settles every one of them.
#[test]
fn the_transcript_settles_every_recorded_block() {
    for scenario in SCENARIOS {
        let now = Instant::now();
        let mut board = Board::default();
        for (_, event) in events(scenario) {
            if event != ModEvent::Bye {
                board.apply(&event, now);
            }
        }
        assert!(board.blocks().values().all(|b| b.stopped.is_some()), "{scenario}: all stopped");
        assert!(
            Overlay::default().update(&board, now, 0).is_empty(),
            "{scenario}: too late to show"
        );
        let mut streaming = board.clone();
        for block in streaming.blocks.values_mut() {
            block.stopped = None;
        }
        let mut overlay = Overlay::default();
        let shown = overlay.update(&streaming, now, 0);
        let starts = shown.iter().filter(|l| matches!(l, Live::Start { .. })).count();
        assert_eq!(starts, board.blocks().len(), "{scenario}");

        let dir = fixture(scenario);
        let subagents: Vec<PathBuf> = std::fs::read_dir(dir.join("subagents"))
            .map(|entries| entries.map(|e| e.expect("an entry").path()).collect())
            .unwrap_or_default();
        let changes = Transcripts::default().read(&dir.join("transcript.jsonl"), &subagents);
        let cleared = overlay.settle(&changes);
        assert_eq!(cleared.len(), starts, "{scenario}: {cleared:?}");
        assert!(overlay.is_empty(), "{scenario}: {overlay:?}");
        assert!(overlay.update(&streaming, now, 0).is_empty(), "{scenario}: settled stay settled");
    }
}

/// What the bash run's blocks were: its answer, then the call with its input, then the answer
/// after the tool; a subagent's text is in its own thread.
#[test]
fn blocks_carry_their_thread_kind_and_text() {
    let now = Instant::now();
    let mut board = Board::default();
    for (_, event) in events("bash") {
        board.apply(&event, now);
        if event == ModEvent::Bye {
            assert!(board.blocks().is_empty(), "the session ended");
        }
    }
    let mut board = Board::default();
    for (_, event) in events("bash").into_iter().filter(|(_, e)| *e != ModEvent::Bye) {
        board.apply(&event, now);
    }
    let blocks: Vec<(&LiveKind, &str)> =
        board.blocks().values().map(|b| (&b.kind, b.text.as_str())).collect();
    let bash = LiveKind::Tool { id: "toolu_fake1".to_owned(), name: "Bash".to_owned() };
    assert_eq!(
        blocks,
        [
            (&LiveKind::Text, "Let me run it."),
            (&bash, r#"{"command": "echo hi", "description": "Say hi"}"#),
            (&LiveKind::Text, "Done: the command said hi."),
        ]
    );
    let mut board = Board::default();
    for (_, event) in events("agent").into_iter().filter(|(_, e)| *e != ModEvent::Bye) {
        board.apply(&event, now);
    }
    let sub = ThreadId::Agent("a0000000000000001".to_owned());
    let texts: Vec<&str> =
        board.blocks().values().filter(|b| b.thread == sub).map(|b| b.text.as_str()).collect();
    assert_eq!(texts, ["Subagent found it."]);
}

/// A follower is sent a block's start once and then only what it grew by; a block the board
/// lets go is cleared.
#[test]
fn a_follower_gets_only_what_is_new() {
    let now = Instant::now();
    let mut board = Board::default();
    let mut overlay = Overlay::default();
    board.apply(&text(at("t", 0, 0), "Sun"), now);
    assert_eq!(
        overlay.update(&board, now, 0),
        [
            Live::Start { thread: ThreadId::Main, id: id("t", 0, 0), kind: LiveKind::Text },
            Live::Append { id: id("t", 0, 0), text: "Sun".to_owned() },
        ]
    );
    assert!(overlay.update(&board, now, 0).is_empty(), "nothing new");
    board.apply(&text(at("t", 0, 0), "day"), now);
    board.apply(&text(at("t", 0, 0), " noon"), now);
    assert_eq!(
        overlay.update(&board, now, 0),
        [Live::Append { id: id("t", 0, 0), text: "day noon".to_owned() }],
        "two pieces between looks come as one"
    );
    board.apply(&ModEvent::Bye, now);
    assert_eq!(overlay.update(&board, now, 0), [Live::Clear { id: id("t", 0, 0) }]);
}

/// The answer's entry settles its block and nothing else; a block no entry matches goes after
/// the grace that follows its stop, and a stopped block leaves the board in time.
#[test]
fn blocks_settle_on_their_entry_or_after_the_grace() {
    let now = Instant::now();
    let mut board = Board::default();
    let mut overlay = Overlay::default();
    board.apply(&text(at("t", 0, 0), "Hello there."), now);
    board.apply(&ModEvent::Thinking { at: at("t", 0, 1), text: "hmm".to_owned() }, now);
    overlay.update(&board, now, 0);
    let other = upsert(ThreadId::Main, "u0:0", Body::Text(plain("Something else.")));
    let elsewhere =
        upsert(ThreadId::Agent("a".to_owned()), "u1:0", Body::Text(plain("Hello there.")));
    assert!(overlay.settle(&[other, elsewhere]).is_empty(), "another text, another thread");
    let answer = upsert(ThreadId::Main, "u2:0", Body::Text(plain(" Hello there.\n")));
    assert_eq!(overlay.settle(&[answer]), [Live::Clear { id: id("t", 0, 0) }]);

    assert!(overlay.expire(now + SETTLE_GRACE).is_empty(), "the thinking's step has not stopped");
    let later = now + Duration::from_secs(1);
    board.apply(&stop("t", 0), later);
    overlay.update(&board, later, 0);
    assert!(overlay.expire(later + SETTLE_GRACE / 2).is_empty());
    assert_eq!(overlay.expire(later + SETTLE_GRACE), [Live::Clear { id: id("t", 0, 1) }]);
    assert!(overlay.update(&board, later + SETTLE_GRACE, 0).is_empty(), "not shown again");

    let gone = later + KEEP_STOPPED;
    board.apply(&ModEvent::Other, gone);
    assert!(board.blocks().is_empty(), "stopped blocks leave the board");
    let mut silent = Board::default();
    silent.apply(&text(at("u", 0, 0), "and then"), now);
    silent.apply(&ModEvent::Other, now + KEEP_SILENT);
    assert!(silent.blocks().is_empty(), "a block that never stopped leaves in time");
}

/// A tool block settles on the call with its id; a clipped answer settles on its head.
#[test]
fn a_call_settles_by_its_id_and_a_long_answer_by_its_head() {
    let now = Instant::now();
    let mut board = Board::default();
    let mut overlay = Overlay::default();
    let call =
        ModEvent::Tool { at: at("t", 0, 1), id: "toolu_1".to_owned(), name: "Bash".to_owned() };
    board.apply(&call, now);
    board.apply(&text(at("t", 0, 0), "a long answer"), now);
    overlay.update(&board, now, 0);
    let clipped = Clipped {
        text: "a long…".to_owned(),
        lines: 1,
        chars: 13,
        full: Some(slopty_proto::conversation::TextRef {
            record: "u".to_owned(),
            part: slopty_proto::conversation::Part::Block { index: 0 },
        }),
    };
    let tool = crate::conversation::proposed("Bash", &json!({"command": "ls"}));
    let call = slopty_proto::conversation::ToolCall {
        name: "Bash".to_owned(),
        detail: tool,
        result: None,
    };
    let settled = overlay.settle(&[
        upsert(ThreadId::Main, "toolu_2", Body::Tool(Box::new(call.clone()))),
        upsert(ThreadId::Main, "u:0", Body::Text(clipped)),
        upsert(ThreadId::Main, "toolu_1", Body::Tool(Box::new(call))),
    ]);
    assert_eq!(settled, [Live::Clear { id: id("t", 0, 0) }, Live::Clear { id: id("t", 0, 1) }]);
}

/// The measure's context and cost go onto the status line's meters, which keep the rest.
#[test]
fn a_measure_updates_the_meters() {
    let measure = Measure {
        context: Some(Context { percent: 12.5, window: 200_000 }),
        cost: Some(Cost { usd: 0.5 }),
    };
    let status =
        Meters { model: Some("Opus".to_owned()), cost_usd: Some(0.1), ..Meters::default() };
    let meters = measure.onto(Some(status));
    assert_eq!(meters.model.as_deref(), Some("Opus"));
    assert_eq!(
        (meters.context_used_pct, meters.context_window, meters.cost_usd),
        (Some(12.5), Some(200_000), Some(0.5))
    );
    assert_eq!(Measure::default().onto(None), Meters::default());
}

/// An entry stamped well before the follower first saw the block is an older one and settles
/// nothing; the whole answer settles a block whose last pieces have not come yet.
#[test]
fn only_a_fresh_entry_settles_and_a_partial_block_settles_on_its_whole() {
    let now = Instant::now();
    let wall = 1_800_000_000_000;
    let mut board = Board::default();
    let mut overlay = Overlay::default();
    board.apply(&ModEvent::Thinking { at: at("t", 0, 0), text: "hmm".to_owned() }, now);
    board.apply(&text(at("t", 0, 1), "Hello th"), now);
    overlay.update(&board, now, wall);
    let stamped = |id: &str, at_ms, body| Change::Upsert {
        thread: ThreadId::Main,
        entry: Entry { id: id.to_owned(), at_ms, body },
    };
    let old = [
        stamped("u0:0", wall - 60_000, Body::Thinking(plain("earlier"))),
        stamped("u0:1", wall - 60_000, Body::Text(plain("Hello there, again."))),
    ];
    assert!(overlay.settle(&old).is_empty(), "entries from a minute before");
    let fresh = [
        stamped("u1:0", wall + 400, Body::Thinking(plain("A summary of the thinking."))),
        stamped("u1:1", wall - 500, Body::Text(plain("Hello there."))),
    ];
    assert_eq!(
        overlay.settle(&fresh),
        [Live::Clear { id: id("t", 0, 0) }, Live::Clear { id: id("t", 0, 1) }]
    );
    assert!(!same_text("", &plain("anything")), "an empty block matches nothing");
}
