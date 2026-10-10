//! What each tile is known by, and which tiles are one body of work: the facts the client
//! already holds turned into [`slopty_client::groups`]'s open map, and the groups the
//! navigator, the projects' names, the breadcrumb, the palette and the placement of a tile
//! from elsewhere read.
//!
//! A tile's facts come from what this client has today: its worker (`machine`, `os`), what it
//! shows (`kind`), the agent at work there (`agent`), its shell's directory, repository,
//! identity and branch (`cwd`, `repo`, `branch`), else the folder it is in (`folder`, a home
//! directory or the root naming nothing), the project the server's board puts its session in
//! or its thread's own facts name (`project`, and every thread fact as `facts.<key>`). A file
//! or a folder tile is in the deepest repository, else the deepest folder, of its worker's
//! shells that holds it, else in its own directory. A window, a display, a note or a page has
//! no place: it sits under its machine until the person pins it to a project.
//!
//! The declared projects claim the clones of the repository they work in, by its identity, so
//! a shell in a clone of an orchestrated project's repository is in that project on any
//! worker.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use slopty_client::groups::{self, Claim, Facts, Group, GroupKey, Grouped, Matcher, fact};
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_proto::items::{Item, ItemKind};
use slopty_proto::server::Os;
use slopty_proto::terminal::RepoId;
use slopty_proto::thread::ThreadId;

use super::WorkspaceView;
use super::faces::ThreadPlace;
use crate::icons::{GitGlyph, Mark, Symbol};

/// Tiles and the groups they fell into.
#[derive(Debug, Default)]
pub(super) struct Grouping {
    /// The tiles grouped, in the order given.
    pub tiles: Vec<TileRef>,
    /// What each was known by.
    pub facts: Vec<Facts>,
    /// The groups, and each tile's.
    pub grouped: Grouped,
    /// Where each tile is in `tiles`.
    pub at: HashMap<TileRef, usize>,
}

impl Grouping {
    /// The group `tile` is in.
    pub(super) fn group_of(&self, tile: TileRef) -> Option<&Group> {
        let at = self.at.get(&tile).copied()?;
        let group = self.grouped.of.get(at).copied().flatten()?;
        self.grouped.groups.get(group)
    }

    /// The group keyed `key`.
    pub(super) fn group(&self, key: &GroupKey) -> Option<&Group> {
        self.grouped.groups.iter().find(|g| g.key == *key)
    }

    /// The tiles of `group`, in the order given.
    pub(super) fn members<'a>(&'a self, group: &'a Group) -> impl Iterator<Item = TileRef> + 'a {
        group.members.iter().filter_map(|&i| self.tiles.get(i).copied())
    }

    /// The workers `group`'s tiles run on.
    pub(super) fn machines(&self, group: &Group) -> BTreeSet<WorkerKey> {
        self.members(group).map(|t| t.worker).collect()
    }
}

/// The projects as one frame groups them. The navigator, its rail, the breadcrumb and every
/// workspace's name each ask for them while the window draws, in the workspace's view and in
/// the chrome's; nothing they read changes in between, so the first asks and the rest share.
/// What is kept goes as the draw ends (the update that drew it flushes its deferred work before
/// any input), so an action, a worker's message or the next frame never reads a frame old.
#[derive(Debug, Default)]
pub(super) struct FrameProjects {
    /// Whether a draw holds what is asked.
    live: Cell<bool>,
    /// What the draw worked out, once asked.
    projects: RefCell<Option<Rc<Grouping>>>,
}

impl FrameProjects {
    /// Keep what is asked until the draw under way ends. `fresh` lets go of what the draw kept
    /// so far: the workspace's view changes its state as it begins to draw.
    pub(super) fn hold(self: &Rc<Self>, fresh: bool, cx: &mut gpui::App) {
        if fresh {
            self.projects.borrow_mut().take();
        }
        if !self.live.replace(true) {
            let memo = Rc::clone(self);
            cx.defer(move |_| {
                memo.live.set(false);
                memo.projects.borrow_mut().take();
            });
        }
    }
}

