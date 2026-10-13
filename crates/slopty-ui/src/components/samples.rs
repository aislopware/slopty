//! What the component preview shows: a thread mid-turn, written here as the worker would send
//! it. Nothing here is asked of a machine; the view draws it as it draws what comes over the
//! wire.

use std::collections::BTreeMap;

use slopty_core::WallMs;
use slopty_proto::thread::detail::{
    EditDetail, ExecDetail, ExecStatus, Hunk, Offered, Question, ReadDetail,
};
use slopty_proto::thread::{
    AgentId, AskId, Cap, Changed, Choice, Clipped, Drive, Effect, Item, ItemBody, ItemId, Notice,
    Patch, Phase, Plan, Request, RequestState, Status, Step, ThreadId, ThreadMeta, ThreadState,
    ToolCall, ToolDetail, ToolState, Turn, TurnId, TurnState, Usage, UserMessage, kind,
};

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
