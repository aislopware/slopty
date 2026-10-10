//! "Review a pull request…": one step, where the pull request's number or page is typed and
//! each repository it may be in is a line ("Review #123 in atlas"), and the machine's usual
//! agent starts on the one picked in a new worktree that checks the pull request out
//! (`NewWorktree::pull`). Its composer holds "Review pull request #N", to be
//! sent as it is or added to, and once the thread is there its review opens beside it on the
//! whole branch, read against the branch the pull request merges into as the forge names it
//! ([`crate::review::Scope::WholeBranch`]).
//!
//! The repositories are those the focused tile is in, then every one a shell or a thread
//! stands in, on each machine that can start an agent, each clone once: a thread in one of
//! the clone's worktrees names the clone. A page names its repository, whose line then leads;
//! otherwise the focused tile's comes first, so ↩ takes it. The number is typed: `123`, `#123`,
//! or the pull request's page; a GitLab merge request's `!123` or its page, which its words
//! then name as one ("Review merge request !123").

use gpui::{Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_core::ItemId;
use slopty_proto::git::Forge;
use slopty_proto::thread::ThreadId;

use super::WorkspaceView;
use super::actions::{ReviewPull, ReviewPullNumber, StartThread};
use crate::palette::PaletteItem;

/// The palette's line.
pub(super) const REVIEW_PULL: &str = "Review a pull request\u{2026}";

/// What the step's field says.
const PICK_NUMBER: &str = "Which pull request, by its number or page";

/// What the number step says until a number is typed.
const TYPE_NUMBER: &str = "Type the pull request's number";

/// What is said when no repository is open on a machine that can start an agent.
pub(super) const NO_REPO: &str = "No repository is open on a machine that can start an agent";

/// Where a clone's worktrees stand, under it: a repository inside one names the clone.
const WORKTREES: &str = "/.claude/worktrees/";

/// The worktree a start on pull request `number` makes for `item`: `pr-<number>-` and the last
/// four hex digits of the item's id, as [`super::starting::worktree_name`] ends its names, so
/// the runs of one message on one pull request are found as runs.
pub(super) fn worktree_of(number: u32, item: ItemId) -> String {
    let id = item.as_uuid().simple().to_string();
    format!("pr-{number}-{}", id.get(id.len().saturating_sub(4)..).unwrap_or(&id))
}

/// The pull request `text` names: its number, `#` and its number, or its page's address,
/// which ends in `/pull/<number>` (and maybe a tab of it, `/files`); a GitLab merge request's
/// `!` and its number, or its page's `/merge_requests/<number>`. `None` for anything else and
/// for 0.
#[cfg(test)]
pub(super) fn pull_number(text: &str) -> Option<u32> {
    typed_pull(text).map(|(number, _)| number)
}

/// The number `text` names, as `pull_number` reads it, with the forge the text says it is on:
/// GitLab for `!12` or a merge request's page, the page's host's for a pull request's page, and
/// none for a bare number, which either forge's repository may hold.
pub(super) fn typed_pull(text: &str) -> Option<(u32, Option<Forge>)> {
    let typed = text.trim();
    let (digits, forge) = match (typed.strip_prefix('#'), typed.strip_prefix('!')) {
        (Some(digits), _) => (digits, None),
        (_, Some(digits)) => (digits, Some(Forge::GitLab)),
        _ => (typed, None),
    };
    let (number, forge) = if digits.bytes().all(|b| b.is_ascii_digit()) {
        (digits, forge)
    } else if let Some((_, after)) = typed.rsplit_once("/merge_requests/") {
        (after.split(['/', '#', '?']).next()?, Some(Forge::GitLab))
    } else {
        let (before, after) = typed.rsplit_once("/pull/")?;
        let host = before.split("://").nth(1).and_then(|rest| rest.split('/').next());
        (after.split(['/', '#', '?']).next()?, host.map(Forge::of_host))
    };
    let number = number.parse::<u32>().ok().filter(|n| *n > 0)?;
    Some((number, forge))
}

/// The repository a pull request's page names: the last part of its path before `/pull/`, or
/// before GitLab's `/-/merge_requests/`. `None` for a bare number.
fn page_repo(text: &str) -> Option<&str> {
    let typed = text.trim();
    let (before, _) =
        typed.rsplit_once("/merge_requests/").or_else(|| typed.rsplit_once("/pull/"))?;
    let before = before.strip_suffix("/-").unwrap_or(before);
    before.rsplit('/').next().filter(|name| !name.is_empty())
}

/// What the start's composer asks of a request typed as `number` on `forge`: its forge's noun
/// and mark where the text said which, a pull request's where it did not.
pub(super) fn review_words(number: u32, forge: Option<Forge>) -> String {
    let forge = forge.unwrap_or(Forge::GitHub);
    format!("Review {} {}{number}", forge.noun(), forge.mark())
}

impl WorkspaceView {
    /// "Review a pull request…": one step, where what is typed makes a line for each
    /// repository the pull request may be in.
    pub(super) fn review_pull(
        &mut self,
        _: &ReviewPull,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let repos = self.pull_repos(cx);
        let Some((first, _)) = repos.first() else {
            self.show_notice(NO_REPO.to_owned(), cx);
            return;
        };
        let first = *first;
        let machines = repos.iter().any(|(w, _)| *w != first);
        // Each repository with its folder's name, and the words its line ends in.
        let named: Vec<(WorkerKey, String, String, String)> = repos
            .into_iter()
            .map(|(worker, repo)| {
                let name = self.repo_name(worker, &repo);
                let shown = if machines {
                    format!("{name} on {}", self.worker_name(worker))
                } else {
                    name.clone()
                };
                (worker, repo, name, shown)
            })
            .collect();
        self.pull_step(Vec::new(), PICK_NUMBER, window, cx);
        let Some(palette) = self.palette.clone() else { return };
        palette.update(cx, |p, cx| {
            p.set_empty(TYPE_NUMBER, cx);
            p.set_typed(
                move |text| {
                    let Some((number, forge)) = typed_pull(text) else { return Vec::new() };
                    let mark = forge.unwrap_or(Forge::GitHub).mark();
                    let mut order: Vec<&(WorkerKey, String, String, String)> =
                        named.iter().collect();
                    if let Some(page) = page_repo(text) {
                        order.sort_by_key(|(_, _, name, _)| name != page);
                    }
                    order
                        .into_iter()
                        .map(|(worker, repo, _, shown)| {
                            let action = Box::new(ReviewPullNumber {
                                worker: *worker,
                                repo: repo.clone(),
                                number,
                                forge,
                            });
                            PaletteItem::new(
                                &format!("Review {mark}{number} in {shown}"),
                                action,
                                &[],
                            )
                        })
                        .collect()
                },
                cx,
            );
        });
    }

    /// A pull request named: the machine's usual agent starts on it in a new worktree that
    /// checks it out, its composer holding the ask to review it.
    pub(super) fn review_pull_number(
        &mut self,
        pick: &ReviewPullNumber,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ReviewPullNumber { worker, repo, number, forge } = pick.clone();
        if !self.workers.get(&worker).is_some_and(super::Worker::is_linked) {
            let text = format!("{} is out of reach", self.worker_name(worker));
            self.show_notice(text, cx);
            return;
        }
        let Some(agent) = self.agent_for(worker) else {
            self.show_notice(super::agent_start::NO_AGENT.to_owned(), cx);
            return;
        };
        let start = StartThread { worker, agent, cwd: repo, worktree: true };
        let item = self.begin_start(start, window, cx);
        let words = review_words(number, forge);
        self.start_on_pull(item, number, &words, window, cx);
    }

    /// The thread a pull request's start began is there: its review opens beside it on the
    /// whole branch.
    pub(super) fn review_pull_thread(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        cx: &mut Context<Self>,
    ) {
        self.ask_review(key, thread, Some(crate::review::Scope::WholeBranch));
        self.faces_dirty = true;
        cx.notify();
    }

    /// The repositories a pull request may be reviewed in, each clone once: the focused
    /// tile's, then those its machine's shells and threads stand in, then every other
    /// machine's, by name. Only machines that can start an agent offer theirs.
    fn pull_repos(&self, cx: &gpui::App) -> Vec<(WorkerKey, String)> {
        let mut machines: Vec<WorkerKey> = self
            .workers
            .iter()
            .filter(|(key, w)| w.is_linked() && self.agent_for(**key).is_some())
            .map(|(key, _)| *key)
            .collect();
        machines.sort_by_key(|k| self.worker_name(*k).to_lowercase());
        if let Some(at) = self.context_worker().and_then(|k| machines.iter().position(|m| *m == k))
        {
            let key = machines.remove(at);
            machines.insert(0, key);
        }
        let here = self.changes_here().filter(|(key, _)| machines.contains(key));
        let mut out: Vec<(WorkerKey, String)> = Vec::new();
        let mut add = |worker: WorkerKey, repo: &str| {
            let clone = repo.split(WORKTREES).next().unwrap_or(repo).trim_end_matches('/');
            if !clone.is_empty() && !out.iter().any(|(w, r)| *w == worker && r == clone) {
                out.push((worker, clone.to_owned()));
            }
        };
        if let Some((worker, repo)) = &here {
            add(*worker, repo);
        }
        for worker in machines {
            let Some(w) = self.workers.get(&worker) else { continue };
            for repo in w.sessions.values().filter_map(|s| s.repo.as_deref()) {
                add(worker, repo);
            }
            for place in self.places_on(worker, cx) {
                if let Some(repo) = place.repo.as_deref() {
                    add(worker, repo);
                }
            }
        }
        out
    }

    /// `repo`'s name on `worker`: its folder's.
    fn repo_name(&self, worker: WorkerKey, repo: &str) -> String {
        super::tile::place_name(repo, Some(repo), self.home_of(worker))
            .unwrap_or_else(|| repo.to_owned())
    }

    /// Open a step of "Review a pull request…", unless a palette is open already.
    fn pull_step(
        &mut self,
        lines: Vec<PaletteItem>,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui::AppContext as _;
        if self.palette.is_some() {
            return;
        }
        let theme = self.theme.clone();
        let palette = cx.new(|cx| {
            crate::palette::CommandPalette::pick_step(lines, placeholder, theme, window, cx)
        });
        self.show_palette(palette, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::git::Forge;

    use super::{page_repo, review_words, typed_pull};

    /// A GitLab merge request is typed as `!12` or its page and named as one; a pull request's
    /// page says its host's forge; a bare number names none and reads as a pull request.
    #[test]
    fn a_merge_request_is_typed_and_named_as_gitlab_writes_it() {
        assert_eq!(typed_pull("!12"), Some((12, Some(Forge::GitLab))));
        let page = "https://gitlab.example.com/o/atlas/-/merge_requests/12/diffs";
        assert_eq!(typed_pull(page), Some((12, Some(Forge::GitLab))));
        let github = "https://github.com/o/atlas/pull/9#discussion";
        assert_eq!(typed_pull(github), Some((9, Some(Forge::GitHub))));
        assert_eq!(typed_pull(" #123 "), Some((123, None)));
        assert_eq!(typed_pull("!0"), None);
        assert_eq!(typed_pull("!x"), None);
        assert_eq!(review_words(12, Some(Forge::GitLab)), "Review merge request !12");
        assert_eq!(review_words(12, None), "Review pull request #12");
        assert_eq!(page_repo(page), Some("atlas"));
        assert_eq!(page_repo(github), Some("atlas"));
        assert_eq!(page_repo("#12"), None);
    }
}
