//! The ladder as the hub ranks it from the rows workers publish, and where its notices go.

use std::collections::BTreeMap;

use slopty_core::SessionId;
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::project::{TaskId, TaskSpec};
use slopty_proto::server::ToServer;
use slopty_proto::thread::attention::Counts;
use slopty_proto::thread::wire::RequestCard;
use slopty_proto::thread::{
    AgentId, AskId, Changed, Cursor, Drive, ItemId, Link, Liveness, Meters, Phase, Status,
};

use super::super::project_tests::{create, project};
use super::super::tests::{registration, summary};
use super::*;

pub(in crate::hub) fn row(phase: Phase, since: u64, terminal: Option<SessionId>) -> ThreadRow {
    ThreadRow {
        id: ThreadId::new(),
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        title: "Make the ladder".to_owned(),
        status: Status {
            phase,
            wait: None,
            liveness: Liveness::Live,
            since_ms: WallMs::from_millis(since),
        },
        requests: Vec::new(),
        last_line: Some("Done.".to_owned()),
        doing: None,
        changed: Changed::default(),
        terminal,
        parent: None,
        drive: Drive::named(Drive::OBSERVED),
        caps: Vec::new(),
        facts: BTreeMap::new(),
        to_review: false,
        pull: None,
        meters: Meters::default(),
        updated_ms: WallMs::from_millis(since),
        cwd: None,
        repo: None,
        repo_id: None,
    }
}

pub(in crate::hub) fn asking(mut row: ThreadRow, title: &str) -> ThreadRow {
    row.requests.push(RequestCard {
        id: AskId("1".to_owned()),
        item: None,
        kind: "permission".to_owned(),
        title: title.to_owned(),
        options: Vec::new(),
        opened_ms: row.status.since_ms,
    });
    row
}

/// `row` as it stands at `since` on `phase`, nothing asked.
fn moved(row: &ThreadRow, phase: Phase, since: u64) -> ThreadRow {
    let status = Status { phase, since_ms: WallMs::from_millis(since), ..row.status.clone() };
    ThreadRow { status, requests: Vec::new(), ..row.clone() }
}

pub(in crate::hub) fn under(mut child: ThreadRow, parent: &ThreadRow) -> ThreadRow {
    child.parent = Some(Link { thread: parent.id, item: ItemId("call".to_owned()) });
    child
}

pub(in crate::hub) fn snapshot(rows: Vec<ThreadRow>) -> ToServer {
    ToServer::Threads(TableFrame::Snapshot { cursor: Cursor::default(), rows })
}

fn delta(rows: Vec<ThreadRow>) -> ToServer {
    ToServer::Threads(TableFrame::Delta { cursor: Cursor::default(), rows, removed: Vec::new() })
}

fn standing<'a, K: PartialEq>(list: &'a [(K, Standing)], key: &K) -> &'a Standing {
    &list.iter().find(|(k, _)| k == key).unwrap().1
}

async fn task(hub: &Hub) -> TaskId {
    let spec = TaskSpec {
        title: "Ladder".to_owned(),
        brief: "Rank it.".to_owned(),
        ..TaskSpec::default()
    };
    match hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await {
        Outcome::Task(task) => task.id,
        other => panic!("{other:?}"),
    }
}

