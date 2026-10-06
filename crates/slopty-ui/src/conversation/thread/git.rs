//! A repository's git as this client works it from a thread's or a review's tile.
//!
//! Its changed files, the commit the person makes of the ones they chose, the push, and the
//! branch's pull request (`slopty_proto::git`).
//!
//! [`GitBook`] numbers each op sent to the worker, holds it until the worker answers, and keeps
//! what each repository last said: the status, the pull request, its working tree's changes,
//! and how the person's last op went. A commit asked "and push" pushes once the commit is made,
//! never alongside it. After every op that changes something, the status is asked again, so the
//! list shows what is left. The hub owns one book per worker ([`super::ThreadHub::git`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use slopty_proto::git::{
    Branches, GitDone, GitOp, GitOutcome, GitStatus, PullStanding, PullStatus,
};
use slopty_proto::thread::wire::{Against, Review, ReviewScope};
use slopty_proto::{ClientMsg, RequestId};

/// What an op asked while the machine is out of reach says.
const OFFLINE: &str = "The machine is out of reach";

/// The branch's pull request, as far as this client has heard.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Pull {
    /// Not asked yet, or not answered.
    #[default]
    Unknown,
    /// The branch has none.
    None,
    /// It has this one.
    Known(Box<PullStatus>),
}

impl Pull {
    /// The pull request, when the branch has one.
    #[must_use]
    pub fn status(&self) -> Option<&PullStatus> {
        match self {
            Self::Known(pull) => Some(pull),
            Self::Unknown | Self::None => None,
        }
    }
}

/// How the person's last op in a repository went.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Said {
    /// A commit made: its short id and how many files it took.
    Committed {
        /// The commit, in hex.
        commit: String,
        /// Files it changed.
        files: u32,
    },
    /// The branch pushed.
    Pushed {
        /// Where: `origin/feature`.
        to: String,
    },
    /// A pull request opened, at its page.
    Opened {
        /// Its page.
        url: String,
    },
    /// The pull request merged, or queued to merge: gh's words.
    Merged {
        /// What gh said.
        said: String,
    },
    /// The worktree removed, and its branch with it unless that holds work not landed.
    Freed {
        /// What went and what stayed, in words.
        said: String,
    },
    /// Not tried, in Slopty's words, or a program it needs is missing.
    Refused {
        /// Why.
        why: String,
    },
    /// git or gh said no, in its own words.
    Failed {
        /// The end of what it said.
        said: String,
    },
}

impl Said {
    /// Whether the op went.
    #[must_use]
    pub const fn went(&self) -> bool {
        !matches!(self, Self::Refused { .. } | Self::Failed { .. })
    }
}

/// One repository, as it last stood.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Repo {
    /// Its branch, upstream and changed files, once read.
    pub status: Option<Box<GitStatus>>,
    /// Why its status could not be read, in the worker's words: not a repository, no git.
    pub unread: Option<String>,
    /// Its branch's pull request.
    pub pull: Pull,
    /// Why its pull request could not be read, in gh's words.
    pub pull_unread: Option<String>,
    /// gh is not on the worker, in words: a pull request can be neither opened nor merged.
    pub no_gh: Option<String>,
    /// How the person's last op went, under the op's number.
    pub said: Option<(RequestId, Said)>,
    /// Its working tree's changes against each commit it was compared with, as last read, or
    /// why they could not be ([`Review::absent`]).
    pub changes: HashMap<Against, Arc<Review>>,
    /// The branches a new worktree of it could start from, as last read.
    pub branches: Option<Arc<Branches>>,
}

/// An op on its way.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Asked {
    repo: String,
    op: GitOp,
    /// A commit the person asked to push once it is made.
    then_push: bool,
}

/// One worker's git ops, numbered, and what each repository last said.
#[derive(Clone, Debug, Default)]
pub struct GitBook {
    last: RequestId,
    asked: BTreeMap<RequestId, Asked>,
    repos: HashMap<String, Repo>,
}

impl GitBook {
    /// What `repo` last said; `None` before anything was asked of it.
    #[must_use]
    pub fn repo(&self, repo: &str) -> Option<&Repo> {
        self.repos.get(repo)
    }

    /// Ask `op` of the repository holding the folder `repo`: the message that asks it, under
    /// its number. An op that changes something lets go of what the last one said.
    pub fn ask(&mut self, repo: &str, op: GitOp) -> (RequestId, ClientMsg) {
        self.ask_then(repo, op, false)
    }

