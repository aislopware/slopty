//! What a change reaches of the e2e smoke set (`docs/decisions/testing.md`, "Goldens render in
//! the test process; e2e is a smoke set chosen by what a change reaches", item 5).
//!
//! The smoke set is seven cases ([`SMOKE`]), each named by its tests. A case reaches what its
//! run compiles: the binaries it starts and its test target ([`roots`]), and everything they
//! depend on, read from the lock and the manifests at the change's head ([`Graph`]), each for the
//! systems it is built for there (macOS, iOS, Linux). So a shared crate or a `vendor/` member maps
//! itself, and a new crate is placed when a case first depends on it.
//!
//! A changed file reaches the cases that compile its package on a system its change compiles
//! on: a Rust line's `cfg` (its attributes, its file's and its module's declarations, an `if
//! cfg!(..)` block), evaluated for each system, on the new side for an added line and the old
//! side for a removed one ([`systems`]). A package the lock moves reaches the cases that compile
//! it. What the graph cannot see is named in [`RULES`]: the toolchain, the workspace's manifest,
//! cargo's and nextest's configuration, the workflows and actions the smoke jobs run, the xtask
//! code an e2e run goes through, and `vendor/ghostty`, which `libghostty-vt-sys` builds from.
//! What only draws or computes, held by goldens and unit tests in the test process, reaches
//! none ([`HELD`]).
//!
//! `cargo xtask smoke reach` says what a change reaches and why; the gate's `smoke plan` job runs
//! `cargo xtask smoke plan`, and its `e2e (mac)` and `e2e (ios)` jobs run what it chose.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result};
use xshell::{Shell, cmd};

/// How a smoke case runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Runs {
    /// The app's e2e target (`slopty-e2e`'s `app`), on a Mac.
    App,
    /// The simulator's (`ios`), on the iPhone and on the iPad.
    Ios,
    /// The server's (`server`): the CLI and MCP through a real server and worker.
    Server,
    /// A Linux worker in Docker (`cargo xtask linux e2e`).
    Linux,
    /// A worker deployed into a macOS guest with its grants (`cargo xtask vm e2e`).
    Vm,
}

impl Runs {
    /// The e2e target its tests are in.
    pub const fn target(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Ios => "ios",
            Self::Server => "server",
            Self::Linux => "linux",
            Self::Vm => "vm",
        }
    }
}

/// One case of the smoke set.
pub struct Smoke {
    /// Its name in the ruling.
    pub id: &'static str,
    /// What it holds that nothing in-process can.
    pub holds: &'static str,
    /// Where it runs.
    pub runs: Runs,
    /// Its tests, by nextest's name in their target.
    pub tests: &'static [&'static str],
}

/// The smoke set as the ruling names it (A's inventory, section 7, as merged). While the suites
/// still hold their other tests, a case is the tests named here; as lane H moves goldens into
/// the test process and merges S1's into one stack, the names here follow, and a name that
/// leaves its target fails `every_smoke_test_is_in_its_target`.
pub const SMOKE: [Smoke; 7] = [
    Smoke {
        id: "S1",
        holds: "a session on the real stack: boot, the inspector, resume, the prompt box's \
                mirror, a split shell, relaunch, the hang watchdog, one shader frame",
        runs: Runs::App,
        tests: &[
            "session::a_session_is_its_terminal_and_its_inspector_opens_a_call_whole",
            "interface::the_interface_comes_up_on_the_stack",
            "hangs::a_hang_of_the_main_thread_is_filed_like_a_crash",
            "past_sessions::a_tui_session_is_found_opened_and_continued",
            "prompt_box::the_inspector_mirrors_the_tuis_prompt_box",
            "surfaces_live::a_split_holds_a_shell_of_the_worker",
            "walk::a_split_shell_comes_back_after_a_dropped_link_and_a_relaunch",
            "agent_ui::an_agents_tools_lay_out_the_window",
            "media::an_animated_shader_draws_on_the_surface_path_and_moves_only_while_motion_runs",
        ],
    },
    Smoke {
        id: "S2",
        holds: "a remote window streamed through VideoToolbox",
        runs: Runs::App,
        tests: &[
            "surfaces_live::stream::a_remote_window_is_a_view_beside_the_session_and_fills_it",
        ],
    },
    Smoke {
        id: "S3",
        holds: "WebKit: a sandboxed page, a favicon, a chart",
        runs: Runs::App,
        tests: &[
            "media::a_page_card_is_sandboxed_and_acts_for_the_person",
            "interface::a_pages_icon_shows_on_its_tab",
            "media::a_chart_fence_a_pdf_and_a_csv_draw_in_the_chat",
        ],
    },
    Smoke {
        id: "S4",
        holds: "the CLI and MCP through a real server and worker",
        runs: Runs::Server,
        tests: &[
            "tests::the_cli_and_mcp_drive_a_real_worker_through_the_server",
            "tests::search_in_files_through_the_cli",
        ],
    },
    Smoke {
        id: "S5",
        holds: "iOS: one session with its inspector, and one shader frame, on the iPhone; the \
                app's launch and layout on the iPad",
        runs: Runs::Ios,
        tests: &[
            "screens::a_session_is_its_terminal_and_shows_its_inspector",
            "visuals::an_animated_shader_draws_on_the_surface_path_in_the_simulator",
        ],
    },
    Smoke {
        id: "S6",
        holds: "a Linux worker: a shell, files, inotify, a tunnel and the clipboard",
        runs: Runs::Linux,
        tests: &[
            "tests::a_linux_worker_runs_a_shell_and_serves_files",
            "tests::a_linux_worker_follows_files_on_inotify_and_searches_them",
            "tests::an_upload_lands_in_a_linux_shell_and_its_server_is_tunnelled_here",
            "tests::the_clipboard_crosses_between_a_linux_shell_and_the_client",
        ],
    },
    Smoke {
        id: "VM",
        holds: "a worker deployed into a macOS guest, with its TCC grants",
        runs: Runs::Vm,
        tests: &["tests::a_guest_worker_greets_as_macos_with_its_grants_and_runs_a_shell"],
    },
];

/// Every case.
const ALL: &[&str] = &["S1", "S2", "S3", "S4", "S5", "S6", "VM"];
/// The cases `xtask e2e` runs, as the gate's smoke jobs and `e2e.yml` do: all but S6 and VM.
const E2E: &[&str] = &["S1", "S2", "S3", "S4", "S5"];

/// Paths the dependency graph cannot see (a directory with its `/`, or a file) and the cases a
/// change under one reaches; a path takes every rule it falls under.
pub const RULES: &[(&str, &[&str])] = &[
    // What every build reads outside any package: the toolchain, the workspace's manifest (its
    // shared dependencies, patches and profiles) and cargo's configuration.
    ("Cargo.toml", ALL),
    ("rust-toolchain.toml", ALL),
    (".cargo/", ALL),
    // What every run reads: nextest's configuration (the setup script that builds the binaries
    // a test spawns, the e2e timeouts), the workflows that run the smoke cases, and the actions
    // they set up with.
    (".config/nextest.toml", ALL),
    (".github/actions/", ALL),
    (".github/workflows/ci.yml", E2E),
    (".github/workflows/e2e.yml", E2E),
    // The plan's own code: the plan that a change to it gets is the changed code's, so it reaches
    // every case the gate's jobs run, and the `smoke plan` job checks it did (ci.yml). `order`
    // decides whether a run stands aside, as the plan and the e2e jobs ask.
    ("xtask/src/gate/impact.rs", E2E),
    ("xtask/src/gate/order.rs", E2E),
    ("xtask/src/lib.rs", E2E),
    // The xtask code an e2e run goes through (`tests::the_e2e_path_s_xtask_modules_are_ruled`).
    ("xtask/Cargo.toml", E2E),
    ("xtask/src/e2e.rs", E2E),
    ("xtask/src/gate.rs", E2E),
    ("xtask/src/tools.rs", E2E),
    ("xtask/src/tools/", E2E),
    ("xtask/src/setup.rs", E2E),
    ("xtask/src/watchdog.rs", E2E),
    ("xtask/src/ios.rs", &["S5"]),
    ("xtask/src/icon.rs", &["S5"]),
    ("xtask/src/icon/", &["S5"]),
    // The app icon, which the iOS build turns into the app's (`xtask/src/icon.rs`).
    ("assets/icon.svg", &["S5"]),
    ("xtask/src/linux.rs", &["S6"]),
    ("xtask/src/vm.rs", &["VM"]),
];

/// Directories a package builds from without holding them, and the package: the terminal's
/// source, which `libghostty-vt-sys` compiles (`GHOSTTY_SOURCE_DIR`, `.cargo/config.toml`).
pub const SOURCES: &[(&str, &str)] = &[("vendor/ghostty", "libghostty-vt-sys")];

/// The workspace's packages that reach no smoke case though cases compile them, and why: each
/// only draws or computes, held by tests in the test process, so no e2e run is the first to see
/// its change (`docs/decisions/testing.md`, "Goldens render in the test process", item 5).
pub const HELD: &[(&str, &str)] = &[
    ("slopty-theme", "design tokens: in-process goldens"),
    ("slopty-shell-theme", "the theme model: in-process goldens"),
    ("slopty-kit", "the chrome kit: in-process tests and goldens"),
    ("slopty-markdown", "UI-free markdown: unit and property tests"),
    ("slopty-math", "math layout: unit tests and goldens"),
    ("slopty-syntax", "highlighting with no UI: unit tests"),
    ("slopty-veil", "the fade veil without a renderer: unit tests"),
];

/// The systems a package or a line is compiled for, one bit each.
pub type Os = u8;
/// macOS.
pub const MACOS: Os = 1;
/// iOS (the device and the simulator).
pub const IOS: Os = 2;
/// Linux.
pub const LINUX: Os = 4;
/// Every system a case runs on.
pub const EVERY: Os = MACOS | IOS | LINUX;
/// Each system, by its `target_os`.
const SYSTEMS: [(Os, &str); 3] = [(MACOS, "macos"), (IOS, "ios"), (LINUX, "linux")];

/// The daemons and the CLI, which every case starts.
const DAEMONS: [&str; 4] = ["slopty-ptyd", "slopty-workerd", "slopty-serverd", "slopty-cli"];

/// What a case's run compiles from: each package, the systems it is built for there, and whether
/// its dev-dependencies count (a test target's). Every case's test target is `slopty-e2e`'s,
/// built for the Mac with the test kit; the daemons and the CLI run on the Mac, or on Linux for
/// S6, built with `workspace-hack` co-selected, as every e2e build selects it to set its
/// dependencies' features (`e2e::PACKAGES`); the app's cases start the app, and S5 the iOS app
/// and its notification extension.
pub fn roots(runs: Runs) -> Vec<(&'static str, Os, bool)> {
    let mut roots = vec![(crate::e2e::LIVE_PACKAGE, MACOS, true), ("slopty-testkit", MACOS, false)];
    let daemons = if runs == Runs::Linux { LINUX } else { MACOS };
    roots.extend(DAEMONS.iter().map(|name| (*name, daemons, false)));
    roots.push((crate::tools::WORKSPACE_HACK, daemons, false));
    match runs {
        Runs::App => roots.push(("slopty", MACOS, false)),
        Runs::Ios => roots.extend([("slopty-ios", IOS, false), ("slopty-notify", IOS, false)]),
        Runs::Server | Runs::Linux | Runs::Vm => {}
    }
    roots
}

/// Every way a case runs.
const RUNS: [Runs; 5] = [Runs::App, Runs::Ios, Runs::Server, Runs::Linux, Runs::Vm];

/// What a change reaches: each case and the paths that reach it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reach(pub BTreeMap<&'static str, BTreeSet<String>>);

impl Reach {
    /// Whether it reaches `id`.
    pub fn has(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    /// The cases reached, in [`SMOKE`]'s order.
    pub fn cases(&self) -> impl Iterator<Item = &'static Smoke> + '_ {
        SMOKE.iter().filter(|smoke| self.has(smoke.id))
    }

    /// The nextest filterset of the reached cases that run as `runs`, `None` when none does:
    /// their tests by name, in their target.
    pub fn filterset(&self, runs: Runs) -> Option<String> {
        let tests: Vec<String> = self
            .cases()
            .filter(|smoke| smoke.runs == runs)
            .flat_map(|smoke| smoke.tests.iter().map(|test| format!("test(={test})")))
            .collect();
        (!tests.is_empty()).then(|| {
            format!(
                "binary_id({}::{}) & ({})",
                crate::e2e::LIVE_PACKAGE,
                runs.target(),
                tests.join(" | ")
            )
        })
    }

    /// One filterset over the reached cases of each of `runs`, `None` when none is reached.
    pub fn filterset_of(&self, runs: &[Runs]) -> Option<String> {
        let sets: Vec<String> =
            runs.iter().filter_map(|r| self.filterset(*r)).map(|f| format!("({f})")).collect();
        (!sets.is_empty()).then(|| sets.join(" | "))
    }

    fn add(&mut self, ids: impl IntoIterator<Item = &'static str>, path: &str) {
        for id in ids {
            let _new = self.0.entry(id).or_default().insert(path.to_owned());
        }
    }
}

impl std::fmt::Display for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            return f.write_str("no smoke case");
        }
        let said: Vec<String> = self
            .0
            .iter()
            .map(|(id, paths)| {
                let first = paths.iter().next().map_or("", String::as_str);
                let more = paths.len().saturating_sub(1);
                if more == 0 {
                    format!("{id} ({first})")
                } else {
                    format!("{id} ({first} and {more} more)")
                }
            })
            .collect();
        f.write_str(&said.join(", "))
    }
}

