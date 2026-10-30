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
        parent: None,
        origin: ThreadMeta::PERSON.to_owned(),
        drive: Drive::named(Drive::DRIVEN),
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
        mode: None,
        context: None,
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

/// A rewind drops the turns after it, and their items.
#[test]
fn truncation_drops_the_turns_after_it() {
    let mut state = run(&[
        Action::ItemStarted(text("pre", 0, "resumed")),
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(text("a", 1, "one")),
        Action::TurnStarted(turn(2)),
        Action::ItemStarted(text("b", 2, "two")),
    ]);
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

/// A thread removed and made again under the same id since the client's cursor is in a delta's
/// removals and its rows both: the client keeps it.
#[test]
fn a_thread_removed_and_made_again_stays_in_the_table() {
    let row = run(&[]).row(WallMs::from_millis(5));
    let mut table = TableState::default();
    table.apply(&TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 1 }, rows: vec![] });
    table.apply(&TableFrame::Delta {
        cursor: Cursor { epoch: 1, seq: 3 },
        rows: vec![row.clone()],
        removed: vec![row.id],
    });
    assert_eq!(table.rows.into_values().collect::<Vec<_>>(), [row]);
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

/// A row says the latest turn once it ended, how long it ran and whether the agent answered
/// it; the person's seen mark only moves up.
#[test]
fn a_row_carries_the_turn_that_ended_and_what_was_seen() {
    let ended = |n: u32, state: TurnState| Action::TurnEnded {
        turn: TurnId(n),
        state,
        usage: Usage::default(),
        ended_ms: WallMs::from_millis(u64::from(n) + 90_000),
    };
    let under_way = run(&[Action::TurnStarted(turn(1))]);
    assert_eq!(under_way.row(WallMs::ZERO).ended, None, "a turn under way has not ended");
    assert_eq!(under_way.row(WallMs::ZERO).began_ms, Some(WallMs::from_millis(1)));
    let mut state = run(&[Action::TurnStarted(turn(1)), ended(1, TurnState::Complete)]);
    let row = state.row(WallMs::ZERO);
    assert_eq!(row.began_ms, None, "no turn is under way");
    assert_eq!(
        row.ended,
        Some(wire::TurnEnded {
            turn: TurnId(1),
            at_ms: WallMs::from_millis(90_001),
            ran_ms: 90_000,
            answered: true,
        })
    );
    assert_eq!(row.seen, TurnId::BEFORE);
    state.apply(&Action::TurnStarted(turn(2)));
    state.apply(&ended(2, TurnState::Interrupted));
    assert_eq!(
        state.row(WallMs::ZERO).ended.map(|e| (e.turn, e.answered)),
        Some((TurnId(2), false))
    );

    state.apply(&Action::Seen(TurnId(2)));
    state.apply(&Action::Seen(TurnId(1)));
    assert_eq!(state.row(WallMs::ZERO).seen, TurnId(2), "a seen mark never moves back");
}

/// The prompt box is what the agent last said of it, a picture pasted in and all, and nothing
/// once it holds nothing; the queue is the last list said, in its order. Neither touches the
/// turns.
#[test]
fn the_prompt_box_and_the_queue_are_the_last_said() {
    let image = Image {
        digest: "ab".repeat(32),
        media_type: "image/png".to_owned(),
        bytes: 68,
        width: 1,
        height: 1,
        at: ContentRef("draft-image:2.png".to_owned()),
    };
    let draft = Draft {
        text: "see [Image #2]".to_owned(),
        images: vec![DraftImage { n: 2, image }],
        at_ms: WallMs::from_millis(5),
    };
    let queued = vec!["first".to_owned(), "then this".to_owned()];
    let mut state = run(&[
        Action::TurnStarted(turn(1)),
        Action::DraftSet(Some(draft.clone())),
        Action::QueuedSet(queued.clone()),
    ]);
    assert_eq!((state.draft.as_ref(), state.queued.as_slice()), (Some(&draft), queued.as_slice()));
    assert_eq!(state.turns.len(), 1);
    state.apply(&Action::DraftSet(None));
    state.apply(&Action::QueuedSet(vec!["then this".to_owned()]));
    assert_eq!((state.draft, state.queued), (None, vec!["then this".to_owned()]));
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
            Action::QueuedSet(vec!["next".to_owned()]),
        ];
        actions.extend(chunked("a", &words, &cuts));
        actions.push(Action::QueuedSet(Vec::new()));
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
            .map(|l| Offered { label: (*l).to_owned(), description: None, preview: None })
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
    assert_eq!(answer("Which panes?", "").parts(&asked[1]), Vec::<String>::new());

    assert_eq!(Answer::read(&asked, &Answer::choice(&asked, &given[..1])), None, "one unanswered");
    let stray = [given[0].clone(), given[1].clone(), answer("Why?", "No")];
    assert_eq!(Answer::read(&asked, &Answer::choice(&asked, &stray)), None, "one not asked");
    assert_eq!(Answer::read(&asked, "Split"), None, "words for questions that offer answers");
}