/// A subagent stands with the thread it hangs from, so the one the person sees needs them;
/// the rest roll up by tile, worker, project node, project and fleet, each counting a thread once
/// and naming the one to go to first. A thread at rest with changes not kept is to review, above
/// working.
#[tokio::test]
async fn subagents_fold_into_their_parents_and_every_node_rolls_up() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, building, testing) = (SessionId::new(), SessionId::new(), SessionId::new());
    let worker = WorkerId::new();
    let sessions = vec![summary(orchestrating), summary(building), summary(testing)];
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub.register(registration(worker, sessions), [100, 64, 0, 7].into(), tx).unwrap();
    let term = |session| TermRef { worker, session };
    create(&hub, Some(term(orchestrating))).await;
    let (build, test) = (task(&hub).await, task(&hub).await);
    for (task, session) in [(build, building), (test, testing)] {
        assert!(matches!(hub.assign_for_test(&project(), task, term(session)), Outcome::Task(_)));
    }

    let orchestrator = row(Phase::Idle, 10, Some(orchestrating));
    let builder = row(Phase::Working, 20, Some(building));
    let tester = row(Phase::Working, 30, Some(testing));
    let subagent = under(asking(row(Phase::NeedsYou, 40, None), "Run cargo test?"), &tester);
    let reviewed = ThreadRow { to_review: true, ..row(Phase::Done, 50, None) };
    lease.handle(snapshot(vec![
        orchestrator,
        builder,
        tester.clone(),
        subagent.clone(),
        reviewed.clone(),
    ]));
    hub.rank_ladder();
    let ladder = hub.ladder();

    let at = |row: &ThreadRow| ThreadAt { worker, thread: row.id };
    assert_eq!(ladder.threads.len(), 4, "the subagent stands with its parent");
    assert_eq!(ladder.rung(at(&tester)), Some(Rung::NeedsYou));
    assert_eq!(ladder.rung(at(&subagent)), None);
    assert_eq!(ladder.rung(at(&reviewed)), Some(Rung::ToReview));
    assert_eq!(ladder.tile(term(testing)).map(|(_, s)| s.rung), Some(Rung::NeedsYou));
    assert_eq!(ladder.tile(term(orchestrating)).map(|(_, s)| s.rung), Some(Rung::Idle));

    let node = |task| NodeAt { project: project(), task };
    let test_node = standing(&ladder.nodes, &node(Some(test)));
    assert_eq!(test_node.rung, Rung::NeedsYou);
    assert_eq!(test_node.top, Some(at(&tester)));
    assert_eq!(test_node.counts, Counts { needs_you: 1, ..Counts::default() });
    let build_node = standing(&ladder.nodes, &node(Some(build)));
    assert_eq!(build_node.counts, Counts { working: 1, ..Counts::default() }, "tasks stand apart");
    assert_eq!(standing(&ladder.nodes, &node(None)).rung, Rung::Idle);
    let whole = standing(&ladder.projects, &project());
    assert_eq!((whole.rung, whole.counts.total()), (Rung::NeedsYou, 3));

    let expected = Counts { needs_you: 1, to_review: 1, working: 1, idle: 1, ..Counts::default() };
    assert_eq!(ladder.fleet.counts, expected);
    assert_eq!(standing(&ladder.workers, &worker).counts, expected);
    let workspace = ladder.over([term(building), term(testing), term(building)]);
    assert_eq!((workspace.rung, workspace.counts.total()), (Rung::NeedsYou, 2));
    assert_eq!(workspace.top, Some(at(&tester)));

    // The subagent's request answered, the to-review thread kept: the ladder moves down.
    let kept = ThreadRow { to_review: false, ..reviewed.clone() };
    lease.handle(delta(vec![moved(&subagent, Phase::Working, 60), kept]));
    hub.rank_ladder();
    let ladder = hub.ladder();
    assert_eq!(ladder.rung(at(&tester)), Some(Rung::Working));
    assert_eq!(ladder.rung(at(&reviewed)), Some(Rung::Idle));
    assert_eq!(ladder.fleet.counts, Counts { working: 2, idle: 2, ..Counts::default() });
}

/// A seated client and where its notices arrive.
pub(in crate::hub) struct Client {
    seated: Seated,
    rx: mpsc::Receiver<FromServer>,
}

impl Client {
    pub(in crate::hub) fn sit(hub: &Hub, name: &str) -> Self {
        let (tx, rx) = mpsc::channel(8);
        Self { seated: hub.seat(hub.number_link(), name.to_owned(), tx), rx }
    }

    pub(in crate::hub) fn at(&self, hub: &Hub, seat: Seat, active: bool, showing: Vec<TermRef>) {
        let presence =
            Presence { seat, active, workspace: None, showing, focus: None, listening: true };
        hub.presence(self.seated.link(), presence);
    }

    fn notices(&mut self) -> Vec<Notice> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            if let FromServer::Notice(notice) = msg {
                out.push(*notice);
            }
        }
        out
    }
}

