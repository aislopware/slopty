//! What each gate lane last passed on, under `target/gate/pass/<lane>`, so a lane whose inputs
//! are the same as then is not run again.
//!
//! A lane's inputs are the index entries it reads (`mode sha path`, as `git ls-files --stage`
//! lists them, submodules at the commit the index pins) and everything outside the tree that can
//! change its verdict: the toolchain, the xtask binary that holds the lane's commands, the
//! environment cargo and the tests read, the OS and Xcode builds, the tools the lane runs.
//! Records are compared as text, entry by entry, so a skip means those inputs are equal, not that
//! two hashes collided. Environment values are stored as a hash, never as themselves.
//!
//! [`Scope::Build`] leaves out the paths no build, lint or test reads ([`inert`]), so a change
//! to the docs alone skips the cargo lanes; the tools lane (typos reads every file) sees them all.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::hash::Hasher as _;

use anyhow::{Context as _, Result};
use camino::{Utf8Path, Utf8PathBuf};
use xshell::{Shell, cmd};

/// Which index entries a lane reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// Every entry.
    Tree,
    /// Every entry but the [`inert`] ones.
    Build,
}

/// What to do with a lane.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Plan {
    /// Its inputs are exactly those of its last pass.
    Skip,
    /// Run it whole.
    Run,
    /// Only these packages' inputs (or their dependencies') changed since its last pass, and
    /// nothing outside a package did (`--since-pass`).
    Only(Vec<String>),
}

/// The packages always rechecked when anything changed: xtask's tests read the whole
/// repository (`tests::no_crate_reads_an_inert_path`).
const ALWAYS: [&str; 1] = ["xtask"];

/// The index's entries and what every lane depends on besides them.
pub struct Inputs {
    dir: Utf8PathBuf,
    common: String,
    listing: Vec<Entry>,
}

/// One index entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: String,
    /// `mode sha`.
    pub id: String,
}

impl Inputs {
    /// Gather the inputs of a gate on `tree`, a snapshot of `listing`, keeping records in
    /// `gate_dir/pass`.
    pub fn gather(gate_dir: &Utf8Path, tree: &Utf8Path, listing: Vec<Entry>) -> Result<Self> {
        let sh = Shell::new()?;
        sh.change_dir(tree);
        let mut common = String::new();
        // The toolchain the tree pins (clippy and rustdoc ship with it).
        common.push_str(&cmd!(sh, "rustc -vV").quiet().read()?);
        common.push('\n');
        // The lanes' commands are compiled into xtask; a rebuild that links the same bytes is
        // the same gate.
        let exe = std::env::current_exe().context("the xtask binary")?;
        let exe = std::fs::read(&exe).with_context(|| format!("read {}", exe.display()))?;
        let _written = writeln!(common, "xtask {:016x}", digest(&exe));
        let xcode = cmd!(sh, "xcode-select -p").quiet().ignore_stderr().read().unwrap_or_default();
        let plist = Utf8Path::new(xcode.trim()).parent().map(|d| d.join("Info.plist"));
        let plist = plist.map_or(0, |p| digest(&std::fs::read(p).unwrap_or_default()));
        let _written = writeln!(common, "xcode {} {plist:016x}", xcode.trim());
        let os = std::fs::read("/System/Library/CoreServices/SystemVersion.plist");
        let _written = writeln!(common, "os {:016x}", digest(&os.unwrap_or_default()));
        let mut vars: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| ENV_PREFIXES.iter().any(|p| name.starts_with(p)))
            .collect();
        vars.sort();
        for (name, value) in vars {
            let _written = writeln!(common, "env {name} {:016x}", digest(value.as_bytes()));
        }
        Ok(Self { dir: gate_dir.join("pass"), common, listing })
    }

    /// The record a lane that reads `scope` and depends on `extra` would leave.
    pub fn key(&self, lane: &str, scope: Scope, extra: &str) -> String {
        let mut key = format!("lane {lane}\n{}{extra}", self.common);
        if !key.ends_with('\n') {
            key.push('\n');
        }
        key.push_str(TREE);
        for entry in &self.listing {
            if scope == Scope::Build && inert(&entry.path) {
                continue;
            }
            key.push_str(&entry.id);
            key.push(' ');
            key.push_str(&entry.path);
            key.push('\n');
        }
        key
    }

    /// What `lane` must do for `key`. With `since_pass`, a lane whose last pass differs only
    /// inside packages runs on those packages and every package that depends on them.
    pub fn plan(&self, lane: &str, key: &str, since_pass: bool, tree: &Utf8Path) -> Result<Plan> {
        let Ok(last) = std::fs::read_to_string(self.record_path(lane)) else {
            return Ok(Plan::Run);
        };
        if last == key {
            return Ok(Plan::Skip);
        }
        if !since_pass {
            return Ok(Plan::Run);
        }
        let (Some((last_head, last_tree)), Some((head, now))) =
            (last.split_once(TREE), key.split_once(TREE))
        else {
            return Ok(Plan::Run);
        };
        if last_head != head {
            return Ok(Plan::Run);
        }
        let changed = changed_paths(last_tree, now);
        let packages = workspace(tree)?;
        Ok(affected(&packages, &changed).map_or(Plan::Run, Plan::Only))
    }

    /// Remember that `lane` passed on `key`.
    pub fn record(&self, lane: &str, key: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.record_path(lane);
        let partial = path.with_extension("partial");
        std::fs::write(&partial, key).with_context(|| format!("write {partial}"))?;
        std::fs::rename(&partial, &path).with_context(|| format!("write {path}"))?;
        Ok(())
    }

    fn record_path(&self, lane: &str) -> Utf8PathBuf {
        self.dir.join(lane.replace(' ', "-"))
    }
}

