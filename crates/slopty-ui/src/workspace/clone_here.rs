//! The repositories another machine has that the one a start goes to does not: what the
//! folder step offers to clone there (readiness R14), and the step that clones one.
//!
//! The workspace knows a checkout by what a machine's shells and threads say of it: its root and
//! its [`RepoId`], whose `url` is the address to clone from with no credential in it. A
//! repository is offered on a machine when no checkout known there is the same repository (by
//! origin, else first commit) and some other machine's has an address. Each is offered once,
//! into the place it has on the machine it was found on, under the home there (`~/work/slopty`),
//! else in the home under its own name.
//!
//! Picked, the machine clones it with its own git and credentials (`ClientMsg::CloneRepo`). A
//! step opens at once saying so, then how far git is, and once the clone is there the agent
//! starts in it as from any folder. A refusal or git's failure is said in the step. A step put
//! away before the clone is done starts nothing; the clone is said as a notice when it lands.

use std::collections::HashMap;

use gpui::{Context, Window};
use slopty_client::layout::WorkerKey;
use slopty_proto::cloning::CloneOutcome;
use slopty_proto::terminal::RepoId;
use slopty_proto::thread::AgentId;
use slopty_proto::{ClientMsg, RequestId};

use super::WorkspaceView;
use super::actions::CloneToStart;
use super::agent_start::start;
use crate::palette::{CommandPalette, PaletteEvent, PaletteItem};

/// The folder step's line for a repository to clone there: "Clone `origin` into `~/…`".
pub(super) const CLONE: &str = "Clone";

/// What the clone step's field says: what comes once the clone is there.
const THEN_STARTS: &str = "Then the agent starts in it";

/// A clone asked for from the folder step, until the machine says how it went.
pub(super) struct CloneAsked {
    worker: WorkerKey,
    request: RequestId,
    agent: AgentId,
    origin: String,
    into: String,
    /// The step that says how far it is.
    step: gpui::EntityId,
}

/// A repository a machine could clone from another's address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Cloneable {
    /// Its origin, normalized (`github.com/aislopware/slopty`): what the line names.
    pub origin: String,
    /// The address to clone from.
    pub url: String,
    /// Where the clone goes on the machine, `~/…`.
    pub into: String,
}

/// Where a clone of the checkout at `root` goes on another machine: the same place under the
/// home there when `root` is under `home` on its own machine, else the home under its last
/// folder's name. `None` for a root with no name to go by.
fn place_for(root: &str, home: Option<&str>) -> Option<String> {
    let root = root.trim_end_matches('/');
    let under = home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
        .and_then(|h| root.strip_prefix(h))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty());
    if let Some(rest) = under {
        return Some(format!("~/{rest}"));
    }
    let name = root.rsplit('/').next().filter(|n| !n.is_empty() && *n != "~")?;
    Some(format!("~/{name}"))
}

/// The repositories of `known`'s checkouts (a machine, its root, its repository) that `here`
/// has none of, one each, by origin, each from the shortest root found for it so a clone wins
/// over its worktrees; `home_of` says each machine's home.
pub(super) fn cloneable_on<'a>(
    here: WorkerKey,
    known: impl IntoIterator<Item = (WorkerKey, &'a str, &'a RepoId)>,
    home_of: impl Fn(WorkerKey) -> Option<&'a str>,
) -> Vec<Cloneable> {
    let known: Vec<(WorkerKey, &str, &RepoId)> = known.into_iter().collect();
    let held: Vec<&RepoId> =
        known.iter().filter(|(w, ..)| *w == here).map(|(_, _, id)| *id).collect();
    let mut found: HashMap<&str, (WorkerKey, &str, &str)> = HashMap::new();
    for (worker, root, id) in known.iter().filter(|(w, ..)| *w != here) {
        let (Some(origin), Some(url)) = (id.origin.as_deref(), id.url.as_deref()) else {
            continue;
        };
        if held.iter().any(|h| h.same(id)) {
            continue;
        }
        let kept = found.entry(origin).or_insert((*worker, root, url));
        if (root.len(), *root) < (kept.1.len(), kept.1) {
            *kept = (*worker, root, url);
        }
    }
    let mut out: Vec<Cloneable> = found
        .into_iter()
        .filter_map(|(origin, (worker, root, url))| {
            Some(Cloneable {
                origin: origin.to_owned(),
                url: url.to_owned(),
                into: place_for(root, home_of(worker))?,
            })
        })
        .collect();
    out.sort_by(|a, b| a.origin.cmp(&b.origin));
    out
}

