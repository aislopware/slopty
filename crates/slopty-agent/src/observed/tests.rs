use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use slopty_proto::agent::{AgentKind, AgentSource};
use slopty_proto::thread::ThreadState;

use super::*;
use crate::Hook;
use crate::conversation::{Conversation, Transcripts};
use crate::live::{Board, ModEvent};
use crate::transcript::Tail;

const CONVERSATIONS: [&str; 6] =
    ["edit", "tools", "interrupt", "compact", "permission", "background"];

fn dir(kind: &str, scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(kind).join(scenario)
}

fn subagents(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir.join("subagents"))
        .map(|entries| entries.map(|e| e.expect("an entry").path()).collect())
        .unwrap_or_default();
    found.sort();
    found
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

    fn thread(&self, id: ThreadId) -> &ThreadState {
        self.threads.get(&id).expect("a thread")
    }
}

fn observed() -> Observed {
    Observed::new("00000000-0000-4000-8000-000000000001", "2.1.286", None, "/work", WallMs::ZERO)
}

/// A scenario's transcripts read whole, through the adapter.
fn replay(dir: &Path) -> (Observed, Host, Transcripts) {
    let mut transcripts = Transcripts::default();
    let changes = transcripts.read(&dir.join("transcript.jsonl"), &subagents(dir));
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.transcript(&changes, &[]));
    (observed, host, transcripts)
}

/// Every entry the decoder reads is an item of its thread, in order with its id; each sits in
/// the turn of the prompt before it; every turn the transcript closed is ended.
#[test]
fn every_entry_is_an_item_in_its_prompts_turn() {
    for scenario in CONVERSATIONS {
        let dir = dir("conversation", scenario);
        let (observed, host, transcripts) = replay(&dir);
        let conversation = transcripts.conversation();
        for thread in conversation.threads() {
            let id = match thread {
                conv::ThreadId::Main => observed.main(),
                conv::ThreadId::Agent(agent) => observed.main().subagent(agent),
            };
            let state = host.thread(id);
            let entries = conversation.entries(thread);
            let ids: Vec<&str> = state.items.iter().map(|i| i.id.0.as_str()).collect();
            let want: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, want, "{scenario} {thread:?}");
            let mut turn = TurnId::BEFORE;
            for (entry, item) in entries.iter().zip(&state.items) {
                if matches!(entry.body, Body::Prompt(_)) {
                    turn = turn.next();
                }
                assert_eq!(item.turn, turn, "{scenario} {}", entry.id);
            }
            let closed = conversation
                .turns(thread)
                .iter()
                .filter(|t| t.ended_ms.is_some() && !t.prompt.is_empty())
                .count();
            let ended = state.turns.iter().filter(|t| t.ended_ms.is_some()).count();
            assert_eq!(ended, closed, "{scenario} {thread:?}: turns ended");
            assert!(
                state.turns.iter().all(|t| t.ended_ms.is_none() || t.state != TurnState::Active)
            );
        }
    }
}

/// A tool call's kind comes from what it is, and an edit's lines count toward its turn.
#[test]
fn tools_are_kinded_and_edits_counted() {
    let (observed, host, _) = replay(&dir("conversation", "edit"));
    let main = host.thread(observed.main());
    let kinds: Vec<&str> = main
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Tool(call) => Some(call.kind.as_str()),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&kind::EDIT), "{kinds:?}");
    let changed = main.turns.iter().map(|t| t.changed.added + t.changed.removed).sum::<u32>();
    assert!(changed > 0, "the edits count: {:?}", main.turns);
    assert!(main.row(WallMs::ZERO).changed.added > 0);
    assert!(!main.meta.title.is_empty(), "titled from the first prompt");
}

/// A subagent is a thread of its own, hung off the call that started it, which names it.
#[test]
fn a_subagent_is_a_linked_thread() {
    let (observed, host, _) = replay(&dir("conversation", "tools"));
    let children: Vec<(ItemId, ThreadId)> = host
        .thread(observed.main())
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Tool(call) => call.child.map(|c| (i.id.clone(), c)),
            _ => None,
        })
        .collect();
    assert!(!children.is_empty(), "an Agent call names its thread");
    for (call, child) in children {
        let sub = host.thread(child);
        let parent = sub.meta.parent.clone().expect("a parent");
        assert_eq!((parent.thread, parent.item), (observed.main(), call));
        assert_eq!(sub.meta.origin, ThreadMeta::SUBAGENT);
        assert_ne!(sub.items, Vec::<Item>::new());
    }
}