/// A notice goes nowhere while its tile is on screen where the person is; to the desk they
/// are at, never to the phone beside it; to the phone when they hold only it; and to every
/// client when they are at none. A subagent's need comes as its parent's, in the subagent's
/// words, and a thread first seen needing the person (a worker back) is no news. A thread that
/// comes to rest says how long it worked.
#[tokio::test]
async fn notices_go_where_the_person_is_and_a_subagent_speaks_through_its_parent() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (shell, worker) = (SessionId::new(), WorkerId::new());
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub
        .register(registration(worker, vec![summary(shell)]), [100, 64, 0, 7].into(), tx)
        .unwrap();
    let tile = TermRef { worker, session: shell };
    let (mut mac, mut phone) = (Client::sit(&hub, "mac"), Client::sit(&hub, "phone"));
    mac.at(&hub, Seat::Desk, true, Vec::new());
    phone.at(&hub, Seat::Handheld, true, Vec::new());
    let present = hub.present();
    assert_eq!(present.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["mac", "phone"]);

    let parent = row(Phase::Working, 1_000, Some(shell));
    let waiting = asking(row(Phase::NeedsYou, 5, None), "Never told");
    lease.handle(snapshot(vec![parent.clone(), waiting]));
    hub.rank_ladder();
    assert!(mac.notices().is_empty() && phone.notices().is_empty(), "first sight is no news");

    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(delta(rows));
        hub.rank_ladder();
    };
    let child = under(asking(row(Phase::NeedsYou, 2_000, None), "Run cargo test?"), &parent);
    rank(vec![child.clone()]);
    let heard = mac.notices();
    assert_eq!(heard.len(), 1);
    let notice = &heard[0];
    let parent_at = Subject::Thread(ThreadAt { worker, thread: parent.id });
    assert_eq!((notice.kind, &notice.about), (NoticeKind::NeedsYou, &parent_at));
    assert_eq!((notice.tile, notice.text.as_str()), (Some(tile), "Run cargo test?"));
    let via = notice.via.as_ref().map(|v| v.thread);
    assert_eq!(via, Some(child.id), "it says which subagent asks");
    assert!(phone.notices().is_empty(), "no push to the phone while at the desk");

    rank(vec![moved(&child, Phase::Working, 3_000)]);
    mac.at(&hub, Seat::Desk, false, Vec::new());
    rank(vec![asking(moved(&child, Phase::NeedsYou, 4_000), "Again?")]);
    assert_eq!(mac.notices(), Vec::<Notice>::new());
    assert_eq!(phone.notices().len(), 1, "the phone, when only it is in hand");

    rank(vec![moved(&child, Phase::Done, 5_000)]);
    phone.at(&hub, Seat::Handheld, true, vec![tile]);
    rank(vec![asking(moved(&child, Phase::NeedsYou, 6_000), "Once more?")]);
    assert!(mac.notices().is_empty() && phone.notices().is_empty(), "its tile is on screen");

    rank(vec![moved(&child, Phase::Done, 7_000)]);
    phone.at(&hub, Seat::Handheld, false, vec![tile]);
    rank(vec![moved(&parent, Phase::Done, 8_000)]);
    let (on_mac, on_phone) = (mac.notices(), phone.notices());
    assert_eq!(on_mac, on_phone, "at no client, every client hears");
    assert_eq!(on_mac.len(), 1);
    assert_eq!(on_mac[0].kind, NoticeKind::Finished);
    assert_eq!(on_mac[0].worked_ms, Some(7_000), "from its first work to its rest");

    drop(phone.seated);
    assert_eq!(hub.present().len(), 1, "a link that ends leaves");
}