/// The string `key` of the lock's package `p`.
fn field<'a>(p: &'a toml::Table, key: &str) -> Option<&'a str> {
    p.get(key).and_then(toml::Value::as_str)
}

/// How many lines `bytes` breaks.
fn newlines(bytes: &[u8]) -> usize {
    bytes.split(|&c| c == b'\n').count().saturating_sub(1)
}

/// How one package depends on another: the systems each kind of dependency is declared for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Edge {
    normal: Os,
    build: Os,
    dev: Os,
}

/// The packages of the lock and how they depend on each other: the workspace's own (its members
/// and its patches, which have no `source`) with each dependency's kind and target from their
/// manifests, every other with what the lock lists, as a normal dependency on every system.
pub struct Graph {
    names: Vec<String>,
    /// Each local package's directory, longest first, and the package.
    dirs: Vec<(String, usize)>,
    deps: Vec<Vec<(usize, Edge)>>,
    /// A proc macro: built for the host, whatever it is compiled into.
    host: Vec<bool>,
}

impl Graph {
    /// The graph of `lock` (`Cargo.lock`'s text) and `manifests` (every `Cargo.toml` of the
    /// tree, by its path, the root's as `Cargo.toml`).
    pub fn new(lock: &str, manifests: &BTreeMap<String, String>) -> Result<Self> {
        let lock: toml::Table = lock.parse().context("Cargo.lock does not parse")?;
        let packages: Vec<&toml::Table> = lock
            .get("package")
            .and_then(toml::Value::as_array)
            .map(|p| p.iter().filter_map(toml::Value::as_table).collect())
            .unwrap_or_default();
        let names: Vec<String> =
            packages.iter().map(|p| field(p, "name").unwrap_or_default().to_owned()).collect();
        let local: Vec<bool> = packages.iter().map(|p| field(p, "source").is_none()).collect();
        let mut by_name: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            by_name.entry(name.as_str()).or_default().push(i);
        }
        // A dependency in the lock is `name`, `name version` or `name version (source)`.
        let resolve = |dep: &str| -> Option<usize> {
            let mut words = dep.split_whitespace();
            let name = words.next()?;
            let version = words.next();
            let source = words.next().map(|s| s.trim_start_matches('(').trim_end_matches(')'));
            by_name.get(name)?.iter().copied().find(|&i| {
                packages.get(i).is_some_and(|p| {
                    version.is_none_or(|v| field(p, "version") == Some(v))
                        && source.is_none_or(|s| field(p, "source") == Some(s))
                })
            })
        };
        let root: Option<toml::Table> = manifests.get("Cargo.toml").and_then(|t| t.parse().ok());
        let shared = root
            .as_ref()
            .and_then(|r| r.get("workspace"))
            .and_then(|w| w.get("dependencies"))
            .and_then(toml::Value::as_table);
        let mut dirs = Vec::new();
        let mut declared: BTreeMap<usize, BTreeMap<String, Edge>> = BTreeMap::new();
        let mut host = vec![false; packages.len()];
        for (path, text) in manifests {
            let Some(dir) = path.strip_suffix("/Cargo.toml") else { continue };
            let Ok(manifest) = text.parse::<toml::Table>() else { continue };
            let Some(name) = manifest.get("package").and_then(|p| p.get("name")) else { continue };
            let Some(i) = name
                .as_str()
                .and_then(|name| by_name.get(name))
                .and_then(|found| found.iter().copied().find(|&i| local.get(i) == Some(&true)))
            else {
                continue;
            };
            if declared.contains_key(&i) {
                continue;
            }
            dirs.push((dir.to_owned(), i));
            let proc_macro = manifest
                .get("lib")
                .and_then(|l| l.get("proc-macro"))
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            if let Some(host) = host.get_mut(i) {
                *host = proc_macro;
            }
            let _new = declared.insert(i, declarations(&manifest, shared));
        }
        dirs.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.len()));
        let deps = packages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let listed = p.get("dependencies").and_then(toml::Value::as_array);
                listed
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_str)
                    .filter_map(resolve)
                    .map(|j| {
                        let every = Edge { normal: EVERY, ..Edge::default() };
                        let edge = match declared.get(&i) {
                            Some(declared) => {
                                names.get(j).and_then(|n| declared.get(n)).copied().unwrap_or(every)
                            }
                            None => every,
                        };
                        (j, edge)
                    })
                    .collect()
            })
            .collect();
        Ok(Self { names, dirs, deps, host })
    }

    /// The graph at `rev`: its lock and every manifest of its tree.
    pub fn at(sh: &Shell, rev: &str) -> Result<Self> {
        let files = cmd!(sh, "git ls-tree -r --full-tree --name-only {rev}").quiet().read()?;
        let mut paths: Vec<String> = files
            .lines()
            .filter(|f| *f == "Cargo.toml" || f.ends_with("/Cargo.toml"))
            .map(str::to_owned)
            .collect();
        paths.push("Cargo.lock".to_owned());
        let mut texts = texts(sh, rev, &paths)?;
        let lock = texts.remove("Cargo.lock").context("no Cargo.lock at the change's head")?;
        Self::new(&lock, &texts)
    }

    /// The graph of the checkout at `root`, from its files: its lock and the manifests of the
    /// workspace's own packages (`crates/*`, `apps/*`, `vendor/*`, xtask and the hack).
    #[cfg(test)]
    pub fn of_checkout(root: &std::path::Path) -> Result<Self> {
        let mut manifests = BTreeMap::new();
        let mut paths = vec!["Cargo.toml".to_owned()];
        paths.extend(["xtask", "workspace-hack"].map(|dir| format!("{dir}/Cargo.toml")));
        for group in ["crates", "apps", "vendor"] {
            for entry in std::fs::read_dir(root.join(group))? {
                let name = entry?.file_name().to_string_lossy().into_owned();
                paths.push(format!("{group}/{name}/Cargo.toml"));
            }
        }
        for path in paths {
            if let Ok(text) = std::fs::read_to_string(root.join(&path)) {
                let _old = manifests.insert(path, text);
            }
        }
        Self::new(&std::fs::read_to_string(root.join("Cargo.lock"))?, &manifests)
    }

    /// Every package named `name`.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = usize> + 'a {
        (0..self.names.len()).filter(move |&i| self.names.get(i).is_some_and(|n| n == name))
    }

    /// The local package `path` is in, and its path inside it.
    fn package_of<'a>(&self, path: &'a str) -> Option<(usize, &'a str)> {
        self.dirs.iter().find_map(|(dir, i)| {
            path.strip_prefix(dir.as_str()).and_then(|r| r.strip_prefix('/')).map(|r| (*i, r))
        })
    }

    /// The systems each package is compiled for in a run from `roots` (as [`roots`] gives them):
    /// a normal dependency on the systems its target holds on, a build dependency and a proc
    /// macro for the host, a dev-dependency only of a root that takes them.
    pub fn compiled(&self, roots: &[(&str, Os, bool)]) -> Vec<Os> {
        let host_of = |os: Os| {
            let apple = if os & (MACOS | IOS) == 0 { 0 } else { MACOS };
            apple | (os & LINUX)
        };
        let mut systems = vec![0; self.names.len()];
        let mut stack: Vec<(usize, Os, bool)> = roots
            .iter()
            .flat_map(|&(name, os, dev)| self.named(name).map(move |i| (i, os, dev)))
            .collect();
        while let Some((i, os, dev)) = stack.pop() {
            let Some(had) = systems.get_mut(i) else { continue };
            let fresh = if dev { os } else { os & !*had };
            if fresh == 0 {
                continue;
            }
            *had |= fresh;
            for &(j, edge) in self.deps.get(i).into_iter().flatten() {
                let normal = fresh & edge.normal;
                let mut to = if self.host.get(j) == Some(&true) { host_of(normal) } else { normal };
                to |= host_of(fresh & edge.build);
                if dev {
                    to |= fresh & edge.dev;
                }
                if to & !systems.get(j).copied().unwrap_or(0) != 0 {
                    stack.push((j, to, false));
                }
            }
        }
        systems
    }
}

/// Each dependency `manifest` declares, by its package's name: the systems each kind applies on.
/// A target table's (`[target.'cfg(..)'.dependencies]`) applies where its predicate may hold, or
/// on its triple's system; a renamed one (`package = ..`) and a workspace one go by the package's
/// own name.
fn declarations(manifest: &toml::Table, shared: Option<&toml::Table>) -> BTreeMap<String, Edge> {
    let mut edges: BTreeMap<String, Edge> = BTreeMap::new();
    let mut take = |table: &toml::Table, systems: Os| {
        for (kind, slot) in
            [("dependencies", 0), ("build-dependencies", 1), ("dev-dependencies", 2)]
        {
            let Some(deps) = table.get(kind).and_then(toml::Value::as_table) else { continue };
            for (key, value) in deps {
                let renamed = |v: &toml::Value| {
                    v.get("package").and_then(toml::Value::as_str).map(str::to_owned)
                };
                let workspace = value.get("workspace").and_then(toml::Value::as_bool) == Some(true);
                let name = renamed(value)
                    .or_else(|| {
                        if workspace {
                            shared.and_then(|s| s.get(key)).and_then(renamed)
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| key.clone());
                let edge = edges.entry(name).or_default();
                match slot {
                    0 => edge.normal |= systems,
                    1 => edge.build |= systems,
                    _ => edge.dev |= systems,
                }
            }
        }
    };
    take(manifest, EVERY);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (spec, table) in targets {
            let Some(table) = table.as_table() else { continue };
            let systems = match spec.strip_prefix("cfg(").and_then(|s| s.strip_suffix(')')) {
                Some(pred) => systems(pred).0,
                None if spec.contains("-apple-ios") => IOS,
                None if spec.contains("-apple-darwin") => MACOS,
                None if spec.contains("-linux") => LINUX,
                None => 0,
            };
            take(table, systems);
        }
    }
    edges
}

/// The text of each of `paths` at `rev` that is a file there, from one `git cat-file --batch`.
fn texts(sh: &Shell, rev: &str, paths: &[String]) -> Result<BTreeMap<String, String>> {
    use std::fmt::Write as _;
    let input = paths.iter().fold(String::new(), |mut input, p| {
        let _ok = writeln!(input, "{rev}:{p}");
        input
    });
    let out = cmd!(sh, "git cat-file --batch").stdin(input).quiet().output()?;
    let mut rest = out.stdout.as_slice();
    let mut texts = BTreeMap::new();
    for path in paths {
        let Some(end) = rest.iter().position(|&b| b == b'\n') else { break };
        let header = String::from_utf8_lossy(rest.get(..end).unwrap_or_default()).into_owned();
        rest = rest.get(end.saturating_add(1)..).unwrap_or_default();
        // `<object> <type> <size>`, or `<name> missing` with no body.
        let mut fields = header.rsplitn(3, ' ');
        let Some(size) = fields.next().and_then(|s| s.parse::<usize>().ok()) else { continue };
        let kind = fields.next();
        let body = rest.get(..size).unwrap_or_default();
        if kind == Some("blob") {
            let _old = texts.insert(path.clone(), String::from_utf8_lossy(body).into_owned());
        }
        rest = rest.get(size.saturating_add(1)..).unwrap_or_default();
    }
    Ok(texts)
}

/// The cases a change to `path` reaches: its rules', its e2e test module's, and those whose run
/// compiles its package on a system `systems` gives for its change (a Rust file's changed lines
/// by their `cfg`, any other file's every one). `compiled` is each way a case runs and the
/// systems it compiles each package of `graph` for ([`Graph::compiled`]); `files` the e2e
/// targets' and the e2e drivers' files, each with its text on both sides of the change, in
/// which a module is looked for by name ([`names`]).
pub fn reached(
    path: &str,
    graph: &Graph,
    compiled: &[(Runs, Vec<Os>)],
    systems: &dyn Fn() -> Os,
    files: &BTreeMap<String, String>,
) -> BTreeSet<&'static str> {
    let mut ids: BTreeSet<&'static str> = RULES
        .iter()
        .filter(|(prefix, _)| under(path, prefix))
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    let e2e_tests = format!("crates/{}/tests/", crate::e2e::LIVE_PACKAGE);
    if let Some(test) = path.strip_prefix(&e2e_tests) {
        // A target's root reaches its cases; a module of the app's or the simulator's reaches the
        // cases with a test in it, and every case of its target when another file of the target
        // uses it (a helper: `served`, `planted`, `compact`); a module neither holds nor lends to
        // a smoke test reaches none.
        for smoke in &SMOKE {
            let target = smoke.runs.target();
            let module = test
                .strip_prefix(target)
                .and_then(|t| t.strip_prefix('/'))
                .and_then(|m| m.strip_suffix(".rs"))
                .map(|m| m.trim_end_matches("/mod").replace('/', "::"));
            let root = test == format!("{target}.rs");
            let reaches = module.is_some_and(|module| {
                let holds = smoke.tests.iter().any(|t| t.starts_with(&format!("{module}::")));
                holds || used_in_target(files, &e2e_tests, target, path, &module)
            });
            if root || reaches {
                let _new = ids.insert(smoke.id);
            }
        }
        return ids;
    }
    let package = SOURCES
        .iter()
        .find(|(dir, _)| under(path, dir))
        .and_then(|(_, name)| graph.named(name).next())
        .map(|i| (i, ""))
        .or_else(|| graph.package_of(path));
    let Some((package, inside)) = package else { return ids };
    let name = graph.names.get(package).map_or("", String::as_str);
    let own_tests = ["tests/", "benches/", "examples/"].iter().any(|d| inside.starts_with(d));
    let goldens = name == crate::e2e::LIVE_PACKAGE && inside.starts_with("golden/");
    if own_tests || goldens {
        return ids;
    }
    let code = std::path::Path::new(inside).extension().is_some_and(|e| e == "rs");
    let changed = std::cell::OnceCell::new();
    let changed =
        || *changed.get_or_init(|| if code && inside != "build.rs" { systems() } else { EVERY });
    // A held package reaches the cases only through an e2e driver that calls into it.
    let through: Vec<usize> = if HELD.iter().any(|(held, _)| *held == name) {
        driven(graph, files, name, inside)
    } else {
        vec![package]
    };
    for (runs, systems_of) in compiled {
        let built = through.iter().fold(0, |os, i| os | systems_of.get(*i).copied().unwrap_or(0));
        if built != 0 && built & changed() != 0 {
            ids.extend(SMOKE.iter().filter(|s| s.runs == *runs).map(|s| s.id));
        }
    }
    ids
}