/// Read a line at a time, the transcript gives the threads it gives read whole.
#[test]
fn read_a_line_at_a_time_it_ends_the_same() {
    for scenario in CONVERSATIONS {
        let source = dir("conversation", scenario).join("transcript.jsonl");
        let text = std::fs::read_to_string(&source).expect("a transcript");
        let scratch = tempfile::tempdir().expect("a temp dir");
        let path = scratch.path().join("s.jsonl");
        let mut file = std::fs::File::create(&path).expect("made");
        let (mut tail, mut conversation) = (Tail::default(), Conversation::default());
        let mut slow = observed();
        let mut streamed = Host::default();
        streamed.take(slow.drain());
        for line in text.lines() {
            writeln!(file, "{line}").expect("written");
            let changes = conversation.read(&mut tail, &path).expect("read");
            streamed.take(slow.transcript(&changes, &[]));
        }
        let changes = Conversation::default().read(&mut Tail::default(), &source).expect("read");
        let mut whole = observed();
        let mut at_once = Host::default();
        at_once.take(whole.drain());
        at_once.take(whole.transcript(&changes, &[]));
        let main = observed().main();
        assert_eq!(streamed.thread(main), at_once.thread(main), "{scenario}");
    }
}

/// The mod's blocks show as items while they are written, and once the transcript settles
/// them they are gone, leaving what the transcript alone gives.
#[test]
fn live_blocks_stream_then_the_transcript_settles_them() {
    for scenario in ["bash", "think", "agent"] {
        let dir = dir("mod", scenario);
        let text = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        let now = Instant::now();
        let mut board = Board::default();
        let mut live = observed();
        let mut host = Host::default();
        host.take(live.drain());
        let mut appended = 0;
        for line in text.lines() {
            let batch: live::Batch = serde_json::from_str(line).expect("a batch");
            for event in batch.decoded().iter().filter(|e| {
                !matches!(e, ModEvent::Bye | ModEvent::Stop(_) | ModEvent::TurnComplete { .. })
            }) {
                board.apply(event, now);
            }
            let outs = live.live(&board, now, WallMs::ZERO);
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
        assert!(appended > 0, "{scenario}: text streamed");
        let provisional = |host: &Host| {
            host.threads
                .values()
                .flat_map(|t| &t.items)
                .filter(|i| i.id.0.starts_with("live:"))
                .count()
        };
        assert!(provisional(&host) > 0, "{scenario}: live items shown");
        let changes = Transcripts::default().read(&dir.join("transcript.jsonl"), &subagents(&dir));
        host.take(live.transcript(&changes, &[]));
        assert_eq!(provisional(&host), 0, "{scenario}: every live item settled");
        let (_, alone, _) = replay(&dir);
        for (id, state) in &alone.threads {
            assert_eq!(host.thread(*id).items, state.items, "{scenario}: as the transcript alone");
        }
    }
}

/// A held prompt is a request with the answers Claude Code takes, each mapping back to the
/// verdict it stands for; settled, it says who answered.
#[test]
fn a_permission_prompt_is_a_request_with_claudes_answers() {
    let text = std::fs::read_to_string(dir("conversation", "permission").join("hooks.jsonl"))
        .expect("hooks");
    let hook: Hook = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|r| serde_json::from_value::<Hook>(r["input"].clone()).ok())
        .find(|h| h.event == crate::HookEvent::PermissionRequest)
        .expect("a permission request");
    let session = SessionId::nil();
    let prompt = crate::permission::prompt(
        session,
        7,
        &hook,
        WallMs::from_millis(1),
        WallMs::from_millis(9),
    );
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.permission(&PermissionEvent::Asked(Box::new(prompt.clone()))));
    let main = host.thread(observed.main());
    let request = main.open_requests().next().expect("open");
    assert_eq!(request.id, AskId("7".to_owned()));
    let ids: Vec<&str> = request.options.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&"allow"));
    assert!(ids.contains(&"deny") && ids.contains(&"deny-stop"), "{ids:?}");
    assert_eq!(ids.contains(&"always"), !prompt.suggestions.is_empty());
    let always = request.options.iter().find(|c| c.id == "always").expect("always");
    assert_eq!(always.scope.as_deref(), Some("/work; accept edits mode"));
    for choice in &request.options {
        let verdict = verdict(&choice.id, Some("no")).expect("a verdict");
        assert_eq!(choice_of(&verdict), choice.id);
    }
    let by = ClientId::from_uuid(uuid::Uuid::from_u128(5));
    let outcome = Settled::Answered { verdict: Verdict::Allow, by };
    host.take(observed.permission(&PermissionEvent::Settled { session, ask: 7, outcome }));
    let main = host.thread(observed.main());
    assert_eq!(main.open_requests().count(), 0);
    let RequestState::Answered { by: answered, choice } = &main.requests[0].state else { panic!() };
    assert_eq!((answered.client, choice.as_str()), (Some(by), "allow"));
}