#[test]
fn a_hunk_is_headed_by_the_nearest_line_git_would_name() {
    let above =
        ["use std::io;", "", "impl Client {   ", "    fn send(&self) {", "        let x = 1;"];
    assert_eq!(detail::heading(above.into_iter()).as_deref(), Some("impl Client {"));
    assert_eq!(
        detail::heading(["$var = 1", "\tindented"].into_iter()).as_deref(),
        Some("$var = 1")
    );
    assert_eq!(detail::heading(["_private:", " x"].into_iter()).as_deref(), Some("_private:"));
    assert_eq!(detail::heading(["    fn f() {", "// note", "}"].into_iter()), None);
    assert_eq!(detail::heading(["été", "1 + 2"].into_iter()), None);
    assert_eq!(detail::heading(std::iter::empty()), None);
}

#[test]
fn a_long_heading_is_cut_at_80_bytes_then_trimmed() {
    let long = format!("fn f{} {}", "a".repeat(75), "b".repeat(20));
    let cut = detail::heading([long.as_str()].into_iter());
    assert_eq!(cut.as_deref(), long.get(..79));
    let wide = format!("fn {}", "é".repeat(60));
    let cut = detail::heading([wide.as_str()].into_iter()).unwrap_or_default();
    assert_eq!(cut.len(), 79, "a cut never splits a character");
}

#[test]
fn a_unified_header_gives_its_heading() {
    assert_eq!(
        detail::header_heading("@@ -10,3 +10,4 @@ impl Client {").as_deref(),
        Some("impl Client {")
    );
    assert_eq!(detail::header_heading("@@ -1 +1 @@"), None);
    assert_eq!(detail::header_heading("@@ -1,2 +1,2 @@   "), None);
    assert_eq!(detail::header_heading(" keep"), None);
}

/// A subagent's thread is named by its session's thread and its agent id alone, so the worker's
/// adapter that keeps it and the client that opens it from the board name the same thread. The
/// value is pinned: a change here renames every subagent thread a worker's log holds.
#[test]
fn a_subagents_thread_is_named_by_its_session_and_agent() {
    let main = ThreadId::from_uuid(Uuid::from_u128(1));
    assert_eq!(main.subagent("a1b2"), main.subagent("a1b2"));
    assert_ne!(main.subagent("a1b2"), main.subagent("a1b3"));
    assert_ne!(main.subagent("a1b2"), ThreadId::from_uuid(Uuid::from_u128(2)).subagent("a1b2"));
    assert_eq!(
        main.subagent("x"),
        ThreadId::derived(&["claude-code subagent", &main.to_string(), "x"])
    );
    // Each part goes behind its length, so parts that join alike name different threads.
    assert_ne!(ThreadId::derived(&["ab", "c"]), ThreadId::derived(&["a", "bc"]));
    assert_eq!(main.subagent("a1b2").to_string(), "1fc1bce5-1343-8f7f-9a36-75875446afad");
}

/// A thread that stopped on an error ranks above one at rest. Every rung is counted apart from
/// the others, and the ladder runs down to rest.
#[test]
fn a_failed_thread_ranks_above_rest() {
    use attention::{Counts, Rung};
    let mut state = ThreadState::new(meta());
    state.status = Status {
        phase: Phase::Done,
        wait: None,
        liveness: Liveness::Exited { resumable: true },
        since_ms: WallMs::from_millis(5),
    };
    let rung = |state: &ThreadState| Rung::of(&state.row(WallMs::ZERO));
    assert_eq!(rung(&state), Rung::Idle);
    assert_eq!(Rung::Idle.word(), None);
    assert_eq!(Rung::Waiting.word(), Some("Background work"), "its own work, not the person");
    state.status.phase = Phase::Failed;
    assert_eq!(rung(&state), Rung::Failed);
    assert_eq!(Rung::DOWN.last(), Some(&Rung::Idle));
    let mut counts = Counts::default();
    for (more, rung) in (0..).zip(Rung::DOWN) {
        counts.count(rung);
        counts.add(rung, more);
    }
    let on: Vec<u32> = Rung::DOWN.iter().map(|&rung| counts.on(rung)).collect();
    assert_eq!((on, counts.total()), (vec![1, 2, 3, 4, 5], 15));
}

/// A thread names the terminal its agent runs in by a fact, which its row carries: the session
/// it names, and none where no fact names one.
#[test]
fn a_thread_names_the_terminal_its_agent_runs_in() {
    let mut meta = meta();
    assert_eq!(meta.terminal(), None);
    let session = slopty_core::SessionId::new();
    meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), session.to_string());
    assert_eq!(meta.terminal(), Some(session));
    assert_eq!(ThreadState::new(meta.clone()).row(WallMs::ZERO).terminal(), Some(session));
    meta.facts.insert(ThreadMeta::TERMINAL_FACT.to_owned(), "tile 3".to_owned());
    assert_eq!(meta.terminal(), None);
    assert_eq!(ThreadState::new(meta).row(WallMs::ZERO).terminal(), None);
}

