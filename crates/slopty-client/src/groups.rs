//! Which tiles are one body of work: the grouping the navigator, the workspaces' names, the
//! breadcrumb and the palette read, and the home a tile arriving from elsewhere is placed by.
//!
//! Pure. The caller says what it knows of each tile as open [`Facts`] (a fact key and the
//! values it is known by), a chain of fact keys to group by, and the declared projects'
//! [`Claim`]s; [`group`] answers which group each tile is in. Nothing here is a closed list:
//! "by machine", "by agent" or "by branch" is a chain, and a key a worker or an agent reports
//! groups as well as the ones named in [`fact`].
//!
//! - A tile joins the group of the **first fact on the chain it has**, so the default chain
//!   ([`DEFAULT_CHAIN`]) gives a tile its declared project, else its repository, else its folder,
//!   else its machine. The machine is last, so nothing is homeless.
//! - **A fact's values are names of one thing.** Two tiles that share any value of the fact they
//!   group by are in one group, transitively: a repository known by its origin, its first commit
//!   and the places it is cloned joins every clone that shares one of them.
//! - **A value says what kind of name it is by its prefix.** Unprefixed is the strongest (an
//!   origin, a machine's key, a project's id); [`COMMIT`] is a first commit; [`AT`] is a place on
//!   one machine (`at:<machine>:<path>`), which names nothing elsewhere. A group is keyed by its
//!   strongest least value, so its key holds as clones come and go, and does not depend on the
//!   order the tiles come in.
//! - **A declared project claims by matchers** ([`Matcher`]): an open map of fact key to a value,
//!   or for a path a directory the value is in. A tile with no `project` fact of its own that
//!   matches one takes the claim's project.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::layout::WorkerKey;

/// What is known of one tile: each fact key with the values it is known by, strongest first.
pub type Facts = BTreeMap<String, Vec<String>>;

/// One way a declared project claims tiles: every key must match ([`matches()`]).
pub type Matcher = BTreeMap<String, String>;

/// The fact keys the client fills in. Any other key a caller reports groups the same way.
pub mod fact {
    /// The project a tile belongs to: a person's pin, a declared project's claim, or the
    /// project its agent's thread says it works for.
    pub const PROJECT: &str = "project";
    /// The repository a tile is in: its origin, its first commit and where it is cloned.
    pub const REPO: &str = "repo";
    /// The folder a tile is in, outside any repository: a place on one machine.
    pub const FOLDER: &str = "folder";
    /// The worker a tile runs on, by its key.
    pub const MACHINE: &str = "machine";
    /// The agent at work there, by its agent id.
    pub const AGENT: &str = "agent";
    /// The branch its repository has checked out.
    pub const BRANCH: &str = "branch";
    /// What the tile shows: `terminal`, `window`, `note`…
    pub const KIND: &str = "kind";
    /// The directory it works in, as its worker spells it.
    pub const CWD: &str = "cwd";
    /// The operating system of its worker.
    pub const OS: &str = "os";
}

/// The chain a tile is grouped by unless the person picks another: its project, else its
/// repository, else its folder, else its machine.
pub const DEFAULT_CHAIN: [&str; 4] = [fact::PROJECT, fact::REPO, fact::FOLDER, fact::MACHINE];

/// The prefix of a value that is a repository's first commit.
pub const COMMIT: &str = "commit:";

/// The prefix of a value that is a place on one machine: `at:<machine>:<path>`.
pub const AT: &str = "at:";

/// `path` on `machine`, as a fact value.
#[must_use]
pub fn at(machine: WorkerKey, path: &str) -> String {
    format!("{AT}{machine}:{}", trim_dir(path))
}

/// The machine and path of a value made by [`at`].
#[must_use]
pub fn place(value: &str) -> Option<(WorkerKey, &str)> {
    let (machine, path) = value.strip_prefix(AT)?.split_once(':')?;
    let key = u128::from_str_radix(machine, 16).ok()?;
    Some((WorkerKey::new(key), path))
}