/// "Always allow" names a mode in words, sentence case over the line, and an unknown mode or
/// update kind still reads as words.
#[test]
fn always_allow_says_a_mode_in_words() {
    let said = |granted: Vec<Grant>| {
        let suggestions = granted
            .into_iter()
            .map(|grant| conv::Suggestion { grant, destination: None })
            .collect();
        let prompt = PermissionPrompt {
            session: SessionId::nil(),
            ask: 1,
            tool: "Bash".to_owned(),
            detail: conv::ToolDetail::Other {
                input: conv::Clipped { text: "{}".to_owned(), lines: 1, chars: 2, full: None },
            },
            suggestions,
            mode: None,
            asked_ms: WallMs::from_millis(1),
            until_ms: WallMs::from_millis(2),
        };
        grants(&prompt)
    };
    let mode = |mode: &str| Grant::Mode { mode: mode.to_owned() };
    let dirs = Grant::Directories { directories: vec!["/work".to_owned()] };
    let rules = Grant::Rules { behavior: "allow".to_owned(), rules: vec!["Bash(ls:*)".to_owned()] };
    assert_eq!(said(vec![mode("acceptEdits")]).as_deref(), Some("Accept edits mode"));
    assert_eq!(
        said(vec![dirs.clone(), mode("acceptEdits")]).as_deref(),
        Some("/work; accept edits mode")
    );
    assert_eq!(said(vec![rules, mode("plan")]).as_deref(), Some("Bash(ls:*); plan mode"));
    assert_eq!(
        said(vec![mode("someFutureMode2X"), dirs]).as_deref(),
        Some("Some future mode2 x mode; /work")
    );
    assert_eq!(said(vec![mode("auto_review")]).as_deref(), Some("Auto review mode"));
    assert_eq!(
        said(vec![Grant::Other { kind: "removeRules".to_owned() }]).as_deref(),
        Some("Remove rules")
    );
    assert_eq!(said(vec![mode(""), mode("  ")]), None);
    assert_eq!(said(Vec::new()), None);
}

/// The tracker's status maps to the phase the ladder ranks, with what it waits on.
#[test]
fn the_status_maps_to_a_phase() {
    let event = |status| AgentEvent {
        session: SessionId::nil(),
        kind: AgentKind::ClaudeCode,
        status,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::from_millis(3),
        mode: None,
    };
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    let cases = [
        (AgentStatus::Working, Phase::Working),
        (
            AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() }),
            Phase::NeedsYou,
        ),
        (AgentStatus::Done, Phase::Done),
        (AgentStatus::Failed { error: "overloaded".to_owned(), until_ms: None }, Phase::Failed),
        (AgentStatus::Waiting { tasks: 2, crons: 0 }, Phase::Waiting),
        (AgentStatus::None, Phase::Idle),
    ];
    for (status, phase) in cases {
        host.take(observed.status(&event(status)));
        assert_eq!(host.thread(observed.main()).status.phase, phase);
    }
    let status = &host.thread(observed.main()).status;
    assert_eq!(status.liveness, Liveness::Exited { resumable: true });
    host.take(observed.cwd("/work/next"));
    assert_eq!(host.thread(observed.main()).meta.cwd, "/work/next", "the directory moved");
    assert!(observed.cwd("/work/next").is_empty(), "and only once");
}