/// A subagent that fails while its parent works reads as working, since the parent may carry
/// on without it; once the whole family is at rest, the failure lifts the parent, and the
/// notice names the subagent. One that needs the person lifts its parent at once.
#[tokio::test]
async fn a_failed_subagent_waits_for_its_family_to_rest() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (shell, worker) = (SessionId::new(), WorkerId::new());
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub
        .register(registration(worker, vec![summary(shell)]), [100, 64, 0, 7].into(), tx)
        .unwrap();
    let mut desk = Client::sit(&hub, "mac");
    desk.at(&hub, Seat::Desk, true, Vec::new());
    let parent = row(Phase::Working, 1_000, Some(shell));
    let (one, two) = (under(row(Phase::Working, 1_100, None), &parent), {
        under(row(Phase::Working, 1_200, None), &parent)
    });
    lease.handle(snapshot(vec![parent.clone(), one.clone(), two.clone()]));
    hub.rank_ladder();
    let at = ThreadAt { worker, thread: parent.id };
    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(delta(rows));
        hub.rank_ladder();
        hub.ladder().rung(at)
    };

    assert_eq!(rank(vec![moved(&one, Phase::Failed, 2_000)]), Some(Rung::Working));
    assert_eq!(rank(vec![moved(&parent, Phase::Done, 3_000)]), Some(Rung::Working), "two works");
    assert!(desk.notices().is_empty(), "no failure while the family works");
    assert_eq!(rank(vec![moved(&two, Phase::Done, 4_000)]), Some(Rung::Failed), "all at rest");
    let heard = desk.notices();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0].kind, NoticeKind::Failed);
    assert_eq!(heard[0].via.as_ref().map(|v| v.thread), Some(one.id));

    let asking = asking(moved(&two, Phase::NeedsYou, 5_000), "May I?");
    assert_eq!(rank(vec![moved(&parent, Phase::Working, 4_500), asking]), Some(Rung::NeedsYou));
    let heard = desk.notices();
    assert_eq!(heard.iter().map(|n| n.kind).collect::<Vec<_>>(), [NoticeKind::NeedsYou]);
}

/// A project task's agent that comes to rest says nothing to the person: its work reaches them
/// when it is ready to merge, and the orchestrator hears of the rest. Its need and its failure
/// still come as notices, and the orchestrator's own rest does too.
#[tokio::test]
async fn a_task_s_agent_that_finishes_sends_the_person_no_notice() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, building) = (SessionId::new(), SessionId::new());
    let worker = WorkerId::new();
    let sessions = vec![summary(orchestrating), summary(building)];
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub.register(registration(worker, sessions), [100, 64, 0, 7].into(), tx).unwrap();
    let term = |session| TermRef { worker, session };
    create(&hub, Some(term(orchestrating))).await;
    let build = task(&hub).await;
    assert!(matches!(hub.assign_for_test(&project(), build, term(building)), Outcome::Task(_)));
    let mut desk = Client::sit(&hub, "mac");
    desk.at(&hub, Seat::Desk, true, Vec::new());
    let orchestrator = row(Phase::Working, 1_000, Some(orchestrating));
    let builder = row(Phase::Working, 1_000, Some(building));
    lease.handle(snapshot(vec![orchestrator.clone(), builder.clone()]));
    hub.rank_ladder();
    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(delta(rows));
        hub.rank_ladder();
    };
    rank(vec![moved(&builder, Phase::Done, 2_000)]);
    assert!(desk.notices().is_empty(), "a task's agent at rest is no notice");
    rank(vec![asking(moved(&builder, Phase::NeedsYou, 3_000), "May I?")]);
    rank(vec![moved(&builder, Phase::Failed, 4_000)]);
    let kinds: Vec<NoticeKind> = desk.notices().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [NoticeKind::NeedsYou, NoticeKind::Failed]);
    rank(vec![moved(&orchestrator, Phase::Done, 5_000)]);
    let kinds: Vec<NoticeKind> = desk.notices().iter().map(|n| n.kind).collect();
    assert_eq!(kinds, [NoticeKind::Finished], "the orchestrator's own still says so");
}

