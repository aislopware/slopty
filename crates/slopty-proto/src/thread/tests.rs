use proptest::prelude::*;
use uuid::Uuid;

use super::wire::TableFrame;
use super::*;

fn meta() -> ThreadMeta {
    ThreadMeta {
        id: ThreadId::from_uuid(Uuid::from_u128(1)),
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
        caps: vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)],
        models: Vec::new(),
        facts: BTreeMap::new(),
        created_ms: WallMs::from_millis(1),
    }
}

fn turn(n: u32) -> Turn {
    Turn {
        id: TurnId(n),
        input: Some(ItemId(format!("u{n}"))),
        state: TurnState::Active,
        started_ms: WallMs::from_millis(u64::from(n)),
        ended_ms: None,
        usage: Usage::default(),
        models: Vec::new(),
        changed: Changed { added: n, removed: 1 },
        before: None,
        after: None,
    }
}

fn text(id: &str, turn: u32, words: &str) -> Item {
    Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(turn),
        at_ms: WallMs::ZERO,
        body: ItemBody::Text(Clipped::whole(words)),
    }
}

fn tool(id: &str, state: ToolState) -> Item {
    Item {
        id: ItemId(id.to_owned()),
        turn: TurnId(1),
        at_ms: WallMs::ZERO,
        body: ItemBody::Tool(Box::new(ToolCall {
            name: "Bash".to_owned(),
            kind: kind::EXEC.to_owned(),
            title: "Run cargo test".to_owned(),
            input: Clipped::default(),
            state,
            output: None,
            images: Vec::new(),
            detail: None,
            child: None,
            ended_ms: None,
        })),
    }
}

fn request(id: &str) -> Request {
    Request {
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
        state: RequestState::Open,
        opened_ms: WallMs::ZERO,
        until_ms: None,
    }
}

fn run(actions: &[Action]) -> ThreadState {
    let mut state = ThreadState::new(meta());
    for action in actions {
        state.apply(action);
    }
    state
}

fn state_of(state: &ThreadState, id: &str) -> ToolState {
    match &state.item(&ItemId(id.to_owned())).map(|i| &i.body) {
        Some(ItemBody::Tool(call)) => call.state.clone(),
        other => panic!("no call {id}: {other:?}"),
    }
}

/// Text grows by appends, counted as if it had come whole.
#[test]
fn appends_grow_an_item_as_if_it_came_whole() {
    let state = run(&[
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(text("a", 1, "")),
        Action::Append {
            item: ItemId("a".to_owned()),
            part: PartKey::Body,
            text: "one\ntw".to_owned(),
        },
        Action::Append {
            item: ItemId("a".to_owned()),
            part: PartKey::Body,
            text: "o\n".to_owned(),
        },
        Action::Append {
            item: ItemId("nowhere".to_owned()),
            part: PartKey::Body,
            text: "x".to_owned(),
        },
    ]);
    assert_eq!(state.items, [text("a", 1, "one\ntwo\n")], "{state:?}");
    assert_eq!(state.row(WallMs::ZERO).last_line.as_deref(), Some("two"));
}

/// A call moves forward through its states; an update never takes it back from a final one,
/// and its completion is authoritative.
#[test]
fn a_tool_call_moves_forward_only() {
    let mut state = run(&[
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(tool("t", ToolState::Streaming)),
        Action::ItemUpdated(tool("t", ToolState::Pending { ask: AskId("r".to_owned()) })),
        Action::ItemUpdated(tool("t", ToolState::Running)),
        Action::ItemUpdated(tool("t", ToolState::Streaming)),
    ]);
    assert_eq!(state_of(&state, "t"), ToolState::Running, "never back to streaming");
    state.apply(&Action::ItemUpdated(tool("t", ToolState::Rejected)));
    state.apply(&Action::ItemUpdated(tool("t", ToolState::Running)));
    assert_eq!(state_of(&state, "t"), ToolState::Rejected, "a final state holds");
    state.apply(&Action::ItemCompleted(tool("t", ToolState::Completed)));
    assert_eq!(state_of(&state, "t"), ToolState::Completed, "completion is authoritative");
    assert!(!ToolState::Completed.may_become(&ToolState::Failed));
    assert!(ToolState::Streaming.may_become(&ToolState::Completed));
}