/// What the agent asks in its own terminal with no prompt held here is still a request on
/// the thread, answered only there, opened once the block has waited [`ASK_GRACE`] for a held
/// prompt, and settled when the block ends. A block a held prompt stands for asks nothing
/// there, before the prompt comes, while it is open, or after it is settled.
#[test]
fn a_block_with_no_prompt_held_asks_in_the_terminal() {
    let event = |status, detail: Option<&str>, since| AgentEvent {
        session: SessionId::nil(),
        kind: AgentKind::ClaudeCode,
        status,
        agent_session: None,
        detail: detail.map(str::to_owned),
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::from_millis(since),
        mode: None,
    };
    let after = |since: u64| WallMs::from_millis(since + 400);
    let question = || AgentStatus::Blocked(BlockReason::Question);
    let bash = || AgentStatus::Blocked(BlockReason::Permission { tool: "Bash".to_owned() });
    let native = "00000000-0000-4000-8000-000000000001";
    let mut observed = Observed::new(native, "", Some(SessionId::new()), "/work", WallMs::ZERO);
    let mut host = Host::default();
    host.take(observed.drain());
    let open = |host: &Host, observed: &Observed| -> Vec<Request> {
        host.thread(observed.main()).open_requests().cloned().collect()
    };

    let asked = "How should the test wait?";
    host.take(observed.status(&event(question(), Some(asked), 5)));
    host.take(observed.waited(WallMs::from_millis(404)));
    assert!(open(&host, &observed).is_empty(), "a held prompt may still come");
    host.take(observed.waited(after(5)));
    let [request] = open(&host, &observed).try_into().expect("one request");
    assert_eq!((request.kind.as_str(), request.title.as_str()), (Request::QUESTION, asked));
    assert!(request.options.is_empty() && request.questions.is_empty(), "answered only there");
    host.take(observed.status(&event(question(), Some(asked), 5)));
    host.take(observed.waited(after(9)));
    assert_eq!(host.thread(observed.main()).requests.len(), 1, "told once");
    host.take(observed.status(&event(AgentStatus::Working, None, 9)));
    assert_eq!(open(&host, &observed), Vec::<Request>::new());
    let by = Answerer { client: None, name: IN_TERMINAL.to_owned() };
    let answered = RequestState::Answered { by, choice: String::new() };
    assert_eq!(host.thread(observed.main()).requests[0].state, answered);

    // A prompt held for the block comes before the grace is out: no card but its own.
    let text = std::fs::read_to_string(dir("conversation", "permission").join("hooks.jsonl"))
        .expect("hooks");
    let hook: Hook = text
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|r| serde_json::from_value::<Hook>(r["input"].clone()).ok())
        .find(|h| h.event == crate::HookEvent::PermissionRequest)
        .expect("a permission request");
    let session = SessionId::nil();
    let prompt = |ask| {
        crate::permission::prompt(
            session,
            ask,
            &hook,
            WallMs::from_millis(1),
            WallMs::from_millis(99),
        )
    };
    host.take(observed.status(&event(bash(), None, 20)));
    host.take(observed.permission(&PermissionEvent::Asked(Box::new(prompt(7)))));
    host.take(observed.waited(after(20)));
    let ids: Vec<String> = open(&host, &observed).into_iter().map(|r| r.id.0).collect();
    assert_eq!(ids, ["7"], "the held prompt alone");
    let by = ClientId::from_uuid(uuid::Uuid::from_u128(5));
    let outcome = Settled::Answered { verdict: Verdict::Allow, by };
    host.take(observed.permission(&PermissionEvent::Settled { session, ask: 7, outcome }));
    host.take(observed.status(&event(bash(), None, 20)));
    host.take(observed.waited(after(20)));
    assert!(open(&host, &observed).is_empty(), "the block was the prompt's");
    assert_eq!(host.thread(observed.main()).requests.len(), 2, "and no card was made for it");

    // A held prompt that comes late takes the terminal's card's place.
    host.take(observed.status(&event(AgentStatus::Working, None, 30)));
    host.take(observed.status(&event(bash(), None, 40)));
    host.take(observed.waited(after(40)));
    assert_eq!(open(&host, &observed)[0].kind, Request::APPROVAL);
    host.take(observed.permission(&PermissionEvent::Asked(Box::new(prompt(8)))));
    let ids: Vec<String> = open(&host, &observed).into_iter().map(|r| r.id.0).collect();
    assert_eq!(ids, ["8"]);
    let outcome = Settled::Withdrawn;
    host.take(observed.permission(&PermissionEvent::Settled { session, ask: 8, outcome }));

    // A block nobody answers ends withdrawn.
    host.take(observed.status(&event(AgentStatus::Working, None, 50)));
    host.take(observed.status(&event(question(), Some(asked), 60)));
    host.take(observed.waited(after(60)));
    assert_eq!(open(&host, &observed).len(), 1, "a new block asks again");
    host.take(observed.status(&event(AgentStatus::Idle, None, 70)));
    let last = host.thread(observed.main()).requests.last().cloned().expect("a request");
    assert_eq!(last.state, RequestState::Withdrawn, "the agent stopped asking");
}

