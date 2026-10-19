use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use slopty_proto::thread::{Limit, ThreadState, Wait};

use super::*;
use crate::conversation::{Conversation, Transcripts};
use crate::transcript::Tail;

const CONVERSATIONS: [&str; 10] = [
    "edit",
    "tools",
    "interrupt",
    "compact",
    "permission",
    "background",
    "auto",
    "approve-edit",
    "notebook",
    "fork",
];

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

fn observed() -> History {
    History::new("00000000-0000-4000-8000-000000000001", "2.1.286", "/work", &[], WallMs::ZERO)
}

/// A scenario's transcripts read whole, through the adapter.
fn replay(dir: &Path) -> (History, Host, Transcripts) {
    let mut transcripts = Transcripts::default();
    let changes = transcripts.read(&dir.join("transcript.jsonl"), &subagents(dir));
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(observed.transcript(&changes, &[]));
    (observed, host, transcripts)
}

/// Every entry the decoder reads is an item of its thread, in order with its id; each sits in
/// the turn of the prompt before it; every turn the transcript closed is ended, and a subagent's
/// once its call ended.
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
            // A subagent's turn ends with the call that started it, too.
            let ran = matches!(thread, conv::ThreadId::Agent(_))
                && matches!(state.status.liveness, Liveness::Exited { .. });
            if ran {
                assert!(ended >= closed, "{scenario} {thread:?}: turns ended");
                let open = state.turns.iter().any(|t| t.state == TurnState::Active);
                assert!(!open, "{scenario} {thread:?}: a turn left open");
            } else {
                assert_eq!(ended, closed, "{scenario} {thread:?}: turns ended");
            }
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

/// A subagent's thread says where its run stands. Its own transcript comes before the call in
/// the foreground names it: begun then, it works, unlinked. Once the call's result names it, it
/// hangs off the call, takes the call's description as its title, and ends as the call did:
/// its turn ended, done, exited and not resumable.
#[test]
fn a_subagent_works_until_its_call_ends_then_is_done() {
    let source = dir("conversation", "tools");
    let text = std::fs::read_to_string(source.join("transcript.jsonl")).expect("a transcript");
    let lines: Vec<&str> = text.lines().collect();
    let result = lines.iter().position(|l| l.contains("agentId")).expect("the call's result");
    let scratch = tempfile::tempdir().expect("a temp dir");
    let main = scratch.path().join("s.jsonl");
    let before: String = lines.iter().take(result).flat_map(|l| [*l, "\n"]).collect();
    std::fs::write(&main, before).expect("written");
    let agents = subagents(&source);
    let (mut transcripts, mut observed, mut host) =
        (Transcripts::default(), observed(), Host::default());
    host.take(observed.drain());
    host.take(observed.transcript(&transcripts.read(&main, &agents), &[]));
    let (child, call) = host
        .thread(observed.main())
        .items
        .iter()
        .find_map(|i| match &i.body {
            ItemBody::Tool(call) if call.kind == kind::AGENT => {
                Some((i.id.clone(), call.title.clone()))
            }
            _ => None,
        })
        .expect("the Agent call");
    let sub = host
        .threads
        .values()
        .find(|t| t.meta.origin == ThreadMeta::SUBAGENT)
        .expect("the subagent begun from its own transcript")
        .clone();
    assert_eq!(sub.meta.parent, None, "not named by its call yet");
    assert_eq!((sub.status.phase, sub.status.liveness), (Phase::Working, Liveness::Live));

    let all: String = lines.iter().flat_map(|l| [*l, "\n"]).collect();
    std::fs::write(&main, all).expect("written");
    host.take(observed.transcript(&transcripts.read(&main, &agents), &[]));
    let sub = host.thread(sub.meta.id);
    let parent = sub.meta.parent.clone().expect("hung off its call");
    assert_eq!((parent.thread, parent.item), (observed.main(), child));
    assert_eq!(sub.meta.title, call, "titled by the call's description");
    assert_eq!(sub.status.phase, Phase::Done);
    assert_eq!(sub.status.liveness, Liveness::Exited { resumable: false });
    assert!(sub.turns.iter().all(|t| t.state != TurnState::Active), "{:?}", sub.turns);
}