/// Requests open and settle; the row carries only the open ones; settled ones are kept up to a
/// bound, oldest dropped first.
#[test]
fn requests_settle_and_the_settled_are_bounded() {
    let mut state = run(&[Action::RequestOpened(Box::new(request("r0")))]);
    assert_eq!(state.row(WallMs::ZERO).requests.len(), 1);
    let by = Answerer { client: None, name: "terminal".to_owned() };
    let answered = RequestState::Answered { by, choice: "allow".to_owned() };
    state.apply(&Action::RequestResolved { id: AskId("r0".to_owned()), state: answered.clone() });
    assert!(state.row(WallMs::ZERO).requests.is_empty(), "no open request");
    assert_eq!(state.requests.first().map(|r| &r.state), Some(&answered), "who answered");
    let extra = RESOLVED_KEPT.saturating_add(3);
    for n in 1..=extra {
        let id = AskId(format!("r{n}"));
        state.apply(&Action::RequestOpened(Box::new(request(&id.0))));
        state.apply(&Action::RequestResolved { id, state: RequestState::Withdrawn });
    }
    state.apply(&Action::RequestOpened(Box::new(request("open"))));
    assert_eq!(state.requests.len(), RESOLVED_KEPT.saturating_add(1));
    assert_eq!(state.requests.first().map(|r| r.id.0.as_str()), Some("r4"), "oldest dropped");
    assert_eq!(state.open_requests().count(), 1);
}

/// A rewind drops the turns after it, and their items; snapshots land on their turn's edge.
#[test]
fn truncation_and_snapshots() {
    let mut state = run(&[
        Action::ItemStarted(text("pre", 0, "resumed")),
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(text("a", 1, "one")),
        Action::TurnStarted(turn(2)),
        Action::ItemStarted(text("b", 2, "two")),
        Action::Snapshot { turn: TurnId(2), edge: Edge::Before, tree: TreeRef("t2".to_owned()) },
    ]);
    assert_eq!(
        state.turn(TurnId(2)).and_then(|t| t.before.clone()),
        Some(TreeRef("t2".to_owned()))
    );
    state.apply(&Action::Truncated { after: Some(TurnId(1)) });
    let ids: Vec<&str> = state.items.iter().map(|i| i.id.0.as_str()).collect();
    assert_eq!(ids, ["pre", "a"]);
    assert_eq!(state.turns.len(), 1);
    state.apply(&Action::Truncated { after: None });
    assert!(state.items.is_empty() && state.turns.is_empty());
}

/// A snapshot of the last turns, then pages back, give the whole thread again.
#[test]
fn a_window_and_its_pages_rebuild_the_thread() {
    let mut actions = vec![Action::ItemStarted(text("pre", 0, "resumed"))];
    for n in 1..=5 {
        actions.push(Action::TurnStarted(turn(n)));
        actions.push(Action::ItemStarted(text(&format!("i{n}"), n, "words")));
    }
    let whole = run(&actions);
    let mut client = whole.window(2);
    assert!(client.older);
    assert_eq!(client.turns.iter().map(|t| t.id.0).collect::<Vec<_>>(), [4, 5]);
    while client.older {
        let first = client.turns.first().map_or(TurnId(u32::MAX), |t| t.id);
        let page = whole.page(first, 2);
        client.prepend(&page);
    }
    assert_eq!(client, whole);
}

/// The table mirrors snapshots and deltas.
#[test]
fn the_table_takes_snapshots_and_deltas() {
    let row = run(&[]).row(WallMs::from_millis(5));
    let mut other = row.clone();
    other.id = ThreadId::from_uuid(Uuid::from_u128(2));
    let mut table = TableState::default();
    let cursor = Cursor { epoch: 1, seq: 1 };
    table.apply(&TableFrame::Snapshot { cursor, rows: vec![row.clone()] });
    let next = Cursor { epoch: 1, seq: 2 };
    table.apply(&TableFrame::Delta {
        cursor: next,
        rows: vec![other.clone()],
        removed: vec![row.id],
    });
    assert_eq!(table.cursor, next);
    assert_eq!(table.rows.into_values().collect::<Vec<_>>(), [other]);
}

/// A row says what the agent is doing while a call of its runs or waits on the person, and
/// nothing once the call is over.
#[test]
fn a_row_says_what_runs_now() {
    let running = run(&[Action::ItemStarted(tool("t1", ToolState::Running))]);
    assert_eq!(running.row(WallMs::ZERO).doing.as_deref(), Some("Run cargo test"));
    let pending = ToolState::Pending { ask: AskId("a".to_owned()) };
    let asking = run(&[Action::ItemStarted(tool("t1", pending))]);
    assert_eq!(asking.row(WallMs::ZERO).doing.as_deref(), Some("Run cargo test"));
    let done = run(&[
        Action::ItemStarted(tool("t1", ToolState::Running)),
        Action::ItemCompleted(tool("t1", ToolState::Completed)),
    ]);
    assert_eq!(done.row(WallMs::ZERO).doing, None);
}

