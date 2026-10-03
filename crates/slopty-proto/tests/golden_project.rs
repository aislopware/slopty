//! Golden byte snapshots of the project messages (`slopty_proto::project`): the verbs and
//! answers, the change pushed to every client, the snapshot a client gets on connecting, and
//! what a worker reports of its agents. A changed snapshot is a wire change: accept it
//! deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_project {
    use std::collections::BTreeMap;

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::agent::{AgentBranch, PullRequest, Review, Worktree};
    use slopty_proto::codec;
    use slopty_proto::orchestration::{
        BranchBundle, ErrorCode, Happening, HubEvent, Outcome, Size, TermRef, Verb,
    };
    use slopty_proto::project::{
        AgentReport, Assignment, Bounds, Budget, Fact, Facts, Limits, LimitsChange, Live, Merge,
        Moment, Native, NativeAgent, NativeChange, NativeTask, Natives, Need, NodeDetail, Peer,
        Placed, Placement, Preference, Project, ProjectId, ProjectStatus, ProjectUpdate,
        ProjectsPart, Reason, Report, ReportKind, RunOn, Runner, Spend, Spent, StepKind, StepState,
        Suggestion, Task, TaskChange, TaskId, TaskLaunch, TaskSpec, TaskState, TaskStep,
        TimelineEntry, VerifierRun, WorkerFacts,
    };
    use slopty_proto::server::{FromServer, ToServer};
    use slopty_proto::terminal::RepoId;
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

    /// A member: the folder at `cwd` on `machine`.
    fn notes_on(machine: &str, cwd: &str) -> slopty_proto::project::Matcher {
        [("machine".to_owned(), machine.to_owned()), ("cwd".to_owned(), cwd.to_owned())].into()
    }

    fn project_id() -> ProjectId {
        ProjectId::new("slopty").expect("a name")
    }

    fn project() -> Project {
        Project {
            spend: Spend {
                cost_micro_usd: 4_200_000,
                windows: BTreeMap::from([("five-hour".to_owned(), 3_100)]),
            },
            needs: Vec::new(),
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
            review: Some("A wire change comes with its goldens".to_owned()),
            push: false,
            ask_to_start: true,
            orchestrator: Some(term()),
            limits: Limits::default(),
            metadata: Some(r#"{"goal":"open"}"#.to_owned()),
            created_ms: at(),
            members: vec![notes_on("studio", "~/notes")],
        }
    }

    fn placement() -> Placement {
        Placement {
            pin: None,
            require: vec![r#"os == "linux" && cpus >= 16"#.to_owned()],
            prefer: vec![Preference { expr: "has(probes.cuda)".to_owned(), weight: 5 }],
            near: vec![Peer::Task(TaskId(1))],
            avoid: vec![Peer::Worker(term().worker)],
        }
    }

    fn spec() -> TaskSpec {
        TaskSpec {
            parent: Some(TaskId(1)),
            depends_on: vec![TaskId(2)],
            kind: "build".to_owned(),
            title: "Server store".to_owned(),
            brief: "Keep projects beside workers.json.".to_owned(),
            owns: vec!["crates/slopty-server".to_owned()],
            read_only: false,
            placement: placement(),
            verifier: Some("cargo nextest run -p slopty-server".to_owned()),
            metadata: Some(r#"{"lane":"server"}"#.to_owned()),
        }
    }

    fn task() -> Task {
        let s = spec();
        Task {
            checks: None,
            spent: Spent {
                active_ms: 754_000,
                since_ms: Some(WallMs::from_millis(1_790_000_004_500)),
            },
            id: TaskId(3),
            parent: s.parent,
            depends_on: s.depends_on,
            kind: s.kind,
            title: s.title,
            brief: s.brief,
            owns: s.owns,
            read_only: s.read_only,
            placement: s.placement,
            verifier: s.verifier,
            metadata: s.metadata,
            state: TaskState::Blocked,
            status: Some("waiting on a permission".to_owned()),
            assignment: Some(Assignment {
                term: term(),
                since_ms: at(),
                ended_ms: None,
                conversation: Some("0199a1b1-c3d4-7000-8000-00000000c0de".to_owned()),
                placed: Some(Placed {
                    pinned: false,
                    score: 110,
                    why: "near #2 +100, os == \"macos\" +10".to_owned(),
                }),
            }),
            branch: Some("slopty/slopty/3".to_owned()),
            worktree: Some("/w/slopty-3".to_owned()),
            base: Some(commit('b')),
            pr: Some(PullRequest {
                number: 42,
                url: "https://github.com/o/r/pull/42".to_owned(),
                review: Some(Review::Pending),
                merge_request: false,
            }),
            verified: Some(run(false, "clippy: 2 errors")),
            reviewed: None,
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
            }),
            proposal: None,
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
            bounds: Bounds { permission_flags: true, ..Bounds::default() },
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

    fn launch(run: Runner) -> TaskLaunch {
        TaskLaunch {
            pin: None,
            cwd: "~/src/slopty".to_owned(),
            run,
            env: vec![("A".to_owned(), "1".to_owned())],
            size: Some(Size { cols: 120, rows: 36 }),
            ignore_dependencies: false,
        }
    }

    #[test]
    fn project_verbs() {
        let limits = LimitsChange {
            live_per_worker: Some(2),
            live_per_project: Some(6),
            depth: Some(4),
            timeline_kept: Some(1024),
            budget: Some(Budget(BTreeMap::from([
                (Budget::USD.to_owned(), 50_000_000),
                ("five-hour".to_owned(), 8_000),
            ]))),
        };
        snap(
            "project_create",
            &request(Verb::ProjectCreate {
                project: project_id(),
                title: "Projects mode".to_owned(),
                repo: "~/src/slopty".to_owned(),
                target: "main".to_owned(),
                verifier: Some("cargo gate".to_owned()),
                review: Some("A wire change comes with its goldens".to_owned()),
                push: false,
                ask_to_start: false,
                orchestrator: Some(term()),
                limits,
                metadata: Some(r#"{"goal":"open"}"#.to_owned()),
                members: vec![notes_on("studio", "~/notes")],
            }),
        );
        snap(
            "project_set",
            &request(Verb::ProjectSet {
                project: project_id(),
                orchestrator: None,
                verifier: None,
                review: Some(String::new()),
                push: Some(true),
                ask_to_start: None,
                limits: LimitsChange { depth: Some(3), ..LimitsChange::default() },
                metadata: None,
                members: Some(vec![notes_on("studio", "~/notes"), notes_on("devbox", "/w/notes")]),
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
        snap(
            "task_claim",
            &request(Verb::TaskClaim {
                project: project_id(),
                task: TaskId(3),
                paths: vec!["docs/decisions/projects.md".to_owned()],
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
            placement: Some(placement()),
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
            "task_assign",
            &request(Verb::TaskAssign { project: project_id(), task: TaskId(3), term: term() }),
        );
        let claude = Runner::Claude {
            prompt: Some("Read your brief: slopty task status.".to_owned()),
            args: vec!["--model".to_owned(), "opus".to_owned()],
        };
        snap(
            "task_spawn",
            &request(Verb::TaskSpawn {
                project: project_id(),
                task: TaskId(3),
                launch: TaskLaunch {
                    pin: Some(term().worker),
                    ignore_dependencies: true,
                    ..launch(claude)
                },
            }),
        );
        let bench = Runner::Command { argv: vec!["cargo".to_owned(), "bench".to_owned()] };
        snap(
            "task_spawn_command",
            &request(Verb::TaskSpawn {
                project: project_id(),
                task: TaskId(4),
                launch: launch(bench),
            }),
        );
        let codex = Runner::Codex {
            prompt: Some("Read your brief.".to_owned()),
            args: vec!["--model".to_owned(), "o3".to_owned()],
        };
        snap(
            "task_spawn_codex",
            &request(Verb::TaskSpawn {
                project: project_id(),
                task: TaskId(5),
                launch: launch(codex),
            }),
        );
        let apple = Need {
            name: "Apple work".to_owned(),
            paths: vec!["apps/slopty-ios".to_owned()],
            require: vec!["os == \"macos\"".to_owned()],
            prefer: Vec::new(),
        };
        let linux = Need {
            name: "Linux first".to_owned(),
            paths: Vec::new(),
            require: Vec::new(),
            prefer: vec![Preference { expr: "os == \"linux\"".to_owned(), weight: 20 }],
        };
        snap(
            "project_needs",
            &request(Verb::ProjectNeeds { project: project_id(), needs: vec![apple, linux] }),
        );
        snap(
            "placement_suggest",
            &request(Verb::PlacementSuggest {
                project: Some(project_id()),
                task: Some(TaskId(3)),
                placement: Some(placement()),
            }),
        );
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
            }),
        );
        snap("rebased", &reply(Outcome::Rebased { head: commit('d'), onto: commit('c') }));
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
        let merged = Merge::Merged {
            target: "main".to_owned(),
            head: commit('d'),
            at_ms: at(),
            pushed: true,
            push_failed: None,
        };
        let card = Task { merge: Some(merged), state: TaskState::Merged, ..task() };
        snap("task_merged_card", &card.card(&Natives::default()));
        let unpushed = Merge::Merged {
            target: "main".to_owned(),
            head: commit('d'),
            at_ms: at(),
            pushed: false,
            push_failed: Some("! [rejected] main -> main (fetch first)".to_owned()),
        };
        let card = Task { merge: Some(unpushed), state: TaskState::Merged, ..task() };
        snap("task_merged_unpushed_card", &card.card(&Natives::default()));
    }

    /// The person letting a project go.
    #[test]
    fn project_delete() {
        snap("project_delete", &request(Verb::ProjectDelete { project: project_id() }));
    }

    /// A start the orchestrator proposed, as the store keeps it and a card shows it, and the
    /// person starting it on a worker of their choice.
    #[test]
    fn proposed_start() {
        use slopty_proto::project::{Proposal, Proposed};
        let launch = TaskLaunch {
            pin: None,
            cwd: String::new(),
            run: Runner::Claude { prompt: Some("Read your brief.".to_owned()), args: Vec::new() },
            env: Vec::new(),
            size: None,
            ignore_dependencies: false,
        };
        let proposed = Proposed {
            since_ms: at(),
            runs: "claude".to_owned(),
            on: Some(term().worker),
            why: r#"os == "macos""#.to_owned(),
        };
        let task = Task {
            state: TaskState::Planned,
            assignment: None,
            step: None,
            merge: None,
            verified: None,
            proposal: Some(Proposal { launch, proposed }),
            ..task()
        };
        snap("task_proposed", &task);
        snap("task_proposed_card", &task.card(&Natives::default()));
        snap(
            "task_start",
            &request(Verb::TaskStart {
                project: project_id(),
                task: TaskId(3),
                pin: Some(term().worker),
            }),
        );
    }

    /// The person's next step for a task's agent, as they said it, on the timeline; and a
    /// rebase that conflicts, the step that gives the work back to be resolved.
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
        };
        snap("step_rebase_failed", &step);
    }

    /// A task's pull request's own checks: the read the server asks of the worker its agent
    /// ran on, what the forge said, and the timeline's word when where they stand moved.
    #[test]
    fn pull_request_checks() {
        use slopty_proto::project::{Checks, ChecksState};
        snap(
            "pull_checks",
            &request(Verb::PullChecks {
                worker: term().worker,
                cwd: "/w/slopty/.claude/worktrees/slopty-slopty-3".to_owned(),
                number: 42,
                merge_request: false,
            }),
        );
        let checks = Checks {
            state: ChecksState::Failing,
            passed: 6,
            failed: 1,
            pending: 0,
            skipped: 2,
            failing: vec!["clippy (macos)".to_owned()],
            why: None,
            at_ms: at(),
        };
        snap("outcome_checks", &reply(Outcome::Checks(checks.clone())));
        let entry = TimelineEntry {
            seq: 13,
            at_ms: at(),
            task: Some(TaskId(3)),
            what: Moment::Checks(checks),
        };
        snap("moment_checks", &entry);
    }

    /// A task's fresh-context review: the checkout the reviewer reads, its verdict, and the
    /// card that carries it.
    #[test]
    fn review() {
        use slopty_proto::project::{Finding, ReviewRun, ReviewVerdict, Reviewer};
        snap(
            "review_checkout",
            &request(Verb::ReviewCheckout {
                worker: term().worker,
                repo: "/w/slopty".to_owned(),
                worktree: "slopty-review-3".to_owned(),
                head: "slopty/slopty/3".to_owned(),
                target: "main".to_owned(),
            }),
        );
        snap(
            "checked_out",
            &reply(Outcome::CheckedOut {
                path: "/home/c/slopty/verify/slopty-review-3".to_owned(),
                head: commit('a'),
                base: commit('b'),
            }),
        );
        let verdict = ReviewVerdict {
            approved: false,
            summary: "The wire change has no golden.".to_owned(),
            findings: vec![
                Finding {
                    path: Some("crates/slopty-proto/src/project.rs".to_owned()),
                    line: Some(431),
                    severity: "blocker".to_owned(),
                    blocking: true,
                    body: "A new field on Project with no golden for it.".to_owned(),
                },
                Finding {
                    path: None,
                    line: None,
                    severity: "nit".to_owned(),
                    blocking: false,
                    body: "The doc says 'verifier' where it means the reviewer.".to_owned(),
                },
            ],
        };
        snap(
            "task_review",
            &request(Verb::TaskReview {
                project: project_id(),
                task: TaskId(3),
                verdict: verdict.clone(),
            }),
        );
        let run = ReviewRun {
            verdict,
            more: 1,
            head: commit('a'),
            base: commit('b'),
            by: Reviewer::Agent(term()),
            took_ms: 95_000,
        };
        let card = Task { reviewed: Some(run), state: TaskState::Waiting, ..task() };
        snap("task_reviewed_card", &card.card(&Natives::default()));
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
        let ranked = vec![
            Suggestion {
                worker: term().worker,
                name: "box".to_owned(),
                fits: true,
                score: 105,
                reasons: vec![
                    Reason {
                        need: None,
                        rule: "online".to_owned(),
                        held: true,
                        points: 0,
                        detail: String::new(),
                    },
                    Reason {
                        need: Some("GPU work".to_owned()),
                        rule: "has(probes.cuda)".to_owned(),
                        held: true,
                        points: 5,
                        detail: String::new(),
                    },
                ],
            },
            Suggestion {
                worker: WorkerId::from_uuid(Uuid::from_u128(2)),
                name: "studio".to_owned(),
                fits: false,
                score: 0,
                reasons: vec![Reason {
                    need: None,
                    rule: r#"os == "linux" && cpus >= 16"#.to_owned(),
                    held: false,
                    points: 0,
                    detail: "false here".to_owned(),
                }],
            },
        ];
        snap("project_reply_suggestions", &reply(Outcome::Suggestions(ranked)));
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
        ] {
            snap(name, &reply(Outcome::Error { code, message: String::new() }));
        }
    }

    fn report() -> Report {
        Report {
            kind: ReportKind::Done,
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
            Moment::Budget { meter: Budget::USD.to_owned(), share_bp: 10_000 },
            Moment::TaskCreated { title: "Server store".to_owned() },
            Moment::Claimed { paths: vec!["crates/slopty-server".to_owned()] },
            Moment::Assigned { term: term(), spawned: false },
            Moment::State { from: TaskState::Planned, to: TaskState::Running },
            Moment::Branch { branch: Some("slopty/slopty/3".to_owned()), pr: Some(42) },
            Moment::Verified(run(true, "gate passed")),
            Moment::AgentGone { term: term() },
            Moment::Reported { report: report() },
            Moment::Delivered { term: term(), reports: 3 },
            Moment::Note { text: "ready".to_owned() },
            Moment::Needs { names: vec!["Apple work".to_owned()] },
            Moment::Step(TaskStep {
                kind: StepKind::Clone,
                worker: term().worker,
                state: StepState::Failed { why: "fatal: Authentication failed".to_owned() },
                since_ms: at(),
                term: None,
            }),
            Moment::Step(TaskStep {
                kind: StepKind::Merge,
                worker: term().worker,
                state: StepState::Done { detail: "main at dddddd".to_owned() },
                since_ms: at(),
                term: None,
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
                pr: None,
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
                context: "<slopty-reports project=\"slopty\">\ntask 3: done\n</slopty-reports>"
                    .to_owned(),
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