/// How strong a name `value` is: an unprefixed one names the thing everywhere, a first commit
/// nearly so, a place only on one machine.
fn strength(value: &str) -> u8 {
    if value.starts_with(AT) { 2 } else { u8::from(value.starts_with(COMMIT)) }
}

/// A path without its trailing slash, the root kept.
fn trim_dir(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && path.starts_with('/') { "/" } else { trimmed }
}

/// A group's identity: `<fact>:<value>`, opaque to the layout, which places an arriving tile
/// by it ([`crate::layout::Tiling::arrive`]).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupKey(String);

impl GroupKey {
    /// The key of the group of `fact` known by `value`.
    #[must_use]
    pub fn new(fact: &str, value: &str) -> Self {
        Self(format!("{fact}:{value}"))
    }

    /// A key as [`Self::as_str`] spells it: a fact, a colon and a value, neither empty.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        let (fact, value) = key.split_once(':')?;
        (!fact.is_empty() && !value.is_empty()).then(|| Self::new(fact, value))
    }

    /// The group of the tiles on `worker` that belong to nothing else.
    #[must_use]
    pub fn machine(worker: WorkerKey) -> Self {
        Self::new(fact::MACHINE, &worker.to_string())
    }

    /// The fact it groups by.
    #[must_use]
    pub fn fact(&self) -> &str {
        self.0.split_once(':').map_or(self.0.as_str(), |(fact, _)| fact)
    }

    /// The value it is keyed by.
    #[must_use]
    pub fn value(&self) -> &str {
        self.0.split_once(':').map_or("", |(_, value)| value)
    }

    /// The worker of a machine's group.
    #[must_use]
    pub fn worker(&self) -> Option<WorkerKey> {
        (self.fact() == fact::MACHINE)
            .then(|| u128::from_str_radix(self.value(), 16).ok().map(WorkerKey::new))
            .flatten()
    }

    /// The whole key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GroupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GroupKey({})", self.0)
    }
}

/// A declared project's claim on tiles: its id, and the matchers any of which claims a tile.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Claim {
    /// The project, as its `project` fact names it.
    pub project: String,
    /// Any of these claims a tile.
    pub matchers: Vec<Matcher>,
}

/// Whether `facts` match `matcher`.
///
/// For every key, one of the tile's values is the matcher's, or, for a path (one starting with
/// `/` or `~`), is in the matcher's directory. An empty matcher matches nothing, so a claim
/// never takes every tile by mistake.
#[must_use]
pub fn matches(facts: &Facts, matcher: &Matcher) -> bool {
    !matcher.is_empty()
        && matcher.iter().all(|(key, want)| {
            facts.get(key).is_some_and(|values| values.iter().any(|v| value_matches(v, want)))
        })
}

fn value_matches(value: &str, want: &str) -> bool {
    if value == want {
        return true;
    }
    let path = want.starts_with('/') || want.starts_with('~');
    path && within(trim_dir(want), value)
}

/// Whether `path` is `dir` or below it, on a directory boundary.
#[must_use]
pub fn within(dir: &str, path: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    path.strip_prefix(dir).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// One group: the tiles that are one body of work under the fact it was found by.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Group {
    /// What it is kept by (folded, placed, ranked).
    pub key: GroupKey,
    /// The fact on the chain its tiles were grouped by.
    pub fact: String,
    /// Every value its tiles are known by under that fact, sorted, once each.
    pub values: Vec<String>,
    /// Its tiles, by their index in what [`group`] was given, in that order.
    pub members: Vec<usize>,
}