/// A subagent runs only in the Claude Code that started it: one the session's transcript left
/// at work works on under a process that started before it, and once a process started after
/// it runs the session (`--resume`, a worker that came back to a new one), it stopped with the
/// one before: its turn ended, exited and not resumable.
#[test]
fn a_subagent_left_at_work_stops_with_the_claude_code_that_ran_it() {
    let source = dir("conversation", "tools");
    let text = std::fs::read_to_string(source.join("transcript.jsonl")).expect("a transcript");
    let lines: Vec<&str> = text.lines().collect();
    let result = lines.iter().position(|l| l.contains("agentId")).expect("the call's result");
    let scratch = tempfile::tempdir().expect("a temp dir");
    let main = scratch.path().join("s.jsonl");
    let before: String = lines.iter().take(result).flat_map(|l| [*l, "\n"]).collect();
    std::fs::write(&main, before).expect("written");
    let (mut transcripts, mut observed, mut host) =
        (Transcripts::default(), observed(), Host::default());
    host.take(observed.drain());
    host.take(observed.transcript(&transcripts.read(&main, &subagents(&source)), &[]));
    let sub = host
        .threads
        .values()
        .find(|t| t.meta.origin == ThreadMeta::SUBAGENT)
        .map(|t| t.meta.id)
        .expect("the subagent at work");
    let began = host.thread(sub).turns.iter().map(|t| t.started_ms).min().expect("its turn");

    host.take(observed.started(WallMs::from_millis(1)));
    let working = host.thread(sub);
    assert_eq!((working.status.phase, working.status.liveness), (Phase::Working, Liveness::Live));

    let after = WallMs::from_millis(began.as_millis() + 60_000);
    host.take(observed.started(after));
    let stopped = host.thread(sub);
    assert_eq!(stopped.status.phase, Phase::Stopped);
    assert_eq!(stopped.status.liveness, Liveness::Exited { resumable: false });
    assert!(stopped.turns.iter().all(|t| t.state != TurnState::Active), "{:?}", stopped.turns);
    let row = stopped.row(after);
    assert!(!matches!(row.status.phase, Phase::Working | Phase::Waiting), "{row:?}");
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

fn done(history: &mut History) -> Vec<Out> {
    history.status(Phase::Done, None, Liveness::Live, WallMs::from_millis(3))
}

/// A turn a usage limit stopped fails with when the limit resets, as Claude Code recorded the
/// quota it hit; its notice is a limit, and the agent's done is a failure, whichever of the
/// engine's word and the record comes first.
#[test]
fn a_turn_a_usage_limit_stopped_fails_until_it_resets() {
    let quota = serde_json::json!({"status": "rejected", "resetsAt": 1_790_020_000_u64,
        "rateLimitType": "five_hour"});
    let changes = read(&stopped_turn(conv::Stop::RATE_LIMIT, Some(quota)));
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    host.take(done(&mut observed));
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
    host.take(done(&mut observed));
    assert_eq!(host.thread(observed.main()).status.phase, Phase::Failed, "and one after it");
    let next = read(&[serde_json::json!({"type": "user", "uuid": "p2", "parentUuid": "d1",
        "timestamp": "2026-09-27T05:00:00.000Z",
        "message": {"role": "user", "content": "Try again"}})]);
    host.take(observed.transcript(&next, &[]));
    host.take(done(&mut observed));
    assert_eq!(host.thread(observed.main()).status.phase, Phase::Done, "a new prompt starts over");
}

/// A limit stop with no quota recorded resets when the full window does; any
/// other error that ended a turn fails it with no reset, as an API error; and a turn the
/// model went on in after an error completes.
#[test]
fn a_stopped_turn_fails_by_what_stopped_it() {
    let full = Meters {
        limits: vec![
            Limit {
                name: "five-hour".to_owned(),
                used_bp: 10_000,
                resets_ms: Some(WallMs::from_millis(1_790_030_000_000)),
            },
            Limit {
                name: "seven-day".to_owned(),
                used_bp: 4_000,
                resets_ms: Some(WallMs::from_millis(1_790_500_000_000)),
            },
        ],
        ..Meters::default()
    };
    let ended = |records: &[serde_json::Value]| {
        let mut observed = observed();
        let mut host = Host::default();
        host.take(observed.drain());
        host.take(observed.meters(full.clone()));
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
            skill: false,
        },
        conv::SlashCommand {
            name: "ship".to_owned(),
            description: "Ship it".to_owned(),
            argument_hint: Some("<branch>".to_owned()),
            source: conv::CommandSource::Project,
            skill: false,
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

/// A thread reviews through Claude Code's own `/code-review` only while Claude Code lists it:
/// the review capability comes with the command and goes with it.
#[test]
fn a_thread_reviews_while_claude_code_lists_code_review() {
    let command = |name: &str| conv::SlashCommand {
        name: name.to_owned(),
        description: String::new(),
        argument_hint: None,
        source: conv::CommandSource::BuiltIn,
        skill: false,
    };
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    let reviews =
        |host: &Host, observed: &History| host.thread(observed.main()).meta.can(Cap::REVIEW);
    host.take(observed.commands(&[command("compact")]));
    assert!(!reviews(&host, &observed), "no door without the command");
    host.take(observed.commands(&[command("compact"), command(REVIEW_COMMAND)]));
    assert!(reviews(&host, &observed), "the command is the door");
    let caps = &host.thread(observed.main()).meta.caps;
    assert!(caps.windows(2).all(|w| w[0] < w[1]), "still sorted: {caps:?}");
    host.take(observed.commands(&[command("compact")]));
    assert!(!reviews(&host, &observed), "and goes with it");
}

/// An agent at rest whose only work left running is commands it started in the background
/// waits on them by name (`Wait::COMMAND`), worded again once the transcript shows them; more
/// work than the transcript shows (a monitor) or a scheduled prompt is a task of its own.
#[test]
fn a_wait_on_commands_left_running_names_them() {
    use serde_json::json;
    let main = conv::ThreadId::Main;
    let mut conversation = Conversation::default();
    let mut observed = observed();
    let mut host = Host::default();
    host.take(observed.drain());
    let wait = |host: &Host, observed: &History| {
        host.thread(observed.main()).status.wait.clone().expect("a wait")
    };

    host.take(done(&mut observed));
    host.take(observed.background(1, 0, None));
    assert_eq!(wait(&host, &observed).kind, Wait::TASK, "nothing named running yet");
    let input = json!({"command": "npm run dev", "run_in_background": true});
    let call = json!({
        "type": "assistant", "uuid": "a1", "timestamp": "2026-09-27T03:15:26.000Z",
        "message": { "role": "assistant", "content": [
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": input },
        ] },
    });
    let started = json!({
        "type": "user", "uuid": "u1", "parentUuid": "a1",
        "timestamp": "2026-09-27T03:15:27.000Z",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "t1",
            "content": "Command running in background with ID: b1. Output is being written \
                        to: /tmp/b1.output.",
        }] },
        "toolUseResult": { "stdout": "", "stderr": "", "backgroundTaskId": "b1" },
    });
    let mut changes = conversation.ingest(&main, &call);
    changes.extend(conversation.ingest(&main, &started));
    host.take(observed.transcript(&changes, &[]));
    let state = host.thread(observed.main());
    let [task] = state.tasks.as_slice() else { panic!("one task: {:?}", state.tasks) };
    assert_eq!(task.kind, BackgroundTask::SHELL);
    assert_eq!(
        wait(&host, &observed),
        Wait { kind: Wait::COMMAND.to_owned(), text: task.title.clone() },
        "worded again once the transcript names it"
    );

    host.take(observed.background(2, 0, None));
    assert_eq!(wait(&host, &observed).kind, Wait::TASK, "a monitor the transcript does not show");
    host.take(observed.background(1, 1, None));
    assert_eq!(wait(&host, &observed).kind, Wait::TASK, "a scheduled prompt holds it");
    host.take(observed.background(1, 0, None));
    assert_eq!(wait(&host, &observed).kind, Wait::COMMAND);
}