/// The screens an agent drives are what the worker last said, and none once it says none.
#[test]
fn the_screens_are_the_last_said() {
    let screen = |id: u32| AgentScreen {
        target: crate::screen::CaptureTarget::Window(slopty_core::WindowId(id)),
        kind: AgentScreen::BROWSER.to_owned(),
        label: format!("Chromium {id}"),
        used_ms: WallMs::ZERO,
    };
    let mut state = run(&[Action::ScreensSet(vec![screen(1)])]);
    state.apply(&Action::ScreensSet(vec![screen(2), screen(1)]));
    assert_eq!(state.screens, [screen(2), screen(1)]);
    assert_eq!(state.window(0).screens, state.screens, "a snapshot carries them");
    state.apply(&Action::ScreensSet(Vec::new()));
    assert_eq!(state.screens, []);
}

/// A blob is named as `blob:` and its id, and read back; any other content names none.
#[test]
fn a_content_ref_names_a_blob() {
    let id = "8ab686eafeb1f44702738c8b0f24f2567c36da6d";
    let blob = ContentRef::blob(id);
    assert_eq!(blob.0, format!("blob:{id}"));
    assert_eq!(blob.blob_id(), Some(id));
    assert_eq!(ContentRef("r1#2".to_owned()).blob_id(), None, "a transcript's part");
    assert_eq!(ContentRef("blob:".to_owned()).blob_id(), None, "no id");
}

/// A call's note lands only on a call the state holds, goes with it, and travels with the
/// window and the page it is in.
#[test]
fn call_notes_follow_their_calls() {
    let said = CallNote { said: Some("exit code 2".to_owned()), ..CallNote::default() };
    let noted = |id: &str| Action::CallNoted { item: ItemId(id.to_owned()), note: said.clone() };
    let mut state = run(&[
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(tool("t1", ToolState::Completed)),
        noted("t1"),
        noted("nowhere"),
    ]);
    assert_eq!(state.calls.keys().map(|k| k.0.as_str()).collect::<Vec<_>>(), ["t1"]);
    let page = state.page(TurnId(2), 5);
    assert_eq!(page.calls.len(), 1, "the page carries its calls' notes");
    let mut fresh = ThreadState::new(meta());
    fresh.prepend(&page);
    assert_eq!(fresh.calls.get(&ItemId("t1".to_owned())), Some(&said));
    state.apply(&Action::ItemRemoved { item: ItemId("t1".to_owned()) });
    assert!(state.calls.is_empty(), "{:?}", state.calls);
}

/// A note says only what it knows: a running time said after the task keeps the task, a
/// header, an ending and a run time said once stay, and what a later note says replaces them.
#[test]
fn a_call_note_keeps_what_a_later_one_leaves_unsaid() {
    let note = |n: CallNote| Action::CallNoted { item: ItemId("t1".to_owned()), note: n };
    let state = run(&[
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(tool("t1", ToolState::Running)),
        note(CallNote { task: Some("b1".to_owned()), ..CallNote::default() }),
        note(CallNote { title: Some("Read a.pdf · pages 2".to_owned()), ..CallNote::default() }),
        note(CallNote { elapsed_ms: Some(12_000), ..CallNote::default() }),
        note(CallNote { ran_ms: Some(11_400), ..CallNote::default() }),
        note(CallNote { said: Some("Read PDF (1.2MB)".to_owned()), ..CallNote::default() }),
    ]);
    let kept = state.calls.get(&ItemId("t1".to_owned())).expect("the call's note");
    assert_eq!(kept.ran_ms, Some(11_400), "the run time the hook said stays");
    assert_eq!(kept.task.as_deref(), Some("b1"));
    assert_eq!(kept.title.as_deref(), Some("Read a.pdf · pages 2"));
    assert_eq!(kept.said.as_deref(), Some("Read PDF (1.2MB)"));
    assert_eq!(kept.elapsed_ms, None, "a running time is the last note's own");
    let state = run(&[
        Action::TurnStarted(turn(1)),
        Action::ItemStarted(tool("t1", ToolState::Running)),
        note(CallNote { said: Some("Denied".to_owned()), ..CallNote::default() }),
        note(CallNote { said: Some("exit code 2".to_owned()), ..CallNote::default() }),
    ]);
    let said = state.calls.get(&ItemId("t1".to_owned())).and_then(|n| n.said.as_deref());
    assert_eq!(said, Some("exit code 2"), "a later word replaces an earlier one");
}

/// An item's asset names its item and name, and nothing else reads as one.
#[test]
fn item_assets_read_back() {
    let asset = ContentRef::item_asset("toolu_1", "models/a.glb");
    assert_eq!(asset.item_asset_of(), Some(("toolu_1", "models/a.glb")));
    assert_eq!(ContentRef("item-asset:toolu_1".to_owned()).item_asset_of(), None);
    assert_eq!(ContentRef::blob("abc").item_asset_of(), None);
}