impl Group {
    /// What a person would call it: a repository by its origin's last part, else its
    /// directory's name; a folder by its name; anything else by its value. The caller names
    /// a machine and a declared project, which it knows by more than their keys.
    #[must_use]
    pub fn name(&self) -> String {
        let origin = self.values.iter().filter(|v| strength(v) == 0).min();
        if let Some(origin) = origin.filter(|_| self.fact == fact::REPO) {
            return last_part(origin).to_owned();
        }
        if let Some((_, path)) = self.places().next() {
            return dir_name(path).to_owned();
        }
        origin.or_else(|| self.values.first()).cloned().unwrap_or_default()
    }

    /// The places its tiles are known by ([`at`]), in order.
    pub fn places(&self) -> impl Iterator<Item = (WorkerKey, &str)> {
        self.values.iter().filter_map(|v| place(v))
    }

    /// Its strongest name that is not a place: a repository's least origin, else its first
    /// commit; `None` for a folder.
    #[must_use]
    pub fn identity(&self) -> Option<&str> {
        self.values
            .iter()
            .filter(|v| strength(v) < 2)
            .min_by_key(|v| (strength(v), v.as_str()))
            .map(String::as_str)
    }
}

/// The groups [`group`] found, in key order, and the group of each tile: `of[i]` indexes
/// `groups` for tile `i`, `None` for a tile with no fact on the chain.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Grouped {
    /// The groups.
    pub groups: Vec<Group>,
    /// Each tile's group, by its index.
    pub of: Vec<Option<usize>>,
}

/// Group `tiles` by the first fact of `chain` each has, a declared project's `claims` lending
/// a `project` to the tiles it matches ([module docs](self)).
#[must_use]
pub fn group(tiles: &[&Facts], chain: &[impl AsRef<str>], claims: &[Claim]) -> Grouped {
    // A claim lends its project to a tile that has none of its own.
    let claimed: Vec<Option<&str>> = tiles
        .iter()
        .map(|facts| {
            if facts.get(fact::PROJECT).is_some_and(|v| !v.is_empty()) {
                return None;
            }
            claims
                .iter()
                .find(|claim| claim.matchers.iter().any(|m| matches(facts, m)))
                .map(|claim| claim.project.as_str())
        })
        .collect();
    let values_of = |i: usize, key: &str| -> Vec<&str> {
        let own = tiles.get(i).and_then(|facts| facts.get(key));
        match own.filter(|v| !v.is_empty()) {
            Some(values) => values.iter().map(String::as_str).collect(),
            None if key == fact::PROJECT => claimed.get(i).copied().flatten().into_iter().collect(),
            None => Vec::new(),
        }
    };
    // Each tile's link on the chain, and the values it is known by there.
    let mut linked: Vec<Option<(usize, Vec<&str>)>> = Vec::with_capacity(tiles.len());
    for i in 0..tiles.len() {
        let found = chain.iter().enumerate().find_map(|(link, key)| {
            let values = values_of(i, key.as_ref());
            (!values.is_empty()).then_some((link, values))
        });
        linked.push(found);
    }
    let mut sets = Sets::new(tiles.len());
    let mut first: HashMap<(usize, &str), usize> = HashMap::new();
    for (i, found) in linked.iter().enumerate() {
        let Some((link, values)) = found else { continue };
        for value in values {
            let seen = *first.entry((*link, value)).or_insert(i);
            sets.join(seen, i);
        }
    }
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, found) in linked.iter().enumerate() {
        if found.is_some() {
            members.entry(sets.find(i)).or_default().push(i);
        }
    }
    let mut groups: Vec<Group> = members
        .into_values()
        .filter_map(|ixs| {
            let (link, _) = linked.get(*ixs.first()?)?.as_ref()?;
            let fact = chain.get(*link)?.as_ref().to_owned();
            let values: BTreeSet<&str> = ixs
                .iter()
                .filter_map(|&i| linked.get(i)?.as_ref())
                .flat_map(|(_, values)| values.iter().copied())
                .collect();
            let least = values.iter().min_by_key(|v| (strength(v), **v))?;
            let key = GroupKey::new(&fact, least);
            let values = values.into_iter().map(str::to_owned).collect();
            Some(Group { key, fact, values, members: ixs })
        })
        .collect();
    groups.sort_by(|a, b| a.key.cmp(&b.key));
    let mut of = vec![None; tiles.len()];
    for (g, group) in groups.iter().enumerate() {
        for &i in &group.members {
            if let Some(slot) = of.get_mut(i) {
                *slot = Some(g);
            }
        }
    }
    Grouped { groups, of }
}