/// A session read again from its records alone is the thread the records make, its last turn
/// ended there, named by the session the records name.
#[test]
fn a_session_replays_from_its_records() {
    for scenario in CONVERSATIONS {
        let dir = dir("conversation", scenario);
        let mut records: Vec<serde_json::Value> = Vec::new();
        for file in std::iter::once(dir.join("transcript.jsonl")).chain(subagents(&dir)) {
            let text = std::fs::read_to_string(&file).expect("a transcript");
            records.extend(text.lines().filter_map(|l| serde_json::from_str(l).ok()));
        }
        let state = super::replay(&records);
        let session = records.iter().find_map(|r| r["sessionId"].as_str()).expect("a session");
        assert_eq!(state.meta.id, thread_of(session), "{scenario}");
        assert_ne!(state.items.len(), 0, "{scenario}");
        // The branch the session worked on, as a Codex thread names it.
        assert_eq!(
            state.meta.facts.get(BRANCH_FACT).map(String::as_str),
            branch_in(&records),
            "{scenario}"
        );
        assert!(
            state.turns.iter().all(|t| t.state != TurnState::Active),
            "{scenario}: {:?}",
            state.turns
        );
    }
}

/// A call of an MCP tool read before the session's servers were listed (a first turn's, a
/// resumed session's) is shown as its app's page once they are, as it ended.
#[test]
fn an_app_call_read_before_the_servers_were_listed_shows_its_page() {
    let records = [
        serde_json::json!({"type": "user", "uuid": "p1", "parentUuid": null,
            "timestamp": "2026-10-17T03:15:25.000Z",
            "message": {"role": "user", "content": "Echo hi"}}),
        serde_json::json!({"type": "assistant", "uuid": "a1", "parentUuid": "p1",
            "timestamp": "2026-10-17T03:15:26.000Z",
            "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_1",
                "name": "mcp__apps__echo", "input": {"text": "hi"}}]}}),
        serde_json::json!({"type": "user", "uuid": "r1", "parentUuid": "a1",
            "timestamp": "2026-10-17T03:15:27.000Z",
            "message": {"role": "user", "content": [{"type": "tool_result",
                "tool_use_id": "toolu_1", "content": "hi"}]}}),
    ];
    let mut history = observed();
    let mut host = Host::default();
    host.take(history.drain());
    host.take(history.transcript(&read(&records), &[]));
    let id = history.meta().id;
    let shown_of = |host: &Host| {
        host.thread(id)
            .items
            .iter()
            .find_map(|item| match &item.body {
                ItemBody::Tool(call) if call.name == "mcp__apps__echo" => {
                    Some((call.detail.clone(), call.state.clone()))
                }
                _ => None,
            })
            .expect("the call")
    };
    assert!(matches!(shown_of(&host).0, Some(detail::ToolDetail::Mcp(_))), "no app known yet");

    let app = slopty_proto::visual::McpApp {
        server: "apps".to_owned(),
        tool: "echo".to_owned(),
        resource_uri: "ui://apps/echo".to_owned(),
        display_mode: None,
        result: None,
    };
    let was = shown_of(&host).1;
    host.take(history.set_apps(HashMap::from([(("apps".to_owned(), "echo".to_owned()), app)])));
    let (shown, state) = shown_of(&host);
    let Some(detail::ToolDetail::Page(page)) = shown else { panic!("{shown:?}") };
    assert!(
        matches!(&page.source, slopty_proto::visual::PageSource::App(a)
            if a.resource_uri == "ui://apps/echo"),
        "{page:?}"
    );
    assert_eq!(state, was, "as it ended");
}