/// A thread's pull request lifts it while it rests: a failed check needs the person, said in
/// the pull request's words, and one ready to merge is to review. While the agent works it
/// stays working. A task's agent's pull request is the project's to tell of, so it sends none.
#[tokio::test]
async fn a_resting_thread_s_pull_request_lifts_it() {
    use slopty_proto::thread::wire::{PullSeen, PullStands};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (shell, building, orchestrating) = (SessionId::new(), SessionId::new(), SessionId::new());
    let worker = WorkerId::new();
    let sessions = vec![summary(shell), summary(building), summary(orchestrating)];
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub.register(registration(worker, sessions), [100, 64, 0, 7].into(), tx).unwrap();
    let term = |session| TermRef { worker, session };
    create(&hub, Some(term(orchestrating))).await;
    let build = task(&hub).await;
    assert!(matches!(hub.assign_for_test(&project(), build, term(building)), Outcome::Task(_)));
    let mut desk = Client::sit(&hub, "mac");
    desk.at(&hub, Seat::Desk, true, Vec::new());
    let pull = |stands| PullSeen {
        number: 42,
        url: "https://github.com/o/r/pull/42".to_owned(),
        title: "Fix the login".to_owned(),
        base: "main".to_owned(),
        stands,
        failed: u32::from(stands == PullStands::ChecksFailed),
        failed_first: (stands == PullStands::ChecksFailed).then(|| "lint".to_owned()),
        running: 0,
    };
    let mine = row(Phase::Done, 1_000, Some(shell));
    let builder = row(Phase::Done, 1_000, Some(building));
    lease.handle(snapshot(vec![mine.clone(), builder.clone()]));
    hub.rank_ladder();
    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(delta(rows));
        hub.rank_ladder();
        hub.ladder()
    };
    let at = |row: &ThreadRow| ThreadAt { worker, thread: row.id };
    let with = |row: &ThreadRow, phase, stands| ThreadRow {
        pull: Some(pull(stands)),
        ..moved(row, phase, 2_000)
    };

    let ladder = rank(vec![with(&mine, Phase::Working, PullStands::ChecksFailed)]);
    assert_eq!(ladder.rung(at(&mine)), Some(Rung::Working), "its agent may be on it");
    let ladder = rank(vec![with(&mine, Phase::Done, PullStands::ChecksFailed)]);
    assert_eq!(ladder.rung(at(&mine)), Some(Rung::NeedsYou));
    let heard = desk.notices();
    let said: Vec<(NoticeKind, &str)> = heard.iter().map(|n| (n.kind, n.text.as_str())).collect();
    assert_eq!(
        said,
        [(NoticeKind::NeedsYou, "#42: lint failed")],
        "work that ends on a failed check needs you"
    );
    let ladder = rank(vec![with(&mine, Phase::Done, PullStands::Ready)]);
    assert_eq!(ladder.rung(at(&mine)), Some(Rung::ToReview), "ready to merge");
    let ladder = rank(vec![with(&mine, Phase::Done, PullStands::Waiting)]);
    assert_eq!(ladder.rung(at(&mine)), Some(Rung::Idle));

    let ladder = rank(vec![with(&builder, Phase::Done, PullStands::ChecksFailed)]);
    assert_eq!(ladder.rung(at(&builder)), Some(Rung::NeedsYou));
    assert!(desk.notices().is_empty(), "the project tells of a task's checks");
}