/// Whether `path` is an e2e driver: an `e2e` module of a package's (the app's self-test
/// socket), which the e2e tests read the app through.
fn is_driver(path: &str) -> bool {
    (path.ends_with("/src/e2e.rs") || path.contains("/src/e2e/")) && !path.starts_with("xtask/")
}

/// The packages of the e2e drivers among `files` that call into the module `inside` of the held
/// package `name` (`slopty_kit::a11y`, whose tree the app's driver dumps), or into any of it for
/// a file outside its modules.
fn driven(graph: &Graph, files: &BTreeMap<String, String>, name: &str, inside: &str) -> Vec<usize> {
    let krate = format!("{}::", name.replace('-', "_"));
    let module = inside
        .strip_prefix("src/")
        .and_then(|m| m.split('/').next())
        .map(|m| m.trim_end_matches(".rs"))
        .filter(|m| !matches!(*m, "lib" | "main"));
    files
        .iter()
        .filter(|(path, text)| is_driver(path) && names(text, &krate, module))
        .filter_map(|(path, _)| graph.package_of(path).map(|(i, _)| i))
        .collect()
}

/// Whether a file of e2e target `target` (under `tests`, `path` aside) uses its module `module`
/// (`served`, `surfaces_live::stream`), by a path from the target's root or from its parent.
fn used_in_target(
    files: &BTreeMap<String, String>,
    tests: &str,
    target: &str,
    path: &str,
    module: &str,
) -> bool {
    let (dir, root) = (format!("{tests}{target}/"), format!("{tests}{target}.rs"));
    let segments: Vec<&str> = module.split("::").collect();
    let last = segments.last().copied().unwrap_or(module);
    files
        .iter()
        .filter(|(file, _)| file.as_str() != path && (file.starts_with(&dir) || **file == root))
        .any(|(_, text)| {
            names(text, "super::", Some(last))
                || (0..segments.len()).any(|k| {
                    let (outer, inner) = segments.split_at(k);
                    let prefix = outer.iter().fold(String::from("crate::"), |mut p, s| {
                        p.push_str(s);
                        p.push_str("::");
                        p
                    });
                    names(text, &prefix, Some(&inner.join("::")))
                })
        })
}

/// Whether `text` names `name` right after `prefix` (`crate::served`, `slopty_kit::a11y`), alone
/// or in a group (`slopty_kit::{a11y, frames}`); with no `name`, whether it names `prefix` at
/// all. A word in a group counts wherever it sits, so this may say yes for more.
fn names(text: &str, prefix: &str, name: Option<&str>) -> bool {
    text.match_indices(prefix).any(|(at, _)| {
        if ident(text.as_bytes().get(at.wrapping_sub(1))) {
            return false;
        }
        let rest = text.get(at.saturating_add(prefix.len())..).unwrap_or_default();
        let Some(name) = name else { return true };
        if let Some(after) = rest.strip_prefix(name) {
            return !ident(after.as_bytes().first());
        }
        rest.strip_prefix('{').is_some_and(|group| {
            let group = group.split('}').next().unwrap_or_default();
            let first = name.split("::").next().unwrap_or(name);
            group.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|w| w == first)
        })
    })
}

/// Whether `path` is `prefix` or under it.
fn under(path: &str, prefix: &str) -> bool {
    let file = prefix.rsplit('/').next().is_some_and(|last| last.contains('.'));
    path == prefix
        || (!file && path.starts_with(prefix))
        || (!prefix.ends_with('/') && path.starts_with(&format!("{prefix}/")))
}

/// The systems the change to the Rust file `path` compiles on: its `lines` (1-based; `None` for
/// the whole file) in `read`'s version of it, each by the `cfg`s it is under ([`spans`]) and
/// its file's, its module's declarations' up the tree. A `cfg` added, removed or changed moves
/// what it is on between its systems and its context's, so a line of one counts both: a
/// `#[cfg(..)]`'s, its item's lines without it; a file's `#![cfg(..)]`, the systems its module
/// is declared for. A file it cannot read compiles on every system.
pub fn systems_of(
    path: &str,
    lines: Option<&[usize]>,
    read: &dyn Fn(&str) -> Option<String>,
) -> Os {
    let Some(text) = read(path) else { return EVERY };
    let declared = module_systems(path, read, 0);
    let file = file_systems(&text) & declared;
    let Some(lines) = lines else { return file };
    let spans = spans(&text);
    let texts: Vec<&str> = text.lines().collect();
    lines.iter().fold(0, |os, &line| {
        let mut here = file & line_systems(&spans, line, None);
        let at = texts.get(line.saturating_sub(1)).map_or("", |l| l.trim_start());
        if at.starts_with("#![cfg") {
            here |= declared;
        }
        for (n, span) in spans.iter().enumerate() {
            if span.attribute.is_some_and(|(first, last)| (first..=last).contains(&line)) {
                for inner in span.lines.0..=span.lines.1 {
                    here |= file & line_systems(&spans, inner, Some(n));
                }
            }
        }
        os | here
    })
}

/// The systems `line` compiles on, within `spans` but the one at `but`.
fn line_systems(spans: &[Span], line: usize, but: Option<usize>) -> Os {
    spans
        .iter()
        .enumerate()
        .filter(|(n, span)| Some(*n) != but && (span.lines.0..=span.lines.1).contains(&line))
        .fold(EVERY, |os, (_, span)| os & span.systems)
}

/// The systems the file's own `#![cfg(..)]`s, before any item, allow.
fn file_systems(text: &str) -> Os {
    text.lines()
        .map(str::trim)
        .take_while(|l| l.is_empty() || l.starts_with("//") || l.starts_with("#!"))
        .filter_map(|l| l.strip_prefix("#![cfg(").and_then(|l| l.strip_suffix(")]")))
        .fold(EVERY, |os, pred| os & systems(pred).0)
}

/// The systems the module file `path` is declared for: where each file that may declare it
/// does (its `mod` lines' `cfg`s, that file's own, and up the tree, at most 8 deep), every
/// system when none does.
fn module_systems(path: &str, read: &dyn Fn(&str) -> Option<String>, depth: usize) -> Os {
    if depth > 8 {
        return EVERY;
    }
    let mut os = 0;
    let mut declared = false;
    for (parent, name) in parent_modules(path) {
        let Some(text) = read(&parent) else { continue };
        let spans = spans(&text);
        let lines: Vec<usize> = text
            .lines()
            .enumerate()
            .filter(|(_, l)| declares(l, &name))
            .map(|(n, _)| n.saturating_add(1))
            .collect();
        if lines.is_empty() {
            continue;
        }
        declared = true;
        let here = lines.iter().fold(0, |os, &line| os | line_systems(&spans, line, None));
        os |= here & file_systems(&text) & module_systems(&parent, read, depth.saturating_add(1));
    }
    if declared { os } else { EVERY }
}

/// Whether `line` declares the module `name` as a file (`mod name;`, with any visibility).
fn declares(line: &str, name: &str) -> bool {
    let mut line = line.trim();
    if let Some(rest) = line.strip_prefix("pub") {
        line = rest.trim_start();
        if let Some(rest) = line.strip_prefix('(') {
            line = rest.split_once(')').map_or("", |(_, r)| r).trim_start();
        }
    }
    line.strip_prefix("mod ").and_then(|l| l.strip_suffix(';')).is_some_and(|l| l.trim() == name)
}

/// The files that may declare the module file `path`, and the module's name: `a/b.rs` and
/// `a/b/mod.rs` are declared in `a.rs` or `a/mod.rs`, or in the crate's `lib.rs` or `main.rs`
/// beside them; a crate root has none.
fn parent_modules(path: &str) -> Vec<(String, String)> {
    let Some((dir, file)) = path.rsplit_once('/') else { return Vec::new() };
    if matches!(file, "lib.rs" | "main.rs") {
        return Vec::new();
    }
    let (dir, name) = if file == "mod.rs" {
        let Some((up, name)) = dir.rsplit_once('/') else { return Vec::new() };
        (up, name.to_owned())
    } else {
        let Some(name) = file.strip_suffix(".rs") else { return Vec::new() };
        (dir, name.to_owned())
    };
    let candidates = match dir.rsplit_once('/') {
        Some((up, last)) if last != "src" => {
            vec![format!("{up}/{last}.rs"), format!("{dir}/mod.rs")]
        }
        _ => vec![format!("{dir}/lib.rs"), format!("{dir}/main.rs")],
    };
    candidates.into_iter().map(|c| (c, name.clone())).collect()
}

/// One span of lines that compiles on fewer than every system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    /// Its first and last line, 1-based.
    lines: (usize, usize),
    /// The systems it compiles on.
    systems: Os,
    /// The lines of the `#[cfg(..)]` that opened it, when one did: a change to them moves the
    /// item between those systems and its context's.
    attribute: Option<(usize, usize)>,
}

/// One span the scanner has open: the line it starts on, the depth it is at, whether its item
/// opened a brace yet, the systems it allows, and what opened it. An attribute's also counts
/// the generic brackets its item has open (`<` after a name, `>` after anything but a space,
/// `-` or `=`) and the line of the first comma it met inside them: a comma there ends nothing,
/// and a span that ends with a bracket still open ends at that comma instead, since the count
/// was wrong. A `where` clause's commas end nothing either.
struct Open {
    start: usize,
    depth: usize,
    braced: bool,
    systems: Os,
    by: By,
    attribute: Option<(usize, usize)>,
    angle: usize,
    pending: Option<usize>,
    clause: bool,
}

impl Open {
    /// Whether it is an attribute's that its item's own `,` or `;` ends: no brace of its own yet,
    /// at `depth`.
    fn unbraced_at(&self, depth: usize) -> bool {
        self.by == By::Attribute && !self.braced && self.depth == depth
    }

    /// The span it makes when it ends on `end`, if it holds a line.
    fn close(&self, end: usize) -> Option<Span> {
        let end = self.pending.map_or(end, |pending| pending.min(end));
        (end >= self.start).then_some(Span {
            lines: (self.start, end),
            systems: self.systems,
            attribute: self.attribute,
        })
    }
}

/// What opened a span.
#[derive(Clone, Copy, PartialEq, Eq)]
enum By {
    /// A `#[cfg(..)]`.
    Attribute,
    /// An `if cfg!(..)` block, and the systems its `else` block allows.
    If(Os),
    /// That `else` block.
    Else,
}