/// The line between a record's header and its entries.
const TREE: &str = "tree\n";

/// The variables that reach cargo, rustc, a build script or a test: their values are part of
/// every lane's inputs.
const ENV_PREFIXES: [&str; 16] = [
    "CARGO",
    "RUST",
    "SLOPTY",
    "NEXTEST",
    "INSTA",
    "PROPTEST",
    "LIBGHOSTTY",
    "GHOSTTY",
    "MACOSX_",
    "IPHONEOS_",
    "SDKROOT",
    "DEVELOPER_DIR",
    "CC",
    "CFLAGS",
    "ZIG",
    "CI",
];

/// Paths no build script, rustc, clippy, rustdoc or test reads: the prose and the configuration
/// of tools that are not cargo's. Enforced by `tests::no_crate_reads_an_inert_path`.
pub fn inert(path: &str) -> bool {
    const DIRS: [&str; 2] = ["docs/", ".github/"];
    const FILES: [&str; 13] = [
        "README.md",
        "CHANGELOG.md",
        "CLAUDE.md",
        "AGENTS.md",
        "cliff.toml",
        "committed.toml",
        "typos.toml",
        "deny.toml",
        "taplo.toml",
        "rustfmt.toml",
        ".gitignore",
        ".gitmodules",
        ".config/hakari.toml",
    ];
    DIRS.iter().any(|d| path.starts_with(d)) || FILES.contains(&path)
}

/// `name path size mtime` of the first `name` on `PATH`, or `name absent`.
pub fn tool_id(name: &str) -> String {
    let found = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|p| p.is_file())
    });
    found.map_or_else(
        || format!("tool {name} absent\n"),
        |path| format!("tool {name} {} {}\n", path.display(), file_id(&path)),
    )
}

fn file_id(path: &std::path::Path) -> String {
    std::fs::metadata(path).map_or_else(
        |_| "absent".to_owned(),
        |m| {
            let modified = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{} {modified}", m.len())
        },
    )
}

fn digest(bytes: &[u8]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// The paths whose entry differs between two records' entry lists (added, removed or changed).
fn changed_paths(last: &str, now: &str) -> BTreeSet<String> {
    let parse = |text: &str| -> BTreeMap<String, String> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, ' ');
                let (mode, sha, path) = (fields.next()?, fields.next()?, fields.next()?);
                Some((path.to_owned(), format!("{mode} {sha}")))
            })
            .collect()
    };
    let (last, now) = (parse(last), parse(now));
    let mut changed: BTreeSet<String> =
        now.iter().filter(|(p, id)| last.get(*p) != Some(id)).map(|(p, _)| p.clone()).collect();
    changed.extend(last.keys().filter(|p| !now.contains_key(*p)).cloned());
    changed
}

/// A workspace member: its name, its directory relative to the tree, and the members it
/// depends on (any kind: normal, build or dev).
#[derive(Debug)]
pub struct Package {
    pub name: String,
    pub dir: String,
    pub deps: Vec<String>,
}

