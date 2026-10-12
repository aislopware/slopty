//! What the component preview shows: a thread mid-turn, a board with a task in every lane and
//! a turn's review, written here as the worker and the server would send them. Nothing here
//! is asked of a machine; the views draw these as they draw what comes over the wire.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use slopty_core::{SessionId, WallMs, WorkerId};
use slopty_proto::orchestration::TermRef;
use slopty_proto::project::{
    Assignment, Bounds, Limits, Live, Merge, NativeCounts, Project, ProjectId, ProjectStatus,
    ProjectsPart, TaskCard, TaskId, TaskState, VerifierRun,
};
use slopty_proto::thread::detail::{
    EditDetail, ExecDetail, ExecStatus, Hunk, Offered, Question, ReadDetail,
};
use slopty_proto::thread::wire::{FileDiff, FileKind, Review, ReviewScope};
use slopty_proto::thread::{
    AgentId, AskId, Cap, Changed, Choice, Clipped, Drive, Effect, Item, ItemBody, ItemId, Notice,
    Patch, Phase, Plan, Request, RequestState, Status, Step, ThreadId, ThreadMeta, ThreadState,
    ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, Usage, UserMessage, kind,
};

use crate::project::model::Projects;
use crate::project::{AgentSeen, Seen, WorkerSeen};

/// The one clock every sample reads, so ages and times read the same on every run.
const AT: WallMs = WallMs::from_millis(1_790_000_000_000);

/// `AT` and `secs` seconds.
const fn at(secs: u64) -> WallMs {
    WallMs::from_millis(AT.as_millis().saturating_add(secs.saturating_mul(1_000)))
}

/// The file the sample turn edits, and its one hunk.
const EDITED: &str = "crates/api/src/session.rs";
const HUNK: [&str; 7] = [
    "     pub async fn refresh(&self, presented: &RefreshToken) -> Result<Session, AuthError> {",
    "-        let row = self.store.find_refresh(presented).await?;",
    "+        let row = self.store.take_refresh(presented).await?;",
    "+        if row.used {",
    "+            return Err(AuthError::Replayed);",
    "+        }",
    "         let next = RefreshToken::new();",
];

fn patch() -> Patch {
    Patch {
        hunks: vec![Hunk {
            old_start: 24,
            old_lines: 3,
            new_start: 24,
            new_lines: 6,
            heading: Some("impl Sessions".to_owned()),
            lines: HUNK.iter().map(|l| (*l).to_owned()).collect(),
        }],
        added: 4,
        removed: 1,
        clipped_lines: 0,
        full: None,
    }
}

fn item(id: &str, turn: u32, secs: u64, body: ItemBody) -> Item {
    Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: at(secs), body }
}

fn call(id: &str, secs: u64, (name, what): (&str, &str), title: &str, detail: ToolDetail) -> Item {
    item(
        id,
        1,
        secs,
        ItemBody::Tool(Box::new(ToolCall {
            name: name.to_owned(),
            kind: what.to_owned(),
            title: title.to_owned(),
            input: Clipped::whole(""),
            state: ToolState::Completed,
            output: None,
            images: Vec::new(),
            detail: Some(detail),
            child: None,
            ended_ms: Some(at(secs.saturating_add(1))),
        })),
    )
}

