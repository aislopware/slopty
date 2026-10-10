use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::orchestration::{ReadEntry, ThreadRead, ThreadView};
use slopty_proto::thread::{
    AgentId, AskId, Cap, Changed, Choice, Clipped, ContentRef, Drive, Effect, Item, ItemBody,
    ItemId, Notice, Phase, Request, RequestState, ThreadId, ThreadMeta, ThreadState, ToolCall,
    ToolState, Turn, TurnId, TurnState, Usage, UserMessage, kind,
};

use super::*;

fn state() -> ThreadState {
    ThreadState::new(ThreadMeta {
        modes: Vec::new(),
        efforts: Vec::new(),
        id: ThreadId::derived(&["read"]),
        agent: AgentId::named(AgentId::PI),
        agent_version: "0.9".to_owned(),
        native: "s1".to_owned(),
        cwd: "/work".to_owned(),
        title: "Fix the build".to_owned(),
        terminal: None,
        parent: None,
        origin: ThreadMeta::ORCHESTRATED.to_owned(),
        forked_from: None,
        drive: Drive::named(Drive::DRIVEN),
        caps: vec![Cap::named(Cap::QUEUE)],
        models: Vec::new(),
        facts: BTreeMap::new(),
        created_ms: WallMs::from_millis(1),
    })
}

fn turn(n: u32, state: TurnState) -> Turn {
    Turn {
        id: TurnId(n),
        input: None,
        state,
        started_ms: WallMs::from_millis(u64::from(n)),
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed::default(),
        before: None,
        after: None,
    }
}

fn item(id: &str, turn: u32, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
}

fn said(words: &str) -> ItemBody {
    ItemBody::User(UserMessage {
        text: Clipped::whole(words),
        images: Vec::new(),
        command: None,
        intent: None,
    })
}

fn answer(words: &str) -> ItemBody {
    ItemBody::Text(Clipped::whole(words))
}

fn call(output: &str) -> ItemBody {
    ItemBody::Tool(Box::new(ToolCall {
        name: "bash".to_owned(),
        kind: kind::EXEC.to_owned(),
        title: "Run cargo test".to_owned(),
        input: Clipped::default(),
        state: ToolState::Failed,
        output: Some(Clipped::whole(output)),
        images: Vec::new(),
        detail: None,
        child: Some(ThreadId::derived(&["child"])),
        ended_ms: None,
    }))
}

fn complete() -> TurnState {
    TurnState::Complete
}

/// Three turns, the last under way: each says something and runs a call.
fn worked() -> ThreadState {
    let mut s = state();
    s.turns = vec![turn(1, complete()), turn(2, complete()), turn(3, TurnState::Active)];
    for n in 1..=3 {
        s.items.push(item(&format!("u{n}"), n, said(&format!("step {n}"))));
        s.items.push(item(&format!("c{n}"), n, call("error: it failed")));
        s.items.push(item(&format!("a{n}"), n, answer(&format!("did {n}"))));
    }
    s.items.push(item(
        "n3",
        3,
        ItemBody::Notice(Notice {
            kind: Notice::API_ERROR.to_owned(),
            text: Clipped::whole("overloaded"),
            retry: None,
        }),
    ));
    s.items.push(item("r3", 3, ItemBody::Reasoning(Clipped::whole("thinking"))));
    s.status.phase = Phase::Working;
    s
}

fn words(read: &ThreadRead) -> Vec<Vec<String>> {
    read.turns
        .iter()
        .map(|t| {
            t.entries
                .iter()
                .map(|e| match e {
                    ReadEntry::User(w) => format!("user: {w}"),
                    ReadEntry::Text(w) => format!("agent: {w}"),
                    ReadEntry::Tool { title, output, .. } => {
                        format!("tool: {title} -> {}", output.as_deref().unwrap_or_default())
                    }
                    ReadEntry::Notice { text, .. } => format!("notice: {text}"),
                })
                .collect()
        })
        .collect()
}

/// The messages view gives what was said, turn by turn, and nothing the agent thought or ran;
/// the activity view adds each call with the end of its output and the child it started, and
/// the agent's own notices. Either way reasoning stays out.
#[test]
fn a_read_gives_what_was_said_or_also_what_was_done() {
    let s = worked();
    let worker = WorkerId::new();
    let messages = read(&s, worker, ThreadView::Messages, None);
    assert_eq!(words(&messages)[0], ["user: step 1", "agent: did 1"]);
    assert_eq!((messages.agent.0.as_str(), messages.phase), ("pi", Phase::Working));
    assert_eq!((messages.worker, messages.title.as_str()), (worker, "Fix the build"));
    let activity = read(&s, worker, ThreadView::Activity, None);
    assert_eq!(
        words(&activity)[2],
        [
            "user: step 3",
            "tool: Run cargo test -> error: it failed",
            "agent: did 3",
            "notice: overloaded"
        ]
    );
    let ReadEntry::Tool { child, state, .. } = &activity.turns[0].entries[1] else { panic!() };
    assert_eq!((*child, state), (Some(ThreadId::derived(&["child"])), &ToolState::Failed));
    assert!(!activity.truncated && !activity.skipped);
}