/// The workspace members of `tree`, from `cargo metadata`.
fn workspace(tree: &Utf8Path) -> Result<Vec<Package>> {
    #[derive(serde::Deserialize)]
    struct Metadata {
        packages: Vec<Member>,
    }
    #[derive(serde::Deserialize)]
    struct Member {
        name: String,
        manifest_path: String,
        dependencies: Vec<Dependency>,
    }
    #[derive(serde::Deserialize)]
    struct Dependency {
        name: String,
        path: Option<String>,
    }
    let sh = Shell::new()?;
    sh.change_dir(tree);
    let json = cmd!(sh, "cargo metadata --format-version 1 --no-deps --offline").quiet().read()?;
    let metadata: Metadata = serde_json::from_str(&json).context("cargo metadata")?;
    let names: BTreeSet<&str> = metadata.packages.iter().map(|p| p.name.as_str()).collect();
    metadata
        .packages
        .iter()
        .map(|p| {
            let manifest = Utf8Path::new(&p.manifest_path);
            let dir = manifest
                .parent()
                .and_then(|d| d.strip_prefix(tree).ok())
                .with_context(|| format!("{manifest} is outside {tree}"))?;
            let deps = p
                .dependencies
                .iter()
                .filter(|d| d.path.is_some() && names.contains(d.name.as_str()))
                .map(|d| d.name.clone())
                .collect();
            Ok(Package { name: p.name.clone(), dir: dir.as_str().to_owned(), deps })
        })
        .collect()
}