/// "Always allow" says a granted folder short, as the TUI does: the chat's own folder as "this
/// folder", one in it relative to it, one under the home from `~`, any other in full.
#[test]
fn a_granted_folder_is_said_relative_to_the_chats_folder_or_home() {
    use crate::conversation::{Clipped, Grant, Suggestion, ToolDetail};
    let places = Places { cwd: "/Users/me/work/".to_owned(), home: "/Users/me".to_owned() };
    let said = |dirs: &[&str], mode: Option<&str>| {
        let mut suggestions = vec![Suggestion {
            grant: Grant::Directories {
                directories: dirs.iter().map(|d| (*d).to_owned()).collect(),
            },
            destination: Some("session".to_owned()),
        }];
        if let Some(mode) = mode {
            suggestions.insert(
                0,
                Suggestion {
                    grant: Grant::Mode { mode: mode.to_owned() },
                    destination: Some("session".to_owned()),
                },
            );
        }
        let prompt = PermissionPrompt {
            reason: None,
            ask: 1,
            tool: "Edit".to_owned(),
            call: None,
            detail: ToolDetail::Other {
                input: Clipped { text: String::new(), lines: 0, chars: 0, full: None },
            },
            suggestions,
            mode: None,
            asked_ms: WallMs::ZERO,
            needs_presence: false,
            input: serde_json::Value::Null,
            exists: None,
            context_used: None,
            before_plan: None,
            on_disk: None,
            default_to_no: false,
        };
        grants(&prompt, &places)
    };
    let (_, folders) = said(&["/Users/me/work/src", " /opt/x "], None);
    assert_eq!(folders, ["/Users/me/work/src", "/opt/x"], "in full, for the tooltip");
    let said = |dirs: &[&str], mode: Option<&str>| said(dirs, mode).0;
    assert_eq!(
        said(&["/Users/me/work"], Some("acceptEdits")).as_deref(),
        Some("edits in this folder this session")
    );
    assert_eq!(
        said(&["/Users/me/work/src/lexer", "/Users/me/notes", "/Users/me", "/opt/x"], None)
            .as_deref(),
        Some("access to src/lexer, ~/notes, ~, /opt/x this session")
    );
    assert_eq!(
        said(&["/Users/me/workbench"], None).as_deref(),
        Some("access to ~/workbench this session")
    );
}