/// A thread's id is the session's own, so a restart finds the same thread, and a clipped
/// text's reference resolves back to where the transcript has it.
#[test]
fn ids_and_references_are_stable() {
    assert_eq!(observed().main(), observed().main());
    assert_ne!(thread_of("a"), thread_of("b"));
    assert_ne!(thread_of("a").subagent("x"), thread_of("b").subagent("x"));
    let at = TextRef { record: "r1".to_owned(), part: conv::Part::Block { index: 2 } };
    let thread = conv::ThreadId::Agent("a1".to_owned());
    assert_eq!(text_ref(&content_ref(&thread, &at)), Some((thread, at)));
}

/// Before any prompt the thread is named by the session's own name in the terminal title, at
/// once and as it changes; the bare program name names nothing; the first prompt then names
/// it for good.
#[test]
fn a_thread_is_named_by_the_session_until_its_first_prompt() {
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.title("✳ Claude Code"));
    assert_eq!(host.thread(observed.main()).meta.title, "", "the program's name is no name");
    host.take(observed.title("✳ Flaky test hunt"));
    assert_eq!(host.thread(observed.main()).meta.title, "Flaky test hunt");
    host.take(observed.title("◐ Flaky test hunt, round two"));
    assert_eq!(host.thread(observed.main()).meta.title, "Flaky test hunt, round two");
    host.take(observed.title("~/work"));
    assert_eq!(host.thread(observed.main()).meta.title, "Flaky test hunt, round two");

    let dir = dir("conversation", "edit");
    let mut transcripts = Transcripts::default();
    let changes = transcripts.read(&dir.join("transcript.jsonl"), &subagents(&dir));
    host.take(observed.transcript(&changes, &[]));
    let titled = host.thread(observed.main()).meta.title.clone();
    assert_ne!(titled, "Flaky test hunt, round two", "the first prompt names it");
    host.take(observed.title("✳ Something else"));
    assert_eq!(host.thread(observed.main()).meta.title, titled, "and keeps it");
}

/// A thread declares approvals once a hook has been heard, since only the hook holds a prompt,
/// and says so once; before that its row is the one that offers to install the hooks.
#[test]
fn approvals_come_with_the_hooks() {
    let approvals = Cap::named(Cap::APPROVALS);
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    assert!(!host.thread(observed.main()).meta.caps.contains(&approvals), "no hook heard");
    let titled = AgentEvent {
        session: SessionId::nil(),
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Working,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Title,
        since_ms: WallMs::from_millis(3),
        mode: None,
    };
    host.take(observed.status(&titled));
    assert!(!host.thread(observed.main()).meta.caps.contains(&approvals), "a title is no hook");
    host.take(observed.status(&AgentEvent { source: AgentSource::Hook, ..titled }));
    let caps = &host.thread(observed.main()).meta.caps;
    assert!(caps.contains(&approvals), "{caps:?}");
    assert!(caps.is_sorted(), "{caps:?}");
    assert!(observed.hooked().is_empty(), "and only once");
}