/// A project's held-up work is a notice about the project, one per timeline entry, routed by the
/// orchestrator's terminal: a failed verifier says so, a pass says nothing, nothing goes while
/// the orchestrator is on screen, and a conflict past the task's give-backs waits on the person.
#[tokio::test]
async fn a_project_s_held_up_work_is_a_notice_about_the_project() {
    use slopty_proto::project::{StepKind, TaskChange, VerifierRun};
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, worker) = (SessionId::new(), WorkerId::new());
    let (tx, _rx) = mpsc::channel(8);
    let _lease = hub
        .register(registration(worker, vec![summary(orchestrating)]), [100, 64, 0, 7].into(), tx)
        .unwrap();
    let orchestrator = TermRef { worker, session: orchestrating };
    create(&hub, Some(orchestrator)).await;
    let build = task(&hub).await;
    let mut desk = Client::sit(&hub, "mac");
    desk.at(&hub, Seat::Desk, true, Vec::new());
    let verified = |passed: bool| Verb::TaskUpdate {
        project: project(),
        task: build,
        change: Box::new(TaskChange {
            verified: Some(VerifierRun {
                passed,
                summary: "1 failed".to_owned(),
                head: "a".repeat(40),
                base: "b".repeat(40),
                exit: Some(u8::from(!passed).into()),
                took_ms: 0,
            }),
            ..TaskChange::default()
        }),
    };
    assert!(matches!(hub.dispatch(verified(true)).await, Outcome::Task(_)));
    assert!(desk.notices().is_empty(), "a pass is the board's to show");
    assert!(matches!(hub.dispatch(verified(false)).await, Outcome::Task(_)));
    let heard = desk.notices();
    let [notice] = heard.as_slice() else { panic!("one notice: {heard:?}") };
    assert_eq!(notice.kind, NoticeKind::Project);
    assert!(
        matches!(&notice.about, Subject::Project { project: p, .. } if *p == project()),
        "{notice:?}"
    );
    assert_eq!(notice.tile, Some(orchestrator), "it opens the orchestrator");
    assert_eq!(notice.text, format!("#{build} Ladder: its verifier failed"));

    desk.at(&hub, Seat::Desk, true, vec![orchestrator]);
    assert!(matches!(hub.dispatch(verified(false)).await, Outcome::Task(_)));
    assert!(desk.notices().is_empty(), "the orchestrator on screen says it already");

    desk.at(&hub, Seat::Desk, true, Vec::new());
    let conflict = "CONFLICT (content): Merge conflict in src/lib.rs";
    for _ in 0..=slopty_proto::project::GIVE_BACKS_MAX {
        hub.give_back((&project(), build), StepKind::Rebase, Some(worker), conflict, None);
    }
    let said: Vec<String> = desk.notices().into_iter().map(|n| n.text).collect();
    let back = format!("#{build} Ladder: its work conflicts with main: {conflict}");
    let mut expected = vec![back.clone(); usize::from(slopty_proto::project::GIVE_BACKS_MAX)];
    expected.push(format!("{back}. It waits on you"));
    assert_eq!(said, expected, "each give-back, then the one held for the person");
}

/// What holds a task's work up, in words: failing checks name the failing ones, a failed step
/// its first line, and a merge only when its push did not go. The rest is the board's.
#[tokio::test]
async fn held_up_work_is_said_by_what_held_it() {
    use slopty_proto::project::{Checks, StepState, TaskStep, TimelineEntry};
    let hub = Hub::new("server".to_owned(), Vec::new());
    create(&hub, None).await;
    let id = task(&hub).await;
    let mut card = hub.inner.state.lock().projects.task(&project(), id).cloned().unwrap();
    let entry = |what| TimelineEntry { seq: 7, at_ms: WallMs::ZERO, task: Some(id), what };
    let step = |kind, state| {
        Moment::Step(TaskStep {
            kind,
            worker: WorkerId::new(),
            state,
            since_ms: WallMs::ZERO,
            term: None,
            commits: None,
        })
    };
    let say = |what, card: &Task| held_up(&entry(what), Some(card), "main");
    let checks = |state| Checks {
        state,
        passed: 3,
        failed: 2,
        pending: 0,
        skipped: 0,
        failing: vec!["test".to_owned(), "lint".to_owned()],
        why: None,
        at_ms: WallMs::ZERO,
    };
    assert_eq!(
        say(Moment::Checks(checks(ChecksState::Failing)), &card).as_deref(),
        Some("its pull request's checks fail (test, lint)")
    );
    assert_eq!(say(Moment::Checks(checks(ChecksState::Passing)), &card), None);
    let failed = StepState::Failed { why: "no space left\nmore".to_owned() };
    assert_eq!(
        say(step(StepKind::Clone, failed), &card).as_deref(),
        Some("the clone it needs failed: no space left")
    );
    let merged = || step(StepKind::Merge, StepState::Done { detail: "main at abc".to_owned() });
    assert_eq!(say(merged(), &card), None, "a merge that pushed is no news");
    card.merge = Some(Merge::Merged {
        target: "main".to_owned(),
        head: "abc".to_owned(),
        at_ms: WallMs::ZERO,
        pushed: false,
        push_failed: Some("rejected (non-fast-forward)".to_owned()),
    });
    assert_eq!(
        say(merged(), &card).as_deref(),
        Some("merged into main, but the push to origin failed: rejected (non-fast-forward)")
    );
}

/// A pusher that answers every push with what it was given, and counts them.
#[derive(Debug)]
struct Answering(slopty_push::apns::Outcome, std::sync::atomic::AtomicUsize);