/// A fork subagent (on by default in `-p` since 2.1.302), recorded from Claude Code: its card
/// says "Fork" and its description, and its drilled view starts at its own first record, the
/// directive it was given, never the parent's call it inherited nor that call's "Fork started";
/// the parent's call keeps its own result, the fork's hand-back.
#[test]
fn a_fork_starts_at_its_own_first_record() {
    use slopty_proto::thread::{ItemBody, ToolDetail as Detail};
    let (observed, host, _) = replay(&dir("conversation", "fork"));
    let main = host.thread(observed.main());
    let call = main
        .items
        .iter()
        .find_map(|i| match &i.body {
            ItemBody::Tool(call) if call.name == "Agent" => Some(call),
            _ => None,
        })
        .expect("the fork's call");
    let Some(Detail::Agent(agent)) = &call.detail else { panic!("an agent call: {call:?}") };
    assert_eq!(agent.agent_type.as_deref(), Some("Fork"));
    assert_eq!(agent.description.as_deref(), Some("Count lines"));
    let output = call.output.as_ref().map(|o| o.text.as_str()).unwrap_or_default();
    assert!(!output.contains("Fork started"), "the parent's own result: {output}");
    let fork = host.thread(observed.main().subagent("a0000000000000001"));
    let first = fork.items.first().expect("the fork's records");
    let ItemBody::User(prompt) = &first.body else { panic!("its directive first: {first:?}") };
    assert_eq!(prompt.text.text, "Read main.rs and reply with just the number of lines.");
    assert!(
        !fork.items.iter().any(|i| matches!(&i.body, ItemBody::Tool(c) if c.name == "Agent")),
        "nothing inherited: {:#?}",
        fork.items
    );
}

