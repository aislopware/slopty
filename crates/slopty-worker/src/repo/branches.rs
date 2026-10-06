//! The branches a new worktree of a repository could start from ([`GitOp::Branches`]).
//!
//! The clone's own and `origin`'s, one entry a name, the newest commit first, as the start's
//! place chip offers them for a base (`docs/decisions/agents.md`, "A start picks its
//! worktree's base branch").
//!
//! Only what the clone already knows is read: nothing is fetched, so the list is at once. The
//! worktree's own start fetches its base from `origin` for a moment, as before.
//!
//! [`GitOp::Branches`]: slopty_proto::git::GitOp::Branches

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use slopty_proto::git::{BRANCHES_MAX, Branch, Branches, GitOutcome};

use super::commit::run;

/// How long the listing may take: three reads of the clone's refs.
const WITHIN: Duration = Duration::from_secs(30);

/// The remote whose branches are offered beside the clone's own.
const ORIGIN: &str = "origin";

/// The branches of the repository at `root`, read by `git`.
pub(super) async fn branches(git: &Path, root: &Path) -> Result<Branches, GitOutcome> {
    let refs = [
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname)%09%(committerdate:unix)",
        "refs/heads",
        "refs/remotes/origin",
    ];
    let listed = run(git, root, &refs, None, WITHIN).await?;
    let current = run(git, root, &["symbolic-ref", "--quiet", "--short", "HEAD"], None, WITHIN)
        .await
        .ok()
        .map(|out| out.trim().to_owned())
        .filter(|name| !name.is_empty());
    let origin_head = ["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"];
    let default = run(git, root, &origin_head, None, WITHIN)
        .await
        .ok()
        .and_then(|out| out.trim().strip_prefix("origin/").map(str::to_owned))
        .filter(|name| !name.is_empty());
    Ok(read(&listed, current, default))
}

/// `git for-each-ref`'s lines (a ref's whole name, a tab, its newest commit's time) read into a
/// listing: a branch `origin` and the clone both have is one entry, at the later time, in the
/// order git sorted them, newest first.
fn read(listed: &str, current: Option<String>, default: Option<String>) -> Branches {
    let mut branches: Vec<Branch> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for line in listed.lines() {
        let Some((refname, time)) = line.split_once('\t') else { continue };
        let committed = time.trim().parse::<i64>().unwrap_or(0);
        let (name, local) = if let Some(name) = refname.strip_prefix("refs/heads/") {
            (name, true)
        } else if let Some(name) = refname
            .strip_prefix("refs/remotes/")
            .and_then(|rest| rest.strip_prefix(ORIGIN))
            .and_then(|rest| rest.strip_prefix('/'))
        {
            // `origin/HEAD` names the default, not a branch.
            if name == "HEAD" {
                continue;
            }
            (name, false)
        } else {
            continue;
        };
        if let Some(branch) = at.get(name).and_then(|&ix| branches.get_mut(ix)) {
            branch.local |= local;
            branch.remote |= !local;
            branch.committed = branch.committed.max(committed);
            continue;
        }
        at.insert(name.to_owned(), branches.len());
        branches.push(Branch { name: name.to_owned(), local, remote: !local, committed });
    }
    let more = branches.len().saturating_sub(BRANCHES_MAX);
    branches.truncate(BRANCHES_MAX);
    Branches { current, default, list: branches, more: u32::try_from(more).unwrap_or(u32::MAX) }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// git in `dir` as a test's own: no global or system config but a name to commit under.
    fn git_in(dir: &Path, args: &[&str]) {
        let git = crate::changes::git().unwrap_or_else(|| panic!("git"));
        let out = std::process::Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
            .output()
            .unwrap_or_else(|e| panic!("git runs: {e}"));
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A branch both have is one entry; `origin/HEAD` is the default and no branch; other
    /// remotes' branches and tags are not offered; the newest stays first.
    #[test]
    fn a_branch_both_have_is_listed_once() {
        let listed = "refs/heads/feature\t300\n\
                      refs/remotes/origin/main\t200\n\
                      refs/heads/main\t100\n\
                      refs/remotes/origin/HEAD\t200\n\
                      refs/remotes/origin/theirs\t50\n\
                      refs/remotes/fork/elsewhere\t400\n\
                      refs/tags/v1\t500\n";
        let read = read(listed, Some("feature".to_owned()), Some("main".to_owned()));
        let names: Vec<(&str, bool, bool, i64)> =
            read.list.iter().map(|b| (b.name.as_str(), b.local, b.remote, b.committed)).collect();
        assert_eq!(
            names,
            [("feature", true, false, 300), ("main", true, true, 200), ("theirs", false, true, 50)]
        );
        assert_eq!(
            (read.current.as_deref(), read.default.as_deref()),
            (Some("feature"), Some("main"))
        );
        assert_eq!(read.more, 0);
    }

    /// A clone of a repository lists its own branch, `origin`'s, and the default `origin`
    /// names; a detached `HEAD` has no current branch.
    #[tokio::test]
    async fn a_clone_lists_its_branches_and_origins() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("a temp dir: {e}"));
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let (upstream, clone): (PathBuf, PathBuf) = (root.join("upstream"), root.join("clone"));
        std::fs::create_dir_all(&upstream).unwrap_or_else(|e| panic!("{e}"));
        git_in(&upstream, &["init", "--quiet"]);
        std::fs::write(upstream.join("a.txt"), "a\n").unwrap_or_else(|e| panic!("{e}"));
        git_in(&upstream, &["add", "."]);
        git_in(&upstream, &["commit", "--quiet", "-m", "first"]);
        git_in(&upstream, &["branch", "theirs"]);
        git_in(&root, &["clone", "--quiet", "upstream", "clone"]);
        git_in(&clone, &["switch", "--quiet", "-c", "mine"]);
        let git = crate::changes::git().unwrap_or_else(|| panic!("git"));
        let listed = branches(git, &clone).await.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(listed.current.as_deref(), Some("mine"));
        assert_eq!(listed.default.as_deref(), Some("main"));
        let find = |name: &str| listed.list.iter().find(|b| b.name == name);
        let flags = |name: &str| find(name).map(|b| (b.local, b.remote));
        assert_eq!(flags("main"), Some((true, true)), "{listed:?}");
        assert_eq!(flags("mine"), Some((true, false)), "{listed:?}");
        assert_eq!(flags("theirs"), Some((false, true)), "{listed:?}");
        assert!(find("HEAD").is_none(), "{listed:?}");
        assert!(listed.list.iter().all(|b| b.committed == 1_700_000_000), "{listed:?}");

        git_in(&clone, &["switch", "--quiet", "--detach"]);
        let detached = branches(git, &clone).await.unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(detached.current, None, "a detached HEAD is on no branch");
    }
}
