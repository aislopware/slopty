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
                conv::ThreadId::Agent(agent) => subagent_of(observed.main(), agent),
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
        assert!(!sub.items.is_empty());
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

/// A thread's id is the session's own, so a restart finds the same thread, and a clipped
/// text's reference resolves back to where the transcript has it.
#[test]
fn ids_and_references_are_stable() {
    assert_eq!(observed().main(), observed().main());
    assert_ne!(thread_of("a"), thread_of("b"));
    assert_ne!(subagent_of(thread_of("a"), "x"), subagent_of(thread_of("b"), "x"));
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