    /// Commit `paths` with `message`, then push once the commit is made.
    pub fn commit_and_push(
        &mut self,
        repo: &str,
        paths: Vec<String>,
        message: String,
    ) -> (RequestId, ClientMsg) {
        self.ask_then(repo, GitOp::Commit { paths, message }, true)
    }

    fn ask_then(&mut self, repo: &str, op: GitOp, then_push: bool) -> (RequestId, ClientMsg) {
        self.last = self.last.wrapping_add(1);
        let request = self.last;
        if changes(&op) {
            self.repos.entry(repo.to_owned()).or_default().said = None;
        } else {
            self.repos.entry(repo.to_owned()).or_default();
        }
        self.asked.insert(request, Asked { repo: repo.to_owned(), op: op.clone(), then_push });
        (request, ClientMsg::Git { request, repo: repo.to_owned(), op })
    }

    /// The op that changes something in `repo` and is on its way, the oldest first; a commit
    /// asked "and push" reads as the commit until it is made, then as the push.
    #[must_use]
    pub fn busy(&self, repo: &str) -> Option<&GitOp> {
        self.asked.values().find(|a| a.repo == repo && changes(&a.op)).map(|a| &a.op)
    }

    /// Whether a read of `repo`'s status, or of its pull request with `pull`, is on its way.
    #[must_use]
    pub fn reading(&self, repo: &str, pull: bool) -> bool {
        let read = if pull { GitOp::PullStatus } else { GitOp::Status };
        self.asked.values().any(|a| a.repo == repo && a.op == read)
    }

    /// The worker answered `request`: the repository it was for, and what goes next (the push
    /// after a commit asked "and push", a fresh status after a change). `None` for a request
    /// this book did not send or already heard.
    pub fn answer(
        &mut self,
        request: RequestId,
        outcome: GitOutcome,
    ) -> Option<(String, Vec<ClientMsg>)> {
        let asked = self.asked.remove(&request)?;
        let repo = self.repos.entry(asked.repo.clone()).or_default();
        let mut then = Vec::new();
        let read = !changes(&asked.op);
        match outcome {
            GitOutcome::Done(done) => {
                repo_done(repo, request, done, asked.then_push, &mut then);
            }
            GitOutcome::Refused { why } => missed(repo, request, &asked.op, why, refused),
            GitOutcome::Failed { said } => {
                missed(repo, request, &asked.op, said, |said| Said::Failed { said });
            }
            GitOutcome::Unavailable { program, why } => {
                if program == "gh" {
                    repo.no_gh = Some(why.clone());
                }
                missed(repo, request, &asked.op, why, refused);
            }
        }
        // A worktree asked to go has no status to read again, whether it went or stayed.
        if !read && asked.op != GitOp::RemoveWorktree {
            then.push(Then::Status);
        }
        let name = asked.repo;
        let msgs = then.into_iter().map(|t| self.ask(&name, t.op()).1).collect();
        Some((name, msgs))
    }

    /// The machine is out of reach, so `op` was not asked: the repository says so where the
    /// op's part shows.
    pub fn offline(&mut self, repo: &str, op: &GitOp) {
        let repo = self.repos.entry(repo.to_owned()).or_default();
        missed(repo, 0, op, OFFLINE.to_owned(), refused);
    }

    /// The link went, and the answers on their way with it. What each op did shows in the
    /// next status.
    pub fn lost(&mut self) {
        self.asked.clear();
    }
}

/// What goes after an answer.
enum Then {
    Push,
    Status,
    Pull,
}

impl Then {
    const fn op(&self) -> GitOp {
        match self {
            Self::Push => GitOp::Push,
            Self::Status => GitOp::Status,
            Self::Pull => GitOp::PullStatus,
        }
    }
}