/// A Claude Code with no session id yet has a thread named by its terminal, apart from every
/// other terminal's, and with no native id.
#[test]
fn a_claude_code_with_no_session_id_is_named_by_its_terminal() {
    let terminal = SessionId::from_uuid(uuid::Uuid::from_u128(9));
    let mut observed = Observed::provisional("", terminal, "/work", WallMs::ZERO);
    let outs = observed.drain();
    let [Out::Begin(meta)] = outs.as_slice() else { panic!("{outs:?}") };
    assert!(observed.is_provisional() && meta.native.is_empty());
    assert_eq!((meta.id, meta.terminal), (terminal_thread(terminal), Some(terminal)));
    assert_ne!(terminal_thread(terminal), terminal_thread(SessionId::nil()));
    assert!(!self::observed().is_provisional(), "a session's own thread is not provisional");
}

/// The questionnaire's one answer to an `AskUserQuestion` (`detail::Answer::choice`) is read
/// as the answers Claude Code takes, each keyed by its question, picks joined as they came.
#[test]
fn a_questionnaires_answer_is_the_answers_claude_code_takes() {
    use slopty_proto::thread::detail::{Answer, Offered, Question};
    let ask = |text: &str, labels: &[&str]| Question {
        text: text.to_owned(),
        header: None,
        options: labels
            .iter()
            .map(|l| Offered { label: (*l).to_owned(), description: None })
            .collect(),
        multi_select: true,
    };
    let questions = [ask("Which layout?", &["Split", "Tabs"]), ask("Which panes?", &["Files"])];
    let given = [
        Answer { question: "Which layout?".to_owned(), answer: "Split".to_owned() },
        Answer { question: "Which panes?".to_owned(), answer: "Files, Logs".to_owned() },
    ];
    let choice = Answer::choice(&questions, &given);
    let Some(Verdict::Answer { answers }) = verdict(&choice, None) else { panic!("{choice}") };
    let read: Vec<(&str, &str)> =
        answers.iter().map(|a| (a.question.as_str(), a.answer.as_str())).collect();
    assert_eq!(read, [("Which layout?", "Split"), ("Which panes?", "Files, Logs")]);
    assert_eq!(choice_of(&Verdict::Answer { answers }), choice, "and said back the same");
}

/// An API error Claude Code retries is a notice that says which attempt comes next, out of how
/// many and when, as its transcript says them.
#[test]
fn an_api_error_it_retries_says_the_attempt() {
    let records = [
        serde_json::json!({"type": "user", "uuid": "p1", "parentUuid": null,
            "timestamp": "2026-09-27T03:15:25.849Z",
            "message": {"role": "user", "content": "Fix the build"}}),
        serde_json::json!({"type": "system", "subtype": "api_error", "uuid": "e1",
            "parentUuid": "p1", "timestamp": "2026-09-27T03:15:40.000Z",
            "error": {"error": {"type": "overloaded_error", "message": "Overloaded"}},
            "retryInMs": 1_084.6, "retryAttempt": 2, "maxRetries": 10}),
    ];
    let scratch = tempfile::tempdir().expect("a temp dir");
    let path = scratch.path().join("s.jsonl");
    let text: String = records.iter().map(|r| format!("{r}\n")).collect::<Vec<_>>().concat();
    std::fs::write(&path, text).expect("written");
    let changes = Transcripts::default().read(&path, &[]);
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.transcript(&changes, &[]));
    let thread = host.thread(observed.main());
    let notice = thread.items.iter().find_map(|i| match &i.body {
        ItemBody::Notice(n) => Some(n),
        _ => None,
    });
    let notice = notice.unwrap_or_else(|| panic!("a notice: {:#?}", thread.items));
    assert_eq!(notice.kind, Notice::API_ERROR);
    assert_eq!(notice.text.text, "Overloaded");
    assert_eq!(notice.retry, Some(Retry { attempt: 2, max: Some(10), in_ms: Some(1_085) }));
}

/// A transcript of `records`, one JSON object a line, read whole by the decoder.
fn read(records: &[serde_json::Value]) -> Vec<Change> {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let path = scratch.path().join("s.jsonl");
    let text: String = records.iter().map(|r| format!("{r}\n")).collect::<Vec<_>>().concat();
    std::fs::write(&path, text).expect("written");
    Transcripts::default().read(&path, &[])
}