impl crate::push::Pusher for Answering {
    fn push<'a>(
        &'a self,
        _push: &'a slopty_push::apns::Push,
        _topic: &'a str,
    ) -> crate::push::PushFuture<'a> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(std::future::ready(self.0.clone()))
    }
}

/// The workers hear whether a pocketed phone can answer a yes or no: once pushing is set up and
/// a phone is known, and again only when that moves. A worker that registers while one can is
/// told so with its welcome; one that registers while none can hears nothing.
#[tokio::test]
async fn the_workers_hear_whether_a_pocketed_phone_can_answer() {
    use slopty_proto::push::PushDevice;

    let hub = Hub::new("server".to_owned(), Vec::new());
    let (tx, mut first) = mpsc::channel(8);
    let _lease =
        hub.register(registration(WorkerId::new(), Vec::new()), [100, 64, 0, 7].into(), tx);
    let told = |rx: &mut mpsc::Receiver<FromServer>| {
        let mut said = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            if let FromServer::Pushes(pushes) = msg {
                said.push(pushes);
            }
        }
        said
    };
    assert!(told(&mut first).is_empty(), "nothing can be pushed yet");
    let (client, phone) = (ClientId::new(), Client::sit(&hub, "phone"));
    let device = PushDevice {
        token: "0f".repeat(32),
        key: [7; 32],
        sandbox: true,
        topic: "dev.aislopware.slopty".to_owned(),
        quiet_ms: 60_000,
    };
    hub.push_device(phone.seated.link(), client, Some(device.clone()));
    assert!(told(&mut first).is_empty(), "a phone, but pushing is off");
    let (out, _pushed) = mpsc::channel(8);
    hub.push_to(Some(out));
    assert_eq!(told(&mut first), [true], "set up, with a phone");
    hub.push_device(phone.seated.link(), client, Some(device.clone()));
    assert!(told(&mut first).is_empty(), "said once");

    let (tx, mut second) = mpsc::channel(8);
    let _later =
        hub.register(registration(WorkerId::new(), Vec::new()), [100, 64, 0, 8].into(), tx);
    assert_eq!(told(&mut second), [true], "with its welcome");
    hub.forget_device(client, &device.token);
    assert_eq!((told(&mut first), told(&mut second)), (vec![false], vec![false]), "no phone now");
}