impl WorkspaceView {
    /// What `here` could clone of the repositories the other machines' shells stand in and
    /// their threads work in ([`cloneable_on`]).
    fn cloneable_here(&self, here: WorkerKey) -> Vec<Cloneable> {
        let shells = self.workers.iter().flat_map(|(w, worker)| {
            worker
                .sessions
                .values()
                .filter_map(move |s| Some((*w, s.repo.as_deref()?, s.repo_id.as_ref()?)))
        });
        cloneable_on(here, shells.chain(self.thread_repos()), |w| self.home_of(w))
    }

    /// The folder step's lines that clone a repository another machine has into `worker`, then
    /// start `agent` in it.
    pub(super) fn clone_lines(&self, agent: &AgentId, worker: WorkerKey) -> Vec<PaletteItem> {
        self.cloneable_here(worker)
            .into_iter()
            .map(|c| {
                let shown = format!("{CLONE} {} into {}", c.origin, c.into);
                let action = Box::new(CloneToStart {
                    worker,
                    agent: agent.clone(),
                    origin: c.origin,
                    url: c.url,
                    into: c.into,
                });
                PaletteItem::new(&shown, action, &[]).with_icon(crate::icons::GitGlyph::Repo)
            })
            .collect()
    }

    /// "Clone … into …" picked: the machine is asked to clone it, and a step opens at once
    /// saying so, then how far it is.
    pub(super) fn clone_to_start(
        &mut self,
        ask: &CloneToStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CloneToStart { worker, agent, origin, url, into } = ask.clone();
        if !self.workers.get(&worker).is_some_and(super::Worker::is_linked) {
            let text = format!("{} is out of reach", self.worker_name(worker));
            self.show_notice(text, cx);
            return;
        }
        let request = self.next_open.get();
        self.next_open.set(request.wrapping_add(1));
        self.send(worker, ClientMsg::CloneRepo { request, url, into: into.clone() });
        self.open_step(Vec::new(), THEN_STARTS, window, cx);
        let Some(palette) = self.palette.clone() else { return };
        let asked = CloneAsked { worker, request, agent, origin, into, step: palette.entity_id() };
        let saying = self.cloning_words(&asked, None);
        palette.update(cx, |p, cx| p.set_empty(saying, cx));
        self.clone_asked = Some(asked);
    }

    /// What the clone step says: what is cloned where, and how far git is when it said.
    fn cloning_words(&self, asked: &CloneAsked, step: Option<(&str, Option<u8>)>) -> String {
        let machine = self.worker_name(asked.worker);
        let what = format!("Cloning {} into {} on {machine}", asked.origin, asked.into);
        match step {
            Some((phase, Some(percent))) => format!("{what} \u{b7} {phase} {percent}%"),
            Some((phase, None)) if !phase.is_empty() => format!("{what} \u{b7} {phase}"),
            _ => format!("{what}\u{2026}"),
        }
    }

    /// The clone step still up for `request` on `key`, when it is.
    fn clone_step(
        &self,
        key: WorkerKey,
        request: RequestId,
    ) -> Option<gpui::Entity<CommandPalette>> {
        let asked =
            self.clone_asked.as_ref().filter(|a| a.worker == key && a.request == request)?;
        self.palette.clone().filter(|p| p.entity_id() == asked.step)
    }

    /// `key` says how far the clone it was asked for is: its step says so. Only the latest
    /// counts; a step the machine skipped while the link was behind is not missed.
    pub fn repo_cloning(
        &self,
        key: WorkerKey,
        request: RequestId,
        phase: &str,
        percent: Option<u8>,
        cx: &mut Context<Self>,
    ) {
        let Some(palette) = self.clone_step(key, request) else { return };
        let Some(asked) = self.clone_asked.as_ref() else { return };
        let saying = self.cloning_words(asked, Some((phase, percent)));
        palette.update(cx, |p, cx| p.set_empty(saying, cx));
    }