fn repo_done(repo: &mut Repo, request: RequestId, done: GitDone, push: bool, then: &mut Vec<Then>) {
    match done {
        GitDone::Status(status) => {
            repo.status = Some(status);
            repo.unread = None;
        }
        GitDone::PullStatus(pull) => {
            repo.pull = pull.map_or(Pull::None, Pull::Known);
            repo.pull_unread = None;
        }
        GitDone::Committed { commit, files, .. } => {
            repo.said = Some((request, Said::Committed { commit, files }));
            if push {
                then.push(Then::Push);
            }
        }
        GitDone::Pushed { remote, branch, pull, .. } => {
            repo.said = Some((request, Said::Pushed { to: format!("{remote}/{branch}") }));
            if let Some(pull) = pull {
                repo.pull = Pull::Known(pull);
            }
        }
        GitDone::PullRequest { url } => {
            repo.said = Some((request, Said::Opened { url }));
            then.push(Then::Pull);
        }
        GitDone::Merged { said, pull } => {
            repo.said = Some((request, Said::Merged { said }));
            match pull {
                Some(pull) => repo.pull = Pull::Known(pull),
                None => then.push(Then::Pull),
            }
        }
        GitDone::Changes(review) => {
            if let ReviewScope::WorkingTree(against) = review.scope.clone() {
                repo.changes.insert(against, Arc::from(review));
            }
        }
        GitDone::Branches(branches) => repo.branches = Some(Arc::from(branches)),
        GitDone::WorktreeRemoved { branch, branch_removed } => {
            let said = match branch {
                Some(branch) if branch_removed => {
                    format!("Removed the worktree and its branch {branch}")
                }
                Some(branch) => {
                    format!(
                        "Removed the worktree; kept its branch {branch}, which holds work not merged"
                    )
                }
                None => "Removed the worktree".to_owned(),
            };
            repo.said = Some((request, Said::Freed { said }));
        }
    }
}

/// An op that did not go: a read says why where its part shows, a change under the buttons.
fn missed(
    repo: &mut Repo,
    request: RequestId,
    op: &GitOp,
    words: String,
    said: impl FnOnce(String) -> Said,
) {
    match op {
        GitOp::Status => repo.unread = Some(words),
        GitOp::PullStatus => repo.pull_unread = Some(words),
        GitOp::Changes { against } => {
            let review = Review {
                scope: ReviewScope::WorkingTree(against.clone()),
                from: None,
                to: None,
                files: Vec::new(),
                absent: Some(words),
            };
            repo.changes.insert(against.clone(), Arc::new(review));
        }
        // Branches that could not be read (not a repository, out of reach) leave a start's
        // place as words: there is no worktree to make of it.
        GitOp::Branches => {}
        _ => repo.said = Some((request, said(words))),
    }
}

const fn refused(why: String) -> Said {
    Said::Refused { why }
}

/// Whether `op` changes the repository or its pull request, rather than reads it.
const fn changes(op: &GitOp) -> bool {
    !matches!(op, GitOp::Status | GitOp::PullStatus | GitOp::Changes { .. } | GitOp::Branches)
}

/// A file's status in git's letters as the person reads it: the working tree's letter where it
/// changed, else the index's; "new" for an untracked file; an unmerged one as git spells it.
#[must_use]
pub fn letters(xy: &str) -> String {
    let mut chars = xy.chars();
    let (x, y) = (chars.next().unwrap_or('.'), chars.next().unwrap_or('.'));
    match (x, y) {
        ('?', '?') => "new".to_owned(),
        ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => xy.to_owned(),
        (x, '.' | ' ') => x.to_string(),
        (_, y) => y.to_string(),
    }
}

/// Where the branch stands against its upstream: "main → origin/main · 2 ahead", "feature ·
/// no upstream", "Detached HEAD".
#[must_use]
pub fn branch_line(status: &GitStatus) -> String {
    let Some(branch) = &status.branch else { return "Detached HEAD".to_owned() };
    let Some(upstream) = &status.upstream else { return format!("{branch} \u{b7} no upstream") };
    let counts: Vec<String> = [(status.ahead, "ahead"), (status.behind, "behind")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
    let line = format!("{branch} \u{2192} {upstream}");
    if counts.is_empty() { line } else { format!("{line} \u{b7} {}", counts.join(", ")) }
}

/// Where a pull request stands, in a few words: "Ready to merge", "Checks failing".
#[must_use]
pub fn standing_words(pull: &PullStatus) -> String {
    match pull.standing() {
        PullStanding::Failing => "Checks failing".to_owned(),
        PullStanding::Conflicting => format!("Conflicts with {}", pull.base),
        PullStanding::ChangesRequested => "Changes requested".to_owned(),
        PullStanding::Ready => "Ready to merge".to_owned(),
        PullStanding::Running => "Checks running".to_owned(),
        PullStanding::Waiting if pull.merge_state.eq_ignore_ascii_case("BEHIND") => {
            format!("Behind {}", pull.base)
        }
        PullStanding::Waiting if pull.review.eq_ignore_ascii_case("REVIEW_REQUIRED") => {
            "Waiting for review".to_owned()
        }
        PullStanding::Waiting => "Blocked".to_owned(),
        PullStanding::Draft => "Draft".to_owned(),
        PullStanding::Merged => "Merged".to_owned(),
        PullStanding::Closed => "Closed".to_owned(),
    }
}

/// How a pull request is merged, as gh names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Method {
    /// One commit of the branch's.
    #[default]
    Squash,
    /// A merge commit.
    Merge,
    /// The branch's commits, replayed on the base.
    Rebase,
}