/// A prompt, then the API error Claude Code gave up on (`error`, with `quota` as its
/// `quotaLimits` when it has them), then its end-of-turn record.
fn stopped_turn(error: &str, quota: Option<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut refusal = serde_json::json!({"type": "assistant", "uuid": "e1", "parentUuid": "p1",
        "timestamp": "2026-09-27T03:15:40.000Z", "isApiErrorMessage": true, "error": error,
        "message": {"role": "assistant", "model": "<synthetic>",
            "content": [{"type": "text", "text": "You've hit your limit · resets 6pm"}]}});
    if let Some(quota) = quota {
        refusal["quotaLimits"] = quota;
    }
    vec![
        serde_json::json!({"type": "user", "uuid": "p1", "parentUuid": null,
            "timestamp": "2026-09-27T03:15:25.849Z",
            "message": {"role": "user", "content": "Fix the build"}}),
        refusal,
        serde_json::json!({"type": "system", "subtype": "turn_duration", "uuid": "d1",
            "parentUuid": "e1", "timestamp": "2026-09-27T03:15:41.000Z", "durationMs": 15_000}),
    ]
}

fn done() -> AgentEvent {
    AgentEvent {
        session: SessionId::nil(),
        kind: AgentKind::ClaudeCode,
        status: AgentStatus::Done,
        agent_session: None,
        detail: None,
        attention: false,
        source: AgentSource::Hook,
        since_ms: WallMs::from_millis(3),
        mode: None,
    }
}

/// A turn a usage limit stopped fails with when the limit resets, as Claude Code recorded the
/// quota it hit; its notice is a limit, and the agent's done is a failure, whichever of the
/// status and the transcript comes first.
#[test]
fn a_turn_a_usage_limit_stopped_fails_until_it_resets() {
    let quota = serde_json::json!({"status": "rejected", "resetsAt": 1_790_020_000_u64,
        "rateLimitType": "five_hour"});
    let changes = read(&stopped_turn(conv::Stop::RATE_LIMIT, Some(quota)));
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.status(&done()));
    assert_eq!(host.thread(observed.main()).status.phase, Phase::Done, "nothing failed yet");
    host.take(observed.transcript(&changes, &[]));
    let thread = host.thread(observed.main());
    let [turn] = thread.turns.as_slice() else { panic!("one turn: {:#?}", thread.turns) };
    assert_eq!(
        turn.state,
        TurnState::Failed {
            error: "You've hit your limit · resets 6pm".to_owned(),
            until_ms: Some(WallMs::from_millis(1_790_020_000_000)),
        }
    );
    let kinds: Vec<&str> = thread
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Notice(n) => Some(n.kind.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(kinds, [Notice::LIMIT]);
    assert_eq!(thread.status.phase, Phase::Failed, "the done before it is told again");
    host.take(observed.status(&done()));
    assert_eq!(host.thread(observed.main()).status.phase, Phase::Failed, "and one after it");
    let next = read(&[serde_json::json!({"type": "user", "uuid": "p2", "parentUuid": "d1",
        "timestamp": "2026-09-27T05:00:00.000Z",
        "message": {"role": "user", "content": "Try again"}})]);
    host.take(observed.transcript(&next, &[]));
    host.take(observed.status(&done()));
    assert_eq!(host.thread(observed.main()).status.phase, Phase::Done, "a new prompt starts over");
}

