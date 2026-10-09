//! Golden byte snapshots of the person's git ops on a folder's repository
//! (`slopty_proto::git`), asked of the worker straight by the commit sheet. A changed snapshot is a
//! wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_git {
    use slopty_proto::git::{
        AgentWorktree, Branch, Branches, Forge, GitDone, GitFile, GitOp, GitOutcome, GitStatus,
        LineSide, PullCheck, PullComments, PullNote, PullStatus, PullThread, ReviewNote,
        ReviewVerdict, Worktrees,
    };
    use slopty_proto::thread::AgentId;
    use slopty_proto::{ClientMsg, WorkerMsg, codec};

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

    fn commit() -> GitOp {
        GitOp::Commit {
            paths: vec!["src/lib.rs".to_owned(), "docs/new.md".to_owned()],
            message: "Keep what matters\n\nIn the person's words.".to_owned(),
        }
    }

    fn pull_request() -> GitOp {
        GitOp::PullRequest {
            title: "Keep what matters".to_owned(),
            body: "Why it matters.".to_owned(),
            base: Some("main".to_owned()),
            draft: true,
        }
    }

    fn status() -> GitDone {
        GitDone::Status(Box::new(GitStatus {
            root: "/Users/c/src/demo".to_owned(),
            forge: Some(Forge::GitHub),
            branch: Some("feature".to_owned()),
            head: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            upstream: Some("origin/feature".to_owned()),
            ahead: 2,
            behind: 1,
            files: vec![
                GitFile { path: "src/lib.rs".to_owned(), from: None, xy: ".M".to_owned() },
                GitFile {
                    path: "docs/new.md".to_owned(),
                    from: Some("docs/old.md".to_owned()),
                    xy: "R.".to_owned(),
                },
                GitFile { path: "notes.txt".to_owned(), from: None, xy: "??".to_owned() },
            ],
            more: 0,
        }))
    }

    fn pull() -> PullStatus {
        PullStatus {
            forge: Forge::GitHub,
            number: 7,
            url: "https://github.com/o/demo/pull/7".to_owned(),
            title: "Keep what matters".to_owned(),
            state: "OPEN".to_owned(),
            draft: false,
            head: "feature".to_owned(),
            head_commit: "89abcdef0123456789abcdef0123456789abcdef".to_owned(),
            base: "main".to_owned(),
            review: "REVIEW_REQUIRED".to_owned(),
            mergeable: "MERGEABLE".to_owned(),
            merge_state: "BLOCKED".to_owned(),
            checks: vec![PullCheck {
                name: "test".to_owned(),
                workflow: Some("CI".to_owned()),
                state: "IN_PROGRESS".to_owned(),
                link: Some("https://github.com/o/demo/actions/runs/1".to_owned()),
            }],
            more_checks: 0,
        }
    }

    fn merge() -> GitOp {
        GitOp::Merge {
            method: "squash".to_owned(),
            head: Some("89abcdef0123456789abcdef0123456789abcdef".to_owned()),
            delete_branch: true,
        }
    }

    #[test]
    fn client_git_ops() {
        let ask = |request, op| ClientMsg::Git { request, repo: "~/src/demo".to_owned(), op };
        snap("client_git_status", &ask(3, GitOp::Status));
        snap("client_git_commit", &ask(4, commit()));
        snap("client_git_push", &ask(5, GitOp::Push));
        snap("client_git_pull_request", &ask(6, pull_request()));
        snap("client_git_pull_status", &ask(7, GitOp::PullStatus));
        snap("client_git_merge", &ask(8, merge()));
        let free = ClientMsg::Git {
            request: 9,
            repo: "~/src/demo/.claude/worktrees/fix-login".to_owned(),
            op: GitOp::RemoveWorktree,
        };
        snap("client_git_remove_worktree", &free);
        snap("client_git_branches", &ask(10, GitOp::Branches));
        snap("client_git_pull_comments", &ask(11, GitOp::PullComments { number: 7 }));
        snap("client_git_worktrees", &ask(12, GitOp::Worktrees));
        snap("client_git_blob", &ask(13, GitOp::Blob { blob: "cc33".to_owned() }));
        let review = GitOp::PullReview {
            number: 7,
            verdict: ReviewVerdict::RequestChanges,
            body: "Two things before this lands.".to_owned(),
            notes: vec![
                ReviewNote {
                    path: "src/lib.rs".to_owned(),
                    line: 12,
                    side: LineSide::New,
                    body: "This unwrap panics on an empty list.".to_owned(),
                },
                ReviewNote {
                    path: "src/old.rs".to_owned(),
                    line: 4,
                    side: LineSide::Old,
                    body: "Keep this guard.".to_owned(),
                },
            ],
            head: Some("89abcdef0123456789abcdef0123456789abcdef".to_owned()),
        };
        snap("client_git_pull_review", &ask(14, review));
    }

    #[test]
    fn worker_git_done() {
        let done = |request, outcome| WorkerMsg::GitDone { request, outcome };
        snap("worker_git_status", &done(3, GitOutcome::Done(status())));
        let blob = GitDone::Blob { blob: "cc33".to_owned(), bytes: b"\x89PNG\r\n\x1a\n".to_vec() };
        snap("worker_git_blob", &done(13, GitOutcome::Done(blob)));
        let reviewed = GitDone::PullReviewed {
            url: Some("https://github.com/o/demo/pull/7#pullrequestreview-1".to_owned()),
            posted: 2,
        };
        snap("worker_git_pull_reviewed", &done(14, GitOutcome::Done(reviewed)));
        let committed = GitDone::Committed {
            commit: "89abcdef0123456789abcdef0123456789abcdef".to_owned(),
            branch: Some("feature".to_owned()),
            files: 2,
        };
        snap("worker_git_committed", &done(4, GitOutcome::Done(committed)));
        let pushed = GitDone::Pushed {
            remote: "origin".to_owned(),
            branch: "feature".to_owned(),
            upstream_set: true,
            pull: Some(Box::new(pull())),
        };
        snap("worker_git_pushed", &done(5, GitOutcome::Done(pushed)));
        let opened = GitDone::PullRequest { url: "https://github.com/o/demo/pull/7".to_owned() };
        snap("worker_git_pull_request", &done(6, GitOutcome::Done(opened)));
        let read = GitDone::PullStatus(Some(Box::new(pull())));
        snap("worker_git_pull_status", &done(7, GitOutcome::Done(read)));
        snap("worker_git_no_pull", &done(7, GitOutcome::Done(GitDone::PullStatus(None))));
        let merge_request = PullStatus {
            forge: Forge::GitLab,
            url: "https://gitlab.example.com/o/demo/-/merge_requests/7".to_owned(),
            ..pull()
        };
        let read = GitDone::PullStatus(Some(Box::new(merge_request)));
        snap("worker_git_merge_request_status", &done(7, GitOutcome::Done(read)));
        let merged = GitDone::Merged {
            said: "Squashed and merged pull request #7".to_owned(),
            pull: Some(Box::new(PullStatus { state: "MERGED".to_owned(), ..pull() })),
        };
        snap("worker_git_merged", &done(8, GitOutcome::Done(merged)));
        let removed = GitDone::WorktreeRemoved {
            branch: Some("worktree-fix-login".to_owned()),
            branch_removed: false,
        };
        snap("worker_git_worktree_removed", &done(9, GitOutcome::Done(removed)));
        let branches = GitDone::Branches(Box::new(Branches {
            current: Some("feature".to_owned()),
            default: Some("main".to_owned()),
            list: vec![
                Branch {
                    name: "feature".to_owned(),
                    local: true,
                    remote: false,
                    committed: 1_700_000_300,
                },
                Branch {
                    name: "main".to_owned(),
                    local: true,
                    remote: true,
                    committed: 1_700_000_200,
                },
                Branch {
                    name: "theirs".to_owned(),
                    local: false,
                    remote: true,
                    committed: 1_700_000_100,
                },
            ],
            more: 3,
        }));
        snap("worker_git_branches", &done(10, GitOutcome::Done(branches)));
        let tree = |name: &str, merged: bool, changed: u32, busy: bool| AgentWorktree {
            path: format!("/Users/ada/src/demo/.claude/worktrees/{name}"),
            branch: Some(format!("worktree-{name}")),
            changed,
            busy,
            ahead: u32::from(!merged),
            merged,
            committed: 1_700_000_300,
            made_by: Some(AgentId::named(AgentId::CLAUDE_CODE)),
        };
        let worktrees = GitDone::Worktrees(Box::new(Worktrees {
            clone: "/Users/ada/src/demo".to_owned(),
            list: vec![
                tree("fix-login", true, 0, false),
                tree("draft", true, 2, true),
                AgentWorktree { branch: None, ..tree("open", false, 0, false) },
                AgentWorktree {
                    path: "/Users/ada/.codex/worktrees/1f2e/demo".to_owned(),
                    made_by: Some(AgentId::named(AgentId::CODEX)),
                    ..tree("codex", false, 1, false)
                },
                AgentWorktree {
                    path: "/Users/ada/src/demo-hotfix".to_owned(),
                    made_by: None,
                    ..tree("hotfix", true, 0, false)
                },
            ],
            more: 1,
        }));
        snap("worker_git_worktrees", &done(12, GitOutcome::Done(worktrees)));
        let note = |author: &str, body: &str| PullNote {
            author: author.to_owned(),
            body: body.to_owned(),
        };
        let comments = GitDone::PullComments(Box::new(PullComments {
            number: 7,
            threads: vec![
                PullThread {
                    path: None,
                    line: None,
                    outdated: false,
                    url: Some("https://github.com/o/demo/pull/7#pullrequestreview-1".to_owned()),
                    notes: vec![note("ada", "Split the parser out before this lands.")],
                },
                PullThread {
                    path: Some("src/lib.rs".to_owned()),
                    line: Some(42),
                    outdated: true,
                    url: Some("https://github.com/o/demo/pull/7#discussion_r2".to_owned()),
                    notes: vec![
                        note("ada", "This unwrap panics on an empty file."),
                        note("lin", "Agreed; return the error."),
                    ],
                },
            ],
            more: 1,
        }));
        snap("worker_git_pull_comments", &done(11, GitOutcome::Done(comments)));
        let refused = GitOutcome::Refused { why: "choose the files to commit".to_owned() };
        snap("worker_git_refused", &done(4, refused));
        let unavailable = GitOutcome::Unavailable {
            program: "gh".to_owned(),
            why: "gh is not on this worker".to_owned(),
        };
        snap("worker_git_unavailable", &done(6, unavailable));
        let failed =
            GitOutcome::Failed { said: "! [rejected] feature -> feature (fetch first)".to_owned() };
        snap("worker_git_failed", &done(5, failed));
    }

    /// A folder's working tree reviewed with no thread: asked against `HEAD` and against the
    /// branch's base, answered as a review or with why there is nothing to compare.
    #[test]
    fn folder_changes() {
        use slopty_proto::thread::detail::Hunk;
        use slopty_proto::thread::wire::{Against, FileDiff, FileKind, Review, ReviewScope};
        use slopty_proto::thread::{Patch, TreeRef};

        let ask = |request, op| ClientMsg::Git { request, repo: "~/src/demo".to_owned(), op };
        snap("client_git_changes_head", &ask(9, GitOp::Changes { against: Against::Head }));
        snap("client_git_changes_base", &ask(10, GitOp::Changes { against: Against::Base }));
        let file = FileDiff {
            path: "src/lib.rs".to_owned(),
            from: Some("1f2e3d4c".to_owned()),
            to: Some("5a6b7c8d".to_owned()),
            kind: FileKind::Text,
            old_path: None,
            modes: None,
            patch: Patch {
                hunks: vec![Hunk {
                    old_start: 3,
                    old_lines: 1,
                    new_start: 3,
                    new_lines: 1,
                    heading: Some("fn main()".to_owned()),
                    lines: vec!["-    old();".to_owned(), "+    new();".to_owned()],
                }],
                added: 1,
                removed: 1,
                clipped_lines: 0,
                full: None,
            },
        };
        let review = Review {
            scope: ReviewScope::WorkingTree(Against::Head),
            from: Some(TreeRef("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned())),
            to: Some(TreeRef("9a8b7c6d5e4f30211203f4e5d6c7b8a99a8b7c6d".to_owned())),
            files: vec![file],
            absent: None,
        };
        let done = |request, review: Review| WorkerMsg::GitDone {
            request,
            outcome: GitOutcome::Done(GitDone::Changes(Box::new(review))),
        };
        snap("worker_git_changes", &done(9, review));
        let none = Review {
            scope: ReviewScope::WorkingTree(Against::Base),
            from: None,
            to: None,
            files: Vec::new(),
            absent: Some("This repository has no base branch to compare with".to_owned()),
        };
        snap("worker_git_changes_absent", &done(10, none));
    }

    /// A clone asked of a worker straight, how far it has come, and how it went.
    #[test]
    fn clone_repo() {
        use slopty_proto::cloning::{CloneOutcome, ClonedRepo};
        use slopty_proto::terminal::RepoId;

        let ask = ClientMsg::CloneRepo {
            request: 13,
            url: "https://github.com/o/demo.git".to_owned(),
            into: "~/src/demo".to_owned(),
        };
        snap("client_clone_repo", &ask);
        let step = WorkerMsg::RepoCloning {
            request: 13,
            phase: "Receiving objects".to_owned(),
            percent: Some(45),
        };
        snap("worker_repo_cloning", &step);
        let repo = RepoId {
            origin: Some("github.com/o/demo".to_owned()),
            root: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            url: Some("https://github.com/o/demo.git".to_owned()),
        };
        let cloned = ClonedRepo { path: "/Users/ada/src/demo".to_owned(), repo };
        let done = |outcome| WorkerMsg::RepoCloned { request: 13, outcome };
        snap("worker_repo_cloned", &done(CloneOutcome::Cloned(cloned)));
        let why = "/Users/ada/src/demo is there already and is not a clone of github.com/o/demo";
        snap("worker_repo_clone_refused", &done(CloneOutcome::Refused { why: why.to_owned() }));
        let said = "fatal: repository 'https://github.com/o/demo.git/' not found".to_owned();
        snap("worker_repo_clone_failed", &done(CloneOutcome::Failed { said }));
    }

    /// A repository's run scripts asked of a worker, and the scripts ready to open.
    #[test]
    fn run_scripts() {
        use slopty_proto::git::{RunScript, RunScripts};

        let ask = ClientMsg::Git { request: 14, repo: "~/src/demo".to_owned(), op: GitOp::Scripts };
        snap("client_git_scripts", &ask);
        let script = RunScript {
            name: "web".to_owned(),
            line: "bun dev".to_owned(),
            command: ["/bin/zsh", "-l", "-i", "-c", "bun dev\nexec /bin/zsh -l"]
                .map(str::to_owned)
                .to_vec(),
            cwd: "/Users/ada/src/demo/apps/web".to_owned(),
            env: vec![("SLOPTY_WORKSPACE_PATH".to_owned(), "/Users/ada/src/demo".to_owned())],
        };
        let scripts =
            RunScripts { from: Some(".conductor/settings.toml".to_owned()), list: vec![script] };
        let done = WorkerMsg::GitDone {
            request: 14,
            outcome: GitOutcome::Done(GitDone::Scripts(Box::new(scripts))),
        };
        snap("worker_git_scripts", &done);
    }

    /// One file of a big review asked whole by its blobs, and its hunks.
    #[test]
    fn file_diff() {
        use slopty_proto::thread::Patch;
        use slopty_proto::thread::detail::Hunk;

        let (from, to) = (Some("1a2b3c4d".to_owned()), Some("5a6b7c8d".to_owned()));
        let op = GitOp::FileDiff { from: from.clone(), to: to.clone() };
        let ask = ClientMsg::Git { request: 15, repo: "~/src/demo".to_owned(), op };
        snap("client_git_file_diff", &ask);
        let patch = Patch {
            hunks: vec![Hunk {
                old_start: 3,
                old_lines: 1,
                new_start: 3,
                new_lines: 1,
                heading: Some("fn main()".to_owned()),
                lines: vec!["-    old();".to_owned(), "+    new();".to_owned()],
            }],
            added: 1,
            removed: 1,
            clipped_lines: 0,
            full: None,
        };
        let done = WorkerMsg::GitDone {
            request: 15,
            outcome: GitOutcome::Done(GitDone::FileDiff { from, to, patch: Box::new(patch) }),
        };
        snap("worker_git_file_diff", &done);
    }
}