/// Records of a session at `minute` past an hour, as Claude Code writes them.
mod records {
    use serde_json::{Value, json};

    pub(super) fn at(second: u32) -> String {
        format!("2026-10-19T01:{:02}:{:02}.000Z", second / 60, second % 60)
    }

    pub(super) fn prompt(uuid: &str, second: u32, agent: Option<&str>) -> Value {
        let mut record = json!({
            "type": "user", "uuid": uuid, "timestamp": at(second),
            "message": { "role": "user", "content": "go" },
        });
        if let Some(agent) = agent {
            record["isSidechain"] = json!(true);
            record["agentId"] = json!(agent);
        }
        record
    }

    pub(super) fn call(
        uuid: &str,
        second: u32,
        agent: Option<&str>,
        id: &str,
        name: &str,
        input: &Value,
    ) -> Value {
        let mut record = json!({
            "type": "assistant", "uuid": uuid, "timestamp": at(second),
            "message": {
                "id": format!("msg-{uuid}"), "role": "assistant",
                "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }],
            },
        });
        if let Some(agent) = agent {
            record["isSidechain"] = json!(true);
            record["agentId"] = json!(agent);
        }
        record
    }

    pub(super) fn result(
        uuid: &str,
        second: u32,
        agent: Option<&str>,
        id: &str,
        said: &Value,
    ) -> Value {
        let mut record = json!({
            "type": "user", "uuid": uuid, "timestamp": at(second),
            "message": {
                "role": "user",
                "content": [{ "type": "tool_result", "tool_use_id": id, "content": "started" }],
            },
            "toolUseResult": said,
        });
        if let Some(agent) = agent {
            record["isSidechain"] = json!(true);
            record["agentId"] = json!(agent);
        }
        record
    }

    pub(super) fn notice(second: u32, task: &str, call: Option<&str>, summary: &str) -> Value {
        let call =
            call.map(|call| format!("<tool-use-id>{call}</tool-use-id>\n")).unwrap_or_default();
        let content = format!(
            "<task-notification>\n<task-id>{task}</task-id>\n{call}<status>completed</status>\n\
             <summary>{summary}</summary>\n</task-notification>"
        );
        json!({ "type": "queue-operation", "operation": "enqueue", "timestamp": at(second), "content": content })
    }
}

/// Feed `records` to `history` and `host`, through one decoder.
fn feed(
    history: &mut History,
    host: &mut Host,
    decoder: &mut Conversation,
    records: &[serde_json::Value],
) {
    for record in records {
        let changes = decoder.ingest(&conv::ThreadId::Main, record);
        host.take(history.transcript(&changes, &[]));
    }
}

