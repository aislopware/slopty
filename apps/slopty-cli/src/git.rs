//! `slopty git …`: the person's commit sheet on the command line, over a repository on a
//! worker (`docs/decisions/projects.md`, "The person commits, pushes and opens a pull request
//! from any thread"). The worker runs the person's own git and gh; a refusal is in their words.

use anyhow::{Result, bail};
use clap::Subcommand;
use serde_json::json;
use slopty_proto::git::{GitDone, GitOp, GitStatus};
use slopty_proto::orchestration::IdempotencyKey;
use slopty_tools::ops;
use slopty_tools::resolve::Resolver;

use crate::link::Link;
use crate::verbs::print_json;

/// `slopty git …`.
#[derive(Subcommand, Debug)]
pub enum GitCmd {
    /// The repository's branch, its upstream, and its changed files in git's own letters.
    Status {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// A folder in the repository on the worker: absolute, or `~/…`.
        repo: String,
    },
    /// Commit the files named, as they are in the working tree, and nothing else staged.
    Commit {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// A folder in the repository on the worker: absolute, or `~/…`.
        repo: String,
        /// The commit message; given more than once, each is a paragraph, as with git.
        #[arg(long, short = 'm', required = true)]
        message: Vec<String>,
        /// Every changed file `slopty git status` lists, instead of naming them.
        #[arg(long, conflicts_with = "paths")]
        all: bool,
        /// Paths from the repository's root; a rename names both of its paths.
        #[arg(required_unless_present = "all")]
        paths: Vec<String>,
    },
    /// Push the branch checked out, setting its upstream on the repository's remote the first
    /// time.
    Push {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// A folder in the repository on the worker: absolute, or `~/…`.
        repo: String,
    },
    /// Open a pull request for the branch checked out with the worker's own `gh`, and print
    /// where it is.
    Pr {
        /// Worker id or name (the only worker online when omitted).
        #[arg(long)]
        worker: Option<String>,
        /// A folder in the repository on the worker: absolute, or `~/…`.
        repo: String,
        /// Its title; gh fills the title and description from the commits when omitted.
        #[arg(long)]
        title: Option<String>,
        /// Its description.
        #[arg(long, default_value = "")]
        body: String,
        /// The branch it merges into (the repository's default when omitted).
        #[arg(long)]
        base: Option<String>,
        /// Open it as a draft.
        #[arg(long)]
        draft: bool,
    },
}

pub async fn git(cmd: GitCmd, link: &Link, json: bool, key: Option<IdempotencyKey>) -> Result<()> {
    let mut res = Resolver::new(link);
    let (worker, repo, op) = match cmd {
        GitCmd::Status { worker, repo } => (worker, repo, GitOp::Status),
        GitCmd::Commit { worker, repo, message, all, mut paths } => {
            if all {
                let status =
                    ops::git(&mut res, worker.as_deref(), repo.clone(), GitOp::Status, None);
                let GitDone::Status(status) = status.await? else { bail!("no status came back") };
                paths = changed(&status);
                if paths.is_empty() {
                    bail!("nothing in {} has changed", status.root);
                }
            }
            (worker, repo, GitOp::Commit { paths, message: message.join("\n\n") })
        }
        GitCmd::Push { worker, repo } => (worker, repo, GitOp::Push),
        GitCmd::Pr { worker, repo, title, body, base, draft } => {
            let op = GitOp::PullRequest { title: title.unwrap_or_default(), body, base, draft };
            (worker, repo, op)
        }
    };
    let done = ops::git(&mut res, worker.as_deref(), repo, op, key).await?;
    if json {
        print_json(&done_json(&done))?;
    } else {
        print!("{}", done_text(&done));
    }
    Ok(())
}

/// Every changed path a status lists, a rename's former path too; not what git ignores.
fn changed(status: &GitStatus) -> Vec<String> {
    let files = status.files.iter().filter(|f| f.xy != "!!");
    files.flat_map(|f| std::iter::once(f.path.clone()).chain(f.from.clone())).collect()
}

fn done_json(done: &GitDone) -> serde_json::Value {
    match done {
        GitDone::Status(status) => json!(status),
        GitDone::Committed { commit, branch, files } => {
            json!({ "commit": commit, "branch": branch, "files": files })
        }
        GitDone::Pushed { remote, branch, upstream_set } => {
            json!({ "remote": remote, "branch": branch, "upstream_set": upstream_set })
        }
        GitDone::PullRequest { url } => json!({ "url": url }),
    }
}

