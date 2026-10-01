//! Golden byte snapshots of the thread model (`slopty_proto::thread`): what a client asks of a
//! worker's threads, the frames a followed thread streams, every action, and the thread table.
//! A changed snapshot is a wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_thread {
    use std::collections::BTreeMap;

    use slopty_core::{ClientId, SessionId, WallMs};
    use slopty_proto::codec;
    use slopty_proto::thread::detail::{
        AgentDetail, Answer, EditDetail, ExecDetail, ExecStatus, FetchDetail, Hunk, McpDetail,
        Offered, Question, QuestionDetail, ReadDetail, SearchDetail, WebLink, WebSearchDetail,
        WriteDetail,
    };
    use slopty_proto::thread::wire::{
        Expanded, FileDiff, Intent, IntentDone, Outcome, Page, Pick, Review, ReviewScope, Start,
        TableFrame, ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        Action, AgentId, Answerer, AskId, BackgroundTask, Cap, Changed, Choice, Clipped, Command,
        Compaction, ContentRef, Cursor, Delivery, Drive, Edge, Effect, Fork, Image, IntentId, Item,
        ItemBody, ItemId, Limit, Link, Liveness, Meters, Model, Notice, PartKey, Patch, Pending,
        PendingState, Phase, Plan, Request, RequestState, Status, Step, ThreadId, ThreadMeta,
        ThreadState, ToolCall, ToolDetail, ToolState, TreeRef, Turn, TurnId, TurnState, Usage,
        UserMessage, Wait, kind,
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
                turn: TurnId(2),
            }),
            drive: Drive::named(Drive::OBSERVED),
            caps: vec![Cap::named(Cap::QUEUE), Cap::named(Cap::STEER)],
            models: vec![Model { id: "opus".to_owned(), label: "Opus".to_owned() }],
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
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        }
    }

    fn meters() -> Meters {
        Meters {
            model: Some("Opus 5.5".to_owned()),
            model_id: Some("claude-opus-5-5".to_owned()),
            mode: Some("default".to_owned()),
            context_tokens: Some(42_000),
            context_window: Some(200_000),
            cost_micro_usd: Some(250_000),
            limits: vec![Limit {
                name: "five-hour".to_owned(),
                used_bp: 1_250,
                resets_ms: Some(ms(99)),
            }],
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
        snap("client_approvals", &ThreadRequest::Approvals { on: true });
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
                    args: vec!["--yolo".to_owned()],
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
            }),
        );
        snap(
            "intent_send_queue",
            &send(Intent::Send { text: "then the docs".to_owned(), delivery: Delivery::Queue }),
        );
        snap("intent_withdraw", &send(Intent::Withdraw { pending: intent() }));
        snap(
            "intent_edit",
            &send(Intent::Edit { pending: intent(), text: "then the README".to_owned() }),
        );
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
        snap("intent_compact", &send(Intent::Compact));
        snap("intent_stop_task", &send(Intent::StopTask { task: "b1".to_owned() }));
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
            Action::Status(Status {
                phase: Phase::Waiting,
                wait: None,
                liveness: Liveness::Sleeping { until_ms: ms(9) },
                since_ms: ms(3),
            }),
            Action::Status(Status {
                phase: Phase::Working,
                wait: None,
                liveness: Liveness::Silent { since_ms: ms(4) },
                since_ms: ms(3),
            }),
            Action::TurnStarted(turn()),
            Action::TurnEnded {
                turn: TurnId(1),
                state: TurnState::Failed { error: "overloaded".to_owned() },
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
                ItemBody::Notice(Notice {
                    kind: Notice::INTERRUPTED.to_owned(),
                    text: clip("Stopped"),
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
                title: "cargo watch".to_owned(),
                state: "running".to_owned(),
                item: Some(ItemId("toolu_3".to_owned())),
                output: Some(clip("watching")),
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
        ];
        snap("frame_actions", &ThreadFrame::Actions { epoch: 1, first: 8, next: 40, actions });
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
                    binary: false,
                    patch,
                },
                FileDiff {
                    path: "logo.png".to_owned(),
                    from: None,
                    to: Some("cc33".to_owned()),
                    binary: true,
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
        };
        snap("review_keep", &Intent::Keep(pick.clone()));
        snap("review_revert", &Intent::Revert(Pick { hunks: vec![], ..pick }));
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
        snap("link_uni_thread", &UniHead::Thread { thread: thread() });
    }

    #[test]
    fn table() {
        let row = state().row(ms(2_500));
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
            Standing, ThreadAt, Via,
        };
        let worker = WorkerId::from_uuid(Uuid::from_u128(0x3011));
        let session = SessionId::from_uuid(Uuid::from_u128(0x5e55));
        let tile = TermRef { worker, session };
        let at = ThreadAt { worker, thread: thread() };
        let ranked = Ranked { at, rung: Rung::NeedsYou, since_ms: ms(1_500) };
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
        };
        snap("attention_presence", &ToServer::Presence(presence.clone()));
        snap("attention_welcome", &FromServer::Welcome { name: "studio".to_owned(), link: 7 });
        let handheld = Presence { seat: Seat::Handheld, active: false, ..presence.clone() };
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
                thread: at,
                tile: Some(session),
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
        let rows = vec![state().row(ms(2_500))];
        snap(
            "attention_threads",
            &ToServer::Threads(TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq: 3 }, rows }),
        );
    }
}