/// Appends split one text at chosen char boundaries.
fn chunked(id: &str, words: &str, cuts: &[usize]) -> Vec<Action> {
    let chars: Vec<char> = words.chars().collect();
    let mut points: Vec<usize> =
        cuts.iter().map(|c| c.checked_rem(chars.len().saturating_add(1)).unwrap_or(0)).collect();
    points.push(0);
    points.push(chars.len());
    points.sort_unstable();
    points.dedup();
    points
        .windows(2)
        .map(|w| Action::Append {
            item: ItemId(id.to_owned()),
            part: PartKey::Body,
            text: chars.get(w[0]..w[1]).map(|s| s.iter().collect()).unwrap_or_default(),
        })
        .collect()
}

proptest! {
    /// However a text is cut into appends, the state is the one its whole would give.
    #[test]
    fn any_chunking_gives_the_same_state(
        words in "[a-z\n é]{0,40}",
        cuts in proptest::collection::vec(0_usize..64, 0..8),
    ) {
        let mut actions = vec![Action::TurnStarted(turn(1)), Action::ItemStarted(text("a", 1, ""))];
        actions.extend(chunked("a", &words, &cuts));
        let streamed = run(&actions);
        let whole = run(&[Action::TurnStarted(turn(1)), Action::ItemStarted(text("a", 1, &words))]);
        prop_assert_eq!(streamed, whole);
    }

    /// A state carried across the wire mid-stream and carried on gives the same state as one
    /// that never stopped: a follower that joins from a snapshot ends where the worker does.
    #[test]
    fn a_snapshot_mid_stream_carries_on_the_same(
        words in "[a-z\n]{1,30}",
        cuts in proptest::collection::vec(0_usize..32, 0..6),
        at in 0_usize..12,
    ) {
        let mut actions = vec![
            Action::TurnStarted(turn(1)),
            Action::ItemStarted(text("a", 1, "")),
            Action::RequestOpened(Box::new(request("r"))),
        ];
        actions.extend(chunked("a", &words, &cuts));
        actions.push(Action::RequestResolved { id: AskId("r".to_owned()), state: RequestState::Released });
        actions.push(Action::TurnEnded {
            turn: TurnId(1), state: TurnState::Complete, usage: Usage::default(),
            ended_ms: WallMs::from_millis(9),
        });
        let split = at.min(actions.len());
        let (head, tail) = actions.split_at(split);
        let snapshot = run(head);
        let bytes = crate::codec::encode_body(&snapshot).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let mut follower: ThreadState =
            crate::codec::decode_body(&bytes).map_err(|e| TestCaseError::fail(e.to_string()))?;
        for action in tail {
            follower.apply(action);
        }
        prop_assert_eq!(follower, run(&actions));
    }
}

/// One answer answers every question: a lone question that offers nothing in its words, else
/// a JSON list keyed by each question's text, read back only when it answers each question
/// asked and none other; several picks and one's own words come apart again as they went.
#[test]
fn one_answer_answers_every_question() {
    use detail::{Answer, Offered, Question};
    let ask = |text: &str, labels: &[&str], multi_select: bool| Question {
        text: text.to_owned(),
        header: None,
        options: labels
            .iter()
            .map(|l| Offered { label: (*l).to_owned(), description: None })
            .collect(),
        multi_select,
    };
    let answer = |q: &str, a: &str| Answer { question: q.to_owned(), answer: a.to_owned() };

    let free = [ask("Name it?", &[], false)];
    let named = [answer("Name it?", "Split, then join")];
    assert_eq!(Answer::choice(&free, &named), "Split, then join", "the words as they are");
    assert_eq!(Answer::read(&free, "Split, then join").unwrap(), named);

    let panes = ["Files", "Files, Terminal", "Terminal", "Editor"];
    let asked =
        [ask("Which layout?", &["Split", "Tabs"], false), ask("Which panes?", &panes, true)];
    let given =
        [answer("Which layout?", "Split"), answer("Which panes?", "Terminal, Editor, Logs")];
    let choice = Answer::choice(&asked, &given);
    assert_eq!(
        choice,
        r#"[{"question":"Which layout?","answer":"Split"},{"question":"Which panes?","answer":"Terminal, Editor, Logs"}]"#
    );
    assert_eq!(Answer::read(&asked, &choice).unwrap(), given);
    assert_eq!(given[1].parts(&asked[1]), ["Terminal", "Editor", "Logs"], "one's own words last");
    assert_eq!(answer("Which panes?", "Files, Terminal").parts(&asked[1]), ["Files, Terminal"]);
    assert_eq!(answer("Which panes?", "Files, Editor").parts(&asked[1]), ["Files", "Editor"]);
    assert!(answer("Which panes?", "").parts(&asked[1]).is_empty());

    assert_eq!(Answer::read(&asked, &Answer::choice(&asked, &given[..1])), None, "one unanswered");
    let stray = [given[0].clone(), given[1].clone(), answer("Why?", "No")];
    assert_eq!(Answer::read(&asked, &Answer::choice(&asked, &stray)), None, "one not asked");
    assert_eq!(Answer::read(&asked, "Split"), None, "words for questions that offer answers");
}
