//! Golden byte snapshots of the thread model (`slopty_proto::thread`): what a client asks of a
//! worker's threads, the frames a followed thread streams, every action, and the thread table.
//! A changed snapshot is a wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_thread {
    use std::collections::BTreeMap;

    use slopty_core::{ClientId, DisplayId, SessionId, WallMs, WindowId};
    use slopty_proto::codec;
    use slopty_proto::screen::CaptureTarget;
    use slopty_proto::search::Span;
    use slopty_proto::thread::detail::{
        AgentDetail, Answer, EditDetail, ExecDetail, ExecStatus, FetchDetail, Hunk, McpDetail,
        Offered, Question, QuestionDetail, ReadDetail, SearchDetail, WebLink, WebSearchDetail,
        WriteDetail,
    };
    use slopty_proto::thread::wire::{
        AuthorRun, Authors, Expanded, FileDiff, FileKind, Intent, IntentDone, ItemHit, Modes,
        NewWorktree, Outcome, Page, PastSession, PastSessions, Pick, PromptHit, PullSeen,
        PullStands, Review, ReviewScope, Setup, Start, TableFrame, ThreadFrame, ThreadHit,
        ThreadHits, ThreadRequest,
    };
    use slopty_proto::thread::{
        Action, AgentId, AgentScreen, Answerer, AskId, BackgroundTask, Cap, Changed, Choice,
        Clipped, Command, Compaction, ContentRef, Cursor, Delivery, Drive, Edge, Effect, Effort,
        Fork, Goal, Image, IntentId, Item, ItemBody, ItemId, Limit, Link, Liveness, Meters, Mode,
        Model, Notice, PartKey, Patch, Pending, PendingState, Phase, Plan, Request, RequestState,
        Retry, Status, Step, ThreadId, ThreadMeta, ThreadState, ToolCall, ToolDetail, ToolState,
        TreeRef, Turn, TurnId, TurnState, Usage, UserMessage, Wait, kind,
    };
    use uuid::Uuid;

    fn hex(bytes: &[u8]) -> String {
        bytes
            .chunks(16)
            .map(|row| row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[track_caller]
    fn snap<T: serde::Serialize>(name: &str, msg: &T) {
        let bytes = codec::encode(msg).expect("encodes");
        insta::assert_snapshot!(name, hex(&bytes));
    }

    fn thread() -> ThreadId {
        ThreadId::from_uuid(Uuid::from_u128(0x7417))
    }

    fn intent() -> IntentId {
        IntentId::from_uuid(Uuid::from_u128(0x1e7))
    }

    fn ms(n: u64) -> WallMs {
        WallMs::from_millis(n)
    }

    fn clip(text: &str) -> Clipped {
        Clipped::whole(text)
    }

    fn meta() -> ThreadMeta {
        ThreadMeta {
            id: thread(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: "2.1.286".to_owned(),
            native: "5e55105e".to_owned(),
            cwd: "/work".to_owned(),
            title: "Fix the build".to_owned(),
            terminal: Some(SessionId::from_uuid(Uuid::from_u128(0x5e55))),
            parent: Some(Link {
                thread: ThreadId::from_uuid(Uuid::from_u128(0x9a)),
                item: ItemId("toolu_1".to_owned()),
            }),
            origin: ThreadMeta::SUBAGENT.to_owned(),
            forked_from: Some(Fork {
                thread: ThreadId::from_uuid(Uuid::from_u128(0xf0)),
                turn: Some(TurnId(2)),
            }),
            drive: Drive::named(Drive::OBSERVED),
            caps: vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)],
            models: vec![Model { id: "opus".to_owned(), label: "Opus".to_owned() }],
            modes: vec![Mode {
                id: "plan".to_owned(),
                label: "Plan".to_owned(),
                description: Some("Reads and plans, changes nothing".to_owned()),
            }],
            efforts: vec![Effort {
                id: "high".to_owned(),
                label: "High".to_owned(),
                description: Some("Thinks longer".to_owned()),
            }],
            facts: BTreeMap::from([("branch".to_owned(), "main".to_owned())]),
            created_ms: ms(1_000),
        }
    }

    fn status() -> Status {
        Status {
            phase: Phase::NeedsYou,
            wait: Some(Wait {
                kind: "permission".to_owned(),
                text: "Wants to run cargo test".to_owned(),
            }),
            liveness: Liveness::Live,
            since_ms: ms(2_000),
        }
    }

    fn usage() -> Usage {
        Usage(BTreeMap::from([(Usage::INPUT.to_owned(), 12), (Usage::OUTPUT.to_owned(), 20)]))
    }

    fn turn() -> Turn {
        Turn {
            id: TurnId(1),
            input: Some(ItemId("u1".to_owned())),
            state: TurnState::Active,
            started_ms: ms(1_500),
            ended_ms: None,
            usage: usage(),
            models: vec!["claude-opus-5-5".to_owned()],
            changed: Changed { added: 3, removed: 1 },
            before: Some(TreeRef("4b825dc6".to_owned())),
            after: None,
        }
    }

    fn patch() -> Patch {
        Patch {
            hunks: vec![Hunk {
                old_start: 1,
                old_lines: 1,
                new_start: 1,
                new_lines: 1,
                heading: Some("fn main() {".to_owned()),
                lines: vec!["-a".to_owned(), "+b".to_owned()],
            }],
            added: 1,
            removed: 1,
            clipped_lines: 0,
            full: None,
        }
    }

    fn image() -> Image {
        Image {
            digest: "af13".to_owned(),
            media_type: "image/png".to_owned(),
            bytes: 2_048,
            width: 64,
            height: 32,
            at: ContentRef("r1#0".to_owned()),
        }
    }

    fn call(kind: &str, detail: Option<ToolDetail>) -> ToolCall {
        ToolCall {
            name: "Bash".to_owned(),
            kind: kind.to_owned(),
            title: "Run cargo test".to_owned(),
            input: clip(r#"{"command":"cargo test"}"#),
            state: ToolState::Running,
            output: Some(Clipped::tail(
                "ok\n",
                slopty_proto::thread::detail::Clip { lines: 4, chars: 80 },
                None,
            )),
            images: vec![],
            detail,
            child: None,
            ended_ms: None,
        }
    }

    fn item(id: &str, body: ItemBody) -> Item {
        Item { id: ItemId(id.to_owned()), turn: TurnId(1), at_ms: ms(1_600), body }
    }

    fn request() -> Request {
        Request {
            id: AskId("ask-1".to_owned()),
            item: Some(ItemId("toolu_2".to_owned())),
            kind: Request::APPROVAL.to_owned(),
            title: "Run cargo test?".to_owned(),
            text: Some(clip("cargo test --workspace")),
            options: vec![
                Choice {
                    id: "allow".to_owned(),
                    label: "Allow".to_owned(),
                    effect: Effect::Allow,
                    scope: None,
                    stops: false,
                },
                Choice {
                    id: "always".to_owned(),
                    label: "Always allow".to_owned(),
                    effect: Effect::Allow,
                    scope: Some("Bash(cargo test:*)".to_owned()),
                    stops: false,
                },
                Choice {
                    id: "deny".to_owned(),
                    label: "No".to_owned(),
                    effect: Effect::Deny,
                    scope: None,
                    stops: true,
                },
            ],
            questions: vec![],
            proposed: Some(patch()),
            schema_json: None,
            url: None,
            state: RequestState::Open,
            opened_ms: ms(1_700),
            until_ms: Some(ms(61_700)),
        }
    }

    fn pending() -> Pending {
        Pending {
            intent: intent(),
            text: "and the docs".to_owned(),
            attachments: vec!["/Users/me/.slopty/drop/x/shot.png".to_owned()],
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        }
    }

    fn goal() -> Goal {
        Goal {
            objective: "Make the parser pass every fixture".to_owned(),
            state: Goal::ACTIVE.to_owned(),
            tokens_used: 41_000,
            token_budget: Some(500_000),
            time_used_s: 380,
            updated_ms: ms(1_800),
        }
    }

    fn meters() -> Meters {
        Meters {
            model: Some("Opus 5.5".to_owned()),
            model_id: Some("claude-opus-5-5".to_owned()),
            mode: Some("default".to_owned()),
            effort: Some("high".to_owned()),
            context_tokens: Some(42_000),
            context_window: Some(200_000),
            limits: vec![Limit {
                name: "five-hour".to_owned(),
                used_bp: 1_250,
                resets_ms: Some(ms(99)),
            }],
        }
    }

    fn screen() -> AgentScreen {
        AgentScreen {
            target: CaptureTarget::Window(WindowId(4_242)),
            kind: AgentScreen::SIMULATOR.to_owned(),
            label: "Simulator — iPhone 17 Pro".to_owned(),
            used_ms: ms(1_727_000_000_000),
        }
    }

    fn state() -> ThreadState {
        let mut state = ThreadState::new(meta());
        for action in [
            Action::Status(status()),
            Action::TurnStarted(turn()),
            Action::ItemStarted(item(
                "u1",
                ItemBody::User(UserMessage {
                    text: clip("fix it"),
                    images: vec![image()],
                    command: None,
                    intent: Some(intent()),
                }),
            )),
            Action::ItemStarted(item("a1", ItemBody::Text(clip("On it.")))),
            Action::ItemStarted(item("toolu_2", ItemBody::Tool(Box::new(call(kind::EXEC, None))))),
            Action::RequestOpened(Box::new(request())),
            Action::PendingSet(vec![pending()]),
            Action::MetersSet(meters()),
            Action::ToReview(true),
            Action::GoalSet(Some(goal())),
            Action::ScreensSet(vec![screen()]),
            Action::PullSeen(Some(pull_seen())),
        ] {
            state.apply(&action);
        }
        state
    }

    #[test]
    fn requests() {
        snap("client_table", &ThreadRequest::Table { have: Some(Cursor { epoch: 2, seq: 40 }) });
        snap(
            "client_follow",
            &ThreadRequest::Follow {
                thread: thread(),
                have: Some(Cursor { epoch: 1, seq: 7 }),
                turns: 20,
                max_latency_ms: 16,
            },
        );
        snap(
            "client_follow_fresh",
            &ThreadRequest::Follow { thread: thread(), have: None, turns: 20, max_latency_ms: 0 },
        );
        snap("client_unfollow", &ThreadRequest::Unfollow { thread: thread() });
        snap(
            "client_page",
            &ThreadRequest::Page { thread: thread(), before: TurnId(4), turns: 10 },
        );
        snap(
            "client_expand",
            &ThreadRequest::Expand { thread: thread(), content: ContentRef("r1#2".to_owned()) },
        );
        snap(
            "client_sessions",
            &ThreadRequest::Sessions {
                agent: Some(AgentId::named(AgentId::CLAUDE_CODE)),
                cwd: Some("/work".to_owned()),
                query: String::new(),
                limit: 50,
            },
        );
        snap(
            "client_sessions_search",
            &ThreadRequest::Sessions {
                agent: None,
                cwd: None,
                query: "flaky login".to_owned(),
                limit: 20,
            },
        );
        snap(
            "client_thread_search",
            &ThreadRequest::Search { query: "flaky login".to_owned(), limit: 20 },
        );
        snap(
            "client_thread_authors",
            &ThreadRequest::Authors { thread: Some(thread()), path: "src/auth.rs".to_owned() },
        );
        snap(
            "client_start",
            &ThreadRequest::Start {
                id: intent(),
                start: Box::new(Start {
                    agent: AgentId::named(AgentId::CODEX),
                    cwd: "/work".to_owned(),
                    drive: Some(Drive::named(Drive::SHARED)),
                    prompt: Some("fix the build".to_owned()),
                    model: Some("gpt-5.5".to_owned()),
                    mode: Some("on-request".to_owned()),
                    effort: Some("high".to_owned()),
                    attachments: vec!["/work/shot.png".to_owned()],
                    args: vec!["--yolo".to_owned()],
                    worktree: None,
                }),
            },
        );
        // A start in a worktree of its own, made from the clone `cwd` is in.
        snap(
            "client_start_in_worktree",
            &ThreadRequest::Start {
                id: intent(),
                start: Box::new(Start {
                    agent: AgentId::named(AgentId::CLAUDE_CODE),
                    cwd: "/work/web".to_owned(),
                    drive: None,
                    prompt: None,
                    model: None,
                    mode: None,
                    effort: None,
                    attachments: Vec::new(),
                    args: Vec::new(),
                    worktree: Some(NewWorktree::named("claude-3f9a2c")),
                }),
            },
        );
    }

    #[test]
    fn intents() {
        let send = |intent_: Intent| ThreadRequest::Intent {
            id: intent(),
            thread: thread(),
            intent: intent_,
        };
        snap(
            "intent_send_steer",
            &send(Intent::Send {
                text: "use the other test".to_owned(),
                delivery: Delivery::Steer,
                attachments: vec![],
            }),
        );
        snap(
            "intent_send_queue",
            &send(Intent::Send {
                text: "then the docs".to_owned(),
                delivery: Delivery::Queue,
                attachments: vec![],
            }),
        );
        snap(
            "intent_send_attachments",
            &send(Intent::Send {
                text: "what is wrong here?".to_owned(),
                delivery: Delivery::Steer,
                attachments: vec![
                    "/Users/me/.slopty/drop/x/shot.png".to_owned(),
                    "/work/notes.md".to_owned(),
                ],
            }),
        );
        snap(
            "intent_send_at",
            &send(Intent::Send {
                text: "Run the nightly checks".to_owned(),
                delivery: Delivery::At { at_ms: ms(1_800_000) },
                attachments: Vec::new(),
            }),
        );
        snap(
            "intent_send_interrupt",
            &send(Intent::Send {
                text: "Stop, and fix the parser first".to_owned(),
                delivery: Delivery::Interrupt,
                attachments: Vec::new(),
            }),
        );
        snap("intent_withdraw", &send(Intent::Withdraw { pending: intent() }));
        snap(
            "intent_edit",
            &send(Intent::Edit { pending: intent(), text: "then the README".to_owned() }),
        );
        snap("intent_promote", &send(Intent::Promote { pending: intent() }));
        snap("intent_interrupt", &send(Intent::Interrupt));
        snap(
            "intent_answer",
            &send(Intent::Answer {
                ask: AskId("ask-1".to_owned()),
                choice: "always".to_owned(),
                message: Some("fine".to_owned()),
            }),
        );
        snap("intent_release", &send(Intent::Release { ask: AskId("ask-1".to_owned()) }));
        snap("intent_set_model", &send(Intent::SetModel { model: "claude-opus-5-5".to_owned() }));
        snap("intent_set_mode", &send(Intent::SetMode { mode: "plan".to_owned() }));
        snap("intent_set_effort", &send(Intent::SetEffort { effort: "high".to_owned() }));
        snap("intent_aside", &send(Intent::Aside));
        snap("intent_discard", &send(Intent::Discard));
        snap("intent_keep_aside", &send(Intent::KeepAside));
        snap("intent_compact", &send(Intent::Compact));
        snap("intent_handoff", &send(Intent::Handoff));
        snap("intent_take_back", &send(Intent::TakeBack));
        snap("intent_fork", &send(Intent::Fork { after: Some(TurnId(3)) }));
        snap("intent_fork_whole", &send(Intent::Fork { after: None }));
        snap("intent_continue", &send(Intent::Continue { agent: AgentId::named(AgentId::PI) }));
        snap("intent_rewind", &send(Intent::Rewind { turn: TurnId(3), files: true }));
    }

    #[test]
    fn outcomes() {
        let done = |outcome| IntentDone { id: intent(), outcome };
        snap("outcome_done", &done(Outcome::Done));
        snap("outcome_accepted", &done(Outcome::Accepted));
        snap("outcome_started", &done(Outcome::Started { thread: thread() }));
        snap(
            "outcome_refused",
            &done(Outcome::Refused {
                reason: "Your draft in the terminal is in the way".to_owned(),
            }),
        );
        snap("outcome_unsupported", &done(Outcome::Unsupported { cap: Cap::named(Cap::SET_MODE) }));
        snap(
            "outcome_setup_failed",
            &done(Outcome::SetupFailed {
                setup: Setup {
                    from: "conductor.json".to_owned(),
                    tail: vec!["bun install".to_owned(), "error: lockfile had changes".to_owned()],
                },
                code: Some(1),
            }),
        );
    }

    #[test]
    fn frames() {
        snap(
            "frame_snapshot",
            &ThreadFrame::Snapshot {
                cursor: Cursor { epoch: 1, seq: 7 },
                state: Box::new(state()),
            },
        );
        snap(
            "frame_page",
            &ThreadFrame::Page(Page {
                turns: vec![turn()],
                items: vec![item("a1", ItemBody::Text(clip("Done.")))],
                older: true,
            }),
        );
        snap(
            "frame_expanded_text",
            &ThreadFrame::Expanded {
                content: ContentRef("r1#2".to_owned()),
                body: Expanded::Text("all".to_owned()),
            },
        );
        snap(
            "frame_expanded_bytes",
            &ThreadFrame::Expanded {
                content: ContentRef("r1#0".to_owned()),
                body: Expanded::Bytes(vec![0x89, 0x50, 0x4e, 0x47]),
            },
        );
        snap(
            "frame_expanded_gone",
            &ThreadFrame::Expanded { content: ContentRef("r9".to_owned()), body: Expanded::Gone },
        );
    }

    /// Every action, one frame.
    #[test]
    fn actions() {
        let answered = RequestState::Answered {
            by: Answerer {
                client: Some(ClientId::from_uuid(Uuid::from_u128(0x42))),
                name: "iPad".to_owned(),
            },
            choice: "allow".to_owned(),
        };
        let actions = vec![
            Action::Meta(Box::new(meta())),
            Action::Status(status()),
            Action::Status(Status {
                phase: Phase::Idle,
                wait: None,
                liveness: Liveness::Exited { resumable: true },
                since_ms: ms(3),
            }),
            Action::TurnStarted(turn()),
            Action::TurnEnded {
                turn: TurnId(1),
                state: TurnState::Failed { error: "overloaded".to_owned(), until_ms: None },
                usage: usage(),
                ended_ms: ms(5),
            },
            Action::ItemStarted(item("a1", ItemBody::Reasoning(clip("thinking")))),
            Action::Append {
                item: ItemId("a1".to_owned()),
                part: PartKey::Body,
                text: " more".to_owned(),
            },
            Action::Append {
                item: ItemId("toolu_2".to_owned()),
                part: PartKey::Input,
                text: "{\"co".to_owned(),
            },
            Action::Append {
                item: ItemId("toolu_2".to_owned()),
                part: PartKey::Output,
                text: "ok\n".to_owned(),
            },
            Action::ItemUpdated(item("toolu_2", ItemBody::Tool(Box::new(call(kind::EXEC, None))))),
            Action::ItemCompleted(item(
                "c1",
                ItemBody::Compaction(Compaction {
                    trigger: Some("auto".to_owned()),
                    before_tokens: Some(180_000),
                    after_tokens: Some(20_000),
                    summary: Some(clip("so far")),
                }),
            )),
            Action::ItemCompleted(item(
                "n1",
                ItemBody::Notice(Notice::new(Notice::INTERRUPTED, clip("Stopped"))),
            )),
            Action::ItemCompleted(item(
                "n2",
                ItemBody::Notice(Notice {
                    kind: Notice::API_ERROR.to_owned(),
                    text: clip("Overloaded"),
                    retry: Some(Retry { attempt: 2, max: Some(10), in_ms: Some(1_200) }),
                }),
            )),
            Action::ItemCompleted(item("v1", ItemBody::Review { entered: true })),
            Action::ItemCompleted(item(
                "x1",
                ItemBody::Extra { kind: "queue-operation".to_owned(), json: clip("{}") },
            )),
            Action::ItemRemoved { item: ItemId("x1".to_owned()) },
            Action::RequestOpened(Box::new(request())),
            Action::RequestResolved { id: AskId("ask-1".to_owned()), state: answered },
            Action::RequestResolved {
                id: AskId("ask-2".to_owned()),
                state: RequestState::Released,
            },
            Action::RequestResolved {
                id: AskId("ask-3".to_owned()),
                state: RequestState::Withdrawn,
            },
            Action::PendingSet(vec![
                pending(),
                Pending {
                    intent: IntentId::from_uuid(Uuid::from_u128(0x1e8)),
                    text: "look".to_owned(),
                    attachments: vec![],
                    delivery: Delivery::Steer,
                    state: PendingState::Held {
                        reason: "Your draft in the terminal is in the way".to_owned(),
                    },
                },
            ]),
            Action::PlanSet(Some(Plan {
                text: Some(clip("Three steps")),
                steps: vec![Step {
                    id: Some("1".to_owned()),
                    text: "Build".to_owned(),
                    status: "in_progress".to_owned(),
                }],
            })),
            Action::PlanSet(None),
            Action::TasksSet(vec![BackgroundTask {
                id: "b1".to_owned(),
                kind: BackgroundTask::SHELL.to_owned(),
                title: "cargo watch".to_owned(),
                state: BackgroundTask::RUNNING.to_owned(),
                item: Some(ItemId("toolu_3".to_owned())),
                output: Some(clip("watching")),
                started_ms: WallMs::from_millis(1_727_000_000_000),
                ended_ms: None,
            }]),
            Action::MetersSet(meters()),
            Action::CommandsSet(vec![Command {
                name: "compact".to_owned(),
                description: "Compact the context".to_owned(),
                argument_hint: Some("[instructions]".to_owned()),
                source: "built-in".to_owned(),
            }]),
            Action::Truncated { after: Some(TurnId(1)) },
            Action::Truncated { after: None },
            Action::Snapshot {
                turn: TurnId(1),
                edge: Edge::After,
                tree: TreeRef("9f2e".to_owned()),
            },
            Action::ToReview(true),
            Action::GoalSet(Some(goal())),
            Action::GoalSet(None),
            Action::ScreensSet(vec![
                screen(),
                AgentScreen {
                    target: CaptureTarget::Display(DisplayId(1)),
                    kind: AgentScreen::DESKTOP.to_owned(),
                    label: "Display 1".to_owned(),
                    used_ms: ms(1_727_000_000_500),
                },
            ]),
            Action::ScreensSet(Vec::new()),
            Action::PullSeen(Some(pull_seen())),
            Action::PullSeen(None),
        ];
        snap("frame_actions", &ThreadFrame::Actions { epoch: 1, first: 8, next: 40, actions });
    }

    /// A branch's pull request with a failed check, as the worker sums it up.
    fn pull_seen() -> PullSeen {
        PullSeen {
            forge: slopty_proto::git::Forge::GitHub,
            number: 42,
            url: "https://github.com/o/r/pull/42".to_owned(),
            title: "Fix the login".to_owned(),
            base: "main".to_owned(),
            stands: PullStands::ChecksFailed,
            failed: 1,
            failed_first: Some("lint".to_owned()),
            running: 1,
        }
    }

    /// Every typed tool detail, and the user message's parts.
    #[test]
    fn tool_details() {
        let details = vec![
            ToolDetail::Edit(EditDetail {
                path: "src/lib.rs".to_owned(),
                edits: 1,
                replace_all: false,
                patch: patch(),
            }),
            ToolDetail::Write(WriteDetail {
                path: "a.txt".to_owned(),
                lines: 3,
                created: Some(true),
                patch: Patch::default(),
            }),
            ToolDetail::Read(ReadDetail {
                path: "a.txt".to_owned(),
                offset: Some(1),
                limit: Some(50),
                lines: Some(3),
                total_lines: Some(3),
            }),
            ToolDetail::Search(SearchDetail {
                pattern: "fn main".to_owned(),
                path: Some("src".to_owned()),
                glob: Some("*.rs".to_owned()),
                files: Some(2),
                matches: Some(4),
                truncated: false,
            }),
            ToolDetail::Exec(ExecDetail {
                command: clip("cargo test"),
                description: Some("Run the tests".to_owned()),
                cwd: Some("/work".to_owned()),
                background: true,
                task: Some("b1".to_owned()),
                status: ExecStatus::Failed,
                exit_code: Some(101),
                stderr: Some(clip("error")),
                duration_ms: Some(1_200),
            }),
            ToolDetail::Fetch(FetchDetail {
                url: "https://example.com".to_owned(),
                prompt: None,
                code: Some(200),
                bytes: Some(512),
            }),
            ToolDetail::WebSearch(WebSearchDetail {
                query: "gpui".to_owned(),
                results: Some(1),
                links: vec![WebLink {
                    title: "GPUI".to_owned(),
                    url: "https://gpui.rs".to_owned(),
                }],
            }),
            ToolDetail::Agent(AgentDetail {
                agent_type: Some("general-purpose".to_owned()),
                description: Some("Look".to_owned()),
                prompt: clip("look around"),
                background: false,
                report: Some(clip("found it")),
                tokens: Some(32),
                tool_uses: Some(0),
                duration_ms: Some(46),
            }),
            ToolDetail::Question(QuestionDetail {
                questions: vec![Question {
                    text: "Which?".to_owned(),
                    header: Some("Pick".to_owned()),
                    options: vec![Offered { label: "A".to_owned(), description: None }],
                    multi_select: false,
                }],
                answers: vec![Answer { question: "Which?".to_owned(), answer: "A".to_owned() }],
            }),
            ToolDetail::Plan { text: clip("1. build") },
            ToolDetail::Tasks {
                steps: vec![Step {
                    id: None,
                    text: "Build".to_owned(),
                    status: "pending".to_owned(),
                }],
                whole: true,
            },
            ToolDetail::Mcp(McpDetail {
                server: "github".to_owned(),
                tool: "create_issue".to_owned(),
            }),
        ];
        let items: Vec<Item> = details
            .into_iter()
            .map(|d| {
                let mut c = call(kind::OTHER, Some(d));
                c.child = Some(thread());
                c.images = vec![image()];
                c.ended_ms = Some(ms(7));
                item("toolu_9", ItemBody::Tool(Box::new(c)))
            })
            .collect();
        snap("frame_tool_details", &ThreadFrame::Page(Page { turns: vec![], items, older: false }));
    }

    /// A review asked for, sent, and a change kept and put back.
    #[test]
    fn review() {
        snap(
            "review_request",
            &ThreadRequest::Review { thread: thread(), scope: ReviewScope::Turn(TurnId(2)) },
        );
        snap("review_since", &ReviewScope::Since(TurnId(1)));
        snap("review_kept", &ReviewScope::Kept);
        let hunk = Hunk {
            old_start: 3,
            old_lines: 1,
            new_start: 3,
            new_lines: 2,
            heading: None,
            lines: vec!["-a".to_owned(), "+b".to_owned(), "+c".to_owned()],
        };
        let patch = Patch { hunks: vec![hunk], added: 2, removed: 1, clipped_lines: 0, full: None };
        let review = Review {
            scope: ReviewScope::Kept,
            from: Some(TreeRef("4b825dc6".to_owned())),
            to: Some(TreeRef("9d1e0f3a".to_owned())),
            files: vec![
                FileDiff {
                    path: "src/lib.rs".to_owned(),
                    from: Some("aa11".to_owned()),
                    to: Some("bb22".to_owned()),
                    kind: FileKind::Text,
                    old_path: None,
                    modes: None,
                    patch,
                },
                FileDiff {
                    path: "bin/run".to_owned(),
                    old_path: Some("scripts/run".to_owned()),
                    from: Some("dd44".to_owned()),
                    to: Some("dd44".to_owned()),
                    kind: FileKind::Text,
                    modes: Some(Modes { from: 0o100_644, to: 0o100_755 }),
                    patch: Patch::default(),
                },
                FileDiff {
                    path: "data/huge.csv".to_owned(),
                    old_path: None,
                    from: Some("ee55".to_owned()),
                    to: Some("ff66".to_owned()),
                    kind: FileKind::TooLarge { bytes: 6_000_000 },
                    modes: None,
                    patch: Patch::default(),
                },
                FileDiff {
                    path: "logo.png".to_owned(),
                    from: None,
                    to: Some("cc33".to_owned()),
                    kind: FileKind::Image { bytes: 2048 },
                    old_path: None,
                    modes: None,
                    patch: Patch {
                        hunks: vec![],
                        added: 0,
                        removed: 0,
                        clipped_lines: 0,
                        full: None,
                    },
                },
            ],
            absent: None,
        };
        snap("review_frame", &ThreadFrame::Review(Box::new(review)));
        let pick = Pick {
            path: "src/lib.rs".to_owned(),
            from: Some("aa11".to_owned()),
            stamp: Some("bb22".to_owned()),
            hunks: vec![0],
            old_path: None,
        };
        snap("review_keep", &Intent::Keep(pick.clone()));
        snap("review_revert", &Intent::Revert(Pick { hunks: vec![], ..pick }));
        let asked = Intent::Review {
            from: TreeRef("4b825dc".to_owned()),
            to: TreeRef("9d1e7aa".to_owned()),
        };
        snap("review_by_agent", &asked);
    }

    /// The thread messages as they ride the control stream and open a stream of their own.
    #[test]
    fn on_the_link() {
        use slopty_proto::transfer::UniHead;
        use slopty_proto::{ClientMsg, WorkerMsg};
        let follow = ThreadRequest::Follow {
            thread: thread(),
            have: Some(Cursor { epoch: 1, seq: 7 }),
            turns: 20,
            max_latency_ms: 16,
        };
        snap("link_client_thread", &ClientMsg::Thread(follow));
        let row = state().row(ms(2_500));
        let delta = TableFrame::Delta {
            cursor: Cursor { epoch: 1, seq: 4 },
            rows: vec![row],
            removed: vec![],
        };
        snap("link_worker_threads", &WorkerMsg::Threads(delta));
        snap(
            "link_worker_intent_done",
            &WorkerMsg::IntentDone(IntentDone { id: intent(), outcome: Outcome::Accepted }),
        );
        snap(
            "link_worker_setting_up",
            &WorkerMsg::SettingUp {
                id: intent(),
                setup: Setup {
                    from: ".cursor/worktrees.json".to_owned(),
                    tail: vec!["Resolving packages".to_owned()],
                },
            },
        );
        snap(
            "link_worker_sessions",
            &WorkerMsg::Sessions(PastSessions {
                agent: Some(AgentId::named(AgentId::CODEX)),
                cwd: Some("/work".to_owned()),
                query: String::new(),
                sessions: vec![PastSession {
                    agent: AgentId::named(AgentId::CODEX),
                    native: "019a-c0de".to_owned(),
                    cwd: Some("/work".to_owned()),
                    title: Some("Fix the build".to_owned()),
                    updated_ms: Some(ms(1_700_000)),
                    thread: Some(thread()),
                    resume: vec!["resume".to_owned(), "019a-c0de".to_owned()],
                    facts: BTreeMap::from([("branch".to_owned(), "main".to_owned())]),
                    prompts: vec![],
                }],
                absent: None,
                cut: None,
            }),
        );
        snap(
            "link_worker_sessions_absent",
            &WorkerMsg::Sessions(PastSessions {
                agent: Some(AgentId::named(AgentId::PI)),
                cwd: Some("/work".to_owned()),
                query: String::new(),
                sessions: vec![],
                absent: Some("pi is not installed".to_owned()),
                cut: None,
            }),
        );
        snap(
            "link_worker_sessions_found",
            &WorkerMsg::Sessions(PastSessions {
                agent: None,
                cwd: None,
                query: "flaky login".to_owned(),
                sessions: vec![PastSession {
                    agent: AgentId::named(AgentId::CLAUDE_CODE),
                    native: "5f0c-1d".to_owned(),
                    cwd: Some("/work".to_owned()),
                    title: Some("Look at CI".to_owned()),
                    updated_ms: Some(ms(1_800_000)),
                    thread: None,
                    resume: vec!["--resume".to_owned(), "5f0c-1d".to_owned()],
                    facts: BTreeMap::new(),
                    prompts: vec![PromptHit {
                        text: "fix the flaky login test".to_owned(),
                        spans: vec![Span { start: 8, end: 13 }, Span { start: 14, end: 19 }],
                        cut_before: false,
                        cut_after: true,
                        at_ms: Some(ms(1_790_000)),
                    }],
                }],
                absent: None,
                cut: Some("Claude Code's prompt history is read from its last 64 MiB".to_owned()),
            }),
        );
        snap(
            "link_worker_thread_hits",
            &WorkerMsg::ThreadHits(ThreadHits {
                query: "flaky login".to_owned(),
                threads: vec![ThreadHit {
                    thread: thread(),
                    hits: vec![ItemHit {
                        item: ItemId("msg-4".to_owned()),
                        turn: TurnId(3),
                        said: ItemHit::AGENT.to_owned(),
                        text: "The flaky login test races".to_owned(),
                        spans: vec![Span { start: 4, end: 9 }, Span { start: 10, end: 15 }],
                        cut_before: true,
                        cut_after: false,
                        at_ms: ms(1_850_000),
                    }],
                    more: 2,
                }],
                more: 1,
            }),
        );
        snap(
            "link_worker_authors",
            &WorkerMsg::Authors(Authors {
                thread: None,
                path: "/Users/mira/atlas/src/auth.rs".to_owned(),
                modified_ms: Some(ms(1_900_000)),
                blob: Some("3b18e512dba79e4c8300dd08aeb37f8e728b8dad".to_owned()),
                runs: vec![
                    AuthorRun {
                        start: 4,
                        lines: 3,
                        thread: thread(),
                        turn: Some(TurnId(2)),
                        commit: None,
                        at_ms: ms(1_880_000),
                    },
                    AuthorRun {
                        start: 12,
                        lines: 1,
                        thread: thread(),
                        turn: None,
                        commit: Some("9d1e7aa".to_owned()),
                        at_ms: ms(1_700_000),
                    },
                ],
                absent: None,
            }),
        );
        snap("link_uni_thread", &UniHead::Thread { thread: thread() });
    }

    #[test]
    fn table() {
        let row = slopty_proto::thread::wire::ThreadRow {
            repo: Some("/w/slopty".to_owned()),
            repo_id: Some(slopty_proto::terminal::RepoId {
                origin: Some("github.com/aislopware/slopty".to_owned()),
                root: Some("c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e".to_owned()),
                url: None,
            }),
            ..state().row(ms(2_500))
        };
        snap(
            "table_snapshot",
            &TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 3 }, rows: vec![row.clone()] },
        );
        snap(
            "table_delta",
            &TableFrame::Delta {
                cursor: Cursor { epoch: 1, seq: 4 },
                rows: vec![row],
                removed: vec![ThreadId::from_uuid(Uuid::from_u128(9))],
            },
        );
    }

    /// The attention ladder and where the person is, on the server's link.
    #[test]
    fn attention() {
        use slopty_core::WorkerId;
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::project::{ProjectId, TaskId};
        use slopty_proto::server::{FromServer, ToServer};
        use slopty_proto::thread::attention::{
            Counts, Ladder, NodeAt, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat,
            Standing, Subject, ThreadAt, Via,
        };
        let worker = WorkerId::from_uuid(Uuid::from_u128(0x3011));
        let session = SessionId::from_uuid(Uuid::from_u128(0x5e55));
        let tile = TermRef { worker, session };
        let at = ThreadAt { worker, thread: thread() };
        let ranked =
            Ranked { at, rung: Rung::NeedsYou, since_ms: ms(1_500), terminal: Some(session) };
        let standing = Standing {
            rung: Rung::NeedsYou,
            counts: Counts {
                needs_you: 1,
                failed: 2,
                to_review: 3,
                working: 4,
                waiting: 5,
                idle: 6,
            },
            top: Some(at),
            since_ms: ms(1_500),
        };
        let project = ProjectId::new("slopty").expect("a project id");
        let ladder = Ladder {
            threads: vec![ranked],
            tiles: vec![(tile, standing)],
            workers: vec![(worker, standing)],
            nodes: vec![
                (NodeAt { project: project.clone(), task: None }, Standing::default()),
                (NodeAt { project: project.clone(), task: Some(TaskId(2)) }, standing),
            ],
            projects: vec![(project, standing)],
            fleet: standing,
        };
        snap("attention_ladder", &FromServer::Ladder(Box::new(ladder)));
        let presence = Presence {
            seat: Seat::Desk,
            active: true,
            workspace: Some("slopty".to_owned()),
            showing: vec![tile],
            focus: Some(tile),
            listening: true,
        };
        snap("attention_presence", &ToServer::Presence(presence.clone()));
        snap("attention_welcome", &FromServer::Welcome { name: "studio".to_owned(), link: 7 });
        let handheld =
            Presence { seat: Seat::Handheld, active: false, listening: false, ..presence.clone() };
        snap(
            "attention_present",
            &FromServer::Present(vec![
                Present { link: 1, name: "mac-studio".to_owned(), presence },
                Present { link: 2, name: "iPhone".to_owned(), presence: handheld },
            ]),
        );
        for (name, kind, worked_ms) in [
            ("attention_notice_needs_you", NoticeKind::NeedsYou, None),
            ("attention_notice_failed", NoticeKind::Failed, None),
            ("attention_notice_finished", NoticeKind::Finished, Some(93_000)),
        ] {
            let notice = Notice {
                kind,
                about: Subject::Thread(at),
                tile: Some(tile),
                title: "Fix the ladder".to_owned(),
                text: "Wants to run cargo test".to_owned(),
                worked_ms,
                via: (kind == NoticeKind::NeedsYou).then(|| Via {
                    thread: ThreadId::from_uuid(Uuid::from_u128(0x5ab)),
                    title: "Explore the hub".to_owned(),
                }),
            };
            snap(name, &FromServer::Notice(Box::new(notice)));
        }
        let held = Notice {
            kind: NoticeKind::Project,
            about: Subject::Project {
                project: ProjectId::new("slopty").expect("an id"),
                entry: 41,
            },
            tile: Some(tile),
            title: "slopty".to_owned(),
            text: "#2 Fix the ladder: its pull request's checks fail (test)".to_owned(),
            worked_ms: None,
            via: None,
        };
        snap("attention_notice_project", &FromServer::Notice(Box::new(held)));
        let rows = vec![state().row(ms(2_500))];
        snap(
            "attention_threads",
            &ToServer::Threads(TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 3 }, rows }),
        );
    }

    /// A phone the server may push to, and what is sealed to it.
    #[test]
    fn push() {
        use slopty_core::{ClientId, WorkerId};
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::push::{PushBody, PushDevice};
        use slopty_proto::server::ToServer;
        use slopty_proto::thread::AskId;
        use slopty_proto::thread::attention::{Notice, NoticeKind, Subject, ThreadAt};
        let client = ClientId::from_uuid(Uuid::from_u128(0xc11e));
        let device = PushDevice {
            token: "0f".repeat(32),
            key: [7; 32],
            sandbox: true,
            topic: "dev.aislopware.slopty".to_owned(),
            quiet_ms: 30_000,
        };
        snap("push_device", &ToServer::PushDevice { client, device: Some(device) });
        snap("push_device_gone", &ToServer::PushDevice { client, device: None });
        let worker = WorkerId::from_uuid(Uuid::from_u128(0x3011));
        let session = SessionId::from_uuid(Uuid::from_u128(0x5e55));
        let notice = Notice {
            kind: NoticeKind::NeedsYou,
            about: Subject::Thread(ThreadAt { worker, thread: thread() }),
            tile: Some(TermRef { worker, session }),
            title: "Fix the ladder".to_owned(),
            text: "Run cargo test?".to_owned(),
            worked_ms: None,
            via: None,
        };
        snap("push_body", &PushBody { notice, ask: Some(AskId("toolu_01".to_owned())) });
        let program = Notice {
            kind: NoticeKind::NeedsYou,
            about: Subject::Terminal(TermRef { worker, session }),
            tile: Some(TermRef { worker, session }),
            title: "deploy".to_owned(),
            text: "Approve the production rollout?".to_owned(),
            worked_ms: None,
            via: None,
        };
        snap("push_body_program", &PushBody { notice: program, ask: None });
        snap("push_answerable", &slopty_proto::server::FromServer::Pushes(true));
    }
}
