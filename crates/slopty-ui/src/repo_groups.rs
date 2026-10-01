//! Which clones the navigator's repository lens lists as one repository.
//!
//! A path names a clone, not a repository: `/w/slopty` on one worker and `/home/c/slopty` on
//! another are one repository when their identities match ([`RepoId::same`]: the same
//! normalized origin or the same first commit). Clones at the same path are one repository too,
//! as before identities were known, so a clone whose identity has not come in yet stays with
//! the others at its path. Matching is transitive: a clone that shares its origin with one and
//! its path with another joins all three.

use std::collections::HashMap;

use slopty_proto::terminal::RepoId;

/// A clone some tile is in: where it is, and which repository it is when known.
#[derive(Clone, Copy, Debug)]
pub struct Clone<'a> {
    /// The clone's root on its worker.
    pub path: &'a str,
    /// Its identity, once the worker has it.
    pub id: Option<&'a RepoId>,
}

/// One repository of the lens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepoGroup {
    /// What it is kept by (folded, picked): its origin when a clone of it has one, else its
    /// first commit, else its path. The least of each, so it does not depend on order.
    pub key: String,
    /// What its header says: the origin's last part, else the directory's name.
    pub name: String,
    /// Every path it is cloned at, in order, once each.
    pub paths: Vec<String>,
}

/// The repositories `clones` are in, in name order, and which one each clone joined:
/// `of[i]` indexes `groups` for `clones[i]`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Grouped {
    /// The repositories.
    pub groups: Vec<RepoGroup>,
    /// The group of each clone, by its index.
    pub of: Vec<usize>,
}

/// Group `clones` into repositories ([module docs](self)).
#[must_use]
pub fn group(clones: &[Clone<'_>]) -> Grouped {
    let mut sets = Sets::new(clones.len());
    // The first clone seen under each key; a later one with that key joins it.
    let mut first: HashMap<(u8, &str), usize> = HashMap::new();
    for (i, clone) in clones.iter().enumerate() {
        let id = clone.id.into_iter();
        let keys = id
            .flat_map(|id| {
                let origin = id.origin.as_deref().map(|o| (0, o));
                origin.into_iter().chain(id.root.as_deref().map(|r| (1, r)))
            })
            .chain([(2, clone.path)]);
        for key in keys {
            let seen = *first.entry(key).or_insert(i);
            sets.join(seen, i);
        }
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..clones.len() {
        members.entry(sets.find(i)).or_default().push(i);
    }
    let mut groups: Vec<(RepoGroup, Vec<usize>)> = members
        .into_values()
        .map(|ixs| {
            let ids = ixs.iter().filter_map(|&i| clones.get(i)?.id);
            let origin = ids.clone().filter_map(|id| id.origin.as_deref()).min();
            let root = ids.filter_map(|id| id.root.as_deref()).min();
            let mut paths: Vec<String> =
                ixs.iter().filter_map(|&i| Some(clones.get(i)?.path.to_owned())).collect();
            paths.sort();
            paths.dedup();
            let least_path = paths.first().map_or("", String::as_str);
            let name = origin.map_or_else(|| dir_name(least_path), last_part).to_owned();
            let key = origin.or(root).unwrap_or(least_path).to_owned();
            (RepoGroup { key, name, paths }, ixs)
        })
        .collect();
    groups.sort_by(|(a, _), (b, _)| {
        a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.key.cmp(&b.key))
    });

    let mut of = vec![0; clones.len()];
    for (g, (_, ixs)) in groups.iter().enumerate() {
        for &i in ixs {
            if let Some(slot) = of.get_mut(i) {
                *slot = g;
            }
        }
    }
    Grouped { groups: groups.into_iter().map(|(g, _)| g).collect(), of }
}

/// A path's last directory.
fn dir_name(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path)
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

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "c08d4c1e5b2a9f7d3e6a1b8c4d2f0e9a7b5c3d1e";

    fn id(origin: Option<&str>, root: Option<&str>) -> RepoId {
        RepoId { origin: origin.map(str::to_owned), root: root.map(str::to_owned) }
    }

    fn names(grouped: &Grouped) -> Vec<&str> {
        grouped.groups.iter().map(|g| g.name.as_str()).collect()
    }

    /// One repository cloned on two workers at two paths is one group, keyed and named by its
    /// origin; one known only by its first commit joins it through that.
    #[test]
    fn clones_of_one_repository_on_two_workers_are_one() {
        let studio = id(Some("github.com/aislopware/slopty"), Some(ROOT));
        let linux = id(None, Some(ROOT));
        let notes = id(Some("github.com/c/notes"), None);
        let clones = [
            Clone { path: "/home/c/src/slopty-main", id: Some(&linux) },
            Clone { path: "/w/notes", id: Some(&notes) },
            Clone { path: "/w/slopty", id: Some(&studio) },
        ];
        let grouped = group(&clones);
        assert_eq!(names(&grouped), ["notes", "slopty"]);
        let slopty = grouped.groups.get(1).expect("two groups");
        assert_eq!(slopty.key, "github.com/aislopware/slopty");
        assert_eq!(slopty.paths, ["/home/c/src/slopty-main", "/w/slopty"]);
        assert_eq!(grouped.of, [1, 0, 1]);
    }

    /// Clones at one path stay together while one's identity is still unknown, and that joins
    /// the clones its identity matches elsewhere too.
    #[test]
    fn a_clone_not_yet_identified_stays_with_its_path() {
        let known = id(Some("github.com/o/r"), None);
        let clones = [
            Clone { path: "/w/r", id: None },
            Clone { path: "/srv/r", id: Some(&known) },
            Clone { path: "/w/r", id: Some(&known) },
        ];
        let grouped = group(&clones);
        assert_eq!(grouped.groups.len(), 1, "{grouped:?}");
        assert_eq!(grouped.of, [0, 0, 0]);
        assert_eq!(grouped.groups.first().map(|g| g.paths.len()), Some(2));
    }

    /// Two repositories with one name are two groups, in key order; with no identity a group
    /// is keyed by its path and named by its directory; nothing in, nothing out.
    #[test]
    fn different_repositories_stay_apart() {
        let a = id(Some("github.com/a/app"), None);
        let b = id(Some("gitlab.com/b/app"), None);
        let clones = [
            Clone { path: "/w/b/app", id: Some(&b) },
            Clone { path: "/w/a/app", id: Some(&a) },
            Clone { path: "/w/tools/", id: None },
        ];
        let grouped = group(&clones);
        let keys: Vec<&str> = grouped.groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["github.com/a/app", "gitlab.com/b/app", "/w/tools/"]);
        assert_eq!(names(&grouped), ["app", "app", "tools"]);
        assert_eq!(grouped.of, [1, 0, 2]);
        assert_eq!(group(&[]), Grouped::default());
    }

    /// The key is the least origin of the group whatever order the clones come in, so a fold
    /// kept under it holds as tiles move.
    #[test]
    fn the_key_does_not_depend_on_order() {
        let (x, y) =
            (id(Some("github.com/z/r"), Some(ROOT)), id(Some("github.com/a/r"), Some(ROOT)));
        let forward = [Clone { path: "/1", id: Some(&x) }, Clone { path: "/2", id: Some(&y) }];
        let backward = [Clone { path: "/2", id: Some(&y) }, Clone { path: "/1", id: Some(&x) }];
        assert_eq!(group(&forward).groups, group(&backward).groups);
        assert_eq!(group(&forward).groups.first().map(|g| g.key.as_str()), Some("github.com/a/r"));
    }
}