/// The 1-based line spans of `text` that compile on fewer than every system, and the systems
/// each does: an item, statement, field, variant, argument or arm under a `#[cfg(..)]`, from the
/// attribute to the `,` or `;` or the closing brace that ends it, or else to the end of the list
/// or block it is in; and the inside of an `if cfg!(..) { .. }` block and of its plain `else {
/// .. }`, the `if` and `else` lines left out. A span holds only whole lines of its own: a line
/// it shares with code outside it (before the attribute, after the end) is left out, so where it
/// cannot tell it reaches more, never less. Strings, characters and comments are skipped; a
/// bracket in them does not count.
fn spans(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let (mut i, mut line, mut depth) = (0_usize, 1_usize, 0_usize);
    let mut open: Vec<Open> = Vec::new();
    while let Some(&b) = bytes.get(i) {
        let next = bytes.get(i.saturating_add(1)).copied();
        match b {
            b'\n' => line = line.saturating_add(1),
            b'/' if next == Some(b'/') => {
                while bytes.get(i).is_some_and(|&c| c != b'\n') {
                    i = i.saturating_add(1);
                }
                continue;
            }
            b'/' if next == Some(b'*') => {
                // Block comments nest.
                i = i.saturating_add(2);
                let mut nested = 1_usize;
                while let Some(&c) = bytes.get(i) {
                    let after = bytes.get(i.saturating_add(1)).copied();
                    if c == b'\n' {
                        line = line.saturating_add(1);
                    }
                    i = i.saturating_add(1);
                    if c == b'/' && after == Some(b'*') {
                        nested = nested.saturating_add(1);
                        i = i.saturating_add(1);
                    } else if c == b'*' && after == Some(b'/') {
                        nested = nested.saturating_sub(1);
                        i = i.saturating_add(1);
                        if nested == 0 {
                            break;
                        }
                    }
                }
                continue;
            }
            b'"' => {
                i = i.saturating_add(1);
                while let Some(&c) = bytes.get(i) {
                    match c {
                        // An escape, a line's end too (a string continued on the next line).
                        b'\\' => {
                            i = i.saturating_add(1);
                            if bytes.get(i) == Some(&b'\n') {
                                line = line.saturating_add(1);
                            }
                        }
                        b'\n' => line = line.saturating_add(1),
                        b'"' => break,
                        _ => {}
                    }
                    i = i.saturating_add(1);
                }
            }
            b'r' if next.is_some_and(|c| c == b'#' || c == b'"') && raw_string_at(bytes, i) => {
                let after = bytes.get(i.saturating_add(1)..).unwrap_or_default();
                let hashes = after.iter().take_while(|&&c| c == b'#').count();
                let start = i.saturating_add(1).saturating_add(hashes);
                if bytes.get(start) == Some(&b'"') {
                    let close: Vec<u8> =
                        std::iter::once(b'"').chain(std::iter::repeat_n(b'#', hashes)).collect();
                    i = start.saturating_add(1);
                    while let Some(rest) = bytes.get(i..)
                        && !rest.is_empty()
                        && !rest.starts_with(&close)
                    {
                        if rest.first() == Some(&b'\n') {
                            line = line.saturating_add(1);
                        }
                        i = i.saturating_add(1);
                    }
                    i = i.saturating_add(close.len());
                    continue;
                }
            }
            b'\'' => {
                // A character literal (`'{'`, `'\''`), not a lifetime (`'a`).
                let rest = bytes.get(i.saturating_add(1)..).unwrap_or_default();
                let len = match rest {
                    [b'\\', _, b'\'', ..] => 4,
                    [c, b'\'', ..] if *c != b'\\' => 3,
                    _ => 1,
                };
                i = i.saturating_add(len);
                continue;
            }
            b'#' if next == Some(b'[') => {
                let end = attribute_end(bytes, i.saturating_add(1));
                let attribute = String::from_utf8_lossy(bytes.get(i..end).unwrap_or_default());
                let last = line.saturating_add(attribute.matches('\n').count());
                if let Some(pred) =
                    attribute.strip_prefix("#[cfg(").and_then(|a| a.strip_suffix(")]"))
                    && let (allows, _) = systems(pred)
                    && allows != EVERY
                {
                    // A line with code before the attribute is not the span's alone.
                    let own = starts_its_line(bytes, i);
                    open.push(Open {
                        start: if own { line } else { line.saturating_add(1) },
                        depth,
                        braced: false,
                        systems: allows,
                        by: By::Attribute,
                        attribute: Some((line, last)),
                        angle: 0,
                        pending: None,
                        clause: false,
                    });
                }
                line = last;
                i = end;
                continue;
            }
            b'c' if bytes.get(i..).is_some_and(|r| r.starts_with(b"cfg!("))
                && !ident(bytes.get(i.wrapping_sub(1))) =>
            {
                let open_paren = i.saturating_add(4);
                let close = matching(bytes, open_paren);
                if let Some(guard) = if_cfg(bytes, i, close) {
                    let pred = String::from_utf8_lossy(
                        bytes.get(open_paren.saturating_add(1)..close).unwrap_or_default(),
                    );
                    let (may_hold, may_fail) = systems(&pred);
                    let (then, otherwise) =
                        if guard.negated { (may_fail, may_hold) } else { (may_hold, may_fail) };
                    let lines = bytes.get(i..guard.brace).unwrap_or_default();
                    let brace_line = line.saturating_add(newlines(lines));
                    open.push(Open {
                        start: brace_line.saturating_add(1),
                        depth,
                        braced: false,
                        systems: then,
                        by: By::If(otherwise),
                        attribute: None,
                        angle: 0,
                        pending: None,
                        clause: false,
                    });
                }
            }
            b'w' if bytes.get(i..).is_some_and(|r| r.starts_with(b"where"))
                && !ident(bytes.get(i.wrapping_sub(1)))
                && !ident(bytes.get(i.saturating_add(5))) =>
            {
                for item in open.iter_mut().filter(|o| o.unbraced_at(depth)) {
                    item.clause = true;
                }
            }
            b'<' if generic_open(bytes, i) => {
                for item in open.iter_mut().filter(|o| o.unbraced_at(depth)) {
                    item.angle = item.angle.saturating_add(1);
                }
            }
            b'>' if generic_close(bytes, i) => {
                for item in open.iter_mut().filter(|o| o.unbraced_at(depth) && o.angle > 0) {
                    item.angle = item.angle.saturating_sub(1);
                    if item.angle == 0 {
                        item.pending = None;
                    }
                }
            }
            b'{' | b'(' | b'[' => {
                if b == b'{' {
                    for item in open.iter_mut().filter(|o| o.depth == depth) {
                        item.braced = true;
                    }
                }
                depth = depth.saturating_add(1);
            }
            b'}' | b')' | b']' => {
                depth = depth.saturating_sub(1);
                while let Some(last) = open.last() {
                    let own = b == b'}' && last.braced && last.depth == depth;
                    if !own && last.depth <= depth {
                        break;
                    }
                    // A brace group in a pattern or an expression (`Touch { .. } => touch(),`,
                    // `} else {`): the item goes on to its own `,`, `;` or brace.
                    if own && last.by == By::Attribute && goes_on(bytes, i.saturating_add(1)) {
                        for item in open.iter_mut().filter(|o| o.depth == depth) {
                            item.braced = false;
                        }
                        break;
                    }
                    let end = match last.by {
                        // Its own brace ends it; the list's or block's brace is the parent's
                        // line when nothing else is on it.
                        By::Attribute if own => {
                            if ends_its_line(bytes, i.saturating_add(1)) {
                                line
                            } else {
                                line.saturating_sub(1)
                            }
                        }
                        By::Attribute if starts_its_line(bytes, i) => line.saturating_sub(1),
                        By::Attribute => line,
                        // A block of an `if cfg!` ends on the line before its closing brace.
                        By::If(_) | By::Else => line.saturating_sub(1),
                    };
                    spans.extend(last.close(end));
                    let by = last.by;
                    let _closed = open.pop();
                    if let By::If(systems) = by
                        && own
                        && let Some(brace) = else_brace(bytes, i.saturating_add(1))
                    {
                        let lines = bytes.get(i..brace).unwrap_or_default();
                        let newlines = newlines(lines);
                        open.push(Open {
                            start: line.saturating_add(newlines).saturating_add(1),
                            depth,
                            braced: false,
                            systems,
                            by: By::Else,
                            attribute: None,
                            angle: 0,
                            pending: None,
                            clause: false,
                        });
                        break;
                    }
                }
            }
            b',' | b';' => {
                // A `,` inside generic brackets ends nothing; one that does, or a `;` (`mod x;`,
                // `use ..;`, a statement), ends the attribute's item.
                if b == b',' {
                    for item in open.iter_mut().filter(|o| o.unbraced_at(depth) && o.angle > 0) {
                        item.pending = item.pending.or(Some(line));
                    }
                }
                let end = if ends_its_line(bytes, i.saturating_add(1)) {
                    line
                } else {
                    line.saturating_sub(1)
                };
                while let Some(last) = open.last() {
                    if !last.unbraced_at(depth) || (b == b',' && (last.angle > 0 || last.clause)) {
                        break;
                    }
                    spans.extend(last.close(end));
                    let _closed = open.pop();
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    spans
}

/// Whether the `r` at `at` starts a raw string (`r"`, `r#"`, `br#"`), not a name ending in `r`.
fn raw_string_at(bytes: &[u8], at: usize) -> bool {
    let before = bytes.get(at.wrapping_sub(1));
    !ident(before) || (before == Some(&b'b') && !ident(bytes.get(at.wrapping_sub(2))))
}

/// Whether the `<` at `at` opens generic brackets: right after a name or `::`, and neither `<<`
/// nor `<=` (rustfmt spaces a comparison).
fn generic_open(bytes: &[u8], at: usize) -> bool {
    let before = bytes.get(at.wrapping_sub(1));
    (ident(before) || before == Some(&b':'))
        && !matches!(bytes.get(at.saturating_add(1)), Some(b'<' | b'='))
}

/// Whether the `>` at `at` closes generic brackets: not after a space (a comparison) unless it
/// starts its line, as rustfmt puts a long list's `>`; not after `-` (`->`) or `=` (`=>`); and
/// not `>=`. A comparison taken for one only ends a span sooner.
fn generic_close(bytes: &[u8], at: usize) -> bool {
    let before = bytes.get(at.wrapping_sub(1));
    let after_a_name = before.is_some_and(|c| !c.is_ascii_whitespace() && *c != b'-' && *c != b'=');
    (after_a_name || starts_its_line(bytes, at)) && bytes.get(at.saturating_add(1)) != Some(&b'=')
}

/// Whether what follows a brace at `at - 1` on its line goes on with the item the brace was in:
/// anything but its end ([`ends_its_line`]), a name (a new item, but for `else`) or an attribute.
fn goes_on(bytes: &[u8], at: usize) -> bool {
    if ends_its_line(bytes, at) {
        return false;
    }
    let rest = bytes.get(at..).unwrap_or_default().trim_ascii_start();
    rest.starts_with(b"else") || !(ident(rest.first()) || rest.first() == Some(&b'#'))
}

/// Whether only spaces come before `at` on its line.
fn starts_its_line(bytes: &[u8], at: usize) -> bool {
    let before = bytes.get(..at).unwrap_or_default();
    let start = before.iter().rposition(|&c| c == b'\n').map_or(0, |n| n.saturating_add(1));
    before.get(start..).unwrap_or_default().iter().all(u8::is_ascii_whitespace)
}

/// Whether the rest of the line from `at` holds nothing past an item's end: spaces, one `,` or
/// `;`, a `//` comment.
fn ends_its_line(bytes: &[u8], at: usize) -> bool {
    let rest = bytes.get(at..).unwrap_or_default();
    let rest = rest.split(|&c| c == b'\n').next().unwrap_or_default().trim_ascii();
    let rest = rest.strip_prefix(b",").or_else(|| rest.strip_prefix(b";")).unwrap_or(rest);
    let rest = rest.trim_ascii_start();
    rest.is_empty() || rest.starts_with(b"//")
}

/// Whether `byte` continues an identifier.
fn ident(byte: Option<&u8>) -> bool {
    byte.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// The index of the bracket closing the one at `open`, or the end.
fn matching(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0_usize;
    let mut i = open;
    while let Some(&c) = bytes.get(i) {
        match c {
            b'(' | b'[' | b'{' => depth = depth.saturating_add(1),
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i = i.saturating_add(1);
    }
    bytes.len()
}

/// The guard of an `if cfg!(..) {` or `if !cfg!(..) {` whose `cfg!` starts at `at` and whose
/// predicate closes at `close`: whether it is negated, and where its block's brace is.
struct Guard {
    negated: bool,
    brace: usize,
}

fn if_cfg(bytes: &[u8], at: usize, close: usize) -> Option<Guard> {
    let before = bytes.get(..at)?;
    let before = before.trim_ascii_end();
    let (negated, before) = match before.strip_suffix(b"!") {
        Some(rest) => (true, rest.trim_ascii_end()),
        None => (false, before),
    };
    let keyword = before.strip_suffix(b"if")?;
    if ident(keyword.last()) {
        return None;
    }
    let after = close.saturating_add(1);
    let gap = bytes.get(after..)?.iter().take_while(|c| c.is_ascii_whitespace()).count();
    let brace = after.saturating_add(gap);
    (bytes.get(brace) == Some(&b'{')).then_some(Guard { negated, brace })
}

/// The index of the brace of a plain `else {` right after `from`, if one follows (not `else
/// if`).
fn else_brace(bytes: &[u8], from: usize) -> Option<usize> {
    let rest = bytes.get(from..)?;
    let gap = rest.iter().take_while(|c| c.is_ascii_whitespace()).count();
    let rest = rest.get(gap..)?.strip_prefix(b"else")?;
    let inner = rest.iter().take_while(|c| c.is_ascii_whitespace()).count();
    (rest.get(inner) == Some(&b'{'))
        .then(|| from.saturating_add(gap).saturating_add(4).saturating_add(inner))
}

/// The index past the `]` closing the attribute whose `[` is at `open`.
fn attribute_end(bytes: &[u8], open: usize) -> usize {
    matching(bytes, open).saturating_add(1).min(bytes.len())
}

/// What a `cfg` predicate says on one system: it holds, it fails, or it turns on what the build
/// sets (a feature, the architecture, a flag of its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    Yes,
    No,
    Maybe,
}

/// The systems the `cfg` predicate `pred` may hold on, and those it may fail on. On the
/// systems the cases build for: `target_os`, `target_vendor` and `target_family` are known,
/// `unix` holds and `windows` fails; the flags a case's binaries are never built with (`test`,
/// `doc`, `miri`, `fuzzing`, `clippy`, …) fail; anything else (`feature`, `target_arch`,
/// `debug_assertions`, a flag of the crate's own) may go either way. One that does not parse
/// may too.
pub fn systems(pred: &str) -> (Os, Os) {
    let tokens = tokens(pred);
    SYSTEMS.iter().fold((0, 0), |(hold, fail), &(bit, os)| {
        let mut at = 0;
        let says = predicate(&tokens, &mut at, os);
        let says = if at == tokens.len() { says } else { Holds::Maybe };
        (
            if says == Holds::No { hold } else { hold | bit },
            if says == Holds::Yes { fail } else { fail | bit },
        )
    })
}

/// A `cfg` predicate's tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'a> {
    Word(&'a str),
    Text(&'a str),
    Open,
    Close,
    Comma,
    Is,
}

fn tokens(pred: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut rest = pred.trim();
    while let Some(c) = rest.chars().next() {
        let (token, len) = match c {
            '(' => (Some(Token::Open), 1),
            ')' => (Some(Token::Close), 1),
            ',' => (Some(Token::Comma), 1),
            '=' => (Some(Token::Is), 1),
            '"' => {
                let end = rest
                    .get(1..)
                    .and_then(|r| r.find('"'))
                    .map_or(rest.len(), |e| e.saturating_add(2));
                (rest.get(1..end.saturating_sub(1)).map(Token::Text), end)
            }
            c if c.is_alphanumeric() || c == '_' => {
                let end =
                    rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
                (rest.get(..end).map(Token::Word), end)
            }
            c => (None, c.len_utf8()),
        };
        tokens.extend(token);
        rest = rest.get(len..).unwrap_or_default();
    }
    tokens
}

/// What the predicate at `at` says on `os`, moving `at` past it.
fn predicate(tokens: &[Token<'_>], at: &mut usize, os: &str) -> Holds {
    let Some(&Token::Word(word)) = tokens.get(*at) else {
        *at = at.saturating_add(1);
        return Holds::Maybe;
    };
    *at = at.saturating_add(1);
    match tokens.get(*at) {
        Some(Token::Open) => {
            *at = at.saturating_add(1);
            let mut parts = Vec::new();
            while !matches!(tokens.get(*at), Some(Token::Close) | None) {
                parts.push(predicate(tokens, at, os));
                if tokens.get(*at) == Some(&Token::Comma) {
                    *at = at.saturating_add(1);
                }
            }
            *at = at.saturating_add(1);
            match word {
                "all" if parts.contains(&Holds::No) => Holds::No,
                "all" if parts.iter().all(|p| *p == Holds::Yes) => Holds::Yes,
                "any" if parts.contains(&Holds::Yes) => Holds::Yes,
                "any" if parts.iter().all(|p| *p == Holds::No) => Holds::No,
                "not" => match parts.as_slice() {
                    [Holds::Yes] => Holds::No,
                    [Holds::No] => Holds::Yes,
                    _ => Holds::Maybe,
                },
                _ => Holds::Maybe,
            }
        }
        Some(Token::Is) => {
            *at = at.saturating_add(1);
            let Some(&Token::Text(value)) = tokens.get(*at) else { return Holds::Maybe };
            *at = at.saturating_add(1);
            let known = |yes: bool| if yes { Holds::Yes } else { Holds::No };
            match word {
                "target_os" => known(value == os),
                "target_vendor" => known(match value {
                    "apple" => os != "linux",
                    "unknown" => os == "linux",
                    _ => false,
                }),
                "target_family" => match value {
                    "unix" => Holds::Yes,
                    "windows" | "wasm" => Holds::No,
                    _ => Holds::Maybe,
                },
                _ => Holds::Maybe,
            }
        }
        _ => match word {
            "unix" => Holds::Yes,
            "windows" | "test" | "doc" | "doctest" | "docsrs" | "miri" | "fuzzing" | "clippy"
            | "rust_analyzer" | "coverage" | "loom" | "kani" => Holds::No,
            _ => Holds::Maybe,
        },
    }
}

/// What the change from `base`'s meeting point with `head` to `head` reaches: every path the
/// diff names (renames as a removal and an addition), each Rust file at its changed lines on
/// both sides, and each package the lock moves. A fixed number of git calls whatever the
/// change's size: one diff of every Rust file's lines, which also fetches their old sides into a
/// partial clone at once, and one read of each side's files.
pub fn reach(sh: &Shell, base: &str, head: &str) -> Result<Reach> {
    let base =
        cmd!(sh, "git merge-base {base} {head}").quiet().read().context("the change's base")?;
    let names = cmd!(sh, "git diff --no-renames --name-status {base} {head}")
        .quiet()
        .read()
        .context("the change's paths")?;
    let rows: Vec<(&str, &str)> = names.lines().filter_map(|row| row.split_once('\t')).collect();
    let graph = Graph::at(sh, head)?;
    let compiled: Vec<(Runs, Vec<Os>)> =
        RUNS.iter().map(|runs| (*runs, graph.compiled(&roots(*runs)))).collect();
    let diff = cmd!(sh, "git diff -U0 --no-color --no-renames {base} {head} -- :(top,glob)**/*.rs")
        .quiet()
        .read()
        .context("the change's lines")?;
    let hunks = file_hunks(&diff, rows.iter().map(|(_, path)| *path));
    // Every file a change's reach reads: the lock, and each Rust file with the files that may
    // declare it up the tree ([`module_systems`]); with a change to an e2e target, the target's
    // files, and with one to a held package, the e2e drivers, where [`reached`] looks for a
    // module by name.
    let mut wanted = BTreeSet::from(["Cargo.lock".to_owned()]);
    for (_, path) in &rows {
        if std::path::Path::new(path).extension().is_some_and(|e| e == "rs") {
            declarers(path, 0, &mut wanted);
        }
    }
    let e2e_tests = format!("crates/{}/tests/", crate::e2e::LIVE_PACKAGE);
    let held = |path: &str| {
        graph.package_of(path).is_some_and(|(i, _)| {
            graph.names.get(i).is_some_and(|name| HELD.iter().any(|(held, _)| held == name))
        })
    };
    let tests = rows.iter().any(|(_, path)| path.starts_with(&e2e_tests));
    let drivers = rows.iter().any(|(_, path)| held(path));
    let searched: Vec<String> = if tests || drivers {
        let listed = cmd!(sh, "git ls-tree -r --name-only --full-tree {head}")
            .quiet()
            .read()
            .context("the change's files")?;
        listed
            .lines()
            .filter(|path| {
                std::path::Path::new(path).extension().is_some_and(|e| e == "rs")
                    && ((tests && path.starts_with(&e2e_tests)) || (drivers && is_driver(path)))
            })
            .map(str::to_owned)
            .collect()
    } else {
        Vec::new()
    };
    wanted.extend(searched.iter().cloned());
    let wanted: Vec<String> = wanted.into_iter().collect();
    let (old, new) = (texts(sh, &base, &wanted)?, texts(sh, head, &wanted)?);
    let files: BTreeMap<String, String> = searched
        .into_iter()
        .map(|path| {
            let sides: Vec<&str> = [old.get(&path), new.get(&path)]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect();
            let both = sides.join("\n");
            (path, both)
        })
        .collect();
    let mut reach = Reach::default();
    for (status, path) in rows {
        if path == "Cargo.lock" {
            let side = |texts: &BTreeMap<String, String>| texts.get(path).cloned();
            let (old, new) = (side(&old).unwrap_or_default(), side(&new).unwrap_or_default());
            match lock_moves(&old, &new) {
                None => reach.add(ALL.iter().copied(), "Cargo.lock (it does not parse)"),
                Some(moved) => {
                    for name in moved {
                        let ids = compiled
                            .iter()
                            .filter(|(_, systems)| {
                                graph.named(&name).any(|i| systems.get(i).is_some_and(|s| *s != 0))
                            })
                            .flat_map(|(runs, _)| SMOKE.iter().filter(move |s| s.runs == *runs))
                            .map(|s| s.id);
                        reach.add(ids, &format!("Cargo.lock ({name})"));
                    }
                }
            }
            continue;
        }
        let at_head = |p: &str| new.get(p).cloned();
        let at_base = |p: &str| old.get(p).cloned();
        let systems = || -> Os {
            if status.starts_with('A') {
                return systems_of(path, None, &at_head);
            }
            if status.starts_with('D') {
                return systems_of(path, None, &at_base);
            }
            // A file the diff did not name as it is named here (a quoted path) is read whole.
            let Some(hunks) = hunks.get(path) else { return EVERY };
            let (old, new) = changed_lines(hunks);
            systems_of(path, Some(&new), &at_head) | systems_of(path, Some(&old), &at_base)
        };
        reach.add(reached(path, &graph, &compiled, &systems, &files), path);
    }
    Ok(reach)
}

/// Each of `paths`' part of the patch `diff`, by its `diff --git` header: the hunk headers
/// [`changed_lines`] reads (none for a change of mode alone).
fn file_hunks<'a>(diff: &str, paths: impl Iterator<Item = &'a str>) -> BTreeMap<&'a str, String> {
    let headers: BTreeMap<String, &str> =
        paths.map(|path| (format!("diff --git a/{path} b/{path}"), path)).collect();
    let mut hunks: BTreeMap<&str, String> = BTreeMap::new();
    let mut file: Option<&str> = None;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            file = headers.get(line).copied();
            if let Some(path) = file {
                let _new = hunks.entry(path).or_default();
            }
        } else if let Some(path) = file.filter(|_| line.starts_with("@@ ")) {
            let text = hunks.entry(path).or_default();
            text.push_str(line);
            text.push('\n');
        }
    }
    hunks
}

/// `path` and every file that may declare it as a module, up the tree as far as
/// [`module_systems`] looks.
fn declarers(path: &str, depth: usize, into: &mut BTreeSet<String>) {
    let _new = into.insert(path.to_owned());
    if depth > 8 {
        return;
    }
    for (parent, _) in parent_modules(path) {
        declarers(&parent, depth.saturating_add(1), into);
    }
}

/// The packages the lock `new` moved from `old` (another version or source of one whose name it
/// keeps), rather than only adding or dropping some: a new dependency comes with its manifest's
/// change. `None` when either does not parse: what it holds is unknown, so it reaches every case.
fn lock_moves(old: &str, new: &str) -> Option<BTreeSet<String>> {
    let packages = |lock: &str| -> Option<BTreeSet<(String, String, String)>> {
        let lock: toml::Table = lock.parse().ok()?;
        let Some(toml::Value::Array(packages)) = lock.get("package") else {
            return Some(BTreeSet::new());
        };
        Some(
            packages
                .iter()
                .filter_map(toml::Value::as_table)
                .map(|p| {
                    let field = |key: &str| {
                        p.get(key).and_then(toml::Value::as_str).unwrap_or("").to_owned()
                    };
                    (field("name"), field("version"), field("source"))
                })
                .collect(),
        )
    };
    let (old, new) = (packages(old)?, packages(new)?);
    let names: BTreeSet<&str> = new.iter().map(|(name, ..)| name.as_str()).collect();
    Some(
        old.iter()
            .filter(|p| !new.contains(*p) && names.contains(p.0.as_str()))
            .map(|p| p.0.clone())
            .collect(),
    )
}

/// The lines a `git diff -U0` removed from the old side and added on the new, from its hunk
/// headers (`@@ -a,b +c,d @@`).
fn changed_lines(diff: &str) -> (Vec<usize>, Vec<usize>) {
    let (mut old, mut new) = (Vec::new(), Vec::new());
    for header in diff.lines().filter(|l| l.starts_with("@@ ")) {
        let mut sides = header.split_whitespace().skip(1);
        for (side, sign, lines) in [(sides.next(), '-', &mut old), (sides.next(), '+', &mut new)] {
            let Some(range) = side.and_then(|s| s.strip_prefix(sign)) else { continue };
            let mut parts = range.splitn(2, ',');
            let start: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            lines.extend(start..start.saturating_add(count));
        }
    }
    (old, new)
}

/// `xtask smoke`: what a change reaches of the e2e smoke set.
#[derive(clap::Subcommand, Debug, Clone)]
pub enum SmokeCmd {
    /// What the change from `--base` to `--head` reaches, and through which paths.
    Reach {
        #[arg(long, default_value = "origin/main")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
    },
    /// The gate's plan: what the change reaches, as the matrix of its `e2e` jobs (`mac`: the
    /// app's and the server's cases; `ios`: the simulator's; each with its tests' filterset) in
    /// `$GITHUB_OUTPUT`, and each case and why in the job's summary.
    Plan {
        #[arg(long, default_value = "origin/main")]
        base: String,
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// Then read `gate`'s tip and say whether the run on `head` stands aside for a newer one
        /// (`superseded=true` in `$GITHUB_OUTPUT`; [`crate::gate::order`]).
        #[arg(long)]
        stand_aside: bool,
    },
}

/// Run `xtask smoke`.
pub fn run(sh: &Shell, cmd: &SmokeCmd) -> Result<()> {
    match cmd {
        SmokeCmd::Reach { base, head } => {
            let reach = reach(sh, base, head)?;
            for (id, paths) in &reach.0 {
                let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
                let holds = SMOKE.iter().find(|s| s.id == *id).map_or("", |s| s.holds);
                println!("{id} ({holds}): {}", paths.join(" "));
            }
            for runs in [Runs::App, Runs::Server, Runs::Ios] {
                if let Some(filter) = reach.filterset(runs) {
                    println!("{}: -E '{filter}'", runs.target());
                }
            }
            if reach.0.is_empty() {
                println!("no smoke case");
            }
        }
        SmokeCmd::Plan { base, head, stand_aside } => {
            let reach = reach(sh, base, head)?;
            let matrix = plan_matrix(&reach);
            let mut outputs = format!("matrix={matrix}\n");
            let mut summary = plan_summary(&reach);
            if *stand_aside {
                // Read last, so a push made while the plan ran counts.
                let wanted = format!("{head}^{{commit}}");
                let sha = cmd!(sh, "git rev-parse --verify {wanted}").quiet().read()?;
                let tip = crate::gate::order::tip(sh);
                let aside = crate::gate::order::stands_aside(tip.as_deref(), &sha);
                outputs = format!("{outputs}superseded={aside}\n");
                if aside && matrix != "[]" {
                    let tip = tip.unwrap_or_default();
                    summary = format!(
                        "{summary}\n### ⤳ `gate` moved on to {tip}, whose run carries this commit \
                         or replaced it: this run stands aside before its e2e, and is cancelled \
                         whole\n"
                    );
                }
            }
            println!("{summary}{outputs}");
            for (var, text) in [("GITHUB_OUTPUT", &outputs), ("GITHUB_STEP_SUMMARY", &summary)] {
                if let Ok(path) = std::env::var(var) {
                    use std::io::Write as _;
                    let mut file =
                        std::fs::OpenOptions::new().append(true).create(true).open(&path)?;
                    file.write_all(text.as_bytes())?;
                }
            }
        }
    }
    Ok(())
}

/// The gate's `e2e` jobs for `reach`, as its matrix: `mac` (`xtask e2e mac`) for the app's and
/// the server's cases, or `server` when it reaches the server's alone, which builds no app; `ios`
/// for the simulator's; each with the filterset of its reached tests; `[]` when it reaches none.
fn plan_matrix(reach: &Reach) -> String {
    let mac: (&str, &[Runs]) = if reach.filterset(Runs::App).is_some() {
        ("mac", &[Runs::App, Runs::Server])
    } else {
        ("server", &[Runs::Server])
    };
    let jobs: Vec<serde_json::Value> = [mac, ("ios", &[Runs::Ios])]
        .into_iter()
        .filter_map(|(case, runs)| {
            reach
                .filterset_of(runs)
                .map(|filter| serde_json::json!({ "case": case, "filter": filter }))
        })
        .collect();
    serde_json::Value::Array(jobs).to_string()
}

/// The plan in the job's summary: each case reached, what it holds and what reaches it, and
/// which run elsewhere.
fn plan_summary(reach: &Reach) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("### e2e smoke\n\n");
    if reach.0.is_empty() {
        out.push_str("The change reaches no smoke case.\n\n");
        return out;
    }
    out.push_str("| case | holds | reached through |\n|---|---|---|\n");
    for smoke in reach.cases() {
        let paths = reach.0.get(smoke.id).map(|p| p.iter().map(String::as_str).collect::<Vec<_>>());
        let paths = paths.unwrap_or_default();
        let shown: Vec<String> = paths.iter().take(3).map(|p| format!("`{p}`")).collect();
        let more = paths.len().saturating_sub(3);
        let more = if more == 0 { String::new() } else { format!(" and {more} more") };
        let _ok = writeln!(out, "| {} | {} | {}{more} |", smoke.id, smoke.holds, shown.join(", "));
    }
    let elsewhere: Vec<&str> =
        reach.cases().filter(|s| matches!(s.runs, Runs::Linux | Runs::Vm)).map(|s| s.id).collect();
    if !elsewhere.is_empty() {
        let _ok = writeln!(
            out,
            "\n{} need Docker or a macOS guest, which no hosted job has yet: `cargo xtask linux e2e` \
             and `cargo xtask vm e2e` run them.",
            elsewhere.join(" and ")
        );
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write as _;

    use super::{EVERY, Graph, IOS, LINUX, MACOS, Os, RUNS, Runs};

    /// The workspace's packages no case compiles, and why: the build's tools and what no smoke
    /// case starts.
    const OUTSIDE: &[(&str, &str)] = &[
        ("xtask", "the build's tools: the e2e path's modules are ruled by name"),
        ("slopty-files", "the File Provider extension: no smoke case opens Finder"),
    ];

    /// A small workspace: the app on the shell, the daemons on the crash reporter, the worker
    /// with a Linux-only crate, the iOS app on the shell, a test-only shaper, a proc macro, the
    /// transport through a registry crate onto a patched local one, and the hack under all.
    fn workspace() -> Graph {
        let local: [(&str, &str, &[&str]); 17] = [
            ("slopty", "apps/slopty", &["slopty-shell", "slopty-crash", "workspace-hack"]),
            (
                "slopty-shell",
                "crates/slopty-shell",
                &["slopty-shape", "slopty-macros", "slopty-theme", "workspace-hack"],
            ),
            ("slopty-theme", "crates/slopty-theme", &["workspace-hack"]),
            ("slopty-crash", "crates/slopty-crash", &["workspace-hack"]),
            ("slopty-ptyd", "apps/slopty-ptyd", &["slopty-crash", "workspace-hack"]),
            (
                "slopty-workerd",
                "apps/slopty-worker",
                &["slopty-crash", "slopty-inotify", "workspace-hack"],
            ),
            ("slopty-inotify", "crates/slopty-inotify", &["workspace-hack"]),
            ("slopty-serverd", "apps/slopty-server", &["slopty-crash", "workspace-hack"]),
            ("slopty-cli", "apps/slopty-cli", &["workspace-hack"]),
            ("slopty-ios", "apps/slopty-ios", &["slopty-shell", "workspace-hack"]),
            ("slopty-notify", "apps/slopty-notify", &["workspace-hack"]),
            ("slopty-e2e", "crates/slopty-e2e", &["noq", "slopty-testkit", "workspace-hack"]),
            ("slopty-testkit", "crates/slopty-testkit", &["workspace-hack"]),
            ("slopty-shape", "crates/slopty-shape", &["workspace-hack"]),
            ("slopty-macros", "crates/slopty-macros", &["workspace-hack"]),
            ("noq-proto", "vendor/noq-proto", &[]),
            ("workspace-hack", "workspace-hack", &[]),
        ];
        let mut lock = String::from("version = 4\n");
        for (name, _, deps) in &local {
            let deps: Vec<String> = deps.iter().map(|d| format!("\"{d}\"")).collect();
            writeln!(
                lock,
                "\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [{}]",
                deps.join(", ")
            )
            .unwrap();
        }
        lock.push_str(
            "\n[[package]]\nname = \"noq\"\nversion = \"1.4.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\ndependencies = [\"noq-proto\"]\n",
        );
        let mut manifests: BTreeMap<String, String> = BTreeMap::new();
        manifests.insert(
            "Cargo.toml".to_owned(),
            "[workspace]\n[workspace.dependencies]\nshaper = { path = \"crates/slopty-shape\", package = \"slopty-shape\" }\n".to_owned(),
        );
        for (name, dir, deps) in &local {
            let mut manifest = format!("[package]\nname = \"{name}\"\n");
            match *name {
                "slopty-shell" => manifest.push_str(
                    "[dependencies]\nslopty-macros = { path = \"../slopty-macros\" }\nslopty-theme = { path = \"../slopty-theme\" }\nworkspace-hack = { path = \"../../workspace-hack\" }\n[dev-dependencies]\nshaper.workspace = true\n",
                ),
                "slopty-workerd" => manifest.push_str(
                    "[dependencies]\nslopty-crash = { path = \"../../crates/slopty-crash\" }\nworkspace-hack = { path = \"../../workspace-hack\" }\n[target.'cfg(target_os = \"linux\")'.dependencies]\nslopty-inotify = { path = \"../../crates/slopty-inotify\" }\n",
                ),
                "slopty-macros" => manifest.push_str("[lib]\nproc-macro = true\n"),
                "slopty-e2e" => manifest.push_str(
                    "[dependencies]\nnoq = \"1.4.0\"\nworkspace-hack = { path = \"../../workspace-hack\" }\n[dev-dependencies]\nslopty-testkit = { path = \"../slopty-testkit\" }\n",
                ),
                _ => {
                    manifest.push_str("[dependencies]\n");
                    for dep in *deps {
                        writeln!(manifest, "{dep} = {{ path = \"x\" }}").unwrap();
                    }
                }
            }
            manifests.insert(format!("{dir}/Cargo.toml"), manifest);
        }
        Graph::new(&lock, &manifests).unwrap()
    }

    fn compiled(graph: &Graph) -> Vec<(Runs, Vec<Os>)> {
        RUNS.iter().map(|runs| (*runs, graph.compiled(&super::roots(*runs)))).collect()
    }

    fn ids(graph: &Graph, path: &str, systems: Os) -> BTreeSet<&'static str> {
        super::reached(path, graph, &compiled(graph), &|| systems, &BTreeMap::new())
    }

    /// A case reaches what its run compiles, for the systems it compiles it: the shell is the
    /// app's and the iPhone's, the crash reporter every case's, the patched transport through
    /// its registry crate too; a Linux-only dependency only S6's; a dev-dependency of a crate no
    /// case tests, and a crate's own tests, none; the hack every case's.
    #[test]
    fn a_case_reaches_what_its_run_compiles() {
        let graph = workspace();
        let at = |path, systems| ids(&graph, path, systems);
        assert_eq!(at("crates/slopty-shell/src/lib.rs", EVERY), ["S1", "S2", "S3", "S5"].into());
        assert_eq!(at("crates/slopty-shell/src/lib.rs", IOS), ["S5"].into(), "iOS-only lines");
        assert_eq!(at("crates/slopty-shell/src/lib.rs", LINUX), BTreeSet::new());
        let every: BTreeSet<&str> = super::ALL.iter().copied().collect();
        assert_eq!(at("crates/slopty-crash/src/lib.rs", EVERY), every);
        assert_eq!(at("vendor/noq-proto/src/lib.rs", EVERY), every);
        assert_eq!(at("workspace-hack/Cargo.toml", EVERY), every);
        assert_eq!(at("crates/slopty-inotify/src/lib.rs", EVERY), ["S6"].into());
        assert_eq!(at("crates/slopty-shape/src/lib.rs", EVERY), BTreeSet::new());
        assert_eq!(at("crates/slopty-shell/tests/goldens.rs", EVERY), BTreeSet::new());
        assert_eq!(at("crates/slopty-theme/src/lib.rs", EVERY), BTreeSet::new(), "held");
        assert_eq!(at("apps/slopty-server/src/main.rs", EVERY), every, "every case starts it");
        // A proc macro compiles for the Mac whatever it is built into: its iOS lines reach none.
        assert_eq!(at("crates/slopty-macros/src/lib.rs", IOS), BTreeSet::new());
        assert_eq!(at("crates/slopty-macros/src/lib.rs", MACOS), ["S1", "S2", "S3", "S5"].into());
        // A file of a package's that is not Rust takes every system.
        assert_eq!(at("apps/slopty-ios/project.yml", 0), ["S5"].into());
    }

    /// What no package holds is ruled by path: the toolchain, the manifest and cargo's and
    /// nextest's configuration every case, the workflows and the e2e path's xtask code the
    /// cases `xtask e2e` runs, and anything else none.
    #[test]
    fn the_rules_reach_what_no_package_holds() {
        let graph = workspace();
        let at = |path| ids(&graph, path, EVERY);
        let every: BTreeSet<&str> = super::ALL.iter().copied().collect();
        assert_eq!(at("Cargo.toml"), every);
        assert_eq!(at(".config/nextest.toml"), every);
        assert_eq!(at(".github/workflows/e2e.yml"), ["S1", "S2", "S3", "S4", "S5"].into());
        assert_eq!(at("xtask/src/gate.rs"), ["S1", "S2", "S3", "S4", "S5"].into());
        assert_eq!(at("xtask/src/ios.rs"), ["S5"].into());
        let e2e: BTreeSet<&str> = ["S1", "S2", "S3", "S4", "S5"].into();
        assert_eq!(at("xtask/src/gate/impact.rs"), e2e, "the plan's own code");
        assert_eq!(at("xtask/src/gate/order.rs"), e2e, "and whether a run stands aside");
        assert_eq!(at("xtask/src/lib.rs"), e2e, "and its command");
        assert_eq!(at("xtask/src/gate/pass.rs"), BTreeSet::new());
        assert_eq!(at("assets/icon.svg"), ["S5"].into(), "the iOS build's icon");
        assert_eq!(at("xtask/src/land.rs"), BTreeSet::new());
        assert_eq!(at("docs/TESTING.md"), BTreeSet::new());
        // A prefix is a directory or a file, never the start of another name.
        assert_eq!(at("Cargo.toml.orig"), BTreeSet::new());
        assert_eq!(at("xtask/src/tools.rsx"), BTreeSet::new());
    }

    /// A smoke test's own module reaches its cases, a target's root all of the target's, and a
    /// module another file of the target uses (a helper, alone or in a group of a `use`) every
    /// case of its target; a module that neither holds nor lends to a smoke test none.
    #[test]
    fn a_smoke_test_s_module_reaches_its_case() {
        let graph = workspace();
        let files: BTreeMap<String, String> = [
            ("app.rs", "mod served;\nmod planted;\nmod session;\nmod transcript;\n"),
            ("app/session.rs", "use crate::served::{STEP, node};\n"),
            ("app/media.rs", "fn plot() {\n    crate::planted::stack();\n}\n"),
            ("app/served.rs", "pub const STEP: u8 = 1;\n"),
            ("app/planted.rs", "pub fn stack() {}\n"),
            ("app/transcript.rs", "fn not_smoke() {}\n"),
            ("ios.rs", "mod compact;\n"),
            ("ios/screens.rs", "use crate::{compact::tap, other};\n"),
            ("ios/compact.rs", "pub fn tap() {}\n"),
        ]
        .into_iter()
        .map(|(path, text)| (format!("crates/slopty-e2e/tests/{path}"), text.to_owned()))
        .collect();
        let at = |path: &str| {
            let path = format!("crates/slopty-e2e/tests/{path}");
            super::reached(&path, &graph, &compiled(&graph), &|| EVERY, &files)
        };
        let app: BTreeSet<&str> = ["S1", "S2", "S3"].into();
        assert_eq!(at("app/media.rs"), ["S1", "S3"].into());
        assert_eq!(at("app/surfaces_live.rs"), ["S1", "S2"].into());
        assert_eq!(at("app/served.rs"), app, "a helper the target's tests use");
        assert_eq!(at("app/planted.rs"), app, "by a path from the root");
        assert_eq!(at("app/transcript.rs"), BTreeSet::new());
        assert_eq!(at("ios/screens.rs"), ["S5"].into());
        assert_eq!(at("ios/compact.rs"), ["S5"].into(), "in a group");
        assert_eq!(at("app.rs"), app);
        assert_eq!(at("server.rs"), ["S4"].into());
        assert_eq!(
            ids(&graph, "crates/slopty-e2e/golden/session-light.png", EVERY),
            BTreeSet::new()
        );
    }

    /// A held package reaches the cases through an e2e driver (an `e2e` module of a package's)
    /// that calls into the module changed, as the app's dumps its tree through
    /// `slopty_kit::a11y`: the cases that compile the driver; another of its modules none.
    #[test]
    fn a_held_module_an_e2e_driver_calls_reaches_the_driver_s_cases() {
        let graph = workspace();
        let files: BTreeMap<String, String> = [(
            "apps/slopty/src/e2e.rs".to_owned(),
            "fn dump() {\n    slopty_theme::tokens::all();\n}\n".to_owned(),
        )]
        .into();
        let at = |path: &str| super::reached(path, &graph, &compiled(&graph), &|| EVERY, &files);
        let app: BTreeSet<&str> = ["S1", "S2", "S3"].into();
        assert_eq!(at("crates/slopty-theme/src/tokens.rs"), app);
        assert_eq!(at("crates/slopty-theme/src/tokens/dark.rs"), app, "a module under it");
        assert_eq!(at("crates/slopty-theme/src/lib.rs"), app, "the crate's root");
        assert_eq!(at("crates/slopty-theme/src/colors.rs"), BTreeSet::new());
        assert_eq!(at("crates/slopty-theme/tests/tokens.rs"), BTreeSet::new(), "its own tests");
    }

    /// A Rust line compiles where every `cfg` it is under may hold: an attribute's (with
    /// `not(..)`, `any`, `target_vendor`), an `if cfg!(..)` block's and its `else`'s, a unit
    /// test's never.
    #[test]
    fn a_line_compiles_where_its_cfgs_hold() {
        let file = "crates/slopty-ui/src/screen.rs";
        let text = "\
use gpui::App;

#[cfg(target_os = \"ios\")]
fn soft_keyboard() {
    let brace = '{';
    let s = \"}\";
}

#[cfg(not(target_os = \"macos\"))]
fn not_the_mac() {}

#[cfg(any(target_os = \"linux\", target_os = \"freebsd\"))]
mod inotify;

fn shared() {
    #[cfg(target_os = \"ios\")]
    let only = 1;
    let both = 2;
    if cfg!(target_os = \"ios\") {
        touch();
    } else {
        click();
    }
    if !cfg!(target_vendor = \"apple\") {
        linux();
    }
    if cfg!(target_os = \"ios\") && wide {
        either();
    } else if other {
        either();
    }
}

#[cfg(test)]
mod tests {
    fn a_test() {}
}
";
        let read = |p: &str| (p == file).then(|| text.to_owned());
        let at = |line: usize| super::systems_of(file, Some(&[line]), &read);
        assert_eq!(at(1), EVERY);
        assert_eq!(at(5), IOS, "inside the iOS function, past a brace in quotes");
        assert_eq!(at(7), IOS, "its closing brace");
        assert_eq!(at(10), IOS | LINUX, "not(macos)");
        assert_eq!(at(13), LINUX, "any(linux, ..)");
        assert_eq!(at(17), IOS, "an iOS statement");
        assert_eq!(at(18), EVERY, "the statement after it");
        assert_eq!(at(19), EVERY, "the `if cfg!` line itself");
        assert_eq!(at(20), IOS, "an `if cfg!` block");
        assert_eq!(at(22), MACOS | LINUX, "its `else`");
        assert_eq!(at(25), LINUX, "`!cfg!(target_vendor = \"apple\")`");
        assert_eq!(at(28), EVERY, "a `cfg!` among other conditions restricts nothing");
        assert_eq!(at(30), EVERY);
        assert_eq!(at(36), 0, "a unit test is in no case's build");
        assert_eq!(super::systems_of(file, Some(&[]), &read), 0, "no line");
        assert_eq!(super::systems_of(file, None, &read), EVERY, "the whole file");
    }

    /// A `cfg` on a field, a variant, an argument or an arm narrows that alone, to its own `,`
    /// (past a comma inside generic brackets) or its own braces, not the list after it; a line
    /// it shares with code outside it is left out; a fn's signature with generics keeps its
    /// body; and where the scanner cannot tell, it reaches more.
    #[test]
    fn a_cfg_narrows_its_own_field_variant_or_arm() {
        let file = "crates/slopty-app/src/lib.rs";
        let text = "\
pub struct Workspace {
    #[cfg(target_os = \"macos\")]
    menu: HashMap<Key, Menu>,
    shared_field: u8,
    #[cfg(target_os = \"ios\")]
    keyboard: Keyboard,
    after_field: u8,
}

fn build() -> Workspace {
    Workspace {
        #[cfg(target_os = \"macos\")]
        menu: Menu::new(a, b),
        shared_value: 1,
        #[cfg(target_os = \"ios\")]
        keyboard: Keyboard::default()
    }
}

enum Event {
    #[cfg(target_os = \"ios\")]
    Touch { at: (f32, f32) },
    Click,
    #[cfg(target_os = \"macos\")]
    Menu(u8, u8),
    Key,
}

fn on(event: Event) {
    match event {
        #[cfg(target_os = \"ios\")]
        Event::Touch { .. } => touch(),
        Event::Click => click(),
        #[cfg(target_os = \"macos\")]
        Event::Menu(a, b) => {
            menu(a, b);
        }
        Event::Key => key(),
    }
}

#[cfg(target_os = \"macos\")]
fn mac_only<A, B>(map: HashMap<A, B>) -> Result<(), Error>
where
    A: Eq,
{
    mac_body();
}

fn after_fn() {}

struct Pair { #[cfg(target_os = \"ios\")] a: u8, b: u8 }

fn call(#[cfg(target_os = \"ios\")] touch: u8, both: u8) {}

fn guard(x: u8) {
    match x {
        #[cfg(target_os = \"ios\")]
        y if y<3 => one(),
        z => other(z),
    }
}

#[cfg(target_os = \"ios\")]
fn closes_late() -> Thing<
    A,
    B,
> {
    late_body();
}
";
        let read = |p: &str| (p == file).then(|| text.to_owned());
        let line = |needle: &str| {
            let at = text.lines().position(|l| l.trim_start().starts_with(needle));
            at.unwrap_or_else(|| panic!("no line holds {needle}")).saturating_add(1)
        };
        let at = |needle: &str| super::systems_of(file, Some(&[line(needle)]), &read);
        assert_eq!(at("menu: HashMap<Key, Menu>,"), MACOS, "a field, past its generics' comma");
        assert_eq!(at("shared_field: u8,"), EVERY, "the field after it");
        assert_eq!(at("keyboard: Keyboard,"), IOS);
        assert_eq!(at("after_field: u8,"), EVERY);
        assert_eq!(at("menu: Menu::new(a, b),"), MACOS, "a field of a struct literal");
        assert_eq!(at("shared_value: 1,"), EVERY);
        assert_eq!(at("keyboard: Keyboard::default()"), IOS, "the last, with no comma");
        assert_eq!(at("Touch { at: (f32, f32) },"), IOS, "a variant with braces");
        assert_eq!(at("Click,"), EVERY);
        assert_eq!(at("Menu(u8, u8),"), MACOS);
        assert_eq!(at("Key,"), EVERY);
        assert_eq!(at("Event::Touch { .. } => touch(),"), IOS, "an arm past its pattern's braces");
        assert_eq!(at("Event::Click => click(),"), EVERY);
        assert_eq!(at("menu(a, b);"), MACOS, "a block arm");
        assert_eq!(at("Event::Key => key(),"), EVERY);
        assert_eq!(at("mac_body();"), MACOS, "a body past generics in the signature");
        assert_eq!(at("fn after_fn"), EVERY);
        assert_eq!(at("struct Pair"), EVERY, "a line shared with code outside the field");
        assert_eq!(at("fn call("), EVERY, "an argument on the fn's line");
        assert_eq!(at("y if y<3"), IOS, "a bracket it miscounts ends the span at its comma");
        assert_eq!(at("z => other(z),"), EVERY, "and never narrows the arm after it");
        assert_eq!(at("late_body();"), IOS, "generics over several lines");
    }

    /// A `cfg` added or removed moves its item between the systems it names and its context's,
    /// so its line counts both; a file's `#![cfg(..)]` counts the systems its module is declared
    /// for.
    #[test]
    fn a_changed_cfg_counts_both_sides() {
        let file = "crates/slopty-app/src/lib.rs";
        let text = "\
fn start(cx: &mut App) {
    #[cfg(target_os = \"macos\")]
    this.publish_device_actions(cx);
    #[cfg(any(
        target_os = \"ios\",
        target_os = \"linux\",
    ))]
    this.touch(cx);
}
";
        let read = |p: &str| (p == file).then(|| text.to_owned());
        let at = |line: usize| super::systems_of(file, Some(&[line]), &read);
        assert_eq!(at(2), EVERY, "the attribute: its item on the Mac and, without it, everywhere");
        assert_eq!(at(3), MACOS, "the item's own line");
        assert_eq!(at(5), EVERY, "a line of a multi-line attribute");
        assert_eq!(at(8), IOS | LINUX);
        let lib = "apps/slopty-notify/src/lib.rs";
        let text = "//! The extension.\n#![cfg(target_os = \"ios\")]\nfn x() {}\n";
        let read = |p: &str| (p == lib).then(|| text.to_owned());
        assert_eq!(super::systems_of(lib, Some(&[2]), &read), EVERY, "a file's own cfg");
        assert_eq!(super::systems_of(lib, Some(&[3]), &read), IOS);
    }

    /// A raw byte string and a nested block comment hide their brackets as strings and comments
    /// do, and a string continued on the next line counts that line.
    #[test]
    fn strings_and_comments_hide_brackets_and_count_lines() {
        let file = "crates/slopty-app/src/lib.rs";
        let text = "\
const NOTE: &str = \"a long \\
    line\";
#[cfg(target_os = \"ios\")]
fn ios() {
    let a = br#\"}\"#;
    /* outer /* inner } */ still a comment } */
    ios_body();
}