    /// The link to `key` went with a clone still out: its answer never comes, so the step
    /// says so. The machine may finish it; asked again, it answers with the clone it finds.
    pub(super) fn clone_lost(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(request) =
            self.clone_asked.as_ref().filter(|a| a.worker == key).map(|a| a.request)
        else {
            return;
        };
        let step = self.clone_step(key, request);
        let Some(asked) = self.clone_asked.take() else { return };
        let text = format!(
            "{} went out of reach before {} was cloned",
            self.worker_name(key),
            asked.origin
        );
        match step {
            Some(palette) => palette.update(cx, |p, cx| p.set_empty(text, cx)),
            None => self.show_failure(text, cx),
        }
    }

    /// `key` says how the clone went. A clone there starts the agent in it, from where the
    /// keyboard was, once the step is put away; a step put away already starts nothing and the
    /// clone is said as a notice. A refusal or git's failure is said in the step, or as a
    /// failure once the step is gone.
    pub fn repo_cloned(
        &mut self,
        key: WorkerKey,
        request: RequestId,
        outcome: CloneOutcome,
        cx: &mut Context<Self>,
    ) {
        if !self.clone_asked.as_ref().is_some_and(|a| a.worker == key && a.request == request) {
            return;
        }
        let step = self.clone_step(key, request);
        let Some(asked) = self.clone_asked.take() else { return };
        let machine = self.worker_name(key);
        let why = match outcome {
            CloneOutcome::Cloned(cloned) => {
                let Some(palette) = step else {
                    let text = format!("Cloned {} into {} on {machine}", asked.origin, cloned.path);
                    self.show_notice(text, cx);
                    return;
                };
                self.palette_action = Some(start(key, &asked.agent, cloned.path, false));
                palette.update(cx, |_, cx| cx.emit(PaletteEvent::Dismiss));
                return;
            }
            CloneOutcome::Refused { why } => why,
            CloneOutcome::Failed { said } => said,
        };
        let text = format!("Could not clone {} on {machine}: {why}", asked.origin);
        match step {
            Some(palette) => palette.update(cx, |p, cx| p.set_empty(text, cx)),
            None => self.show_failure(text, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(origin: &str, root: &str) -> RepoId {
        RepoId {
            origin: Some(origin.to_owned()),
            root: Some(root.to_owned()),
            url: Some(format!("https://{origin}.git")),
        }
    }

    /// A repository another machine has and this one does not is offered once, from its clone
    /// rather than a worktree, into the same place under the home; one this machine has, or
    /// one with no address, is not.
    #[test]
    fn a_repository_known_elsewhere_is_offered_where_it_is_missing() {
        let (here, laptop, studio) = (WorkerKey::new(1), WorkerKey::new(2), WorkerKey::new(3));
        let slopty = id("github.com/a/slopty", "c0ffee");
        let notes = id("github.com/a/notes", "beef");
        let local = RepoId { origin: None, root: Some("f00d".to_owned()), url: None };
        let known = [
            (laptop, "/Users/l/work/slopty/.worktrees/x", &slopty),
            (laptop, "/Users/l/work/slopty", &slopty),
            (studio, "/srv/slopty", &slopty),
            (studio, "/srv/notes", &notes),
            (laptop, "/Users/l/scratch", &local),
            (here, "/home/h/notes", &notes),
        ];
        let homes = |w: WorkerKey| {
            Some(if w == laptop {
                "/Users/l"
            } else if w == studio {
                "/home/s"
            } else {
                "/home/h"
            })
        };
        let offered = cloneable_on(here, known, homes);
        assert_eq!(
            offered,
            [Cloneable {
                origin: "github.com/a/slopty".to_owned(),
                url: "https://github.com/a/slopty.git".to_owned(),
                into: "~/slopty".to_owned(),
            }],
            "the shortest root wins: /srv/slopty, outside its home, goes in this home by name"
        );
        let elsewhere =
            cloneable_on(here, known.into_iter().filter(|k| k.1 != "/srv/slopty"), homes);
        assert_eq!(elsewhere[0].into, "~/work/slopty", "the same place under the home");
    }

    /// The place of a clone keeps its path under the home, else its name; a root with none
    /// gives none.
    #[test]
    fn a_clone_goes_where_it_stood_under_the_home() {
        assert_eq!(place_for("/Users/l/src/app/", Some("/Users/l")).as_deref(), Some("~/src/app"));
        assert_eq!(place_for("/opt/app", Some("/Users/l")).as_deref(), Some("~/app"));
        assert_eq!(place_for("/Users/l", Some("/Users/l")).as_deref(), Some("~/l"));
        assert_eq!(place_for("/", None), None);
    }
}
