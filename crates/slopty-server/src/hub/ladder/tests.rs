//! The ladder as the hub ranks it from the rows workers publish, and where its notices go.

use std::collections::BTreeMap;

use slopty_core::SessionId;
use slopty_proto::orchestration::{Outcome, Verb};
use slopty_proto::project::{Placement, TaskId, TaskSpec};
use slopty_proto::server::ToServer;
use slopty_proto::thread::attention::Counts;
use slopty_proto::thread::wire::RequestCard;
use slopty_proto::thread::{
    AgentId, AskId, Changed, Cursor, Drive, ItemId, Link, Liveness, Meters, Phase, Status,
};

use super::super::project_tests::{create, project};
use super::super::tests::{registration, summary};
use super::*;

fn row(phase: Phase, since: u64, terminal: Option<SessionId>) -> ThreadRow {
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
        meters: Meters::default(),
        updated_ms: WallMs::from_millis(since),
        cwd: None,
        repo: None,
        repo_id: None,
    }
}

fn asking(mut row: ThreadRow, title: &str) -> ThreadRow {
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

fn under(mut child: ThreadRow, parent: &ThreadRow) -> ThreadRow {
    child.parent = Some(Link { thread: parent.id, item: ItemId("call".to_owned()) });
    child
}

fn snapshot(rows: Vec<ThreadRow>) -> ToServer {
    ToServer::Threads(TableFrame::Snapshot { cursor: Cursor::default(), rows })
}

fn delta(rows: Vec<ThreadRow>) -> ToServer {
    ToServer::Threads(TableFrame::Delta { cursor: Cursor::default(), rows, removed: Vec::new() })
}

fn standing<'a, K: PartialEq>(list: &'a [(K, Standing)], key: &K) -> &'a Standing {
    &list.iter().find(|(k, _)| k == key).unwrap().1
}

async fn task(hub: &Hub, parent: Option<TaskId>) -> TaskId {
    let spec = TaskSpec {
        title: "Ladder".to_owned(),
        brief: "Rank it.".to_owned(),
        parent,
        placement: Placement::default(),
        ..TaskSpec::default()
    };
    match hub.dispatch(Verb::TaskCreate { project: project(), spec: Box::new(spec) }).await {
        Outcome::Task(task) => task.id,
        other => panic!("{other:?}"),
    }
}

/// A subagent stands with the thread it hangs from, so the one the person sees needs them;
/// the rest roll up by tile, worker, project node (a subtask into its task), project and
/// fleet, each counting a thread once and naming the one to go to first. A thread at rest
/// with changes not kept is to review, above working.
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
    let build = task(&hub, None).await;
    let test = task(&hub, Some(build)).await;
    for (task, session) in [(build, building), (test, testing)] {
        let verb = Verb::TaskAssign { project: project(), task, term: term(session) };
        assert!(matches!(hub.dispatch(verb).await, Outcome::Task(_)));
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
    let build_node = standing(&ladder.nodes, &node(Some(build)));
    assert_eq!(build_node.rung, Rung::NeedsYou, "a subtask rolls up into its task");
    assert_eq!(build_node.top, Some(at(&tester)));
    assert_eq!(build_node.counts, Counts { needs_you: 1, working: 1, ..Counts::default() });
    assert_eq!(standing(&ladder.nodes, &node(Some(test))).counts.total(), 1);
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
struct Client {
    seated: Seated,
    rx: mpsc::Receiver<FromServer>,
}

impl Client {
    fn sit(hub: &Hub, name: &str) -> Self {
        let (tx, rx) = mpsc::channel(8);
        Self { seated: hub.seat(hub.number_link(), name.to_owned(), tx), rx }
    }

    fn at(&self, hub: &Hub, seat: Seat, active: bool, showing: Vec<TermRef>) {
        let presence = Presence { seat, active, workspace: None, showing, focus: None };
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
    assert_eq!((notice.kind, notice.thread.thread), (NoticeKind::NeedsYou, parent.id));
    assert_eq!((notice.tile, notice.text.as_str()), (Some(shell), "Run cargo test?"));
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

/// What a project's agents spent comes with the thread table every worker publishes, the
/// whole table again at each registration: each thread's own figure under the terminal its
/// family runs in, so a subagent's counts on its root's task, and a thread in no project's
/// terminal counts for none. A figure published again counts once.
#[tokio::test]
async fn a_project_s_spend_comes_with_its_threads_rows() {
    let hub = Hub::new("server".to_owned(), Vec::new());
    let (orchestrating, building, elsewhere) =
        (SessionId::new(), SessionId::new(), SessionId::new());
    let worker = WorkerId::new();
    let sessions = vec![summary(orchestrating), summary(building), summary(elsewhere)];
    let (tx, _rx) = mpsc::channel(8);
    let lease = hub.register(registration(worker, sessions), [100, 64, 0, 7].into(), tx).unwrap();
    let term = |session| TermRef { worker, session };
    create(&hub, Some(term(orchestrating))).await;
    let build = task(&hub, None).await;
    let verb = Verb::TaskAssign { project: project(), task: build, term: term(building) };
    assert!(matches!(hub.dispatch(verb).await, Outcome::Task(_)));

    let costing = |terminal, cost| {
        let mut row = row(Phase::Working, 10, terminal);
        row.meters.cost_micro_usd = Some(cost);
        row
    };
    let orchestrator = costing(Some(orchestrating), 1_000_000);
    let builder = costing(Some(building), 2_000_000);
    let subagent = under(costing(None, 500_000), &builder);
    let stranger = costing(Some(elsewhere), 9_000_000);
    let mut windowed = costing(None, 0);
    windowed.meters.limits = vec![slopty_proto::thread::Limit {
        name: "five-hour".to_owned(),
        used_bp: 4_200,
        resets_ms: None,
    }];
    let windowed = under(windowed, &orchestrator);
    let rows = vec![orchestrator.clone(), builder, subagent, stranger, windowed];
    lease.handle(snapshot(rows.clone()));
    let spend = || hub.inner.state.lock().projects.project(&project()).unwrap().spend.clone();
    assert_eq!(spend().cost_micro_usd, 3_500_000);
    assert_eq!(spend().windows.get("five-hour"), Some(&4_200));
    lease.handle(snapshot(rows));
    assert_eq!(spend().cost_micro_usd, 3_500_000, "the same figures again");
    lease.handle(delta(vec![costing(Some(orchestrating), 1_500_000)]));
    assert_eq!(spend().cost_micro_usd, 5_000_000, "a new thread of the orchestrator's adds");
    let orchestrator = ThreadRow {
        meters: Meters { cost_micro_usd: Some(1_200_000), ..Meters::default() },
        ..orchestrator
    };
    lease.handle(delta(vec![orchestrator]));
    assert_eq!(spend().cost_micro_usd, 5_200_000, "a later figure replaces its own");
}