impl Method {
    /// Every method, in the menu's order.
    pub const ALL: [Self; 3] = [Self::Squash, Self::Merge, Self::Rebase];

    /// gh's name for it.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }

    /// What the button says.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Squash => "Squash and merge",
            Self::Merge => "Merge",
            Self::Rebase => "Rebase and merge",
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::ClientMsg;
    use slopty_proto::git::{GitDone, GitFile, GitOp, GitOutcome, GitStatus, PullStatus};

    use super::{GitBook, Pull, Said, branch_line, letters};

    fn status(files: &[(&str, &str)]) -> GitStatus {
        GitStatus {
            root: "/w/r".to_owned(),
            branch: Some("feature".to_owned()),
            head: Some("abc".to_owned()),
            upstream: Some("origin/feature".to_owned()),
            ahead: 2,
            behind: 0,
            files: files
                .iter()
                .map(|(xy, path)| GitFile {
                    path: (*path).to_owned(),
                    from: None,
                    xy: (*xy).to_owned(),
                })
                .collect(),
            more: 0,
        }
    }

    fn pull() -> PullStatus {
        PullStatus {
            number: 7,
            url: "https://github.com/o/r/pull/7".to_owned(),
            title: "Add the sheet".to_owned(),
            state: "OPEN".to_owned(),
            draft: false,
            head: "feature".to_owned(),
            head_commit: "abc".to_owned(),
            base: "main".to_owned(),
            review: String::new(),
            mergeable: "MERGEABLE".to_owned(),
            merge_state: "CLEAN".to_owned(),
            checks: Vec::new(),
            more_checks: 0,
        }
    }

    fn ops(msgs: &[ClientMsg]) -> Vec<GitOp> {
        msgs.iter()
            .filter_map(|m| match m {
                ClientMsg::Git { op, .. } => Some(op.clone()),
                _ => None,
            })
            .collect()
    }

    /// A read of the branches is a read: what the last change said stays, nothing reads as
    /// busy, and no status is asked after it. Its answer is kept; a miss says nothing under
    /// the buttons.
    #[test]
    fn a_branches_read_changes_nothing_and_is_kept() {
        use slopty_proto::git::{Branch, Branches};

        let mut book = GitBook::default();
        let (commit, _) =
            book.ask("/w/r", GitOp::Commit { paths: Vec::new(), message: "m".into() });
        let done = GitDone::Committed { commit: "abc".to_owned(), branch: None, files: 1 };
        let _then = book.answer(commit, GitOutcome::Done(done));
        let (read, msg) = book.ask("/w/r", GitOp::Branches);
        assert_eq!(ops(&[msg]), [GitOp::Branches]);
        assert_eq!(book.busy("/w/r"), None, "a read is not busy");
        let branch = Branch { name: "main".to_owned(), local: true, remote: true, committed: 1 };
        let listed = Branches {
            current: Some("main".to_owned()),
            default: Some("main".to_owned()),
            list: vec![branch],
            more: 0,
        };
        let answered = GitOutcome::Done(GitDone::Branches(Box::new(listed.clone())));
        let (_, then) = book.answer(read, answered).expect("ours");
        assert!(then.is_empty(), "no status after a read: {then:?}");
        let repo = book.repo("/w/r").expect("asked of");
        assert_eq!(repo.branches.as_deref(), Some(&listed));
        assert!(matches!(repo.said, Some((_, Said::Committed { .. }))), "the commit still says");

        let (missed, _) = book.ask("/w/x", GitOp::Branches);
        let refused = GitOutcome::Refused { why: "not a repository".to_owned() };
        let _then = book.answer(missed, refused);
        let repo = book.repo("/w/x").expect("asked of");
        assert_eq!((repo.branches.as_ref(), repo.said.as_ref()), (None, None));
    }