/// A limit stop with no quota recorded resets when the status line's full window does; any
/// other error that ended a turn fails it with no reset, as an API error; and a turn the
/// model went on in after an error completes.
#[test]
fn a_stopped_turn_fails_by_what_stopped_it() {
    let full = conv::Meters {
        five_hour: Some(conv::RateWindow { used_pct: 100.0, resets_at: Some(1_790_030_000) }),
        seven_day: Some(conv::RateWindow { used_pct: 40.0, resets_at: Some(1_790_500_000) }),
        ..conv::Meters::default()
    };
    let ended = |records: &[serde_json::Value]| {
        let mut observed = observed();
        let mut host = Host::default();
        host.take(observed.drain());
        host.take(observed.meters(&full));
        host.take(observed.transcript(&read(records), &[]));
        let thread = host.thread(observed.main());
        let notice = thread.items.iter().find_map(|i| match &i.body {
            ItemBody::Notice(n) => Some(n.kind.clone()),
            _ => None,
        });
        (thread.turns.first().map(|t| t.state.clone()), notice)
    };
    let (state, notice) = ended(&stopped_turn(conv::Stop::RATE_LIMIT, None));
    assert!(
        matches!(state, Some(TurnState::Failed { until_ms: Some(at), .. })
            if at == WallMs::from_millis(1_790_030_000_000)),
        "{state:?}"
    );
    assert_eq!(notice.as_deref(), Some(Notice::LIMIT));
    let (state, notice) = ended(&stopped_turn("server_error", None));
    assert!(matches!(state, Some(TurnState::Failed { until_ms: None, .. })), "{state:?}");
    assert_eq!(notice.as_deref(), Some(Notice::API_ERROR));
    let mut went_on = stopped_turn("server_error", None);
    went_on.insert(
        2,
        serde_json::json!({"type": "assistant", "uuid": "a1", "parentUuid": "e1",
            "timestamp": "2026-09-27T03:15:40.500Z",
            "message": {"id": "m1", "role": "assistant", "model": "claude-opus-5-5",
                "content": [{"type": "text", "text": "Done."}]}}),
    );
    let (state, _) = ended(&went_on);
    assert_eq!(state, Some(TurnState::Complete));
}

/// A command Claude Code runs in the background is in the thread's background work, by the
/// task id Claude Code gave it, from its call to its end, which its queued notice stamps.
#[test]
fn background_commands_are_the_threads_background_work() {
    let (observed, host, _) = replay(&dir("conversation", "background"));
    let thread = host.thread(observed.main());
    let [task] = thread.tasks.as_slice() else { panic!("one task: {:#?}", thread.tasks) };
    assert_eq!(task.id, "b00000001");
    assert_eq!(task.kind, BackgroundTask::SHELL);
    let call = task.item.as_ref().expect("the call that started it");
    let started = thread.items.iter().find(|i| i.id == *call).expect("the call is in the thread");
    assert_eq!(task.started_ms, started.at_ms);
    assert_ne!(task.title, "");
    assert_eq!(task.state, BackgroundTask::COMPLETED, "its notice came: {task:#?}");
    assert!(task.ended_ms.is_some_and(|end| end >= task.started_ms), "{task:#?}");
}

/// Only the scenarios that ran something in the background have background work. Read a line
/// at a time it ends the same (`read_a_line_at_a_time_it_ends_the_same` compares whole states).
#[test]
fn only_what_ran_in_the_background_is_background_work() {
    for scenario in CONVERSATIONS {
        let (observed, host, _) = replay(&dir("conversation", scenario));
        let tasks = &host.thread(observed.main()).tasks;
        let foreground = !matches!(scenario, "background" | "tools");
        assert_eq!(tasks.is_empty(), foreground, "{scenario}: {tasks:#?}");
    }
}

/// The slash commands Claude Code takes are the thread's, each by where it comes from, told
/// once, and told again when the thread begins anew from its transcript.
#[test]
fn slash_commands_are_the_threads() {
    let listed = vec![
        conv::SlashCommand {
            name: "compact".to_owned(),
            description: "Compact the conversation".to_owned(),
            argument_hint: None,
            source: conv::CommandSource::BuiltIn,
        },
        conv::SlashCommand {
            name: "ship".to_owned(),
            description: "Ship it".to_owned(),
            argument_hint: Some("<branch>".to_owned()),
            source: conv::CommandSource::Project,
        },
    ];
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.commands(&listed));
    let thread = host.thread(observed.main());
    let named: Vec<(&str, &str)> =
        thread.commands.iter().map(|c| (c.name.as_str(), c.source.as_str())).collect();
    assert_eq!(named, [("compact", "built-in"), ("ship", "project")]);
    assert_eq!(thread.commands[1].argument_hint.as_deref(), Some("<branch>"));
    assert!(observed.commands(&listed).is_empty(), "told once");
}