/// A path's last directory.
fn dir_name(path: &str) -> &str {
    let path = trim_dir(path);
    path.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(path)
}

/// An origin's last part: `slopty` of `github.com/aislopware/slopty`.
fn last_part(origin: &str) -> &str {
    origin.rsplit('/').next().unwrap_or(origin)
}

/// Disjoint sets over `0..n`, joined by union by size with path halving.
struct Sets {
    parent: Vec<usize>,
    size: Vec<usize>,
}

impl Sets {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), size: vec![1; n] }
    }

    fn find(&mut self, mut i: usize) -> usize {
        while let Some(&up) = self.parent.get(i)
            && up != i
        {
            let grand = self.parent.get(up).copied().unwrap_or(up);
            if let Some(slot) = self.parent.get_mut(i) {
                *slot = grand;
            }
            i = grand;
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return;
        }
        let size = |s: &Self, i: usize| s.size.get(i).copied().unwrap_or(1);
        let (big, small) = if size(self, a) >= size(self, b) { (a, b) } else { (b, a) };
        if let Some(slot) = self.parent.get_mut(small) {
            *slot = big;
        }
        let both = size(self, big).saturating_add(size(self, small));
        if let Some(slot) = self.size.get_mut(big) {
            *slot = both;
        }
    }
}

/// How often and how lately each group was gone to on this device, for the palette to rank
/// them by.
///
/// It ranks as zoxide ranks directories: a visit adds one, and a score counts four times within
/// the hour, twice within the day, half within the week and a quarter after. Once the counts
/// add up past [`Frecency::MAX_AGE`] they are all scaled down and the faint ones forgotten, so
/// what is no longer gone to fades out.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Frecency {
    /// Every group gone to, with its count and when it was last gone to.
    visits: Vec<Visit>,
}

/// One group's visits.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
struct Visit {
    key: GroupKey,
    count: f64,
    last_ms: u64,
}

impl Frecency {
    /// The total count past which every count is scaled down (zoxide's `_ZO_MAXAGE`).
    pub const MAX_AGE: f64 = 10_000.0;

    /// `key` was gone to at `now_ms` (Unix milliseconds).
    pub fn visit(&mut self, key: &GroupKey, now_ms: u64) {
        match self.visits.iter_mut().find(|v| v.key == *key) {
            Some(visit) => {
                visit.count += 1.0;
                visit.last_ms = visit.last_ms.max(now_ms);
            }
            None => self.visits.push(Visit { key: key.clone(), count: 1.0, last_ms: now_ms }),
        }
        let total: f64 = self.visits.iter().map(|v| v.count).sum();
        if total > Self::MAX_AGE {
            let scale = 0.9 * Self::MAX_AGE / total;
            for visit in &mut self.visits {
                visit.count *= scale;
            }
            self.visits.retain(|v| v.count >= 1.0);
        }
    }

