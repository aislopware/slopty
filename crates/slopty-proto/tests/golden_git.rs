//! Golden byte snapshots of the person's git ops on a folder's repository
//! (`slopty_proto::git`), asked of the worker straight by the commit sheet. A changed snapshot is a
//! wire change: accept it deliberately (`cargo insta review`).

#[cfg(test)]
mod golden_git {
    use slopty_proto::git::{
        Branch, Branches, GitDone, GitFile, GitOp, GitOutcome, GitStatus, PullCheck, PullStatus,
    };
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
    }

    #[test]
    fn worker_git_done() {
        let done = |request, outcome| WorkerMsg::GitDone { request, outcome };
        snap("worker_git_status", &done(3, GitOutcome::Done(status())));
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
        use slopty_proto::thread::wire::{Against, FileDiff, Review, ReviewScope};
        use slopty_proto::thread::{Patch, TreeRef};

        let ask = |request, op| ClientMsg::Git { request, repo: "~/src/demo".to_owned(), op };
        snap("client_git_changes_head", &ask(9, GitOp::Changes { against: Against::Head }));
        snap("client_git_changes_base", &ask(10, GitOp::Changes { against: Against::Base }));
        let file = FileDiff {
            path: "src/lib.rs".to_owned(),
            from: Some("1f2e3d4c".to_owned()),
            to: Some("5a6b7c8d".to_owned()),
            binary: false,
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
}
