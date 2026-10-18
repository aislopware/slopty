//! Golden byte snapshots of the agent messages: the primitives one agent drives another with,
//! what the server asks of a worker to start one or carry a branch, a thread read and answered,
//! the news a worker hands an agent as a message, and what a worker reports of itself and
//! its agents. A changed snapshot is a wire change: accept it deliberately (`cargo insta
//! review`).

#[cfg(test)]
mod golden_agent {
    use std::collections::BTreeMap;

    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::agent::{AgentEntry, AgentStart, PARENT_FACT, Worktree};
    use slopty_proto::codec;
    use slopty_proto::folder::FsOp;
    use slopty_proto::orchestration::{BranchBundle, Outcome, TermRef, ThreadOf, Verb};
    use slopty_proto::server::{AgentReport, Fact, Facts, FromServer, ToServer, WorkerFacts};
    use slopty_proto::settings::{DaemonSettings, SettingEdit};
    use slopty_proto::terminal::RepoId;
    use slopty_proto::thread::wire::{NewWorktree, Start};
    use slopty_proto::thread::{AgentId, Phase, ThreadId};
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

    fn thread(n: u128) -> ThreadId {
        ThreadId::from_uuid(Uuid::from_u128(0x0199_a1b1_c3d4_7000_8000_0000_0000_0000 | n))
    }