/// The packages to recheck for `changed` paths: those the paths are in, every package that
/// depends on one of them, and [`ALWAYS`]. `None` when a path is outside every package (the
/// manifests' root, the lockfile, cargo's config, a vendored tree), or when that is all of them.
pub fn affected(packages: &[Package], changed: &BTreeSet<String>) -> Option<Vec<String>> {
    let mut picked: BTreeSet<&str> = BTreeSet::new();
    for path in changed {
        let owner = packages
            .iter()
            .filter(|p| p.dir.is_empty() || path.starts_with(&format!("{}/", p.dir)))
            .max_by_key(|p| p.dir.len())?;
        picked.insert(&owner.name);
    }
    if !changed.is_empty() {
        picked.extend(ALWAYS.iter().filter(|a| packages.iter().any(|p| p.name == **a)));
    }
    let mut frontier: Vec<&str> = picked.iter().copied().collect();
    while let Some(dep) = frontier.pop() {
        for dependent in packages.iter().filter(|p| p.deps.iter().any(|d| d == dep)) {
            if picked.insert(&dependent.name) {
                frontier.push(&dependent.name);
            }
        }
    }
    (picked.len() < packages.len()).then(|| picked.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{Entry, Inputs, Package, Plan, Scope, affected, changed_paths, inert};

    fn package(name: &str, dir: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_owned(),
            dir: dir.to_owned(),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    fn workspace() -> Vec<Package> {
        vec![
            package("proto", "crates/proto", &[]),
            package("ui", "crates/ui", &["proto"]),
            package("app", "apps/app", &["ui"]),
            package("worker", "crates/worker", &["proto"]),
            package("xtask", "xtask", &[]),
        ]
    }

    fn paths(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn a_change_in_one_crate_rechecks_it_its_dependents_and_xtask() {
        assert_eq!(
            affected(&workspace(), &paths(&["crates/ui/src/lib.rs"])),
            Some(vec!["app".to_owned(), "ui".to_owned(), "xtask".to_owned()])
        );
    }

    #[test]
    fn a_change_outside_every_crate_rechecks_everything() {
        assert_eq!(affected(&workspace(), &paths(&["crates/ui/src/a.rs", "Cargo.lock"])), None);
        assert_eq!(affected(&workspace(), &paths(&["vendor/noq-proto/src/lib.rs"])), None);
    }

    #[test]
    fn a_change_under_every_crate_rechecks_everything() {
        assert_eq!(affected(&workspace(), &paths(&["crates/proto/src/lib.rs"])), None);
    }

    #[test]
    fn a_crate_named_like_another_is_not_its_owner() {
        let mut packages = workspace();
        packages.push(package("ui-kit", "crates/ui-kit", &[]));
        assert_eq!(
            affected(&packages, &paths(&["crates/ui-kit/src/lib.rs"])),
            Some(vec!["ui-kit".to_owned(), "xtask".to_owned()])
        );
    }

    #[test]
    fn added_removed_and_changed_entries_all_count() {
        let last = "100644 aaa crates/ui/a.rs\n100644 bbb crates/ui/b.rs\n100644 ccc same.rs\n";
        let now = "100755 aaa crates/ui/a.rs\n100644 ccc same.rs\n100644 ddd new file.rs\n";
        assert_eq!(
            changed_paths(last, now),
            paths(&["crates/ui/a.rs", "crates/ui/b.rs", "new file.rs"])
        );
    }

    fn inputs(dir: &camino::Utf8Path, common: &str, listing: &[(&str, &str)]) -> Inputs {
        Inputs {
            dir: dir.to_owned(),
            common: common.to_owned(),
            listing: listing
                .iter()
                .map(|(path, id)| Entry { path: (*path).to_owned(), id: (*id).to_owned() })
                .collect(),
        }
    }

    /// A lane runs until it has passed, is skipped on the same inputs, and runs again (or on
    /// the changed packages only, under `--since-pass`) on others. The workspace is this one.
    #[test]
    fn a_lane_is_skipped_only_on_the_inputs_it_passed_on() {
        let root = crate::tools::repo_root().expect("repo root");
        let dir = camino::Utf8PathBuf::from_path_buf(std::env::temp_dir())
            .expect("a UTF-8 temp dir")
            .join(format!("xtask-pass-{}", std::process::id()));
        let ui = "crates/slopty-ui/src/lib.rs";
        let passed = inputs(&dir, "rustc 1\n", &[(ui, "100644 a"), ("docs/DEV.md", "100644 d")]);
        let key = passed.key("tests", Scope::Build, "");
        assert_eq!(passed.plan("tests", &key, true, &root).ok(), Some(Plan::Run));
        passed.record("tests", &key).expect("record");
        assert_eq!(passed.plan("tests", &key, false, &root).ok(), Some(Plan::Skip));

        let prose = inputs(&dir, "rustc 1\n", &[(ui, "100644 a"), ("docs/DEV.md", "100644 e")]);
        let prose_key = prose.key("tests", Scope::Build, "");
        assert_eq!(prose.plan("tests", &prose_key, false, &root).ok(), Some(Plan::Skip));
        let whole = prose.key("tests", Scope::Tree, "");
        assert_eq!(prose.plan("tests", &whole, false, &root).ok(), Some(Plan::Run));

        let edited = inputs(&dir, "rustc 1\n", &[(ui, "100644 b"), ("docs/DEV.md", "100644 d")]);
        let edited_key = edited.key("tests", Scope::Build, "");
        assert_eq!(edited.plan("tests", &edited_key, false, &root).ok(), Some(Plan::Run));
        let Ok(Plan::Only(packages)) = edited.plan("tests", &edited_key, true, &root) else {
            panic!("a change inside slopty-ui narrows the lane");
        };
        assert!(packages.iter().any(|p| p == "slopty-ui") && packages.iter().any(|p| p == "xtask"));
        assert!(!packages.iter().any(|p| p == "slopty-proto"), "{packages:?}");

        let toolchain = inputs(&dir, "rustc 2\n", &[(ui, "100644 b"), ("docs/DEV.md", "100644 d")]);
        let toolchain_key = toolchain.key("tests", Scope::Build, "");
        assert_eq!(toolchain.plan("tests", &toolchain_key, true, &root).ok(), Some(Plan::Run));
        let _removed = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_cargo_reads_is_never_inert() {
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "clippy.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            ".config/nextest.toml",
            "assets/icon.svg",
            "crates/ui/README.md",
            "crates/ui/docs/a.md",
        ] {
            assert!(!inert(path), "{path}");
        }
        assert!(inert("docs/decisions/tooling.md"));
        assert!(inert("README.md"));
    }

    /// The [`inert`] paths stay out of the cargo lanes' inputs only while no crate reads them:
    /// no `include_str!`, `include_bytes!` or path into them from any Rust source.
    #[test]
    fn no_crate_reads_an_inert_path() {
        let root = crate::tools::repo_root().expect("repo root");
        let mut sources = Vec::new();
        for group in ["crates", "apps", "xtask", "workspace-hack"] {
            collect_rs(&root.join(group).into_std_path_buf(), &mut sources);
        }
        let needles = [
            "docs/",
            ".github/",
            "README.md",
            "CHANGELOG.md",
            "CLAUDE.md",
            "AGENTS.md",
            "cliff.toml",
            "committed.toml",
            "typos.toml",
            "deny.toml",
            "taplo.toml",
            "rustfmt.toml",
            "hakari.toml",
        ];
        let mut readers = Vec::new();
        for file in sources.iter().filter(|f| !f.starts_with(root.join("xtask"))) {
            let text = std::fs::read_to_string(file).unwrap_or_default();
            for (n, line) in text.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                let reads = code.contains("include_str!")
                    || code.contains("include_bytes!")
                    || code.contains("CARGO_MANIFEST_DIR")
                    || code.contains("../");
                if reads && needles.iter().any(|needle| code.contains(needle)) {
                    readers.push(format!("{}:{}: {code}", file.display(), n + 1));
                }
            }
        }
        assert!(
            readers.is_empty(),
            "these read a path the gate treats as inert (xtask/src/gate/pass.rs `inert`):\n{}",
            readers.join("\n")
        );
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}