/// The values a clone of a repository is known by: its origin, its first commit and its place.
fn repo_values(worker: WorkerKey, root: &str, id: Option<&RepoId>) -> Vec<String> {
    let mut values: Vec<String> = Vec::with_capacity(3);
    if let Some(id) = id {
        values.extend(id.origin.clone());
        values.extend(id.root.as_ref().map(|r| format!("{}{r}", groups::COMMIT)));
    }
    values.push(groups::at(worker, root));
    values
}

/// The group a thread with no tile here lists under, by its `facts`: the project it names
/// or a declared one claims it for, else a group of `projects` it shares a value with,
/// else one of its own (the first link of the default chain it has, with no tile). None
/// where it has nothing but its machine.
pub(super) fn listing_group(projects: &Grouping, claims: &[Claim], facts: &Facts) -> Option<Group> {
    let own = |fact: &str, value: &str| Group {
        key: GroupKey::new(fact, value),
        fact: fact.to_owned(),
        values: vec![value.to_owned()],
        members: Vec::new(),
    };
    let named = facts.get(fact::PROJECT).and_then(|v| v.first()).cloned().or_else(|| {
        claims
            .iter()
            .find(|c| c.matchers.iter().any(|m| groups::matches(facts, m)))
            .map(|c| c.project.clone())
    });
    if let Some(project) = named {
        return Some(own(fact::PROJECT, &project));
    }
    let shared = projects.grouped.groups.iter().find(|g| {
        g.key.worker().is_none()
            && facts.get(&g.fact).is_some_and(|vs| vs.iter().any(|v| g.values.contains(v)))
    });
    if let Some(group) = shared {
        return Some(group.clone());
    }
    groups::DEFAULT_CHAIN.iter().filter(|f| **f != fact::MACHINE).find_map(|f| {
        let least = facts.get(*f)?.iter().min()?;
        Some(own(f, least))
    })
}

/// What a group of `fact` is drawn with: a repository's glyph, a folder for a folder and a
/// declared project (a project is a folder first), a machine's, a branch's, else a grid for any
/// other grouping. A machine's own header wears its form ([`crate::icons::machine`]).
pub(super) fn group_glyph(fact: &str) -> Mark {
    match fact {
        fact::REPO => GitGlyph::Repo.into(),
        fact::FOLDER | fact::PROJECT => Symbol::Folder.into(),
        fact::MACHINE => Symbol::ServerRack.into(),
        fact::BRANCH => GitGlyph::Branch.into(),
        fact::AGENT => crate::icons::AGENT.into(),
        _ => Symbol::SquareGrid2x2.into(),
    }
}

/// The heading over the groups of `fact`: *Projects* for the default chain's, else the fact
/// in the plural where it has a word for it, else the key as the person would type it.
pub(super) fn section_name(fact: &str) -> String {
    match fact {
        fact::PROJECT | fact::REPO | fact::FOLDER => "Projects".to_owned(),
        fact::AGENT => "Agents".to_owned(),
        fact::BRANCH => "Branches".to_owned(),
        fact::KIND => "Kinds".to_owned(),
        fact::OS => "Systems".to_owned(),
        fact::MACHINE => "Machines".to_owned(),
        other => crate::palette::sentence_case(other),
    }
}

/// Where a repository or a folder is when another listed has its name: an origin's owner, else
/// the path's parent's last two parts.
pub(super) fn parent_of(group: &Group) -> Option<String> {
    if let Some(origin) = group.identity().filter(|id| !id.starts_with(groups::COMMIT)) {
        let (owner, _) = origin.rsplit_once('/')?;
        return Some(owner.to_owned());
    }
    let (_, path) = group.places().next()?;
    let trimmed = path.trim_end_matches('/');
    let (parent, _) = trimmed.rsplit_once('/')?;
    let parts: Vec<&str> = parent.rsplit('/').take(2).filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.into_iter().rev().collect::<Vec<_>>().join("/"))
}