/// What a git op did, as lines: a status as `git status --short` shows one.
fn done_text(done: &GitDone) -> String {
    match done {
        GitDone::Status(status) => {
            let branch = status.branch.as_deref().unwrap_or("HEAD (detached)");
            let head = match &status.upstream {
                Some(up) => {
                    format!("## {branch}...{up} (ahead {}, behind {})", status.ahead, status.behind)
                }
                None => format!("## {branch} (no upstream)"),
            };
            let files = status.files.iter().map(|file| {
                let xy = file.xy.replace('.', " ");
                match &file.from {
                    Some(from) => format!("{xy} {from} -> {}", file.path),
                    None => format!("{xy} {}", file.path),
                }
            });
            let more = (status.more > 0).then(|| format!("… and {} more", status.more));
            let lines: Vec<String> = std::iter::once(head).chain(files).chain(more).collect();
            format!("{}\n", lines.join("\n"))
        }
        GitDone::Committed { commit, branch, files } => {
            let short = commit.get(..12).unwrap_or(commit);
            let on = branch.as_deref().unwrap_or("a detached HEAD");
            let plural = if *files == 1 { "" } else { "s" };
            format!("committed {short} on {on}: {files} file{plural}\n")
        }
        GitDone::Pushed { remote, branch, upstream_set } => {
            let set = if *upstream_set { ", its upstream set" } else { "" };
            format!("pushed {branch} to {remote}{set}\n")
        }
        GitDone::PullRequest { url } => format!("{url}\n"),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use slopty_proto::git::GitFile;

    use super::*;

    #[derive(clap::Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: GitCmd,
    }

    fn parse(args: &[&str]) -> Result<GitCmd, clap::Error> {
        Cli::try_parse_from(std::iter::once("git").chain(args.iter().copied())).map(|c| c.cmd)
    }

    /// A commit takes the person's message, as paragraphs, and either the paths named or
    /// `--all`, never both and never neither; a pull request's title may be left to gh.
    #[test]
    fn the_git_verbs_parse() {
        let GitCmd::Commit { repo, message, all, paths, .. } =
            parse(&["commit", "~/r", "-m", "Subject", "-m", "Body", "a.rs", "b.rs"]).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            (repo.as_str(), message.join("\n\n"), all),
            ("~/r", "Subject\n\nBody".into(), false)
        );
        assert_eq!(paths, ["a.rs", "b.rs"]);
        assert!(matches!(
            parse(&["commit", "~/r", "-m", "s", "--all"]),
            Ok(GitCmd::Commit { all: true, .. })
        ));
        parse(&["commit", "~/r", "a.rs"]).unwrap_err();
        parse(&["commit", "~/r", "-m", "s"]).unwrap_err();
        parse(&["commit", "~/r", "-m", "s", "--all", "a.rs"]).unwrap_err();
        let GitCmd::Pr { title, draft, .. } = parse(&["pr", "~/r", "--draft"]).unwrap() else {
            panic!()
        };
        assert_eq!((title, draft), (None, true));
    }

    /// `--all` commits what the status lists, both paths of a rename, and nothing ignored; the
    /// status prints as git's short form does.
    #[test]
    fn all_is_every_changed_path_and_a_status_reads_as_git_s_short_form() {
        let file = |path: &str, from: Option<&str>, xy: &str| GitFile {
            path: path.to_owned(),
            from: from.map(str::to_owned),
            xy: xy.to_owned(),
        };
        let status = GitStatus {
            root: "/w/r".to_owned(),
            branch: Some("main".to_owned()),
            head: Some("abc".to_owned()),
            upstream: Some("origin/main".to_owned()),
            ahead: 1,
            behind: 0,
            files: vec![
                file("src/a.rs", None, ".M"),
                file("new.md", Some("old.md"), "R."),
                file("target/x", None, "!!"),
                file("notes.txt", None, "??"),
            ],
            more: 0,
        };
        assert_eq!(changed(&status), ["src/a.rs", "new.md", "old.md", "notes.txt"]);
        assert_eq!(
            done_text(&GitDone::Status(Box::new(status))),
            "## main...origin/main (ahead 1, behind 0)\n M src/a.rs\nR  old.md -> new.md\n!! \
             target/x\n?? notes.txt\n"
        );
    }
}