    /// How highly `key` ranks at `now_ms`; zero for a group never gone to.
    #[must_use]
    pub fn score(&self, key: &GroupKey, now_ms: u64) -> f64 {
        const HOUR: u64 = 3_600_000;
        let Some(visit) = self.visits.iter().find(|v| v.key == *key) else { return 0.0 };
        let age = now_ms.saturating_sub(visit.last_ms);
        let weight = if age < HOUR {
            4.0
        } else if age < HOUR.saturating_mul(24) {
            2.0
        } else if age < HOUR.saturating_mul(24 * 7) {
            0.5
        } else {
            0.25
        };
        visit.count * weight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e";
    const STUDIO: WorkerKey = WorkerKey::new(1);
    const DEVBOX: WorkerKey = WorkerKey::new(2);

    fn facts(pairs: &[(&str, &[&str])]) -> Facts {
        pairs
            .iter()
            .map(|(k, vs)| ((*k).to_owned(), vs.iter().map(|v| (*v).to_owned()).collect()))
            .collect()
    }

    fn machine(worker: WorkerKey) -> String {
        worker.to_string()
    }

    /// A shell in a clone: its origin and first commit when known, its place, its machine.
    fn clone(worker: WorkerKey, path: &str, origin: Option<&str>, root: Option<&str>) -> Facts {
        let mut repo: Vec<String> = origin.map(str::to_owned).into_iter().collect();
        repo.extend(root.map(|r| format!("{COMMIT}{r}")));
        repo.push(at(worker, path));
        let mut out = facts(&[(fact::MACHINE, &[&machine(worker)])]);
        out.insert(fact::REPO.to_owned(), repo);
        out
    }

    fn by_default(tiles: &[Facts]) -> Grouped {
        let refs: Vec<&Facts> = tiles.iter().collect();
        group(&refs, &DEFAULT_CHAIN, &[])
    }

    fn keys(grouped: &Grouped) -> Vec<&str> {
        grouped.groups.iter().map(|g| g.key.as_str()).collect()
    }

    /// A tile goes by the first fact of the chain it has: a project before its repository, a
    /// repository before its folder, the machine last.
    #[test]
    fn a_tile_takes_the_first_fact_on_the_chain() {
        let mut agent = clone(STUDIO, "/w/slopty", Some("github.com/o/slopty"), None);
        agent.insert(fact::PROJECT.to_owned(), vec!["parser".to_owned()]);
        let shell = clone(STUDIO, "/w/slopty", Some("github.com/o/slopty"), None);
        let mut logs = facts(&[(fact::MACHINE, &[&machine(STUDIO)])]);
        logs.insert(fact::FOLDER.to_owned(), vec![at(STUDIO, "/var/log")]);
        let home = facts(&[(fact::MACHINE, &[&machine(STUDIO)])]);
        let grouped = by_default(&[agent, shell, logs, home]);
        let of: Vec<&str> = grouped
            .of
            .iter()
            .map(|g| g.and_then(|g| grouped.groups.get(g)).map_or("", |g| g.key.as_str()))
            .collect();
        assert_eq!(
            of,
            [
                "project:parser",
                "repo:github.com/o/slopty",
                &*format!("folder:{}", at(STUDIO, "/var/log")),
                &*format!("machine:{STUDIO}"),
            ]
        );
        let names: Vec<String> = grouped.groups.iter().map(Group::name).collect();
        assert!(names.contains(&"slopty".to_owned()) && names.contains(&"log".to_owned()));
    }

    /// One repository cloned on two machines at two paths is one project, keyed by its origin;
    /// a clone known only by its first commit joins through that, and one whose identity has
    /// not come yet joins the clone at its place on the same machine.
    #[test]
    fn clones_on_two_machines_are_one_project() {
        let tiles = [
            clone(DEVBOX, "/home/c/src/slopty", None, Some(ROOT)),
            clone(STUDIO, "/w/slopty", Some("github.com/aislopware/slopty"), Some(ROOT)),
            clone(STUDIO, "/w/slopty/", None, None),
            clone(STUDIO, "/w/notes", Some("github.com/c/notes"), None),
        ];
        let grouped = by_default(&tiles);
        assert_eq!(
            keys(&grouped),
            ["repo:github.com/aislopware/slopty", "repo:github.com/c/notes"]
        );
        assert_eq!(grouped.of, [Some(0), Some(0), Some(0), Some(1)]);
        let slopty = grouped.groups.first().expect("a group");
        assert_eq!(slopty.name(), "slopty");
        assert_eq!(slopty.identity(), Some("github.com/aislopware/slopty"));
        let places: Vec<(WorkerKey, &str)> = slopty.places().collect();
        assert!(places.contains(&(DEVBOX, "/home/c/src/slopty")), "{places:?}");
        assert!(places.contains(&(STUDIO, "/w/slopty")), "{places:?}");
    }

    /// A folder of one name on two machines is two groups until the person says they are one
    /// (a claim naming each); the same name is not the same thing.
    #[test]
    fn two_folders_of_one_name_on_two_machines_stay_two_until_named() {
        let notes = |worker: WorkerKey| {
            let mut f =
                facts(&[(fact::MACHINE, &[&machine(worker)]), (fact::CWD, &["/Users/c/notes"])]);
            f.insert(fact::FOLDER.to_owned(), vec![at(worker, "/Users/c/notes")]);
            f
        };
        let tiles = [notes(STUDIO), notes(DEVBOX)];
        let grouped = by_default(&tiles);
        assert_eq!(grouped.groups.len(), 2, "{grouped:?}");
        assert!(grouped.groups.iter().all(|g| g.name() == "notes"));

        let refs: Vec<&Facts> = tiles.iter().collect();
        let matcher = |worker: WorkerKey| -> Matcher {
            [(fact::MACHINE, machine(worker)), (fact::CWD, "/Users/c/notes".to_owned())]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect()
        };
        let claim =
            Claim { project: "notes".to_owned(), matchers: vec![matcher(STUDIO), matcher(DEVBOX)] };
        let named = group(&refs, &DEFAULT_CHAIN, &[claim]);
        assert_eq!(keys(&named), ["project:notes"]);
        assert_eq!(named.of, [Some(0), Some(0)]);
    }

    /// A window with no place of its own joins its project once pinned there.
    #[test]
    fn a_pinned_window_joins_its_project() {
        let shell = {
            let mut f = clone(STUDIO, "/w/ios", Some("github.com/o/ios"), None);
            f.insert(fact::PROJECT.to_owned(), vec!["ios".to_owned()]);
            f
        };
        let window = facts(&[
            (fact::MACHINE, &[&machine(DEVBOX)]),
            (fact::KIND, &["window"]),
            (fact::PROJECT, &["ios"]),
        ]);
        let grouped = by_default(&[shell, window]);
        assert_eq!(keys(&grouped), ["project:ios"]);
        assert_eq!(grouped.of, [Some(0), Some(0)]);
    }

    /// A desktop nobody pinned has no folder: it sits under its machine.
    #[test]
    fn a_desktop_with_no_pin_sits_under_its_machine() {
        let display = facts(&[(fact::MACHINE, &[&machine(STUDIO)]), (fact::KIND, &["display"])]);
        let grouped = by_default(&[display]);
        assert_eq!(keys(&grouped), [&*format!("machine:{STUDIO}")]);
        let key = &grouped.groups.first().expect("a group").key;
        assert_eq!(key.worker(), Some(STUDIO));
        assert_eq!(GroupKey::machine(STUDIO), *key);
    }

    /// A thread whose orchestrator said its project joins that project, wherever it runs.
    #[test]
    fn a_thread_with_a_project_fact_joins_that_project() {
        let thread = facts(&[
            (fact::MACHINE, &[&machine(DEVBOX)]),
            (fact::KIND, &["thread"]),
            (fact::PROJECT, &["parser"]),
            ("facts.task", &["3"]),
        ]);
        let orchestrator = {
            let mut f = clone(STUDIO, "/w/parser", Some("github.com/o/parser"), None);
            f.insert(fact::PROJECT.to_owned(), vec!["parser".to_owned()]);
            f
        };
        let grouped = by_default(&[thread, orchestrator]);
        assert_eq!(keys(&grouped), ["project:parser"]);
        assert_eq!(grouped.groups.first().map(|g| g.members.len()), Some(2));
    }

    /// A declared project of an app and its API claims both repositories, by origin and by a
    /// directory one is cloned in; a tile already pinned elsewhere keeps its pin.
    #[test]
    fn a_declared_project_claims_two_repositories() {
        let app = clone(STUDIO, "/w/app", Some("github.com/o/app"), None);
        let api = {
            let mut f = clone(DEVBOX, "/srv/api", None, Some(ROOT));
            f.insert(fact::CWD.to_owned(), vec!["/srv/api/src".to_owned()]);
            f
        };
        let pinned = {
            let mut f = clone(STUDIO, "/w/app", Some("github.com/o/app"), None);
            f.insert(fact::PROJECT.to_owned(), vec!["other".to_owned()]);
            f
        };
        let unrelated = clone(STUDIO, "/w/site", Some("github.com/o/site"), None);
        let matcher = |k: &str, v: &str| -> Matcher { [(k.to_owned(), v.to_owned())].into() };
        let claim = Claim {
            project: "shop".to_owned(),
            matchers: vec![matcher(fact::REPO, "github.com/o/app"), matcher(fact::CWD, "/srv/api")],
        };
        let tiles = [app, api, pinned, unrelated];
        let refs: Vec<&Facts> = tiles.iter().collect();
        let grouped = group(&refs, &DEFAULT_CHAIN, &[claim]);
        assert_eq!(keys(&grouped), ["project:other", "project:shop", "repo:github.com/o/site"]);
        assert_eq!(grouped.of, [Some(1), Some(1), Some(0), Some(2)]);
        assert!(!matches(&tiles[3], &Matcher::new()), "an empty matcher claims nothing");
        assert!(!matches(&tiles[1], &matcher(fact::CWD, "/srv/ap")), "on a directory boundary");
    }

    /// Any fact groups: by agent, the tiles with none fall to their machine; with nothing on
    /// the chain at all a tile is in no group.
    #[test]
    fn any_fact_is_a_grouping() {
        let codex = facts(&[(fact::MACHINE, &[&machine(STUDIO)]), (fact::AGENT, &["codex"])]);
        let claude =
            facts(&[(fact::MACHINE, &[&machine(DEVBOX)]), (fact::AGENT, &["claude-code"])]);
        let shell = facts(&[(fact::MACHINE, &[&machine(DEVBOX)])]);
        let tiles = [&codex, &claude, &shell];
        let grouped = group(&tiles, &[fact::AGENT, fact::MACHINE], &[]);
        assert_eq!(
            keys(&grouped),
            ["agent:claude-code", "agent:codex", &*format!("machine:{DEVBOX}")]
        );
        let alone = group(&tiles, &["labels.team"], &[]);
        assert_eq!(alone, Grouped { groups: Vec::new(), of: vec![None, None, None] });
        assert_eq!(group(&[], &DEFAULT_CHAIN, &[]), Grouped::default());
    }

    /// Whatever order the tiles come in, each lands in a group of the same key with the same
    /// company: every order of six tiles that merge through three kinds of name.
    #[test]
    fn group_keys_do_not_depend_on_tile_order() {
        let tiles = [
            clone(STUDIO, "/w/slopty", Some("github.com/z/slopty"), Some(ROOT)),
            clone(DEVBOX, "/src/slopty", Some("github.com/a/slopty"), None),
            clone(DEVBOX, "/src/slopty", None, Some(ROOT)),
            clone(STUDIO, "/w/x", None, None),
            facts(&[(fact::MACHINE, &[&machine(STUDIO)])]),
            facts(&[(fact::MACHINE, &[&machine(STUDIO)]), (fact::PROJECT, &["p"])]),
        ];
        let truth = by_default(&tiles);
        let key_of = |grouped: &Grouped, i: usize| {
            grouped
                .of
                .get(i)
                .copied()
                .flatten()
                .and_then(|g| grouped.groups.get(g))
                .map(|g| g.key.clone())
        };
        let want: Vec<Option<GroupKey>> = (0..tiles.len()).map(|i| key_of(&truth, i)).collect();
        assert_eq!(
            want.first().cloned().flatten().map(|k| k.to_string()).as_deref(),
            Some("repo:github.com/a/slopty"),
            "the least origin of the merged clones"
        );
        let mut order: Vec<usize> = (0..tiles.len()).collect();
        let mut seen = 0_usize;
        permute(&mut order, 0, &mut |order| {
            let shuffled: Vec<&Facts> = order.iter().map(|&i| &tiles[i]).collect();
            let grouped = group(&shuffled, &DEFAULT_CHAIN, &[]);
            for (at, &i) in order.iter().enumerate() {
                assert_eq!(key_of(&grouped, at), want[i], "{order:?}");
            }
            assert_eq!(keys(&grouped), keys(&truth), "{order:?}");
            seen += 1;
        });
        assert_eq!(seen, 720, "every order of six");
    }

    fn permute(items: &mut Vec<usize>, k: usize, visit: &mut impl FnMut(&[usize])) {
        if k == items.len() {
            visit(items);
            return;
        }
        for i in k..items.len() {
            items.swap(k, i);
            permute(items, k.saturating_add(1), visit);
            items.swap(k, i);
        }
    }

    /// A place names its machine and path and nothing else parses as one; a key splits into
    /// its fact and value.
    #[test]
    fn a_place_is_a_machine_and_a_path() {
        let value = at(DEVBOX, "/w/a:b/");
        assert_eq!(place(&value), Some((DEVBOX, "/w/a:b")));
        assert_eq!(place("github.com/o/r"), None);
        assert_eq!(place("at:nothex:/w"), None);
        assert_eq!(at(STUDIO, "/"), format!("at:{STUDIO}:/"));
        let key = GroupKey::new(fact::REPO, "github.com/o/r");
        assert_eq!((key.fact(), key.value()), ("repo", "github.com/o/r"));
        assert_eq!(key.worker(), None);
        assert_eq!(GroupKey::parse(key.as_str()), Some(key));
        let folder = GroupKey::new(fact::FOLDER, &at(STUDIO, "/w/notes"));
        assert_eq!(GroupKey::parse(folder.as_str()), Some(folder), "a key whose value has colons");
        assert_eq!(GroupKey::parse("atlas"), None);
        assert_eq!(GroupKey::parse(":atlas"), None);
    }

    /// A group gone to often and lately ranks first; one gone to long ago fades, and past the
    /// total's ceiling the faint ones are forgotten.
    #[test]
    fn projects_rank_by_frecency_and_age_out() {
        const HOUR: u64 = 3_600_000;
        let (a, b, c) =
            (GroupKey::new("repo", "a"), GroupKey::new("repo", "b"), GroupKey::new("repo", "c"));
        let now = 1_000 * HOUR;
        let mut ranks = Frecency::default();
        for _ in 0..3 {
            ranks.visit(&a, now - 30 * 24 * HOUR);
        }
        ranks.visit(&b, now - HOUR / 2);
        assert!(ranks.score(&b, now) > ranks.score(&a, now), "lately beats often, long ago");
        assert!((ranks.score(&c, now)).abs() < f64::EPSILON, "never gone to");
        ranks.visit(&a, now);
        assert!(ranks.score(&a, now) > ranks.score(&b, now), "often and lately beats lately");

        let mut full = Frecency::default();
        for _ in 0..10_000 {
            full.visit(&a, now);
        }
        full.visit(&c, now);
        assert!(full.score(&a, now) > 0.0, "the one gone to most stays");
        assert!(
            full.score(&c, now).abs() < f64::EPSILON,
            "past the ceiling the faint are forgotten"
        );
    }
}
