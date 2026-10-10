//! Golden byte snapshots of the project messages (`slopty_proto::project`): the verbs and
//! answers, the change pushed to every client, the snapshot a client gets on connecting, and
//! what a worker reports of its agents. A changed snapshot is a wire change: accept it
//! deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_project {
    use std::collections::BTreeMap;

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::agent::{AgentBranch, Worktree};
    use slopty_proto::codec;
    use slopty_proto::folder::FsOp;
    use slopty_proto::git::Forge;
    use slopty_proto::orchestration::{
        BranchBundle, ErrorCode, Happening, HubEvent, Outcome, TermRef, Verb,
    };
    use slopty_proto::project::{
        AgentReport, Assignment, Bounds, Commits, Fact, Facts, GiveBacks, Limits, LimitsChange,
        Live, Merge, Moment, Native, NativeAgent, NativeChange, NativeTask, Natives, NodeDetail,
        Project, ProjectId, ProjectStatus, ProjectUpdate, ProjectsPart, Report, RunOn, Spent,
        StepKind, StepState, Task, TaskChange, TaskId, TaskLaunch, TaskSpec, TaskState, TaskStep,
        TestDiff, TimelineEntry, VerifierRun, WorkerFacts,
    };
    use slopty_proto::server::{FromServer, ToServer};
    use slopty_proto::settings::{DaemonSettings, SettingEdit};
    use slopty_proto::terminal::RepoId;
    use slopty_proto::thread::wire::{NewWorktree, PullSeen, PullStands, Start};
    use slopty_proto::thread::{AgentId, ThreadId};
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

    fn at() -> WallMs {
        WallMs::from_millis(1_790_000_000_000)
    }

    fn term() -> TermRef {
        TermRef {
            worker: WorkerId::from_uuid(Uuid::from_u128(0x0199_a000_0000_7000_8000_0000_0000_0001)),
            session: SessionId::from_uuid(Uuid::from_u128(
                0x0199_a1b1_c3d4_7000_8000_0000_0000_abcd,
            )),
        }
    }

    fn project_id() -> ProjectId {
        ProjectId::new("slopty").expect("a name")
    }

    fn project() -> Project {
        Project {
            orchestrator_spent: Spent { active_ms: 480_000, since_ms: None },
            id: project_id(),
            title: "Projects mode".to_owned(),
            repo: "~/src/slopty".to_owned(),
            repo_id: Some(RepoId {
                origin: Some("github.com/aislopware/slopty".to_owned()),
                root: Some("c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e".to_owned()),
                url: Some("https://github.com/aislopware/slopty.git".to_owned()),
            }),
            target: "main".to_owned(),
            verifier: Some("cargo gate".to_owned()),
            push: false,
            orchestrator: Some(term()),
            limits: Limits::default(),
            metadata: Some(r#"{"goal":"open"}"#.to_owned()),
            created_ms: at(),
            goal: Some("Projects mode, end to end".to_owned()),
            autonomy: slopty_proto::project::Autonomy::Edits,
            progress: Some(slopty_proto::project::Progress {
                summary: "Store and verbs merged; the board runs".to_owned(),
                next: Some("The phone's inbox".to_owned()),
                done: false,
                at_ms: at(),
            }),
        }
    }

    fn spec() -> TaskSpec {
        TaskSpec {
            depends_on: vec![TaskId(2)],
            start_from: Some(TaskId(2)),
            kind: "build".to_owned(),
            title: "Server store".to_owned(),
            brief: "Keep projects beside workers.json.".to_owned(),
            read_only: false,
            pin: Some(term().worker),
            verifier: Some("cargo nextest run -p slopty-server".to_owned()),
            metadata: Some(r#"{"lane":"server"}"#.to_owned()),
        }
    }

    fn task() -> Task {
        let s = spec();
        Task {
            spent: Spent {
                active_ms: 754_000,
                since_ms: Some(WallMs::from_millis(1_790_000_004_500)),
            },
            id: TaskId(3),
            depends_on: s.depends_on,
            start_from: s.start_from,
            started_on: Some(commit('d')),
            kind: s.kind,
            title: s.title,
            brief: s.brief,
            read_only: s.read_only,
            pin: s.pin,
            verifier: s.verifier,
            metadata: s.metadata,
            state: TaskState::Blocked,
            status: Some("waiting on a permission".to_owned()),
            assignment: Some(Assignment {
                term: term(),
                thread: None,
                since_ms: at(),
                ended_ms: None,
                conversation: Some("0199a1b1-c3d4-7000-8000-00000000c0de".to_owned()),
                spawned: true,
            }),
            branch: Some("slopty/slopty/3".to_owned()),
            worktree: Some("/w/slopty-3".to_owned()),
            base: Some(commit('b')),
            pull: Some(pull()),
            verified: Some(run(false, "clippy: 2 errors")),
            merge: Some(Merge::Queued { since_ms: at() }),
            created_ms: at(),
            updated_ms: WallMs::from_millis(1_790_000_005_000),
            step: Some(TaskStep {
                kind: StepKind::Verify,
                worker: term().worker,
                state: StepState::Running {
                    phase: "Compiling slopty-server".to_owned(),
                    percent: None,
                },
                since_ms: at(),
                term: Some(term()),
                commits: None,
            }),
            give_backs: GiveBacks { count: 2, held: false },
            tests: Some(TestDiff {
                head: commit('a'),
                deleted: vec!["crates/slopty-server/tests/store.rs".to_owned()],
                changed: vec!["crates/slopty-server/src/hub/queue/tests.rs".to_owned()],
                deleted_count: 1,
                changed_count: 1,
                added_count: 2,
            }),
        }
    }

    fn commit(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn run(passed: bool, summary: &str) -> VerifierRun {
        VerifierRun {
            passed,
            summary: summary.to_owned(),
            head: commit('a'),
            base: commit('b'),
            exit: Some(if passed { 0 } else { 101 }),
            took_ms: 133_000,
        }
    }

    fn native_agent() -> NativeAgent {
        NativeAgent {
            id: "ag1".to_owned(),
            kind: "Explore".to_owned(),
            started_ms: at(),
            stopped_ms: Some(WallMs::from_millis(1_790_000_004_000)),
            transcript: Some("/t/ag1.jsonl".to_owned()),
            last: Some("Found it.".to_owned()),
        }
    }

    fn natives() -> Natives {
        Natives {
            agents: vec![native_agent()],
            tasks: vec![NativeTask {
                id: "1".to_owned(),
                subject: "Read the hub".to_owned(),
                done: true,
            }],
        }
    }

    fn status(timeline: Vec<TimelineEntry>, next: u64) -> ProjectStatus {
        ProjectStatus {
            project: project(),
            tasks: vec![task().card(&natives())],
            orchestrator_natives: Natives::default().counts(),
            timeline,
            next,
            bounds: Bounds::default(),
            live: Live { fleet: 7, project: 3 },
        }
    }

    fn entry(seq: u64, what: Moment) -> TimelineEntry {
        TimelineEntry { seq, at_ms: at(), task: Some(TaskId(3)), what }
    }

    fn request(verb: Verb) -> ToServer {
        ToServer::Request { id: 21, key: None, verb }
    }

    fn reply(outcome: Outcome) -> FromServer {
        FromServer::Reply { id: 21, outcome }
    }

    /// An agent's change to a worker's files, and where it left the entry. These ride the
    /// orchestration verbs beside the project ones, so their goldens sit here.
    #[test]
    fn folder_changes() {
        let worker = term().worker;
        let op = FsOp::Move { from: "~/drafts".to_owned(), to: "~/notes".to_owned() };
        snap("fs_change_move", &request(Verb::FsChange { worker, op }));
        let op = FsOp::MakeDir { parent: "~/src".to_owned(), name: "new".to_owned() };
        snap("fs_change_make_dir", &request(Verb::FsChange { worker, op }));
        let op = FsOp::Trash { path: "~/old".to_owned() };
        snap("fs_change_trash", &request(Verb::FsChange { worker, op }));
        snap("fs_done", &reply(Outcome::FsDone { path: "/Users/c/notes".to_owned() }));
    }

    /// A thread read and answered through the thread model, whatever its agent: asked by task
    /// and sent to its worker once found, the whole turns it gives with an open request, and
    /// the person's answer to that request.
    #[test]
    fn thread_reads() {
        use slopty_proto::orchestration::{
            ReadEntry, RequestRead, ThreadOf, ThreadRead, ThreadView, TurnRead,
        };
        use slopty_proto::thread::{AskId, Choice, Effect, Phase, ToolState, TurnId, TurnState};

        let thread =
            ThreadId::from_uuid(Uuid::from_u128(0x0199_a1b1_c3d4_7000_8000_0000_0000_7e7e));
        let child = ThreadId::from_uuid(Uuid::from_u128(0x0199_a1b1_c3d4_7000_8000_0000_0000_c41d));
        let by_task = Verb::ReadThread {
            of: ThreadOf::Task { project: project_id(), task: TaskId(3) },
            view: ThreadView::Messages,
            after: None,
            hold: false,
        };
        snap("read_thread_task", &request(by_task));
        let found = Verb::ReadThread {
            of: ThreadOf::On { worker: term().worker, thread },
            view: ThreadView::Activity,
            after: Some(TurnId(4)),
            hold: true,
        };
        snap("read_thread_on", &request(found));
        let read = ThreadRead {
            worker: term().worker,
            thread,
            agent: AgentId::named(AgentId::PI),
            title: "Server store".to_owned(),
            parent: None,
            phase: Phase::NeedsYou,
            wait: Some("Wants to run cargo test".to_owned()),
            turns: vec![TurnRead {
                id: TurnId(5),
                state: TurnState::Active,
                started_ms: at(),
                ended_ms: None,
                entries: vec![
                    ReadEntry::User("Build the store.".to_owned()),
                    ReadEntry::Tool {
                        kind: "subagent".to_owned(),
                        title: "Explore the hub".to_owned(),
                        state: ToolState::Completed,
                        output: Some("Found it.".to_owned()),
                        child: Some(child),
                    },
                    ReadEntry::Text(format!("Half done{}", ThreadRead::CUT)),
                    ReadEntry::Notice {
                        kind: "api-error".to_owned(),
                        text: "overloaded".to_owned(),
                    },
                ],
            }],
            requests: vec![RequestRead {
                ask: AskId("7".to_owned()),
                kind: "approval".to_owned(),
                title: "Run cargo test?".to_owned(),
                choices: vec![Choice {
                    id: "allow".to_owned(),
                    label: "Allow".to_owned(),
                    effect: Effect::Allow,
                    scope: Some("this session".to_owned()),
                    stops: false,
                }],
                questions: Vec::new(),
                picks: Vec::new(),
            }],
            next: TurnId(4),
            truncated: true,
            skipped: false,
        };
        snap("thread_read", &reply(Outcome::Thread(Box::new(read))));
        let answer = Verb::AnswerRequest {
            of: ThreadOf::Thread(thread),
            ask: AskId("7".to_owned()),
            choice: "deny".to_owned(),
            message: Some("Not on main.".to_owned()),
        };
        snap("answer_request", &request(answer));
        let reply = Verb::SendMessage {
            of: ThreadOf::Thread(thread),
            text: "Use the staging database.".to_owned(),
        };
        snap("send_message", &request(reply));
    }

    #[test]
    fn project_verbs() {
        let limits = LimitsChange { review: Some(4) };
        snap(
            "project_create",
            &request(Verb::ProjectCreate {
                project: project_id(),
                title: "Projects mode".to_owned(),
                repo: "~/src/slopty".to_owned(),
                target: "main".to_owned(),
                verifier: Some("cargo gate".to_owned()),
                push: false,
                orchestrator: Some(term()),
                limits,
                metadata: Some(r#"{"goal":"open"}"#.to_owned()),
                goal: Some("Projects mode, end to end".to_owned()),
                autonomy: slopty_proto::project::Autonomy::Own,
            }),
        );
        snap(
            "project_set",
            &request(Verb::ProjectSet {
                project: project_id(),
                orchestrator: None,
                verifier: None,
                push: Some(true),
                limits: LimitsChange { review: Some(2) },
                metadata: None,
                autonomy: Some(slopty_proto::project::Autonomy::Ask),
            }),
        );
        snap("project_list", &request(Verb::ProjectList));
        snap(
            "project_status",
            &request(Verb::ProjectStatus { project: project_id(), since: Some(7), timeout_ms: 0 }),
        );
        snap(
            "task_create",
            &request(Verb::TaskCreate { project: project_id(), spec: Box::new(spec()) }),
        );
        snap(
            "task_create_read_only",
            &request(Verb::TaskCreate {
                project: project_id(),
                spec: Box::new(TaskSpec {
                    title: "Review".to_owned(),
                    read_only: true,
                    ..TaskSpec::default()
                }),
            }),
        );
        let change = TaskChange {
            state: Some(TaskState::Done),
            status: Some("gate passed; ready".to_owned()),
            branch: Some("slopty/slopty/3".to_owned()),
            verified: Some(run(true, "gate passed")),
            base: Some(commit('b')),
            note: Some("ready".to_owned()),
            depends_on: Some(vec![TaskId(1), TaskId(2)]),
            run_on: Some(RunOn::Worker(term().worker)),
            verifier: Some(String::new()),
            metadata: Some("{}".to_owned()),
        };
        snap(
            "task_update",
            &request(Verb::TaskUpdate {
                project: project_id(),
                task: TaskId(3),
                change: Box::new(change),
            }),
        );
        snap(
            "task_spawn",
            &request(Verb::TaskSpawn {
                project: project_id(),
                task: TaskId(3),
                launch: TaskLaunch {
                    pin: Some(term().worker),
                    agent: AgentId::named(AgentId::CLAUDE_CODE),
                },
            }),
        );
        snap(
            "task_spawn_agent",
            &request(Verb::TaskSpawn {
                project: project_id(),
                task: TaskId(6),
                launch: TaskLaunch { pin: None, agent: AgentId::named(AgentId::PI) },
            }),
        );
        snap(
            "project_progress",
            &request(Verb::ProjectProgress {
                project: project_id(),
                summary: "Store and verbs merged; the board runs".to_owned(),
                next: Some("The phone's inbox".to_owned()),
                done: true,
            }),
        );
        let restart = |agent: Option<AgentId>| Verb::TaskRestart {
            project: project_id(),
            task: TaskId(6),
            agent,
        };
        snap("task_restart", &request(restart(None)));
        snap("task_restart_codex", &request(restart(Some(AgentId::named(AgentId::CODEX)))));
        let start = Start {
            agent: AgentId::acp("gemini"),
            cwd: "/w/slopty".to_owned(),
            drive: None,
            prompt: Some("Read your brief.".to_owned()),
            model: None,
            mode: None,
            effort: None,
            attachments: Vec::new(),
            args: Vec::new(),
            worktree: Some(NewWorktree {
                name: "slopty-slopty-6".to_owned(),
                base: Some("slopty/slopty/2".to_owned()),
                merge_base: Some("main".to_owned()),
                pull: None,
                setup: true,
            }),
        };
        snap(
            "start_thread",
            &request(Verb::StartThread {
                worker: term().worker,
                start: Box::new(start),
                seat: term().session,
                env: vec![("SLOPTY_TASK".to_owned(), "6".to_owned())],
                role: Some("You are the agent of task 6.".to_owned()),
            }),
        );
        let made = Worktree {
            name: "slopty-slopty-6".to_owned(),
            path: "/w/slopty/.claude/worktrees/slopty-slopty-6".to_owned(),
            branch: Some("worktree-slopty-slopty-6".to_owned()),
            original_cwd: "/w/slopty".to_owned(),
            original_branch: Some("main".to_owned()),
        };
        let thread =
            ThreadId::from_uuid(Uuid::from_u128(0x0199_a000_0000_7000_8000_0000_0000_0006));
        snap(
            "opened_in",
            &reply(Outcome::OpenedIn { term: term(), worktree: Box::new(made.clone()) }),
        );
        snap(
            "thread_started",
            &reply(Outcome::ThreadStarted { thread, worktree: Some(Box::new(made)) }),
        );
        let seated = Assignment {
            term: term(),
            thread: Some(thread),
            since_ms: at(),
            ended_ms: None,
            conversation: None,
            spawned: true,
        };
        snap("assignment_thread", &seated);
        snap("worker_facts", &request(Verb::WorkerFacts { worker: Some(term().worker) }));
        snap("task_get", &request(Verb::TaskGet { project: project_id(), task: Some(TaskId(3)) }));
        snap("working_on", &request(Verb::WorkingOn { session: term().session }));
        snap(
            "task_report",
            &request(Verb::TaskReport { project: project_id(), task: TaskId(3), report: report() }),
        );
        snap("task_merge", &request(Verb::TaskMerge { project: project_id(), task: TaskId(3) }));
        snap("task_push", &request(Verb::TaskPush { project: project_id(), task: TaskId(3) }));
    }

    /// What the server asks of the orchestrator's worker to verify a task and merge it: a
    /// verifier run in the project's checkout, a rebase there, the target moved; and how it
    /// answers.
    #[test]
    fn verify_and_merge() {
        let worker = term().worker;
        let repo = "/w/slopty".to_owned();
        snap(
            "verify",
            &request(Verb::Verify {
                worker,
                repo: repo.clone(),
                worktree: "slopty".to_owned(),
                head: "slopty/slopty/3".to_owned(),
                target: "main".to_owned(),
                command: "cargo gate".to_owned(),
                session: term().session,
                title: "Verifier for slopty #3".to_owned(),
            }),
        );
        snap(
            "verifying",
            &reply(Outcome::Verifying { term: term(), head: commit('a'), base: commit('b') }),
        );
        snap(
            "rebase",
            &request(Verb::Rebase {
                worker,
                repo: repo.clone(),
                worktree: "slopty".to_owned(),
                head: commit('a'),
                onto: "main".to_owned(),
                after: Some("slopty/slopty/2".to_owned()),
                trailers: vec![
                    ("Slopty-Task".to_owned(), "slopty#3".to_owned()),
                    ("Slopty-Thread".to_owned(), "0199a000-0000-7000-8000-000000000006".to_owned()),
                ],
                verified: Some(commit('a')),
            }),
        );
        snap(
            "rebased",
            &reply(Outcome::Rebased {
                head: commit('d'),
                from: commit('a'),
                onto: commit('c'),
                verified: true,
            }),
        );
        snap(
            "test_diff",
            &request(Verb::TestDiff {
                worker,
                repo: repo.clone(),
                head: "slopty/slopty/3".to_owned(),
                target: "main".to_owned(),
                test_paths: vec!["crates/slopty-e2e/golden".to_owned()],
            }),
        );
        let tests = TestDiff {
            head: commit('a'),
            deleted: vec!["tests/store.rs".to_owned()],
            changed: Vec::new(),
            deleted_count: 1,
            changed_count: 0,
            added_count: 0,
        };
        snap("tested", &reply(Outcome::TestDiff(tests)));
        snap(
            "fast_forward",
            &request(Verb::FastForward {
                worker,
                repo,
                target: "main".to_owned(),
                from: commit('c'),
                to: commit('d'),
                push: true,
            }),
        );
        snap(
            "fast_forwarded",
            &reply(Outcome::FastForwarded {
                head: commit('d'),
                pushed: false,
                push_failed: Some("! [rejected] main -> main (fetch first)".to_owned()),
            }),
        );
        snap(
            "catch_up",
            &request(Verb::CatchUp {
                worker,
                repo: "/w/slopty".to_owned(),
                target: "main".to_owned(),
            }),
        );
        snap(
            "remove_worktree",
            &request(Verb::RemoveWorktree {
                worker,
                worktree: "/w/slopty/.claude/worktrees/slopty-slopty-3".to_owned(),
                landed: vec![commit('d'), "main".to_owned(), "origin/main".to_owned()],
            }),
        );
        snap(
            "worktree_removed",
            &reply(Outcome::WorktreeRemoved {
                branch: Some("worktree-slopty-slopty-3".to_owned()),
                branch_removed: true,
            }),
        );
        snap(
            "drop_branches",
            &request(Verb::DropBranches {
                worker,
                repo: "/w/slopty".to_owned(),
                branches: vec!["slopty/slopty/3".to_owned(), "slopty/slopty/target".to_owned()],
            }),
        );
        let edit = |key: &str, entry: Option<&str>, literal: Option<&str>| SettingEdit {
            table: "worker".to_owned(),
            key: key.to_owned(),
            entry: entry.map(str::to_owned),
            literal: literal.map(str::to_owned),
        };
        snap(
            "settings_edit",
            &request(Verb::Settings {
                of: Some(worker),
                edits: vec![
                    edit("acp", Some("gemini"), Some(r#"["gemini", "--experimental-acp"]"#)),
                    edit("allow", None, None),
                ],
            }),
        );
        snap("settings_read_server", &request(Verb::Settings { of: None, edits: vec![] }));
        snap(
            "settings",
            &reply(Outcome::Settings(Box::new(DaemonSettings {
                path: "/Users/me/Library/Application Support/Slopty/settings.toml".to_owned(),
                text: "[worker]\nacp = { gemini = [\"gemini\", \"--experimental-acp\"] }\n"
                    .to_owned(),
                tables: vec!["worker".to_owned()],
                problems: vec!["worker.display_linger: expected a number".to_owned()],
            }))),
        );
        let merged = Merge::Merged {
            target: "main".to_owned(),
            head: commit('d'),
            from: commit('a'),
            at_ms: at(),
            pushed: true,
            push_failed: None,
        };
        let card = Task { merge: Some(merged), state: TaskState::Merged, ..task() };
        snap("task_merged_card", &card.card(&Natives::default()));
        let unpushed = Merge::Merged {
            target: "main".to_owned(),
            head: commit('d'),
            from: commit('a'),
            at_ms: at(),
            pushed: false,
            push_failed: Some("! [rejected] main -> main (fetch first)".to_owned()),
        };
        let card = Task { merge: Some(unpushed), state: TaskState::Merged, ..task() };
        snap("task_merged_unpushed_card", &card.card(&Natives::default()));
        snap(
            "land_pull",
            &request(Verb::LandPull {
                worker,
                repo: "/w/slopty".to_owned(),
                head: commit('d'),
                branch: "worktree-slopty-slopty-3".to_owned(),
                target: "main".to_owned(),
                title: "Split the parser".to_owned(),
                body: "Task #3 of the Slopty project slopty.".to_owned(),
            }),
        );
        let url = "https://github.com/o/slopty/pull/12".to_owned();
        snap("pull_opened", &reply(Outcome::PullOpened { number: 12, url: url.clone() }));
        let waiting = Merge::Pull {
            target: "main".to_owned(),
            head: commit('d'),
            from: commit('a'),
            number: 12,
            url,
            since_ms: at(),
        };
        let card = Task { merge: Some(waiting), state: TaskState::Done, ..task() };
        snap("task_in_pull_card", &card.card(&Natives::default()));
    }

    /// The person letting a project go.
    #[test]
    fn project_delete() {
        snap("project_delete", &request(Verb::ProjectDelete { project: project_id() }));
    }

    /// The person's next step for a task's agent, as they said it, on the timeline; and a
    /// rebase that conflicts, the step that gives the work back to be resolved; and a
    /// verification that names the commits it reads.
    #[test]
    fn told_and_conflicted() {
        snap(
            "task_tell",
            &request(Verb::TaskTell {
                project: project_id(),
                task: Some(TaskId(3)),
                text: "Resolve the conflicts with main, then report done again.".to_owned(),
            }),
        );
        let told = TimelineEntry {
            seq: 12,
            at_ms: at(),
            task: Some(TaskId(3)),
            what: Moment::Told { text: "Fix CI: cargo gate failed at 9c1e2f3.".to_owned() },
        };
        snap("moment_told", &told);
        let step = TaskStep {
            kind: StepKind::Rebase,
            worker: term().worker,
            state: StepState::Failed { why: "CONFLICT (content): crates/a.rs".to_owned() },
            since_ms: at(),
            term: None,
            commits: None,
        };
        snap("step_rebase_failed", &step);
        let verifying = TaskStep {
            kind: StepKind::Verify,
            worker: term().worker,
            state: StepState::Running {
                phase: "Verifying aaaaaaa over bbbbbbb".to_owned(),
                percent: None,
            },
            since_ms: at(),
            term: Some(term()),
            commits: Some(Commits { head: commit('a'), base: commit('b') }),
        };
        snap("step_verify_commits", &verifying);
    }

    /// What the server asks of workers for a task around its agent: a clone, a branch
    /// bundled, a bundle fetched; and how they answer.
    #[test]
    fn task_steps() {
        let worker = term().worker;
        let url = "https://github.com/aislopware/slopty.git".to_owned();
        snap("clone_repo", &request(Verb::CloneRepo { worker, url, clone: 7 }));
        let progress = "Receiving objects".to_owned();
        snap("worker_cloning", &ToServer::Cloning { clone: 7, phase: progress, percent: Some(45) });
        let repo = RepoId {
            origin: Some("github.com/aislopware/slopty".to_owned()),
            root: Some(commit('c')),
            url: Some("https://github.com/aislopware/slopty.git".to_owned()),
        };
        let path = "/home/c/slopty/clones/github.com/aislopware/slopty".to_owned();
        snap("cloned", &reply(Outcome::Cloned { path: path.clone(), repo }));
        let branch = "worktree-slopty-slopty-3".to_owned();
        snap(
            "bundle_branch",
            &request(Verb::BundleBranch {
                worker,
                repo: path,
                branch: branch.clone(),
                target: Some("main".to_owned()),
            }),
        );
        let name = format!("{branch}-4a7aa6d00000.bundle");
        snap(
            "bundle",
            &reply(Outcome::Bundle(Box::new(BranchBundle {
                path: format!("/home/c/.cache/slopty/bundles/{name}"),
                name: name.clone(),
                size: 81_920,
                digest: [7; 32],
                head: commit('a'),
                base: Some(commit('b')),
            }))),
        );
        snap(
            "fetch_bundle",
            &request(Verb::FetchBundle {
                worker,
                repo: "/w/slopty".to_owned(),
                bundle: name,
                branch,
                into: "slopty/slopty/3".to_owned(),
                head: commit('a'),
            }),
        );
        let into = "slopty/slopty/3".to_owned();
        snap("fetched", &reply(Outcome::Fetched { branch: into, head: commit('a') }));
    }

    #[test]
    fn project_answers() {
        let timeline = vec![
            entry(8, Moment::Assigned { term: term(), spawned: true }),
            entry(9, Moment::State { from: TaskState::Running, to: TaskState::Blocked }),
        ];
        snap("project_reply_status", &reply(Outcome::Project(Box::new(status(timeline, 10)))));
        snap("project_reply_list", &reply(Outcome::Projects(vec![project()])));
        snap("project_reply_task", &reply(Outcome::Task(Box::new(task()))));
        snap(
            "project_reply_facts",
            &reply(Outcome::Facts(vec![WorkerFacts { worker: term().worker, facts: facts() }])),
        );
        let refused = Outcome::Error {
            code: ErrorCode::Unplaced,
            message: "no worker can take task 3".to_owned(),
        };
        snap(
            "project_reply_node",
            &reply(Outcome::Node(Box::new(NodeDetail { task: Some(task()), natives: natives() }))),
        );
        snap(
            "project_reply_working_on",
            &reply(Outcome::WorkingOn(Some((project_id(), Some(TaskId(3)))))),
        );
        snap("project_reply_unplaced", &reply(refused));
        for (name, code) in [
            ("project_reply_conflict", ErrorCode::Conflict),
            ("project_reply_unknown_project", ErrorCode::UnknownProject),
            ("project_reply_unknown_task", ErrorCode::UnknownTask),
            ("project_reply_limit", ErrorCode::Limit),
            ("project_reply_bad_expression", ErrorCode::BadExpression),
            ("project_reply_nothing_new", ErrorCode::NothingNew),
            ("project_reply_protected", ErrorCode::Protected),
        ] {
            snap(name, &reply(Outcome::Error { code, message: String::new() }));
        }
    }

    /// A task's pull request with a failed check, as its thread's row says it.
    fn pull() -> PullSeen {
        PullSeen {
            forge: Forge::GitHub,
            number: 42,
            url: "https://github.com/o/r/pull/42".to_owned(),
            title: "Keep projects beside workers.json".to_owned(),
            base: "main".to_owned(),
            stands: PullStands::ChecksFailed,
            failed: 2,
            failed_first: Some("clippy (macos)".to_owned()),
            running: 1,
        }
    }

    fn report() -> Report {
        Report {
            note: "Store keeps a log; gate passed.".to_owned(),
            artifacts: vec!["docs/decisions/projects.md".to_owned()],
            branch: Some("slopty/slopty/3".to_owned()),
            pr: Some(42),
        }
    }

    /// Every shape a fact takes.
    fn facts() -> Facts {
        let labels = BTreeMap::from([
            ("fast-disk".to_owned(), Fact::Bool(true)),
            ("vram_gb".to_owned(), Fact::Int(24)),
            ("rack".to_owned(), Fact::Text("b2".to_owned())),
        ]);
        BTreeMap::from([
            ("load".to_owned(), Fact::Float(1.5)),
            ("labels".to_owned(), Fact::Map(labels)),
            ("rust_targets".to_owned(), Fact::List(vec![Fact::Text("wasm32-wasip2".to_owned())])),
        ])
    }

    /// Every timeline moment, as it is pushed inside a change, and the snapshot's sequence.
    #[test]
    fn project_pushed() {
        let moments = [
            Moment::Created,
            Moment::Orchestrator { term: term() },
            Moment::Limits { limits: Limits::default() },
            Moment::TaskCreated { title: "Server store".to_owned() },
            Moment::Assigned { term: term(), spawned: false },
            Moment::State { from: TaskState::Planned, to: TaskState::Running },
            Moment::Branch { branch: Some("slopty/slopty/3".to_owned()) },
            Moment::Verified(run(true, "gate passed")),
            Moment::Pull(pull()),
            Moment::AgentGone { term: term() },
            Moment::Reported { report: report() },
            Moment::Delivered { term: term(), reports: 3 },
            Moment::Note { text: "ready".to_owned() },
            Moment::Update(slopty_proto::project::Progress {
                summary: "Store merged; verbs next".to_owned(),
                next: Some("The verbs".to_owned()),
                done: false,
                at_ms: at(),
            }),
            Moment::Step(TaskStep {
                kind: StepKind::Clone,
                worker: term().worker,
                state: StepState::Failed { why: "fatal: Authentication failed".to_owned() },
                since_ms: at(),
                term: None,
                commits: None,
            }),
            Moment::Step(TaskStep {
                kind: StepKind::Merge,
                worker: term().worker,
                state: StepState::Done { detail: "main at dddddd".to_owned() },
                since_ms: at(),
                term: None,
                commits: None,
            }),
        ];
        let timeline: Vec<TimelineEntry> =
            (1..).zip(moments).map(|(seq, what)| entry(seq, what)).collect();
        let snapshot = status(timeline.clone(), 16);
        let part = ProjectsPart { seq: 49, first: true, last: true, projects: vec![snapshot] };
        snap("project_snapshot", &FromServer::Projects(Box::new(part)));
        let update = ProjectUpdate {
            project: project_id(),
            record: Some(project()),
            task: Some(task().card(&natives())),
            native: None,
            entry: timeline.last().cloned(),
        };
        let event =
            HubEvent { seq: 50, at_ms: at(), what: Happening::Project(Box::new(update.clone())) };
        snap("project_event_pushed", &FromServer::Event(event));
        let quiet = ProjectUpdate { record: None, entry: None, ..update.clone() };
        let event = HubEvent { seq: 51, at_ms: at(), what: Happening::Project(Box::new(quiet)) };
        snap("project_event_quiet", &FromServer::Event(event));
        let leaf = ProjectUpdate {
            record: None,
            task: None,
            entry: None,
            native: Some(NativeChange {
                task: Some(TaskId(3)),
                native: Native::Agent(native_agent()),
            }),
            ..update
        };
        let event = HubEvent { seq: 52, at_ms: at(), what: Happening::Project(Box::new(leaf)) };
        snap("project_event_native", &FromServer::Event(event));
    }

    /// What a worker says it is and has.
    #[test]
    fn worker_facts_reported() {
        snap("report_facts", &ToServer::Facts(facts()));
    }

    /// What a worker tells the server of its agents beyond their status.
    #[test]
    fn agent_reports() {
        let session = term().session;
        snap(
            "report_branch",
            &ToServer::Report(AgentReport::Branch(AgentBranch {
                session,
                worktree: Some(Worktree {
                    name: "rows".to_owned(),
                    path: "/w/.claude/worktrees/rows".to_owned(),
                    branch: Some("worktree-rows".to_owned()),
                    original_cwd: "/w".to_owned(),
                    original_branch: Some("main".to_owned()),
                }),
            })),
        );
        snap(
            "report_subagent_started",
            &ToServer::Report(AgentReport::SubagentStarted {
                session,
                agent: "ag1".to_owned(),
                kind: "Explore".to_owned(),
            }),
        );
        snap(
            "report_subagent_stopped",
            &ToServer::Report(AgentReport::SubagentStopped {
                session,
                agent: "ag1".to_owned(),
                transcript: Some("/t/ag1.jsonl".to_owned()),
                last: Some("Found it.".to_owned()),
            }),
        );
        let task = NativeTask { id: "1".to_owned(), subject: "Read".to_owned(), done: false };
        snap("report_native_task", &ToServer::Report(AgentReport::NativeTask { session, task }));
        snap(
            "report_permission_mode",
            &ToServer::Report(AgentReport::PermissionMode {
                session,
                mode: "bypassPermissions".to_owned(),
            }),
        );
        snap(
            "report_loosened",
            &ToServer::Report(AgentReport::Loosened {
                session,
                found: vec!["--allowedTools".to_owned()],
            }),
        );
        snap("report_delivered", &ToServer::Report(AgentReport::Delivered { session, batch: 7 }));
        snap(
            "deliver",
            &FromServer::Deliver {
                session,
                batch: 7,
                reports: slopty_proto::server::Reports {
                    open: "<slopty-reports project=\"slopty\">".to_owned(),
                    blocks: vec![slopty_proto::server::ReportBlock {
                        id: 12,
                        text: "task 3: done".to_owned(),
                    }],
                    close: "</slopty-reports>".to_owned(),
                },
            },
        );
    }

    /// A project name is checked as it decodes, so a bad one never reaches the store.
    #[test]
    fn a_bad_project_name_does_not_decode() {
        let bytes = codec::encode_body(&"Not A Name").expect("encodes");
        let decoded: Result<ProjectId, _> = codec::decode_body(&bytes);
        decoded.unwrap_err();
    }
}