/// The cursor is the last whole turn: a turn under way is given and read again until it ends,
/// and a read from the cursor gives only what came after it.
#[test]
fn a_read_goes_on_from_its_cursor_and_rereads_a_turn_under_way() {
    let mut s = worked();
    let first = read(&s, WorkerId::new(), ThreadView::Messages, None);
    assert_eq!((first.turns.len(), first.next), (3, TurnId(2)));
    let again = read(&s, WorkerId::new(), ThreadView::Messages, Some(first.next));
    assert_eq!(again.turns.iter().map(|t| t.id).collect::<Vec<_>>(), [TurnId(3)]);
    assert_eq!(again.next, TurnId(2), "the turn under way is not past");
    if let Some(t) = s.turns.last_mut() {
        t.state = complete();
    }
    let ended = read(&s, WorkerId::new(), ThreadView::Messages, Some(again.next));
    assert_eq!(ended.next, TurnId(3));
    let nothing = read(&s, WorkerId::new(), ThreadView::Messages, Some(ended.next));
    assert_eq!((nothing.turns.len(), nothing.next), (0, TurnId(3)));
}

/// A long text gives its head and a long output its tail, each marked, and the read says it
/// left something out; past the read's budget it stops at a whole turn, and the next read
/// from its cursor gives the rest. One turn always goes.
#[test]
fn a_read_is_bounded_and_says_what_it_left_out() {
    let mut s = state();
    let long = "x".repeat(TEXT_CHARS + 10);
    let printed = format!("{}END", "y".repeat(OUTPUT_CHARS));
    let turns = u32::try_from(READ_CHARS / TEXT_CHARS + 3).unwrap();
    s.turns = (1..=turns).map(|n| turn(n, complete())).collect();
    for n in 1..=turns {
        s.items.push(item(&format!("a{n}"), n, answer(&long)));
    }
    s.items.push(item("c1", 1, call(&printed)));
    let first = read(&s, WorkerId::new(), ThreadView::Activity, None);
    assert!(first.truncated);
    let ReadEntry::Text(text) = &first.turns[0].entries[0] else { panic!() };
    assert!(text.ends_with(ThreadRead::CUT) && text.chars().count() < TEXT_CHARS + 10, "{text}");
    let ReadEntry::Tool { output: Some(output), .. } = &first.turns[0].entries[1] else { panic!() };
    assert!(output.ends_with("END") && output.chars().count() <= OUTPUT_CHARS + 3, "{output}");
    assert!(first.turns.len() < usize::try_from(turns).unwrap(), "stopped at its budget");
    let rest = read(&s, WorkerId::new(), ThreadView::Activity, Some(first.next));
    assert_eq!(rest.turns.first().map(|t| t.id), Some(TurnId(first.next.0 + 1)));

    let mut clipped = state();
    clipped.turns = vec![turn(1, complete())];
    let at_source = Clipped { full: Some(ContentRef("a".to_owned())), ..Clipped::whole("head") };
    clipped.items.push(item("a", 1, ItemBody::Text(at_source)));
    let read_clipped = read(&clipped, WorkerId::new(), ThreadView::Messages, None);
    assert_eq!(words(&read_clipped)[0], [format!("agent: head{}", ThreadRead::CUT)]);
    assert!(read_clipped.truncated, "cut by the agent's adapter is cut too");
}

/// A read from a turn the worker no longer holds starts at the first it holds, and says so.
/// Only open requests come with it, each with the choices it offers, and a small question with
/// the picks its note's buttons answer by their place.
#[test]
fn a_read_says_what_it_skipped_and_offers_the_open_requests() {
    let mut s = worked();
    s.older = true;
    s.turns.remove(0);
    let from_start = read(&s, WorkerId::new(), ThreadView::Messages, None);
    assert!(from_start.skipped);
    let held = read(&s, WorkerId::new(), ThreadView::Messages, Some(TurnId(1)));
    assert!(!held.skipped, "nothing after turn 1 was let go");

    let ask = |id: &str, state: RequestState| Request {
        id: AskId(id.to_owned()),
        item: None,
        kind: Request::APPROVAL.to_owned(),
        title: "Run cargo test?".to_owned(),
        text: None,
        options: vec![Choice {
            id: "allow".to_owned(),
            label: "Allow".to_owned(),
            effect: Effect::Allow,
            scope: None,
            stops: false,
        }],
        questions: Vec::new(),
        proposed: None,
        schema_json: None,
        url: None,
        state,
        opened_ms: WallMs::ZERO,
        until_ms: None,
    };
    s.requests = vec![ask("1", RequestState::Withdrawn), ask("2", RequestState::Open)];
    let asking = read(&s, WorkerId::new(), ThreadView::Messages, None);
    assert_eq!(asking.requests.len(), 1);
    assert_eq!(asking.requests[0].ask, AskId("2".to_owned()));
    assert_eq!(asking.requests[0].choices[0].id, "allow");
    assert!(asking.requests[0].picks.is_empty(), "a yes or no takes Allow and Deny");

    let mut question = ask("3", RequestState::Open);
    question.kind = Request::QUESTION.to_owned();
    question.questions = vec![slopty_proto::thread::detail::Question {
        text: "Layout?".to_owned(),
        header: None,
        options: ["Split", "Unified"]
            .map(|label| slopty_proto::thread::detail::Offered {
                label: label.to_owned(),
                description: None,
            })
            .to_vec(),
        multi_select: false,
    }];
    s.requests = vec![question.clone()];
    let asked = read(&s, WorkerId::new(), ThreadView::Messages, None);
    let picks = &asked.requests[0].picks;
    assert_eq!(picks.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["Split", "Unified"]);
    assert_eq!(picks, &slopty_proto::thread::wire::NoteChoice::of(&question));
}