/// A Claude Code thread at work: the person's ask, the agent's answer, its plan, a read, an
/// edit with its diff, a command run, a notice, and a question put to the person.
pub(super) fn thread() -> ThreadState {
    let mut state = ThreadState::new(ThreadMeta {
        modes: Vec::new(),
        efforts: Vec::new(),
        id: ThreadId::new(),
        agent: AgentId::named(AgentId::CLAUDE_CODE),
        agent_version: String::new(),
        native: "preview".to_owned(),
        cwd: "/Users/me/atlas".to_owned(),
        title: "Refuse a replayed refresh token".to_owned(),
        terminal: None,
        parent: None,
        origin: ThreadMeta::PERSON.to_owned(),
        forked_from: None,
        drive: Drive::named(Drive::OBSERVED),
        caps: vec![Cap::named(Cap::APPROVALS), Cap::named(Cap::REVIEW)],
        models: Vec::new(),
        facts: BTreeMap::new(),
        created_ms: AT,
    });
    state.status =
        Status { phase: Phase::NeedsYou, wait: None, since_ms: at(40), ..Status::default() };
    state.turns = vec![Turn {
        id: TurnId(1),
        input: Some(ItemId("ask".to_owned())),
        state: TurnState::Active,
        started_ms: AT,
        ended_ms: None,
        usage: Usage::default(),
        models: vec!["claude-opus".to_owned()],
        changed: Changed { added: 4, removed: 1 },
        before: None,
        after: None,
    }];
    let exec = ExecDetail {
        command: Clipped::whole("cargo test -p atlas-api refresh"),
        description: Some("Run the refresh tests".to_owned()),
        cwd: None,
        background: false,
        task: None,
        status: ExecStatus::Done,
        exit_code: Some(0),
        stderr: None,
        duration_ms: Some(6_200),
    };
    state.items = vec![
        item(
            "ask",
            1,
            0,
            ItemBody::User(UserMessage {
                text: Clipped::whole(
                    "Two refreshes of one token can race. Make the second one fail as a replay.",
                ),
                images: Vec::new(),
                command: None,
                intent: None,
            }),
        ),
        item(
            "first",
            1,
            4,
            ItemBody::Text(Clipped::whole(
                "The store reads the token and replaces it in two steps, so two requests can \
                 both read it before either replaces it. I'll take it in one step and refuse a \
                 token that was already used.",
            )),
        ),
        call(
            "read",
            8,
            ("Read", kind::READ),
            EDITED,
            ToolDetail::Read(ReadDetail {
                path: format!("/Users/me/atlas/{EDITED}"),
                offset: None,
                limit: None,
                lines: Some(142),
                total_lines: Some(142),
            }),
        ),
        call(
            "edit",
            14,
            ("Edit", kind::EDIT),
            EDITED,
            ToolDetail::Edit(EditDetail {
                path: format!("/Users/me/atlas/{EDITED}"),
                edits: 1,
                replace_all: false,
                patch: patch(),
            }),
        ),
        call(
            "test",
            22,
            ("Bash", kind::EXEC),
            "cargo test -p atlas-api refresh",
            ToolDetail::Exec(exec),
        ),
        item(
            "notice",
            1,
            30,
            ItemBody::Notice(Notice::new(
                Notice::HOOK,
                Clipped::whole("The format hook ran on session.rs"),
            )),
        ),
        item(
            "then",
            1,
            34,
            ItemBody::Text(Clipped::whole(
                "The replay now fails with `AuthError::Replayed`. One choice is yours first.",
            )),
        ),
    ];
    state.plan = Some(Plan {
        text: None,
        steps: vec![
            step("Take the token in one step", "completed"),
            step("Refuse a token already used", "completed"),
            step("Test two refreshes that race", "in_progress"),
            step("Say so in the changelog", "pending"),
        ],
    });
    let deny = Choice {
        id: "deny".to_owned(),
        label: "Deny".to_owned(),
        effect: Effect::Deny,
        scope: None,
        stops: false,
    };
    state.requests = vec![Request {
        id: AskId("ask".to_owned()),
        item: None,
        kind: Request::QUESTION.to_owned(),
        title: "What should a replay do to the session?".to_owned(),
        text: None,
        options: vec![deny],
        questions: vec![Question {
            text: "What should a replay do to the session?".to_owned(),
            header: Some("Replay".to_owned()),
            options: vec![
                offered("End it", "Both clients sign in again"),
                offered("Keep it", "Only the second request fails"),
            ],
            multi_select: false,
        }],
        proposed: None,
        schema_json: None,
        url: None,
        state: RequestState::Open,
        opened_ms: at(40),
        until_ms: None,
    }];
    state
}

fn step(text: &str, status: &str) -> Step {
    Step { id: None, text: text.to_owned(), status: status.to_owned() }
}

fn offered(label: &str, means: &str) -> Offered {
    Offered { label: label.to_owned(), description: Some(means.to_owned()) }
}

/// The turn's review: the edited file, and a test added beside it.
pub(super) fn review() -> Review {
    let test = Patch {
        hunks: vec![Hunk {
            old_start: 0,
            old_lines: 0,
            new_start: 1,
            new_lines: 5,
            heading: None,
            lines: vec![
                "+#[tokio::test]".to_owned(),
                "+async fn a_raced_refresh_is_a_replay() {".to_owned(),
                "+    let (a, b) = tokio::join!(refresh(), refresh());".to_owned(),
                "+    assert!(a.is_ok() != b.is_ok());".to_owned(),
                "+}".to_owned(),
            ],
        }],
        added: 5,
        removed: 0,
        clipped_lines: 0,
        full: None,
    };
    let file = |path: &str, from: Option<&str>, patch: Patch| FileDiff {
        path: path.to_owned(),
        old_path: None,
        from: from.map(str::to_owned),
        to: Some(format!("{path}@new")),
        kind: FileKind::Text,
        modes: None,
        patch,
    };
    Review {
        scope: ReviewScope::Turn(TurnId(1)),
        from: None,
        to: None,
        files: vec![
            file(EDITED, Some("session@old"), patch()),
            file("crates/api/tests/refresh.rs", None, test),
        ],
        absent: None,
    }
}

