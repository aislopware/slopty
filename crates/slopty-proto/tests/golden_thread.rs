//! Golden byte snapshots of the thread model (`slopty_proto::thread`): what a client asks of a
//! worker's threads, the frames a followed thread streams, every action, and the thread table.
//! A changed snapshot is a wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_thread {
    use std::collections::BTreeMap;

    use slopty_core::{DisplayId, SessionId, WallMs, WindowId};
    use slopty_proto::codec;
    use slopty_proto::screen::CaptureTarget;
    use slopty_proto::search::Span;
    use slopty_proto::thread::detail::{
        AgentDetail, Answer, EditDetail, ExecDetail, ExecStatus, FetchDetail, Hunk, McpDetail,
        Offered, Question, QuestionDetail, ReadDetail, SearchDetail, WebLink, WebSearchDetail,
        WriteDetail,
    };
    use slopty_proto::thread::wire::{
        Against, Expanded, FileDiff, FileKind, Intent, IntentDone, Modes, NewWorktree, Outcome,
        Page, PastSession, PastSessions, PromptHit, Review, ReviewScope, Setup, Start, TableFrame,
        ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        Action, AgentId, AgentScreen, BackgroundTask, CallNote, Changed, Clipped, Compaction,
        ContentRef, Cursor, Draft, DraftImage, Drive, Image, IntentId, Item, ItemBody, ItemId,
        Link, Liveness, Notice, PartKey, Patch, Phase, Plan, Retry, Status, Step, TaskNote,
        TaskUsage, ThreadId, ThreadMeta, ThreadState, ToolCall, ToolDetail, ToolState, TreeRef,
        Turn, TurnId, TurnState, Usage, UserMessage, Wait, kind,
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
            parent: Some(Link {
                thread: ThreadId::from_uuid(Uuid::from_u128(0x9a)),
                item: ItemId("toolu_1".to_owned()),
            }),
            origin: ThreadMeta::SUBAGENT.to_owned(),
            drive: Drive::named(Drive::DRIVEN),
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
            mode: Some("acceptEdits".to_owned()),
            context: Some(48_210),
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
                }),
            )),
            Action::ItemStarted(item("a1", ItemBody::Text(clip("On it.")))),
            Action::ItemStarted(item("toolu_2", ItemBody::Tool(Box::new(call(kind::EXEC, None))))),
            Action::QueuedSet(vec!["and the docs".to_owned()]),
            Action::ScreensSet(vec![screen()]),
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
        snap("intent_seen", &send(Intent::Seen { turn: TurnId(3) }));
        snap("intent_name", &send(Intent::Name { title: "Release 0.9".to_owned() }));
    }

    #[test]
    fn outcomes() {
        let done = |outcome| IntentDone { id: intent(), outcome };
        snap("outcome_done", &done(Outcome::Done));
        snap("outcome_started", &done(Outcome::Started { thread: thread() }));
        snap(
            "outcome_refused",
            &done(Outcome::Refused {
                reason: "Your draft in the terminal is in the way".to_owned(),
            }),
        );
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
                calls: BTreeMap::from([(
                    ItemId("toolu_1".to_owned()),
                    CallNote { elapsed_ms: Some(12_000), at_ms: ms(9), ..CallNote::default() },
                )]),
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
            Action::QueuedSet(vec!["and the docs".to_owned(), "then the README".to_owned()]),
            Action::QueuedSet(Vec::new()),
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
                item: Some(ItemId("toolu_3".to_owned())),
                output: Some(clip("watching")),
                ..BackgroundTask::new(
                    "b1",
                    BackgroundTask::SHELL,
                    "cargo watch",
                    WallMs::from_millis(1_727_000_000_000),
                )
            }]),
            Action::Truncated { after: Some(TurnId(1)) },
            Action::Truncated { after: None },
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
            Action::Seen(TurnId(3)),
            Action::DraftSet(Some(Draft {
                text: "and the README [Image #2]".to_owned(),
                images: vec![DraftImage { n: 2, image: image() }],
                at_ms: ms(1_727_000_000_900),
            })),
            Action::DraftSet(None),
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
                edits: None,
                interpretation: None,
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
                tool_uses: Some(3),
                duration_ms: Some(46),
                model: Some("claude-haiku-5-5".to_owned()),
                tally: BTreeMap::from([(kind::READ.to_owned(), 2), (kind::SEARCH.to_owned(), 1)]),
                changed: Changed { added: 4, removed: 2 },
            }),
            ToolDetail::Question(QuestionDetail {
                questions: vec![Question {
                    text: "Which?".to_owned(),
                    header: Some("Pick".to_owned()),
                    options: vec![Offered {
                        label: "A".to_owned(),
                        description: None,
                        preview: None,
                    }],
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
                structured: None,
                links: Vec::new(),
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
        snap(
            "frame_tool_details",
            &ThreadFrame::Page(Page { turns: vec![], items, older: false, calls: BTreeMap::new() }),
        );
    }

    /// The work an agent does beside its turn: task rows, call timers, the turn's live line and
    /// context categories.
    #[test]
    fn work() {
        let actions = vec![
            Action::ItemCompleted(item(
                "task:b1:1",
                ItemBody::Task(Box::new(TaskNote {
                    task: "b1".to_owned(),
                    kind: BackgroundTask::AGENT.to_owned(),
                    status: TaskNote::COMPLETED.to_owned(),
                    title: "Explore the codec".to_owned(),
                    summary: "Agent \"Explore the codec\" completed".to_owned(),
                    body: Some(clip("Found it.")),
                    call: Some(ItemId("toolu_3".to_owned())),
                    usage: Some(TaskUsage { tokens: 12_400, tool_uses: 7, duration_ms: 64_000 }),
                    exit_code: None,
                    links: Vec::new(),
                })),
            )),
            Action::CallNoted {
                item: ItemId("toolu_3".to_owned()),
                note: CallNote {
                    elapsed_ms: Some(30_000),
                    ran_ms: Some(29_400),
                    at_ms: ms(30),
                    task: Some("b1".to_owned()),
                    retry: Some(Retry { attempt: 1, max: None, in_ms: Some(500) }),
                    said: Some("Reading files".to_owned()),
                    title: Some("Read report.pdf · pages 1-3".to_owned()),
                },
            },
        ];
        snap("frame_work", &ThreadFrame::Actions { epoch: 1, first: 40, next: 60, actions });
    }

    /// A review asked for, and sent.
    #[test]
    fn review() {
        snap(
            "review_request",
            &ThreadRequest::Review {
                thread: thread(),
                scope: ReviewScope::WorkingTree(Against::Base),
            },
        );
        snap(
            "review_range",
            &ReviewScope::Range { base: "main".to_owned(), head: "fix-login".to_owned() },
        );
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
            scope: ReviewScope::WorkingTree(Against::Head),
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
            &WorkerMsg::IntentDone(IntentDone { id: intent(), outcome: Outcome::Done }),
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
            ended: Some(slopty_proto::thread::wire::TurnEnded {
                turn: TurnId(1),
                at_ms: ms(2_300),
                ran_ms: 93_000,
                answered: true,
            }),
            seen: TurnId(1),
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
        use slopty_proto::server::{FromServer, ToServer};
        use slopty_proto::thread::attention::{
            Counts, Ladder, Notice, NoticeKind, Presence, Present, Ranked, Rung, Seat, SpaceAt,
            Standing, Subject, ThreadAt, Via,
        };
        let worker = WorkerId::from_uuid(Uuid::from_u128(0x3011));
        let session = SessionId::from_uuid(Uuid::from_u128(0x5e55));
        let tile = TermRef { worker, session };
        let at = ThreadAt { worker, thread: thread() };
        let ranked = Ranked { at, rung: Rung::NeedsYou, since_ms: ms(1_500) };
        let standing = Standing {
            rung: Rung::NeedsYou,
            counts: Counts { needs_you: 1, failed: 2, working: 4, waiting: 5, idle: 6 },
            top: Some(at),
            since_ms: ms(1_500),
        };
        let space = SpaceAt { worker, folder: "/Users/c/src/slopty".to_owned() };
        let ladder = Ladder {
            threads: vec![ranked],
            tiles: vec![(tile, standing)],
            workers: vec![(worker, standing)],
            trees: vec![(at, standing)],
            spaces: vec![(space, standing)],
            fleet: standing,
        };
        snap("attention_ladder", &FromServer::Ladder(Box::new(ladder)));
        let presence = Presence {
            seat: Seat::Desk,
            active: true,
            showing: vec![tile],
            focus: Some(tile),
            listening: true,
        };
        snap("attention_presence", &ToServer::Presence(presence.clone()));
        snap(
            "attention_welcome",
            &FromServer::Welcome {
                name: "studio".to_owned(),
                link: 7,
                build: "0.4.0+wire.0badf00d.20261009T2307Z.commit.1a2b3c4d5e6f.20261010T0930Z"
                    .to_owned(),
            },
        );
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

    /// A phone the server may push to, and what is sealed to it.
    #[test]
    fn push() {
        use slopty_core::{ClientId, WorkerId};
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::push::{PushBody, PushDevice};
        use slopty_proto::server::ToServer;
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
            title: "Fix the ladder".to_owned(),
            text: "Run cargo test?".to_owned(),
            worked_ms: None,
            via: None,
        };
        snap("push_body", &PushBody { notice });
        let program = Notice {
            kind: NoticeKind::NeedsYou,
            about: Subject::Terminal(TermRef { worker, session }),
            title: "deploy".to_owned(),
            text: "Approve the production rollout?".to_owned(),
            worked_ms: None,
            via: None,
        };
        snap("push_body_program", &PushBody { notice: program });
        snap("push_pushable", &slopty_proto::server::FromServer::Pushable(false));
    }
}