impl WorkspaceView {
    /// What `tile` is known by ([module docs](self)).
    pub(super) fn tile_facts(&self, tile: TileRef, item: &Item) -> Facts {
        let mut facts = Facts::new();
        let mut put = |key: &str, values: Vec<String>| {
            if !values.is_empty() {
                facts.insert(key.to_owned(), values);
            }
        };
        put(fact::MACHINE, vec![tile.worker.to_string()]);
        put(fact::KIND, vec![super::tile::kind_name(item).to_owned()]);
        let os = self.workers.get(&tile.worker).and_then(|w| w.caps.as_ref()).map(|c| match c.os {
            Os::MacOs => "macos",
            Os::Linux => "linux",
        });
        put(fact::OS, os.map(str::to_owned).into_iter().collect());
        put(fact::AGENT, self.item_agent(item).map(str::to_owned).into_iter().collect());
        let thread = match item.kind {
            ItemKind::Thread { thread } | ItemKind::Review { thread } => Some(thread),
            ItemKind::Terminal { session } => self.session_thread(session),
            _ => None,
        };
        let session = match item.kind {
            ItemKind::Terminal { session } => Some(session),
            ItemKind::Thread { thread } | ItemKind::Review { thread } => {
                self.thread_terminal(thread)
            }
            _ => None,
        };
        let thread_facts = thread.and_then(|t| self.thread_facts(t));
        for (key, value) in thread_facts.into_iter().flatten() {
            put(&format!("facts.{key}"), vec![value.clone()]);
        }
        let mut project = thread_facts.and_then(|f| f.get(fact::PROJECT)).cloned();
        if let Some(session) = session {
            let mirror = &self.projects.mirror;
            let board =
                mirror.of_orchestrator(session).or_else(|| Some(mirror.of_agent(session)?.0));
            project = board.map(|b| b.project.id.as_str().to_owned()).or(project);
            if let Some(summary) = self.summary(session) {
                put(fact::CWD, summary.cwd.clone().into_iter().collect());
                put(fact::BRANCH, summary.branch.clone().into_iter().collect());
                match (summary.repo.as_deref(), summary.cwd.as_deref()) {
                    (Some(root), _) => {
                        put(fact::REPO, repo_values(tile.worker, root, summary.repo_id.as_ref()));
                    }
                    (None, Some(cwd)) if self.names_a_folder(tile.worker, cwd) => {
                        put(fact::FOLDER, vec![groups::at(tile.worker, cwd)]);
                    }
                    (None, _) => {}
                }
            }
        }
        // A thread with no terminal here (a Codex, pi or ACP one) is where its row says.
        let placed = session.is_some_and(|s| self.summary(s).is_some());
        if let Some(place) = thread.filter(|_| !placed).and_then(|t| self.thread_place(t)) {
            self.put_thread_place(tile.worker, place, &mut put);
        }
        if let ItemKind::File { path } | ItemKind::Folder { path } = &item.kind {
            let path = self.expand_home(tile.worker, path);
            let file = matches!(item.kind, ItemKind::File { .. });
            put(fact::CWD, vec![path.clone()]);
            self.place_path(tile.worker, &path, file, &mut put);
        }
        put(fact::PROJECT, project.into_iter().collect());
        // What the person said of the item wins over everything above: a pin (`project`, the
        // key of the group it joins) stands in for every link of the default chain.
        for (key, value) in &item.facts {
            match (key.as_str(), GroupKey::parse(value)) {
                (fact::PROJECT, Some(pin)) => {
                    for link in groups::DEFAULT_CHAIN {
                        facts.remove(link);
                    }
                    facts.insert(pin.fact().to_owned(), vec![pin.value().to_owned()]);
                }
                _ => {
                    facts.insert(key.clone(), vec![value.clone()]);
                }
            }
        }
        facts
    }