/// A notice that finds the person at no client is pushed to a phone whose link is gone, once
/// per moment: a thread needing them for a plain yes or no carries its ask, urgent. Nothing is
/// pushed while they are at a desk, nor to a phone still listening on its link, but one that
/// said it stops listening is pushed to. A finished turn shorter than the phone's quiet time
/// is not pushed. A phone that withdraws, or that APNs says is gone, is forgotten.
#[tokio::test]
async fn needs_you_pushes_once_per_ask() {
    use slopty_proto::push::PushDevice;
    use slopty_proto::thread::{Choice, Effect};

    let hub = Hub::new("server".to_owned(), Vec::new());
    let kept = hub.keep_devices(Devices::new());
    let (out, mut pushed) = mpsc::channel(8);
    hub.push_to(Some(out));
    let (shell, worker) = (SessionId::new(), WorkerId::new());
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub
        .register(registration(worker, vec![summary(shell)]), [100, 64, 0, 7].into(), tx)
        .unwrap();
    let rank = |rows: Vec<ThreadRow>| {
        lease.handle(delta(rows));
        hub.rank_ladder();
    };
    let mut pushes = || {
        let mut out = Vec::new();
        while let Ok(push) = pushed.try_recv() {
            out.push(push);
        }
        out
    };

    let mac = Client::sit(&hub, "mac");
    mac.at(&hub, Seat::Desk, false, Vec::new());
    let (phone, client) = (Client::sit(&hub, "phone"), ClientId::new());
    let device = PushDevice {
        token: "0f".repeat(32),
        key: [7; 32],
        sandbox: true,
        topic: "dev.aislopware.slopty".to_owned(),
        quiet_ms: 60_000,
    };
    hub.push_device(phone.seated.link(), client, Some(device.clone()));
    assert_eq!(kept.borrow().get(&client), Some(&device), "kept for the next start");
    let odd = PushDevice { token: "not hex".to_owned(), ..device.clone() };
    hub.push_device(Client::sit(&hub, "odd").seated.link(), ClientId::new(), Some(odd));
    assert_eq!(hub.devices().len(), 1, "a token that is no token is no phone");

    let thread = row(Phase::Working, 1_000, Some(shell));
    lease.handle(snapshot(vec![thread.clone()]));
    hub.rank_ladder();
    let choice = |id: &str, effect| Choice {
        id: id.to_owned(),
        label: id.to_owned(),
        effect,
        scope: None,
        stops: false,
    };
    let mut needs = asking(moved(&thread, Phase::NeedsYou, 2_000), "Run cargo test?");
    needs.requests[0].kind = Request::APPROVAL.to_owned();
    needs.requests[0].options = vec![choice("yes", Effect::Allow), choice("no", Effect::Deny)];
    phone.at(&hub, Seat::Handheld, false, Vec::new());
    rank(vec![needs.clone()]);
    assert!(pushes().is_empty(), "a phone listening on its link hears it there");

    rank(vec![moved(&thread, Phase::Working, 3_000)]);
    let gone = Presence {
        seat: Seat::Handheld,
        active: false,
        workspace: None,
        showing: Vec::new(),
        focus: None,
        listening: false,
    };
    hub.presence(phone.seated.link(), gone);
    rank(vec![needs.clone()]);
    let first = pushes();
    assert_eq!(first.len(), 1, "a phone that stopped listening is pushed to");
    let push = &first[0];
    assert_eq!((push.client, &push.device), (client, &device));
    assert_eq!(push.body.notice.kind, NoticeKind::NeedsYou);
    assert_eq!(push.body.notice.text, "Run cargo test?");
    assert_eq!(push.body.ask, Some(AskId("1".to_owned())), "a yes or no its buttons answer");
    assert!(crate::push::sealed(push).unwrap().urgent);
    hub.rank_ladder();
    rank(vec![needs.clone()]);
    assert!(pushes().is_empty(), "nothing moved, nothing pushed");

    rank(vec![moved(&thread, Phase::Working, 4_000)]);
    mac.at(&hub, Seat::Desk, true, Vec::new());
    rank(vec![needs.clone()]);
    assert!(pushes().is_empty(), "the person at a desk hears it there");
    mac.at(&hub, Seat::Desk, false, Vec::new());

    rank(vec![moved(&thread, Phase::Working, 5_000)]);
    rank(vec![moved(&thread, Phase::Done, 30_000)]);
    assert!(pushes().is_empty(), "a turn under the phone's quiet time");
    rank(vec![moved(&thread, Phase::Working, 31_000)]);
    rank(vec![moved(&thread, Phase::Done, 200_000)]);
    let finished = pushes();
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].body.notice.kind, NoticeKind::Finished);
    assert_eq!(finished[0].body.ask, None);
    assert!(!crate::push::sealed(&finished[0]).unwrap().urgent);

    drop(phone);
    rank(vec![needs.clone()]);
    let queued = pushes();
    assert_eq!(queued.len(), 1, "a phone whose link is gone");
    let (queue_out, queue) = mpsc::channel(8);
    let answering = Arc::new(Answering(
        slopty_push::apns::Outcome::Gone,
        std::sync::atomic::AtomicUsize::new(0),
    ));
    let pusher: Arc<dyn crate::push::Pusher> = Arc::<Answering>::clone(&answering);
    let delivering = tokio::spawn(crate::push::deliver(hub.downgrade(), queue, pusher));
    queue_out.send(queued[0].clone()).await.unwrap();
    drop(queue_out);
    delivering.await.unwrap();
    for _ in 0..100 {
        if hub.devices().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(answering.1.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(hub.devices().is_empty(), "APNs said it is gone");
    assert!(kept.borrow().is_empty(), "and the store forgets it");

    let again = Client::sit(&hub, "phone");
    hub.push_device(again.seated.link(), client, Some(device));
    hub.push_device(Client::sit(&hub, "other link").seated.link(), client, None);
    assert!(hub.devices().is_empty(), "a phone withdraws on any link");
}