    /// A commit asked "and push" pushes only once it is made, and every change asks the status
    /// again; the push's answer brings the pull request it carries.
    #[test]
    fn a_commit_and_push_pushes_once_committed_and_refreshes() {
        let mut book = GitBook::default();
        let (commit, msg) = book.commit_and_push("/w/r", vec!["a.rs".to_owned()], "Fix".to_owned());
        assert_eq!(
            ops(&[msg]),
            [GitOp::Commit { paths: vec!["a.rs".to_owned()], message: "Fix".to_owned() }]
        );
        assert!(matches!(book.busy("/w/r"), Some(GitOp::Commit { .. })));
        let done = GitDone::Committed { commit: "abc".to_owned(), branch: None, files: 1 };
        let (repo, then) = book.answer(commit, GitOutcome::Done(done)).expect("ours");
        assert_eq!(repo, "/w/r");
        assert_eq!(ops(&then), [GitOp::Push, GitOp::Status], "the push, then a fresh status");
        assert!(matches!(book.busy("/w/r"), Some(GitOp::Push)));
        let push = book
            .asked
            .keys()
            .copied()
            .find(|r| book.asked.get(r).is_some_and(|a| a.op == GitOp::Push));
        let pushed = GitDone::Pushed {
            remote: "origin".to_owned(),
            branch: "feature".to_owned(),
            upstream_set: false,
            pull: Some(Box::new(pull())),
        };
        let (_, then) = book.answer(push.expect("asked"), GitOutcome::Done(pushed)).expect("ours");
        assert_eq!(ops(&then), [GitOp::Status]);
        let repo = book.repo("/w/r").expect("asked of");
        assert!(matches!(&repo.said, Some((_, Said::Pushed { to })) if to == "origin/feature"));
        assert!(matches!(&repo.pull, Pull::Known(p) if p.number == 7));
        assert!(book.answer(commit, GitOutcome::Refused { why: "x".to_owned() }).is_none());
    }

    /// A failed commit says git's words and pushes nothing; a status read that fails says why
    /// where the files would show, and leaves the last op's words alone.
    #[test]
    fn a_failure_says_its_words_where_its_part_shows() {
        let mut book = GitBook::default();
        let (commit, _) = book.commit_and_push("/w/r", vec!["a".to_owned()], "m".to_owned());
        let failed = GitOutcome::Failed { said: "hook said no".to_owned() };
        let (_, then) = book.answer(commit, failed).expect("ours");
        assert_eq!(ops(&then), [GitOp::Status], "no push after a failed commit");
        let (read, _) = book.ask("/w/r", GitOp::Status);
        let unread = GitOutcome::Refused { why: "not a git repository".to_owned() };
        let (_, then) = book.answer(read, unread).expect("ours");
        assert!(then.is_empty(), "a read asks nothing after it");
        let repo = book.repo("/w/r").expect("asked of");
        assert_eq!(repo.unread.as_deref(), Some("not a git repository"));
        assert!(matches!(&repo.said, Some((_, Said::Failed { said })) if said == "hook said no"));
    }

    /// Without gh, a pull request read says so, and the repository remembers gh is missing.
    #[test]
    fn a_missing_gh_is_remembered() {
        let mut book = GitBook::default();
        let (read, _) = book.ask("/w/r", GitOp::PullStatus);
        let missing =
            GitOutcome::Unavailable { program: "gh".to_owned(), why: "gh is not here".to_owned() };
        book.answer(read, missing);
        let repo = book.repo("/w/r").expect("asked of");
        assert_eq!(repo.no_gh.as_deref(), Some("gh is not here"));
        assert_eq!(repo.pull_unread.as_deref(), Some("gh is not here"));
        let (read, _) = book.ask("/w/r", GitOp::Status);
        book.answer(read, GitOutcome::Done(GitDone::Status(Box::new(status(&[("??", "n")])))));
        assert!(book.repo("/w/r").and_then(|r| r.status.as_ref()).is_some());
    }

    /// A file's letters read as git's, the working tree's first; an untracked one as new.
    #[test]
    fn a_file_reads_in_git_s_letters() {
        assert_eq!(letters(".M"), "M");
        assert_eq!(letters("M."), "M");
        assert_eq!(letters("A."), "A");
        assert_eq!(letters("RM"), "M");
        assert_eq!(letters("R."), "R");
        assert_eq!(letters("??"), "new");
        assert_eq!(letters("UU"), "UU");
        assert_eq!(letters("AA"), "AA");
    }

    /// The branch line names the upstream and how far apart they are, or says there is none.
    #[test]
    fn the_branch_line_says_where_it_stands() {
        assert_eq!(branch_line(&status(&[])), "feature \u{2192} origin/feature \u{b7} 2 ahead");
        let mut none = status(&[]);
        none.upstream = None;
        assert_eq!(branch_line(&none), "feature \u{b7} no upstream");
        none.branch = None;
        assert_eq!(branch_line(&none), "Detached HEAD");
    }
}