    /// Where a thread works, as its facts: its directory, and its repository else its folder.
    fn put_thread_place(
        &self,
        worker: WorkerKey,
        place: &ThreadPlace,
        put: &mut impl FnMut(&str, Vec<String>),
    ) {
        put(fact::CWD, place.cwd.clone().into_iter().collect());
        match (place.repo.as_deref(), place.cwd.as_deref()) {
            (Some(root), _) => put(fact::REPO, repo_values(worker, root, place.repo_id.as_ref())),
            (None, Some(cwd)) if self.names_a_folder(worker, cwd) => {
                put(fact::FOLDER, vec![groups::at(worker, cwd)]);
            }
            (None, _) => {}
        }
    }

    /// What `thread` on `worker`, which has no tile here, is known by, as a tile of it would
    /// be: its machine, its agent, its open facts and where it works.
    pub(super) fn thread_listing_facts(&self, worker: WorkerKey, thread: ThreadId) -> Facts {
        let mut facts = Facts::new();
        let mut put = |key: &str, values: Vec<String>| {
            if !values.is_empty() {
                facts.insert(key.to_owned(), values);
            }
        };
        put(fact::MACHINE, vec![worker.to_string()]);
        put(fact::AGENT, self.thread_agent(thread).map(str::to_owned).into_iter().collect());
        let said = self.thread_facts(thread);
        for (key, value) in said.into_iter().flatten() {
            put(&format!("facts.{key}"), vec![value.clone()]);
        }
        put(fact::PROJECT, said.and_then(|f| f.get(fact::PROJECT)).cloned().into_iter().collect());
        if let Some(place) = self.thread_place(thread) {
            self.put_thread_place(worker, place, &mut put);
        }
        facts
    }

    /// Whether `cwd` on `worker` names a folder worth a group: not its home, not the root.
    fn names_a_folder(&self, worker: WorkerKey, cwd: &str) -> bool {
        let cwd = cwd.trim_end_matches('/');
        let home = self.home_of(worker).map(|h| h.trim_end_matches('/'));
        !cwd.is_empty() && cwd != "~" && Some(cwd) != home
    }