fn shared() {}
";
        let read = |p: &str| (p == file).then(|| text.to_owned());
        let at = |line: usize| super::systems_of(file, Some(&[line]), &read);
        assert_eq!(at(2), EVERY);
        assert_eq!(at(7), IOS);
        assert_eq!(at(8), IOS, "its closing brace");
        assert_eq!(at(10), EVERY);
    }

    /// A module file compiles where each file that declares it does, up the tree, and where its
    /// own `#![cfg(..)]` allows.
    #[test]
    fn a_module_compiles_where_it_is_declared() {
        let files = [
            (
                "crates/slopty-platform/src/web.rs",
                "#[cfg(target_os = \"ios\")]\nmod ios;\nmod page;\n#[cfg(target_os = \"macos\")]\npub(crate) mod mac;\n",
            ),
            ("crates/slopty-platform/src/web/ios.rs", "pub fn view() {}\nmod sheet;\n"),
            ("crates/slopty-platform/src/web/mac.rs", "pub fn view() {}\n"),
            ("crates/slopty-platform/src/web/page.rs", "pub fn card() {}\nmod view;\n"),
            ("crates/slopty-platform/src/web/page/view.rs", "pub fn draw() {}\n"),
            ("crates/slopty-platform/src/web/ios/sheet.rs", "pub fn draw() {}\n"),
            (
                "crates/slopty-platform/src/lib.rs",
                "#[cfg(not(target_os = \"linux\"))]\npub mod web;\n",
            ),
            (
                "apps/slopty-notify/src/lib.rs",
                "//! The extension.\n#![cfg(target_os = \"ios\")]\nfn x() {}\n",
            ),
        ];
        let read = |p: &str| files.iter().find(|(f, _)| *f == p).map(|(_, t)| (*t).to_owned());
        let os = |path| super::systems_of(path, Some(&[1]), &read);
        assert_eq!(os("crates/slopty-platform/src/web/ios.rs"), IOS);
        assert_eq!(os("crates/slopty-platform/src/web/mac.rs"), MACOS, "`pub(crate) mod`");
        assert_eq!(os("crates/slopty-platform/src/web/page.rs"), MACOS | IOS, "its parent's");
        assert_eq!(os("crates/slopty-platform/src/web/page/view.rs"), MACOS | IOS);
        assert_eq!(os("crates/slopty-platform/src/web/ios/sheet.rs"), IOS);
        assert_eq!(os("apps/slopty-notify/src/lib.rs"), IOS);
        assert_eq!(os("crates/slopty-platform/src/undeclared.rs"), EVERY, "none declares it");
    }

    /// `cfg` predicates, for the systems they may hold on and those they may fail on.
    #[test]
    fn a_cfg_is_evaluated_for_each_system() {
        let says = super::systems;
        assert_eq!(says("target_os = \"ios\""), (IOS, MACOS | LINUX));
        assert_eq!(says("not(target_os = \"macos\")"), (IOS | LINUX, MACOS));
        assert_eq!(says("not(target_vendor = \"apple\")"), (LINUX, MACOS | IOS));
        assert_eq!(says("any(target_os = \"macos\", target_os = \"ios\")"), (MACOS | IOS, LINUX));
        assert_eq!(says("all(unix, not(target_os = \"ios\"))"), (MACOS | LINUX, IOS));
        assert_eq!(says("not(not(target_os = \"ios\"))"), (IOS, MACOS | LINUX));
        assert_eq!(says("feature = \"e2e\""), (EVERY, EVERY));
        assert_eq!(says("all(target_os = \"macos\", feature = \"e2e\")"), (MACOS, EVERY));
        assert_eq!(says("test"), (0, EVERY));
        assert_eq!(says("all(test, target_os = \"ios\")"), (0, EVERY));
        assert_eq!(says("any(test, feature = \"x\")"), (EVERY, EVERY));
        assert_eq!(says("windows"), (0, EVERY));
        assert_eq!(says("all("), (EVERY, EVERY), "one that does not parse may go either way");
    }

    /// A lock moves the packages it had that take another version or source, not those that
    /// come or go; one that does not parse is unknown, and reaches every case.
    #[test]
    fn a_lock_moves_the_packages_it_had() {
        let lock = |packages: &[(&str, &str)]| {
            packages.iter().fold(String::from("version = 4\n"), |lock, (name, version)| {
                format!("{lock}\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n")
            })
        };
        let old = lock(&[("tokio", "1.50.0"), ("serde", "1.0.229")]);
        let moved = |new: &str| super::lock_moves(&old, new);
        let none = Some(BTreeSet::new());
        assert_eq!(
            moved(&lock(&[("tokio", "1.50.0"), ("serde", "1.0.229"), ("wgpu", "29.0.0")])),
            none
        );
        assert_eq!(moved(&lock(&[("tokio", "1.50.0")])), none);
        assert_eq!(
            moved(&lock(&[("tokio", "1.51.0"), ("serde", "1.0.229")])),
            Some(["tokio".to_owned()].into())
        );
        assert_eq!(moved(&old), none, "the same lock moved nothing");
        assert_eq!(moved("[[package]\n"), None, "an unreadable lock is unknown");
    }

    /// A hunk header's removed lines on the old side and added ones on the new.
    #[test]
    fn a_diff_s_hunks_name_both_sides() {
        let diff = "diff --git a/x.rs b/x.rs\n@@ -3 +3 @@\n-a\n+b\n@@ -10,2 +11,3 @@\n@@ -20,4 +22,0 @@\n@@ -30,0 +31 @@\n";
        assert_eq!(
            super::changed_lines(diff),
            (vec![3, 10, 11, 20, 21, 22, 23], vec![3, 11, 12, 13, 31])
        );
    }

    /// One diff of every file is split by file: a file's hunks are its own, a change of mode
    /// alone has none, and a file the diff names otherwise (quoted) is left out, so it is read
    /// whole.
    #[test]
    fn one_diff_splits_into_each_file_s_hunks() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n\
                    @@ -3 +3 @@\n-x\n+y\n@@ -9,0 +10 @@\n+@@ not a header\n\
                    diff --git a/m.rs b/m.rs\nold mode 100644\nnew mode 100755\n\
                    diff --git \"a/q\\tq.rs\" \"b/q\\tq.rs\"\n@@ -1 +1 @@\n\
                    diff --git a/b.rs b/b.rs\n@@ -1,2 +0,0 @@\n-x\n-y\n";
        let hunks = super::file_hunks(diff, ["a.rs", "m.rs", "q\tq.rs", "b.rs"].into_iter());
        assert_eq!(hunks.get("a.rs").map(String::as_str), Some("@@ -3 +3 @@\n@@ -9,0 +10 @@\n"));
        assert_eq!(hunks.get("m.rs").map(String::as_str), Some(""));
        assert_eq!(hunks.get("q\tq.rs"), None);
        assert_eq!(hunks.get("b.rs").map(String::as_str), Some("@@ -1,2 +0,0 @@\n"));
    }

    /// The files a Rust change reads for its module's `cfg`: the crate's root for a top
    /// module, both ways of declaring a nested one, up the tree.
    #[test]
    fn a_module_s_declarers_are_read_up_the_tree() {
        let mut wanted = BTreeSet::new();
        super::declarers("crates/c/src/a/b.rs", 0, &mut wanted);
        assert_eq!(
            wanted.into_iter().collect::<Vec<_>>(),
            [
                "crates/c/src/a.rs",
                "crates/c/src/a/b.rs",
                "crates/c/src/a/mod.rs",
                "crates/c/src/lib.rs",
                "crates/c/src/main.rs"
            ]
        );
    }

    /// A reached case runs its own tests alone, in the target that holds them; the Mac's job
    /// runs the app's and the server's in one filterset.
    #[test]
    fn a_reached_case_runs_its_tests_alone() {
        let mut reach = super::Reach::default();
        reach.add(["S1", "S3", "S4", "S6"], "crates/slopty-app/src/lib.rs");
        let app = reach.filterset(Runs::App).unwrap();
        assert_eq!(app.matches("test(=").count(), 12, "S1's nine and S3's three");
        assert!(app.starts_with("binary_id(slopty-e2e::app) & ("), "{app}");
        assert_eq!(reach.filterset(Runs::Server).map(|f| f.matches("test(=").count()), Some(2));
        assert_eq!(reach.filterset(Runs::Ios), None);
        let mac = reach.filterset_of(&[Runs::App, Runs::Server]).unwrap();
        assert!(mac.contains("binary_id(slopty-e2e::server)") && mac.contains(&app), "{mac}");
        assert_eq!(reach.filterset_of(&[Runs::Ios]), None);
        let matrix: serde_json::Value = serde_json::from_str(&super::plan_matrix(&reach)).unwrap();
        assert_eq!(matrix.as_array().map(Vec::len), Some(1), "the Mac's job alone: {matrix}");
        assert_eq!(matrix[0]["case"], "mac");
        assert_eq!(matrix[0]["filter"], mac.as_str());
        assert_eq!(super::plan_matrix(&super::Reach::default()), "[]", "no job");
        let mut server = super::Reach::default();
        server.add(["S4"], "apps/slopty-server/src/main.rs");
        let matrix: serde_json::Value = serde_json::from_str(&super::plan_matrix(&server)).unwrap();
        assert_eq!(matrix[0]["case"], "server", "the server's cases alone build no app: {matrix}");
    }

    /// Every smoke test is a test of its target: a renamed or merged one is renamed here too.
    #[test]
    fn every_smoke_test_is_in_its_target() {
        let tests =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/slopty-e2e/tests");
        for smoke in &super::SMOKE {
            let target = smoke.runs.target();
            for test in smoke.tests {
                let mut parts: Vec<&str> = test.split("::").collect();
                let name = parts.pop().unwrap_or_default();
                let file = match smoke.runs {
                    Runs::App | Runs::Ios => tests.join(target).join(format!("{}.rs", parts[0])),
                    Runs::Server | Runs::Linux | Runs::Vm => tests.join(format!("{target}.rs")),
                };
                let text = std::fs::read_to_string(&file).unwrap_or_default();
                assert!(
                    text.contains(&format!("fn {name}()")),
                    "{} names {test}, which {} does not hold",
                    smoke.id,
                    file.display()
                );
            }
        }
    }

    /// The workspace's own graph: the crates the review found shared reach every case that runs
    /// them, `vendor/` members map themselves, the iOS app compiles the app's crate for iOS, and
    /// every local package is compiled by a case, held, or left outside by name with its reason.
    #[test]
    fn the_workspace_s_packages_are_placed_by_its_graph() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let graph = Graph::of_checkout(&root).unwrap();
        let systems: BTreeMap<Runs, Vec<Os>> =
            RUNS.iter().map(|runs| (*runs, graph.compiled(&super::roots(*runs)))).collect();
        let on = |runs: Runs, name: &str| -> Os {
            graph.named(name).fold(0, |os, i| os | systems[&runs].get(i).copied().unwrap_or(0))
        };
        for shared in [
            "slopty-crash",
            "slopty-settings",
            "slopty-agent",
            "slopty-platform",
            "slopty-tailnet",
            "noq-proto",
            "noq-udp",
            "workspace-hack",
        ] {
            for runs in [Runs::App, Runs::Server, Runs::Ios] {
                assert_ne!(on(runs, shared) & MACOS, 0, "{shared} in {runs:?}");
            }
        }
        for app in [
            "slopty-app",
            "slopty-shell",
            "slopty-ui",
            "slopty-terminal",
            "gpui_tokio",
            "libghostty-vt-sys",
        ] {
            assert_ne!(on(Runs::App, app), 0, "{app} in the app's cases");
        }
        assert_ne!(on(Runs::Ios, "slopty-app") & IOS, 0, "the iOS app is the app's crate on iOS");
        assert_eq!(on(Runs::Server, "slopty-app"), 0, "the server's case starts no app");
        assert_ne!(on(Runs::Linux, "slopty-pty") & LINUX, 0, "S6 builds the daemons for Linux");
        for (dir, i) in &graph.dirs {
            let name = &graph.names[*i];
            let compiled = RUNS.iter().any(|runs| systems[runs].get(*i).is_some_and(|s| *s != 0));
            let held = super::HELD.iter().any(|(h, _)| h == name);
            let outside = OUTSIDE.iter().any(|(o, _)| o == name);
            assert!(
                compiled != outside && !(held && outside),
                "{name} ({dir}): compiled by a case ({compiled}) or left outside by name \
                 ({outside}), not both"
            );
        }
        for (name, _) in super::HELD.iter().chain(OUTSIDE) {
            assert!(graph.named(name).next().is_some(), "{name} is no package of the lock");
        }
        for (_, name) in super::SOURCES {
            assert!(graph.named(name).next().is_some(), "{name} is no package of the lock");
        }
    }

    /// Every xtask module the e2e path and the plan call into is ruled, as are the plan and its
    /// command, so a change to them reaches the cases `xtask e2e` runs.
    #[test]
    fn the_e2e_path_s_xtask_modules_are_ruled() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for own in ["xtask/src/gate/impact.rs", "xtask/src/lib.rs"] {
            assert!(super::RULES.iter().any(|(path, _)| *path == own), "{own} is not ruled");
        }
        for file in ["e2e.rs", "ios.rs", "gate/impact.rs"] {
            let text = std::fs::read_to_string(src.join(file)).unwrap();
            // Its tests' texts, and its comments, name modules of e2e targets.
            let text = text.split("\n#[cfg(test)]\nmod tests").next().unwrap_or_default();
            let code = text.lines().filter(|line| !line.trim_start().starts_with("//"));
            for line in code {
                for (at, _) in line.match_indices("crate::") {
                    let after = line.get(at.saturating_add(7)..).unwrap_or_default();
                    let module: String = after
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    if module.is_empty() {
                        continue;
                    }
                    let ruled = super::RULES.iter().any(|(path, _)| {
                        *path == format!("xtask/src/{module}.rs")
                            || *path == format!("xtask/src/{module}/")
                    });
                    assert!(ruled, "xtask/src/{file} calls crate::{module}, which no rule names");
                }
            }
        }
    }
}