/// A subagent run in the background, taken up again by more words (`SendMessage`), ends when
/// Claude Code says it finished, though that notice names only its task: its task completes and
/// its thread is done, not left waiting on the work it said it waited on before.
#[test]
fn a_subagent_taken_up_again_ends_by_its_tasks_notice() {
    use serde_json::json;
    let (mut history, mut host, mut decoder) =
        (observed(), Host::default(), Conversation::default());
    host.take(history.drain());
    let agent = json!({ "description": "Lane 2", "prompt": "go", "run_in_background": true });
    let words = json!({ "to": "a1", "message": "go on" });
    feed(
        &mut history,
        &mut host,
        &mut decoder,
        &[
            records::prompt("p1", 0, None),
            records::call("c1", 1, None, "toolu_a", "Agent", &agent),
            records::result(
                "r1",
                2,
                None,
                "toolu_a",
                &json!({ "isAsync": true, "status": "async_launched", "agentId": "a1" }),
            ),
            records::prompt("s1", 2, Some("a1")),
            records::notice(
                10,
                "a1",
                Some("toolu_a"),
                "Agent \"Lane 2\" is waiting on its own background work",
            ),
            records::call("c2", 20, None, "toolu_s", "SendMessage", &words),
            records::result("r2", 21, None, "toolu_s", &json!({ "success": true })),
            records::notice(
                30,
                "a1",
                Some("toolu_s"),
                "Agent \"Lane 2\" is waiting on its own background work",
            ),
        ],
    );
    let sub = history.main().subagent("a1");
    assert_eq!(host.thread(sub).status.phase, Phase::Waiting, "waiting on its own work");

    feed(
        &mut history,
        &mut host,
        &mut decoder,
        &[records::notice(40, "a1", None, "Agent \"Lane 2\" finished")],
    );
    let task = host.thread(history.main()).tasks.iter().find(|t| t.id == "a1").cloned();
    assert_eq!(task.map(|t| t.state), Some(BackgroundTask::COMPLETED.to_owned()));
    let sub = host.thread(sub);
    assert_eq!(
        (sub.status.phase, sub.status.liveness),
        (Phase::Done, Liveness::Exited { resumable: false })
    );
}

/// A session read again with no Claude Code running it runs nothing it started: a subagent
/// left at work, the work it left in the background, and the session's own command left
/// running all ended with the last Claude Code that ran it, as of its last entry.
#[test]
fn a_session_at_rest_runs_nothing_it_started() {
    use serde_json::json;
    let (mut history, mut host, mut decoder) =
        (observed(), Host::default(), Conversation::default());
    host.take(history.drain());
    let agent = json!({ "description": "Lane P", "prompt": "go", "run_in_background": true });
    let command =
        json!({ "command": "sleep 600", "description": "Fuzz", "run_in_background": true });
    feed(
        &mut history,
        &mut host,
        &mut decoder,
        &[
            records::prompt("p1", 0, None),
            records::call("c1", 1, None, "toolu_a", "Agent", &agent),
            records::result(
                "r1",
                2,
                None,
                "toolu_a",
                &json!({ "isAsync": true, "status": "async_launched", "agentId": "a1" }),
            ),
            records::prompt("s1", 2, Some("a1")),
            records::call("s2", 3, Some("a1"), "toolu_b", "Bash", &command),
            records::result("s3", 4, Some("a1"), "toolu_b", &json!({ "backgroundTaskId": "b1" })),
            records::call("c2", 5, None, "toolu_c", "Bash", &command),
            records::result("r2", 6, None, "toolu_c", &json!({ "backgroundTaskId": "b2" })),
        ],
    );
    let sub = history.main().subagent("a1");
    let live = |host: &Host, id| host.thread(id).tasks.iter().filter(|t| t.is_live()).count();
    assert_eq!(host.thread(sub).status.phase, Phase::Working);
    assert_eq!((live(&host, history.main()), live(&host, sub)), (2, 1), "all at work as read");

    host.take(history.rest(WallMs::from_millis(4_000_000_000_000)));
    let stopped = host.thread(sub);
    assert_eq!(
        (stopped.status.phase, stopped.status.liveness),
        (Phase::Stopped, Liveness::Exited { resumable: false })
    );
    assert_eq!((live(&host, history.main()), live(&host, sub)), (0, 0), "nothing runs on");
    let tasks = &host.thread(history.main()).tasks;
    let command = tasks.iter().find(|t| t.kind == BackgroundTask::SHELL).expect("the command");
    // Its call is the last entry: a result joins its call's.
    let last = command.started_ms;
    let ended: Vec<Option<WallMs>> = tasks.iter().map(|t| t.ended_ms).collect();
    assert!(ended.iter().all(|at| *at == Some(last)), "as of its last entry: {ended:?}");
}