    /// `path` with a leading `~` spelled as `worker`'s home, once it has said.
    fn expand_home(&self, worker: WorkerKey, path: &str) -> String {
        match (path.strip_prefix('~'), self.home_of(worker)) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{rest}", home.trim_end_matches('/'))
            }
            _ => path.to_owned(),
        }
    }

    /// Where a file or a folder at `path` on `worker` is: the deepest repository of the
    /// worker's shells that holds it, else the deepest folder a shell there is in, else its own
    /// directory (a file's, or the folder itself).
    fn place_path(
        &self,
        worker: WorkerKey,
        path: &str,
        file: bool,
        put: &mut impl FnMut(&str, Vec<String>),
    ) {
        let Some(w) = self.workers.get(&worker) else { return };
        let repo = w
            .sessions
            .values()
            .filter_map(|s| Some((s.repo.as_deref()?, s.repo_id.as_ref())))
            .filter(|(root, _)| groups::within(root, path))
            .max_by_key(|(root, id)| (root.len(), id.is_some()));
        if let Some((root, id)) = repo {
            put(fact::REPO, repo_values(worker, root, id));
            return;
        }
        let folder = w
            .sessions
            .values()
            .filter_map(|s| s.cwd.as_deref())
            .filter(|cwd| self.names_a_folder(worker, cwd) && groups::within(cwd, path))
            .max_by_key(|cwd| cwd.len());
        let own = if file { path.rsplit_once('/').map(|(dir, _)| dir) } else { Some(path) };
        let folder = folder.or_else(|| own.filter(|dir| self.names_a_folder(worker, dir)));
        if let Some(folder) = folder {
            put(fact::FOLDER, vec![groups::at(worker, folder)]);
        }
    }

    /// What the server's declared projects claim: the clones of the repository each works in,
    /// by its origin and by its first commit.
    pub(super) fn claims(&self) -> Vec<Claim> {
        self.projects
            .mirror
            .boards()
            .filter_map(|board| {
                let repo = |value: String| -> Matcher { [(fact::REPO.to_owned(), value)].into() };
                let id = board.project.repo_id.as_ref();
                let clones = id.into_iter().flat_map(|id| {
                    id.origin
                        .iter()
                        .cloned()
                        .chain(id.root.iter().map(|r| format!("{}{r}", groups::COMMIT)))
                        .map(repo)
                });
                let matchers: Vec<Matcher> = clones.collect();
                let project = board.project.id.as_str().to_owned();
                (!matchers.is_empty()).then_some(Claim { project, matchers })
            })
            .collect()
    }

    /// `tiles` grouped by `chain`.
    pub(super) fn grouping(&self, tiles: Vec<TileRef>, chain: &[impl AsRef<str>]) -> Grouping {
        let mut kept = Vec::with_capacity(tiles.len());
        let mut facts = Vec::with_capacity(tiles.len());
        for tile in tiles {
            if let Some(item) = self.item(tile) {
                facts.push(self.tile_facts(tile, item));
                kept.push(tile);
            }
        }
        let refs: Vec<&Facts> = facts.iter().collect();
        let grouped = groups::group(&refs, chain, &self.claims());
        let at = kept.iter().enumerate().map(|(i, t)| (*t, i)).collect();
        Grouping { tiles: kept, facts, grouped, at }
    }

    /// Every tile of the layout, in reading order, grouped by project: what a workspace is
    /// named after, a tile from elsewhere is placed by and the palette goes to. Worked out once
    /// a frame while the window draws ([`FrameProjects`]), else each time asked.
    pub(super) fn project_groups(&self) -> Rc<Grouping> {
        let memo = &*self.frame_projects;
        if memo.live.get()
            && let Some(projects) = memo.projects.borrow().as_ref()
        {
            return Rc::clone(projects);
        }
        let projects = Rc::new(self.grouping(self.reading_order(), &groups::DEFAULT_CHAIN));
        if memo.live.get() {
            *memo.projects.borrow_mut() = Some(Rc::clone(&projects));
        }
        projects
    }

    /// What a person calls `group`: a machine by its worker's name, a declared project by its
    /// title, anything else as [`Group::name`] says.
    pub(super) fn group_name(&self, group: &Group) -> String {
        if let Some(worker) = group.key.worker() {
            return self.worker_name(worker);
        }
        if group.fact == fact::PROJECT {
            let board =
                self.projects.mirror.boards().find(|b| b.project.id.as_str() == group.key.value());
            if let Some(board) = board {
                return board.project.title.clone();
            }
        }
        group.name()
    }

    /// The machines a group spans as a quiet word, where more than one worker is known: one
    /// name, two names, else how many.
    pub(super) fn machines_word(&self, machines: &BTreeSet<WorkerKey>) -> Option<String> {
        if self.workers.len() < 2 {
            return None;
        }
        let names: Vec<String> = machines.iter().map(|w| self.worker_name(*w)).collect();
        match names.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            [a, b] => Some(format!("{a}, {b}")),
            more => Some(format!("{} machines", more.len())),
        }
    }

    /// Put `tile`, opened here, by the room rule beside the focused one in the project on
    /// show; with nothing on show, in a tab of its own project.
    pub(super) fn open_here(&mut self, tile: TileRef) {
        self.open_as(tile, super::tabs::Opening::Beside);
    }

    /// Each project takes the project its tiles turned out to share, once all of them share
    /// one other than its home: one first made at its machine's or its folder's, before what
    /// its work is was known, is named for its repository or its declared project then, and
    /// what arrives of it joins it ([`slopty_client::layout::Tiling::rehome`]). A project
    /// holding a machine's own work (a window, a note) beside a shell keeps its home. Whether
    /// one moved.
    pub(super) fn rehome_projects(&mut self) -> bool {
        let grouping = self.project_groups();
        let moves: Vec<(GroupKey, GroupKey)> = self
            .layout
            .projects()
            .iter()
            .filter_map(|p| {
                let mut keys = p
                    .tabs()
                    .iter()
                    .flat_map(slopty_client::layout::Tab::tiles)
                    .map(|t| grouping.group_of(t).map(|g| &g.key));
                let first = keys.next().flatten()?;
                let shared = first != p.home() && keys.all(|k| k == Some(first));
                shared.then(|| (p.home().clone(), first.clone()))
            })
            .collect();
        let mut moved = false;
        for (from, to) in moves {
            moved |= self.layout.rehome(&from, to);
        }
        moved
    }

    /// Put `arriving` (tiles from elsewhere: another client, an orchestrator, the worker's own
    /// list) in the tiling: a task's agent nowhere, a row until it is opened
    /// ([`super::seating`]), any other a background tab in its project
    /// ([`slopty_client::layout::Tiling::arrive`]), kept in mind in case the project names it a
    /// task's agent later. The projects are worked out once over the tiling and the arrivals
    /// together, so a snapshot of many lands in one pass, and the tiles already placed say
    /// their project first.
    pub(super) fn place_from_elsewhere(&mut self, arriving: &[TileRef]) {
        let mut tiles = self.reading_order();
        tiles.extend(arriving.iter().filter(|t| !self.layout.contains(**t)));
        let grouping = self.grouping(tiles, &groups::DEFAULT_CHAIN);
        let layout = &self.layout;
        self.projects.arrived.retain(|t| layout.contains(*t));
        for &tile in arriving {
            if self.layout.contains(tile) || self.is_task_agent(tile) {
                continue;
            }
            let home = grouping
                .group_of(tile)
                .map_or_else(|| GroupKey::machine(tile.worker), |g| g.key.clone());
            self.layout.arrive(tile, &home);
            self.projects.arrived.insert(tile);
        }
    }

    /// The project `tile` would be in, among every tile of the layout: what an arriving tile
    /// is placed by. A tile whose project is not known yet is its machine's.
    pub(super) fn home_for(&self, tile: TileRef) -> GroupKey {
        let mut tiles = self.reading_order();
        if !tiles.contains(&tile) {
            tiles.push(tile);
        }
        let grouping = self.grouping(tiles, &groups::DEFAULT_CHAIN);
        grouping.group_of(tile).map_or_else(|| GroupKey::machine(tile.worker), |g| g.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use slopty_client::groups::AT;

    use super::*;

    fn group(fact: &str, values: &[&str]) -> Group {
        let values: Vec<String> = values.iter().map(|v| (*v).to_owned()).collect();
        let key = GroupKey::new(fact, values.first().map_or("", String::as_str));
        Group { key, fact: fact.to_owned(), values, members: Vec::new() }
    }

    /// Two groups of one name say where each is: a repository its origin's owner, a folder
    /// its parent; a group of nothing says nothing.
    #[test]
    fn a_shared_name_says_where_each_is() {
        let fork = group(fact::REPO, &["github.com/someone/slopty", "at:1:/w/forks/slopty"]);
        assert_eq!(parent_of(&fork).as_deref(), Some("github.com/someone"));
        let clone = group(fact::REPO, &[&format!("{AT}1:/w/oss/slopty")]);
        assert_eq!(parent_of(&clone).as_deref(), Some("w/oss"));
        let notes = group(fact::FOLDER, &[&format!("{AT}1:/Users/c/notes")]);
        assert_eq!(parent_of(&notes).as_deref(), Some("Users/c"));
        assert_eq!(parent_of(&group("agent", &["codex"])), None);
        let tile = TileRef { worker: WorkerKey::new(1), item: slopty_core::ItemId::new() };
        assert_eq!(Grouping::default().group_of(tile), None, "a tile not grouped");
    }

    /// Every heading is sentence case, whatever fact it heads.
    #[test]
    fn section_names_are_sentence_case() {
        for fact in ["project", "repo", "agent", "branch", "kind", "os", "machine", "labels.team"] {
            let name = section_name(fact);
            assert!(name.chars().next().is_some_and(char::is_uppercase), "{name}");
        }
        assert_eq!(group_glyph("labels.team"), Mark::Symbol(Symbol::SquareGrid2x2));
    }
}
