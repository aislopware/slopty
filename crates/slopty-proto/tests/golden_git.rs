//! Golden byte snapshots of the person's git ops on a folder's repository
//! (`slopty_proto::git`): asked of the worker straight by the commit sheet, and through the
//! server by the CLI. A changed snapshot is a wire change: accept it deliberately
//! (`cargo insta review`).

#[cfg(test)]
mod golden_git {
    use slopty_core::WorkerId;
    use slopty_proto::git::{GitDone, GitFile, GitOp, GitOutcome, GitStatus};
    use slopty_proto::orchestration::{Outcome, Verb};
    use slopty_proto::server::{FromServer, ToServer};
    use slopty_proto::{ClientMsg, WorkerMsg, codec};
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

    #[test]
    fn client_git_ops() {
        let ask = |request, op| ClientMsg::Git { request, repo: "~/src/demo".to_owned(), op };
        snap("client_git_status", &ask(3, GitOp::Status));
        snap("client_git_commit", &ask(4, commit()));
        snap("client_git_push", &ask(5, GitOp::Push));
        snap("client_git_pull_request", &ask(6, pull_request()));
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
        };
        snap("worker_git_pushed", &done(5, GitOutcome::Done(pushed)));
        let opened = GitDone::PullRequest { url: "https://github.com/o/demo/pull/7".to_owned() };
        snap("worker_git_pull_request", &done(6, GitOutcome::Done(opened)));
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

    #[test]
    fn server_git_verbs() {
        let worker =
            WorkerId::from_uuid(Uuid::from_u128(0x0199_a000_0000_7000_8000_0000_0000_0001));
        let request = |op| ToServer::Request {
            id: 21,
            key: None,
            verb: Verb::Git { worker, repo: "~/src/demo".to_owned(), op },
        };
        snap("verb_git_commit", &request(commit()));
        snap("verb_git_pull_request", &request(pull_request()));
        snap(
            "outcome_git_status",
            &FromServer::Reply { id: 21, outcome: Outcome::Git(Box::new(status())) },
        );
    }
}