/// The project the board shows.
pub(super) fn project_id() -> Option<ProjectId> {
    ProjectId::new("atlas").ok()
}

/// A board with a task in each lane, as the server sends it, and what the workspace hands the
/// board with it: the machine the tasks run on and the agent that waits on the person.
pub(super) fn board() -> Option<Seen> {
    let id = project_id()?;
    let worker = WorkerId::new();
    let (blocked, running) = (SessionId::new(), SessionId::new());
    let on = |mut card: TaskCard, session: SessionId| {
        card.assignment = Some(Assignment {
            term: TermRef { worker, session },
            thread: None,
            since_ms: AT,
            ended_ms: None,
            conversation: None,
            spawned: true,
        });
        card.branch = Some(format!("atlas/task-{}", card.id));
        card
    };
    let run = |passed: bool| VerifierRun {
        passed,
        summary: if passed {
            "test result: ok. 113 passed".to_owned()
        } else {
            "error[E0308]: mismatched types\n  --> crates/api/src/session.rs:31:9".to_owned()
        },
        head: "9f3c2a1".to_owned(),
        base: "c08d4c1".to_owned(),
        exit: (!passed).then_some(101),
        took_ms: 72_000,
    };
    let mut failed = card(2, "Move sessions to the new store", TaskState::Failed);
    failed.verified = Some(run(false));
    let mut ready = card(6, "Rotate the signing key", TaskState::Done);
    ready.verified = Some(run(true));
    ready.merge = Some(Merge::Queued { since_ms: at(60) });
    let tasks = vec![
        on(card(1, "Refuse a replayed refresh token", TaskState::Blocked), blocked),
        failed,
        on(card(3, "Rate-limit the sign-in route", TaskState::Running), running),
        card(4, "Say so in the changelog", TaskState::Planned),
        card(5, "Expire idle sessions", TaskState::Verifying),
        ready,
        card(7, "Hash tokens at rest", TaskState::Merged),
    ];
    let project = Project {
        orchestrator_spent: slopty_proto::project::Spent::default(),
        id: id.clone(),
        title: "Harden session refresh".to_owned(),
        repo: "atlas".to_owned(),
        repo_id: None,
        target: "main".to_owned(),
        base: None,
        verifier: Some("cargo test".to_owned()),
        push: false,
        orchestrator: None,
        orchestrator_thread: None,
        landed: None,
        limits: Limits::default(),
        metadata: None,
        created_ms: AT,
        goal: None,
        autonomy: slopty_proto::project::Autonomy::default(),
        progress: None,
    };
    let status = ProjectStatus {
        project,
        tasks,
        orchestrator_natives: NativeCounts::default(),
        timeline: Vec::new(),
        next: 1,
        bounds: Bounds::default(),
        live: Live::default(),
    };
    let mut projects = Projects::default();
    projects.apply_part(ProjectsPart { seq: 1, first: true, last: true, projects: vec![status] });
    let board = Arc::clone(projects.get(&id)?);
    let studio =
        WorkerSeen { name: "studio".to_owned(), os: None, form: None, away: false, gone: false };
    let asks = AgentSeen {
        status: slopty_kit::icons::Status::NeedsYou,
        asks: Some("Allow `cargo test`?".to_owned()),
        asked: None,
    };
    let working = AgentSeen { status: slopty_kit::icons::Status::Working, asks: None, asked: None };
    Some(Seen {
        board: Some(board),
        workers: BTreeMap::from([(worker, studio)]),
        agents: HashMap::from([(blocked, asks), (running, working)]),
        now: at(90),
        recap: None,
        orchestrator_ended: false,
    })
}

/// Task `n`, titled, in `state`.
fn card(n: u32, title: &str, state: TaskState) -> TaskCard {
    TaskCard {
        spent: slopty_proto::project::Spent::default(),
        id: TaskId(n),
        depends_on: Vec::new(),
        kind: "build".to_owned(),
        title: title.to_owned(),
        read_only: false,
        state,
        status: None,
        assignment: None,
        branch: None,
        worktree: None,
        pull: None,
        verified: None,
        merge: None,
        step: None,
        natives: NativeCounts::default(),
        created_ms: AT,
        updated_ms: AT,
        give_backs: slopty_proto::project::GiveBacks::default(),
        tests: None,
    }
}