    fn commit(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn request(verb: Verb) -> ToServer {
        ToServer::Request { id: 21, key: None, verb }
    }

    fn reply(outcome: Outcome) -> FromServer {
        FromServer::Reply { id: 21, outcome }
    }

    /// The primitives: a start, words told, a wait, a stop, the tree, a branch brought across.
    #[test]
    fn agent_verbs() {
        let start = AgentStart {
            worker: term().worker,
            folder: Some("~/src/slopty".to_owned()),
            prompt: "Make the store keep its agents.".to_owned(),
            agent: Some(AgentId::named(AgentId::CODEX)),
            model: Some("gpt-6".to_owned()),
            effort: Some("high".to_owned()),
            worktree: Some("store".to_owned()),
            repo: Some("https://github.com/aislopware/slopty.git".to_owned()),
        };
        snap("agent_start", &request(Verb::AgentStart(Box::new(start))));
        let text = "The schema moved; read crates/slopty-server/src/store.rs again.".to_owned();
        snap("agent_tell", &request(Verb::AgentTell { agent: thread(7), text }));
        let agents = vec![thread(7), thread(8)];
        snap("agent_wait", &request(Verb::AgentWait { agents, all: true, timeout_ms: 600_000 }));
        snap("agent_stop", &request(Verb::AgentStop { agent: thread(7) }));
        snap("agents", &request(Verb::Agents));
        let bring = Verb::BranchBring {
            from: term().worker,
            repo: "/w/.claude/worktrees/store".to_owned(),
            branch: "worktree-store".to_owned(),
            to: WorkerId::from_uuid(Uuid::from_u128(0x0199_a000_0000_7000_8000_0000_0000_0002)),
            into: None,
            name: Some("store".to_owned()),
        };
        snap("branch_bring", &request(bring));
    }

    /// The tree as the server answers it, and a wait that ran out.
    #[test]
    fn agent_answers() {
        let entry = AgentEntry {
            id: thread(7),
            parent: Some(thread(1)),
            worker: term().worker,
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: "Store keeps agents".to_owned(),
            folder: Some("/w/.claude/worktrees/store".to_owned()),
            branch: Some("worktree-store".to_owned()),
            phase: Phase::Idle,
            wait: None,
            ended: false,
            asks: None,
            last: Some("Done: the store keeps them.".to_owned()),
            since_ms: at(),
            rests: true,
        };
        let agents = vec![entry];
        snap("agents_listed", &reply(Outcome::Agents { agents, timed_out: false }));
        snap("agents_timed_out", &reply(Outcome::Agents { agents: Vec::new(), timed_out: true }));
    }

    /// What the server asks a worker to start an agent at a seat, with its parent on its row.
    #[test]
    fn start_thread() {
        let start = Start {
            agent: AgentId::named(AgentId::PI),
            cwd: "/w/slopty".to_owned(),
            drive: None,
            prompt: Some("Make the store keep its agents.".to_owned()),
            model: None,
            mode: Some("edits".to_owned()),
            effort: None,
            attachments: Vec::new(),
            args: Vec::new(),
            options: BTreeMap::new(),
            worktree: Some(NewWorktree {
                name: "store".to_owned(),
                base: None,
                merge_base: None,
                pull: None,
                setup: true,
            }),
        };
        let verb = Verb::StartThread {
            worker: term().worker,
            start: Box::new(start),
            seat: term().session,
            facts: vec![(PARENT_FACT.to_owned(), thread(1).to_string())],
            system: Some("The slopty tools reach the person's machines and agents.".to_owned()),
        };
        snap("start_thread", &request(verb));
        let worktree = Worktree {
            name: "store".to_owned(),
            path: "/w/slopty/.claude/worktrees/store".to_owned(),
            branch: Some("worktree-store".to_owned()),
            original_cwd: "/w/slopty".to_owned(),
            original_branch: Some("main".to_owned()),
        };
        let started =
            Outcome::ThreadStarted { thread: thread(7), worktree: Some(Box::new(worktree)) };
        snap("thread_started", &reply(started));
    }

    /// An agent's change to a worker's files, and where it left the entry.
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

    /// A thread read and answered through the thread model, whatever its agent: sent to its
    /// worker once found, the whole turns it gives with an open request, and the person's
    /// answer to that request.
    #[test]
    fn thread_reads() {
        use slopty_proto::orchestration::{
            ReadEntry, RequestRead, ThreadRead, ThreadView, TurnRead,
        };
        use slopty_proto::thread::{AskId, Choice, Effect, ToolState, TurnId, TurnState};

        let found = Verb::ReadThread {
            of: ThreadOf::On { worker: term().worker, thread: thread(0x7e7e) },
            view: ThreadView::Activity,
            after: Some(TurnId(4)),
        };
        snap("read_thread_on", &request(found));
        let read = ThreadRead {
            worker: term().worker,
            thread: thread(0x7e7e),
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
                        child: Some(thread(0xc41d)),
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
                    folders: Vec::new(),
                    stops: false,
                }],
                questions: Vec::new(),
                picks: Vec::new(),
                needs_presence: false,
            }],
            next: TurnId(4),
            truncated: true,
            skipped: false,
        };
        snap("thread_read", &reply(Outcome::Thread(Box::new(read))));
        let answer = Verb::AnswerRequest {
            of: ThreadOf::Thread(thread(0x7e7e)),
            ask: AskId("7".to_owned()),
            choice: "deny".to_owned(),
            message: Some("Not on main.".to_owned()),
        };
        snap("answer_request", &request(answer));
        let reply = Verb::SendMessage {
            of: ThreadOf::Thread(thread(0x7e7e)),
            text: "Use the staging database.".to_owned(),
        };
        snap("send_message", &request(reply));
    }

    /// A daemon's own settings, read and edited from another device.
    #[test]
    fn daemon_settings() {
        let edit = |key: &str, entry: Option<&str>, literal: Option<&str>| SettingEdit {
            table: "worker".to_owned(),
            key: key.to_owned(),
            entry: entry.map(str::to_owned),
            literal: literal.map(str::to_owned),
        };
        snap(
            "settings_edit",
            &request(Verb::Settings {
                of: Some(term().worker),
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
    }

    /// What the server asks of workers to start an agent beside a clone and to bring a branch
    /// across: a clone, a branch bundled, a bundle fetched; and how they answer.
    #[test]
    fn clones_and_bundles() {
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
        let branch = "worktree-store".to_owned();
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
                branch: branch.clone(),
                into: branch.clone(),
                head: commit('a'),
            }),
        );
        snap("fetched", &reply(Outcome::Fetched { branch, head: commit('a') }));
    }

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

    /// What a worker says it is and has, and the machines as the server answers them.
    #[test]
    fn worker_facts() {
        snap("report_facts", &ToServer::Facts(facts()));
        snap("worker_facts", &request(Verb::WorkerFacts { worker: Some(term().worker) }));
        let all = vec![WorkerFacts { worker: term().worker, facts: facts() }];
        snap("facts", &reply(Outcome::Facts(all)));
    }

    /// What a worker tells the server of its agents beyond their status, and the news the
    /// server hands an agent as a message.
    #[test]
    fn agent_reports() {
        let session = term().session;
        snap("report_delivered", &ToServer::Report(AgentReport::Delivered { session, batch: 7 }));
        snap(
            "deliver",
            &FromServer::Deliver {
                session,
                batch: 7,
                reports: slopty_proto::server::Reports {
                    open: "<slopty-news>".to_owned(),
                    blocks: vec![slopty_proto::server::ReportBlock {
                        id: 12,
                        text: "Agent 0199a1b1 (Store keeps agents) came to rest.".to_owned(),
                    }],
                    close: "</slopty-news>".to_owned(),
                },
            },
        );
    }
}
